//! Persistent ambient surface departures.
//!
//! This layer is deliberately independent of the full-flight scene publisher. It provides the
//! opening airport with taxi-out traffic that already has a departure route and can take off
//! without waiting for a SID/STAR/arrival pair.

use openatc_core::airport::Airport;
use openatc_core::state::{GeneratedTraffic, Point, TrafficDebug, TrafficTarget};
use openatc_core::traffic::animation::{self, Animation, Handling, Kind};
use openatc_core::traffic::csl::{Catalogue, Model};
use openatc_core::traffic::motion::{Stage, Waypoint};
use openatc_core::traffic::profiles::{Aircraft as Profile, Airline, Category, Profiles};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(test)]
const TAXI_STRAIGHT_METERS_PER_SECOND: f64 = 25.0 * 0.514_444;

/// Speed through the takeoff roll. A short runway borrows extra acceleration so
/// Vr is still met at the rotation point.
fn takeoff_roll_speed(
    handling: &Handling,
    distance: f64,
    runway_start: f64,
    rotation_end: f64,
    taxi: f64,
) -> f64 {
    let rotation = handling.rotation_knots * 0.514_444;
    let roll = (rotation_end - runway_start).max(1.0);
    let required = (rotation.powi(2) - taxi.powi(2)).max(0.0) / (2.0 * roll);
    let acceleration = required.max(handling.acceleration_knots_sec * 0.514_444);
    let rolled = (distance - runway_start).max(0.0);
    (taxi.powi(2) + 2.0 * acceleration * rolled)
        .sqrt()
        .min(rotation)
}

/// Speed through the landing roll, braking harder only when the exit is close.
fn landing_rollout_speed(
    handling: &Handling,
    distance: f64,
    ground_start: f64,
    runway_end: f64,
    taxi: f64,
    touchdown: f64,
) -> f64 {
    let roll = (runway_end - ground_start).max(1.0);
    let required = ((touchdown.powi(2) - taxi.powi(2)).max(0.0) / (2.0 * roll)).max(0.1);
    let braking = (handling.braking_knots_sec * 0.514_444).max(required);
    let rolled = (distance - ground_start).max(0.0);
    (touchdown.powi(2) - 2.0 * braking * rolled)
        .max(taxi.powi(2))
        .sqrt()
}

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

fn climb_speed(profile: &Profile, handling: &Handling) -> f64 {
    (profile.approach_knots + 10.0).max(handling.rotation_knots + 10.0) * 0.514_444
}

fn approach_speed(profile: &Profile) -> f64 {
    profile.approach_knots * 0.514_444
}

fn touchdown_speed(profile: &Profile) -> f64 {
    (profile.approach_knots - 5.0).max(45.0) * 0.514_444
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
) -> Vec<(u64, AmbientCandidate)> {
    let hub = use_hub.then(|| profiles.hubs.get(airport)).flatten();
    let mut candidates = Vec::new();
    for (id, airline) in &profiles.airlines {
        let category_weight = match airline.category {
            Category::Airline => airline_weight,
            Category::Regional => regional_weight,
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
        );
    }
    pick_weighted(&candidates, seed)
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
    climb_fpm: f64,
    descent_fpm: f64,
    approach_speed: f64,
    touchdown_speed: f64,
    distance: f64,
    cleared: bool,
    hold_since: Option<Instant>,
    requested: bool,
    speed: f64,
    blocked: bool,
    last: Instant,
    frequency_khz: Option<i32>,
}

