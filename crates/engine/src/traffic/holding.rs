//! Discretionary player delay holds before final, with explicit readback and onward clearance.
use crate::EngineState;
use openatc_core::aircraft::AircraftBehaviour;
use openatc_core::{
    arrival::{bearing, distance_nm},
    navigation,
    state::{PhaseCode, Request, Telemetry},
    stations::Station,
    traffic::{
        holding::Pattern,
        motion::{Route, Waypoint},
        sequencing::{self, Decision},
    },
};

pub(super) struct Hold {
    fix: usize,
    name: String,
    route: String,
    runway: String,
    station: Station,
    pattern: Pattern,
    level: f64,
    speed: f64,
    sequence: u32,
    accepted: bool,
    onward: bool,
    entered: bool,
    left_fix: bool,
    clear_since: Option<f64>,
    clock: f64,
    clock_changed: bool,
    paused: bool,
    expected: f64,
}

fn efc(zulu: f64) -> String {
    let minutes = ((zulu + 300.0) / 60.0).ceil().rem_euclid(1440.0);
    format!("{:02.0}{:02.0}", (minutes / 60.0).floor(), minutes % 60.0)
}
fn coordinate(point: Waypoint) -> [f64; 2] {
    [point.latitude, point.longitude]
}

fn protected(state: &EngineState, station: &Station, pattern: &Pattern) -> bool {
    // A radio horizon alone does not establish authority to issue vectors or a hold.
    if station.coverage.is_empty() {
        return false;
    }
    let floor = |a: Waypoint, b: Waypoint| {
        state
            .mora
            .corridor(coordinate(a), coordinate(b))
            .is_some_and(|floor| {
                a.altitude_feet >= f64::from(floor) + 1000.0
                    && b.altitude_feet >= f64::from(floor) + 1000.0
            })
            && [a, b].iter().all(|point| {
                let t = Telemetry {
                    latitude: point.latitude,
                    longitude: point.longitude,
                    altitude_feet: point.altitude_feet,
                    on_ground: false,
                    position_valid: true,
                    ..Default::default()
                };
                station.coverage.iter().any(|v| v.contains(&t))
                    && openatc_core::stations::reception_distance(station, &t).is_some()
            })
    };
    pattern
        .compile(floor)
        .and_then(|segment| Route::compile(vec![segment], 1000.0, 1000.0))
        .is_ok()
}

fn evaluate(state: &EngineState, position: Option<&Pattern>) -> Decision {
    let (Some(airport), Some(profiles)) =
        (&state.arrival_airport, state.traffic_profiles.current())
    else {
        return Decision::Unknown;
    };
    let mut t = state.session.telemetry.clone();
    if t.aircraft_icao.is_empty() {
        t.aircraft_icao.clone_from(&state.session.plan.aircraft);
    }
    if let Some(pattern) = position {
        t.latitude = pattern.fix.latitude;
        t.longitude = pattern.fix.longitude;
        t.altitude_feet = pattern.fix.altitude_feet;
        t.heading_degrees = pattern.inbound_degrees;
        t.ground_track_degrees = Some(pattern.inbound_degrees);
    }
    sequencing::assess(
        airport,
        &state.session.plan.arrival_runway,
        &t,
        &profiles,
        None,
        None,
        5.0,
        5.0,
    )
}

