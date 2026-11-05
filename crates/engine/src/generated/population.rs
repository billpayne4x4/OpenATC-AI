//! Keep traffic near the player without replacing aircraft that are still visible or audible.

use super::{BTreeMap, BTreeSet, Runtime, Scene};
use openatc_core::{
    arrival::{destination, distance_nm},
    state::{Telemetry, TrafficTarget},
    traffic::motion::Stage,
};

#[derive(Clone, Default)]
pub(super) struct Population {
    pub started: bool,
    pub revision: u64,
    pub target: usize,
    /// Moving ground aircraft target, rolled once when this session starts.
    pub taxi_target: usize,
    outside_since: BTreeMap<u32, f64>,
    /// A landed arrival has reached its stand and is waiting to be recycled.
    arrival_since: BTreeMap<u32, f64>,
    turnaround_retry: BTreeMap<u32, f64>,
    /// A preparation that could not add any flight waits before planning the same scene again.
    pub(super) refill_blocked_until: Option<std::time::Instant>,
    /// Last time a nearby STAR scene refresh was requested, keyed by airport.
    nearby_reseed: BTreeMap<String, std::time::Instant>,
    player_stand: String,
}

fn distance(target: &TrafficTarget, telemetry: &Telemetry, seconds: f64) -> f64 {
    let own = destination(
        [telemetry.latitude, telemetry.longitude],
        telemetry
            .ground_track_degrees
            .unwrap_or(telemetry.heading_degrees),
        telemetry.ground_speed_knots * seconds / 3600.0,
    );
    let other = destination(
        [target.latitude, target.longitude],
        target.track_degrees,
        target.ground_speed_knots.unwrap_or(0.0) * seconds / 3600.0,
    );
    distance_nm(own, other)
}

/// A radio change alone never retires an aircraft. It must stay outside the visual
/// boundary and be separating from the player throughout the grace period.
fn outside(target: &TrafficTarget, telemetry: &Telemetry, radius: f64, audible: bool) -> bool {
    if audible || !telemetry.position_valid {
        return false;
    }
    let now = distance(target, telemetry, 0.0);
    now.is_finite()
        && now > radius
        && [60.0, 180.0, 300.0].into_iter().all(|seconds| {
            let next = distance(target, telemetry, seconds);
            next.is_finite() && next >= now
        })
}

pub(super) fn publish_pending(state: &mut crate::EngineState) {
    if state.generated.simulation_job.is_some()
        || state.generated.turnaround_job.is_some()
        || state.generated.turnaround_pending.is_some()
    {
        return;
    }
    let Some((scene, key, epoch)) = state.generated.queued_scene.take() else {
        return;
    };
    if epoch != state.session_generation
        || key != super::scene_key(state)
        || !state.settings.generated_traffic
        || !scene.weather_fresh()
    {
        return;
    }
    let telemetry = state.session.telemetry.clone();
    let Some(profiles) = state.traffic_profiles.current() else {
        return;
    };
    let counts = [
        state.settings.airline_traffic,
        state.settings.regional_traffic,
        state.settings.cargo_traffic,
        state.settings.ga_traffic,
    ];
    merge(
        &mut state.generated,
        (*scene).clone(),
        key,
        epoch,
        &telemetry,
        &profiles,
        counts,
    );
}