/// Owns the ambient airport traffic. Not part of `generated::Runtime`.
#[derive(Default)]
pub struct TrafficManager {
    aircraft: Vec<AmbientAircraft>,
    models: Option<Arc<Catalogue>>,
    last_spawn: Option<Instant>,
    next_id: u32,
    attempts: u32,
    rejected_route: u32,
    rejected_pavement: u32,
    rejected_model: u32,
    last_error: String,
    announced: std::collections::BTreeSet<(u32, &'static str)>,
}

impl TrafficManager {
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
        state
            .session
            .generated_animation_channels
            .clone_from(&state.traffic_channels);
        let Some(models) = self.models.clone() else {
            return;
        };
        let Some(airport) = state.airport.clone() else {
            return;
        };
        let own = state.session.telemetry.position_valid.then_some([
            state.session.telemetry.latitude,
            state.session.telemetry.longitude,
        ]);
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
        let target = usize::from(
            state
                .settings
                .traffic_taxi_max
                .max(state.settings.traffic_taxi_min),
        )
        .clamp(2, 6);
        let now = Instant::now();
        let missing = target.saturating_sub(self.aircraft.len());
        if missing > 0
            && !away
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
        for index in 0..self.aircraft.len() {
            let (kind, on_ground, distance) = {
                let aircraft = &self.aircraft[index];
                let on_ground = match aircraft.kind {
                    AmbientKind::Departure => aircraft.distance < aircraft.ground_end,
                    AmbientKind::Arrival => aircraft.distance >= aircraft.ground_start,
                };
                (aircraft.kind, on_ground, aircraft.distance)
            };
            // Simple physical follow: do not drive into an aircraft ahead on the ground.
            let follow_blocked = on_ground
                && self
                    .aircraft
                    .iter()
                    .enumerate()
                    .any(|(other_index, other)| {
                        other_index != index
                            && other.kind == kind
                            && other.distance > distance
                            && other.distance - distance < 80.0
                    });
            let own_runway = self.aircraft[index].runway.clone();
            let own_airport = self.aircraft[index].airport.clone();
            let own_id = self.aircraft[index].id;
            if self.aircraft[index].on_runway() {
                state.airports.claim(&own_airport, &own_runway, own_id);
            } else {
                state.airports.release(&own_airport, &own_runway, own_id);
            }
            let runway_blocked = self.aircraft[index].kind == AmbientKind::Departure
                && state
                    .airports
                    .occupied_by_other(&own_airport, &own_runway, own_id)
                && self.aircraft[index].approaching_runway(5.0);
            // Give way to traffic on the right at a converging taxiway. The higher id yields
            // when both aircraft see each other, which breaks the mutual-yield case.
            let right_blocked = on_ground
                && self
                    .aircraft
                    .iter()
                    .enumerate()
                    .any(|(other_index, other)| {
                        if other_index == index
                            || !other.on_runway() && other.distance >= other.taxi_end
                        {
                            return false;
                        }
                        let (own_lat, own_lon, own_heading) = position_heading_at(
                            &self.aircraft[index].points,
                            &self.aircraft[index].cumulative,
                            distance,
                        );
                        let (other_lat, other_lon, _) =
                            position_heading_at(&other.points, &other.cumulative, other.distance);
                        let separation = openatc_core::arrival::distance_nm(
                            [own_lat, own_lon],
                            [other_lat, other_lon],
                        ) * 1852.0;
                        if separation > 100.0 {
                            return false;
                        }
                        let bearing = openatc_core::arrival::bearing(
                            [own_lat, own_lon],
                            [other_lat, other_lon],
                        );
                        let relative = (bearing - own_heading).rem_euclid(360.0);
                        let yields = relative > 10.0 && relative < 170.0;
                        yields && self.aircraft[index].id > other.id
                    });
            let hold_for_clearance = self.aircraft[index].kind == AmbientKind::Departure
                && distance >= self.aircraft[index].hold_distance
                && !self.aircraft[index].cleared;
            if hold_for_clearance && self.aircraft[index].hold_since.is_none() {
                self.aircraft[index].hold_since = Some(now);
                self.aircraft[index].requested = true;
            }
            let wake_delay = if hold_for_clearance
                && self.aircraft[index]
                    .hold_since
                    .is_some_and(|at| at.elapsed() >= Duration::from_secs(8))
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
            let final_blocked = hold_for_clearance
                && openatc_core::holding::blocked_for_movement(
                    &airport,
                    &self.aircraft[index].runway,
                    traffic.iter(),
                    120.0,
                )
                .is_some();
            if hold_for_clearance
                && self.aircraft[index]
                    .hold_since
                    .is_some_and(|at| at.elapsed() >= Duration::from_secs(8))
                && wake_delay <= 0.0
                && !final_blocked
            {
                self.aircraft[index].cleared = true;
            }
            let blocked = follow_blocked || runway_blocked || right_blocked || hold_for_clearance;
            let aircraft = &mut self.aircraft[index];
            aircraft.last = now;
            aircraft.blocked = blocked;
            let taxi = taxi_speed(
                &aircraft.points,
                &aircraft.cumulative,
                aircraft.distance,
                aircraft.taxi_knots,
                aircraft.taxi_turn_knots,
            );
            aircraft.speed = if blocked {
                0.0
            } else if aircraft.kind == AmbientKind::Departure
                && aircraft.distance >= aircraft.runway_start
                && aircraft.distance < aircraft.ground_end
            {
                takeoff_roll_speed(
                    &aircraft.handling,
                    aircraft.distance,
                    aircraft.runway_start,
                    aircraft.ground_end,
                    taxi,
                )
            } else if aircraft.kind == AmbientKind::Departure
                && aircraft.distance < aircraft.runway_start
            {
                taxi
            } else if aircraft.kind == AmbientKind::Departure {
                aircraft.climb_speed
            } else if aircraft.kind == AmbientKind::Arrival
                && aircraft.distance < aircraft.ground_start
            {
                // Final reduction from approach speed to touchdown speed.
                let remaining = (aircraft.ground_start - aircraft.distance).max(0.0);
                let span = aircraft.ground_start.max(1.0);
                aircraft.approach_speed
                    - (aircraft.approach_speed - aircraft.touchdown_speed)
                        * (1.0 - remaining / span)
            } else if aircraft.kind == AmbientKind::Arrival
                && aircraft.distance < aircraft.runway_end
            {
                landing_rollout_speed(
                    &aircraft.handling,
                    aircraft.distance,
                    aircraft.ground_start,
                    aircraft.runway_end,
                    taxi,
                    aircraft.touchdown_speed,
                )
            } else {
                taxi
            };
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
            aircraft.animation.advance(
                animation::Input {
                    stage,
                    knots: aircraft.speed * 1.943_844,
                    turn_degrees_sec: turn_rate,
                    vertical_fpm: match stage {
                        Stage::Climb => aircraft.climb_fpm,
                        Stage::Approach => -aircraft.descent_fpm,
                        _ => 0.0,
                    },
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
        let lead_holding = self
            .aircraft
            .iter()
            .filter(|aircraft| {
                aircraft.kind == AmbientKind::Departure
                    && !aircraft.cleared
                    && aircraft.distance >= aircraft.hold_distance
            })
            .map(|aircraft| aircraft.id)
            .min();
        let mut calls: Vec<(openatc_core::state::Radio, String, String, String)> = Vec::new();
        for aircraft in &mut self.aircraft {
            let Some(frequency) = aircraft.frequency_khz else {
                continue;
            };
            let Some(radio) = state.session.receiving_radio(frequency) else {
                continue;
            };
            let phase = match aircraft.kind {
                AmbientKind::Departure if aircraft.distance < aircraft.hold_distance => continue,
                AmbientKind::Departure if aircraft.cleared => "takeoff",
                AmbientKind::Departure if Some(aircraft.id) == lead_holding => "hold",
                AmbientKind::Departure => "lineup",
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
                "takeoff" => openatc_core::dialogue::say("ambient_takeoff", &slots),
                _ => openatc_core::dialogue::say("ambient_landing", &slots),
            };
            let pilot_reply = match phase {
                "hold" => openatc_core::dialogue::say("ambient_hold_short_readback", &slots),
                "lineup" => openatc_core::dialogue::say("ambient_line_up_readback", &slots),
                "takeoff" => openatc_core::dialogue::say("ambient_takeoff_readback", &slots),
                _ => openatc_core::dialogue::say("ambient_landing_readback", &slots),
            };
            calls.push((radio, text, pilot_reply, aircraft.callsign.clone()));
        }
        for (radio, text, pilot_reply, callsign) in calls {
            openatc_core::apply::add_transmission(
                &mut state.session,
                "ATC",
                &text,
                &openatc_core::state::SpeechTag {
                    radio: Some(radio),
                    position: "Tower".into(),
                    voice: "tower".into(),
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
                    position: "Traffic".into(),
                    ..Default::default()
                },
            );
            if let Some(last) = state.session.transcript.last_mut() {
                last.background = true;
            }
        }

        let frames: Vec<GeneratedTraffic> =
            self.aircraft.iter().filter_map(ambient_frame).collect();
        let debug: Vec<TrafficDebug> = self
            .aircraft
            .iter()
            .map(|aircraft| TrafficDebug {
                id: aircraft.id,
                callsign: aircraft.callsign.clone(),
                stage: match aircraft.kind {
                    AmbientKind::Departure if aircraft.distance < aircraft.ground_end => {
                        "TaxiOut".to_owned()
                    }
                    AmbientKind::Departure => "Takeoff".to_owned(),
                    AmbientKind::Arrival if aircraft.distance < aircraft.ground_start => {
                        "Approach".to_owned()
                    }
                    AmbientKind::Arrival => "TaxiIn".to_owned(),
                },
                reason: if aircraft.blocked {
                    "holding: traffic or runway".to_owned()
                } else {
                    match aircraft.kind {
                        AmbientKind::Departure => "ambient departure".to_owned(),
                        AmbientKind::Arrival => "ambient arrival".to_owned(),
                    }
                },
                blocker: None,
                awaiting_clearance: false,
                hold: aircraft.hold,
            })
            .collect();
        // Replace the ambient portion only; keep frames produced by other layers.
        let ambient_ids: std::collections::BTreeSet<u32> =
            self.aircraft.iter().map(|aircraft| aircraft.id).collect();
        state
            .session
            .generated_traffic
            .retain(|frame| !ambient_ids.contains(&frame.target.id));
        state.session.generated_traffic.extend(frames);
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
        let mix_total = u64::from(state.settings.airline_traffic)
            + u64::from(state.settings.regional_traffic)
            + u64::from(state.settings.ga_traffic);
        let ga_percent = if mix_total == 0 {
            0
        } else {
            u64::from(state.settings.ga_traffic) * 100 / mix_total
        };
        let mut used_nodes = std::collections::BTreeSet::new();
        for slot in 0..target {
            self.attempts = self.attempts.saturating_add(1);
            let kind = if slot == target - 1
                && target >= 3
                && !self
                    .aircraft
                    .iter()
                    .any(|aircraft| aircraft.kind == AmbientKind::Arrival)
            {
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
            let Some(node) = spawn_node(airport, size, &used_nodes, kind == AmbientKind::Arrival)
            else {
                continue;
            };
            let latitude = airport.reference_latitude + node.point.north / 111_320.0;
            let longitude = airport.reference_longitude
                + node.point.east
                    / (111_320.0 * (airport.reference_latitude.to_radians().cos()).max(0.01));
            let Some(surface_points) = plan_taxi(airport, node.id, route_runway, size) else {
                self.rejected_route = self.rejected_route.saturating_add(1);
                self.last_error = format!("no taxi graph route from node {}", node.id);
                continue;
            };
            let stand_waypoint = Waypoint {
                latitude,
                longitude,
                altitude_feet: airport.elevation_feet,
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
            let path = match kind {
                AmbientKind::Departure => departure_path(
                    airport,
                    &surface_points,
                    &departure_runway,
                    stand_waypoint,
                    &handling,
                ),
                AmbientKind::Arrival => {
                    arrival_path(airport, &surface_points, &arrival_runway, stand_waypoint)
                }
            };
            let Some((points, ground_start, ground_end, taxi_end, runway_start, runway_end)) = path
            else {
                self.rejected_route = self.rejected_route.saturating_add(1);
                self.last_error = format!("path build failed for node {}", node.id);
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
                    let fraction = if target <= 1 {
                        0.25
                    } else {
                        (slot as f64 + 1.0) / (target as f64 + 1.0)
                    };
                    (total * fraction).min(hold_distance * 0.8)
                }
            };
            let candidate = position_at(&points, &cumulative, distance);
            if self.aircraft.iter().any(|other| {
                openatc_core::arrival::distance_nm(
                    [candidate.0, candidate.1],
                    [
                        position_at(&other.points, &other.cumulative, other.distance).0,
                        position_at(&other.points, &other.cumulative, other.distance).1,
                    ],
                ) * 1852.0
                    < 120.0
            }) {
                self.rejected_route = self.rejected_route.saturating_add(1);
                self.last_error = "spawn point too close to another aircraft".to_owned();
                continue;
            }
            let taxi_knots = handling.taxi_knots.max(1.0) * 0.514_444;
            let taxi_turn_knots = handling.taxi_turn_knots.max(1.0) * 0.514_444;
            let climb_speed = climb_speed(&identity.profile, &handling);
            let approach_speed = approach_speed(&identity.profile);
            let touchdown_speed = touchdown_speed(&identity.profile);
            let climb_fpm = identity.profile.climb_fpm;
            let descent_fpm = identity.profile.descent_fpm;
            let callsign = identity.callsign.clone();
            self.aircraft.push(AmbientAircraft {
                id: 0x00e0_0000 + self.next_id,
                callsign: callsign.clone(),
                aircraft: identity.aircraft,
                profile: identity.profile,
                wake_category: identity.wake_category,
                airport: airport.icao.clone(),
                runway: route_runway.clone(),
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
                climb_fpm,
                descent_fpm,
                approach_speed,
                touchdown_speed,
                distance,
                cleared: false,
                hold_since: None,
                requested: false,
                speed: taxi_knots,
                blocked: false,
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
    fn stage(&self) -> Stage {
        match self.kind {
            AmbientKind::Departure => {
                if self.distance >= self.runway_end {
                    Stage::Climb
                } else if self.distance >= self.runway_start {
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

fn hold_line_distance(airport: &Airport, point: Point) -> f64 {
    let mut best = f64::INFINITY;
    for line in &airport.taxi_hold_lines {
        let a = line[0];
        let b = line[1];
        let dx = b.east - a.east;
        let dn = b.north - a.north;
        let length = dx.hypot(dn);
        if length < 0.5 {
            continue;
        }
        let along = (((point.east - a.east) * dx + (point.north - a.north) * dn) / length)
            .clamp(0.0, length);
        let projected = Point {
            east: a.east + dx / length * along,
            north: a.north + dn / length * along,
            height: point.height,
        };
        best = best.min((point.east - projected.east).hypot(point.north - projected.north));
    }
    best
}

fn spawn_node<'a>(
    airport: &'a Airport,
    size: char,
    used: &std::collections::BTreeSet<i64>,
    arrival: bool,
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
                && !openatc_core::airport::within_runway(airport, &node.point)
                && openatc_core::airport::on_taxiway(airport, &node.point, 8.0)
        })
        .min_by(|left, right| {
            let key = |node: &openatc_core::airport::TaxiNode| {
                stand.map_or(node.id as f64, |stand| {
                    (node.point.east - stand.east).hypot(node.point.north - stand.north)
                })
            };
            key(left).total_cmp(&key(right))
        })
}

fn plan_taxi(airport: &Airport, start: i64, runway: &str, size: char) -> Option<Vec<Point>> {
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
    let target = airport
        .nodes
        .iter()
        .filter(|node| {
            if openatc_core::airport::within_runway(airport, &node.point)
                || !openatc_core::airport::on_taxiway(airport, &node.point, 12.0)
            {
                return false;
            }
            let latitude = airport.reference_latitude + node.point.north / 111_320.0;
            let longitude = airport.reference_longitude
                + node.point.east
                    / (111_320.0 * (airport.reference_latitude.to_radians().cos()).max(0.01));
            openatc_core::traffic::runway_position(airport, runway, latitude, longitude)
                .is_none_or(|(_, across, _, _)| across.abs() > 50.0)
        })
        .min_by(|left, right| {
            let key = |node: &openatc_core::airport::TaxiNode| {
                let hold = hold_line_distance(airport, node.point) <= 40.0;
                let distance =
                    (node.point.east - threshold.east).hypot(node.point.north - threshold.north);
                (!hold, distance)
            };
            key(left)
                .partial_cmp(&key(right))
                .unwrap_or(std::cmp::Ordering::Equal)
        })?
        .id;
    let mut adjacency: BTreeMap<i64, Vec<(i64, f64)>> = BTreeMap::new();
    for edge in &airport.edges {
        if edge.runway || edge.size < size {
            continue;
        }
        let (Some(first), Some(second)) = (nodes.get(&edge.first), nodes.get(&edge.second)) else {
            continue;
        };
        let cost = (second.east - first.east).hypot(second.north - first.north);
        adjacency
            .entry(edge.first)
            .or_default()
            .push((edge.second, cost));
        if !edge.one_way {
            adjacency
                .entry(edge.second)
                .or_default()
                .push((edge.first, cost));
        }
    }
    let mut costs = BTreeMap::from([(start, 0.0)]);
    let mut previous = BTreeMap::new();
    let mut heap = BinaryHeap::from([(Reverse(0u64), start)]);
    while let Some((Reverse(bits), node)) = heap.pop() {
        let cost = f64::from_bits(bits);
        if node == target {
            break;
        }
        if cost > costs.get(&node).copied().unwrap_or(f64::INFINITY) {
            continue;
        }
        for (next, step) in adjacency.get(&node).into_iter().flatten() {
            let next_cost = cost + step;
            if next_cost < costs.get(next).copied().unwrap_or(f64::INFINITY) {
                costs.insert(*next, next_cost);
                previous.insert(*next, node);
                heap.push((Reverse(next_cost.to_bits()), *next));
            }
        }
    }
    if !costs.contains_key(&target) {
        return None;
    }
    let mut path = vec![target];
    let mut node = target;
    while let Some(previous_node) = previous.get(&node) {
        path.push(*previous_node);
        node = *previous_node;
    }
    path.reverse();
    if path.len() < 2 || path.first() != Some(&start) {
        return None;
    }
    Some(
        path.into_iter()
            .filter_map(|id| nodes.get(&id).copied())
            .collect(),
    )
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

/// Distance from the threshold needed to accelerate to the rotation speed.
fn takeoff_rotation_meters(handling: &Handling, airport: &Airport, runway: &str) -> f64 {
    let length = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)
        .map_or(2_000.0, |runway| {
            (runway.second.east - runway.first.east).hypot(runway.second.north - runway.first.north)
        });
    // Plan for the slowest line-up so Vr is met even from a turn.
    let taxi = handling.taxi_turn_knots.max(1.0);
    let acceleration = handling.acceleration_knots_sec.max(0.1);
    let required = ((handling.rotation_knots.powi(2) - taxi.powi(2)).max(0.0)
        / (2.0 * acceleration))
        * 0.514_444;
    required.clamp(300.0, (length - 150.0).max(300.0))
}

fn departure_path(
    airport: &Airport,
    points: &[Point],
    runway: &str,
    stand: Waypoint,
    handling: &Handling,
) -> Option<(Vec<Waypoint>, f64, f64, f64, f64, f64)> {
    let mut path = surface_waypoints(airport, points);
    if path.is_empty() {
        return None;
    }
    let last = path.last().copied();
    let (_, threshold, far) = runway_ends(airport, runway, last)?;
    let _ = stand;
    let taxi_end = cumulative_distances(&path).last().copied().unwrap_or(0.0);
    let heading = openatc_core::arrival::bearing(
        [threshold.latitude, threshold.longitude],
        [far.latitude, far.longitude],
    );
    let offset = |nm: f64, altitude: f64| {
        let radians = heading.to_radians();
        let latitude = threshold.latitude + radians.cos() * nm * 1852.0 / 111_320.0;
        let longitude = threshold.longitude
            + radians.sin() * nm * 1852.0 / (111_320.0 * (latitude.to_radians().cos()).max(0.01));
        Waypoint {
            latitude,
            longitude,
            altitude_feet: airport.elevation_feet + altitude,
        }
    };
    let rotation = offset(
        takeoff_rotation_meters(handling, airport, runway) / 1_852.0,
        0.0,
    );
    let climb = offset(5.0, 1_500.0);
    path.push(threshold);
    let runway_start = cumulative_distances(&path)
        .last()
        .copied()
        .unwrap_or(taxi_end);
    path.push(rotation);
    path.push(climb);
    let cumulative = cumulative_distances(&path);
    let ground_end = cumulative
        .get(path.len().saturating_sub(2))
        .copied()
        .unwrap_or(0.0);
    Some((path, 0.0, ground_end, taxi_end, runway_start, ground_end))
}

fn arrival_path(
    airport: &Airport,
    points: &[Point],
    runway: &str,
    stand: Waypoint,
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
    if let Some(entry) = points
        .last()
        .and_then(|point| entry_waypoint(airport, runway, *point))
    {
        path.push(entry);
    }
    let exit_index = path.len().saturating_sub(1);
    let surface = surface_waypoints(airport, points);
    path.extend(surface.into_iter().rev().skip(1));
    path.push(stand);
    let cumulative = cumulative_distances(&path);
    let ground_start = cumulative.get(3).copied().unwrap_or(0.0);
    let ground_end = cumulative.last().copied().unwrap_or(0.0);
    let runway_end = cumulative.get(exit_index).copied().unwrap_or(ground_start);
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
        let local = openatc_core::airport::airport_point(airport, latitude, longitude);
        if !openatc_core::airport::within_runway(airport, &local) {
            break;
        }
        stop = (stop - 5.0).max(0.0);
    }
    stop
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

fn taxi_speed(
    points: &[Waypoint],
    cumulative: &[f64],
    distance: f64,
    straight: f64,
    turn: f64,
) -> f64 {
    let index = cumulative
        .iter()
        .position(|value| *value >= distance)
        .unwrap_or(points.len().saturating_sub(1))
        .max(1);
    let Some(previous) = points.get(index.saturating_sub(1)) else {
        return straight;
    };
    let Some(current) = points.get(index) else {
        return straight;
    };
    let Some(next) = points.get(index + 1) else {
        return straight;
    };
    let first = openatc_core::arrival::bearing(
        [previous.latitude, previous.longitude],
        [current.latitude, current.longitude],
    );
    let second = openatc_core::arrival::bearing(
        [current.latitude, current.longitude],
        [next.latitude, next.longitude],
    );
    if openatc_core::navigation::turn(first, second).abs() > 15.0 {
        turn
    } else {
        straight
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
    let track = openatc_core::arrival::bearing(
        [start.latitude, start.longitude],
        [end.latitude, end.longitude],
    );
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
                    &profiles, &models, "LFMN", 2500.0, false, seed, &used, "C-GTLT", 8, 4,
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
            )
            .is_none(),
            "an unrelated paint must not be used as a fallback"
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
                    &profiles, &models, "LFMN", 2500.0, true, seed, &used, "C-GTLT", 8, 4,
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
            climb_fpm: 2_200.0,
            descent_fpm: 1_800.0,
            approach_speed: 75.0,
            touchdown_speed: 70.0,
            distance,
            cleared: true,
            hold_since: None,
            requested: false,
            speed: TAXI_STRAIGHT_METERS_PER_SECOND,
            blocked: false,
            last: Instant::now(),
            frequency_khz: None,
        }
    }

    #[test]
    fn light_ga_uses_piston_speeds_and_light_wake() {
        let mut handling = animation::builtin().expect("bundled handling catalogue");
        let piston = handling.remove(&Kind::Piston).expect("piston handling");
        let jet = handling.remove(&Kind::Jet).expect("jet handling");
        let c172 = openatc_core::traffic::profiles::aircraft("C172").expect("C172 profile");
        let a320 = openatc_core::traffic::profiles::aircraft("A320").expect("A320 profile");
        assert!(climb_speed(c172, &piston) < climb_speed(a320, &jet));
        assert!((approach_speed(c172) - 65.0 * 0.514_444).abs() < 0.01);
        assert!((touchdown_speed(c172) - 60.0 * 0.514_444).abs() < 0.01);
        assert_eq!(wake_category(c172), Some(0));
        assert_eq!(wake_category(a320), Some(1));
        let start = 0.0;
        let end = 300.0;
        let rotation = piston.rotation_knots * 0.514_444;
        assert!((takeoff_roll_speed(&piston, end, start, end, 5.0) - rotation).abs() < 0.01);
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
            (takeoff_roll_speed(&handling, start, start, rotation_distance, taxi) - taxi).abs()
                < 0.01
        );
        assert!(
            (takeoff_roll_speed(&handling, rotation_distance, start, rotation_distance, taxi)
                - rotation)
                .abs()
                < 0.01
        );
        let later = takeoff_roll_speed(&handling, start + 600.0, start, rotation_distance, taxi);
        let rate = (later.powi(2) - taxi.powi(2)) / (2.0 * 600.0);
        assert!((rate - handling.acceleration_knots_sec * 0.514_444).abs() < 0.001);
        assert!(
            takeoff_roll_speed(
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
            (takeoff_roll_speed(&handling, short_end, start, short_end, taxi) - rotation).abs()
                < 0.01
        );
    }

    #[test]
    fn landing_rollout_brakes_at_the_handling_rate_and_stops_by_the_exit() {
        let handling = jet_handling();
        let taxi = TAXI_STRAIGHT_METERS_PER_SECOND;
        let touchdown_speed = 70.0;
        let touchdown = 1_000.0;
        let long_exit = 4_000.0;
        let at_touchdown = landing_rollout_speed(
            &handling,
            touchdown,
            touchdown,
            long_exit,
            taxi,
            touchdown_speed,
        );
        assert!((at_touchdown - touchdown_speed).abs() < 0.01);
        let later = landing_rollout_speed(
            &handling,
            touchdown + 500.0,
            touchdown,
            long_exit,
            taxi,
            touchdown_speed,
        );
        let rate = (at_touchdown.powi(2) - later.powi(2)) / (2.0 * 500.0);
        assert!((rate - handling.braking_knots_sec * 0.514_444).abs() < 0.01);
        let short_exit = 1_400.0;
        let at_exit = landing_rollout_speed(
            &handling,
            short_exit,
            touchdown,
            short_exit,
            taxi,
            touchdown_speed,
        );
        assert!((at_exit - taxi).abs() < 0.01);
        assert!(
            landing_rollout_speed(
                &handling,
                short_exit + 200.0,
                touchdown,
                short_exit,
                taxi,
                touchdown_speed,
            ) >= taxi
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
