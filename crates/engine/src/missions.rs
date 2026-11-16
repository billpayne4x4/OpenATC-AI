//! Mission catalogue, branching cabin stories and flight-local progress.
use crate::{EngineState, Shared, routes::bad_request};
use axum::{
    Json,
    extract::State as AxumState,
    response::{IntoResponse, Response},
};
#[cfg(test)]
use openatc_core::aircraft::AircraftBehaviour;
use openatc_core::{
    apply::add_transmission,
    dialogue::say,
    missions::{Mission, Status},
    state::{PhaseCode, SpeechTag},
};
use rand::Rng;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};

#[derive(Default)]
pub struct Runtime {
    mission: Option<Mission>,
    elapsed: BTreeMap<String, f64>,
    outcomes: BTreeMap<String, String>,
    last: Option<Instant>,
    active: Option<usize>,
}

pub fn builtin() -> Vec<Mission> {
    [
        include_str!("../../../missions/coastal-medical.toml"),
        include_str!("../../../missions/new-arrival.toml"),
        include_str!("../../../missions/rough-air.toml"),
        include_str!("../../../missions/parts-shuttle.toml"),
        include_str!("../../../missions/nervous-flyer.toml"),
        include_str!("../../../missions/island-reunion.toml"),
        include_str!("../../../missions/cabin-smoke.toml"),
        include_str!("../../../missions/assisted-arrival.toml"),
    ]
    .into_iter()
    .map(|text| openatc_core::missions::parse(text).expect("bundled mission validated by tests"))
    .collect()
}

fn catalogue(dir: &std::path::Path) -> (Vec<Mission>, Vec<String>) {
    let exe = std::env::current_exe().ok();
    let installed = exe
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(|p| p.join("../missions"));
    let configured = std::env::var_os("OPENATC_MISSIONS_DIR").map(std::path::PathBuf::from);
    catalogue_from_dirs(
        installed
            .into_iter()
            .chain(configured)
            .chain([dir.join("missions")]),
    )
}

fn catalogue_from_dirs(
    dirs: impl IntoIterator<Item = std::path::PathBuf>,
) -> (Vec<Mission>, Vec<String>) {
    let mut missions: BTreeMap<_, _> = builtin().into_iter().map(|m| (m.id.clone(), m)).collect();
    let mut errors = Vec::new();
    for dir in dirs {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten().take(256) {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "toml")
                    || entry.metadata().is_ok_and(|m| m.len() > 256 * 1024)
                {
                    continue;
                }
                match std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|s| openatc_core::missions::parse(&s))
                {
                    Ok(mission) => {
                        missions.insert(mission.id.clone(), mission);
                    }
                    Err(error) => errors.push(format!(
                        "{}: {error}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    )),
                }
            }
        }
    }
    (missions.into_values().collect(), errors)
}

pub async fn list(AxumState(shared): AxumState<Shared>) -> Response {
    let dir = shared.read().await.config_dir.clone();
    match tokio::task::spawn_blocking(move || catalogue(&dir)).await {
        Ok((missions, errors)) => {
            Json(json!({"missions":missions,"errors":errors})).into_response()
        }
        Err(_) => bad_request(openatc_core::dialogue::say(
            "notice_could_not_read_mission_catalogue_53196b63",
            &[],
        )),
    }
}

pub async fn save(AxumState(shared): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    let text = body["source"].as_str().unwrap_or("").to_owned();
    let mission = match openatc_core::missions::parse(&text) {
        Ok(m) => m,
        Err(e) => return bad_request(e),
    };
    let dir = shared.read().await.config_dir.join("missions");
    let id = mission.id.clone();
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let pending = dir.join(format!("{id}.pending"));
        std::fs::write(&pending, text)?;
        std::fs::rename(pending, dir.join(format!("{id}.toml")))
    })
    .await;
    match result {
        Ok(Ok(())) => Json(json!({"accepted":true,"mission":mission})).into_response(),
        _ => bad_request(openatc_core::dialogue::say(
            "notice_could_not_save_mission_file_2d41c78c",
            &[],
        )),
    }
}

pub async fn start(AxumState(shared): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    let id = body["id"].as_str().unwrap_or("").to_owned();
    let (dir, generation) = {
        let s = shared.read().await;
        (s.config_dir.clone(), s.session_generation)
    };
    let Ok((catalogue, _)) = tokio::task::spawn_blocking(move || catalogue(&dir)).await else {
        return bad_request(openatc_core::dialogue::say(
            "notice_could_not_read_mission_catalogue_53196b63",
            &[],
        ));
    };
    let Some(mission) = catalogue.into_iter().find(|m| m.id == id) else {
        return bad_request(openatc_core::dialogue::say(
            "notice_mission_not_found_7e10c47e",
            &[],
        ));
    };
    let mut state = shared.write().await;
    if state.session_generation != generation
        || !matches!(
            state.session.phase,
            PhaseCode::Parked | PhaseCode::Clearance
        )
        || state.session.mission.running
    {
        return bad_request(openatc_core::dialogue::say(
            "notice_start_a_mission_before_taxi_with_no_other_mission_runnin_01a2d004",
            &[],
        ));
    }
    let aircraft = if state.session.telemetry.aircraft_icao.is_empty() {
        &state.session.plan.aircraft
    } else {
        &state.session.telemetry.aircraft_icao
    };
    if !mission.compatible(aircraft) {
        return bad_request(openatc_core::dialogue::say(
            "notice_this_mission_is_not_suitable_for_the_loaded_aircraft_typ_8187f302",
            &[],
        ));
    }
    state.session.mission = Status {
        id: mission.id.clone(),
        title: mission.title.clone(),
        running: true,
        ..Default::default()
    };
    state.mission = Runtime {
        mission: Some(mission),
        ..Default::default()
    };
    Json(json!({"accepted":true})).into_response()
}

