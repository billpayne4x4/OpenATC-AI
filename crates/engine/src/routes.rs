//! HTTP endpoints for dialogue, flight plans, simulator data and speech.

#![allow(clippy::default_trait_access)]

use super::Shared;
use super::llm::{clean_reply, text as chat_complete};
use super::support::service_ok;
use crate::EngineState;
use axum::{
    Json,
    extract::{FromRequest, State as AxumState},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use openatc_core::airport::{demo_airport, load_airport_from_simulator};
use openatc_core::apply::{
    AssignOrder, RequestContext, RunwaySequence, SimpleRng, add_transmission, apply_request,
    assign_controller,
};
use openatc_core::catalog::render_template;
use openatc_core::identify::interpret_text;
use openatc_core::intents::{Realism, discipline_preset};
use openatc_core::ops::{delivery_preset, parse_voice_pool, validate_flight_plan};
use openatc_core::regions::{region_for, units_for_region};
use openatc_core::simbrief::parse_simbrief;
use openatc_core::speech::{PromptContext, SpeechPool, parse_ai_output, sample_slots};
use openatc_core::state::{PhaseCode, Request, SpeechTag, State, TaxiClearance};
use openatc_core::{UnitSystem, resolve_units};
use openatc_settings::{CallsignNumbers, Congestion, Delivery, Settings, Units};
use rand::Rng;
use serde_json::{Value, json};
use std::fmt::Write as _;

/// JSON error response with HTTP status 400.
pub(crate) fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message.into()})),
    )
        .into_response()
}

fn delivery_name(delivery: Delivery) -> &'static str {
    match delivery {
        Delivery::Standard => "standard",
        Delivery::Brisk => "brisk",
        Delivery::Urgent => "urgent",
    }
}

fn realism_from(settings: &Settings) -> Realism {
    Realism {
        strict_readbacks: settings.strict_readbacks,
        require_frequency: settings.require_frequency,
        require_callsign: settings.require_callsign,
        strict_phraseology: settings.strict_phraseology,
        teaching_corrections: settings.teaching_corrections,
        practice_emergencies: settings.practice_emergencies,
    }
}

pub(crate) fn snapshot(session: &State, settings: &Settings) -> Value {
    let mut state = serde_json::to_value(session).unwrap_or(Value::Null);
    if let Some(entries) = state["transcript"].as_array_mut() {
        for entry in entries {
            if entry["speaker"] == "ATC" && entry["background"] != true {
                let text = entry["text"].as_str().unwrap_or("");
                if !text.is_empty()
                    && !session.plan.callsign.is_empty()
                    && !text
                        .to_ascii_uppercase()
                        .contains(&session.plan.callsign.to_ascii_uppercase())
                {
                    entry["text"] = Value::String(openatc_core::dialogue::say(
                        "addressed_controller_transmission",
                        &[
                            ("callsign", session.plan.callsign.clone()),
                            ("message", text.to_owned()),
                        ],
                    ));
                }
            }
        }
    }
    if let Some(map) = state.as_object_mut() {
        map.insert(
            "settings".to_owned(),
            serde_json::to_value(settings).unwrap_or(Value::Null),
        );
    }
    state
}

fn units_for(
    settings: &Settings,
    departure: &str,
    region: &openatc_core::regions::Region,
) -> UnitSystem {
    if settings.units == Units::Region {
        return units_for_region(region);
    }
    let preference = match settings.units {
        Units::Imperial => "imperial",
        Units::Metric => "metric",
        Units::Region => "region",
    };
    resolve_units(preference, departure)
}

pub async fn health(AxumState(state): AxumState<Shared>) -> Json<Value> {
    let state = state.read().await;
    Json(json!({
        "service": "open-atc",
        "protocol": 2,
        "cpuWorkers": state.workers.count(),
        "version": super::VERSION,
        "prompts": state.prompts.source,
        "regions": if state.regions_path.is_empty() { "built-in".to_owned() } else { state.regions_path.clone() },
    }))
}

pub async fn voice_health(AxumState(state): AxumState<Shared>) -> Json<Value> {
    let (client, stt, tts) = {
        let state = state.read().await;
        (
            state.client.clone(),
            state.settings.stt_url.clone(),
            state.settings.tts_url.clone(),
        )
    };
    let ai_version = if stt.trim().is_empty() {
        Value::Null
    } else {
        match client
            .get(format!("{}/health", stt.trim_end_matches('/')))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(reply) => match reply.json::<Value>().await {
                Ok(body) => body.get("version").cloned().unwrap_or(Value::Null),
                Err(_) => Value::Null,
            },
            Err(_) => Value::Null,
        }
    };
    Json(json!({
        "stt": service_ok(&client, &stt).await,
        "tts": service_ok(&client, &tts).await,
        "engineVersion": super::VERSION,
        "aiVersion": ai_version,
    }))
}

/// Fetch provider voices without blocking simulator telemetry or exposing credentials.
pub async fn post_voices(AxumState(shared): AxumState<Shared>) -> Response {
    match refresh_voices(&shared, true).await {
        Ok(()) => Json(json!({"voices":shared.read().await.voices})).into_response(),
        Err(error) => bad_request(error),
    }
}

async fn refresh_voices(shared: &Shared, force: bool) -> Result<(), String> {
    let (client, origin) = {
        let mut state = shared.write().await;
        if !force
            && state.voices_source == state.settings.tts_url
            && state
                .voices_refreshed
                .is_some_and(|t| t.elapsed().as_secs() < 60)
        {
            return Ok(());
        }
        if state.voices_source != state.settings.tts_url {
            state.voices.clear();
        }
        state.voices_refreshed = Some(std::time::Instant::now());
        let tts_url = state.settings.tts_url.clone();
        state.voices_source = tts_url;
        (state.client.clone(), state.settings.tts_url.clone())
    };
    let mut call = client
        .get(format!("{}/v1/voices", origin.trim_end_matches('/')))
        .timeout(std::time::Duration::from_secs(10));
    if let Ok(key) = std::env::var("OPENATC_TTS_KEY") {
        call = call.bearer_auth(key);
    }
    let body = match call.send().await {
        Ok(reply) if reply.status().is_success() => match reply.json::<Value>().await {
            Ok(body) => body,
            Err(_) => return Err("Voice API returned invalid JSON.".into()),
        },
        Ok(reply) => return Err(format!("Voice API returned HTTP {}.", reply.status())),
        Err(_) => return Err("Could not reach the configured TTS voice API.".into()),
    };
    let voices = match openatc_core::voices::parse(&body) {
        Ok(voices) => voices,
        Err(error) => return Err(error),
    };
    let mut state = shared.write().await;
    if state.settings.tts_url != origin {
        return Err("TTS service changed; refresh the voice list.".into());
    }
    state.voices.clone_from(&voices);
    state.voices_source = origin;
    Ok(())
}

pub async fn full_state(AxumState(state): AxumState<Shared>) -> Json<Value> {
    let state = state.read().await;
    let mut result = snapshot(&state.session, &state.settings);
    result["version"] = json!(super::VERSION);
    let mut airports = Vec::new();
    if state.station_root == state.settings.simulator_root {
        let nearby = openatc_core::stations::nearby(&state.stations, &state.session.telemetry);
        let tuned_airports = [
            Some(state.session.telemetry.com1_khz),
            state.session.telemetry.com2_khz,
        ]
        .into_iter()
        .flatten()
        .filter_map(|khz| openatc_core::stations::tuned(&nearby, khz))
        .map(|station| &station.airport);
        // Local weather is needed before filing a plan as well as en route.
        for id in [
            &state.session.plan.departure,
            &state.session.plan.destination,
        ]
        .into_iter()
        .chain(tuned_airports)
        .chain(
            state
                .generated
                .operational_airports()
                .map(|airport| &airport.icao),
        ) {
            if !id.is_empty()
                && !airports
                    .iter()
                    .any(|airport: &openatc_core::state::WeatherAirport| airport.airport == *id)
                && let Some(location) = crate::radio::weather_location(&state, id)
            {
                airports.push(location);
            }
        }
    }
    result["weatherAirports"] = serde_json::to_value(airports).unwrap_or(Value::Null);
    Json(result)
}

pub async fn post_settings(
    AxumState(state): AxumState<Shared>,
    Json(mut proposed): Json<Settings>,
) -> Response {
    proposed.fade_delay = proposed.fade_delay.clamp(1.0, 60.0);
    proposed.faded_opacity = proposed.faded_opacity.clamp(0.1, 1.0);
    if let Err(error) = proposed.validate() {
        return bad_request(error.to_string());
    }
    let mut state = state.write().await;
    let path = state.config_dir.join("settings.json");
    if super::support::save_json(
        &path,
        &serde_json::to_value(&proposed).unwrap_or(Value::Null),
    )
    .is_err()
    {
        return bad_request(openatc_core::dialogue::say(
            "notice_cannot_save_configuration_bc3aa171",
            &[],
        ));
    }
    let previous_callsign = state.settings.callsign.clone();
    state.settings = proposed.clone();
    if state.session.clearance.is_none() {
        let current = state.session.plan.callsign.trim().to_owned();
        if current.is_empty() || current.eq_ignore_ascii_case(previous_callsign.trim()) {
            let callsign = proposed.callsign.trim();
            if !callsign.is_empty() {
                state.session.plan.callsign = callsign.to_ascii_uppercase();
                let registration = state.session.plan.registration.trim();
                if registration.is_empty()
                    || registration.eq_ignore_ascii_case(previous_callsign.trim())
                {
                    let callsign = state.session.plan.callsign.clone();
                    state.session.plan.registration.clone_from(&callsign);
                }
            }
        }
    }
    state.apply_settings_callsign();
    let controllers_path = state.config_dir.join("controllers.json");
    let _ = super::support::save_json(
        &controllers_path,
        &serde_json::to_value(&state.controllers).unwrap_or(Value::Null),
    );
    Json(serde_json::to_value(&proposed).unwrap_or(Value::Null)).into_response()
}

pub async fn post_simulator_root(
    AxumState(state): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let Some(root) = body.get("root").and_then(Value::as_str) else {
        return bad_request(openatc_core::dialogue::say(
            "notice_invalid_x_plane_folder_80d5409f",
            &[],
        ));
    };
    if !std::path::Path::new(root).is_dir() {
        return bad_request(openatc_core::dialogue::say(
            "notice_invalid_x_plane_folder_80d5409f",
            &[],
        ));
    }
    let mut state = state.write().await;
    if state.settings.simulator_root.is_empty() {
        root.clone_into(&mut state.settings.simulator_root);
        let path = state.config_dir.join("settings.json");
        let _ = super::support::save_json(
            &path,
            &serde_json::to_value(&state.settings).unwrap_or(Value::Null),
        );
    }
    Json(json!({"accepted": true})).into_response()
}

pub use super::surface::post_telemetry;

pub async fn debug_cancel_actions(AxumState(shared): AxumState<Shared>) -> Response {
    let mut state = shared.write().await;
    if !state.settings.dev_mode {
        return bad_request(openatc_core::dialogue::say(
            "notice_debug_mode_required_259698ff",
            &[],
        ));
    }
    state.session_generation = state.session_generation.wrapping_add(1);
    state.pending_copilot_reply = None;
    state.debug_jump = None;
    state.session.crew_actions.clear();
    state.crew.batch = None;
    state.crew.pushback = None;
    state.crew.checklist = None;
    state.crew.social.clear();
    state.crew.ground_taxi_origin = None;
    state.request_memory.cancel_pending();
    state.session.taxi_clearance.holding_channel = 0;
    Json(json!({"accepted":true})).into_response()
}

pub async fn post_demo(AxumState(state): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    let mut state = state.write().await;
    if !state.session.demo {
        return bad_request(openatc_core::dialogue::say(
            "notice_demo_controls_are_unavailable_while_connected_to_x_plane_816ca12f",
            &[],
        ));
    }
    let phase = body.get("phase").and_then(Value::as_i64).unwrap_or(0);
    if !(0..=10).contains(&phase) {
        return bad_request(openatc_core::dialogue::say(
            "notice_invalid_phase_34dd4dac",
            &[],
        ));
    }
    let phase = match phase {
        1 => PhaseCode::Clearance,
        2 => PhaseCode::Taxi,
        3 => PhaseCode::Departure,
        4 => PhaseCode::Cruise,
        5 => PhaseCode::Arrival,
        6 => PhaseCode::Approach,
        7 => PhaseCode::Landed,
        8 => PhaseCode::Pushback,
        9 => PhaseCode::TaxiIn,
        10 => PhaseCode::Finished,
        _ => PhaseCode::Parked,
    };
    let Ok(telemetry) =
        serde_json::from_value(body.get("telemetry").cloned().unwrap_or(Value::Null))
    else {
        return bad_request(openatc_core::dialogue::say(
            "notice_invalid_telemetry_116192a2",
            &[],
        ));
    };
    state.session.telemetry = telemetry;
    state.session.phase = phase;
    state.session.has_departed = !state.session.telemetry.on_ground;
    let snapshot = snapshot(&state.session, &state.settings);
    Json(snapshot).into_response()
}

pub async fn session_reset(AxumState(state): AxumState<Shared>) -> Json<Value> {
    let mut state = state.write().await;
    reset_flight(&mut state);
    Json(snapshot(&state.session, &state.settings))
}

fn reset_flight(state: &mut crate::EngineState) {
    state.debug_jump = None;
    state.session_generation = state.session_generation.wrapping_add(1);
    state.traffic_seed = crate::traffic_seed();
    state.pending_copilot_reply = None;
    state.generated = Default::default();
    let telemetry = state.session.telemetry.clone();
    let demo = state.session.demo;
    let next_sequence = state.session.next_sequence;
    state.session = State::default();
    state.apply_settings_callsign();
    state.atis_logged.clear();
    state.flow = super::flight::FlightFlow::default();
    state.crew.checklist = None;
    state.crew.batch = None;
    state.crew.social.clear();
    state.crew.pushback = None;
    state.request_memory = Default::default();
    state.cabin.reset();
    state.mission = Default::default();
    state.traffic = Default::default();
    state.aircraft = Default::default();
    state.atc = Default::default();
    state.parking = Default::default();
    state.initiative = Default::default();
    state.expectations.clear();
    state.airport = None;
    state.arrival_airport = None;
    state.arrival_profile = None;
    state.session.next_sequence = next_sequence;
    state.session.telemetry = telemetry;
    state.session.demo = demo;
    if demo {
        "09".clone_into(&mut state.session.plan.runway);
    }
}

pub async fn session_save(AxumState(state): AxumState<Shared>) -> Response {
    let state = state.read().await;
    let path = state.config_dir.join("session.json");
    let value = serde_json::to_value(&state.session).unwrap_or(Value::Null);
    if super::support::save_json(&path, &value).is_err() {
        return bad_request(openatc_core::dialogue::say(
            "notice_cannot_save_configuration_bc3aa171",
            &[],
        ));
    }
    Json(json!({"saved": true})).into_response()
}

pub async fn session_load(AxumState(state): AxumState<Shared>) -> Response {
    let path = state.read().await.config_dir.join("session.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return bad_request(openatc_core::dialogue::say(
            "notice_no_saved_session_18c2ddcf",
            &[],
        ));
    };
    let Ok(restored) = serde_json::from_str::<State>(&text) else {
        return bad_request(openatc_core::dialogue::say(
            "notice_no_saved_session_18c2ddcf",
            &[],
        ));
    };
    let mut state = state.write().await;
    if !state.session.demo {
        return bad_request(openatc_core::dialogue::say(
            "notice_cannot_restore_over_a_connected_simulator_flight_1bcb59f1",
            &[],
        ));
    }
    state.session = restored;
    state.apply_settings_callsign();
    state.session.taxi_clearance = TaxiClearance::default();
    let snapshot = snapshot(&state.session, &state.settings);
    Json(snapshot).into_response()
}

/// A filed plan keeps its own callsign; the settings value only fills an empty one.
fn fill_plan_callsign(plan: &mut openatc_core::state::FlightPlan, fallback: &str) {
    if !plan.callsign.trim().is_empty() {
        return;
    }
    let callsign = fallback.trim();
    if callsign.is_empty() {
        return;
    }
    plan.callsign = callsign.to_ascii_uppercase();
    if plan.registration.trim().is_empty() {
        plan.registration.clone_from(&plan.callsign);
    }
}