pub(super) fn maintain(state: &mut crate::EngineState) {
    let telemetry = &state.session.telemetry;
    if telemetry.paused || state.generated.simulation_job.is_some() {
        return;
    }
    let Some(seconds) = telemetry
        .simulation_seconds
        .filter(|value| value.is_finite())
    else {
        return;
    };
    let radius = state
        .generated
        .policy
        .as_ref()
        .map_or(50.0, |policy| policy.display_leave_nm);
    let target = [
        state.settings.airline_traffic,
        state.settings.regional_traffic,
        state.settings.cargo_traffic,
        state.settings.ga_traffic,
    ]
    .into_iter()
    .map(usize::from)
    .sum::<usize>()
    .min(super::MAXIMUM_AIRCRAFT);
    if target > state.generated.population.target {
        state.generated.population.refill_blocked_until = None;
    }
    state.generated.population.target = target;
    let poses = state.generated.world.snapshot();
    state
        .generated
        .population
        .player_stand
        .clone_from(&state.session.plan.arrival_stand);
    for pose in &poses {
        if !matches!(pose.stage, Stage::Turnaround | Stage::Parked) {
            state.generated.turnarounds_attempted.remove(&pose.id);
            state.generated.population.turnaround_retry.remove(&pose.id);
        } else if pose.complete && state.generated.turnarounds_attempted.contains(&pose.id) {
            let retry = state
                .generated
                .population
                .turnaround_retry
                .entry(pose.id)
                .or_insert(seconds);
            if seconds < *retry || seconds - *retry >= 120.0 {
                state.generated.turnarounds_attempted.remove(&pose.id);
                state.generated.population.turnaround_retry.remove(&pose.id);
            }
        }
    }
    let mut retired = Vec::new();
    for frame in &state.generated.frames {
        let id = frame.target.id;
        let engaged = state
            .generated
            .exchange
            .as_ref()
            .is_some_and(|exchange| exchange.id == id)
            || state.generated.turnaround_target == Some(id)
            || poses.iter().any(|pose| {
                pose.id == id
                    && (pose.recovering
                        || matches!(
                            pose.stage,
                            Stage::Holding | Stage::Takeoff | Stage::LineUp | Stage::Rollout
                        ))
            });
        let audible = frame
            .target
            .com1_khz
            .is_some_and(|frequency| state.session.receiving_radio(frequency).is_some());
        if engaged || !outside(&frame.target, telemetry, radius, audible) {
            state.generated.population.outside_since.remove(&id);
            continue;
        }
        let since = state
            .generated
            .population
            .outside_since
            .entry(id)
            .or_insert(seconds);
        if seconds < *since {
            *since = seconds;
        }
        if seconds - *since >= 60.0 {
            let returns = poses.iter().find(|pose| pose.id == id).is_some_and(|pose| {
                if pose.complete {
                    return false;
                }
                state
                    .generated
                    .world
                    .assigned_route(id)
                    .is_some_and(|route| {
                        (pose.segment..route.segment_count())
                            .filter_map(|index| route.segment(index))
                            .flat_map(|segment| segment.points.windows(2))
                            .any(|pair| {
                                let first = [pair[0].latitude, pair[0].longitude];
                                let last = [pair[1].latitude, pair[1].longitude];
                                let length = distance_nm(first, last);
                                [0.0, 300.0].into_iter().any(|seconds| {
                                    let own = destination(
                                        [telemetry.latitude, telemetry.longitude],
                                        telemetry
                                            .ground_track_degrees
                                            .unwrap_or(telemetry.heading_degrees),
                                        telemetry.ground_speed_knots * seconds / 3600.0,
                                    );
                                    distance_nm(own, first).min(distance_nm(own, last))
                                        <= radius + length
                                })
                            })
                    })
            });
            if !returns {
                retired.push(id);
            }
        }
    }
    // Once the player is beyond the circuit radius, the airport behind them is no longer the
    // show. Recycle its ground and arrival traffic into airborne traffic near the player.
    if !telemetry.on_ground
        && let Some(airport) = state.generated.airport.clone()
    {
        let wingspan = telemetry.wingspan_meters.unwrap_or(20.0);
        let radius = if wingspan >= 20.0 {
            state.settings.traffic_circuit_jet_nm
        } else if wingspan >= 12.0 {
            state.settings.traffic_circuit_turboprop_nm
        } else {
            state.settings.traffic_circuit_ga_nm
        };
        let distance = distance_nm(
            [telemetry.latitude, telemetry.longitude],
            [airport.reference_latitude, airport.reference_longitude],
        );
        if distance.is_finite() && distance > radius {
            for frame in &state.generated.frames {
                let id = frame.target.id;
                let Some(pose) = poses.iter().find(|pose| pose.id == id) else {
                    continue;
                };
                let local = pose.stage.on_ground()
                    || matches!(
                        pose.stage,
                        Stage::Approach | Stage::Descent | Stage::Rollout | Stage::TaxiIn
                    );
                if !local
                    || state
                        .generated
                        .exchange
                        .as_ref()
                        .is_some_and(|exchange| exchange.id == id)
                {
                    continue;
                }
                retired.push(id);
            }
        }
    }
    // Landed arrivals are not parked scenery. Taxi to the stand, pause briefly, then free the
    // slot for another arrival on final.
    for frame in &state.generated.frames {
        let id = frame.target.id;
        let Some(pose) = poses.iter().find(|pose| pose.id == id) else {
            continue;
        };
        let arrival = state.generated.flights.get(&id).is_some_and(|plan| {
            let start = plan.route.start_position();
            let end = plan.route.end_position();
            let at = [pose.position.latitude, pose.position.longitude];
            openatc_core::arrival::distance_nm(at, [end.latitude, end.longitude]) <= 0.05
                && openatc_core::arrival::distance_nm(at, [start.latitude, start.longitude]) > 0.1
        });
        if !arrival || !matches!(pose.stage, Stage::Parked | Stage::Turnaround) || !pose.complete {
            state.generated.population.arrival_since.remove(&id);
            continue;
        }
        if state
            .generated
            .exchange
            .as_ref()
            .is_some_and(|exchange| exchange.id == id)
        {
            continue;
        }
        let since = state
            .generated
            .population
            .arrival_since
            .entry(id)
            .or_insert(seconds);
        if seconds < *since {
            *since = seconds;
        }
        if seconds - *since >= 5.0 {
            retired.push(id);
        }
    }
    retired.sort_unstable();
    retired.dedup();
    for id in retired {
        remove(&mut state.generated, id);
    }
}

