//! Crew requests become bounded aircraft-profile actions, confirmed by the plugin.
use crate::{Shared, routes::snapshot};
use axum::{
    Json,
    extract::State as AxumState,
    response::{IntoResponse, Response},
};
use openatc_core::{
    apply::add_transmission,
    crew::{Action, CrewProfile},
    dialogue::say,
    state::{Request, SpeechTag},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub struct CrewSession {
    pub aircraft: String,
    pub profile: CrewProfile,
    pub values: BTreeMap<String, f64>,
    pub checklist: Option<(String, usize, bool)>,
    pub sequence: u32,
    pub pushback: Option<openatc_core::crew::PushbackRequest>,
    pub observed_at: Option<std::time::Instant>,
}
impl Default for CrewSession {
    fn default() -> Self {
        Self {
            aircraft: String::new(),
            profile: CrewProfile::default(),
            values: BTreeMap::new(),
            checklist: None,
            sequence: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |t| t.as_millis() as u32),
            pushback: None,
            observed_at: None,
        }
    }
}
#[derive(Deserialize, Serialize)]
pub struct Observation {
    pub aircraft: String,
    pub profile: Option<CrewProfile>,
    pub values: BTreeMap<String, f64>,
    #[serde(default)]
    pub available: Vec<String>,
}
pub async fn observe(
    AxumState(shared): AxumState<Shared>,
    Json(input): Json<Observation>,
) -> Response {
    let mut s = shared.write().await;
    let profile = input.profile.unwrap_or_else(|| s.crew.profile.clone());
    if s.crew.aircraft != input.aircraft || s.crew.profile != profile {
        s.crew.checklist = None;
        s.session.crew_actions.clear();
    }
    s.crew.aircraft = input.aircraft;
    s.session.crew_controls = profile
        .controls
        .iter()
        .map(|(id, c)| openatc_core::crew::Capability {
            id: id.clone(),
            label: c.label.clone(),
            role: c.role.clone(),
            available: input.available.contains(id),
            value: input.values.get(id).copied(),
        })
        .collect();
    s.crew.profile = profile;
    s.crew.values = input.values;
    s.crew.observed_at = Some(std::time::Instant::now());
    Json(json!({"accepted":true})).into_response()
}
fn speak(s: &mut crate::EngineState, role: &str, text: &str) {
    let voice = match role {
        "cabin" => &s.settings.attendant_voice,
        "ground" => &s.settings.ground_voice,
        _ => &s.settings.copilot_voice,
    }
    .clone();
    add_transmission(
        &mut s.session,
        match role {
            "cabin" => "ATTENDANT",
            "ground" => "GROUND",
            _ => "COPILOT",
        },
        text,
        &SpeechTag {
            voice,
            speed: 1.0,
            delivery: "standard".into(),
            ..Default::default()
        },
    );
}
fn reply(
    s: &mut crate::EngineState,
    role: &str,
    id: &str,
    slots: &[(&str, String)],
    ok: bool,
) -> Response {
    let text = say(id, slots);
    speak(s, role, &text);
    Json(json!({"result":{"accepted":ok,"message":text},"state":snapshot(&s.session,&s.settings)}))
        .into_response()
}
fn queue(s: &mut crate::EngineState, role: &str, id: &str, value: f64, response: String) -> bool {
    let t = &s.session.telemetry;
    let Some(c) = s.crew.profile.controls.get(id) else {
        return false;
    };
    if !openatc_core::crew::validate(&s.crew.profile, role, id, value)
        || !t.position_valid
        || t.paused
        || (c.ground_only && (!t.on_ground || t.ground_speed_knots > 0.5))
        || (c.airborne_only && t.on_ground)
        || (id == "gear" && value == c.off && t.on_ground)
        || s.crew
            .observed_at
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(3))
        || s.session.crew_actions.len() >= 8
    {
        return false;
    }
    s.crew.sequence = s.crew.sequence.wrapping_add(1).max(1);
    s.session.crew_actions.push(Action {
        sequence: s.crew.sequence,
        aircraft: s.crew.aircraft.clone(),
        control: id.into(),
        value,
        role: role.into(),
        response,
    });
    true
}
fn next_item(s: &mut crate::EngineState) -> Response {
    let Some((name, index, perform)) = s.crew.checklist.clone() else {
        return reply(s, "copilot", "crew_checklist_none", &[], false);
    };
    let Some(item) = s
        .crew
        .profile
        .checklists
        .get(&name)
        .and_then(|items| items.get(index))
        .cloned()
    else {
        s.crew.checklist = None;
        return reply(
            s,
            "copilot",
            "crew_checklist_complete",
            &[("checklist", name.replace('_', " "))],
            true,
        );
    };
    if perform {
        if !queue(
            s,
            "copilot",
            &item.control,
            item.value,
            say(&item.response, &[]),
        ) {
            return reply(
                s,
                "copilot",
                "crew_action_unavailable",
                &[("item", say(&item.challenge, &[]))],
                false,
            );
        }
        Json(json!({"result":{"accepted":true,"pending":true},"state":snapshot(&s.session,&s.settings)})).into_response()
    } else {
        reply(
            s,
            "copilot",
            "crew_checklist_challenge",
            &[("item", say(&item.challenge, &[]))],
            true,
        )
    }
}
#[derive(Deserialize)]
pub struct Ack {
    pub sequence: u32,
    pub aircraft: String,
    pub success: bool,
    pub detail: String,
}
pub async fn acknowledge(AxumState(shared): AxumState<Shared>, Json(ack): Json<Ack>) -> Response {
    let mut s = shared.write().await;
    let Some(action) = s
        .session
        .crew_actions
        .first()
        .filter(|a| a.sequence == ack.sequence && a.aircraft == ack.aircraft)
        .cloned()
    else {
        return Json(json!({"accepted":false})).into_response();
    };
    s.session.crew_actions.remove(0);
    let label = s
        .crew
        .profile
        .controls
        .get(&action.control)
        .map_or(action.control.clone(), |_| {
            say(&format!("crew_control_{}", action.control), &[])
        });
    if ack.success {
        s.crew.values.insert(action.control.clone(), action.value);
        let template = if action.control == "pushback_start" {
            "crew_pushback_start_sent"
        } else if s
            .crew
            .profile
            .controls
            .get(&action.control)
            .is_some_and(|c| c.momentary)
        {
            "crew_button_pressed"
        } else if action.role == "cabin" && action.control == "slides" {
            if action.value > 0.0 {
                "crew_slides_armed_checked"
            } else {
                "crew_slides_disarmed_checked"
            }
        } else {
            "crew_action_confirmed"
        };
        let text = say(template, &[("item", label), ("state", action.response)]);
        speak(&mut s, &action.role, &text);
        if let Some((_, index, true)) = s.crew.checklist.as_mut() {
            *index += 1;
            return next_item(&mut s);
        }
    } else {
        s.session.crew_actions.clear();
        s.crew.checklist = None;
        eprintln!("Crew action {} failed: {}", action.control, ack.detail);
        let text = say("crew_action_failed", &[("item", label)]);
        speak(&mut s, &action.role, &text);
    }
    Json(json!({"accepted":true,"state":snapshot(&s.session,&s.settings)})).into_response()
}

