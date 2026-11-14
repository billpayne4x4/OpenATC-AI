//! Ambient airport traffic.
//!
//! Keeps the airport alive around the player with surface departures, arrivals and the
//! runway queue. Complete flight planning and the SID/STAR/approach planners live in
//! `generated`; this module owns the aircraft that are actually moving.

use crate::traffic::{phase::TrafficPhase, phases, separation};
use openatc_core::airport::Airport;
use openatc_core::apply::{RunwaySequence, TakeoffDecision};
use openatc_core::state::{GeneratedTraffic, Point, TakeoffState, TrafficDebug, TrafficTarget};
use openatc_core::traffic::animation::{self, Animation, Handling, Kind};
use openatc_core::traffic::csl::{Catalogue, Model};
use openatc_core::traffic::motion::{Stage, Waypoint};
use openatc_core::traffic::profiles::{Aircraft as Profile, Airline, Category, Profiles};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(test)]
const TAXI_STRAIGHT_METERS_PER_SECOND: f64 = 25.0 * 0.514_444;

const KNOTS_TO_MPS: f64 = 0.514_444;
/// Same-runway distance from the threshold before the next departure may roll.
const SAME_RUNWAY_DEPARTURE_SEPARATION_METERS: f64 = 1_800.0;

/// Minimum distance from a runway-flagged graph edge for spawn, hold and seed points.
const RUNWAY_EDGE_MARGIN: f64 = 25.0;
/// Preferred spacing between seeded departures along the surface route.
const SEED_SPACING: f64 = 150.0;
/// Space kept between the last queued departure and the runway hold.
const HOLD_QUEUE_MARGIN: f64 = 60.0;
/// How long traffic may wait behind a stopped player before the player is asked to move.
const MOVE_UP_AFTER: Duration = Duration::from_secs(20);
/// Minimum gap between two requests for the player to keep taxiing.
const MOVE_UP_REPEAT: Duration = Duration::from_secs(90);
/// Deterministic mixture used for the random GA share and weighted operator draw.
fn mix(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// Pick one item from weighted candidates. Weights are relative, not percentages.
fn pick_weighted<T: Clone>(items: &[(u64, T)], seed: u64) -> Option<T> {
    let total: u64 = items.iter().map(|(weight, _)| *weight).sum();
    if total == 0 {
        return None;
    }
    let mut roll = mix(seed) % total;
    for (weight, item) in items {
        if roll < *weight {
            return Some(item.clone());
        }
        roll -= *weight;
    }
    items.last().map(|(_, item)| item.clone())
}

/// Score a model for one fleet type. Exact operator paint wins, then an unmarked
/// model of the same type. A different airline's paint is never accepted.
fn find_model(
    models: &Catalogue,
    code: &str,
    profile: &Profile,
    airline: &Airline,
    seed: u64,
) -> Option<Arc<Model>> {
    let codes: BTreeSet<&str> = std::iter::once(code)
        .chain(profile.model_aliases.iter().map(String::as_str))
        .collect();
    let paints: BTreeSet<&str> = std::iter::once(airline.operator.as_str())
        .chain(airline.liveries.iter().map(String::as_str))
        .filter(|paint| !paint.is_empty())
        .collect();
    let mut best = 0;
    let mut choices = Vec::new();
    for model in &models.models {
        let score = model
            .matches
            .iter()
            .filter(|entry| codes.contains(entry.aircraft.as_str()))
            .map(|entry| {
                if paints.contains(entry.operator.as_str()) {
                    3
                } else if entry.operator.is_empty() {
                    1
                } else {
                    0
                }
            })
            .max()
            .unwrap_or(0);
        if score > best {
            best = score;
            choices.clear();
        }
        if score > 0 && score == best {
            choices.push(model);
        }
    }
    if choices.is_empty() {
        None
    } else {
        Some(Arc::new(
            choices[(mix(seed) % choices.len() as u64) as usize].clone(),
        ))
    }
}

#[derive(Clone)]
struct AmbientCandidate {
    callsign: String,
    aircraft: String,
    profile: Profile,
    model: Arc<Model>,
    wake_category: Option<u8>,
}

fn wake_category(profile: &Profile) -> Option<u8> {
    match profile.wake {
        'L' => Some(0),
        'M' => Some(1),
        'H' => Some(2),
        'J' => Some(3),
        _ => None,
    }
}

fn scheduled_callsign(airline: &Airline, airport: &str, seed: u64) -> Option<String> {
    if airline.operator.is_empty() {
        airline.registration(airport, seed)
    } else {
        Some(format!("{}{}", airline.operator, 100 + mix(seed) % 8900))
    }
}

fn airport_runway_length(airport: &Airport, runway: &str) -> f64 {
    airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)
        .map_or(2_000.0, |runway| {
            (runway.second.east - runway.first.east).hypot(runway.second.north - runway.first.north)
        })
}

fn airline_candidates(
    profiles: &Profiles,
    models: &Catalogue,
    airport: &str,
    runway_meters: f64,
    seed: u64,
    used: &BTreeSet<String>,
    player: &str,
    use_hub: bool,
    airline_weight: u64,
    regional_weight: u64,
    cargo_weight: u64,
) -> Vec<(u64, AmbientCandidate)> {
    let hub = use_hub.then(|| profiles.hubs.get(airport)).flatten();
    let mut candidates = Vec::new();
    for (id, airline) in &profiles.airlines {
        let category_weight = match airline.category {
            Category::Airline => airline_weight,
            Category::Regional => regional_weight,
            Category::Cargo => cargo_weight,
            _ => continue,
        };
        if category_weight == 0 {
            continue;
        }
        let hub_weight = hub.and_then(|hub| hub.airlines.get(id)).copied();
        if hub.is_some() && hub_weight.is_none() {
            continue;
        }
        if hub.is_none() {
            let area = airline.home_prefixes.is_empty()
                || airline
                    .home_prefixes
                    .iter()
                    .any(|prefix| airport.starts_with(prefix));
            let serves = airline.serves.is_empty()
                || airline
                    .serves
                    .iter()
                    .any(|prefix| airport.starts_with(prefix));
            if !area || !serves {
                continue;
            }
        }
        for (kind, fleet_weight) in &airline.fleet {
            let Some(profile) = profiles.aircraft.get(kind) else {
                continue;
            };
            if runway_meters + 50.0 < profile.runway {
                continue;
            }
            let Some(model) = find_model(models, kind, profile, airline, seed) else {
                continue;
            };
            let Some(callsign) =
                scheduled_callsign(airline, airport, seed.wrapping_add(*fleet_weight as u64))
            else {
                continue;
            };
            if used.contains(&callsign) || callsign.eq_ignore_ascii_case(player) {
                continue;
            }
            let weight =
                u64::from(*fleet_weight) * u64::from(hub_weight.unwrap_or(1)) * category_weight;
            if weight == 0 {
                continue;
            }
            candidates.push((
                weight,
                AmbientCandidate {
                    callsign,
                    aircraft: kind.clone(),
                    profile: profile.clone(),
                    model,
                    wake_category: wake_category(profile),
                },
            ));
        }
    }
    candidates
}

fn ga_candidates(
    profiles: &Profiles,
    models: &Catalogue,
    airport: &str,
    runway_meters: f64,
    seed: u64,
    used: &BTreeSet<String>,
    player: &str,
) -> Vec<(u64, AmbientCandidate)> {
    let Some(airline) = profiles.airlines.get("GA") else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for (kind, fleet_weight) in &airline.fleet {
        let Some(profile) = profiles.aircraft.get(kind) else {
            continue;
        };
        if runway_meters + 50.0 < profile.runway {
            continue;
        }
        let Some(model) = find_model(models, kind, profile, airline, seed) else {
            continue;
        };
        let Some(callsign) =
            scheduled_callsign(airline, airport, seed.wrapping_add(*fleet_weight as u64))
        else {
            continue;
        };
        if used.contains(&callsign) || callsign.eq_ignore_ascii_case(player) {
            continue;
        }
        candidates.push((
            u64::from(*fleet_weight).max(1),
            AmbientCandidate {
                callsign,
                aircraft: kind.clone(),
                profile: profile.clone(),
                model,
                wake_category: wake_category(profile),
            },
        ));
    }
    candidates
}

/// Choose an airline or GA aircraft that has an installed model and fits the runway.
fn ambient_identity(
    profiles: &Profiles,
    models: &Catalogue,
    airport: &str,
    runway_meters: f64,
    ga: bool,
    seed: u64,
    used: &BTreeSet<String>,
    player: &str,
    airline_weight: u64,
    regional_weight: u64,
    cargo_weight: u64,
) -> Option<AmbientCandidate> {
    let mut candidates = if ga {
        ga_candidates(profiles, models, airport, runway_meters, seed, used, player)
    } else {
        Vec::new()
    };
    if candidates.is_empty() {
        candidates = airline_candidates(
            profiles,
            models,
            airport,
            runway_meters,
            seed,
            used,
            player,
            true,
            airline_weight,
            regional_weight,
            cargo_weight,
        );
    }
    if candidates.is_empty() && profiles.hubs.get(airport).is_none() {
        candidates = airline_candidates(
            profiles,
            models,
            airport,
            runway_meters,
            seed,
            used,
            player,
            false,
            airline_weight,
            regional_weight,
            cargo_weight,
        );
    }
    pick_weighted(&candidates, seed)
}

/// How the ambient traffic is using one runway.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunwayState {
    /// No aircraft using the runway.
    Clear,
    /// An aircraft is lined up and waiting.
    LineUp,
    /// An aircraft has started its takeoff roll.
    Rolling,
    /// Landing traffic is on or immediately over the runway.
    Landing,
}

/// The player's own use of a departure runway.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerRunway {
    /// Lined up and waiting.
    pub lined_up: bool,
    /// Cleared for takeoff and rolling.
    pub rolling: bool,
}

#[derive(Clone, Copy, Default)]
struct RunwaySlots {
    departing: Option<u32>,
    line_up: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AmbientKind {
    Departure,
    Arrival,
}

struct AmbientAircraft {
    id: u32,
    callsign: String,
    aircraft: String,
    profile: Profile,
    wake_category: Option<u8>,
    airport: String,
    runway: String,
    route: String,
    spawn_node: i64,
    model: Arc<Model>,
    kind: AmbientKind,
    points: Vec<Waypoint>,
    cumulative: Vec<f64>,
    ground_start: f64,
    ground_end: f64,
    taxi_end: f64,
    runway_start: f64,
    runway_end: f64,
    hold_distance: f64,
    hold: Option<[f64; 2]>,
    animation: Animation,
    handling: Arc<Handling>,
    taxi_knots: f64,
    taxi_turn_knots: f64,
    climb_speed: f64,
    /// Lowest safe climb speed; the anti-overtake cap cannot go below it.
    minimum_climb_speed: f64,
    climb_fpm: f64,
    descent_fpm: f64,
    approach_speed: f64,
    touchdown_speed: f64,
    distance: f64,
    cleared: bool,
    takeoff: bool,
    hold_since: Option<Instant>,
    requested: bool,
    speed: f64,
    motion: crate::traffic::motion::MotionController,
    blocked: bool,
    /// Aircraft or player currently causing the block, when one exists.
    blocker: Option<u32>,
    /// Waiting for a departure clearance at the hold.
    awaiting_clearance: bool,
    /// Remaining wake delay for the next line-up or takeoff clearance.
    wake_delay: f64,
    last: Instant,
    frequency_khz: Option<i32>,
    /// Aircraft or player this aircraft is giving way to at a junction.
    yield_to: Option<u32>,
    /// Whether the give-way call was heard, so a resume call is owed when the yield ends.
    yield_announced: bool,
    /// When the player began holding this aircraft on the taxiway.
    player_hold_since: Option<Instant>,
}

/// Pick a stable density target for one airport and traffic-settings key.
fn density_target(seed: u64, key: &str, minimum: usize, maximum: usize) -> usize {
    let span = maximum.saturating_sub(minimum) + 1;
    let mut mixed = seed;
    for byte in key.bytes() {
        mixed = mixed
            .wrapping_mul(1_099_511_628_211)
            .wrapping_add(u64::from(byte));
    }
    minimum + (mix(mixed) % span as u64) as usize
}

/// Owns the ambient airport traffic. Not part of `generated::Runtime`.
#[derive(Default)]
pub struct TrafficManager {
    aircraft: Vec<AmbientAircraft>,
    models: Option<Arc<Catalogue>>,
    last_spawn: Option<Instant>,
    next_id: u32,
    /// Ambient frames currently present in the published traffic set.
    published: BTreeSet<u32>,
    /// Settings and runway that selected the current density target.
    density_key: String,
    /// Stable ambient ground target for the current airport and traffic settings.
    density_target: usize,
    /// Nodes with no usable surface route for a given aircraft size.
    rejected_spawn_nodes: BTreeSet<(char, i64)>,
    attempts: u32,
    rejected_route: u32,
    rejected_pavement: u32,
    rejected_model: u32,
    last_error: String,
    announced: std::collections::BTreeSet<(u32, &'static str)>,
    /// Last time the player was asked to continue taxiing.
    move_request_at: Option<Instant>,
    /// When the player reached the departure hold for the current route.
    player_hold_since: Option<Instant>,
    /// Traffic the player has been told to give way to, until the conflict clears.
    player_yield: Option<u32>,
    movement_started: bool,
    active_airport: String,
    generation: u64,
}

impl TrafficManager {
    /// Runway use for sequencing the player and the ambient queue.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn runway_state(&self, airport: &str, runway: &str) -> RunwayState {
        self.runway_state_for(airport, runway, None, None, None)
    }

    /// Runway use with the player's place in the departure queue.
    ///
    /// An ambient departure still taxiing behind the player is not the runway leader,
    /// even if it has already been given a line-up clearance.
    #[must_use]
    pub(crate) fn runway_state_for(
        &self,
        airport: &str,
        runway: &str,
        _player_remaining: Option<f64>,
        player: Option<PlayerRunway>,
        exclude: Option<u32>,
    ) -> RunwayState {
        let mut line_up = player.is_some_and(|player| player.lined_up && !player.rolling);
        let mut rolling = player.is_some_and(|player| player.rolling);
        let mut landing = false;
        for aircraft in &self.aircraft {
            if aircraft.airport != airport
                || aircraft.runway != runway
                || Some(aircraft.id) == exclude
            {
                continue;
            }
            match aircraft.kind {
                AmbientKind::Departure => {
                    if aircraft.cleared {
                        if aircraft.takeoff {
                            let separation_end = (aircraft.runway_start
                                + SAME_RUNWAY_DEPARTURE_SEPARATION_METERS)
                                .min(aircraft.runway_end);
                            let clear_of_runway = aircraft.distance >= aircraft.ground_end
                                && aircraft.distance >= separation_end;
                            if aircraft.distance >= aircraft.runway_start && !clear_of_runway {
                                rolling = true;
                            } else if aircraft.distance < aircraft.runway_start {
                                line_up = true;
                            }
                        } else {
                            // The line-up clearance reserves the slot before the aircraft
                            // reaches the hold line.
                            line_up = true;
                        }
                    }
                }
                AmbientKind::Arrival => {
                    if aircraft.on_runway() {
                        landing = true;
                    }
                }
            }
        }
        if landing {
            RunwayState::Landing
        } else if line_up {
            // A line-up clearance owns the slot even while an earlier departure is
            // still inside the separation window. The rolling state must not hide it.
            RunwayState::LineUp
        } else if rolling {
            RunwayState::Rolling
        } else {
            RunwayState::Clear
        }
    }

    /// Tracks the one departing and one lined-up aircraft on a physical runway.
    fn runway_slots(&self, airport: &str, runway: &str) -> RunwaySlots {
        let mut slots = RunwaySlots::default();
        for aircraft in &self.aircraft {
            if aircraft.kind != AmbientKind::Departure
                || aircraft.airport != airport
                || aircraft.runway != runway
            {
                continue;
            }
            if aircraft.takeoff && aircraft.distance < aircraft.runway_end + 300.0 {
                slots.departing.get_or_insert(aircraft.id);
            } else if aircraft.cleared && !aircraft.takeoff {
                slots.line_up.get_or_insert(aircraft.id);
            }
        }
        slots
    }

    /// The ambient departure already rolling on a runway, if any.
    #[must_use]
    pub fn rolling_departure_id(&self, airport: &str, runway: &str) -> Option<u32> {
        self.aircraft
            .iter()
            .find(|aircraft| {
                aircraft.kind == AmbientKind::Departure
                    && aircraft.airport == airport
                    && aircraft.runway == runway
                    && aircraft.takeoff
                    && aircraft.distance >= aircraft.runway_start
                    && aircraft.distance < aircraft.runway_end + 600.0
            })
            .map(|aircraft| aircraft.id)
    }

    /// Whether the manager still needs the CSL catalogue before it can spawn.
    #[must_use]
    pub fn needs_models(&self) -> bool {
        self.models.is_none()
    }

    /// Install the scanned CSL catalogue.
    pub fn set_models(&mut self, catalogue: Arc<Catalogue>) {
        self.models = Some(catalogue);
    }

    /// Number of ambient aircraft currently alive.
    #[must_use]
    pub fn len(&self) -> usize {
        self.aircraft.len()
    }

    /// Whether the ambient layer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.aircraft.is_empty()
    }

    /// Advance ambient traffic and publish frames. Called once per telemetry update.
    pub fn tick(&mut self, state: &mut crate::EngineState) {
        self.advance(state);
        self.publish(state);
    }