/// Request one fresh preparation before the player reaches the STAR.
///
/// The scene is prepared once the filed destination is within `ARRIVAL_PREPARE_NM` while
/// the aircraft is still high, or inside 30 NM when the flight is already low. Each band
/// is requested once per ten minutes so preparation does not run every tick.
pub(super) fn request_nearby_scene(
    runtime: &mut Runtime,
    telemetry: &Telemetry,
    generated: bool,
    destination: Option<(String, f64, f64)>,
) {
    if telemetry.on_ground || telemetry.paused || !telemetry.position_valid || !generated {
        return;
    }
    let target = destination.or_else(|| {
        runtime
            .destination
            .as_ref()
            .or(runtime.airport.as_ref())
            .map(|airport| {
                (
                    airport.icao.clone(),
                    airport.reference_latitude,
                    airport.reference_longitude,
                )
            })
    });
    let Some((icao, latitude, longitude)) = target else {
        return;
    };
    let direct = distance_nm(
        [telemetry.latitude, telemetry.longitude],
        [latitude, longitude],
    );
    if !direct.is_finite() {
        return;
    }
    let before_star = direct <= super::ARRIVAL_PREPARE_NM && telemetry.height_agl_feet >= 5_000.0;
    if !before_star && direct > 30.0 {
        return;
    }
    // The early and terminal attempts are tracked apart, so a scene that could not add
    // flights before the STAR does not suppress the terminal refresh.
    let key = format!(
        "{}:{}",
        icao,
        if direct > 30.0 { "early" } else { "terminal" }
    );
    let now = std::time::Instant::now();
    if runtime
        .population
        .nearby_reseed
        .get(&key)
        .is_some_and(|at| now.duration_since(*at) < std::time::Duration::from_secs(600))
    {
        return;
    }
    runtime.population.nearby_reseed.insert(key, now);
    runtime.scene_revision = runtime.scene_revision.wrapping_add(1);
    runtime.retry = None;
    runtime.population.refill_blocked_until = None;
}

pub(super) fn remove(runtime: &mut Runtime, id: u32) {
    runtime.world.remove(id);
    runtime.frames.retain(|frame| frame.target.id != id);
    macro_rules! maps {
        ($($field:ident),+ $(,)?) => { $(runtime.$field.remove(&id);)+ };
    }
    maps!(
        apron_routes,
        apron_segments,
        movement,
        nominal_departures,
        decoded_terminals,
        nominal_arrivals,
        nominal_flights,
        flights,
        orders,
        departure_wake,
        cancelled_takeoffs,
        controllers,
        seeded_stations,
        advisories,
        rejoins,
        handoff_targets,
        pending_sector_checkins,
        observed_climbs
    );
    macro_rules! sets {
        ($($field:ident),+ $(,)?) => { $(runtime.$field.remove(&id);)+ };
    }
    sets!(
        apron_ids,
        apron_radio,
        delivery_pending,
        delivery_cleared,
        held,
        visible,
        fixed_gear,
        active_flights,
        tower_ready,
        tower_notified,
        turnarounds_attempted
    );
    macro_rules! keyed {
        ($($field:ident),+ $(,)?) => { $(runtime.$field.retain(|(aircraft, _)| *aircraft != id);)+ };
    }
    keyed!(
        approved_orders,
        reported_crossings,
        reported_recoveries,
        reported_holds,
        pending_checkins,
        cancelled_landings
    );
    runtime
        .rejoin_attempts
        .retain(|(aircraft, _), _| *aircraft != id);
    runtime
        .sector_transfers
        .retain(|(aircraft, _), _| *aircraft != id);
    super::Arc::make_mut(&mut runtime.turnaround_inputs).remove(&id);
    runtime.population.outside_since.remove(&id);
    runtime.population.arrival_since.remove(&id);
    runtime.population.turnaround_retry.remove(&id);
    runtime.population.refill_blocked_until = None;
    runtime.population.revision = runtime.population.revision.wrapping_add(1);
    runtime.retry = None;
    reservations(runtime);
}