fn plan(state: &EngineState, station: &Station) -> Option<Hold> {
    let t = &state.session.telemetry;
    let airport = state.arrival_airport.as_ref()?;
    sequencing::Region::for_airport(&airport.icao)?;
    let clock = t.simulation_seconds?;
    t.zulu_seconds?;
    if state.flow.approach_cleared
        || t.vertical_speed_fpm.abs() > 200.0
        || state.flow.monitor.heading.is_some()
        || state.flow.monitor.deviation.is_some()
        || state
            .session
            .clearance
            .as_ref()
            .is_none_or(|c| !c.acknowledged)
    {
        return None;
    }
    let index = state.flow.monitor.next_fix.max(1);
    let fix = state.session.plan.fixes.get(index)?;
    if fix.identifier.is_empty() || !matches!(fix.path_terminator.as_str(), "" | "TF" | "IF" | "DF")
    {
        return None;
    }
    let level = (t.indicated_altitude_feet? / 1000.0).round() * 1000.0;
    // Restrict these holds to level flight below the transition to flight levels.
    if !(3000.0..10000.0).contains(&level)
        || (t.indicated_altitude_feet? - level).abs() > 100.0
        || fix.minimum_altitude_is_flight_level == Some(true)
        || fix.maximum_altitude_is_flight_level == Some(true)
        || fix
            .minimum_altitude_feet
            .is_some_and(|v| level < f64::from(v))
        || fix
            .maximum_altitude_feet
            .is_some_and(|v| level > f64::from(v))
    {
        return None;
    }
    let assigned = state
        .flow
        .monitor
        .step_target
        .unwrap_or(state.session.clearance.as_ref()?.altitude_feet);
    if (f64::from(assigned) - level).abs() > 100.0 {
        return None;
    }
    let profiles = state.traffic_profiles.current()?;
    let aircraft = profiles.aircraft_for(if t.aircraft_icao.is_empty() {
        &state.session.plan.aircraft
    } else {
        &t.aircraft_icao
    })?;
    let envelope = aircraft.spacing_speed.as_ref()?;
    let speed = t.indicated_speed_knots?;
    if !(envelope.minimum_knots..=envelope.maximum_knots.min(200.0)).contains(&speed)
        || fix.minimum_speed_knots.is_some_and(|s| speed < s)
        || fix.maximum_speed_knots.is_some_and(|s| speed > s)
    {
        return None;
    }
    let (weather, at) = state.sim_weather.get(&airport.icao)?;
    if at.elapsed().as_secs() > 180 || weather["windKnots"].as_f64()? > 20.0 {
        return None;
    }
    let atmosphere = openatc_core::traffic::air::Atmosphere::surface(
        weather["pressureHpa"].as_f64()?,
        weather["temperatureC"].as_f64()?,
        airport.elevation_feet,
    )
    .ok()?;
    let geometric = atmosphere.geometric(level, false).ok()?;
    if (geometric - t.altitude_feet).abs() > 200.0 {
        return None;
    }
    let own = [t.latitude, t.longitude];
    let target = [fix.latitude, fix.longitude];
    let inbound = bearing(own, target);
    let seconds = distance_nm(own, target) / t.ground_speed_knots * 3600.0;
    if !(60.0..=360.0).contains(&seconds)
        || navigation::turn(t.ground_track_degrees.unwrap_or(t.heading_degrees), inbound).abs()
            > 10.0
        || distance_nm(
            target,
            [airport.reference_latitude, airport.reference_longitude],
        ) < 12.0
    {
        return None;
    }
    let pattern = Pattern {
        fix: Waypoint {
            latitude: fix.latitude,
            longitude: fix.longitude,
            altitude_feet: geometric,
        },
        inbound_degrees: inbound,
        right: true,
        ground_knots: atmosphere.true_knots(speed, geometric).ok()?
            + weather["windKnots"].as_f64()?
            + 10.0,
        leg_seconds: 60.0,
        bank_degrees: 20.0,
        turn_rate_degrees_sec: 3.0,
    };
    if !protected(state, station, &pattern) {
        return None;
    }
    if !traffic_clear(state, &pattern, seconds) {
        return None;
    }
    Some(Hold {
        fix: index,
        name: fix.identifier.clone(),
        route: state.session.plan.route.clone(),
        runway: state.session.plan.arrival_runway.clone(),
        station: station.clone(),
        pattern,
        level,
        speed,
        sequence: 0,
        accepted: false,
        onward: false,
        entered: false,
        left_fix: false,
        clear_since: None,
        clock,
        clock_changed: false,
        paused: false,
        expected: clock + 300.0,
    })
}

