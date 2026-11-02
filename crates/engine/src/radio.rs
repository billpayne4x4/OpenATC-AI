//! Station discovery and simulator-authoritative airport weather/ATIS.

#![allow(clippy::default_trait_access)]
use crate::Shared;
use axum::{
    Json,
    extract::State as AxumState,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

/// End only the affected receiver's ATIS log session when tuning or power changes.
pub(crate) fn observe_atis_tuning(
    state: &mut crate::EngineState,
    previous: [Option<i32>; 2],
    was_powered: bool,
) {
    use openatc_core::state::Radio;
    for (index, radio) in [Radio::Com1, Radio::Com2].into_iter().enumerate() {
        if !state.session.telemetry.radio_power
            || !was_powered
            || previous[index] != state.session.telemetry.frequency(radio)
        {
            let prefix = format!("{radio:?}/");
            state.atis_logged.retain(|key, _| !key.starts_with(&prefix));
        }
    }
}

/// Wording history is separate from flight state, so a new flight keeps phrase rotation.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PhraseHistory {
    pub choices: std::collections::BTreeMap<String, usize>,
    pub templates: std::collections::BTreeMap<String, String>,
    pub last: std::collections::BTreeMap<String, String>,
}
impl PhraseHistory {
    pub fn load(dir: &std::path::Path) -> Self {
        std::fs::read(dir.join("phrase-history.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, dir: &std::path::Path) {
        if let Ok(bytes) = serde_json::to_vec(self) {
            let temporary = dir.join("phrase-history.json.tmp");
            if let Err(error) = std::fs::write(&temporary, bytes)
                .and_then(|()| std::fs::rename(temporary, dir.join("phrase-history.json")))
            {
                eprintln!("Could not save phrase history: {error}");
            }
        }
    }
}

fn error(text: impl Into<String>) -> Response {
    let text = text.into();
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(json!({"error":text})),
    )
        .into_response()
}
/// Requests must work even when the pilot has never opened the station browser.
pub async fn ensure_index(shared: &Shared) -> Result<(), String> {
    let scan = shared.read().await.station_index_lock.clone();
    let _scan = scan.lock().await;
    let (root, old) = {
        let s = shared.read().await;
        (s.settings.simulator_root.clone(), s.station_root.clone())
    };
    if root.is_empty() {
        return Err("Set the X-Plane folder".into());
    }
    if root != old {
        let copy = root.clone();
        let workers = shared.read().await.workers.clone();
        let result = workers
            .run(move || {
                let stations = openatc_core::stations::load_index(&copy)?;
                let airports = openatc_core::stations::map_airports(&copy)?;
                Ok::<_, String>((stations, airports))
            })
            .await;
        let index = match result {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err("Station indexing failed".into()),
        };
        let mut s = shared.write().await;
        if s.settings.simulator_root == root {
            s.map_root.clone_from(&root);
            s.station_root = root;
            s.stations = index.0;
            s.map_airports = index.1;
            s.sim_weather.clear();
            s.atis_information.clear();
            s.atis_runways.clear();
            s.atis_last_runway.clear();
            s.atis_logged.clear();
        }
    }
    Ok(())
}

/// Build the station index on a worker without making the first UI request wait for it.
/// The station browser polls this route, so an indexing reply is better than a timed-out
/// request that leaves the UI loading forever.
pub fn warm_station_index(shared: Shared) {
    tokio::spawn(async move {
        {
            let mut state = shared.write().await;
            if state.station_indexing {
                return;
            }
            state.station_indexing = true;
        }
        let _ = ensure_index(&shared).await;
        shared.write().await.station_indexing = false;
    });
}

/// Index scenery outside the state lock and return shared reception results.
pub async fn nearby(AxumState(shared): AxumState<Shared>, Json(_): Json<Value>) -> Response {
    if shared.read().await.settings.simulator_root.is_empty() {
        return Json(
            json!({"stations":[], "occupiedAirport":null, "notice":"Set the X-Plane folder"}),
        )
        .into_response();
    }
    let (root, old, indexing) = {
        let s = shared.read().await;
        (
            s.settings.simulator_root.clone(),
            s.station_root.clone(),
            s.station_indexing,
        )
    };
    if root != old || indexing {
        warm_station_index(shared.clone());
        return Json(json!({
            "stations": [],
            "occupiedAirport": null,
            "notice": "Indexing installed radio stations"
        }))
        .into_response();
    }
    let s = shared.read().await;
    let airport = openatc_core::stations::occupied_airport(&s.map_airports, &s.session.telemetry)
        .map(|a| a.id.as_str());
    Json(json!({"stations":openatc_core::stations::nearby(&s.stations,&s.session.telemetry),
        "occupiedAirport": airport,
        "notice": if s.stations.is_empty() { "No radio stations found in the installed scenery" } else { "" }}))
        .into_response()
}
fn weather_signature(body: &Value) -> String {
    format!(
        "{:.0}/{:.0}/{:.0}/{:.0}/{:.0}/{:.0}/{}",
        body["windDegrees"].as_f64().unwrap_or(0.) / 10.,
        body["windKnots"].as_f64().unwrap_or(0.) / 5.,
        body["pressureHpa"].as_f64().unwrap_or(0.),
        body["visibilityMeters"].as_f64().unwrap_or(0.) / 1000.,
        body["temperatureC"].as_f64().unwrap_or(0.),
        body["dewpointC"].as_f64().unwrap_or(0.),
        body.get("cloudSignature").map_or_else(
            || body["clouds"].as_str().unwrap_or("").to_owned(),
            std::string::ToString::to_string
        )
    )
}

/// Store actual simulator samples, without accepting invented/stale observations.
pub(crate) fn weather_location(
    state: &crate::EngineState,
    id: &str,
) -> Option<openatc_core::state::WeatherAirport> {
    if let Some(airport) = [state.airport.as_ref(), state.arrival_airport.as_ref()]
        .into_iter()
        .flatten()
        .chain(state.generated.operational_airports())
        .find(|airport| airport.icao == id)
    {
        return Some(openatc_core::state::WeatherAirport {
            airport: id.into(),
            latitude: airport.reference_latitude,
            longitude: airport.reference_longitude,
            elevation_feet: airport.elevation_feet,
        });
    }
    state
        .stations
        .iter()
        .find(|station| station.airport == id)
        .map(|station| openatc_core::state::WeatherAirport {
            airport: id.into(),
            latitude: station.latitude,
            longitude: station.longitude,
            elevation_feet: station.elevation_feet,
        })
}

pub async fn weather(AxumState(shared): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    let Some(airport) = body.get("airport").and_then(Value::as_str) else {
        return error(openatc_core::dialogue::say(
            "notice_airport_required_af12598c",
            &[],
        ));
    };
    if ![
        "temperatureC",
        "dewpointC",
        "pressureHpa",
        "windDegrees",
        "windKnots",
        "visibilityMeters",
    ]
    .iter()
    .all(|k| {
        body.get(k)
            .and_then(Value::as_f64)
            .is_some_and(f64::is_finite)
    }) {
        return error(openatc_core::dialogue::say(
            "notice_incomplete_simulator_weather_a96360ec",
            &[],
        ));
    }
    if !(800.0..1100.0).contains(&body["pressureHpa"].as_f64().unwrap_or(0.)) {
        return error(openatc_core::dialogue::say(
            "notice_invalid_simulator_pressure_eb56730f",
            &[],
        ));
    }
    let mut s = shared.write().await;
    if s.station_root != s.settings.simulator_root {
        return error(openatc_core::dialogue::say(
            "notice_station_catalogue_not_ready_213e537d",
            &[],
        ));
    }
    let Some(station) = weather_location(&s, airport) else {
        return error(openatc_core::dialogue::say(
            "notice_unknown_weather_airport_53c2de2b",
            &[],
        ));
    };
    if body.get("magneticVariationDegrees").is_some_and(|value| {
        !value.is_null()
            && value
                .as_f64()
                .is_none_or(|number| !(-180.0..=180.0).contains(&number))
    }) {
        return error(openatc_core::dialogue::say(
            "notice_invalid_request_330509ca",
            &[],
        ));
    }
    if !["latitude", "longitude", "sampleAltitudeFeet"]
        .iter()
        .all(|k| {
            body.get(k)
                .and_then(Value::as_f64)
                .is_some_and(f64::is_finite)
        })
        || (body["latitude"].as_f64().unwrap_or(999.) - station.latitude).abs() > 0.001
        || (body["longitude"].as_f64().unwrap_or(999.) - station.longitude).abs() > 0.001
        || (body["sampleAltitudeFeet"].as_f64().unwrap_or(-9999.) - station.elevation_feet).abs()
            > 50.
        || !(0.0..=400.0).contains(&body["windKnots"].as_f64().unwrap_or(-1.))
        || !(0.0..=360.0).contains(&body["windDegrees"].as_f64().unwrap_or(-1.))
        || !(0.0..=1_000_000.0).contains(&body["visibilityMeters"].as_f64().unwrap_or(-1.))
        || !(-100.0..=70.0).contains(&body["temperatureC"].as_f64().unwrap_or(f64::NAN))
        || !(-100.0..=70.0).contains(&body["dewpointC"].as_f64().unwrap_or(f64::NAN))
    {
        return error(openatc_core::dialogue::say(
            "notice_weather_must_be_a_valid_airport_surface_sample_786d4f09",
            &[],
        ));
    }
    let signature = weather_signature(&body);
    let information = s
        .atis_information
        .entry(airport.to_owned())
        .or_insert((signature.clone(), 0));
    if information.0 != signature {
        information.0 = signature;
        information.1 = (information.1 + 1) % 26;
    }

    let airport = airport.to_owned();
    let sample = (body, std::time::Instant::now());
    s.generated.observe_weather(&airport, &sample);
    s.sim_weather.insert(airport, sample);
    Json(json!({"accepted":true})).into_response()
}
/// Deterministic simulated ATIS. Never use online METAR or aircraft-local weather.
pub async fn atis(AxumState(shared): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    let radio = match body.get("radio") {
        None => openatc_core::state::Radio::Com1,
        Some(value) => match serde_json::from_value(value.clone()) {
            Ok(radio) => radio,
            Err(_) => {
                return error(openatc_core::dialogue::say(
                    "notice_invalid_request_330509ca",
                    &[],
                ));
            }
        },
    };
    let s = shared.read().await;
    if s.station_root != s.settings.simulator_root {
        return error(openatc_core::dialogue::say(
            "notice_station_catalogue_not_ready_213e537d",
            &[],
        ));
    }
    if !s.session.telemetry.radio_power {
        return error(openatc_core::dialogue::say(
            "notice_radio_power_off_c2267a7a",
            &[],
        ));
    }
    let nearby = openatc_core::stations::nearby(&s.stations, &s.session.telemetry);
    let Some(station) = s
        .session
        .telemetry
        .frequency(radio)
        .and_then(|frequency| openatc_core::stations::tuned(&nearby, frequency))
    else {
        return error(openatc_core::dialogue::say(
            "notice_no_station_receiving_42659d5f",
            &[],
        ));
    };
    if station.service != "ATIS" {
        return error(openatc_core::dialogue::say(
            "notice_not_tuned_to_atis_f8ae1999",
            &[],
        ));
    }
    let station = station.clone();
    let Some((weather, when)) = s
        .sim_weather
        .get(&station.airport)
        .filter(|(_, t)| t.elapsed().as_secs() < 45)
    else {
        return error(openatc_core::dialogue::say(
            "notice_simulator_airport_weather_unavailable_8fc27abd",
            &[],
        ));
    };
    let weather = weather.clone();
    let observed_at = *when;
    let _ = when;
    let cached = s
        .airport
        .as_ref()
        .filter(|airport| airport.icao == station.airport)
        .or_else(|| {
            s.arrival_airport
                .as_ref()
                .filter(|airport| airport.icao == station.airport)
        })
        .map(|airport| airport.runways.clone())
        .filter(|runways| !runways.is_empty())
        .or_else(|| s.atis_runways.get(&station.airport).cloned());
    let root = s.settings.simulator_root.clone();
    let workers = s.workers.clone();
    drop(s);
    let runways = if let Some(runways) = cached {
        runways
    } else {
        let id = station.airport.clone();
        let path = root.clone();
        workers
            .run(move || {
                openatc_core::airport::load_airport_from_simulator(
                    &path,
                    &id,
                    &|path| std::path::Path::new(path).is_file(),
                    &|path| std::fs::read_to_string(path).map_err(|error| error.to_string()),
                )
                .map(|airport| airport.runways)
            })
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
    };
    let mut s = shared.write().await;
    if s.settings.simulator_root != root
        || s.session.telemetry.frequency(radio) != Some(station.khz)
        || observed_at.elapsed().as_secs() >= 45
    {
        return error(openatc_core::dialogue::say(
            "notice_not_tuned_to_atis_f8ae1999",
            &[],
        ));
    }
    if !runways.is_empty() {
        if s.atis_runways.len() >= 32
            && !s.atis_runways.contains_key(&station.airport)
            && let Some(key) = s.atis_runways.keys().next().cloned()
        {
            s.atis_runways.remove(&key);
            s.atis_last_runway.remove(&key);
        }
        s.atis_runways
            .insert(station.airport.clone(), runways.clone());
    }
    let number = |key: &str| weather[key].as_f64().unwrap_or(0.);
    let clouds = weather.get("clouds").and_then(Value::as_str).map_or_else(
        || openatc_core::dialogue::say("weather_cloud_report_unavailable", &[]),
        str::to_owned,
    );
    let runway = if station.airport == s.session.plan.departure {
        &s.session.plan.runway
    } else if station.airport == s.session.plan.destination {
        &s.session.plan.arrival_runway
    } else {
        ""
    };
    let runway = atis_runway(&runways, runway, number("windDegrees"), number("windKnots"));
    if !runway.is_empty()
        && s.atis_last_runway
            .insert(station.airport.clone(), runway.clone())
            .is_some_and(|previous| previous != runway)
        && let Some(information) = s.atis_information.get_mut(&station.airport)
    {
        information.1 = (information.1 + 1) % 26;
    }
    let letter = s.atis_information.get(&station.airport).map_or(0, |v| v.1);
    let code = openatc_core::dialogue::say(&format!("atis_information_{letter:02}"), &[]);
    let runway_text = if runway.is_empty() {
        openatc_core::dialogue::say("runway_information_unavailable", &[])
    } else {
        openatc_core::dialogue::say("atis_runway_in_use", &[("runway", (runway).clone())])
    };
    let pressure = if station.airport.starts_with('K') {
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
    let text = openatc_core::dialogue::say(
        "atis_broadcast",
        &[
            ("airport", (station.airport_name).clone()),
            ("code", code.clone()),
            ("wind_direction", format!("{:.0}", number("windDegrees"))),
            ("wind_speed", format!("{:.0}", number("windKnots"))),
            ("visibility", format!("{:.0}", number("visibilityMeters"))),
            ("clouds", (clouds).clone()),
            ("temperature", format!("{:.0}", number("temperatureC"))),
            ("dewpoint", format!("{:.0}", number("dewpointC"))),
            ("pressure", pressure),
            ("runway_text", (runway_text).clone()),
        ],
    );
    let mut tag = crate::flight::tag(&mut s, &station);
    tag.radio = Some(radio);
    tag.frequency = Some(station.khz);
    let prefix = format!("{radio:?}/");
    let log_key = format!("{prefix}{}", station.airport);
    let bulletin = format!("{code}|{}|{runway}", weather_signature(&weather));
    let repeated = s.atis_logged.get(&log_key) == Some(&bulletin);
    if !repeated {
        // A receiver has one current listening session. Retuning or a changed
        // bulletin adds a new row; unchanged audio loops leave history alone.
        s.atis_logged.retain(|key, _| !key.starts_with(&prefix));
        s.atis_logged.insert(log_key, bulletin);
        openatc_core::apply::add_transmission(&mut s.session, "ATIS", &text, &tag);
        s.session.transcript.last_mut().unwrap().information_only = true;
    }
    Json(json!({"text":text,"airport":station.airport,"information":code,"source":"simulator","frequency":station.khz,"radio":radio})).into_response()
}

pub(crate) fn atis_runway(
    runways: &[openatc_core::airport::Runway],
    assigned: &str,
    wind: f64,
    knots: f64,
) -> String {
    let mut candidates = Vec::new();
    for runway in runways {
        let length = (runway.second.east - runway.first.east)
            .hypot(runway.second.north - runway.first.north);
        if !length.is_finite() || length < 100.0 {
            continue;
        }
        let course = (runway.second.east - runway.first.east)
            .atan2(runway.second.north - runway.first.north)
            .to_degrees();
        for (name, course) in [
            (&runway.first_name, course),
            (&runway.second_name, course + 180.0),
        ] {
            if !name.is_empty() {
                candidates.push((
                    name.clone(),
                    knots * (wind - course).to_radians().cos(),
                    length,
                ));
            }
        }
    }
    candidates.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then(b.2.total_cmp(&a.2))
            .then(a.0.cmp(&b.0))
    });
    if let Some(best) = candidates.first()
        && let Some(planned) = candidates.iter().find(|candidate| candidate.0 == assigned)
        && (planned.1 - best.1).abs() < 0.01
    {
        return planned.0.clone();
    }
    candidates
        .first()
        .map_or_else(String::new, |candidate| candidate.0.clone())
}