    fn advance(&mut self, state: &mut crate::EngineState) {
        state
            .session
            .generated_animation_channels
            .clone_from(&state.traffic_channels);
        if self.generation != state.session_generation {
            self.generation = state.session_generation;
            self.aircraft.clear();
            self.announced.clear();
            self.movement_started = false;
            self.last_spawn = None;
            self.active_airport.clear();
            self.density_key.clear();
            self.density_target = 0;
            self.rejected_spawn_nodes.clear();
        }
        let Some(models) = self.models.clone() else {
            return;
        };
        if state.session.telemetry.paused {
            // Keep the published poses where they are and reset the movement clock so
            // the first frame after unpausing does not apply the whole pause as dt.
            let now = Instant::now();
            for aircraft in &mut self.aircraft {
                aircraft.last = now;
            }
            return;
        }
        self.player_hold_since = state.flow.takeoff_hold_since;
        let telemetry = &state.session.telemetry;
        let own = telemetry
            .position_valid
            .then_some([telemetry.latitude, telemetry.longitude]);
        let arrival_phase = matches!(
            state.session.phase,
            openatc_core::state::PhaseCode::Approach
                | openatc_core::state::PhaseCode::Landed
                | openatc_core::state::PhaseCode::TaxiIn
        );
        let near_arrival = (!telemetry.on_ground || arrival_phase)
            .then(|| {
                state.arrival_airport.as_ref().filter(|airport| {
                    own.is_some_and(|own| {
                        openatc_core::arrival::distance_nm(
                            own,
                            [airport.reference_latitude, airport.reference_longitude],
                        ) <= 30.0
                    })
                })
            })
            .flatten();
        let Some(airport) = near_arrival.cloned().or_else(|| state.airport.clone()) else {
            return;
        };
        if self.active_airport != airport.icao {
            self.aircraft.clear();
            self.announced.clear();
            self.movement_started = false;
            self.last_spawn = None;
            self.active_airport.clone_from(&airport.icao);
            self.density_key.clear();
            self.density_target = 0;
            self.rejected_spawn_nodes.clear();
        }
        if telemetry.on_ground && telemetry.ground_speed_knots > 1.0 {
            self.movement_started = true;
        }
        let away = own.is_some_and(|own| {
            openatc_core::arrival::distance_nm(
                own,
                [airport.reference_latitude, airport.reference_longitude],
            ) > 60.0
        });
        if away && self.aircraft.is_empty() {
            state.airports.prune(own, 60.0);
            return;
        }
        state.airports.ensure(Arc::new(airport.clone()));
        // Ambient frames are published after this tick, so add their current state here.
        let mut traffic = state.traffic_targets();
        traffic.extend(
            self.aircraft
                .iter()
                .filter_map(ambient_frame)
                .map(|frame| frame.target),
        );
        traffic.sort_by_key(|target| target.id);
        traffic.dedup_by_key(|target| target.id);
        if telemetry.position_valid {
            let player_aircraft = if telemetry.aircraft_icao.is_empty() {
                state.session.plan.aircraft.clone()
            } else {
                telemetry.aircraft_icao.clone()
            };
            traffic.push(TrafficTarget {
                id: 0,
                callsign: state.session.plan.callsign.clone(),
                aircraft: player_aircraft,
                latitude: telemetry.latitude,
                longitude: telemetry.longitude,
                altitude_feet: telemetry.altitude_feet,
                track_degrees: telemetry
                    .ground_track_degrees
                    .unwrap_or(telemetry.heading_degrees),
                on_ground: telemetry.on_ground,
                ground_speed_knots: Some(telemetry.ground_speed_knots),
                wingspan_meters: telemetry.wingspan_meters,
                wake_category: telemetry.wake_category,
                runway_occupancy: Some(
                    telemetry.on_ground
                        && openatc_core::airport::aircraft_on_runway(&airport, telemetry),
                ),
                ..Default::default()
            });
        }
        let minimum = usize::from(state.settings.traffic_taxi_min).min(16);
        let maximum = usize::from(state.settings.traffic_taxi_max)
            .min(16)
            .max(minimum);
        let density_key = format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}",
            airport.icao,
            state.session.plan.runway,
            state.session.plan.arrival_runway,
            minimum,
            maximum,
            state.settings.airline_traffic,
            state.settings.regional_traffic,
            state.settings.cargo_traffic,
            state.settings.ga_traffic
        );
        if self.density_key != density_key {
            self.density_key.clone_from(&density_key);
            self.density_target =
                density_target(state.traffic_seed, &density_key, minimum, maximum);
            self.rejected_spawn_nodes.clear();
        }
        let target = self.density_target;
        let now = Instant::now();
        let missing = target.saturating_sub(self.aircraft.len());
        // Keep the airport alive while the player is on the ground, including during
        // taxi. Spawns stop on the takeoff roll so nothing appears beside a rolling
        // aircraft.
        let airport_active = telemetry.on_ground && telemetry.ground_speed_knots < 40.0;
        if missing > 0
            && !away
            && airport_active
            && self
                .last_spawn
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(4))
        {
            self.spawn(&airport, &models, state, missing);
            self.last_spawn = Some(now);
        }

        let dt = self.aircraft.first().map_or(0.016, |aircraft| {
            now.duration_since(aircraft.last)
                .as_secs_f64()
                .clamp(0.0, 0.2)
        });
        let player_runway = state.session.plan.runway.clone();
        let player_takeoff = state.session.taxi_clearance.takeoff_state;
        let player_runway_state = (telemetry.on_ground
            && !state.session.has_departed
            && state.session.plan.runway == player_runway
            && matches!(
                player_takeoff,
                TakeoffState::LineUpAndWait | TakeoffState::Cleared
            ))
        .then_some(PlayerRunway {
            lined_up: player_takeoff == TakeoffState::LineUpAndWait,
            rolling: player_takeoff == TakeoffState::Cleared,
        });
        let player_departing = matches!(
            state.session.phase,
            openatc_core::state::PhaseCode::Taxi | openatc_core::state::PhaseCode::Departure
        ) && telemetry.on_ground;
        let player_remaining = (player_departing && state.session.plan.runway == player_runway)
            .then(|| player_runway_remaining(&airport, &player_runway, telemetry))
            .flatten();
        for index in 0..self.aircraft.len() {
            let (on_ground, distance) = {
                let aircraft = &self.aircraft[index];
                let on_ground = match aircraft.kind {
                    AmbientKind::Departure => aircraft.distance < aircraft.ground_end,
                    AmbientKind::Arrival => aircraft.distance >= aircraft.ground_start,
                };
                (on_ground, aircraft.distance)
            };
            // A departure on its takeoff roll is separated by runway occupancy and wake
            // spacing, never by taxi car-following.
            let rolling = self.aircraft[index].takeoff
                || (self.aircraft[index].kind == AmbientKind::Departure
                    && distance >= self.aircraft[index].runway_start - 2.0);
            // Follow the player like any other aircraft ahead, including around a corner
            // where the straight-line check cannot see it.
            let own_length = self.aircraft[index].profile.length;
            let player_leader = (on_ground && !rolling)
                .then(|| traffic.iter().find(|target| target.id == 0))
                .flatten()
                .and_then(|player| {
                    let gap = phases::ground::player_gap_ahead(
                        &self.aircraft[index].points,
                        &self.aircraft[index].cumulative,
                        self.aircraft[index].distance,
                        &airport,
                        player,
                        own_length,
                    )?;
                    let player_length = openatc_core::traffic::profiles::aircraft(&player.aircraft)
                        .map_or(40.0, |profile| profile.length);
                    Some(separation::Ahead {
                        id: 0,
                        gap,
                        required: separation::player_gap(own_length, player_length),
                        speed: player.ground_speed_knots.unwrap_or(0.0) * KNOTS_TO_MPS,
                    })
                });
            let (latitude, longitude, heading) = position_heading_at(
                &self.aircraft[index].points,
                &self.aircraft[index].cumulative,
                distance,
            );
            // A merge can put two aircraft on the same point without either seeing the
            // other ahead. The lower-priority aircraft stops, and the winner ignores it
            // so the pair cannot deadlock.
            let (overlap_stop, overlap_ignore) = if on_ground {
                let closest = self
                    .aircraft
                    .iter()
                    .enumerate()
                    .filter_map(|(other_index, other)| {
                        if other_index == index {
                            return None;
                        }
                        let other_on_ground = match other.kind {
                            AmbientKind::Departure => other.distance < other.ground_end,
                            AmbientKind::Arrival => other.distance >= other.ground_start,
                        };
                        if !other_on_ground {
                            return None;
                        }
                        let (other_latitude, other_longitude, _) =
                            position_heading_at(&other.points, &other.cumulative, other.distance);
                        let gap = openatc_core::arrival::distance_nm(
                            [latitude, longitude],
                            [other_latitude, other_longitude],
                        ) * 1852.0;
                        let required = separation::required_gap(
                            self.aircraft[index].profile.length,
                            other.profile.length,
                        );
                        (gap < required).then_some((gap, other.id, other))
                    })
                    .min_by(|left, right| left.0.total_cmp(&right.0));
                match closest {
                    Some((_, other_id, other)) => {
                        let key = |aircraft: &AmbientAircraft, along: f64| {
                            let remaining = match aircraft.kind {
                                AmbientKind::Departure => aircraft.hold_distance - along,
                                AmbientKind::Arrival => -along,
                            };
                            // A departure already cleared onto the runway owns the merge.
                            // Comparing only distance can let an uncleared aircraft hold it.
                            let priority = match aircraft.kind {
                                AmbientKind::Departure if aircraft.takeoff => 0,
                                AmbientKind::Departure if aircraft.cleared => 1,
                                _ => 2,
                            };
                            (priority, remaining, aircraft.id)
                        };
                        if key(&self.aircraft[index], distance) > key(other, other.distance) {
                            (Some(other_id), None)
                        } else {
                            (None, Some(other_id))
                        }
                    }
                    None => (None, None),
                }
            } else {
                (None, None)
            };
            let own_runway = self.aircraft[index].runway.clone();
            let own_route = self.aircraft[index].route.clone();
            let ahead = (on_ground && !rolling)
                .then(|| {
                    separation::closest_ahead(
                        latitude,
                        longitude,
                        heading,
                        self.aircraft[index].profile.length,
                        self.aircraft[index].id,
                        &traffic,
                        |id| {
                            self.aircraft.iter().find(|other| other.id == id).map_or(
                                Some(150.0),
                                |other| {
                                    if other.runway != own_runway {
                                        None
                                    } else if own_route.is_empty() || other.route == own_route {
                                        Some(150.0)
                                    } else {
                                        Some(90.0)
                                    }
                                },
                            )
                        },
                    )
                })
                .flatten()
                .filter(|leader| Some(leader.id) != overlap_ignore);
            let ahead = player_leader
                .filter(|player| ahead.is_none_or(|leader| player.gap < leader.gap))
                .or(ahead);
            let follow_speed = ahead.map_or(f64::INFINITY, |leader| {
                separation::follow_speed(
                    leader,
                    self.aircraft[index].taxi_knots,
                    self.aircraft[index].handling.braking_knots_sec.max(0.1) * KNOTS_TO_MPS,
                )
            });
            let physical_blocked = follow_speed <= 0.01;
            let own_runway = self.aircraft[index].runway.clone();
            let own_airport = self.aircraft[index].airport.clone();
            let own_id = self.aircraft[index].id;
            let queue_lead = queue_leader(&self.aircraft, self.player_hold_since);
            if self.aircraft[index].on_runway() {
                state.airports.claim(&own_airport, &own_runway, own_id);
            } else {
                state.airports.release(&own_airport, &own_runway, own_id);
            }
            let slots = self.runway_slots(&own_airport, &own_runway);
            let rolling_lead = slots.departing.filter(|id| *id != own_id);
            let line_up_ahead = slots.line_up.is_some_and(|id| id != own_id);
            let rolling_clear = rolling_lead
                .and_then(|id| self.aircraft.iter().find(|aircraft| aircraft.id == id))
                .is_some_and(|aircraft| aircraft.distance >= aircraft.runway_start + 300.0);
            let player_on_runway = telemetry.on_ground
                && openatc_core::airport::aircraft_on_named_runway(
                    &airport,
                    &own_runway,
                    telemetry,
                );
            let runway_blocked = self.aircraft[index].kind == AmbientKind::Departure
                && (player_on_runway
                    || state
                        .airports
                        .occupied_by_other(&own_airport, &own_runway, own_id))
                && self.aircraft[index].approaching_runway(5.0)
                && (rolling_lead.is_none() || self.aircraft[index].takeoff);
            // Give way to traffic on the right at a converging taxiway. The higher id yields
            // when both aircraft see each other, which breaks the mutual-yield case.
            let leader_id = ahead.map(|leader| leader.id);
            let own_position = position_heading_at(
                &self.aircraft[index].points,
                &self.aircraft[index].cumulative,
                distance,
            );
            let right_blocker = if on_ground && queue_lead != Some(self.aircraft[index].id) {
                let ambient = self
                    .aircraft
                    .iter()
                    .enumerate()
                    .find_map(|(other_index, other)| {
                        if other_index == index
                            || other.on_runway()
                            || Some(other.id) == overlap_ignore
                            || Some(other.id) == leader_id
                            || (!other.route.is_empty()
                                && other.route == self.aircraft[index].route)
                        {
                            return None;
                        }
                        let (other_lat, other_lon, other_heading) =
                            position_heading_at(&other.points, &other.cumulative, other.distance);
                        separation::right_conflict(
                            own_position.0,
                            own_position.1,
                            own_position.2,
                            other_lat,
                            other_lon,
                            other_heading,
                            other.id,
                            leader_id,
                            other.route == self.aircraft[index].route,
                            100.0,
                        )
                        .then_some(other.id)
                    });
                ambient.or_else(|| {
                    // A joining ambient aircraft gives way to a moving player on its right.
                    // A stopped player is not joining anything, so the aircraft carries on.
                    traffic
                        .iter()
                        .find(|target| {
                            target.id == 0
                                && target.on_ground
                                && target.ground_speed_knots.unwrap_or(0.0) > 1.0
                        })
                        .and_then(|player| {
                            let p = openatc_core::airport::airport_point(
                                &airport,
                                player.latitude,
                                player.longitude,
                            );
                            if openatc_core::airport::within_runway(&airport, &p) {
                                return None;
                            }
                            separation::right_conflict(
                                own_position.0,
                                own_position.1,
                                own_position.2,
                                player.latitude,
                                player.longitude,
                                player.track_degrees,
                                0,
                                leader_id,
                                false,
                                100.0,
                            )
                            .then_some(0)
                        })
                })
            } else {
                None
            };
            let right_blocked = right_blocker.is_some();
            let hold_for_clearance = self.aircraft[index].kind == AmbientKind::Departure
                && distance >= self.aircraft[index].hold_distance
                && !self.aircraft[index].cleared;
            if hold_for_clearance && self.aircraft[index].hold_since.is_none() {
                self.aircraft[index].hold_since = Some(now);
                self.aircraft[index].requested = true;
            }
            let wake_delay = if self.aircraft[index].kind == AmbientKind::Departure
                && (hold_for_clearance
                    || (self.aircraft[index].cleared && !self.aircraft[index].takeoff))
            {
                let aircraft = &self.aircraft[index];
                let (latitude, longitude) =
                    position_at(&aircraft.points, &aircraft.cumulative, aircraft.distance);
                crate::traffic::departure_wake_delay(
                    state,
                    aircraft.id,
                    &airport,
                    &aircraft.runway,
                    &aircraft.aircraft,
                    latitude,
                    longitude,
                )
            } else {
                0.0
            };
            self.aircraft[index].wake_delay = wake_delay;
            let takeoff_targets: Vec<&TrafficTarget> = traffic
                .iter()
                .filter(|target| target.id != own_id)
                .collect();
            let sequence = match self.runway_state_for(
                &airport.icao,
                &own_runway,
                player_remaining,
                player_runway_state,
                Some(own_id),
            ) {
                RunwayState::Landing => RunwaySequence::Landing,
                RunwayState::LineUp => RunwaySequence::LineUp,
                RunwayState::Rolling => RunwaySequence::Rolling,
                RunwayState::Clear => RunwaySequence::Clear,
            };
            let mut decision = openatc_core::apply::takeoff_decision(
                &airport,
                &own_runway,
                takeoff_targets.iter().copied(),
                sequence,
                wake_delay,
            );
            if decision == openatc_core::apply::TakeoffDecision::Cleared
                && self.performance_hold(index)
            {
                decision = openatc_core::apply::TakeoffDecision::LineUpAndWait;
            }
            let behind_player = player_remaining.is_some_and(|threshold| {
                departure_behind_player(
                    &self.aircraft[index],
                    [telemetry.latitude, telemetry.longitude],
                    &player_runway,
                    threshold,
                )
            });
            let is_lead = queue_lead == Some(own_id) && !behind_player;
            if hold_for_clearance && is_lead {
                match decision {
                    TakeoffDecision::Cleared => {
                        self.aircraft[index].cleared = true;
                        self.aircraft[index].takeoff = true;
                    }
                    TakeoffDecision::LineUpAndWait => {
                        // One aircraft may line up behind a departure that has
                        // already rolled clear of the hold.
                        if !line_up_ahead && (rolling_lead.is_none() || rolling_clear) {
                            self.aircraft[index].cleared = true;
                        }
                    }
                    TakeoffDecision::Hold(_) => {}
                }
            }
            // An aircraft behind the player cannot keep a line-up clearance. It has not
            // entered the runway, so the queue can correct it without a radio call.
            if behind_player
                && self.aircraft[index].cleared
                && !self.aircraft[index].takeoff
                && self.aircraft[index].distance < self.aircraft[index].runway_start - 2.0
            {
                self.aircraft[index].cleared = false;
            }
            if self.aircraft[index].cleared
                && !self.aircraft[index].takeoff
                && distance >= self.aircraft[index].runway_start - 5.0
            {
                let sequence = match self.runway_state_for(
                    &airport.icao,
                    &own_runway,
                    player_remaining,
                    player_runway_state,
                    Some(own_id),
                ) {
                    RunwayState::Landing => RunwaySequence::Landing,
                    RunwayState::LineUp => RunwaySequence::LineUp,
                    RunwayState::Rolling => RunwaySequence::Rolling,
                    RunwayState::Clear => RunwaySequence::Clear,
                };
                let decision = openatc_core::apply::takeoff_decision(
                    &airport,
                    &own_runway,
                    takeoff_targets.iter().copied(),
                    sequence,
                    wake_delay,
                );
                if decision == TakeoffDecision::Cleared && !self.performance_hold(index) {
                    self.aircraft[index].takeoff = true;
                }
            }
            let runway_blocker = if player_on_runway {
                Some(0)
            } else {
                traffic
                    .iter()
                    .find(|target| target.id != own_id && target.runway_occupancy == Some(true))
                    .map(|target| target.id)
            };
            let blocker = if let Some(id) = overlap_stop {
                Some(id)
            } else if physical_blocked {
                ahead.map(|leader| leader.id)
            } else if let Some(id) = right_blocker {
                Some(id)
            } else if runway_blocked {
                runway_blocker
            } else {
                None
            };
            let held_by_player = physical_blocked
                && overlap_stop.is_none()
                && ahead.is_some_and(|leader| leader.id == 0);
            let own_progress = distance - self.aircraft[index].runway_start;
            let flight_leader = climb_leader(&self.aircraft, index);
            let blocked = runway_blocked
                || right_blocked
                || physical_blocked
                || overlap_stop.is_some()
                || hold_for_clearance;
            let aircraft = &mut self.aircraft[index];
            aircraft.last = now;
            aircraft.blocked = blocked;
            aircraft.blocker = blocker;
            aircraft.awaiting_clearance = hold_for_clearance;
            aircraft.yield_to = right_blocker;
            aircraft.player_hold_since = if held_by_player {
                aircraft.player_hold_since.or(Some(now))
            } else {
                None
            };
            let braking = aircraft.handling.braking_knots_sec.max(0.1) * KNOTS_TO_MPS;
            let acceleration = aircraft.handling.acceleration_knots_sec.max(0.1) * KNOTS_TO_MPS;
            let taxi = phases::ground::taxi_speed(
                &aircraft.points,
                &aircraft.cumulative,
                aircraft.distance,
                aircraft.taxi_knots,
                aircraft.taxi_turn_knots,
                braking,
            );
            let (mut target_speed, target_acceleration) = if blocked {
                (0.0, braking)
            } else if aircraft.kind == AmbientKind::Departure
                && aircraft.distance >= aircraft.runway_start
                && aircraft.distance < aircraft.ground_end
            {
                if aircraft.takeoff {
                    let rotation = aircraft.handling.rotation_knots * KNOTS_TO_MPS;
                    let roll = (aircraft.ground_end - aircraft.runway_start).max(1.0);
                    let required = ((rotation * rotation - taxi * taxi).max(0.0) / (2.0 * roll))
                        .max(acceleration);
                    (
                        phases::takeoff::roll_speed(
                            &aircraft.handling,
                            aircraft.distance,
                            aircraft.runway_start,
                            aircraft.ground_end,
                            taxi,
                        ),
                        required,
                    )
                } else {
                    (0.0, braking)
                }
            } else if aircraft.kind == AmbientKind::Departure
                && aircraft.distance < aircraft.runway_start
            {
                if aircraft.cleared {
                    (taxi, acceleration)
                } else {
                    let remaining = (aircraft.hold_distance - aircraft.distance).max(0.0);
                    ((2.0 * braking * remaining).sqrt().min(taxi), acceleration)
                }
            } else if aircraft.kind == AmbientKind::Departure {
                (aircraft.climb_speed, acceleration)
            } else if aircraft.kind == AmbientKind::Arrival
                && aircraft.distance < aircraft.ground_start
            {
                // Final reduction from approach speed to touchdown speed.
                let remaining = (aircraft.ground_start - aircraft.distance).max(0.0);
                let span = aircraft.ground_start.max(1.0);
                (
                    aircraft.approach_speed
                        - (aircraft.approach_speed - aircraft.touchdown_speed)
                            * (1.0 - remaining / span),
                    braking,
                )
            } else if aircraft.kind == AmbientKind::Arrival
                && aircraft.distance < aircraft.runway_end
            {
                (
                    phases::landing::rollout_speed(
                        &aircraft.handling,
                        aircraft.distance,
                        aircraft.ground_start,
                        aircraft.runway_end,
                        taxi,
                        aircraft.touchdown_speed,
                    ),
                    braking,
                )
            } else {
                (taxi, acceleration)
            };
            if let Some((leader_progress, leader_speed)) = flight_leader {
                let gap = leader_progress - own_progress;
                if gap < 3.0 * 1_852.0 {
                    target_speed = target_speed.min(leader_speed);
                    if gap < 1.5 * 1_852.0 {
                        target_speed *= (gap / (1.5 * 1_852.0)).clamp(0.35, 1.0);
                    }
                    // Never command an airborne aircraft below its safe climb speed.
                    target_speed = target_speed.max(aircraft.minimum_climb_speed);
                }
            }
            if target_speed > follow_speed {
                target_speed = follow_speed;
            }
            aircraft.speed = aircraft
                .motion
                .update(dt, target_speed, target_acceleration, braking);
            let heading_before =
                position_heading_at(&aircraft.points, &aircraft.cumulative, aircraft.distance).2;
            aircraft.distance += aircraft.speed * dt;
            let heading_after =
                position_heading_at(&aircraft.points, &aircraft.cumulative, aircraft.distance).2;
            let turn_rate = if dt > 0.0 {
                openatc_core::navigation::turn(heading_before, heading_after) / dt
            } else {
                0.0
            };
            let stage = aircraft.stage();
            let target_vertical = match stage {
                Stage::Climb => aircraft.climb_fpm,
                Stage::Approach => -aircraft.descent_fpm,
                _ => 0.0,
            };
            let vertical_fpm = aircraft
                .motion
                .update_vertical(dt, target_vertical, 1_200.0);
            aircraft.animation.advance(
                animation::Input {
                    stage,
                    knots: aircraft.speed * 1.943_844,
                    turn_degrees_sec: turn_rate,
                    vertical_fpm,
                    engines_running: true,
                    fixed_gear: aircraft.profile.fixed_gear,
                    landing_lights: stage.landing_lights(),
                },
                &aircraft.handling,
                dt,
            );
        }
        self.aircraft.retain(|aircraft| {
            aircraft.distance < aircraft.cumulative.last().copied().unwrap_or(0.0)
        });
        state.airports.sync_users(
            &airport.icao,
            self.aircraft.iter().map(|aircraft| aircraft.id),
        );
        state.airports.prune(own, 60.0);

        // Speak only when the player is tuned to the aircraft's frequency.
        let mut tower_tags = std::collections::BTreeMap::new();
        for aircraft in &self.aircraft {
            let Some(frequency) = aircraft.frequency_khz else {
                continue;
            };
            let key = (aircraft.airport.clone(), frequency);
            if tower_tags.contains_key(&key) {
                continue;
            }
            let station = state
                .stations
                .iter()
                .find(|station| {
                    station.airport == aircraft.airport
                        && station.khz == frequency
                        && (station.service == "Tower"
                            || station.services.iter().any(|service| service == "Tower"))
                })
                .cloned();
            let tag = station.map_or_else(
                || openatc_core::state::SpeechTag {
                    position: "Tower".into(),
                    voice: "tower".into(),
                    ..Default::default()
                },
                |station| crate::flight::tag(state, &station),
            );
            tower_tags.insert(key, tag);
        }
        self.ground_calls(state);
        self.player_give_way(state, &traffic);
        let mut calls: Vec<(
            openatc_core::state::Radio,
            i32,
            String,
            String,
            String,
            String,
            String,
            String,
        )> = Vec::new();
        for aircraft in &mut self.aircraft {
            let Some(frequency) = aircraft.frequency_khz else {
                continue;
            };
            let Some(radio) = state.session.receiving_radio(frequency) else {
                continue;
            };
            let phase = match aircraft.kind {
                AmbientKind::Departure if aircraft.distance < aircraft.hold_distance => continue,
                AmbientKind::Departure if aircraft.takeoff => "takeoff",
                AmbientKind::Departure if aircraft.cleared && aircraft.wake_delay > 0.0 => "wake",
                AmbientKind::Departure if aircraft.cleared => "lineup",
                AmbientKind::Departure => "hold",
                AmbientKind::Arrival if aircraft.distance >= aircraft.ground_start => "landing",
                AmbientKind::Arrival => continue,
            };
            if !self.announced.insert((aircraft.id, phase)) {
                continue;
            }
            let slots = [
                ("callsign", aircraft.callsign.clone()),
                ("runway", aircraft.runway.clone()),
            ];
            let text = match phase {
                // The aircraft is already on the Tower frequency; a handoff here would be circular.
                "hold" => openatc_core::dialogue::say("ambient_hold_short", &slots),
                "lineup" => openatc_core::dialogue::say("ambient_line_up", &slots),
                "wake" => openatc_core::dialogue::say("ambient_wake", &slots),
                "takeoff" => openatc_core::dialogue::say("ambient_takeoff", &slots),
                _ => openatc_core::dialogue::say("ambient_landing", &slots),
            };
            let pilot_reply = match phase {
                "hold" => openatc_core::dialogue::say("ambient_hold_short_readback", &slots),
                "lineup" => openatc_core::dialogue::say("ambient_line_up_readback", &slots),
                "wake" => openatc_core::dialogue::say("ambient_wake_readback", &slots),
                "takeoff" => openatc_core::dialogue::say("ambient_takeoff_readback", &slots),
                _ => openatc_core::dialogue::say("ambient_landing_readback", &slots),
            };
            let key = (aircraft.airport.clone(), frequency);
            let atc_tag = tower_tags.get(&key).cloned().unwrap_or_default();
            let pilot_voice = crate::traffic::pilot_voice_for_callsign(state, &aircraft.callsign);
            calls.push((
                radio,
                frequency,
                text,
                pilot_reply,
                aircraft.callsign.clone(),
                atc_tag.voice,
                atc_tag.position,
                pilot_voice,
            ));
        }
        for (radio, frequency, text, pilot_reply, callsign, atc_voice, atc_position, pilot_voice) in
            calls
        {
            openatc_core::apply::add_transmission(
                &mut state.session,
                "ATC",
                &text,
                &openatc_core::state::SpeechTag {
                    radio: Some(radio),
                    frequency: Some(frequency),
                    position: atc_position,
                    voice: atc_voice,
                    ..Default::default()
                },
            );
            if let Some(last) = state.session.transcript.last_mut() {
                last.background = true;
                last.pilot_reply.clone_from(&pilot_reply);
            }
            openatc_core::apply::add_transmission(
                &mut state.session,
                &callsign,
                &pilot_reply,
                &openatc_core::state::SpeechTag {
                    radio: Some(radio),
                    frequency: Some(frequency),
                    position: "Traffic".into(),
                    voice: pilot_voice,
                    ..Default::default()
                },
            );
            if let Some(last) = state.session.transcript.last_mut() {
                last.background = true;
            }
        }
    }

    /// Station for a taxi instruction to an ambient aircraft. Ground owns taxiing
    /// traffic even after the aircraft has switched to Tower for its departure call.
    /// A Tower-only field falls back to the aircraft's own channel.
    fn ground_call_station(
        &self,
        state: &crate::EngineState,
        airport: &str,
        frequency: Option<i32>,
    ) -> Option<(openatc_core::stations::Station, openatc_core::state::Radio)> {
        if airport != self.active_airport {
            return None;
        }
        let ground = state.stations.iter().find(|station| {
            station.airport == airport
                && (station.service == "Ground"
                    || station.services.iter().any(|service| service == "Ground"))
        });
        if let Some(station) = ground {
            return state
                .session
                .receiving_radio(station.khz)
                .map(|radio| (station.clone(), radio));
        }
        let frequency = frequency?;
        state
            .stations
            .iter()
            .find(|station| {
                station.airport == airport
                    && station.khz == frequency
                    && (station.service == "Tower"
                        || station.services.iter().any(|service| service == "Tower"))
            })
            .and_then(|station| {
                state
                    .session
                    .receiving_radio(station.khz)
                    .map(|radio| (station.clone(), radio))
            })
    }

    /// Give-way and move-up calls on the ground. Every call is spoken only on a frequency
    /// the player is receiving, so a radio that is not tuned to the controller hears nothing.
    fn ground_calls(&mut self, state: &mut crate::EngineState) {
        let player = state.session.plan.callsign.clone();
        let callsigns: std::collections::BTreeMap<u32, String> = self
            .aircraft
            .iter()
            .map(|aircraft| (aircraft.id, aircraft.callsign.clone()))
            .collect();
        let stations: Vec<_> = self
            .aircraft
            .iter()
            .map(|aircraft| {
                self.ground_call_station(state, &aircraft.airport, aircraft.frequency_khz)
            })
            .collect();
        let mut calls: Vec<(
            openatc_core::state::Radio,
            openatc_core::stations::Station,
            String,
            String,
            String,
        )> = Vec::new();
        for (aircraft, heard) in self.aircraft.iter_mut().zip(stations) {
            match (aircraft.yield_to, aircraft.yield_announced) {
                (Some(other), false) => {
                    let Some((station, radio)) = heard else {
                        continue;
                    };
                    let other = if other == 0 {
                        player.clone()
                    } else if let Some(callsign) = callsigns.get(&other) {
                        callsign.clone()
                    } else {
                        continue;
                    };
                    aircraft.yield_announced = true;
                    let slots = [("callsign", aircraft.callsign.clone()), ("other", other)];
                    calls.push((
                        radio,
                        station,
                        openatc_core::dialogue::say("ambient_give_way", &slots),
                        openatc_core::dialogue::say("ambient_give_way_readback", &slots),
                        aircraft.callsign.clone(),
                    ));
                }
                (None, true) => {
                    aircraft.yield_announced = false;
                    let Some((station, radio)) = heard else {
                        continue;
                    };
                    let slots = [("callsign", aircraft.callsign.clone())];
                    calls.push((
                        radio,
                        station,
                        openatc_core::dialogue::say("ambient_resume", &slots),
                        openatc_core::dialogue::say("ambient_resume_readback", &slots),
                        aircraft.callsign.clone(),
                    ));
                }
                _ => {}
            }
        }
        for (radio, station, text, pilot_reply, callsign) in calls {
            let atc_tag = crate::flight::tag(state, &station);
            let pilot_voice = crate::traffic::pilot_voice_for_callsign(state, &callsign);
            openatc_core::apply::add_transmission(
                &mut state.session,
                "ATC",
                &text,
                &openatc_core::state::SpeechTag {
                    radio: Some(radio),
                    frequency: Some(station.khz),
                    ..atc_tag
                },
            );
            if let Some(last) = state.session.transcript.last_mut() {
                last.background = true;
                last.pilot_reply.clone_from(&pilot_reply);
            }
            openatc_core::apply::add_transmission(
                &mut state.session,
                &callsign,
                &pilot_reply,
                &openatc_core::state::SpeechTag {
                    radio: Some(radio),
                    frequency: Some(station.khz),
                    position: "Traffic".into(),
                    voice: pilot_voice,
                    ..Default::default()
                },
            );
            if let Some(last) = state.session.transcript.last_mut() {
                last.background = true;
            }
        }

        let now = Instant::now();
        let held = self.aircraft.iter().any(|aircraft| {
            aircraft
                .player_hold_since
                .is_some_and(|since| now.duration_since(since) >= MOVE_UP_AFTER)
        });
        let stopped = state.session.telemetry.ground_speed_knots < 1.0;
        let due = self
            .move_request_at
            .is_none_or(|last| now.duration_since(last) >= MOVE_UP_REPEAT);
        if !(held && stopped && due) {
            return;
        }
        let Some((station, radio)) = self.tuned_ground_station(state) else {
            return;
        };
        let text = openatc_core::dialogue::say("ground_traffic_waiting", &[("callsign", player)]);
        let tag = crate::flight::tag(state, &station);
        openatc_core::apply::add_transmission(
            &mut state.session,
            "ATC",
            &text,
            &openatc_core::state::SpeechTag {
                radio: Some(radio),
                frequency: Some(station.khz),
                ..tag
            },
        );
        self.move_request_at = Some(now);
    }

    /// The ground station of the active airport that the player is receiving, with its radio.
    fn tuned_ground_station(
        &self,
        state: &crate::EngineState,
    ) -> Option<(openatc_core::stations::Station, openatc_core::state::Radio)> {
        state
            .stations
            .iter()
            .filter(|station| {
                station.airport == self.active_airport
                    && (station.service == "Ground"
                        || station.services.iter().any(|service| service == "Ground"))
            })
            .find_map(|station| {
                state
                    .session
                    .receiving_radio(station.khz)
                    .map(|radio| (station.clone(), radio))
            })
    }

    /// The player gives way to moving traffic on its right at a junction. ATC says so once per
    /// conflict on a ground frequency the player is receiving, and says continue when it clears.
    fn player_give_way(&mut self, state: &mut crate::EngineState, traffic: &[TrafficTarget]) {
        let player = traffic.iter().find(|target| {
            target.id == 0 && target.on_ground && target.ground_speed_knots.unwrap_or(0.0) > 1.0
        });
        let mut nearest: Option<(f64, u32)> = None;
        if let Some(player) = player {
            for aircraft in &self.aircraft {
                if aircraft.on_runway()
                    || aircraft.airport != self.active_airport
                    || aircraft.speed <= 0.5
                {
                    continue;
                }
                let (latitude, longitude, heading) =
                    position_heading_at(&aircraft.points, &aircraft.cumulative, aircraft.distance);
                if !separation::right_conflict(
                    player.latitude,
                    player.longitude,
                    player.track_degrees,
                    latitude,
                    longitude,
                    heading,
                    aircraft.id,
                    None,
                    false,
                    100.0,
                ) {
                    continue;
                }
                let gap = openatc_core::arrival::distance_nm(
                    [player.latitude, player.longitude],
                    [latitude, longitude],
                ) * 1852.0;
                if nearest.is_none_or(|(best, _)| gap < best) {
                    nearest = Some((gap, aircraft.id));
                }
            }
        }
        let target = nearest.map(|(_, id)| id);
        if target == self.player_yield {
            return;
        }
        let Some((station, radio)) = self.tuned_ground_station(state) else {
            if target.is_none() {
                self.player_yield = None;
            }
            return;
        };
        let callsign = state.session.plan.callsign.clone();
        let text = match target {
            Some(id) => {
                let other = self
                    .aircraft
                    .iter()
                    .find(|aircraft| aircraft.id == id)
                    .map_or_else(
                        || "traffic".to_owned(),
                        |aircraft| aircraft.callsign.clone(),
                    );
                openatc_core::dialogue::say(
                    "player_give_way",
                    &[("callsign", callsign), ("other", other)],
                )
            }
            None => openatc_core::dialogue::say("player_give_way_clear", &[("callsign", callsign)]),
        };
        let tag = crate::flight::tag(state, &station);
        openatc_core::apply::add_transmission(
            &mut state.session,
            "ATC",
            &text,
            &openatc_core::state::SpeechTag {
                radio: Some(radio),
                frequency: Some(station.khz),
                ..tag
            },
        );
        self.player_yield = target;
    }

    /// Republish the ambient frames even when traffic advancement stopped early.
    fn publish(&mut self, state: &mut crate::EngineState) {
        let frames: Vec<GeneratedTraffic> =
            self.aircraft.iter().filter_map(ambient_frame).collect();
        let debug: Vec<TrafficDebug> = self
            .aircraft
            .iter()
            .map(|aircraft| TrafficDebug {
                id: aircraft.id,
                callsign: aircraft.callsign.clone(),
                stage: aircraft.phase().label().to_owned(),
                reason: if aircraft.blocked {
                    "holding: traffic or runway".to_owned()
                } else {
                    match aircraft.kind {
                        AmbientKind::Departure => "ambient departure".to_owned(),
                        AmbientKind::Arrival => "ambient arrival".to_owned(),
                    }
                },
                blocker: aircraft.blocker,
                awaiting_clearance: aircraft.awaiting_clearance,
                hold: aircraft.hold,
            })
            .collect();
        // Replace the ambient portion only; keep frames produced by other layers.
        let ambient_ids: BTreeSet<u32> = self
            .published
            .iter()
            .copied()
            .chain(self.aircraft.iter().map(|aircraft| aircraft.id))
            .collect();
        state
            .session
            .generated_traffic
            .retain(|frame| !ambient_ids.contains(&frame.target.id));
        state.session.generated_traffic.extend(frames);
        if let Some(time) = state
            .session
            .telemetry
            .simulation_seconds
            .filter(|time| time.is_finite())
        {
            state.session.generated_traffic_clock = Some((state.session_generation, time));
        }
        state
            .session
            .generated_traffic_debug
            .retain(|debug| !ambient_ids.contains(&debug.id));
        state.session.generated_traffic_debug.extend(debug);
        if state.session.generated_traffic_debug.len() > 126 {
            state.session.generated_traffic_debug.truncate(126);
        }
        state
            .session
            .generated_traffic
            .sort_by_key(|frame| frame.target.id);
        self.published = self.aircraft.iter().map(|aircraft| aircraft.id).collect();
    }

    /// Read-only route diagnostics for the ambient layer.
    #[must_use]
    pub fn debug(&self, airport: &Airport) -> serde_json::Value {
        let aircraft: Vec<serde_json::Value> = self
            .aircraft
            .iter()
            .map(|aircraft| {
                let points = aircraft
                    .points
                    .iter()
                    .map(|point| {
                        let local = openatc_core::airport::airport_point(
                            airport,
                            point.latitude,
                            point.longitude,
                        );
                        openatc_core::airport::on_taxiway(airport, &local, 5.0)
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({
                    "id": aircraft.id,
                    "callsign": aircraft.callsign,
                    "kind": match aircraft.kind {
                        AmbientKind::Departure => "departure",
                        AmbientKind::Arrival => "arrival",
                    },
                    "distance": aircraft.distance,
                    "stage": format!("{:?}", aircraft.stage()),
                    "wakeDelay": aircraft.wake_delay,
                    "runway": aircraft.runway,
                    "taxiEnd": aircraft.taxi_end,
                    "runwayStart": aircraft.runway_start,
                    "runwayEnd": aircraft.runway_end,
                    "holdDistance": aircraft.hold_distance,
                    "hold": aircraft.hold,
                    "points": aircraft.points.len(),
                    "onTaxiway": points.iter().filter(|on| **on).count(),
                    "first": aircraft.points.first(),
                    "last": aircraft.points.last(),
                })
            })
            .collect();
        serde_json::json!({
            "aircraft": aircraft,
            "models": self.models.is_some(),
            "attempts": self.attempts,
            "rejectedRoute": self.rejected_route,
            "rejectedPavement": self.rejected_pavement,
            "rejectedModel": self.rejected_model,
            "lastError": self.last_error,
        })
    }

    fn spawn(
        &mut self,
        airport: &Airport,
        models: &Catalogue,
        state: &crate::EngineState,
        target: usize,
    ) {
        let (wind_from, wind_knots) = state
            .sim_weather
            .get(&airport.icao)
            .map(|(sample, _)| {
                (
                    sample["windDegrees"].as_f64().unwrap_or(0.0),
                    sample["windKnots"].as_f64().unwrap_or(0.0),
                )
            })
            .unwrap_or((0.0, 0.0));
        let fallback = airport
            .runways
            .first()
            .map(|runway| runway.first_name.clone())
            .unwrap_or_default();
        let assignment = state.airports.assignment(
            &airport.icao,
            &state.session.plan.runway,
            &state.session.plan.arrival_runway,
            wind_from,
            wind_knots,
            1_200.0,
        );
        let departure_runway = assignment
            .as_ref()
            .map(|assignment| assignment.departure.clone())
            .filter(|runway| !runway.is_empty())
            .unwrap_or_else(|| fallback.clone());
        let arrival_runway = assignment
            .as_ref()
            .map(|assignment| assignment.arrival.clone())
            .filter(|runway| !runway.is_empty())
            .unwrap_or_else(|| departure_runway.clone());
        if departure_runway.is_empty() && arrival_runway.is_empty() {
            return;
        }
        let frequency = state
            .stations
            .iter()
            .find(|station| {
                station.airport == airport.icao
                    && (station.service == "Tower"
                        || station.services.iter().any(|service| service == "Tower"))
            })
            .map(|station| station.khz);
        if models.models.is_empty() {
            return;
        }
        let Some(profiles) = state.traffic_profiles.current() else {
            return;
        };
        let player = state.session.plan.callsign.to_uppercase();
        let mut used_callsigns: BTreeSet<String> = self
            .aircraft
            .iter()
            .map(|aircraft| aircraft.callsign.to_uppercase())
            .collect();
        used_callsigns.insert(player.clone());
        used_callsigns.extend(
            state
                .session
                .generated_traffic
                .iter()
                .map(|frame| frame.target.callsign.to_uppercase()),
        );
        used_callsigns.extend(
            state
                .session
                .telemetry
                .traffic
                .iter()
                .flatten()
                .map(|target| target.callsign.to_uppercase())
                .filter(|callsign| !callsign.is_empty()),
        );
        let mix_total = u64::from(state.settings.airline_traffic)
            + u64::from(state.settings.regional_traffic)
            + u64::from(state.settings.cargo_traffic)
            + u64::from(state.settings.ga_traffic);
        let ga_percent = if mix_total == 0 {
            0
        } else {
            u64::from(state.settings.ga_traffic) * 100 / mix_total
        };
        let mut used_nodes: BTreeSet<i64> = self
            .aircraft
            .iter()
            .map(|aircraft| aircraft.spawn_node)
            .collect();
        let occupied: Vec<Point> = {
            let mut points: Vec<Point> = self
                .aircraft
                .iter()
                .map(|aircraft| {
                    let (latitude, longitude) =
                        position_at(&aircraft.points, &aircraft.cumulative, aircraft.distance);
                    openatc_core::airport::airport_point(airport, latitude, longitude)
                })
                .collect();
            if state.session.telemetry.position_valid {
                points.push(openatc_core::airport::airport_point(
                    airport,
                    state.session.telemetry.latitude,
                    state.session.telemetry.longitude,
                ));
            }
            for target in state.traffic_targets() {
                if target.id == 0
                    || !target.on_ground
                    || !target.latitude.is_finite()
                    || !target.longitude.is_finite()
                {
                    continue;
                }
                points.push(openatc_core::airport::airport_point(
                    airport,
                    target.latitude,
                    target.longitude,
                ));
            }
            points
        };
        let ground_targets: Vec<TrafficTarget> = state
            .traffic_targets()
            .into_iter()
            .filter(|target| {
                target.id != 0
                    && target.on_ground
                    && target.latitude.is_finite()
                    && target.longitude.is_finite()
            })
            .collect();
        let player_valid = state.session.telemetry.position_valid;
        let arrival_slot = target >= 3
            && !self
                .aircraft
                .iter()
                .any(|aircraft| aircraft.kind == AmbientKind::Arrival);
        let departures_requested = if arrival_slot { target - 1 } else { target };
        for slot in 0..target {
            self.attempts = self.attempts.saturating_add(1);
            let kind = if slot == target - 1 && arrival_slot {
                AmbientKind::Arrival
            } else {
                AmbientKind::Departure
            };
            let route_runway = if kind == AmbientKind::Departure {
                &departure_runway
            } else {
                &arrival_runway
            };
            let runway_meters = airport_runway_length(airport, route_runway);
            if kind == AmbientKind::Arrival
                && state.session.telemetry.position_valid
                && openatc_core::traffic::approach_position(
                    airport,
                    route_runway,
                    state.session.telemetry.latitude,
                    state.session.telemetry.longitude,
                )
                .is_some_and(|position| {
                    position.0 < 0.0 && -position.0 < 8.0 * 1852.0 && position.1.abs() < 500.0
                })
            {
                self.last_error = "arrival slot waits for the player on final".to_owned();
                continue;
            }
            let ga = mix(state.traffic_seed ^ u64::from(self.next_id)) % 100 < ga_percent;
            let identity = (0..8).find_map(|attempt| {
                ambient_identity(
                    &profiles,
                    models,
                    &airport.icao,
                    runway_meters,
                    ga,
                    state
                        .traffic_seed
                        .wrapping_add(u64::from(self.next_id))
                        .wrapping_add(attempt * 7919),
                    &used_callsigns,
                    &player,
                    u64::from(state.settings.airline_traffic),
                    u64::from(state.settings.regional_traffic),
                    u64::from(state.settings.cargo_traffic),
                )
            });
            let Some(identity) = identity else {
                self.rejected_model = self.rejected_model.saturating_add(1);
                self.last_error = "no fitted airline or GA model".to_owned();
                continue;
            };
            let size = identity.profile.parking_size();
            // Start on the taxiway network. Arrivals start from a stand-side node so the
            // reversed route exits the runway toward the parking area.
            let Some(node) = spawn_node(
                airport,
                size,
                &used_nodes,
                &self.rejected_spawn_nodes,
                &occupied,
                kind == AmbientKind::Arrival,
                state.traffic_seed ^ u64::from(self.next_id),
            ) else {
                break;
            };
            let handling_kind = identity.profile.handling.unwrap_or(Kind::Jet);
            let Some(handling) = state
                .traffic_handling
                .get(&handling_kind)
                .cloned()
                .map(Arc::new)
            else {
                self.last_error = format!("no {handling_kind:?} animation handling");
                continue;
            };
            let latitude = airport.reference_latitude + node.point.north / 111_320.0;
            let longitude = airport.reference_longitude
                + node.point.east
                    / (111_320.0 * (airport.reference_latitude.to_radians().cos()).max(0.01));
            let minimum_rollout = phases::landing::minimum_rollout(&identity.profile, &handling);
            let targets = state.traffic_targets();
            let arrival_exit = (kind == AmbientKind::Arrival)
                .then(|| {
                    choose_arrival_exit(
                        airport,
                        route_runway,
                        size,
                        &targets,
                        0.0,
                        25.0,
                        minimum_rollout,
                        u32::MAX,
                    )
                })
                .flatten();
            if kind == AmbientKind::Arrival && arrival_exit.is_none() {
                self.rejected_route = self.rejected_route.saturating_add(1);
                self.last_error = "no clear arrival exit".to_owned();
                continue;
            }
            let Some((surface_points, route)) = plan_taxi(
                airport,
                node.id,
                route_runway,
                size,
                (kind == AmbientKind::Arrival).then_some(minimum_rollout),
                arrival_exit.as_ref().map(|plan| plan.chosen.node),
            ) else {
                self.rejected_route = self.rejected_route.saturating_add(1);
                self.last_error = format!("no taxi graph route from node {}", node.id);
                self.rejected_spawn_nodes.insert((size, node.id));
                continue;
            };
            let surface_points =
                phases::ground::round_taxi_path(&airport, &surface_points, &handling);
            let entry_points = (kind == AmbientKind::Departure)
                .then(|| {
                    runway_entry_points(
                        &airport,
                        surface_points.last().copied(),
                        &departure_runway,
                        size,
                    )
                })
                .flatten();
            let stand_waypoint = Waypoint {
                latitude,
                longitude,
                altitude_feet: airport.elevation_feet,
            };
            let climb_speed = phases::sid::climb_speed(&identity.profile, &handling);
            let minimum_climb_speed =
                phases::sid::minimum_climb_speed(&identity.profile, &handling);
            let climb_fpm = identity.profile.climb_fpm;
            let path = match kind {
                AmbientKind::Departure => departure_path(
                    airport,
                    &surface_points,
                    entry_points.as_deref(),
                    &departure_runway,
                    stand_waypoint,
                    &handling,
                    climb_speed,
                    climb_fpm,
                ),
                AmbientKind::Arrival => arrival_path(
                    airport,
                    &surface_points,
                    &arrival_runway,
                    stand_waypoint,
                    minimum_rollout,
                ),
            };
            let Some((points, ground_start, ground_end, taxi_end, runway_start, runway_end)) = path
            else {
                self.rejected_route = self.rejected_route.saturating_add(1);
                self.last_error = format!("path build failed for node {}", node.id);
                self.rejected_spawn_nodes.insert((size, node.id));
                continue;
            };
            let cumulative = cumulative_distances(&points);
            let total = cumulative.last().copied().unwrap_or(0.0);
            if total < 100.0 {
                continue;
            }
            let hold_marker = if kind == AmbientKind::Departure {
                openatc_core::airport::holding_marker_position(airport, &surface_points).map(
                    |point| {
                        [
                            airport.reference_latitude + point.north / 111_320.0,
                            airport.reference_longitude
                                + point.east
                                    / (111_320.0
                                        * (airport.reference_latitude.to_radians().cos())
                                            .max(0.01)),
                        ]
                    },
                )
            } else {
                None
            };
            let hold_distance = if kind == AmbientKind::Departure {
                hold_marker.map_or(taxi_end - 15.0, |[latitude, longitude]| {
                    let target = Waypoint {
                        latitude,
                        longitude,
                        altitude_feet: airport.elevation_feet,
                    };
                    let projected = route_distance_to(&points, &cumulative, target, taxi_end)
                        .clamp(0.0, taxi_end);
                    hold_clear_of_runway(airport, &points, &cumulative, projected)
                })
            } else {
                taxi_end
            };
            let hold = hold_marker.map(|_| {
                let (latitude, longitude) = position_at(&points, &cumulative, hold_distance);
                [latitude, longitude]
            });
            let distance = match kind {
                AmbientKind::Arrival => 0.0,
                AmbientKind::Departure => {
                    let positions = seed_distances(hold_distance, departures_requested);
                    let Some(raw) = positions.get(slot).copied() else {
                        self.rejected_route = self.rejected_route.saturating_add(1);
                        self.last_error = "no safe taxiway length for another departure".to_owned();
                        continue;
                    };
                    let distance = hold_clear_of_runway(airport, &points, &cumulative, raw);
                    if distance <= 0.0 {
                        self.rejected_route = self.rejected_route.saturating_add(1);
                        self.last_error = "seeded departure would start on the runway".to_owned();
                        continue;
                    }
                    distance
                }
            };
            let candidate = position_at(&points, &cumulative, distance);
            let local_candidate =
                openatc_core::airport::airport_point(airport, candidate.0, candidate.1);
            if openatc_core::airport::within_runway(airport, &local_candidate)
                || openatc_core::airport::runway_edge_distance(airport, &local_candidate)
                    .is_some_and(|distance| distance <= RUNWAY_EDGE_MARGIN)
            {
                self.rejected_route = self.rejected_route.saturating_add(1);
                self.last_error = "seed point too close to a runway edge".to_owned();
                continue;
            }
            if ground_targets.iter().any(|other| {
                openatc_core::arrival::distance_nm(
                    [candidate.0, candidate.1],
                    [other.latitude, other.longitude],
                ) * 1852.0
                    < 80.0
            }) || self.aircraft.iter().any(|other| {
                openatc_core::arrival::distance_nm(
                    [candidate.0, candidate.1],
                    [
                        position_at(&other.points, &other.cumulative, other.distance).0,
                        position_at(&other.points, &other.cumulative, other.distance).1,
                    ],
                ) * 1852.0
                    < 80.0
            }) {
                self.rejected_route = self.rejected_route.saturating_add(1);
                self.last_error = "spawn point too close to another aircraft".to_owned();
                continue;
            }
            if player_valid
                && openatc_core::arrival::distance_nm(
                    [candidate.0, candidate.1],
                    [
                        state.session.telemetry.latitude,
                        state.session.telemetry.longitude,
                    ],
                ) * 1852.0
                    < 100.0
            {
                self.rejected_route = self.rejected_route.saturating_add(1);
                self.last_error = "spawn point too close to the player".to_owned();
                continue;
            }
            let taxi_knots = handling.taxi_knots.max(1.0) * 0.514_444;
            let taxi_turn_knots = handling.taxi_turn_knots.max(1.0) * 0.514_444;
            let approach_speed = phases::approach::speed(&identity.profile);
            let touchdown_speed = phases::landing::touchdown_speed(&identity.profile);
            let descent_fpm = identity.profile.descent_fpm;
            let initial_speed = if kind == AmbientKind::Arrival {
                approach_speed
            } else {
                phases::ground::taxi_speed(
                    &points,
                    &cumulative,
                    distance,
                    taxi_knots,
                    taxi_turn_knots,
                    handling.braking_knots_sec.max(0.1) * KNOTS_TO_MPS,
                )
            };
            let callsign = identity.callsign.clone();
            self.aircraft.push(AmbientAircraft {
                id: 0x00e0_0000 + self.next_id,
                callsign: callsign.clone(),
                aircraft: identity.aircraft,
                profile: identity.profile,
                wake_category: identity.wake_category,
                airport: airport.icao.clone(),
                runway: route_runway.clone(),
                route,
                spawn_node: node.id,
                model: identity.model,
                kind,
                points,
                cumulative,
                ground_start,
                ground_end,
                taxi_end,
                runway_start,
                runway_end,
                hold_distance,
                hold,
                animation: {
                    let mut animation = Animation::default();
                    animation.engine = 1.0;
                    animation.propeller_rpm = handling.propeller_rpm;
                    animation.beacon = true;
                    animation.navigation_lights = true;
                    animation
                },
                handling,
                taxi_knots,
                taxi_turn_knots,
                climb_speed,
                minimum_climb_speed,
                climb_fpm,
                descent_fpm,
                approach_speed,
                touchdown_speed,
                distance,
                motion: crate::traffic::motion::MotionController::new(initial_speed, 0.0),
                cleared: false,
                takeoff: false,
                hold_since: None,
                requested: false,
                speed: initial_speed,
                blocked: false,
                blocker: None,
                awaiting_clearance: false,
                yield_to: None,
                yield_announced: false,
                player_hold_since: None,
                wake_delay: 0.0,
                last: Instant::now(),
                frequency_khz: frequency,
            });
            used_callsigns.insert(callsign.to_uppercase());
            used_nodes.insert(node.id);
            self.next_id = self.next_id.wrapping_add(1);
        }
    }
}

impl AmbientAircraft {
    fn phase(&self) -> TrafficPhase {
        TrafficPhase::from_stage(self.stage())
    }

    fn stage(&self) -> Stage {
        match self.kind {
            AmbientKind::Departure => {
                if self.distance >= self.runway_end {
                    Stage::Climb
                } else if self.takeoff && self.distance >= self.runway_start {
                    Stage::Takeoff
                } else if self.cleared && self.distance >= self.hold_distance {
                    Stage::LineUp
                } else {
                    Stage::TaxiOut
                }
            }
            AmbientKind::Arrival => {
                if self.distance < self.ground_start {
                    Stage::Approach
                } else if self.distance < self.runway_end {
                    Stage::Rollout
                } else {
                    Stage::TaxiIn
                }
            }
        }
    }

    fn on_runway(&self) -> bool {
        match self.kind {
            AmbientKind::Departure => {
                self.distance >= self.runway_start && self.distance <= self.runway_end + 600.0
            }
            AmbientKind::Arrival => {
                self.distance >= self.ground_start - 500.0
                    && self.distance <= self.runway_end + 60.0
            }
        }
    }

    fn approaching_runway(&self, seconds: f64) -> bool {
        let next = self.distance + self.speed.max(1.0) * seconds;
        match self.kind {
            AmbientKind::Departure => {
                self.distance < self.runway_start && next >= self.runway_start
            }
            AmbientKind::Arrival => {
                self.distance < self.ground_start - 500.0 && next >= self.ground_start - 500.0
            }
        }
    }
}

/// Safe departure queue positions along the surface route, spaced from the hold.
///
/// Returns fewer positions when the taxiway is too short for the requested fleet;
/// stacking aircraft at the hold is worse than seeding fewer.
fn seed_distances(hold_distance: f64, requested: usize) -> Vec<f64> {
    let usable_end = (hold_distance - HOLD_QUEUE_MARGIN).max(0.0);
    if usable_end < 60.0 || requested == 0 {
        return Vec::new();
    }
    let count = ((usable_end / SEED_SPACING).floor() as usize)
        .max(1)
        .min(requested);
    // Spread the queue across the whole usable route, not just the first few
    // hundred metres near the spawn node. On a long taxiway the first aircraft
    // should already be well clear of the apron.
    let spacing = (usable_end / (count as f64 + 1.0)).max(SEED_SPACING);
    (0..count)
        .map(|slot| (spacing * (slot as f64 + 1.0)).min(usable_end))
        .collect()
}

fn spawn_node<'a>(
    airport: &'a Airport,
    size: char,
    used: &std::collections::BTreeSet<i64>,
    rejected: &BTreeSet<(char, i64)>,
    blocked: &[Point],
    arrival: bool,
    seed: u64,
) -> Option<&'a openatc_core::airport::TaxiNode> {
    let stand = arrival
        .then(|| {
            airport
                .parking
                .iter()
                .find(|stand| {
                    !stand.name.is_empty()
                        && stand.size.chars().next().is_some_and(|class| class >= size)
                })
                .map(|stand| stand.point)
        })
        .flatten();
    airport
        .nodes
        .iter()
        .filter(|node| {
            !used.contains(&node.id)
                && !rejected.contains(&(size, node.id))
                && !openatc_core::airport::within_runway(airport, &node.point)
                && openatc_core::airport::on_non_runway_taxiway(airport, &node.point, 8.0)
                && openatc_core::airport::taxi_point_on_pavement(airport, node.point)
                && openatc_core::airport::runway_edge_distance(airport, &node.point)
                    .is_none_or(|distance| distance > RUNWAY_EDGE_MARGIN)
                && blocked.iter().all(|point| {
                    (node.point.east - point.east).hypot(node.point.north - point.north) > 100.0
                })
        })
        .min_by(|left, right| {
            let key = |node: &openatc_core::airport::TaxiNode| {
                stand.map_or(mix(node.id as u64 ^ seed) as f64, |stand| {
                    (node.point.east - stand.east).hypot(node.point.north - stand.north)
                })
            };
            key(left).total_cmp(&key(right))
        })
}