pub async fn stop(AxumState(shared): AxumState<Shared>) -> Response {
    let mut s = shared.write().await;
    s.session.mission.running = false;
    s.session.mission.active_event.clear();
    s.session.mission.choices.clear();
    s.session.mission.severity.clear();
    s.session.mission.incident.clear();
    s.session
        .mission
        .debrief
        .push(say("mission_stopped_by_pilot", &[]));
    s.mission = Runtime::default();
    Json(json!({"accepted":true})).into_response()
}

pub fn clock(state: &mut EngineState) {
    let now = Instant::now();
    let elapsed = state
        .mission
        .last
        .replace(now)
        .map_or(0.0, |t| now.duration_since(t).as_secs_f64().min(2.0));
    advance(state, elapsed);
}

fn advance(state: &mut EngineState, seconds: f64) {
    if !state.session.mission.running || state.session.telemetry.paused {
        return;
    }
    if let Some(m) = &state.mission.mission {
        for e in &m.events {
            if openatc_core::missions::phase(&e.phase) == Some(state.session.phase) {
                *state.mission.elapsed.entry(e.id.clone()).or_default() += seconds;
            }
        }
    }
}

fn wording(lines: &[String], seat: &str, passengers: i32, variety: bool) -> String {
    let index = if variety {
        rand::rng().random_range(0..lines.len())
    } else {
        0
    };
    lines[index].replace("{seat}", seat).replace(
        "{passengers}",
        &if passengers > 0 {
            passengers.to_string()
        } else {
            "unconfirmed".into()
        },
    )
}

/// Called only after current radio and operational instructions have finished.
pub fn tick(state: &mut EngineState) {
    if !state.session.mission.running
        || state.session.telemetry.paused
        || !state.session.crew_actions.is_empty()
    {
        return;
    }
    let Some(mission) = &state.mission.mission else {
        return;
    };
    let aircraft = if state.session.telemetry.aircraft_icao.is_empty() {
        &state.session.plan.aircraft
    } else {
        &state.session.telemetry.aircraft_icao
    };
    if !mission.compatible(aircraft) {
        state.session.mission.running = false;
        state
            .session
            .mission
            .debrief
            .push(say("mission_aircraft_changed", &[]));
        state.session.mission.active_event.clear();
        state.session.mission.choices.clear();
        state.session.mission.severity.clear();
        state.session.mission.incident.clear();
        state.mission.active = None;
        return;
    }
    if state.session.phase == PhaseCode::Finished {
        if let Some(index) = state.mission.active.take() {
            state
                .session
                .mission
                .debrief
                .push(format!("{}: unanswered at gate.", mission.events[index].id));
        }
        state.session.mission.running = false;
        state.session.mission.active_event.clear();
        state.session.mission.choices.clear();
        state.session.mission.severity.clear();
        state.session.mission.incident.clear();
        state
            .session
            .mission
            .debrief
            .push(say("mission_reached_parking", &[]));
        return;
    }
    if state.mission.active.is_some() {
        return;
    }
    for (index, event) in mission.events.iter().enumerate() {
        if state.mission.outcomes.contains_key(&event.id)
            || openatc_core::missions::phase(&event.phase) != Some(state.session.phase)
            || state.mission.elapsed.get(&event.id).copied().unwrap_or(0.0)
                < f64::from(event.delay_seconds)
            || (!event.after_event.is_empty()
                && state.mission.outcomes.get(&event.after_event) != Some(&event.after_outcome))
        {
            continue;
        }
        if rand::rng().random_range(0..100u8) >= event.probability {
            state
                .mission
                .outcomes
                .insert(event.id.clone(), "not_triggered".into());
            state
                .session
                .mission
                .debrief
                .push(format!("{}: did not occur on this flight.", event.id));
            continue;
        }
        let text = wording(
            &event.say,
            &event.seat,
            state.session.plan.passengers,
            state.settings.llm_phrase_variety,
        );
        state.mission.active = Some(index);
        state.session.mission.active_event = event.id.clone();
        state.session.mission.choices = event.choices.clone();
        if event.severity != "normal" {
            state.session.mission.severity = event.severity.clone();
            state.session.mission.incident.clone_from(&text);
        }
        state
            .session
            .mission
            .debrief
            .push(format!("{}: reported during {}.", event.id, event.phase));
        crate::crew::speak(state, "cabin", &text);
        break;
    }
}

