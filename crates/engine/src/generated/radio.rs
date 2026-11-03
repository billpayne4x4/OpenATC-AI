//! Generated surface exchanges use the same readback templates as the player.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
#[allow(unused_imports, clippy::wildcard_imports)]
use super::*;
use super::{Airport, Arc, Duration, EngineState, Instant, cells, external_surface_cells};
use openatc_core::{
    apply::add_transmission,
    dialogue::say,
    state::{SpeechTag, State},
    stations::Station,
};

#[derive(Clone)]
pub(super) struct Exchange {
    pub(super) speed: Option<super::sequencing::Instruction>,
    pub(super) id: u32,
    pub(super) segment: usize,
    pub(super) station: Station,
    pub(super) callsign: String,
    pub(super) messages: Vec<(bool, String)>,
    pub(super) next: usize,
    pub(super) due: Instant,
    pub(super) grant: bool,
    pub(super) tune_after: Option<Station>,
    pub(super) sector_transfer: Option<(Arc<super::flight::Prepared>, usize)>,
    /// Recovery reports cannot complete an exchange from the original route.
    pub(super) recovery_from: Option<usize>,
    pub(super) rejoin: bool,
}

pub(super) fn hold_takeoff(mut exchange: Exchange, cancelled: bool, wake: bool) -> Exchange {
    let slots = [("callsign", exchange.callsign.clone())];
    exchange.messages = vec![
        (
            true,
            say(
                match (cancelled, wake) {
                    (true, true) => "generated_takeoff_wake_cancelled",
                    (false, true) => "generated_takeoff_wake_hold",
                    (true, false) => "generated_takeoff_traffic_cancelled",
                    (false, false) => "generated_takeoff_traffic_hold",
                },
                &slots,
            ),
        ),
        (false, say("pilot_traffic_hold", &slots)),
    ];
    exchange.next = 0;
    exchange.due = Instant::now();
    exchange.grant = false;
    exchange.tune_after = None;
    exchange
}

pub(super) fn hold_landing(mut exchange: Exchange, cancelled: bool) -> Exchange {
    let slots = [("callsign", exchange.callsign.clone())];
    exchange.messages = vec![
        (
            true,
            say(
                if cancelled {
                    "generated_landing_cancelled"
                } else {
                    "arrival_continue_no_landing"
                },
                &slots,
            ),
        ),
        (false, say("generated_landing_withheld_readback", &slots)),
    ];
    exchange.next = 0;
    exchange.due = Instant::now();
    exchange.grant = false;
    exchange.tune_after = None;
    exchange
}

pub(super) fn hold_airborne(mut exchange: Exchange) -> Exchange {
    let slots = [("callsign", exchange.callsign.clone())];
    exchange.messages = vec![
        (true, say("generated_airborne_clearance_withheld", &slots)),
        (
            false,
            say("generated_airborne_clearance_withheld_readback", &slots),
        ),
    ];
    exchange.next = 0;
    exchange.due = Instant::now();
    exchange.grant = false;
    exchange.tune_after = None;
    exchange
}

pub(super) fn cancel_crossing(mut exchange: Exchange, runway: Option<&str>) -> Exchange {
    let slots = [("callsign", exchange.callsign.clone())];
    let instruction = runway.map_or_else(
        || say("generated_taxi_hold", &slots),
        |runway| {
            say(
                "generated_crossing_cancelled",
                &[
                    ("callsign", exchange.callsign.clone()),
                    ("runway", runway.to_owned()),
                ],
            )
        },
    );
    exchange.messages = vec![
        (true, instruction),
        (false, say("pilot_traffic_hold", &slots)),
    ];
    exchange.next = 0;
    exchange.grant = false;
    exchange.tune_after = None;
    exchange.due = Instant::now();
    exchange
}

/// Keep the compact flight identity in the transcript tag while the spoken text uses the
/// airline name (for example tag AAF115, text "Aigle Azur 115").
fn decorate_apron_exchange(state: &EngineState, exchange: &mut Exchange) {
    if !state.generated.apron_ids.contains(&exchange.id) {
        return;
    }
    let Some(frame) = state
        .generated
        .frames
        .iter()
        .find(|frame| frame.target.id == exchange.id)
    else {
        return;
    };
    let display = super::display_callsign(&frame.target);
    if display == frame.target.callsign {
        return;
    }
    for (_, text) in &mut exchange.messages {
        *text = text.replace(&frame.target.callsign, &display);
    }
}

/// A short ground exchange for an apron taxi aircraft that has no full flight plan yet.
fn apron_exchange(state: &mut EngineState, parked_only: bool) -> Option<Exchange> {
    // A planned departure at a stand first obtains its IFR clearance. Pushback, engine start
    // and taxi are separate Ground exchanges; granting the taxi route here bypassed them.
    let planned = {
        let mut candidates = state
            .generated
            .world
            .snapshot()
            .into_iter()
            .filter(|pose| {
                (matches!(
                    pose.stage,
                    openatc_core::traffic::motion::Stage::Parked
                        | openatc_core::traffic::motion::Stage::Pushback
                ) && (!parked_only || pose.stage == openatc_core::traffic::motion::Stage::Parked))
                    && state.generated.flights.contains_key(&pose.id)
                    && !state.generated.delivery_cleared.contains(&pose.id)
                    && !state.generated.delivery_pending.contains(&pose.id)
                    && state.generated.orders.contains_key(&pose.id)
            })
            .map(|pose| pose.id)
            .collect::<Vec<_>>();
        // Rotate the gate queue with the run seed so the first voice is not always the same
        // operator, while every aircraft still gets exactly one call.
        candidates.sort_by_key(|id| super::seeded_number(state.traffic_seed, u64::from(*id)));
        candidates.first().copied()
    }?;
    let id = planned;
    let airport = state.generated.departure_airport(id)?;
    let delivery = delivery_station(state, &airport, id)?;
    let pose = state
        .generated
        .world
        .snapshot()
        .into_iter()
        .find(|pose| pose.id == id)?;
    let frame = state
        .generated
        .frames
        .iter()
        .find(|frame| frame.target.id == id)?
        .clone();
    let runway = state
        .generated
        .orders
        .get(&id)
        .and_then(|orders| orders.last())
        .map(|(_, order)| order.hold_short_runway.clone())
        .filter(|runway| !runway.is_empty())
        .unwrap_or_else(|| state.session.plan.runway.clone());
    let destination = state
        .generated
        .destination
        .as_ref()
        .map_or_else(|| "destination".into(), |airport| airport.icao.clone());
    let (altitude, climb_hint) = state
        .generated
        .flights
        .get(&id)
        .and_then(|plan| {
            (0..plan.route.segment_count()).find_map(|index| {
                let segment = plan.route.segment(index)?;
                (segment.stage == openatc_core::traffic::motion::Stage::Climb).then(|| {
                    (
                        segment
                            .points
                            .first()
                            .map_or(5000, |point| point.altitude_feet.max(0.0) as i32),
                        plan.clearances
                            .get(&index)
                            .map(|clearance| clearance.instruction.clone()),
                    )
                })
            })
        })
        .unwrap_or((5000, None));
    let altitude = if altitude < 1000 { 5000 } else { altitude };
    let route = climb_hint
        .as_deref()
        .and_then(|instruction| {
            instruction
                .split_whitespace()
                .find(|word| word.chars().any(|c| c.is_ascii_digit()) && word.len() <= 8)
        })
        .unwrap_or("the published departure")
        .trim_matches(|character: char| !character.is_ascii_alphanumeric())
        .to_owned();
    let squawk = format!("{:04}", 1000 + (id % 7000));
    let callsign = frame.target.callsign.clone();
    let display = super::display_callsign(&frame.target);
    state.generated.log(
        openatc_settings::TrafficLog::Summary,
        format!(
            "{callsign} requests IFR clearance from {} on {} kHz",
            delivery.name, delivery.khz
        ),
    );
    let request = say(
        "pilot_request_ifr_clearance",
        &[
            ("station", delivery.name.clone()),
            ("callsign", display.clone()),
            ("dest", destination.clone()),
        ],
    );
    let clearance = format!(
        "{display}, cleared to {destination} via {route}, climb {altitude} feet, squawk {squawk}, departure runway {runway}."
    );
    let readback = say(
        "pilot_ifr_readback",
        &[
            ("destination", destination),
            ("route", route),
            ("altitude", altitude.to_string()),
            ("squawk", squawk),
            ("runway", runway),
            ("callsign", display),
        ],
    );
    state.generated.delivery_pending.insert(id);
    if let Some(frame) = state
        .generated
        .frames
        .iter_mut()
        .find(|frame| frame.target.id == id)
    {
        frame.target.com1_khz = Some(delivery.khz);
    }
    Some(Exchange {
        speed: None,
        id,
        segment: pose.segment,
        station: delivery,
        callsign,
        messages: vec![(false, request), (true, clearance), (false, readback)],
        next: 0,
        due: Instant::now(),
        grant: false,
        tune_after: None,
        sector_transfer: None,
        recovery_from: None,
        rejoin: false,
    })
}

