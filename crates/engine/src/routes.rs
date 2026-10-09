//! HTTP endpoints for dialogue, flight plans, simulator data and speech.

use super::Shared;
use super::support::{chat_complete, clean_reply, service_ok};
use axum::{
    Json,
    extract::{FromRequest, State as AxumState},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use openatc_core::airport::{demo_airport, load_airport_from_simulator};
use openatc_core::apply::{
    AssignOrder, RequestContext, SimpleRng, add_transmission, apply_request, assign_controller,
};
use openatc_core::catalog::render_template;
use openatc_core::identify::interpret_text;
use openatc_core::intents::{Realism, discipline_preset};
use openatc_core::ops::{delivery_preset, parse_voice_pool, validate_flight_plan};
use openatc_core::regions::{region_for, units_for_region};
use openatc_core::simbrief::parse_simbrief;
use openatc_core::speech::{
    PromptContext, SpeechPool, build_prompt, parse_ai_output, sample_slots,
};
use openatc_core::state::{PhaseCode, Request, SpeechTag, State, TaxiClearance};
use openatc_core::{UnitSystem, resolve_units};
use openatc_settings::{Congestion, Delivery, Settings, Units};
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

/// Strip nulls recursively so AI JSON parses into requests with defaults.
fn drop_nulls(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, value| !value.is_null());
            for value in map.values_mut() {
                drop_nulls(value);
            }
        }
        Value::Array(items) => {
            for item in items {
                drop_nulls(item);
            }
        }
        _ => {}
    }
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
    Json(json!({
        "stt": service_ok(&client, &stt).await,
        "tts": service_ok(&client, &tts).await,
    }))
}

pub async fn full_state(AxumState(state): AxumState<Shared>) -> Json<Value> {
    let state = state.read().await;
    Json(snapshot(&state.session, &state.settings))
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
        return bad_request("Cannot save configuration");
    }
    state.settings = proposed.clone();
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
        return bad_request("Invalid X-Plane folder");
    };
    if !std::path::Path::new(root).is_dir() {
        return bad_request("Invalid X-Plane folder");
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
        return bad_request("Debug mode required");
    }
    state.session_generation = state.session_generation.wrapping_add(1);
    state.session.crew_actions.clear();
    state.session.taxi_clearance.holding_channel = 0;
    Json(json!({"accepted":true})).into_response()
}

pub async fn post_demo(AxumState(state): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    let mut state = state.write().await;
    if !state.session.demo {
        return bad_request("Demo controls are unavailable while connected to X-Plane.");
    }
    let phase = body.get("phase").and_then(Value::as_i64).unwrap_or(0);
    if !(0..=10).contains(&phase) {
        return bad_request("Invalid phase");
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
        return bad_request("Invalid telemetry");
    };
    state.session.telemetry = telemetry;
    state.session.phase = phase;
    state.session.has_departed = !state.session.telemetry.on_ground;
    let snapshot = snapshot(&state.session, &state.settings);
    Json(snapshot).into_response()
}

pub async fn session_reset(AxumState(state): AxumState<Shared>) -> Json<Value> {
    let mut state = state.write().await;
    state.session_generation = state.session_generation.wrapping_add(1);
    let telemetry = state.session.telemetry.clone();
    let demo = state.session.demo;
    let next_sequence = state.session.next_sequence;
    state.session = State::default();
    state.flow = super::flight::FlightFlow::default();
    state.crew.checklist = None;
    state.crew.pushback = None;
    state.expectations.clear();
    state.airport = None;
    state.session.next_sequence = next_sequence;
    state.session.telemetry = telemetry;
    state.session.demo = demo;
    if demo {
        "09".clone_into(&mut state.session.plan.runway);
    }
    Json(snapshot(&state.session, &state.settings))
}

pub async fn session_save(AxumState(state): AxumState<Shared>) -> Response {
    let state = state.read().await;
    let path = state.config_dir.join("session.json");
    let value = serde_json::to_value(&state.session).unwrap_or(Value::Null);
    if super::support::save_json(&path, &value).is_err() {
        return bad_request("Cannot save configuration");
    }
    Json(json!({"saved": true})).into_response()
}