/// Rebuild reservations from retained aircraft so retired flights cannot consume parking forever.
pub(super) fn reservations(runtime: &mut Runtime) {
    runtime.reserved_stands = runtime
        .turnaround_inputs
        .values()
        .map(|input| input.arrival_stand.clone())
        .chain(std::iter::once(runtime.population.player_stand.clone()))
        .filter(|name| !name.is_empty())
        .collect();
    let mut stands = BTreeSet::new();
    let mut corridors = BTreeSet::new();
    for frame in &runtime.frames {
        let id = frame.target.id;
        let routes = runtime
            .world
            .assigned_route(id)
            .into_iter()
            .chain(runtime.flights.get(&id).map(|plan| plan.route.clone()));
        for route in routes {
            for index in 0..route.segment_count() {
                let segment = route.segment(index).unwrap();
                if matches!(segment.stage, Stage::Parked | Stage::Turnaround) {
                    // Only current parking and the arrival stand remain reserved.
                    if index + 1 == route.segment_count()
                        || runtime
                            .world
                            .current_segment(id)
                            .is_some_and(|current| std::ptr::eq(current, segment))
                    {
                        stands.extend(segment.resources.iter().cloned());
                    }
                } else if segment.stage.on_ground() {
                    corridors.extend(segment.resources.iter().cloned());
                }
            }
        }
    }
    runtime.occupied_stand_cells = super::Arc::new(stands);
    runtime.surface_corridors = super::Arc::new(corridors);
    let callsigns: BTreeSet<_> = runtime
        .frames
        .iter()
        .map(|frame| frame.target.callsign.as_str())
        .collect();
    runtime
        .spoken_callsigns
        .retain(|callsign, _| callsigns.contains(callsign.as_str()));
    let mut terminals = Vec::new();
    let mut indices = BTreeMap::new();
    for decoded in runtime.decoded_terminals.values_mut() {
        let Some(terminal) = runtime.terminal_routes.get(decoded.candidate) else {
            continue;
        };
        let index = *indices.entry(decoded.candidate).or_insert_with(|| {
            terminals.push(terminal.clone());
            terminals.len() - 1
        });
        super::Arc::make_mut(decoded).candidate = index;
    }
    runtime.terminal_routes = terminals;
    let mut airports: BTreeSet<_> = runtime
        .turnaround_inputs
        .values()
        .flat_map(|input| [input.origin.icao.clone(), input.destination.icao.clone()])
        .collect();
    airports.extend(
        runtime
            .controllers
            .values()
            .map(|station| station.airport.clone()),
    );
    airports.extend(
        runtime
            .airport
            .iter()
            .chain(runtime.destination.iter())
            .map(|airport| airport.icao.clone()),
    );
    if let Some(plan) = &runtime.network {
        airports.extend(plan.airports.keys().cloned());
    }
    runtime.airports.retain(|id, _| airports.contains(id));
    runtime.environments.retain(|id, _| airports.contains(id));
}

pub(super) fn needs_refill(runtime: &Runtime) -> bool {
    let moving = runtime
        .frames
        .iter()
        .filter(|frame| {
            runtime.flights.contains_key(&frame.target.id)
                || runtime.advisories.contains_key(&frame.target.id)
        })
        .count();
    moving < runtime.population.target && runtime.frames.len() < runtime.population.target
}

fn promote_apron(runtime: &mut Runtime, incoming: &mut Runtime) -> usize {
    let mut promoted = Vec::new();
    let positions: BTreeMap<_, _> = runtime
        .world
        .snapshot()
        .into_iter()
        .map(|pose| (pose.id, pose.segment))
        .collect();
    let mut frames: Vec<_> = runtime.frames.iter().collect();
    frames.sort_by_key(|frame| {
        runtime
            .apron_routes
            .get(&frame.target.id)
            .map_or(usize::MAX, |route| {
                route
                    .segment_count()
                    .saturating_sub(*positions.get(&frame.target.id).unwrap_or(&0))
            })
    });
    for frame in frames {
        let id = frame.target.id;
        if !runtime.apron_routes.contains_key(&id) || runtime.flights.contains_key(&id) {
            continue;
        }
        let hold = runtime.apron_routes[&id].end_position();
        let candidate = incoming.frames.iter().find(|candidate| {
            if !runtime.population.started
                && incoming
                    .world
                    .current_segment(candidate.target.id)
                    .is_some_and(|segment| {
                        matches!(
                            segment.stage,
                            Stage::TaxiOut | Stage::Crossing | Stage::Pushback
                        )
                    })
            {
                return false;
            }
            (candidate.target.on_ground
                || incoming
                    .world
                    .current_segment(candidate.target.id)
                    .is_some_and(|segment| matches!(segment.stage, Stage::Climb | Stage::Cruise)))
                && candidate.target.aircraft == frame.target.aircraft
                && !promoted.contains(&candidate.target.id)
                && incoming
                    .flights
                    .get(&candidate.target.id)
                    .is_some_and(|plan| {
                        let start = plan.route.start_position();
                        distance_nm(
                            [hold.latitude, hold.longitude],
                            [start.latitude, start.longitude],
                        ) <= 0.001
                            && (hold.altitude_feet - start.altitude_feet).abs() <= 1.0
                    })
        });
        let Some(candidate) = candidate else {
            continue;
        };
        let donor = candidate.target.id;
        if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
            eprintln!(
                "Traffic departure: {} {} receives {} at its existing holding point",
                frame.target.callsign, frame.target.aircraft, candidate.target.callsign
            );
        }
        let mut plan = (*incoming.flights[&donor]).clone();
        for clearance in plan.clearances.values_mut() {
            clearance.instruction = clearance
                .instruction
                .replace(&candidate.target.callsign, &frame.target.callsign);
            clearance.readback = clearance
                .readback
                .replace(&candidate.target.callsign, &frame.target.callsign);
        }
        runtime.flights.insert(id, super::Arc::new(plan));
        // The existing taxi route and readbacks finish before flight::install joins the checked roll.
        runtime.active_flights.remove(&id);
        if let Some(input) = incoming.turnaround_inputs.get(&donor) {
            super::Arc::make_mut(&mut runtime.turnaround_inputs).insert(id, input.clone());
        }
        promoted.push(donor);
    }
    for id in &promoted {
        remove(incoming, *id);
    }
    promoted.len()
}

