//! Airborne instructions, route progress and compliance monitoring.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use crate::EngineState;
use openatc_core::aircraft::AircraftBehaviour;
use openatc_core::{
    apply::add_transmission,
    dialogue::say,
    navigation,
    state::{PhaseCode, Request, SpeechTag},
    stations::Station,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
mod planning;
pub use planning::Policy as VerticalPolicy;

#[derive(Default)]
pub struct Monitor {
    pub next_fix: usize,
    pub route: String,
    pub direct_origin: Option<[f64; 2]>,
    pub heading: Option<f64>,
    pub speed: Option<f64>,
    pub deviation: Option<(String, f64)>,
    pub last_call: Option<Instant>,
    pub warning: openatc_core::compliance::WarningCount,
    pub previous: Option<openatc_core::state::Telemetry>,
    pub step_target: Option<i32>,
    pub checked_fix: usize,
    pub speed_reminder: Option<(usize, f64)>,
    pub pending_sequence: u32,
    pub pending_since: Option<Instant>,
}
fn transmit(state: &mut EngineState, station: &Station, text: &str, reply: Option<String>) {
    let tag = crate::flight::tag(state, station);
    add_transmission(&mut state.session, "ATC", text, &tag);
    if let Some(reply) = reply
        && let Some(last) = state.session.transcript.last_mut()
    {
        last.pilot_reply = reply;
    }
    state.flow.monitor.last_call = Some(Instant::now());
}
fn reply(
    state: &mut EngineState,
    request: &Request,
    tag: &SpeechTag,
    accepted: bool,
    text: &str,
    pilot: Option<String>,
) -> Value {
    if !request.controller_initiated {
        let call = state.session.plan.callsign.clone();
        add_transmission(
            &mut state.session,
            &call,
            &request.text,
            &SpeechTag::default(),
        );
    }
    add_transmission(&mut state.session, "ATC", text, tag);
    if let Some(pilot) = pilot
        && let Some(last) = state.session.transcript.last_mut()
    {
        last.pilot_reply = pilot;
    }
    json!({"accepted":accepted,"message":text})
}
/// Remaining imported-route distance; a direct distance is only a fallback.
pub fn remaining_nm(state: &EngineState) -> Option<f64> {
    let t = &state.session.telemetry;
    let mut point = [t.latitude, t.longitude];
    let mut distance = 0.0;
    let next = state.flow.monitor.next_fix.max(1);
    for fix in state.session.plan.fixes.iter().skip(next) {
        let target = [fix.latitude, fix.longitude];
        distance += openatc_core::arrival::distance_nm(point, target);
        point = target;
    }
    if let Some(airport) = &state.arrival_airport {
        distance += openatc_core::arrival::distance_nm(
            point,
            [airport.reference_latitude, airport.reference_longitude],
        );
    }
    (distance > 0.0).then_some(distance)
}
/// A nominal three-degree descent with ten miles left for final approach.
pub fn descent_feasible(state: &EngineState, altitude: i32) -> bool {
    let Some(distance) = remaining_nm(state) else {
        return f64::from(altitude) <= state.session.telemetry.altitude_feet;
    };
    let elevation = state
        .arrival_airport
        .as_ref()
        .map_or(0.0, |a| a.elevation_feet);
    altitude_within_descent(distance, elevation, altitude)
}
fn altitude_within_descent(distance: f64, elevation: f64, altitude: i32) -> bool {
    let ceiling = elevation + (distance - 10.0).max(0.0) * openatc_core::arrival::gradient(3.0);
    distance.is_finite()
        && elevation.is_finite()
        && f64::from(altitude) <= ceiling.max(elevation + 2000.0)
}
/// Reject amendments that cannot meet the next published crossing window.
pub fn altitude_allowed(state: &EngineState, altitude: i32) -> bool {
    if !state.settings.enforce_clearance_constraints {
        return true;
    }
    let next = state.flow.monitor.next_fix.max(1);
    state.session.plan.fixes.get(next).is_none_or(|f| {
        let t = &state.session.telemetry;
        let distance =
            openatc_core::ops::distance_nm(t.latitude, t.longitude, f.latitude, f.longitude);
        f.minimum_altitude_feet.is_none_or(|v| altitude >= v)
            && f.maximum_altitude_feet.is_none_or(|v| {
                f64::from(altitude)
                    <= f64::from(v)
                        + (distance - 5.0).max(0.0) * openatc_core::arrival::gradient(3.0)
            })
    })
}
pub fn request(
    state: &mut EngineState,
    station: &Station,
    request: &Request,
    tag: &SpeechTag,
) -> Option<Value> {
    if !["speed", "deviation", "resume_course"].contains(&request.intent.as_str()) {
        return None;
    }
    if !openatc_core::ops::request_available(&state.session, &request.intent) {
        return Some(reply(
            state,
            request,
            tag,
            false,
            &say("amendment_unavailable", &[]),
            None,
        ));
    }
    let call = state.session.plan.callsign.clone();
    let value = request.waypoint.trim().to_lowercase();
    let (text, pilot) = match request.intent.as_str() {
        "speed" => {
            let speed = value
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && (50.0..=400.0).contains(v));
            let Some(speed) = speed else {
                return Some(reply(
                    state,
                    request,
                    tag,
                    false,
                    &say("speed_request_invalid", &[]),
                    None,
                ));
            };
            if state.session.telemetry.indicated_speed_knots.is_none() {
                return Some(reply(
                    state,
                    request,
                    tag,
                    false,
                    &say("speed_indication_unavailable", &[]),
                    None,
                ));
            }
            let limit = state
                .session
                .plan
                .fixes
                .get(state.flow.monitor.next_fix.max(1))
                .and_then(|f| f.maximum_speed_knots);
            let us = station.airport.starts_with('K')
                || station.airport.starts_with("PA")
                || station.airport.starts_with("PH");
            if limit.is_some_and(|v| speed > v)
                || (us && state.session.telemetry.altitude_feet < 10000.0 && speed > 250.0)
            {
                return Some(reply(
                    state,
                    request,
                    tag,
                    false,
                    &say("speed_restriction_unable", &[]),
                    None,
                ));
            }
            state.flow.monitor.speed = Some(speed);
            let slots = [("callsign", call), ("speed", format!("{speed:.0}"))];
            (
                say("assigned_speed", &slots),
                say("pilot_speed_readback", &slots),
            )
        }
        "deviation" => {
            let parts: Vec<_> = value.split_whitespace().collect();
            let Some(degrees) = parts
                .get(1)
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|v| v.is_finite() && (1.0..=45.0).contains(v))
            else {
                return Some(reply(
                    state,
                    request,
                    tag,
                    false,
                    &say("deviation_request_invalid", &[]),
                    None,
                ));
            };
            let direction = parts.first().copied().unwrap_or("");
            if !["left", "right"].contains(&direction) {
                return Some(reply(
                    state,
                    request,
                    tag,
                    false,
                    &say("deviation_request_invalid", &[]),
                    None,
                ));
            }
            state.flow.monitor.deviation = Some((direction.into(), degrees));
            state.flow.monitor.heading = None;
            state.flow.monitor.warning.resolved();
            let slots = [
                ("callsign", call),
                ("direction", direction.into()),
                ("degrees", format!("{degrees:.0}")),
            ];
            (
                say("deviation_approved", &slots),
                say("pilot_deviation_readback", &slots),
            )
        }
        _ => {
            state.flow.monitor.deviation = None;
            state.flow.monitor.heading = None;
            let next = navigation::route_ahead(
                &state.session.plan.fixes,
                state.flow.monitor.next_fix.max(1),
                &state.session.telemetry,
            );
            state.flow.monitor.next_fix = next;
            let Some(fix) = state.session.plan.fixes.get(next) else {
                return Some(reply(
                    state,
                    request,
                    tag,
                    false,
                    &say("route_geometry_unavailable", &[]),
                    None,
                ));
            };
            let slots = [("callsign", call), ("fix", fix.identifier.clone())];
            (
                say("deviation_resume", &slots),
                say("pilot_resume_readback", &slots),
            )
        }
    };
    state.flow.monitor.last_call = Some(Instant::now());
    Some(reply(state, request, tag, true, &text, Some(pilot)))
}
/// One transmission per tick, after handoffs and pending readbacks have cleared.
/// Keep handoffs and new instructions behind the current readback.
pub fn awaiting_reply(state: &mut EngineState, station: &Station) -> bool {
    let pending = state
        .session
        .clearance
        .as_ref()
        .is_some_and(|c| !c.acknowledged)
        || state
            .session
            .transcript
            .iter()
            .rev()
            .find(|e| e.is_controller_instruction())
            .is_some_and(|e| !e.pilot_reply.is_empty());
    if !pending {
        state.flow.monitor.pending_since = None;
        return false;
    }
    let sequence = state
        .session
        .transcript
        .iter()
        .rev()
        .find(|e| e.is_controller_instruction())
        .map_or(0, |e| e.sequence);
    let monitor = &mut state.flow.monitor;
    if monitor.pending_since.is_none() || monitor.pending_sequence != sequence {
        monitor.pending_sequence = sequence;
        monitor.pending_since = Some(Instant::now());
    }
    if monitor
        .pending_since
        .is_some_and(|t| t.elapsed() >= Duration::from_secs(45))
    {
        let text = say(
            "instruction_readback_reminder",
            &[("callsign", state.session.plan.callsign.clone())],
        );
        // Preserve the actionable reply on the reminder rather than losing it.
        let reply = state
            .session
            .transcript
            .iter()
            .rev()
            .find(|e| e.is_controller_instruction())
            .and_then(|e| (!e.pilot_reply.is_empty()).then(|| e.pilot_reply.clone()));
        transmit(state, station, &text, reply);
        state.flow.monitor.pending_since = Some(Instant::now());
        state.flow.monitor.pending_sequence =
            state.session.transcript.last().map_or(0, |e| e.sequence);
        crate::traffic::holding::reminder(state, sequence, state.flow.monitor.pending_sequence);
    }
    true
}
pub fn tick(state: &mut EngineState, station: &Station) {
    let t = state.session.telemetry.clone();
    if t.on_ground
        || !["Approach", "Departure", "Center"].contains(&station.service.as_str())
        || state.session.clearance_cancelled
        || state
            .session
            .clearance
            .as_ref()
            .is_none_or(|c| !c.acknowledged)
        || state
            .session
            .transcript
            .iter()
            .rev()
            .find(|e| e.is_controller_instruction())
            .is_some_and(|e| !e.pilot_reply.is_empty())
    {
        return;
    }
    let previous = state.flow.monitor.previous.replace(t.clone());
    let c = state.session.clearance.as_ref().unwrap().clone();
    let own = [t.latitude, t.longitude];
    if state.flow.monitor.route != c.route {
        state.flow.monitor.route.clone_from(&c.route);
        state.flow.monitor.speed_reminder = None;
        state.flow.monitor.heading = None;
        state.flow.monitor.next_fix = state
            .session
            .plan
            .fixes
            .iter()
            .position(|f| f.identifier == c.route)
            .unwrap_or(1)
            .max(1);
        state.flow.monitor.direct_origin = (c.readback_scope == "direct").then_some(own);
    }
    let mut fixes = state.session.plan.fixes.clone();
    let mut next = state.flow.monitor.next_fix.max(1);
    if let Some(origin) = state.flow.monitor.direct_origin
        && next > 0
        && let Some(f) = fixes.get_mut(next - 1)
    {
        f.latitude = origin[0];
        f.longitude = origin[1];
    }
    if c.readback_scope != "direct"
        && state.flow.monitor.heading.is_none()
        && navigation::leg(&fixes, next, &t).is_none_or(|leg| leg.cross_track_nm > 2.0)
        && let Some(established) = navigation::established_star_leg(&fixes, next, &t)
    {
        state.flow.monitor.next_fix = established;
        state.flow.monitor.checked_fix = established - 1;
        next = established;
    }
    let mut leg = navigation::leg(&fixes, next, &t);
    let call = state.session.plan.callsign.clone();
    let ready = state
        .flow
        .monitor
        .last_call
        .is_none_or(|last| last.elapsed() >= Duration::from_secs(30));
    let captured = fixes.get(next).is_some_and(|fix| {
        openatc_core::arrival::distance_nm(own, [fix.latitude, fix.longitude]) < 0.05
    });
    if captured
        || navigation::fix_passed_on_outbound(&fixes, next, &t)
        || leg.as_ref().is_some_and(|leg| {
            leg.along >= 1.0
                && (leg.cross_track_nm < 1.5 || fixes.get(next).is_some_and(|fix| !fix.fly_over))
        })
    {
        if next != state.flow.monitor.checked_fix {
            state.flow.monitor.checked_fix = next;
            if let Some(fix) = fixes.get(next)
                && state.settings.enforce_clearance_constraints
            {
                let interpolation = previous
                    .as_ref()
                    .and_then(|p| {
                        navigation::leg(&fixes, next, p)
                            .zip(leg.as_ref())
                            .map(|(l, current)| (p, l, current))
                    })
                    .filter(|(_, l, current)| {
                        l.along < 1.0 && current.along >= 1.0 && current.along > l.along
                    })
                    .map(|(p, l, current)| {
                        (
                            p,
                            ((1.0 - l.along) / (current.along - l.along)).clamp(0.0, 1.0),
                        )
                    });
                let airport = if fix.constraint_source.starts_with("SID ") {
                    &state.session.plan.departure
                } else {
                    &state.session.plan.destination
                };
                let transition =
                    openatc_core::regions::region_for(&state.regions, airport).transition_feet;
                let altitude = |limit, reference: Option<bool>| {
                    let sample = |telemetry: &openatc_core::state::Telemetry| match reference {
                        Some(true) => telemetry
                            .pressure_altitude_feet
                            .unwrap_or_else(|| telemetry.reported_altitude_feet()),
                        Some(false) => telemetry.reported_altitude_feet(),
                        None => telemetry.clearance_altitude_feet(limit, transition),
                    };
                    interpolation.map_or_else(
                        || sample(&t),
                        |(previous, fraction)| {
                            sample(previous) + (sample(&t) - sample(previous)) * fraction
                        },
                    )
                };
                let speed = interpolation
                    .and_then(|(p, f)| {
                        p.indicated_speed_knots
                            .zip(t.indicated_speed_knots)
                            .map(|(a, b)| a + (b - a) * f)
                    })
                    .or(t.indicated_speed_knots);
                let altitude_missed = fix.minimum_altitude_feet.is_some_and(|v| {
                    altitude(v, fix.minimum_altitude_is_flight_level) < f64::from(v) - 200.0
                }) || fix.maximum_altitude_feet.is_some_and(|v| {
                    altitude(v, fix.maximum_altitude_is_flight_level) > f64::from(v) + 200.0
                });
                let speed_missed = matches!((fix.maximum_speed_knots,speed),(Some(v),Some(speed)) if speed>v+10.0)
                    || matches!((fix.minimum_speed_knots,speed),(Some(v),Some(speed)) if speed<v-10.0);
                if altitude_missed || speed_missed {
                    let text = say(
                        match (altitude_missed, speed_missed) {
                            (true, true) => "crossing_restriction_missed",
                            (true, false) => "crossing_altitude_missed",
                            _ => "crossing_speed_missed",
                        },
                        &[("callsign", call.clone()), ("fix", fix.identifier.clone())],
                    );
                    correct(
                        state,
                        station,
                        &text,
                        Some(say(
                            match (altitude_missed, speed_missed) {
                                (true, true) => "pilot_checking_restriction",
                                (true, false) => "pilot_checking_crossing_altitude",
                                _ => "pilot_checking_crossing_speed",
                            },
                            &[("callsign", call.clone()), ("fix", fix.identifier.clone())],
                        )),
                    );
                    state.flow.monitor.next_fix = next + 1;
                    state.flow.monitor.direct_origin = None;
                    return;
                }
            }
        }
        state.flow.monitor.step_target = fixes.get(next).and_then(|f| f.step_altitude_feet);
        state.flow.monitor.next_fix = next + 1;
        state.flow.monitor.direct_origin = None;
        state.flow.monitor.heading = None;
    }
    let ahead = navigation::route_ahead(&fixes, state.flow.monitor.next_fix.max(1), &t);
    if ahead != next {
        state.flow.monitor.next_fix = ahead;
        next = ahead;
        leg = navigation::leg(&fixes, next, &t);
    }
    if !ready {
        return;
    }
    if let Some(heading) = state.flow.monitor.heading {
        if previous.as_ref().is_some_and(|p| {
            navigation::turn(p.heading_degrees, heading).abs()
                > navigation::turn(t.heading_degrees, heading).abs() + 1.0
        }) {
            return;
        }
        if navigation::turn(t.heading_degrees, heading).abs() > 15.0
            && state.settings.enforce_clearance_constraints
        {
            let turn = navigation::turn(t.heading_degrees, heading);
            let slots = [
                ("callsign", call.clone()),
                (
                    "direction",
                    if turn < 0.0 { "left" } else { "right" }.into(),
                ),
                ("degrees", format!("{:.0}", turn.abs())),
            ];
            correct(
                state,
                station,
                &say("course_correction_relative", &slots),
                Some(say("pilot_turn_relative", &slots)),
            );
            return;
        }
    }
    if let Some(speed) = state.flow.monitor.speed
        && let Some(ias) = t.indicated_speed_knots
        && (ias - speed).abs() > 20.0
        && state.settings.enforce_clearance_constraints
    {
        let slots = [("callsign", call.clone()), ("speed", format!("{speed:.0}"))];
        correct(
            state,
            station,
            &say("speed_correction", &slots),
            Some(say("pilot_speed_readback", &slots)),
        );
        return;
    }
    // Do not infer a straight airport-to-fix path through an unresolved SID.
    if let Some(leg) = leg
        && (next > 1 || state.session.plan.sid.is_empty() || c.readback_scope == "direct")
        && leg.cross_track_nm > 2.0
        && t.height_agl_feet > 1500.0
        && !matches!(state.session.phase, PhaseCode::Approach | PhaseCode::Landed)
    {
        let track = t.ground_track_degrees.unwrap_or(t.heading_degrees);
        let course = fixes
            .get(next - 1)
            .zip(fixes.get(next))
            .map_or(leg.bearing, |(a, b)| {
                openatc_core::arrival::bearing([a.latitude, a.longitude], [b.latitude, b.longitude])
            });
        let deviation = navigation::turn(course, track);
        if state
            .flow
            .monitor
            .deviation
            .as_ref()
            .is_some_and(|(side, limit)| {
                deviation.abs() <= *limit + 5.0 && (deviation < 0.0) == (side == "left")
            })
        {
            return;
        }
        let turn = navigation::turn(t.heading_degrees, leg.bearing);
        if turn.abs() > 5.0 {
            let destination = [fixes[next].latitude, fixes[next].longitude];
            if state
                .mora
                .corridor(own, destination)
                .is_none_or(|floor| t.altitude_feet < f64::from(floor))
            {
                transmit(
                    state,
                    station,
                    &say(
                        "off_course_vector_unavailable",
                        &[("callsign", call.clone())],
                    ),
                    Some(say("pilot_checking_navigation", &[("callsign", call)])),
                );
                return;
            }
            state.flow.monitor.heading = Some(leg.bearing);
            let direction = if turn < 0.0 { "left" } else { "right" };
            let slots = [
                ("callsign", call.clone()),
                ("direction", direction.into()),
                ("degrees", format!("{:.0}", turn.abs())),
                ("fix", fixes[next].identifier.clone()),
            ];
            if let Some(magnetic) = t.magnetic_heading_degrees {
                let heading =
                    (leg.bearing - navigation::turn(magnetic, t.heading_degrees)).rem_euclid(360.0);
                let mut slots = slots.to_vec();
                slots.push((
                    "heading",
                    format!("{:03.0}", if heading < 0.5 { 360.0 } else { heading }),
                ));
                correct(
                    state,
                    station,
                    &say("course_correction_heading", &slots),
                    Some(say("pilot_heading_readback", &slots)),
                );
            } else {
                correct(
                    state,
                    station,
                    &say("course_correction_relative", &slots),
                    Some(say("pilot_turn_relative", &slots)),
                );
            }
            return;
        }
    } else {
        state.flow.monitor.warning.resolved();
    }
    if descend(state, station) {
        return;
    }
    if published_speed_reminder(state, station) {
        return;
    }
    climb(state, station);
}