pub async fn session_load(AxumState(state): AxumState<Shared>) -> Response {
    let path = state.read().await.config_dir.join("session.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return bad_request("No saved session");
    };
    let Ok(restored) = serde_json::from_str::<State>(&text) else {
        return bad_request("No saved session");
    };
    let mut state = state.write().await;
    if !state.session.demo {
        return bad_request("Cannot restore over a connected simulator flight");
    }
    state.session = restored;
    state.session.taxi_clearance = TaxiClearance::default();
    let snapshot = snapshot(&state.session, &state.settings);
    Json(snapshot).into_response()
}

pub async fn post_plan(
    AxumState(state): AxumState<Shared>,
    Json(plan): Json<openatc_core::state::FlightPlan>,
) -> Response {
    if validate_flight_plan(&plan).is_err() {
        return bad_request("Check ICAO codes, callsign and initial/cruise altitudes.");
    }
    let mut state = state.write().await;
    if state.session.phase != PhaseCode::Parked && state.session.phase != PhaseCode::Finished {
        return bad_request("Finish or reset this flight before replacing its plan.");
    }
    let telemetry = state.session.telemetry.clone();
    let demo = state.session.demo;
    state.session = State::default();
    state.session.plan = plan;
    state.session.telemetry = telemetry;
    state.session.demo = demo;
    let snapshot = snapshot(&state.session, &state.settings);
    Json(snapshot).into_response()
}

pub async fn post_parking(
    AxumState(state): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let Some(stand) = body.get("stand").and_then(Value::as_str) else {
        return bad_request("Invalid stand");
    };
    let mut state = state.write().await;
    stand.clone_into(&mut state.session.plan.arrival_stand);
    Json(json!({"accepted": true})).into_response()
}

/// Load the current simulator airport for the local services page and controller.
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
        return bad_request("Set a destination ICAO in Flight Plan.");
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
        return bad_request("Airport request must be an object");
    }
    body["active"] = Value::Bool(true);
    post_airport_load(state, Json(body)).await
}