/// Typed or spoken mission choices use the same path as their buttons.
pub fn respond(state: &mut EngineState, text: &str) -> Option<String> {
    let index = state.mission.active?;
    let event = state.mission.mission.as_ref()?.events.get(index)?;
    let normalized = text.trim().trim_end_matches(['.', '!', '?']).to_lowercase();
    let choice = event.choices.iter().find(|c| {
        c.id == normalized
            || c.label
                .trim_end_matches(['.', '!', '?'])
                .eq_ignore_ascii_case(&normalized)
            || c.aliases
                .iter()
                .any(|a| a.eq_ignore_ascii_case(&normalized))
    })?;
    let reply = wording(
        &choice.say,
        &event.seat,
        state.session.plan.passengers,
        state.settings.llm_phrase_variety,
    );
    state
        .mission
        .outcomes
        .insert(event.id.clone(), choice.outcome.clone());
    state
        .session
        .mission
        .debrief
        .push(format!("{}: {}", event.id, choice.outcome));
    state.mission.active = None;
    state.session.mission.active_event.clear();
    state.session.mission.choices.clear();
    Some(reply)
}

pub async fn choose(AxumState(shared): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    let mut state = shared.write().await;
    if state.session.telemetry.radio_busy
        || state.pending_copilot_reply.is_some()
        || state
            .session
            .telemetry
            .radio_sequence_seen
            .is_some_and(|seen| {
                state
                    .session
                    .transcript
                    .iter()
                    .any(|t| t.sequence > seen && !t.background)
            })
    {
        return bad_request(openatc_core::dialogue::say(
            "notice_wait_for_the_current_transmission_to_finish_44e12dd6",
            &[],
        ));
    }
    let id = body["choice"].as_str().unwrap_or("");
    let Some(label) = state
        .session
        .mission
        .choices
        .iter()
        .find(|c| c.id == id)
        .map(|c| c.label.clone())
    else {
        return bad_request(openatc_core::dialogue::say(
            "notice_this_mission_choice_is_no_longer_available_b5a3eeae",
            &[],
        ));
    };
    let Some(reply) = respond(&mut state, id) else {
        return bad_request(openatc_core::dialogue::say(
            "notice_no_active_mission_event_b6db1eb1",
            &[],
        ));
    };
    let callsign = state.session.plan.callsign.clone();
    let phase = state.session.phase;
    let request_id = state.request_memory.begin("cabin", phase, &label);
    let tag = SpeechTag {
        voice: state.settings.pilot_voice.clone(),
        ..Default::default()
    };
    add_transmission(&mut state.session, &callsign, &label, &tag);
    crate::crew::speak(&mut state, "cabin", &reply);
    state.request_memory.finish(
        request_id,
        &json!({"result":{"accepted":true,"message":reply}}),
        &[],
    );
    Json(json!({"accepted":true})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_bundled_mission_validates_and_is_editable() {
        for mission in builtin() {
            mission.validate().unwrap();
            assert!(mission.compatible("A20N"));
            assert!(mission.media.is_empty());
        }
    }
    #[test]
    fn phase_clock_pause_probability_choices_and_incident_are_consistent() {
        let mut state = crate::test_state();
        let mut mission = builtin().remove(0);
        mission.events[1].phase = "departure".into();
        mission.events[1].after_event = "cabin_call".into();
        mission.events[1].after_outcome = "coordinating".into();
        state.session.mission = Status {
            running: true,
            ..Default::default()
        };
        state.mission = Runtime {
            mission: Some(mission),
            ..Default::default()
        };
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Taxi);
        advance(&mut state, 60.0);
        tick(&mut state);
        assert!(state.session.transcript.is_empty());
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Departure);
        state.session.telemetry.paused = true;
        advance(&mut state, 60.0);
        tick(&mut state);
        assert!(state.session.transcript.is_empty());
        state.session.telemetry.paused = false;
        advance(&mut state, 21.0);
        tick(&mut state);
        assert_eq!(state.session.mission.active_event, "cabin_call");
        let count = state.session.transcript.len();
        tick(&mut state);
        assert_eq!(state.session.transcript.len(), count);
        assert!(respond(&mut state, "coordinate").is_some());
        assert_eq!(state.session.mission.severity, "emergency");
        assert!(!state.session.mission.incident.is_empty());
        assert!(respond(&mut state, "coordinate").is_none());
        tick(&mut state);
        assert_eq!(state.session.mission.active_event, "arrival_handover");
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Finished);
        tick(&mut state);
        assert!(!state.session.mission.running);
        assert!(state.session.mission.incident.is_empty());
        let mut zero = builtin().remove(0);
        zero.events[0].probability = 0;
        state.session.mission = Status {
            running: true,
            ..Default::default()
        };
        state.mission = Runtime {
            mission: Some(zero),
            ..Default::default()
        };
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Departure);
        advance(&mut state, 100.0);
        tick(&mut state);
        assert!(state.session.mission.active_event.is_empty());
        assert_eq!(state.mission.outcomes["cabin_call"], "not_triggered");
    }
}
