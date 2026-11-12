//! Published-station handoffs and explicit arrival permissions.

#![allow(clippy::default_trait_access)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use crate::EngineState;
use openatc_core::aircraft::AircraftBehaviour;
use openatc_core::{
    apply::{TakeoffDecision, add_transmission},
    dialogue::say,
    ops::distance_nm,
    state::{PhaseCode, Request, SpeechTag, TakeoffState},
    stations::{Station, nearby, tuned},
};
use serde_json::{Value, json};

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct FlightFlow {
    pub discipline: crate::discipline::Monitor,
    pub monitor: crate::instructions::Monitor,
    arrival_requested: bool,
    approach_query_sent: bool,
    checked_in_station: String,
    landing_hold: bool,
    landing_retry: Option<std::time::Instant>,
    candidate: String,
    evidence: u8,
    pending_station: String,
    pub(crate) approach_cleared: bool,
    pub(crate) announce_approach: bool,
    pub(crate) landing_cleared: bool,
    pub(crate) takeoff_cleared: bool,
    pub(crate) missed_approach: bool,
    pub(crate) vacate_reported: bool,
    pub(crate) vacate_ground_khz: Option<i32>,
    pub(crate) wake_warned: bool,
    warning: openatc_core::compliance::WarningCount,
    clearance_sequence: u32,
    last_correction: Option<std::time::Instant>,
    /// When the player first reached the departure hold for the current route.
    pub(crate) takeoff_hold_since: Option<std::time::Instant>,
    takeoff_hold_route: u32,
}

pub fn station_key(station: &Station) -> String {
    format!(
        "{}:{}:{}:{}",
        station.service, station.name, station.khz, station.airport
    )
}

/// One controller may be published more than once for adjacent airports or
/// duplicated frequencies. A handoff only transfers when the facility and
/// frequency actually change.
pub fn same_controller(a: &Station, b: &Station) -> bool {
    a.khz == b.khz && a.name.trim().eq_ignore_ascii_case(b.name.trim())
}

/// A runway-vacate report belongs to the surface controller, not to a
/// departure or en-route position that happens to share the tuned frequency.
fn surface_owner(state: &EngineState, stations: &[Station], current: &Station) -> Station {
    ["Tower", "Ground"]
        .iter()
        .find_map(|service| {
            stations
                .iter()
                .filter(|station| {
                    station.receivable
                        && station.service == *service
                        && station.airport == state.session.plan.destination
                })
                .min_by(|a, b| a.distance_nm.total_cmp(&b.distance_nm))
                .cloned()
        })
        .unwrap_or_else(|| current.clone())
}

/// Nearest receivable Ground station at the destination, when it is a
/// different controller from the station issuing the vacate instruction.
fn ground_station(state: &EngineState, stations: &[Station], current: &Station) -> Option<Station> {
    stations
        .iter()
        .filter(|station| {
            station.receivable
                && station.service == "Ground"
                && station.airport == state.session.plan.destination
                && !same_controller(station, current)
        })
        .min_by(|a, b| a.distance_nm.total_cmp(&b.distance_nm))
        .cloned()
}