pub(crate) fn active(state: &EngineState) -> bool {
    state.traffic.hold.is_some()
}
pub(crate) fn clear(state: &mut EngineState) {
    state.traffic.hold = None;
}

/// Observe even while the radio/flight controller is paused or busy.
pub(crate) fn observe(state: &mut EngineState) {
    let Some(hold) = state.traffic.hold.as_mut() else {
        return;
    };
    let t = &state.session.telemetry;
    let Some(now) = t.simulation_seconds else {
        return;
    };
    let delta = now - hold.clock;
    if (t.paused || hold.paused) && delta >= 0.0 {
        hold.clear_since = hold.clear_since.map(|at| at + delta);
        hold.expected += delta;
    } else if !(0.0..=30.0).contains(&delta) {
        hold.clear_since = None;
        hold.expected = now;
        hold.left_fix = false;
        hold.clock_changed = true;
    }
    hold.clock = now;
    hold.paused = t.paused;
}

fn traffic_clear(state: &EngineState, pattern: &Pattern, eta: f64) -> bool {
    if state.session.telemetry.traffic.is_none() && state.session.generated_traffic.is_empty() {
        return false;
    }
    let targets = state.traffic_targets();
    if targets.len() > 126 {
        return false;
    }
    // Geometry has already passed protection; use its samples for continuous encounter checks.
    let Ok(segment) = pattern.compile(|_, _| true) else {
        return false;
    };
    let mut seconds = eta;
    for path in segment.points.windows(2) {
        let end = seconds
            + distance_nm(coordinate(path[0]), coordinate(path[1])) / pattern.ground_knots * 3600.0;
        for target in targets.iter().take(126).filter(|t| !t.on_ground) {
            if !openatc_core::traffic::advisory::valid(target) {
                return false;
            }
            let Some(speed) = target
                .ground_speed_knots
                .filter(|s| (30.0..=400.0).contains(s))
            else {
                return false;
            };
            let at = |time| {
                let p = openatc_core::arrival::destination(
                    [target.latitude, target.longitude],
                    target.track_degrees,
                    speed * time / 3600.0,
                );
                Waypoint {
                    latitude: p[0],
                    longitude: p[1],
                    // No reliable future climb profile is supplied for observed traffic.
                    // Reserve this horizontal corridor at every level rather than assume
                    // an aircraft currently above/below the hold remains separated.
                    altitude_feet: 0.0,
                }
            };
            if openatc_core::traffic::separation::breach(
                [
                    Waypoint {
                        altitude_feet: 0.0,
                        ..path[0]
                    },
                    Waypoint {
                        altitude_feet: 0.0,
                        ..path[1]
                    },
                ],
                [at(seconds), at(end)],
                5.0,
                1500.0,
            ) != Ok(false)
            {
                return false;
            }
        }
        seconds = end;
    }
    true
}

/// Called only after the core has verified the exact readback and its radio sequence.
pub(crate) fn acknowledge(state: &mut EngineState, sequence: u32) {
    let Some(hold) = state.traffic.hold.as_mut() else {
        return;
    };
    if sequence != hold.sequence {
        return;
    }
    hold.accepted = true;
    if !hold.onward {
        state.flow.monitor.speed = None;
        state.traffic.speed = None;
    }
}

/// Preserve hold identity when the readback reminder repeats the same instruction.
pub(crate) fn reminder(state: &mut EngineState, previous: u32, sequence: u32) {
    if let Some(hold) = state
        .traffic
        .hold
        .as_mut()
        .filter(|h| h.sequence == previous && !h.accepted)
    {
        hold.sequence = sequence;
    }
}