pub async fn post_airport_load(
    AxumState(state): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let Some(icao) = body.get("icao").and_then(Value::as_str) else {
        return bad_request("Enter a four-character ICAO.");
    };
    if icao.len() != 4 || !icao.chars().all(|value| value.is_ascii_alphanumeric()) {
        return bad_request("Enter a four-character ICAO.");
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
    Json(serde_json::to_value(&loaded).unwrap_or(Value::Null)).into_response()
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
        return bad_request("Enter station ICAOs separated by commas.");
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
        return bad_request("Enter station ICAOs separated by commas");
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
        return bad_request("Enter your numeric SimBrief Pilot ID in Settings.");
    }
    let client = state.read().await.client.clone();
    let reply = client
        .get(format!(
            "https://www.simbrief.com/api/xml.fetcher.php?userid={identifier}&json=1"
        ))
        .send()
        .await;
    let Ok(reply) = reply else {
        return bad_request(
            "SimBrief download failed. Check Pilot ID and generate a flight first.",
        );
    };
    if !reply.status().is_success() {
        return bad_request(
            "SimBrief download failed. Check Pilot ID and generate a flight first.",
        );
    }
    let Ok(document) = reply.json::<Value>().await else {
        return bad_request(
            "SimBrief download failed. Check Pilot ID and generate a flight first.",
        );
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
            return bad_request("Invalid multipart body");
        };
        while let Ok(Some(field)) = fields.next_field().await {
            if field.name() == Some("file") {
                audio = field.bytes().await.unwrap_or_default().to_vec();
            }
        }
    } else {
        audio = match axum::body::to_bytes(request.into_body(), 32 * 1024 * 1024).await {
            Ok(bytes) => bytes.to_vec(),
            Err(_) => return bad_request("Invalid audio body"),
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
            "STT request failed. Check endpoint, model and OPENATC_STT_KEY on the engine."
                .to_owned(),
        );
    };
    if !reply.status().is_success() {
        return bad_request(
            "STT request failed. Check endpoint, model and OPENATC_STT_KEY on the engine."
                .to_owned(),
        );
    }
    let Ok(mut transcription) = reply.json::<Value>().await else {
        return bad_request(
            "STT request failed. Check endpoint, model and OPENATC_STT_KEY on the engine."
                .to_owned(),
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

pub async fn post_speak(AxumState(state): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    let Some(text) = body.get("text").and_then(Value::as_str) else {
        return bad_request("Speech text too long");
    };
    if text.len() > 4000 {
        return bad_request("Speech text too long");
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
        return bad_request("Unknown speaker");
    }
    let settings = state.read().await.settings.clone();
    let client = state.read().await.client.clone();
    let (mut voice, delivery, mut speed) = tts_voice(&body, &settings, &speaker);
    if !openatc_core::ops::english_voice(&voice) {
        voice = "alloy".to_owned();
    }
    let urgent = body.get("urgent").and_then(Value::as_bool).unwrap_or(false);
    let preset = delivery_preset(&delivery);
    let automated_atis = delivery == "atis";
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
        1.0
    } else {
        (settings.tts_speed * speed).clamp(0.5, 2.0)
    };
    let effects_on = settings.radio_effects
        && (speaker != "cabin" || settings.radio_fx_cabin)
        && (speaker != "ground" || settings.radio_fx_ground);
    let hiss = if effects_on { settings.radio_hiss } else { 0.0 };
    let crackle = if effects_on {
        settings.radio_crackle
    } else {
        0.0
    };
    let static_ = if effects_on {
        settings.radio_static
    } else {
        0.0
    };
    let bandpass = effects_on && settings.radio_bandpass;
    let tts_body = json!({
        "model": settings.tts_model,
        "voice": voice,
        "input": text,
        "response_format": "wav",
        "speed": speed,
        "sentence_pause": sentence_pause,
        "clause_pause": clause_pause,
        "effects": {"hiss": hiss, "crackle": crackle, "static": static_, "bandpass": bandpass},
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
                "OpenATC TTS transport failure: speaker={speaker} voice={voice} model={} text={text:?} error={error}",
                settings.tts_model
            );
            return bad_request(
                "TTS connection failed; see engine.log for the failed text and error.",
            );
        }
    };
    if !reply.status().is_success() {
        let status = reply.status();
        eprintln!(
            "OpenATC TTS provider failure: status={status} speaker={speaker} voice={voice} model={} text={text:?}",
            settings.tts_model
        );
        return bad_request(
            "TTS request failed. Check endpoint, model and OPENATC_TTS_KEY on the engine."
                .to_owned(),
        );
    }
    let Ok(bytes) = reply.bytes().await else {
        eprintln!(
            "OpenATC TTS response read failure: speaker={speaker} voice={voice} text={text:?}"
        );
        return bad_request(
            "TTS request failed. Check endpoint, model and OPENATC_TTS_KEY on the engine."
                .to_owned(),
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
    let request = { openatc_core::readback::auto_reply(&shared.read().await.session) };
    let Some(request) = request else {
        return bad_request("No ATC instruction is awaiting your reply.");
    };
    post_request(
        AxumState(shared),
        Json(serde_json::to_value(request).unwrap_or(Value::Null)),
    )
    .await
}

/// Record the crew transmission without advancing clearance or surface state.
pub async fn post_copilot_prepare(AxumState(shared): AxumState<Shared>) -> Response {
    let mut state = shared.write().await;
    if !state.settings.copilot_replies {
        return bad_request("Copilot readbacks are disabled.");
    }
    let Some(mut request) = openatc_core::readback::auto_reply(&state.session) else {
        return bad_request("No ATC instruction is awaiting your reply.");
    };
    let tag = SpeechTag {
        voice: state.settings.copilot_voice.clone(),
        delivery: delivery_name(state.settings.copilot_delivery).into(),
        speed: state.settings.copilot_speed,
        ..Default::default()
    };
    add_transmission(&mut state.session, "COPILOT", &request.text, &tag);
    request.controller_initiated = true;
    state.pending_copilot_reply = Some((state.session_generation, request));
    Json(json!({"state":snapshot(&state.session,&state.settings)})).into_response()
}

/// Apply the prepared readback only after its playback has completed.
pub async fn post_copilot_reply(AxumState(shared): AxumState<Shared>) -> Response {
    let request = {
        let mut state = shared.write().await;
        match state.pending_copilot_reply.take() {
            Some((generation, request))
                if generation == state.session_generation && state.settings.copilot_replies =>
            {
                request
            }
            _ => return bad_request("No prepared copilot reply for this flight."),
        }
    };
    let settings = shared.read().await.settings.clone();
    atc_request(&shared, &settings, request).await
}

/// Shared request handling: station roles, controller and crew replies, and readbacks.
pub async fn post_request(
    AxumState(shared): AxumState<Shared>,
    Json(body): Json<Value>,
) -> Response {
    let generation = shared.read().await.session_generation;
    let mut request: Request = match serde_json::from_value(body) {
        Ok(request) => request,
        Err(_) => return bad_request("Invalid request"),
    };
    if request.text.len() > 4000 {
        return bad_request("Message too long");
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
    let settings = shared.read().await.settings.clone();
    if let Some(response) = crate::crew::request(&shared, &request).await {
        return response;
    }
    if role == "cabin" || role == "ground" {
        return crew_chat(&shared, &settings, &request, role == "cabin").await;
    }
    if role == "atc" {
        let state = shared.read().await;
        let stations = openatc_core::stations::nearby(&state.stations, &state.session.telemetry);
        let receiving = openatc_core::stations::tuned(&stations, state.session.telemetry.com1_khz);
        if state.station_root != state.settings.simulator_root
            || !state.session.telemetry.radio_power
            || receiving.is_none_or(|s| s.service == "ATIS" || s.service == "Unicom")
        {
            return Json(json!({"result":{"accepted":false,"message":"","silent":true},"state":snapshot(&state.session,&state.settings)})).into_response();
        }
    }
    if role == "atc" {
        openatc_core::readback::prepare(&shared.read().await.session, &mut request);
    }
    if request.intent == "conversation" && settings.ai_enabled && !settings.ai_model.is_empty() {
        match classify_request(&shared, &settings, &request, &role).await {
            Ok(classified) => request = classified,
            Err(error) => return bad_request(error.as_str()),
        }
    }
    if shared.read().await.session_generation != generation {
        return bad_request("Flight was reset; previous request discarded.");
    }
    if role == "copilot" {
        return copilot_chat(&shared, &settings, &request).await;
    }
    atc_request(&shared, &settings, request).await
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
    let greeting = attendant
        && [
            "how are you",
            "how's it going",
            "hello",
            "hi",
            "good morning",
            "good evening",
        ]
        .iter()
        .any(|g| {
            request
                .text
                .trim()
                .trim_end_matches(['?', '.', '!'])
                .eq_ignore_ascii_case(g)
        });
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
    let mut reply_text = if unconfirmed_service {
        openatc_core::dialogue::say("crew_ground_service_unconfirmed", &[])
    } else if greeting {
        openatc_core::dialogue::say("crew_cabin_greeting", &[])
    } else {
        off.clone()
    };
    if !greeting && !unconfirmed_service && settings.ai_enabled && !settings.ai_model.is_empty() {
        let system = if personality.is_empty() {
            base
        } else {
            format!("{personality} {base}")
        };
        let system = {
            let state = shared.read().await;
            let role = if attendant { "attendant" } else { "ground" };
            let phase = format!("{:?}", state.session.phase).to_lowercase();
            let live = format!(
                "\nLive flight phase: {phase}. On ground: {}. Ground speed: {:.1} knots. Departure: {}. Destination: {}. Answer only the captain's current request. Examples are not current events. Do not claim an action was performed unless simulator confirmation is supplied.\n",
                state.session.telemetry.on_ground,
                state.session.telemetry.ground_speed_knots,
                state.session.plan.departure,
                state.session.plan.destination
            );
            system + &live + &few_shots(&state.speech, role, &[], &phase)
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
        // keep AI text
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
        return bad_request("Flight was reset; previous request discarded.");
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
    classify += " Recover clear typing errors and speech-recognition mistakes in request words regardless of phraseology settings. Preserve all operational identifiers and numeric values; never fill missing readback items from the issued clearance. Ask for clarification by returning conversation when the intended request or value is ambiguous.";
    if preset == "relaxed" {
        classify += " Accept casual phrasing and slang; resolve the best-guess intent.";
    } else if preset == "real" {
        classify += " Require standard phraseology; use conversation when unsure.";
    }
    let client = shared.read().await.client.clone();
    let mut call = client.post(format!("{}/v1/chat/completions", settings.ai_url));
    if let Ok(key) = std::env::var("OPENATC_AI_KEY") {
        call = call.header("Authorization", format!("Bearer {key}"));
    }
    let reply = call
        .json(&json!({
            "model": settings.ai_model,
            "temperature": 0,
            "messages": [
                {"role": "system", "content": classify},
                {"role": "user", "content": request.text},
            ],
        }))
        .send()
        .await;
    let Ok(reply) = reply else {
        return Err(
            "AI endpoint unavailable. Use request buttons or supported text commands.".to_owned(),
        );
    };
    if !reply.status().is_success() {
        return Err(
            "AI endpoint unavailable. Use request buttons or supported text commands.".to_owned(),
        );
    }
    let Ok(parsed) = reply.json::<Value>().await else {
        return Err(
            "AI endpoint unavailable. Use request buttons or supported text commands.".to_owned(),
        );
    };
    let Some(content) = parsed
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
    else {
        return Err(
            "AI endpoint unavailable. Use request buttons or supported text commands.".to_owned(),
        );
    };
    let mut classification: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    drop_nulls(&mut classification);
    let Ok(mut interpreted) = serde_json::from_value::<Request>(classification) else {
        return Err(
            "AI endpoint unavailable. Use request buttons or supported text commands.".to_owned(),
        );
    };
    interpreted.text.clone_from(&request.text);
    role.clone_into(&mut interpreted.role);
    if interpreted.intent == "readback" {
        openatc_core::readback::prepare(&shared.read().await.session, &mut interpreted);
    }
    Ok(interpreted)
}

async fn copilot_chat(shared: &Shared, settings: &Settings, request: &Request) -> Response {
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
        system += " The captain addressed an ATC task to you, but you cannot transmit on frequency. Briefly redirect them to say it themselves, then offer to read it back.";
    }
    {
        let state = shared.read().await;
        let phase = format!("{:?}", state.session.phase).to_lowercase();
        system += &few_shots(&state.speech, "copilot", &[], &phase);
    }
    let voice = if settings.copilot_voice.is_empty() {
        "alloy"
    } else {
        &settings.copilot_voice
    };
    let off = openatc_core::dialogue::say("copilot_chat_needs_ai_intent_classification_on", &[]);
    let busy = openatc_core::dialogue::say("copilot_line_is_busy_try_again", &[]);
    let mut reply_text = off.clone();
    if settings.ai_enabled && !settings.ai_model.is_empty() {
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
        return bad_request("Flight was reset; previous request discarded.");
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
    let mut pool = openatc_core::ops::english_voice_pool(&settings.voice_pool);
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
        let duplicate = state
            .controllers
            .assignments
            .iter()
            .any(|(key, c)| key != airspace && identity(&c.voice) == identity(&saved.voice));
        if !valid || (duplicate && !unused.is_empty()) {
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
        position,
        voice: controller.voice.clone(),
        delivery: controller.delivery.clone(),
        speed: controller.speed,
        urgent: false,
    }
}

pub(crate) async fn atc_request(
    shared: &Shared,
    settings: &Settings,
    request: Request,
) -> Response {
    let generation = shared.read().await.session_generation;
    if settings.congestion != Congestion::Off {
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
            openatc_core::stations::tuned(&stations, state.session.telemetry.com1_khz).cloned()
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
        "checkin",
        "position",
    ]
    .contains(&request.intent.as_str())
    {
        let root = shared.read().await.settings.simulator_root.clone();
        let id = receiving.airport.clone();
        if let Ok(Ok(airport)) = tokio::task::spawn_blocking(move || {
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
            return bad_request("Receiving airport operational data unavailable");
        }
    }
    let mut state = shared.write().await;
    if state.session_generation != generation {
        return bad_request("Flight was reset; previous request discarded.");
    }

    let current_stations =
        openatc_core::stations::nearby(&state.stations, &state.session.telemetry);
    if state.station_root != state.settings.simulator_root
        || !state.session.telemetry.radio_power
        || !openatc_core::stations::tuned(&current_stations, state.session.telemetry.com1_khz)
            .is_some_and(|s| {
                s.airport == receiving.airport
                    && s.khz == receiving.khz
                    && s.service == receiving.service
            })
    {
        return Json(json!({"result":{"accepted":false,"message":"","silent":true},"state":snapshot(&state.session,&state.settings)})).into_response();
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
            &receiving.name,
            openatc_core::dialogue::say("departure_clearance_is_already_active_read_back", &[]),
        );
    }
    if !openatc_core::stations::station_serves(&receiving, &request.intent) {
        let wanted = match request.intent.as_str() {
            "clearance" => "Clearance",
            "ready" | "backtrack" | "cross_runway" | "landing" => "Tower",
            _ => "Ground",
        };
        let station = state.stations.iter().find(|s| {
            s.airport == receiving.airport
                && openatc_core::stations::station_serves(s, &request.intent)
        });
        let text = station.map_or_else(
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
        add_transmission(&mut state.session, "ATC", &text, &tag);
        return Json(json!({"result":{"accepted":false,"message":text},"state":snapshot(&state.session,&state.settings)})).into_response();
    }
    if ["weather", "altimeter"].contains(&request.intent.as_str()) {
        let Some((sample, _)) = state
            .sim_weather
            .get(&receiving.airport)
            .filter(|(_, t)| t.elapsed().as_secs() < 45)
        else {
            return bad_request("Simulator airport weather unavailable");
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
            return clearance_rejection(&mut state, &receiving.name, error);
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
            return clearance_rejection(&mut state, &receiving.name, message);
        }
        if !airport.as_ref().is_some_and(|a| {
            a.runways.iter().any(|r| {
                r.first_name == state.session.plan.runway
                    || r.second_name == state.session.plan.runway
            })
        }) {
            return clearance_rejection(
                &mut state,
                &receiving.name,
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
    atc_tag.position.clone_from(&receiving.name);
    let pilot_tag = SpeechTag {
        voice: settings.pilot_voice.clone(),
        delivery: "standard".to_owned(),
        speed: settings.pilot_speed,
        ..Default::default()
    };
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
    if let Some(result) = super::flight::request(&mut state, &receiving, &request, &atc_tag) {
        return Json(json!({"result": result, "state":snapshot(&state.session,&state.settings)}))
            .into_response();
    }
    let ctx = RequestContext {
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
    };
    let mut result = apply_request(&mut state.session, &ctx);
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
            .is_some_and(|c| c.acknowledged)
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
        state.session.frequency_sequence = state.session.next_sequence;
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
    let role_description = match receiving.service.as_str() {
        "Clearance" => {
            "You are the clearance-delivery controller, issuing the recorded IFR route, initial altitude, squawk and departure runway."
        }
        "Ground" => {
            "You are the ground controller, coordinating authorized surface movements and runway hold restrictions."
        }
        "Tower" => {
            "You are the tower controller, responsible for runway operations. You may cover delivery or ground duties only when the supplied station services and recorded instruction authorize that duty."
        }
        "Approach" => {
            "You are the approach controller, managing the recorded terminal-arrival instructions."
        }
        "Departure" => {
            "You are the departure controller, managing the recorded climb and departure instructions."
        }
        "Center" => {
            "You are the area controller, managing the recorded en-route instructions and handoffs."
        }
        _ => {
            "You are the tuned station controller. Speak only within the supplied station services and recorded instruction."
        }
    };
    let phase = format!("{:?}", state.session.phase).to_lowercase();
    let rules = if state.session.ifr { "ifr" } else { "vfr" };
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
        let system = format!(
            "Sound natural and concise as an air traffic controller. Use the examples as style references, not scripts to copy. Compose fresh wording where filler permits; standard ATC terms may repeat. Example values are fictional. You may adjust filler words only. Preserve every operational token, instruction and value in its original order. Never add commands, change numbers, remove negations or introduce facts. Return only the transmission. Service: {}. {role_description} Your current task is {intent}; the live flight phase is {phase}. The controller decision is already made; you are wording it, not making a new decision. All supplied examples are fictional style references. Do not copy a full example verbatim. Previous transmission for this station and task: {previous_wording:?}. Avoid repeating that wording where safe filler permits; standard operational terms must remain exact. Style examples:\n{examples}",
            receiving.service
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
        return bad_request("Flight was reset; previous request discarded.");
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
        return bad_request("Unknown role".to_owned());
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
            return bad_request("flightRules must be ifr or vfr".to_owned());
        }
        let examples =
            state
                .speech
                .select_for_airport(role, &tags, &phase, airport, flight_rules, usize::MAX);
        if examples.is_empty() {
            return bad_request("No examples for that role and situation".to_owned());
        }
        let mut prompt = build_prompt(&examples, &context);
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
        return bad_request("AI generation is off".to_owned());
    }
    match chat_complete(
        &client,
        &settings.ai_url,
        &settings.ai_model,
        0.3,
        &prompt,
        "Speak.",
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
    station: &str,
    message: impl Into<String>,
) -> Response {
    let message = message.into();
    let tag = SpeechTag {
        position: station.to_owned(),
        voice: state.settings.voice.clone(),
        ..Default::default()
    };
    add_transmission(&mut state.session, "ATC", &message, &tag);
    Json(json!({"result":{"accepted":false,"message":message},"state":snapshot(&state.session,&state.settings)})).into_response()
}

#[cfg(test)]
mod chatter_tests {
    use super::*;
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