pub(crate) fn tag(state: &mut EngineState, station: &Station) -> SpeechTag {
    let settings = state.settings.clone();
    let mut tag = crate::routes::assign_airspace_controller(
        state,
        &settings,
        &station_key(station),
        "standard",
    );
    tag.position.clone_from(&station.name);
    tag.radio = Some(state.session.control_radio);
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
    ) || (state.session.has_departed
        && arrival_distance < 45.0
        && state.session.phase != PhaseCode::Departure);
    let choose = |roles: &[&str], airport: Option<&str>| {
        roles.iter().find_map(|role| {
            stations
                .iter()
                .filter(|s| {
                    s.receivable && s.service == *role && airport.is_none_or(|id| s.airport == id)
                })
                .filter(|s| s.coverage.is_empty() || s.coverage.iter().any(|c| c.contains(t)))
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
            if openatc_core::airport::aircraft_on_runway(airport, t) {
                return None;
            }
            return choose(&["Ground", "Tower"], Some(&state.session.plan.destination));
        }
        return None;
    }
    if state.session.phase == PhaseCode::Departure && t.height_agl_feet < 500.0 {
        return None;
    }
    if arrival
        && state.flow.approach_cleared
        && final_position(state).is_some_and(|distance| distance < 12.0)
    {
        // Transfer final-approach communications before entering Tower's surface volume.
        return stations
            .iter()
            .filter(|station| {
                station.receivable
                    && station.service == "Tower"
                    && station.airport == state.session.plan.destination
            })
            .min_by(|a, b| a.distance_nm.total_cmp(&b.distance_nm))
            .cloned();
    }
    let mut ahead = t.clone();
    let position = openatc_core::arrival::destination(
        [t.latitude, t.longitude],
        t.ground_track_degrees.unwrap_or(t.heading_degrees),
        (t.ground_speed_knots / 120.0).clamp(0.0, 5.0),
    );
    ahead.latitude = position[0];
    ahead.longitude = position[1];
    ahead.altitude_feet += t.vertical_speed_fpm * 0.5;
    let roles = if arrival {
        ["Approach", "Departure", "Center"]
    } else {
        ["Departure", "Approach", "Center"]
    };
    let owns = |s: &Station, at: &openatc_core::state::Telemetry| {
        s.coverage.iter().any(|v| v.contains(at))
    };
    let regional = |at: &openatc_core::state::Telemetry| {
        roles.iter().find_map(|role| {
            stations
                .iter()
                .filter(|s| s.receivable && s.service == *role && owns(s, at))
                .min_by(|a, b| a.distance_nm.total_cmp(&b.distance_nm))
                .cloned()
        })
    };
    let preferred = if arrival {
        // An approach sector becomes the next controller only when it already
        // owns the aircraft's current altitude. Selecting it from a position
        // further along the descent makes Center and Approach hand the flight
        // back and forth at the sector ceiling.
        regional(&ahead)
            .filter(|station| owns(station, t))
            .or_else(|| regional(t))
    } else {
        regional(&ahead).or_else(|| regional(t))
    };
    if ["Approach", "Departure", "Center"].contains(&current.service.as_str())
        && state.session.clearance.as_ref().is_some_and(|c| {
            c.acknowledged && (t.altitude_feet - f64::from(c.altitude_feet)).abs() < 300.0
        })
        && let Some(ceiling) = current
            .coverage
            .iter()
            .filter(|v| v.contains(t))
            .map(|v| v.ceiling)
            .reduce(f64::max)
        && f64::from(state.session.plan.cruise_feet) > ceiling
        && t.altitude_feet >= ceiling - 1500.0
        && !arrival
        && let Some(center) = stations
            .iter()
            .filter(|s| {
                s.receivable
                    && s.service == "Center"
                    && owns(s, t)
                    && s.coverage
                        .iter()
                        .any(|v| v.contains(t) && v.ceiling > ceiling)
            })
            .min_by(|a, b| a.distance_nm.total_cmp(&b.distance_nm))
    {
        return Some(center.clone());
    }
    if owns(current, t) && owns(current, &ahead) {
        if arrival
            && current.service == "Center"
            && preferred.as_ref().is_some_and(|s| s.service == "Approach")
        {
            return preferred;
        }
        return Some(current.clone());
    }
    if let Some(next) = preferred {
        return Some(next);
    }
    // Airports without published sector polygons retain the reception-based fallback.
    if !current.coverage.is_empty() {
        return None;
    }
    if arrival {
        // A missing Approach sector does not transfer descent responsibility to
        // a surface-only Tower. Center can retain the arrival through its
        // approach clearance; the established-final transfer is handled above.
        return choose(&["Approach"], Some(&state.session.plan.destination));
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
    choose(&["Center"], None)
}

/// Process handoffs only on a powered, receiving radio with unpaused valid telemetry.
/// Runway protection can interrupt a normal readback on short final.
pub fn urgent(state: &mut EngineState) -> bool {
    let t = &state.session.telemetry;
    if state.session.demo || t.paused || !t.position_valid || !t.radio_power || t.on_ground {
        return false;
    }
    let stations = nearby(&state.stations, t);
    let Some(current) = state
        .session
        .control_frequency()
        .and_then(|frequency| tuned(&stations, frequency))
        .cloned()
    else {
        return false;
    };
    protect_final(state, &current)
}

/// First usable runway exit, named taxiway and side relative to the landing direction.
fn runway_exit(state: &EngineState, runway_name: &str) -> Option<(&'static str, String)> {
    let airport = state
        .arrival_airport
        .as_ref()
        .or(state.airport.as_ref())
        .filter(|airport| airport.icao == state.session.plan.destination)?;
    let runway = airport
        .runways
        .iter()
        .find(|runway| runway.first_name == runway_name || runway.second_name == runway_name)?;
    let (start, end) = if runway.first_name == runway_name {
        (runway.first, runway.second)
    } else {
        (runway.second, runway.first)
    };
    let (east, north) = (end.east - start.east, end.north - start.north);
    let length = east.hypot(north);
    if length < 1.0 {
        return None;
    }
    let (along_east, along_north) = (east / length, north / length);
    // The left-hand side of the landing direction in the local east and north plane.
    let (left_east, left_north) = (-along_north, along_east);
    let node = |id: i64| airport.nodes.iter().find(|node| node.id == id);
    let place = |node: &openatc_core::airport::TaxiNode| {
        let offset_east = node.point.east - start.east;
        let offset_north = node.point.north - start.north;
        (
            offset_east * along_east + offset_north * along_north,
            offset_east * left_east + offset_north * left_north,
        )
    };
    let mut best: Option<(f64, f64, String)> = None;
    for edge in &airport.edges {
        if edge.runway || edge.name.trim().is_empty() {
            continue;
        }
        let (Some(first), Some(second)) = (node(edge.first), node(edge.second)) else {
            continue;
        };
        let (first_along, first_across) = place(first);
        let (second_along, second_across) = place(second);
        let on_runway = |along: f64, across: f64| {
            along >= -20.0 && along <= length + 20.0 && across.abs() < 30.0
        };
        let off_runway = |along: f64, across: f64| {
            along >= -20.0 && along <= length + 40.0 && across.abs() >= 30.0
        };
        let (along, across) = if on_runway(first_along, first_across)
            && off_runway(second_along, second_across)
        {
            (second_along, second_across)
        } else if on_runway(second_along, second_across) && off_runway(first_along, first_across) {
            (first_along, first_across)
        } else {
            continue;
        };
        if best.as_ref().is_none_or(|(current, _, _)| along < *current) {
            best = Some((along, across, edge.name.clone()));
        }
    }
    let (_, across, taxiway) = best?;
    Some((if across > 0.0 { "left" } else { "right" }, taxiway))
}

/// Reserve an unoccupied published stand for the arrival and retain it for taxi-in.
fn reserve_arrival_stand(state: &mut EngineState) -> Option<String> {
    let airport = state
        .arrival_airport
        .as_ref()
        .or(state.airport.as_ref())
        .filter(|airport| airport.icao == state.session.plan.destination)?
        .clone();
    let size = openatc_core::parking::aircraft_size(&state.session)?;
    let current = state.session.plan.arrival_stand.clone();
    if !current.is_empty()
        && let Some(parking) = airport.parking.iter().find(|stand| stand.name == current)
        && openatc_core::parking::fits(parking, size)
        && openatc_core::parking::occupied(&airport, parking, &state.session.telemetry, size)
            == Some(false)
    {
        return Some(current);
    }
    let stand = openatc_core::parking::candidates(&airport, &state.session, size)
        .into_iter()
        .find(|stand| !stand.kind.eq_ignore_ascii_case("hangar"))
        .map(|stand| stand.name.clone())?;
    state.session.plan.arrival_stand.clone_from(&stand);
    Some(stand)
}

/// The arrival runway under the aircraft, named in the direction it is travelling.
///
/// A vacate report only makes sense at the destination and on the pavement (or just past its
/// ends). `state.airport` can already have been replaced by the planned arrival airport, so the
/// runway geometry is what proves the aircraft is actually there.
fn landed_runway(state: &EngineState) -> Option<String> {
    let telemetry = &state.session.telemetry;
    if !telemetry.on_ground {
        return None;
    }
    let airport = state
        .arrival_airport
        .as_ref()
        .or(state.airport.as_ref())
        .filter(|airport| airport.icao == state.session.plan.destination)?;
    let planned = state.session.plan.arrival_runway.trim();
    if planned.is_empty() {
        return None;
    }
    let runway = airport
        .runways
        .iter()
        .find(|runway| runway.first_name == planned || runway.second_name == planned)?;
    let (along, across, length, course) = openatc_core::traffic::runway_position(
        airport,
        planned,
        telemetry.latitude,
        telemetry.longitude,
    )?;
    if !(-60.0..=length + 60.0).contains(&along) || across.abs() > runway.width / 2.0 + 35.0 {
        return None;
    }
    let track = telemetry
        .ground_track_degrees
        .unwrap_or(telemetry.heading_degrees);
    let reciprocal = if runway.first_name == planned {
        runway.second_name.clone()
    } else {
        runway.first_name.clone()
    };
    Some(
        if openatc_core::navigation::turn(course, track).abs() <= 90.0 {
            planned.to_owned()
        } else {
            reciprocal
        },
    )
}

pub fn tick(state: &mut EngineState) {
    if matches!(state.session.phase, PhaseCode::Landed | PhaseCode::TaxiIn)
        && let Some(airport) = &state.arrival_airport
        && state
            .airport
            .as_ref()
            .is_none_or(|a| a.icao != airport.icao)
    {
        state.airport = Some(airport.clone());
    }
    if !state.session.telemetry.on_ground {
        state.flow.vacate_reported = false;
    }
    if !state.session.telemetry.on_ground {
        state.flow.vacate_ground_khz = None;
    }
    if let Some(ground_khz) = state.flow.vacate_ground_khz {
        let clear = state
            .arrival_airport
            .as_ref()
            .or(state.airport.as_ref())
            .is_some_and(|airport| {
                let point = openatc_core::airport::airport_point(
                    airport,
                    state.session.telemetry.latitude,
                    state.session.telemetry.longitude,
                );
                !openatc_core::airport::within_runway(airport, &point)
            });
        if clear {
            state.session.recommended_frequency_khz = ground_khz;
            state.session.frequency_sequence = state.session.next_sequence;
            if let Some(station) =
                openatc_core::stations::nearby(&state.stations, &state.session.telemetry)
                    .into_iter()
                    .find(|station| station.khz == ground_khz)
            {
                state.flow.pending_station = station_key(&station);
            }
            state.flow.vacate_ground_khz = None;
        }
    }
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
    let Some(current) = state
        .session
        .control_frequency()
        .and_then(|frequency| tuned(&stations, frequency))
        .cloned()
        .filter(|s| !["ATIS", "Unicom"].contains(&s.service.as_str()))
    else {
        return;
    };
    if !state.session.telemetry.on_ground
        && !state.flow.missed_approach
        && matches!(
            state.session.phase,
            PhaseCode::Departure | PhaseCode::Cruise
        )
        && let (Some(origin), Some(destination)) = (
            airport_distance(state, &state.session.plan.departure),
            airport_distance(state, &state.session.plan.destination),
        )
        && origin > 10.0
        && destination < 45.0
        && destination < origin
    {
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
    }
    if protect_final(state, &current) {
        return;
    }
    if crate::instructions::awaiting_reply(state, &current) {
        return;
    }
    if crate::traffic::holding::tick(state, &current) || crate::traffic::spacing(state, &current) {
        return;
    }
    if arrival_tick(state, &current) {
        return;
    }
    let before = state.session.next_sequence;
    compliance(state, &current);
    if state.session.next_sequence != before {
        return;
    }
    if state.session.clearance_cancelled {
        return;
    }
    if state.session.phase == PhaseCode::Landed
        && state.session.telemetry.ground_speed_knots < 40.0
        && !state.flow.vacate_reported
        && let Some(runway) = landed_runway(state)
    {
        state.flow.vacate_reported = true;
        let owner = surface_owner(state, &stations, &current);
        let exit = runway_exit(state, &runway);
        let ground = ground_station(state, &stations, &owner);
        let contact = ground.as_ref().map(|station| {
            (
                station.name.clone(),
                format!("{:.3}", f64::from(station.khz) / 1000.0),
            )
        });
        let runway_for_readback = runway.clone();
        let text = match (&exit, contact) {
            (Some((side, taxiway)), Some((station, frequency))) => say(
                "arrival_vacate_contact_ground",
                &[
                    ("callsign", state.session.plan.callsign.clone()),
                    ("runway", runway.clone()),
                    ("side", (*side).to_owned()),
                    ("taxiway", taxiway.clone()),
                    ("station", station),
                    ("frequency", frequency),
                ],
            ),
            (None, Some((station, frequency))) => say(
                "arrival_vacate_any_contact_ground",
                &[
                    ("callsign", state.session.plan.callsign.clone()),
                    ("runway", runway.clone()),
                    ("station", station),
                    ("frequency", frequency),
                ],
            ),
            (Some((side, taxiway)), None) => say(
                "arrival_vacate_taxiway",
                &[
                    ("callsign", state.session.plan.callsign.clone()),
                    ("runway", runway.clone()),
                    ("side", (*side).to_owned()),
                    ("taxiway", taxiway.clone()),
                ],
            ),
            (None, None) => say(
                "arrival_vacate_any",
                &[
                    ("callsign", state.session.plan.callsign.clone()),
                    ("runway", runway),
                ],
            ),
        };
        if let Some(ground) = ground {
            // The exit instruction names Ground, but the automatic handoff waits
            // until the aircraft is clear of the runway.
            state.flow.vacate_ground_khz = Some(ground.khz);
        }
        let voice = tag(state, &owner);
        add_transmission(&mut state.session, "ATC", &text, &voice);
        if let Some(entry) = state.session.transcript.last_mut() {
            entry.pilot_reply = say(
                "pilot_exit_runway",
                &[
                    ("callsign", state.session.plan.callsign.clone()),
                    ("runway", runway_for_readback),
                ],
            );
        }
        return;
    }
    let Some(next) = next_station(state, &stations, &current) else {
        crate::instructions::tick(state, &current);
        return;
    };
    let key = station_key(&next);
    if same_controller(&next, &current)
        || key == station_key(&current)
        || key == state.flow.pending_station
    {
        state.flow.evidence = 0;
        if state.flow.pending_station.is_empty() {
            crate::instructions::tick(state, &current);
        }
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
    text: &str,
) -> Value {
    let callsign = state.session.plan.callsign.clone();
    if !request.controller_initiated {
        add_transmission(
            &mut state.session,
            &callsign,
            &request.text,
            &SpeechTag::default(),
        );
    }
    add_transmission(&mut state.session, "ATC", text, tag);
    json!({"accepted":accepted,"message":text})
}

/// Handle check-in, frequency requests and approach/landing permissions on their actual stations.
pub fn request(
    state: &mut EngineState,
    station: &Station,
    request: &Request,
    tag: &SpeechTag,
) -> Option<Value> {
    if ["departure_runway", "arrival_runway", "change_approach"].contains(&request.intent.as_str())
    {
        return Some(amend_airport(state, station, request, tag));
    }
    if !["checkin", "frequency", "approach", "landing"].contains(&request.intent.as_str()) {
        return None;
    }
    let callsign = state.session.plan.callsign.clone();
    if request.intent == "frequency" {
        let stations = nearby(&state.stations, &state.session.telemetry);
        let next = next_station(state, &stations, station);
        let (accepted, text) = if let Some(next) = next.filter(|s| !same_controller(s, station)) {
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
        return Some(respond(state, request, tag, accepted, &text));
    }
    if request.intent == "checkin" {
        if !state.session.telemetry.on_ground {
            crate::initiative::roll_for_flight(state);
        }
        state.session.control_radio = request.radio;
        state.flow.checked_in_station = station_key(station);
        state.flow.pending_station.clear();
        state.flow.evidence = 0;
        state.session.recommended_frequency_khz = station.khz;
        state.session.frequency_sequence = 0;
        let pending_readback = state
            .session
            .clearance
            .as_ref()
            .is_some_and(|c| !c.acknowledged)
            || state
                .session
                .transcript
                .iter()
                .rev()
                .find(|entry| entry.is_controller_instruction())
                .is_some_and(|entry| !entry.pilot_reply.is_empty());
        let mut checkin = request.clone();
        if !checkin.controller_initiated {
            add_transmission(
                &mut state.session,
                &callsign,
                &checkin.text,
                &SpeechTag::default(),
            );
            checkin.controller_initiated = true;
        }
        // The normal arrival gate may answer first contact with the clearance
        // itself. If it cannot, retain the ordinary check-in acknowledgement.
        if !pending_readback
            && station.service == "Tower"
            && state.flow.approach_cleared
            && !state.flow.landing_cleared
            && arrival_tick(state, station)
        {
            let text = state
                .session
                .transcript
                .last()
                .map(|entry| entry.text.clone())
                .unwrap_or_default();
            return Some(json!({"accepted":true,"message":text}));
        }
        let altitude = openatc_core::altitude_text(
            state.session.telemetry.reported_altitude_feet(),
            openatc_core::UnitSystem::Imperial,
            true,
        );
        let assigned = state.session.clearance.as_ref().filter(|c| c.acknowledged);
        let (phrase, altitude) =
            if !state.session.telemetry.on_ground && !state.flow.approach_cleared {
                assigned.map_or(("flight_checkin", altitude), |c| {
                    let transition = openatc_core::regions::region_for(
                        &state.regions,
                        &state.session.plan.departure,
                    )
                    .transition_feet;
                    let observed = state
                        .session
                        .telemetry
                        .clearance_altitude_feet(c.altitude_feet, transition);
                    let phrase = if observed < f64::from(c.altitude_feet) - 200.0 {
                        "flight_checkin_climb"
                    } else if observed > f64::from(c.altitude_feet) + 200.0 {
                        "flight_checkin_descent"
                    } else {
                        "flight_checkin_level"
                    };
                    (
                        phrase,
                        openatc_core::altitude_text(
                            f64::from(c.altitude_feet),
                            openatc_core::UnitSystem::Imperial,
                            true,
                        ),
                    )
                })
            } else {
                ("flight_checkin", altitude)
            };
        let text = say(
            phrase,
            &[
                ("callsign", callsign),
                ("station", station.name.clone()),
                ("altitude", altitude.clone()),
            ],
        );
        let reply = match phrase {
            "flight_checkin_climb" => Some(say(
                "pilot_continue_climb",
                &[
                    ("callsign", state.session.plan.callsign.clone()),
                    ("altitude", altitude),
                ],
            )),
            "flight_checkin_descent" => Some(say(
                "pilot_continue_descent",
                &[
                    ("callsign", state.session.plan.callsign.clone()),
                    ("altitude", altitude),
                ],
            )),
            "flight_checkin_level" => Some(say(
                "pilot_maintain_altitude",
                &[
                    ("callsign", state.session.plan.callsign.clone()),
                    ("altitude", altitude),
                ],
            )),
            _ => None,
        };
        let result = respond(state, &checkin, tag, true, &text);
        if pending_readback {
            // Contact acknowledgement must not hide an instruction still awaiting readback.
            if let Some(last) = state.session.transcript.last_mut() {
                last.information_only = true;
            }
            return Some(result);
        }
        if let Some(reply) = reply
            && let Some(last) = state.session.transcript.last_mut()
        {
            last.pilot_reply = reply;
        }
        return Some(result);
    }
    let airport = state
        .arrival_airport
        .as_ref()
        .or(state.airport.as_ref())
        .filter(|a| a.icao == state.session.plan.destination)
        .cloned();
    let Some(airport) = airport else {
        return Some(respond(
            state,
            request,
            tag,
            false,
            &say("arrival_airport_unavailable", &[]),
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
            &say("arrival_runway_required", &[]),
        ));
    }
    if state.session.telemetry.on_ground {
        return Some(respond(
            state,
            request,
            tag,
            false,
            &say("arrival_airborne_required", &[]),
        ));
    }
    if request.intent == "approach" {
        if let Some(r) = state.session.plan.approach.strip_prefix("ILS") {
            state.session.plan.approach = format!("I{r}");
        }
        if !["Approach", "Tower"].contains(&station.service.as_str()) {
            return Some(respond(
                state,
                request,
                tag,
                false,
                &say("arrival_contact_approach", &[]),
            ));
        }
        state.flow.arrival_requested = true;
        state.flow.missed_approach = false;
        state.flow.landing_cleared = false;
        state.flow.landing_hold = false;
        state.flow.landing_retry = None;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
        state.flow.approach_cleared = false;
        let text = say(
            "arrival_expect_runway",
            &[
                ("callsign", callsign),
                ("runway", runway),
                ("airport", airport.name),
            ],
        );
        return Some(respond(state, request, tag, true, &text));
    }
    if station.service != "Tower" {
        return Some(respond(
            state,
            request,
            tag,
            false,
            &say("arrival_contact_tower", &[]),
        ));
    }
    if !state.flow.approach_cleared || final_position(state).is_none_or(|d| d > 12.0) {
        return Some(respond(
            state,
            request,
            tag,
            false,
            &say(
                "arrival_report_final",
                &[("callsign", callsign), ("runway", runway)],
            ),
        ));
    }
    if let Some(blocked) = landing_conflict(state, &airport, &runway) {
        return Some(respond(
            state,
            request,
            tag,
            false,
            &say(blocked, &[("runway", runway)]),
        ));
    }
    state.flow.landing_cleared = true;
    let stand = reserve_arrival_stand(state);
    let weather = weather(state, station);
    let text = match &stand {
        Some(stand) => say(
            "arrival_landing_clearance_stand",
            &[
                ("callsign", callsign.clone()),
                ("runway", runway.clone()),
                ("weather", weather),
                ("stand", stand.clone()),
            ],
        ),
        None => say(
            "arrival_landing_clearance",
            &[
                ("callsign", callsign.clone()),
                ("runway", runway.clone()),
                ("weather", weather),
            ],
        ),
    };
    let result = respond(state, request, tag, true, &text);
    let reply = match &stand {
        Some(stand) => say(
            "pilot_landing_readback_stand",
            &[
                ("callsign", state.session.plan.callsign.clone()),
                ("runway", state.session.plan.arrival_runway.clone()),
                ("stand", stand.clone()),
            ],
        ),
        None => say(
            "pilot_landing_readback",
            &[
                ("callsign", state.session.plan.callsign.clone()),
                ("runway", state.session.plan.arrival_runway.clone()),
            ],
        ),
    };
    if let Some(last) = state.session.transcript.last_mut() {
        last.pilot_reply = reply;
    }
    Some(result)
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
        let pressure = w["pressureHpa"].as_f64().unwrap_or(0.0).round();
        say(
            if pressure < 1000.0 {
                "flight_qnh_units"
            } else {
                "flight_qnh"
            },
            &[("pressure", format!("{pressure:.0}"))],
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

/// Speak a sequencing instruction and record the pilot readback that matches it, so the copilot
/// never acknowledges a take-off clearance that was not given.
#[cfg(test)]
fn return_with_reply(
    state: &mut EngineState,
    result: &mut openatc_core::apply::RequestOutcome,
    instruction: &str,
    readback: &str,
) {
    let slots = [
        ("callsign", state.session.plan.callsign.clone()),
        ("runway", state.session.plan.runway.clone()),
    ];
    result.message = say(instruction, &slots);
    if let Some(entry) = state
        .session
        .transcript
        .iter_mut()
        .rev()
        .find(|entry| entry.speaker == "ATC")
    {
        entry.text.clone_from(&result.message);
        entry.pilot_reply = say(readback, &slots);
    }
}

/// Apply the committed takeoff clearance phraseology.
///
/// The runway decision is made once in `apply_ready`; this function only replaces the
/// generic clearance with the full callsign, weather and climb instruction. It never
/// turns a committed clearance into a hold.
pub fn takeoff_wording(
    state: &mut EngineState,
    station: &Station,
    result: &mut openatc_core::apply::RequestOutcome,
) {
    if state.session.taxi_clearance.takeoff_state != TakeoffState::Cleared {
        return;
    }
    state.flow.takeoff_cleared = true;
    result.message = takeoff_clearance_text(state, station);
    if let Some(entry) = state
        .session
        .transcript
        .iter_mut()
        .rev()
        .find(|entry| entry.speaker == "ATC")
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

/// Record when the player first reaches the departure hold. The runway queue compares
/// this with each ambient departure's hold time, so the first aircraft at the hold goes
/// first regardless of which taxiway or hold line it used.
pub(crate) fn observe_takeoff_hold(state: &mut EngineState) {
    let route = &state.session.taxi_clearance;
    let Some(airport) = state.airport.as_ref() else {
        state.flow.takeoff_hold_since = None;
        return;
    };
    if state.session.phase != PhaseCode::Taxi
        || !route.approved
        || route.pending_readback
        || !matches!(
            route.takeoff_state,
            TakeoffState::Held | TakeoffState::LineUpAndWait
        )
        || !state.session.telemetry.on_ground
        || state.session.telemetry.paused
        || !state.session.telemetry.position_valid
    {
        state.flow.takeoff_hold_since = None;
        return;
    }
    let Some(end) = route.holding_marker_point.as_ref().or(route.points.last()) else {
        state.flow.takeoff_hold_since = None;
        return;
    };
    let here = openatc_core::airport::airport_point(
        airport,
        state.session.telemetry.latitude,
        state.session.telemetry.longitude,
    );
    if (here.east - end.east).hypot(here.north - end.north) > 20.0 {
        state.flow.takeoff_hold_since = None;
        return;
    }
    if state.flow.takeoff_hold_route != route.sequence || state.flow.takeoff_hold_since.is_none() {
        state.flow.takeoff_hold_route = route.sequence;
        state.flow.takeoff_hold_since = Some(std::time::Instant::now());
    }
}

/// Progress a held or lined-up departure without another ready call.
pub(crate) fn takeoff_tick(state: &mut EngineState) {
    if state.session.demo
        || !state.session.telemetry.on_ground
        || state.session.telemetry.paused
        || !state.session.telemetry.position_valid
        || !state.session.telemetry.radio_power
        || state.session.phase != PhaseCode::Taxi
    {
        return;
    }
    let route = &state.session.taxi_clearance;
    if !route.approved || !route.guidance_complete {
        return;
    }
    if !matches!(
        route.takeoff_state,
        TakeoffState::Held | TakeoffState::LineUpAndWait
    ) {
        return;
    }
    let Some(airport) = state.airport.clone() else {
        return;
    };
    let runway = state.session.plan.runway.clone();
    if !route.hold_short_runway.is_empty()
        && route.hold_short_runway != runway
        && !route.backtrack_required
    {
        return;
    }
    let stations = nearby(&state.stations, &state.session.telemetry);
    let Some(station) = state
        .session
        .control_frequency()
        .and_then(|frequency| tuned(&stations, frequency))
        .cloned()
        .filter(|station| {
            station.airport == airport.icao
                && (station.service == "Tower"
                    || station.services.iter().any(|service| service == "Tower"))
        })
    else {
        return;
    };
    if !crate::traffic::departure_ready(state) {
        return;
    }
    // An ambient departure still waiting at the hold is ahead of the player; do not
    // clear the player to line up past it.
    if state.ambient.departure_ahead_of_player(
        &airport,
        &runway,
        &state.session.telemetry,
        state.flow.takeoff_hold_since,
    ) {
        return;
    }
    let sequence = crate::routes::runway_sequence(state, &airport, &runway);
    let targets = state.traffic_targets();
    if state.session.departure_wake_seconds <= 0.0 {
        state.flow.wake_warned = false;
    }
    let decision = openatc_core::apply::takeoff_decision(
        &airport,
        &runway,
        targets.iter(),
        sequence,
        state.session.departure_wake_seconds,
    );
    match decision {
        TakeoffDecision::Hold(_) => return,
        TakeoffDecision::LineUpAndWait => {
            if state.session.taxi_clearance.takeoff_state == TakeoffState::Held {
                line_up_departure(state, &station, &runway);
                if state.session.departure_wake_seconds > 0.0 {
                    state.flow.wake_warned = true;
                }
            } else if state.session.departure_wake_seconds > 0.0 && !state.flow.wake_warned {
                state.flow.wake_warned = true;
                let slots = [
                    ("callsign", state.session.plan.callsign.clone()),
                    ("runway", runway.clone()),
                ];
                let text = say("flight_wake_caution", &slots);
                let voice = tag(state, &station);
                add_transmission(&mut state.session, "ATC", &text, &voice);
                if let Some(entry) = state.session.transcript.last_mut() {
                    entry.information_only = true;
                    entry.pilot_reply = say("pilot_wake_caution_readback", &slots);
                }
            }
            return;
        }
        TakeoffDecision::Cleared => state.flow.wake_warned = false,
    }
    clear_takeoff(state, &station, &runway);
}

fn line_up_departure(state: &mut EngineState, station: &Station, runway: &str) {
    let slots = [
        ("callsign", state.session.plan.callsign.clone()),
        ("runway", runway.to_owned()),
    ];
    let text = say(
        if state.session.departure_wake_seconds > 0.0 {
            "flight_line_up_and_wait_wake"
        } else {
            "flight_line_up_and_wait"
        },
        &slots,
    );
    let voice = tag(state, station);
    add_transmission(&mut state.session, "ATC", &text, &voice);
    if let Some(entry) = state.session.transcript.last_mut() {
        entry.pilot_reply = say("pilot_line_up_and_wait_readback", &slots);
    }
    state.session.taxi_clearance.takeoff_state = TakeoffState::LineUpAndWait;
    state.session.taxi_clearance.waiting_for_traffic = false;
}

fn clear_takeoff(state: &mut EngineState, station: &Station, runway: &str) {
    let text = takeoff_clearance_text(state, station);
    let voice = tag(state, station);
    add_transmission(&mut state.session, "ATC", &text, &voice);
    if let Some(entry) = state.session.transcript.last_mut() {
        entry.pilot_reply = say(
            "pilot_takeoff_readback",
            &[
                ("runway", runway.to_owned()),
                ("callsign", state.session.plan.callsign.clone()),
            ],
        );
    }
    state.session.taxi_clearance.takeoff_state = TakeoffState::Cleared;
    state.session.taxi_clearance.waiting_for_traffic = false;
    state.session.taxi_clearance.approved = false;
    state.flow.takeoff_cleared = true;
    state
        .session
        .on_event(openatc_core::aircraft::AircraftEvent::Departure);
}

/// Full takeoff clearance text for the current flight plan.
fn takeoff_clearance_text(state: &EngineState, station: &Station) -> String {
    let plan = &state.session.plan;
    let sid = state.airport.as_ref().is_some_and(|airport| {
        airport
            .procedures
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
    say(
        "flight_takeoff",
        &[
            ("callsign", plan.callsign.clone()),
            ("runway", plan.runway.clone()),
            ("weather", weather(state, station)),
            ("climb", climb),
            ("expected", expected),
        ],
    )
}

fn compliance(state: &mut EngineState, station: &Station) {
    if !state.settings.enforce_clearance_constraints || state.session.emergency_declared {
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
    // Once cleared and established on final, the approach path replaces the entry level.
    if state.flow.approach_cleared
        && !state.flow.missed_approach
        && let Some(distance) = final_position(state).filter(|distance| *distance <= 15.0)
        && let Some(airport) = state.arrival_airport.as_ref()
    {
        let glide = state
            .arrival_profile
            .as_ref()
            .and_then(|profile| {
                openatc_core::arrival::target(
                    profile,
                    &state.session.plan.arrival_runway,
                    &state.session.plan.approach,
                )
            })
            .and_then(|target| target.glide_degrees)
            .unwrap_or(3.0);
        let path = airport.elevation_feet + openatc_core::arrival::gradient(glide) * distance;
        let below_cleared =
            state.session.telemetry.altitude_feet < f64::from(clearance.altitude_feet) - 300.0;
        if below_cleared || (state.session.telemetry.altitude_feet - path).abs() <= 500.0 {
            state.flow.warning.resolved();
            state.flow.last_correction = None;
            return;
        }
    }
    let sequence = clearance.sequence;
    let target = f64::from(clearance.altitude_feet);
    let t = &state.session.telemetry;
    let transition =
        openatc_core::regions::region_for(&state.regions, &state.session.plan.departure)
            .transition_feet;
    let error = t.clearance_altitude_feet(clearance.altitude_feet, transition) - target;
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
    let repeated = state.flow.warning.continuing();
    let altitude = openatc_core::altitude_text(target, openatc_core::UnitSystem::Imperial, true);
    let mut text = say(
        "compliance_altitude_correction",
        &[
            ("callsign", state.session.plan.callsign.clone()),
            ("altitude", altitude),
        ],
    );
    if repeated && state.settings.terminate_after_warnings {
        text.push(' ');
        text.push_str(&say("compliance_assistance", &[]));
    }
    let voice = tag(state, station);
    add_transmission(&mut state.session, "ATC", &text, &voice);
}

fn amend_airport(
    state: &mut EngineState,
    station: &Station,
    request: &Request,
    tag: &SpeechTag,
) -> Value {
    if !openatc_core::ops::request_available(&state.session, &request.intent) {
        return respond(
            state,
            request,
            tag,
            false,
            &say("amendment_unavailable", &[]),
        );
    }
    let departure = request.intent == "departure_runway";
    let id = if departure {
        &state.session.plan.departure
    } else {
        &state.session.plan.destination
    };
    let Some(airport) = state
        .arrival_airport
        .as_ref()
        .filter(|a| a.icao == *id)
        .or_else(|| state.airport.as_ref().filter(|a| a.icao == *id))
    else {
        return respond(
            state,
            request,
            tag,
            false,
            &say("arrival_airport_unavailable", &[]),
        );
    };
    if departure && station.airport != *id {
        return respond(
            state,
            request,
            tag,
            false,
            &say("amendment_wrong_airport", &[]),
        );
    }
    let value = request.waypoint.trim().to_uppercase();
    let callsign = state.session.plan.callsign.clone();
    if request.intent == "change_approach" {
        let canonical = value
            .strip_prefix("ILS")
            .map_or_else(|| value.clone(), |r| format!("I{r}"));
        if !airport
            .procedures
            .iter()
            .any(|p| p.kind == "APPCH" && p.name == canonical)
        {
            return respond(
                state,
                request,
                tag,
                false,
                &say("amendment_approach_unavailable", &[]),
            );
        }
        if let Some(runway) = approach_runway(&canonical) {
            if !airport
                .runways
                .iter()
                .any(|r| r.first_name == runway || r.second_name == runway)
            {
                return respond(
                    state,
                    request,
                    tag,
                    false,
                    &say("amendment_runway_unavailable", &[]),
                );
            }
            state.session.plan.arrival_runway = runway.into();
        }
        state.session.plan.approach.clone_from(&canonical);
        state.flow.announce_approach = false;
        state.flow.vacate_reported = false;
        openatc_core::navigation::enrich(&mut state.session.plan, &state.settings.simulator_root);
        state.flow.arrival_requested = true;
        state.flow.approach_cleared = false;
        state.flow.landing_cleared = false;
        if state.session.phase == PhaseCode::Approach {
            state
                .session
                .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
        }
        let text = say(
            "amendment_expect_approach",
            &[
                ("callsign", callsign.clone()),
                ("approach", approach_label(&canonical)),
            ],
        );
        let result = respond(state, request, tag, true, &text);
        if let Some(last) = state.session.transcript.last_mut() {
            last.pilot_reply = say(
                "pilot_approach_amendment",
                &[
                    ("callsign", callsign),
                    ("approach", approach_label(&canonical)),
                ],
            );
        }
        return result;
    }
    if !airport
        .runways
        .iter()
        .any(|r| r.first_name == value || r.second_name == value)
    {
        return respond(
            state,
            request,
            tag,
            false,
            &say("amendment_runway_unavailable", &[]),
        );
    }
    if departure {
        state.flow.takeoff_cleared = false;
        state.flow.vacate_reported = false;
        let sid_entries: Vec<_> = airport
            .procedures
            .iter()
            .filter(|p| p.kind == "SID" && p.name == state.session.plan.sid)
            .collect();
        let has_runway_transitions = sid_entries.iter().any(|p| p.transition.starts_with("RW"));
        if !state.session.plan.sid.is_empty()
            && (sid_entries.is_empty()
                || (has_runway_transitions
                    && !sid_entries.iter().any(|p| {
                        p.transition == format!("RW{value}")
                            || p.transition
                                == format!("RW{}B", value.trim_end_matches(['L', 'R', 'C']))
                    })))
        {
            return respond(
                state,
                request,
                tag,
                false,
                &say("amendment_departure_incompatible", &[]),
            );
        }
        state.session.plan.runway.clone_from(&value);
        state.session.taxi_clearance = Default::default();
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::IfrClearanceReceived);
        if let Some(c) = state.session.clearance.as_mut() {
            c.runway.clone_from(&value);
            c.sequence = state.session.next_sequence;
            c.readback_scope = "runway".into();
            c.acknowledged = false;
        }
    } else {
        state.session.plan.arrival_runway.clone_from(&value);
        state.session.plan.approach.clear();
        state.flow.announce_approach = false;
        state.flow.vacate_reported = false;
        if state.vertical_policy.select_runway_ils
            && let Some(approach) = state
                .arrival_profile
                .as_ref()
                .and_then(|profile| profile.default_ils(&value))
        {
            state.session.plan.approach = approach;
            state.flow.announce_approach = true;
        }
        state.flow.approach_cleared = false;
        state.flow.landing_cleared = false;
        if state.session.phase == PhaseCode::Approach {
            state
                .session
                .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
        }
    }
    openatc_core::navigation::enrich(&mut state.session.plan, &state.settings.simulator_root);
    let text = say(
        "amendment_runway",
        &[("callsign", callsign.clone()), ("runway", value.clone())],
    );
    let result = respond(state, request, tag, true, &text);
    if !departure && let Some(last) = state.session.transcript.last_mut() {
        last.pilot_reply = say(
            "pilot_runway_amendment",
            &[("callsign", callsign), ("runway", value)],
        );
    }
    result
}

fn final_position(state: &EngineState) -> Option<f64> {
    let airport = state
        .arrival_airport
        .as_ref()
        .or(state.airport.as_ref())
        .filter(|a| a.icao == state.session.plan.destination)?;
    let name = &state.session.plan.arrival_runway;
    let runway = airport
        .runways
        .iter()
        .find(|r| r.first_name == *name || r.second_name == *name)?;
    let start = runway.landing_threshold(name)?;
    let end = if runway.first_name == *name {
        runway.second
    } else {
        runway.first
    };
    let t = &state.session.telemetry;
    let p = openatc_core::airport::airport_point(airport, t.latitude, t.longitude);
    let dx = end.east - start.east;
    let dn = end.north - start.north;
    let length = dx.hypot(dn);
    if length < 100.0 {
        return None;
    }
    let along = ((p.east - start.east) * dx + (p.north - start.north) * dn) / length;
    let cross = ((p.east - start.east) * dn - (p.north - start.north) * dx).abs() / length;
    let track = t.ground_track_degrees.unwrap_or(t.heading_degrees);
    let course = dx.atan2(dn).to_degrees().rem_euclid(360.0);
    if along > 0.0
        || cross > 0.3 * 1852.0
        || openatc_core::navigation::turn(track, course).abs() > 20.0
    {
        return None;
    }
    Some(-along / 1852.0)
}
pub(crate) fn go_around(state: &mut EngineState) {
    crate::traffic::holding::clear(state);
    state.flow.approach_cleared = false;
    state.flow.landing_cleared = false;
    state.flow.arrival_requested = false;
    state.flow.missed_approach = true;
    state.flow.landing_hold = false;
    state.flow.landing_retry = None;
    state
        .session
        .on_event(openatc_core::aircraft::AircraftEvent::Departure);
    state.session.phase_evidence = 0;
    state.pending_copilot_reply = None;
}

fn landing_conflict(
    state: &EngineState,
    airport: &openatc_core::airport::Airport,
    runway: &str,
) -> Option<&'static str> {
    if state.session.arrival_wake_conflict {
        Some("holding_wake")
    } else {
        let telemetry = state.telemetry_with_traffic();
        openatc_core::traffic::landing_blocked(airport, runway, &telemetry)
    }
}

// Recheck the runway even while a landing readback is outstanding.
fn protect_final(state: &mut EngineState, current: &Station) -> bool {
    if current.service != "Tower"
        || state.session.telemetry.on_ground
        || state.flow.missed_approach
        || !state.flow.approach_cleared
    {
        return false;
    }
    let Some(distance) = final_position(state).filter(|d| *d <= 15.0) else {
        return false;
    };
    let Some(airport) = state.arrival_airport.as_ref().or(state.airport.as_ref()) else {
        return false;
    };
    let blocked = landing_conflict(state, airport, &state.session.plan.arrival_runway);
    let confirmed_conflict = blocked.is_some_and(|reason| reason != "holding_standby");
    if !(state.session.arrival_wake_conflict
        || confirmed_conflict && state.flow.landing_cleared && distance < 2.5
        || blocked.is_some() && distance < 1.0)
    {
        return false;
    }
    go_around(state);
    let mut voice = tag(state, current);
    voice.urgent = true;
    voice.delivery = "urgent".into();
    let slots = [("callsign", state.session.plan.callsign.clone())];
    add_transmission(
        &mut state.session,
        "ATC",
        &say(
            match blocked {
                Some("holding_traffic_final") => "arrival_go_around_traffic",
                Some("holding_runway_occupied") => "arrival_go_around_blocked",
                Some("holding_wake") => "arrival_go_around_wake",
                _ => "arrival_go_around_unconfirmed",
            },
            &slots,
        ),
        &voice,
    );
    if let Some(last) = state.session.transcript.last_mut() {
        last.pilot_reply = say("pilot_go_around_blocked", &slots);
    }
    true
}

fn arrival_tick(state: &mut EngineState, current: &Station) -> bool {
    if state.flow.missed_approach || state.session.clearance_cancelled {
        return false;
    }
    if matches!(
        state.session.phase,
        PhaseCode::Arrival | PhaseCode::Approach
    ) && !state.session.telemetry.on_ground
        && ["Approach", "Center"].contains(&current.service.as_str())
        && !state.flow.approach_query_sent
        && (state.session.plan.approach.is_empty()
            || state.arrival_airport.as_ref().is_some_and(|airport| {
                !airport.procedures.iter().any(|procedure| {
                    procedure.kind == "APPCH" && procedure.name == state.session.plan.approach
                })
            }))
    {
        state.flow.approach_query_sent = true;
        let voice = tag(state, current);
        let text = say(
            "arrival_request_approach",
            &[("callsign", state.session.plan.callsign.clone())],
        );
        add_transmission(&mut state.session, "ATC", &text, &voice);
        return true;
    }
    if matches!(
        state.session.phase,
        PhaseCode::Arrival | PhaseCode::Approach
    ) && !state.session.plan.approach.is_empty()
    {
        state.flow.arrival_requested = true;
    }
    if state.session.telemetry.on_ground || !state.flow.arrival_requested {
        return false;
    }
    if state.flow.announce_approach && ["Approach", "Center"].contains(&current.service.as_str()) {
        state.flow.announce_approach = false;
        let slots = [
            ("callsign", state.session.plan.callsign.clone()),
            ("approach", approach_label(&state.session.plan.approach)),
        ];
        let voice = tag(state, current);
        add_transmission(
            &mut state.session,
            "ATC",
            &say("arrival_expect_approach", &slots),
            &voice,
        );
        if let Some(last) = state.session.transcript.last_mut() {
            last.pilot_reply = say("pilot_approach_amendment", &slots);
        }
        return true;
    }
    let Some(distance) = final_position(state).filter(|d| *d <= 15.0) else {
        return false;
    };
    if !state.flow.approach_cleared && ["Approach", "Center"].contains(&current.service.as_str()) {
        let Some(airport) = state.arrival_airport.as_ref().or(state.airport.as_ref()) else {
            return false;
        };
        let approach = &state.session.plan.approach;
        if approach.is_empty()
            || !airport
                .procedures
                .iter()
                .any(|p| p.kind == "APPCH" && p.name == *approach)
        {
            return false;
        }
        // Issue the published entry-level amendment before clearing the approach.
        // An aircraft already below its last cleared level cannot be corrected
        // upward into the old level: the approach clearance is the resolution.
        let below_cleared = state.session.clearance.as_ref().is_some_and(|clearance| {
            state
                .session
                .telemetry
                .indicated_altitude_feet
                .unwrap_or(state.session.telemetry.altitude_feet)
                < f64::from(clearance.altitude_feet) - 300.0
        });
        if !below_cleared
            && let Some(entry) = state
                .arrival_profile
                .as_ref()
                .and_then(|profile| {
                    openatc_core::arrival::target(
                        profile,
                        &state.session.plan.arrival_runway,
                        approach,
                    )
                })
                .and_then(|target| target.intercept_feet)
            && state
                .session
                .clearance
                .as_ref()
                .is_some_and(|clearance| f64::from(clearance.altitude_feet) > entry)
            && state
                .session
                .telemetry
                .indicated_altitude_feet
                .unwrap_or(state.session.telemetry.altitude_feet)
                > entry + 300.0
        {
            return false;
        }
        let agl = state.session.telemetry.altitude_feet - airport.elevation_feet;
        if agl < 200.0
            || (!below_cleared && agl > openatc_core::arrival::gradient(3.0) * distance + 2000.0)
        {
            return false;
        }
        let slots = [
            ("callsign", state.session.plan.callsign.clone()),
            ("approach", approach_label(approach)),
            ("runway", state.session.plan.arrival_runway.clone()),
        ];
        state.flow.approach_cleared = true;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Approach);
        let speed_release = crate::traffic::clear_approach_speed(state);
        let mut text = say("arrival_approach_clearance", &slots);
        if let Some((controller, _)) = &speed_release {
            text.push(' ');
            text.push_str(controller);
        }
        let voice = tag(state, current);
        add_transmission(&mut state.session, "ATC", &text, &voice);
        if let Some(last) = state.session.transcript.last_mut() {
            last.pilot_reply = say("pilot_approach_clearance", &slots);
            if let Some((_, pilot)) = speed_release {
                last.pilot_reply.push(' ');
                last.pilot_reply.push_str(&pilot);
            }
        }
        return true;
    }
    if current.service != "Tower"
        || !state.flow.approach_cleared
        || state.flow.landing_cleared
        || state.flow.checked_in_station != station_key(current)
    {
        return false;
    }
    if state
        .session
        .transcript
        .iter()
        .rev()
        .find(|e| e.is_controller_instruction())
        .is_some_and(|e| !e.pilot_reply.is_empty())
    {
        return false;
    }
    if state
        .flow
        .landing_retry
        .is_some_and(|last| last.elapsed().as_secs() < 15)
    {
        return false;
    }
    let Some(airport) = state.arrival_airport.as_ref().or(state.airport.as_ref()) else {
        return false;
    };
    let blocked = landing_conflict(state, airport, &state.session.plan.arrival_runway);
    if blocked.is_some() {
        state.flow.landing_retry = Some(std::time::Instant::now());
        if !state.flow.landing_hold {
            state.flow.landing_hold = true;
            let voice = tag(state, current);
            let text = say(
                match blocked {
                    Some("holding_traffic_final") => "arrival_continue_traffic",
                    Some("holding_runway_occupied") => "arrival_continue_no_landing",
                    Some("holding_wake") => "arrival_continue_wake",
                    _ => "arrival_continue_unconfirmed",
                },
                &[("callsign", state.session.plan.callsign.clone())],
            );
            add_transmission(&mut state.session, "ATC", &text, &voice);
            return true;
        }
        return false;
    }
    state.flow.landing_cleared = true;
    state.flow.landing_hold = false;
    // Reserve before landing so generated traffic cannot take the assigned stand.
    let stand = reserve_arrival_stand(state);
    let slots = [
        ("callsign", state.session.plan.callsign.clone()),
        ("runway", state.session.plan.arrival_runway.clone()),
        ("weather", weather(state, current)),
    ];
    let voice = tag(state, current);
    let clearance = match &stand {
        Some(stand) => say(
            "arrival_landing_clearance_stand",
            &[
                ("callsign", slots[0].1.clone()),
                ("runway", slots[1].1.clone()),
                ("weather", slots[2].1.clone()),
                ("stand", stand.clone()),
            ],
        ),
        None => say("arrival_landing_clearance", &slots),
    };
    add_transmission(&mut state.session, "ATC", &clearance, &voice);
    if let Some(last) = state.session.transcript.last_mut() {
        last.pilot_reply = match &stand {
            Some(stand) => say(
                "pilot_landing_readback_stand",
                &[
                    ("callsign", slots[0].1.clone()),
                    ("runway", slots[1].1.clone()),
                    ("stand", stand.clone()),
                ],
            ),
            None => say("pilot_landing_readback", &slots),
        };
    }
    true
}

fn approach_runway(name: &str) -> Option<&str> {
    let bytes = name.as_bytes();
    if bytes.len() < 3
        || !name.is_ascii()
        || !b"IRLVN".contains(&bytes[0])
        || !bytes[1..3].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let end = if bytes.get(3).is_some_and(|c| b"LRC".contains(c)) {
        4
    } else {
        3
    };
    let suffix = &name[end..];
    (suffix.is_empty() || matches!(suffix, "X" | "Y" | "Z")).then_some(&name[1..end])
}
/// Turn a published procedure identifier into a readable approach name.
pub(crate) fn approach_label(name: &str) -> String {
    let Some(runway) = approach_runway(name) else {
        return name.into();
    };
    let template = match name.as_bytes()[0] {
        b'I' => "approach_label_ils",
        b'R' => "approach_label_rnav",
        b'L' => "approach_label_localizer",
        b'V' => "approach_label_vor",
        b'N' => "approach_label_ndb",
        _ => return name.into(),
    };
    let variant = match &name[runway.len() + 1..] {
        "X" => say("approach_variant_x", &[]),
        "Y" => say("approach_variant_y", &[]),
        "Z" => say("approach_variant_z", &[]),
        _ => String::new(),
    };
    say(template, &[("runway", runway.into()), ("variant", variant)])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use openatc_core::stations::Coverage;
    #[test]
    fn missing_approach_is_requested_once_without_granting_permission() {
        let (mut state, station, _) = arrival_state();
        state.session.plan.approach.clear();
        assert!(arrival_tick(&mut state, &station));
        assert!(!state.flow.approach_cleared);
        assert_eq!(state.session.transcript.len(), 1);
        assert!(!arrival_tick(&mut state, &station));
        assert_eq!(state.session.transcript.len(), 1);
    }
    #[tokio::test]
    #[ignore = "requires installed navdata and OPENATC_TEST_SIMBRIEF_JSON"]
    async fn imported_nice_milan_plan_reaches_ils_entry_tower_and_landing() {
        use openatc_core::state::{Clearance, Telemetry};
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let document =
            std::fs::read_to_string(std::env::var("OPENATC_TEST_SIMBRIEF_JSON").unwrap()).unwrap();
        let plan =
            openatc_core::simbrief::parse_simbrief(&serde_json::from_str(&document).unwrap())
                .unwrap();
        assert_eq!(
            (
                &*plan.departure,
                &*plan.destination,
                &*plan.star,
                &*plan.arrival_runway
            ),
            ("LFMN", "LIMC", "DEVO3E", "35R")
        );
        assert!(
            plan.approach.is_empty(),
            "This regression must exercise the missing imported approach"
        );
        let mut state = crate::test_state();
        state.settings.simulator_root = root;
        state.settings.enforce_clearance_constraints = true;
        state.session.demo = false;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Parked);
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        assert!(
            crate::routes::post_plan(axum::extract::State(shared.clone()), axum::Json(plan))
                .await
                .status()
                .is_success()
        );
        let mut state = std::sync::Arc::try_unwrap(shared)
            .ok()
            .unwrap()
            .into_inner();
        assert_eq!(state.session.plan.approach, "I35RY");
        for kind in ["sid", "cruise", "star", "approach"] {
            let checkpoint = openatc_core::debug_jump::route(&state.session.plan, kind)
                .unwrap_or_else(|error| panic!("{kind}: {error}"));
            eprintln!(
                "Checkpoint {kind}: next {} at {:.0} ft ({:.5}, {:.5})",
                state.session.plan.fixes[checkpoint.next_fix].identifier,
                checkpoint.altitude_feet,
                checkpoint.latitude,
                checkpoint.longitude
            );
        }
        let fixes = state.session.plan.fixes.clone();
        let ci = fixes
            .iter()
            .position(|fix| fix.identifier == "CI3RY")
            .unwrap();
        let fi = fixes
            .iter()
            .position(|fix| fix.identifier == "FI3RY")
            .unwrap();
        assert_eq!(fixes[ci].maximum_altitude_feet, Some(4000));
        assert_eq!(fixes[fi].maximum_altitude_feet, Some(4000));
        assert!(
            fixes
                .iter()
                .position(|fix| fix.identifier == "INLER")
                .unwrap()
                < ci
        );
        let airport = state.arrival_airport.clone().unwrap();
        let center = Station {
            name: "MILAN Center".into(),
            service: "Center".into(),
            khz: 118_675,
            latitude: airport.reference_latitude,
            longitude: airport.reference_longitude,
            ..Default::default()
        };
        let tower = openatc_core::stations::from_airport(&airport)
            .into_iter()
            .find(|station| station.service == "Tower")
            .unwrap();
        state.stations = vec![center.clone(), tower.clone()];
        state.session.has_departed = true;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Cruise);
        state.session.telemetry = Telemetry {
            on_ground: false,
            position_valid: true,
            radio_power: true,
            com1_khz: center.khz,
            ground_speed_knots: 200.0,
            indicated_speed_knots: Some(170.0),
            altitude_feet: 18000.0,
            height_agl_feet: 17250.0,
            traffic: Some(vec![]),
            ..Default::default()
        };
        state.session.clearance = Some(Clearance {
            altitude_feet: 18000,
            acknowledged: true,
            route: state.session.plan.route.clone(),
            ..Default::default()
        });
        let acknowledge = |state: &mut EngineState| {
            if let Some(clearance) = state.session.clearance.as_mut() {
                clearance.acknowledged = true;
            }
            if let Some(last) = state.session.transcript.last_mut() {
                last.pilot_reply.clear();
            }
        };
        let mut checkpoint_fixes = fixes[..=fi].to_vec();
        if std::env::var("OPENATC_TEST_CHECKPOINT").as_deref() == Ok("approach") {
            state.settings.dev_mode = true;
            state.session.telemetry.engines_running = true;
            let checkpoint =
                openatc_core::debug_jump::route(&state.session.plan, "approach").unwrap();
            let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
            assert!(
                crate::debug_jump::prepare(
                    axum::extract::State(shared.clone()),
                    axum::Json(json!({"kind":"approach"}))
                )
                .await
                .status()
                .is_success()
            );
            let token = shared.read().await.session_generation;
            let mut observed = shared.read().await.session.telemetry.clone();
            observed.latitude = checkpoint.latitude;
            observed.longitude = checkpoint.longitude;
            observed.altitude_feet = checkpoint.altitude_feet;
            observed.indicated_altitude_feet = Some(checkpoint.altitude_feet);
            observed.paused = true;
            assert!(
                crate::debug_jump::finish(
                    axum::extract::State(shared.clone()),
                    axum::Json(json!({"token":token,"moved":true,"telemetry":observed}))
                )
                .await
                .status()
                .is_success()
            );
            state = std::sync::Arc::try_unwrap(shared)
                .ok()
                .unwrap()
                .into_inner();
            assert!(!state.flow.approach_cleared && !state.flow.landing_cleared);
            state.session.telemetry.paused = false;
            checkpoint_fixes = vec![openatc_core::state::RouteFix {
                latitude: checkpoint.latitude,
                longitude: checkpoint.longitude,
                ..Default::default()
            }];
            checkpoint_fixes.extend_from_slice(&fixes[checkpoint.next_fix..=fi]);
        }
        let mut entry_distance = None;
        // Advance every imported/expanded leg in small steps. No next-fix or
        // arrival-phase injection can hide an import or sequencing failure.
        let fly_by = std::env::var_os("OPENATC_TEST_EXACT_FIXES").is_none();
        let flown = checkpoint_fixes
            .iter()
            .filter(|fix| !fly_by || !["TOC", "TOD"].contains(&fix.identifier.as_str()))
            .collect::<Vec<_>>();
        for pair in flown.windows(2) {
            let a = [pair[0].latitude, pair[0].longitude];
            let b = [pair[1].latitude, pair[1].longitude];
            let distance = openatc_core::arrival::distance_nm(a, b);
            let steps = (distance * 4.0).ceil().max(1.0) as usize;
            let course = openatc_core::arrival::bearing(a, b);
            for step in 1..=steps {
                let travelled = distance * step as f64 / steps as f64;
                if fly_by && (travelled < 0.5 || travelled > distance - 0.5) {
                    continue;
                }
                let position = openatc_core::arrival::destination(
                    a,
                    course,
                    distance * step as f64 / steps as f64,
                );
                let altitude = f64::from(state.session.clearance.as_ref().unwrap().altitude_feet);
                state.session.telemetry.latitude = position[0];
                state.session.telemetry.longitude = position[1];
                state.session.telemetry.ground_track_degrees = Some(course);
                state.session.telemetry.heading_degrees = course;
                state.session.telemetry.altitude_feet = altitude;
                state.session.telemetry.indicated_altitude_feet = Some(altitude);
                state.session.telemetry.pressure_altitude_feet = Some(altitude);
                state.session.telemetry.height_agl_feet = altitude - airport.elevation_feet;
                state.flow.monitor.last_call = Some(
                    std::time::Instant::now()
                        .checked_sub(std::time::Duration::from_secs(31))
                        .unwrap(),
                );
                tick(&mut state);
                if entry_distance.is_none()
                    && state.session.clearance.as_ref().unwrap().altitude_feet == 4000
                {
                    entry_distance = Some(openatc_core::arrival::distance_nm(
                        position,
                        [fixes[ci].latitude, fixes[ci].longitude],
                    ));
                }
                acknowledge(&mut state);
            }
            if pair[1].identifier == "MC420" {
                assert_eq!(
                    state.session.clearance.as_ref().unwrap().altitude_feet,
                    4000,
                    "The screenshot's MC420 position must already have the ILS entry descent"
                );
            }
        }
        assert!(
            entry_distance.is_some_and(|distance| distance > 3.0),
            "Entry descent was not issued before CI3RY: {entry_distance:?}; next={}",
            state.flow.monitor.next_fix
        );
        assert!(
            state.flow.approach_cleared,
            "No ILS permission: {:?}",
            state
                .session
                .transcript
                .iter()
                .map(|entry| &entry.text)
                .collect::<Vec<_>>()
        );
        // At ten miles final the published Tower must be offered despite its
        // surface coverage ceiling, followed by check-in and landing permission.
        let target =
            openatc_core::arrival::target(state.arrival_profile.as_ref().unwrap(), "35R", "I35RY")
                .unwrap();
        let threshold = [target.latitude, target.longitude];
        let course =
            openatc_core::arrival::bearing([fixes[fi].latitude, fixes[fi].longitude], threshold);
        let position = openatc_core::arrival::destination(threshold, course + 180.0, 10.0);
        state.session.telemetry.latitude = position[0];
        state.session.telemetry.longitude = position[1];
        state.session.telemetry.ground_track_degrees = Some(course);
        for _ in 0..8 {
            tick(&mut state);
            acknowledge(&mut state);
        }
        assert_eq!(state.session.recommended_frequency_khz, tower.khz);
        state.session.telemetry.com1_khz = tower.khz;
        let response = request(
            &mut state,
            &tower,
            &Request {
                intent: "checkin".into(),
                ..Default::default()
            },
            &SpeechTag::default(),
        )
        .unwrap();
        assert_eq!(response["accepted"], true);
        acknowledge(&mut state);
        tick(&mut state);
        assert!(state.flow.landing_cleared);
        assert!(
            state
                .session
                .transcript
                .iter()
                .any(|entry| entry.text.contains("expect")
                    && entry.text.contains("ILS Yankee runway 35R"))
        );
        eprintln!(
            "Entry descent issued {:.1} NM before CI3RY",
            entry_distance.unwrap()
        );
        for entry in &state.session.transcript {
            eprintln!("{}", entry.text);
        }
    }
    #[test]
    fn surface_only_tower_does_not_take_an_uncleared_arrival() {
        let (mut state, approach, mut tower) = arrival_state();
        state.session.telemetry.latitude = -0.1;
        let center = Station {
            service: "Center".into(),
            khz: 123_000,
            ..approach
        };
        tower.receivable = true;
        tower.airport.clone_from(&state.session.plan.destination);
        tower.coverage.clear();
        assert!(next_station(&state, std::slice::from_ref(&tower), &center).is_none());
        state.flow.approach_cleared = true;
        assert_eq!(
            next_station(&state, std::slice::from_ref(&tower), &center)
                .unwrap()
                .khz,
            tower.khz
        );
    }
    #[test]
    fn routine_qnh_omits_units_but_low_pressure_keeps_them() {
        let mut state = crate::test_state();
        let station = Station {
            airport: "LFMN".into(),
            ..Default::default()
        };
        for (pressure, units) in [(1022.0, false), (993.0, true)] {
            state.sim_weather.insert(
                "LFMN".into(),
                (
                    json!({"pressureHpa":pressure,"windDegrees":95,"windKnots":7}),
                    std::time::Instant::now(),
                ),
            );
            let report = weather(&state, &station);
            assert!(report.contains(&format!("QNH {pressure:.0}")), "{report}");
            assert_eq!(report.contains("hectopascals"), units, "{report}");
        }
    }
    #[test]
    fn standard_pressure_level_does_not_bust_geometric_altitude() {
        let mut state = crate::test_state();
        state.settings.enforce_clearance_constraints = true;
        state.session.plan.departure = "LFMN".into();
        state.session.telemetry.on_ground = false;
        state.session.telemetry.altitude_feet = 18311.7;
        state.session.telemetry.indicated_altitude_feet = Some(18000.0);
        state.session.telemetry.pressure_altitude_feet = Some(18000.0);
        state.session.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 18000,
            acknowledged: true,
            sequence: 1,
            ..Default::default()
        });
        let station = Station {
            service: "Center".into(),
            ..Default::default()
        };
        for altitude in [18000.0, 17950.0, 18070.0, 18299.0, 18000.0] {
            state.session.telemetry.pressure_altitude_feet = Some(altitude);
            state.flow.last_correction = Some(
                std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(31))
                    .unwrap(),
            );
            compliance(&mut state, &station);
            assert!(state.session.transcript.is_empty());
        }
        state.session.telemetry.pressure_altitude_feet = Some(18500.0);
        compliance(&mut state, &station);
        assert!(
            state.session.transcript.is_empty(),
            "A sustained deviation needs grace"
        );
        state.flow.last_correction = Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(31))
                .unwrap(),
        );
        compliance(&mut state, &station);
        assert_eq!(state.session.transcript.len(), 1);
        state.session.telemetry.vertical_speed_fpm = -500.0;
        state.flow.last_correction = Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(31))
                .unwrap(),
        );
        compliance(&mut state, &station);
        assert_eq!(
            state.session.transcript.len(),
            1,
            "A correcting descent is compliant"
        );
    }
    #[test]
    fn altitude_corrections_continue_without_cancelling_ifr() {
        for assistance in [false, true] {
            let mut state = crate::test_state();
            state.settings.enforce_clearance_constraints = true;
            state.settings.terminate_after_warnings = assistance;
            state.session.telemetry.on_ground = false;
            state.session.telemetry.altitude_feet = 6000.0;
            state.session.clearance = Some(openatc_core::state::Clearance {
                sequence: 1,
                altitude_feet: 5000,
                acknowledged: true,
                ..Default::default()
            });
            let station = Station {
                service: "Center".into(),
                ..Default::default()
            };
            compliance(&mut state, &station);
            for count in 1..=5 {
                state.flow.last_correction = Some(
                    std::time::Instant::now()
                        .checked_sub(std::time::Duration::from_secs(31))
                        .unwrap(),
                );
                compliance(&mut state, &station);
                assert_eq!(state.session.transcript.len(), count);
                assert!(!state.session.clearance_cancelled);
                assert_eq!(
                    state.session.clearance.as_ref().unwrap().altitude_feet,
                    5000
                );
            }
            state.session.telemetry.altitude_feet = 5000.0;
            compliance(&mut state, &station);
            assert!(state.flow.last_correction.is_none());
        }
    }
    fn station(name: &str, left: f64, right: f64, distance: f64) -> Station {
        Station {
            name: name.into(),
            service: "Approach".into(),
            receivable: true,
            distance_nm: distance,
            coverage: vec![Coverage {
                floor: 0.0,
                ceiling: 10000.0,
                points: vec![[-1.0, left], [1.0, left], [1.0, right], [-1.0, right]],
            }],
            ..Default::default()
        }
    }
    #[test]
    fn a_duplicate_station_record_does_not_hand_off_to_the_same_controller() {
        let mut state = crate::test_state();
        state.session.demo = false;
        state.session.has_departed = true;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
        state.session.plan.destination = "KMCO".into();
        state.session.plan.arrival_runway = "36R".into();
        state.session.plan.approach = "I36R".into();
        state.session.telemetry = openatc_core::state::Telemetry {
            on_ground: false,
            position_valid: true,
            radio_power: true,
            com1_khz: 124_800,
            latitude: 28.43,
            longitude: -81.31,
            altitude_feet: 10000.0,
            height_agl_feet: 10000.0,
            ground_speed_knots: 240.0,
            heading_degrees: 360.0,
            ground_track_degrees: Some(360.0),
            traffic: Some(vec![]),
            ..Default::default()
        };
        let facility = |airport: &str, ceiling: f64| Station {
            airport: airport.into(),
            name: "Orlando Approach".into(),
            service: "Approach".into(),
            khz: 124_800,
            latitude: 28.43,
            longitude: -81.31,
            coverage: vec![Coverage {
                floor: 0.0,
                ceiling,
                points: vec![[27.0, -83.0], [29.0, -83.0], [29.0, -80.0], [27.0, -80.0]],
            }],
            ..Default::default()
        };
        // The tuned record does not own this altitude; the adjacent airport
        // record with the same name and frequency does. That is one controller,
        // so no transfer may be issued.
        state.stations = vec![facility("KMCO", 5000.0), facility("KSFB", 20000.0)];
        for _ in 0..8 {
            tick(&mut state);
        }
        assert!(
            state.session.transcript.iter().all(|entry| {
                !entry.text.contains("Contact")
                    && !entry.text.contains("contact")
                    && !entry.text.contains("124.800")
            }),
            "{:?}",
            state
                .session
                .transcript
                .iter()
                .map(|entry| &entry.text)
                .collect::<Vec<_>>()
        );
        assert!(state.flow.pending_station.is_empty());
    }

    #[test]
    #[ignore = "requires installed X-Plane navigation data"]
    fn installed_center_coverage_remains_available_at_cruise_levels() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").expect("simulator root");
        let stations = openatc_core::stations::load_index(&root).expect("station index");
        for (label, latitude, longitude) in [
            ("KMCO", 28.43, -81.31),
            ("KRDU", 35.88, -78.79),
            ("central US", 39.0, -98.0),
        ] {
            let telemetry = openatc_core::state::Telemetry {
                position_valid: true,
                radio_power: true,
                on_ground: false,
                latitude,
                longitude,
                altitude_feet: 35000.0,
                height_agl_feet: 35000.0,
                indicated_altitude_feet: Some(35000.0),
                ground_speed_knots: 450.0,
                heading_degrees: 90.0,
                ground_track_degrees: Some(90.0),
                ..Default::default()
            };
            let nearby = openatc_core::stations::nearby(&stations, &telemetry);
            let center = nearby.iter().find(|station| {
                station.service == "Center"
                    && station
                        .coverage
                        .iter()
                        .any(|volume| volume.contains(&telemetry))
            });
            assert!(
                center.is_some(),
                "No published Center owns FL350 at {label}; ceilings came from {root}"
            );
            let mut state = crate::test_state();
            state
                .session
                .on_event(openatc_core::aircraft::AircraftEvent::Cruise);
            state.session.has_departed = true;
            state.session.plan.cruise_feet = 35000;
            state.session.telemetry = telemetry;
            state.stations.clone_from(&nearby);
            let current = center.unwrap().clone();
            let next = next_station(&state, &nearby, &current)
                .unwrap_or_else(|| panic!("No controller is reachable at FL350 over {label}"));
            assert!(
                next.coverage
                    .iter()
                    .any(|volume| volume.contains(&state.session.telemetry))
                    || next.service == "Center",
                "FL350 over {label} selected {} ({})",
                next.name,
                next.service
            );
        }
    }

    #[test]
    fn a_climb_is_not_capped_by_the_current_low_sector_ceiling() {
        let mut state = crate::test_state();
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Cruise);
        state.session.has_departed = true;
        state.session.plan.cruise_feet = 35000;
        state.session.telemetry = openatc_core::state::Telemetry {
            position_valid: true,
            on_ground: false,
            latitude: 40.0,
            longitude: -90.0,
            altitude_feet: 24000.0,
            height_agl_feet: 24000.0,
            ground_speed_knots: 300.0,
            heading_degrees: 90.0,
            ground_track_degrees: Some(90.0),
            ..Default::default()
        };
        state.session.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 24000,
            acknowledged: true,
            ..Default::default()
        });
        let volume = |floor: f64, ceiling: f64| Coverage {
            floor,
            ceiling,
            points: vec![[38.0, -92.0], [42.0, -92.0], [42.0, -88.0], [38.0, -88.0]],
        };
        let low = Station {
            name: "Kansas City Center".into(),
            service: "Center".into(),
            khz: 118_700,
            latitude: 40.0,
            longitude: -90.0,
            coverage: vec![volume(0.0, 24000.0)],
            receivable: true,
            ..Default::default()
        };
        let high = Station {
            name: "Kansas City Center".into(),
            service: "Center".into(),
            khz: 125_200,
            latitude: 40.0,
            longitude: -90.0,
            coverage: vec![volume(24000.0, 60000.0)],
            receivable: true,
            ..Default::default()
        };
        let next = next_station(&state, &[low.clone(), high.clone()], &low)
            .expect("upper sector must be reachable");
        assert_eq!(next.khz, high.khz);
        assert!(next.coverage.iter().any(|v| v.ceiling > 24000.0));
    }

    #[test]
    fn arrival_handoff_does_not_flap_at_the_approach_ceiling() {
        let mut state = crate::test_state();
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
        state.session.plan.destination = "TEST".into();
        state.session.telemetry = openatc_core::state::Telemetry {
            position_valid: true,
            on_ground: false,
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 5500.0,
            height_agl_feet: 5500.0,
            ground_speed_knots: 220.0,
            heading_degrees: 90.0,
            ground_track_degrees: Some(90.0),
            ..Default::default()
        };
        let volume = |ceiling: f64| Coverage {
            floor: 0.0,
            ceiling,
            points: vec![[-1.0, -1.0], [-1.0, 1.0], [1.0, 1.0], [1.0, -1.0]],
        };
        let center = Station {
            name: "Washington Center".into(),
            service: "Center".into(),
            khz: 118_700,
            receivable: true,
            coverage: vec![volume(20000.0)],
            ..Default::default()
        };
        let approach = Station {
            airport: "TEST".into(),
            name: "Raleigh Approach".into(),
            service: "Approach".into(),
            khz: 124_800,
            receivable: true,
            coverage: vec![volume(5000.0)],
            ..Default::default()
        };
        let next = next_station(&state, &[center.clone(), approach.clone()], &center)
            .expect("Center must retain the descent above the approach ceiling");
        assert_eq!(next.khz, center.khz);
        state.session.telemetry.altitude_feet = 4500.0;
        state.session.telemetry.height_agl_feet = 4500.0;
        let next = next_station(&state, &[center.clone(), approach.clone()], &center)
            .expect("Approach takes the flight inside its ceiling");
        assert_eq!(next.khz, approach.khz);
    }

    #[test]
    fn a_flight_below_its_last_level_gets_an_approach_clearance_not_a_climb() {
        let (mut state, approach, _) = arrival_state();
        state.settings.enforce_clearance_constraints = true;
        state.session.telemetry.latitude = -0.1;
        state.session.telemetry.altitude_feet = 4200.0;
        state.session.telemetry.height_agl_feet = 4200.0;
        state.session.telemetry.indicated_altitude_feet = Some(4200.0);
        state.session.telemetry.ground_speed_knots = 160.0;
        state.session.telemetry.heading_degrees = 0.0;
        state.session.telemetry.ground_track_degrees = Some(0.0);
        state.session.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 5000,
            acknowledged: true,
            sequence: 1,
            ..Default::default()
        });
        assert!(
            arrival_tick(&mut state, &approach),
            "An approach clearance must resolve a flight below its last level"
        );
        assert!(state.flow.approach_cleared);
        assert!(
            state
                .session
                .transcript
                .iter()
                .any(|entry| entry.text.to_ascii_lowercase().contains("approach"))
        );
        state.flow.clearance_sequence = 1;
        state.flow.last_correction = Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(31))
                .unwrap(),
        );
        let before = state.session.transcript.len();
        compliance(&mut state, &approach);
        assert_eq!(
            state.session.transcript.len(),
            before,
            "The superseded level must not be demanded after the approach clearance"
        );
    }

    #[test]
    fn published_owner_wins_over_a_nearer_unrelated_airport() {
        let mut state = crate::test_state();
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Departure);
        state.session.has_departed = true;
        state.session.telemetry = openatc_core::state::Telemetry {
            on_ground: false,
            altitude_feet: 5000.0,
            height_agl_feet: 5000.0,
            longitude: 0.5,
            ground_speed_knots: 120.0,
            ground_track_degrees: Some(90.0),
            ..Default::default()
        };
        let old = station("Old", -1.0, 0.0, 1.0);
        let wrong = station("Wrong", 2.0, 3.0, 0.1);
        let owner = station("Owner", 0.0, 1.0, 20.0);
        assert_eq!(
            next_station(&state, &[wrong, owner.clone()], &old)
                .unwrap()
                .name,
            "Owner"
        );
        assert_eq!(
            next_station(&state, std::slice::from_ref(&owner), &owner)
                .unwrap()
                .name,
            "Owner"
        );
    }
    #[test]
    fn lookahead_transfers_before_the_boundary() {
        let mut state = crate::test_state();
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Departure);
        state.session.has_departed = true;
        state.session.telemetry = openatc_core::state::Telemetry {
            on_ground: false,
            altitude_feet: 5000.0,
            height_agl_feet: 5000.0,
            longitude: -0.01,
            ground_speed_knots: 240.0,
            ground_track_degrees: Some(90.0),
            ..Default::default()
        };
        let old = station("Old", -1.0, 0.0, 1.0);
        let next = station("Next", 0.0, 1.0, 2.0);
        assert!(old.coverage[0].contains(&state.session.telemetry));
        assert_eq!(
            next_station(&state, &[old.clone(), next], &old)
                .unwrap()
                .name,
            "Next"
        );
    }
    fn arrival_state() -> (EngineState, Station, Station) {
        use openatc_core::{
            airport::{Airport, Procedure, Runway},
            state::{Point, Telemetry},
        };
        let mut state = crate::test_state();
        state.session.plan.destination = "TEST".into();
        state.session.plan.arrival_runway = "36".into();
        state.session.plan.approach = "I36".into();
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
        state.arrival_airport = Some(Airport {
            icao: "TEST".into(),
            runways: vec![Runway {
                first_name: "36".into(),
                second_name: "18".into(),
                first: Point::default(),
                second: Point {
                    north: 2000.0,
                    ..Default::default()
                },
                width: 45.0,
                ..Default::default()
            }],
            procedures: vec![Procedure {
                kind: "APPCH".into(),
                name: "I36".into(),
                transition: String::new(),
            }],
            ..Default::default()
        });
        state.session.telemetry = Telemetry {
            latitude: -0.15,
            on_ground: false,
            altitude_feet: 2000.0,
            height_agl_feet: 2000.0,
            ground_track_degrees: Some(0.0),
            traffic: Some(vec![]),
            ..Default::default()
        };
        let approach = Station {
            name: "Test Approach".into(),
            service: "Approach".into(),
            ..Default::default()
        };
        let tower = Station {
            name: "Test Tower".into(),
            service: "Tower".into(),
            khz: 118_100,
            ..Default::default()
        };
        (state, approach, tower)
    }

    #[test]
    fn cleared_final_descent_does_not_reinstate_old_level() {
        let (mut state, approach, _) = arrival_state();
        state.settings.enforce_clearance_constraints = true;
        state.session.telemetry.latitude = -0.1;
        state.session.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 5000,
            acknowledged: true,
            sequence: 1,
            ..Default::default()
        });
        assert!(arrival_tick(&mut state, &approach));
        state.flow.clearance_sequence = 1;
        state.flow.last_correction = Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(31))
                .unwrap(),
        );
        let before = state.session.transcript.len();
        compliance(&mut state, &approach);
        assert_eq!(state.session.transcript.len(), before);
        state.session.telemetry.longitude = 0.02;
        state.flow.last_correction = Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(31))
                .unwrap(),
        );
        compliance(&mut state, &approach);
        assert_eq!(
            state.session.transcript.len(),
            before + 1,
            "An off-final aircraft still needs level compliance"
        );
    }

    #[test]
    fn approach_clearance_waits_for_published_entry_level_amendment() {
        let (mut state, approach, _) = arrival_state();
        state.session.telemetry.latitude = -0.24;
        state.session.telemetry.altitude_feet = 5000.0;
        let mut airport = state.arrival_airport.clone().unwrap();
        airport.navaids.push(openatc_core::airport::Navaid {
            kind: "GS".into(),
            runway: "36".into(),
            glide_angle: 3.0,
            ..Default::default()
        });
        state.arrival_profile = Some(openatc_core::arrival::ArrivalAirport {
            airport,
            intercepts: vec![openatc_core::arrival::Intercept {
                procedure: "I36".into(),
                altitude_feet: 4000.0,
            }],
            ..Default::default()
        });
        state.session.clearance = Some(openatc_core::state::Clearance {
            altitude_feet: 5000,
            acknowledged: true,
            ..Default::default()
        });
        assert!(!arrival_tick(&mut state, &approach));
        assert!(!state.flow.approach_cleared);
        state.session.clearance.as_mut().unwrap().altitude_feet = 4000;
        assert!(
            arrival_tick(&mut state, &approach),
            "A valid entry clearance allows the approach while descending toward it"
        );
    }

    #[test]
    fn final_handoff_does_not_wait_for_tower_surface_altitude() {
        let (mut state, approach, mut tower) = arrival_state();
        state.session.telemetry.latitude = -0.19;
        state.session.telemetry.altitude_feet = 4000.0;
        tower.airport = "TEST".into();
        tower.receivable = true;
        tower.coverage = vec![Coverage {
            floor: 0.0,
            ceiling: 2000.0,
            points: vec![[-1.0, -1.0], [-1.0, 1.0], [1.0, 1.0], [1.0, -1.0]],
        }];
        assert!(!tower.coverage[0].contains(&state.session.telemetry));
        assert!(next_station(&state, &[tower.clone()], &approach).is_none());
        assert!(arrival_tick(&mut state, &approach));
        assert_eq!(
            next_station(&state, &[tower.clone()], &approach)
                .unwrap()
                .khz,
            tower.khz
        );
        state.session.telemetry.longitude = 0.02;
        assert!(
            next_station(&state, &[tower.clone()], &approach).is_none(),
            "Being near the airport is not final establishment"
        );
        state.session.telemetry.longitude = 0.0;
        tower.receivable = false;
        assert!(
            next_station(&state, &[tower], &approach).is_none(),
            "A handoff still requires radio reception"
        );
    }

    #[test]
    fn tower_checkin_receives_one_ready_or_withheld_landing_response() {
        for (blocked, radio, prepared) in [
            (false, openatc_core::state::Radio::Com1, false),
            (true, openatc_core::state::Radio::Com1, false),
            (false, openatc_core::state::Radio::Com2, true),
            (true, openatc_core::state::Radio::Com2, true),
        ] {
            let (mut state, _, tower) = arrival_state();
            state.flow.approach_cleared = true;
            state.session.arrival_wake_conflict = blocked;
            if prepared {
                add_transmission(
                    &mut state.session,
                    "COPILOT",
                    "Test Tower, TEST, established final",
                    &SpeechTag {
                        radio: Some(radio),
                        ..Default::default()
                    },
                );
            }
            let response = request(
                &mut state,
                &tower,
                &Request {
                    radio,
                    controller_initiated: prepared,
                    intent: "checkin".into(),
                    text: "Test Tower, TEST, established final".into(),
                    ..Default::default()
                },
                &SpeechTag {
                    position: tower.name.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(response["accepted"], true);
            assert_eq!(state.session.transcript.len(), 2);
            let controller = state.session.transcript.last().unwrap();
            assert_eq!(controller.speaker, "ATC");
            assert_eq!(controller.radio, Some(radio));
            assert!(!controller.text.to_lowercase().contains("roger"));
            assert_eq!(state.flow.landing_cleared, !blocked);
            assert_eq!(controller.text.contains("cleared to land"), !blocked);
            if !blocked {
                assert!(!controller.pilot_reply.is_empty());
                assert!(!arrival_tick(&mut state, &tower));
                assert_eq!(state.session.transcript.len(), 2);
            }
        }
        let (mut state, _, tower) = arrival_state();
        state.flow.approach_cleared = true;
        // Away from the protected final, acknowledge contact without inventing a clearance.
        state.session.telemetry.longitude = 1.0;
        request(
            &mut state,
            &tower,
            &Request {
                intent: "checkin".into(),
                text: "Test Tower, TEST".into(),
                ..Default::default()
            },
            &SpeechTag::default(),
        )
        .unwrap();
        assert_eq!(state.session.transcript.len(), 2);
        assert!(!state.flow.landing_cleared);
    }
    #[test]
    fn the_landing_clearance_reserves_a_stand_before_the_aircraft_lands() {
        use openatc_core::{airport::Parking, state::Point};
        let (mut state, _, tower) = arrival_state();
        if let Some(airport) = state.arrival_airport.as_mut() {
            airport.parking = vec![
                Parking {
                    name: "A1".into(),
                    kind: "gate".into(),
                    size: "C".into(),
                    point: Point {
                        east: 120.0,
                        north: 60.0,
                        height: 0.0,
                    },
                    ..Default::default()
                },
                Parking {
                    name: "B2".into(),
                    kind: "gate".into(),
                    size: "C".into(),
                    point: Point {
                        east: 180.0,
                        north: 60.0,
                        height: 0.0,
                    },
                    ..Default::default()
                },
            ];
        }
        state.session.telemetry.wingspan_meters = Some(30.0);
        state.flow.approach_cleared = true;
        request(
            &mut state,
            &tower,
            &Request {
                intent: "checkin".into(),
                text: "Test Tower, TEST".into(),
                ..Default::default()
            },
            &SpeechTag::default(),
        )
        .unwrap();
        assert!(state.flow.landing_cleared);
        assert_eq!(state.session.plan.arrival_stand, "A1");
        let clearance = state.session.transcript.last().unwrap().text.clone();
        assert!(clearance.contains("cleared to land"), "{clearance}");
        assert!(clearance.contains("A1"), "{clearance}");
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .pilot_reply
                .contains("A1"),
            "the pilot reads the stand back"
        );
    }

    #[test]
    fn tower_checkin_cannot_hide_a_pending_instruction_or_grant_landing() {
        let (mut state, _, tower) = arrival_state();
        state.flow.approach_cleared = true;
        let tag = SpeechTag {
            position: tower.name.clone(),
            ..Default::default()
        };
        add_transmission(&mut state.session, "ATC", "Maintain 4000 feet.", &tag);
        let sequence = state.session.transcript.last().unwrap().sequence;
        state.session.transcript.last_mut().unwrap().pilot_reply =
            "Maintain 4000 feet, TEST.".into();
        request(
            &mut state,
            &tower,
            &Request {
                intent: "checkin".into(),
                text: "Test Tower, TEST".into(),
                ..Default::default()
            },
            &tag,
        )
        .unwrap();
        assert!(!state.flow.landing_cleared);
        assert!(state.session.transcript.last().unwrap().information_only);
        let outstanding = state
            .session
            .transcript
            .iter()
            .rev()
            .find(|entry| entry.is_controller_instruction())
            .unwrap();
        assert_eq!(outstanding.sequence, sequence);
        assert_eq!(outstanding.pilot_reply, "Maintain 4000 feet, TEST.");
        assert!(!arrival_tick(&mut state, &tower));
    }
    #[test]
    fn approach_tower_checkin_and_landing_are_separate_permissions() {
        let (mut state, approach, tower) = arrival_state();
        state.flow.monitor.speed = Some(190.0);
        assert!(arrival_tick(&mut state, &approach));
        assert!(state.flow.monitor.speed.is_none());
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("normal speed")
        );
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .pilot_reply
                .contains("normal speed")
        );
        assert!(state.flow.approach_cleared);
        assert!(!state.flow.landing_cleared);
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("ILS runway 36")
        );
        state
            .session
            .transcript
            .last_mut()
            .unwrap()
            .pilot_reply
            .clear();
        assert!(!arrival_tick(&mut state, &tower));
        state.flow.checked_in_station = station_key(&tower);
        assert!(arrival_tick(&mut state, &tower));
        assert!(state.flow.landing_cleared);
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("cleared to land")
        );
    }

    #[test]
    fn runway_conflict_overrides_pending_landing_readback() {
        let (mut state, _, tower) = arrival_state();
        state.flow.approach_cleared = true;
        state.flow.arrival_requested = true;
        state.flow.checked_in_station = station_key(&tower);
        assert!(arrival_tick(&mut state, &tower));
        assert!(
            !state
                .session
                .transcript
                .last()
                .unwrap()
                .pilot_reply
                .is_empty()
        );
        state.session.telemetry.traffic = Some(vec![openatc_core::state::TrafficTarget {
            latitude: 0.005,
            on_ground: true,
            ..Default::default()
        }]);
        assert!(!protect_final(&mut state, &tower));
        state.session.telemetry.latitude = -0.03;
        state.session.telemetry.radio_busy = true;
        state.session.telemetry.position_valid = true;
        state.session.telemetry.radio_power = true;
        state.session.telemetry.com1_khz = tower.khz;
        state.session.demo = false;
        state.stations = vec![tower.clone()];
        assert!(urgent(&mut state));
        assert!(!state.flow.landing_cleared);
        assert!(state.flow.missed_approach);
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .to_lowercase()
                .contains("go around")
        );
        state.session.telemetry.traffic = Some(vec![]);
        assert!(!arrival_tick(&mut state, &tower));
        assert!(!protect_final(&mut state, &tower));
    }

    #[test]
    fn short_final_go_around_does_not_wait_for_retry_timer() {
        let (mut state, _, tower) = arrival_state();
        state.flow.approach_cleared = true;
        state.flow.arrival_requested = true;
        state.flow.checked_in_station = station_key(&tower);
        state.session.telemetry.traffic = None;
        assert!(arrival_tick(&mut state, &tower));
        assert!(state.flow.landing_retry.is_some());
        state.session.telemetry.latitude = -0.008;
        assert!(protect_final(&mut state, &tower));
        assert!(state.flow.missed_approach);
    }
    #[test]
    fn approach_identifiers_are_spoken_as_approach_names() {
        assert_eq!(approach_label("I19R"), "ILS runway 19R");
        assert_eq!(approach_label("R27"), "RNAV runway 27");
        assert_eq!(approach_label("I35RY"), "ILS Yankee runway 35R");
        assert_eq!(approach_label("I35RZ"), "ILS Zulu runway 35R");
        assert_eq!(approach_label("R09LX"), "RNAV X-ray runway 09L");
        assert_eq!(approach_label("L18Y"), "Localizer Yankee runway 18");
        assert_eq!(approach_label("V04"), "VOR runway 04");
        assert_eq!(approach_label("N22Z"), "NDB Zulu runway 22");
        assert_eq!(approach_label("not a procedure"), "not a procedure");
        assert_eq!(approach_label(""), "");
        let slots = [
            ("callsign", "C-GTLT".into()),
            ("approach", approach_label("I35RY")),
        ];
        for template in [
            "arrival_expect_approach",
            "arrival_approach_clearance",
            "pilot_approach_clearance",
        ] {
            let text = say(template, &slots);
            assert!(text.contains("ILS Yankee runway 35R"), "{text}");
            assert_eq!(text.matches("35R").count(), 1, "{text}");
            assert!(!text.contains("35RY"), "{text}");
        }
    }

    #[test]
    fn a_runway_vacate_report_is_not_attributed_to_departure() {
        let (mut state, _, mut tower) = arrival_state();
        tower.airport = "TEST".into();
        let departure = Station {
            airport: "TEST".into(),
            name: "Test Departure".into(),
            service: "Departure".into(),
            khz: 122_000,
            ..Default::default()
        };
        state.stations = vec![tower.clone(), departure.clone()];
        state.session.demo = false;
        state.session.has_departed = true;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Approach);
        let telemetry = openatc_core::state::Telemetry {
            on_ground: true,
            position_valid: true,
            radio_power: true,
            com1_khz: departure.khz,
            latitude: 0.005,
            traffic: Some(vec![]),
            ..Default::default()
        };
        state.session.telemetry = telemetry.clone();
        for _ in 0..4 {
            openatc_core::ops::update_flight_phase(&mut state.session, &telemetry).unwrap();
        }
        tick(&mut state);
        let vacate = state
            .session
            .transcript
            .iter()
            .find(|entry| entry.text.to_lowercase().contains("vacate"))
            .expect("vacate instruction");
        assert_eq!(vacate.position, tower.name);
        assert!(!vacate.position.contains("Departure"));
    }

    #[test]
    fn a_vacate_instruction_combines_the_ground_handoff_without_repeating_it() {
        use openatc_core::{
            airport::{Airport, Runway},
            state::{Point, Telemetry},
        };
        for separate_ground in [false, true] {
            let mut state = crate::test_state();
            state.settings.enforce_clearance_constraints = false;
            state.session.demo = false;
            state.session.has_departed = true;
            state
                .session
                .on_event(openatc_core::aircraft::AircraftEvent::Approach);
            state.session.plan.destination = "DEST".into();
            state.session.plan.arrival_runway = "36".into();
            state.arrival_airport = Some(Airport {
                icao: "DEST".into(),
                runways: vec![Runway {
                    first_name: "36".into(),
                    second_name: "18".into(),
                    first: Point::default(),
                    second: Point {
                        north: 2000.0,
                        ..Default::default()
                    },
                    width: 45.0,
                    ..Default::default()
                }],
                ..Default::default()
            });
            let tower = Station {
                airport: "DEST".into(),
                name: "Arrival Tower".into(),
                service: "Tower".into(),
                khz: 118_100,
                ..Default::default()
            };
            state.stations = vec![tower.clone()];
            if separate_ground {
                state.stations.push(Station {
                    airport: "DEST".into(),
                    name: "Arrival Ground".into(),
                    service: "Ground".into(),
                    khz: 121_900,
                    ..Default::default()
                });
            }
            let mut telemetry = Telemetry {
                on_ground: true,
                position_valid: true,
                radio_power: true,
                com1_khz: 118_100,
                latitude: 0.005,
                traffic: Some(vec![]),
                ..Default::default()
            };
            for _ in 0..4 {
                openatc_core::ops::update_flight_phase(&mut state.session, &telemetry).unwrap();
            }
            assert_eq!(state.session.phase, PhaseCode::Landed);
            tick(&mut state);
            assert_eq!(state.airport.as_ref().unwrap().icao, "DEST");
            assert_eq!(state.session.transcript.len(), 1);
            let vacate = state.session.transcript[0].text.to_lowercase();
            assert!(vacate.contains("vacate") || vacate.contains("exit runway"));
            if separate_ground {
                assert!(vacate.contains("arrival ground"), "{vacate}");
                assert!(vacate.contains("121.900"), "{vacate}");
                assert_eq!(
                    state.session.recommended_frequency_khz, 0,
                    "Ground handoff must wait until the runway is clear"
                );
            } else {
                assert!(!vacate.contains("arrival ground"), "{vacate}");
            }
            for _ in 0..5 {
                tick(&mut state);
            }
            assert_eq!(
                state.session.transcript.len(),
                1,
                "The combined handoff is not repeated while on the runway"
            );
            telemetry.longitude = 0.002;
            openatc_core::ops::update_flight_phase(&mut state.session, &telemetry).unwrap();
            for _ in 0..4 {
                tick(&mut state);
            }
            assert_eq!(
                state.session.transcript.len(),
                1,
                "No second Ground handoff after vacating"
            );
            if separate_ground {
                assert_eq!(
                    state.session.recommended_frequency_khz, 121_900,
                    "Ground handoff after the runway is clear"
                );
            }
        }
    }

    #[test]
    fn the_first_runway_exit_names_the_side_the_pilot_should_vacate() {
        use openatc_core::airport::{Runway, TaxiEdge, TaxiNode};
        use openatc_core::state::Point;

        let mut state = crate::test_state();
        let mut airport = openatc_core::airport::demo_airport();
        airport.icao = "LFRX".into();
        airport.runways = vec![Runway {
            first_name: "09".into(),
            second_name: "27".into(),
            first: Point {
                east: -1200.0,
                north: 0.0,
                height: 0.0,
            },
            second: Point {
                east: 1200.0,
                north: 0.0,
                height: 0.0,
            },
            first_displaced_meters: 0.0,
            second_displaced_meters: 0.0,
            width: 45.0,
        }];
        airport.nodes = vec![
            TaxiNode {
                id: 1,
                point: Point {
                    east: -1200.0,
                    north: 0.0,
                    height: 0.0,
                },
            },
            TaxiNode {
                id: 2,
                point: Point {
                    east: 300.0,
                    north: 0.0,
                    height: 0.0,
                },
            },
            TaxiNode {
                id: 3,
                point: Point {
                    east: 300.0,
                    north: 45.0,
                    height: 0.0,
                },
            },
        ];
        airport.edges = vec![
            TaxiEdge {
                first: 1,
                second: 2,
                name: "09".into(),
                runway: true,
                one_way: false,
                active_runways: String::new(),
                size: 'B',
            },
            TaxiEdge {
                first: 2,
                second: 3,
                name: "N1".into(),
                runway: false,
                one_way: false,
                active_runways: String::new(),
                size: 'B',
            },
        ];
        state.arrival_airport = Some(airport);
        state.session.plan.destination = "LFRX".into();
        let (side, taxiway) = runway_exit(&state, "09").expect("the first exit");
        assert_eq!(side, "left");
        assert_eq!(taxiway, "N1");
    }

    #[test]
    fn arrival_vacate_requires_the_destination_runway_and_resets_after_takeoff() {
        use openatc_core::{
            airport::{Airport, Runway},
            state::{Point, Telemetry},
        };
        let destination = Airport {
            icao: "DEST".into(),
            reference_latitude: 0.0,
            reference_longitude: 0.0,
            runways: vec![Runway {
                first_name: "36".into(),
                second_name: "18".into(),
                first: Point::default(),
                second: Point {
                    north: 2000.0,
                    ..Default::default()
                },
                width: 45.0,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut state = crate::test_state();
        state.session.demo = false;
        state.session.has_departed = true;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Landed);
        state.session.plan.destination = "DEST".into();
        state.session.plan.arrival_runway = "36".into();
        state.arrival_airport = Some(destination);
        state.stations = vec![Station {
            airport: "DEST".into(),
            name: "Arrival Tower".into(),
            service: "Tower".into(),
            khz: 118_100,
            ..Default::default()
        }];
        state.session.telemetry = Telemetry {
            on_ground: true,
            position_valid: true,
            radio_power: true,
            com1_khz: 118_100,
            latitude: 10.0,
            longitude: 10.0,
            ground_speed_knots: 10.0,
            traffic: Some(vec![]),
            ..Default::default()
        };
        // Slow movement far from the destination must not name its runway.
        assert_eq!(landed_runway(&state), None);
        for _ in 0..4 {
            tick(&mut state);
        }
        assert!(
            state
                .session
                .transcript
                .iter()
                .all(|entry| !entry.text.to_lowercase().contains("vacate"))
        );
        // On the planned runway the report is issued once, in the direction of travel.
        state.session.telemetry.latitude = 0.005;
        state.session.telemetry.longitude = 0.0;
        assert_eq!(landed_runway(&state).as_deref(), Some("36"));
        tick(&mut state);
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .to_lowercase()
                .contains("vacate runway 36")
        );
        state
            .session
            .transcript
            .last_mut()
            .unwrap()
            .pilot_reply
            .clear();
        state.session.telemetry.heading_degrees = 180.0;
        assert_eq!(landed_runway(&state).as_deref(), Some("18"));
        // Becoming airborne clears the report so a later landing reports on its own runway.
        state.session.telemetry.on_ground = false;
        state.session.telemetry.heading_degrees = 0.0;
        tick(&mut state);
        assert!(!state.flow.vacate_reported);
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 0.005;
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Landed);
        tick(&mut state);
        assert!(state.flow.vacate_reported);
    }
}

