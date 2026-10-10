//! Published-station handoffs and explicit arrival permissions.
use crate::EngineState;
use openatc_core::{
    apply::add_transmission,
    dialogue::say,
    ops::distance_nm,
    state::{PhaseCode, Request, SpeechTag},
    stations::{Station, nearby, tuned},
};
use serde_json::{Value, json};

#[derive(Default)]
pub struct FlightFlow {
    candidate: String,
    evidence: u8,
    pending_station: String,
    approach_cleared: bool,
    landing_cleared: bool,
    vacate_reported: bool,
    warning: openatc_core::compliance::WarningCount,
    clearance_sequence: u32,
    last_correction: Option<std::time::Instant>,
}

pub fn station_key(station: &Station) -> String {
    format!("{}:{}:{}", station.service, station.name, station.khz)
}

fn tag(state: &mut EngineState, station: &Station) -> SpeechTag {
    let settings = state.settings.clone();
    let mut tag = crate::routes::assign_airspace_controller(
        state,
        &settings,
        &station_key(station),
        "standard",
    );
    tag.position.clone_from(&station.name);
    tag
}

fn airport_distance(state: &EngineState, id: &str) -> Option<f64> {
    let station = state.stations.iter().find(|s| s.airport == id)?;
    Some(distance_nm(
        state.session.telemetry.latitude,
        state.session.telemetry.longitude,
        station.latitude,
        station.longitude,
    ))
}

fn next_station(state: &EngineState, stations: &[Station], current: &Station) -> Option<Station> {
    let t = &state.session.telemetry;
    let arrival_distance =
        airport_distance(state, &state.session.plan.destination).unwrap_or(f64::INFINITY);
    let arrival = matches!(
        state.session.phase,
        PhaseCode::Arrival | PhaseCode::Approach | PhaseCode::Landed | PhaseCode::TaxiIn
    ) || (state.session.has_departed && arrival_distance < 45.0);
    let choose = |roles: &[&str], airport: Option<&str>| {
        roles.iter().find_map(|role| {
            stations
                .iter()
                .filter(|s| {
                    s.receivable && s.service == *role && airport.is_none_or(|id| s.airport == id)
                })
                .filter(|s| !matches!(*role, "Center") || s.coverage.iter().any(|c| c.contains(t)))
                .min_by(|a, b| a.distance_nm.total_cmp(&b.distance_nm))
                .cloned()
        })
    };
    if t.on_ground {
        if matches!(state.session.phase, PhaseCode::Landed | PhaseCode::TaxiIn) {
            let airport = state
                .airport
                .as_ref()
                .filter(|a| a.icao == state.session.plan.destination)?;
            let p = openatc_core::airport::airport_point(airport, t.latitude, t.longitude);
            if openatc_core::airport::within_runway(airport, &p) {
                return None;
            }
            return choose(&["Ground", "Tower"], Some(&state.session.plan.destination));
        }
        return None;
    }
    if arrival {
        let ready_tower = state.flow.approach_cleared && arrival_distance < 12.0;
        return if ready_tower {
            choose(&["Tower"], Some(&state.session.plan.destination))
        } else {
            choose(
                &["Approach", "Tower"],
                Some(&state.session.plan.destination),
            )
        };
    }
    if state.session.phase == PhaseCode::Departure && t.height_agl_feet < 500.0 {
        return None;
    }
    if state.session.phase == PhaseCode::Departure
        && airport_distance(state, &state.session.plan.departure).is_some_and(|d| d < 30.0)
    {
        return choose(
            &["Departure", "Approach", "Center"],
            Some(&state.session.plan.departure),
        )
        .or_else(|| choose(&["Center"], None));
    }
    if current.service == "Center" && current.coverage.iter().any(|c| c.contains(t)) {
        return Some(current.clone());
    }
    choose(&["Center"], None)
}