pub async fn post_plan(
    AxumState(state): AxumState<Shared>,
    Json(mut plan): Json<openatc_core::state::FlightPlan>,
) -> Response {
    let shared = state.clone();
    let (root, generation, config) = {
        let state = state.read().await;
        (
            state.settings.simulator_root.clone(),
            state.session_generation,
            state.config_dir.clone(),
        )
    };
    fill_plan_callsign(&mut plan, &state.read().await.settings.callsign.clone());
    let workers = state.read().await.workers.clone();
    let loaded = workers
        .run(move || {
            let airport = openatc_core::airport::load_airport_from_simulator(
                &root,
                &plan.destination,
                &|p| std::path::Path::new(p).is_file(),
                &|p| std::fs::read_to_string(p).map_err(|e| e.to_string()),
            )
            .ok();
            let mora = openatc_core::navigation::Mora::load(&root);
            let profile = airport.as_ref().and_then(|airport| {
                openatc_core::arrival::ArrivalAirport::load(&root, airport.clone()).ok()
            });
            let policy = super::instructions::VerticalPolicy::load(&config)?;
            let selected_approach = if plan.approach.is_empty() && policy.select_runway_ils {
                profile
                    .as_ref()
                    .and_then(|profile| profile.default_ils(&plan.arrival_runway))
            } else {
                None
            };
            if let Some(approach) = &selected_approach {
                plan.approach.clone_from(approach);
            }
            openatc_core::navigation::enrich(&mut plan, &root);
            Ok::<_, String>((
                plan,
                airport,
                mora,
                profile,
                policy,
                selected_approach.is_some(),
            ))
        })
        .await;
    let Ok(Ok((plan, arrival_airport, mora, profile, policy, selected_approach))) = loaded else {
        return bad_request(openatc_core::dialogue::say(
            "notice_could_not_load_flight_navigation_data_b01baf21",
            &[],
        ));
    };
    if validate_flight_plan(&plan).is_err() {
        return bad_request(openatc_core::dialogue::say(
            "notice_check_icao_codes_callsign_and_initial_cruise_altitudes_dce87a98",
            &[],
        ));
    }
    let mut state = state.write().await;
    if state.session_generation != generation {
        return bad_request(openatc_core::dialogue::say(
            "notice_flight_changed_while_navigation_data_was_loading_dbd22f76",
            &[],
        ));
    }
    if state.session.phase != PhaseCode::Parked && state.session.phase != PhaseCode::Finished {
        return bad_request(openatc_core::dialogue::say(
            "notice_finish_or_reset_this_flight_before_replacing_its_plan_06cba76b",
            &[],
        ));
    }
    if state.session.phase == PhaseCode::Finished {
        reset_flight(&mut state);
    }
    // Loading the dispatch plan at the gate must not erase a crew exchange in progress.
    state.flow = Default::default();
    state.flow.announce_approach = selected_approach;
    state.session.plan = plan;
    state.arrival_airport = arrival_airport;
    state.arrival_profile = profile;
    state.vertical_policy = policy;
    state.mora = mora;
    let generated = state.settings.generated_traffic;
    let snapshot = snapshot(&state.session, &state.settings);
    drop(state);
    if generated {
        start_generated(shared);
    }
    Json(snapshot).into_response()
}

pub async fn post_parking(
    AxumState(state): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let Some(stand) = body.get("stand").and_then(Value::as_str) else {
        return bad_request(openatc_core::dialogue::say(
            "notice_invalid_stand_fc950a75",
            &[],
        ));
    };
    let mut state = state.write().await;
    let airport = state.arrival_airport.as_ref().or_else(|| {
        state
            .airport
            .as_ref()
            .filter(|a| a.icao == state.session.plan.destination)
    });
    let Some((airport, size)) = airport.zip(openatc_core::parking::aircraft_size(&state.session))
    else {
        return bad_request(openatc_core::dialogue::say(
            "notice_load_the_arrival_airport_and_aircraft_size_before_select_ccd7c916",
            &[],
        ));
    };
    let Some(parking) = airport.parking.iter().find(|p| p.name == stand) else {
        return bad_request(openatc_core::dialogue::say(
            "notice_unknown_arrival_stand_8d191566",
            &[],
        ));
    };
    if !openatc_core::parking::fits(parking, size) {
        return bad_request(openatc_core::dialogue::say(
            "notice_this_stand_is_too_small_or_has_no_published_size_1b3f0c4e",
            &[],
        ));
    }
    if openatc_core::parking::occupied(airport, parking, &state.session.telemetry, size)
        != Some(false)
    {
        return bad_request(openatc_core::dialogue::say(
            "notice_stand_occupancy_could_not_be_cleared_select_another_stan_dfab9d7d",
            &[],
        ));
    }
    stand.clone_into(&mut state.session.plan.arrival_stand);
    Json(json!({"accepted": true})).into_response()
}

/// Load destination geometry without changing local controller services.
pub async fn post_airport_arrival(
    AxumState(shared): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let icao = body
        .get("icao")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_uppercase();
    if icao.len() != 4 || !icao.chars().all(|c| c.is_ascii_alphanumeric()) {
        return bad_request(openatc_core::dialogue::say(
            "notice_set_a_destination_icao_in_flight_plan_ed0c4fad",
            &[],
        ));
    }
    let (root, transition) = {
        let state = shared.read().await;
        (
            state.settings.simulator_root.clone(),
            region_for(&state.regions, &icao).transition_feet,
        )
    };
    let exists = |path: &str| std::path::Path::new(path).exists();
    let read = |path: &str| std::fs::read_to_string(path).map_err(|error| error.to_string());
    let airport = match load_airport_from_simulator(&root, &icao, &exists, &read) {
        Ok(a) => a,
        Err(error) => return bad_request(&error),
    };
    let mut data = openatc_core::arrival::ArrivalAirport {
        airport,
        transition_feet: transition,
        ..Default::default()
    };
    let custom = format!("{root}/Custom Data/CIFP/{icao}.dat");
    let path = if exists(&custom) {
        custom
    } else {
        format!("{root}/Resources/default data/CIFP/{icao}.dat")
    };
    if let Ok(text) = read(&path) {
        data.read_cifp(&text);
    }
    Json(data).into_response()
}

pub async fn post_airport_local(state: AxumState<Shared>, Json(mut body): Json<Value>) -> Response {
    if !body.is_object() {
        return bad_request(openatc_core::dialogue::say(
            "notice_airport_request_must_be_an_object_a290c7e5",
            &[],
        ));
    }
    body["active"] = Value::Bool(true);
    post_airport_load(state, Json(body)).await
}

pub async fn post_airport_load(
    AxumState(state): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let Some(icao) = body.get("icao").and_then(Value::as_str) else {
        return bad_request(openatc_core::dialogue::say(
            "notice_enter_a_four_character_icao_384e3e19",
            &[],
        ));
    };
    if icao.len() != 4 || !icao.chars().all(|value| value.is_ascii_alphanumeric()) {
        return bad_request(openatc_core::dialogue::say(
            "notice_enter_a_four_character_icao_384e3e19",
            &[],
        ));
    }
    // Normalize station identifiers to uppercase letters and digits.
    let icao = icao.to_uppercase();
    let demo = body.get("demo").and_then(Value::as_bool).unwrap_or(false);
    let root = state.read().await.settings.simulator_root.clone();
    let loaded = if demo {
        demo_airport()
    } else {
        let exists = |path: &str| std::path::Path::new(path).exists();
        let read = |path: &str| std::fs::read_to_string(path).map_err(|error| error.to_string());
        match load_airport_from_simulator(&root, &icao, &exists, &read) {
            Ok(airport) => airport,
            Err(error) => return bad_request(error.as_str()),
        }
    };
    if body.get("active").and_then(Value::as_bool).unwrap_or(true) {
        state.write().await.airport = Some(loaded.clone());
    }
    let start = {
        let state = state.read().await;
        state.settings.generated_traffic
            && (state.session.plan.departure.is_empty() || state.session.plan.departure == icao)
    };
    if start {
        start_generated(state.clone());
    }
    Json(serde_json::to_value(&loaded).unwrap_or(Value::Null)).into_response()
}

/// Start controlled traffic preparation as soon as the departure airport or plan is known,
/// instead of waiting for the first telemetry frame. Every actor comes from a complete flight
/// plan; parked visual previews must never occupy a stand or taxi route.
fn start_generated(shared: Shared) {
    tokio::spawn(async move {
        crate::generated::prepare(shared).await;
    });
}

pub async fn post_online_weather(
    AxumState(state): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let stations = body
        .get("stations")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if !valid_stations(&stations) {
        return bad_request(openatc_core::dialogue::say(
            "notice_enter_station_icaos_separated_by_commas_615c3757",
            &[],
        ));
    }
    match fetch_metar(&state.read().await.client, &stations).await {
        Ok(reports) => Json(reports).into_response(),
        Err(error) => bad_request(error.as_str()),
    }
}

/// Simulator surface reports. Online weather is a separate planning-only endpoint.
pub async fn post_weather(
    AxumState(shared): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let airports = body["stations"].as_str().unwrap_or("");
    if !valid_stations(airports) {
        return bad_request(openatc_core::dialogue::say(
            "notice_enter_station_icaos_separated_by_commas_7c3c1240",
            &[],
        ));
    }
    let state = shared.read().await;
    let mut reports = Vec::new();
    for airport in airports.split(',') {
        let Some((sample, _)) = state
            .sim_weather
            .get(airport)
            .filter(|(_, t)| t.elapsed().as_secs() < 45)
        else {
            return bad_request(format!(
                "Simulator surface weather unavailable for {airport}"
            ));
        };
        reports.push(sample.clone());
    }
    Json(json!({"source":"simulator","reports":reports})).into_response()
}

fn valid_stations(stations: &str) -> bool {
    let parts: Vec<&str> = stations.split(',').collect();
    if parts.is_empty() || parts.len() > 40 {
        return false;
    }
    parts
        .iter()
        .all(|part| part.len() == 4 && part.chars().all(|value| value.is_ascii_alphanumeric()))
}

pub async fn fetch_metar(client: &reqwest::Client, stations: &str) -> Result<Value, String> {
    let reply = client
        .get(format!(
            "https://aviationweather.gov/api/data/metar?ids={stations}&format=json&hours=2"
        ))
        .send()
        .await
        .map_err(|_| "Weather download failed or no current reports are available.".to_owned())?;
    if !reply.status().is_success() {
        return Err("Weather download failed or no current reports are available.".to_owned());
    }
    let reports: Value = reply
        .json()
        .await
        .map_err(|_| "Invalid weather response".to_owned())?;
    if !reports.is_array() {
        return Err("Invalid weather response".to_owned());
    }
    Ok(reports)
}

pub async fn post_simbrief(
    AxumState(state): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let mut identifier = body
        .get("userid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if identifier.is_empty() {
        identifier = state.read().await.settings.simbrief_id.clone();
    }
    if identifier.is_empty()
        || identifier.len() > 12
        || !identifier.chars().all(|value| value.is_ascii_digit())
    {
        return bad_request(openatc_core::dialogue::say(
            "notice_enter_your_numeric_simbrief_pilot_id_in_settings_c61590ed",
            &[],
        ));
    }
    let client = state.read().await.client.clone();
    let reply = client
        .get(format!(
            "https://www.simbrief.com/api/xml.fetcher.php?userid={identifier}&json=1"
        ))
        .send()
        .await;
    let Ok(reply) = reply else {
        return bad_request(openatc_core::dialogue::say(
            "notice_simbrief_download_failed_check_pilot_id_and_generate_a_f_9cc00b82",
            &[],
        ));
    };
    if !reply.status().is_success() {
        return bad_request(openatc_core::dialogue::say(
            "notice_simbrief_download_failed_check_pilot_id_and_generate_a_f_9cc00b82",
            &[],
        ));
    }
    let Ok(document) = reply.json::<Value>().await else {
        return bad_request(openatc_core::dialogue::say(
            "notice_simbrief_download_failed_check_pilot_id_and_generate_a_f_9cc00b82",
            &[],
        ));
    };
    match parse_simbrief(&document) {
        Ok(plan) => Json(serde_json::to_value(&plan).unwrap_or(Value::Null)).into_response(),
        Err(error) => bad_request(error.as_str()),
    }
}

pub async fn post_transcribe(
    AxumState(state): AxumState<Shared>,
    request: axum::extract::Request,
) -> Response {
    let (client, stt_url, stt_model) = {
        let state = state.read().await;
        (
            state.client.clone(),
            state.settings.stt_url.clone(),
            state.settings.stt_model.clone(),
        )
    };
    let multipart = request
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .starts_with("multipart/");
    let mut audio: Vec<u8> = Vec::new();
    if multipart {
        let Ok(mut fields) = axum::extract::Multipart::from_request(request, &()).await else {
            return bad_request(openatc_core::dialogue::say(
                "notice_invalid_multipart_body_fd6146b9",
                &[],
            ));
        };
        while let Ok(Some(field)) = fields.next_field().await {
            if field.name() == Some("file") {
                audio = field.bytes().await.unwrap_or_default().to_vec();
            }
        }
    } else {
        audio = match axum::body::to_bytes(request.into_body(), 32 * 1024 * 1024).await {
            Ok(bytes) => bytes.to_vec(),
            Err(_) => {
                return bad_request(openatc_core::dialogue::say(
                    "notice_invalid_audio_body_3bfa74cf",
                    &[],
                ));
            }
        };
    }
    let form = reqwest::multipart::Form::new()
        .part(
            "file",
            reqwest::multipart::Part::bytes(audio)
                .file_name("request.wav")
                .mime_str("audio/wav")
                .unwrap_or(reqwest::multipart::Part::bytes(Vec::new())),
        )
        .text("model", stt_model);
    let mut call = client.post(format!("{stt_url}/v1/audio/transcriptions"));
    if let Ok(key) = std::env::var("OPENATC_STT_KEY") {
        call = call.header("Authorization", format!("Bearer {key}"));
    }
    let reply = call.multipart(form).send().await;
    let Ok(reply) = reply else {
        return bad_request(
            openatc_core::dialogue::say(
                "notice_stt_request_failed_check_endpoint_model_and_openatc_stt_1016f227",
                &[],
            )
            .clone(),
        );
    };
    if !reply.status().is_success() {
        return bad_request(
            openatc_core::dialogue::say(
                "notice_stt_request_failed_check_endpoint_model_and_openatc_stt_1016f227",
                &[],
            )
            .clone(),
        );
    }
    let Ok(mut transcription) = reply.json::<Value>().await else {
        return bad_request(
            openatc_core::dialogue::say(
                "notice_stt_request_failed_check_endpoint_model_and_openatc_stt_1016f227",
                &[],
            )
            .clone(),
        );
    };
    if transcription.get("text").and_then(Value::as_str).is_none()
        && let Some(map) = transcription.as_object_mut()
    {
        map.insert("text".to_owned(), Value::String(String::new()));
    }
    Json(transcription).into_response()
}

/// Voice, delivery and base speed for one TTS speaker.
fn tts_voice(body: &Value, settings: &Settings, speaker: &str) -> (String, String, f32) {
    let mut voice = body
        .get("voice")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if voice.is_empty() {
        voice = match speaker {
            "copilot" => settings.copilot_voice.clone(),
            "pilot" => settings.pilot_voice.clone(),
            "cabin" => settings.attendant_voice.clone(),
            "ground" => settings.ground_voice.clone(),
            _ => settings.voice.clone(),
        };
    }
    if voice.is_empty() {
        "alloy".clone_into(&mut voice);
    }
    let mut delivery = body
        .get("delivery")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if delivery.is_empty() {
        delivery = match speaker {
            "copilot" => delivery_name(settings.copilot_delivery).to_owned(),
            "pilot" | "cabin" | "ground" => "standard".to_owned(),
            _ => delivery_name(settings.controller_delivery).to_owned(),
        };
    }
    // JSON hands us f64; speech speeds are small sane numbers, so this is exact.
    #[allow(clippy::cast_possible_truncation)]
    let mut speed = body.get("speed").and_then(Value::as_f64).unwrap_or(0.0) as f32;
    if speed <= 0.0 {
        speed = match speaker {
            "copilot" => settings.copilot_speed,
            "pilot" => settings.pilot_speed,
            "cabin" | "ground" => 1.0,
            _ => f32::midpoint(settings.controller_speed_min, settings.controller_speed_max),
        };
    }
    (voice, delivery, speed)
}

fn speech_effects(settings: &Settings, speaker: &str, received_radio: bool) -> Value {
    let enabled = settings.radio_effects
        && match speaker {
            "copilot" => false,
            "pilot" => received_radio,
            "cabin" => settings.radio_fx_cabin,
            "ground" => settings.radio_fx_ground,
            _ => true,
        };
    json!({
        "hiss": if enabled { settings.radio_hiss } else { 0.0 },
        "crackle": if enabled { settings.radio_crackle } else { 0.0 },
        "static": if enabled { settings.radio_static } else { 0.0 },
        "bandpass": enabled && settings.radio_bandpass,
    })
}