#[cfg(test)]
mod sequencing_tests {
    use super::*;

    fn departure_state() -> EngineState {
        let mut state = crate::test_state();
        state.session.demo = false;
        state.session.phase = PhaseCode::Taxi;
        state.session.plan.callsign = "TEST123".into();
        state.session.plan.runway = "09".into();
        state.session.plan.departure = "KTEST".into();
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.com1_khz = 118_700;
        state.airport = Some(openatc_core::airport::Airport {
            icao: "KTEST".into(),
            runways: vec![openatc_core::airport::Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                second: openatc_core::state::Point {
                    east: 3000.0,
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        });
        state.stations = vec![Station {
            airport: "KTEST".into(),
            name: "KTEST Tower".into(),
            service: "Tower".into(),
            khz: 118_700,
            ..Default::default()
        }];
        state.session.taxi_clearance = openatc_core::state::TaxiClearance {
            airport: "KTEST".into(),
            approved: true,
            guidance_complete: true,
            hold_short_runway: "09".into(),
            points: vec![openatc_core::state::Point::default()],
            takeoff_state: TakeoffState::Held,
            ..Default::default()
        };
        state
    }

    #[test]
    fn a_held_departure_is_cleared_without_another_ready_call() {
        let mut state = departure_state();
        takeoff_tick(&mut state);
        assert_eq!(
            state.session.taxi_clearance.takeoff_state,
            TakeoffState::Cleared
        );
        assert!(!state.session.taxi_clearance.approved);
        let entry = state
            .session
            .transcript
            .last()
            .expect("takeoff transmission");
        assert!(
            entry.text.to_lowercase().contains("cleared for takeoff"),
            "{}",
            entry.text
        );
        assert!(
            entry
                .pilot_reply
                .to_lowercase()
                .contains("cleared for takeoff"),
            "{}",
            entry.pilot_reply
        );
    }

    #[test]
    fn a_lined_up_departure_clears_when_the_wake_interval_expires() {
        let mut state = departure_state();
        state.session.taxi_clearance.takeoff_state = TakeoffState::LineUpAndWait;
        state.session.departure_wake_seconds = 22.0;
        takeoff_tick(&mut state);
        assert_eq!(
            state.session.taxi_clearance.takeoff_state,
            TakeoffState::LineUpAndWait
        );
        assert!(!state.flow.takeoff_cleared);
        state.session.departure_wake_seconds = 0.0;
        takeoff_tick(&mut state);
        assert_eq!(
            state.session.taxi_clearance.takeoff_state,
            TakeoffState::Cleared
        );
        assert!(state.flow.takeoff_cleared);
    }

    #[test]
    fn a_lined_up_aircraft_gets_one_wake_caution() {
        let mut state = departure_state();
        state.session.taxi_clearance.takeoff_state = TakeoffState::LineUpAndWait;
        state.session.departure_wake_seconds = 22.0;
        takeoff_tick(&mut state);
        let text = state
            .session
            .transcript
            .last()
            .expect("wake caution")
            .text
            .to_lowercase();
        assert!(text.contains("caution wake turbulence"), "{text}");
        let count = state.session.transcript.len();
        takeoff_tick(&mut state);
        assert_eq!(
            state.session.transcript.len(),
            count,
            "the wake caution must not repeat every tick"
        );
    }

    #[test]
    fn a_wake_line_up_names_the_wake_turbulence() {
        let mut state = departure_state();
        state.session.taxi_clearance.takeoff_state = TakeoffState::Held;
        state.session.departure_wake_seconds = 22.0;
        takeoff_tick(&mut state);
        assert_eq!(
            state.session.taxi_clearance.takeoff_state,
            TakeoffState::LineUpAndWait
        );
        let text = state
            .session
            .transcript
            .last()
            .expect("line up call")
            .text
            .to_lowercase();
        assert!(text.contains("line up and wait"), "{text}");
        assert!(text.contains("wake turbulence"), "{text}");
    }

    #[test]
    fn a_stale_copilot_reply_does_not_stop_the_takeoff_sequencer() {
        let mut state = departure_state();
        state.pending_copilot_reply = Some(crate::routes::PreparedReply {
            generation: state.session_generation,
            sequence: 0,
            frequency: 118_700,
            request: Request::default(),
        });
        takeoff_tick(&mut state);
        assert_eq!(
            state.session.taxi_clearance.takeoff_state,
            TakeoffState::Cleared
        );
        assert!(state.pending_copilot_reply.is_some());
    }

    #[test]
    fn a_hold_reply_is_not_rewritten_as_a_takeoff_clearance() {
        let mut state = departure_state();
        let mut result = openatc_core::apply::RequestOutcome {
            accepted: false,
            message: "Holding short of runway 09.".into(),
        };
        let station = state.stations[0].clone();
        takeoff_wording(&mut state, &station, &mut result);
        assert_eq!(result.message, "Holding short of runway 09.");
        assert!(!state.flow.takeoff_cleared);
    }

    #[test]
    fn a_committed_takeoff_clearance_gets_the_full_phrase() {
        let mut state = departure_state();
        state.session.taxi_clearance.takeoff_state = TakeoffState::Cleared;
        let mut result = openatc_core::apply::RequestOutcome {
            accepted: true,
            message: "Cleared for takeoff.".into(),
        };
        let station = state.stations[0].clone();
        takeoff_wording(&mut state, &station, &mut result);
        assert!(
            result
                .message
                .to_lowercase()
                .contains("cleared for takeoff"),
            "{}",
            result.message
        );
        assert!(
            result.message.to_lowercase().contains("runway 09"),
            "{}",
            result.message
        );
        assert!(state.flow.takeoff_cleared);
    }

    #[test]
    fn a_sequencing_instruction_never_reads_back_as_a_takeoff_clearance() {
        let mut state = crate::test_state();
        state.session.plan.callsign = "TEST123".into();
        state.session.plan.runway = "09".into();
        state
            .session
            .transcript
            .push(openatc_core::state::Transmission {
                radio: None,
                frequency: None,
                gap_ms: 0,
                information_only: false,
                pilot_reply: String::new(),
                background: false,
                chatter: false,
                speaker: "ATC".into(),
                text: String::new(),
                sequence: 0,
                position: String::new(),
                voice: String::new(),
                delivery: String::new(),
                speed: 1.0,
                urgent: false,
            });
        let mut result = openatc_core::apply::RequestOutcome {
            accepted: true,
            message: String::new(),
        };
        for (instruction, readback, expected) in [
            (
                "flight_line_up_and_wait",
                "pilot_line_up_and_wait_readback",
                "line up and wait",
            ),
            (
                "flight_hold_short_landing",
                "pilot_hold_short_readback",
                "short",
            ),
            (
                "flight_hold_short_traffic",
                "pilot_hold_short_readback",
                "holding in position",
            ),
        ] {
            return_with_reply(&mut state, &mut result, instruction, readback);
            let spoken = result.message.to_lowercase();
            assert!(spoken.contains(expected), "{spoken}");
            assert!(!spoken.contains("cleared for takeoff"), "{spoken}");
            let entry = state.session.transcript.last().expect("transcript entry");
            let reply = entry.pilot_reply.to_lowercase();
            assert!(
                reply.contains("short") || reply.contains("line up and wait"),
                "{reply}"
            );
            assert!(!reply.contains("cleared for takeoff"), "{reply}");
        }
    }
}

#[cfg(test)]
mod hold_tests {
    use super::*;

    #[test]
    fn the_player_hold_time_starts_at_the_hold_and_clears_away() {
        let mut state = crate::test_state();
        let airport = openatc_core::airport::demo_airport();
        state.session.phase = PhaseCode::Taxi;
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = airport.reference_latitude;
        state.session.telemetry.longitude = airport.reference_longitude;
        state.session.taxi_clearance = openatc_core::state::TaxiClearance {
            approved: true,
            takeoff_state: TakeoffState::Held,
            points: vec![openatc_core::state::Point::default()],
            sequence: 3,
            ..Default::default()
        };
        state.airport = Some(airport);
        observe_takeoff_hold(&mut state);
        assert!(state.flow.takeoff_hold_since.is_some());
        state.session.telemetry.latitude += 0.01;
        observe_takeoff_hold(&mut state);
        assert!(state.flow.takeoff_hold_since.is_none());
    }
}
