//! Read-back speed amendments for pre-final movement on already protected routes.
use super::{Runtime, flight::Prepared, radio::Exchange};
use openatc_core::{
    state::Telemetry,
    traffic::{
        motion::Stage,
        sequencing::{self, Decision},
    },
};
use std::{collections::BTreeMap, sync::Arc, time::Instant};

#[derive(Clone)]
pub(super) struct Instruction {
    plan: Arc<Prepared>,
    limits: BTreeMap<usize, f64>,
    indicated: f64,
}

fn candidate(state: &crate::EngineState, id: u32) -> Option<Instruction> {
    let runtime = &state.generated;
    let pose = runtime.world.snapshot().into_iter().find(|p| p.id == id)?;
    if pose.recovering
        || !matches!(pose.stage, Stage::Cruise | Stage::Descent)
        || runtime.world.arrival_speed_restricted(id)
    {
        return None;
    }
    let plan = runtime.flights.get(&id)?;
    let floor = plan.spacing_floor?;
    let report = plan.vacated_reports.range(pose.segment..).next()?.1;
    let airport = runtime.surface_airport(&report.airport)?;
    if !runtime.environments.get(&airport.icao)?.fresh() {
        return None;
    }
    let profiles = state.traffic_profiles.current()?;
    let frame = runtime.frames.iter().find(|f| f.target.id == id)?;
    let reference = *plan.airspeeds.get(&pose.segment)?;
    let profile = profiles.aircraft_for(&frame.target.aircraft)?;
    // Do not speak a segment's nominal IAS as the current IAS during a speed transition.
    if (reference
        .ground_knots(pose.position.altitude_feet, pose.heading)
        .ok()?
        - pose.knots)
        .abs()
        > 5.0
    {
        return None;
    }
    let position = openatc_core::traffic::approach_position(
        airport,
        &report.runway,
        pose.position.latitude,
        pose.position.longitude,
    )?;
    if !(profiles.spacing.start_nm..=profiles.spacing.end_nm).contains(&(-position.0 / 1852.0))
        || pose.position.altitude_feet >= profiles.spacing.maximum_altitude_feet
    {
        return None;
    }
    let mut targets = super::external_targets(runtime, &state.session.telemetry);
    targets.extend(
        runtime
            .frames
            .iter()
            .filter(|f| f.target.id != id)
            .map(|f| f.target.clone()),
    );
    let observed = Telemetry {
        aircraft_icao: frame.target.aircraft.clone(),
        latitude: pose.position.latitude,
        longitude: pose.position.longitude,
        altitude_feet: pose.position.altitude_feet,
        ground_speed_knots: pose.knots,
        indicated_speed_knots: Some(reference.indicated_knots),
        heading_degrees: pose.heading,
        ground_track_degrees: Some(pose.heading),
        on_ground: false,
        position_valid: true,
        traffic: Some(targets),
        ..Default::default()
    };
    let final_index = (pose.segment..512).find(|&index| {
        plan.route
            .segment(index)
            .is_none_or(|s| !matches!(s.stage, Stage::Cruise | Stage::Descent))
    })?;
    let final_segment = plan.route.segment(final_index)?;
    if final_segment.stage != Stage::Approach {
        return None;
    }
    let final_start = final_segment.points.first()?;
    let release = -openatc_core::traffic::approach_position(
        airport,
        &report.runway,
        final_start.latitude,
        final_start.longitude,
    )?
    .0 / 1852.0;
    let Decision::Reduce(indicated) = sequencing::assess(
        airport,
        &report.runway,
        &observed,
        &profiles,
        Some(floor.max(profile.spacing_speed.as_ref()?.minimum_knots)),
        None,
        release.max(5.0),
        // Explicit simulated terminal FUSION service, otherwise the 5 NM floor.
        if runtime
            .controllers
            .get(&id)
            .is_some_and(|s| s.service == "Approach")
        {
            3.0
        } else {
            5.0
        },
    ) else {
        return None;
    };
    let mut limits = BTreeMap::new();
    for index in pose.segment..final_index {
        let segment = plan.route.segment(index)?;
        let reference = plan.airspeeds.get(&index)?;
        let mut limit = segment.knots;
        for points in segment.points.windows(2) {
            let course = openatc_core::arrival::bearing(
                [points[0].latitude, points[0].longitude],
                [points[1].latitude, points[1].longitude],
            );
            let speed = openatc_core::traffic::trajectory::Airspeed {
                indicated_knots: indicated,
                ..*reference
            };
            limit = limit.min(speed.ground_knots(points[0].altitude_feet, course).ok()?);
        }
        // One movement cap covers the entire segment. Refuse it if its IAS equivalent
        // at any sampled altitude/course would fall below the aircraft/published floor.
        for points in segment.points.windows(2) {
            let course = openatc_core::arrival::bearing(
                [points[0].latitude, points[0].longitude],
                [points[1].latitude, points[1].longitude],
            );
            let minimum = openatc_core::traffic::trajectory::Airspeed {
                indicated_knots: floor.max(profile.spacing_speed.as_ref()?.minimum_knots),
                ..*reference
            };
            for point in points {
                if limit + 0.01 < minimum.ground_knots(point.altitude_feet, course).ok()? {
                    return None;
                }
            }
        }
        limits.insert(index, limit);
    }
    Some(Instruction {
        plan: plan.clone(),
        limits,
        indicated,
    })
}