fn plan_taxi(
    airport: &Airport,
    start: i64,
    runway: &str,
    size: char,
    arrival: Option<f64>,
    exit_node: Option<i64>,
) -> Option<(Vec<Point>, String)> {
    use std::cmp::Reverse;
    use std::collections::{BTreeMap, BinaryHeap};
    let runway_data = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)?;
    let threshold = if runway_data.first_name == runway {
        runway_data.first
    } else {
        runway_data.second
    };
    let nodes: BTreeMap<i64, Point> = airport
        .nodes
        .iter()
        .map(|node| (node.id, node.point))
        .collect();
    let start_point = nodes.get(&start).copied().unwrap_or(threshold);
    if arrival.is_none() {
        let cos = (airport.reference_latitude.to_radians().cos()).max(0.01);
        let telemetry = openatc_core::state::Telemetry {
            latitude: airport.reference_latitude + start_point.north / 111_320.0,
            longitude: airport.reference_longitude + start_point.east / (111_320.0 * cos),
            position_valid: true,
            on_ground: true,
            ..Default::default()
        };
        let route =
            openatc_core::airport::calculate_taxi_route(airport, &telemetry, runway, false, size)
                .ok()?;
        return Some((route.points, route.via));
    }
    let target = if let Some(exit) = exit_node {
        exit
    } else {
        airport
            .nodes
            .iter()
            .filter(|node| {
                if openatc_core::airport::within_runway(airport, &node.point)
                    || !openatc_core::airport::on_non_runway_taxiway(airport, &node.point, 12.0)
                    || (arrival.is_none()
                        && openatc_core::airport::runway_edge_distance(airport, &node.point)
                            .is_some_and(|distance| distance <= RUNWAY_EDGE_MARGIN))
                {
                    return false;
                }
                let latitude = airport.reference_latitude + node.point.north / 111_320.0;
                let longitude = airport.reference_longitude
                    + node.point.east
                        / (111_320.0 * (airport.reference_latitude.to_radians().cos()).max(0.01));
                let Some((along, across, length, _)) =
                    openatc_core::traffic::runway_position(airport, runway, latitude, longitude)
                else {
                    return true;
                };
                if let Some(minimum_rollout) = arrival {
                    along >= minimum_rollout
                        && along <= length - 100.0
                        && across.abs() > runway_data.width / 2.0 + 35.0
                        && across.abs() < 250.0
                } else {
                    across.abs() > 50.0
                }
            })
            .min_by(|left, right| {
                let key = |node: &openatc_core::airport::TaxiNode| {
                    let latitude = airport.reference_latitude + node.point.north / 111_320.0;
                    let longitude = airport.reference_longitude
                        + node.point.east
                            / (111_320.0
                                * (airport.reference_latitude.to_radians().cos()).max(0.01));
                    let along = openatc_core::traffic::runway_position(
                        airport, runway, latitude, longitude,
                    )
                    .map_or(f64::INFINITY, |(along, _, _, _)| along);
                    let stand = (node.point.east - start_point.east)
                        .hypot(node.point.north - start_point.north);
                    (along, stand)
                };
                key(left)
                    .partial_cmp(&key(right))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })?
            .id
    };
    let mut adjacency: BTreeMap<i64, Vec<(i64, f64, String)>> = BTreeMap::new();
    for edge in &airport.edges {
        if edge.runway || edge.size < size {
            continue;
        }
        let (Some(first), Some(second)) = (nodes.get(&edge.first), nodes.get(&edge.second)) else {
            continue;
        };
        if !openatc_core::airport::taxi_segment_on_pavement(airport, *first, *second) {
            continue;
        }
        let cost = (second.east - first.east).hypot(second.north - first.north);
        adjacency
            .entry(edge.first)
            .or_default()
            .push((edge.second, cost, edge.name.clone()));
        if !edge.one_way {
            adjacency
                .entry(edge.second)
                .or_default()
                .push((edge.first, cost, edge.name.clone()));
        }
    }
    let mut costs = BTreeMap::from([(start, 0.0)]);
    let mut previous = BTreeMap::new();
    let mut previous_edge = BTreeMap::new();
    let mut heap = BinaryHeap::from([(Reverse(0u64), start)]);
    while let Some((Reverse(bits), node)) = heap.pop() {
        let cost = f64::from_bits(bits);
        if node == target {
            break;
        }
        if cost > costs.get(&node).copied().unwrap_or(f64::INFINITY) {
            continue;
        }
        for (next, step, name) in adjacency.get(&node).into_iter().flatten() {
            let next_cost = cost + step;
            if next_cost < costs.get(next).copied().unwrap_or(f64::INFINITY) {
                costs.insert(*next, next_cost);
                previous.insert(*next, node);
                previous_edge.insert(*next, name.clone());
                heap.push((Reverse(next_cost.to_bits()), *next));
            }
        }
    }
    if !costs.contains_key(&target) {
        return None;
    }
    let mut path = vec![target];
    let mut node = target;
    let mut via = Vec::new();
    while let Some(previous_node) = previous.get(&node) {
        if let Some(name) = previous_edge.get(&node) {
            via.push(name.clone());
        }
        path.push(*previous_node);
        node = *previous_node;
    }
    path.reverse();
    via.reverse();
    if path.len() < 2 || path.first() != Some(&start) {
        return None;
    }
    let points = path
        .into_iter()
        .filter_map(|id| nodes.get(&id).copied())
        .collect();
    Some((points, via.join(", ")))
}

