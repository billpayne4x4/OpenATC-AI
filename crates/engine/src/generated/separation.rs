//! Check proposed airborne grants against the movement world and current external observations.
#[allow(unused_imports, clippy::wildcard_imports)]
use super::*;
use super::{BTreeSet, Runtime, TrafficTarget, flight};
use openatc_core::{
    state::Telemetry,
    traffic::motion::{Pose, Stage, Waypoint, World},
};

fn apply(world: &mut World, id: u32, index: usize, clearance: &flight::Clearance) -> bool {
    let Some(pose) = world.snapshot().into_iter().find(|pose| pose.id == id) else {
        return false;
    };
    let generation = world.generation();
    if let Some(end) = clearance.initial_climb
        && !world.authorise_airborne_until(generation, id, 2, end)
    {
        return false;
    }
    if let Some(end) = clearance.airborne_through {
        return world.authorise_airborne_until(generation, id, index, end);
    }
    if index == pose.segment {
        world.authorise(generation, id, index, &BTreeSet::new())
    } else {
        world.authorise_continuation(generation, id, index, &BTreeSet::new())
    }
}

fn external(target: &TrafficTarget, rate: f64, time: f64) -> Waypoint {
    let coordinate = openatc_core::arrival::destination(
        [target.latitude, target.longitude],
        target.track_degrees,
        target.ground_speed_knots.unwrap_or(0.0) * time / 3600.0,
    );
    Waypoint {
        latitude: coordinate[0],
        longitude: coordinate[1],
        altitude_feet: target.altitude_feet + rate * time / 60.0,
    }
}

pub(super) fn accepted_world(runtime: &Runtime) -> World {
    let mut world = runtime.world.clone();
    for &(id, index) in &runtime.approved_orders {
        if let Some(contract) = runtime
            .flights
            .get(&id)
            .and_then(|plan| plan.clearances.get(&index))
        {
            let _ = apply(&mut world, id, index, contract);
        }
    }
    world
}

fn runway_endpoints(airport: &Airport, name: &str) -> Option<(Point, Point)> {
    let runway = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == name || candidate.second_name == name)?;
    if runway.first_name == name {
        Some((runway.first, runway.second))
    } else {
        Some((runway.second, runway.first))
    }
}

fn runway_heading(start: Point, end: Point) -> f64 {
    (end.east - start.east)
        .atan2(end.north - start.north)
        .to_degrees()
        .rem_euclid(360.0)
}

/// True when two named runways are parallel and far enough apart to be treated as separate
/// movement surfaces. The 04L/04R pair at Nice is about 309 m apart.
fn parallel_runways(airport: &Airport, runway: &str, other: &str) -> bool {
    if runway == other {
        return false;
    }
    let (Some((start, end)), Some((other_start, other_end))) = (
        runway_endpoints(airport, runway),
        runway_endpoints(airport, other),
    ) else {
        return false;
    };
    let angle = openatc_core::navigation::turn(
        runway_heading(start, end),
        runway_heading(other_start, other_end),
    )
    .abs();
    if angle.min((180.0 - angle).abs()) >= 20.0 {
        return false;
    }
    let dx = end.east - start.east;
    let dn = end.north - start.north;
    let length = dx.hypot(dn);
    if length < 100.0 {
        return false;
    }
    let across =
        |point: Point| ((point.east - start.east) * dn - (point.north - start.north) * dx) / length;
    let near = across(other_start);
    let far = across(other_end);
    // Both ends must stay on the same side of this centreline and clear of it. A runway
    // that converges on, or crosses, this one is never treated as parallel even when its
    // heading happens to match within the angular tolerance.
    near * far > 0.0 && near.abs() > 250.0 && far.abs() > 250.0
}