fn checked(
    runtime: &Runtime,
    telemetry: &Telemetry,
    id: u32,
    instruction: &Instruction,
) -> Option<openatc_core::traffic::motion::World> {
    if runtime
        .flights
        .get(&id)
        .is_none_or(|p| !Arc::ptr_eq(p, &instruction.plan))
    {
        return None;
    }
    let mut probe = super::separation::accepted_world(runtime);
    if !probe.restrict_arrival_speed(probe.generation(), id, &instruction.limits) {
        return None;
    }
    // Include the current recovery and every other aircraft, especially a faster follower.
    if !super::separation::forecast_clear(runtime, telemetry, id, &probe, 900.0, None).ok()? {
        return None;
    }
    Some(probe)
}

pub(super) fn exchange(state: &mut crate::EngineState) -> Option<Exchange> {
    let now = state.session.telemetry.simulation_seconds?;
    if state
        .generated
        .last_speed_check
        .is_some_and(|at| (0.0..5.0).contains(&(now - at)))
    {
        return None;
    }
    state.generated.last_speed_check = Some(now);
    let previous = state.generated.last_speed_aircraft;
    let ids: Vec<_> = state
        .generated
        .active_flights
        .range((
            std::ops::Bound::Excluded(previous),
            std::ops::Bound::Unbounded,
        ))
        .chain(state.generated.active_flights.range(..=previous))
        .copied()
        .collect();
    for id in ids {
        let Some(instruction) = candidate(state, id) else {
            continue;
        };
        let Some(station) = state.generated.controllers.get(&id) else {
            continue;
        };
        if !["Approach", "Center"].contains(&station.service.as_str()) {
            continue;
        }
        let Some(frame) = state
            .generated
            .frames
            .iter()
            .find(|f| f.target.id == id && f.target.com1_khz == Some(station.khz))
        else {
            continue;
        };
        state.generated.last_speed_aircraft = id;
        // At most one full movement forecast per interval; rotate fairly on refusal.
        checked(&state.generated, &state.session.telemetry, id, &instruction)?;
        let slots = [
            ("callsign", frame.target.callsign.clone()),
            ("speed", format!("{:.0}", instruction.indicated)),
        ];
        return Some(Exchange {
            speed: Some(instruction),
            id,
            segment: state
                .generated
                .world
                .snapshot()
                .iter()
                .find(|p| p.id == id)?
                .segment,
            station: station.clone(),
            callsign: frame.target.callsign.clone(),
            messages: vec![
                (
                    true,
                    openatc_core::dialogue::say("generated_arrival_speed", &slots),
                ),
                (
                    false,
                    openatc_core::dialogue::say("pilot_generated_arrival_speed", &slots),
                ),
            ],
            next: 0,
            due: Instant::now(),
            grant: false,
            tune_after: None,
            sector_transfer: None,
            recovery_from: None,
            rejoin: false,
        });
    }
    None
}