pub async fn post_speak(AxumState(state): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    let Some(text) = body.get("text").and_then(Value::as_str) else {
        return bad_request(openatc_core::dialogue::say(
            "notice_speech_text_too_long_b4830210",
            &[],
        ));
    };
    if text.len() > 4000 {
        return bad_request(openatc_core::dialogue::say(
            "notice_speech_text_too_long_b4830210",
            &[],
        ));
    }
    let text = text.to_owned();
    let mut speaker = body
        .get("speaker")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if speaker.is_empty() {
        speaker = if body
            .get("copilot")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "copilot".to_owned()
        } else {
            "atc".to_owned()
        };
    }
    if !["atc", "copilot", "pilot", "cabin", "ground"].contains(&speaker.as_str()) {
        return bad_request(openatc_core::dialogue::say(
            "notice_unknown_speaker_8cd85329",
            &[],
        ));
    }
    let settings = state.read().await.settings.clone();
    let client = state.read().await.client.clone();
    let _ = refresh_voices(&state, false).await;
    let (mut voice, delivery, mut speed) = tts_voice(&body, &settings, &speaker);
    {
        let current = state.read().await;
        if current.voices_source == settings.tts_url && !current.voices.is_empty() {
            let allowed: Vec<_> = current
                .voices
                .iter()
                .filter(|v| !settings.english_voices_only || v.english)
                .collect();
            if !allowed.iter().any(|v| v.id == voice) {
                if allowed.is_empty() {
                    return bad_request(openatc_core::dialogue::say(
                        "notice_no_voices_match_the_language_setting_b249173f",
                        &[],
                    ));
                }
                let index = match speaker.as_str() {
                    "copilot" => 1,
                    "pilot" => 2,
                    "cabin" => 3,
                    "ground" => 4,
                    _ => 0,
                };
                voice = allowed[index % allowed.len()].id.clone();
            }
        }
    }
    let urgent = body.get("urgent").and_then(Value::as_bool).unwrap_or(false);
    let preset = delivery_preset(&delivery);
    let automated_atis = matches!(delivery.as_str(), "atis" | "atis-natural");
    let mut sentence_pause = preset.sentence_pause;
    let mut clause_pause = preset.clause_pause;
    if urgent {
        speed = 1.22;
        sentence_pause = 0.06;
        clause_pause = 0.02;
    }
    speed = if automated_atis {
        sentence_pause = 0.08;
        clause_pause = 0.04;
        settings.atis_speed
    } else {
        (settings.tts_speed * speed).clamp(0.5, 2.0)
    };
    let effects = speech_effects(
        &settings,
        &speaker,
        body.get("receivedRadio")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    let (callsign, airports, pronunciation, traffic_codes, group_numbers) = {
        let current = state.read().await;
        let mut airports = Vec::new();
        for token in text.split_whitespace() {
            let id = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            if id.len() == 4
                && id
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && let Some(station) = current
                    .stations
                    .iter()
                    .find(|s| s.airport == id && !s.airport_name.is_empty())
            {
                airports.push((id.to_owned(), station.airport_name.clone()));
            } else if let Some(airport) = current.map_airports.iter().find(|a| a.id == id) {
                airports.push((id.to_owned(), airport.name.clone()));
            }
        }
        let traffic_codes = current
            .session
            .generated_traffic
            .iter()
            .map(|frame| frame.target.callsign.clone())
            .collect::<Vec<_>>();
        (
            current.session.plan.callsign.clone(),
            airports,
            current.pronunciation.clone(),
            traffic_codes,
            current.settings.callsign_numbers == CallsignNumbers::Group,
        )
    };
    let mut spoken_text = openatc_core::radiotelephony::spoken_with_pronunciation(
        &text,
        &callsign,
        &airports,
        &pronunciation,
    );
    for code in traffic_codes {
        spoken_text =
            openatc_core::radiotelephony::spoken_flight_number(&spoken_text, &code, group_numbers);
    }
    let tts_body = json!({
        "model": settings.tts_model,
        "voice": voice,
        "input": spoken_text,
        "response_format": "wav",
        "speed": speed,
        "sentence_pause": sentence_pause,
        "clause_pause": clause_pause,
        "effects": effects,
    });
    let mut call = client.post(format!("{}/v1/audio/speech", settings.tts_url));
    if let Ok(key) = std::env::var("OPENATC_TTS_KEY") {
        call = call.header("Authorization", format!("Bearer {key}"));
    }
    let reply = call.json(&tts_body).send().await;
    let reply = match reply {
        Ok(reply) => reply,
        Err(error) => {
            eprintln!(
                "OpenATC TTS transport failure: speaker={speaker} voice={voice} model={} text={text:?} spoken={spoken_text:?} error={error}",
                settings.tts_model
            );
            return bad_request(openatc_core::dialogue::say(
                "notice_tts_connection_failed_see_engine_log_for_the_failed_text_fa6fab84",
                &[],
            ));
        }
    };
    if !reply.status().is_success() {
        let status = reply.status();
        eprintln!(
            "OpenATC TTS provider failure: status={status} speaker={speaker} voice={voice} model={} text={text:?} spoken={spoken_text:?}",
            settings.tts_model
        );
        return bad_request(
            openatc_core::dialogue::say(
                "notice_tts_request_failed_check_endpoint_model_and_openatc_tts_0db775d1",
                &[],
            )
            .clone(),
        );
    }
    let Ok(bytes) = reply.bytes().await else {
        eprintln!(
            "OpenATC TTS response read failure: speaker={speaker} voice={voice} text={text:?}"
        );
        return bad_request(
            openatc_core::dialogue::say(
                "notice_tts_request_failed_check_endpoint_model_and_openatc_tts_0db775d1",
                &[],
            )
            .clone(),
        );
    };
    (
        StatusCode::OK,
        [("content-type", "audio/wav")],
        bytes.to_vec(),
    )
        .into_response()
}

/// Resolve the pending instruction on the engine, avoiding stale UI readbacks.
pub async fn post_auto_reply(AxumState(shared): AxumState<Shared>) -> Response {
    let request = { named_auto_reply(&*shared.read().await) };
    let Some(request) = request else {
        return bad_request(openatc_core::dialogue::say(
            "notice_no_atc_instruction_is_awaiting_your_reply_5eab90d4",
            &[],
        ));
    };
    post_request(
        AxumState(shared),
        Json(serde_json::to_value(request).unwrap_or(Value::Null)),
    )
    .await
}

fn named_auto_reply(state: &crate::EngineState) -> Option<Request> {
    let mut session = state.session.clone();
    if let Some(name) = state
        .stations
        .iter()
        .find(|station| {
            station.airport == session.plan.destination && !station.airport_name.trim().is_empty()
        })
        .map(|station| station.airport_name.trim())
        .or_else(|| {
            state
                .map_airports
                .iter()
                .find(|airport| airport.id == session.plan.destination)
                .map(|airport| airport.name.trim())
                .filter(|name| !name.is_empty())
        })
    {
        session.plan.destination = name.to_owned();
    }
    openatc_core::readback::auto_reply(&session).map(|mut request| {
        request.radio = session
            .transcript
            .iter()
            .rev()
            .find(|entry| entry.is_controller_instruction())
            .and_then(|entry| entry.radio)
            .unwrap_or_default();
        request
    })
}

pub struct PreparedReply {
    pub generation: u64,
    pub sequence: u32,
    pub frequency: i32,
    pub request: Request,
}

fn prepare_reply(state: &mut crate::EngineState, request: Request) {
    state.pending_copilot_reply = Some(PreparedReply {
        generation: state.session_generation,
        sequence: state.session.transcript.last().map_or(0, |t| t.sequence),
        frequency: state
            .session
            .telemetry
            .frequency(request.radio)
            .unwrap_or(0),
        request,
    });
}

/// Drop a prepared readback that belongs to an older controller instruction.
///
/// A prepared reply is only a playback handshake. Clear it when a newer controller
/// instruction makes it obsolete; it must not block the radio or the takeoff sequencer.
pub(crate) fn reconcile_pending_reply(state: &mut crate::EngineState) {
    let latest = state
        .session
        .transcript
        .iter()
        .rev()
        .find(|entry| entry.speaker == "ATC" && !entry.background)
        .map_or(0, |entry| entry.sequence);
    if let Some(pending) = state.pending_copilot_reply.as_ref()
        && (pending.generation != state.session_generation || latest > pending.sequence)
    {
        state.pending_copilot_reply = None;
    }
}

/// Record the crew transmission without advancing clearance or surface state.
pub async fn post_copilot_prepare(AxumState(shared): AxumState<Shared>) -> Response {
    let mut state = shared.write().await;
    if !state.settings.copilot_replies {
        return bad_request(openatc_core::dialogue::say(
            "notice_copilot_readbacks_are_disabled_38d1a964",
            &[],
        ));
    }
    reconcile_pending_reply(&mut state);
    if state.pending_copilot_reply.is_some() {
        return Json(json!({"state":snapshot(&state.session,&state.settings)})).into_response();
    }
    let Some(mut request) = named_auto_reply(&state) else {
        return bad_request(openatc_core::dialogue::say(
            "notice_no_atc_instruction_is_awaiting_your_reply_5eab90d4",
            &[],
        ));
    };
    let tag = SpeechTag {
        radio: Some(request.radio),
        voice: state.settings.copilot_voice.clone(),
        delivery: delivery_name(state.settings.copilot_delivery).into(),
        speed: state.settings.copilot_speed,
        ..Default::default()
    };
    add_transmission(&mut state.session, "COPILOT", &request.text, &tag);
    request.controller_initiated = true;
    prepare_reply(&mut state, request);
    Json(json!({"state":snapshot(&state.session,&state.settings)})).into_response()
}

/// Speak the new-controller check-in before submitting it to ATC.
pub async fn post_copilot_checkin(
    AxumState(shared): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let mut state = shared.write().await;
    let radio = match body.get("radio") {
        None => state.session.control_radio,
        Some(value) => match serde_json::from_value(value.clone()) {
            Ok(radio) => radio,
            Err(_) => {
                return bad_request(openatc_core::dialogue::say(
                    "notice_invalid_request_330509ca",
                    &[],
                ));
            }
        },
    };
    if !state.settings.copilot_replies || state.pending_copilot_reply.is_some() {
        return bad_request(openatc_core::dialogue::say(
            "notice_copilot_communication_is_unavailable_6fd03909",
            &[],
        ));
    }
    let stations = openatc_core::stations::nearby(&state.stations, &state.session.telemetry);
    let Some(station) = state
        .session
        .telemetry
        .frequency(radio)
        .and_then(|frequency| openatc_core::stations::tuned(&stations, frequency))
        .filter(|s| openatc_core::stations::station_serves(s, "checkin"))
    else {
        return bad_request(openatc_core::dialogue::say(
            "notice_no_receiving_controller_for_check_in_e227b0ef",
            &[],
        ));
    };
    let text = openatc_core::readback::checkin_text(&state.session, &station.name);
    let request = Request {
        radio,
        intent: "checkin".into(),
        text: text.clone(),
        controller_initiated: true,
        ..Default::default()
    };
    let tag = SpeechTag {
        radio: Some(radio),
        voice: state.settings.copilot_voice.clone(),
        delivery: delivery_name(state.settings.copilot_delivery).into(),
        speed: state.settings.copilot_speed,
        ..Default::default()
    };
    add_transmission(&mut state.session, "COPILOT", &text, &tag);
    prepare_reply(&mut state, request);
    Json(json!({"state":snapshot(&state.session,&state.settings)})).into_response()
}

/// Apply the prepared readback only after its playback has completed.
pub async fn post_copilot_reply(
    AxumState(shared): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let (request, source_sequence) = {
        let mut state = shared.write().await;
        if let Some(expected) = body.get("sequence")
            && state
                .pending_copilot_reply
                .as_ref()
                .is_none_or(|p| expected.as_u64() != Some(u64::from(p.sequence)))
        {
            return bad_request(openatc_core::dialogue::say(
                "notice_that_copilot_transmission_is_no_longer_pending_66a00d56",
                &[],
            ));
        }
        match state.pending_copilot_reply.take() {
            Some(prepared)
                if prepared.generation == state.session_generation
                    && Some(prepared.frequency)
                        == state.session.telemetry.frequency(prepared.request.radio)
                    && state.settings.copilot_replies =>
            {
                let source_sequence = readback_source_sequence(&state, prepared.sequence);
                (prepared.request, source_sequence)
            }
            _ => {
                return bad_request(openatc_core::dialogue::say(
                    "notice_no_prepared_copilot_reply_for_this_flight_9605a9ad",
                    &[],
                ));
            }
        }
    };
    let (id, generation, settings) = {
        let mut state = shared.write().await;
        let phase = state.session.phase;
        (
            state.request_memory.begin("atc", phase, &request.text),
            state.session_generation,
            state.settings.clone(),
        )
    };
    let response = atc_request(&shared, &settings, request).await;
    clear_readback(&mut *shared.write().await, source_sequence);
    record_response(&shared, id, generation, response).await
}

/// Find the controller instruction that a prepared copilot transmission answers.
fn readback_source_sequence(state: &crate::EngineState, prepared: u32) -> u32 {
    state
        .session
        .transcript
        .iter()
        .rev()
        .find(|entry| entry.sequence < prepared && entry.is_controller_instruction())
        .map_or(0, |entry| entry.sequence)
}

/// A spoken readback clears the outstanding reply marker. Without this, a
/// frequency handoff remains "awaiting readback" and suppresses the automatic
/// check-in and the next controller clearance.
fn clear_readback(state: &mut crate::EngineState, sequence: u32) {
    if let Some(entry) = state
        .session
        .transcript
        .iter_mut()
        .find(|entry| entry.sequence == sequence && entry.speaker == "ATC")
    {
        entry.pilot_reply.clear();
    }
}

/// Shared request handling: station roles, controller and crew replies, and readbacks.
pub async fn post_request(
    AxumState(shared): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let Ok(request) = serde_json::from_value::<Request>(body.clone()) else {
        return bad_request(openatc_core::dialogue::say(
            "notice_invalid_request_330509ca",
            &[],
        ));
    };
    if request.text.len() > 4000 {
        return bad_request(openatc_core::dialogue::say(
            "notice_message_too_long_dee7bc40",
            &[],
        ));
    }
    let role = if ["cabin", "ground", "copilot"].contains(&request.role.as_str()) {
        request.role.as_str()
    } else {
        "atc"
    };
    let (id, generation) = {
        let mut state = shared.write().await;
        let phase = state.session.phase;
        let text = if request.text.is_empty() {
            &request.intent
        } else {
            &request.text
        };
        (
            state.request_memory.begin(role, phase, text),
            state.session_generation,
        )
    };
    let response = handle_request(AxumState(shared.clone()), Json(body)).await;
    record_response(&shared, id, generation, response).await
}

async fn record_response(
    shared: &Shared,
    id: u64,
    generation: u64,
    response: Response,
) -> Response {
    let (parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 16 * 1024 * 1024).await else {
        return bad_request(openatc_core::dialogue::say(
            "notice_request_response_could_not_be_read_85e2a2a9",
            &[],
        ));
    };
    let result = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    let mut state = shared.write().await;
    if state.session_generation == generation {
        let actions: Vec<openatc_core::crew::Action> =
            serde_json::from_value(result["state"]["crewActions"].clone()).unwrap_or_default();
        state.request_memory.finish(id, &result, &actions);
    }
    Response::from_parts(parts, axum::body::Body::from(bytes))
}