/// A low aircraft on short final to this runway can go around across its departure path.
/// A parallel runway is handled by the runway-pair filter instead: a departure on one strip
/// is allowed while an arrival is landing on the other, including a short final.
fn short_final_for(
    airport: &Airport,
    runway: &str,
    latitude: f64,
    longitude: f64,
    altitude_feet: f64,
    track_degrees: f64,
) -> bool {
    let agl = altitude_feet - airport.elevation_feet;
    if !(0.0..2000.0).contains(&agl) {
        return false;
    }
    let Some((along, across, length, heading)) =
        openatc_core::traffic::runway_position(airport, runway, latitude, longitude)
    else {
        return false;
    };
    let turn = openatc_core::navigation::turn(heading, track_degrees).abs();
    let inbound = along < 0.0 && turn < 35.0 || along > length && turn > 145.0;
    let distance = if along < 0.0 { -along } else { along - length };
    inbound && distance < 3.0 * 1852.0 && across.abs() < 300.0
}

/// True when an airborne target is established on a genuinely parallel runway rather than
/// crossing the departure path. A 04L arrival must not withhold a 04R departure, but traffic
/// on a converging or crossing final is never dismissed as parallel just because its track
/// happens to line up with the departure runway.
pub(super) fn parallel_other_runway(
    airport: &Airport,
    runway: &str,
    latitude: f64,
    longitude: f64,
    track_degrees: f64,
) -> bool {
    let Some((_, _, _, _)) =
        openatc_core::traffic::runway_position(airport, runway, latitude, longitude)
    else {
        return false;
    };
    let nearest = airport
        .runways
        .iter()
        .filter_map(|candidate| {
            let name = if openatc_core::navigation::turn(
                track_degrees,
                runway_heading(candidate.first, candidate.second),
            )
            .abs()
                < 25.0
            {
                &candidate.first_name
            } else if openatc_core::navigation::turn(
                track_degrees,
                runway_heading(candidate.second, candidate.first),
            )
            .abs()
                < 25.0
            {
                &candidate.second_name
            } else {
                return None;
            };
            let (_, across, _, _) =
                openatc_core::traffic::runway_position(airport, name, latitude, longitude)?;
            (across.abs() < 800.0).then_some((across.abs(), name))
        })
        .min_by(|(left, _), (right, _)| left.total_cmp(right));
    let Some((_, target_runway)) = nearest else {
        return false;
    };
    if target_runway == runway {
        // On the departure runway itself.
        return false;
    }
    // A different parallel runway is never traffic for this runway. For a non-parallel
    // runway, only dismiss it when the extended track does not cross this runway's path.
    parallel_runways(airport, runway, target_runway)
        || !track_crosses_runway(airport, runway, latitude, longitude, track_degrees)
}

/// True when the target's extended ground track crosses the runway centreline path.
fn track_crosses_runway(
    airport: &Airport,
    runway: &str,
    latitude: f64,
    longitude: f64,
    track_degrees: f64,
) -> bool {
    let Some((along, across, length, heading)) =
        openatc_core::traffic::runway_position(airport, runway, latitude, longitude)
    else {
        return true;
    };
    let Some(width) = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)
        .map(|candidate| candidate.width)
    else {
        return true;
    };
    let half_width = width / 2.0 + 45.0;
    let angle = (track_degrees - heading).to_radians();
    if angle.cos().abs() < 0.01 {
        // Perpendicular crossing: the current lateral offset decides.
        return across.abs() < half_width;
    }
    // Sample the runway path and a short extension past each end.
    let start = -2.0 * 1852.0;
    let end = length + 2.0 * 1852.0;
    for step in 0..=40 {
        let s = start + (end - start) * f64::from(step) / 40.0;
        let projected = across + (s - along) * angle.tan();
        if projected.abs() < half_width {
            return true;
        }
    }
    false
}