fn published_speed_notice(state: &EngineState) -> Option<(usize, f64, bool)> {
    if !matches!(
        state.session.phase,
        PhaseCode::Arrival | PhaseCode::Approach
    ) || state.flow.monitor.heading.is_some()
        || state.flow.monitor.speed.is_some()
    {
        return None;
    }
    let t = &state.session.telemetry;
    let ias = t
        .indicated_speed_knots
        .filter(|v| v.is_finite() && *v > 0.0)?;
    let next = state.flow.monitor.next_fix.max(1);
    let fixes = &state.session.plan.fixes;
    // An initial fix can carry a STAR speed before its first TF leg. A reminder
    // names that published fix; it does not infer a vector or clear a shortcut.
    if navigation::leg(fixes, next, t).map_or_else(
        || {
            fixes
                .get(next)
                .is_none_or(|fix| fix.path_terminator != "IF")
        },
        |leg| leg.cross_track_nm > 2.0,
    ) {
        return None;
    }
    let mut point = [t.latitude, t.longitude];
    let mut distance = 0.0;
    for (index, fix) in fixes.iter().enumerate().skip(next).take(16) {
        let target = [fix.latitude, fix.longitude];
        distance += openatc_core::arrival::distance_nm(point, target);
        point = target;
        let Some(limit) = fix
            .maximum_speed_knots
            .filter(|v| v.is_finite() && *v > 0.0)
        else {
            continue;
        };
        if ias <= limit + 5.0 {
            continue;
        }
        let policy = &state.vertical_policy;
        let seconds =
            policy.speed_notice_seconds + (ias - limit) / policy.speed_notice_deceleration;
        let lead = (t.ground_speed_knots.max(ias) * seconds / 3600.0)
            .clamp(policy.speed_notice_min_nm, policy.speed_notice_max_nm);
        if distance > lead || state.flow.monitor.speed_reminder == Some((index, limit)) {
            return None;
        }
        let exact = fix.minimum_speed_knots == Some(limit);
        return Some((index, limit, exact));
    }
    None
}