pub(crate) fn request(state: &mut EngineState, station: &Station, request: &Request) -> bool {
    let Some(hold) = state.traffic.hold.as_ref() else {
        return false;
    };
    if !crate::flight::same_controller(&hold.station, station) {
        return false;
    }
    if ![
        "unable", "direct", "altitude", "descent", "approach", "landing", "heading",
    ]
    .contains(&request.intent.as_str())
    {
        return false;
    }
    let slots = [
        ("callsign", state.session.plan.callsign.clone()),
        ("fix", hold.name.clone()),
    ];
    let declined = request.intent == "unable" && !hold.accepted;
    if !request.controller_initiated {
        let callsign = state.session.plan.callsign.clone();
        openatc_core::apply::add_transmission(
            &mut state.session,
            &callsign,
            &request.text,
            &openatc_core::state::SpeechTag {
                radio: Some(request.radio),
                ..Default::default()
            },
        );
    }
    if declined {
        if let Some(last) = state
            .session
            .transcript
            .iter_mut()
            .rev()
            .find(|t| t.is_controller_instruction())
        {
            last.pilot_reply.clear();
        }
        clear(state);
        state.traffic.hold_declined = true;
        super::atc(state, station, "arrival_hold_unable", &slots[..1], "");
    } else {
        super::atc(
            state,
            station,
            "arrival_hold_amendment_required",
            &slots,
            "",
        );
        // Informational refusal must not erase the outstanding hold readback.
        if let Some(last) = state.session.transcript.last_mut() {
            last.information_only = true;
        }
    }
    true
}