/// The forecast includes existing accepted scopes, their fallback holds and the proposed grant.
pub(super) fn blocked(
    runtime: &Runtime,
    telemetry: &Telemetry,
    id: u32,
    index: usize,
    clearance: &flight::Clearance,
) -> bool {
    let Some(plan) = runtime.flights.get(&id) else {
        return true;
    };
    let Some(segment) = plan.route.segment(index) else {
        return true;
    };
    if segment.stage.on_ground() && segment.stage != Stage::Takeoff {
        return false;
    }
    let runway = segment
        .resources
        .iter()
        .find_map(|resource| {
            resource
                .strip_prefix("RUNWAY/")
                .and_then(|rest| rest.split('/').nth(1))
        })
        .or_else(|| {
            // The initial-climb clearance follows the takeoff segment and does not repeat
            // its runway. Keep using the departure runway for the parallel-runway filter.
            plan.clearances
                .get(&1)
                .filter(|clearance| clearance.initial_climb.is_some())
                .and_then(|_| plan.route.segment(1))
                .and_then(|segment| {
                    segment.resources.iter().find_map(|resource| {
                        resource
                            .strip_prefix("RUNWAY/")
                            .and_then(|rest| rest.split('/').nth(1))
                    })
                })
        });
    let Some(policy) = runtime.policy.as_ref() else {
        return true;
    };
    if telemetry.traffic.is_none() || !telemetry.position_valid {
        return true;
    }
    let mut world = accepted_world(runtime);
    if !apply(&mut world, id, index, clearance) {
        return true;
    }
    let Some(pose) = runtime
        .world
        .snapshot()
        .into_iter()
        .find(|pose| pose.id == id)
    else {
        return true;
    };
    let end = clearance
        .initial_climb
        .or(clearance.airborne_through)
        .unwrap_or(index);
    let remaining = if end > pose.segment {
        plan.route
            .nominal_seconds(pose.segment + 1, end)
            .unwrap_or(f64::INFINITY)
    } else {
        0.0
    };
    let horizon = pose.seconds_to_limit + remaining + policy.airborne_prediction_seconds;
    if horizon > 900.0 {
        return true;
    }
    !forecast_clear(runtime, telemetry, id, &world, horizon, runway).unwrap_or(false)
}

pub(super) fn rejoin_blocked(
    runtime: &Runtime,
    telemetry: &Telemetry,
    id: u32,
    plan: &flight::Prepared,
) -> bool {
    rejoin_check(runtime, telemetry, id, plan).is_err()
}

pub(super) fn rejoin_check(
    runtime: &Runtime,
    telemetry: &Telemetry,
    id: u32,
    plan: &flight::Prepared,
) -> Result<(), String> {
    let policy = runtime
        .policy
        .as_ref()
        .ok_or("Missing onward prediction policy")?;
    let mut world = accepted_world(runtime);
    let through = plan.clearances[&0].airborne_through.unwrap_or(0);
    world.queue_hold_exit(
        world.generation(),
        id,
        plan.route.clone(),
        plan.recovery.clone(),
        through,
    )?;
    let horizon = world.holding_exit_seconds(id).unwrap_or(f64::INFINITY)
        + plan
            .route
            .nominal_seconds(0, through)
            .unwrap_or(f64::INFINITY)
        + policy.airborne_prediction_seconds;
    if horizon > 900.0 {
        return Err(format!(
            "Onward forecast needs {horizon:.1} seconds, above its 900-second budget"
        ));
    }
    if forecast_clear(runtime, telemetry, id, &world, horizon, None)? {
        Ok(())
    } else {
        Err("Onward forecast conflicts with another aircraft".into())
    }
}

/// The runway named by the next protected movement of a generated flight. An arrival on a
/// parallel runway is identified here even while its current track is still turning onto final.
fn assigned_runway(runtime: &Runtime, airport: &Airport, pose: &Pose) -> Option<String> {
    let plan = runtime.flights.get(&pose.id)?;
    (pose.segment..plan.route.segment_count())
        .filter_map(|index| plan.route.segment(index))
        .flat_map(|segment| segment.resources.iter())
        .find_map(|resource| {
            let rest = resource.strip_prefix("RUNWAY/")?;
            let mut parts = rest.split('/');
            let icao = parts.next()?;
            let name = parts.next()?;
            (icao == airport.icao).then(|| name.to_owned())
        })
}