fn station_for(
    state: &EngineState,
    airport: &Airport,
    intent: &str,
    tuned_khz: Option<i32>,
    position: [f64; 2],
) -> Option<Station> {
    if let Some(station) = openatc_core::stations::station_for_intent(
        &state.stations,
        &airport.icao,
        intent,
        tuned_khz,
        position,
    ) {
        return Some(station.clone());
    }
    // The station index is built asynchronously at startup. Until it is ready, use the
    // airport's published frequencies through the same intent mapping, never a role guess.
    airport
        .frequencies
        .iter()
        .find(|frequency| openatc_core::stations::serves(&frequency.service, intent))
        .map(|frequency| Station {
            airport: airport.icao.clone(),
            airport_name: airport.name.clone(),
            name: frequency.name.clone(),
            service: frequency.service.clone(),
            khz: frequency.khz,
            latitude: airport.reference_latitude,
            longitude: airport.reference_longitude,
            elevation_feet: airport.elevation_feet,
            ..Default::default()
        })
}

/// Resolve a station for one aircraft using its current tuning and position. Generated
/// traffic and pilot routing share `stations::station_for_intent`.
fn station_for_id(
    state: &EngineState,
    airport: &Airport,
    intent: &str,
    id: u32,
) -> Option<Station> {
    let frame = state
        .generated
        .frames
        .iter()
        .find(|frame| frame.target.id == id)?;
    station_for(
        state,
        airport,
        intent,
        frame.target.com1_khz,
        [frame.target.latitude, frame.target.longitude],
    )
}

fn delivery_station(state: &EngineState, airport: &Airport, id: u32) -> Option<Station> {
    // The pilot's clearance intent maps to Clearance/Delivery only. Ground and Tower are
    // not fallbacks: contacting them for an IFR clearance was the original defect.
    station_for_id(state, airport, "clearance", id)
}

fn ground_station(
    state: &EngineState,
    airport: &Airport,
    id: u32,
    crossing: bool,
) -> Option<Station> {
    station_for_id(
        state,
        airport,
        if crossing { "cross_runway" } else { "taxi" },
        id,
    )
}

fn tower_station(state: &EngineState, airport: &Airport, id: u32) -> Option<Station> {
    station_for_id(state, airport, "ready", id)
}

/// Log a changed reason immediately and repeat persistent waits at most every 30 seconds.
fn departure_wait(state: &mut EngineState, id: u32, reason: &str) {
    let now = Instant::now();
    if state
        .generated
        .departure_waits
        .get(&id)
        .is_some_and(|(previous, last)| {
            previous == reason && now.duration_since(*last).as_secs() < 30
        })
    {
        return;
    }
    state
        .generated
        .departure_waits
        .insert(id, (reason.to_owned(), now));
    state.generated.log(
        openatc_settings::TrafficLog::Summary,
        format!("departure {id} waiting at hold: {reason}"),
    );
}

fn departure_hold(state: &mut EngineState) {
    // The surface route ends at the departure hold; the onward flight is installed only
    // after the Ground handoff and Tower ready exchange finish.
    let holding = |pose: &openatc_core::traffic::motion::Pose| {
        // Use the same holding marker/endpoint/hold-line geometry as the player. A rolling
        // aircraft is recognized as soon as it crosses the line, not only when the motion
        // world has marked its final taxi segment complete.
        super::flight::at_departure_hold(&state.generated, pose)
            || (pose.complete
                && state
                    .generated
                    .world
                    .assigned_route(pose.id)
                    .is_some_and(|route| pose.segment + 1 == route.segment_count()))
    };
    let all = state.generated.world.snapshot();
    // Start the Ground-to-Tower handoff while the aircraft is still rolling toward the
    // hold. Waiting until the last taxi segment is complete serialized a handoff, a ready
    // call and a clearance behind every other controller exchange.
    for pose in all.iter().filter(|pose| {
        pose.stage == openatc_core::traffic::motion::Stage::TaxiOut
            && !pose.complete
            && !state.generated.tower_notified.contains(&pose.id)
            && state.generated.flights.contains_key(&pose.id)
    }) {
        let distance = state
            .generated
            .world
            .assigned_route(pose.id)
            .map(|route| {
                let end = route.end_position();
                openatc_core::arrival::distance_nm(
                    [pose.position.latitude, pose.position.longitude],
                    [end.latitude, end.longitude],
                ) * 1852.0
            })
            .unwrap_or(f64::INFINITY);
        if distance > 550.0 || !state.generated.departure_ready(pose.id) {
            continue;
        }
        let Some(airport) = state.generated.departure_airport(pose.id) else {
            continue;
        };
        let Some(tower) = tower_station(state, &airport, pose.id) else {
            continue;
        };
        let Some(ground) = ground_station(state, &airport, pose.id, false) else {
            continue;
        };
        if ground.khz == tower.khz {
            continue;
        }
        let Some(frame) = state
            .generated
            .frames
            .iter()
            .find(|frame| frame.target.id == pose.id)
        else {
            continue;
        };
        let callsign = frame.target.callsign.clone();
        let slots = [
            ("callsign", callsign.clone()),
            ("station", tower.name.clone()),
            ("frequency", format!("{:.3}", f64::from(tower.khz) / 1000.0)),
        ];
        state.generated.tower_notified.insert(pose.id);
        if let Some(frame) = state
            .generated
            .frames
            .iter_mut()
            .find(|frame| frame.target.id == pose.id)
        {
            frame.target.com1_khz = Some(ground.khz);
        }
        state.generated.log(
            openatc_settings::TrafficLog::Summary,
            format!(
                "departure {} approaching hold: Ground handoff to {} on {} kHz while taxiing, {:.0} m remaining",
                pose.id, tower.name, tower.khz, distance
            ),
        );
        state.generated.exchange = Some(Exchange {
            speed: None,
            id: pose.id,
            segment: pose.segment,
            station: ground,
            callsign,
            messages: vec![
                (true, say("flight_handoff", &slots)),
                (false, say("pilot_frequency_readback", &slots)),
            ],
            next: 0,
            due: Instant::now(),
            grant: false,
            tune_after: Some(tower),
            sector_transfer: None,
            recovery_from: None,
            rejoin: false,
        });
        return;
    }
    let at_hold: std::collections::BTreeSet<u32> = all
        .iter()
        .filter(|pose| holding(pose))
        .map(|pose| pose.id)
        .collect();
    state
        .generated
        .departure_waits
        .retain(|id, _| at_hold.contains(id));
    let candidates: Vec<_> = all
        .iter()
        .filter(|pose| at_hold.contains(&pose.id))
        .cloned()
        .collect();
    // A short, rate-limited trace of the departure gate: how many aircraft exist, how many are
    // at a holding point, and what the first one's segment looks like.
    {
        use std::sync::atomic::{AtomicU64, Ordering};
        static LAST: AtomicU64 = AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if now.saturating_sub(LAST.load(Ordering::Relaxed)) >= 5 {
            LAST.store(now, Ordering::Relaxed);
            let sample = all
                .iter()
                .find(|pose| state.generated.flights.contains_key(&pose.id))
                .map(|pose| {
                    let next = state
                        .generated
                        .flights
                        .get(&pose.id)
                        .and_then(|plan| plan.route.segment(pose.segment + 1))
                        .map(|segment| format!("{:?}", segment.stage));
                    // Compare the aircraft with the end of its own taxi route, which is the
                    // holding point the clearance should be keyed at.
                    let end = state
                        .generated
                        .flights
                        .get(&pose.id)
                        .map(|plan| plan.route.end_position());
                    let distance = end.map_or(f64::NAN, |end| {
                        openatc_core::arrival::distance_nm(
                            [pose.position.latitude, pose.position.longitude],
                            [end.latitude, end.longitude],
                        ) * 1852.0
                    });
                    format!(
                        "{} stage {:?} seg {} next {:?} complete {} pos {:.6}/{:.6} end {} dist {:.0} m",
                        pose.id,
                        pose.stage,
                        pose.segment,
                        next,
                        pose.complete,
                        pose.position.latitude,
                        pose.position.longitude,
                        end.map_or_else(
                            || "none".to_owned(),
                            |end| format!("{:.6}/{:.6}", end.latitude, end.longitude)
                        ),
                        distance
                    )
                })
                .unwrap_or_else(|| "none".to_owned());
            let taxi = all
                .iter()
                .filter(|pose| pose.stage == openatc_core::traffic::motion::Stage::TaxiOut)
                .count();
            let finished = all
                .iter()
                .filter(|pose| {
                    pose.stage == openatc_core::traffic::motion::Stage::TaxiOut && pose.complete
                })
                .count();
            state.generated.log(
                openatc_settings::TrafficLog::Summary,
                format!(
                    "departure gate {} poses, {taxi} taxiing, {finished} taxi-complete, {} holding; sample {sample}",
                    all.len(),
                    candidates.len()
                ),
            );
        }
    }
    for pose in candidates {
        // An apron aircraft without a departure plan has nothing to ask for: Tower could only
        // answer "hold short", so a runway with no traffic looked blocked.
        let planned = state.generated.flights.contains_key(&pose.id);
        if !planned && state.generated.apron_ids.contains(&pose.id) {
            departure_wait(
                state,
                pose.id,
                "apron aircraft has no complete onward flight",
            );
            continue;
        }
        if !state.generated.departure_ready(pose.id) {
            let reason = if !planned {
                "no complete onward flight"
            } else {
                "sequenced flight capacity reached"
            };
            departure_wait(state, pose.id, reason);
            continue;
        }
        let Some(airport) = state.generated.departure_airport(pose.id) else {
            departure_wait(state, pose.id, "no departure airport in taxi clearance");
            continue;
        };
        let Some(tower) = tower_station(state, &airport, pose.id).or_else(|| {
            state
                .generated
                .flights
                .get(&pose.id)
                .and_then(|flight| flight.clearances.get(&0))
                .filter(|clearance| openatc_core::stations::has_role(&clearance.station, "Tower"))
                .map(|clearance| clearance.station.clone())
        }) else {
            departure_wait(state, pose.id, "no departure controller frequency");
            continue;
        };
        let Some(ground) = ground_station(state, &airport, pose.id, false) else {
            departure_wait(state, pose.id, "no ground controller frequency");
            continue;
        };
        let runway = state
            .generated
            .orders
            .get(&pose.id)
            .and_then(|orders| orders.last())
            .map(|(_, order)| order.hold_short_runway.clone())
            .filter(|runway| !runway.is_empty())
            .or_else(|| {
                // A route that ends at a runway still names it in its resources, even when the
                // clearance was worded without a hold-short runway.
                state.generated.flights.get(&pose.id).and_then(|plan| {
                    (0..plan.route.segment_count())
                        .rev()
                        .filter_map(|index| plan.route.segment(index))
                        .flat_map(|segment| segment.resources.iter())
                        .find_map(|resource| {
                            resource
                                .strip_prefix("RUNWAY/")
                                .and_then(|rest| rest.split('/').nth(1))
                                .map(ToOwned::to_owned)
                        })
                })
            })
            .unwrap_or_default();
        if state.generated.tower_ready.contains(&pose.id) {
            if state.generated.active_flights.contains(&pose.id) {
                continue;
            }
            if super::flight::lineup_ahead(&mut state.generated, pose.id, &airport, &runway)
                .is_some()
            {
                departure_wait(state, pose.id, "runway queue ahead");
                continue;
            }
            if let Some(prepared) = state.generated.flights.get(&pose.id).cloned() {
                match super::flight::install(&mut state.generated, pose.id, &prepared) {
                    Ok(()) => {
                        state.generated.departure_waits.remove(&pose.id);
                        state.generated.log(openatc_settings::TrafficLog::Summary,
                            format!("departure {} taxi complete; onward flight installed, awaiting {} clearance on {} kHz",
                                pose.id, tower.name, tower.khz));
                    }
                    Err(error) => {
                        departure_wait(state, pose.id, &format!("onward flight refused: {error}"));
                        // Keep transiently refused plans so a later tick can retry.
                        if !state.generated.warnings.contains(&error)
                            && state.generated.warnings.len() < 128
                        {
                            state.generated.warnings.push(error);
                        }
                    }
                }
            }
            continue;
        }
        let Some(frame) = state
            .generated
            .frames
            .iter()
            .find(|frame| frame.target.id == pose.id)
        else {
            continue;
        };
        let callsign = frame.target.callsign.clone();
        if super::flight::lineup_ahead(&mut state.generated, pose.id, &airport, &runway).is_some() {
            departure_wait(state, pose.id, "runway queue ahead");
            continue;
        }
        let mut messages = Vec::new();
        let (speaker, tune_after) =
            if !state.generated.tower_notified.contains(&pose.id) && ground.khz != tower.khz {
                let slots = [
                    ("callsign", callsign.clone()),
                    ("station", tower.name.clone()),
                    ("frequency", format!("{:.3}", f64::from(tower.khz) / 1000.0)),
                ];
                messages.push((true, say("flight_handoff", &slots)));
                messages.push((false, say("pilot_frequency_readback", &slots)));
                state.generated.tower_notified.insert(pose.id);
                (ground.clone(), Some(tower.clone()))
            } else {
                state.generated.tower_ready.insert(pose.id);
                let installed = if state.generated.active_flights.contains(&pose.id) {
                    true
                } else if let Some(prepared) = state.generated.flights.get(&pose.id).cloned() {
                    super::flight::install(&mut state.generated, pose.id, &prepared).is_ok()
                } else {
                    false
                };
                if installed
                    && let Some(mut clearance) = super::flight::takeoff_exchange_for(state, pose.id)
                        .or_else(|| super::flight::ground_exchange_for(state, pose.id))
                {
                    let request = say(
                        "pilot_request_ready",
                        &[
                            ("callsign", callsign.clone()),
                            ("station", tower.name.clone()),
                        ],
                    );
                    clearance.messages.insert(0, (false, request));
                    if let Some(frame) = state
                        .generated
                        .frames
                        .iter_mut()
                        .find(|frame| frame.target.id == pose.id)
                    {
                        frame.target.com1_khz = Some(clearance.station.khz);
                    }
                    state.generated.log(
                        openatc_settings::TrafficLog::Summary,
                        format!(
                            "departure {} at hold: NICE Tower answers ready immediately on {} kHz",
                            pose.id, clearance.station.khz
                        ),
                    );
                    state.generated.exchange = Some(clearance);
                    break;
                }
                messages.push((
                    false,
                    say(
                        "pilot_request_ready",
                        &[
                            ("callsign", callsign.clone()),
                            ("station", tower.name.clone()),
                        ],
                    ),
                ));
                messages.push((
                    true,
                    say(
                        "generated_departure_standby",
                        &[("callsign", callsign.clone()), ("runway", runway.clone())],
                    ),
                ));
                messages.push((
                    false,
                    say(
                        "generated_hold_short_readback",
                        &[("callsign", callsign.clone()), ("runway", runway)],
                    ),
                ));
                (tower.clone(), Some(tower.clone()))
            };
        state.generated.log(
            openatc_settings::TrafficLog::Summary,
            format!(
                "departure {} at hold: {} on {} kHz -> {} on {} kHz; {}",
                pose.id,
                speaker.name,
                speaker.khz,
                tower.name,
                tower.khz,
                if state.generated.tower_ready.contains(&pose.id) {
                    "ready exchange"
                } else {
                    "frequency handoff"
                }
            ),
        );
        if let Some(frame) = state
            .generated
            .frames
            .iter_mut()
            .find(|frame| frame.target.id == pose.id)
        {
            frame.target.com1_khz = Some(speaker.khz);
        }
        state.generated.exchange = Some(Exchange {
            speed: None,
            id: pose.id,
            segment: pose.segment,
            station: speaker,
            callsign,
            messages,
            next: 0,
            due: Instant::now(),
            grant: false,
            tune_after,
            sector_transfer: None,
            recovery_from: None,
            rejoin: false,
        });
        break;
    }
}

