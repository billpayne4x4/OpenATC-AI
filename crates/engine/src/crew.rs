//! Crew requests become bounded aircraft-profile actions, confirmed by the plugin.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use crate::{Shared, routes::snapshot};
use axum::{
    Json,
    extract::State as AxumState,
    response::{IntoResponse, Response},
};
use openatc_core::aircraft::AircraftBehaviour;
use openatc_core::{
    apply::add_transmission,
    crew::{Action, CrewProfile},
    dialogue::say,
    state::{Request, SpeechTag},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlAuthority {
    evidence: String,
    requested: bool,
}
impl ControlAuthority {
    fn matches(&self, request: &str) -> bool {
        let normalize = |text: &str| {
            text.to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        self.requested
            && !self.evidence.trim().is_empty()
            && normalize(request).contains(&normalize(&self.evidence))
    }
}

/// Retain only questions needed to clarify the current operation.
async fn pace_crew_reply(
    client: &reqwest::Client,
    settings: &openatc_settings::Settings,
    prompt: &str,
    request: &str,
    reply: &str,
    previous_reply: &str,
) -> String {
    pace_crew_reply_with_joke(
        client,
        settings,
        prompt,
        request,
        reply,
        previous_reply,
        None,
    )
    .await
}

async fn pace_crew_reply_with_joke(
    client: &reqwest::Client,
    settings: &openatc_settings::Settings,
    prompt: &str,
    request: &str,
    reply: &str,
    previous_reply: &str,
    joke: Option<&str>,
) -> String {
    let cleaned = openatc_core::dialogue::speech_text(reply);
    let reply = cleaned.as_str();
    let normalized_joke = joke.map(openatc_core::dialogue::speech_text);
    let joke = normalized_joke.as_deref();
    if !reply.contains(['?', '？']) {
        return reply.to_owned();
    }
    let schema = json!({"type":"object","additionalProperties":false,"required":["kind"],"properties":{"kind":{"type":"string","enum":["answer","clarification","follow_up"]}}});
    let mut cleaned = String::new();
    let mut kept_until = 0;
    for (question, _) in reply
        .char_indices()
        .filter(|(_, c)| matches!(c, '?' | '？'))
    {
        let start = reply[..question]
            .rfind(['.', '!', '\n', '?', '？'])
            .map_or(0, |index| {
                index + reply[index..].chars().next().unwrap().len_utf8()
            });
        let end = question + reply[question..].chars().next().unwrap().len_utf8();
        if joke.filter(|joke| !joke.is_empty()).is_some_and(|joke| {
            reply.find(joke).is_some_and(|offset| {
                if question < offset || question >= offset + joke.len() {
                    return false;
                }
                let after = &joke
                    [question - offset + reply[question..].chars().next().unwrap().len_utf8()..];
                let end = after
                    .find(['.', '!', '?', '？', '\n'])
                    .unwrap_or(after.len());
                !after[end..].starts_with(['?', '？'])
                    && after[..end].split_whitespace().count() >= 2
            })
        }) {
            continue;
        }
        let context = json!({"previousCrewReply":previous_reply,"captainMessage":request,
            "proposedReply":&reply[start..end],"fullReply":reply})
        .to_string();
        let decision = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            crate::llm::structured_format::<BTreeMap<String, String>>(
                client,
                &settings.ai_url,
                &settings.ai_model,
                0.0,
                prompt,
                &context,
                Some(schema.clone()),
            ),
        )
        .await
        .ok()
        .and_then(Result::ok);
        let allowed = decision
            .as_ref()
            .and_then(|decision| decision.get("kind"))
            .is_some_and(|kind| matches!(kind.as_str(), "answer" | "clarification"));
        if !allowed {
            cleaned.push_str(&reply[kept_until..start]);
            kept_until = end;
        }
    }
    cleaned.push_str(&reply[kept_until..]);
    if cleaned.trim().is_empty() {
        say("crew_conversation_pause_ack", &[])
    } else {
        cleaned.trim().to_owned()
    }
}

pub struct Batch {
    outcomes: Vec<String>,
    held_reply: String,
    started: std::time::Instant,
}

pub struct CrewSession {
    pub revision: u64,
    pub social: crate::memory::SocialMemory,
    pub ground_taxi_origin: Option<[f64; 2]>,
    pub aircraft: String,
    pub profile: CrewProfile,
    pub values: BTreeMap<String, f64>,
    pub checklist: Option<(String, usize, bool)>,
    pub sequence: u32,
    pub pushback: Option<openatc_core::crew::PushbackRequest>,
    pub observed_at: Option<std::time::Instant>,
    pub batch: Option<Batch>,
}
impl Default for CrewSession {
    fn default() -> Self {
        Self {
            revision: 0,
            social: crate::memory::SocialMemory::default(),
            ground_taxi_origin: None,
            aircraft: String::new(),
            profile: CrewProfile::default(),
            values: BTreeMap::new(),
            checklist: None,
            sequence: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |t| t.as_millis() as u32),
            pushback: None,
            observed_at: None,
            batch: None,
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
#[derive(Deserialize)]
pub struct CalloutLog {
    entries: Vec<String>,
    callsign: String,
}
/// Record clips already played by the cockpit's independent callout channel.
pub async fn callout_log(
    AxumState(shared): AxumState<Shared>,
    Json(input): Json<CalloutLog>,
) -> Response {
    let mut state = shared.write().await;
    if input.callsign != state.session.plan.callsign
        || input.entries.is_empty()
        || input.entries.len() > 2
        || input
            .entries
            .iter()
            .any(|text| text.is_empty() || text.len() > 128)
    {
        return Json(json!({"accepted":false})).into_response();
    }
    for text in input.entries {
        add_transmission(&mut state.session, "COPILOT", &text, &SpeechTag::default());
        if let Some(entry) = state.session.transcript.last_mut() {
            entry.delivery = "preloaded-callout".into();
            entry.information_only = true;
        }
    }
    Json(json!({"accepted":true,"state":snapshot(&state.session,&state.settings)})).into_response()
}
pub async fn observe(
    AxumState(shared): AxumState<Shared>,
    Json(input): Json<Observation>,
) -> Response {
    let mut s = shared.write().await;
    let profile = input.profile.unwrap_or_else(|| s.crew.profile.clone());
    if s.crew.aircraft != input.aircraft || s.crew.profile != profile {
        s.crew.revision = s.crew.revision.wrapping_add(1);
        s.crew.social.clear();
        s.request_memory.forget_crew();
        s.crew.checklist = None;
        s.crew.batch = None;
        s.session.crew_actions.clear();
    }
    s.crew.aircraft = input.aircraft;
    s.session.crew_controls = profile
        .controls
        .iter()
        .map(|(id, c)| {
            let value = input.values.get(id).copied();
            let state = value.map_or_else(
                || "unknown".to_owned(),
                |value| {
                    let tolerance = c.tolerance.max(0.01);
                    if (value - c.on).abs() <= tolerance {
                        "on".to_owned()
                    } else if (value - c.off).abs() <= tolerance {
                        "off".to_owned()
                    } else {
                        "value".to_owned()
                    }
                },
            );
            openatc_core::crew::Capability {
                id: id.clone(),
                label: c.label.clone(),
                role: c.role.clone(),
                available: input.available.contains(id),
                value,
                state,
            }
        })
        .collect();
    s.crew.profile = profile;
    s.crew.values = input.values;
    s.crew.observed_at = Some(std::time::Instant::now());
    if s.crew
        .batch
        .as_ref()
        .is_some_and(|b| b.started.elapsed().as_secs() >= 90)
    {
        let actions = std::mem::take(&mut s.session.crew_actions);
        if let Some(mut batch) = s.crew.batch.take() {
            for action in &actions {
                s.request_memory.confirm(action.sequence, false);
                let item = say(&format!("crew_control_{}", action.control), &[]);
                batch
                    .outcomes
                    .push(say("crew_completed_unconfirmed", &[("item", item)]));
            }
            if let Some(action) = actions.first() {
                let text = batch_reply(&mut s, batch);
                s.request_memory.reply_for_action(action.sequence, &text);
                speak(&mut s, &action.role, &text);
            }
        }
    }
    Json(json!({"accepted":true})).into_response()
}
fn speaker(role: &str) -> &'static str {
    match role {
        "cabin" => "ATTENDANT",
        "ground" => "GROUND",
        _ => "COPILOT",
    }
}
pub(crate) fn speak(s: &mut crate::EngineState, role: &str, text: &str) {
    let voice = match role {
        "cabin" => &s.settings.attendant_voice,
        "ground" => &s.settings.ground_voice,
        _ => &s.settings.copilot_voice,
    }
    .clone();
    add_transmission(
        &mut s.session,
        speaker(role),
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
        || (id == "gear" && (value - c.off).abs() <= f64::EPSILON && t.on_ground)
        || s.crew
            .observed_at
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(3))
        || s.session.crew_actions.len() >= 24
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
    s.request_memory.confirm(action.sequence, ack.success);
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
        let text = if s.crew.batch.is_some() {
            let key = format!(
                "crew_completed_{}_{}",
                action.control,
                if action.value == 0.0 { "off" } else { "on" }
            );
            if ["chocks", "external_power", "high_pressure_air"].contains(&action.control.as_str())
            {
                say(&key, &[])
            } else if !openatc_core::dialogue::phrases(&format!(
                "crew_completed_{}_value",
                action.control
            ))
            .is_empty()
            {
                say(
                    &format!("crew_completed_{}_value", action.control),
                    &[("value", action.value.to_string())],
                )
            } else if action.control == "slides" || action.control == "pushback_start" {
                say(template, &[]).trim_end_matches('.').to_owned()
            } else {
                say(
                    "crew_completed_control",
                    &[("item", label), ("state", action.response)],
                )
            }
        } else {
            say(template, &[("item", label), ("state", action.response)])
        };
        if let Some(batch) = s.crew.batch.as_mut() {
            batch.outcomes.push(text);
        } else {
            s.request_memory.reply_for_action(action.sequence, &text);
            speak(&mut s, &action.role, &text);
        }
        if let Some((_, index, true)) = s.crew.checklist.as_mut() {
            *index += 1;
            return next_item(&mut s);
        }
    } else {
        eprintln!("Crew action {} failed: {}", action.control, ack.detail);
        if let Some(batch) = s.crew.batch.as_mut() {
            batch
                .outcomes
                .push(say("crew_completed_unconfirmed", &[("item", label)]));
        } else {
            s.session.crew_actions.clear();
            s.crew.checklist = None;
            let text = say("crew_action_failed", &[("item", label)]);
            s.request_memory.reply_for_action(action.sequence, &text);
            speak(&mut s, &action.role, &text);
        }
    }
    if s.session.crew_actions.is_empty()
        && let Some(batch) = s.crew.batch.take()
    {
        let text = batch_reply(&mut s, batch);
        s.request_memory.reply_for_action(action.sequence, &text);
        speak(&mut s, &action.role, &text);
    }
    Json(json!({"accepted":true,"state":snapshot(&s.session,&s.settings)})).into_response()
}

pub(crate) async fn narrate(
    shared: &Shared,
    role: &str,
    request: &str,
    fallback: &str,
    protected: &[String],
) -> String {
    compose(shared, role, request, fallback, protected).await
}
fn crew_humour_allowed(state: &crate::EngineState) -> bool {
    !state.cabin.preparing_for_turbulence
        && state.session.taxi_clearance.crossing_runway.is_empty()
        && !matches!(
            state.session.phase,
            openatc_core::state::PhaseCode::Departure
                | openatc_core::state::PhaseCode::Approach
                | openatc_core::state::PhaseCode::Landed
        )
        && state.session.mission.incident.is_empty()
}

fn flight_situation(state: &crate::EngineState) -> &str {
    let key = if !state.session.telemetry.on_ground {
        "crew_situation_airborne"
    } else if matches!(
        state.session.phase,
        openatc_core::state::PhaseCode::Landed
            | openatc_core::state::PhaseCode::TaxiIn
            | openatc_core::state::PhaseCode::Finished
    ) {
        "crew_situation_after_landing"
    } else {
        "crew_situation_before_departure"
    };
    state.prompts.guidance.get(key)
}

#[allow(clippy::items_after_statements)]
async fn converse(shared: &Shared, role: &str, request: &str, points: &[String]) -> Option<String> {
    let state = shared.read().await;
    let role_prompt = match role {
        "ground" => &state.prompts.ground,
        "cabin" => &state.prompts.cabin,
        _ => &state.prompts.copilot_chat,
    };
    let personality = match role {
        "ground" => &state.settings.ground_personality,
        "cabin" => &state.settings.attendant_personality,
        _ => &state.settings.copilot_personality,
    };
    let instructions = [
        crate::support::fill_prompt(role_prompt, &state.session.plan.callsign),
        personality.clone(),
        state
            .prompts
            .guidance
            .get("crew_conversation_rules")
            .to_owned(),
        state.prompts.guidance.get("crew_dialogue").to_owned(),
    ]
    .join("\n");
    let request_context = state.request_memory.context(role);
    let recent_records = request_context["recentRequests"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|record| {
            record["request"]
                .as_str()
                .is_some_and(|text| text != request)
        })
        .take(6)
        .collect::<Vec<_>>();
    let recent = recent_records
        .iter()
        .filter_map(|record| record["request"].as_str())
        .map(|text| text.chars().take(600).collect::<String>())
        .collect::<Vec<_>>();
    let mut history = recent_records.iter().rev().flat_map(|record| {
        let mut exchange=vec![json!({"role":"user","content":record["request"].as_str().unwrap_or_default().chars().take(600).collect::<String>()})];
        if let Some(reply)=record["reply"].as_str().filter(|reply|!reply.is_empty()) {
            exchange.push(json!({"role":"assistant","content":reply.chars().take(600).collect::<String>()}));
        }
        exchange
    }).collect::<Vec<_>>();
    let mut sources = recent.clone();
    sources.push(request.to_owned());
    let fresh = state
        .crew
        .observed_at
        .is_some_and(|at| at.elapsed().as_secs() < 5);
    let words = request
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.chars().count() > 2)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut readings = state
        .session
        .crew_controls
        .iter()
        .filter(|control| control.role == role)
        .filter(|control| {
            !state
                .crew
                .profile
                .controls
                .get(&control.id)
                .is_some_and(|control| control.momentary)
        })
        .collect::<Vec<_>>();
    readings.sort_by_key(|control| {
        std::cmp::Reverse(
            words
                .iter()
                .filter(|word| {
                    format!("{} {}", control.id, control.label)
                        .to_lowercase()
                        .contains(word.as_str())
                })
                .count(),
        )
    });
    let controls = readings.into_iter().filter(|_|points.is_empty()).filter(|control|words.iter().any(|word|format!("{} {}",control.id,control.label).to_lowercase().contains(word.as_str())))
        .take(16)
        .map(|control|json!({"control":control.id,"label":control.label,"value":if fresh{state.crew.values.get(&control.id)}else{None}}))
        .collect::<Vec<_>>();
    let humour_allowed = crew_humour_allowed(&state);
    let mut facts = json!({"role":role,"humourAllowed":humour_allowed,"phase":format!("{:?}",state.session.phase),"onGround":state.session.telemetry.on_ground,
        "currentSituation":flight_situation(&state),
        "passengers":state.session.plan.passengers,"groundSpeedKnots":state.session.telemetry.ground_speed_knots,
        "departure":state.session.plan.departure,"destination":state.session.plan.destination,
        "estimatedMinutes":(state.session.plan.estimated_minutes.is_finite() && state.session.plan.estimated_minutes>0.0).then_some(state.session.plan.estimated_minutes),
        "durationNote":if state.session.plan.estimated_minutes.is_finite() && state.session.plan.estimated_minutes>0.0 {""} else {state.prompts.guidance.get("crew_duration_unknown")},
        "recentCaptainMessagesNewestFirst":[],"social":state.crew.social.context(role,request),
        "observedControls":controls,"controlsFresh":fresh});
    facts["social"]["recentCrewRepliesNewestFirst"] = json!([]);
    facts["social"]["recentJokesNewestFirst"] = json!([]);
    let generation = state.session_generation;
    let revision = state.crew.revision;
    let aircraft = state.crew.aircraft.clone();
    let client = state.client.clone();
    let settings = state.settings.clone();
    let language_path = state.config_dir.join("cabin/response-policy.toml");
    let pacing = state.prompts.guidance.get("crew_follow_up").to_owned();
    let previous = state
        .session
        .transcript
        .iter()
        .rev()
        .find(|entry| entry.speaker == speaker(role))
        .map(|entry| entry.text.clone())
        .unwrap_or_default();
    drop(state);
    let language = openatc_core::crew::request::ReplyPolicy::load(&language_path).ok()?;
    let spoken_request = if points.is_empty() {
        request.to_owned()
    } else {
        points.join(" ")
    };
    let mut prompt = format!("{instructions}\n{facts}");
    while crate::llm::prompt_fits_history(
        &client,
        &settings.ai_url,
        &prompt,
        &spoken_request,
        &history,
    )
    .await
        == Some(false)
    {
        let replies = facts["social"]["recentCrewRepliesNewestFirst"]
            .as_array_mut()
            .unwrap();
        if !history.is_empty() {
            let count = history
                .iter()
                .skip(1)
                .position(|message| message["role"] == "user")
                .map_or(history.len(), |index| index + 1);
            history.drain(..count);
        } else if !replies.is_empty() {
            replies.pop();
        } else if facts["social"]["recentJokesNewestFirst"]
            .as_array()
            .is_some_and(|jokes| !jokes.is_empty())
        {
            facts["social"]["recentJokesNewestFirst"]
                .as_array_mut()
                .unwrap()
                .pop();
        } else if facts["recentCaptainMessagesNewestFirst"]
            .as_array()
            .is_some_and(|recent| recent.len() > 1)
        {
            facts["recentCaptainMessagesNewestFirst"]
                .as_array_mut()
                .unwrap()
                .pop();
        } else if facts["social"]["captainQuotesNewestFirst"]
            .as_array()
            .is_some_and(|quotes| quotes.len() > 1)
        {
            facts["social"]["captainQuotesNewestFirst"]
                .as_array_mut()
                .unwrap()
                .pop();
        } else {
            return None;
        }
        prompt = format!("{instructions}\n{facts}");
    }
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Dialogue {
        reply: String,
        #[serde(default)]
        joke: String,
        #[serde(default)]
        remember: Vec<String>,
    }
    let schema = json!({"type":"object","additionalProperties":false,"required":["reply","joke"],
        "properties":{"reply":{"type":"string"},"joke":{"type":"string"}}});
    let mut result = None;
    let mut preserved_reply = None;
    for attempt in 0..2 {
        // The retry includes the rejected draft and must fit the same server context.
        if attempt > 0
            && crate::llm::prompt_fits_history(
                &client,
                &settings.ai_url,
                &prompt,
                &spoken_request,
                &history,
            )
            .await
                == Some(false)
        {
            return None;
        }
        let mut candidate = match crate::llm::structured_history::<Dialogue>(
            &client,
            &settings.ai_url,
            &settings.ai_model,
            if settings.llm_phrase_variety {
                0.6
            } else {
                0.2
            },
            &prompt,
            &spoken_request,
            &history,
            Some(schema.clone()),
        )
        .await
        {
            Ok(candidate) => candidate,
            Err(error) => {
                eprintln!("Crew dialogue failed: {error}");
                return None;
            }
        };
        if std::env::var_os("OPENATC_ACTOR_TRACE").is_some() {
            eprintln!("Crew dialogue: {candidate:?}");
        }
        if !humour_allowed {
            candidate.joke.clear();
        }
        for sentence in conversation_sentences(&candidate.joke) {
            candidate.reply = candidate.reply.replace(sentence, "");
        }
        candidate.reply = candidate.reply.trim().to_owned();
        if !candidate.reply.trim().is_empty() {
            crate::llm::spoken(&candidate.reply).ok()?;
        }
        if !candidate.joke.trim().is_empty() {
            crate::llm::spoken(&candidate.joke).ok()?;
        }
        if !language.narration_allowed(&candidate.reply)
            || !language.narration_allowed(&candidate.joke)
        {
            return None;
        }
        let repeated = !candidate.joke.is_empty()
            && shared
                .read()
                .await
                .crew
                .social
                .repeated_joke(&candidate.joke);
        if repeated && attempt == 0 {
            preserved_reply = Some(candidate.reply.clone());
            let state = shared.read().await;
            prompt.push('\n');
            prompt.push_str(state.prompts.guidance.get("crew_dialogue_retry"));
            prompt.push_str(
                &json!({"rejectedJoke":candidate.joke,"retainReply":candidate.reply}).to_string(),
            );
            continue;
        }
        if repeated {
            candidate.joke.clear();
        }
        if let Some(reply) = preserved_reply.take() {
            candidate.reply = reply;
        }
        result = Some(candidate);
        break;
    }
    let result = result?;
    let complete =
        openatc_core::dialogue::speech_text(&format!("{} {}", result.reply, result.joke));
    crate::llm::spoken(&complete).ok()?;
    let joke = (!result.joke.is_empty()).then_some(result.joke.as_str());
    let reply = pace_crew_reply_with_joke(
        &client, &settings, &pacing, request, &complete, &previous, joke,
    )
    .await;
    {
        let state = shared.read().await;
        if state.session_generation != generation
            || state.crew.aircraft != aircraft
            || state.crew.revision != revision
        {
            return None;
        }
    }
    crate::llm::spoken(&reply).ok()?;
    let mut state = shared.write().await;
    if state.session_generation != generation
        || state.crew.aircraft != aircraft
        || state.crew.revision != revision
    {
        return None;
    }
    // Retain original captain wording, never generated summaries of personal facts.
    let quoted_message = if points.is_empty() {
        request.to_owned()
    } else {
        points.join(" ")
    };
    let quotes = conversation_sentences(&quoted_message)
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    state.crew.social.remember(role, &sources, &quotes);
    state.crew.social.remember(role, &sources, &result.remember);
    state.crew.social.record_reply(role, &reply);
    state.crew.social.record_joke(&result.joke);
    Some(reply)
}