pub(super) fn forecast_clear(
    runtime: &Runtime,
    telemetry: &Telemetry,
    id: u32,
    world: &World,
    seconds: f64,
    runway: Option<&str>,
) -> Result<bool, String> {
    let policy = runtime
        .policy
        .as_ref()
        .ok_or("Missing airborne prediction policy")?;
    let aircraft_airport = runtime.airport.as_deref();
    let parallel_other = |latitude: f64, longitude: f64, track_degrees: f64| {
        runway
            .zip(aircraft_airport)
            .is_some_and(|(runway, airport)| {
                parallel_other_runway(airport, runway, latitude, longitude, track_degrees)
            })
    };
    let assigned_runway =
        |other: &Pose| -> Option<String> { assigned_runway(runtime, aircraft_airport?, other) };
    let generated: BTreeSet<_> = world.snapshot().iter().map(|pose| pose.id).collect();
    let mut observed = telemetry
        .traffic
        .as_ref()
        .ok_or("Unknown traffic observations")?
        .iter()
        .filter(|target| !generated.contains(&target.id))
        .take(126)
        .map(|target| {
            (
                target.clone(),
                (target.id != 0)
                    .then(|| runtime.observed_climbs.get(&target.id).copied())
                    .flatten(),
            )
        })
        .collect::<Vec<_>>();
    observed.push((
        TrafficTarget {
            id: 0,
            latitude: telemetry.latitude,
            longitude: telemetry.longitude,
            altitude_feet: telemetry.altitude_feet,
            track_degrees: telemetry
                .ground_track_degrees
                .unwrap_or(telemetry.heading_degrees),
            ground_speed_knots: Some(telemetry.ground_speed_knots),
            on_ground: telemetry.on_ground,
            ..Default::default()
        },
        Some(telemetry.vertical_speed_fpm),
    ));
    if observed
        .iter()
        .any(|(target, _)| !openatc_core::traffic::advisory::valid(target))
    {
        return Err("Invalid external airborne observation".into());
    }
    let mut previous: Option<(f64, Vec<Pose>)> = None;
    let mut error = None;
    let clear = world.forecast(seconds, |time, poses| {
        let Some((before, last)) = previous.as_ref() else {
            previous = Some((time, poses.to_vec()));
            return true;
        };
        let Some(own) = poses.iter().find(|pose| pose.id == id) else {
            return false;
        };
        let Some(old) = last.iter().find(|pose| pose.id == id) else {
            return false;
        };
        if own.stage.on_ground() && old.stage.on_ground() {
            previous = Some((time, poses.to_vec()));
            return true;
        }
        let path = [old.position, own.position];
        for other in poses
            .iter()
            .filter(|other| other.id != id && !other.stage.on_ground())
            .filter(|other| {
                let short = runway.is_some_and(|own| {
                    aircraft_airport.is_some_and(|airport| {
                        short_final_for(
                            airport,
                            own,
                            other.position.latitude,
                            other.position.longitude,
                            other.position.altitude_feet,
                            other.heading,
                        )
                    })
                });
                // A generated arrival assigned to the parallel runway is a separate
                // operation, whether it is established or still joining final. A missed
                // approach that turns across the departure path is a separate stage and is
                // still checked by the generic parallel filter below.
                if !short
                    && other.stage == Stage::Approach
                    && let (Some(own), Some(other_runway)) = (runway, assigned_runway(other))
                    && let Some(airport) = aircraft_airport
                    && parallel_runways(airport, own, &other_runway)
                {
                    return false;
                }
                short
                    || !parallel_other(
                        other.position.latitude,
                        other.position.longitude,
                        other.heading,
                    )
            })
        {
            let Some(old) = last.iter().find(|pose| pose.id == other.id) else {
                return false;
            };
            match openatc_core::traffic::separation::breach(
                path,
                [old.position, other.position],
                policy.airborne_separation_nm + 0.01,
                policy
                    .vertical_minimum(old.position.altitude_feet.max(other.position.altitude_feet)),
            ) {
                Ok(false) => {}
                Ok(true) => return false,
                Err(reason) => {
                    error = Some(reason);
                    return false;
                }
            }
        }
        for (target, rate) in observed
            .iter()
            .filter(|(target, _)| !target.on_ground)
            .filter(|(target, _)| {
                runway.is_some_and(|own| {
                    aircraft_airport.is_some_and(|airport| {
                        short_final_for(
                            airport,
                            own,
                            target.latitude,
                            target.longitude,
                            target.altitude_feet,
                            target.track_degrees,
                        )
                    })
                }) || !parallel_other(target.latitude, target.longitude, target.track_degrees)
            })
        {
            let vertical = policy.vertical_minimum(target.altitude_feet)
                + if rate.is_none() {
                    15000.0 * time / 60.0
                } else {
                    0.0
                };
            let horizontal = policy.airborne_separation_nm
                + 0.01
                + if target.ground_speed_knots.is_none() {
                    2000.0 * time / 3600.0
                } else {
                    0.0
                };
            let other = [
                external(target, rate.unwrap_or(0.0), *before),
                external(target, rate.unwrap_or(0.0), time),
            ];
            match openatc_core::traffic::separation::breach(path, other, horizontal, vertical) {
                Ok(false) => {}
                Ok(true) => return false,
                Err(reason) => {
                    error = Some(reason);
                    return false;
                }
            }
        }
        previous = Some((time, poses.to_vec()));
        true
    })?;
    if let Some(error) = error {
        return Err(error);
    }
    Ok(clear)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::{
        stations::Station,
        traffic::{
            holding::Pattern,
            motion::{Route, Segment},
        },
    };
    fn fixture(other_feet: f64, other_authorised: bool) -> (Runtime, Telemetry, flight::Clearance) {
        let point = |east, north, altitude| Waypoint {
            latitude: north / 60.0,
            longitude: east / 60.0,
            altitude_feet: altitude,
        };
        let mut runtime = Runtime {
            policy: Some(
                parse_operations(include_str!("../../../../traffic/operations.toml")).unwrap(),
            ),
            ..Default::default()
        };
        let clearance = flight::Clearance {
            amended_route: None,
            transfers: Vec::new(),
            coordinated_recovery: Vec::new(),
            station: Station {
                service: "Center".into(),
                khz: 118_675,
                ..Default::default()
            },
            instruction: "Maintain the cleared route and level".into(),
            readback: "Maintaining the cleared route and level".into(),
            initial_climb: None,
            taxi_through: None,
            airborne_through: None,
            landing_rollout: None,
            missed: None,
            holding: None,
        };
        for (id, start, end, inbound) in [
            (1, point(-6.0, 0.0, 5000.0), point(6.0, 0.0, 5000.0), 90.0),
            (
                2,
                point(0.0, 6.0, other_feet),
                point(0.0, -6.0, other_feet),
                180.0,
            ),
        ] {
            let hold = Pattern {
                fix: end,
                inbound_degrees: inbound,
                right: true,
                ground_knots: 300.0,
                leg_seconds: 60.0,
                bank_degrees: 25.0,
                turn_rate_degrees_sec: 3.0,
            }
            .compile(|_, _| true)
            .unwrap();
            let recovery = Arc::new(Route::compile(vec![hold.clone()], 3000.0, 3000.0).unwrap());
            let route = Arc::new(
                Route::compile(
                    vec![
                        Segment {
                            stage: Stage::Cruise,
                            points: vec![start, end],
                            knots: 300.0,
                            resources: vec![],
                            dwell_seconds: 0.0,
                        },
                        hold,
                    ],
                    3000.0,
                    3000.0,
                )
                .unwrap(),
            );
            assert!(runtime.world.insert(0, id, route.clone()));
            assert!(runtime.world.recovery(0, id, 0, recovery.clone()));
            runtime.flights.insert(
                id,
                Arc::new(flight::Prepared {
                    spacing_floor: None,
                    airspeeds: BTreeMap::new(),
                    route,
                    recovery: BTreeMap::from([(0, recovery)]),
                    clearances: BTreeMap::from([(0, clearance.clone())]),
                    vacated_reports: BTreeMap::new(),
                    amendment: None,
                }),
            );
        }
        if other_authorised {
            assert!(runtime.world.authorise(0, 2, 0, &BTreeSet::new()));
        }
        runtime.world.advance(0.0, false);
        let telemetry = Telemetry {
            position_valid: true,
            on_ground: true,
            traffic: Some(vec![]),
            ..Default::default()
        };
        (runtime, telemetry, clearance)
    }

    #[test]
    fn proposed_airborne_grants_check_crossing_routes_and_pending_grants_without_moving_live_aircraft()
     {
        let (runtime, telemetry, clearance) = fixture(5000.0, true);
        let before = runtime.world.snapshot();
        assert!(blocked(&runtime, &telemetry, 1, 0, &clearance));
        for (old, new) in before.iter().zip(runtime.world.snapshot()) {
            assert_eq!(old.position, new.position);
            assert_eq!(old.awaiting_clearance, new.awaiting_clearance);
        }
        let (mut runtime, telemetry, clearance) = fixture(5000.0, false);
        assert!(!blocked(&runtime, &telemetry, 1, 0, &clearance));
        runtime.approved_orders.insert((2, 0));
        assert!(blocked(&runtime, &telemetry, 1, 0, &clearance));
        assert!(
            runtime
                .world
                .snapshot()
                .iter()
                .all(|pose| pose.awaiting_clearance)
        );
        let (runtime, mut telemetry, clearance) = fixture(7000.0, true);
        assert!(!blocked(&runtime, &telemetry, 1, 0, &clearance));
        telemetry.traffic = None;
        assert!(blocked(&runtime, &telemetry, 1, 0, &clearance));
    }

    #[test]
    fn parallel_runways_need_separation_and_alignment() {
        let airport = Airport {
            runways: vec![
                openatc_core::airport::Runway {
                    first_name: "09".into(),
                    second_name: "27".into(),
                    width: 45.0,
                    first: Point {
                        east: 0.0,
                        north: 0.0,
                        ..Default::default()
                    },
                    second: Point {
                        east: 3000.0,
                        north: 0.0,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                openatc_core::airport::Runway {
                    first_name: "09L".into(),
                    second_name: "27R".into(),
                    width: 45.0,
                    first: Point {
                        east: 0.0,
                        north: 800.0,
                        ..Default::default()
                    },
                    second: Point {
                        east: 3000.0,
                        north: 800.0,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                openatc_core::airport::Runway {
                    first_name: "18".into(),
                    second_name: "36".into(),
                    width: 45.0,
                    first: Point {
                        east: 0.0,
                        north: 0.0,
                        ..Default::default()
                    },
                    second: Point {
                        east: 0.0,
                        north: 3000.0,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                openatc_core::airport::Runway {
                    first_name: "09C".into(),
                    second_name: "27C".into(),
                    width: 45.0,
                    first: Point {
                        east: 0.0,
                        north: 500.0,
                        ..Default::default()
                    },
                    second: Point {
                        east: 3000.0,
                        north: 200.0,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(parallel_runways(&airport, "09", "09L"));
        assert!(parallel_runways(&airport, "09L", "09"));
        assert!(parallel_runways(&airport, "09", "27R"));
        assert!(!parallel_runways(&airport, "09", "09"));
        assert!(!parallel_runways(&airport, "09", "18"));
        assert!(
            !parallel_runways(&airport, "09", "09C"),
            "a converging runway is not parallel even when its heading is close"
        );
        assert!(
            !parallel_other_runway(&airport, "09", 480.0 / 111_320.0, -0.02, 95.7),
            "an aircraft on a converging final must stay a conflict"
        );
        assert!(
            parallel_other_runway(&airport, "09", 800.0 / 111_320.0, -0.02, 90.0),
            "an aircraft established on the parallel final is not a conflict"
        );
    }

    #[test]
    fn external_motion_uses_recent_climb_observations_and_unknown_identity_is_not_the_player() {
        let (mut runtime, mut telemetry, clearance) = fixture(7000.0, true);
        let crossing = TrafficTarget {
            id: 99,
            latitude: 0.1,
            longitude: 0.0,
            altitude_feet: 5000.0,
            ground_speed_knots: Some(300.0),
            track_degrees: 180.0,
            on_ground: false,
            ..Default::default()
        };
        telemetry.traffic = Some(vec![crossing.clone()]);
        assert!(blocked(&runtime, &telemetry, 1, 0, &clearance));
        runtime.observed_climbs.insert(99, 6000.0);
        assert!(!blocked(&runtime, &telemetry, 1, 0, &clearance));
        telemetry.traffic.as_mut().unwrap()[0].id = 0;
        telemetry.vertical_speed_fpm = 6000.0;
        assert!(blocked(&runtime, &telemetry, 1, 0, &clearance));
        telemetry.traffic = Some(vec![TrafficTarget {
            ground_speed_knots: None,
            ..crossing
        }]);
        assert!(blocked(&runtime, &telemetry, 1, 0, &clearance));
    }
}