/// Recheck current traffic after the readback before changing movement.
pub(super) fn accept(
    runtime: &mut Runtime,
    telemetry: &Telemetry,
    id: u32,
    instruction: &Instruction,
) -> bool {
    if checked(runtime, telemetry, id, instruction).is_none() {
        return false;
    }
    runtime
        .world
        .restrict_arrival_speed(runtime.world.generation(), id, &instruction.limits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::{
        state::{GeneratedTraffic, TrafficTarget},
        stations::Station,
        traffic::motion::{Route, Segment, Waypoint},
    };
    #[test]
    fn observed_slower_arrival_produces_a_bounded_offer_only_with_current_weather_and_floors() {
        let mut s = crate::test_state();
        let airport = Arc::new(openatc_core::airport::Airport {
            icao: "KTEST".into(),
            runways: vec![openatc_core::airport::Runway {
                first_name: "36".into(),
                second_name: "18".into(),
                width: 45.0,
                second: openatc_core::state::Point {
                    north: 3000.0,
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        });
        s.generated.destination = Some(airport.clone());
        s.generated.controllers.insert(
            1,
            Station {
                service: "Approach".into(),
                ..Default::default()
            },
        );
        let weather = (
            serde_json::json!({"airport":"KTEST","latitude":0.0,"longitude":0.0,
            "sampleAltitudeFeet":0.0,"pressureHpa":1013.25,"temperatureC":15.0,
            "magneticVariationDegrees":0.0,"windDegrees":0.0,"windKnots":0.0}),
            Instant::now(),
        );
        let environment =
            super::super::environment::Environment::from_sample(&airport, &weather).unwrap();
        let reference = openatc_core::traffic::trajectory::Airspeed {
            indicated_knots: 220.0,
            atmosphere: environment.atmosphere,
            wind_from: 0.0,
            wind_knots: 0.0,
        };
        s.generated.environments.insert("KTEST".into(), environment);
        let point = |nm: f64| Waypoint {
            latitude: -nm / 60.0,
            longitude: 0.0,
            altitude_feet: 6000.0,
        };
        let ground = reference.ground_knots(6000.0, 0.0).unwrap();
        let route = Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Descent,
                        points: vec![point(19.5), point(5.0)],
                        knots: ground,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Approach,
                        points: vec![point(5.0), point(0.0)],
                        knots: 140.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    openatc_core::traffic::holding::Pattern {
                        fix: point(0.0),
                        inbound_degrees: 0.0,
                        right: true,
                        ground_knots: 140.0,
                        leg_seconds: 60.0,
                        bank_degrees: 25.0,
                        turn_rate_degrees_sec: 3.0,
                    }
                    .compile(|_, _| true)
                    .unwrap(),
                ],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        assert!(s.generated.world.insert(0, 1, route.clone()));
        assert!(s.generated.world.ground_rates(1, 1.0, 0.5));
        let recovery = openatc_core::traffic::holding::Pattern {
            fix: point(5.0),
            inbound_degrees: 0.0,
            right: true,
            ground_knots: 140.0,
            leg_seconds: 60.0,
            bank_degrees: 25.0,
            turn_rate_degrees_sec: 3.0,
        }
        .compile(|_, _| true)
        .unwrap();
        assert!(s.generated.world.recovery(
            0,
            1,
            0,
            Arc::new(Route::compile(vec![recovery], 1000.0, 1000.0).unwrap())
        ));
        assert!(s.generated.world.authorise(0, 1, 0, &Default::default()));
        s.generated.flights.insert(
            1,
            Arc::new(Prepared {
                spacing_floor: Some(170.0),
                route,
                airspeeds: BTreeMap::from([(0, reference)]),
                recovery: BTreeMap::new(),
                clearances: BTreeMap::new(),
                vacated_reports: BTreeMap::from([(
                    2,
                    super::super::flight::RunwayReport {
                        airport: "KTEST".into(),
                        runway: "36".into(),
                    },
                )]),
                amendment: None,
            }),
        );
        s.generated.frames.push(GeneratedTraffic {
            target: TrafficTarget {
                id: 1,
                aircraft: "A320".into(),
                ..Default::default()
            },
            model: Arc::new(openatc_core::traffic::csl::Model {
                id: "TEST".into(),
                objects: vec![],
                matches: vec![],
                vertical_offset: None,
            }),
            animation: Default::default(),
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
        });
        s.session.telemetry = Telemetry {
            position_valid: true,
            on_ground: true,
            traffic: Some(vec![TrafficTarget {
                id: 7,
                aircraft: "A320".into(),
                callsign: "LEADER".into(),
                latitude: -10.5 / 60.0,
                altitude_feet: 6000.0,
                ground_speed_knots: Some(140.0),
                on_ground: false,
                ..Default::default()
            }]),
            ..Default::default()
        };
        let prediction = sequencing::assess(
            &airport,
            "36",
            &Telemetry {
                aircraft_icao: "A320".into(),
                latitude: point(19.5).latitude,
                longitude: 0.0,
                altitude_feet: 6000.0,
                ground_speed_knots: ground,
                indicated_speed_knots: Some(220.0),
                on_ground: false,
                traffic: s.session.telemetry.traffic.clone(),
                ..Default::default()
            },
            &s.traffic_profiles.current().unwrap(),
            Some(170.0),
            None,
            5.01,
            3.0,
        );
        assert_eq!(
            prediction,
            Decision::PathRequired,
            "the smaller gap cannot meet threshold timing"
        );
        assert!(candidate(&s, 1).is_none());
        s.session.telemetry.traffic.as_mut().unwrap()[0].latitude = -10.0 / 60.0;
        let instruction = candidate(&s, 1).expect("early feasible speed instruction");
        assert!((170.0..220.0).contains(&instruction.indicated));
        assert!(instruction.limits[&0] < ground);
        s.generated.controllers.get_mut(&1).unwrap().service = "Center".into();
        assert!(
            candidate(&s, 1).is_none(),
            "terminal 3 NM cannot be reused for an en-route service"
        );
        s.generated.controllers.get_mut(&1).unwrap().service = "Approach".into();
        assert!(
            !instruction.limits.contains_key(&1),
            "final speed unchanged"
        );
        let mut plan = (*s.generated.flights[&1]).clone();
        plan.spacing_floor = Some(220.0);
        s.generated.flights.insert(1, Arc::new(plan));
        assert!(
            candidate(&s, 1).is_none(),
            "published floor prevents infeasible speed"
        );
        s.generated.flights.insert(1, instruction.plan.clone());
        s.generated.environments.clear();
        assert!(candidate(&s, 1).is_none(), "fresh weather required");
    }
    #[test]
    fn radio_readback_applies_speed_without_moving_or_granting_a_new_route() {
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.generated.epoch = Some(state.session_generation);
        state.generated.policy = Some(
            super::super::parse_operations(include_str!("../../../../traffic/operations.toml"))
                .unwrap(),
        );
        state.session.telemetry = Telemetry {
            on_ground: true,
            position_valid: true,
            traffic: Some(vec![]),
            simulation_seconds: Some(0.0),
            ..Default::default()
        };
        let point = |nm| {
            let p = openatc_core::arrival::destination([0.0, 0.0], 90.0, nm);
            Waypoint {
                latitude: p[0],
                longitude: p[1],
                altitude_feet: 6000.0,
            }
        };
        let hold = openatc_core::traffic::holding::Pattern {
            fix: point(10.0),
            inbound_degrees: 90.0,
            right: true,
            ground_knots: 140.0,
            leg_seconds: 60.0,
            bank_degrees: 25.0,
            turn_rate_degrees_sec: 3.0,
        }
        .compile(|_, _| true)
        .unwrap();
        let recovery = Arc::new(Route::compile(vec![hold.clone()], 1000.0, 1000.0).unwrap());
        let route = Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Descent,
                        points: vec![point(0.0), point(10.0)],
                        knots: 220.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    hold,
                ],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        let plan = Arc::new(Prepared {
            spacing_floor: Some(170.0),
            route: route.clone(),
            airspeeds: BTreeMap::new(),
            recovery: BTreeMap::from([(0, recovery.clone())]),
            clearances: BTreeMap::new(),
            vacated_reports: BTreeMap::new(),
            amendment: None,
        });
        assert!(state.generated.world.insert(0, 1, route));
        assert!(state.generated.world.ground_rates(1, 1.0, 0.5));
        assert!(state.generated.world.recovery(0, 1, 0, recovery));
        assert!(
            state
                .generated
                .world
                .authorise(0, 1, 0, &Default::default())
        );
        state.generated.world.advance(0.0, false);
        state.generated.flights.insert(1, plan.clone());
        state.generated.active_flights.insert(1);
        state.generated.frames.push(GeneratedTraffic {
            target: TrafficTarget {
                id: 1,
                callsign: "TEST1".into(),
                aircraft: "A320".into(),
                com1_khz: Some(123_100),
                ..Default::default()
            },
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
            model: Arc::new(openatc_core::traffic::csl::Model {
                id: "TEST".into(),
                objects: vec![],
                vertical_offset: None,
                matches: vec![],
            }),
            animation: Default::default(),
        });
        let instruction = Instruction {
            plan,
            limits: BTreeMap::from([(0, 180.0)]),
            indicated: 180.0,
        };
        let before = state.generated.world.snapshot()[0].position;
        assert!(checked(&state.generated, &state.session.telemetry, 1, &instruction).is_some());
        assert!(!state.generated.world.arrival_speed_restricted(1));
        state.generated.exchange = Some(Exchange {
            speed: Some(instruction.clone()),
            id: 1,
            segment: 0,
            station: Station {
                service: "Approach".into(),
                name: "Test Approach".into(),
                khz: 123_100,
                ..Default::default()
            },
            callsign: "TEST1".into(),
            messages: vec![
                (
                    true,
                    "TEST1, reduce speed to 180 knots until final approach.".into(),
                ),
                (false, "180 knots until final approach, TEST1.".into()),
            ],
            next: 0,
            due: Instant::now(),
            grant: false,
            tune_after: None,
            sector_transfer: None,
            recovery_from: None,
            rejoin: false,
        });
        for _ in 0..8 {
            if let Some(exchange) = state.generated.exchange.as_mut() {
                exchange.due = Instant::now();
            }
            super::super::radio::tick(&mut state);
            if state.generated.exchange.is_none() {
                break;
            }
        }
        assert!(state.generated.world.arrival_speed_restricted(1));
        assert_eq!(state.generated.world.snapshot()[0].position, before);
        assert!(state.generated.approved_orders.is_empty());
        state.generated.world.advance(1.0, false);
        assert!((state.generated.world.snapshot()[0].knots - 219.5).abs() < 1e-6);
        let mut outdated = instruction;
        outdated.plan = Arc::new((*outdated.plan).clone());
        assert!(!accept(
            &mut state.generated,
            &state.session.telemetry,
            1,
            &outdated
        ));
        state.session.telemetry.traffic = None;
        let missing_feed = Instruction {
            plan: state.generated.flights[&1].clone(),
            limits: BTreeMap::from([(0, 175.0)]),
            indicated: 175.0,
        };
        assert!(!accept(
            &mut state.generated,
            &state.session.telemetry,
            1,
            &missing_feed
        ));
    }
}