/// Publish only additional actors. Every live route, clearance, controller and simulator
/// clock remains in the current runtime, including flights still crossing the survey boundary.
pub(super) fn merge(
    runtime: &mut Runtime,
    scene: Scene,
    key: String,
    epoch: u64,
    telemetry: &Telemetry,
    profiles: &openatc_core::traffic::profiles::Profiles,
    counts: [u8; 4],
) {
    // The first complete scene replaces the plan-less seed outright. Incremental admission
    // only accepts aircraft at segment zero, so merging a cast that already contains
    // mid-taxi, holding and taxi-in aircraft silently keeps the parked ones and drops
    // everything that is already moving. A scene for another airport replaces the old one
    // instead of merging two airports into one visible set.
    if super::replaces_airport(runtime, &scene.airport)
        || (runtime.flights.is_empty() && !scene.flights.is_empty())
    {
        super::publish_scene(runtime, scene, key, epoch);
        return;
    }
    let mut incoming = Runtime::default();
    super::publish_scene(&mut incoming, scene, key.clone(), epoch);
    if runtime
        .airports
        .keys()
        .chain(incoming.airports.keys())
        .collect::<BTreeSet<_>>()
        .len()
        > 16
    {
        runtime.status =
            "Waiting for distant flights to leave before extending airport coverage".into();
        return;
    }
    if runtime
        .policy
        .as_ref()
        .is_some_and(|policy| Some(policy) != incoming.policy.as_ref())
    {
        runtime.status =
            "Traffic policy changed; restart the plugin to apply it to new flights".into();
        return;
    }
    let promoted = promote_apron(runtime, &mut incoming);
    let stages: BTreeMap<_, _> = incoming
        .world
        .snapshot()
        .into_iter()
        .map(|pose| (pose.id, pose.stage))
        .collect();
    incoming
        .frames
        .sort_by_key(|frame| match stages.get(&frame.target.id) {
            Some(Stage::Approach) => 0,
            Some(Stage::Descent) => 1,
            Some(Stage::Cruise) => 2,
            Some(Stage::Climb) => 3,
            _ => 4,
        });
    let mut accepted = BTreeSet::new();
    let category = |frame: &openatc_core::state::GeneratedTraffic| {
        use openatc_core::traffic::profiles::Category;
        if super::ctaf::is_local(frame.target.id) {
            return Some(3);
        }
        profiles
            .airlines
            .values()
            .find(|airline| {
                airline.name == frame.target.name
                    || !airline.operator.is_empty()
                        && frame
                            .target
                            .callsign
                            .strip_prefix(&airline.operator)
                            .is_some_and(|number| {
                                !number.is_empty() && number.bytes().all(|ch| ch.is_ascii_digit())
                            })
            })
            .map(|airline| match airline.category {
                Category::Airline | Category::Regional => 0,
                Category::Cargo => 1,
                Category::GeneralAviation => 2,
                Category::Military => 2,
            })
            // Private registrations do not match an airline profile. They are still planned
            // flights and must be counted as GA instead of being dropped from the cast.
            .or(Some(2))
    };
    let mut occupied = [0usize; 4];
    for frame in &runtime.frames {
        if let Some(index) = category(frame) {
            occupied[index] += 1;
        }
    }
    let limit = if runtime.population.target == 0 {
        super::MAXIMUM_AIRCRAFT
    } else {
        runtime.population.target
    };
    for frame in &incoming.frames {
        let id = frame.target.id;
        let Some(category) = category(frame) else {
            continue;
        };
        if occupied[category] >= usize::from(counts[category]) {
            continue;
        }
        if runtime.population.started
            && !frame.target.on_ground
            && distance(&frame.target, telemetry, 0.0)
                < incoming
                    .policy
                    .as_ref()
                    .map_or(20.0, |policy| policy.display_enter_nm / 2.0)
        {
            continue;
        }
        if runtime.frames.len() + accepted.len() >= limit.min(super::MAXIMUM_AIRCRAFT)
            || runtime.frames.iter().any(|old| {
                old.target.id == id
                    || old
                        .target
                        .callsign
                        .eq_ignore_ascii_case(&frame.target.callsign)
            })
            || runtime.frames.iter().any(|old| {
                old.target.on_ground && frame.target.on_ground && super::frames_overlap(old, frame)
            })
        {
            continue;
        }
        let mut world = runtime.world.clone();
        if !world.admit_prepared(&incoming.world, id) {
            continue;
        }
        if !frame.target.on_ground {
            let mut forecast = super::separation::accepted_world(runtime);
            if !forecast.admit_prepared(&incoming.world, id)
                || !super::separation::forecast_clear(
                    runtime,
                    telemetry,
                    id,
                    &forecast,
                    incoming
                        .policy
                        .as_ref()
                        .map_or(180.0, |policy| policy.airborne_prediction_seconds),
                    None,
                )
                .unwrap_or(false)
            {
                if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
                    eprintln!(
                        "Traffic admission: {} {:?} deferred by airborne forecast",
                        frame.target.callsign,
                        stages.get(&id)
                    );
                }
                continue;
            }
        }
        // Ignore any simulation job that started before these aircraft were admitted.
        runtime.simulation_job = None;
        runtime.world = world;
        accepted.insert(id);
        occupied[category] += 1;
    }
    let offset = runtime.terminal_routes.len();
    for decoded in incoming.decoded_terminals.values_mut() {
        super::Arc::make_mut(decoded).candidate += offset;
    }
    runtime.terminal_routes.extend(incoming.terminal_routes);
    macro_rules! maps {
        ($($field:ident),+ $(,)?) => { $(runtime.$field.extend(incoming.$field.into_iter()
            .filter(|(id, _)| accepted.contains(id)));)+ };
    }
    maps!(
        movement,
        nominal_departures,
        decoded_terminals,
        nominal_arrivals,
        nominal_flights,
        flights,
        orders,
        controllers,
        seeded_stations,
        advisories,
        handoff_targets
    );
    runtime.frames.extend(
        incoming
            .frames
            .into_iter()
            .filter(|frame| accepted.contains(&frame.target.id)),
    );
    runtime.active_flights.extend(
        incoming
            .active_flights
            .into_iter()
            .filter(|id| accepted.contains(id)),
    );

    runtime.fixed_gear.extend(
        incoming
            .fixed_gear
            .into_iter()
            .filter(|id| accepted.contains(id)),
    );
    super::Arc::make_mut(&mut runtime.turnaround_inputs).extend(
        incoming
            .turnaround_inputs
            .iter()
            .filter(|(id, _)| accepted.contains(id))
            .map(|(id, input)| (*id, input.clone())),
    );
    runtime.spoken_callsigns.extend(incoming.spoken_callsigns);
    runtime.airports.extend(incoming.airports);
    runtime.environments.extend(incoming.environments);
    runtime.policy = incoming.policy;
    runtime.channels = incoming.channels;
    runtime.stations = incoming.stations;
    runtime.regions = incoming.regions;
    runtime.mora = incoming.mora;
    runtime.simulator_root = incoming.simulator_root;
    runtime.airport = incoming.airport;
    runtime.destination = incoming.destination;
    runtime.departure_procedures = incoming.departure_procedures;
    runtime.arrival_procedures = incoming.arrival_procedures;
    runtime.terminal_points = incoming.terminal_points;
    runtime.published_holds = incoming.published_holds;
    runtime.require_complete_flights = incoming.require_complete_flights;
    runtime.key = key;
    runtime.epoch = Some(epoch);
    runtime.population.revision = runtime.population.revision.wrapping_add(1);
    runtime.population.started |= !accepted.is_empty() || promoted > 0;
    if accepted.is_empty() && promoted == 0 && runtime.frames.len() < runtime.population.target {
        // The planner has no additional complete flights for this area. Do not rebuild the
        // same scene every few seconds; wait for a retirement, a settings change or a move.
        runtime.population.refill_blocked_until =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(180));
    }
    runtime.status = format!(
        "{} additional flights admitted; {} taxi departures connected; {} aircraft retained",
        accepted.len(),
        promoted,
        runtime.frames.len()
    );
    runtime.warnings = incoming.warnings;
    reservations(runtime);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_prepared_departure_goes_to_the_front_of_the_existing_taxi_queue() {
        use super::super::{Arc, flight};
        use openatc_core::{
            state::GeneratedTraffic,
            traffic::{
                animation::Animation,
                csl::Model,
                motion::{Route, Segment, Waypoint},
            },
        };
        let point = |east: f64| Waypoint {
            latitude: 0.0,
            longitude: east / 111_320.0,
            altitude_feet: 0.0,
        };
        let model = Arc::new(Model {
            id: "fixture".into(),
            objects: Vec::new(),
            matches: Vec::new(),
            vertical_offset: None,
        });
        let frame = |id| GeneratedTraffic {
            target: TrafficTarget {
                id,
                callsign: format!("TEST{id}"),
                aircraft: "A320".into(),
                on_ground: true,
                ..Default::default()
            },
            model: model.clone(),
            animation: Animation::default(),
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
        };
        let mut runtime = Runtime::default();
        for id in [1, 2] {
            let route = Arc::new(
                Route::compile(
                    (0..2)
                        .map(|index| Segment {
                            stage: Stage::TaxiOut,
                            points: vec![
                                point(f64::from(index) * 100.0),
                                point(f64::from(index + 1) * 100.0),
                            ],
                            knots: 10.0,
                            resources: vec![format!("TAXI/{id}/{index}")],
                            dwell_seconds: 0.0,
                        })
                        .collect(),
                    2000.0,
                    2000.0,
                )
                .unwrap(),
            );
            assert!(runtime.world.insert(0, id, route.clone()));
            if id == 2 {
                assert!(runtime.world.seed_ground(0, id, route.clone(), 1));
            }
            runtime.apron_routes.insert(id, route);
            runtime.frames.push(frame(id));
        }
        let before = runtime.world.snapshot();
        let mut incoming = Runtime::default();
        let route = Arc::new(
            Route::compile(
                vec![Segment {
                    stage: Stage::LineUp,
                    points: vec![point(200.0), point(250.0)],
                    knots: 10.0,
                    resources: vec!["RUNWAY/TEST/01".into()],
                    dwell_seconds: 0.0,
                }],
                2000.0,
                2000.0,
            )
            .unwrap(),
        );
        incoming.flights.insert(
            3,
            Arc::new(flight::Prepared {
                spacing_floor: None,
                route,
                airspeeds: BTreeMap::new(),
                recovery: BTreeMap::new(),
                clearances: BTreeMap::new(),
                vacated_reports: BTreeMap::new(),
                amendment: None,
            }),
        );
        incoming.frames.push(frame(3));
        assert_eq!(promote_apron(&mut runtime, &mut incoming), 1);
        assert!(runtime.flights.contains_key(&2));
        assert!(!runtime.flights.contains_key(&1));
        assert!(incoming.frames.is_empty());
        for (after, before) in runtime.world.snapshot().iter().zip(before) {
            assert_eq!(
                (after.id, after.segment, after.position),
                (before.id, before.segment, before.position),
                "Connecting a departure must not move either taxiing aircraft"
            );
        }
    }

    #[test]
    fn retirement_waits_for_sim_time_and_preserves_heard_nearby_and_converging_targets() {
        use openatc_core::{
            state::GeneratedTraffic,
            traffic::{animation::Animation, csl::Model},
        };
        let mut state = crate::test_state();
        state.session.telemetry = Telemetry {
            position_valid: true,
            on_ground: true,
            com1_khz: 118_000,
            simulation_seconds: Some(0.0),
            ..Default::default()
        };
        let model = super::super::Arc::new(Model {
            id: "fixture".into(),
            objects: Vec::new(),
            vertical_offset: None,
            matches: Vec::new(),
        });
        state.generated.frames = (1..=4)
            .map(|id| GeneratedTraffic {
                target: TrafficTarget {
                    id,
                    callsign: format!("TEST{id}"),
                    latitude: if id == 4 { 0.1 } else { 1.0 },
                    on_ground: id != 3,
                    track_degrees: if id == 3 { 180.0 } else { 0.0 },
                    ground_speed_knots: Some(if id == 3 { 200.0 } else { 0.0 }),
                    com1_khz: Some(if id == 2 { 118_000 } else { 121_700 }),
                    ..Default::default()
                },
                model: model.clone(),
                animation: Animation::default(),
                vertical_speed_fpm: 0.0,
                body_heading_degrees: None,
            })
            .collect();
        maintain(&mut state);
        state.session.telemetry.simulation_seconds = Some(59.0);
        maintain(&mut state);
        assert_eq!(state.generated.frames.len(), 4);
        state.session.telemetry.simulation_seconds = Some(61.0);
        state.session.telemetry.paused = true;
        maintain(&mut state);
        assert_eq!(state.generated.frames.len(), 4);
        state.session.telemetry.paused = false;
        maintain(&mut state);
        assert_eq!(
            state
                .generated
                .frames
                .iter()
                .map(|frame| frame.target.id)
                .collect::<Vec<_>>(),
            [2, 3, 4]
        );
        state.session.telemetry.com1_khz = 119_000;
        maintain(&mut state);
        state.session.telemetry.simulation_seconds = Some(10.0);
        maintain(&mut state);
        state.session.telemetry.simulation_seconds = Some(69.0);
        maintain(&mut state);
        assert_eq!(state.generated.frames.len(), 3);
        state.session.telemetry.simulation_seconds = Some(70.0);
        maintain(&mut state);
        assert_eq!(
            state
                .generated
                .frames
                .iter()
                .map(|frame| frame.target.id)
                .collect::<Vec<_>>(),
            [3, 4]
        );
    }

    #[test]
    fn retirement_requires_distance_divergence_and_no_radio_reception() {
        let telemetry = Telemetry {
            position_valid: true,
            on_ground: true,
            ..Default::default()
        };
        let mut target = TrafficTarget {
            latitude: 1.0,
            track_degrees: 0.0,
            ground_speed_knots: Some(250.0),
            ..Default::default()
        };
        assert!(outside(&target, &telemetry, 50.0, false));
        assert!(!outside(&target, &telemetry, 50.0, true));
        target.track_degrees = 180.0;
        assert!(!outside(&target, &telemetry, 50.0, false));
        target.latitude = 0.5;
        target.track_degrees = 0.0;
        assert!(!outside(&target, &telemetry, 50.0, false));
    }

    #[test]
    fn nearby_star_entry_requests_one_scene_refresh() {
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.session.telemetry = Telemetry {
            position_valid: true,
            on_ground: false,
            latitude: 0.0,
            longitude: 0.0,
            ..Default::default()
        };
        state.generated.destination =
            Some(super::super::Arc::new(openatc_core::airport::Airport {
                icao: "TEST".to_owned(),
                reference_latitude: 0.0,
                reference_longitude: 0.3,
                ..Default::default()
            }));
        state.generated.scene_revision = 5;
        let telemetry = state.session.telemetry.clone();
        request_nearby_scene(&mut state.generated, &telemetry, true, None);
        assert_eq!(state.generated.scene_revision, 6);
        request_nearby_scene(&mut state.generated, &telemetry, true, None);
        assert_eq!(state.generated.scene_revision, 6);
    }

    #[test]
    fn a_high_arrival_prepares_the_scene_before_the_star() {
        fn state_at(agl: f64) -> crate::EngineState {
            let mut state = crate::test_state();
            state.settings.generated_traffic = true;
            state.session.telemetry = Telemetry {
                position_valid: true,
                on_ground: false,
                latitude: 0.0,
                longitude: 0.0,
                height_agl_feet: agl,
                ..Default::default()
            };
            state
        }
        fn request(state: &mut crate::EngineState, nm: f64) {
            let telemetry = state.session.telemetry.clone();
            request_nearby_scene(
                &mut state.generated,
                &telemetry,
                true,
                Some(("TEST".to_owned(), 0.0, nm / 60.0)),
            );
        }
        let mut high = state_at(12_000.0);
        let before = high.generated.scene_revision;
        request(&mut high, 85.0);
        assert_eq!(high.generated.scene_revision, before.wrapping_add(1));
        let early = high.generated.scene_revision;
        request(&mut high, 18.0);
        assert_eq!(
            high.generated.scene_revision,
            early.wrapping_add(1),
            "the terminal refresh is not blocked by an early attempt"
        );

        let mut distant = state_at(12_000.0);
        let before = distant.generated.scene_revision;
        request(&mut distant, 95.0);
        assert_eq!(distant.generated.scene_revision, before);

        let mut low = state_at(2_000.0);
        let before = low.generated.scene_revision;
        request(&mut low, 36.0);
        assert_eq!(
            low.generated.scene_revision, before,
            "a low aircraft outside the terminal radius waits"
        );
        let terminal = low.generated.scene_revision;
        request(&mut low, 25.0);
        assert_eq!(low.generated.scene_revision, terminal.wrapping_add(1));

        let mut ground = state_at(12_000.0);
        ground.session.telemetry.on_ground = true;
        let before = ground.generated.scene_revision;
        request(&mut ground, 20.0);
        assert_eq!(ground.generated.scene_revision, before);
    }
}