#[allow(clippy::items_after_statements)]
async fn conversation_points(shared: &Shared, role: &str, request: &str) -> Option<Vec<String>> {
    let state = shared.read().await;
    let prompt = state
        .prompts
        .guidance
        .get("crew_dialogue_points")
        .to_owned();
    let settings = state.settings.clone();
    let client = state.client.clone();
    drop(state);
    let user = json!({"role":role,"captainMessage":request}).to_string();
    if crate::llm::prompt_fits(&client, &settings.ai_url, &prompt, &user).await == Some(false) {
        return None;
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Points {
        points: Vec<String>,
        #[serde(default, rename = "operational")]
        _operational: Vec<String>,
    }
    let schema = json!({"type":"object","additionalProperties":false,"required":["points","operational"],"properties":{
        "points":{"type":"array","maxItems":16,"items":{"type":"string"}},
        "operational":{"type":"array","maxItems":16,"items":{"type":"string"}}}});
    let points = crate::llm::structured_format::<Points>(
        &client,
        &settings.ai_url,
        &settings.ai_model,
        0.0,
        &prompt,
        &user,
        Some(schema),
    )
    .await
    .ok()?;
    if std::env::var_os("OPENATC_ACTOR_TRACE").is_some() {
        eprintln!("Crew conversational excerpts: {:?}", points.points);
    }
    let source = request.to_lowercase();
    Some(
        points
            .points
            .into_iter()
            .filter(|point| !point.trim().is_empty() && source.contains(&point.to_lowercase()))
            .collect(),
    )
}

fn conversation_sentences(text: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;
    for (index, character) in text.char_indices() {
        let end = index + character.len_utf8();
        if matches!(character, '.' | '!' | '?')
            && text[end..].chars().next().is_none_or(char::is_whitespace)
        {
            if !text[start..end].trim().is_empty() {
                sentences.push(text[start..end].trim());
            }
            start = end;
        }
    }
    if !text[start..].trim().is_empty() {
        sentences.push(text[start..].trim());
    }
    sentences
}

async fn compose(
    shared: &Shared,
    role: &str,
    request: &str,
    fallback: &str,
    protected: &[String],
) -> String {
    let s = shared.read().await;
    if !s.settings.ai_enabled || s.settings.ai_model.is_empty() {
        return fallback.to_owned();
    }
    let personality = match role {
        "cabin" => &s.settings.attendant_personality,
        "ground" => &s.settings.ground_personality,
        _ => &s.settings.copilot_personality,
    };
    let role_prompt = match role {
        "cabin" => &s.prompts.cabin,
        "ground" => &s.prompts.ground,
        _ => &s.prompts.copilot_chat,
    };
    let role_prompt = crate::support::fill_prompt(role_prompt, &s.session.plan.callsign);
    let voice_role = if role == "cabin" { "attendant" } else { role };
    let examples = s
        .speech
        .select(
            voice_role,
            &["actor_conversation"],
            &format!("{:?}", s.session.phase).to_lowercase(),
            "icao",
            4,
        )
        .into_iter()
        .flat_map(|example| example.say.iter().take(2).cloned().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let mut readings = s
        .session
        .crew_controls
        .iter()
        .filter(|control| control.role == role)
        .collect::<Vec<_>>();
    readings.sort_by_key(|control| {
        s.crew
            .profile
            .controls
            .get(&control.id)
            .is_some_and(|profile| profile.momentary)
    });
    let controls=readings.into_iter().take(24).map(|control|json!({"id":control.id,"available":control.available,"value":s.crew.values.get(&control.id)})).collect::<Vec<_>>();
    let context=json!({"phase":format!("{:?}",s.session.phase),"onGround":s.session.telemetry.on_ground,"currentSituation":flight_situation(&s),"groundSpeedKnots":s.session.telemetry.ground_speed_knots,
        "departure":s.session.plan.departure,"destination":s.session.plan.destination,
        "estimatedMinutes":(s.session.plan.estimated_minutes.is_finite() && s.session.plan.estimated_minutes>0.0).then_some(s.session.plan.estimated_minutes),
        "durationNote":if s.session.plan.estimated_minutes.is_finite() && s.session.plan.estimated_minutes>0.0 {""} else {s.prompts.guidance.get("crew_duration_unknown")},
        "cabin":s.cabin,"controls":controls,
        "missionIncident":s.session.mission.incident,"humourAllowed":crew_humour_allowed(&s)}).to_string();
    let system = crate::llm::prompt(
        &[
            role_prompt.as_str(),
            personality.as_str(),
            s.prompts.guidance.get("crew_conversation_rules"),
            &s.prompts.crew_response,
        ]
        .join("\n"),
        s.prompts.guidance.get("examples"),
        &examples,
        &context,
    );
    let pacing_prompt = s.prompts.guidance.get("crew_follow_up").to_owned();
    let previous_reply = s
        .session
        .transcript
        .iter()
        .rev()
        .find(|entry| entry.speaker == speaker(role))
        .map(|entry| entry.text.clone())
        .unwrap_or_default();
    let generation = s.session_generation;
    let aircraft = s.crew.aircraft.clone();
    let client = s.client.clone();
    let settings = s.settings.clone();
    let language_path = s.config_dir.join("cabin/response-policy.toml");
    drop(s);
    let language = openatc_core::crew::request::ReplyPolicy::load(&language_path).ok();
    let user = if protected.is_empty() {
        request.to_owned()
    } else {
        json!({"confirmedResult":fallback}).to_string()
    };
    if crate::llm::prompt_fits(&client, &settings.ai_url, &system, &user).await == Some(false) {
        eprintln!("Crew response exceeds the configured context; retaining verified outcomes");
        return fallback.to_owned();
    }
    let response = crate::llm::narration(
        &client,
        &settings.ai_url,
        &settings.ai_model,
        if settings.llm_phrase_variety {
            0.6
        } else {
            0.2
        },
        &system,
        &user,
    )
    .await;
    let Ok(reply) = response else {
        eprintln!(
            "Crew response could not be composed: {}",
            response.unwrap_err()
        );
        return fallback.to_owned();
    };
    let reply = pace_crew_reply(
        &client,
        &settings,
        &pacing_prompt,
        request,
        &reply,
        &previous_reply,
    )
    .await;
    if valid_narration(&reply, fallback, protected)
        && language
            .as_ref()
            .is_some_and(|policy| policy.narration_allowed(&reply))
    {
        let state = shared.read().await;
        if state.session_generation != generation || state.crew.aircraft != aircraft {
            return fallback.to_owned();
        }
        reply.trim().to_owned()
    } else {
        eprintln!("Crew response did not preserve confirmed outcomes");
        fallback.to_owned()
    }
}

fn valid_narration(reply: &str, fallback: &str, protected: &[String]) -> bool {
    let lower = reply.to_lowercase();
    let failed = fallback.to_lowercase();
    if lower.matches("captain").count() > 1
        || reply.contains("?.")
        || reply.contains("!.")
        || reply.trim().is_empty()
        || reply.len() > 1200
        || [
            "simulat",
            "plugin",
            "language model",
            "artificial intelligence",
        ]
        .iter()
        .any(|word| lower.contains(word))
        || ((failed.contains("unconfirmed")
            || failed.contains("not confirm")
            || failed.contains("could not")
            || failed.contains("couldn’t")
            || failed.contains("can’t confirm"))
            && ![
                "not",
                "can't",
                "can’t",
                "cannot",
                "couldn",
                "unable",
                "haven’t",
                "haven't",
            ]
            .iter()
            .any(|word| lower.contains(word)))
    {
        return false;
    }
    let outcomes = protected.join(" ").to_lowercase();
    if [
        "heading", "altitude", "chocks", "power", "lights", "gear", "flaps", "speed",
    ]
    .iter()
    .any(|term| outcomes.contains(term) && !lower.contains(term))
        || (outcomes.contains("slides") && !lower.contains("slides") && !lower.contains("doors"))
    {
        return false;
    }
    let numbers = |text: &str| {
        text.split(|c: char| !c.is_ascii_digit() && c != '.')
            .filter_map(|part| part.trim_matches('.').parse::<f64>().ok())
            .collect::<Vec<_>>()
    };
    let actual = numbers(reply);
    if numbers(outcomes.as_str())
        .iter()
        .any(|number| !actual.contains(number))
    {
        return false;
    }
    true
}

fn batch_reply(_s: &mut crate::EngineState, batch: Batch) -> String {
    let mut text = String::new();
    if let Some((last, rest)) = batch.outcomes.split_last() {
        let items = if rest.is_empty() {
            last.clone()
        } else {
            say(
                "crew_completed_list",
                &[("first", rest.join(", ")), ("last", last.clone())],
            )
        };
        text = if batch.held_reply.trim().is_empty() {
            say("crew_batch_complete", &[("items", items)])
        } else {
            say(
                "crew_batch_with_conversation",
                &[
                    ("items", items.trim_end_matches('.').to_owned()),
                    ("reply", batch.held_reply),
                ],
            )
        };
        if !text.ends_with(['.', '?', '!']) {
            text.push('.');
        }
    }

    text
}

fn cross_check(s: &crate::EngineState) -> &'static str {
    match s.crew.values.get("slides").copied() {
        Some(1.0) => "crew_slides_armed_checked",
        Some(0.0) => "crew_slides_disarmed_checked",
        _ => "crew_slides_not_confirmed",
    }
}

/// The ramp headset is available during stand work and pushback, not during taxi or flight.
pub fn update_ground_connection(state: &mut crate::EngineState) {
    use openatc_core::state::PhaseCode;
    let t = &state.session.telemetry;
    if !t.on_ground {
        state.session.ground_interphone_disconnected = true;
    }
    if t.on_ground && (!t.position_valid || t.paused) {
        return;
    }
    let (knots, meters) = state.cabin.ground_disconnect_limits();
    if t.on_ground
        && state.session.phase == PhaseCode::Finished
        && t.ground_speed_knots < knots
        && state.session.taxi_clearance.to_parking
        && state.session.taxi_clearance.guidance_complete
    {
        state.session.ground_interphone_disconnected = false;
        state.crew.ground_taxi_origin = None;
    } else if matches!(
        state.session.phase,
        PhaseCode::Taxi | PhaseCode::TaxiIn | PhaseCode::Departure
    ) {
        let origin = *state
            .crew
            .ground_taxi_origin
            .get_or_insert([t.latitude, t.longitude]);
        if t.ground_speed_knots >= knots
            && openatc_core::arrival::distance_nm(origin, [t.latitude, t.longitude]) * 1852.0
                >= meters
        {
            state.session.ground_interphone_disconnected = true;
        }
    } else {
        state.crew.ground_taxi_origin = None;
    }
    if state.session.ground_interphone_disconnected
        && state
            .session
            .crew_actions
            .iter()
            .any(|action| action.role == "ground")
    {
        state
            .session
            .crew_actions
            .retain(|action| action.role != "ground");
        state.crew.batch = None;
        state.crew.pushback = None;
    }
}

pub async fn request(shared: &Shared, request: &Request) -> Option<Response> {
    let role = request.role.as_str();
    if !["copilot", "cabin", "ground"].contains(&role) {
        return None;
    }
    let text = request.text.to_lowercase();
    let mut s = shared.write().await;
    update_ground_connection(&mut s);
    if role == "ground" && s.session.ground_interphone_disconnected {
        return Some(Json(json!({"result":{"accepted":false,"silent":true,"notice":say("crew_ground_interphone_disconnected",&[])},"state":snapshot(&s.session,&s.settings)})).into_response());
    }
    if role == "cabin"
        && let Some(text) = crate::missions::respond(&mut s, &request.text)
    {
        let pilot = s.session.plan.callsign.clone();
        add_transmission(&mut s.session, &pilot, &request.text, &SpeechTag::default());
        speak(&mut s, role, &text);
        return Some(Json(json!({"result":{"accepted":true,"message":text},"state":snapshot(&s.session,&s.settings)})).into_response());
    }
    let pilot = s.session.plan.callsign.clone();
    let mut pilot_sequence = s.session.next_sequence;
    let using_ai = s.settings.ai_enabled && !s.settings.ai_model.is_empty();
    if using_ai || !request.controls.is_empty() {
        add_transmission(&mut s.session, &pilot, &request.text, &SpeechTag::default());
    }
    let profile = s.crew.profile.clone();
    let parsed = if !request.controls.is_empty() {
        let mut actions = Vec::new();
        let mut resolved = true;
        for command in &request.controls {
            let Some(control) = profile.controls.get(&command.control) else {
                resolved = false;
                break;
            };
            let value = match command.value.as_str() {
                "on" => control.on,
                "off" => control.off,
                other => match other.parse::<f64>() {
                    Ok(value) => value,
                    Err(_) => {
                        resolved = false;
                        break;
                    }
                },
            };
            actions.push(openatc_core::crew::request::ControlAction {
                control: command.control.clone(),
                value,
            });
        }
        let intent = openatc_core::crew::request::Intent {
            actions,
            ..Default::default()
        };
        (resolved && intent.valid(&profile, role)).then_some(intent)
    } else if s.settings.ai_enabled && !s.settings.ai_model.is_empty() {
        let generation = s.session_generation;
        let revision = s.crew.revision;
        let aircraft = s.crew.aircraft.clone();
        let settings = s.settings.clone();
        let client = s.client.clone();
        let interpretation_context = s.prompts.guidance.get("crew_interpret_context").to_owned();
        let authority_prompt = s.prompts.guidance.get("crew_control_extract").to_owned();
        let proposal_prompt = s.prompts.guidance.get("crew_control_authority").to_owned();
        let role_guidance = s
            .prompts
            .guidance
            .get(&format!("crew_actions_{role}"))
            .to_owned();
        let instructions = s.prompts.crew_actions.replace("{{role}}", role);
        let query = request.text.to_lowercase();
        let words: Vec<_> = query
            .split(|character: char| !character.is_alphanumeric())
            .filter(|word| word.len() >= 3)
            .collect();
        let mut candidates: Vec<_> = profile
            .controls
            .iter()
            .filter(|(_, control)| control.role == role)
            .collect();
        candidates.sort_by_key(|(id, control)| {
            let label = format!("{id} {}", control.label).to_lowercase();
            (
                control.momentary,
                std::cmp::Reverse(words.iter().filter(|word| label.contains(**word)).count()),
            )
        });
        let mut allowed: Vec<_> = candidates.into_iter().take(64)
            .map(|(id,c)| {
                let mut control = json!({"control":id,"label":c.label,"min":c.min,"max":c.max,"on":c.on,"off":c.off,"states":c.states,"value":s.crew.values.get(id),"momentary":c.momentary});
                if !c.instructions.is_empty() { control["instructions"] = json!(c.instructions); }
                control
            }).collect();
        let humour_allowed = !s.cabin.preparing_for_turbulence
            && s.session.taxi_clearance.crossing_runway.is_empty()
            && !matches!(
                s.session.phase,
                openatc_core::state::PhaseCode::Departure
                    | openatc_core::state::PhaseCode::Approach
                    | openatc_core::state::PhaseCode::Landed
            )
            && s.session.mission.incident.is_empty();
        let facts = json!({"recipient":role,"onGround":s.session.telemetry.on_ground,"phase":format!("{:?}",s.session.phase),
            "passengers":s.session.plan.passengers,"humourAllowed":humour_allowed,
            "flight":{"departure":s.session.plan.departure,"destination":s.session.plan.destination}});
        let mut prompt = format!(
            "{instructions}\n{}\n{role_guidance}",
            interpretation_context
                .replace("{{controls}}", &json!(allowed).to_string())
                .replace("{{facts}}", &facts.to_string())
        );
        drop(s);
        while crate::llm::prompt_fits(&client, &settings.ai_url, &prompt, &request.text).await
            == Some(false)
        {
            if allowed.len() > 8 {
                allowed.truncate(allowed.len() / 2);
            } else {
                let mut state = shared.write().await;
                return Some(reply(&mut state, role, "crew_action_clarify", &[], false));
            }
            prompt = format!(
                "{instructions}\n{}\n{role_guidance}",
                interpretation_context
                    .replace("{{controls}}", &json!(allowed).to_string())
                    .replace("{{facts}}", &facts.to_string())
            );
        }
        let control_schema = if allowed.is_empty() {
            json!({"type":"string"})
        } else {
            json!({"type":"string","enum":allowed.iter().filter_map(|control|control["control"].as_str()).collect::<Vec<_>>()})
        };
        let schema = json!({"type":"object","additionalProperties":false,
        "required":["actions","cross_check"],"properties":{
            "cross_check":{"type":"boolean"},
            "actions":{"type":"array","maxItems":8,"items":{"type":"object","additionalProperties":false,"required":["control","value"],
                "properties":{"control":control_schema,"value":{"type":"number"}}}}
        }});
        let intent = crate::llm::structured_format::<openatc_core::crew::request::Intent>(
            &client,
            &settings.ai_url,
            &settings.ai_model,
            if settings.llm_phrase_variety {
                0.6
            } else {
                0.2
            },
            &prompt,
            &request.text,
            Some(schema),
        )
        .await
        .inspect_err(|error| eprintln!("Crew interpretation failed for {role}: {error}"))
        .ok()
        .map(|mut intent| {
            intent.actions.extend(
                std::mem::take(&mut intent.control_targets)
                    .into_iter()
                    .filter_map(|(control, value)| {
                        value.map(|value| openatc_core::crew::request::ControlAction {
                            control,
                            value,
                        })
                    }),
            );
            // Repeating an identical proposal must not discard the rest of a mixed request.
            let mut unique = Vec::new();
            for action in std::mem::take(&mut intent.actions) {
                if !unique
                    .iter()
                    .any(|previous: &openatc_core::crew::request::ControlAction| {
                        previous.control == action.control
                            && (previous.value - action.value).abs() <= f64::EPSILON
                    })
                {
                    unique.push(action);
                }
            }
            intent.actions = unique;
            intent
                .conversation_points
                .retain(|point| !point.trim().is_empty() && request.text.contains(point.as_str()));
            intent.conversation_only = intent.actions.is_empty() && !intent.cross_check;
            intent
        });

        if std::env::var_os("OPENATC_ACTOR_TRACE").is_some() {
            eprintln!("Crew interpretation: {intent:?}");
        }
        let intent = intent.filter(|intent| {
            intent.conversation_points.len() <= 16
                && intent
                    .conversation_points
                    .iter()
                    .map(String::len)
                    .sum::<usize>()
                    <= 8192
        });
        let intent = if let Some(mut intent) = intent {
            let properties = allowed
                .iter()
                .filter_map(|control| control["control"].as_str())
                .map(|id| (id.to_owned(), json!({"type":["number","null"]})))
                .collect::<serde_json::Map<_, _>>();
            let schema = json!({"type":"object","additionalProperties":false,
                "required":["control_targets","cross_check"],"properties":{
                    "control_targets":{"type":"object","additionalProperties":false,
                        "required":properties.keys().collect::<Vec<_>>(),"properties":properties},
                    "cross_check":{"type":"boolean"}}});
            let context =
                json!({"recipient":role,"captainMessage":request.text,"allowedControls":allowed})
                    .to_string();
            match crate::llm::structured_format::<openatc_core::crew::request::Intent>(
                &client,
                &settings.ai_url,
                &settings.ai_model,
                0.0,
                &authority_prompt,
                &context,
                Some(schema),
            )
            .await
            {
                Ok(mut controls) => {
                    if std::env::var_os("OPENATC_ACTOR_TRACE").is_some() {
                        eprintln!("Crew control targets: {controls:?}");
                    }
                    controls.actions.extend(
                        std::mem::take(&mut controls.control_targets)
                            .into_iter()
                            .filter_map(|(control, value)| {
                                value.map(|value| openatc_core::crew::request::ControlAction {
                                    control,
                                    value,
                                })
                            }),
                    );
                    let mut candidates = Vec::new();
                    for action in intent.actions.iter().chain(controls.actions.iter()) {
                        if openatc_core::crew::validate(
                            &profile,
                            role,
                            &action.control,
                            action.value,
                        ) && !candidates.iter().any(
                            |previous: &&openatc_core::crew::request::ControlAction| {
                                previous.control == action.control
                                    && (previous.value - action.value).abs() <= f64::EPSILON
                            },
                        ) {
                            candidates.push(action);
                        }
                    }
                    // Independent current-message interpretations can agree on a target; disagreements need quoted evidence.
                    let agreed = candidates
                        .iter()
                        .filter(|action| {
                            intent.actions.iter().any(|proposal| {
                                proposal.control == action.control
                                    && (proposal.value - action.value).abs() <= f64::EPSILON
                            }) && controls.actions.iter().any(|proposal| {
                                proposal.control == action.control
                                    && (proposal.value - action.value).abs() <= f64::EPSILON
                            })
                        })
                        .copied()
                        .collect::<Vec<_>>();
                    let disputed = candidates
                        .iter()
                        .filter(|action| {
                            !agreed.iter().any(|agreed| {
                                agreed.control == action.control
                                    && (agreed.value - action.value).abs() <= f64::EPSILON
                            })
                        })
                        .copied()
                        .collect::<Vec<_>>();
                    let mut approved_actions = agreed
                        .into_iter()
                        .map(|action| openatc_core::crew::request::ControlAction {
                            control: action.control.clone(),
                            value: action.value,
                        })
                        .collect::<Vec<_>>();
                    let check_agreed = intent.cross_check && controls.cross_check;
                    let check_disputed = intent.cross_check != controls.cross_check;
                    controls.cross_check = check_agreed;
                    if !disputed.is_empty() || check_disputed {
                        let mut targets=disputed.iter().enumerate().map(|(index,action)|{
                            let control=&profile.controls[&action.control];
                            let meaning=control.states.iter().find(|(_,value)|(**value-action.value).abs()<0.001).map_or_else(||{
                                    if (action.value - control.off).abs() <= f64::EPSILON {say("crew_state_off",&[])}
                                    else if (action.value - control.on).abs() <= f64::EPSILON {say("crew_state_on",&[])}
                                    else {format!("{}",action.value)}
                                }, |(state,_)|state.clone());
                            (format!("candidate_{index}"),json!({"control":action.control,
                                "proposedOperation":format!("{}: {}",control.label,meaning),"value":action.value,
                                "instructions":control.instructions}))
                        }).collect::<serde_json::Map<_,_>>();
                        if check_disputed {
                            targets.insert("@cross_check".into(),json!({"control":"@cross_check","proposedOperation":"@cross_check"}));
                        }
                        let properties=targets.keys().map(|id|(id.clone(),json!({"type":"object","additionalProperties":false,
                            "required":["evidence","requested"],"properties":{"evidence":{"type":"string"},"requested":{"type":"boolean"}}}))).collect::<serde_json::Map<_,_>>();
                        let schema = json!({"type":"object","additionalProperties":false,"required":properties.keys().collect::<Vec<_>>(),"properties":properties});
                        let current =
                            json!({"captainMessage":request.text,"targets":targets}).to_string();
                        if crate::llm::prompt_fits(
                            &client,
                            &settings.ai_url,
                            &proposal_prompt,
                            &current,
                        )
                        .await
                            == Some(false)
                        {
                            let mut state = shared.write().await;
                            return Some(reply(
                                &mut state,
                                role,
                                "crew_action_clarify",
                                &[],
                                false,
                            ));
                        }
                        let approved = match crate::llm::structured_format::<
                            BTreeMap<String, ControlAuthority>,
                        >(
                            &client,
                            &settings.ai_url,
                            &settings.ai_model,
                            0.0,
                            &proposal_prompt,
                            &current,
                            Some(schema),
                        )
                        .await
                        {
                            Ok(approved) => approved,
                            Err(error) => {
                                eprintln!("Crew control authority could not be checked: {error}");
                                let mut state = shared.write().await;
                                return Some(reply(
                                    &mut state,
                                    role,
                                    "crew_action_clarify",
                                    &[],
                                    false,
                                ));
                            }
                        };
                        if std::env::var_os("OPENATC_ACTOR_TRACE").is_some() {
                            eprintln!("Crew control authority: {approved:?}");
                        }
                        for (index, action) in disputed.iter().enumerate() {
                            if approved
                                .get(&format!("candidate_{index}"))
                                .is_some_and(|authority| authority.matches(&request.text))
                            {
                                approved_actions.push(openatc_core::crew::request::ControlAction {
                                    control: action.control.clone(),
                                    value: action.value,
                                });
                            }
                        }
                        controls.cross_check = check_agreed
                            || (check_disputed
                                && approved
                                    .get("@cross_check")
                                    .is_some_and(|authority| authority.matches(&request.text)));
                    }
                    controls.actions = approved_actions;
                    if !controls.valid_controls(&profile, role) {
                        let mut state = shared.write().await;
                        return Some(reply(&mut state, role, "crew_action_clarify", &[], false));
                    }
                    intent.actions = controls.actions;
                    intent.cross_check = controls.cross_check;
                    intent.conversation_only = intent.actions.is_empty() && !intent.cross_check;
                }
                Err(error) => {
                    eprintln!("Crew control request could not be checked: {error}");
                    let mut state = shared.write().await;
                    return Some(reply(&mut state, role, "crew_action_clarify", &[], false));
                }
            }
            Some(intent)
        } else {
            None
        };
        s = shared.write().await;
        update_ground_connection(&mut s);
        if role == "ground" && s.session.ground_interphone_disconnected {
            return Some(Json(json!({"result":{"accepted":false,"silent":true,"notice":say("crew_ground_interphone_disconnected",&[])},"state":snapshot(&s.session,&s.settings)})).into_response());
        }
        if s.session_generation != generation
            || s.crew.aircraft != aircraft
            || s.crew.revision != revision
        {
            return Some(Json(json!({"result":{"accepted":false,"silent":true},"state":snapshot(&s.session,&s.settings)})).into_response());
        }
        let Some(mut intent) = intent else {
            return Some(reply(&mut s, role, "crew_action_clarify", &[], false));
        };
        if intent.actions.is_empty() && !intent.cross_check {
            drop(s);
            let response = converse(shared, role, &request.text, &intent.conversation_points).await;
            s = shared.write().await;
            if s.session_generation != generation
                || s.crew.aircraft != aircraft
                || s.crew.revision != revision
            {
                return Some(Json(json!({"result":{"accepted":false,"silent":true},"state":snapshot(&s.session,&s.settings)})).into_response());
            }
            update_ground_connection(&mut s);
            if role == "ground" && s.session.ground_interphone_disconnected {
                return Some(Json(json!({"result":{"accepted":false,"silent":true,"notice":say("crew_ground_interphone_disconnected",&[])},"state":snapshot(&s.session,&s.settings)})).into_response());
            }
            let Some(response) = response else {
                return Some(Json(json!({"result":{"accepted":false,"silent":true,"notice":say("crew_conversation_unavailable",&[])},"state":snapshot(&s.session,&s.settings)})).into_response());
            };
            intent.reply = response;
            if crate::llm::spoken(&intent.reply).is_ok() && intent.reply.len() <= 3000 {
                speak(&mut s, role, &intent.reply);
                return Some(Json(json!({"result":{"accepted":true,"message":intent.reply},"state":snapshot(&s.session,&s.settings)})).into_response());
            }
            return Some(reply(&mut s, role, "crew_action_clarify", &[], false));
        }
        {
            drop(s);
            let points = conversation_points(shared, role, &request.text)
                .await
                .unwrap_or_default();
            intent.reply = if points.is_empty() {
                String::new()
            } else {
                converse(shared, role, &request.text, &points)
                    .await
                    .unwrap_or_default()
            };
            s = shared.write().await;
            update_ground_connection(&mut s);
            if s.session_generation != generation
                || s.crew.aircraft != aircraft
                || s.crew.revision != revision
                || (role == "ground" && s.session.ground_interphone_disconnected)
            {
                return Some(Json(json!({"result":{"accepted":false,"silent":true},"state":snapshot(&s.session,&s.settings)})).into_response());
            }
        }
        Some(intent)
    } else {
        openatc_core::crew::request::parse(&profile, role, &text)
    };
    let recognized = parsed.is_some()
        || profile.controls.iter().any(|(id, c)| {
            c.role == role
                && openatc_core::dialogue::phrases(&format!("crew_request_{id}"))
                    .iter()
                    .any(|a| text.contains(a))
        })
        || [
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
    if !using_ai {
        pilot_sequence = s.session.next_sequence;
        add_transmission(&mut s.session, &pilot, &request.text, &SpeechTag::default());
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
    if parsed.is_none()
        && role == "cabin"
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
                .map_or_else(
                    || {
                        if (value - c.off).abs() <= f64::EPSILON {
                            say("crew_state_off", &[])
                        } else if (value - c.on).abs() <= f64::EPSILON {
                            say("crew_state_on", &[])
                        } else {
                            say("crew_state_value", &[("value", format!("{value:.0}"))])
                        }
                    },
                    |(name, _)| say(&format!("crew_state_{name}"), &[]),
                );
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
    if !using_ai && (text.contains("don't ") || text.contains("do not ") || text.contains("never "))
    {
        return Some(reply(&mut s, role, "crew_no_action", &[], true));
    }
    let generation = s.session_generation;
    let aircraft = s.crew.aircraft.clone();
    drop(s);
    let mut s = shared.write().await;
    if s.session_generation != generation {
        return Some(
            (
                axum::http::StatusCode::CONFLICT,
                Json(json!({"error":say("notice_flight_was_reset_previous_request_discarded_c0a5c48d", &[])})),
            )
                .into_response(),
        );
    }
    if s.crew.aircraft != aircraft {
        return Some(reply(&mut s, role, "crew_action_clarify", &[], false));
    }
    if let Some(intent) = parsed {
        if intent.conversation_only {
            s.session
                .transcript
                .retain(|entry| entry.sequence != pilot_sequence);
            let settings = s.settings.clone();
            drop(s);
            let mut conversation = request.clone();
            conversation.intent = "conversation".into();
            return Some(crate::routes::actor_chat(shared, &settings, &conversation).await);
        }
        if !s.session.crew_actions.is_empty() {
            return Some(reply(&mut s, role, "crew_actions_busy", &[], false));
        }
        let mut batch = Batch {
            outcomes: Vec::new(),
            held_reply: intent.reply.clone(),
            started: std::time::Instant::now(),
        };
        if intent.cross_check && !intent.actions.iter().any(|a| a.control == "slides") {
            batch
                .outcomes
                .push(say(cross_check(&s), &[]).trim_end_matches('.').to_owned());
        }
        for action in intent.actions {
            let (id, value) = (action.control, action.value);
            let c = profile.controls.get(&id)?;
            let state = if c.momentary {
                say("crew_state_button", &[])
            } else {
                c.states
                    .iter()
                    .find(|(_, v)| (**v - value).abs() < 0.01)
                    .map_or_else(
                        || {
                            if (value - c.off).abs() <= f64::EPSILON {
                                say("crew_state_off", &[])
                            } else if (value - c.on).abs() <= f64::EPSILON {
                                say("crew_state_on", &[])
                            } else {
                                say("crew_state_value", &[("value", format!("{value:.0}"))])
                            }
                        },
                        |(name, _)| say(&format!("crew_state_{name}"), &[]),
                    )
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
        if s.session.crew_actions.is_empty() {
            let generation = s.session_generation;
            let protected = batch.outcomes.clone();
            let message = batch_reply(&mut s, batch);
            drop(s);
            let message = narrate(shared, role, &request.text, &message, &protected).await;
            s = shared.write().await;
            if s.session_generation != generation {
                return Some(reply(&mut s, role, "crew_action_clarify", &[], false));
            }
            speak(&mut s, role, &message);
            return Some(Json(json!({"result":{"accepted":true,"message":message},"state":snapshot(&s.session,&s.settings)})).into_response());
        }
        s.crew.batch = Some(batch);
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

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod narration_tests {
    #[test]
    fn partial_control_failure_does_not_discard_held_conversation() {
        let mut state = crate::test_state();
        let text = super::batch_reply(
            &mut state,
            super::Batch {
                outcomes: vec![
                    "Slides could not be confirmed".into(),
                    "Cabin lighting set to 50 percent".into(),
                ],
                held_reply: "I'll check for a spare lobster and bring a coffee.".into(),
                started: std::time::Instant::now(),
            },
        );
        assert!(text.contains("could not be confirmed") && text.contains("50 percent"));
        assert!(text.contains("lobster") && text.contains("coffee"));
        assert!(!text.contains("Slides armed"));
    }

    #[test]
    fn generated_reply_keeps_control_outcomes_and_operational_numbers() {
        let protected = vec!["Heading 275 set".to_owned()];
        assert!(super::valid_narration(
            "Heading 275 set. You're welcome.",
            "Heading 275 set",
            &protected
        ));
        assert!(!super::valid_narration(
            "Heading 270 set.",
            "Heading 275 set",
            &protected
        ));
        assert!(!super::valid_narration(
            "You're welcome.",
            "Heading 275 set",
            &protected
        ));
        assert!(!super::valid_narration(
            "Yes, Captain, Captain, I cannot confirm the slides?.",
            "Slides could not be confirmed",
            &[]
        ));
        let failed = vec!["Slides could not be confirmed".to_owned()];
        assert!(!super::valid_narration(
            "Doors armed and checked.",
            &failed[0],
            &failed
        ));
        assert!(!super::valid_narration(
            "The simulator shows heading 275.",
            "Heading 275 set",
            &protected
        ));
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod conversation_routing_tests {
    use super::*;
    use axum::{Router, routing::post};

    #[tokio::test]
    async fn dialogue_receives_readable_current_situation_and_known_duration() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/v1/chat/completions",
                    post(|Json(body): Json<serde_json::Value>| async move {
                        let system = body["messages"][0]["content"].as_str().unwrap();
                        assert!(system.contains("\"phase\":\"TaxiIn\""));
                        assert!(system.contains("\"onGround\":true"));
                        assert!(system.contains("\"estimatedMinutes\":43.0"));
                        assert!(system.contains("already landed"));
                        dialogue_response(
                            &body,
                            json!({"reply":"We're on the ground after landing.","joke":""}),
                        )
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let mut state = crate::test_state();
        state.settings.ai_url = origin;
        state.settings.ai_model = "test".into();
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::TaxiIn);
        state.session.telemetry.on_ground = true;
        state.session.plan.estimated_minutes = 43.0;
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        assert_eq!(
            converse(&shared, "copilot", "Where are we in the flight?", &[])
                .await
                .as_deref(),
            Some("We're on the ground after landing.")
        );
        server.abort();
    }

    #[tokio::test]
    #[ignore = "requires the configured live model server"]
    async fn live_crew_phase_and_short_flight_context() {
        let origin = std::env::var("OPENATC_LIVE_AI_URL").expect("live model URL");
        let mut report = Vec::new();
        for (phase, on_ground, minutes) in [
            (openatc_core::state::PhaseCode::Pushback, true, 43.0),
            (openatc_core::state::PhaseCode::Cruise, false, 43.0),
            (openatc_core::state::PhaseCode::TaxiIn, true, 43.0),
            (openatc_core::state::PhaseCode::Cruise, false, 0.0),
        ] {
            for role in ["ground", "cabin", "copilot"] {
                if role == "ground" && phase != openatc_core::state::PhaseCode::Pushback {
                    continue;
                }
                let mut state = crate::test_state();
                state.settings.ai_enabled = true;
                state.settings.ai_url = origin.clone();
                state.settings.ai_model = "qwen2.5-7b-instruct-q4_k_m".into();
                state.settings.llm_phrase_variety = true;
                state.session.plan.departure = "LFMN".into();
                state.session.plan.destination = "LIMC".into();
                state.session.plan.estimated_minutes = minutes;
                state.session.phase = phase;
                state.session.telemetry.on_ground = on_ground;
                state.session.telemetry.position_valid = true;
                let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
                let started = std::time::Instant::now();
                let reply = converse(
                    &shared,
                    role,
                    "Is this a long-haul flight? Are we on the ground or in the air now?",
                    &[],
                )
                .await
                .expect("spoken response");
                report.push(json!({"role":role,"phase":format!("{phase:?}"),"onGround":on_ground,"estimatedMinutes":minutes,"reply":reply,"seconds":started.elapsed().as_secs_f64()}));
            }
        }
        std::fs::write(
            std::env::temp_dir().join("openatc-crew-phase-validation.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();
    }

    fn dialogue_response(
        body: &serde_json::Value,
        answer: serde_json::Value,
    ) -> Json<serde_json::Value> {
        let answer = if let Some(properties) = body
            .pointer("/response_format/json_schema/schema/properties")
            .and_then(serde_json::Value::as_object)
            && !properties.is_empty()
            && properties
                .values()
                .all(|property| property["type"] == "boolean")
        {
            serde_json::Value::Object(
                properties
                    .keys()
                    .map(|key| (key.clone(), json!(true)))
                    .collect(),
            )
        } else {
            answer
        };
        Json(json!({"choices":[{"message":{"content":answer.to_string()}}]}))
    }

    #[test]
    fn control_authority_requires_current_source_evidence() {
        let accepted = ControlAuthority {
            evidence: "Turn OFF the cabin lights".into(),
            requested: true,
        };
        assert!(accepted.matches("Please turn off the cabin lights and bring a sandwich."));
        assert!(!accepted.matches("Please bring a sandwich."));
        assert!(
            !ControlAuthority {
                requested: false,
                ..accepted
            }
            .matches("Turn off the cabin lights.")
        );
        assert!(
            !ControlAuthority {
                evidence: String::new(),
                requested: true
            }
            .matches("Turn off the cabin lights.")
        );
    }

    #[tokio::test]
    async fn conversation_extraction_keeps_quoted_clauses_and_rejects_invented_sources() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener,Router::new().route("/v1/chat/completions",post(|Json(body):Json<serde_json::Value>|async move{
                let context:serde_json::Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
                assert_eq!(context["role"],"cabin");
                Json(json!({"choices":[{"message":{"content":json!({"points":["Can you check for lobster?","Tell me a joke while we wait!","Emily is the captain's sister."]}).to_string()}}]}))
            }))).await.unwrap();
        });
        let mut state = crate::test_state();
        state.settings.ai_url = origin;
        state.settings.ai_model = "test".into();
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        let result=conversation_points(&shared,"cabin","Dim the lights to 50.5 percent. Can you check for lobster? Tell me a joke while we wait!").await.unwrap();
        assert_eq!(
            result,
            vec![
                "Can you check for lobster?",
                "Tell me a joke while we wait!"
            ]
        );
        assert!(shared.read().await.session.crew_actions.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn dialogue_compacts_optional_history_but_keeps_relevant_original_facts() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let token_calls = Arc::new(AtomicUsize::new(0));
        let counter = token_calls.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener,Router::new()
                .route("/v1/tokenize",post(move |Json(body):Json<serde_json::Value>| {
                    counter.fetch_add(1,Ordering::SeqCst);
                    async move {
                        let prompt = body["messages"][0]["content"].as_str().unwrap();
                        assert!(body["messages"].to_string().contains("Nora") && body["messages"].to_string().contains("Hobart"));
                        assert_eq!(body["max_tokens"],512);
                        Json(json!({"fits":!prompt.contains("UNRELATED_HISTORY")}))
                    }
                }))
                .route("/v1/chat/completions",post(|Json(body):Json<serde_json::Value>|async move {
                    dialogue_response(&body,json!({"reply":"Your sister is Nora, and you are meeting in Hobart.","joke":""}))
                }))).await.unwrap();
        });
        let mut state = crate::test_state();
        state.settings.ai_url = origin;
        state.settings.ai_model = "test".into();
        let fact = "My sister is Nora, and we are meeting in Hobart.".to_owned();
        state.crew.social.remember(
            "copilot",
            std::slice::from_ref(&fact),
            std::slice::from_ref(&fact),
        );
        for index in 0..100 {
            let quote = format!("UNRELATED_HISTORY preference {index} is blue.");
            state.crew.social.remember(
                "copilot",
                std::slice::from_ref(&quote),
                std::slice::from_ref(&quote),
            );
        }
        let shared = Arc::new(tokio::sync::RwLock::new(state));
        let result = converse(
            &shared,
            "copilot",
            "What is my sister's name and where are we meeting?",
            &[],
        )
        .await
        .unwrap();
        assert!(result.contains("Nora") && result.contains("Hobart"));
        assert!(token_calls.load(Ordering::SeqCst) > 1);
        assert!(shared.read().await.session.crew_actions.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn dialogue_refuses_tiny_context_without_generation_or_memory_changes() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/v1/tokenize",
                        post(|| async { Json(json!({"fits":false})) }),
                    )
                    .route(
                        "/v1/chat/completions",
                        post(move || {
                            counter.fetch_add(1, Ordering::SeqCst);
                            async { Json(json!({})) }
                        }),
                    ),
            )
            .await
            .unwrap();
        });
        let mut state = crate::test_state();
        state.settings.ai_url = origin;
        state.settings.ai_model = "test".into();
        let shared = Arc::new(tokio::sync::RwLock::new(state));
        assert!(
            converse(&shared, "copilot", "Hello, how are you?", &[])
                .await
                .is_none()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let state = shared.read().await;
        assert!(state.session.crew_actions.is_empty() && state.session.transcript.is_empty());
        assert!(
            state.crew.social.context("copilot", "hello")["captainQuotesNewestFirst"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        server.abort();
    }

    #[tokio::test]
    async fn dialogue_discards_replies_after_flight_or_aircraft_profile_changes() {
        use std::sync::Arc;
        for reset_profile in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let server = tokio::spawn({
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    axum::serve(listener,Router::new().route("/v1/chat/completions",post(move || {
                    let entered = entered.clone();
                    let release = release.clone();
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        Json(json!({"choices":[{"message":{"content":json!({"reply":"Your sister is Nora.","joke":"","remember":["My sister is Nora."]}).to_string()}}]}))
                    }
                }))).await.unwrap();
                }
            });
            let mut state = crate::test_state();
            state.settings.ai_url = origin;
            state.settings.ai_model = "test".into();
            let shared = Arc::new(tokio::sync::RwLock::new(state));
            let request = tokio::spawn({
                let shared = shared.clone();
                async move { converse(&shared, "copilot", "My sister is Nora.", &[]).await }
            });
            entered.notified().await;
            {
                let mut state = shared.write().await;
                if reset_profile {
                    state.crew.revision = state.crew.revision.wrapping_add(1);
                } else {
                    state.session_generation = state.session_generation.wrapping_add(1);
                }
                state.crew.social.clear();
            }
            release.notify_one();
            assert!(request.await.unwrap().is_none());
            let state = shared.read().await;
            assert!(state.session.transcript.is_empty() && state.session.crew_actions.is_empty());
            assert!(
                !state
                    .crew
                    .social
                    .context("copilot", "sister")
                    .to_string()
                    .contains("Nora")
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn one_extractor_cannot_authorize_unrequested_controls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener,Router::new().route("/v1/chat/completions",post(|Json(body):Json<serde_json::Value>|async move{
                let properties=body.pointer("/response_format/json_schema/schema/properties").and_then(serde_json::Value::as_object).unwrap();
                let answer=if properties.values().all(|property|property.pointer("/properties/evidence").is_some()) {
                    serde_json::Value::Object(properties.keys().map(|key|(key.clone(),json!({"evidence":"","requested":false}))).collect())
                } else if properties.values().all(|property|property["type"]=="boolean") {
                    serde_json::Value::Object(properties.keys().map(|key|(key.clone(),json!(true))).collect())
                } else if properties.contains_key("control_targets") {
                    json!({"control_targets":{},"cross_check":false})
                } else if properties.contains_key("actions") {
                    json!({"actions":[{"control":"heading","value":275},{"control":"beacon","value":1}],"cross_check":false,"conversation_points":["Are you ready for landing?"]})
                } else {
                    json!({"reply":"Ready here, Captain.","joke":""})
                };
                Json(json!({"choices":[{"message":{"content":answer.to_string()}}]}))
            }))).await.unwrap();
        });
        let catalogue: toml::Value =
            toml::from_str(include_str!("../../../aircraft/toliss_a320.toml")).unwrap();
        let profile: CrewProfile =
            serde_json::from_value(serde_json::to_value(&catalogue["aircraft"]["crew"]).unwrap())
                .unwrap();
        let mut state = crate::test_state();
        state.settings.ai_enabled = true;
        state.settings.ai_model = "test".into();
        state.settings.ai_url = origin;
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        observe(
            AxumState(shared.clone()),
            Json(Observation {
                aircraft: "test".into(),
                available: profile.controls.keys().cloned().collect(),
                profile: Some(profile),
                values: BTreeMap::new(),
            }),
        )
        .await;
        request(
            &shared,
            &Request {
                role: "copilot".into(),
                intent: "conversation".into(),
                text: "Are you ready for landing?".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let state = shared.read().await;
        assert!(state.session.crew_actions.is_empty());
        assert_eq!(
            state.session.transcript.last().unwrap().text,
            "Ready here, Captain."
        );
        server.abort();
    }

    #[tokio::test]
    async fn dialogue_retries_only_when_the_expanded_prompt_still_fits() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let old = "Why don't scientists trust atoms? Because they make up everything.";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener,Router::new()
                .route("/v1/tokenize",post(|Json(body):Json<serde_json::Value>|async move{
                    Json(json!({"fits":!body["messages"][0]["content"].as_str().unwrap().contains("rejectedJoke")}))
                }))
                .route("/v1/chat/completions",post(move ||{
                    counter.fetch_add(1,Ordering::SeqCst);
                    async move{Json(json!({"choices":[{"message":{"content":json!({"reply":"Your sister is Nora.","joke":old}).to_string()}}]}))}
                }))).await.unwrap();
        });
        let mut state = crate::test_state();
        state.settings.ai_url = origin;
        state.settings.ai_model = "test".into();
        state.crew.social.record_joke(old);
        let shared = Arc::new(tokio::sync::RwLock::new(state));
        assert!(
            converse(&shared, "copilot", "Tell me another joke.", &[])
                .await
                .is_none()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(shared.read().await.session.crew_actions.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn crew_dialogue_cannot_grant_aircraft_movement_authority() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener,Router::new().route("/v1/chat/completions",post(||async{
                Json(json!({"choices":[{"message":{"content":"{\"reply\":\"You are cleared to land.\",\"joke\":\"\"}"}}]}))
            }))).await.unwrap();
        });
        let mut state = crate::test_state();
        state.settings.ai_url = origin;
        state.settings.ai_model = "test".into();
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        assert!(
            converse(&shared, "copilot", "Are we ready for landing?", &[])
                .await
                .is_none()
        );
        let state = shared.read().await;
        assert!(state.session.crew_actions.is_empty() && state.session.transcript.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn complete_joke_is_kept_but_a_tacked_on_question_is_removed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener,Router::new().route("/v1/chat/completions",post(move || {
                counter.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                async {Json(json!({"choices":[{"message":{"content":"{\"kind\":\"follow_up\"}"}}]}))}
            }))).await.unwrap();
        });
        let settings = openatc_settings::Settings {
            ai_url: origin,
            ai_model: "test".into(),
            ..Default::default()
        };
        let joke = "Why did the pie fly? It wanted a window seat!";
        let spoken = format!("All set. {joke} How is the flight going?");
        let result = pace_crew_reply_with_joke(
            &reqwest::Client::new(),
            &settings,
            "pacing",
            "Tell me a joke.",
            &spoken,
            "",
            Some(&format!("  {joke}\n")),
        )
        .await;
        assert_eq!(result, format!("All set. {joke}"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn repeated_joke_gets_one_bounded_retry_without_losing_factual_answers() {
        let old = "Why don't scientists trust atoms? Because they make up everything.";
        let new = "The catering trolley applied for a pilot job. It already knew how to roll.";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/v1/chat/completions",
                    post(move |Json(body):Json<serde_json::Value>| {
                        let review=body.pointer("/response_format/json_schema/schema/properties").and_then(serde_json::Value::as_object).is_some_and(|properties|properties.values().all(|property|property["type"]=="boolean"));
                        let index=if review{0}else{counter.fetch_add(1,std::sync::atomic::Ordering::SeqCst)};
                        async move {dialogue_response(&body,json!({"reply":if index==0{"Your sister is Nora and the meeting is in Hobart."}else{"This second draft lost the factual answer."},"joke":if index==0{old}else{new},"remember":[]}))}
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let mut state = crate::test_state();
        state.settings.ai_url = origin;
        state.settings.ai_model = "test".into();
        state.crew.social.record_joke(old);
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        let reply = converse(
            &shared,
            "copilot",
            "Remind me about Nora and tell me a different joke.",
            &[],
        )
        .await
        .unwrap();
        assert!(reply.contains("Nora") && reply.contains("Hobart") && reply.contains(new));
        assert!(!reply.contains(old));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(shared.read().await.session.crew_actions.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn conversation_keeps_exact_recall_evidence_without_the_full_control_catalogue() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let source = "My sister’s name is Nora and we are meeting in Hobart.".to_owned();
        let quoted = source.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/chat/completions",post(move |Json(body):Json<serde_json::Value>| {
                let quoted = quoted.clone();
                async move {
                    let prompt = body["messages"][0]["content"].as_str().unwrap();
                    assert!(body["messages"].to_string().contains("Nora") && body["messages"].to_string().contains("Hobart"));
                    assert!(body["messages"].to_string().contains("heading") && body["messages"].to_string().contains("275"));
                    assert!(!prompt.contains("button_000900"));
                    dialogue_response(&body,json!({"reply":"Your sister is Nora, and you are meeting in Hobart. The heading is 275.","remember":[quoted,"Your sister is Emily."]}))
                }
            }))).await.unwrap();
        });
        let mut state = crate::test_state();
        state.settings.ai_url = origin;
        state.settings.ai_model = "test".into();
        state.crew.observed_at = Some(std::time::Instant::now());
        for index in 0..1000 {
            state
                .session
                .crew_controls
                .push(openatc_core::crew::Capability {
                    id: format!("button_{index:06}"),
                    label: "Other control".into(),
                    role: "copilot".into(),
                    available: true,
                    value: None,
                    state: "unknown".into(),
                });
        }
        state
            .session
            .crew_controls
            .push(openatc_core::crew::Capability {
                id: "heading".into(),
                label: "Heading".into(),
                role: "copilot".into(),
                available: true,
                value: Some(275.0),
                state: "value".into(),
            });
        state.crew.values.insert("heading".into(), 275.0);
        state
            .request_memory
            .begin("copilot", openatc_core::state::PhaseCode::Cruise, &source);
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        let reply = converse(
            &shared,
            "copilot",
            "What is my sister’s name, where are we meeting and what heading did you set?",
            &[],
        )
        .await
        .unwrap();
        assert!(reply.contains("Nora") && reply.contains("Hobart"));
        let state = shared.read().await;
        let memory = state.crew.social.context("copilot", "sister");
        assert!(
            memory["captainQuotesNewestFirst"]
                .as_array()
                .unwrap()
                .iter()
                .any(|quote| quote == &json!(source))
        );
        assert!(!memory.to_string().contains("Emily"));
        assert!(state.session.crew_actions.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn control_interpreter_excludes_social_history_and_refuses_tiny_contexts() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let tiny = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let token_calls = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn({
            let tiny = tiny.clone();
            let calls = calls.clone();
            let token_calls = token_calls.clone();
            async move {
                axum::serve(listener, Router::new()
                    .route("/v1/tokenize", post(move |Json(body): Json<serde_json::Value>| {
                        let tiny = tiny.clone();
                        let token_calls = token_calls.clone();
                        async move {
                            token_calls.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(body["max_tokens"], 512);
                            let prompt = body["messages"][0]["content"].as_str().unwrap();
                            assert!(prompt.contains("external_power") && prompt.contains("chocks"));
                            assert_eq!(body["messages"][1]["content"], "Hello, Joe. How are the kids?");
                            Json(json!({"fits": !tiny.load(Ordering::SeqCst) && !prompt.contains("OPTIONAL_SOCIAL_NOTE")}))
                        }
                    }))
                    .route("/v1/chat/completions", post(move || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        async { Json(json!({"choices":[{"message":{"content":json!({
                            "actions":[],"cross_check":false,"conversation_reply":"The kids are doing well, Captain.","memory":""
                        }).to_string()}}]})) }
                    }))
                ).await.unwrap();
            }
        });
        let catalogue: toml::Value =
            toml::from_str(include_str!("../../../aircraft/toliss_a320.toml")).unwrap();
        let profile: CrewProfile =
            serde_json::from_value(serde_json::to_value(&catalogue["aircraft"]["crew"]).unwrap())
                .unwrap();
        let mut state = crate::test_state();
        state.settings.ai_enabled = true;
        state.settings.ai_model = "test".into();
        state.settings.ai_url = origin;
        state.session.telemetry.on_ground = true;
        let shared = Arc::new(tokio::sync::RwLock::new(state));
        observe(
            AxumState(shared.clone()),
            Json(Observation {
                aircraft: "test".into(),
                available: profile.controls.keys().cloned().collect(),
                profile: Some(profile),
                values: BTreeMap::new(),
            }),
        )
        .await;
        let optional = "OPTIONAL_SOCIAL_NOTE".repeat(10);
        shared.write().await.crew.social.remember(
            "ground",
            std::slice::from_ref(&optional),
            std::slice::from_ref(&optional),
        );
        let request = Request {
            role: "ground".into(),
            intent: "conversation".into(),
            text: "Hello, Joe. How are the kids?".into(),
            ..Default::default()
        };
        super::request(&shared, &request).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(token_calls.load(Ordering::SeqCst), 2);
        assert!(shared.read().await.session.crew_actions.is_empty());
        tiny.store(true, Ordering::SeqCst);
        super::request(&shared, &request).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(shared.read().await.session.crew_actions.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn refreshed_observations_allow_actions_after_slow_inference() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/chat/completions", post(|Json(body):Json<serde_json::Value>| async move {
                if let Some(properties)=body.pointer("/response_format/json_schema/schema/properties").and_then(serde_json::Value::as_object)
                    && properties.values().all(|value|value.pointer("/properties/evidence").is_some()) {
                    let context:serde_json::Value=serde_json::from_str(body["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap()).unwrap();
                    let approved=properties.keys().map(|key|(key.clone(),json!({"evidence":context["captainMessage"],"requested":true}))).collect::<serde_json::Map<_,_>>();
                    return Json(json!({"choices":[{"message":{"content":json!(approved).to_string()}}]}));
                }
                if let Some(properties)=body.pointer("/response_format/json_schema/schema/properties").and_then(serde_json::Value::as_object)
                    && properties.values().all(|value|value["type"]=="boolean") {
                    let approved=properties.keys().map(|key|(key.clone(),json!(true))).collect::<serde_json::Map<_,_>>();
                    return Json(json!({"choices":[{"message":{"content":json!(approved).to_string()}}]}));
                }
                tokio::time::sleep(std::time::Duration::from_millis(3200)).await;
                Json(json!({"choices":[{"message":{"content":json!({"reply":"I'll take care of both. Coffee when you're back sounds good!","actions":[{"control":"external_power","value":0},{"control":"chocks","value":0}],"cross_check":false,"memory":"Captain would like to catch up after the flight."}).to_string()}}]}))
            }))).await.unwrap();
        });
        let catalogue: toml::Value =
            toml::from_str(include_str!("../../../aircraft/toliss_a320.toml")).unwrap();
        let profile: CrewProfile =
            serde_json::from_value(serde_json::to_value(&catalogue["aircraft"]["crew"]).unwrap())
                .unwrap();
        let available = profile.controls.keys().cloned().collect::<Vec<_>>();
        let mut state = crate::test_state();
        state.settings.ai_enabled = true;
        state.settings.ai_model = "test".into();
        state.settings.ai_url = format!("http://{address}");
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        observe(
            AxumState(shared.clone()),
            Json(Observation {
                aircraft: "test".into(),
                profile: Some(profile),
                values: BTreeMap::new(),
                available: available.clone(),
            }),
        )
        .await;
        let observed = shared.clone();
        let sampling = tokio::spawn(async move {
            loop {
                observe(
                    AxumState(observed.clone()),
                    Json(Observation {
                        aircraft: "test".into(),
                        profile: None,
                        values: BTreeMap::new(),
                        available: available.clone(),
                    }),
                )
                .await;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        });
        let response = request(
            &shared,
            &Request {
                role: "ground".into(),
                intent: "conversation".into(),
                text:
                    "Hey buddy, disconnect external power and remove chocks. Coffee when I return?"
                        .into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(response.status().is_success());
        let state = shared.read().await;
        assert_eq!(state.session.crew_actions.len(), 2);
        assert!(state.session.transcript[0].text.contains("Hey buddy"));
        sampling.abort();
        server.abort();
    }

    #[tokio::test]
    async fn a_controls_wait_reply_keeps_the_captains_request_in_the_log() {
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(crate::test_state()));
        let text = "disconnect external power and remove chocks please";
        let response = request(
            &shared,
            &Request {
                role: "ground".into(),
                intent: "conversation".into(),
                text: text.into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(response.status().is_success());
        let state = shared.read().await;
        assert_eq!(state.session.transcript.len(), 2);
        assert_eq!(state.session.transcript[0].text, text);
        assert_eq!(state.session.transcript[1].speaker, "GROUND");
        assert!(state.session.crew_actions.is_empty());
    }

    #[tokio::test]
    async fn ramp_interphone_disconnects_for_actual_taxi_and_flight_not_pushback_permission() {
        use openatc_core::state::PhaseCode;
        let mut state = crate::test_state();
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::PushbackComplete);
        state.session.telemetry.ground_speed_knots = 3.0;
        update_ground_connection(&mut state);
        assert!(!state.session.ground_interphone_disconnected);
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Taxi);
        state.session.telemetry.ground_speed_knots = 0.0;
        update_ground_connection(&mut state);
        assert!(
            !state.session.ground_interphone_disconnected,
            "Taxi permission at the gate is not observed taxi movement"
        );
        state.session.telemetry.latitude += 0.0001;
        state.session.telemetry.ground_speed_knots = 2.0;
        update_ground_connection(&mut state);
        assert!(state.session.ground_interphone_disconnected);
        state.session.telemetry.ground_speed_knots = 0.0;
        update_ground_connection(&mut state);
        assert!(
            state.session.ground_interphone_disconnected,
            "Stopping at a hold does not reconnect a ramp headset"
        );
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Cruise);
        state.session.telemetry.on_ground = false;
        state.settings.ai_enabled = true;
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        let response = request(
            &shared,
            &Request {
                role: "ground".into(),
                intent: "conversation".into(),
                text: "Are you well?".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let body = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response["result"]["silent"], true);
        assert!(
            shared.read().await.session.transcript.is_empty(),
            "Disconnected ramp staff must not speak from the air"
        );
        let mut state = shared.write().await;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Finished);
        state.session.telemetry.on_ground = true;
        state.session.taxi_clearance.to_parking = true;
        state.session.taxi_clearance.guidance_complete = true;
        update_ground_connection(&mut state);
        assert!(!state.session.ground_interphone_disconnected);
    }

    #[tokio::test]
    async fn pacing_preserves_operational_clarification_but_closes_social_questions() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let allowed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let decision = allowed.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener,Router::new().route("/v1/chat/completions",post(move |Json(body):Json<serde_json::Value>| {
                let decision=decision.clone();async move {
                    assert!(body["messages"][1]["content"].as_str().unwrap().contains("previousCrewReply"));
                    Json(json!({"choices":[{"message":{"content":json!({"kind":if decision.load(std::sync::atomic::Ordering::SeqCst){"clarification"}else{"follow_up"}}).to_string()}}]}))
                }
            }))).await.unwrap();
        });
        let settings = openatc_settings::Settings {
            ai_url: format!("http://{address}"),
            ai_model: "test".into(),
            ..Default::default()
        };
        let client = reqwest::Client::new();
        let text = "Understood, Captain. I'll leave you to it. How is the flight going?";
        assert_eq!(
            pace_crew_reply(
                &client,
                &settings,
                "pacing",
                "Not yet",
                text,
                "Would you like a coffee?"
            )
            .await,
            "Understood, Captain. I'll leave you to it."
        );
        assert_eq!(
            pace_crew_reply(
                &client, &settings, "pacing", "Ready for landing, heading two seven five and beacon on",
                "Got it, ready for landing. Heading two seven five and beacon on are set. How's the family doing today?", ""
            ).await,
            "Got it, ready for landing. Heading two seven five and beacon on are set."
        );
        allowed.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            pace_crew_reply(
                &client,
                &settings,
                "pacing",
                "Turn the lights down",
                "Which lights should I dim?",
                ""
            )
            .await,
            "Which lights should I dim?"
        );
        server.abort();
    }

    #[tokio::test]
    async fn requested_joke_does_not_authorize_a_separate_social_question() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/chat/completions", post(|Json(body): Json<serde_json::Value>| async move {
                let context: serde_json::Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
                let question = context["proposedReply"].as_str().unwrap();
                let kind = if question.contains("Why") {"answer"} else {"follow_up"};
                Json(json!({"choices":[{"message":{"content":json!({"kind":kind}).to_string()}}]}))
            }))).await.unwrap();
        });
        let settings = openatc_settings::Settings {
            ai_url: origin,
            ai_model: "test".into(),
            ..Default::default()
        };
        assert_eq!(
            pace_crew_reply(
                &reqwest::Client::new(),
                &settings,
                "pacing",
                "Tell me a joke.",
                "All set. Why did the pie fly? It wanted a window seat! How is the flight going?",
                ""
            )
            .await,
            "All set. Why did the pie fly? It wanted a window seat!"
        );
        server.abort();
    }

    #[tokio::test]
    async fn polite_joke_request_is_conversation_without_pushback_or_control_actions() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let generations = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = generations.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/chat/completions", post(move |Json(body):Json<serde_json::Value>| { let calls = calls.clone(); async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let classifier = body["messages"][0]["content"].as_str().unwrap().contains("Allowed controls:");
                let pacing = body["response_format"]["json_schema"]["schema"]["properties"].get("kind").is_some();
                let content = if pacing {json!({"kind":"answer"}).to_string()}
                    else if classifier { json!({"reply":"Here is a quick one: why did the biscuit visit the doctor? It felt crummy!","actions":[],"cross_check":false,"memory":"Captain enjoys biscuit jokes."}).to_string() }
                    else if body.get("response_format").is_some() { json!({"reply":"Here's a quick one: why did the biscuit visit the doctor? It felt crummy!"}).to_string() }
                    else { "Here's a quick one: why did the biscuit visit the doctor? It felt crummy!".into() };
                dialogue_response(&body,serde_json::from_str(&content).unwrap())
            }}))).await.unwrap();
        });
        let mut state = crate::test_state();
        state.settings.ai_enabled = true;
        state.settings.ai_model = "test".into();
        state.settings.llm_phrase_variety = false;
        state.settings.ai_url = format!("http://{address}");
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::PushbackComplete);
        state.crew.observed_at = None;
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        let response = request(
            &shared,
            &Request {
                role: "cabin".into(),
                intent: "pushback".into(),
                text: "can you tell me a joke while we are waiting for pushback?".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(response.status().is_success());
        let state = shared.read().await;
        assert!(state.session.crew_actions.is_empty());
        assert_eq!(
            state.session.phase,
            openatc_core::state::PhaseCode::Pushback
        );
        assert_eq!(state.session.transcript.len(), 2);
        assert_eq!(
            state.session.transcript.last().unwrap().speaker,
            "ATTENDANT"
        );
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("biscuit")
        );
        drop(state);
        for (role, speaker, text) in [
            (
                "cabin",
                "ATTENDANT",
                "what about the joke? can you tell me one?",
            ),
            ("copilot", "COPILOT", "are you okay?"),
            ("ground", "GROUND", "can you tell me something funny?"),
        ] {
            shared.write().await.crew.observed_at = None;
            let response = request(
                &shared,
                &Request {
                    role: role.into(),
                    intent: "conversation".into(),
                    text: text.into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert!(response.status().is_success());
            let state = shared.read().await;
            assert!(state.session.crew_actions.is_empty());
            assert_eq!(state.session.transcript.last().unwrap().speaker, speaker);
            assert!(
                state
                    .session
                    .transcript
                    .last()
                    .unwrap()
                    .text
                    .contains("biscuit")
            );
        }
        // Four short exchanges stay within a bounded number of model calls.
        assert!(generations.load(std::sync::atomic::Ordering::SeqCst) <= 20);
        let state = shared.read().await;
        for role in ["cabin", "copilot", "ground"] {
            let context = state.crew.social.context(role, "biscuit");
            assert!(
                !context["captainQuotesNewestFirst"]
                    .to_string()
                    .contains("biscuit")
            );
            assert!(
                context["recentCrewRepliesNewestFirst"][0]
                    .as_str()
                    .unwrap()
                    .contains("biscuit")
            );
        }
        server.abort();
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod callout_log_tests {
    use super::*;
    #[tokio::test]
    async fn preloaded_calls_are_logged_without_replacing_radio_instructions() {
        let state = crate::test_state();
        let callsign = state.session.plan.callsign.clone();
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        callout_log(
            AxumState(shared.clone()),
            Json(CalloutLog {
                callsign,
                entries: vec!["V1 · 127 kt".into(), "Rotate · 146 kt".into()],
            }),
        )
        .await;
        let state = shared.read().await;
        assert_eq!(state.session.transcript.len(), 2);
        assert!(
            state
                .session
                .transcript
                .iter()
                .all(|entry| entry.speaker == "COPILOT"
                    && entry.delivery == "preloaded-callout"
                    && !entry.is_controller_instruction())
        );
        assert!(state.session.crew_actions.is_empty());
    }
}