fn surface_waypoints(airport: &Airport, points: &[Point]) -> Vec<Waypoint> {
    points
        .iter()
        .map(|point| Waypoint {
            latitude: airport.reference_latitude + point.north / 111_320.0,
            longitude: airport.reference_longitude
                + point.east
                    / (111_320.0 * (airport.reference_latitude.to_radians().cos()).max(0.01)),
            altitude_feet: airport.elevation_feet,
        })
        .collect()
}

fn runway_ends(
    airport: &Airport,
    runway: &str,
    near: Option<Waypoint>,
) -> Option<(openatc_core::airport::Runway, Waypoint, Waypoint)> {
    let runway_data = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)?
        .clone();
    let (first, second) = if runway_data.first_name == runway {
        (runway_data.first, runway_data.second)
    } else {
        (runway_data.second, runway_data.first)
    };
    let (threshold_point, far_point) = if let Some(near) = near {
        let cos = (airport.reference_latitude.to_radians().cos()).max(0.01);
        let here = Point {
            east: (near.longitude - airport.reference_longitude) * 111_320.0 * cos,
            north: (near.latitude - airport.reference_latitude) * 111_320.0,
            height: airport.elevation_feet,
        };
        let distance = |point: Point| (point.east - here.east).hypot(point.north - here.north);
        // A takeoff run has to start from an end the aircraft can reach without
        // backtracking down the runway, so prefer the end behind the route.
        let behind = |threshold: Point, far: Point| {
            (here.east - threshold.east) * (far.east - threshold.east)
                + (here.north - threshold.north) * (far.north - threshold.north)
                < 0.0
        };
        match (behind(first, second), behind(second, first)) {
            (true, false) => (first, second),
            (false, true) => (second, first),
            _ if distance(first) <= distance(second) => (first, second),
            _ => (second, first),
        }
    } else {
        (first, second)
    };
    let waypoint = |point: Point, altitude: f64| Waypoint {
        latitude: airport.reference_latitude + point.north / 111_320.0,
        longitude: airport.reference_longitude
            + point.east / (111_320.0 * (airport.reference_latitude.to_radians().cos()).max(0.01)),
        altitude_feet: altitude,
    };
    Some((
        runway_data,
        waypoint(threshold_point, airport.elevation_feet),
        waypoint(far_point, airport.elevation_feet + 2_000.0),
    ))
}

fn entry_waypoint(airport: &Airport, runway: &str, point: Point) -> Option<Waypoint> {
    let runway_data = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)?;
    let (start, end) = if runway_data.first_name == runway {
        (runway_data.first, runway_data.second)
    } else {
        (runway_data.second, runway_data.first)
    };
    let dx = end.east - start.east;
    let dn = end.north - start.north;
    let length = dx.hypot(dn);
    if length < 1.0 {
        return None;
    }
    let along = (((point.east - start.east) * dx + (point.north - start.north) * dn) / length)
        .clamp(0.0, length);
    let entry = Point {
        east: start.east + dx / length * along,
        north: start.north + dn / length * along,
        height: airport.elevation_feet,
    };
    Some(Waypoint {
        latitude: airport.reference_latitude + entry.north / 111_320.0,
        longitude: airport.reference_longitude
            + entry.east / (111_320.0 * (airport.reference_latitude.to_radians().cos()).max(0.01)),
        altitude_feet: airport.elevation_feet,
    })
}

/// One selected arrival exit and the next fallback.
#[derive(Clone, Debug)]
pub(crate) struct ArrivalExitPlan {
    pub chosen: openatc_core::airport::ArrivalExit,
    pub fallback: Option<openatc_core::airport::ArrivalExit>,
}

/// Shared arrival exit rule for the player and ambient traffic.
///
/// The departure entry is never used, an exit above the aircraft's safe turn-off
/// speed is skipped, and a stopped queue inside the exit throat moves the plan to
/// the next exit. A moving queue is assumed to clear.
#[must_use]
pub(crate) fn choose_arrival_exit(
    airport: &Airport,
    runway: &str,
    size: char,
    targets: &[openatc_core::state::TrafficTarget],
    along: f64,
    speed_knots: f64,
    minimum_rollout: f64,
    self_id: u32,
) -> Option<ArrivalExitPlan> {
    let mut viable = openatc_core::airport::arrival_exits(airport, runway, size)
        .into_iter()
        .filter(|exit| {
            if exit.entry || exit.along < minimum_rollout.max(along + 30.0) {
                return false;
            }
            // A distant exit leaves room to slow; an immediate exit does not.
            let distance = (exit.along - along).max(0.0);
            distance >= 300.0 || speed_knots <= exit.maximum_knots + 5.0
        })
        .filter(|exit| !exit_blocked(airport, exit, targets, self_id));
    let chosen = viable.next()?;
    Some(ArrivalExitPlan {
        chosen,
        fallback: viable.next(),
    })
}

/// A stopped aircraft inside the turn-off throat blocks the exit.
fn exit_blocked(
    airport: &Airport,
    exit: &openatc_core::airport::ArrivalExit,
    targets: &[openatc_core::state::TrafficTarget],
    self_id: u32,
) -> bool {
    targets.iter().any(|target| {
        target.id != self_id
            && target.on_ground
            && target.latitude.is_finite()
            && target.longitude.is_finite()
            && {
                let point = openatc_core::airport::airport_point(
                    airport,
                    target.latitude,
                    target.longitude,
                );
                let gap = (point.east - exit.point.east).hypot(point.north - exit.point.north);
                gap < 150.0 && target.ground_speed_knots.unwrap_or(0.0) < 5.0
            }
    })
}

/// Project an arrival exit onto the runway and require a minimum rollout.
fn arrival_exit_waypoint(
    airport: &Airport,
    runway: &str,
    point: Point,
    minimum_rollout: f64,
) -> Option<Waypoint> {
    let waypoint = entry_waypoint(airport, runway, point)?;
    let (along, _, length, _) = openatc_core::traffic::runway_position(
        airport,
        runway,
        waypoint.latitude,
        waypoint.longitude,
    )?;
    (along >= minimum_rollout && along <= length - 50.0).then_some(waypoint)
}

fn runway_entry_points(
    airport: &Airport,
    hold: Option<Point>,
    runway: &str,
    size: char,
) -> Option<Vec<Point>> {
    let hold = hold?;
    let latitude = airport.reference_latitude + hold.north / 111_320.0;
    let longitude = airport.reference_longitude
        + hold.east / (111_320.0 * (airport.reference_latitude.to_radians().cos()).max(0.01));
    let telemetry = openatc_core::state::Telemetry {
        latitude,
        longitude,
        position_valid: true,
        on_ground: true,
        ..Default::default()
    };
    let mut points = openatc_core::airport::calculate_runway_taxi_route_with(
        airport, &telemetry, runway, size, false,
    )
    .ok()?
    .points;
    points.pop()?;
    (points.len() > 1).then_some(points)
}

fn departure_path(
    airport: &Airport,
    points: &[Point],
    entry: Option<&[Point]>,
    runway: &str,
    stand: Waypoint,
    handling: &Handling,
    climb_speed: f64,
    climb_fpm: f64,
) -> Option<(Vec<Waypoint>, f64, f64, f64, f64, f64)> {
    let mut combined = points.to_vec();
    if let Some(entry) = entry {
        for point in entry {
            if combined
                .last()
                .is_none_or(|last| (last.east - point.east).hypot(last.north - point.north) > 1.0)
            {
                combined.push(*point);
            }
        }
    }
    let combined = phases::ground::round_taxi_path(airport, &combined, handling);
    let mut path = surface_waypoints(airport, &combined);
    if path.is_empty() {
        return None;
    }
    let hold = points.last().copied();
    let (_, threshold, far) = runway_ends(airport, runway, path.last().copied())?;
    if entry.is_none()
        && let Some(last) = path.last()
        && let Some((along, across, _, _)) =
            openatc_core::traffic::runway_position(airport, runway, last.latitude, last.longitude)
        && across.abs() < 80.0
        && along > 50.0
    {
        return None;
    }
    let _ = stand;
    let taxi_end = if entry.is_some() {
        let hold = hold.map(|point| Waypoint {
            latitude: airport.reference_latitude + point.north / 111_320.0,
            longitude: airport.reference_longitude
                + point.east
                    / (111_320.0 * (airport.reference_latitude.to_radians().cos()).max(0.01)),
            altitude_feet: airport.elevation_feet,
        });
        let cumulative = cumulative_distances(&path);
        hold.and_then(|hold| {
            path.iter()
                .zip(&cumulative)
                .min_by(|(a, _), (b, _)| {
                    openatc_core::arrival::distance_nm(
                        [a.latitude, a.longitude],
                        [hold.latitude, hold.longitude],
                    )
                    .total_cmp(&openatc_core::arrival::distance_nm(
                        [b.latitude, b.longitude],
                        [hold.latitude, hold.longitude],
                    ))
                })
                .map(|(_, distance)| *distance)
        })
        .unwrap_or(0.0)
    } else {
        cumulative_distances(&path).last().copied().unwrap_or(0.0)
    };
    let heading = openatc_core::arrival::bearing(
        [threshold.latitude, threshold.longitude],
        [far.latitude, far.longitude],
    );
    let advance = |from: Waypoint, nm: f64, altitude: f64| {
        let radians = heading.to_radians();
        let latitude = from.latitude + radians.cos() * nm * 1852.0 / 111_320.0;
        let longitude = from.longitude
            + radians.sin() * nm * 1852.0 / (111_320.0 * (latitude.to_radians().cos()).max(0.01));
        Waypoint {
            latitude,
            longitude,
            altitude_feet: airport.elevation_feet + altitude,
        }
    };
    let rotation_nm = phases::takeoff::rotation_meters(handling, airport, runway) / 1_852.0;
    let base = if entry.is_some() {
        *path.last()?
    } else {
        path.push(threshold);
        threshold
    };
    let runway_start = cumulative_distances(&path)
        .last()
        .copied()
        .unwrap_or(taxi_end);
    let rotation = advance(base, rotation_nm, 0.0);
    let rotation_index = path.len();
    path.push(rotation);
    // Climb away from the departure end at the aircraft's own gradient. A fixed
    // 1,500 ft over 5 NM made every type look like it was levelling off after
    // liftoff, regardless of its published climb rate.
    let climb_knots = (climb_speed * 1.943_844).max(40.0);
    let feet_per_nm = (climb_fpm * 60.0 / climb_knots).max(100.0);
    for after_liftoff_nm in [2.0, 5.0, 10.0] {
        path.push(advance(
            rotation,
            after_liftoff_nm,
            feet_per_nm * after_liftoff_nm,
        ));
    }
    let cumulative = cumulative_distances(&path);
    let ground_end = cumulative.get(rotation_index).copied().unwrap_or(0.0);
    Some((path, 0.0, ground_end, taxi_end, runway_start, ground_end))
}