/// Process handoffs only on a powered, receiving radio with unpaused valid telemetry.
pub fn tick(state: &mut EngineState) {
    let t = &state.session.telemetry;
    if state.session.demo
        || t.paused
        || !t.position_valid
        || !t.radio_power
        || !state.session.has_departed
    {
        return;
    }
    let stations = nearby(&state.stations, t);
    let Some(current) = tuned(&stations, t.com1_khz)
        .cloned()
        .filter(|s| !["ATIS", "Unicom"].contains(&s.service.as_str()))
    else {
        return;
    };
    compliance(state, &current);
    if state.session.clearance_cancelled {
        return;
    }
    if state.session.phase == PhaseCode::Landed && !state.flow.vacate_reported {
        state.flow.vacate_reported = true;
        let text = say(
            "arrival_vacate",
            &[
                ("callsign", state.session.plan.callsign.clone()),
                ("runway", state.session.plan.arrival_runway.clone()),
            ],
        );
        let voice = tag(state, &current);
        add_transmission(&mut state.session, "ATC", &text, &voice);
    }
    let Some(next) = next_station(state, &stations, &current) else {
        return;
    };
    let key = station_key(&next);
    if key == station_key(&current) || key == state.flow.pending_station {
        state.flow.evidence = 0;
        return;
    }
    if state.flow.candidate != key {
        state.flow.candidate.clone_from(&key);
        state.flow.evidence = 0;
    }
    state.flow.evidence = state.flow.evidence.saturating_add(1);
    if state.flow.evidence < 4 {
        return;
    }
    state.flow.pending_station = key;
    state.session.recommended_frequency_khz = next.khz;
    state.session.frequency_sequence = state.session.next_sequence;
    let text = say(
        "flight_handoff",
        &[
            ("callsign", state.session.plan.callsign.clone()),
            ("station", next.name.clone()),
            ("frequency", format!("{:.3}", f64::from(next.khz) / 1000.0)),
        ],
    );
    let tag = tag(state, &current);
    add_transmission(&mut state.session, "ATC", &text, &tag);
    if let Some(entry) = state.session.transcript.last_mut() {
        entry.pilot_reply = say(
            "pilot_frequency_readback",
            &[
                ("station", next.name.clone()),
                ("frequency", format!("{:.3}", f64::from(next.khz) / 1000.0)),
                ("callsign", state.session.plan.callsign.clone()),
            ],
        );
    }
}

fn respond(
    state: &mut EngineState,
    request: &Request,
    tag: &SpeechTag,
    accepted: bool,
    text: String,
) -> Value {
    let callsign = state.session.plan.callsign.clone();
    add_transmission(
        &mut state.session,
        &callsign,
        &request.text,
        &SpeechTag::default(),
    );
    add_transmission(&mut state.session, "ATC", &text, tag);
    json!({"accepted":accepted,"message":text})
}