fn published_speed_reminder(state: &mut EngineState, station: &Station) -> bool {
    let Some((index, limit, exact)) = published_speed_notice(state) else {
        return false;
    };
    let fix = &state.session.plan.fixes[index];
    let slots = [
        ("callsign", state.session.plan.callsign.clone()),
        ("fix", fix.identifier.clone()),
        ("speed", format!("{limit:.0}")),
    ];
    let text = say(
        if exact {
            "published_speed_exact_reminder"
        } else {
            "published_speed_maximum_reminder"
        },
        &slots,
    );
    let pilot = say(
        if exact {
            "pilot_published_speed_exact"
        } else {
            "pilot_published_speed_maximum"
        },
        &slots,
    );
    state.flow.monitor.speed_reminder = Some((index, limit));
    transmit(state, station, &text, Some(pilot));
    true
}

fn correct(state: &mut EngineState, station: &Station, text: &str, pilot: Option<String>) {
    if !state.settings.enforce_clearance_constraints {
        state.flow.monitor.warning.resolved();
    }
    let repeated = state.flow.monitor.warning.continuing()
        && state.settings.enforce_clearance_constraints
        && state.settings.terminate_after_warnings
        && !state.session.emergency_declared;
    if repeated {
        let assistance = say("compliance_assistance", &[]);
        transmit(state, station, &format!("{text} {assistance}"), pilot);
    } else {
        transmit(state, station, text, pilot);
    }
}
fn climb(state: &mut EngineState, station: &Station) {
    let t = &state.session.telemetry;
    let c = state.session.clearance.as_ref().unwrap();
    if !matches!(
        state.session.phase,
        PhaseCode::Departure | PhaseCode::Cruise
    ) || t.altitude_feet < f64::from(c.altitude_feet) - 500.0
        || state.flow.monitor.deviation.is_some()
    {
        return;
    }
    let ceiling = station
        .coverage
        .iter()
        .filter(|v| v.contains(t))
        .map(|v| v.ceiling)
        .reduce(f64::max)
        .unwrap_or(f64::from(state.session.plan.cruise_feet));
    let requested = state
        .flow
        .monitor
        .step_target
        .unwrap_or(state.session.plan.cruise_feet);
    let mut target = requested.min(((ceiling - 500.0) / 1000.0).floor() as i32 * 1000);
    if state.settings.enforce_clearance_constraints
        && let Some(limit) = state
            .session
            .plan
            .fixes
            .get(state.flow.monitor.next_fix.max(1))
            .and_then(|f| f.maximum_altitude_feet)
    {
        target = target.min(limit);
    }
    if target <= c.altitude_feet
        || !altitude_allowed(state, target)
        || !descent_feasible(state, target)
    {
        return;
    }
    let phrase = if !state.session.plan.sid.is_empty()
        && !state.session.plan.sid_has_vectors
        && state
            .session
            .plan
            .fixes
            .iter()
            .any(|f| f.constraint_source.starts_with("SID"))
    {
        "automatic_climb_via_sid"
    } else if t.vertical_speed_fpm > 200.0 {
        "automatic_continue_climb"
    } else {
        "automatic_climb"
    };
    let slots = [
        ("sid", state.session.plan.sid.clone()),
        ("callsign", state.session.plan.callsign.clone()),
        (
            "altitude",
            openatc_core::altitude_text(
                f64::from(target),
                openatc_core::UnitSystem::Imperial,
                true,
            ),
        ),
    ];
    let text = say(phrase, &slots);
    if state.flow.monitor.step_target.is_some() && target == requested {
        state.session.plan.cruise_feet = target;
        state.flow.monitor.step_target = None;
    }
    let sequence = state.session.next_sequence;
    let c = state.session.clearance.as_mut().unwrap();
    c.altitude_feet = target;
    c.sequence = sequence;
    c.acknowledged = false;
    c.readback_scope = "altitude".into();
    transmit(
        state,
        station,
        &text,
        Some(say("pilot_continue_climb", &slots)),
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Vertical {
    Climb,
    Descend,
    Maintain,
}

/// The instruction word follows the actual altitude, never the source of the target.
fn vertical_for(target: i32, actual: f64) -> Vertical {
    const TOLERANCE_FEET: f64 = 200.0;
    if actual < f64::from(target) - TOLERANCE_FEET {
        Vertical::Climb
    } else if actual > f64::from(target) + TOLERANCE_FEET {
        Vertical::Descend
    } else {
        Vertical::Maintain
    }
}

fn transmit_descent(
    state: &mut EngineState,
    station: &Station,
    mut text: String,
    target: i32,
    direction: Vertical,
    altitude_crossing: Option<(String, i32, bool)>,
) {
    if direction == Vertical::Descend {
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
    }
    let notice = published_speed_notice(state);
    let speed = notice.map(|(index, limit, exact)| {
        let fix = state.session.plan.fixes[index].identifier.clone();
        let slots = [("fix", fix.clone()), ("speed", format!("{limit:.0}"))];
        text.push(' ');
        text.push_str(&say(
            if exact {
                "published_speed_exact_continuation"
            } else {
                "published_speed_maximum_continuation"
            },
            &slots,
        ));
        state.flow.monitor.speed_reminder = Some((index, limit));
        openatc_core::state::SpeedReadback {
            sequence: state.session.next_sequence,
            fix,
            knots: limit,
            exact,
        }
    });
    let altitude_readback = altitude_crossing.map(|(fix, altitude_feet, exact)| {
        openatc_core::state::AltitudeReadback {
            sequence: state.session.next_sequence,
            fix,
            altitude_feet,
            exact,
        }
    });
    let clearance = state.session.clearance.as_mut().unwrap();
    clearance.altitude_feet = target;
    clearance.sequence = state.session.next_sequence;
    clearance.readback_scope = "altitude".into();
    clearance.readback_speed = speed;
    clearance.readback_altitude = altitude_readback;
    clearance.acknowledged = false;
    let altitude =
        openatc_core::altitude_text(f64::from(target), openatc_core::UnitSystem::Imperial, true);
    let pilot = match direction {
        Vertical::Descend => {
            openatc_core::readback::descent_reply(clearance, &state.session.plan.callsign)
        }
        Vertical::Climb => say(
            "pilot_continue_climb",
            &[
                ("callsign", state.session.plan.callsign.clone()),
                ("altitude", altitude),
            ],
        ),
        Vertical::Maintain => say(
            "pilot_maintain_altitude",
            &[
                ("callsign", state.session.plan.callsign.clone()),
                ("altitude", altitude),
            ],
        ),
    };
    transmit(state, station, &text, Some(pilot));
}

fn descend(state: &mut EngineState, station: &Station) -> bool {
    if !matches!(
        state.session.phase,
        PhaseCode::Cruise | PhaseCode::Arrival | PhaseCode::Departure | PhaseCode::Approach
    ) || state.flow.approach_cleared
        || state.flow.missed_approach
    {
        return false;
    }
    let t = &state.session.telemetry;
    let c = state.session.clearance.as_ref().unwrap();
    let next = navigation::route_ahead(
        &state.session.plan.fixes,
        state.flow.monitor.next_fix.max(1),
        t,
    );
    let mut point = [t.latitude, t.longitude];
    let mut distance = 0.0;
    let mut preceding_floor = i32::MIN;
    let mut selected = None;
    for fix in state.session.plan.fixes.iter().skip(next) {
        let target = [fix.latitude, fix.longitude];
        distance += openatc_core::arrival::distance_nm(point, target);
        point = target;
        if let Some(upper) = fix.maximum_altitude_feet
            && upper < c.altitude_feet
            && preceding_floor <= upper
            && distance.is_finite()
            && distance <= state.vertical_policy.distance(c.altitude_feet, upper)
        {
            let target = upper.max(fix.minimum_altitude_feet.unwrap_or(upper));
            // An at-or-below restriction already met at the aircraft's actual
            // altitude no longer limits the flight; keep looking ahead.
            if fix.minimum_altitude_feet != Some(upper)
                && t.altitude_feet <= f64::from(target) + 200.0
            {
                preceding_floor =
                    preceding_floor.max(fix.minimum_altitude_feet.unwrap_or(i32::MIN));
                continue;
            }
            selected = Some((fix, upper));
            break;
        }
        preceding_floor = preceding_floor.max(fix.minimum_altitude_feet.unwrap_or(i32::MIN));
    }
    let Some((fix, upper)) = selected else {
        return destination_descent(state, station);
    };
    let target = upper.max(fix.minimum_altitude_feet.unwrap_or(upper));
    let slots = [
        ("callsign", state.session.plan.callsign.clone()),
        (
            "altitude",
            openatc_core::altitude_text(
                f64::from(target),
                openatc_core::UnitSystem::Imperial,
                true,
            ),
        ),
    ];
    let direction = vertical_for(target, t.altitude_feet);
    let phrase = match direction {
        Vertical::Climb if t.vertical_speed_fpm > 200.0 => "automatic_continue_climb",
        Vertical::Climb => "automatic_climb",
        Vertical::Descend if t.vertical_speed_fpm < -200.0 => "automatic_continue_descent",
        Vertical::Descend => "automatic_descent",
        Vertical::Maintain => "automatic_maintain",
    };
    let mut text = say(phrase, &slots);
    let crossing = say(
        if fix.minimum_altitude_feet == fix.maximum_altitude_feet {
            "crossing_at_continuation"
        } else {
            "crossing_below_continuation"
        },
        &[
            ("callsign", state.session.plan.callsign.clone()),
            ("fix", fix.identifier.clone()),
            ("altitude", slots[1].1.clone()),
        ],
    );
    text.push(' ');
    text.push_str(&crossing);
    transmit_descent(
        state,
        station,
        text,
        target,
        direction,
        Some((
            fix.identifier.clone(),
            target,
            fix.minimum_altitude_feet == fix.maximum_altitude_feet,
        )),
    );
    true
}

fn destination_descent(state: &mut EngineState, station: &Station) -> bool {
    let Some(profile) = &state.arrival_profile else {
        return false;
    };
    let Some(arrival) = openatc_core::arrival::target(
        profile,
        &state.session.plan.arrival_runway,
        &state.session.plan.approach,
    ) else {
        return false;
    };
    let Some(entry) = arrival.intercept_feet.filter(|altitude| {
        altitude.is_finite() && *altitude > arrival.elevation_feet && *altitude <= 20000.0
    }) else {
        return false;
    };
    let current = state.session.clearance.as_ref().unwrap().altitude_feet;
    let Some(distance) = remaining_nm(state).filter(|distance| distance.is_finite()) else {
        return false;
    };
    if distance > state.vertical_policy.distance(current, entry.ceil() as i32) {
        return false;
    }
    let mut floor = entry;
    let mut point = [
        state.session.telemetry.latitude,
        state.session.telemetry.longitude,
    ];
    for fix in state
        .session
        .plan
        .fixes
        .iter()
        .skip(state.flow.monitor.next_fix.max(1))
    {
        if !["", "IF", "TF", "DF"].contains(&fix.path_terminator.as_str()) {
            return false;
        }
        let destination = [fix.latitude, fix.longitude];
        let Some(terrain) = state.mora.corridor(point, destination) else {
            return false;
        };
        floor = floor
            .max(f64::from(terrain))
            .max(f64::from(fix.minimum_altitude_feet.unwrap_or(0)));
        point = destination;
    }
    let Some(terrain) = state
        .mora
        .corridor(point, [arrival.latitude, arrival.longitude])
    else {
        return false;
    };
    floor = floor.max(f64::from(terrain));
    let step = f64::from(state.vertical_policy.level_step_feet);
    let target = ((floor / step).ceil() * step) as i32;
    if target > current - state.vertical_policy.minimum_change_feet {
        return false;
    }
    let transition =
        openatc_core::regions::region_for(&state.regions, &state.session.plan.destination)
            .transition_feet;
    let pressure = state
        .sim_weather
        .get(&state.session.plan.destination)
        .filter(|(_, at)| at.elapsed().as_secs() < 45)
        .and_then(|(weather, _)| weather["pressureHpa"].as_f64())
        .filter(|pressure| (800.0..=1100.0).contains(pressure));
    if target < transition && pressure.is_none() {
        return false;
    }
    let slots = [
        ("callsign", state.session.plan.callsign.clone()),
        (
            "altitude",
            openatc_core::altitude_text(
                f64::from(target),
                openatc_core::UnitSystem::Imperial,
                true,
            ),
        ),
    ];
    let mut text = say(
        if state.session.telemetry.vertical_speed_fpm < -200.0 {
            "automatic_continue_descent"
        } else {
            "automatic_descent"
        },
        &slots,
    );
    if target < transition
        && let Some(pressure) = pressure
    {
        let (phrase, value) = if state.session.plan.destination.starts_with('K') {
            (
                "flight_altimeter",
                format!("{:.2}", pressure / 33.863_886_666_7),
            )
        } else {
            (
                if pressure.round() < 1000.0 {
                    "flight_qnh_units"
                } else {
                    "flight_qnh"
                },
                format!("{pressure:.0}"),
            )
        };
        text.push(' ');
        text.push_str(&say(phrase, &[("pressure", value)]));
    }
    transmit_descent(state, station, text, target, Vertical::Descend, None);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    fn speed_fixture() -> (EngineState, Station) {
        let mut state = crate::test_state();
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
        state.flow.monitor.next_fix = 1;
        state.session.telemetry = openatc_core::state::Telemetry {
            position_valid: true,
            on_ground: false,
            longitude: 0.9,
            indicated_speed_knots: Some(290.0),
            ground_speed_knots: 400.0,
            heading_degrees: 90.0,
            altitude_feet: 9000.0,
            indicated_altitude_feet: Some(9000.0),
            ..Default::default()
        };
        state.session.clearance = Some(openatc_core::state::Clearance {
            acknowledged: true,
            altitude_feet: 9000,
            ..Default::default()
        });
        state.session.plan.fixes = vec![
            openatc_core::state::RouteFix {
                longitude: 0.0,
                ..Default::default()
            },
            openatc_core::state::RouteFix {
                identifier: "LIMIT".into(),
                longitude: 1.0,
                maximum_speed_knots: Some(250.0),
                path_terminator: "TF".into(),
                ..Default::default()
            },
        ];
        (
            state,
            Station {
                service: "Center".into(),
                ..Default::default()
            },
        )
    }
    #[test]
    fn published_speed_notice_is_early_once_and_uses_ias() {
        let (mut state, station) = speed_fixture();
        tick(&mut state, &station);
        let entry = state.session.transcript.last().unwrap();
        assert!(entry.text.contains("250") && entry.text.contains("LIMIT"));
        assert!(entry.pilot_reply.contains("250") && entry.pilot_reply.contains("LIMIT"));
        assert_eq!(state.flow.monitor.next_fix, 1);
        assert_eq!(
            state.session.clearance.as_ref().unwrap().altitude_feet,
            9000
        );
        state
            .session
            .transcript
            .last_mut()
            .unwrap()
            .pilot_reply
            .clear();
        state.flow.monitor.last_call = None;
        tick(&mut state, &station);
        assert_eq!(state.session.transcript.len(), 1);
        for ias in [Some(240.0), None] {
            let (mut state, station) = speed_fixture();
            state.session.telemetry.indicated_speed_knots = ias;
            assert!(!published_speed_reminder(&mut state, &station));
            assert!(state.session.transcript.is_empty());
        }
        let (mut state, station) = speed_fixture();
        state.session.telemetry.longitude = 0.1;
        assert!(!published_speed_reminder(&mut state, &station));
        state.session.telemetry.longitude = 0.9;
        state.flow.monitor.speed = Some(280.0);
        assert!(!published_speed_reminder(&mut state, &station));
        state.flow.monitor.speed = None;
        state.session.plan.fixes[1].minimum_speed_knots = Some(250.0);
        assert!(published_speed_reminder(&mut state, &station));
        assert!(
            !state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("or less")
        );
    }
    #[test]
    fn altitude_instruction_direction_follows_the_actual_altitude() {
        let fixture = |actual: f64, assigned: i32, maximum: Option<i32>, minimum: Option<i32>| {
            let mut state = crate::test_state();
            state
                .session
                .on_event(openatc_core::aircraft::AircraftEvent::Cruise);
            state.session.telemetry = openatc_core::state::Telemetry {
                position_valid: true,
                on_ground: false,
                longitude: 0.0,
                altitude_feet: actual,
                indicated_altitude_feet: Some(actual),
                ground_speed_knots: 300.0,
                heading_degrees: 90.0,
                ..Default::default()
            };
            state.session.plan.cruise_feet = 17000;
            state.session.plan.fixes = vec![
                openatc_core::state::RouteFix {
                    longitude: 0.0,
                    ..Default::default()
                },
                openatc_core::state::RouteFix {
                    identifier: "LIMIT".into(),
                    longitude: 0.2,
                    maximum_altitude_feet: maximum,
                    minimum_altitude_feet: minimum,
                    path_terminator: "TF".into(),
                    ..Default::default()
                },
                openatc_core::state::RouteFix {
                    identifier: "FAR".into(),
                    longitude: 5.0,
                    path_terminator: "TF".into(),
                    ..Default::default()
                },
            ];
            state.session.clearance = Some(openatc_core::state::Clearance {
                altitude_feet: assigned,
                acknowledged: true,
                ..Default::default()
            });
            (
                state,
                Station {
                    service: "Center".into(),
                    ..Default::default()
                },
            )
        };

        let (mut state, station) = fixture(4800.0, 5000, None, None);
        climb(&mut state, &station);
        let text = &state
            .session
            .transcript
            .last()
            .expect("climb instruction")
            .text;
        assert!(
            text.contains("climb") && !text.contains("descend"),
            "{text}"
        );

        let (mut state, station) = fixture(10000.0, 12000, Some(7000), None);
        assert!(descend(&mut state, &station));
        let text = &state
            .session
            .transcript
            .last()
            .expect("descent instruction")
            .text;
        assert!(
            text.contains("descend") && !text.contains("climb"),
            "{text}"
        );

        let (mut state, station) = fixture(7000.0, 9000, Some(7000), Some(7000));
        assert!(descend(&mut state, &station));
        let entry = state.session.transcript.last().expect("level instruction");
        assert!(
            entry.text.contains("maintain") && !entry.text.contains("descend"),
            "{}",
            entry.text
        );
        assert!(
            entry.pilot_reply.contains("Maintain"),
            "{}",
            entry.pilot_reply
        );

        let (mut state, station) = fixture(5000.0, 12000, Some(7000), None);
        assert!(
            !descend(&mut state, &station),
            "An at-or-below restriction already met must not command a descent"
        );
        assert!(state.session.transcript.is_empty());
    }

    #[test]
    fn resume_route_uses_the_active_arrival_leg_not_a_departure_fix() {
        let mut state = crate::test_state();
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
        state.session.telemetry = openatc_core::state::Telemetry {
            position_valid: true,
            on_ground: false,
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 10000.0,
            heading_degrees: 90.0,
            ground_track_degrees: Some(90.0),
            ..Default::default()
        };
        state.session.plan.fixes = vec![
            openatc_core::state::RouteFix {
                identifier: "KMCO".into(),
                longitude: -1.0,
                ..Default::default()
            },
            openatc_core::state::RouteFix {
                identifier: "JWOLF".into(),
                longitude: -0.5,
                path_terminator: "TF".into(),
                ..Default::default()
            },
            openatc_core::state::RouteFix {
                identifier: "ARRIVAL".into(),
                longitude: 1.0,
                path_terminator: "TF".into(),
                ..Default::default()
            },
        ];
        state.flow.monitor.next_fix = 1;
        state.session.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 10000,
            acknowledged: true,
            ..Default::default()
        });
        let resume = Request {
            intent: "resume_course".into(),
            text: "Resume planned route".into(),
            ..Default::default()
        };
        let station = Station {
            service: "Center".into(),
            ..Default::default()
        };
        let result = request(
            &mut state,
            &station,
            &resume,
            &openatc_core::state::SpeechTag::default(),
        )
        .expect("resume request");
        let message = result["message"].as_str().unwrap_or_default();
        assert!(message.contains("ARRIVAL"), "{message}");
        assert!(!message.contains("JWOLF"), "{message}");
    }

    #[test]
    fn a_passed_crossing_restriction_stops_limiting_the_level() {
        let mut state = crate::test_state();
        state.settings.enforce_clearance_constraints = false;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Cruise);
        state.session.telemetry = openatc_core::state::Telemetry {
            position_valid: true,
            on_ground: false,
            latitude: 0.0,
            longitude: 0.4,
            altitude_feet: 35000.0,
            indicated_altitude_feet: Some(35000.0),
            ground_speed_knots: 300.0,
            heading_degrees: 90.0,
            ground_track_degrees: Some(90.0),
            ..Default::default()
        };
        state.session.plan.cruise_feet = 39000;
        state.session.plan.fixes = vec![
            openatc_core::state::RouteFix {
                longitude: 0.0,
                ..Default::default()
            },
            openatc_core::state::RouteFix {
                identifier: "GOTHA".into(),
                longitude: 0.2,
                maximum_altitude_feet: Some(7000),
                path_terminator: "TF".into(),
                ..Default::default()
            },
            openatc_core::state::RouteFix {
                identifier: "AFTER".into(),
                longitude: 1.0,
                path_terminator: "TF".into(),
                ..Default::default()
            },
        ];
        state.session.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 39000,
            acknowledged: true,
            route: "ROUTE".into(),
            ..Default::default()
        });
        state.flow.monitor.route = "ROUTE".into();
        state.flow.monitor.next_fix = 1;
        let station = Station {
            service: "Center".into(),
            ..Default::default()
        };
        let _ = descend(&mut state, &station);
        assert!(
            state
                .session
                .transcript
                .iter()
                .all(|entry| !entry.text.contains("GOTHA")),
            "A passed restriction was still applied"
        );
        let before = state.session.transcript.len();
        tick(&mut state, &station);
        assert!(state.flow.monitor.next_fix > 1);
        assert!(!descend(&mut state, &station));
        assert_eq!(
            state.session.transcript.len(),
            before,
            "The granted 39000 level was contradicted after GOTHA"
        );
        assert!(
            state
                .session
                .transcript
                .iter()
                .all(|entry| !entry.text.contains("GOTHA") && !entry.text.contains("descend")),
            "{:?}",
            state
                .session
                .transcript
                .iter()
                .map(|entry| &entry.text)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn off_course_vector_targets_the_leg_ahead_not_the_passed_fix() {
        let mut state = crate::test_state();
        state.settings.enforce_clearance_constraints = true;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Cruise);
        state.session.telemetry = openatc_core::state::Telemetry {
            position_valid: true,
            on_ground: false,
            latitude: 0.05,
            longitude: 0.4,
            altitude_feet: 20000.0,
            height_agl_feet: 20000.0,
            indicated_altitude_feet: Some(20000.0),
            ground_speed_knots: 300.0,
            heading_degrees: 60.0,
            magnetic_heading_degrees: Some(60.0),
            ground_track_degrees: Some(60.0),
            ..Default::default()
        };
        state.session.plan.cruise_feet = 20000;
        state.session.plan.fixes = vec![
            openatc_core::state::RouteFix {
                longitude: 0.0,
                ..Default::default()
            },
            openatc_core::state::RouteFix {
                identifier: "GM439".into(),
                longitude: 0.2,
                path_terminator: "TF".into(),
                ..Default::default()
            },
            openatc_core::state::RouteFix {
                identifier: "GM614".into(),
                longitude: 0.6,
                path_terminator: "TF".into(),
                ..Default::default()
            },
        ];
        state.session.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 20000,
            acknowledged: true,
            route: "ROUTE".into(),
            ..Default::default()
        });
        state.flow.monitor.route = "ROUTE".into();
        state.flow.monitor.next_fix = 1;
        for latitude in -1..=1 {
            for longitude in 0..=2 {
                state.mora.0.insert((latitude, longitude), 1000);
            }
        }
        let station = Station {
            service: "Center".into(),
            ..Default::default()
        };
        tick(&mut state, &station);
        let text = state
            .session
            .transcript
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(text.contains("GM614"), "{text}");
        assert!(!text.contains("GM439"), "{text}");
    }

    #[test]
    fn descent_combines_due_speed_with_one_complete_readback() {
        for exact in [false, true] {
            let (mut state, mut station) = speed_fixture();
            station.name = "Test Center".into();
            state.session.clearance.as_mut().unwrap().altitude_feet = 12000;
            state.session.plan.fixes[1].maximum_altitude_feet = Some(6000);
            state.session.plan.fixes[1].minimum_speed_knots = exact.then_some(250.0);
            assert!(descend(&mut state, &station));
            assert_eq!(state.session.transcript.len(), 1);
            let entry = state.session.transcript.last().unwrap();
            for value in ["6000", "250", "LIMIT"] {
                assert!(entry.text.contains(value), "{}", entry.text);
                assert!(entry.pilot_reply.contains(value), "{}", entry.pilot_reply);
            }
            let clearance = state.session.clearance.as_ref().unwrap();
            assert_eq!(
                clearance.readback_speed.as_ref().unwrap().sequence,
                clearance.sequence
            );
            let reply = openatc_core::readback::auto_reply(&state.session).unwrap();
            assert_eq!(reply.clearance_sequence, clearance.sequence);
            assert!(openatc_core::readback::matches_clearance_speed(
                &reply.text,
                clearance
            ));
            assert!(!clearance.acknowledged);
            assert!(!published_speed_reminder(&mut state, &station));
            assert_eq!(state.session.transcript.len(), 1);
        }
    }
    #[test]
    fn descent_does_not_append_speed_when_vectoring_assigned_speed_or_missing_ias() {
        for reason in 0..3 {
            let (mut state, station) = speed_fixture();
            state.session.clearance.as_mut().unwrap().altitude_feet = 12000;
            state.session.plan.fixes[1].maximum_altitude_feet = Some(6000);
            match reason {
                0 => state.flow.monitor.heading = Some(270.0),
                1 => state.flow.monitor.speed = Some(280.0),
                _ => state.session.telemetry.indicated_speed_knots = None,
            }
            assert!(descend(&mut state, &station));
            assert!(
                state
                    .session
                    .clearance
                    .as_ref()
                    .unwrap()
                    .readback_speed
                    .is_none()
            );
            assert!(
                !state
                    .session
                    .transcript
                    .last()
                    .unwrap()
                    .pilot_reply
                    .contains("250")
            );
        }
    }
    #[test]
    fn successive_descent_readbacks_keep_direction_altitude_and_sequence() {
        let (mut state, mut station) = speed_fixture();
        station.name = "Test Center".into();
        state.session.clearance.as_mut().unwrap().altitude_feet = 12000;
        for (target, observed) in [(9000, 11000.0), (6000, 9085.0)] {
            state.session.telemetry.altitude_feet = observed;
            state.session.telemetry.indicated_altitude_feet = Some(observed);
            state.session.plan.fixes[1].maximum_altitude_feet = Some(target);
            assert!(descend(&mut state, &station));
            let reply = openatc_core::readback::auto_reply(&state.session).unwrap();
            assert_eq!(reply.altitude_feet, target);
            assert_eq!(
                reply.clearance_sequence,
                state.session.clearance.as_ref().unwrap().sequence
            );
            assert!(
                reply.text.to_lowercase().contains("descen"),
                "{}",
                reply.text
            );
            assert!(reply.text.contains(&target.to_string()), "{}", reply.text);
            assert!(!reply.text.to_lowercase().contains("maintaining"));
            state.session.clearance.as_mut().unwrap().acknowledged = true;
            state
                .session
                .transcript
                .last_mut()
                .unwrap()
                .pilot_reply
                .clear();
        }
    }
    #[test]
    fn missed_crossing_reports_only_the_failed_quantity() {
        for (altitude, ias, expected, excluded) in [
            (9000.0, Some(290.0), "speed", "altitude"),
            (9500.0, Some(240.0), "altitude", "speed"),
            (9500.0, None, "altitude", "speed"),
        ] {
            let (mut state, station) = speed_fixture();
            state.settings.enforce_clearance_constraints = true;
            state.session.telemetry.longitude = 1.01;
            state.session.telemetry.indicated_altitude_feet = Some(altitude);
            state.session.telemetry.indicated_speed_knots = ias;
            state.session.plan.fixes[1].maximum_altitude_feet = Some(9000);
            state.session.plan.fixes[1].maximum_altitude_is_flight_level = Some(false);
            tick(&mut state, &station);
            let entry = state.session.transcript.last().unwrap();
            assert!(entry.text.contains(expected), "{}", entry.text);
            assert!(!entry.text.contains(excluded), "{}", entry.text);
            assert!(entry.pilot_reply.contains(expected));
            assert!(!entry.pilot_reply.contains(excluded));
        }
    }
    #[test]
    fn waypoint_limits_use_the_published_pressure_reference() {
        let fixture = |flight_level: bool, indicated: f64| {
            let mut state = crate::test_state();
            state.settings.enforce_clearance_constraints = true;
            state
                .session
                .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
            state.session.plan.departure = "KLAX".into();
            state.session.plan.destination = "KONT".into();
            state.session.telemetry = openatc_core::state::Telemetry {
                position_valid: true,
                longitude: 1.01,
                on_ground: false,
                heading_degrees: 90.0,
                ground_speed_knots: 200.0,
                altitude_feet: 12800.0,
                indicated_altitude_feet: Some(indicated),
                pressure_altitude_feet: Some(12000.0),
                ..Default::default()
            };
            state.session.clearance = Some(openatc_core::state::Clearance {
                acknowledged: true,
                altitude_feet: 12000,
                ..Default::default()
            });
            state.session.plan.fixes = vec![
                openatc_core::state::RouteFix {
                    longitude: 0.0,
                    ..Default::default()
                },
                openatc_core::state::RouteFix {
                    identifier: "LIMIT".into(),
                    longitude: 1.0,
                    maximum_altitude_feet: Some(12000),
                    maximum_altitude_is_flight_level: Some(flight_level),
                    path_terminator: "TF".into(),
                    ..Default::default()
                },
                openatc_core::state::RouteFix {
                    longitude: 2.0,
                    path_terminator: "TF".into(),
                    ..Default::default()
                },
            ];
            state
        };
        let station = Station {
            service: "Center".into(),
            ..Default::default()
        };
        let mut standard = fixture(true, 12800.0);
        tick(&mut standard, &station);
        assert_eq!(standard.flow.monitor.next_fix, 2);
        assert!(standard.session.transcript.is_empty());
        let mut qnh = fixture(false, 12000.0);
        tick(&mut qnh, &station);
        assert!(qnh.session.transcript.is_empty());
        let mut missed = fixture(false, 12300.0);
        tick(&mut missed, &station);
        assert!(
            missed
                .session
                .transcript
                .iter()
                .any(|entry| entry.text.contains("LIMIT"))
        );
    }
    #[test]
    fn published_entry_descent_is_not_specific_to_an_ils_or_airport() {
        for (approach, runway, entry) in [
            ("I09", "09", 3000),
            ("R27Z", "27", 2500),
            ("V18", "18", 4500),
            ("L36", "36", 2000),
        ] {
            let mut state = crate::test_state();
            state
                .session
                .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
            state.session.telemetry.on_ground = false;
            state.session.telemetry.longitude = -0.15;
            state.session.telemetry.altitude_feet = 6000.0;
            state.session.plan.approach = approach.into();
            state.session.plan.arrival_runway = runway.into();
            state.session.plan.fixes = vec![
                openatc_core::state::RouteFix {
                    longitude: -1.0,
                    ..Default::default()
                },
                openatc_core::state::RouteFix {
                    identifier: "ENTRY".into(),
                    minimum_altitude_feet: Some(entry),
                    maximum_altitude_feet: Some(entry),
                    constraint_source: format!("APPCH {approach} / installed CIFP"),
                    path_terminator: "IF".into(),
                    ..Default::default()
                },
            ];
            state.session.clearance = Some(openatc_core::state::Clearance {
                altitude_feet: 6000,
                acknowledged: true,
                ..Default::default()
            });
            let station = Station {
                service: "Approach".into(),
                ..Default::default()
            };
            assert!(descend(&mut state, &station), "{approach}");
            assert_eq!(
                state.session.clearance.as_ref().unwrap().altitude_feet,
                entry
            );
            state.flow.approach_cleared = true;
            assert!(
                !descend(&mut state, &station),
                "A cleared {approach} must not reinstate its entry level"
            );
        }
    }
    #[test]
    fn approach_entry_descent_starts_early_and_preserves_terrain_and_floors() {
        let mut state = crate::test_state();
        state.session.plan.destination = "TEST".into();
        state.session.plan.arrival_runway = "09".into();
        state.session.plan.approach = "I09".into();
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Cruise);
        state.session.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 18000,
            acknowledged: true,
            ..Default::default()
        });
        state.session.telemetry.latitude = 0.0;
        state.session.telemetry.longitude = -0.98;
        state.session.telemetry.on_ground = false;
        state.session.telemetry.altitude_feet = 18000.0;
        state.session.plan.fixes = vec![
            openatc_core::state::RouteFix {
                longitude: -2.0,
                ..Default::default()
            },
            openatc_core::state::RouteFix {
                identifier: "TEST".into(),
                ..Default::default()
            },
        ];
        let mut airport = openatc_core::airport::demo_airport();
        airport.icao = "TEST".into();
        airport.navaids.push(openatc_core::airport::Navaid {
            kind: "GS".into(),
            runway: "09".into(),
            glide_angle: 3.0,
            ..Default::default()
        });
        state.arrival_profile = Some(openatc_core::arrival::ArrivalAirport {
            airport: airport.clone(),
            intercepts: vec![openatc_core::arrival::Intercept {
                procedure: "I09".into(),
                altitude_feet: 4000.0,
            }],
            ..Default::default()
        });
        state.arrival_airport = Some(airport);
        let station = Station {
            service: "Center".into(),
            ..Default::default()
        };
        assert!(
            !destination_descent(&mut state, &station),
            "Unknown terrain must stay unknown"
        );
        for latitude in -1..=1 {
            for longitude in -2..=1 {
                state.mora.0.insert((latitude, longitude), 12000);
            }
        }
        state.session.plan.fixes[1].minimum_altitude_feet = Some(15000);
        assert!(destination_descent(&mut state, &station));
        assert_eq!(
            state.session.clearance.as_ref().unwrap().altitude_feet,
            15000
        );
        state.session.plan.fixes[1].minimum_altitude_feet = None;
        state.session.telemetry.longitude = -0.8;
        state.session.clearance.as_mut().unwrap().acknowledged = true;
        assert!(destination_descent(&mut state, &station));
        assert_eq!(
            state.session.clearance.as_ref().unwrap().altitude_feet,
            12000
        );
        assert!(!state.session.clearance.as_ref().unwrap().acknowledged);
        state.session.telemetry.longitude = -0.5;
        for floor in state.mora.0.values_mut() {
            *floor = 2000;
        }
        state.session.clearance.as_mut().unwrap().acknowledged = true;
        assert!(
            !destination_descent(&mut state, &station),
            "An altitude clearance needs current pressure information"
        );
        state
            .sim_weather
            .insert("TEST".into(), (json!({"pressureHpa":1022}), Instant::now()));
        assert!(destination_descent(&mut state, &station));
        assert_eq!(
            state.session.clearance.as_ref().unwrap().altitude_feet,
            4000
        );
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("1022")
        );
    }
    #[test]
    #[ignore = "requires installed X-Plane navdata"]
    fn installed_devo3e_controller_clears_descent_before_mc588() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let mut state = crate::test_state();
        state.session.plan = openatc_core::state::FlightPlan {
            callsign: "C-GTLT".into(),
            departure: "LFMN".into(),
            destination: "LIMC".into(),
            star: "DEVO3E".into(),
            arrival_runway: "35R".into(),
            fixes: vec![
                openatc_core::state::RouteFix {
                    identifier: "ANAKI".into(),
                    latitude: 44.201_111_111,
                    longitude: 8.725_555_556,
                    ..Default::default()
                },
                openatc_core::state::RouteFix {
                    identifier: "DEVOX".into(),
                    latitude: 44.696_111_111,
                    longitude: 8.746_944_444,
                    ..Default::default()
                },
                openatc_core::state::RouteFix {
                    identifier: "LIMC".into(),
                    latitude: 45.63,
                    longitude: 8.72,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        navigation::enrich(&mut state.session.plan, &root);
        let devox = state
            .session
            .plan
            .fixes
            .iter()
            .find(|fix| fix.identifier == "DEVOX")
            .unwrap();
        assert_eq!(devox.maximum_speed_knots, Some(250.0));
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Cruise);
        state.session.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 18000,
            acknowledged: true,
            ..Default::default()
        });
        state.session.telemetry.on_ground = false;
        state.session.telemetry.latitude = 44.63;
        state.session.telemetry.longitude = 8.744;
        state.session.telemetry.altitude_feet = 18000.0;
        state.session.telemetry.height_agl_feet = 17000.0;
        state.session.telemetry.vertical_speed_fpm = 0.0;
        tick(
            &mut state,
            &Station {
                service: "Center".into(),
                ..Default::default()
            },
        );
        let clearance = state.session.clearance.as_ref().unwrap();
        assert_eq!(clearance.altitude_feet, 12000);
        assert_eq!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .matches("C-GTLT")
                .count(),
            1
        );
        assert!(!clearance.acknowledged);
        assert_eq!(state.session.phase, PhaseCode::Arrival);
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("MC588")
        );
        state.session.clearance.as_mut().unwrap().acknowledged = true;
        state
            .session
            .transcript
            .last_mut()
            .unwrap()
            .pilot_reply
            .clear();
        state.flow.monitor.last_call = None;
        state.session.telemetry.indicated_speed_knots = Some(290.0);
        state.session.telemetry.ground_speed_knots = 400.0;
        tick(
            &mut state,
            &Station {
                service: "Center".into(),
                ..Default::default()
            },
        );
        let entry = state.session.transcript.last().unwrap();
        assert!(
            entry.text.contains("DEVOX") && entry.text.contains("250"),
            "{}",
            entry.text
        );
        assert!(entry.pilot_reply.contains("DEVOX") && entry.pilot_reply.contains("250"));
        assert_eq!(
            state.session.clearance.as_ref().unwrap().altitude_feet,
            12000
        );
    }
    #[test]
    #[ignore = "requires installed X-Plane navdata"]
    fn installed_ils35ry_descends_to_intercept_before_approach_clearance() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let mut state = crate::test_state();
        state.session.plan = openatc_core::state::FlightPlan {
            callsign: "C-GTLT".into(),
            departure: "LFMN".into(),
            destination: "LIMC".into(),
            star: "DEVO3E".into(),
            approach: "I35RY".into(),
            arrival_runway: "35R".into(),
            fixes: vec![
                openatc_core::state::RouteFix {
                    identifier: "ANAKI".into(),
                    latitude: 44.201_111_111,
                    longitude: 8.725_555_556,
                    ..Default::default()
                },
                openatc_core::state::RouteFix {
                    identifier: "DEVOX".into(),
                    latitude: 44.696_111_111,
                    longitude: 8.746_944_444,
                    ..Default::default()
                },
                openatc_core::state::RouteFix {
                    identifier: "LIMC".into(),
                    latitude: 45.63,
                    longitude: 8.72,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        navigation::enrich(&mut state.session.plan, &root);
        let intercept = state
            .session
            .plan
            .fixes
            .iter()
            .position(|fix| fix.identifier == "CI3RY")
            .expect("Published ILS entry must be expanded");
        assert_eq!(
            state.session.plan.fixes[intercept].maximum_altitude_feet,
            Some(4000)
        );
        let entry = state.session.plan.fixes[intercept].clone();
        state.flow.monitor.next_fix = intercept;
        state.session.telemetry.latitude = entry.latitude - 0.15;
        state.session.telemetry.longitude = entry.longitude;
        state.session.telemetry.on_ground = false;
        state.session.telemetry.altitude_feet = 5000.0;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Approach);
        state.session.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 5000,
            acknowledged: true,
            ..Default::default()
        });
        let station = Station {
            service: "Center".into(),
            ..Default::default()
        };
        tick(&mut state, &station);
        assert_eq!(
            state.session.clearance.as_ref().unwrap().altitude_feet,
            4000
        );
        assert!(!state.session.clearance.as_ref().unwrap().acknowledged);
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("CI3RY")
        );
        state.session.clearance.as_mut().unwrap().altitude_feet = 5000;
        state.flow.approach_cleared = true;
        assert!(
            !descend(&mut state, &station),
            "A cleared approach must not reassign its entry level"
        );
    }

    #[test]
    fn descent_looks_past_unrestricted_fix_and_preserves_intermediate_floor() {
        let mut state = crate::test_state();
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
        state.session.telemetry.latitude = 44.0;
        state.session.telemetry.longitude = 8.0;
        state.session.telemetry.on_ground = false;
        state.session.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 18000,
            acknowledged: true,
            ..Default::default()
        });
        state.session.plan.fixes = vec![
            openatc_core::state::RouteFix {
                latitude: 44.0,
                longitude: 8.0,
                ..Default::default()
            },
            openatc_core::state::RouteFix {
                identifier: "BEFORE".into(),
                latitude: 44.05,
                longitude: 8.0,
                ..Default::default()
            },
            openatc_core::state::RouteFix {
                identifier: "MC588".into(),
                latitude: 44.1,
                longitude: 8.0,
                maximum_altitude_feet: Some(12000),
                minimum_altitude_feet: Some(12000),
                ..Default::default()
            },
        ];
        let station = Station {
            service: "Center".into(),
            ..Default::default()
        };
        let mut protected = state.session.plan.fixes.clone();
        protected[1].minimum_altitude_feet = Some(15000);
        state.session.plan.fixes = protected;
        assert!(!descend(&mut state, &station));
        state.session.plan.fixes[1].minimum_altitude_feet = None;
        assert!(descend(&mut state, &station));
        let clearance = state.session.clearance.as_ref().unwrap();
        assert_eq!(clearance.altitude_feet, 12000);
        assert!(!clearance.acknowledged);
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("MC588")
        );
    }
    #[test]
    fn repeated_corrections_keep_clearance_and_required_readback() {
        let mut state = crate::test_state();
        state.settings.enforce_clearance_constraints = true;
        state.settings.terminate_after_warnings = true;
        state.session.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 5000,
            acknowledged: true,
            ..Default::default()
        });
        state.flow.approach_cleared = true;
        state.flow.landing_cleared = true;
        let station = Station {
            service: "Approach".into(),
            ..Default::default()
        };
        for _ in 0..5 {
            correct(
                &mut state,
                &station,
                "Turn left heading 270.",
                Some("Left heading 270, TEST.".into()),
            );
            assert!(!state.session.clearance_cancelled);
            assert!(state.flow.approach_cleared && state.flow.landing_cleared);
            assert_eq!(
                state.session.clearance.as_ref().unwrap().altitude_feet,
                5000
            );
            assert_eq!(
                state.session.transcript.last().unwrap().pilot_reply,
                "Left heading 270, TEST."
            );
        }
        assert!(
            state.session.transcript.last().unwrap().text.len() > "Turn left heading 270.".len()
        );
    }
    #[test]
    fn pending_readback_blocks_new_instructions_and_keeps_the_reply() {
        let mut state = crate::test_state();
        let station = Station {
            name: "Test".into(),
            service: "Center".into(),
            ..Default::default()
        };
        state.session.clearance = Some(openatc_core::state::Clearance {
            acknowledged: false,
            ..Default::default()
        });
        transmit(
            &mut state,
            &station,
            "Climb to 8000 feet",
            Some("8000 feet, TEST".into()),
        );
        assert!(awaiting_reply(&mut state, &station));
        let count = state.session.transcript.len();
        assert!(awaiting_reply(&mut state, &station));
        assert_eq!(state.session.transcript.len(), count);
        state.flow.monitor.pending_since =
            Some(Instant::now().checked_sub(Duration::from_secs(46)).unwrap());
        assert!(awaiting_reply(&mut state, &station));
        assert_eq!(
            state.session.transcript.last().unwrap().pilot_reply,
            "8000 feet, TEST"
        );
        state.session.clearance.as_mut().unwrap().acknowledged = true;
        state
            .session
            .transcript
            .last_mut()
            .unwrap()
            .pilot_reply
            .clear();
        assert!(!awaiting_reply(&mut state, &station));
    }
    #[test]
    fn higher_altitudes_need_enough_track_miles_to_descend() {
        assert!(altitude_within_descent(100.0, 50.0, 28000));
        assert!(!altitude_within_descent(100.0, 50.0, 38000));
        assert!(altitude_within_descent(30.0, 50.0, 6000));
        assert!(!altitude_within_descent(30.0, 50.0, 10000));
        assert!(!altitude_within_descent(f64::NAN, 50.0, 5000));
        assert!(altitude_within_descent(20.0, 6000.0, 8000));
    }
}