fn arrival_path(
    airport: &Airport,
    points: &[Point],
    runway: &str,
    stand: Waypoint,
    minimum_rollout: f64,
) -> Option<(Vec<Waypoint>, f64, f64, f64, f64, f64)> {
    let (_, threshold, far) = runway_ends(airport, runway, None)?;
    let bearing = openatc_core::arrival::bearing(
        [threshold.latitude, threshold.longitude],
        [far.latitude, far.longitude],
    );
    let offset = |nm: f64, altitude: f64| {
        let radians = bearing.to_radians();
        let latitude = threshold.latitude - radians.cos() * nm * 1852.0 / 111_320.0;
        let longitude = threshold.longitude
            - radians.sin() * nm * 1852.0 / (111_320.0 * (latitude.to_radians().cos()).max(0.01));
        Waypoint {
            latitude,
            longitude,
            altitude_feet: airport.elevation_feet + altitude,
        }
    };
    let mut path = vec![offset(5.0, 1_590.0), offset(3.0, 954.0), offset(1.0, 318.0)];
    path.push(threshold);
    let exit = points
        .last()
        .and_then(|point| arrival_exit_waypoint(airport, runway, *point, minimum_rollout))?;
    path.push(exit);
    let exit_index = path.len().saturating_sub(1);
    // Keep the exit node itself: the centreline point is the rollout end, and the
    // next segment is the lateral turn onto the exit taxiway.
    let surface = surface_waypoints(airport, points);
    path.extend(surface.into_iter().rev());
    path.push(stand);
    let cumulative = cumulative_distances(&path);
    let ground_start = cumulative.get(3).copied().unwrap_or(0.0);
    let ground_end = cumulative.last().copied().unwrap_or(0.0);
    let runway_end = cumulative.get(exit_index).copied().unwrap_or(ground_start);
    if runway_end - ground_start < minimum_rollout - 1.0 {
        return None;
    }
    Some((
        path,
        ground_start,
        ground_end,
        ground_start,
        ground_start,
        runway_end,
    ))
}

fn cumulative_distances(points: &[Waypoint]) -> Vec<f64> {
    let mut result = Vec::with_capacity(points.len());
    let mut total = 0.0;
    result.push(0.0);
    for pair in points.windows(2) {
        total += openatc_core::arrival::distance_nm(
            [pair[0].latitude, pair[0].longitude],
            [pair[1].latitude, pair[1].longitude],
        ) * 1852.0;
        result.push(total);
    }
    result
}