fn blocked(state: &EngineState, id: u32, order: &openatc_core::state::TaxiClearance) -> bool {
    if order.crossing_runway.is_empty() {
        return false;
    }
    let reason = blocked_reason(state, id, order);
    if let Some(reason) = reason {
        use std::sync::atomic::{AtomicU64, Ordering};
        static LAST: AtomicU64 = AtomicU64::new(u64::MAX);
        let now = state.session.telemetry.simulation_seconds.unwrap_or(0.0) as u64;
        if LAST.load(Ordering::Relaxed) == u64::MAX
            || now.saturating_sub(LAST.load(Ordering::Relaxed)) >= 60
        {
            LAST.store(now, Ordering::Relaxed);
            let callsign = state
                .generated
                .frames
                .iter()
                .find(|frame| frame.target.id == id)
                .map_or_else(|| id.to_string(), |frame| frame.target.callsign.clone());
            traffic_log(
                openatc_settings::TrafficLog::Summary,
                format!(
                    "{callsign} ({id}) cannot cross runway {} via {}: {reason} at {now} s",
                    order.crossing_runway, order.airport
                ),
            );
        }
    }
    reason.is_some()
}

fn blocked_reason(
    state: &EngineState,
    id: u32,
    order: &openatc_core::state::TaxiClearance,
) -> Option<&'static str> {
    let Some(airport) = state.generated.surface_airport(&order.airport) else {
        return Some("unknown airport");
    };
    // Generated conflicts are held by the movement world; only observed traffic can add a
    // runway-occupancy conflict here.
    let targets = super::external_targets(&state.generated, &state.session.telemetry);
    let Some(policy) = &state.generated.policy else {
        return Some("no surface policy");
    };
    let Some(segment) = state.generated.world.current_segment(id) else {
        return Some("no current segment");
    };
    let distance = segment
        .points
        .windows(2)
        .map(|pair| {
            openatc_core::arrival::distance_nm(
                [pair[0].latitude, pair[0].longitude],
                [pair[1].latitude, pair[1].longitude],
            ) * 1852.0
        })
        .sum::<f64>();
    let window = distance / (segment.knots.max(1.0) * 0.514_444)
        + order.instructions.chars().count() as f64 * 2.0 / policy.radio_characters_per_second
        + policy.radio_gap_seconds * 2.0
        + policy.runway_clearance_buffer_seconds;
    let relevant = targets.iter().filter(|target| {
        target.id != id
            && (target.on_ground
                || !super::separation::parallel_other_runway(
                    airport,
                    &order.crossing_runway,
                    target.latitude,
                    target.longitude,
                    target.track_degrees,
                ))
    });
    openatc_core::holding::blocked_for_movement(airport, &order.crossing_runway, relevant, window)
}