/// Render the appropriate speech event after the structured controller decision.
pub fn apply_wording(
    state: &mut crate::EngineState,
    intent: &str,
    station: &openatc_core::stations::Station,
    result: &mut openatc_core::apply::RequestOutcome,
) {
    if !result.accepted {
        return;
    }
    let id = match intent {
        "clearance" => "delivery.ifr_route",
        "start" => "ground.startup_only",
        "pushback" => "ground.pushback_basic",
        "start_pushback" => "ground.start_and_pushback",
        _ => return,
    };
    let Some(entry) = state.speech.get(id) else {
        return;
    };
    let service = if intent == "clearance" {
        "Clearance"
    } else {
        "Ground"
    };
    let phase = format!("{:?}", state.session.phase).to_lowercase();
    if entry.role != "atc"
        || (!entry.service.is_empty() && !entry.service.iter().any(|v| v == service))
        || !entry.phase.contains(&phase)
    {
        return;
    }
    let Some(clearance) = state.session.clearance.as_ref() else {
        return;
    };
    let slots = openatc_core::catalog::Slots {
        callsign: Some(state.session.plan.callsign.clone()),
        dest: Some(
            state
                .stations
                .iter()
                .find(|s| s.airport == state.session.plan.destination && !s.airport_name.is_empty())
                .map_or_else(
                    || {
                        state
                            .map_airports
                            .iter()
                            .find(|a| a.id == state.session.plan.destination)
                            .map_or_else(
                                || state.session.plan.destination.clone(),
                                |a| a.name.clone(),
                            )
                    },
                    |s| s.airport_name.clone(),
                ),
        ),
        runway: Some(clearance.runway.clone()),
        altitude_feet: Some(clearance.altitude_feet),
        squawk: Some(clearance.squawk.clone()),
        via: Some(if clearance.as_filed {
            if clearance.departure_readback.is_empty() {
                openatc_core::dialogue::say("filed_routing", &[])
            } else {
                openatc_core::dialogue::say(
                    "departure_then_filed",
                    &[("sid", clearance.departure_readback.clone())],
                )
            }
        } else if state.session.plan.sid.is_empty()
            || clearance.route.split_whitespace().next() == Some(state.session.plan.sid.as_str())
        {
            clearance.route.clone()
        } else {
            format!("{} then {}", state.session.plan.sid, clearance.route)
        }),
        ..Default::default()
    };
    let next = state.phrase_history.choices.get(id).copied().unwrap_or(0);
    let previous = state.phrase_history.templates.get(id);
    let Some((pick, line)) = (0..entry.say.len())
        .map(|offset| {
            let pick = (next + offset) % entry.say.len();
            (pick, &entry.say[pick])
        })
        .find(|(_, line)| previous != Some(*line))
        .or_else(|| entry.say.first().map(|line| (0, line)))
    else {
        return;
    };
    match openatc_core::speech::render_checked(line, &slots) {
        Ok(text) => {
            state.phrase_history.choices.insert(id.to_owned(), pick + 1);
            state
                .phrase_history
                .templates
                .insert(id.to_owned(), line.clone());
            eprintln!(
                "ATC wording: template={id} alternative={} station={} text={text}",
                pick + 1,
                station.name
            );
            result.message.clone_from(&text);
            if let Some(last) = state
                .session
                .transcript
                .last_mut()
                .filter(|e| e.speaker == "ATC")
            {
                last.text = text;
                last.position.clone_from(&station.name);
            }
        }
        Err(error) => eprintln!("ATC wording template {id} could not be rendered: {error}"),
    }
}
/// Conservative paraphrase check: every non-filler token stays in the same order.
/// Numbers, instructions, negation, callsign and route cannot change or be added.
pub fn safe_variety(original: &str, candidate: &str) -> bool {
    fn tokens(text: &str) -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric() && c != '-')
            .filter(|t| !t.is_empty())
            .map(str::to_ascii_lowercase)
            .collect()
    }
    if candidate.trim().is_empty() || candidate.len() > original.len() + 80 {
        return false;
    }
    let original = tokens(original);
    let mut expected = original.iter().peekable();
    for token in tokens(candidate) {
        if expected.peek().is_some_and(|next| **next == token) {
            expected.next();
        } else if !["please", "the", "your", "is"].contains(&token.as_str()) {
            return false;
        }
    }
    expected.next().is_none()
}