async fn handle_request(AxumState(shared): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    let generation = shared.read().await.session_generation;
    let planning_plan = body.get("planningFlightPlan").cloned();
    let planning_callsign = body
        .get("planningCallsign")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut request: Request = match serde_json::from_value(body) {
        Ok(request) => request,
        Err(_) => {
            return bad_request(openatc_core::dialogue::say(
                "notice_invalid_request_330509ca",
                &[],
            ));
        }
    };
    if request.text.len() > 4000 {
        return bad_request(openatc_core::dialogue::say(
            "notice_message_too_long_dee7bc40",
            &[],
        ));
    }
    let mut role = request.role.clone();
    if role != "cabin" && role != "ground" && role != "copilot" {
        "atc".clone_into(&mut role);
    }
    if request.intent.is_empty() || request.intent == "conversation" {
        let interpreted = interpret_text(&request.text);
        request.intent = interpreted.intent;
        request.altitude_feet = interpreted.altitude_feet;
        request.waypoint = interpreted.waypoint;
    }
    request.role = role.clone();
    if role == "atc"
        && let Some(proposed) = planning_callsign
    {
        let mut state = shared.write().await;
        if state.session_generation != generation {
            return bad_request(openatc_core::dialogue::say(
                "notice_flight_was_reset_previous_request_discarded_c0a5c48d",
                &[],
            ));
        }
        if let Some(callsign) = openatc_core::ops::planning_callsign(&state.session, &proposed) {
            state.session.plan.callsign = callsign.to_ascii_uppercase();
        }
    }
    let settings = shared.read().await.settings.clone();
    if let Some(response) = crate::crew::request(&shared, &request).await {
        return response;
    }
    if role == "cabin" || role == "ground" {
        return crew_chat(&shared, &settings, &request, role == "cabin").await;
    }
    if role == "atc" {
        if let Err(error) = super::radio::ensure_index(&shared).await {
            return bad_request(error);
        }
        let state = shared.read().await;
        let stations = openatc_core::stations::nearby(&state.stations, &state.session.telemetry);
        let receiving = state
            .session
            .telemetry
            .frequency(request.radio)
            .and_then(|frequency| openatc_core::stations::tuned(&stations, frequency));
        if state.station_root != state.settings.simulator_root
            || !state.session.telemetry.radio_power
            || receiving.is_none_or(|s| s.service == "ATIS" || s.service == "Unicom")
        {
            return Json(json!({"result":{"accepted":false,"message":"","silent":true},"state":snapshot(&state.session,&state.settings)})).into_response();
        }
    }
    if role == "atc" {
        let state = shared.read().await;
        let name = state
            .stations
            .iter()
            .find(|s| s.airport == state.session.plan.destination && !s.airport_name.is_empty())
            .map(|s| &s.airport_name)
            .or_else(|| {
                state
                    .map_airports
                    .iter()
                    .find(|a| a.id == state.session.plan.destination)
                    .map(|a| &a.name)
            });
        if let Some(name) =
            name.filter(|name| request.text.to_lowercase().contains(&name.to_lowercase()))
        {
            let mut session = state.session.clone();
            session.plan.destination.clone_from(name);
            openatc_core::readback::prepare(&session, &mut request);
        } else {
            openatc_core::readback::prepare(&state.session, &mut request);
        }
    }
    let assistance_answer = super::discipline::assistance_answer(&*shared.read().await, &request);
    if request.intent == "conversation"
        && assistance_answer.is_none()
        && settings.ai_enabled
        && !settings.ai_model.is_empty()
    {
        match classify_request(&shared, &settings, &request, &role).await {
            Ok(mut classified) => {
                classified.radio = request.radio;
                request = classified;
            }
            Err(error) => return bad_request(error.as_str()),
        }
    }
    if shared.read().await.session_generation != generation {
        return bad_request(openatc_core::dialogue::say(
            "notice_flight_was_reset_previous_request_discarded_c0a5c48d",
            &[],
        ));
    }
    if role == "copilot" {
        return copilot_chat(&shared, &settings, &request).await;
    }
    if role == "atc" && request.intent == "clearance" {
        let mut state = shared.write().await;
        let feedback = match planning_plan {
            Some(value) => match serde_json::from_value::<openatc_core::state::FlightPlan>(value) {
                Ok(plan) => openatc_core::ops::flight_plan_clearance_feedback(&plan),
                Err(_) => Some(openatc_core::dialogue::say(
                    "clearance_flight_plan_incomplete",
                    &[
                        ("callsign", state.session.plan.callsign.clone()),
                        ("reason", "Check the flight plan fields.".into()),
                    ],
                )),
            },
            None => openatc_core::ops::flight_plan_clearance_feedback(&state.session.plan),
        }
        .or_else(|| openatc_core::ops::flight_plan_clearance_feedback(&state.session.plan));
        if state.session.clearance.is_none()
            && let Some(message) = feedback
        {
            let stations =
                openatc_core::stations::nearby(&state.stations, &state.session.telemetry);
            if let Some(station) = state
                .session
                .telemetry
                .frequency(request.radio)
                .and_then(|frequency| openatc_core::stations::tuned(&stations, frequency))
                .cloned()
            {
                let callsign = state.session.plan.callsign.clone();
                let pilot_tag = SpeechTag {
                    radio: Some(request.radio),
                    voice: settings.pilot_voice.clone(),
                    delivery: "standard".into(),
                    speed: settings.pilot_speed,
                    ..Default::default()
                };
                add_transmission(&mut state.session, &callsign, &request.text, &pilot_tag);
                return clearance_rejection(&mut state, &station, request.radio, message);
            }
        }
    }
    if request.intent == "clearance" {
        let state = shared.read().await;
        let id = &state.session.plan.destination;
        if let Some(name) = state
            .stations
            .iter()
            .find(|s| s.airport == *id && !s.airport_name.is_empty())
            .map(|s| &s.airport_name)
            .or_else(|| {
                state
                    .map_airports
                    .iter()
                    .find(|a| a.id == *id)
                    .map(|a| &a.name)
            })
        {
            request.text = request
                .text
                .split_whitespace()
                .map(|token| {
                    let bare = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
                    if bare == id {
                        token.replacen(bare, name, 1)
                    } else {
                        token.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    atc_request(&shared, &settings, request).await
}

fn social_reply(state: &crate::EngineState, text: &str, role: &str) -> Option<String> {
    let lower = text.to_lowercase();
    // A greeting must not swallow a control request in the same sentence.
    if state.crew.profile.controls.iter().any(|(id, control)| {
        control.role == role
            && openatc_core::dialogue::phrases(&format!("crew_request_{id}"))
                .iter()
                .any(|alias| lower.contains(alias))
    }) {
        return None;
    }
    let facts = std::collections::BTreeMap::from([("role".into(), json!(role))]);
    openatc_core::dialogue::matched_response("crew_social_", text, false, &facts)
        .map(|key| openatc_core::dialogue::say(&key, &[]))
}

pub(crate) async fn actor_chat(
    shared: &Shared,
    settings: &Settings,
    request: &Request,
) -> Response {
    if request.role == "copilot" {
        copilot_chat(shared, settings, request).await
    } else {
        crew_chat(shared, settings, request, request.role == "cabin").await
    }
}

async fn crew_chat(
    shared: &Shared,
    settings: &Settings,
    request: &Request,
    attendant: bool,
) -> Response {
    let generation = shared.read().await.session_generation;
    let crew = if attendant { "ATTENDANT" } else { "GROUND" };
    let voice = if attendant {
        &settings.attendant_voice
    } else {
        &settings.ground_voice
    };
    let voice = if voice.is_empty() { "alloy" } else { voice };
    let callsign = shared.read().await.session.plan.callsign.clone();
    let prompts = shared.read().await.prompts_text(attendant);
    let base = crate::support::fill_prompt(&prompts, &callsign);
    let personality = if attendant {
        &settings.attendant_personality
    } else {
        &settings.ground_personality
    };
    let off = openatc_core::dialogue::say("crew_chat_needs_ai_intent_classification_on", &[]);
    let busy = openatc_core::dialogue::say("crew_line_is_busy_try_again", &[]);
    let social = if settings.ai_enabled {
        None
    } else {
        social_reply(
            &*shared.read().await,
            &request.text,
            if attendant { "cabin" } else { "ground" },
        )
    };
    let unconfirmed_service = !attendant
        && [
            "connect ",
            "disconnect ",
            "remove ",
            "add ",
            "set ",
            "turn ",
            "can you remove ",
            "can you connect ",
            "can you disconnect ",
        ]
        .iter()
        .any(|prefix| request.text.trim().to_lowercase().starts_with(prefix));
    let cabin_report = if attendant && !settings.ai_enabled {
        super::cabin::report(&mut *shared.write().await, &request.text)
    } else {
        None
    };
    let mut reply_text = if let Some(report) = &cabin_report {
        report.clone()
    } else if unconfirmed_service {
        openatc_core::dialogue::say("crew_ground_service_unconfirmed", &[])
    } else if let Some(social) = &social {
        social.clone()
    } else {
        off.clone()
    };
    if cabin_report.is_some() || social.is_some() {
        reply_text = crate::crew::narrate(
            shared,
            if attendant { "cabin" } else { "ground" },
            &request.text,
            &reply_text,
            &[],
        )
        .await;
    }
    if cabin_report.is_none()
        && social.is_none()
        && !unconfirmed_service
        && settings.ai_enabled
        && !settings.ai_model.is_empty()
    {
        let system = if personality.is_empty() {
            base
        } else {
            format!("{personality} {base}")
        };
        let system = {
            let state = shared.read().await;
            let role = if attendant { "attendant" } else { "ground" };
            let phase = format!("{:?}", state.session.phase).to_lowercase();
            let live = state.prompts.guidance.get("crew_live").replace("{{facts}}", &json!({"phase":phase,"onGround":state.session.telemetry.on_ground,"groundSpeedKnots":state.session.telemetry.ground_speed_knots,"departure":state.session.plan.departure,"destination":state.session.plan.destination}).to_string());
            system
                + &live
                + &crate::context::prompt(&state, role, &request.text)
                + state.prompts.guidance.get("examples")
                + &if social.is_some() {
                    reply_text.clone()
                } else {
                    let samples = sample_slots();
                    state
                        .speech
                        .select(role, &["actor_conversation"], &phase, "icao", 4)
                        .into_iter()
                        .flat_map(|example| {
                            example
                                .say
                                .iter()
                                .take(2)
                                .map(|line| render_template(line, &samples))
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
        };
        let client = shared.read().await.client.clone();
        if let Ok(generated) = chat_complete(
            &client,
            &settings.ai_url,
            &settings.ai_model,
            0.7,
            &system,
            &format!("Captain: {}", request.text),
        )
        .await
        {
            let cleaned = clean_reply(generated);
            if !cleaned.is_empty() {
                let claims_action = !attendant
                    && [
                        "removed",
                        "connected",
                        "disconnected",
                        "chocks clear",
                        "power available",
                    ]
                    .iter()
                    .any(|claim| cleaned.to_lowercase().contains(claim));
                reply_text = if claims_action {
                    openatc_core::dialogue::say("crew_ground_service_unconfirmed", &[])
                } else {
                    cleaned
                };
            }
        }
        if reply_text == off {
            busy.clone_into(&mut reply_text);
        }
    }
    // Report unavailable AI replies only when AI is enabled.
    if settings.ai_enabled
        && !settings.ai_model.is_empty()
        && reply_text != off
        && reply_text != busy
    {
        // Preserve the usable model reply.
    } else if reply_text.is_empty() {
        busy.clone_into(&mut reply_text);
    }
    let ok = !unconfirmed_service
        && reply_text != off
        && reply_text != busy
        && !openatc_core::dialogue::phrases("crew_ground_service_unconfirmed")
            .contains(&reply_text);
    let mut state = shared.write().await;
    if state.session_generation != generation {
        return bad_request(openatc_core::dialogue::say(
            "notice_flight_was_reset_previous_request_discarded_c0a5c48d",
            &[],
        ));
    }
    let pilot = state.session.plan.callsign.clone();
    add_transmission(
        &mut state.session,
        pilot.as_str(),
        if request.text.is_empty() {
            request.role.as_str()
        } else {
            request.text.as_str()
        },
        &SpeechTag::default(),
    );
    add_transmission(
        &mut state.session,
        crew,
        &reply_text,
        &SpeechTag {
            voice: voice.to_owned(),
            delivery: "standard".to_owned(),
            speed: 1.0,
            ..Default::default()
        },
    );
    let result = json!({"accepted": ok, "message": reply_text});
    let snapshot = snapshot(&state.session, &state.settings);
    Json(json!({"result": result, "state": snapshot})).into_response()
}

async fn classify_request(
    shared: &Shared,
    settings: &Settings,
    request: &Request,
    role: &str,
) -> Result<Request, String> {
    let level = realism_from(settings);
    let preset = discipline_preset(
        level,
        match settings.congestion {
            Congestion::Off => "off",
            Congestion::Quiet => "quiet",
            Congestion::Busy => "busy",
        },
    );
    let prompts = shared.read().await.classify_prompt();
    let mut classify = prompts;
    {
        let state = shared.read().await;
        classify.push('\n');
        classify += state.prompts.guidance.get("classify_common");
        if preset == "relaxed" {
            classify.push('\n');
            classify += state.prompts.guidance.get("classify_relaxed");
        } else if preset == "real" {
            classify.push('\n');
            classify += state.prompts.guidance.get("classify_real");
        }
    }
    classify += &crate::context::prompt(&*shared.read().await, role, &request.text);
    let client = shared.read().await.client.clone();
    let content = match crate::llm::structured::<Value>(
        &client,
        &settings.ai_url,
        &settings.ai_model,
        0.0,
        &classify,
        &request.text,
    )
    .await
    {
        Ok(parsed) => parsed.to_string(),
        Err(crate::llm::StructuredError::Invalid(error)) => {
            eprintln!("ATC classification rejected: {error}");
            String::new()
        }
        Err(crate::llm::StructuredError::Service(_)) => {
            return Err(openatc_core::dialogue::say(
                "notice_ai_endpoint_unavailable",
                &[],
            ));
        }
    };
    let mut interpreted = crate::intent::parse_or_clarify(&content, request, role);
    if interpreted.intent == "readback" {
        openatc_core::readback::prepare(&shared.read().await.session, &mut interpreted);
    }
    Ok(interpreted)
}

async fn copilot_chat(shared: &Shared, settings: &Settings, request: &Request) -> Response {
    let social = if settings.ai_enabled {
        None
    } else {
        social_reply(&*shared.read().await, &request.text, "copilot")
    };
    let generation = shared.read().await.session_generation;
    let callsign = shared.read().await.session.plan.callsign.clone();
    let prompts = shared.read().await.copilot_chat_prompt();
    let mut system = if settings.copilot_personality.is_empty() {
        crate::support::fill_prompt(&prompts, &callsign)
    } else {
        format!(
            "{} {}",
            settings.copilot_personality,
            crate::support::fill_prompt(&prompts, &callsign)
        )
    };
    if request.intent != "conversation" {
        system += shared.read().await.prompts.guidance.get("copilot_redirect");
    }
    {
        let state = shared.read().await;
        let phase = format!("{:?}", state.session.phase).to_lowercase();
        system += &crate::context::prompt(&state, "copilot", &request.text);
        system += state.prompts.guidance.get("examples");
        system += &social.clone().unwrap_or_else(|| {
            few_shots(&state.speech, "copilot", &["actor_conversation"], &phase)
        });
    }
    let voice = if settings.copilot_voice.is_empty() {
        "alloy"
    } else {
        &settings.copilot_voice
    };
    let off = openatc_core::dialogue::say("copilot_chat_needs_ai_intent_classification_on", &[]);
    let busy = openatc_core::dialogue::say("copilot_line_is_busy_try_again", &[]);
    let mut reply_text = social.clone().unwrap_or_else(|| off.clone());
    if (social.is_none() || settings.llm_phrase_variety)
        && settings.ai_enabled
        && !settings.ai_model.is_empty()
    {
        let client = shared.read().await.client.clone();
        if let Ok(generated) = chat_complete(
            &client,
            &settings.ai_url,
            &settings.ai_model,
            0.7,
            &system,
            &format!("Captain: {}", request.text),
        )
        .await
        {
            let cleaned = clean_reply(generated);
            if !cleaned.is_empty() {
                reply_text = cleaned;
            }
        }
        if reply_text == off {
            busy.clone_into(&mut reply_text);
        }
    }
    let ok = reply_text != off && reply_text != busy;
    let mut state = shared.write().await;
    if state.session_generation != generation {
        return bad_request(openatc_core::dialogue::say(
            "notice_flight_was_reset_previous_request_discarded_c0a5c48d",
            &[],
        ));
    }
    let speaker = state.session.plan.callsign.clone();
    add_transmission(
        &mut state.session,
        speaker.as_str(),
        if request.text.is_empty() {
            request.role.as_str()
        } else {
            request.text.as_str()
        },
        &SpeechTag::default(),
    );
    add_transmission(
        &mut state.session,
        "COPILOT",
        &reply_text,
        &SpeechTag {
            voice: voice.to_owned(),
            delivery: "standard".to_owned(),
            speed: 1.0,
            ..Default::default()
        },
    );
    let result = json!({"accepted": ok, "message": reply_text});
    let snapshot = snapshot(&state.session, &state.settings);
    Json(json!({"result": result, "state": snapshot})).into_response()
}

fn chatter_exchanges() -> Vec<(String, String)> {
    vec![
        (
            openatc_core::dialogue::say("qfa_request_taxi", &[]),
            openatc_core::dialogue::say("qantas_taxi_approved", &[]),
        ),
        (
            openatc_core::dialogue::say("jst_ready_for_departure", &[]),
            openatc_core::dialogue::say("jetstar_hold_short_traffic_on_final", &[]),
        ),
        (
            openatc_core::dialogue::say("voz_request_altitude_fl", &[]),
            openatc_core::dialogue::say("velocity_maintain_flight_level", &[]),
        ),
        (
            openatc_core::dialogue::say("rxa_going_around", &[]),
            openatc_core::dialogue::say("rex_roger_go_around", &[]),
        ),
        (
            openatc_core::dialogue::say("fdx_on_the_gate_request_pushback", &[]),
            openatc_core::dialogue::say("fedex_pushback_approved", &[]),
        ),
    ]
}

/// Assign (or recall) the controller voice for one airspace, persisting new
/// assignments, and build its speech tag.
pub(crate) fn assign_airspace_controller(
    state: &mut super::EngineState,
    settings: &Settings,
    airspace: &str,
    delivery: &str,
) -> SpeechTag {
    let mut pool = openatc_core::ops::parse_voice_pool(&settings.voice_pool);
    let roster = (state.voices_source == settings.tts_url && !state.voices.is_empty())
        .then_some(state.voices.as_slice());
    if let Some(roster) = roster {
        pool.retain(|id| roster.iter().any(|v| v.id == *id));
        if pool.is_empty() {
            pool = roster.iter().map(|v| v.id.clone()).collect();
        }
    }
    if settings.english_voices_only {
        pool.retain(|id| {
            state
                .voices
                .iter()
                .find(|v| state.voices_source == settings.tts_url && v.id == *id)
                .map_or_else(|| openatc_core::ops::english_voice(id), |v| v.english)
        });
    }
    if pool.is_empty() && roster.is_none() {
        pool = openatc_core::ops::english_voice_pool("");
    }
    let mut identities = std::collections::BTreeSet::new();
    pool.retain(|voice| identities.insert(openatc_core::ops::voice_identity(voice).to_owned()));
    let roster_before = state.controllers.clone();
    let identity = openatc_core::ops::voice_identity;
    let unused: Vec<_> = pool
        .iter()
        .filter(|v| {
            !state
                .controllers
                .assignments
                .iter()
                .any(|(key, c)| key != airspace && identity(&c.voice) == identity(v))
        })
        .cloned()
        .collect();
    if let Some(saved) = state.controllers.assignments.get(airspace) {
        let valid = pool.iter().any(|v| identity(v) == identity(&saved.voice));
        if !valid {
            state.controllers.assignments.remove(airspace);
        }
    }
    if !unused.is_empty() {
        pool = unused;
    }
    let fallback = "alloy";
    let order = AssignOrder {
        key: airspace,
        pool: &pool,
        fallback_voice: fallback,
        delivery,
        speed_min: settings.controller_speed_min,
        speed_max: settings.controller_speed_max,
    };
    let mut rng = SimpleRng::new(rand::rng().random());
    let controller = assign_controller(&mut state.controllers, &order, &mut rng);
    if state.controllers != roster_before {
        let path = state.config_dir.join("controllers.json");
        let _ = super::support::save_json(
            &path,
            &serde_json::to_value(&state.controllers).unwrap_or(Value::Null),
        );
    }
    let position = airspace.split(':').nth(1).unwrap_or(airspace).to_owned();
    SpeechTag {
        radio: None,
        frequency: None,
        gap_ms: 0,
        chatter: false,
        position,
        voice: controller.voice.clone(),
        delivery: controller.delivery.clone(),
        speed: controller.speed,
        urgent: false,
    }
}

/// Runway state used to sequence the player's departure.
pub(crate) fn runway_sequence(
    state: &EngineState,
    airport: &openatc_core::airport::Airport,
    runway: &str,
) -> RunwaySequence {
    use crate::generated::RunwayUse;
    use crate::traffic::manager::RunwayState;
    let player_remaining = (state.session.phase == PhaseCode::Taxi
        && state.session.plan.runway == runway
        && state.session.taxi_clearance.approved)
        .then(|| {
            crate::traffic::manager::player_runway_remaining(
                airport,
                runway,
                &state.session.telemetry,
            )
        })
        .flatten();
    let ambient =
        state
            .ambient
            .runway_state_for(&airport.icao, runway, player_remaining, None, None);
    let generated = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)
        .map_or(RunwayUse::Clear, |candidate| {
            state.generated.runway_use(&format!(
                "RUNWAY/{}/{}/{}",
                airport.icao, candidate.first_name, candidate.second_name
            ))
        });
    if ambient == RunwayState::Landing || generated == RunwayUse::Landing {
        RunwaySequence::Landing
    } else if ambient == RunwayState::LineUp || generated == RunwayUse::LineUp {
        RunwaySequence::LineUp
    } else if ambient == RunwayState::Rolling || generated == RunwayUse::Departing {
        RunwaySequence::Rolling
    } else {
        RunwaySequence::Clear
    }
}

pub(crate) async fn atc_request(
    shared: &Shared,
    settings: &Settings,
    request: Request,
) -> Response {
    if let Err(error) = super::radio::ensure_index(shared).await {
        return bad_request(error);
    }
    if !request.controller_initiated {
        let _ = refresh_voices(shared, false).await;
    }
    if super::debug_jump::waiting(&mut *shared.write().await) {
        return bad_request(
            "A developer checkpoint is loading; wait for native positioning to finish.",
        );
    }
    let generation = shared.read().await.session_generation;
    if !request.controller_initiated && settings.congestion != Congestion::Off {
        let millis = if settings.congestion == Congestion::Busy {
            1000 + rand::rng().random_range(0..3000)
        } else {
            500 + rand::rng().random_range(0..1500)
        };
        tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
    }
    let mut realism = realism_from(settings);
    realism.require_frequency = false;
    let departure = shared.read().await.session.plan.departure.clone();
    let region = {
        let state = shared.read().await;
        region_for(&state.regions, &departure)
    };
    let units = units_for(settings, &departure, &region);
    let receiving = {
        let state = shared.read().await;
        let stations = openatc_core::stations::nearby(&state.stations, &state.session.telemetry);
        if state.station_root == state.settings.simulator_root
            && state.session.telemetry.radio_power
        {
            state
                .session
                .telemetry
                .frequency(request.radio)
                .and_then(|frequency| openatc_core::stations::tuned(&stations, frequency))
                .cloned()
        } else {
            None
        }
    };
    let Some(receiving) = receiving else {
        let state = shared.read().await;
        return Json(json!({"result":{"accepted":false,"message":"","silent":true},"state":snapshot(&state.session,&state.settings)})).into_response();
    };
    if receiving.service == "ATIS" || receiving.service == "Unicom" {
        let state = shared.read().await;
        return Json(json!({"result":{"accepted":false,"message":"","silent":true},"state":snapshot(&state.session,&state.settings)})).into_response();
    }
    // Load the receiving airport's operational data, not the airport browser selection.
    let mut operational_airport = None;
    if [
        "clearance",
        "start",
        "pushback",
        "start_pushback",
        "taxi",
        "backtrack",
        "gate",
        "ready",
        "progressive",
        "cross_runway",
        "landing",
        "approach",
        "departure_runway",
        "arrival_runway",
        "change_approach",
    ]
    .contains(&request.intent.as_str())
    {
        let root = shared.read().await.settings.simulator_root.clone();
        let id = match request.intent.as_str() {
            "departure_runway" => shared.read().await.session.plan.departure.clone(),
            "arrival_runway" | "change_approach" | "approach" | "landing" => {
                shared.read().await.session.plan.destination.clone()
            }
            _ => receiving.airport.clone(),
        };
        let cached = if request.controller_initiated {
            shared
                .read()
                .await
                .airport
                .as_ref()
                .filter(|airport| airport.icao == id)
                .cloned()
        } else {
            None
        };
        let workers = shared.read().await.workers.clone();
        if let Some(airport) = cached {
            operational_airport = Some(airport);
        } else if let Ok(Ok(airport)) = workers
            .run(move || {
                openatc_core::airport::load_airport_from_simulator(
                    &root,
                    &id,
                    &|p| std::path::Path::new(p).is_file(),
                    &|p| std::fs::read_to_string(p).map_err(|e| e.to_string()),
                )
            })
            .await
        {
            operational_airport = Some(airport);
        } else {
            return bad_request(openatc_core::dialogue::say(
                "notice_receiving_airport_operational_data_unavailable_eb037b27",
                &[],
            ));
        }
    }
    let mut state = shared.write().await;
    if state.session_generation != generation {
        return bad_request(openatc_core::dialogue::say(
            "notice_flight_was_reset_previous_request_discarded_c0a5c48d",
            &[],
        ));
    }

    if request.controller_initiated
        && request.intent == "cross_runway"
        && (state.session.taxi_clearance.sequence != request.clearance_sequence
            || state.session.taxi_clearance.stage() != openatc_core::state::SurfaceStage::AtHold)
    {
        return Json(json!({"result":{"accepted":false,"silent":true},"state":snapshot(&state.session,&state.settings)})).into_response();
    }

    let current_stations =
        openatc_core::stations::nearby(&state.stations, &state.session.telemetry);
    if state.station_root != state.settings.simulator_root
        || !state.session.telemetry.radio_power
        || !state
            .session
            .telemetry
            .frequency(request.radio)
            .and_then(|frequency| openatc_core::stations::tuned(&current_stations, frequency))
            .is_some_and(|s| {
                s.airport == receiving.airport
                    && s.khz == receiving.khz
                    && s.service == receiving.service
            })
    {
        return Json(json!({"result":{"accepted":false,"message":"","silent":true},"state":snapshot(&state.session,&state.settings)})).into_response();
    }
    if let Some(accepted) = crate::traffic::advisories::reply(&mut state, &receiving, &request) {
        return Json(json!({"result":{"accepted":accepted},"state":snapshot(&state.session,&state.settings)})).into_response();
    }
    if let Some(accepted) = super::discipline::answer_assistance(&mut state, &receiving, &request) {
        return Json(json!({"result":{"accepted":accepted},"state":snapshot(&state.session,&state.settings)})).into_response();
    }
    if request.intent == "unable" && crate::traffic::unable(&mut state, &receiving, &request) {
        return Json(
            json!({"result":{"accepted":true},"state":snapshot(&state.session,&state.settings)}),
        )
        .into_response();
    }
    if request.intent == "clearance"
        && state
            .session
            .clearance
            .as_ref()
            .is_some_and(|c| c.acknowledged)
        && state.session.phase != PhaseCode::Parked
    {
        return clearance_rejection(
            &mut state,
            &receiving,
            request.radio,
            openatc_core::dialogue::say("departure_clearance_is_already_active_read_back", &[]),
        );
    }
    if !openatc_core::stations::station_serves(&receiving, &request.intent) {
        let wanted = match request.intent.as_str() {
            "clearance" => "Clearance",
            "ready" | "backtrack" | "cross_runway" | "landing" => "Tower",
            _ => "Ground",
        };
        let station = state
            .stations
            .iter()
            .find(|s| {
                s.airport == receiving.airport
                    && openatc_core::stations::station_serves(s, &request.intent)
            })
            .cloned();
        let text = station.as_ref().map_or_else(
            || {
                openatc_core::dialogue::say(
                    "does_not_provide_that_service_no_published",
                    &[
                        ("name", (receiving.name).clone()),
                        ("wanted", (wanted).to_string()),
                    ],
                )
            },
            |s| {
                openatc_core::dialogue::say(
                    "contact_on_mhz",
                    &[
                        ("station", (s.name).clone()),
                        ("frequency", format!("{:.3}", f64::from(s.khz) / 1000.)),
                    ],
                )
            },
        );
        let mut tag = assign_airspace_controller(
            &mut state,
            settings,
            &super::flight::station_key(&receiving),
            "standard",
        );
        tag.position.clone_from(&receiving.name);
        tag.radio = Some(request.radio);
        add_transmission(&mut state.session, "ATC", &text, &tag);
        if let Some(station) = station {
            state.session.recommended_frequency_khz = station.khz;
            let callsign = state.session.plan.callsign.clone();
            if let Some(last) = state.session.transcript.last_mut() {
                last.pilot_reply = openatc_core::dialogue::say(
                    "pilot_frequency_readback",
                    &[
                        ("station", station.name),
                        (
                            "frequency",
                            format!("{:.3}", f64::from(station.khz) / 1000.0),
                        ),
                        ("callsign", callsign),
                    ],
                );
                state.session.frequency_sequence = last.sequence;
            }
        }
        return Json(json!({"result":{"accepted":false,"message":text},"state":snapshot(&state.session,&state.settings)})).into_response();
    }
    if ["weather", "altimeter"].contains(&request.intent.as_str()) {
        let Some((sample, _)) = state
            .sim_weather
            .get(&receiving.airport)
            .filter(|(_, t)| t.elapsed().as_secs() < 45)
        else {
            return bad_request(openatc_core::dialogue::say(
                "notice_simulator_airport_weather_unavailable_8fc27abd",
                &[],
            ));
        };
        let number = |key: &str| sample[key].as_f64().unwrap_or(0.0);
        let pressure = if receiving.airport.starts_with('K') {
            openatc_core::dialogue::say(
                "altimeter",
                &[(
                    "pressure",
                    format!("{:.2}", number("pressureHpa") * 0.029_529_983),
                )],
            )
        } else {
            openatc_core::dialogue::say(
                "qnh",
                &[("pressure", format!("{:.0}", number("pressureHpa")))],
            )
        };
        let text = if request.intent == "altimeter" {
            pressure
        } else {
            openatc_core::dialogue::say(
                "surface_weather_wind_degrees_knots_visibility_metres",
                &[
                    ("airport", (receiving.airport_name).clone()),
                    ("wind_direction", format!("{:.0}", number("windDegrees"))),
                    ("wind_speed", format!("{:.0}", number("windKnots"))),
                    ("visibility", format!("{:.0}", number("visibilityMeters"))),
                    (
                        "clouds",
                        (sample["clouds"].as_str().map_or_else(
                            || openatc_core::dialogue::say("weather_cloud_report_unavailable", &[]),
                            str::to_owned,
                        ))
                        .clone(),
                    ),
                    ("temperature", format!("{:.0}", number("temperatureC"))),
                    ("dewpoint", format!("{:.0}", number("dewpointC"))),
                    ("pressure", (pressure).clone()),
                ],
            )
        };
        let mut tag = assign_airspace_controller(
            &mut state,
            settings,
            &super::flight::station_key(&receiving),
            "standard",
        );
        tag.position.clone_from(&receiving.name);
        tag.radio = Some(request.radio);
        add_transmission(&mut state.session, "ATC", &text, &tag);
        return Json(json!({"result":{"accepted":true,"message":text},"state":snapshot(&state.session,&state.settings)})).into_response();
    }
    let airport = operational_airport.or_else(|| {
        state
            .airport
            .clone()
            .filter(|a| a.icao == receiving.airport)
    });
    if let Some(airport) = airport.as_ref() {
        state.airport = Some(airport.clone());
    }
    if request.intent == "clearance" {
        if let Err(error) = openatc_core::ops::validate_flight_plan(&state.session.plan) {
            return clearance_rejection(&mut state, &receiving, request.radio, error);
        }
        if receiving.airport != state.session.plan.departure {
            let message = openatc_core::dialogue::say(
                "you_are_tuned_to_at_your_flight",
                &[
                    ("name", (receiving.name).clone()),
                    ("airport", (receiving.airport).clone()),
                    ("departure", (state.session.plan.departure).clone()),
                ],
            );
            return clearance_rejection(&mut state, &receiving, request.radio, message);
        }
        if !airport.as_ref().is_some_and(|a| {
            a.runways.iter().any(|r| {
                r.first_name == state.session.plan.runway
                    || r.second_name == state.session.plan.runway
            })
        }) {
            return clearance_rejection(
                &mut state,
                &receiving,
                request.radio,
                openatc_core::dialogue::say("clearance_runway_missing", &[]),
            );
        }
    }
    let airspace = super::flight::station_key(&receiving);
    let pool = parse_voice_pool(&settings.voice_pool);
    let mut delivery = delivery_name(settings.controller_delivery).to_owned();
    if settings.randomize_delivery {
        let styles = ["standard", "brisk", "urgent"];
        styles[rand::rng().random_range(0..3)].clone_into(&mut delivery);
    }
    let mut atc_tag = assign_airspace_controller(&mut state, settings, &airspace, &delivery);
    atc_tag.radio = Some(request.radio);
    atc_tag.position.clone_from(&receiving.name);
    let pilot_tag = SpeechTag {
        radio: Some(request.radio),
        voice: settings.pilot_voice.clone(),
        delivery: "standard".to_owned(),
        speed: settings.pilot_speed,
        ..Default::default()
    };
    if request.intent == "vacated"
        && state.session.taxi_clearance.stage()
            == openatc_core::state::SurfaceStage::CrossingComplete
        && state.session.telemetry.on_ground
        && state.session.taxi_clearance.airport == receiving.airport
        && let Some(airport) = state.airport.as_ref()
        && openatc_core::airport::crossing_vacated(
            airport,
            &state.session.taxi_clearance,
            &state.session.telemetry,
        )
    {
        let crossing = state.session.taxi_clearance.clone();
        if let Ok(mut route) = openatc_core::airport::calculate_taxi_route(
            airport,
            &state.session.telemetry,
            &crossing.destination,
            crossing.to_parking,
            openatc_core::parking::aircraft_size(&state.session).unwrap_or('F'),
        ) {
            route.pending_readback = true;
            route.sequence = state.session.next_sequence;
            let text = route.instructions.clone();
            if !request.controller_initiated {
                let callsign = state.session.plan.callsign.clone();
                add_transmission(&mut state.session, &callsign, &request.text, &pilot_tag);
            }
            state.session.taxi_clearance = route;
            add_transmission(&mut state.session, "ATC", &text, &atc_tag);
            return Json(json!({"result":{"accepted":true,"message":text},"state":snapshot(&state.session,&state.settings)})).into_response();
        }
    }
    if request.intent == "vacated" {
        let route = &state.session.taxi_clearance;
        if route.airport == receiving.airport
            && route.crossing_runway.is_empty()
            && (route.approved || route.pending_readback)
            && !route.instructions.is_empty()
        {
            let text = route.instructions.clone();
            let callsign = state.session.plan.callsign.clone();
            add_transmission(&mut state.session, &callsign, &request.text, &pilot_tag);
            add_transmission(&mut state.session, "ATC", &text, &atc_tag);
            return Json(json!({"result":{"accepted":true,"message":text},"state":snapshot(&state.session,&state.settings)})).into_response();
        }
    }
    if crate::traffic::holding::request(&mut state, &receiving, &request) {
        return Json(json!({"result":{"accepted":false,"message":state.session.transcript.last().map(|t| &t.text)},"state":snapshot(&state.session,&state.settings)})).into_response();
    }
    if request.intent == "direct" && !state.session.plan.fixes.is_empty() {
        let next = openatc_core::navigation::route_ahead(
            &state.session.plan.fixes,
            state.flow.monitor.next_fix.max(1),
            &state.session.telemetry,
        );
        state.flow.monitor.next_fix = next;
        let target = state
            .session
            .plan
            .fixes
            .iter()
            .enumerate()
            .skip(next)
            .find(|(_, fix)| fix.identifier.eq_ignore_ascii_case(&request.waypoint))
            .map(|(i, _)| i);
        if target.is_none() {
            return clearance_rejection(
                &mut state,
                &receiving,
                request.radio,
                openatc_core::dialogue::say("direct_fix_unavailable", &[]),
            );
        }
        if state.settings.enforce_clearance_constraints
            && state.session.plan.fixes[next..target.unwrap()]
                .iter()
                .any(|fix| {
                    fix.minimum_altitude_feet.is_some()
                        || fix.maximum_altitude_feet.is_some()
                        || fix.minimum_speed_knots.is_some()
                        || fix.maximum_speed_knots.is_some()
                })
        {
            return clearance_rejection(
                &mut state,
                &receiving,
                request.radio,
                openatc_core::dialogue::say("direct_restriction_unable", &[]),
            );
        }
    }
    if request.intent == "altitude"
        && !crate::instructions::descent_feasible(&state, request.altitude_feet)
    {
        let phrase = if crate::instructions::remaining_nm(&state).is_none() {
            "altitude_profile_unavailable"
        } else {
            "altitude_descent_distance_unable"
        };
        return clearance_rejection(
            &mut state,
            &receiving,
            request.radio,
            openatc_core::dialogue::say(phrase, &[]),
        );
    }
    if ["altitude", "descent"].contains(&request.intent.as_str())
        && !crate::instructions::altitude_allowed(&state, request.altitude_feet)
    {
        return clearance_rejection(
            &mut state,
            &receiving,
            request.radio,
            openatc_core::dialogue::say("altitude_restriction_unable", &[]),
        );
    }
    if let Some(result) = crate::instructions::request(&mut state, &receiving, &request, &atc_tag) {
        if result["accepted"].as_bool() == Some(true) {
            state.session.control_radio = request.radio;
        }
        return Json(json!({"result":result,"state":snapshot(&state.session,&state.settings)}))
            .into_response();
    }
    if let Some(result) = super::flight::request(&mut state, &receiving, &request, &atc_tag) {
        return Json(json!({"result": result, "state":snapshot(&state.session,&state.settings)}))
            .into_response();
    }
    let squawk = if request.intent == "clearance" && state.session.phase == PhaseCode::Parked {
        state.squawks.next()
    } else {
        state
            .session
            .clearance
            .as_ref()
            .map(|c| c.squawk.clone())
            .unwrap_or_default()
    };
    let targets = state
        .session
        .telemetry
        .traffic
        .is_some()
        .then(|| state.traffic_targets());
    let runway_sequence = airport.as_ref().map_or(RunwaySequence::Clear, |airport| {
        runway_sequence(&state, airport, &state.session.plan.runway)
    });
    let ctx = RequestContext {
        squawk: &squawk,
        runway_taxi_authorized: receiving.service == "Tower"
            || receiving.services.iter().any(|s| s == "Tower")
            || (request.intent == "cross_runway" && receiving.service == "Ground"),
        request: &request,
        airport: airport.as_ref(),
        atc_tag: &atc_tag,
        pilot_tag: &pilot_tag,
        realism: &realism,
        units,
        region: &region,
        traffic: targets.as_deref(),
        runway_sequence,
    };
    let mut result = apply_request(&mut state.session, &ctx);
    if result.accepted && ["acknowledge", "readback"].contains(&request.intent.as_str()) {
        crate::traffic::holding::acknowledge(&mut state, request.clearance_sequence);
    }
    if result.accepted
        && !matches!(
            request.intent.as_str(),
            "radio_check" | "repeat" | "standby" | "weather" | "altimeter"
        )
    {
        state.session.control_radio = request.radio;
    }
    if result.accepted && request.intent == "go_around" {
        super::flight::go_around(&mut state);
    }
    super::radio::apply_wording(&mut state, &request.intent, &receiving, &mut result);
    if result.accepted && request.intent == "ready" {
        super::flight::takeoff_wording(&mut state, &receiving, &mut result);
    }
    if result.accepted
        && request.intent == "readback"
        && state
            .session
            .clearance
            .as_ref()
            .is_some_and(|c| c.acknowledged && c.readback_scope.is_empty())
        && receiving.service == "Clearance"
        && let Some(ground) =
            openatc_core::stations::nearby(&state.stations, &state.session.telemetry)
                .into_iter()
                .find(|s| s.airport == receiving.airport && s.service == "Ground")
    {
        let handoff = openatc_core::dialogue::say(
            "clearance_contact_ground_when_ready",
            &[
                ("station", ground.name.clone()),
                (
                    "frequency",
                    format!("{:.3}", f64::from(ground.khz) / 1000.0),
                ),
            ],
        );
        result.message.push(' ');
        result.message.push_str(&handoff);
        state.session.recommended_frequency_khz = ground.khz;
        state.session.frequency_sequence = state
            .session
            .transcript
            .last()
            .map_or(0, |entry| entry.sequence);
        if let Some(last) = state.session.transcript.last_mut() {
            last.text.clone_from(&result.message);
        }
    }
    if state.session.demo
        && settings.congestion == Congestion::Busy
        && rand::rng().random_range(0..100) < 25
    {
        add_transmission(
            &mut state.session,
            "ATC",
            &openatc_core::dialogue::say("standby", &[]),
            &atc_tag,
        );
    }
    maybe_chatter(&mut state.session, settings, &pool, &atc_tag);
    let intent = request.intent.clone();
    let message = result.message.clone();
    let accepted = result.accepted;
    let response_sequence = state
        .session
        .transcript
        .iter()
        .rev()
        .find(|e| e.speaker == "ATC" && e.text == message)
        .map(|e| e.sequence);
    // Optional wording only, never a second controller decision. Do not hold the
    // session lock while the model works; discard if any newer transmission exists.
    let sequence = state.session.transcript.last().map_or(0, |e| e.sequence);
    let mut final_message = message.clone();
    let client = state.client.clone();
    let history_key = format!("{}|{}", super::flight::station_key(&receiving), intent);
    let previous_wording = state
        .phrase_history
        .last
        .get(&history_key)
        .cloned()
        .unwrap_or_default();
    let role_key = format!("atc_{}", receiving.service.to_lowercase());
    let role_description = match receiving.service.as_str() {
        "Clearance" | "Ground" | "Tower" | "Approach" | "Departure" | "Center" => {
            state.prompts.guidance.get(&role_key)
        }
        _ => state.prompts.guidance.get("atc_default"),
    };
    let wording_context = state
        .prompts
        .guidance
        .get("atc_wording")
        .replace("{{service}}", &receiving.service)
        .replace("{{role}}", role_description)
        .replace("{{intent}}", &intent)
        .replace("{{previous}}", &format!("{previous_wording:?}"));
    let example_guidance = state.prompts.guidance.get("examples").to_owned();
    let phase = format!("{:?}", state.session.phase).to_lowercase();
    let rules = if state.session.ifr { "ifr" } else { "vfr" };
    let wording_prompt =
        super::support::fill_prompt(&state.prompts.atc, &state.session.plan.callsign);
    let examples =
        state
            .speech
            .select_for_airport("atc", &[], &phase, &receiving.airport, rules, usize::MAX);
    let examples = examples
        .into_iter()
        .filter(|e| {
            e.service.is_empty()
                || e.service.contains(&receiving.service)
                || receiving.services.iter().any(|s| e.service.contains(s))
        })
        .flat_map(|e| e.say.iter().cloned())
        .collect::<Vec<_>>()
        .join("\n");
    drop(state);
    if accepted
        && settings.llm_phrase_variety
        && settings.ai_enabled
        && !settings.ai_model.is_empty()
        && !["ready", "readback", "acknowledge"].contains(&request.intent.as_str())
    {
        let system = crate::llm::prompt(
            &wording_prompt,
            &example_guidance,
            &[examples],
            &wording_context.replace("{{phase}}", &phase),
        );
        if let Ok(Ok(candidate)) = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            chat_complete(
                &client,
                &settings.ai_url,
                &settings.ai_model,
                0.3,
                &system,
                &final_message,
            ),
        )
        .await
            && candidate.trim() != previous_wording.trim()
            && super::radio::safe_variety(&final_message, &candidate)
        {
            final_message = candidate;
        }
    }
    let mut state = shared.write().await;
    if state.session_generation != generation {
        return bad_request(openatc_core::dialogue::say(
            "notice_flight_was_reset_previous_request_discarded_c0a5c48d",
            &[],
        ));
    }

    if state
        .session
        .transcript
        .last()
        .is_some_and(|e| e.sequence == sequence)
    {
        if let Some(last) = state
            .session
            .transcript
            .iter_mut()
            .find(|e| Some(e.sequence) == response_sequence)
        {
            last.text.clone_from(&final_message);
        }
    } else {
        final_message = message;
    }
    if accepted
        && state
            .session
            .transcript
            .last()
            .is_some_and(|e| e.sequence == sequence)
    {
        state
            .phrase_history
            .last
            .insert(history_key, final_message.clone());
        state.phrase_history.save(&state.config_dir);
    }
    let result = json!({"accepted": accepted, "message": final_message});
    let snapshot = snapshot(&state.session, &state.settings);
    Json(json!({"result": result, "state": snapshot})).into_response()
}

/// Canned frequency exchanges for demo sessions.
fn maybe_chatter(session: &mut State, settings: &Settings, pool: &[String], atc_tag: &SpeechTag) {
    if !session.demo
        || settings.congestion == Congestion::Off
        || rand::rng().random_range(0..100) >= chance(settings)
    {
        return;
    }
    let exchanges = chatter_exchanges();
    let exchange = &exchanges[rand::rng().random_range(0..exchanges.len())];
    let other = exchange.0.to_lowercase();
    let own = session.plan.callsign.to_lowercase();
    if other.starts_with(&own) || own.get(..3) == other.get(..3) {
        return;
    }
    let mut voice = settings.copilot_voice.clone();
    if !pool.is_empty() {
        voice.clone_from(&pool[rand::rng().random_range(0..pool.len())]);
    }
    if voice.is_empty() {
        "alloy".clone_into(&mut voice);
    }
    let caller = exchange
        .0
        .split(' ')
        .next()
        .unwrap_or(&exchange.0)
        .to_owned();
    let tag = SpeechTag {
        voice,
        delivery: "standard".to_owned(),
        speed: 1.0,
        ..Default::default()
    };
    add_transmission(session, &caller, &exchange.0, &tag);
    if let Some(entry) = session.transcript.last_mut() {
        entry.background = true;
    }
    add_transmission(session, "ATC", &exchange.1, atc_tag);
    if let Some(entry) = session.transcript.last_mut() {
        entry.background = true;
    }
}

/// Few-shot voice examples from the speech pool, rendered with sample slots
/// so the model hears finished transmissions, not templates.
fn few_shots(pool: &SpeechPool, role: &str, situations: &[&str], phase: &str) -> String {
    let examples = pool.select(role, situations, phase, "icao", usize::MAX);
    if examples.is_empty() {
        return String::new();
    }
    let samples = sample_slots();
    let mut out = String::from(
        "\nSound natural and concise for your role. Use these fictional examples as style references, not scripts to copy. Compose a fresh response from supplied live facts; standard phraseology may repeat.\nVoice examples from the library:",
    );
    for example in examples {
        for line in &example.say {
            out.push_str("\n- ");
            out.push_str(&render_template(line, &samples));
        }
    }
    out
}

/// Generate a fresh transmission from the speech library: retrieve examples,
/// assemble the prompt, call the model, split the effect line.
pub async fn post_suggest(
    AxumState(shared): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let role = body.get("role").and_then(Value::as_str).unwrap_or("atc");
    if !["atc", "copilot", "ground", "attendant"].contains(&role) {
        return bad_request(
            openatc_core::dialogue::say("notice_unknown_role_1db2d66a", &[]).clone(),
        );
    }
    let situations: Vec<String> = body
        .get("situations")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let phase = body
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let facts = body
        .get("facts")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let (callsign, settings, client) = {
        let state = shared.read().await;
        (
            state.session.plan.callsign.clone(),
            state.settings.clone(),
            state.client.clone(),
        )
    };
    let situation = if facts.is_empty() {
        format!("Situation tags: {}", situations.join(", "))
    } else {
        facts
    };
    let context = PromptContext {
        role: role.to_owned(),
        callsign,
        phase: phase.clone(),
        situation,
    };
    let prompt = {
        let state = shared.read().await;
        let tags: Vec<&str> = situations.iter().map(String::as_str).collect();
        let airport = body
            .get("airport")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                if ["arrival", "approach", "landed", "taxi_in"].contains(&phase.as_str()) {
                    state.session.plan.destination.as_str()
                } else {
                    state.session.plan.departure.as_str()
                }
            });
        let flight_rules = body
            .get("flightRules")
            .and_then(Value::as_str)
            .unwrap_or("ifr");
        if !["ifr", "vfr"].contains(&flight_rules) {
            return bad_request(
                openatc_core::dialogue::say("notice_flightrules_must_be_ifr_or_vfr_014fc7fc", &[])
                    .clone(),
            );
        }
        let examples =
            state
                .speech
                .select_for_airport(role, &tags, &phase, airport, flight_rules, usize::MAX);
        if examples.is_empty() {
            return bad_request(
                openatc_core::dialogue::say(
                    "notice_no_examples_for_that_role_and_situation_7bdff3c5",
                    &[],
                )
                .clone(),
            );
        }
        let samples = sample_slots();
        let lines = examples
            .iter()
            .flat_map(|entry| entry.say.iter().map(|line| render_template(line, &samples)))
            .collect::<Vec<_>>();
        let mut prompt = crate::llm::prompt(state.prompts.guidance.get("speech_rules"), state.prompts.guidance.get("examples"), &lines, &json!({"role":context.role,"situation":context.situation,"phase":context.phase,"callsign":context.callsign}).to_string());
        let _ = write!(
            prompt,
            "\nSpeech context: airport {airport}, flight rules {flight_rules}."
        );
        if let Some(profile) = state.speech.region_for_airport(airport) {
            let _ = write!(
                prompt,
                "\nRegional scope: {}. {}",
                profile.name, profile.notes
            );
        }
        prompt
    };
    if !settings.ai_enabled || settings.ai_model.is_empty() {
        return bad_request(
            openatc_core::dialogue::say("notice_ai_generation_is_off_587c5e7e", &[]).clone(),
        );
    }
    let current_request = shared
        .read()
        .await
        .prompts
        .guidance
        .get("suggest_request")
        .to_owned();
    match chat_complete(
        &client,
        &settings.ai_url,
        &settings.ai_model,
        0.3,
        &prompt,
        &current_request,
    )
    .await
    {
        Ok(generated) => {
            let (transmission, effect) = parse_ai_output(&generated);
            Json(json!({"transmission": transmission, "effect": effect})).into_response()
        }
        Err(error) => bad_request(error),
    }
}

fn chance(settings: &Settings) -> i32 {
    if settings.congestion == Congestion::Busy {
        35
    } else {
        15
    }
}

/// Keep clearance validation feedback identical in the transcript and UI result.
fn clearance_rejection(
    state: &mut crate::EngineState,
    station: &openatc_core::stations::Station,
    radio: openatc_core::state::Radio,
    message: impl Into<String>,
) -> Response {
    let message = message.into();
    let mut tag = crate::flight::tag(state, station);
    tag.radio = Some(radio);
    add_transmission(&mut state.session, "ATC", &message, &tag);
    Json(json!({"result":{"accepted":false,"message":message},"state":snapshot(&state.session,&state.settings)})).into_response()
}

#[cfg(test)]
mod chatter_tests {
    use super::*;
    use openatc_core::aircraft::AircraftBehaviour;
    #[test]
    fn a_prepared_readback_is_dropped_when_a_newer_instruction_supersedes_it() {
        let mut state = crate::test_state();
        let tag = SpeechTag::default();
        add_transmission(&mut state.session, "ATC", "Hold short of runway 04R.", &tag);
        add_transmission(
            &mut state.session,
            "COPILOT",
            "Holding short of runway 04R.",
            &tag,
        );
        let sequence = state.session.transcript.last().unwrap().sequence;
        state.pending_copilot_reply = Some(PreparedReply {
            generation: state.session_generation,
            sequence,
            frequency: 118_700,
            request: Request::default(),
        });
        reconcile_pending_reply(&mut state);
        assert!(state.pending_copilot_reply.is_some());

        add_transmission(
            &mut state.session,
            "ATC",
            "Line up and wait runway 04R.",
            &tag,
        );
        reconcile_pending_reply(&mut state);
        assert!(state.pending_copilot_reply.is_none());
    }

    #[test]
    fn a_completed_frequency_readback_is_cleared() {
        let mut state = crate::test_state();
        let tag = SpeechTag::default();
        add_transmission(&mut state.session, "ATC", "Contact Tower, 118.700.", &tag);
        let sequence = state.session.transcript.last().unwrap().sequence;
        state.session.transcript.last_mut().unwrap().pilot_reply =
            "Contact Tower, 118.700, C-GTLT.".into();
        clear_readback(&mut state, sequence);
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .pilot_reply
                .is_empty()
        );
    }

    #[test]
    fn a_copilot_readback_clears_the_instruction_it_answers() {
        let mut state = crate::test_state();
        let tag = SpeechTag::default();
        add_transmission(&mut state.session, "ATC", "Contact Tower, 118.700.", &tag);
        add_transmission(
            &mut state.session,
            "COPILOT",
            "Contact Tower, 118.700.",
            &tag,
        );
        let prepared = state.session.transcript.last().unwrap().sequence;
        let source = readback_source_sequence(&state, prepared);
        assert_eq!(
            state
                .session
                .transcript
                .iter()
                .find(|entry| entry.sequence == source)
                .map(|entry| entry.text.as_str()),
            Some("Contact Tower, 118.700."),
            "the readback must clear the controller instruction, not the copilot line"
        );
        clear_readback(&mut state, source);
        assert!(
            state
                .session
                .transcript
                .iter()
                .find(|entry| entry.sequence == source)
                .is_some_and(|entry| entry.pilot_reply.is_empty())
        );
    }

    #[test]
    fn background_traffic_does_not_supersede_a_prepared_readback() {
        let mut state = crate::test_state();
        let tag = SpeechTag::default();
        add_transmission(&mut state.session, "ATC", "Hold short of runway 04R.", &tag);
        add_transmission(
            &mut state.session,
            "COPILOT",
            "Holding short of runway 04R.",
            &tag,
        );
        let sequence = state.session.transcript.last().unwrap().sequence;
        state.pending_copilot_reply = Some(PreparedReply {
            generation: state.session_generation,
            sequence,
            frequency: 118_700,
            request: Request::default(),
        });
        add_transmission(
            &mut state.session,
            "ATC",
            "Traffic on final, runway 04R.",
            &tag,
        );
        state.session.transcript.last_mut().unwrap().background = true;
        reconcile_pending_reply(&mut state);
        assert!(state.pending_copilot_reply.is_some());
    }

    #[test]
    fn a_prepared_copilot_reply_does_not_block_controller_readiness() {
        let mut state = crate::test_state();
        state.session.telemetry.position_valid = true;
        state.pending_copilot_reply = Some(PreparedReply {
            generation: state.session_generation,
            sequence: 1,
            frequency: 118_700,
            request: Request::default(),
        });
        assert!(crate::traffic::ready(&state));
    }

    #[tokio::test]
    async fn speech_proxy_pronounces_taxiways_without_editing_the_transcript() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let inputs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = inputs.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().route(
                    "/v1/audio/speech",
                    axum::routing::post(move |Json(body): Json<Value>| {
                        let captured = captured.clone();
                        async move {
                            captured
                                .lock()
                                .unwrap()
                                .push(body["input"].as_str().unwrap().to_owned());
                            vec![0u8; 44]
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let mut state = crate::test_state();
        state.settings.tts_url = format!("http://{address}");
        state.session.plan.callsign = "C-GTLT".into();
        let text = "C-GTLT, Taxi via T, A, A1, hold short of runway 04L.";
        add_transmission(&mut state.session, "ATC", text, &SpeechTag::default());
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        for speaker in ["atc", "copilot", "pilot"] {
            let response = post_speak(
                AxumState(shared.clone()),
                Json(json!({"text":text,"speaker":speaker})),
            )
            .await;
            assert!(response.status().is_success());
        }
        {
            let inputs = inputs.lock().unwrap();
            assert_eq!(inputs.len(), 3);
            assert!(inputs.iter().all(|input| {
                input.contains("via Tango, Alfa, Alfa one, hold short of runway zero four left.")
            }));
        }
        assert_eq!(shared.read().await.session.transcript[0].text, text);
        server.abort();
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn cockpit_voices_are_clean_while_received_radio_keeps_effects() {
        let mut settings = Settings {
            radio_effects: true,
            radio_bandpass: true,
            radio_hiss: 0.4,
            radio_crackle: 0.3,
            radio_static: 0.2,
            ..Default::default()
        };
        let clean = json!({"hiss":0.0,"crackle":0.0,"static":0.0,"bandpass":false});
        for (speaker, received_radio) in [("pilot", false), ("copilot", false), ("copilot", true)] {
            assert_eq!(speech_effects(&settings, speaker, received_radio), clean);
        }
        for (speaker, received_radio) in [("atc", false), ("pilot", true)] {
            let effects = speech_effects(&settings, speaker, received_radio);
            assert_eq!(effects["bandpass"], true);
            assert!(effects["hiss"].as_f64().unwrap() > 0.0);
            assert!(effects["crackle"].as_f64().unwrap() > 0.0);
            assert!(effects["static"].as_f64().unwrap() > 0.0);
        }
        settings.radio_fx_cabin = false;
        settings.radio_fx_ground = false;
        assert_eq!(speech_effects(&settings, "cabin", false), clean);
        assert_eq!(speech_effects(&settings, "ground", false), clean);
        settings.radio_effects = false;
        assert_eq!(speech_effects(&settings, "atc", true), clean);
        assert_eq!(speech_effects(&settings, "pilot", true), clean);
    }
    #[tokio::test]
    #[allow(clippy::float_cmp)]
    async fn planned_airports_get_weather_before_a_radio_handoff() {
        let mut state = crate::test_state();
        state
            .station_root
            .clone_from(&state.settings.simulator_root);
        state.session.plan.departure = "ORIG".into();
        state.session.plan.destination = "DEST".into();
        state.airport = Some(openatc_core::airport::Airport {
            icao: "ORIG".into(),
            reference_latitude: 1.0,
            reference_longitude: 2.0,
            elevation_feet: 50.0,
            ..Default::default()
        });
        state.arrival_airport = Some(openatc_core::airport::Airport {
            icao: "DEST".into(),
            reference_latitude: 3.0,
            reference_longitude: 4.0,
            elevation_feet: 300.0,
            ..Default::default()
        });
        state.stations.clear();
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        let snapshot = super::full_state(AxumState(shared.clone())).await.0;
        let airports: Vec<openatc_core::state::WeatherAirport> =
            serde_json::from_value(snapshot["weatherAirports"].clone()).unwrap();
        assert_eq!(airports.len(), 2);
        assert_eq!(airports[1].airport, "DEST");
        assert_eq!(airports[1].elevation_feet, 300.0);
        let response = crate::radio::weather(AxumState(shared.clone()), Json(json!({
            "airport":"DEST", "latitude":3.0,"longitude":4.0,"sampleAltitudeFeet":300.0,
            "source":"simulator-airport", "temperatureC":20.0,"dewpointC":10.0,"pressureHpa":1020.0,
            "windDegrees":180.0,"windKnots":10.0,"visibilityMeters":10000.0,"clouds":"Clear",
            "magneticVariationDegrees":2.5
        }))).await;
        assert!(response.status().is_success());
        assert_eq!(
            shared.read().await.sim_weather["DEST"].0["magneticVariationDegrees"],
            2.5
        );
    }

    #[tokio::test]
    async fn parked_radio_and_information_requests_work_without_a_flight_plan() {
        use openatc_core::state::{FlightPlan, PhaseCode};
        for service in ["Clearance", "Ground", "Tower"] {
            for radio in ["com1", "com2"] {
                let mut state = crate::test_state();
                state.settings.simulator_root = "parked-radio-fixture".into();
                state
                    .station_root
                    .clone_from(&state.settings.simulator_root);
                state.settings.congestion = Congestion::Off;
                state.voices_source.clone_from(&state.settings.tts_url);
                state.voices_refreshed = Some(std::time::Instant::now());
                state.session.plan = FlightPlan::default();
                state.session.clearance = None;
                state
                    .session
                    .on_event(openatc_core::aircraft::AircraftEvent::Parked);
                state.session.telemetry.on_ground = true;
                state.session.telemetry.radio_power = true;
                state.session.telemetry.position_valid = true;
                state.session.telemetry.latitude = 0.0;
                state.session.telemetry.longitude = 0.0;
                state.session.telemetry.com1_khz = 121_400;
                state.session.telemetry.com2_khz = Some(121_400);
                state.stations = vec![openatc_core::stations::Station {
                    airport: "TEST".into(),
                    name: format!("Test {service}"),
                    service: service.into(),
                    khz: 121_400,
                    ..Default::default()
                }];
                let plan = state.session.plan.clone();
                let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
                let snapshot = super::full_state(AxumState(shared.clone())).await.0;
                assert_eq!(snapshot["weatherAirports"].as_array().unwrap().len(), 1);
                assert_eq!(snapshot["weatherAirports"][0]["airport"], "TEST");
                let response = crate::radio::weather(AxumState(shared.clone()), Json(json!({
                    "airport":"TEST", "latitude":0.0,"longitude":0.0,"sampleAltitudeFeet":0.0,
                    "source":"simulator-airport", "temperatureC":20.0,"dewpointC":10.0,"pressureHpa":1020.0,
                    "windDegrees":180.0,"windKnots":10.0,"visibilityMeters":10000.0,"clouds":"Clear"
                }))).await;
                assert!(response.status().is_success());
                for intent in ["radio_check", "altimeter", "weather"] {
                    let response = handle_request(
                        AxumState(shared.clone()),
                        Json(json!({
                            "intent":intent,"text":intent,"role":"atc","radio":radio
                        })),
                    )
                    .await;
                    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                        .await
                        .unwrap();
                    let body: Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(
                        body["result"]["accepted"], true,
                        "{service}/{radio}/{intent}: {body}"
                    );
                    let state = shared.read().await;
                    assert_eq!(state.session.phase, PhaseCode::Parked);
                    assert_eq!(state.session.plan, plan);
                    assert!(state.session.clearance.is_none());
                    let reply = state.session.transcript.last().unwrap();
                    assert_eq!(reply.position, format!("Test {service}"));
                    assert_eq!(
                        reply.radio,
                        Some(if radio == "com1" {
                            openatc_core::state::Radio::Com1
                        } else {
                            openatc_core::state::Radio::Com2
                        })
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn missing_and_incomplete_plans_receive_spoken_clearance_rejections() {
        for scenario in [
            "empty",
            "partial",
            "invalid",
            "malformed",
            "direct",
            "stale",
        ] {
            let mut state = crate::test_state();
            state.settings.simulator_root = "radio-plan-fixture".into();
            state
                .station_root
                .clone_from(&state.settings.simulator_root);
            state.settings.congestion = Congestion::Off;
            state.voices_source.clone_from(&state.settings.tts_url);
            state.voices_refreshed = Some(std::time::Instant::now());
            state.session.telemetry.position_valid = true;
            state.session.telemetry.com1_khz = 121_400;
            state.session.telemetry.com2_khz = Some(118_700);
            state.session.plan.callsign = "C-GTLT".into();
            state.stations.push(openatc_core::stations::Station {
                airport: "TEST".into(),
                name: "Test Delivery".into(),
                service: "Clearance".into(),
                khz: 118_700,
                ..Default::default()
            });
            if scenario == "stale" {
                state.session.plan.departure = "LFMN".into();
                state.session.plan.destination = "LIMC".into();
                state.session.plan.runway = "04R".into();
            }
            let filed_plan = state.session.plan.clone();
            let mut draft = openatc_core::state::FlightPlan {
                callsign: "C-GTLT".into(),
                ..Default::default()
            };
            if scenario == "partial" {
                draft.departure = "LFMN".into();
            }
            if scenario == "invalid" {
                draft.departure = "LFMN".into();
                draft.destination = "LIMC".into();
                draft.runway = "04R".into();
                draft.initial_altitude_feet = draft.cruise_feet + 1000;
            }
            let mut request = json!({"intent":"clearance", "text":"Test Delivery, C-GTLT, request IFR clearance", "role":"atc", "radio":"com2", "planningCallsign":"C-GTLT"});
            if scenario != "direct" {
                request["planningFlightPlan"] = if scenario == "malformed" {
                    json!({"cruiseFeet":"bad"})
                } else {
                    serde_json::to_value(draft).unwrap()
                };
            }
            let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
            let response = handle_request(AxumState(shared.clone()), Json(request)).await;
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let response: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                response["result"]["accepted"], false,
                "{scenario}: {response}"
            );
            let state = shared.read().await;
            assert!(state.session.clearance.is_none());
            assert_eq!(state.session.plan, filed_plan);
            let reply = state.session.transcript.last().unwrap();
            assert_eq!(reply.speaker, "ATC");
            assert!(reply.text.starts_with("C-GTLT,"), "{}", reply.text);
            assert!(
                reply
                    .text
                    .contains(if matches!(scenario, "empty" | "direct" | "stale") {
                        "not filed a flight plan"
                    } else {
                        "incomplete or invalid"
                    }),
                "{scenario}: {}",
                reply.text
            );
            assert!(!reply.voice.is_empty());
            assert_eq!(reply.radio, Some(openatc_core::state::Radio::Com2));
            assert_eq!(reply.position, "Test Delivery");
            assert_eq!(state.session.telemetry.com1_khz, 121_400);
        }
    }

    #[test]
    fn rejected_request_keeps_the_receiving_radio_and_station_voice() {
        let mut state = crate::test_state();
        let station = openatc_core::stations::Station {
            airport: "TEST".into(),
            name: "Test Delivery".into(),
            service: "Clearance".into(),
            khz: 121_700,
            ..Default::default()
        };
        let identity = crate::flight::tag(&mut state, &station);
        state.settings.voice = "different-global-voice".into();
        clearance_rejection(
            &mut state,
            &station,
            openatc_core::state::Radio::Com2,
            openatc_core::dialogue::say("clearance_runway_missing", &[]),
        );
        let reply = state.session.transcript.last().unwrap();
        assert_eq!(reply.voice, identity.voice);
        assert_eq!(reply.position, station.name);
        assert_eq!(reply.radio, Some(openatc_core::state::Radio::Com2));
        assert_eq!(
            state.session.control_radio,
            openatc_core::state::Radio::Com1
        );
    }
    #[tokio::test]
    async fn requests_on_com2_use_its_station_without_retuning_com1() {
        let mut state = crate::test_state();
        state.settings.simulator_root = "radio-test-fixture".into();
        state
            .station_root
            .clone_from(&state.settings.simulator_root);
        state.settings.congestion = Congestion::Off;
        state.voices_source.clone_from(&state.settings.tts_url);
        state.voices_refreshed = Some(std::time::Instant::now());
        state.session.telemetry.position_valid = true;
        state.session.telemetry.com1_khz = 121_400;
        state.session.telemetry.com2_khz = Some(118_700);
        state.stations.push(openatc_core::stations::Station {
            airport: "TEST".into(),
            name: "Test Tower".into(),
            service: "Tower".into(),
            khz: 118_700,
            ..Default::default()
        });
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        let request = json!({"intent":"radio_check","text":"Test Tower, C-GTLT, Radio check","role":"atc","radio":"com2","planningCallsign":"C-GTLT"});
        let response = handle_request(AxumState(shared.clone()), Json(request.clone())).await;
        assert!(response.status().is_success());
        let first = shared
            .read()
            .await
            .session
            .transcript
            .last()
            .unwrap()
            .clone();
        assert_eq!(first.position, "Test Tower");
        assert_eq!(shared.read().await.session.plan.callsign, "C-GTLT");
        assert!(
            shared
                .read()
                .await
                .session
                .transcript
                .iter()
                .any(|entry| entry.speaker == "C-GTLT" && entry.text.contains("C-GTLT"))
        );
        assert_eq!(first.radio, Some(openatc_core::state::Radio::Com2));
        assert_eq!(shared.read().await.session.telemetry.com1_khz, 121_400);
        shared.write().await.session.telemetry.com1_khz = 118_700;
        let response = handle_request(
            AxumState(shared.clone()),
            Json(json!({"intent":"radio_check","role":"atc"})),
        )
        .await;
        assert!(response.status().is_success());
        let second = shared
            .read()
            .await
            .session
            .transcript
            .last()
            .unwrap()
            .clone();
        assert_eq!(first.voice, second.voice);
        assert_eq!(second.radio, Some(openatc_core::state::Radio::Com1));
        assert_eq!(first.position, second.position);
        let count = shared.read().await.session.transcript.len();
        shared.write().await.session.telemetry.com2_khz = None;
        let response = handle_request(AxumState(shared.clone()), Json(request)).await;
        assert!(response.status().is_success());
        assert_eq!(shared.read().await.session.transcript.len(), count);
    }
    #[test]
    fn social_replies_match_farewell_and_do_not_swallow_controls() {
        let mut state = crate::test_state();
        for role in ["ground", "cabin", "copilot"] {
            let reply = social_reply(&state, "have a good day done there sir", role).unwrap();
            assert!(!reply.to_lowercase().contains("approval"));
            assert!(!reply.to_lowercase().contains("confirm"));
            assert!(social_reply(&state, "thank you", role).is_some());
        }
        let aircraft: toml::Value =
            toml::from_str(include_str!("../../../aircraft/toliss_a320.toml")).unwrap();
        state.crew.profile = aircraft["aircraft"]["crew"].clone().try_into().unwrap();
        assert!(social_reply(&state, "thanks and remove chocks", "ground").is_none());
        assert!(social_reply(&state, "hello and turn off cabin lights", "cabin").is_none());
    }
    #[test]
    #[ignore = "requires installed X-Plane scenery via OPENATC_TEST_SIMULATOR_ROOT"]
    fn installed_los_angeles_center_receives_the_reported_radio() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let stations = openatc_core::stations::load_index(&root).unwrap();
        let telemetry = openatc_core::state::Telemetry {
            latitude: 33.914_847,
            longitude: -118.384_915,
            altitude_feet: 3600.0,
            height_agl_feet: 3569.9,
            on_ground: false,
            position_valid: true,
            radio_power: true,
            com1_khz: 118_025,
            ..Default::default()
        };
        let nearby = openatc_core::stations::nearby(&stations, &telemetry);
        let center = openatc_core::stations::tuned(&nearby, telemetry.com1_khz).unwrap();
        assert_eq!(center.service, "Center");
        assert!(center.name.contains("LOS ANGELES"), "{}", center.name);
    }
    #[tokio::test]
    async fn legacy_center_handoff_receives_the_completed_copilot_checkin() {
        use openatc_core::state::{Clearance, PhaseCode, Telemetry};
        let mut state = crate::test_state();
        let root = state.config_dir.join("center-handoff-scenery");
        let apt = root.join("Global Scenery/Global Airports/Earth nav data/apt.dat");
        let atc = root.join("Resources/default scenery/1200 atc data/Earth nav data/atc.dat");
        std::fs::create_dir_all(apt.parent().unwrap()).unwrap();
        std::fs::create_dir_all(atc.parent().unwrap()).unwrap();
        std::fs::write(&apt, "1 125 0 0 KLAX Los Angeles\n100 45 1 0 0 1 1 0 25L 33.93 -118.38 0 0 1 0 0 0 07R 33.93 -118.42 0 0 1 0 0 0\n54 12095 LOS ANGELES Tower\n99\n").unwrap();
        std::fs::write(&atc, "CONTROLLER\nNAME SOCAL\nFACILITY_ID SOCAL\nROLE tracon\nFREQ 12450\nAIRSPACE_POLYGON_BEGIN 0 10000\nPOINT 33.92 -118.5\nPOINT 34.1 -118.5\nPOINT 34.1 -118.2\nPOINT 33.92 -118.2\nAIRSPACE_POLYGON_END\nCONTROLLER_END\nCONTROLLER\nNAME LOS ANGELES\nFACILITY_ID KZLA\nROLE ctr\nFREQ 11802\nAIRSPACE_POLYGON_BEGIN 0 60000\nPOINT 33 -119\nPOINT 34.5 -119\nPOINT 34.5 -117\nPOINT 33 -117\nAIRSPACE_POLYGON_END\nCONTROLLER_END\n").unwrap();
        state.settings.simulator_root = root.to_str().unwrap().into();
        state
            .station_root
            .clone_from(&state.settings.simulator_root);
        state.stations = openatc_core::stations::load_index(&state.station_root).unwrap();
        state.settings.copilot_replies = true;
        state.settings.congestion = Congestion::Off;
        state.settings.tts_url = "http://127.0.0.1:1".into();
        state.settings.ai_enabled = false;
        state.session.demo = false;
        state.session.has_departed = true;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Departure);
        state.session.plan.callsign = "TOL1975".into();
        state.session.plan.departure = "KLAX".into();
        state.session.plan.destination = "KONT".into();
        state.session.clearance = Some(Clearance {
            altitude_feet: 5000,
            acknowledged: true,
            ..Default::default()
        });
        state.session.telemetry = Telemetry {
            latitude: 33.914_847,
            longitude: -118.384_915,
            altitude_feet: 3600.0,
            height_agl_feet: 3569.9,
            on_ground: false,
            position_valid: true,
            radio_power: true,
            ground_speed_knots: 266.59,
            heading_degrees: 180.0,
            com1_khz: 124_500,
            ..Default::default()
        };
        for _ in 0..4 {
            crate::flight::tick(&mut state);
        }
        assert_eq!(state.session.recommended_frequency_khz, 118_025);
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("118.025")
        );
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        let response = post_copilot_prepare(AxumState(shared.clone())).await;
        assert!(response.status().is_success());
        let sequence = shared
            .read()
            .await
            .pending_copilot_reply
            .as_ref()
            .unwrap()
            .sequence;
        assert!(
            post_copilot_reply(
                AxumState(shared.clone()),
                Json(json!({"sequence":sequence}))
            )
            .await
            .status()
            .is_success()
        );
        shared.write().await.session.telemetry.com1_khz = 118_025;
        assert!(
            post_copilot_checkin(AxumState(shared.clone()), Json(json!({})))
                .await
                .status()
                .is_success()
        );
        let sequence = shared
            .read()
            .await
            .pending_copilot_reply
            .as_ref()
            .unwrap()
            .sequence;
        assert!(
            post_copilot_reply(
                AxumState(shared.clone()),
                Json(json!({"sequence":sequence}))
            )
            .await
            .status()
            .is_success()
        );
        let state = shared.read().await;
        let reply = state.session.transcript.last().unwrap();
        assert_eq!(reply.speaker, "ATC");
        assert!(reply.text.contains("5000"), "{}", reply.text);
        assert!(
            reply.text.to_lowercase().contains("climb"),
            "{}",
            reply.text
        );
        assert_eq!(state.session.frequency_sequence, 0);
        assert_eq!(state.session.phase, PhaseCode::Departure);
        assert!(
            openatc_core::stations::tuned(
                &openatc_core::stations::nearby(&state.stations, &state.session.telemetry),
                118_025
            )
            .is_some()
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn com2_readback_stays_on_com2_and_retuning_invalidates_it() {
        let mut state = crate::test_state();
        state.settings.copilot_replies = true;
        state.session.telemetry.com1_khz = 121_400;
        state.session.telemetry.com2_khz = Some(121_400);
        add_transmission(
            &mut state.session,
            "ATC",
            "Hold position.",
            &SpeechTag {
                radio: Some(openatc_core::state::Radio::Com2),
                position: "Test Ground".into(),
                ..Default::default()
            },
        );
        state.session.transcript.last_mut().unwrap().pilot_reply =
            "Holding position, TEST123.".into();
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        assert!(
            post_copilot_prepare(AxumState(shared.clone()))
                .await
                .status()
                .is_success()
        );
        let sequence = {
            let state = shared.read().await;
            let pending = state.pending_copilot_reply.as_ref().unwrap();
            assert_eq!(pending.request.radio, openatc_core::state::Radio::Com2);
            assert_eq!(pending.frequency, 121_400);
            assert_eq!(
                state.session.transcript.last().unwrap().radio,
                Some(openatc_core::state::Radio::Com2)
            );
            pending.sequence
        };
        shared.write().await.session.telemetry.com2_khz = Some(119_100);
        let response = post_copilot_reply(
            AxumState(shared.clone()),
            Json(json!({"sequence":sequence})),
        )
        .await;
        assert!(!response.status().is_success());
        assert!(shared.read().await.pending_copilot_reply.is_none());
        assert_eq!(shared.read().await.session.telemetry.com1_khz, 121_400);
    }

    #[tokio::test]
    async fn delayed_playback_cannot_acknowledge_a_replacement_or_another_frequency() {
        let mut state = crate::test_state();
        state.settings.copilot_replies = true;
        state.session.telemetry.com1_khz = 121_400;
        add_transmission(
            &mut state.session,
            "ATC",
            "Hold position.",
            &SpeechTag {
                position: "Test Ground".into(),
                ..Default::default()
            },
        );
        state.session.transcript.last_mut().unwrap().pilot_reply =
            "Holding position, TEST123.".into();
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        assert!(
            post_copilot_prepare(AxumState(shared.clone()))
                .await
                .status()
                .is_success()
        );
        let old = shared
            .read()
            .await
            .pending_copilot_reply
            .as_ref()
            .unwrap()
            .sequence;
        let count = shared.read().await.session.transcript.len();
        assert!(
            post_copilot_prepare(AxumState(shared.clone()))
                .await
                .status()
                .is_success()
        );
        assert_eq!(shared.read().await.session.transcript.len(), count);
        shared.write().await.pending_copilot_reply = None;
        assert!(
            post_copilot_prepare(AxumState(shared.clone()))
                .await
                .status()
                .is_success()
        );
        let current = shared
            .read()
            .await
            .pending_copilot_reply
            .as_ref()
            .unwrap()
            .sequence;
        assert_ne!(old, current);
        assert!(
            !post_copilot_reply(AxumState(shared.clone()), Json(json!({"sequence":old})))
                .await
                .status()
                .is_success()
        );
        assert_eq!(
            shared
                .read()
                .await
                .pending_copilot_reply
                .as_ref()
                .unwrap()
                .sequence,
            current
        );
        let mut telemetry = shared.read().await.session.telemetry.clone();
        telemetry.com1_khz = 118_100;
        assert!(
            crate::surface::post_telemetry(AxumState(shared.clone()), Json(telemetry))
                .await
                .status()
                .is_success()
        );
        assert!(shared.read().await.pending_copilot_reply.is_none());
        assert!(
            !post_copilot_reply(AxumState(shared.clone()), Json(json!({"sequence":current})))
                .await
                .status()
                .is_success()
        );
        assert!(shared.read().await.pending_copilot_reply.is_none());
        assert_eq!(shared.read().await.session.transcript.len(), count + 1);
    }

    #[tokio::test]
    async fn plan_import_keeps_preflight_exchange_but_resets_a_finished_flight() {
        let mut state = crate::test_state();
        state.session.demo = false;
        state.session.telemetry.com1_khz = 121_400;
        add_transmission(
            &mut state.session,
            "GROUND",
            "Chocks removed.",
            &SpeechTag::default(),
        );
        state.session.mission.running = true;
        state.session.mission.id = "preflight-story".into();
        let sequence = state.session.next_sequence;
        let plan = openatc_core::state::FlightPlan {
            departure: "TEST".into(),
            destination: "DEST".into(),
            callsign: "TEST123".into(),
            cruise_feet: 5000,
            initial_altitude_feet: 3000,
            ..Default::default()
        };
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        assert!(
            post_plan(AxumState(shared.clone()), Json(plan.clone()))
                .await
                .status()
                .is_success()
        );
        {
            let mut s = shared.write().await;
            assert_eq!(s.session.next_sequence, sequence);
            assert_eq!(s.session.transcript.len(), 1);
            assert_eq!(s.session.mission.id, "preflight-story");
            s.session
                .on_event(openatc_core::aircraft::AircraftEvent::Finished);
            s.session.emergency_declared = true;
        }
        assert!(
            post_plan(AxumState(shared.clone()), Json(plan))
                .await
                .status()
                .is_success()
        );
        let s = shared.read().await;
        assert_eq!(s.session_generation, 1);
        assert_eq!(s.session.next_sequence, sequence);
        assert_eq!(s.session.telemetry.com1_khz, 121_400);
        assert!(!s.session.demo);
        assert_eq!(s.session.phase, PhaseCode::Parked);
        assert!(s.session.transcript.is_empty());
        assert!(s.session.mission.id.is_empty());
        assert!(!s.session.emergency_declared);
    }

    #[test]
    fn controller_assignments_use_and_remember_the_provider_roster() {
        let mut state = crate::test_state();
        let settings = Settings::default();
        state.voices_source = settings.tts_url.clone();
        let names = [
            "bella", "bruno", "hugo", "jasper", "kiki", "leo", "luna", "rosie",
        ];
        let mut records: Vec<_> = names
            .iter()
            .map(|name| serde_json::json!({"id": format!("kitten-{name}"), "language": "en-US"}))
            .collect();
        records.push(serde_json::json!({"id": "french-only", "language": "fr-FR"}));
        state.voices =
            openatc_core::voices::parse(&serde_json::json!({"voices": records})).unwrap();
        let mut assigned = std::collections::BTreeSet::new();
        for index in 0..names.len() {
            let key = format!("station:{index}");
            let voice = assign_airspace_controller(&mut state, &settings, &key, "neutral").voice;
            assert!(voice.starts_with("kitten-"));
            assert!(
                assigned.insert(voice.clone()),
                "Use every available voice before reusing one"
            );
            assert_eq!(
                assign_airspace_controller(&mut state, &settings, &key, "neutral").voice,
                voice
            );
        }
        let saved = state.controllers.clone();
        state.controllers = serde_json::from_slice(
            &std::fs::read(state.config_dir.join("controllers.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(state.controllers, saved);
        let voice =
            assign_airspace_controller(&mut state, &settings, "station:overflow", "neutral").voice;
        assert!(assigned.contains(&voice));
    }

    #[test]
    fn crew_prompt_includes_every_matching_phrase() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../speech");
        let pool = openatc_core::speech::load_speech_dir(&path).unwrap();
        let entries = pool.select("copilot", &[], "parked", "icao", usize::MAX);
        assert!(entries.len() > 4, "fixture must exercise the old cutoff");
        let prompt = few_shots(&pool, "copilot", &[], "parked");
        let slots = sample_slots();
        for entry in entries {
            for phrase in &entry.say {
                assert!(
                    prompt.contains(&render_template(phrase, &slots)),
                    "{}",
                    entry.id
                );
            }
        }
        assert!(prompt.contains("not scripts to copy"));
    }

    #[test]
    fn live_busy_frequency_never_invents_aircraft() {
        let mut session = State {
            demo: false,
            ..Default::default()
        };
        let settings = Settings {
            congestion: Congestion::Busy,
            ..Default::default()
        };
        for _ in 0..100 {
            maybe_chatter(&mut session, &settings, &[], &SpeechTag::default());
        }
        assert!(session.transcript.is_empty());
    }
}

#[cfg(test)]
mod callsign_tests {
    use super::*;

    #[test]
    fn filed_callsign_wins_over_the_settings_default() {
        let mut plan = openatc_core::state::FlightPlan {
            callsign: "QFA123".into(),
            ..Default::default()
        };
        fill_plan_callsign(&mut plan, "VH-BIL");
        assert_eq!(plan.callsign, "QFA123");
        assert!(plan.registration.is_empty());

        let mut empty = openatc_core::state::FlightPlan::default();
        fill_plan_callsign(&mut empty, "VH-BIL");
        assert_eq!(empty.callsign, "VH-BIL");
        assert_eq!(empty.registration, "VH-BIL");
    }
}