/// Release a taxiing aircraft that was told to hold once its blocker has cleared. The hold
/// exchange is temporary; it must not require the pilot to request taxi again.
fn release_held(state: &mut EngineState) -> Option<Exchange> {
    let ids = state.generated.held.keys().copied().collect::<Vec<_>>();
    let occupied = external_surface_cells(&state.generated, &state.session.telemetry);
    for id in ids {
        let Some(pose) = state
            .generated
            .world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == id)
        else {
            state.generated.held.remove(&id);
            continue;
        };
        if !pose.awaiting_clearance {
            state.generated.held.remove(&id);
            continue;
        }
        let Some(order) = state
            .generated
            .orders
            .get(&id)
            .and_then(|orders| orders.iter().find(|(segment, _)| *segment == pose.segment))
            .map(|(_, order)| order.clone())
        else {
            state.generated.held.remove(&id);
            continue;
        };
        if !order.crossing_runway.is_empty() {
            state.generated.held.remove(&id);
            continue;
        }
        if blocked(state, id, &order)
            || !state.generated.world.reserve_surface_route(
                state.generated.world.generation(),
                id,
                pose.segment,
                &occupied,
            )
        {
            continue;
        }
        let Some(airport) = state.generated.surface_airport(&order.airport) else {
            continue;
        };
        let Some(station) = ground_station(state, &airport, id, false) else {
            continue;
        };
        let Some(frame) = state
            .generated
            .frames
            .iter()
            .find(|frame| frame.target.id == id)
        else {
            continue;
        };
        let callsign = frame.target.callsign.clone();
        let display = super::display_callsign(&frame.target);
        if let Some(frame) = state
            .generated
            .frames
            .iter_mut()
            .find(|frame| frame.target.id == id)
        {
            frame.target.com1_khz = Some(station.khz);
        }
        state.generated.held.remove(&id);
        state.generated.exchange = Some(Exchange {
            speed: None,
            id,
            segment: pose.segment,
            station,
            callsign: callsign.clone(),
            messages: vec![
                (
                    true,
                    say("generated_taxi_continue", &[("callsign", display.clone())]),
                ),
                (
                    false,
                    say("generated_taxi_continue_readback", &[("callsign", display)]),
                ),
            ],
            next: 0,
            due: Instant::now(),
            grant: true,
            tune_after: None,
            sector_transfer: None,
            recovery_from: None,
            rejoin: false,
        });
        return state.generated.exchange.take();
    }
    None
}

fn process_delivery(state: &mut EngineState, rate: f64, gap: f64) {
    let Some(mut exchange) = state.generated.delivery_exchange.take() else {
        return;
    };
    if exchange.due > Instant::now() {
        state.generated.delivery_exchange = Some(exchange);
        return;
    }
    if exchange.next == exchange.messages.len() {
        state.generated.delivery_pending.remove(&exchange.id);
        state.generated.delivery_cleared.insert(exchange.id);
        return;
    }
    let (controller, text) = &exchange.messages[exchange.next];
    let spoken_text = state.generated.spoken_text(text);
    if state.settings.hear_traffic_radio
        && state.session.telemetry.radio_power
        && let Some(radio) = state.session.receiving_radio(exchange.station.khz)
    {
        let mut tag = if *controller {
            crate::flight::tag(state, &exchange.station)
        } else {
            SpeechTag {
                position: "Traffic".into(),
                voice: crate::traffic::pilot_voice_for_callsign(state, &exchange.callsign),
                ..Default::default()
            }
        };
        tag.radio = Some(radio);
        tag.frequency = Some(exchange.station.khz);
        let spoken_callsign = state.generated.spoken_text(&exchange.callsign);
        state.station_bus.transmit(
            exchange.station.khz,
            crate::traffic_runtime::Transmission {
                session: exchange.id,
                speaker: if *controller {
                    "ATC".into()
                } else {
                    spoken_callsign
                },
                text: spoken_text.clone(),
                controller: *controller,
                tag,
                due: Instant::now(),
            },
        );
        deliver_station_transmissions(state, Duration::from_millis(2));
    }
    exchange.due =
        Instant::now() + Duration::from_secs_f64(spoken_text.chars().count() as f64 / rate + gap);
    exchange.next += 1;
    state.generated.delivery_exchange = Some(exchange);
}

fn deliver_station_transmissions(state: &mut EngineState, timeout: Duration) {
    let transmissions = if timeout.is_zero() {
        state.station_bus.drain()
    } else {
        state.station_bus.flush(timeout)
    };
    for (_, transmission) in transmissions {
        add_transmission(
            &mut state.session,
            &transmission.speaker,
            &transmission.text,
            &transmission.tag,
        );
        if let Some(last) = state.session.transcript.last_mut() {
            last.background = true;
        }
    }
}

/// Reissue the current segment for a stopped crossing or taxi aircraft. This runs after the
/// Tower queue but before the general surface queue, so a lost grant is recovered without
/// starving a departure that is already at the runway.
fn reissue_stopped_crossing(state: &mut EngineState) -> Option<Exchange> {
    if state.generated.reissued_crossings.len() > 256 {
        state
            .generated
            .reissued_crossings
            .retain(|_, last| last.elapsed() < Duration::from_secs(60));
    }
    let targets = super::external_targets(&state.generated, &state.session.telemetry);
    for pose in state.generated.world.snapshot().into_iter().filter(|pose| {
        matches!(
            pose.stage,
            openatc_core::traffic::motion::Stage::Crossing
                | openatc_core::traffic::motion::Stage::TaxiOut
                | openatc_core::traffic::motion::Stage::TaxiIn
        ) && pose.awaiting_clearance
            && !state
                .generated
                .approved_orders
                .contains(&(pose.id, pose.segment))
    }) {
        let Some(plan) = state.generated.flights.get(&pose.id).cloned() else {
            continue;
        };
        let Some(clearance) = plan.clearances.get(&pose.segment).cloned() else {
            continue;
        };
        let Some(frame) = state
            .generated
            .frames
            .iter()
            .find(|frame| frame.target.id == pose.id)
        else {
            continue;
        };
        if !super::flight::on_station(
            &state.generated,
            pose.id,
            frame.target.com1_khz,
            &clearance.station,
        ) || state
            .generated
            .crossing_blocked(pose.id, pose.segment, &targets)
        {
            continue;
        }
        let key = (pose.id, pose.segment);
        if state
            .generated
            .reissued_crossings
            .get(&key)
            .is_some_and(|last| last.elapsed() < Duration::from_secs(10))
        {
            continue;
        }
        state
            .generated
            .reissued_crossings
            .insert(key, Instant::now());
        state.generated.log(
            openatc_settings::TrafficLog::Summary,
            format!(
                "{} reissued crossing clearance at segment {}",
                frame.target.callsign, pose.segment
            ),
        );
        return Some(Exchange {
            speed: None,
            id: pose.id,
            segment: pose.segment,
            station: clearance.station.clone(),
            callsign: frame.target.callsign.clone(),
            messages: vec![(true, clearance.instruction), (false, clearance.readback)],
            next: 0,
            due: Instant::now(),
            grant: true,
            tune_after: None,
            sector_transfer: None,
            recovery_from: None,
            rejoin: false,
        });
    }
    None
}

