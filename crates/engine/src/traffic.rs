//! Local traffic observations, surface conflict holds and optional simulated pilot reports.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub mod advisories;
mod conversation;
#[cfg(test)]
mod conversation_tests;
pub(crate) mod holding;
pub mod manager;
pub mod phase;
pub mod phases;
mod policy;
pub mod separation;
mod wake;
use crate::{EngineState, Shared};
use axum::{
    Json,
    extract::State as AxumState,
    response::{IntoResponse, Response},
};
use openatc_core::{
    apply::add_transmission,
    dialogue::say,
    state::{PhaseCode, SpeechTag, TrafficTarget},
    stations::{Station, nearby, tuned},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Landing {
    vacated: bool,
    airport: String,
    runway: String,
    weather: Option<Value>,
    heading: f64,
    at: Instant,
}
struct Pilot {
    target: TrafficTarget,
    seen: Instant,
    landing: Option<Landing>,
    voice: String,
    climb_fpm: Option<f64>,
    conversation: std::collections::VecDeque<(String, String)>,
}
#[derive(Default)]
pub struct Traffic {
    wake: wake::Monitor,
    advisories: BTreeMap<u32, advisories::Call>,
    last_advisory: Option<Instant>,
    last_urgent: Option<Instant>,
    pilots: BTreeMap<u32, Pilot>,
    observed: Option<Instant>,
    report: Option<(u32, String)>,
    last_radio: Option<Instant>,
    clear_since: Option<Instant>,
    hold_target: Option<u32>,
    speed: Option<(f64, Option<f64>)>,
    speed_declined: bool,
    speed_sequence: u32,
    pub(crate) spacing_path_required: bool,
    hold: Option<holding::Hold>,
    hold_declined: bool,
    chat_pending: bool,
    last_chat: Option<Instant>,
    last_chat_pilot: u32,
    chat_attempts: std::collections::BTreeSet<(u32, String, String, u8)>,
    policy: Option<policy::Chat>,
}

pub(crate) fn departure_wake_delay(
    state: &EngineState,
    id: u32,
    airport: &openatc_core::airport::Airport,
    runway: &str,
    aircraft: &str,
    latitude: f64,
    longitude: f64,
) -> f64 {
    state
        .traffic
        .wake
        .departure_delay(id, airport, runway, aircraft, latitude, longitude)
}

pub fn observe(state: &mut EngineState) {
    holding::observe(state);
    wake::observe(state);
    crate::generated::refresh_departure_wake(state);
    if state.session.telemetry.paused
        || state
            .traffic
            .observed
            .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
    {
        return;
    }
    state.traffic.observed = Some(Instant::now());
    crate::atc::sync_traffic(state);
    state
        .traffic
        .pilots
        .retain(|_, p| p.seen.elapsed() < Duration::from_secs(120));
    let Some(targets) = &state.session.telemetry.traffic else {
        state.generated.observed_climbs(BTreeMap::new());
        return;
    };
    for target in targets
        .iter()
        .take(126)
        .filter(|t| t.id != 0 && !t.callsign.is_empty())
    {
        if !openatc_core::traffic::advisory::valid(target) {
            continue;
        }
        let climb_fpm = state.traffic.pilots.get(&target.id).and_then(|old| {
            let elapsed = old.seen.elapsed().as_secs_f64();
            if old.target.callsign != target.callsign || !(0.5..5.0).contains(&elapsed) {
                return None;
            }
            let speed = (target.altitude_feet - old.target.altitude_feet) * 60.0 / elapsed;
            (speed.abs() <= 15000.0).then_some(speed)
        });
        let landed = state.traffic.pilots.get(&target.id).is_some_and(|p| {
            p.target.callsign == target.callsign
                && !p.target.on_ground
                && target.on_ground
                && p.seen.elapsed() < Duration::from_secs(5)
        });
        let landing = if landed {
            state
                .arrival_airport
                .iter()
                .chain(state.airport.iter())
                .find_map(|airport| {
                    airport.runways.iter().find_map(|runway| {
                        let p = openatc_core::traffic::runway_position(
                            airport,
                            &runway.first_name,
                            target.latitude,
                            target.longitude,
                        )?;
                        if !(0.0..=p.2).contains(&p.0) || p.1.abs() > runway.width / 2.0 + 10.0 {
                            return None;
                        }
                        let first =
                            openatc_core::navigation::turn(target.track_degrees, p.3).abs() < 90.0;
                        Some(Landing {
                            vacated: false,
                            airport: airport.icao.clone(),
                            runway: if first {
                                runway.first_name.clone()
                            } else {
                                runway.second_name.clone()
                            },
                            heading: if first { p.3 } else { (p.3 + 180.0) % 360.0 },
                            weather: state
                                .sim_weather
                                .get(&airport.icao)
                                .filter(|(_, at)| at.elapsed().as_secs() < 45)
                                .map(|(value, _)| value.clone()),
                            at: Instant::now(),
                        })
                    })
                })
        } else {
            None
        };
        let voice = state
            .traffic
            .pilots
            .get(&target.id)
            .filter(|p| p.target.callsign == target.callsign)
            .map_or_else(
                || {
                    let available: Vec<_> = state
                        .voices
                        .iter()
                        .filter(|v| !state.settings.english_voices_only || v.english)
                        .collect();
                    available
                        .iter()
                        .min_by_key(|v| {
                            let reserved = state
                                .controllers
                                .assignments
                                .values()
                                .any(|c| c.voice == v.id)
                                || [
                                    &state.settings.pilot_voice,
                                    &state.settings.copilot_voice,
                                    &state.settings.attendant_voice,
                                    &state.settings.ground_voice,
                                ]
                                .contains(&&v.id);
                            (
                                reserved,
                                state
                                    .traffic
                                    .pilots
                                    .values()
                                    .filter(|p| p.voice == v.id)
                                    .count(),
                            )
                        })
                        .map_or_else(|| state.settings.pilot_voice.clone(), |v| v.id.clone())
                },
                |p| p.voice.clone(),
            );
        let old = state
            .traffic
            .pilots
            .remove(&target.id)
            .filter(|p| p.target.callsign == target.callsign);
        let conversation = old
            .as_ref()
            .map(|p| p.conversation.clone())
            .unwrap_or_default();
        let mut landing = landing.or_else(|| old.and_then(|p| p.landing));
        if let Some(l) = &mut landing
            && !l.vacated
            && target.on_ground
            && l.at.elapsed().as_secs() < 300
            && let Some(airport) = state
                .arrival_airport
                .iter()
                .chain(state.airport.iter())
                .find(|a| a.icao == l.airport)
        {
            let observed = openatc_core::state::Telemetry {
                aircraft_icao: target.aircraft.clone(),
                latitude: target.latitude,
                longitude: target.longitude,
                heading_degrees: target.track_degrees,
                wingspan_meters: target.wingspan_meters,
                ..Default::default()
            };
            if !openatc_core::airport::aircraft_on_runway(airport, &observed) {
                l.vacated = true;
                state.traffic.report = Some((target.id, l.airport.clone()));
            }
        }
        if state.traffic.pilots.len() < 126 {
            state.traffic.pilots.insert(
                target.id,
                Pilot {
                    climb_fpm,
                    conversation,
                    target: target.clone(),
                    seen: Instant::now(),
                    landing,
                    voice,
                },
            );
        }
    }
    state.generated.observed_climbs(
        state
            .traffic
            .pilots
            .iter()
            .filter_map(|(&id, pilot)| {
                pilot
                    .climb_fpm
                    .filter(|_| pilot.seen.elapsed() < Duration::from_secs(5))
                    .map(|rate| (id, rate))
            })
            .collect(),
    );
}

pub(crate) fn pilot_voice(state: &EngineState, id: u32) -> String {
    state.traffic.pilots.get(&id).map_or_else(
        || state.settings.pilot_voice.clone(),
        |pilot| pilot.voice.clone(),
    )
}

pub(crate) fn ready(state: &EngineState) -> bool {
    !state.session.telemetry.paused
        && state.session.telemetry.radio_power
        && !state.session.telemetry.radio_busy
        && !state
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
        && state
            .session
            .transcript
            .iter()
            .rev()
            .find(|t| t.is_controller_instruction())
            .is_none_or(|t| t.pilot_reply.is_empty())
}
fn atc(
    state: &mut EngineState,
    station: &Station,
    key: &str,
    slots: &[(&str, String)],
    pilot_key: &str,
) {
    let voice = crate::flight::tag(state, station);
    add_transmission(&mut state.session, "ATC", &say(key, slots), &voice);
    if !pilot_key.is_empty()
        && let Some(last) = state.session.transcript.last_mut()
    {
        last.pilot_reply = say(pilot_key, slots);
    }
}

/// Called after higher-priority flight instructions, never from the render callback.
pub fn tick(state: &mut EngineState) {
    if !ready(state) || !state.session.telemetry.position_valid {
        return;
    }
    let stations = nearby(&state.stations, &state.session.telemetry);
    let Some(station) = state
        .session
        .control_frequency()
        .and_then(|frequency| tuned(&stations, frequency))
        .cloned()
    else {
        return;
    };
    if advisories::tick(state, &station)
        || surface(state, &station)
        || holding::active(state)
        || spacing(state, &station)
    {
        return;
    }
    if conversation::initiate(state) {
        return;
    }
    if !state.settings.hear_traffic_radio
        || state
            .traffic
            .last_radio
            .is_some_and(|at| at.elapsed().as_secs() < 45)
    {
        return;
    }
    let Some((id, airport)) = state.traffic.report.take() else {
        return;
    };
    if station.airport != airport || !["Tower", "Ground"].contains(&station.service.as_str()) {
        return;
    }
    let Some(pilot) = state
        .traffic
        .pilots
        .get(&id)
        .filter(|p| p.seen.elapsed().as_secs() < 5)
    else {
        return;
    };
    let Some(landing) = pilot
        .landing
        .as_ref()
        .filter(|l| l.vacated && l.at.elapsed().as_secs() < 300)
    else {
        return;
    };
    let text = say(
        "traffic_vacated_report",
        &[
            ("traffic", pilot.target.callsign.clone()),
            ("runway", landing.runway.clone()),
        ],
    );
    let callsign = pilot.target.callsign.clone();
    let voice = pilot.voice.clone();
    transmit(state, &callsign, &voice, &text);
    state.traffic.last_radio = Some(Instant::now());
}

fn surface(state: &mut EngineState, station: &Station) -> bool {
    let route = &state.session.taxi_clearance;
    if !matches!(state.session.phase, PhaseCode::Taxi | PhaseCode::TaxiIn)
        || !route.approved
        || route.pending_readback
        || !route.crossing_runway.is_empty()
        || !["Ground", "Tower"].contains(&station.service.as_str())
        || station.airport != route.airport
    {
        return false;
    }
    let Some(airport) = &state.airport else {
        return false;
    };
    if !route.traffic_hold
        && !route.hold_short_runway.is_empty()
        && openatc_core::airport::taxi_hold_reached(airport, route, &state.session.telemetry)
    {
        return false;
    }
    let conflict =
        openatc_core::traffic::taxi_conflict(airport, &state.session.telemetry).filter(|target| {
            !openatc_core::traffic::ahead_on_same_taxi_route(
                airport,
                route,
                &state.session.telemetry,
                target,
            )
        });
    if let Some(target) = conflict {
        state.traffic.clear_since = None;
        if route.traffic_hold {
            return true;
        }
        if state.traffic.hold_target == Some(target.id) {
            // Already holding for this traffic; do not repeat the instruction.
            state.session.taxi_clearance.traffic_hold = true;
            return true;
        }
        let traffic = if target.callsign.is_empty() {
            say("traffic_unidentified", &[])
        } else {
            target.callsign.clone()
        };
        state.session.taxi_clearance.traffic_hold = true;
        state.traffic.hold_target = Some(target.id);
        atc(
            state,
            station,
            "traffic_taxi_hold",
            &[
                ("callsign", state.session.plan.callsign.clone()),
                ("traffic", traffic),
            ],
            "pilot_traffic_hold",
        );
        return true;
    }
    if route.traffic_hold {
        if state.session.telemetry.traffic.is_none() {
            return true;
        }
        let since = state.traffic.clear_since.get_or_insert_with(Instant::now);
        if since.elapsed().as_secs() < 3 {
            return true;
        }
        let instructions = route.instructions.clone();
        state.session.taxi_clearance.traffic_hold = false;
        state.traffic.clear_since = None;
        state.traffic.hold_target = None;
        atc(
            state,
            station,
            "traffic_taxi_resume",
            &[
                ("callsign", state.session.plan.callsign.clone()),
                ("instructions", instructions),
            ],
            "pilot_traffic_resume",
        );
        return true;
    }
    false
}

/// Approach clearances here do not restate a prior assigned speed.
pub(crate) fn clear_approach_speed(state: &mut EngineState) -> Option<(String, String)> {
    let assigned = state.flow.monitor.speed.take().is_some();
    state.traffic.speed = None;
    state.traffic.speed_sequence = 0;
    if !assigned {
        return None;
    }
    let published = state
        .session
        .plan
        .fixes
        .iter()
        .skip(state.flow.monitor.next_fix)
        .any(|fix| fix.maximum_speed_knots.is_some());
    Some((
        say(
            if published {
                "arrival_resume_published_speed"
            } else {
                "arrival_resume_normal_speed"
            },
            &[],
        ),
        say(
            if published {
                "pilot_resume_published_speed_instruction"
            } else {
                "pilot_resume_normal_speed_instruction"
            },
            &[],
        ),
    ))
}

pub(crate) fn spacing(state: &mut EngineState, station: &Station) -> bool {
    let Some(profiles) = state.traffic_profiles.current() else {
        return false;
    };
    let policy = &profiles.spacing;
    if state.session.emergency_declared
        || holding::active(state)
        || !["Approach", "Tower"].contains(&station.service.as_str())
        || state.session.telemetry.on_ground
        || !(state.flow.approach_cleared || state.session.phase == PhaseCode::Arrival)
    {
        return false;
    }
    let Some(airport) = state.arrival_airport.as_ref() else {
        return false;
    };
    let t = &state.session.telemetry;
    let Some(position) = openatc_core::traffic::approach_position(
        airport,
        &state.session.plan.arrival_runway,
        t.latitude,
        t.longitude,
    ) else {
        return false;
    };
    let distance = -position.0 / 1852.0;
    if position.1.abs() > policy.lateral_meters
        || openatc_core::navigation::turn(
            t.ground_track_degrees.unwrap_or(t.heading_degrees),
            position.3,
        )
        .abs()
            > policy.alignment_degrees
    {
        return false;
    }
    if state.traffic.speed.is_some() && distance <= policy.release_nm {
        let (issued, prior) = state.traffic.speed.take().unwrap();
        if state.flow.monitor.speed != Some(issued) {
            return false;
        }
        state.flow.monitor.speed = prior;
        let slots = [("callsign", state.session.plan.callsign.clone())];
        let published = state
            .session
            .plan
            .fixes
            .iter()
            .skip(state.flow.monitor.next_fix)
            .any(|fix| fix.maximum_speed_knots.is_some());
        atc(
            state,
            station,
            if published {
                "traffic_resume_published_speed"
            } else {
                "traffic_resume_normal_speed"
            },
            &slots,
            if published {
                "pilot_resume_published_speed"
            } else {
                "pilot_resume_normal_speed"
            },
        );
        return true;
    }
    if state.traffic.speed_declined
        || state.traffic.speed.is_some()
        || !(policy.start_nm..=policy.end_nm).contains(&distance)
        || state.flow.monitor.speed.is_some()
        || t.indicated_altitude_feet
            .unwrap_or(t.altitude_feet)
            .max(t.altitude_feet)
            >= policy.maximum_altitude_feet
    {
        return false;
    }
    let aircraft = if t.aircraft_icao.is_empty() {
        &state.session.plan.aircraft
    } else {
        &t.aircraft_icao
    };
    let mut observed = t.clone();
    observed.aircraft_icao = aircraft.clone();
    let fixes = state
        .session
        .plan
        .fixes
        .iter()
        .skip(state.flow.monitor.next_fix)
        .take(2);
    let minimum = fixes
        .clone()
        .filter_map(|f| f.minimum_speed_knots)
        .max_by(f64::total_cmp);
    let maximum = fixes
        .filter_map(|f| f.maximum_speed_knots)
        .min_by(f64::total_cmp);
    use openatc_core::traffic::sequencing::{self, Decision};
    let decision = sequencing::assess(
        airport,
        &state.session.plan.arrival_runway,
        &observed,
        &profiles,
        minimum,
        maximum,
        policy.release_nm,
        // Represented terminal service uses the FAA FUSION standard; en-route
        // control is not admitted by this workflow's service guard.
        3.0,
    );
    state.traffic.spacing_path_required = matches!(decision, Decision::PathRequired);
    let Decision::Reduce(speed) = decision else {
        return false;
    };
    // Australian MATS prohibits changing final approach speed. An approach clearance
    // ends the pre-final speed workflow here; it is not used as permission to slow on final.
    if sequencing::Region::for_airport(&airport.icao) == Some(sequencing::Region::Australia)
        && state.session.phase == PhaseCode::Approach
    {
        return false;
    }
    state.traffic.speed = Some((speed, state.flow.monitor.speed));
    state.flow.monitor.speed = Some(speed);
    let slots = [
        ("callsign", state.session.plan.callsign.clone()),
        ("speed", format!("{speed:.0}")),
    ];
    atc(
        state,
        station,
        "traffic_reduce_speed",
        &slots,
        "pilot_speed_readback",
    );
    state.traffic.speed_sequence = state.session.transcript.last().map_or(0, |t| t.sequence);
    true
}

pub fn unable(
    state: &mut EngineState,
    station: &Station,
    request: &openatc_core::state::Request,
) -> bool {
    if state
        .session
        .transcript
        .iter()
        .rev()
        .find(|t| t.is_controller_instruction())
        .is_none_or(|t| t.sequence != state.traffic.speed_sequence)
    {
        return false;
    }
    let Some((issued, prior)) = state.traffic.speed.take() else {
        return false;
    };
    state.traffic.speed_declined = true;
    state.traffic.spacing_path_required = true;
    if state.flow.monitor.speed == Some(issued) {
        state.flow.monitor.speed = prior;
    }
    if let Some(last) = state
        .session
        .transcript
        .iter_mut()
        .rev()
        .find(|t| t.is_controller_instruction())
    {
        last.pilot_reply.clear();
    }
    if !request.controller_initiated {
        let callsign = state.session.plan.callsign.clone();
        add_transmission(
            &mut state.session,
            &callsign,
            &request.text,
            &SpeechTag {
                radio: Some(request.radio),
                ..Default::default()
            },
        );
    }
    atc(
        state,
        station,
        "traffic_unable_speed",
        &[("callsign", state.session.plan.callsign.clone())],
        "",
    );
    if let Some(reply) = state.session.transcript.last_mut() {
        reply.radio = Some(request.radio);
    }
    true
}

fn transmit(state: &mut EngineState, callsign: &str, voice: &str, text: &str) {
    let spoken_text = state.generated.spoken_text(text);
    let spoken_callsign = state.generated.spoken_text(callsign);
    add_transmission(
        &mut state.session,
        &spoken_callsign,
        &spoken_text,
        &SpeechTag {
            position: "Traffic".into(),
            voice: voice.into(),
            ..Default::default()
        },
    );
    if let Some(last) = state.session.transcript.last_mut() {
        last.background = true;
    }
}

pub async fn talk(AxumState(shared): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    if body["message"]
        .as_str()
        .is_some_and(|text| !text.trim().is_empty())
    {
        return conversation::request(shared, body).await;
    }
    let mut state = shared.write().await;
    if !state.settings.talk_to_traffic {
        return crate::routes::bad_request(
            "Enable conversations with simulated traffic pilots first.",
        );
    }
    if !ready(&state) {
        return crate::routes::bad_request("Wait for the current radio exchange to finish.");
    }
    let id = body["id"].as_u64().unwrap_or(0) as u32;
    let topic = body["topic"].as_str().unwrap_or("");
    if !["weather", "landing", "crosswind"].contains(&topic) {
        return crate::routes::bad_request("Ask about weather, landing or crosswind.");
    }
    let Some(pilot) = state
        .traffic
        .pilots
        .get(&id)
        .filter(|p| p.seen.elapsed().as_secs() < 5)
    else {
        return crate::routes::bad_request(
            "That aircraft is no longer in the current traffic scan.",
        );
    };
    if openatc_core::ops::distance_nm(
        state.session.telemetry.latitude,
        state.session.telemetry.longitude,
        pilot.target.latitude,
        pilot.target.longitude,
    ) > 20.0
    {
        return crate::routes::bad_request(
            "That aircraft is outside local traffic conversation range.",
        );
    }
    let callsign = pilot.target.callsign.clone();
    let voice = pilot.voice.clone();
    let landing = pilot
        .landing
        .as_ref()
        .filter(|l| l.at.elapsed().as_secs() < 300);
    let text = if topic == "landing" {
        if let Some(l) = landing {
            say("traffic_landing_observed", &[("runway", l.runway.clone())])
        } else {
            say("traffic_landing_unknown", &[])
        }
    } else if topic == "crosswind" {
        if let Some(l) = landing
            && let Some(weather) = &l.weather
            && let Some((component, right)) = weather["windDegrees"]
                .as_f64()
                .zip(weather["windKnots"].as_f64())
                .and_then(|(d, s)| openatc_core::traffic::crosswind(d, s, l.heading))
        {
            say(
                "traffic_crosswind_observed",
                &[
                    ("runway", l.runway.clone()),
                    ("speed", format!("{component:.0}")),
                    (
                        "side",
                        say(
                            if component < 1.0 {
                                "traffic_negligible_crosswind"
                            } else if right {
                                "traffic_from_right"
                            } else {
                                "traffic_from_left"
                            },
                            &[],
                        ),
                    ),
                ],
            )
        } else {
            say("traffic_crosswind_unknown", &[])
        }
    } else {
        let nearby = state
            .airport
            .iter()
            .chain(state.arrival_airport.iter())
            .filter(|a| {
                openatc_core::ops::distance_nm(
                    pilot.target.latitude,
                    pilot.target.longitude,
                    a.reference_latitude,
                    a.reference_longitude,
                ) < 5.0
            })
            .find_map(|a| {
                state
                    .sim_weather
                    .get(&a.icao)
                    .filter(|(_, at)| at.elapsed().as_secs() < 45)
                    .map(|(w, _)| (a, w))
            });
        if let Some((a, w)) = nearby
            && let Some((direction, speed)) = w["windDegrees"].as_f64().zip(w["windKnots"].as_f64())
            && let Some(visibility) = w["visibilityMeters"].as_f64()
        {
            say(
                "traffic_weather_observed",
                &[
                    ("airport", a.name.clone()),
                    ("direction", format!("{direction:03.0}")),
                    ("speed", format!("{speed:.0}")),
                    ("visibility", format!("{:.0}", visibility / 1000.0)),
                ],
            )
        } else {
            say("traffic_weather_unknown", &[])
        }
    };
    let call = state.session.plan.callsign.clone();
    let tag = SpeechTag {
        voice: state.settings.pilot_voice.clone(),
        ..Default::default()
    };
    let request = say(
        &format!("pilot_traffic_{topic}"),
        &[("callsign", call.clone()), ("traffic", callsign.clone())],
    );
    let phase = state.session.phase;
    let record = state.request_memory.begin("traffic", phase, &request);
    add_transmission(&mut state.session, &call, &request, &tag);
    transmit(&mut state, &callsign, &voice, &text);
    state.request_memory.finish(
        record,
        &json!({"result":{"accepted":true,"message":text}}),
        &[],
    );
    Json(json!({"accepted":true})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::aircraft::AircraftBehaviour;
    use openatc_core::{
        airport::{Airport, Runway},
        state::{Point, TaxiClearance, Telemetry},
    };
    fn airport() -> Airport {
        Airport {
            icao: "TEST".into(),
            runways: vec![Runway {
                first_name: "36".into(),
                second_name: "18".into(),
                second: Point {
                    north: 2000.0,
                    ..Default::default()
                },
                width: 45.0,
                ..Default::default()
            }],
            ..Default::default()
        }
    }
    #[test]
    fn taxi_hold_releases_only_after_clear_scan_and_preserves_runway_limit() {
        let mut s = crate::test_state();
        s.airport = Some(Airport {
            icao: "TEST".into(),
            ..Default::default()
        });
        s.session
            .on_event(openatc_core::aircraft::AircraftEvent::Taxi);
        s.session.taxi_clearance = TaxiClearance {
            airport: "TEST".into(),
            approved: true,
            instructions: "Taxi via A, hold short of runway 36.".into(),
            hold_short_runway: "36".into(),
            ..Default::default()
        };
        s.session.telemetry = Telemetry {
            on_ground: true,
            heading_degrees: 0.0,
            ground_speed_knots: 10.0,
            traffic: Some(vec![TrafficTarget {
                id: 4,
                callsign: "QFA1453".into(),
                latitude: 0.0005,
                longitude: 0.0006,
                on_ground: true,
                track_degrees: 270.0,
                ground_speed_knots: Some(10.0),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let station = Station {
            airport: "TEST".into(),
            service: "Ground".into(),
            ..Default::default()
        };
        assert!(surface(&mut s, &station));
        assert!(s.session.taxi_clearance.traffic_hold);
        assert!(
            s.session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("QFA1453")
        );
        let count = s.session.transcript.len();
        assert!(surface(&mut s, &station));
        assert_eq!(s.session.transcript.len(), count);
        s.session.telemetry.traffic = None;
        assert!(surface(&mut s, &station));
        assert!(s.session.taxi_clearance.traffic_hold);
        s.session.telemetry.traffic = Some(vec![]);
        assert!(surface(&mut s, &station));
        assert!(s.session.taxi_clearance.traffic_hold);
        s.traffic.clear_since = Some(Instant::now().checked_sub(Duration::from_secs(4)).unwrap());
        assert!(surface(&mut s, &station));
        assert!(!s.session.taxi_clearance.traffic_hold);
        assert_eq!(s.session.taxi_clearance.hold_short_runway, "36");
        assert!(s.session.taxi_clearance.crossing_runway.is_empty());
    }
    fn arrival(
        code: &str,
        speed: f64,
        distance: f64,
        lead_code: &str,
        lead_speed: f64,
        lead_distance: f64,
    ) -> EngineState {
        let mut state = crate::test_state();
        let mut port = airport();
        port.icao = "KTEST".into();
        state.arrival_airport = Some(port);
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
        state.session.plan.arrival_runway = "36".into();
        state.session.plan.aircraft = code.into();
        state.session.telemetry = Telemetry {
            latitude: -distance / 60.0,
            on_ground: false,
            aircraft_icao: code.into(),
            position_valid: true,
            ground_speed_knots: speed,
            indicated_speed_knots: Some(speed),
            traffic: Some(vec![TrafficTarget {
                id: 1,
                latitude: -lead_distance / 60.0,
                aircraft: lead_code.into(),
                on_ground: false,
                ground_speed_knots: Some(lead_speed),
                ..Default::default()
            }]),
            ..Default::default()
        };
        state
    }
    #[test]
    fn mixed_spacing_predicts_the_whole_approach_and_respects_aircraft_envelopes() {
        let station = Station {
            service: "Approach".into(),
            ..Default::default()
        };
        for (code, speed, distance, lead_code, lead_speed, lead_distance, minimum) in [
            ("C172", 105.0, 12.0, "PA28", 60.0, 6.0, 80.0),
            ("DH8D", 180.0, 14.0, "DH8D", 120.0, 8.0, 135.0),
            ("A320", 210.0, 18.0, "A320", 140.0, 10.0, 170.0),
        ] {
            let mut state = arrival(code, speed, distance, lead_code, lead_speed, lead_distance);
            assert!(spacing(&mut state, &station), "{code}");
            let issued = state.flow.monitor.speed.unwrap();
            assert!((minimum..speed).contains(&issued));
            assert!(
                state
                    .session
                    .transcript
                    .last()
                    .unwrap()
                    .pilot_reply
                    .contains(&format!("{issued:.0}"))
            );
            let mut impossible = arrival(
                code,
                minimum - 5.0,
                distance,
                lead_code,
                40.0,
                lead_distance,
            );
            assert!(!spacing(&mut impossible, &station));
            assert!(impossible.flow.monitor.speed.is_none());
            for published in [false, true] {
                let mut release = arrival(code, speed, 4.5, lead_code, lead_speed, 1.0);
                release.flow.monitor.speed = Some(issued);
                release.traffic.speed = Some((issued, None));
                if published {
                    release
                        .session
                        .plan
                        .fixes
                        .push(openatc_core::state::RouteFix {
                            maximum_speed_knots: Some(speed),
                            ..Default::default()
                        });
                }
                assert!(spacing(&mut release, &station));
                assert!(release.flow.monitor.speed.is_none());
                assert!(
                    release
                        .session
                        .transcript
                        .last()
                        .unwrap()
                        .text
                        .contains(if published {
                            "published"
                        } else {
                            "normal speed"
                        })
                );
            }
        }
        let mut ample = arrival("A320", 210.0, 20.0, "A320", 140.0, 4.0);
        assert!(!spacing(&mut ample, &station));
        assert!(!ample.traffic.spacing_path_required);
        let mut slow_ga = arrival("A320", 210.0, 18.0, "PA28", 70.0, 10.0);
        assert!(!spacing(&mut slow_ga, &station));
        assert!(slow_ga.traffic.spacing_path_required);
        assert!(slow_ga.flow.monitor.speed.is_none());
    }
    #[test]
    fn spacing_obeys_published_limits_final_regional_scope_and_unable() {
        let station = Station {
            service: "Approach".into(),
            ..Default::default()
        };
        let mut state = arrival("A320", 210.0, 18.0, "A320", 140.0, 10.0);
        state
            .session
            .plan
            .fixes
            .push(openatc_core::state::RouteFix {
                minimum_speed_knots: Some(210.0),
                ..Default::default()
            });
        assert!(!spacing(&mut state, &station));
        state.session.plan.fixes.clear();
        assert!(spacing(&mut state, &station));
        assert!(unable(
            &mut state,
            &station,
            &openatc_core::state::Request {
                text: "Unable that speed".into(),
                ..Default::default()
            }
        ));
        assert!(state.flow.monitor.speed.is_none());
        assert!(state.traffic.spacing_path_required);
        assert!(!spacing(&mut state, &station));
        let mut final_leg = arrival("A320", 210.0, 4.0, "PA28", 70.0, 1.0);
        assert!(!spacing(&mut final_leg, &station));
        let mut unsupported = arrival("A320", 210.0, 18.0, "A320", 140.0, 10.0);
        unsupported.arrival_airport.as_mut().unwrap().icao = "LFMN".into();
        assert!(!spacing(&mut unsupported, &station));
    }
    #[tokio::test]
    async fn traffic_talk_requires_current_target_and_remembers_observed_landing_weather() {
        let mut s = crate::test_state();
        s.airport = Some(airport());
        s.settings.talk_to_traffic = true;
        s.session.telemetry = Telemetry {
            radio_power: true,
            latitude: 0.001,
            traffic: Some(vec![TrafficTarget {
                id: 42,
                callsign: "QFA42".into(),
                latitude: -0.001,
                track_degrees: 0.0,
                ..Default::default()
            }]),
            ..Default::default()
        };
        s.sim_weather.insert(
            "TEST".into(),
            (
                json!({"windDegrees":90.0,"windKnots":20.0,"visibilityMeters":10000.0}),
                Instant::now(),
            ),
        );
        observe(&mut s);
        s.traffic.observed = None;
        {
            let target = &mut s.session.telemetry.traffic.as_mut().unwrap()[0];
            target.on_ground = true;
            target.latitude = 0.001;
        }
        observe(&mut s);
        assert!(s.traffic.pilots[&42].landing.is_some());
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(s));
        let response = talk(
            AxumState(shared.clone()),
            Json(json!({"id":42,"topic":"crosswind"})),
        )
        .await;
        assert!(response.status().is_success());
        {
            let s = shared.read().await;
            assert!(s.session.transcript.last().unwrap().text.contains("20"));
            assert!(s.session.transcript.last().unwrap().background);
        }
        shared.write().await.settings.talk_to_traffic = false;
        assert!(
            !talk(
                AxumState(shared.clone()),
                Json(json!({"id":42,"topic":"landing"}))
            )
            .await
            .status()
            .is_success()
        );
        shared.write().await.settings.talk_to_traffic = true;
        shared
            .write()
            .await
            .traffic
            .pilots
            .get_mut(&42)
            .unwrap()
            .seen = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        assert!(
            !talk(AxumState(shared), Json(json!({"id":42,"topic":"landing"})))
                .await
                .status()
                .is_success()
        );
    }
}