/// The debug map reads the same controller catalogue used for handoffs.
pub async fn debug_map(AxumState(shared): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    let (root, indexed_root, latitude, longitude) = {
        let s = shared.read().await;
        if !s.settings.dev_mode {
            return error(openatc_core::dialogue::say(
                "notice_map_is_available_in_developer_mode_282def75",
                &[],
            ));
        }
        (
            s.settings.simulator_root.clone(),
            s.map_root.clone(),
            s.session.telemetry.latitude,
            s.session.telemetry.longitude,
        )
    };
    if root.is_empty() {
        return error(openatc_core::dialogue::say(
            "notice_set_the_x_plane_folder_to_load_the_map_c68b9a0c",
            &[],
        ));
    }
    if root != indexed_root {
        let path = root.clone();
        let workers = shared.read().await.workers.clone();
        let Ok(Ok(airports)) = workers
            .run(move || openatc_core::stations::map_airports(&path))
            .await
        else {
            return error(openatc_core::dialogue::say(
                "notice_could_not_index_airport_markers_bb8fa639",
                &[],
            ));
        };
        let mut s = shared.write().await;
        if s.settings.simulator_root != root {
            return error(openatc_core::dialogue::say(
                "notice_simulator_folder_changed_refresh_the_map_d952617e",
                &[],
            ));
        }
        s.map_airports = airports;
        s.map_root.clone_from(&root);
    }
    let s = shared.read().await;
    let lat = body
        .get("latitude")
        .and_then(Value::as_f64)
        .unwrap_or(latitude);
    let lon = body
        .get("longitude")
        .and_then(Value::as_f64)
        .unwrap_or(longitude);
    let range = body
        .get("rangeNm")
        .and_then(Value::as_f64)
        .unwrap_or(80.)
        .clamp(2., 1500.);
    if !lat.is_finite() || !lon.is_finite() || lat.abs() > 90. || lon.abs() > 180. {
        return error(openatc_core::dialogue::say(
            "notice_invalid_map_position_5a458578",
            &[],
        ));
    }
    let mut t = s.session.telemetry.clone();
    t.latitude = lat;
    t.longitude = lon;
    let distance = |a, b| openatc_core::ops::distance_nm(lat, lon, a, b);
    let stations: Vec<_> = s
        .stations
        .iter()
        .filter(|station| {
            distance(station.latitude, station.longitude) <= range * 1.5
                || station
                    .coverage
                    .iter()
                    .any(|volume| volume.radio_distance_nm(&t) <= range * 1.5)
        })
        .collect();
    let airports: Vec<_> = s
        .map_airports
        .iter()
        .filter(|a| distance(a.latitude, a.longitude) <= range * 1.5)
        .collect();
    Json(json!({"stations":stations,"airports":airports,
        "notice":if s.station_root == root { "Installed airport data and published ATC coverage. Green outlines contain the aircraft on the tuned frequency." } else { "Waiting for the radio station index." }})).into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn weather_signature_ignores_cloud_wording_variety() {
        let mut observation = serde_json::json!({"windDegrees":333,"windKnots":9,"pressureHpa":1026,"visibilityMeters":31157,"temperatureC":18,"dewpointC":11,"cloudSignature":[[2,24],[2,112]],"clouds":"Few clouds at 2400 feet"});
        let original = super::weather_signature(&observation);
        observation["clouds"] = serde_json::json!("Few clouds with a base of 2400 feet");
        observation["windDegrees"] = serde_json::json!(334);
        observation["visibilityMeters"] = serde_json::json!(31175);
        assert_eq!(super::weather_signature(&observation), original);
        observation["cloudSignature"] = serde_json::json!([[8, 24]]);
        assert_ne!(super::weather_signature(&observation), original);
    }

    use super::safe_variety;
    #[test]
    fn controller_identity_belongs_to_the_station_not_the_receiver() {
        let mut state = crate::test_state();
        let mut station = openatc_core::stations::Station {
            airport: "TEST".into(),
            name: "Test Tower".into(),
            service: "Tower".into(),
            khz: 118_700,
            ..Default::default()
        };
        let settings = state.settings.clone();
        let key = crate::flight::station_key(&station);
        state.session.telemetry.com1_khz = station.khz;
        let first =
            crate::routes::assign_airspace_controller(&mut state, &settings, &key, "standard");
        state.session.telemetry.com2_khz = Some(station.khz);
        let second =
            crate::routes::assign_airspace_controller(&mut state, &settings, &key, "standard");
        assert_eq!(first, second);
        assert_eq!(state.controllers.assignments.len(), 1);
        station.airport = "OTHER".into();
        assert_ne!(key, crate::flight::station_key(&station));
    }
    #[tokio::test]
    async fn atis_uses_the_selected_radio_and_rejects_stale_weather() {
        use axum::{Json, extract::State};
        use serde_json::json;
        let mut state = crate::test_state();
        state
            .station_root
            .clone_from(&state.settings.simulator_root);
        state.session.telemetry.position_valid = true;
        state.session.telemetry.com1_khz = 121_400;
        state.session.telemetry.com2_khz = Some(118_700);
        state.stations.push(openatc_core::stations::Station {
            airport: "TEST".into(),
            airport_name: "Test Airport".into(),
            name: "Test Information".into(),
            service: "ATIS".into(),
            khz: 118_700,
            ..Default::default()
        });
        let sample = json!({"windDegrees":240,"windKnots":8,"visibilityMeters":10000,
            "temperatureC":20,"dewpointC":12,"pressureHpa":1015,"clouds":"Clear"});
        state.airport = Some(openatc_core::airport::Airport {
            icao: "TEST".into(),
            runways: vec![openatc_core::airport::Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                first: Default::default(),
                second: openatc_core::state::Point {
                    east: 2000.0,
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        });
        state.session.plan.departure = "TEST".into();
        state.session.plan.runway = "09".into();
        state
            .atis_information
            .insert("TEST".into(), ("weather".into(), 0));
        state
            .sim_weather
            .insert("TEST".into(), (sample, std::time::Instant::now()));
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        let response = super::atis(State(shared.clone()), Json(json!({"radio":"com2"}))).await;
        assert!(response.status().is_success());
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(result["radio"], "com2");
        assert_eq!(result["frequency"], 118_700);
        assert!(
            result["text"]
                .as_str()
                .unwrap()
                .contains("Runway 27 in use")
        );
        assert!(
            !result["text"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("simulat")
        );
        {
            let state = shared.read().await;
            let entry = state.session.transcript.last().unwrap();
            assert_eq!(entry.speaker, "ATIS");
            assert_eq!(entry.text, result["text"].as_str().unwrap());
            assert_eq!(entry.radio, Some(openatc_core::state::Radio::Com2));
            assert!(!entry.is_controller_instruction());
        }
        let count = shared.read().await.session.transcript.len();
        let repeated = super::atis(State(shared.clone()), Json(json!({"radio":"com2"}))).await;
        assert!(repeated.status().is_success());
        assert_eq!(shared.read().await.session.transcript.len(), count);
        shared.write().await.sim_weather.get_mut("TEST").unwrap().0["windDegrees"] = json!(90);
        let response = super::atis(State(shared.clone()), Json(json!({"radio":"com2"}))).await;
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let revised: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            revised["text"]
                .as_str()
                .unwrap()
                .contains("Runway 09 in use")
        );
        assert_ne!(revised["information"], result["information"]);
        let state = shared.read().await;
        assert_eq!(state.session.transcript.len(), count + 1);
        assert_eq!(
            state.session.transcript[count - 1].text,
            result["text"].as_str().unwrap()
        );
        assert_eq!(
            state.session.transcript.last().unwrap().text,
            revised["text"].as_str().unwrap()
        );
        drop(state);
        {
            let mut state = shared.write().await;
            state
                .atis_logged
                .insert("Com1/OTHER".into(), "unchanged".into());
        }
        for frequency in [121_700, 118_700] {
            let mut telemetry = shared.read().await.session.telemetry.clone();
            telemetry.com2_khz = Some(frequency);
            telemetry.radio_busy = true;
            assert!(
                crate::surface::post_telemetry(State(shared.clone()), Json(telemetry))
                    .await
                    .status()
                    .is_success()
            );
        }
        assert!(shared.read().await.atis_logged.contains_key("Com1/OTHER"));
        let atis_count = |state: &crate::EngineState| {
            state
                .session
                .transcript
                .iter()
                .filter(|entry| entry.speaker == "ATIS")
                .count()
        };
        let before_retune = atis_count(&*shared.read().await);
        assert!(
            super::atis(State(shared.clone()), Json(json!({"radio":"com2"})))
                .await
                .status()
                .is_success()
        );
        assert_eq!(atis_count(&*shared.read().await), before_retune + 1);
        assert!(
            super::atis(State(shared.clone()), Json(json!({"radio":"com2"})))
                .await
                .status()
                .is_success()
        );
        assert_eq!(atis_count(&*shared.read().await), before_retune + 1);
        for powered in [false, true] {
            let mut telemetry = shared.read().await.session.telemetry.clone();
            telemetry.radio_power = powered;
            assert!(
                crate::surface::post_telemetry(State(shared.clone()), Json(telemetry))
                    .await
                    .status()
                    .is_success()
            );
        }
        assert!(
            super::atis(State(shared.clone()), Json(json!({"radio":"com2"})))
                .await
                .status()
                .is_success()
        );
        assert_eq!(atis_count(&*shared.read().await), before_retune + 2);
        assert_eq!(result["airport"], "TEST");
        assert!(result["text"].as_str().is_some_and(|text| !text.is_empty()));
        assert!(
            !super::atis(State(shared.clone()), Json(json!({})))
                .await
                .status()
                .is_success()
        );
        assert!(
            !super::atis(State(shared.clone()), Json(json!({"radio":"com3"})))
                .await
                .status()
                .is_success()
        );
        {
            let mut state = shared.write().await;
            state.sim_weather.get_mut("TEST").unwrap().1 = std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(46))
                .unwrap();
        }
        assert!(
            !super::atis(State(shared.clone()), Json(json!({"radio":"com2"})))
                .await
                .status()
                .is_success()
        );
        shared.write().await.session.telemetry.com2_khz = None;
        assert!(
            !super::atis(State(shared.clone()), Json(json!({"radio":"com2"})))
                .await
                .status()
                .is_success()
        );
        assert_eq!(shared.read().await.session.telemetry.com1_khz, 121_400);
    }

    #[test]
    fn phrase_history_survives_reload() {
        let dir = std::env::temp_dir().join(format!(
            "openatc-phrase-history-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut history = super::PhraseHistory::default();
        history.choices.insert("delivery.ifr_route".into(), 3);
        history.last.insert(
            "station|clearance".into(),
            "Recorded clearance wording".into(),
        );
        history.save(&dir);
        let restored = super::PhraseHistory::load(&dir);
        assert_eq!(restored.choices["delivery.ifr_route"], 3);
        assert_eq!(
            restored.last["station|clearance"],
            "Recorded clearance wording"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn wording_must_preserve_all_operational_tokens() {
        assert!(!safe_variety("THE, taxi via YOUR.", "Taxi via."));
        let text = "VH-BIL, taxi via Alpha. Hold short runway 27.";
        assert!(safe_variety(
            text,
            "VH-BIL, please taxi via Alpha. Hold short runway 27."
        ));
        for changed in [
            "VH-BIL, taxi via Alpha. Hold short runway 09.",
            "VH-BIL, taxi via Bravo. Hold short runway 27.",
            "VH-BIL, taxi via Alpha. Cross runway 27.",
            "VH-BIL, taxi via Alpha. Do not hold short runway 27.",
        ] {
            assert!(!safe_variety(text, changed));
        }
    }
}