pub async fn request(shared: &Shared, request: &Request) -> Option<Response> {
    let role = request.role.as_str();
    if !["copilot", "cabin", "ground"].contains(&role) {
        return None;
    }
    let mut text = request.text.to_lowercase();
    let coffee = role == "cabin"
        && text.contains("coffee")
        && text
            .split_once(',')
            .is_some_and(|(control, _)| control.contains("arm") || control.contains("cross"));
    if coffee {
        text = text
            .split_once(',')
            .map_or(text.clone(), |(control, _)| control.to_owned());
    }
    let mut s = shared.write().await;
    if coffee {
        speak(&mut s, role, &say("crew_coffee_requested", &[]));
    }
    let profile = s.crew.profile.clone();
    let recognized = profile.controls.iter().any(|(id, c)| {
        c.role == role
            && openatc_core::dialogue::phrases(&format!("crew_request_{id}"))
                .iter()
                .any(|a| text.contains(a))
    }) || [
        "set ",
        "remove ",
        "disconnect ",
        "connect ",
        "add ",
        "please ",
        "can you ",
        "could you ",
        "remov",
        "discon",
        "conn",
        "lower ",
        "raise ",
        "engage ",
        "disengage ",
        "turn ",
        "arm ",
        "disarm ",
    ]
    .iter()
    .any(|word| text.starts_with(word))
        || text.contains("checklist")
        || text.contains("cross check")
        || text.contains("crosscheck")
        || text.contains("xcheck")
        || (role == "ground" && (text.contains("pushback") || text.contains("push back")))
        || s.crew.checklist.is_some() && role == "copilot";
    let ai_recovery = s.settings.ai_enabled
        && !s.settings.ai_model.is_empty()
        && ![
            "how are",
            "hello",
            "hi",
            "good morning",
            "good evening",
            "thanks",
            "thank you",
        ]
        .iter()
        .any(|prefix| text.starts_with(prefix));
    if !recognized && !ai_recovery {
        return None;
    }
    if s.crew
        .observed_at
        .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(3))
    {
        return Some(reply(&mut s, role, "crew_controls_wait", &[], false));
    }
    if !s.session.crew_actions.is_empty() {
        return Some(reply(&mut s, role, "crew_actions_busy", &[], false));
    }
    let pilot = s.session.plan.callsign.clone();
    let pilot_sequence = s.session.next_sequence;
    add_transmission(&mut s.session, &pilot, &request.text, &SpeechTag::default());
    if role == "ground" && (text.contains("pushback") || text.contains("push back")) {
        s.crew.pushback = openatc_core::crew::parse_pushback(&text);
        let words = text.trim().trim_end_matches(['.', '!']);
        let start_request = !text.contains('?')
            && (matches!(words, "pushback" | "push back")
                || [
                    "start pushback",
                    "start push back",
                    "request pushback",
                    "begin pushback",
                    "please start pushback",
                ]
                .iter()
                .any(|prefix| words == *prefix || words.starts_with(&format!("{prefix} "))));
        if start_request {
            if queue(&mut s, role, "pushback_start", 1.0, String::new()) {
                return Some(Json(json!({"result":{"accepted":true,"pending":true},"state":snapshot(&s.session,&s.settings)})).into_response());
            }
            return Some(reply(
                &mut s,
                role,
                "crew_action_unavailable",
                &[("item", say("crew_control_pushback_start", &[]))],
                false,
            ));
        }
        return Some(reply(&mut s, role, "crew_pushback_clarify", &[], false));
    }
    if role == "cabin"
        && !["arm", "disarm", "automatic", "manual"].iter().any(|word| {
            text.split_whitespace()
                .any(|w| w.trim_matches(|c: char| !c.is_alphanumeric()) == *word)
        })
        && ["cross check", "crosscheck", "xcheck"]
            .iter()
            .any(|word| text.contains(word))
    {
        let state = s.crew.values.get("slides").copied();
        return Some(match state {
            Some(1.0) => reply(&mut s, role, "crew_slides_armed_checked", &[], true),
            Some(0.0) => reply(&mut s, role, "crew_slides_disarmed_checked", &[], true),
            _ => reply(&mut s, role, "crew_slides_not_confirmed", &[], false),
        });
    }
    if role == "copilot" && text.contains("checklist") {
        if text.contains("cancel") || text.contains("stop") {
            s.crew.checklist = None;
            return Some(reply(&mut s, role, "crew_checklist_stopped", &[], true));
        }
        let name = profile
            .checklists
            .keys()
            .find(|name| text.contains(&name.replace('_', " ")))
            .cloned();
        if let Some(name) = name {
            s.crew.checklist = Some((name, 0, text.contains("perform") || text.contains("do the")));
            return Some(next_item(&mut s));
        }
        return Some(reply(
            &mut s,
            role,
            "crew_checklist_choose",
            &[(
                "checklist",
                profile
                    .checklists
                    .keys()
                    .map(|k| k.replace('_', " "))
                    .collect::<Vec<_>>()
                    .join(", "),
            )],
            false,
        ));
    }
    if role == "copilot"
        && let Some((name, index, false)) = s.crew.checklist.clone()
    {
        let item = profile
            .checklists
            .get(&name)
            .and_then(|items| items.get(index))
            .cloned()?;
        if text.split_whitespace().any(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .eq_ignore_ascii_case(&say(&item.response, &[]))
                || ["set", "checked"].contains(&word.trim_matches(|c: char| !c.is_alphanumeric()))
        }) {
            let checked = s
                .crew
                .values
                .get(&item.control)
                .is_some_and(|v| (*v - item.value).abs() <= 0.01);
            if checked {
                s.crew.checklist = Some((name, index + 1, false));
                return Some(next_item(&mut s));
            }
            return Some(reply(
                &mut s,
                role,
                "crew_checklist_mismatch",
                &[
                    ("item", say(&item.challenge, &[])),
                    ("state", say(&item.response, &[])),
                ],
                false,
            ));
        }
        return Some(reply(
            &mut s,
            role,
            "crew_checklist_repeat",
            &[
                ("item", say(&item.challenge, &[])),
                ("state", say(&item.response, &[])),
            ],
            false,
        ));
    }
    let explicit_action = [
        "connect ",
        "disconnect ",
        "turn ",
        "set ",
        "add ",
        "remove ",
        "arm ",
        "disarm ",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix));
    if ((!explicit_action
        && profile.controls.iter().any(|(id, c)| {
            c.role == role
                && !c.momentary
                && openatc_core::dialogue::phrases(&format!("crew_request_{id}"))
                    .iter()
                    .any(|alias| text.trim_end_matches('?') == *alias)
        }))
        || text.starts_with("is ")
        || text.starts_with("are ")
        || text.starts_with("check ")
        || text.starts_with("confirm "))
        && let Some((id, c)) = profile.controls.iter().find(|(id, c)| {
            c.role == role
                && openatc_core::dialogue::phrases(&format!("crew_request_{id}"))
                    .iter()
                    .any(|a| text.contains(a))
        })
    {
        let value = s.crew.values.get(id).copied();
        let item = say(&format!("crew_control_{id}"), &[]);
        return Some(if let Some(value) = value {
            let state = c
                .states
                .iter()
                .find(|(_, n)| (**n - value).abs() < 0.01)
                .map(|(name, _)| say(&format!("crew_state_{name}"), &[]))
                .unwrap_or_else(|| {
                    if value == c.off {
                        say("crew_state_off", &[])
                    } else if value == c.on {
                        say("crew_state_on", &[])
                    } else {
                        say("crew_state_value", &[("value", format!("{value:.0}"))])
                    }
                });
            reply(
                &mut s,
                role,
                "crew_action_confirmed",
                &[("item", item), ("state", state)],
                true,
            )
        } else {
            reply(&mut s, role, "crew_action_failed", &[("item", item)], false)
        });
    }
    if text.contains("don't ") || text.contains("do not ") || text.contains("never ") {
        return Some(reply(&mut s, role, "crew_no_action", &[], true));
    }
    let generation = s.session_generation;
    let aircraft = s.crew.aircraft.clone();
    let parsed = openatc_core::crew::parse_many(&profile, role, &text);
    drop(s);
    let parsed = if parsed.is_some() {
        parsed
    } else {
        let settings = shared.read().await.settings.clone();
        if settings.ai_enabled && !settings.ai_model.is_empty() {
            let client = shared.read().await.client.clone();
            let mut ranked = profile
                .controls
                .iter()
                .filter(|(_, c)| c.role == role)
                .map(|(id, c)| {
                    let words = format!(
                        "{} {}",
                        c.label,
                        openatc_core::dialogue::phrases(&format!("crew_request_{id}")).join(" ")
                    )
                    .to_lowercase();
                    let score = text
                        .split(|c: char| !c.is_alphanumeric())
                        .filter(|w| w.len() > 2 && words.contains(*w))
                        .count();
                    (score, id, c)
                })
                .collect::<Vec<_>>();
            ranked.sort_by_key(|(score, _, _)| std::cmp::Reverse(*score));
            let allowed=ranked.into_iter().take(32).map(|(_,id,c)|json!({"control":id,"label":c.label,"min":c.min,"max":c.max,"on":c.on,"off":c.off,"states":c.states,"button":c.momentary})).collect::<Vec<_>>();
            let pilot_examples = {
                let state = shared.read().await;
                let phase = format!("{:?}", state.session.phase).to_lowercase();
                state
                    .speech
                    .select("pilot", &["request"], &phase, "icao", usize::MAX)
                    .iter()
                    .flat_map(|entry| entry.say.iter().cloned())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let prompt = format!(
                "Interpret explicit aircraft-control requests, tolerating obvious spelling and speech-recognition mistakes. Return only JSON {{\"actions\":[{{\"control\":\"id\",\"value\":number}}]}}, or {{}} if any requested action is unclear, a question, negated, or unsupported. Include every explicitly requested action in order, at most eight. An action verb may apply to multiple named controls. Never invent targets, repair numeric values or perform extra actions. Use only these controls: {}. Pilot wording examples follow; their numbers are fictional and must never replace the current request. Interpret the current request only. Return JSON, never a spoken reply.\n{}",
                json!(allowed),
                pilot_examples
            );
            crate::support::chat_complete(
                &client,
                &settings.ai_url,
                &settings.ai_model,
                0.0,
                &prompt,
                &request.text,
            )
            .await
            .ok()
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .and_then(|v| {
                let entries = v
                    .get("actions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_else(|| vec![v]);
                if entries.is_empty() || entries.len() > 8 {
                    return None;
                }
                let mut actions = Vec::new();
                for item in entries {
                    let id = item.get("control")?.as_str()?.to_owned();
                    let value = item.get("value")?.as_f64()?;
                    if !openatc_core::crew::validate(&profile, role, &id, value)
                        || actions.iter().any(|(existing, _)| existing == &id)
                    {
                        return None;
                    }
                    actions.push((id, value));
                }
                Some(actions)
            })
        } else {
            None
        }
    };
    let mut s = shared.write().await;
    if s.session_generation != generation {
        return Some(
            (
                axum::http::StatusCode::CONFLICT,
                Json(json!({"error":"Flight was reset; previous request discarded."})),
            )
                .into_response(),
        );
    }
    if s.crew.aircraft != aircraft {
        return Some(reply(&mut s, role, "crew_action_clarify", &[], false));
    }
    if let Some(actions) = parsed {
        if !s.session.crew_actions.is_empty() {
            return Some(reply(&mut s, role, "crew_actions_busy", &[], false));
        }
        for (id, value) in actions {
            let c = profile.controls.get(&id)?;
            let state = if c.momentary {
                say("crew_state_button", &[])
            } else {
                c.states
                    .iter()
                    .find(|(_, v)| (**v - value).abs() < 0.01)
                    .map(|(name, _)| say(&format!("crew_state_{name}"), &[]))
                    .unwrap_or_else(|| {
                        if value == c.off {
                            say("crew_state_off", &[])
                        } else if value == c.on {
                            say("crew_state_on", &[])
                        } else {
                            say("crew_state_value", &[("value", format!("{value:.0}"))])
                        }
                    })
            };
            let ok = queue(&mut s, role, &id, value, state);
            if !ok {
                s.session.crew_actions.clear();
                return Some(reply(
                    &mut s,
                    role,
                    "crew_action_unavailable",
                    &[("item", say(&format!("crew_control_{id}"), &[]))],
                    false,
                ));
            }
        }
        return Some(Json(json!({"result":{"accepted":true,"pending":true},"state":snapshot(&s.session,&s.settings)})).into_response());
    }
    if !recognized {
        s.session
            .transcript
            .retain(|entry| entry.sequence != pilot_sequence);
        return None;
    }
    Some(reply(&mut s, role, "crew_action_clarify", &[], false))
}