/// Handle check-in, frequency requests and approach/landing permissions on their actual stations.
pub fn request(
    state: &mut EngineState,
    station: &Station,
    request: &Request,
    tag: &SpeechTag,
) -> Option<Value> {
    if !["checkin", "frequency", "approach", "landing"].contains(&request.intent.as_str()) {
        return None;
    }
    let callsign = state.session.plan.callsign.clone();
    if request.intent == "frequency" {
        let stations = nearby(&state.stations, &state.session.telemetry);
        let next = next_station(state, &stations, station);
        let (accepted, text) =
            if let Some(next) = next.filter(|s| station_key(s) != station_key(station)) {
                state.session.recommended_frequency_khz = next.khz;
                state.session.frequency_sequence = state.session.next_sequence;
                state.flow.pending_station = station_key(&next);
                (
                    true,
                    say(
                        "flight_handoff",
                        &[
                            ("callsign", callsign),
                            ("station", next.name),
                            ("frequency", format!("{:.3}", f64::from(next.khz) / 1000.0)),
                        ],
                    ),
                )
            } else {
                (
                    true,
                    say(
                        "flight_remain_frequency",
                        &[("callsign", callsign), ("station", station.name.clone())],
                    ),
                )
            };
        return Some(respond(state, request, tag, accepted, text));
    }
    if request.intent == "checkin" {
        state.flow.pending_station.clear();
        state.flow.evidence = 0;
        state.session.recommended_frequency_khz = station.khz;
        state.session.frequency_sequence = 0;
        let altitude = openatc_core::altitude_text(
            state.session.telemetry.altitude_feet,
            openatc_core::UnitSystem::Imperial,
            true,
        );
        let text = say(
            "flight_checkin",
            &[
                ("callsign", callsign),
                ("station", station.name.clone()),
                ("altitude", altitude),
            ],
        );
        return Some(respond(state, request, tag, true, text));
    }
    let airport = state
        .airport
        .as_ref()
        .filter(|a| a.icao == state.session.plan.destination)
        .cloned();
    let Some(airport) = airport else {
        return Some(respond(
            state,
            request,
            tag,
            false,
            say("arrival_airport_unavailable", &[]),
        ));
    };
    let runway = state.session.plan.arrival_runway.clone();
    if runway.is_empty()
        || !airport
            .runways
            .iter()
            .any(|r| r.first_name == runway || r.second_name == runway)
    {
        return Some(respond(
            state,
            request,
            tag,
            false,
            say("arrival_runway_required", &[]),
        ));
    }
    if state.session.telemetry.on_ground {
        return Some(respond(
            state,
            request,
            tag,
            false,
            say("arrival_airborne_required", &[]),
        ));
    }
    if request.intent == "approach" {
        if !["Approach", "Tower"].contains(&station.service.as_str()) {
            return Some(respond(
                state,
                request,
                tag,
                false,
                say("arrival_contact_approach", &[]),
            ));
        }
        // Procedure names alone do not establish leg constraints or vectoring authority.
        state.session.phase = PhaseCode::Approach;
        state.flow.approach_cleared = true;
        let text = say(
            "arrival_expect_runway",
            &[
                ("callsign", callsign),
                ("runway", runway),
                ("airport", airport.name),
            ],
        );
        return Some(respond(state, request, tag, true, text));
    }
    if station.service != "Tower" {
        return Some(respond(
            state,
            request,
            tag,
            false,
            say("arrival_contact_tower", &[]),
        ));
    }
    if airport_distance(state, &airport.icao).is_none_or(|d| d > 12.0) {
        return Some(respond(
            state,
            request,
            tag,
            false,
            say(
                "arrival_report_final",
                &[("callsign", callsign), ("runway", runway)],
            ),
        ));
    }
    if let Some(blocked) =
        openatc_core::holding::blocked(&airport, &runway, &state.session.telemetry)
    {
        return Some(respond(
            state,
            request,
            tag,
            false,
            say(blocked, &[("runway", runway)]),
        ));
    }
    state.flow.landing_cleared = true;
    let text = say(
        "arrival_landing_clearance",
        &[
            ("callsign", callsign),
            ("runway", runway),
            ("weather", weather(state, station)),
        ],
    );
    Some(respond(state, request, tag, true, text))
}

fn weather(state: &EngineState, station: &Station) -> String {
    let Some((w, _)) = state
        .sim_weather
        .get(&station.airport)
        .filter(|(_, t)| t.elapsed().as_secs() < 45)
    else {
        return String::new();
    };
    let pressure = if station.airport.starts_with('K') {
        say(
            "flight_altimeter",
            &[(
                "pressure",
                format!(
                    "{:.2}",
                    w["pressureHpa"].as_f64().unwrap_or(0.0) / 33.863_886_666_7
                ),
            )],
        )
    } else {
        say(
            "flight_qnh",
            &[(
                "pressure",
                format!("{:.0}", w["pressureHpa"].as_f64().unwrap_or(0.0)),
            )],
        )
    };
    say(
        "flight_surface_weather",
        &[
            (
                "direction",
                format!("{:03.0}", w["windDegrees"].as_f64().unwrap_or(0.0)),
            ),
            (
                "speed",
                format!("{:.0}", w["windKnots"].as_f64().unwrap_or(0.0)),
            ),
            ("pressure", pressure),
        ],
    )
}