fn advance_exchange(state: &mut EngineState, mut exchange: Exchange) -> Option<Exchange> {
    // An exchange belongs to the phase that created it. If the aircraft has already moved on,
    // drop the message instead of holding the controller queue for a stale clearance.
    let exchange_stage = state
        .generated
        .flights
        .get(&exchange.id)
        .and_then(|flight| flight.route.segment(exchange.segment))
        .map(|segment| segment.stage);
    let live_stage = state
        .generated
        .world
        .snapshot()
        .into_iter()
        .find(|pose| pose.id == exchange.id)
        .map(|pose| pose.stage);
    if matches!(
        exchange_stage,
        Some(
            openatc_core::traffic::motion::Stage::Takeoff
                | openatc_core::traffic::motion::Stage::LineUp
        )
    ) && matches!(
        live_stage,
        Some(
            openatc_core::traffic::motion::Stage::TaxiIn
                | openatc_core::traffic::motion::Stage::Rollout
                | openatc_core::traffic::motion::Stage::Turnaround
                | openatc_core::traffic::motion::Stage::Parked
        )
    ) {
        state.generated.cancelled_takeoffs.remove(&exchange.id);
        return None;
    }
    // The player's microphone only blocks a controller on the frequency being used. Ground,
    // Tower and Delivery are separate positions and may work different aircraft at once.
    if state.session.telemetry.radio_busy
        && [
            Some(state.session.telemetry.com1_khz),
            state.session.telemetry.com2_khz,
        ]
        .into_iter()
        .flatten()
        .any(|frequency| frequency == exchange.station.khz)
    {
        return Some(exchange);
    }
    decorate_apron_exchange(state, &mut exchange);
    if exchange.sector_transfer.as_ref().is_some_and(|(plan, _)| {
        !state
            .generated
            .flights
            .get(&exchange.id)
            .is_some_and(|current| Arc::ptr_eq(plan, current))
    }) {
        return None;
    }
    if exchange.due > Instant::now() {
        return Some(exchange);
    }
    if exchange.next == exchange.messages.len() {
        if exchange.segment == 1 && !exchange.grant {
            // The cancellation message has now been transmitted; the next event can be a
            // fresh clearance.
            state.generated.cancelled_takeoffs.remove(&exchange.id);
        }
        if state.generated.delivery_pending.remove(&exchange.id) {
            state.generated.delivery_cleared.insert(exchange.id);
            return None;
        }
        if let Some(speed) = exchange.speed.take() {
            if !super::sequencing::accept(
                &mut state.generated,
                &state.session.telemetry,
                exchange.id,
                &speed,
            ) {
                exchange.messages = vec![(
                    true,
                    say(
                        "generated_arrival_speed_cancelled",
                        &[("callsign", exchange.callsign.clone())],
                    ),
                )];
                exchange.next = 0;
                return Some(exchange);
            }
            return None;
        }
        if exchange.rejoin {
            if let Some(receiver) = exchange.tune_after.as_ref() {
                if let Some(frame) = state
                    .generated
                    .frames
                    .iter_mut()
                    .find(|frame| frame.target.id == exchange.id)
                {
                    frame.target.com1_khz = Some(receiver.khz);
                    state
                        .generated
                        .controllers
                        .insert(exchange.id, receiver.clone());
                }
            } else if exchange.grant
                && let Some(source) = exchange.recovery_from
            {
                if super::flight::rejoin_blocked(
                    &state.generated,
                    &state.session.telemetry,
                    exchange.id,
                ) {
                    return Some(hold_airborne(exchange));
                }
                super::flight::accept_rejoin(&mut state.generated, exchange.id, source);
            }
            return None;
        }
        if exchange.grant
            && state.generated.active_flights.contains(&exchange.id)
            && state
                .generated
                .flights
                .get(&exchange.id)
                .and_then(|plan| plan.clearances.get(&exchange.segment))
                .is_some_and(|clearance| {
                    super::separation::blocked(
                        &state.generated,
                        &state.session.telemetry,
                        exchange.id,
                        exchange.segment,
                        clearance,
                    )
                })
        {
            state
                .generated
                .reported_holds
                .insert((exchange.id, exchange.segment));
            let plan = &state.generated.flights[&exchange.id];
            if plan
                .route
                .segment(exchange.segment)
                .is_some_and(|s| s.stage == openatc_core::traffic::motion::Stage::Takeoff)
            {
                state.generated.log(
                    openatc_settings::TrafficLog::Summary,
                    format!(
                        "{} takeoff cancelled on readback: airborne separation at segment {}",
                        exchange.callsign, exchange.segment
                    ),
                );
                state.generated.exchange = Some(hold_takeoff(exchange, true, false));
            } else if plan.clearances[&exchange.segment].landing_rollout.is_some() {
                state.generated.exchange = Some(hold_landing(exchange, true));
            } else {
                state.generated.exchange = Some(hold_airborne(exchange));
            }
            return None;
        }
        if exchange.grant
            && state
                .generated
                .flights
                .get(&exchange.id)
                .and_then(|plan| plan.clearances.get(&exchange.segment))
                .is_some_and(|clearance| clearance.landing_rollout.is_some())
            && state.generated.crossing_blocked(
                exchange.id,
                exchange.segment,
                &super::external_targets(&state.generated, &state.session.telemetry),
            )
        {
            super::flight::revoke_landing(&mut state.generated, exchange.id, exchange.segment);
            return Some(hold_landing(exchange, true));
        }
        if exchange.grant
            && state.generated.crossing_blocked(
                exchange.id,
                exchange.segment,
                &super::external_targets(&state.generated, &state.session.telemetry),
            )
            && state
                .generated
                .flights
                .get(&exchange.id)
                .and_then(|flight| flight.route.segment(exchange.segment))
                .is_some_and(|segment| {
                    segment.stage == openatc_core::traffic::motion::Stage::Takeoff
                })
        {
            // Issue the hold only once per segment; keep holding without repeating.
            if state
                .generated
                .reported_holds
                .contains(&(exchange.id, exchange.segment))
            {
                return None;
            }
            state
                .generated
                .reported_holds
                .insert((exchange.id, exchange.segment));
            let wake = state
                .generated
                .departure_wake
                .get(&exchange.id)
                .is_some_and(|seconds| *seconds > 0.0);
            return Some(hold_takeoff(exchange, true, wake));
        }
        if exchange.grant
            && state.generated.active_flights.contains(&exchange.id)
            && state
                .generated
                .flights
                .get(&exchange.id)
                .and_then(|flight| flight.route.segment(exchange.segment))
                .is_some_and(|segment| {
                    segment.stage == openatc_core::traffic::motion::Stage::Crossing
                })
            && state.generated.crossing_blocked(
                exchange.id,
                exchange.segment,
                &super::external_targets(&state.generated, &state.session.telemetry),
            )
        {
            let runway = state
                .generated
                .flights
                .get(&exchange.id)
                .and_then(|flight| flight.vacated_reports.get(&(exchange.segment + 1)))
                .map(|report| report.runway.as_str());
            return Some(cancel_crossing(exchange, runway));
        }
        if let Some((plan, _)) = exchange.sector_transfer.as_ref()
            && (!state
                .generated
                .flights
                .get(&exchange.id)
                .is_some_and(|current| Arc::ptr_eq(plan, current))
                || state
                    .generated
                    .world
                    .snapshot()
                    .iter()
                    .any(|pose| pose.id == exchange.id && pose.recovering))
        {
            return None;
        }
        if let Some(receiver) = exchange.tune_after.as_ref()
            && let Some(frame) = state
                .generated
                .frames
                .iter_mut()
                .find(|frame| frame.target.id == exchange.id)
        {
            frame.target.com1_khz = Some(receiver.khz);
            state
                .generated
                .controllers
                .insert(exchange.id, receiver.clone());
            if let Some((plan, count)) = exchange.sector_transfer.as_ref() {
                if state
                    .generated
                    .flights
                    .get(&exchange.id)
                    .is_some_and(|current| Arc::ptr_eq(plan, current))
                    && state
                        .generated
                        .world
                        .snapshot()
                        .iter()
                        .any(|pose| pose.id == exchange.id && !pose.recovering)
                {
                    state
                        .generated
                        .sector_transfers
                        .insert((exchange.id, exchange.segment), *count);
                    state
                        .generated
                        .pending_sector_checkins
                        .insert(exchange.id, (plan.clone(), receiver.clone()));
                }
            } else if state.generated.active_flights.contains(&exchange.id) {
                state
                    .generated
                    .handoff_targets
                    .insert(exchange.id, exchange.segment);
                state
                    .generated
                    .pending_checkins
                    .insert((exchange.id, exchange.segment));
            }
        }
        if exchange.grant {
            state
                .generated
                .approved_orders
                .insert((exchange.id, exchange.segment));
            // A ground clearance covers the contiguous movement of its own stage. It must not
            // approve a later pushback, taxi or takeoff segment that needs its own readback.
            if !state.generated.active_flights.contains(&exchange.id)
                && let Some(route) = state
                    .generated
                    .apron_routes
                    .get(&exchange.id)
                    .cloned()
                    .or_else(|| state.generated.world.assigned_route(exchange.id))
            {
                let stage = route.segment(exchange.segment).map(|segment| segment.stage);
                if matches!(
                    stage,
                    Some(
                        openatc_core::traffic::motion::Stage::Pushback
                            | openatc_core::traffic::motion::Stage::TaxiOut
                            | openatc_core::traffic::motion::Stage::TaxiIn
                    )
                ) {
                    for index in exchange.segment..route.segment_count() {
                        let Some(segment) = route.segment(index) else {
                            break;
                        };
                        if Some(segment.stage) != stage {
                            break;
                        }
                        state.generated.approved_orders.insert((exchange.id, index));
                    }
                }
            }
            super::flight::acknowledge(&mut state.generated, exchange.id, exchange.segment);
        }
        return None;
    }
    if exchange.next == 0 {
        let role = if exchange.station.services.is_empty() {
            exchange.station.service.clone()
        } else {
            format!(
                "{} [{}]",
                exchange.station.service,
                exchange.station.services.join(" + ")
            )
        };
        state.generated.log(
            openatc_settings::TrafficLog::Summary,
            format!(
                "{} on {} ({role}) {} kHz",
                exchange.callsign, exchange.station.name, exchange.station.khz
            ),
        );
    }
    let (controller, text) = &exchange.messages[exchange.next];
    let spoken_text = state.generated.spoken_text(text);
    if state.settings.hear_traffic_radio
        && state.session.telemetry.radio_power
        && let Some(radio) = state.session.receiving_radio(exchange.station.khz)
    {
        let mut tag = if *controller {
            crate::flight::tag(state, &exchange.station)
        } else {
            SpeechTag {
                position: "Traffic".into(),
                voice: crate::traffic::pilot_voice_for_callsign(state, &exchange.callsign),
                ..Default::default()
            }
        };
        tag.radio = Some(radio);
        tag.frequency = Some(exchange.station.khz);
        let spoken_callsign = state.generated.spoken_text(&exchange.callsign);
        state.station_bus.transmit(
            exchange.station.khz,
            crate::traffic_runtime::Transmission {
                session: exchange.id,
                speaker: if *controller {
                    "ATC".into()
                } else {
                    spoken_callsign
                },
                text: spoken_text.clone(),
                controller: *controller,
                tag,
                due: Instant::now(),
            },
        );
        deliver_station_transmissions(state, Duration::from_millis(2));
    }
    let (rate, gap) = state
        .generated
        .policy
        .as_ref()
        .map_or((12.0, 0.3), |policy| {
            (policy.radio_characters_per_second, policy.radio_gap_seconds)
        });
    exchange.due =
        Instant::now() + Duration::from_secs_f64(spoken_text.chars().count() as f64 / rate + gap);
    exchange.next += 1;
    return Some(exchange);
}
pub fn tick(state: &mut EngineState) {
    deliver_station_transmissions(state, Duration::ZERO);
    let sessions = state
        .generated
        .sessions
        .sessions()
        .filter(|(id, session)| **id != 0 && session.frequency > 0)
        .map(|(&id, session)| (id, session.frequency, session.phase))
        .collect::<Vec<_>>();
    let now = state.session.telemetry.simulation_seconds.unwrap_or(0.0);
    state.station_bus.sync_sessions(sessions, now);
    let triggers = state.station_bus.take_triggers();
    for trigger in &triggers {
        state
            .generated
            .sessions
            .phase(trigger.session, trigger.phase, trigger.at);
        state
            .generated
            .sessions
            .progress(trigger.session, trigger.at);
    }
    if !triggers.is_empty() {
        state.generated.log(
            openatc_settings::TrafficLog::Detail,
            format!(
                "station triggers: {}",
                triggers
                    .iter()
                    .map(|trigger| format!(
                        "{}:{:?}@{}",
                        trigger.session, trigger.phase, trigger.frequency
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    super::flight::synchronise_recoveries(&mut state.generated);
    super::ctaf::announce(state);
    let held = if !state.settings.generated_traffic {
        Some("generated traffic disabled")
    } else if state.generated.epoch != Some(state.session_generation)
        && state.generated.flights.is_empty()
    {
        Some("scene epoch changed")
    } else if state.session.telemetry.paused {
        Some("simulator paused")
    } else {
        None
    };
    if let Some(reason) = held {
        // A controller that never gets a turn looks exactly like traffic with nothing to say.
        // Name the reason instead of returning silently.
        use std::sync::atomic::{AtomicU64, Ordering};
        static LAST: AtomicU64 = AtomicU64::new(u64::MAX);
        let now = state.session.telemetry.simulation_seconds.unwrap_or(0.0) as u64;
        if LAST.load(Ordering::Relaxed) == u64::MAX
            || now.saturating_sub(LAST.load(Ordering::Relaxed)) >= 30
        {
            LAST.store(now, Ordering::Relaxed);
            state.generated.log(
                openatc_settings::TrafficLog::Summary,
                format!(
                    "controller held at {now} s ({reason}); {} poses, {} flights",
                    state.generated.world.snapshot().len(),
                    state.generated.flights.len()
                ),
            );
        }
        return;
    }
    let Some((rate, gap)) = state
        .generated
        .policy
        .as_ref()
        .map(|policy| (policy.radio_characters_per_second, policy.radio_gap_seconds))
    else {
        return;
    };
    process_delivery(state, rate, gap);
    if state.generated.delivery_exchange.is_none()
        && state.generated.exchange.is_some()
        && let Some(exchange) = apron_exchange(state, true)
    {
        state.generated.delivery_exchange = Some(exchange);
    }

    if state.generated.exchange.is_none() {
        // Ground traffic talks first: the runway queue, then taxiing/pushback, then gate
        // clearances, then airborne arrivals and approach sequencing.
        departure_hold(state);
    }
    if state.generated.exchange.is_none() {
        state.generated.exchange = release_held(state);
    }
    // An aircraft already handed to Tower gets its line-up or takeoff clearance before the
    // next Ground taxi readback. Otherwise a busy taxi queue can starve the runway departure.
    if state.generated.exchange.is_none() {
        state.generated.exchange = super::flight::ground_exchange(state);
    }
    if state.generated.exchange.is_none() {
        state.generated.exchange = reissue_stopped_crossing(state);
    }
    if state.generated.exchange.is_none() {
        let mut poses = state
            .generated
            .world
            .snapshot()
            .into_iter()
            .filter(|pose| {
                pose.awaiting_clearance
                    && state
                        .generated
                        .approved_orders
                        .contains(&(pose.id, pose.segment))
            })
            .collect::<Vec<_>>();
        // Keep taxiing traffic talking before gate pushbacks.
        poses.sort_by_key(|pose| match pose.stage {
            openatc_core::traffic::motion::Stage::TaxiOut
            | openatc_core::traffic::motion::Stage::TaxiIn
            | openatc_core::traffic::motion::Stage::Crossing => 0,
            openatc_core::traffic::motion::Stage::Pushback => 1,
            _ => 2,
        });
        for pose in poses {
            if !state.generated.departure_ready(pose.id) {
                continue;
            }
            let Some(order) = state
                .generated
                .orders
                .get(&pose.id)
                .and_then(|orders| orders.iter().find(|(segment, _)| *segment == pose.segment))
                .map(|(_, order)| order.clone())
            else {
                continue;
            };
            if order.crossing_runway.is_empty() || !blocked(state, pose.id, &order) {
                continue;
            }
            let Some(airport) = state.generated.surface_airport(&order.airport) else {
                continue;
            };
            let Some(station) = ground_station(state, airport, pose.id, true) else {
                continue;
            };
            let Some(frame) = state
                .generated
                .frames
                .iter()
                .find(|frame| frame.target.id == pose.id)
            else {
                continue;
            };
            let callsign = frame.target.callsign.clone();
            let occupied = cells(
                airport,
                openatc_core::airport::airport_point(
                    airport,
                    pose.position.latitude,
                    pose.position.longitude,
                ),
                frame.target.wingspan_meters.unwrap_or(80.0) / 2.0,
                state
                    .generated
                    .policy
                    .as_ref()
                    .unwrap()
                    .conflict_cell_meters,
            );
            if !state.generated.world.cancel_reservation(
                state.generated.world.generation(),
                pose.id,
                &occupied,
            ) {
                continue;
            }
            state
                .generated
                .approved_orders
                .remove(&(pose.id, pose.segment));
            state.generated.exchange = Some(Exchange {
                speed: None,
                id: pose.id,
                segment: pose.segment,
                station,
                callsign: callsign.clone(),
                messages: vec![
                    (
                        true,
                        say(
                            "generated_crossing_cancelled",
                            &[
                                ("callsign", callsign.clone()),
                                ("runway", order.crossing_runway),
                            ],
                        ),
                    ),
                    (false, say("pilot_traffic_hold", &[("callsign", callsign)])),
                ],
                next: 0,
                due: Instant::now(),
                grant: false,
                tune_after: None,
                sector_transfer: None,
                recovery_from: None,
                rejoin: false,
            });
            break;
        }
    }
    if state.generated.exchange.is_none() {
        for pose in state
            .generated
            .world
            .snapshot()
            .into_iter()
            .filter(|pose| pose.awaiting_clearance)
        {
            let ready = state.generated.departure_ready(pose.id);
            let approved = state
                .generated
                .approved_orders
                .contains(&(pose.id, pose.segment));
            let order = state
                .generated
                .orders
                .get(&pose.id)
                .and_then(|orders| orders.iter().find(|(index, _)| *index == pose.segment));
            let airport =
                order.and_then(|(_, order)| state.generated.surface_airport(&order.airport));
            let station_present = airport
                .as_ref()
                .and_then(|a| ground_station(state, a, pose.id, false))
                .is_some();
            let frame_present = state
                .generated
                .frames
                .iter()
                .any(|frame| frame.target.id == pose.id);
            if !(ready && !approved && order.is_some() && station_present && frame_present) {
                use std::sync::atomic::{AtomicU64, Ordering};
                static LAST: AtomicU64 = AtomicU64::new(u64::MAX);
                let now = state.session.telemetry.simulation_seconds.unwrap_or(0.0) as u64;
                if LAST.load(Ordering::Relaxed) == u64::MAX
                    || now.saturating_sub(LAST.load(Ordering::Relaxed)) >= 60
                {
                    LAST.store(now, Ordering::Relaxed);
                    let sequenced = state
                        .generated
                        .world
                        .snapshot()
                        .iter()
                        .filter(|pose| {
                            state.generated.active_flights.contains(&pose.id)
                                && !pose.complete
                                && !matches!(
                                    pose.stage,
                                    openatc_core::traffic::motion::Stage::Parked
                                        | openatc_core::traffic::motion::Stage::Turnaround
                                )
                        })
                        .count();
                    state.generated.log(
                        openatc_settings::TrafficLog::Detail,
                        format!(
                        "{} at segment {} {:?} cannot request its clearance (ready {ready}, approved {approved}, order {}, airport {}, station {station_present}, frame {frame_present}, plan {}, active {}, complete-flights {}, sequenced {sequenced}) at {now} s",
                        pose.id,
                        pose.segment,
                        pose.stage,
                        order.is_some(),
                        airport.is_some(),
                        state.generated.flights.contains_key(&pose.id),
                        state.generated.active_flights.contains(&pose.id),
                        state.generated.require_complete_flights
                        ),
                    );
                }
            }
            if !ready {
                continue;
            }
            if state
                .generated
                .approved_orders
                .contains(&(pose.id, pose.segment))
            {
                continue;
            }
            if pose.stage == openatc_core::traffic::motion::Stage::Pushback {
                if state.generated.require_complete_flights
                    && !state.generated.delivery_cleared.contains(&pose.id)
                {
                    continue;
                }
                let Some(airport) = state.generated.departure_airport(pose.id) else {
                    continue;
                };
                let Some(station) = ground_station(state, &airport, pose.id, false) else {
                    continue;
                };
                let Some(frame) = state
                    .generated
                    .frames
                    .iter()
                    .find(|frame| frame.target.id == pose.id)
                else {
                    continue;
                };
                let callsign = frame.target.callsign.clone();
                let occupied = external_surface_cells(&state.generated, &state.session.telemetry);
                if !state.generated.world.reserve_surface_route(
                    state.generated.world.generation(),
                    pose.id,
                    pose.segment,
                    &occupied,
                ) {
                    continue;
                }
                let slots = [
                    ("callsign", callsign.clone()),
                    ("station", station.name.clone()),
                ];
                if let Some(frame) = state
                    .generated
                    .frames
                    .iter_mut()
                    .find(|frame| frame.target.id == pose.id)
                {
                    frame.target.com1_khz = Some(station.khz);
                }
                state.generated.exchange = Some(Exchange {
                    speed: None,
                    id: pose.id,
                    segment: pose.segment,
                    station,
                    callsign,
                    messages: vec![
                        (false, say("pilot_request_start_pushback", &slots)),
                        (true, say("generated_start_pushback_approved", &slots)),
                        (false, say("pilot_start_pushback_ack", &slots)),
                    ],
                    next: 0,
                    due: Instant::now(),
                    grant: true,
                    tune_after: None,
                    sector_transfer: None,
                    recovery_from: None,
                    rejoin: false,
                });
                break;
            }
            let Some(order) = state
                .generated
                .orders
                .get(&pose.id)
                .and_then(|orders| orders.iter().find(|(segment, _)| *segment == pose.segment))
                .map(|(_, order)| order.clone())
            else {
                continue;
            };
            if blocked(state, pose.id, &order) {
                continue;
            }
            let Some(airport) = state.generated.surface_airport(&order.airport) else {
                continue;
            };
            let Some(station) = ground_station(state, airport, pose.id, false) else {
                continue;
            };
            let Some(frame) = state
                .generated
                .frames
                .iter()
                .find(|frame| frame.target.id == pose.id)
            else {
                continue;
            };
            let callsign = frame.target.callsign.clone();
            let mut actor = State::default();
            actor.plan.callsign.clone_from(&callsign);
            actor.taxi_clearance = order.clone();
            actor.taxi_clearance.pending_readback = true;
            add_transmission(
                &mut actor,
                "ATC",
                &order.instructions,
                &SpeechTag {
                    position: station.name.clone(),
                    ..Default::default()
                },
            );
            let Some(reply) = openatc_core::readback::auto_reply(&actor) else {
                continue;
            };
            let occupied = external_surface_cells(&state.generated, &state.session.telemetry);
            if !state.generated.world.reserve_surface_route(
                state.generated.world.generation(),
                pose.id,
                pose.segment,
                &occupied,
            ) {
                let blocker_id = state.generated.world.surface_blocker(pose.id);
                if order.crossing_runway.is_empty() {
                    state
                        .generated
                        .held
                        .insert(pose.id, blocker_id.unwrap_or(0));
                }
                let first = state
                    .generated
                    .reported_holds
                    .insert((pose.id, pose.segment));
                if first {
                    traffic_log(
                        openatc_settings::TrafficLog::Summary,
                        format!(
                            "{callsign} cannot be cleared at segment {} {:?}{}; blocker {:?}",
                            pose.segment,
                            pose.stage,
                            if order.crossing_runway.is_empty() {
                                String::new()
                            } else {
                                format!(" before runway {}", order.crossing_runway)
                            },
                            state.generated.world.surface_blocker(pose.id)
                        ),
                    );
                }
                if order.crossing_runway.is_empty() && first {
                    let blocker = blocker_id
                        .and_then(|id| {
                            state
                                .generated
                                .frames
                                .iter()
                                .find(|frame| frame.target.id == id)
                        })
                        .map(|frame| frame.target.callsign.clone());
                    let text = if let Some(traffic) = blocker {
                        say(
                            "generated_taxi_hold_traffic",
                            &[("callsign", callsign.clone()), ("traffic", traffic)],
                        )
                    } else {
                        say("generated_taxi_hold", &[("callsign", callsign.clone())])
                    };
                    state.generated.exchange = Some(Exchange {
                        speed: None,
                        id: pose.id,
                        segment: pose.segment,
                        station,
                        callsign: callsign.clone(),
                        messages: vec![
                            (true, text),
                            (false, say("pilot_traffic_hold", &[("callsign", callsign)])),
                        ],
                        next: 0,
                        due: Instant::now(),
                        grant: false,
                        tune_after: None,
                        sector_transfer: None,
                        recovery_from: None,
                        rejoin: false,
                    });
                    break;
                }
                continue;
            }
            let mut messages = Vec::new();
            if let Some((segment, previous)) =
                state.generated.orders.get(&pose.id).and_then(|orders| {
                    orders.iter().rev().find(|(segment, order)| {
                        *segment < pose.segment && !order.crossing_runway.is_empty()
                    })
                })
                && !state
                    .generated
                    .reported_crossings
                    .contains(&(pose.id, *segment))
            {
                messages.push((
                    false,
                    say(
                        "pilot_runway_vacated",
                        &[
                            ("callsign", callsign.clone()),
                            ("runway", previous.crossing_runway.clone()),
                        ],
                    ),
                ));
                state
                    .generated
                    .reported_crossings
                    .insert((pose.id, *segment));
            }
            if order.crossing_runway.is_empty()
                && state.generated.orders.get(&pose.id).is_some_and(|orders| {
                    orders
                        .first()
                        .is_some_and(|(segment, _)| *segment == pose.segment)
                })
            {
                messages.push((
                    false,
                    say(
                        "pilot_request_taxi",
                        &[
                            ("callsign", callsign.clone()),
                            ("station", station.name.clone()),
                        ],
                    ),
                ));
            }
            messages.push((
                true,
                say(
                    "generated_surface_instruction",
                    &[
                        ("callsign", callsign.clone()),
                        ("instructions", order.instructions),
                    ],
                ),
            ));
            messages.push((false, reply.text));
            for frame in &mut state.generated.frames {
                if frame.target.id == pose.id {
                    frame.target.com1_khz = Some(station.khz);
                }
            }
            state.generated.exchange = Some(Exchange {
                speed: None,
                id: pose.id,
                segment: pose.segment,
                station,
                callsign,
                messages,
                next: 0,
                due: Instant::now(),
                grant: true,
                tune_after: None,
                sector_transfer: None,
                recovery_from: None,
                rejoin: false,
            });
            break;
        }
    }
    if state.generated.exchange.is_none() {
        state.generated.exchange = apron_exchange(state, false);
    }
    if state.generated.exchange.is_none() {
        super::flight::prepare_limit_rejoins(&mut state.generated);
        state.generated.exchange =
            super::flight::exchange(state).or_else(|| super::sequencing::exchange(state));
    }

    let Some(exchange) = state.generated.exchange.take() else {
        return;
    };
    if let Some(exchange) = advance_exchange(state, exchange) {
        state.generated.exchange = Some(exchange);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::traffic::motion::{Route, Segment, Stage, Waypoint};

    #[test]
    fn gate_departure_requests_ifr_before_pushback() {
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.generated.airport = Some(Arc::new(Airport {
            icao: "TEST".into(),
            name: "Test".into(),
            ..Default::default()
        }));
        state.generated.destination = Some(Arc::new(Airport {
            icao: "DEST".into(),
            name: "Destination".into(),
            ..Default::default()
        }));
        state.stations.push(Station {
            airport: "TEST".into(),
            name: "Test Delivery".into(),
            service: "Delivery".into(),
            khz: 121_900,
            ..Default::default()
        });
        state.stations.push(Station {
            airport: "TEST".into(),
            name: "Test Ground".into(),
            service: "Ground".into(),
            khz: 121_700,
            ..Default::default()
        });
        let point = Waypoint {
            latitude: 43.0,
            longitude: 7.0,
            altitude_feet: 10.0,
        };
        let route = Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Parked,
                        points: vec![point],
                        knots: 0.0,
                        resources: vec!["STAND/1".into()],
                        dwell_seconds: 1.0,
                    },
                    Segment {
                        stage: Stage::Pushback,
                        points: vec![
                            point,
                            Waypoint {
                                latitude: 43.0001,
                                ..point
                            },
                        ],
                        knots: 3.0,
                        resources: vec!["PUSHBACK".into()],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::TaxiOut,
                        points: vec![
                            Waypoint {
                                latitude: 43.0001,
                                ..point
                            },
                            Waypoint {
                                latitude: 43.0002,
                                ..point
                            },
                        ],
                        knots: 10.0,
                        resources: vec!["TAXI".into()],
                        dwell_seconds: 0.0,
                    },
                ],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        let clearances = (0..route.segment_count())
            .map(|index| {
                (
                    index,
                    crate::generated::flight::Clearance {
                        station: state.stations[1].clone(),
                        transfers: Vec::new(),
                        coordinated_recovery: Vec::new(),
                        instruction: "taxi".into(),
                        readback: "taxi".into(),
                        initial_climb: None,
                        taxi_through: None,
                        airborne_through: None,
                        landing_rollout: None,
                        missed: None,
                        amended_route: None,
                        holding: None,
                    },
                )
            })
            .collect();
        state.generated.flights.insert(
            1,
            Arc::new(crate::generated::flight::Prepared {
                spacing_floor: None,
                route: route.clone(),
                airspeeds: BTreeMap::new(),
                recovery: BTreeMap::new(),
                clearances,
                vacated_reports: BTreeMap::new(),
                amendment: None,
            }),
        );
        state.generated.orders.insert(
            1,
            vec![(
                2,
                openatc_core::state::TaxiClearance {
                    airport: "TEST".into(),
                    hold_short_runway: "09".into(),
                    ..Default::default()
                },
            )],
        );
        state.generated.world.insert(0, 1, route);
        state.generated.frames.push(GeneratedTraffic {
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
            target: TrafficTarget {
                id: 1,
                callsign: "TEST1".into(),
                on_ground: true,
                ..Default::default()
            },
            model: Arc::new(openatc_core::traffic::csl::Model {
                id: "TEST".into(),
                objects: vec![],
                vertical_offset: None,
                matches: vec![],
            }),
            animation: Animation::default(),
        });
        let exchange =
            apron_exchange(&mut state, true).expect("gate departure should call Delivery");
        assert_eq!(exchange.station.service, "Delivery");
        assert_eq!(exchange.messages.len(), 3);
        assert!(exchange.messages[0].0 == false);
        assert!(exchange.messages[1].1.contains("cleared to DEST"));
        assert!(exchange.messages[2].1.contains("TEST1"));
        assert!(state.generated.delivery_pending.contains(&1));
        assert!(!state.generated.delivery_cleared.contains(&1));
        state.generated.epoch = Some(state.session_generation);
        state.generated.policy = Some(
            crate::generated::parse_operations(include_str!("../../../../traffic/operations.toml"))
                .unwrap(),
        );
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.radio_power = true;
        state.session.telemetry.traffic = Some(Vec::new());
        state.session.telemetry.simulation_seconds = Some(0.0);
        state.settings.hear_traffic_radio = false;
        state.generated.exchange = Some(exchange);
        for _ in 0..12 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::tick(&mut state);
        }
        assert!(state.generated.delivery_cleared.contains(&1));
        assert!(!state.generated.delivery_pending.contains(&1));
    }
    #[test]
    fn held_taxi_aircraft_is_released_to_follow_when_the_blocker_stays_ahead() {
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.generated.airport = Some(Arc::new(Airport {
            icao: "TEST".into(),
            name: "Test".into(),
            ..Default::default()
        }));
        state.stations.push(Station {
            airport: "TEST".into(),
            name: "Test Ground".into(),
            service: "Ground".into(),
            khz: 121_700,
            ..Default::default()
        });
        state.generated.policy = Some(
            crate::generated::parse_operations(include_str!("../../../../traffic/operations.toml"))
                .unwrap(),
        );
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.radio_power = true;
        state.session.telemetry.traffic = Some(Vec::new());
        state.session.telemetry.simulation_seconds = Some(0.0);
        let point = Waypoint {
            latitude: 43.0,
            longitude: 7.0,
            altitude_feet: 10.0,
        };
        let route = |resources: [&str; 2], offset: f64| {
            Arc::new(
                Route::compile(
                    vec![
                        Segment {
                            stage: Stage::TaxiOut,
                            points: vec![
                                Waypoint {
                                    latitude: point.latitude + offset,
                                    ..point
                                },
                                Waypoint {
                                    latitude: point.latitude + offset,
                                    longitude: point.longitude + 0.001,
                                    ..point
                                },
                            ],
                            knots: 10.0,
                            resources: vec![resources[0].into()],
                            dwell_seconds: 0.0,
                        },
                        Segment {
                            stage: Stage::TaxiOut,
                            points: vec![
                                Waypoint {
                                    latitude: point.latitude + offset,
                                    longitude: point.longitude + 0.001,
                                    ..point
                                },
                                Waypoint {
                                    latitude: point.latitude,
                                    longitude: point.longitude + 0.002,
                                    ..point
                                },
                            ],
                            knots: 10.0,
                            resources: vec![resources[1].into()],
                            dwell_seconds: 0.0,
                        },
                    ],
                    1000.0,
                    1000.0,
                )
                .unwrap(),
            )
        };
        let follower = route(["GATE/2", "TAXI"], 0.001);
        let blocker = route(["GATE/1", "TAXI"], 0.0);
        let generation = state.generated.world.generation();
        assert!(state.generated.world.insert(generation, 2, follower));
        assert!(state.generated.world.insert(generation, 1, blocker));
        assert!(
            state
                .generated
                .world
                .reserve_surface_route(generation, 1, 0, &BTreeSet::new())
        );
        state.generated.orders.insert(
            2,
            vec![(
                0,
                openatc_core::state::TaxiClearance {
                    airport: "TEST".into(),
                    instructions: "Taxi via TAXI".into(),
                    ..Default::default()
                },
            )],
        );
        state.generated.held.insert(2, 1);
        state.generated.frames.push(GeneratedTraffic {
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
            target: TrafficTarget {
                id: 2,
                callsign: "TEST2".into(),
                on_ground: true,
                ..Default::default()
            },
            model: Arc::new(openatc_core::traffic::csl::Model {
                id: "TEST".into(),
                objects: vec![],
                vertical_offset: None,
                matches: vec![],
            }),
            animation: Animation::default(),
        });
        let exchange = release_held(&mut state).expect("a held aircraft should be released");
        assert!(exchange.grant);
        assert!(exchange.messages[0].1.contains("continue"));
        assert!(!state.generated.held.contains_key(&2));
        assert!(!state.generated.world.owned_resources(2).contains("TAXI"));
    }
    fn station_test_state(service: &str, services: &[&str], khz: i32, name: &str) -> EngineState {
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.generated.airport = Some(Arc::new(Airport {
            icao: "TEST".into(),
            name: "Test".into(),
            ..Default::default()
        }));
        state.stations.push(Station {
            airport: "TEST".into(),
            name: name.into(),
            service: service.into(),
            services: services.iter().map(|value| (*value).into()).collect(),
            khz,
            latitude: 43.0,
            longitude: 7.0,
            ..Default::default()
        });
        state.generated.frames.push(GeneratedTraffic {
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
            target: TrafficTarget {
                id: 7,
                callsign: "TEST7".into(),
                latitude: 43.0,
                longitude: 7.0,
                on_ground: true,
                ..Default::default()
            },
            model: Arc::new(openatc_core::traffic::csl::Model {
                id: "TEST".into(),
                objects: vec![],
                vertical_offset: None,
                matches: vec![],
            }),
            animation: Animation::default(),
        });
        state
    }

    #[test]
    fn generated_ground_calls_use_a_combined_ground_duty_on_the_tower_station() {
        let state = station_test_state("Tower", &["Ground"], 118_700, "Test Tower");
        let airport = state.generated.airport.as_ref().unwrap();
        let station = ground_station(&state, airport, 7, false).unwrap();
        assert_eq!(station.khz, 118_700);
        assert_eq!(station.name, "Test Tower");
    }

    #[test]
    fn generated_ifr_prefers_delivery_over_the_ground_fallback() {
        let mut state = station_test_state("Ground", &[], 121_700, "Test Ground");
        state.stations.push(Station {
            airport: "TEST".into(),
            name: "Test Delivery".into(),
            service: "Delivery".into(),
            khz: 121_900,
            latitude: 43.0,
            longitude: 7.0,
            ..Default::default()
        });
        openatc_core::stations::assign_airport_capabilities(&mut state.stations);
        let airport = state.generated.airport.as_ref().unwrap();
        let station = delivery_station(&state, airport, 7).unwrap();
        assert_eq!(station.khz, 121_900);
        assert_eq!(station.service, "Delivery");
    }

    #[test]
    fn generated_ifr_uses_the_pilots_ground_fallback_only_without_delivery() {
        let mut state = station_test_state("Ground", &[], 121_700, "Test Ground");
        openatc_core::stations::assign_airport_capabilities(&mut state.stations);
        let airport = state.generated.airport.as_ref().unwrap();
        let station = delivery_station(&state, airport, 7).unwrap();
        assert_eq!(station.khz, 121_700);
        assert!(
            openatc_core::stations::station_serves(&station, "clearance"),
            "the inferred Ground clearance duty must come from the shared capability map"
        );
    }

    #[test]
    fn generated_ground_calls_prefer_the_aircraft_tuned_frequency() {
        let mut state = station_test_state("Ground", &[], 121_700, "Test Ground North");
        state.stations.push(Station {
            airport: "TEST".into(),
            name: "Test Ground South".into(),
            service: "Ground".into(),
            khz: 121_900,
            latitude: 43.5,
            longitude: 7.5,
            ..Default::default()
        });
        if let Some(frame) = state
            .generated
            .frames
            .iter_mut()
            .find(|frame| frame.target.id == 7)
        {
            frame.target.com1_khz = Some(121_900);
        }
        let airport = state.generated.airport.as_ref().unwrap();
        let station = ground_station(&state, airport, 7, false).unwrap();
        assert_eq!(station.khz, 121_900);
    }
}