/// Along-track distance from the start of the route to the point on it closest
/// to `target`, limited to the taxi portion of the route.
fn route_distance_to(points: &[Waypoint], cumulative: &[f64], target: Waypoint, limit: f64) -> f64 {
    let mut nearest = f64::INFINITY;
    let mut along = limit;
    for (index, pair) in points.windows(2).enumerate() {
        let start_distance = cumulative[index];
        let end_distance = cumulative[index + 1];
        if end_distance > limit + 0.001 {
            break;
        }
        let cos = (pair[0].latitude.to_radians().cos()).max(0.01);
        let east = (pair[1].longitude - pair[0].longitude) * 111_320.0 * cos;
        let north = (pair[1].latitude - pair[0].latitude) * 111_320.0;
        let east_to_target = (target.longitude - pair[0].longitude) * 111_320.0 * cos;
        let north_to_target = (target.latitude - pair[0].latitude) * 111_320.0;
        let length_squared = east * east + north * north;
        let fraction = if length_squared > 0.0 {
            ((east_to_target * east + north_to_target * north) / length_squared).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let offset_east = east * fraction - east_to_target;
        let offset_north = north * fraction - north_to_target;
        let offset = offset_east.hypot(offset_north);
        if offset < nearest {
            nearest = offset;
            along = start_distance + (end_distance - start_distance) * fraction;
        }
    }
    along
}

/// Pull a planned stop back until it is clear of the runway pavement, so a
/// departure cannot come to rest with its nose on the active runway.
fn hold_clear_of_runway(
    airport: &Airport,
    points: &[Waypoint],
    cumulative: &[f64],
    distance: f64,
) -> f64 {
    let mut stop = distance.max(0.0);
    while stop > 0.0 {
        let (latitude, longitude) = position_at(points, cumulative, stop);
        if clear_of_runway_surface(airport, latitude, longitude) {
            break;
        }
        stop = (stop - 5.0).max(0.0);
    }
    stop
}

/// A position is safe for queueing only when it is off the runway strip and away
/// from every runway-flagged taxi-graph edge.
fn clear_of_runway_surface(airport: &Airport, latitude: f64, longitude: f64) -> bool {
    let local = openatc_core::airport::airport_point(airport, latitude, longitude);
    !openatc_core::airport::within_runway(airport, &local)
        && openatc_core::airport::runway_edge_distance(airport, &local)
            .is_none_or(|distance| distance > RUNWAY_EDGE_MARGIN)
}

fn position_heading_at(points: &[Waypoint], cumulative: &[f64], distance: f64) -> (f64, f64, f64) {
    let (latitude, longitude) = position_at(points, cumulative, distance);
    let index = cumulative
        .iter()
        .position(|value| *value >= distance)
        .unwrap_or(points.len().saturating_sub(1))
        .max(1);
    let start = points.get(index.saturating_sub(1)).copied();
    let end = points.get(index).copied();
    let heading = start.zip(end).map_or(0.0, |(start, end)| {
        openatc_core::arrival::bearing(
            [start.latitude, start.longitude],
            [end.latitude, end.longitude],
        )
    });
    (latitude, longitude, heading)
}

fn position_at(points: &[Waypoint], cumulative: &[f64], distance: f64) -> (f64, f64) {
    let index = cumulative
        .iter()
        .position(|value| *value >= distance)
        .unwrap_or(points.len().saturating_sub(1))
        .max(1);
    let start = points.get(index - 1).copied().unwrap_or(Waypoint {
        latitude: 0.0,
        longitude: 0.0,
        altitude_feet: 0.0,
    });
    let end = points.get(index).copied().unwrap_or(start);
    let start_distance = cumulative.get(index - 1).copied().unwrap_or(0.0);
    let end_distance = cumulative.get(index).copied().unwrap_or(start_distance);
    let fraction = if end_distance > start_distance {
        ((distance - start_distance) / (end_distance - start_distance)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (
        start.latitude + (end.latitude - start.latitude) * fraction,
        start.longitude + (end.longitude - start.longitude) * fraction,
    )
}

/// Player distance to the named departure threshold while in the departure queue.
///
/// Returns metres to the threshold point. `None` when the player is not on the ground,
/// not in the departure queue, or too far away to be the runway queue leader.
#[must_use]
pub(crate) fn player_runway_remaining(
    airport: &Airport,
    runway: &str,
    telemetry: &openatc_core::state::Telemetry,
) -> Option<f64> {
    if !telemetry.on_ground || !telemetry.position_valid || telemetry.paused {
        return None;
    }
    let runway_data = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)?;
    let threshold = if runway_data.first_name == runway {
        runway_data.first
    } else {
        runway_data.second
    };
    let cos = (airport.reference_latitude.to_radians().cos()).max(0.01);
    let latitude = airport.reference_latitude + threshold.north / 111_320.0;
    let longitude = airport.reference_longitude + threshold.east / (111_320.0 * cos);
    let distance = openatc_core::arrival::distance_nm(
        [telemetry.latitude, telemetry.longitude],
        [latitude, longitude],
    ) * 1852.0;
    (distance <= 600.0).then_some(distance)
}

/// Closest departure ahead on the same takeoff path after both have lifted off.
///
/// Route distance includes the surface taxi path, which differs between aircraft.
/// Comparing it directly can make a taxiing aircraft on a long route look like a
/// leader and freeze an aircraft that has already taken off. Progress is measured
/// from each aircraft's own runway threshold so only aircraft on the same climb
/// path are compared.
fn climb_leader(aircraft: &[AmbientAircraft], index: usize) -> Option<(f64, f64)> {
    let own = aircraft.get(index)?;
    if own.kind != AmbientKind::Departure || own.distance < own.ground_end {
        return None;
    }
    let own_progress = own.distance - own.runway_start;
    aircraft
        .iter()
        .filter(|other| {
            other.id != own.id
                && other.kind == AmbientKind::Departure
                && other.airport == own.airport
                && other.runway == own.runway
                && other.distance >= other.ground_end
                && other.distance - other.runway_start > own_progress
        })
        .min_by(|left, right| {
            (left.distance - left.runway_start).total_cmp(&(right.distance - right.runway_start))
        })
        .map(|other| (other.distance - other.runway_start, other.speed))
}

impl TrafficManager {
    /// A faster departure must not be released immediately behind a much slower
    /// airborne departure. The follower waits at the hold or lines up and waits
    /// until the performance gap is safe, then climbs at its own speed.
    fn performance_hold(&self, index: usize) -> bool {
        let own = &self.aircraft[index];
        if own.kind != AmbientKind::Departure {
            return false;
        }
        let own_progress = own.distance - own.runway_start;
        self.aircraft.iter().any(|other| {
            other.id != own.id
                && other.kind == AmbientKind::Departure
                && other.airport == own.airport
                && other.runway == own.runway
                && other.distance >= other.ground_end
                && other.distance - other.runway_start > own_progress
                && own.climb_speed > other.speed * 1.4
                && (other.distance - other.runway_start) - own_progress < 3.0 * 1_852.0
        })
    }

    /// A departure waiting at the hold ahead of the player. The player must hold short rather
    /// than line up past it. A player already on the runway is ahead, so it does not block.
    #[must_use]
    pub(crate) fn departure_ahead_of_player(
        &self,
        airport: &Airport,
        runway: &str,
        telemetry: &openatc_core::state::Telemetry,
        player_hold: Option<Instant>,
    ) -> bool {
        if !telemetry.on_ground
            || !telemetry.position_valid
            || openatc_core::airport::aircraft_on_named_runway(airport, runway, telemetry)
        {
            return false;
        }
        let player = [telemetry.latitude, telemetry.longitude];
        self.aircraft.iter().any(|aircraft| {
            aircraft.kind == AmbientKind::Departure
                && aircraft.airport == airport.icao
                && aircraft.runway == runway
                && !aircraft.cleared
                && !aircraft.takeoff
                && aircraft.distance >= aircraft.hold_distance - 60.0
                && aircraft.hold.is_some_and(|hold| {
                    let player_to_hold = openatc_core::arrival::distance_nm(player, hold) * 1852.0;
                    let own_to_hold = (aircraft.hold_distance - aircraft.distance).max(0.0);
                    let ahead_by_position = own_to_hold <= player_to_hold + 15.0;
                    // The first aircraft at its hold goes first. If the player was already
                    // there, an ambient aircraft that arrived later does not jump ahead.
                    let ahead_by_time = match (player_hold, aircraft.hold_since) {
                        (None, _) => true,
                        (Some(_), None) => false,
                        (Some(player), Some(ambient)) => ambient < player,
                    };
                    ahead_by_position && ahead_by_time
                })
        })
    }
}

/// Whether an ambient departure is still behind the player in the runway queue.
///
/// Both are measured to the holding point. A straight line from the player to the
/// threshold can put the player ahead of a jet already at the hold on a curved
/// taxiway. Airports without a holding marker fall back to the runway start and
/// the threshold distance.
fn departure_behind_player(
    aircraft: &AmbientAircraft,
    player: [f64; 2],
    player_runway: &str,
    threshold_remaining: f64,
) -> bool {
    if aircraft.runway != player_runway || aircraft.distance >= aircraft.runway_start {
        return false;
    }
    let (own_remaining, player_remaining) = match aircraft.hold {
        Some(hold) => (
            (aircraft.hold_distance - aircraft.distance).max(0.0),
            openatc_core::arrival::distance_nm(player, hold) * 1852.0,
        ),
        None => (
            aircraft.runway_start - aircraft.distance,
            threshold_remaining,
        ),
    };
    own_remaining > player_remaining + 30.0
}

/// First departure at the hold in arrival order. The player is part of the same queue:
/// if the player reached the hold first, no ambient aircraft is the runway leader.
fn queue_leader(aircraft: &[AmbientAircraft], player_hold: Option<Instant>) -> Option<u32> {
    let leader = aircraft
        .iter()
        .filter(|aircraft| {
            aircraft.kind == AmbientKind::Departure
                && !aircraft.takeoff
                && aircraft.distance >= aircraft.hold_distance
        })
        .min_by(|left, right| {
            left.hold_since
                .cmp(&right.hold_since)
                .then(left.id.cmp(&right.id))
        })?;
    match (player_hold, leader.hold_since) {
        (Some(player), Some(ambient)) if player < ambient => None,
        (Some(_), None) => None,
        _ => Some(leader.id),
    }
}

fn ambient_frame(aircraft: &AmbientAircraft) -> Option<GeneratedTraffic> {
    let index = aircraft
        .cumulative
        .iter()
        .position(|distance| *distance >= aircraft.distance)
        .unwrap_or(aircraft.points.len().saturating_sub(1));
    let index = index.max(1);
    let start = aircraft.points.get(index - 1)?;
    let end = aircraft.points.get(index)?;
    let start_distance = aircraft.cumulative[index - 1];
    let end_distance = aircraft.cumulative[index];
    let fraction = if end_distance > start_distance {
        ((aircraft.distance - start_distance) / (end_distance - start_distance)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let latitude = start.latitude + (end.latitude - start.latitude) * fraction;
    let longitude = start.longitude + (end.longitude - start.longitude) * fraction;
    let altitude = start.altitude_feet + (end.altitude_feet - start.altitude_feet) * fraction;
    let before = position_at(
        &aircraft.points,
        &aircraft.cumulative,
        (aircraft.distance - (aircraft.profile.length / 2.0).max(5.0)).max(0.0),
    );
    let after = position_at(
        &aircraft.points,
        &aircraft.cumulative,
        (aircraft.distance + (aircraft.profile.length / 2.0).max(5.0)).min(
            aircraft
                .cumulative
                .last()
                .copied()
                .unwrap_or(aircraft.distance),
        ),
    );
    let track = if (before.0 - after.0).abs() > 1e-9 || (before.1 - after.1).abs() > 1e-9 {
        openatc_core::arrival::bearing([before.0, before.1], [after.0, after.1])
    } else {
        openatc_core::arrival::bearing(
            [start.latitude, start.longitude],
            [end.latitude, end.longitude],
        )
    };
    let on_ground = match aircraft.kind {
        AmbientKind::Departure => aircraft.distance < aircraft.ground_end,
        AmbientKind::Arrival => aircraft.distance >= aircraft.ground_start,
    };
    Some(GeneratedTraffic {
        vertical_speed_fpm: if on_ground {
            0.0
        } else if aircraft.kind == AmbientKind::Departure {
            aircraft.climb_fpm
        } else {
            -aircraft.descent_fpm
        },
        body_heading_degrees: Some(track),
        target: TrafficTarget {
            id: aircraft.id,
            callsign: aircraft.callsign.clone(),
            aircraft: aircraft.aircraft.clone(),
            latitude,
            longitude,
            altitude_feet: altitude,
            track_degrees: track,
            on_ground,
            ground_speed_knots: Some(aircraft.speed * 1.943_844),
            wingspan_meters: Some(aircraft.profile.wingspan),
            wake_category: aircraft.wake_category,
            runway_occupancy: Some(aircraft.on_runway()),
            com1_khz: aircraft.frequency_khz,
            ..Default::default()
        },
        model: aircraft.model.clone(),
        animation: aircraft.animation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(latitude: f64, longitude: f64) -> Waypoint {
        Waypoint {
            latitude,
            longitude,
            altitude_feet: 0.0,
        }
    }

    #[test]
    fn hold_distance_projects_onto_the_route_segment() {
        let points = [point(0.0, 0.0), point(0.0, 0.001), point(0.0, 0.002)];
        let cumulative = cumulative_distances(&points);
        let marker = point(0.000_05, 0.001_5);
        let along = route_distance_to(&points, &cumulative, marker, f64::INFINITY);
        let expected = 0.001_5 * 111_320.0;
        assert!(
            (along - expected).abs() < 1.0,
            "expected {expected} m along the route, got {along} m"
        );
    }

    #[test]
    fn hold_distance_stays_inside_the_taxi_portion() {
        let points = [point(0.0, 0.0), point(0.0, 0.001), point(0.0, 0.002)];
        let cumulative = cumulative_distances(&points);
        let marker = point(0.0, 0.001_5);
        let limit = cumulative[1];
        let along = route_distance_to(&points, &cumulative, marker, limit);
        assert_eq!(along, limit);
    }
}

#[cfg(test)]
mod phase_tests {
    use super::*;
    use openatc_core::traffic::csl::{Match, Model};
    use openatc_core::traffic::profiles::Hub;
    use std::collections::BTreeMap;

    fn fake_model(id: &str, aircraft: &str, operator: &str) -> Model {
        Model {
            id: id.to_owned(),
            objects: Vec::new(),
            vertical_offset: None,
            matches: vec![Match {
                aircraft: aircraft.to_owned(),
                operator: operator.to_owned(),
                livery: String::new(),
            }],
        }
    }

    #[test]
    fn ambient_uses_hub_operators_and_never_the_first_model() {
        let profiles = Profiles::builtin().expect("bundled profiles");
        let models = Catalogue {
            models: vec![
                fake_model("xsn-a319", "A319", "XSN"),
                fake_model("ezy-a320", "A320", "EZY"),
                fake_model("afr-a320", "A320", "AFR"),
            ],
            warnings: Vec::new(),
        };
        let used = BTreeSet::new();
        let chosen: Vec<_> = (0..24)
            .filter_map(|seed| {
                ambient_identity(
                    &profiles, &models, "LFMN", 2500.0, false, seed, &used, "C-GTLT", 8, 4, 0,
                )
            })
            .collect();
        assert!(!chosen.is_empty());
        for candidate in &chosen {
            assert!(
                candidate.callsign.starts_with("EZY") || candidate.callsign.starts_with("AFR"),
                "{}",
                candidate.callsign
            );
            assert_ne!(candidate.model.id, "xsn-a319");
        }
        let only_spanish = Catalogue {
            models: vec![fake_model("xsn-a319", "A319", "XSN")],
            warnings: Vec::new(),
        };
        assert!(
            ambient_identity(
                &profiles,
                &only_spanish,
                "LFMN",
                2500.0,
                false,
                1,
                &used,
                "C-GTLT",
                8,
                4,
                0,
            )
            .is_none(),
            "an unrelated paint must not be used as a fallback"
        );
    }

    #[test]
    fn cargo_density_reaches_the_ambient_mix() {
        let profiles = Profiles::builtin().expect("bundled profiles");
        let models = Catalogue {
            models: vec![fake_model("fdx-b752", "B752", "FDX")],
            warnings: Vec::new(),
        };
        let candidates = airline_candidates(
            &profiles,
            &models,
            "KJFK",
            3_000.0,
            1,
            &BTreeSet::new(),
            "TEST",
            false,
            0,
            0,
            100,
        );
        assert!(
            candidates
                .iter()
                .any(|(_, candidate)| candidate.callsign.starts_with("FDX")),
            "cargo should contribute ambient aircraft when its weight is set"
        );
    }

    #[test]
    fn ga_gets_a_registration_and_an_unmarked_model() {
        let profiles = Profiles::builtin().expect("bundled profiles");
        let models = Catalogue {
            models: vec![
                fake_model("c172", "C172", ""),
                fake_model("be20", "BE20", ""),
            ],
            warnings: Vec::new(),
        };
        let used = BTreeSet::new();
        let chosen: Vec<_> = (0..24)
            .filter_map(|seed| {
                ambient_identity(
                    &profiles, &models, "LFMN", 2500.0, true, seed, &used, "C-GTLT", 8, 4, 0,
                )
            })
            .collect();
        assert!(!chosen.is_empty());
        for candidate in &chosen {
            assert!(candidate.aircraft == "C172" || candidate.aircraft == "BE20");
            assert!(
                candidate.callsign.starts_with("F-"),
                "{}",
                candidate.callsign
            );
            assert!(
                candidate
                    .model
                    .matches
                    .iter()
                    .all(|entry| entry.operator.is_empty())
            );
        }
    }

    #[test]
    fn a_hub_can_select_an_installed_regional_turboprop() {
        let mut profiles = Profiles::builtin().expect("bundled profiles");
        profiles.hubs.insert(
            "LFXX".into(),
            Hub {
                name: "Test".into(),
                airlines: BTreeMap::from([("CCM".to_owned(), 100)]),
                latitude: None,
                longitude: None,
            },
        );
        let models = Catalogue {
            models: vec![fake_model("ccm-at72", "AT72", "CCM")],
            warnings: Vec::new(),
        };
        let chosen = ambient_identity(
            &profiles,
            &models,
            "LFXX",
            1800.0,
            false,
            1,
            &BTreeSet::new(),
            "C-GTLT",
            8,
            4,
            0,
        )
        .expect("regional turboprop");
        assert_eq!(chosen.aircraft, "AT72");
        assert!(chosen.callsign.starts_with("CCM"));
        assert_eq!(chosen.profile.handling, Some(Kind::Turboprop));
        assert!(
            ambient_identity(
                &profiles,
                &models,
                "LFXX",
                1800.0,
                false,
                1,
                &BTreeSet::new(),
                "C-GTLT",
                8,
                0,
                0,
            )
            .is_none(),
            "a zero regional share removes the turboprop"
        );
    }

    fn jet_handling() -> Arc<Handling> {
        let mut handling = animation::builtin().expect("bundled handling catalogue");
        Arc::new(handling.remove(&Kind::Jet).expect("jet handling"))
    }

    fn ambient(
        kind: AmbientKind,
        distance: f64,
        ground_start: f64,
        ground_end: f64,
    ) -> AmbientAircraft {
        let points = vec![
            Waypoint {
                latitude: 0.0,
                longitude: 0.0,
                altitude_feet: 0.0,
            },
            Waypoint {
                latitude: 0.0,
                longitude: 0.01,
                altitude_feet: 0.0,
            },
        ];
        let profile = openatc_core::traffic::profiles::aircraft("A320")
            .cloned()
            .expect("A320 profile");
        AmbientAircraft {
            id: 1,
            callsign: "TEST1".to_owned(),
            aircraft: "A320".to_owned(),
            profile,
            wake_category: Some(1),
            airport: "TEST".to_owned(),
            runway: "04L".to_owned(),
            route: String::new(),
            spawn_node: 1,
            model: Arc::new(Model {
                id: "test".to_owned(),
                objects: Vec::new(),
                vertical_offset: None,
                matches: Vec::new(),
            }),
            kind,
            cumulative: cumulative_distances(&points),
            points,
            ground_start,
            ground_end,
            taxi_end: ground_start,
            runway_start: ground_start,
            runway_end: ground_end,
            hold_distance: 0.0,
            hold: None,
            animation: Animation::default(),
            handling: jet_handling(),
            taxi_knots: TAXI_STRAIGHT_METERS_PER_SECOND,
            taxi_turn_knots: 10.0 * 0.514_444,
            climb_speed: 80.0,
            minimum_climb_speed: 0.0,
            climb_fpm: 2_200.0,
            descent_fpm: 1_800.0,
            approach_speed: 75.0,
            touchdown_speed: 70.0,
            distance,
            motion: crate::traffic::motion::MotionController::new(
                TAXI_STRAIGHT_METERS_PER_SECOND,
                0.0,
            ),
            cleared: true,
            takeoff: false,
            hold_since: None,
            requested: false,
            speed: TAXI_STRAIGHT_METERS_PER_SECOND,
            blocked: false,
            blocker: None,
            awaiting_clearance: false,
            yield_to: None,
            yield_announced: false,
            player_hold_since: None,
            wake_delay: 0.0,
            last: Instant::now(),
            frequency_khz: None,
        }
    }

    #[test]
    fn ambient_spawning_stops_once_the_player_moves() {
        let mut state = crate::test_state();
        state.airport = Some(openatc_core::airport::demo_airport());
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.ground_speed_knots = 0.0;
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            ..Default::default()
        };
        manager.tick(&mut state);
        assert!(!manager.movement_started);
        state.session.telemetry.ground_speed_knots = 2.0;
        manager.tick(&mut state);
        assert!(manager.movement_started);
        state.session.telemetry.ground_speed_knots = 0.0;
        manager.tick(&mut state);
        assert!(
            manager.movement_started,
            "stopping at a hold after taxiing must not re-enable spawns"
        );
    }

    #[test]
    fn light_ga_uses_piston_speeds_and_light_wake() {
        let mut handling = animation::builtin().expect("bundled handling catalogue");
        let piston = handling.remove(&Kind::Piston).expect("piston handling");
        let jet = handling.remove(&Kind::Jet).expect("jet handling");
        let c172 = openatc_core::traffic::profiles::aircraft("C172").expect("C172 profile");
        let a320 = openatc_core::traffic::profiles::aircraft("A320").expect("A320 profile");
        assert!(phases::sid::climb_speed(c172, &piston) < phases::sid::climb_speed(a320, &jet));
        assert!((phases::approach::speed(c172) - 65.0 * 0.514_444).abs() < 0.01);
        assert!((phases::landing::touchdown_speed(c172) - 60.0 * 0.514_444).abs() < 0.01);
        assert_eq!(wake_category(c172), Some(0));
        assert_eq!(wake_category(a320), Some(1));
        let start = 0.0;
        let end = 300.0;
        let rotation = piston.rotation_knots * 0.514_444;
        assert!(
            (phases::takeoff::roll_speed(&piston, end, start, end, 5.0) - rotation).abs() < 0.01
        );
    }

    #[test]
    fn runway_state_distinguishes_line_up_rolling_and_landing() {
        let mut manager = TrafficManager::default();
        assert_eq!(manager.runway_state("LFMN", "04R"), RunwayState::Clear);
        let mut departure = ambient(AmbientKind::Departure, 600.0, 0.0, 1_000.0);
        departure.airport = "LFMN".into();
        departure.runway = "04R".into();
        departure.hold_distance = 600.0;
        departure.distance = 600.0;
        departure.cleared = true;
        departure.takeoff = false;
        manager.aircraft.push(departure);
        assert_eq!(manager.runway_state("LFMN", "04R"), RunwayState::LineUp);
        manager.aircraft[0].takeoff = true;
        manager.aircraft[0].distance = 700.0;
        assert_eq!(manager.runway_state("LFMN", "04R"), RunwayState::Rolling);
        manager.aircraft[0].takeoff = false;
        manager.aircraft[0].cleared = false;
        let mut arrival = ambient(AmbientKind::Arrival, 500.0, 500.0, 2_000.0);
        arrival.airport = "LFMN".into();
        arrival.runway = "04L".into();
        manager.aircraft.push(arrival);
        assert_eq!(manager.runway_state("LFMN", "04L"), RunwayState::Landing);
        assert_eq!(manager.runway_state("LFMN", "04R"), RunwayState::Clear);
    }

    #[test]
    fn ambient_frames_are_published_when_advancement_cannot_run() {
        let mut state = crate::test_state();
        state.session.telemetry.simulation_seconds = Some(12.0);
        let mut manager = TrafficManager {
            aircraft: vec![ambient(AmbientKind::Departure, 100.0, 0.0, 1_000.0)],
            ..Default::default()
        };
        manager.tick(&mut state);
        assert_eq!(
            state.session.generated_traffic_clock,
            Some((state.session_generation, 12.0))
        );
        assert!(
            state
                .session
                .generated_traffic
                .iter()
                .any(|frame| frame.target.id == 1),
            "a missing model catalogue must not hide ambient traffic"
        );
        manager.aircraft.clear();
        manager.tick(&mut state);
        assert!(
            state
                .session
                .generated_traffic
                .iter()
                .all(|frame| frame.target.id != 1),
            "retired ambient traffic must leave the published set"
        );
    }

    #[test]
    fn the_player_lined_up_blocks_the_ambient_runway() {
        let manager = TrafficManager::default();
        assert_eq!(
            manager.runway_state_for(
                "LFMN",
                "04R",
                None,
                Some(PlayerRunway {
                    lined_up: true,
                    rolling: false,
                }),
                None,
            ),
            RunwayState::LineUp
        );
        assert_eq!(
            manager.runway_state_for(
                "LFMN",
                "04R",
                None,
                Some(PlayerRunway {
                    lined_up: false,
                    rolling: true,
                }),
                None,
            ),
            RunwayState::Rolling
        );
        assert_eq!(
            manager.runway_state_for("LFMN", "04R", None, None, None),
            RunwayState::Clear
        );
    }

    #[test]
    fn a_lined_up_aircraft_blocks_the_player_regardless_of_threshold_distance() {
        let mut manager = TrafficManager::default();
        let mut departure = ambient(AmbientKind::Departure, 600.0, 0.0, 2_000.0);
        departure.airport = "LFMN".into();
        departure.runway = "04R".into();
        departure.hold_distance = 600.0;
        departure.distance = 600.0;
        departure.runway_start = 1_000.0;
        departure.runway_end = 2_000.0;
        departure.cleared = true;
        departure.takeoff = false;
        manager.aircraft.push(departure);
        assert_eq!(
            manager.runway_state_for("LFMN", "04R", Some(100.0), None, None),
            RunwayState::LineUp,
            "a lined-up aircraft owns the runway until it rolls"
        );
        assert_eq!(
            manager.runway_state_for("LFMN", "04R", Some(500.0), None, None),
            RunwayState::LineUp,
            "an ambient ahead of the player still owns the runway"
        );
        manager.aircraft[0].distance = 500.0;
        assert_eq!(
            manager.runway_state_for("LFMN", "04R", Some(100.0), None, None),
            RunwayState::LineUp,
            "a line-up clearance reserves the slot before the hold line"
        );
    }

    #[test]
    fn a_rolling_departure_does_not_hide_a_lined_up_aircraft() {
        let mut manager = TrafficManager::default();
        let mut rolling = ambient(AmbientKind::Departure, 1_200.0, 0.0, 3_000.0);
        rolling.id = 1;
        rolling.airport = "LFMN".into();
        rolling.runway = "04R".into();
        rolling.distance = 1_200.0;
        rolling.runway_start = 1_000.0;
        rolling.runway_end = 2_500.0;
        rolling.ground_end = 3_000.0;
        rolling.cleared = true;
        rolling.takeoff = true;
        let mut lined_up = ambient(AmbientKind::Departure, 1_000.0, 0.0, 3_000.0);
        lined_up.id = 2;
        lined_up.airport = "LFMN".into();
        lined_up.runway = "04R".into();
        lined_up.hold_distance = 600.0;
        lined_up.distance = 1_000.0;
        lined_up.runway_start = 1_000.0;
        lined_up.runway_end = 2_500.0;
        lined_up.ground_end = 3_000.0;
        lined_up.cleared = true;
        lined_up.takeoff = false;
        manager.aircraft = vec![rolling, lined_up];
        assert_eq!(
            manager.runway_state("LFMN", "04R"),
            RunwayState::LineUp,
            "the lined-up aircraft keeps the next departure at the hold"
        );
        manager.aircraft.pop();
        assert_eq!(
            manager.runway_state("LFMN", "04R"),
            RunwayState::Rolling,
            "a rolling departure alone still allows the next aircraft to line up"
        );
    }

    #[test]
    fn the_next_departure_clears_after_the_same_runway_separation() {
        let mut manager = TrafficManager::default();
        let mut departure = ambient(AmbientKind::Departure, 600.0, 0.0, 4_000.0);
        departure.airport = "LFMN".into();
        departure.runway = "04R".into();
        departure.hold_distance = 600.0;
        departure.distance = 2_700.0;
        departure.runway_start = 1_000.0;
        departure.ground_end = 1_500.0;
        departure.runway_end = 4_000.0;
        departure.cleared = true;
        departure.takeoff = true;
        manager.aircraft.push(departure);
        assert_eq!(
            manager.runway_state("LFMN", "04R"),
            RunwayState::Rolling,
            "the next departure must wait for the separation point"
        );
        manager.aircraft[0].distance = 2_800.0;
        assert_eq!(
            manager.runway_state("LFMN", "04R"),
            RunwayState::Clear,
            "the next departure may roll before the far runway end"
        );
    }

    #[test]
    fn a_departure_is_clear_once_it_lifts_off() {
        let mut manager = TrafficManager::default();
        let mut departure = ambient(AmbientKind::Departure, 1_000.0, 0.0, 1_000.0);
        departure.airport = "LFMN".into();
        departure.runway = "04R".into();
        departure.hold_distance = 600.0;
        departure.cleared = true;
        departure.takeoff = true;
        departure.distance = 2_000.0;
        departure.runway_start = 1_000.0;
        departure.runway_end = 2_000.0;
        manager.aircraft.push(departure);
        assert_eq!(manager.runway_state("LFMN", "04R"), RunwayState::Clear);
    }

    #[test]
    fn the_player_distance_to_the_runway_is_measured_from_the_threshold() {
        let airport = openatc_core::airport::demo_airport();
        let at_hold = openatc_core::state::Telemetry {
            position_valid: true,
            on_ground: true,
            latitude: 0.0,
            longitude: -1_300.0 / 111_320.0,
            ..Default::default()
        };
        let remaining =
            player_runway_remaining(&airport, "09", &at_hold).expect("on the approach side");
        assert!((remaining - 100.0).abs() < 2.0, "{remaining}");
        let far = openatc_core::state::Telemetry {
            longitude: -4_000.0 / 111_320.0,
            ..at_hold.clone()
        };
        assert!(player_runway_remaining(&airport, "09", &far).is_none());
    }

    #[test]
    fn an_arrival_exits_after_a_real_rollout_not_at_the_threshold() {
        let airport = openatc_core::airport::demo_airport();
        let stand = Waypoint {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 0.0,
        };
        let exit = Point {
            east: -300.0,
            north: 80.0,
            height: 0.0,
        };
        let (path, ground_start, _, _, _, runway_end) =
            arrival_path(&airport, &[exit], "09", stand, 700.0).expect("arrival path");
        assert!(runway_end - ground_start >= 699.0);
        assert!(path.len() > 5, "the exit and taxi-in remain on the path");
        let early = Point {
            east: -1_100.0,
            north: 80.0,
            height: 0.0,
        };
        assert!(
            arrival_path(&airport, &[early], "09", stand, 700.0).is_none(),
            "an exit at the threshold must reject the arrival instead of reversing"
        );
    }

    #[test]
    fn an_arrival_route_targets_the_exit_not_the_threshold() {
        use openatc_core::airport::{Runway, TaxiEdge, TaxiNode};
        let airport = Airport {
            icao: "TEST".into(),
            reference_latitude: 0.0,
            reference_longitude: 0.0,
            runways: vec![Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                first: Point::default(),
                second: Point {
                    east: 2_000.0,
                    ..Default::default()
                },
                width: 45.0,
                ..Default::default()
            }],
            nodes: vec![
                TaxiNode {
                    id: 1,
                    point: Point {
                        east: 0.0,
                        north: -200.0,
                        ..Default::default()
                    },
                },
                TaxiNode {
                    id: 2,
                    point: Point {
                        east: 900.0,
                        north: 60.0,
                        ..Default::default()
                    },
                },
                TaxiNode {
                    id: 3,
                    point: Point {
                        east: 0.0,
                        north: 60.0,
                        ..Default::default()
                    },
                },
            ],
            edges: vec![
                TaxiEdge {
                    first: 1,
                    second: 2,
                    size: 'C',
                    ..Default::default()
                },
                TaxiEdge {
                    first: 2,
                    second: 3,
                    size: 'C',
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let (arrival, _) =
            plan_taxi(&airport, 1, "09", 'C', Some(700.0), None).expect("arrival taxi route");
        assert!(
            (arrival.last().expect("exit").east - 900.0).abs() < 1.0,
            "arrival should route to the exit at 900 m, got {:?}",
            arrival.last()
        );
        let (departure, _) =
            plan_taxi(&airport, 1, "09", 'C', None, None).expect("departure taxi route");
        assert!(
            departure.last().expect("hold").east.abs() < 1.0,
            "departure should route to the threshold hold"
        );
    }

    #[test]
    fn a_departure_climbs_at_the_aircraft_gradient() {
        let airport = openatc_core::airport::demo_airport();
        let points = [Point {
            east: -1_300.0,
            north: 0.0,
            height: 0.0,
        }];
        let stand = Waypoint {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 0.0,
        };
        let handling = jet_handling();
        let (path, _, ground_end, _, runway_start, runway_end) = departure_path(
            &airport, &points, None, "09", stand, &handling, 82.0, 2_200.0,
        )
        .expect("departure path");
        assert!(runway_start < ground_end);
        assert_eq!(runway_end, ground_end);
        assert!(
            path.len() >= 5,
            "the climb continues beyond the old single waypoint"
        );
        let last = path.last().expect("climb end");
        assert!(
            last.altitude_feet > 4_000.0,
            "a jet at 2,200 fpm should climb well over 4,000 ft in the first 10 NM, got {}",
            last.altitude_feet
        );
    }

    #[test]
    fn a_jet_climb_path_uses_the_cruise_derived_speed() {
        let airport = openatc_core::airport::demo_airport();
        let points = [Point {
            east: -1_300.0,
            north: 0.0,
            height: 0.0,
        }];
        let stand = Waypoint {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 0.0,
        };
        let handling = jet_handling();
        let a320 = openatc_core::traffic::profiles::aircraft("A320").expect("A320");
        let climb_speed = phases::sid::climb_speed(a320, &handling);
        let (path, _, _, _, _, _) = departure_path(
            &airport,
            &points,
            None,
            "09",
            stand,
            &handling,
            climb_speed,
            a320.climb_fpm,
        )
        .expect("departure path");
        let last = path.last().expect("climb end");
        assert!(
            (4_500.0..=6_000.0).contains(&last.altitude_feet),
            "a jet at 2,200 fpm should be near 5,300 ft at 10 NM, got {}",
            last.altitude_feet
        );
    }

    #[test]
    fn seeds_are_spaced_along_the_surface_route() {
        let positions = seed_distances(1_000.0, 6);
        assert_eq!(positions.len(), 6);
        assert!(
            positions
                .windows(2)
                .all(|pair| pair[1] - pair[0] >= SEED_SPACING),
            "{positions:?}"
        );
        assert!(positions.last().is_some_and(|last| *last <= 940.0));
        let short = seed_distances(120.0, 4);
        assert_eq!(short.len(), 1);
        assert!(seed_distances(50.0, 4).is_empty());
        let spread = seed_distances(2_000.0, 3);
        assert_eq!(spread.len(), 3);
        assert!(
            spread[0] > 400.0,
            "the first departure should be clear of the apron: {spread:?}"
        );
        assert!(
            spread
                .windows(2)
                .all(|pair| pair[1] - pair[0] >= SEED_SPACING),
            "{spread:?}"
        );
    }

    #[test]
    fn a_node_on_a_runway_edge_is_not_a_spawn_point() {
        use openatc_core::airport::{Runway, TaxiEdge, TaxiNode};
        let airport = Airport {
            icao: "TEST".into(),
            reference_latitude: 0.0,
            reference_longitude: 0.0,
            runways: vec![Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                first: Point::default(),
                second: Point {
                    east: 2_000.0,
                    ..Default::default()
                },
                width: 45.0,
                ..Default::default()
            }],
            nodes: vec![
                TaxiNode {
                    id: 1,
                    point: Point {
                        east: -100.0,
                        north: 0.0,
                        ..Default::default()
                    },
                },
                TaxiNode {
                    id: 2,
                    point: Point {
                        east: -100.0,
                        north: 150.0,
                        ..Default::default()
                    },
                },
                TaxiNode {
                    id: 3,
                    point: Point {
                        east: -100.0,
                        north: 250.0,
                        ..Default::default()
                    },
                },
                TaxiNode {
                    id: 4,
                    point: Point {
                        east: -100.0,
                        north: 100.0,
                        ..Default::default()
                    },
                },
            ],
            edges: vec![
                TaxiEdge {
                    first: 1,
                    second: 4,
                    runway: true,
                    size: 'C',
                    ..Default::default()
                },
                TaxiEdge {
                    first: 2,
                    second: 3,
                    runway: false,
                    size: 'C',
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let node = spawn_node(
            &airport,
            'C',
            &BTreeSet::new(),
            &BTreeSet::new(),
            &[],
            false,
            7,
        )
        .expect("a non-runway taxi node");
        assert_ne!(node.id, 1, "a node on the runway edge must be rejected");
        assert!(openatc_core::airport::on_non_runway_taxiway(
            &airport,
            &node.point,
            8.0
        ));
    }

    #[test]
    fn a_fast_departure_waits_behind_a_slow_airborne_leader() {
        let mut leader = ambient(AmbientKind::Departure, 1_000.0, 0.0, 900.0);
        leader.id = 1;
        leader.runway_start = 0.0;
        leader.speed = 20.0;
        leader.climb_speed = 20.0;
        let mut follower = ambient(AmbientKind::Departure, 500.0, 0.0, 4_000.0);
        follower.id = 2;
        follower.runway_start = 0.0;
        follower.climb_speed = 120.0;
        let manager = TrafficManager {
            aircraft: vec![leader, follower],
            ..Default::default()
        };
        assert!(
            manager.performance_hold(1),
            "a jet must not be released behind a much slower airborne aircraft"
        );
        let mut similar = manager;
        similar.aircraft[1].climb_speed = 25.0;
        assert!(!similar.performance_hold(1));
    }

    #[test]
    fn climb_spacing_ignores_taxiing_traffic_on_a_longer_route() {
        let mut taxiing = ambient(AmbientKind::Departure, 3_198.0, 0.0, 5_288.0);
        taxiing.id = 1;
        taxiing.runway_start = 0.0;
        taxiing.speed = 0.0;
        let mut airborne = ambient(AmbientKind::Departure, 3_168.0, 0.0, 3_147.0);
        airborne.id = 2;
        airborne.runway_start = 2_847.0;
        airborne.speed = 80.0;
        let aircraft = vec![taxiing, airborne];
        assert_eq!(
            climb_leader(&aircraft, 1),
            None,
            "a taxiing aircraft cannot be a climb leader"
        );
    }

    #[test]
    fn climb_spacing_compares_progress_from_the_runway_threshold() {
        // This aircraft has the larger absolute route distance but is behind the
        // other aircraft relative to its own runway threshold.
        let mut longer = ambient(AmbientKind::Departure, 4_000.0, 0.0, 3_900.0);
        longer.id = 1;
        longer.runway_start = 3_900.0;
        longer.speed = 70.0;
        let mut ahead = ambient(AmbientKind::Departure, 3_500.0, 0.0, 3_400.0);
        ahead.id = 2;
        ahead.runway_start = 3_000.0;
        ahead.speed = 90.0;
        let aircraft = vec![longer, ahead];
        assert_eq!(
            climb_leader(&aircraft, 1),
            None,
            "absolute route distance must not order aircraft on different routes"
        );

        let mut leader = ambient(AmbientKind::Departure, 3_500.0, 0.0, 3_400.0);
        leader.id = 3;
        leader.runway_start = 3_000.0;
        leader.speed = 90.0;
        let mut follower = ambient(AmbientKind::Departure, 3_200.0, 0.0, 3_100.0);
        follower.id = 4;
        follower.runway_start = 2_900.0;
        follower.speed = 80.0;
        let aircraft = vec![leader, follower];
        assert_eq!(climb_leader(&aircraft, 1), Some((500.0, 90.0)));
    }

    #[test]
    fn a_faster_departure_does_not_overrun_a_slower_leader() {
        let airport = openatc_core::airport::demo_airport();
        let mut state = crate::test_state();
        state.airport = Some(airport);
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 0.02;
        state.session.telemetry.longitude = 0.02;
        state.session.telemetry.traffic = Some(Vec::new());
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            active_airport: "DEMO".into(),
            ..Default::default()
        };
        let mut leader = ambient(AmbientKind::Departure, 500.0, 0.0, 300.0);
        leader.id = 1;
        leader.airport = "DEMO".into();
        leader.runway = "09".into();
        leader.climb_speed = 80.0;
        leader.distance = 500.0;
        let mut follower = ambient(AmbientKind::Departure, 300.0, 0.0, 300.0);
        follower.id = 2;
        follower.airport = "DEMO".into();
        follower.runway = "09".into();
        follower.climb_speed = 160.0;
        follower.distance = 300.0;
        manager.aircraft.push(leader);
        manager.aircraft.push(follower);
        manager.tick(&mut state);
        assert!(manager.aircraft[1].speed <= manager.aircraft[0].speed + 0.01);
    }

    #[test]
    fn two_aircraft_at_the_same_point_do_not_both_hold() {
        let airport = openatc_core::airport::demo_airport();
        let mut state = crate::test_state();
        state.airport = Some(airport);
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 0.02;
        state.session.telemetry.longitude = 0.02;
        state.session.telemetry.traffic = Some(Vec::new());
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            active_airport: "DEMO".into(),
            ..Default::default()
        };
        for id in [1, 2] {
            let mut departure = ambient(AmbientKind::Departure, 0.0, 0.0, 2_000.0);
            departure.id = id;
            departure.airport = "DEMO".into();
            departure.runway = "09".into();
            departure.hold_distance = 600.0;
            departure.runway_start = 1_000.0;
            departure.runway_end = 2_000.0;
            departure.distance = 0.0;
            departure.speed = 0.0;
            departure.frequency_khz = None;
            manager.aircraft.push(departure);
        }
        manager.tick(&mut state);
        assert!(
            !manager.aircraft[0].blocked,
            "the queue leader must keep moving even when another aircraft is on its point"
        );
        assert!(manager.aircraft[1].blocked);
        assert_eq!(manager.aircraft[1].blocker, Some(1));
    }

    #[test]
    fn a_blocked_ambient_aircraft_reports_the_player_as_its_blocker() {
        let airport = openatc_core::airport::demo_airport();
        let mut state = crate::test_state();
        state.airport = Some(airport);
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 0.01;
        state.session.telemetry.longitude = 0.0005;
        state.session.telemetry.traffic = Some(Vec::new());
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            active_airport: "DEMO".into(),
            ..Default::default()
        };
        let mut departure = ambient(AmbientKind::Departure, 0.0, 0.0, 2_000.0);
        departure.airport = "DEMO".into();
        departure.runway = "09".into();
        departure.hold_distance = 600.0;
        departure.runway_start = 1_000.0;
        departure.runway_end = 2_000.0;
        departure.points = vec![
            Waypoint {
                latitude: 0.01,
                longitude: 0.0,
                altitude_feet: 0.0,
            },
            Waypoint {
                latitude: 0.01,
                longitude: 0.01,
                altitude_feet: 0.0,
            },
        ];
        departure.cumulative = cumulative_distances(&departure.points);
        departure.distance = 0.0;
        departure.speed = 0.0;
        manager.aircraft.push(departure);
        manager.tick(&mut state);
        assert_eq!(manager.aircraft[0].blocker, Some(0));
        assert!(manager.aircraft[0].blocked);
    }

    #[test]
    fn a_joining_ambient_yields_to_the_player_on_its_right() {
        let airport = openatc_core::airport::demo_airport();
        let mut state = crate::test_state();
        state.airport = Some(airport);
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 0.0006;
        state.session.telemetry.longitude = 0.0005;
        state.session.telemetry.heading_degrees = 270.0;
        state.session.telemetry.ground_speed_knots = 10.0;
        state.session.telemetry.traffic = Some(Vec::new());
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            active_airport: "DEMO".into(),
            ..Default::default()
        };
        let mut departure = ambient(AmbientKind::Departure, 0.0, 0.0, 2_000.0);
        departure.airport = "DEMO".into();
        departure.runway = "09".into();
        departure.hold_distance = 600.0;
        departure.runway_start = 1_000.0;
        departure.runway_end = 2_000.0;
        departure.points = vec![
            Waypoint {
                latitude: 0.0,
                longitude: 0.0,
                altitude_feet: 0.0,
            },
            Waypoint {
                latitude: 0.01,
                longitude: 0.0,
                altitude_feet: 0.0,
            },
        ];
        departure.cumulative = cumulative_distances(&departure.points);
        departure.distance = 0.0;
        departure.speed = 0.0;
        manager.aircraft.push(departure);
        manager.tick(&mut state);
        assert_eq!(manager.aircraft[0].blocker, Some(0));
        assert!(manager.aircraft[0].blocked);
        assert_eq!(manager.aircraft[0].yield_to, Some(0));
    }

    #[test]
    fn a_stopped_player_is_not_a_right_of_way_participant() {
        let airport = openatc_core::airport::demo_airport();
        let mut state = crate::test_state();
        state.airport = Some(airport);
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 0.0006;
        state.session.telemetry.longitude = 0.0005;
        state.session.telemetry.heading_degrees = 270.0;
        state.session.telemetry.ground_speed_knots = 0.0;
        state.session.telemetry.traffic = Some(Vec::new());
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            active_airport: "DEMO".into(),
            ..Default::default()
        };
        let mut departure = ambient(AmbientKind::Departure, 0.0, 0.0, 2_000.0);
        departure.airport = "DEMO".into();
        departure.runway = "09".into();
        departure.hold_distance = 600.0;
        departure.runway_start = 1_000.0;
        departure.runway_end = 2_000.0;
        departure.points = vec![
            Waypoint {
                latitude: 0.0,
                longitude: 0.0,
                altitude_feet: 0.0,
            },
            Waypoint {
                latitude: 0.01,
                longitude: 0.0,
                altitude_feet: 0.0,
            },
        ];
        departure.cumulative = cumulative_distances(&departure.points);
        departure.distance = 0.0;
        departure.speed = 0.0;
        manager.aircraft.push(departure);
        manager.tick(&mut state);
        assert_eq!(manager.aircraft[0].yield_to, None);
    }

    #[test]
    fn the_held_player_is_recorded_while_traffic_waits_behind_on_the_route() {
        let airport = openatc_core::airport::demo_airport();
        let mut state = crate::test_state();
        state.airport = Some(airport);
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 0.01;
        state.session.telemetry.longitude = 0.0005;
        state.session.telemetry.traffic = Some(Vec::new());
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            active_airport: "DEMO".into(),
            ..Default::default()
        };
        let mut departure = ambient(AmbientKind::Departure, 0.0, 0.0, 2_000.0);
        departure.airport = "DEMO".into();
        departure.runway = "09".into();
        departure.hold_distance = 600.0;
        departure.runway_start = 1_000.0;
        departure.runway_end = 2_000.0;
        departure.points = vec![
            Waypoint {
                latitude: 0.01,
                longitude: 0.0,
                altitude_feet: 0.0,
            },
            Waypoint {
                latitude: 0.01,
                longitude: 0.01,
                altitude_feet: 0.0,
            },
        ];
        departure.cumulative = cumulative_distances(&departure.points);
        departure.distance = 0.0;
        departure.speed = 0.0;
        manager.aircraft.push(departure);
        manager.tick(&mut state);
        assert!(manager.aircraft[0].player_hold_since.is_some());
    }

    #[test]
    fn the_player_gives_way_to_crossing_traffic_on_its_right_until_it_clears() {
        let mut state = crate::test_state();
        state.session.plan.callsign = "BIL1975".into();
        state.session.telemetry.com1_khz = 121_900;
        state.stations = vec![openatc_core::stations::Station {
            airport: "DEMO".into(),
            service: "Ground".into(),
            khz: 121_900,
            ..Default::default()
        }];
        let mut manager = TrafficManager {
            active_airport: "DEMO".into(),
            ..Default::default()
        };
        let mut crossing = ambient(AmbientKind::Departure, 0.0, 0.0, 2_000.0);
        crossing.id = 7;
        crossing.airport = "DEMO".into();
        crossing.runway = "09".into();
        crossing.runway_start = 1_000.0;
        crossing.runway_end = 2_000.0;
        crossing.points = vec![
            Waypoint {
                latitude: 0.0012,
                longitude: 0.0005,
                altitude_feet: 0.0,
            },
            Waypoint {
                latitude: 0.0,
                longitude: 0.0005,
                altitude_feet: 0.0,
            },
        ];
        crossing.cumulative = cumulative_distances(&crossing.points);
        crossing.distance = 0.0;
        crossing.speed = 5.0;
        manager.aircraft.push(crossing);
        let moving = TrafficTarget {
            id: 0,
            callsign: "BIL1975".into(),
            latitude: 0.0006,
            longitude: 0.0005,
            track_degrees: 270.0,
            on_ground: true,
            ground_speed_knots: Some(10.0),
            ..Default::default()
        };
        manager.player_give_way(&mut state, std::slice::from_ref(&moving));
        assert!(
            state
                .session
                .transcript
                .iter()
                .any(|entry| entry.text.contains("give way to"))
        );
        assert_eq!(manager.player_yield, Some(7));
        let stopped = TrafficTarget {
            ground_speed_knots: Some(0.0),
            ..moving
        };
        manager.player_give_way(&mut state, std::slice::from_ref(&stopped));
        assert!(
            state
                .session
                .transcript
                .iter()
                .any(|entry| entry.text.contains("continue, clear of traffic"))
        );
        assert_eq!(manager.player_yield, None);
    }

    #[test]
    fn the_shared_exit_rule_skips_the_entry_and_a_blocked_queue() {
        use openatc_core::airport::{Runway, TaxiEdge, TaxiNode};
        use openatc_core::state::{Point, TrafficTarget};
        let point = |east: f64, north: f64| Point {
            east,
            north,
            height: 0.0,
        };
        let airport = Airport {
            icao: "TEST".into(),
            runways: vec![Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                first: point(0.0, 0.0),
                second: point(2_000.0, 0.0),
                width: 45.0,
                ..Default::default()
            }],
            nodes: vec![
                TaxiNode {
                    id: 1,
                    point: point(0.0, 0.0),
                },
                TaxiNode {
                    id: 2,
                    point: point(700.0, 0.0),
                },
                TaxiNode {
                    id: 3,
                    point: point(700.0, 80.0),
                },
                TaxiNode {
                    id: 4,
                    point: point(1_100.0, 0.0),
                },
                TaxiNode {
                    id: 5,
                    point: point(1_300.0, 80.0),
                },
                TaxiNode {
                    id: 6,
                    point: point(1_500.0, 0.0),
                },
                TaxiNode {
                    id: 7,
                    point: point(1_500.0, 80.0),
                },
            ],
            edges: vec![
                TaxiEdge {
                    first: 1,
                    second: 2,
                    name: "09".into(),
                    runway: true,
                    size: 'E',
                    ..Default::default()
                },
                TaxiEdge {
                    first: 2,
                    second: 3,
                    name: "N1".into(),
                    size: 'E',
                    ..Default::default()
                },
                TaxiEdge {
                    first: 4,
                    second: 5,
                    name: "N2".into(),
                    size: 'E',
                    ..Default::default()
                },
                TaxiEdge {
                    first: 6,
                    second: 7,
                    name: "N3".into(),
                    size: 'E',
                    ..Default::default()
                },
            ],
            taxi_hold_lines: vec![[point(680.0, 70.0), point(720.0, 70.0)]],
            ..Default::default()
        };
        let empty: Vec<TrafficTarget> = Vec::new();
        let plan = choose_arrival_exit(&airport, "09", 'E', &empty, 0.0, 20.0, 500.0, 99)
            .expect("an exit after the entry");
        assert_eq!(plan.chosen.taxiway, "N2");
        let blocked = TrafficTarget {
            id: 7,
            latitude: 0.0,
            longitude: 1_200.0 / 111_320.0,
            on_ground: true,
            ground_speed_knots: Some(0.0),
            ..Default::default()
        };
        let plan = choose_arrival_exit(&airport, "09", 'E', &[blocked], 0.0, 20.0, 500.0, 99)
            .expect("the next clear exit");
        assert_eq!(plan.chosen.taxiway, "N3");
        assert!(
            choose_arrival_exit(&airport, "09", 'E', &empty, 1_450.0, 60.0, 0.0, 99).is_none(),
            "an aircraft too fast for the remaining exit goes around"
        );
    }

    #[test]
    fn ambient_departures_use_the_player_taxi_planner() {
        let airport = openatc_core::airport::demo_airport();
        let cos = (airport.reference_latitude.to_radians().cos()).max(0.01);
        let planned = airport.nodes.iter().find_map(|node| {
            let telemetry = openatc_core::state::Telemetry {
                latitude: airport.reference_latitude + node.point.north / 111_320.0,
                longitude: airport.reference_longitude + node.point.east / (111_320.0 * cos),
                position_valid: true,
                on_ground: true,
                ..Default::default()
            };
            openatc_core::airport::calculate_taxi_route(&airport, &telemetry, "09", false, 'C')
                .ok()
                .filter(|route| route.points.len() > 1)
                .map(|route| (node.id, route))
        });
        let Some((start, player)) = planned else {
            return;
        };
        let (points, via) = plan_taxi(&airport, start, "09", 'C', None, None)
            .expect("the ambient route uses the same planner");
        assert_eq!(points, player.points);
        assert_eq!(via, player.via);
    }

    #[test]
    fn a_cleared_departure_wins_an_overlap_with_an_uncleared_one() {
        let airport = openatc_core::airport::demo_airport();
        let mut state = crate::test_state();
        state.airport = Some(airport);
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 0.1;
        state.session.telemetry.longitude = 0.1;
        state.session.telemetry.traffic = Some(Vec::new());
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            active_airport: "DEMO".into(),
            ..Default::default()
        };
        let mut cleared = ambient(AmbientKind::Departure, 0.0, 0.0, 2_000.0);
        cleared.id = 10;
        cleared.airport = "DEMO".into();
        cleared.runway = "09".into();
        cleared.hold_distance = 600.0;
        cleared.runway_start = 1_000.0;
        cleared.runway_end = 2_000.0;
        cleared.cleared = true;
        let mut waiting = ambient(AmbientKind::Departure, 0.0, 0.0, 2_000.0);
        waiting.id = 11;
        waiting.airport = "DEMO".into();
        waiting.runway = "09".into();
        waiting.hold_distance = 540.0;
        waiting.runway_start = 1_000.0;
        waiting.runway_end = 2_000.0;
        waiting.cleared = false;
        manager.aircraft = vec![cleared, waiting];
        manager.tick(&mut state);
        assert_ne!(
            manager.aircraft[0].blocker,
            Some(11),
            "the uncleared aircraft must not block the one cleared onto the runway"
        );
        assert_eq!(
            manager.aircraft[1].blocker,
            Some(10),
            "the uncleared aircraft waits for the cleared one"
        );
    }

    #[test]
    fn ambient_traffic_freezes_while_the_simulator_is_paused() {
        let airport = openatc_core::airport::demo_airport();
        let mut state = crate::test_state();
        state.airport = Some(airport);
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.paused = true;
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            ..Default::default()
        };
        let mut departure = ambient(AmbientKind::Departure, 0.0, 0.0, 2_000.0);
        departure.airport = "DEMO".into();
        departure.runway = "09".into();
        departure.distance = 100.0;
        departure.speed = 5.0;
        manager.aircraft.push(departure);
        let before = manager.aircraft[0].distance;
        manager.tick(&mut state);
        assert_eq!(
            manager.aircraft[0].distance, before,
            "a pause must not advance ambient traffic"
        );
    }

    #[test]
    fn ground_calls_stay_silent_when_the_player_is_not_on_the_frequency() {
        let mut state = crate::test_state();
        state.session.plan.callsign = "BIL1975".into();
        state.session.telemetry.com1_khz = 118_700;
        state.stations = vec![
            openatc_core::stations::Station {
                airport: "DEMO".into(),
                name: "DEMO Ground".into(),
                service: "Ground".into(),
                khz: 121_900,
                ..Default::default()
            },
            openatc_core::stations::Station {
                airport: "DEMO".into(),
                name: "DEMO Tower".into(),
                service: "Tower".into(),
                khz: 118_700,
                ..Default::default()
            },
        ];
        let mut manager = TrafficManager {
            active_airport: "DEMO".into(),
            ..Default::default()
        };
        let mut waiting = ambient(AmbientKind::Departure, 0.0, 0.0, 2_000.0);
        waiting.airport = "DEMO".into();
        waiting.frequency_khz = Some(118_700);
        waiting.yield_to = Some(0);
        manager.aircraft.push(waiting);
        let transcript = state.session.transcript.len();
        manager.ground_calls(&mut state);
        assert_eq!(state.session.transcript.len(), transcript);
        assert!(!manager.aircraft[0].yield_announced);
    }

    #[test]
    fn a_give_way_is_called_once_on_frequency_and_then_released() {
        let mut state = crate::test_state();
        state.session.plan.callsign = "BIL1975".into();
        state.session.telemetry.com1_khz = 121_900;
        state.stations = vec![
            openatc_core::stations::Station {
                airport: "DEMO".into(),
                name: "DEMO Ground".into(),
                service: "Ground".into(),
                khz: 121_900,
                ..Default::default()
            },
            openatc_core::stations::Station {
                airport: "DEMO".into(),
                name: "DEMO Tower".into(),
                service: "Tower".into(),
                khz: 118_700,
                ..Default::default()
            },
        ];
        let mut manager = TrafficManager {
            active_airport: "DEMO".into(),
            ..Default::default()
        };
        let mut waiting = ambient(AmbientKind::Departure, 0.0, 0.0, 2_000.0);
        waiting.airport = "DEMO".into();
        waiting.frequency_khz = Some(118_700);
        waiting.yield_to = Some(0);
        manager.aircraft.push(waiting);
        manager.ground_calls(&mut state);
        let call = state
            .session
            .transcript
            .iter()
            .find(|entry| entry.text.contains("give way to BIL1975"))
            .expect("the give-way is spoken");
        assert_eq!(call.frequency, Some(121_900));
        assert_eq!(call.position, "DEMO Ground");
        assert!(manager.aircraft[0].yield_announced);
        manager.ground_calls(&mut state);
        let calls = state
            .session
            .transcript
            .iter()
            .filter(|entry| entry.text.contains("give way to BIL1975"))
            .count();
        assert_eq!(calls, 1, "a give-way is announced once per yield");
        manager.aircraft[0].yield_to = None;
        manager.ground_calls(&mut state);
        assert!(
            state
                .session
                .transcript
                .iter()
                .any(|entry| entry.text.contains("continue"))
        );
        assert!(!manager.aircraft[0].yield_announced);
    }

    #[test]
    fn a_tower_only_field_uses_the_tower_channel_and_skips_the_multicast_entry() {
        let mut state = crate::test_state();
        state.session.plan.callsign = "BIL1975".into();
        state.session.telemetry.com1_khz = 118_700;
        state.stations = vec![
            openatc_core::stations::Station {
                airport: "DEMO".into(),
                name: "DEMO MULT".into(),
                service: "Unicom".into(),
                khz: 118_700,
                ..Default::default()
            },
            openatc_core::stations::Station {
                airport: "DEMO".into(),
                name: "DEMO Tower".into(),
                service: "Tower".into(),
                khz: 118_700,
                ..Default::default()
            },
        ];
        let mut manager = TrafficManager {
            active_airport: "DEMO".into(),
            ..Default::default()
        };
        let mut waiting = ambient(AmbientKind::Departure, 0.0, 0.0, 2_000.0);
        waiting.airport = "DEMO".into();
        waiting.frequency_khz = Some(118_700);
        waiting.yield_to = Some(0);
        manager.aircraft.push(waiting);
        manager.ground_calls(&mut state);
        let call = state
            .session
            .transcript
            .iter()
            .find(|entry| entry.text.contains("give way to BIL1975"))
            .expect("the give-way is spoken");
        assert_eq!(call.frequency, Some(118_700));
        assert_eq!(call.position, "DEMO Tower");
    }

    #[test]
    fn a_held_player_is_asked_to_keep_taxiing_once() {
        let mut state = crate::test_state();
        state.session.plan.callsign = "BIL1975".into();
        state.session.telemetry.com1_khz = 121_900;
        state.session.telemetry.ground_speed_knots = 0.0;
        state.stations = vec![openatc_core::stations::Station {
            airport: "DEMO".into(),
            service: "Ground".into(),
            khz: 121_900,
            ..Default::default()
        }];
        let mut manager = TrafficManager {
            active_airport: "DEMO".into(),
            ..Default::default()
        };
        let mut waiting = ambient(AmbientKind::Departure, 0.0, 0.0, 2_000.0);
        waiting.airport = "DEMO".into();
        waiting.player_hold_since = Some(Instant::now() - Duration::from_secs(30));
        manager.aircraft.push(waiting);
        manager.ground_calls(&mut state);
        assert!(
            state
                .session
                .transcript
                .iter()
                .any(|entry| entry.text.contains("traffic is waiting behind you"))
        );
        let asked = state.session.transcript.len();
        manager.ground_calls(&mut state);
        assert_eq!(
            state.session.transcript.len(),
            asked,
            "not asked again straight away"
        );
    }

    #[test]
    fn an_ambient_lead_with_a_clear_runway_is_cleared_in_one_tick() {
        let airport = openatc_core::airport::demo_airport();
        let mut state = crate::test_state();
        state.airport = Some(airport);
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 0.02;
        state.session.telemetry.longitude = 0.02;
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            active_airport: "DEMO".into(),
            ..Default::default()
        };
        let mut departure = ambient(AmbientKind::Departure, 600.0, 0.0, 2_000.0);
        departure.airport = "DEMO".into();
        departure.runway = "09".into();
        departure.hold_distance = 600.0;
        departure.distance = 600.0;
        departure.runway_start = 1_000.0;
        departure.runway_end = 2_000.0;
        departure.cleared = false;
        departure.takeoff = false;
        departure.frequency_khz = None;
        manager.aircraft.push(departure);
        manager.tick(&mut state);
        assert!(
            manager.aircraft[0].cleared,
            "the queue lead must be cleared"
        );
        assert!(
            manager.aircraft[0].takeoff,
            "a clear runway must produce takeoff clearance, not line up and wait"
        );
    }

    #[test]
    fn a_final_approach_holds_a_line_up_behind_a_rolling_departure() {
        let airport = openatc_core::airport::demo_airport();
        let mut manager = TrafficManager::default();
        let mut rolling = ambient(AmbientKind::Departure, 700.0, 0.0, 2_000.0);
        rolling.airport = "DEMO".into();
        rolling.runway = "09".into();
        rolling.hold_distance = 0.0;
        rolling.runway_start = 600.0;
        rolling.runway_end = 2_000.0;
        rolling.cleared = true;
        rolling.takeoff = true;
        rolling.distance = 700.0;
        manager.aircraft.push(rolling);

        let mut state = crate::test_state();
        state.airport = Some(airport.clone());
        state.ambient = manager;
        state.session.phase = openatc_core::state::PhaseCode::Taxi;
        state.session.plan.runway = "09".into();
        state.session.taxi_clearance.approved = true;
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;

        let sequence = crate::routes::runway_sequence(&state, &airport, "09");
        assert_eq!(sequence, openatc_core::apply::RunwaySequence::Rolling);
        let targets = state.traffic_targets();
        assert_eq!(
            openatc_core::apply::takeoff_decision(
                &airport,
                "09",
                targets.iter(),
                sequence,
                state.session.departure_wake_seconds,
            ),
            openatc_core::apply::TakeoffDecision::LineUpAndWait
        );

        state.session.telemetry.traffic = Some(vec![TrafficTarget {
            latitude: 0.0,
            longitude: -0.044,
            altitude_feet: 1_200.0,
            track_degrees: 90.0,
            ground_speed_knots: Some(140.0),
            on_ground: false,
            ..Default::default()
        }]);
        let targets = state.traffic_targets();
        assert_eq!(
            openatc_core::apply::takeoff_decision(
                &airport,
                "09",
                targets.iter(),
                sequence,
                state.session.departure_wake_seconds,
            ),
            openatc_core::apply::TakeoffDecision::Hold("flight_hold_short_landing"),
            "a short-final arrival must prevent line-up even behind a rolling departure"
        );
    }

    #[test]
    fn the_player_at_the_hold_first_is_not_displaced_by_an_ambient_leader() {
        let now = Instant::now();
        let mut ambient = ambient(AmbientKind::Departure, 600.0, 0.0, 1_000.0);
        ambient.id = 9;
        ambient.hold_distance = 600.0;
        ambient.distance = 600.0;
        ambient.hold_since = Some(now - Duration::from_secs(30));
        let aircraft = vec![ambient];
        assert_eq!(queue_leader(&aircraft, None), Some(9));
        assert_eq!(
            queue_leader(&aircraft, Some(now - Duration::from_secs(60))),
            None,
            "the player was at the hold first"
        );
        assert_eq!(
            queue_leader(&aircraft, Some(now - Duration::from_secs(10))),
            Some(9),
            "the ambient aircraft was at the hold first"
        );
    }

    #[test]
    fn the_departure_queue_is_first_in_first_out() {
        let earlier = Instant::now() - Duration::from_secs(30);
        let later = Instant::now() - Duration::from_secs(10);
        let mut first = ambient(AmbientKind::Departure, 600.0, 0.0, 1_000.0);
        first.id = 9;
        first.hold_distance = 600.0;
        first.distance = 600.0;
        first.cleared = false;
        first.hold_since = Some(earlier);
        let mut second = ambient(AmbientKind::Departure, 600.0, 0.0, 1_000.0);
        second.id = 3;
        second.hold_distance = 600.0;
        second.distance = 600.0;
        second.cleared = false;
        second.hold_since = Some(later);
        assert_eq!(queue_leader(&[first, second], None), Some(9));
        let mut cleared = ambient(AmbientKind::Departure, 600.0, 0.0, 1_000.0);
        cleared.id = 9;
        cleared.hold_distance = 600.0;
        cleared.distance = 600.0;
        cleared.cleared = true;
        let mut next = ambient(AmbientKind::Departure, 600.0, 0.0, 1_000.0);
        next.id = 3;
        next.hold_distance = 600.0;
        next.distance = 600.0;
        next.cleared = false;
        next.hold_since = Some(later);
        assert_eq!(
            queue_leader(&[cleared, next], None),
            Some(9),
            "a lined-up aircraft remains the queue leader until it starts to roll"
        );
        let mut rolling = ambient(AmbientKind::Departure, 600.0, 0.0, 1_000.0);
        rolling.id = 9;
        rolling.hold_distance = 600.0;
        rolling.distance = 600.0;
        rolling.cleared = true;
        rolling.takeoff = true;
        let mut next = ambient(AmbientKind::Departure, 600.0, 0.0, 1_000.0);
        next.id = 3;
        next.hold_distance = 600.0;
        next.distance = 600.0;
        next.hold_since = Some(later);
        assert_eq!(queue_leader(&[rolling, next], None), Some(3));
    }

    #[test]
    fn departures_are_ordered_by_distance_to_the_hold() {
        let hold = [-35.319_982_6, 149.193_047_4];
        let player = [-35.319_464_3, 149.192_489];
        let mut leader = ambient(AmbientKind::Departure, 3250.4, 0.0, 5_000.0);
        leader.runway = "35".into();
        leader.hold_distance = 3_249.96;
        leader.runway_start = 3_653.9;
        leader.hold = Some(hold);
        assert!(
            !departure_behind_player(&leader, player, "35", 283.0),
            "a jet at the hold is ahead even when the player is closer to the threshold"
        );
        assert!(!departure_behind_player(&leader, hold, "35", 0.0));
        assert!(!departure_behind_player(&leader, player, "17", 283.0));
        let mut behind = ambient(AmbientKind::Departure, 3049.96, 0.0, 5_000.0);
        behind.runway = "35".into();
        behind.hold_distance = 3_249.96;
        behind.runway_start = 3_653.9;
        behind.hold = Some(hold);
        assert!(
            departure_behind_player(&behind, hold, "35", 0.0),
            "a jet still 200 m before the hold is behind a player at the hold"
        );
        behind.hold = None;
        assert!(
            departure_behind_player(&behind, hold, "35", 0.0),
            "without a holding marker the runway start and threshold are compared"
        );
    }

    #[test]
    fn an_ambient_at_the_hold_ahead_of_the_player_blocks_the_player() {
        let airport = openatc_core::airport::demo_airport();
        let mut manager = TrafficManager::default();
        let mut jet = ambient(AmbientKind::Departure, 600.0, 0.0, 2_000.0);
        jet.airport.clone_from(&airport.icao);
        jet.runway = "09".into();
        jet.hold_distance = 600.0;
        jet.hold = Some([0.003, 0.0]);
        jet.cleared = false;
        manager.aircraft.push(jet);
        let player = openatc_core::state::Telemetry {
            position_valid: true,
            on_ground: true,
            latitude: 0.003 - 27.0 / 111_320.0,
            longitude: 0.0,
            ..Default::default()
        };
        assert!(
            manager.departure_ahead_of_player(&airport, "09", &player, None),
            "a jet at the hold is ahead of a player 27 m before it"
        );
        assert!(!manager.departure_ahead_of_player(&airport, "27", &player, None));
        let on_runway = openatc_core::state::Telemetry {
            latitude: 0.0,
            ..player.clone()
        };
        assert!(
            !manager.departure_ahead_of_player(&airport, "09", &on_runway, None),
            "a player already on the runway is ahead of the hold"
        );
        manager.aircraft[0].distance = 550.0;
        assert!(
            !manager.departure_ahead_of_player(&airport, "09", &player, None),
            "a jet 50 m behind the hold is not ahead of a player at the hold"
        );
        manager.aircraft[0].distance = 600.0;
        let now = Instant::now();
        manager.aircraft[0].hold_since = Some(now - Duration::from_secs(30));
        assert!(
            !manager.departure_ahead_of_player(
                &airport,
                "09",
                &player,
                Some(now - Duration::from_secs(60)),
            ),
            "a player at the hold first does not yield to a later arrival"
        );
        assert!(
            manager.departure_ahead_of_player(
                &airport,
                "09",
                &player,
                Some(now - Duration::from_secs(10)),
            ),
            "an ambient aircraft at the hold first is ahead"
        );
        manager.aircraft[0].cleared = true;
        assert!(
            !manager.departure_ahead_of_player(&airport, "09", &player, None),
            "a cleared aircraft is handled by the runway state"
        );
    }

    #[test]
    fn a_jet_at_the_hold_is_cleared_ahead_of_a_player_taxiing_toward_it() {
        let mut state = crate::test_state();
        state.airport = Some(openatc_core::airport::demo_airport());
        state.session.phase = openatc_core::state::PhaseCode::Departure;
        state.session.plan.runway = "09".into();
        state.session.taxi_clearance.takeoff_state = openatc_core::state::TakeoffState::Held;
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 0.0;
        state.session.telemetry.longitude = -1_300.0 / 111_320.0;
        state.session.telemetry.traffic = Some(Vec::new());
        let player_longitude = state.session.telemetry.longitude;
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            active_airport: "DEMO".into(),
            movement_started: true,
            ..Default::default()
        };
        let mut jet = ambient(AmbientKind::Departure, 600.0, 0.0, 2_000.0);
        jet.id = 1;
        jet.airport = "DEMO".into();
        jet.runway = "09".into();
        jet.hold_distance = 600.0;
        jet.distance = 600.0;
        jet.runway_start = 1_000.0;
        jet.runway_end = 2_000.0;
        jet.hold = Some([0.0, player_longitude + 78.0 / 111_320.0]);
        jet.hold_since = Some(Instant::now() - Duration::from_secs(30));
        jet.cleared = false;
        jet.takeoff = false;
        manager.aircraft.push(jet);
        manager.tick(&mut state);
        let jet = manager
            .aircraft
            .iter()
            .find(|aircraft| aircraft.id == 1)
            .expect("the jet stays in the traffic set");
        assert!(
            jet.cleared,
            "the jet at the hold leads the queue, so it is cleared before the player"
        );
        assert_eq!(
            manager.runway_state_for("DEMO", "09", None, None, None),
            RunwayState::LineUp,
            "a cleared jet holds the runway for a player who asks next"
        );
    }

    #[test]
    fn spawn_node_skips_nodes_without_a_surface_route() {
        let airport = openatc_core::airport::demo_airport();
        let first = spawn_node(
            &airport,
            'C',
            &BTreeSet::new(),
            &BTreeSet::new(),
            &[],
            false,
            7,
        )
        .expect("initial taxi node");
        let rejected = BTreeSet::from([('C', first.id)]);
        let next = spawn_node(&airport, 'C', &BTreeSet::new(), &rejected, &[], false, 7);
        assert_ne!(next.map(|node| node.id), Some(first.id));
    }

    #[test]
    fn density_target_follows_the_traffic_settings() {
        let mut state = crate::test_state();
        state.airport = Some(openatc_core::airport::demo_airport());
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 0.02;
        state.session.telemetry.longitude = 0.02;
        state.settings.traffic_taxi_min = 7;
        state.settings.traffic_taxi_max = 9;
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            ..Default::default()
        };
        manager.tick(&mut state);
        assert!((7..=9).contains(&manager.density_target));
        let first = manager.density_target;
        manager.tick(&mut state);
        assert_eq!(
            manager.density_target, first,
            "the density target is stable"
        );

        state.settings.traffic_taxi_min = 11;
        state.settings.traffic_taxi_max = 12;
        manager.tick(&mut state);
        assert!((11..=12).contains(&manager.density_target));
    }

    #[test]
    fn only_one_ambient_departure_holds_a_line_up_clearance() {
        let airport = openatc_core::airport::demo_airport();
        let mut state = crate::test_state();
        state.airport = Some(airport);
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 0.02;
        state.session.telemetry.longitude = 0.02;
        state.session.telemetry.traffic = Some(Vec::new());
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            active_airport: "DEMO".into(),
            movement_started: true,
            ..Default::default()
        };
        for (id, hold_since) in [
            (1, Instant::now() - Duration::from_secs(30)),
            (2, Instant::now() - Duration::from_secs(20)),
            (3, Instant::now() - Duration::from_secs(10)),
        ] {
            let mut departure = ambient(AmbientKind::Departure, 600.0, 0.0, 2_000.0);
            departure.id = id;
            departure.airport = "DEMO".into();
            departure.runway = "09".into();
            departure.hold_distance = 600.0;
            departure.distance = 600.0;
            departure.runway_start = 1_000.0;
            departure.runway_end = 2_000.0;
            departure.hold_since = Some(hold_since);
            departure.frequency_khz = None;
            departure.cleared = false;
            departure.takeoff = false;
            manager.aircraft.push(departure);
        }
        manager.tick(&mut state);
        assert_eq!(
            manager
                .aircraft
                .iter()
                .filter(|aircraft| aircraft.cleared && !aircraft.takeoff)
                .count(),
            0,
            "a clear runway clears the leader for takeoff, not line up"
        );
        for _ in 0..5 {
            manager.tick(&mut state);
            assert!(
                manager
                    .aircraft
                    .iter()
                    .filter(|aircraft| aircraft.cleared && !aircraft.takeoff)
                    .count()
                    <= 1,
                "only one aircraft may hold a line-up clearance"
            );
        }
    }

    #[test]
    fn a_rolling_departure_allows_one_follower_to_line_up() {
        let airport = openatc_core::airport::demo_airport();
        let mut state = crate::test_state();
        state.airport = Some(airport);
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 0.02;
        state.session.telemetry.longitude = 0.02;
        state.session.telemetry.traffic = Some(Vec::new());
        let mut manager = TrafficManager {
            models: Some(Arc::new(Catalogue::default())),
            active_airport: "DEMO".into(),
            movement_started: true,
            ..Default::default()
        };
        let mut leader = ambient(AmbientKind::Departure, 1_400.0, 0.0, 2_000.0);
        leader.id = 1;
        leader.airport = "DEMO".into();
        leader.runway = "09".into();
        leader.hold_distance = 600.0;
        leader.distance = 1_050.0;
        leader.runway_start = 700.0;
        leader.runway_end = 1_100.0;
        leader.cleared = true;
        leader.takeoff = true;
        leader.frequency_khz = None;
        manager.aircraft.push(leader);
        for id in [2, 3] {
            let mut follower = ambient(AmbientKind::Departure, 600.0, 0.0, 2_000.0);
            follower.id = id;
            follower.airport = "DEMO".into();
            follower.runway = "09".into();
            follower.hold_distance = 600.0;
            follower.distance = 600.0;
            follower.runway_start = 700.0;
            follower.runway_end = 1_100.0;
            follower.frequency_khz = None;
            follower.cleared = false;
            follower.takeoff = false;
            manager.aircraft.push(follower);
        }
        manager.tick(&mut state);
        assert!(manager.aircraft[0].takeoff);
        assert!(
            manager.aircraft[1].cleared && !manager.aircraft[1].takeoff,
            "the next aircraft should line up behind the rolling departure"
        );
        assert!(
            !manager.aircraft[2].cleared,
            "the third aircraft must remain at the hold"
        );
    }

    #[test]
    fn ambient_stops_for_another_aircraft_in_the_same_hold_box() {
        let aircraft = ambient(AmbientKind::Departure, 0.0, 0.0, 900.0);
        let ahead = TrafficTarget {
            id: 2,
            callsign: "TRAFFIC2".into(),
            aircraft: "A320".into(),
            latitude: 0.0,
            longitude: 0.0002,
            track_degrees: 90.0,
            on_ground: true,
            ..Default::default()
        };
        assert!(separation::ahead_conflict(
            0.0,
            0.0,
            90.0,
            aircraft.profile.length,
            1,
            &[ahead.clone()],
        ));
        let behind = TrafficTarget {
            longitude: -0.0002,
            ..ahead
        };
        assert!(!separation::ahead_conflict(
            0.0,
            0.0,
            90.0,
            aircraft.profile.length,
            1,
            &[behind],
        ));
    }

    #[test]
    fn a_taxi_corner_heading_turns_through_the_corner() {
        let mut aircraft = ambient(AmbientKind::Departure, 0.0, 0.0, 10_000.0);
        aircraft.points = vec![
            Waypoint {
                latitude: 0.0,
                longitude: 0.0,
                altitude_feet: 0.0,
            },
            Waypoint {
                latitude: 0.0,
                longitude: 0.001,
                altitude_feet: 0.0,
            },
            Waypoint {
                latitude: 0.001,
                longitude: 0.001,
                altitude_feet: 0.0,
            },
        ];
        aircraft.cumulative = cumulative_distances(&aircraft.points);
        aircraft.distance = aircraft.cumulative[1];
        let frame = ambient_frame(&aircraft).expect("frame");
        assert!(
            frame.target.track_degrees > 5.0 && frame.target.track_degrees < 85.0,
            "corner heading {}",
            frame.target.track_degrees
        );
    }

    #[test]
    fn ambient_stops_for_a_player_ahead_on_its_route() {
        let airport = openatc_core::airport::demo_airport();
        let mut aircraft = ambient(AmbientKind::Departure, 0.0, 0.0, 900.0);
        aircraft.distance = 0.0;
        let ahead = TrafficTarget {
            id: 0,
            latitude: 0.0,
            longitude: 0.0005,
            on_ground: true,
            ..Default::default()
        };
        let gap = phases::ground::player_gap_ahead(
            &aircraft.points,
            &aircraft.cumulative,
            aircraft.distance,
            &airport,
            &ahead,
            aircraft.profile.length,
        );
        assert!(gap.is_some_and(|gap| gap > 0.0));
        let behind = TrafficTarget {
            longitude: -0.0005,
            ..ahead
        };
        assert!(
            phases::ground::player_gap_ahead(
                &aircraft.points,
                &aircraft.cumulative,
                aircraft.distance,
                &airport,
                &behind,
                aircraft.profile.length,
            )
            .is_none()
        );
    }

    #[test]
    fn departure_stages_follow_the_runway() {
        let mut aircraft = ambient(AmbientKind::Departure, 100.0, 0.0, 900.0);
        aircraft.runway_start = 700.0;
        aircraft.hold_distance = 600.0;
        aircraft.cleared = false;
        assert_eq!(aircraft.stage(), Stage::TaxiOut);
        aircraft.cleared = true;
        aircraft.distance = 650.0;
        assert_eq!(aircraft.stage(), Stage::LineUp);
        aircraft.takeoff = true;
        aircraft.distance = 750.0;
        assert_eq!(aircraft.stage(), Stage::Takeoff);
        aircraft.distance = 950.0;
        assert_eq!(aircraft.stage(), Stage::Climb);
    }

    #[test]
    fn takeoff_roll_reaches_rotation_at_the_handling_rate() {
        let handling = jet_handling();
        let taxi = TAXI_STRAIGHT_METERS_PER_SECOND;
        let rotation = handling.rotation_knots * 0.514_444;
        let start = 800.0;
        let rotation_distance = start
            + (rotation.powi(2) - taxi.powi(2))
                / (2.0 * handling.acceleration_knots_sec * 0.514_444);
        assert!(
            (phases::takeoff::roll_speed(&handling, start, start, rotation_distance, taxi) - taxi)
                .abs()
                < 0.01
        );
        assert!(
            (phases::takeoff::roll_speed(
                &handling,
                rotation_distance,
                start,
                rotation_distance,
                taxi
            ) - rotation)
                .abs()
                < 0.01
        );
        let later =
            phases::takeoff::roll_speed(&handling, start + 600.0, start, rotation_distance, taxi);
        let rate = (later.powi(2) - taxi.powi(2)) / (2.0 * 600.0);
        assert!((rate - handling.acceleration_knots_sec * 0.514_444).abs() < 0.001);
        assert!(
            phases::takeoff::roll_speed(
                &handling,
                rotation_distance + 500.0,
                start,
                rotation_distance,
                taxi
            ) <= rotation
        );
        // A short runway accelerates harder but still reaches Vr at rotation.
        let short_end = start + 600.0;
        assert!(
            (phases::takeoff::roll_speed(&handling, short_end, start, short_end, taxi) - rotation)
                .abs()
                < 0.01
        );
    }

    #[test]
    fn landing_rollout_uses_the_rate_needed_to_reach_the_exit() {
        let handling = jet_handling();
        let taxi = TAXI_STRAIGHT_METERS_PER_SECOND;
        let touchdown_speed = 70.0;
        let touchdown = 1_000.0;
        let long_exit = 4_000.0;
        let at_touchdown = phases::landing::rollout_speed(
            &handling,
            touchdown,
            touchdown,
            long_exit,
            taxi,
            touchdown_speed,
        );
        assert!((at_touchdown - touchdown_speed).abs() < 0.01);
        let later = phases::landing::rollout_speed(
            &handling,
            touchdown + 500.0,
            touchdown,
            long_exit,
            taxi,
            touchdown_speed,
        );
        let rate = (at_touchdown.powi(2) - later.powi(2)) / (2.0 * 500.0);
        let required = (touchdown_speed.powi(2) - taxi.powi(2)) / (2.0 * (long_exit - touchdown));
        assert!((rate - required).abs() < 0.01);
        assert!(rate < handling.braking_knots_sec * 0.514_444);
        let short_exit = 3_000.0;
        let at_exit = phases::landing::rollout_speed(
            &handling,
            short_exit,
            touchdown,
            short_exit,
            taxi,
            touchdown_speed,
        );
        assert!((at_exit - taxi).abs() < 0.1);
        assert!(
            phases::landing::rollout_speed(
                &handling,
                short_exit + 200.0,
                touchdown,
                short_exit,
                taxi,
                touchdown_speed,
            ) >= taxi
        );
        let too_short = 1_400.0;
        assert!(
            phases::landing::rollout_speed(
                &handling,
                too_short,
                touchdown,
                too_short,
                taxi,
                touchdown_speed,
            ) > taxi,
            "an exit shorter than the stopping distance cannot reach taxi speed"
        );
    }

    #[test]
    fn arrival_stages_follow_the_runway() {
        let mut aircraft = ambient(AmbientKind::Arrival, 100.0, 500.0, 2_000.0);
        aircraft.runway_end = 1_200.0;
        assert_eq!(aircraft.stage(), Stage::Approach);
        aircraft.distance = 700.0;
        assert_eq!(aircraft.stage(), Stage::Rollout);
        aircraft.distance = 1_600.0;
        assert_eq!(aircraft.stage(), Stage::TaxiIn);
    }

    #[test]
    fn departure_occupies_the_runway_from_the_threshold() {
        let mut aircraft = ambient(AmbientKind::Departure, 650.0, 0.0, 900.0);
        aircraft.runway_start = 700.0;
        assert!(!aircraft.on_runway());
        aircraft.distance = 700.0;
        assert!(aircraft.on_runway());
    }

    #[test]
    fn landing_lights_follow_runway_phases() {
        for stage in [
            Stage::Crossing,
            Stage::Takeoff,
            Stage::Climb,
            Stage::Approach,
            Stage::MissedApproach,
            Stage::Rollout,
        ] {
            assert!(stage.landing_lights(), "{stage:?} needs landing lights");
        }
        for stage in [
            Stage::LineUp,
            Stage::TaxiOut,
            Stage::TaxiIn,
            Stage::Cruise,
            Stage::Parked,
        ] {
            assert!(
                !stage.landing_lights(),
                "{stage:?} keeps landing lights off"
            );
        }
    }

    #[test]
    fn line_up_extends_flaps_and_takeoff_switches_landing_lights_on() {
        let mut animation = Animation::default();
        let line_up = animation::Input {
            stage: Stage::LineUp,
            knots: 10.0,
            turn_degrees_sec: 0.0,
            vertical_fpm: 0.0,
            engines_running: true,
            fixed_gear: false,
            landing_lights: Stage::LineUp.landing_lights(),
        };
        for _ in 0..30 {
            animation.advance(line_up, &jet_handling(), 0.5);
        }
        assert!(animation.flaps > 0.0);
        assert!(!animation.landing_lights);
        assert!(animation.strobes);
        assert_eq!(animation.gear, 1.0);
        let takeoff = animation::Input {
            stage: Stage::Takeoff,
            landing_lights: Stage::Takeoff.landing_lights(),
            ..line_up
        };
        animation.advance(takeoff, &jet_handling(), 0.5);
        assert!(animation.landing_lights);
    }

    #[test]
    fn climb_retracts_gear_after_rotation() {
        let mut animation = Animation::default();
        let input = animation::Input {
            stage: Stage::Climb,
            knots: 160.0,
            turn_degrees_sec: 0.0,
            vertical_fpm: 1_800.0,
            engines_running: true,
            fixed_gear: false,
            landing_lights: Stage::Climb.landing_lights(),
        };
        for _ in 0..40 {
            animation.advance(input, &jet_handling(), 0.5);
        }
        assert!(animation.gear < 0.1);
        assert!(animation.landing_lights);
        assert!(animation.beacon);
        assert!(animation.navigation_lights);
    }
}