pub fn takeoff_wording(
    state: &mut EngineState,
    station: &Station,
    result: &mut openatc_core::apply::RequestOutcome,
) {
    let plan = &state.session.plan;
    let sid = state.airport.as_ref().is_some_and(|a| {
        a.procedures
            .iter()
            .any(|p| p.kind.eq_ignore_ascii_case("SID") && p.name == plan.sid)
    });
    let altitude = state
        .session
        .clearance
        .as_ref()
        .map_or(plan.initial_altitude_feet, |c| c.altitude_feet);
    let climb = if sid {
        say(
            "flight_sid_climb",
            &[
                ("sid", plan.sid.clone()),
                (
                    "altitude",
                    openatc_core::altitude_text(
                        f64::from(altitude),
                        openatc_core::UnitSystem::Imperial,
                        true,
                    ),
                ),
            ],
        )
    } else {
        say(
            "flight_initial_climb",
            &[(
                "altitude",
                openatc_core::altitude_text(
                    f64::from(altitude),
                    openatc_core::UnitSystem::Imperial,
                    true,
                ),
            )],
        )
    };
    let expected = if plan.cruise_feet > altitude {
        say(
            "flight_expect_level",
            &[(
                "altitude",
                openatc_core::altitude_text(
                    f64::from(plan.cruise_feet),
                    openatc_core::UnitSystem::Imperial,
                    true,
                ),
            )],
        )
    } else {
        String::new()
    };
    result.message = say(
        "flight_takeoff",
        &[
            ("callsign", plan.callsign.clone()),
            ("runway", plan.runway.clone()),
            ("weather", weather(state, station)),
            ("climb", climb),
            ("expected", expected),
        ],
    );
    if let Some(entry) = state
        .session
        .transcript
        .iter_mut()
        .rev()
        .find(|e| e.speaker == "ATC")
    {
        entry.text.clone_from(&result.message);
        entry.pilot_reply = say(
            "pilot_takeoff_readback",
            &[
                ("runway", state.session.plan.runway.clone()),
                ("callsign", state.session.plan.callsign.clone()),
            ],
        );
    }
}

fn compliance(state: &mut EngineState, station: &Station) {
    if !state.settings.enforce_clearance_constraints {
        state.flow.warning.resolved();
        state.flow.last_correction = None;
        state.flow.clearance_sequence = 0;
        return;
    }
    if state.session.clearance_cancelled || state.session.telemetry.on_ground {
        return;
    }
    let Some(clearance) = state.session.clearance.as_ref().filter(|c| c.acknowledged) else {
        return;
    };
    let sequence = clearance.sequence;
    let target = f64::from(clearance.altitude_feet);
    let t = &state.session.telemetry;
    let error = t.altitude_feet - target;
    // Climbing or descending toward an assigned level is compliance, not a level bust.
    let converging = (error < -300.0 && t.vertical_speed_fpm > 200.0)
        || (error > 300.0 && t.vertical_speed_fpm < -200.0);
    if error.abs() <= 300.0 || converging {
        state.flow.warning.resolved();
        state.flow.last_correction = None;
        return;
    }
    let now = std::time::Instant::now();
    if state.flow.clearance_sequence != sequence {
        state.flow.clearance_sequence = sequence;
        state.flow.warning.resolved();
        state.flow.last_correction = Some(now);
        return;
    }
    let Some(last) = state.flow.last_correction else {
        state.flow.last_correction = Some(now);
        return;
    };
    if now.duration_since(last).as_secs() < 30 {
        return;
    }
    state.flow.last_correction = Some(now);
    let cancelled = state.flow.warning.continuing();
    let altitude = openatc_core::altitude_text(target, openatc_core::UnitSystem::Imperial, true);
    let text = if cancelled {
        state.session.clearance_cancelled = true;
        state.session.taxi_clearance.approved = false;
        state.flow.landing_cleared = false;
        state.flow.approach_cleared = false;
        say(
            "compliance_cancelled",
            &[("callsign", state.session.plan.callsign.clone())],
        )
    } else {
        say(
            "compliance_altitude_correction",
            &[
                ("callsign", state.session.plan.callsign.clone()),
                ("altitude", altitude),
            ],
        )
    };
    let voice = tag(state, station);
    add_transmission(&mut state.session, "ATC", &text, &voice);
}