/// The setting controls new discretionary holds only. Accepted holds require onward clearance.
pub(crate) fn tick(state: &mut EngineState, station: &Station) -> bool {
    observe(state);
    if state.session.telemetry.paused {
        return active(state);
    }
    if let Some(mut hold) = state.traffic.hold.take() {
        if state.session.telemetry.on_ground || state.flow.missed_approach {
            return false;
        }
        let t = &state.session.telemetry;
        let now = t.simulation_seconds.unwrap_or(hold.clock);
        let changed = std::mem::take(&mut hold.clock_changed);
        let distance = distance_nm([t.latitude, t.longitude], coordinate(hold.pattern.fix));
        let at_fix = distance < 0.7
            && navigation::turn(
                t.ground_track_degrees.unwrap_or(t.heading_degrees),
                hold.pattern.inbound_degrees,
            )
            .abs()
                < 20.0;
        if hold.accepted && at_fix {
            hold.entered = true;
        }
        if hold.entered && distance > 1.0 {
            hold.left_fix = true;
        }
        if hold.onward
            && hold.accepted
            && at_fix
            && hold.left_fix
            && !changed
            && hold.route == state.session.plan.route
            && hold.runway == state.session.plan.arrival_runway
        {
            state.flow.monitor.next_fix = hold.fix + 1;
            state.flow.monitor.previous = None;
            state
                .session
                .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
            state.traffic.spacing_path_required = false;
            return false;
        }
        let same = hold.route == state.session.plan.route
            && hold.runway == state.session.plan.arrival_runway
            && crate::flight::same_controller(&hold.station, station);
        let communication_ready =
            hold.accepted && !hold.onward && same && !changed && super::ready(state);
        let ready = communication_ready
            && protected(state, station, &hold.pattern)
            && traffic_clear(state, &hold.pattern, 0.0);
        let runway_clear = state.arrival_airport.as_ref().is_some_and(|airport| {
            let mut t = Telemetry {
                latitude: hold.pattern.fix.latitude,
                longitude: hold.pattern.fix.longitude,
                heading_degrees: hold.pattern.inbound_degrees,
                ground_track_degrees: Some(hold.pattern.inbound_degrees),
                ..state.session.telemetry.clone()
            };
            if state.session.telemetry.traffic.is_some() {
                t.traffic = Some(state.traffic_targets());
            }
            openatc_core::traffic::landing_blocked(airport, &hold.runway, &t).is_none()
        });
        if ready && runway_clear && evaluate(state, Some(&hold.pattern)) == Decision::Clear {
            let since = *hold.clear_since.get_or_insert(now);
            if now - since >= 20.0 && hold.entered {
                super::atc(
                    state,
                    station,
                    "arrival_hold_onward",
                    &[
                        ("callsign", state.session.plan.callsign.clone()),
                        ("fix", hold.name.clone()),
                    ],
                    "pilot_arrival_hold_onward",
                );
                hold.sequence = state.session.transcript.last().unwrap().sequence;
                hold.accepted = false;
                hold.onward = true;
            }
        } else {
            hold.clear_since = None;
        }
        if communication_ready
            && !hold.onward
            && now >= hold.expected
            && let Some(zulu) = state.session.telemetry.zulu_seconds
        {
            super::atc(
                state,
                station,
                "arrival_hold_extended",
                &[
                    ("callsign", state.session.plan.callsign.clone()),
                    ("fix", hold.name.clone()),
                    ("delay", efc(zulu)),
                ],
                "pilot_arrival_hold_extended",
            );
            hold.sequence = state.session.transcript.last().unwrap().sequence;
            hold.accepted = false;
            hold.expected = now + 300.0;
        }
        state.traffic.hold = Some(hold);
        return true;
    }
    if !state.settings.player_traffic_holding
        || state.traffic.hold_declined
        || state.session.emergency_declared
        || state.session.phase != PhaseCode::Arrival
        || !["Approach", "Center"].contains(&station.service.as_str())
        || !super::ready(state)
    {
        return false;
    }
    let needs_path = match evaluate(state, None) {
        Decision::PathRequired => true,
        Decision::Reduce(_) => state.traffic.speed_declined,
        _ => false,
    };
    if !needs_path {
        return false;
    }
    let Some(mut hold) = plan(state, station) else {
        return false;
    };
    let direction = [
        "north",
        "northeast",
        "east",
        "southeast",
        "south",
        "southwest",
        "west",
        "northwest",
    ][(((hold.pattern.inbound_degrees + 180.0).rem_euclid(360.0) + 22.5) / 45.0) as usize % 8];
    let variation = state
        .arrival_airport
        .as_ref()
        .and_then(|airport| state.sim_weather.get(&airport.icao))
        .and_then(|(weather, _)| weather["magneticVariationDegrees"].as_f64());
    let Some(variation) = variation.filter(|v| (-180.0..=180.0).contains(v)) else {
        return false;
    };
    let slots = [
        ("callsign", state.session.plan.callsign.clone()),
        ("fix", hold.name.clone()),
        ("direction", direction.into()),
        (
            "course",
            format!(
                "{:03.0}",
                (hold.pattern.inbound_degrees - variation).rem_euclid(360.0)
            ),
        ),
        ("altitude", format!("{:.0}", hold.level)),
        ("speed", format!("{:.0}", hold.speed)),
        ("delay", efc(state.session.telemetry.zulu_seconds.unwrap())),
    ];
    super::atc(
        state,
        station,
        "arrival_traffic_hold",
        &slots,
        "pilot_arrival_traffic_hold",
    );
    hold.sequence = state.session.transcript.last().unwrap().sequence;
    state.traffic.hold = Some(hold);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::{
        airport::{Airport, Runway},
        apply::RunwaySequence,
        state::{Clearance, Point, RouteFix, TrafficTarget},
        stations::Coverage,
    };
    fn scenario() -> (EngineState, Station) {
        let mut s = crate::test_state();
        s.session.plan.destination = "KTEST".into();
        s.session.plan.arrival_runway = "36".into();
        s.session.plan.aircraft = "A320".into();
        s.session.plan.callsign = "TEST123".into();
        s.session.plan.route = "DCT WAIT DCT END".into();
        s.session.plan.fixes = vec![
            RouteFix::default(),
            RouteFix {
                identifier: "WAIT".into(),
                latitude: -20.0 / 60.0,
                ..Default::default()
            },
            RouteFix {
                identifier: "END".into(),
                ..Default::default()
            },
        ];
        s.session
            .on_event(openatc_core::aircraft::AircraftEvent::Arrival);
        s.session.has_departed = true;
        s.session.clearance = Some(Clearance {
            altitude_feet: 6000,
            acknowledged: true,
            ..Default::default()
        });
        s.session.telemetry = Telemetry {
            latitude: -30.0 / 60.0,
            altitude_feet: 6000.0,
            indicated_altitude_feet: Some(6000.0),
            aircraft_icao: "A320".into(),
            indicated_speed_knots: Some(180.0),
            ground_speed_knots: 180.0,
            position_valid: true,
            on_ground: false,
            com1_khz: 123100,
            simulation_seconds: Some(100.0),
            zulu_seconds: Some(86300.0),
            traffic: Some(vec![TrafficTarget {
                id: 7,
                latitude: -12.0 / 60.0,
                altitude_feet: 6000.0,
                aircraft: "PA28".into(),
                on_ground: false,
                ground_speed_knots: Some(70.0),
                ..Default::default()
            }]),
            ..Default::default()
        };
        s.arrival_airport = Some(Airport {
            icao: "KTEST".into(),
            runways: vec![Runway {
                first_name: "36".into(),
                second_name: "18".into(),
                second: Point {
                    north: 3000.0,
                    ..Default::default()
                },
                width: 45.0,
                ..Default::default()
            }],
            ..Default::default()
        });
        for lat in -2..=1 {
            for lon in -2..=1 {
                s.mora.0.insert((lat, lon), 1000);
            }
        }
        s.sim_weather.insert("KTEST".into(), (serde_json::json!({"windKnots":0.0,"pressureHpa":1013.25,"temperatureC":15.0,"magneticVariationDegrees":0.0}), std::time::Instant::now()));
        let station = Station {
            service: "Approach".into(),
            name: "Test Approach".into(),
            airport: "KTEST".into(),
            khz: 123100,
            coverage: vec![Coverage {
                floor: 0.0,
                ceiling: 20000.0,
                points: vec![[-2.0, -2.0], [-2.0, 2.0], [2.0, 2.0], [2.0, -2.0]],
            }],
            ..Default::default()
        };
        s.stations.push(station.clone());
        (s, station)
    }
    fn readback(s: &mut EngineState) {
        let request = openatc_core::readback::auto_reply(&s.session).expect("actionable readback");
        let tag = openatc_core::state::SpeechTag {
            position: s
                .session
                .transcript
                .iter()
                .rev()
                .find(|t| t.is_controller_instruction())
                .unwrap()
                .position
                .clone(),
            ..Default::default()
        };
        let realism = openatc_core::intents::Realism {
            strict_readbacks: true,
            ..Default::default()
        };
        let region = openatc_core::regions::builtin_region("KTEST");
        let result = openatc_core::apply::apply_request(
            &mut s.session,
            &openatc_core::apply::RequestContext {
                squawk: "1200",
                request: &request,
                runway_taxi_authorized: false,
                airport: None,
                atc_tag: &tag,
                pilot_tag: &tag,
                realism: &realism,
                units: openatc_core::UnitSystem::Imperial,
                region: &region,
                traffic: None,
                runway_sequence: RunwaySequence::Clear,
            },
        );
        assert!(result.accepted, "{}", result.message);
        acknowledge(s, request.clearance_sequence);
    }
    #[test]
    fn delay_hold_readback_setting_pause_and_onward_fix_crossing() {
        let (mut s, station) = scenario();
        assert_eq!(evaluate(&s, None), Decision::PathRequired);
        s.settings.player_traffic_holding = false;
        assert!(!tick(&mut s, &station));
        s.settings.player_traffic_holding = true;
        assert!(tick(&mut s, &station));
        assert!(!s.traffic.hold.as_ref().unwrap().accepted);
        assert!(
            s.session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("0004 Zulu")
        );
        let seq = s.traffic.hold.as_ref().unwrap().sequence;
        acknowledge(&mut s, seq + 1);
        assert!(!s.traffic.hold.as_ref().unwrap().accepted);
        readback(&mut s);
        assert!(s.traffic.hold.as_ref().unwrap().accepted);
        s.settings.player_traffic_holding = false;
        s.session.telemetry.latitude = -20.0 / 60.0;
        s.session.telemetry.simulation_seconds = Some(101.0);
        assert!(tick(&mut s, &station));
        assert!(s.traffic.hold.as_ref().unwrap().entered);
        s.session.telemetry.traffic = Some(vec![]);
        s.session.telemetry.latitude = -21.5 / 60.0;
        s.session.telemetry.simulation_seconds = Some(102.0);
        tick(&mut s, &station);
        s.session.telemetry.paused = true;
        s.session.telemetry.simulation_seconds = Some(122.0);
        tick(&mut s, &station);
        assert!(!s.traffic.hold.as_ref().unwrap().onward);
        s.session.telemetry.paused = false;
        tick(&mut s, &station);
        assert!(!s.traffic.hold.as_ref().unwrap().onward);
        s.session.telemetry.simulation_seconds = Some(143.0);
        tick(&mut s, &station);
        assert!(s.traffic.hold.as_ref().unwrap().onward);
        assert!(!s.traffic.hold.as_ref().unwrap().accepted);
        readback(&mut s);
        s.session.telemetry.latitude = -20.0 / 60.0;
        s.session.telemetry.simulation_seconds = Some(144.0);
        let original = s.session.plan.route.clone();
        s.session.plan.route.push_str(" AMENDED");
        assert!(
            tick(&mut s, &station),
            "old onward clearance cannot advance a replacement route"
        );
        assert!(active(&s));
        s.session.plan.route = original;
        assert!(!tick(&mut s, &station));
        assert!(!active(&s));
        assert_eq!(s.flow.monitor.next_fix, 2);
        assert!(!s.flow.approach_cleared && !s.flow.landing_cleared);
    }
    #[test]
    fn hold_refuses_missing_protection_weather_and_unable_never_means_permission() {
        let (mut s, station) = scenario();
        s.mora.0.clear();
        assert!(!tick(&mut s, &station));
        let (mut s, mut station) = scenario();
        station.coverage.clear();
        assert!(!tick(&mut s, &station));
        let (mut s, station) = scenario();
        s.sim_weather.clear();
        assert!(!tick(&mut s, &station));
        let (mut s, station) = scenario();
        assert!(tick(&mut s, &station));
        assert!(request(
            &mut s,
            &station,
            &Request {
                intent: "unable".into(),
                ..Default::default()
            }
        ));
        assert!(!active(&s));
        assert!(!tick(&mut s, &station));
        assert!(!s.flow.approach_cleared && !s.flow.landing_cleared);
    }
    #[test]
    fn hold_does_not_assume_vertical_separation_or_release_on_clock_jump() {
        let (mut s, station) = scenario();
        let hold = plan(&s, &station).unwrap();
        s.session.telemetry.traffic = Some(vec![TrafficTarget {
            latitude: hold.pattern.fix.latitude,
            longitude: hold.pattern.fix.longitude,
            altitude_feet: 20000.0,
            ground_speed_knots: Some(180.0),
            on_ground: false,
            ..Default::default()
        }]);
        assert!(!traffic_clear(&s, &hold.pattern, 0.0));
        let (mut s, station) = scenario();
        assert!(tick(&mut s, &station));
        readback(&mut s);
        s.session.telemetry.traffic = Some(vec![]);
        s.session.telemetry.latitude = -20.0 / 60.0;
        s.session.telemetry.simulation_seconds = Some(101.0);
        tick(&mut s, &station);
        s.session.telemetry.simulation_seconds = Some(500.0);
        tick(&mut s, &station);
        assert!(!s.traffic.hold.as_ref().unwrap().onward);
        let other = Station {
            khz: 125000,
            ..station.clone()
        };
        assert!(!request(
            &mut s,
            &other,
            &Request {
                intent: "unable".into(),
                ..Default::default()
            }
        ));
        assert!(active(&s));
    }
}
