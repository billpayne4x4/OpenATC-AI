//! Advisory-only traffic at untowered fields, selected and flown without a controller.
//!
//! A CTAF field publishes a common traffic advisory frequency and no Tower service, so the
//! traffic there makes its own position calls. The advisory procedure is the authority for
//! that traffic: the movement world grants its segments, never a controller clearance, and
//! the calls are spoken on the published frequency.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
#[allow(unused_imports, clippy::wildcard_imports)]
use super::*;
use super::{
    Airport, Animation, Arc, BTreeSet, Category, EngineState, GeneratedTraffic, MAXIMUM_AIRCRAFT,
    Operations, Preparation, Runtime, Scene, TrafficTarget, animation, policy,
};
use openatc_core::{
    apply::add_transmission,
    state::SpeechTag,
    traffic::ctaf::{self, Circuit, CrossCountry, Field, Pattern},
};
use std::time::{Duration, Instant};

/// Advisory aircraft published at one untowered field.
const MAXIMUM_ADVISORY_AIRCRAFT: usize = 2;

/// Identity range reserved for advisory traffic, clear of the generated fleet.
const ADVISORY_ID_BASE: u32 = 0x00d0_0000;

/// Identity range reserved for Tower-controlled local traffic.
const LOCAL_ID_BASE: u32 = 0x00d1_0000;

/// Whether an identity belongs to the local Tower traffic layer.
pub(super) fn is_local(id: u32) -> bool {
    (LOCAL_ID_BASE..LOCAL_ID_BASE + 0x1000).contains(&id)
}

/// Whether an identity belongs to traffic that is not part of the released network.
pub(super) fn is_generated(id: u32) -> bool {
    (ADVISORY_ID_BASE..ADVISORY_ID_BASE + 0x1000).contains(&id) || is_local(id)
}

/// A prepared local departure waiting for a runway slot on the active scene.
#[derive(Clone)]
pub(super) struct LocalTemplate {
    pub(super) plan: Plan,
    pub(super) frame: GeneratedTraffic,
    pub(super) movement: Option<animation::Handling>,
}

/// One aircraft flying a published pattern, either CTAF-advisory or Tower-controlled.
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct Plan {
    /// Aircraft identity.
    pub(super) id: u32,
    /// Callsign making the advisories.
    pub(super) callsign: String,
    /// The field the pattern is flown at.
    pub(super) field: Field,
    /// The pattern, which produces the circuits.
    pub(super) pattern: Arc<Pattern>,
    /// The circuit currently being flown.
    pub(super) circuit: Circuit,
    /// Segments of the current circuit whose advisory has already been made.
    pub(super) announced: BTreeSet<usize>,
    /// When the next advisory may be made.
    pub(super) due: Option<Instant>,
    /// The go-around call, made instead of the final call when the runway is occupied.
    pub(super) go_around: Option<openatc_core::traffic::ctaf::Advisory>,
    /// A straight-in arrival ends after one approach instead of repeating a circuit.
    pub(super) straight_in: bool,
    /// A departure leaves the field after one climb-out instead of repeating a circuit.
    pub(super) departure: bool,
    /// A VFR flight between this field and another.
    pub(super) cross: Option<Arc<CrossCountry>>,
    /// Tower-controlled local traffic rather than a self-announced CTAF advisory.
    pub(super) controlled: bool,
    /// Runway used by the controlled calls.
    pub(super) runway: String,
    /// Tower has issued the clearance that permits the next movement.
    pub(super) cleared: bool,
}

impl Plan {
    /// The advisory the aircraft makes on its present segment, when it makes one.
    fn advisory(&self, segment: usize) -> Option<&openatc_core::traffic::ctaf::Advisory> {
        match &self.cross {
            Some(cross) => cross.advisory(segment).map(|(advisory, _)| advisory),
            None => self.circuit.advisory(segment),
        }
    }

    fn route(&self) -> Arc<openatc_core::traffic::motion::Route> {
        self.cross
            .as_ref()
            .map_or_else(|| self.circuit.route(), |cross| cross.route())
    }

    fn field_for(&self, segment: usize) -> &Field {
        self.cross
            .as_ref()
            .and_then(|cross| cross.advisory(segment).map(|(_, field)| field))
            .unwrap_or(&self.field)
    }

    fn runway_resource(&self) -> &str {
        self.cross.as_ref().map_or_else(
            || self.pattern.runway_resource(),
            |cross| cross.landing_runway_resource(),
        )
    }

    fn final_segment(&self) -> Option<usize> {
        match &self.cross {
            Some(cross) => cross
                .legs()
                .iter()
                .find(|leg| leg.advisory.leg == "final")
                .map(|leg| leg.segment),
            None => self
                .circuit
                .legs()
                .iter()
                .find(|leg| leg.advisory.leg == "final")
                .map(|leg| leg.segment),
        }
    }

    fn one_shot(&self) -> bool {
        self.cross.is_some() || self.straight_in || self.departure
    }
}

/// Slots left for advisory aircraft in a scene that already holds `used` aircraft.
fn advisory_capacity(used: usize) -> usize {
    MAXIMUM_AIRCRAFT
        .saturating_sub(used)
        .min(MAXIMUM_ADVISORY_AIRCRAFT)
}

/// Untwowered fields with an advisory frequency near the player, nearest first.
pub(super) fn fields(input: &Preparation, policy: &Operations) -> Vec<String> {
    let mut candidates = ctaf::candidates(&input.stations)
        .into_iter()
        .filter_map(|id| {
            let station = input
                .stations
                .iter()
                .find(|station| station.airport == id)?;
            let distance = openatc_core::arrival::distance_nm(
                input.own,
                [station.latitude, station.longitude],
            );
            (distance.is_finite() && distance <= policy.network_radius_nm).then_some((distance, id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    candidates
        .into_iter()
        .map(|(_, id)| id)
        .take(policy.network_candidate_limit)
        .collect()
}

/// The longest runway at a field.
fn longest_runway(airport: &Airport) -> Option<String> {
    airport
        .runways
        .iter()
        .max_by(|a, b| {
            let length = |runway: &openatc_core::airport::Runway| {
                (runway.second.east - runway.first.east)
                    .hypot(runway.second.north - runway.first.north)
            };
            length(a).total_cmp(&length(b))
        })
        .map(|runway| runway.first_name.clone())
}

/// Identity for local traffic: an airline flight number, or a registration for private GA.
fn local_callsign(input: &Preparation, airline: &str, seed: u64) -> Option<String> {
    let profile = input.profiles.airlines.get(airline)?;
    if profile.operator.is_empty() {
        profile.registration(&input.origin.icao, seed)
    } else {
        Some(format!("{}{}", profile.operator, 100 + seed % 8900))
    }
}

/// A local circuit aircraft: prefer a regional turboprop with an installed model, then GA.
fn select_local_aircraft(
    input: &Preparation,
    runway_meters: f64,
    seed: u64,
) -> Option<(String, String, String)> {
    for category in [Category::Regional, Category::GeneralAviation] {
        let mut choices = Vec::new();
        for (id, airline) in &input.profiles.airlines {
            if airline.category != category {
                continue;
            }
            for (kind, weight) in &airline.fleet {
                let Some(aircraft) = input.profiles.aircraft.get(kind) else {
                    continue;
                };
                if runway_meters < aircraft.runway {
                    continue;
                }
                if category == Category::Regional && !aircraft.propeller {
                    continue;
                }
                let model = std::iter::once(kind.clone())
                    .chain(aircraft.model_aliases.iter().cloned())
                    .find_map(|code| {
                        input
                            .models
                            .choose(&code, &airline.operator, "", seed)
                            .map(|model| model.id.clone())
                    });
                let Some(model) = model else {
                    continue;
                };
                choices.push((u64::from(*weight), kind.clone(), model, id.clone()));
            }
        }
        if choices.is_empty() {
            continue;
        }
        choices.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let index = (seed % choices.len() as u64) as usize;
        return choices
            .into_iter()
            .nth(index)
            .map(|(_, kind, model, airline)| (kind, model, airline));
    }
    None
}

/// A light aircraft with an installed model that can use the field's runway.
fn select_aircraft(
    input: &Preparation,
    runway_meters: f64,
    seed: u64,
) -> Option<(String, String, String)> {
    let mut choices = Vec::new();
    for (id, airline) in &input.profiles.airlines {
        if airline.category != Category::GeneralAviation {
            continue;
        }
        for (kind, weight) in &airline.fleet {
            let Some(aircraft) = input.profiles.aircraft.get(kind) else {
                continue;
            };
            if runway_meters < aircraft.runway {
                continue;
            }
            let model = std::iter::once(kind.clone())
                .chain(aircraft.model_aliases.iter().cloned())
                .find_map(|code| {
                    input
                        .models
                        .choose(&code, &airline.operator, "", seed)
                        .map(|model| model.id.clone())
                });
            let Some(model) = model else {
                continue;
            };
            choices.push((u64::from(*weight), kind.clone(), model, id.clone()));
        }
    }
    if choices.is_empty() {
        return None;
    }
    choices.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let index = (seed % choices.len() as u64) as usize;
    choices
        .into_iter()
        .nth(index)
        .map(|(_, kind, model, airline)| (kind, model, airline))
}

/// Plan advisory traffic for one untowered field.
fn plan(
    input: &Preparation,
    airport: &Airport,
    seed: u64,
    identity: u32,
    straight_in: bool,
    departure: bool,
) -> Result<(Plan, GeneratedTraffic, Option<animation::Kind>), String> {
    let field = ctaf::field(airport, &input.stations)?;
    let runway = longest_runway(airport).ok_or("advisory field has no runway")?;
    let runway_meters = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)
        .map_or(0.0, |candidate| {
            (candidate.second.east - candidate.first.east)
                .hypot(candidate.second.north - candidate.first.north)
        });
    let (kind, model, airline) = select_aircraft(input, runway_meters, seed)
        .ok_or("no light aircraft has an installed model")?;
    let aircraft = input
        .profiles
        .aircraft
        .get(&kind)
        .ok_or("advisory traffic has no movement envelope")?;
    let installed = input
        .models
        .models
        .iter()
        .find(|candidate| candidate.id == model)
        .cloned()
        .ok_or("advisory traffic has no installed model")?;
    let callsign = input.profiles.airlines[&airline]
        .registration(&airport.icao, seed)
        .ok_or("advisory traffic has no registration pattern")?;
    let pattern = if straight_in {
        Pattern::straight_in(airport, &runway, &callsign, field.clone())?
    } else if departure {
        Pattern::departure(airport, &runway, &callsign, field.clone())?
    } else {
        Pattern::new(airport, &runway, &callsign, field.clone())?
    };
    let circuit = pattern.circuit().clone();
    let start = circuit.route().start_position();
    let track = circuit
        .route()
        .segment(0)
        .and_then(|segment| segment.points.windows(2).next())
        .map_or(0.0, |pair| {
            openatc_core::arrival::bearing(
                [pair[0].latitude, pair[0].longitude],
                [pair[1].latitude, pair[1].longitude],
            )
        });
    let frame = GeneratedTraffic {
        vertical_speed_fpm: 0.0,
        body_heading_degrees: Some(track),
        target: TrafficTarget {
            id: identity,
            callsign: callsign.clone(),
            name: input.profiles.airlines[&airline].name.clone(),
            aircraft: kind.clone(),
            latitude: start.latitude,
            longitude: start.longitude,
            altitude_feet: start.altitude_feet,
            track_degrees: track,
            on_ground: false,
            ground_speed_knots: Some(90.0),
            wingspan_meters: Some(aircraft.wingspan),
            com1_khz: Some(field.frequency_khz),
            ..Default::default()
        },
        model: Arc::new(installed),
        animation: Animation::default(),
    };
    Ok((
        Plan {
            id: identity,
            callsign,
            field,
            pattern: Arc::new(pattern),
            circuit,
            announced: BTreeSet::new(),
            due: None,
            go_around: None,
            straight_in,
            departure,
            cross: None,
            controlled: false,
            runway: String::new(),
            cleared: false,
        },
        frame,
        aircraft.handling,
    ))
}

/// Plan a VFR flight between two untowered fields.
fn plan_cross(
    input: &Preparation,
    origin: &Airport,
    origin_runway: &str,
    destination: &Airport,
    destination_runway: &str,
    seed: u64,
    identity: u32,
) -> Result<(Plan, GeneratedTraffic, Option<animation::Kind>), String> {
    let origin_field = ctaf::field(origin, &input.stations)?;
    let destination_field = ctaf::field(destination, &input.stations)?;
    let length = |airport: &Airport, name: &str| {
        airport
            .runways
            .iter()
            .find(|runway| runway.first_name == name || runway.second_name == name)
            .map_or(0.0, |runway| {
                (runway.second.east - runway.first.east)
                    .hypot(runway.second.north - runway.first.north)
            })
    };
    let available = length(origin, origin_runway).min(length(destination, destination_runway));
    let (kind, model, airline) = select_aircraft(input, available, seed)
        .ok_or("no light aircraft has an installed model")?;
    let aircraft = input
        .profiles
        .aircraft
        .get(&kind)
        .ok_or("advisory traffic has no movement envelope")?;
    let installed = input
        .models
        .models
        .iter()
        .find(|candidate| candidate.id == model)
        .cloned()
        .ok_or("advisory traffic has no installed model")?;
    let callsign = input.profiles.airlines[&airline]
        .registration(&origin.icao, seed)
        .ok_or("advisory traffic has no registration pattern")?;
    let cross = CrossCountry::new(
        origin,
        origin_runway,
        destination,
        destination_runway,
        &callsign,
        origin_field.clone(),
        destination_field,
    )?;
    let start = cross.route().start_position();
    let track = cross
        .route()
        .segment(0)
        .and_then(|segment| segment.points.windows(2).next())
        .map_or(0.0, |pair| {
            openatc_core::arrival::bearing(
                [pair[0].latitude, pair[0].longitude],
                [pair[1].latitude, pair[1].longitude],
            )
        });
    let frame = GeneratedTraffic {
        vertical_speed_fpm: 0.0,
        body_heading_degrees: Some(track),
        target: TrafficTarget {
            id: identity,
            callsign: callsign.clone(),
            name: input.profiles.airlines[&airline].name.clone(),
            aircraft: kind.clone(),
            latitude: start.latitude,
            longitude: start.longitude,
            altitude_feet: start.altitude_feet,
            track_degrees: track,
            on_ground: false,
            ground_speed_knots: Some(75.0),
            wingspan_meters: Some(aircraft.wingspan),
            com1_khz: Some(origin_field.frequency_khz),
            ..Default::default()
        },
        model: Arc::new(installed),
        animation: Animation::default(),
    };
    let origin_pattern = Pattern::new(origin, origin_runway, &callsign, origin_field.clone())?;
    let circuit = origin_pattern.circuit().clone();
    Ok((
        Plan {
            id: identity,
            callsign,
            field: origin_field,
            pattern: Arc::new(origin_pattern),
            circuit,
            announced: BTreeSet::new(),
            due: None,
            go_around: None,
            straight_in: false,
            departure: false,
            cross: Some(Arc::new(cross)),
            controlled: false,
            runway: String::new(),
            cleared: false,
        },
        frame,
        aircraft.handling,
    ))
}

/// Published Tower frequency for an airport, if it has one.
fn tower_frequency(airport: &Airport, stations: &[openatc_core::stations::Station]) -> Option<i32> {
    stations
        .iter()
        .filter(|station| {
            station.airport == airport.icao
                && (station.service.eq_ignore_ascii_case("Tower")
                    || station
                        .services
                        .iter()
                        .any(|service| service.eq_ignore_ascii_case("Tower")))
        })
        .map(|station| station.khz)
        .find(|khz| (118_000..=136_975).contains(khz))
        .or_else(|| {
            airport
                .frequencies
                .iter()
                .filter(|frequency| {
                    frequency.service.eq_ignore_ascii_case("Tower")
                        || frequency.name.to_ascii_lowercase().contains("tower")
                })
                .map(|frequency| frequency.khz)
                .find(|khz| (118_000..=136_975).contains(khz))
        })
}

/// Protect a short local route against the same physical scenery gates as a full flight.
fn local_route_clear(
    terrain: &mut super::scenery::Terrain,
    route: &openatc_core::traffic::motion::Route,
    radius: f64,
) -> Result<bool, String> {
    for index in 0..route.segment_count() {
        let Some(segment) = route.segment(index) else {
            continue;
        };
        for pair in segment.points.windows(2) {
            if !terrain.terrain_clear(pair[0], pair[1], 0.0)? {
                return Ok(false);
            }
            if terrain.has_unresolved_obstacles() {
                return Ok(false);
            }
            if !terrain.obstacles_clear(pair[0], pair[1], radius)? {
                return Ok(false);
            }
            if terrain.has_unresolved_obstacles() {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn local_frame(
    input: &Preparation,
    identity: u32,
    callsign: &str,
    airline: &str,
    kind: &str,
    aircraft: &openatc_core::traffic::profiles::Aircraft,
    model: openatc_core::traffic::csl::Model,
    route: &openatc_core::traffic::motion::Route,
    frequency_khz: i32,
) -> Option<GeneratedTraffic> {
    let start = route.start_position();
    let first = route.segment(0)?;
    let track = first.points.windows(2).next().map_or(0.0, |pair| {
        openatc_core::arrival::bearing(
            [pair[0].latitude, pair[0].longitude],
            [pair[1].latitude, pair[1].longitude],
        )
    });
    Some(GeneratedTraffic {
        vertical_speed_fpm: 0.0,
        body_heading_degrees: Some(track),
        target: TrafficTarget {
            id: identity,
            callsign: callsign.to_owned(),
            name: input.profiles.airlines[airline].name.clone(),
            aircraft: kind.to_owned(),
            latitude: start.latitude,
            longitude: start.longitude,
            altitude_feet: start.altitude_feet,
            track_degrees: track,
            on_ground: false,
            ground_speed_knots: Some(first.knots),
            wingspan_meters: Some(aircraft.wingspan),
            com1_khz: Some(frequency_khz),
            ..Default::default()
        },
        model: Arc::new(model),
        animation: Animation::default(),
    })
}

/// Ambient taxi-out traffic for the opening scene.
///
/// This layer deliberately does not require a complete flight plan, SID/STAR or terrain
/// check. It only needs a compatible stand, a surface route to the active runway and a CSL
/// model, so the airport looks alive even when the full route catalogue cannot release a
/// city pair.
fn prepare_surface_departures(
    scene: &mut Scene,
    input: &Preparation,
    handling: &std::collections::BTreeMap<animation::Kind, animation::Handling>,
    policy: &Operations,
) {
    if input.simulator_root.is_empty() {
        return;
    }
    let existing_taxi_out = scene
        .frames
        .iter()
        .filter(|frame| {
            scene
                .world
                .current_segment(frame.target.id)
                .is_some_and(|segment| {
                    segment.stage == openatc_core::traffic::motion::Stage::TaxiOut
                })
        })
        .count();
    if existing_taxi_out >= input.taxi_target.clamp(2, 6) {
        return;
    }
    let airport = &input.origin;
    let runway = if airport
        .runways
        .iter()
        .any(|runway| runway.first_name == input.runway || runway.second_name == input.runway)
    {
        input.runway.clone()
    } else {
        let Some(runway) = longest_runway(airport) else {
            return;
        };
        runway
    };
    let runway_meters = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)
        .map_or(0.0, |candidate| {
            (candidate.second.east - candidate.first.east)
                .hypot(candidate.second.north - candidate.first.north)
        });
    let Some(frequency_khz) = tower_frequency(airport, &input.stations) else {
        return;
    };
    let target = input.taxi_target.clamp(2, 6);
    let generation = scene.world.generation();
    let mut used_stands = BTreeSet::new();
    let mut seed = input.epoch.wrapping_add(0x9E37_79B9);
    for slot in 0..target {
        seed = seed.wrapping_mul(16_777_619) ^ (seed >> 32);
        let Some((kind, model, airline)) = select_local_aircraft(input, runway_meters, seed) else {
            continue;
        };
        let Some(aircraft) = input.profiles.aircraft.get(&kind) else {
            continue;
        };
        let Some(installed) = input
            .models
            .models
            .iter()
            .find(|candidate| candidate.id == model)
            .cloned()
        else {
            continue;
        };
        let Some(stand) = airport.parking.iter().find(|stand| {
            !input.reserved_stands.contains(&stand.name)
                && !used_stands.contains(&stand.name)
                && stand
                    .size
                    .chars()
                    .next()
                    .is_some_and(|size| size >= aircraft.parking_size())
        }) else {
            continue;
        };
        let Some(animation_kind) = aircraft.handling else {
            continue;
        };
        let Some(handling_for) = handling.get(&animation_kind) else {
            continue;
        };
        let Ok(plan) = super::ground_route(
            airport,
            stand.point,
            stand.heading,
            stand.kind == "gate",
            aircraft,
            handling_for,
            &runway,
            seed,
            policy,
            false,
            &input.observed,
            false,
        ) else {
            continue;
        };
        // Surface route plus a departure plan, so the aircraft can actually take off.
        let mut segments: Vec<openatc_core::traffic::motion::Segment> =
            (0..plan.route.segment_count())
                .filter_map(|index| plan.route.segment(index).cloned())
                .collect();
        let hold = plan.route.end_position();
        let Some(runway_data) = airport
            .runways
            .iter()
            .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)
        else {
            continue;
        };
        let to_waypoint = |point: openatc_core::state::Point, altitude: f64| {
            openatc_core::traffic::motion::Waypoint {
                latitude: airport.reference_latitude + point.north / 111_320.0,
                longitude: airport.reference_longitude
                    + point.east
                        / (111_320.0 * (airport.reference_latitude.to_radians().cos()).max(0.01)),
                altitude_feet: altitude,
            }
        };
        let (threshold_point, far_point) = if runway_data.first_name == runway {
            (runway_data.first, runway_data.second)
        } else {
            (runway_data.second, runway_data.first)
        };
        let threshold = to_waypoint(threshold_point, airport.elevation_feet);
        let far = to_waypoint(far_point, airport.elevation_feet + 2_000.0);
        segments.push(openatc_core::traffic::motion::Segment {
            stage: openatc_core::traffic::motion::Stage::Takeoff,
            points: vec![hold, threshold],
            knots: 120.0,
            resources: vec![format!(
                "RUNWAY/{}/{}/{}",
                airport.icao, runway_data.first_name, runway_data.second_name
            )],
            dwell_seconds: 0.0,
        });
        segments.push(openatc_core::traffic::motion::Segment {
            stage: openatc_core::traffic::motion::Stage::Climb,
            points: vec![threshold, far],
            knots: 160.0,
            resources: vec![],
            dwell_seconds: 0.0,
        });
        let Ok(combined) = openatc_core::traffic::motion::Route::compile(
            segments,
            aircraft.climb_fpm,
            aircraft.descent_fpm,
        ) else {
            continue;
        };
        let route = Arc::new(combined);
        let surface: Vec<usize> = (0..route.segment_count())
            .filter(|index| {
                route.segment(*index).is_some_and(|segment| {
                    matches!(
                        segment.stage,
                        openatc_core::traffic::motion::Stage::TaxiOut
                            | openatc_core::traffic::motion::Stage::TaxiIn
                    )
                })
            })
            .collect();
        if surface.is_empty() {
            continue;
        }
        let identity = LOCAL_ID_BASE + u32::try_from(slot + 1).unwrap_or(1);
        if scene.frames.iter().any(|frame| frame.target.id == identity) {
            continue;
        }
        if !scene.world.insert(generation, identity, route.clone()) {
            continue;
        }
        let index = surface[(slot * surface.len() / target).min(surface.len() - 1)];
        scene.world.place_on_segment(identity, index);
        let Some(callsign) = local_callsign(input, &airline, seed) else {
            scene.world.remove(identity);
            continue;
        };
        let Some(frame) = local_frame(
            input,
            identity,
            &callsign,
            &airline,
            &kind,
            aircraft,
            installed,
            &route,
            frequency_khz,
        ) else {
            scene.world.remove(identity);
            continue;
        };
        scene.frames.push(frame);
        scene.sync_frame(identity);
        used_stands.insert(stand.name.clone());
    }
}

/// A Tower-controlled straight-in arrival and departure at the player's airport.
///
/// The published local pattern is only admitted after every segment passes the same terrain and
/// obstacle checks as a full flight. It therefore supplements, rather than bypasses, the
/// generated network when that network has no released airborne traffic.
pub(super) fn prepare_tower(
    scene: &mut Scene,
    input: &Preparation,
    handling: &std::collections::BTreeMap<animation::Kind, animation::Handling>,
) {
    if input.simulator_root.is_empty() {
        return;
    }
    let airport = &input.origin;
    let Some(frequency_khz) = tower_frequency(airport, &input.stations) else {
        return;
    };
    // The local layer is a fallback for an airport the network did not fill. It is only added
    // when the user asks for it and the network released fewer than two flights, because a
    // pattern that flies the circuit beside the network's own departures would hold the runway
    // and the departure forecast against them.
    if !input.local_pattern_traffic || scene.flights.len() >= 2 {
        return;
    }
    let used = scene.flights.len().max(scene.frames.len());
    let capacity = MAXIMUM_AIRCRAFT.saturating_sub(used).min(3);
    if capacity == 0 {
        return;
    }
    let runway = if airport
        .runways
        .iter()
        .any(|runway| runway.first_name == input.runway || runway.second_name == input.runway)
    {
        input.runway.clone()
    } else {
        let Some(runway) = longest_runway(airport) else {
            return;
        };
        runway
    };
    let runway_meters = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)
        .map_or(0.0, |candidate| {
            (candidate.second.east - candidate.first.east)
                .hypot(candidate.second.north - candidate.first.north)
        });
    let field = Field {
        airport: airport.icao.clone(),
        frequency_khz,
    };
    let mut terrain =
        match super::scenery::Terrain::new(std::path::Path::new(&input.simulator_root)) {
            Ok(terrain) => terrain,
            Err(error) => {
                scene
                    .warnings
                    .push(format!("{}: local Tower traffic: {error}", airport.icao));
                return;
            }
        };
    terrain.begin_check();
    let generation = scene.world.generation();
    let mut seed = input.epoch.wrapping_add(41);
    let mut added = 0u32;
    let mut departure_added = false;
    for departure in [false] {
        if added >= capacity as u32 {
            break;
        }
        seed = seed.wrapping_mul(16_777_619) ^ (seed >> 32);
        // The visual circuit is a GA pattern; keep its smaller envelope and proven geometry.
        // Departures prefer a regional turboprop so airline identities are visible.
        let selected = if departure {
            select_local_aircraft(input, runway_meters, seed)
        } else {
            select_aircraft(input, runway_meters, seed)
        };
        let Some((kind, model, airline)) = selected else {
            continue;
        };
        let Some(aircraft) = input.profiles.aircraft.get(&kind) else {
            continue;
        };
        let Some(installed) = input
            .models
            .models
            .iter()
            .find(|candidate| candidate.id == model)
            .cloned()
        else {
            continue;
        };
        let Some(callsign) = local_callsign(input, &airline, seed) else {
            continue;
        };
        let pattern = match if departure {
            Pattern::departure(airport, &runway, &callsign, field.clone())
        } else {
            // The arrival flies the published circuit so it lands and climbs out repeatedly
            // instead of disappearing after one straight-in.
            Pattern::new(airport, &runway, &callsign, field.clone())
        } {
            Ok(pattern) => pattern,
            Err(error) => {
                scene
                    .warnings
                    .push(format!("{}: local Tower traffic: {error}", airport.icao));
                continue;
            }
        };
        let circuit = pattern.circuit().clone();
        let route = circuit.route();
        let radius = aircraft.wingspan / 2.0 + 10.0;
        match local_route_clear(&mut terrain, &route, radius) {
            Ok(true) => {}
            Ok(false) if departure => {
                // Surface departures are needed to keep the airport alive at startup. The
                // circuit may be unverified, but the taxi-out itself is on the airport surface
                // and the controller still gates the takeoff.
                scene.warnings.push(format!(
                    "{}: local Tower departure uses unverified terrain/obstacle protection",
                    airport.icao
                ));
            }
            Ok(false) => {
                scene.warnings.push(format!(
                    "{}: local Tower arrival lacks verified terrain/obstacle protection",
                    airport.icao
                ));
                continue;
            }
            Err(error) => {
                scene
                    .warnings
                    .push(format!("{}: local Tower traffic: {error}", airport.icao));
                continue;
            }
        }
        let identity = LOCAL_ID_BASE + added + 1;
        if scene.frames.iter().any(|frame| frame.target.id == identity)
            || input.observed.iter().any(|target| target.id == identity)
        {
            continue;
        }
        let Some(frame) = local_frame(
            input,
            identity,
            &callsign,
            &airline,
            &kind,
            aircraft,
            installed,
            &route,
            frequency_khz,
        ) else {
            continue;
        };
        if !scene
            .world
            .insert_advisory(generation, identity, route.clone(), 0)
        {
            scene.warnings.push(format!(
                "{}: local Tower traffic could not be admitted",
                airport.icao
            ));
            continue;
        }
        if let Some(kind) = aircraft.handling
            && let Some(movement) = handling.get(&kind)
        {
            scene.movement.insert(identity, movement.clone());
        }
        scene.advisories.insert(
            identity,
            Plan {
                id: identity,
                callsign,
                field: field.clone(),
                pattern: Arc::new(pattern),
                circuit,
                announced: BTreeSet::new(),
                due: None,
                go_around: None,
                straight_in: false,
                departure,
                cross: None,
                controlled: true,
                runway: runway.clone(),
                cleared: false,
            },
        );
        scene.frames.push(frame);
        if departure {
            departure_added = true;
        }
        added += 1;
    }
    if departure_added {
        // The spare routes use the same runway geometry as the checked active departure, so
        // their protection is already established. Only identity, callsign and model change.
        let mut callsigns = scene
            .frames
            .iter()
            .map(|frame| frame.target.callsign.clone())
            .collect::<BTreeSet<_>>();
        for index in 0..6u32 {
            seed = seed.wrapping_mul(16_777_619) ^ (seed >> 32);
            let Some((kind, model, airline)) = select_local_aircraft(input, runway_meters, seed)
            else {
                continue;
            };
            let Some(aircraft) = input.profiles.aircraft.get(&kind) else {
                continue;
            };
            let Some(installed) = input
                .models
                .models
                .iter()
                .find(|candidate| candidate.id == model)
                .cloned()
            else {
                continue;
            };
            let Some(callsign) = local_callsign(input, &airline, seed) else {
                continue;
            };
            if !callsigns.insert(callsign.clone()) {
                continue;
            }
            let Ok(pattern) = Pattern::departure(airport, &runway, &callsign, field.clone()) else {
                continue;
            };
            let circuit = pattern.circuit().clone();
            let route = circuit.route();
            let identity = LOCAL_ID_BASE + 16 + index;
            if scene.frames.iter().any(|frame| frame.target.id == identity)
                || input.observed.iter().any(|target| target.id == identity)
            {
                continue;
            }
            let Some(frame) = local_frame(
                input,
                identity,
                &callsign,
                &airline,
                &kind,
                aircraft,
                installed,
                &route,
                frequency_khz,
            ) else {
                continue;
            };
            let movement = aircraft
                .handling
                .and_then(|kind| handling.get(&kind))
                .cloned();
            scene.local_spares.push(LocalTemplate {
                plan: Plan {
                    id: identity,
                    callsign,
                    field: field.clone(),
                    pattern: Arc::new(pattern),
                    circuit,
                    announced: BTreeSet::new(),
                    due: None,
                    go_around: None,
                    straight_in: false,
                    departure: true,
                    cross: None,
                    controlled: true,
                    runway: runway.clone(),
                    cleared: false,
                },
                frame,
                movement,
            });
        }
    }
}

/// Publish advisory traffic at the untowered fields near the player.
pub(super) fn prepare(scene: &mut Scene, input: &Preparation) {
    if input.simulator_root.is_empty() {
        return;
    }
    let (handling, policy, _) = match policy::load(&input.config) {
        Ok(loaded) => loaded,
        Err(error) => {
            scene.warnings.push(format!("advisory traffic: {error}"));
            return;
        }
    };
    prepare_surface_departures(scene, input, &handling, &policy);
    prepare_tower(scene, input, &handling);
    if input.counts[3] == 0 {
        return;
    }
    let generation = scene.world.generation();
    let used = scene.flights.len().max(scene.frames.len());
    let capacity = advisory_capacity(used);
    if capacity == 0 {
        return;
    }
    let mut seed = input.epoch.wrapping_add(7);
    let fields = fields(input, &policy);
    for (index, id) in fields.iter().enumerate() {
        if scene.advisories.len() >= capacity {
            break;
        }
        let airport = match super::airport(&input.simulator_root, id) {
            Ok(airport) => airport,
            Err(error) => {
                scene
                    .warnings
                    .push(format!("{id}: advisory field: {error}"));
                continue;
            }
        };
        seed = seed.wrapping_mul(16_777_619) ^ (seed >> 32);
        let identity = ADVISORY_ID_BASE + u32::try_from(scene.advisories.len() + 1).unwrap_or(1);
        if scene.frames.iter().any(|frame| frame.target.id == identity)
            || input.observed.iter().any(|target| target.id == identity)
        {
            continue;
        }
        let mut cross = None;
        if index == 1
            && let Some(origin_id) = fields.first()
            && let Ok(origin) = super::airport(&input.simulator_root, origin_id)
        {
            let distance = openatc_core::arrival::distance_nm(
                [origin.reference_latitude, origin.reference_longitude],
                [airport.reference_latitude, airport.reference_longitude],
            );
            if (10.0..=40.0).contains(&distance)
                && let (Some(origin_runway), Some(destination_runway)) =
                    (longest_runway(&origin), longest_runway(&airport))
            {
                match plan_cross(
                    input,
                    &origin,
                    &origin_runway,
                    &airport,
                    &destination_runway,
                    seed,
                    identity,
                ) {
                    Ok(planned) => cross = Some(planned),
                    Err(error) => scene
                        .warnings
                        .push(format!("{origin_id}-{id}: cross-country: {error}")),
                }
            }
        }
        let (plan, frame, kind) = match cross {
            Some(planned) => planned,
            None => match plan(input, &airport, seed, identity, false, index > 0) {
                Ok(planned) => planned,
                Err(error) => {
                    scene
                        .warnings
                        .push(format!("{id}: advisory traffic: {error}"));
                    continue;
                }
            },
        };
        if !scene
            .world
            .insert_advisory(generation, identity, plan.route(), 0)
        {
            scene
                .warnings
                .push(format!("{id}: advisory traffic could not be admitted"));
            continue;
        }
        if let Some(kind) = kind
            && let Some(movement) = handling.get(&kind)
        {
            scene.movement.insert(identity, movement.clone());
        }
        scene.advisories.insert(identity, plan);
        scene.frames.push(frame);
    }
    if scene.advisories.len() < capacity
        && fields.len() == 1
        && let Some(id) = fields.first()
        && let Ok(airport) = super::airport(&input.simulator_root, id)
    {
        seed = seed.wrapping_mul(16_777_619) ^ (seed >> 32);
        let identity = ADVISORY_ID_BASE + u32::try_from(scene.advisories.len() + 1).unwrap_or(1);
        if !scene.frames.iter().any(|frame| frame.target.id == identity)
            && !input.observed.iter().any(|target| target.id == identity)
        {
            match plan(input, &airport, seed, identity, true, false) {
                Ok((plan, frame, kind)) => {
                    if scene
                        .world
                        .insert_advisory(generation, identity, plan.route(), 0)
                    {
                        if let Some(kind) = kind
                            && let Some(movement) = handling.get(&kind)
                        {
                            scene.movement.insert(identity, movement.clone());
                        }
                        scene.advisories.insert(identity, plan);
                        scene.frames.push(frame);
                    } else {
                        scene
                            .warnings
                            .push(format!("{id}: straight-in arrival could not be admitted"));
                    }
                }
                Err(error) => scene
                    .warnings
                    .push(format!("{id}: straight-in arrival: {error}")),
            }
        }
    }
}

/// Grant the next segment of every advisory procedure and continue the finished circuits.
pub(super) fn advance(runtime: &mut Runtime, blocked: &BTreeSet<String>) {
    if runtime.advisories.is_empty() {
        return;
    }
    let generation = runtime.world.generation();
    let poses = runtime.world.snapshot();
    for pose in &poses {
        if !runtime.world.is_advisory(pose.id) {
            continue;
        }
        let cleared = runtime
            .advisories
            .get(&pose.id)
            .is_none_or(|plan| !plan.controlled || plan.cleared);
        if pose.awaiting_clearance && cleared {
            runtime
                .world
                .authorise_advisory(generation, pose.id, pose.segment, blocked);
        }
    }
    // An occupied runway is never landed on: the aircraft goes around instead.
    for pose in &poses {
        if !runtime.world.is_advisory(pose.id) {
            continue;
        }
        let occupied = runtime
            .advisories
            .get(&pose.id)
            .is_some_and(|plan| runway_occupied(runtime, plan, blocked));
        if occupied {
            go_around(runtime, pose.id, pose.position);
        }
    }
    // A finished circuit is replaced by the next one, from the exact point it ended.
    let finished = runtime
        .world
        .snapshot()
        .into_iter()
        .filter(|pose| pose.complete && runtime.world.is_advisory(pose.id))
        .map(|pose| pose.id)
        .collect::<Vec<_>>();
    for id in finished {
        if runtime.advisories.get(&id).is_some_and(Plan::one_shot) {
            let local = is_local(id);
            runtime.advisories.remove(&id);
            runtime.frames.retain(|frame| frame.target.id != id);
            runtime.world.retire(id);
            if local {
                activate_local_spare(runtime);
            }
            continue;
        }
        let Some(plan) = runtime.advisories.get(&id) else {
            continue;
        };
        let Ok(circuit) = plan.pattern.next_circuit() else {
            continue;
        };
        if !runtime
            .world
            .continue_advisory(generation, id, circuit.route())
        {
            continue;
        }
        if let Some(plan) = runtime.advisories.get_mut(&id) {
            plan.circuit = circuit;
            plan.announced.clear();
            plan.go_around = None;
            plan.cleared = false;
        }
    }
}

/// Admit the next prepared local departure after a one-shot departure finishes.
fn activate_local_spare(runtime: &mut Runtime) {
    let Some(template) = runtime.local_spares.pop() else {
        return;
    };
    let generation = runtime.world.generation();
    if !runtime
        .world
        .insert_advisory(generation, template.plan.id, template.plan.route(), 0)
    {
        return;
    }
    if let Some(movement) = template.movement {
        runtime.movement.insert(template.plan.id, movement);
    }
    runtime.advisories.insert(template.plan.id, template.plan);
    runtime.frames.push(template.frame);
}

/// Whether the runway the pattern uses is held by another aircraft.
fn runway_occupied(runtime: &Runtime, plan: &Plan, blocked: &BTreeSet<String>) -> bool {
    let resource = plan.runway_resource();
    if blocked.contains(resource) {
        return true;
    }
    runtime
        .world
        .resource_owner(resource)
        .is_some_and(|owner| owner != plan.id)
}

/// Turn an aircraft away from an occupied runway and make the go-around call.
fn go_around(runtime: &mut Runtime, id: u32, position: openatc_core::traffic::motion::Waypoint) {
    let Some(plan) = runtime.advisories.get(&id) else {
        return;
    };
    let Some(segment) = plan.final_segment() else {
        return;
    };
    let Some(pose) = runtime
        .world
        .snapshot()
        .into_iter()
        .find(|pose| pose.id == id)
    else {
        return;
    };
    if pose.segment != segment {
        return;
    }
    let (circuit, destination) = match &plan.cross {
        Some(cross) => (
            cross.go_around_circuit(position),
            Some((
                cross.destination_pattern(),
                cross.destination_field().clone(),
            )),
        ),
        None => (plan.pattern.go_around_circuit(position), None),
    };
    let Ok(circuit) = circuit else {
        return;
    };
    let generation = runtime.world.generation();
    if !runtime
        .world
        .redirect_advisory(generation, id, circuit.route())
    {
        return;
    }
    if let Some(plan) = runtime.advisories.get_mut(&id) {
        if let Some((pattern, field)) = destination {
            plan.cross = None;
            plan.pattern = pattern;
            plan.field = field;
            plan.straight_in = false;
            plan.departure = false;
        }
        plan.circuit = circuit;
        plan.announced.clear();
        plan.go_around = Some(plan.pattern.go_around());
    }
}

/// Tower phrase for a controlled local leg, resolved from the speech TOML.
#[allow(clippy::match_same_arms)]
fn controlled_text(plan: &Plan, leg: &str) -> String {
    let key = match leg {
        "five_miles" => "tower_five_miles",
        "final" => "tower_final",
        "landing" => "tower_landing",
        "departing" => "tower_departing",
        "leaving" => "tower_leaving",
        "downwind" => "tower_downwind",
        "base" => "tower_base",
        "go_around" => "tower_go_around",
        _ => "tower_final",
    };
    openatc_core::dialogue::say(
        key,
        &[
            ("callsign", plan.callsign.clone()),
            ("runway", plan.runway.clone()),
        ],
    )
}

/// Speak the advisories made by the aircraft that are established on a leg.
pub(super) fn announce(state: &mut EngineState) {
    if state.generated.advisories.is_empty() || state.generated.simulation_job.is_some() {
        return;
    }
    let poses = state.generated.world.snapshot();
    let mut pending = Vec::new();
    for (id, plan) in &state.generated.advisories {
        let Some(pose) = poses.iter().find(|pose| pose.id == *id) else {
            continue;
        };
        if plan.due.is_some_and(|due| Instant::now() < due) {
            continue;
        }
        // A go-around call is made instead of the call for the leg the aircraft abandoned.
        if let Some(advisory) = &plan.go_around {
            let text = if plan.controlled {
                controlled_text(plan, "go_around")
            } else {
                advisory.text.clone()
            };
            pending.push((
                *id,
                None,
                plan.callsign.clone(),
                text,
                plan.field_for(pose.segment).frequency_khz,
                None,
            ));
            continue;
        }
        if plan.announced.contains(&pose.segment) {
            continue;
        }
        let Some(advisory) = plan.advisory(pose.segment) else {
            continue;
        };
        let text = if plan.controlled {
            controlled_text(plan, advisory.leg)
        } else {
            advisory.text.clone()
        };
        let clearance = if plan.controlled {
            match advisory.leg {
                "downwind" | "five_miles" => Some("tower_clear_land"),
                "departing" => Some("tower_clear_takeoff"),
                _ => None,
            }
        } else {
            None
        };
        pending.push((
            *id,
            Some(pose.segment),
            plan.callsign.clone(),
            text,
            plan.field_for(pose.segment).frequency_khz,
            clearance,
        ));
    }
    let Some((id, segment, callsign, text, frequency, clearance)) = pending.into_iter().next()
    else {
        return;
    };
    // The call is made on the leg the aircraft is established on, whether or not anybody is
    // listening; a later arrival on the frequency does not repeat it.
    let spoken_text = state.generated.spoken_text(&text);
    let rate = state
        .generated
        .policy
        .as_ref()
        .map_or(12.0, |policy| policy.radio_characters_per_second);
    let gap = state
        .generated
        .policy
        .as_ref()
        .map_or(0.4, |policy| policy.radio_gap_seconds);
    let runway = state
        .generated
        .advisories
        .get(&id)
        .map_or_else(String::new, |plan| plan.runway.clone());
    if let Some(plan) = state.generated.advisories.get_mut(&id) {
        match segment {
            Some(segment) => {
                plan.announced.insert(segment);
            }
            None => {
                plan.go_around = None;
            }
        }
        if clearance.is_some() {
            plan.cleared = true;
        }
        plan.due = Some(
            Instant::now()
                + Duration::from_secs_f64(spoken_text.chars().count() as f64 / rate + gap),
        );
    }
    if !state.settings.hear_traffic_radio || !state.session.telemetry.radio_power {
        return;
    }
    let Some(radio) = state.session.receiving_radio(frequency) else {
        return;
    };
    let mut tag = SpeechTag {
        position: "Traffic".into(),
        voice: crate::traffic::pilot_voice(state, id),
        ..Default::default()
    };
    tag.radio = Some(radio);
    tag.frequency = Some(frequency);
    let spoken_callsign = state.generated.spoken_text(&callsign);
    add_transmission(&mut state.session, &spoken_callsign, &spoken_text, &tag);
    if let Some(entry) = state.session.transcript.last_mut() {
        entry.background = true;
        entry.information_only = true;
    }
    if let Some(clearance) = clearance {
        let text = openatc_core::dialogue::say(
            clearance,
            &[("callsign", callsign.clone()), ("runway", runway.clone())],
        );
        if !text.is_empty() {
            let spoken = state.generated.spoken_text(&text);
            let station = state
                .stations
                .iter()
                .find(|station| {
                    station.khz == frequency
                        && (station.service.eq_ignore_ascii_case("Tower")
                            || station
                                .services
                                .iter()
                                .any(|service| service.eq_ignore_ascii_case("Tower")))
                })
                .cloned();
            let mut tag = if let Some(station) = station {
                crate::flight::tag(state, &station)
            } else {
                SpeechTag {
                    position: "Tower".into(),
                    voice: state.settings.voice.clone(),
                    ..Default::default()
                }
            };
            tag.radio = Some(radio);
            tag.frequency = Some(frequency);
            add_transmission(&mut state.session, "Tower", &spoken, &tag);
            if let Some(entry) = state.session.transcript.last_mut() {
                entry.background = true;
                entry.information_only = true;
            }
            let readback_key = if clearance == "tower_clear_land" {
                "tower_readback_land"
            } else {
                "tower_readback_takeoff"
            };
            let readback = openatc_core::dialogue::say(
                readback_key,
                &[("callsign", callsign), ("runway", runway)],
            );
            if !readback.is_empty() {
                let spoken_readback = state.generated.spoken_text(&readback);
                let mut tag = SpeechTag {
                    position: "Traffic".into(),
                    voice: crate::traffic::pilot_voice(state, id),
                    ..Default::default()
                };
                tag.radio = Some(radio);
                tag.frequency = Some(frequency);
                add_transmission(&mut state.session, &spoken_callsign, &spoken_readback, &tag);
                if let Some(entry) = state.session.transcript.last_mut() {
                    entry.background = true;
                    entry.information_only = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::traffic::csl::{Match, Model};

    fn station(
        airport: &str,
        service: &str,
        khz: i32,
        latitude: f64,
    ) -> openatc_core::stations::Station {
        openatc_core::stations::Station {
            airport: airport.into(),
            service: service.into(),
            khz,
            latitude,
            longitude: -90.0,
            ..Default::default()
        }
    }

    fn preparation(stations: Vec<openatc_core::stations::Station>) -> Preparation {
        let mut models = Catalogue::default();
        models.models.push(Model {
            id: "bluebell/c172".into(),
            objects: Vec::new(),
            vertical_offset: None,
            matches: vec![Match {
                aircraft: "C172".into(),
                operator: String::new(),
                livery: String::new(),
            }],
        });
        Preparation {
            stations: Arc::from(stations),
            regions: BTreeMap::new(),
            simulator_root: String::new(),
            origin: Airport::default(),
            destination: Airport::default(),
            profiles: Arc::new(Profiles::builtin().unwrap()),
            models: Arc::new(models),
            counts: [0, 0, 0, 2],
            own: [35.0, -90.0],
            observed: Vec::new(),
            reserved_stand_cells: BTreeSet::new(),
            reserved_surface_corridors: BTreeSet::new(),
            reserved_stands: BTreeSet::new(),
            epoch: 1,
            seed: 1,
            startup_cast: false,
            taxi_target: 4,
            force_runway: false,
            force_arrival_runway: false,
            runway: String::new(),
            arrival_runway: String::new(),
            config: std::path::PathBuf::new(),
            weather: BTreeMap::new(),
            weather_observations: None,
            local_pattern_traffic: false,
            seed_single_arrival: false,
            player_airborne: false,
            minimum_airborne_spawn_nm: 0.0,
            airport_codes: BTreeMap::new(),
        }
    }

    fn policy() -> Operations {
        super::super::parse_operations(include_str!("../../../../traffic/operations.toml")).unwrap()
    }

    #[test]
    fn tower_frequency_ignores_ground_and_ctaf_channels() {
        let input = preparation(vec![
            station("KTOW", "Ground", 121_700, 35.0),
            station("KTOW", "Tower", 118_300, 35.0),
            station("KTOW", "CTAF", 122_800, 35.0),
        ]);
        let airport = Airport {
            icao: "KTOW".into(),
            ..Default::default()
        };
        assert_eq!(tower_frequency(&airport, &input.stations), Some(118_300));
    }

    #[test]
    fn controlled_calls_use_tower_phrases_from_the_speech_toml() {
        let airport = openatc_core::airport::demo_airport();
        let field = Field {
            airport: airport.icao.clone(),
            frequency_khz: 118_300,
        };
        let pattern = Pattern::straight_in(&airport, "09", "TEST", field.clone()).unwrap();
        let circuit = pattern.circuit().clone();
        let plan = Plan {
            id: 0x00d1_0001,
            callsign: "TEST".into(),
            field,
            pattern: Arc::new(pattern),
            circuit,
            announced: BTreeSet::new(),
            due: None,
            go_around: None,
            straight_in: true,
            departure: false,
            cross: None,
            controlled: true,
            runway: "09".into(),
            cleared: false,
        };
        let text = controlled_text(&plan, "five_miles");
        assert!(text.starts_with("Tower, TEST"), "{text}");
        assert!(text.contains("five miles final runway 09"), "{text}");
    }

    #[test]
    #[ignore = "requires installed LFMN scenery and CSL models"]
    fn installed_local_tower_patterns_pass_the_physical_scenery_gates() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let airport = super::airport(&root, "LFMN").unwrap();
        let stations: Vec<_> = openatc_core::stations::load_index(&root)
            .unwrap()
            .into_iter()
            .filter(|station| station.airport == "LFMN")
            .collect();
        let mut input = preparation(stations);
        input.origin = airport.clone();
        input.simulator_root = root.clone();
        input.models = Arc::new(
            Catalogue::scan(
                &std::path::Path::new(&root).join("Resources/plugins/OpenATC/Resources/CSL"),
            )
            .unwrap(),
        );
        let frequency = tower_frequency(&airport, &input.stations).expect("LFMN Tower");
        let (handling, _, _) = policy::load(std::path::Path::new("")).unwrap();
        let mut scene = Scene {
            require_complete_flights: false,
            surface_corridors: Arc::new(BTreeSet::new()),
            airports: BTreeMap::new(),
            frames: Vec::new(),
            world: Default::default(),
            movement: BTreeMap::new(),
            airport: airport.clone(),
            destination: Airport::default(),
            departure_procedures: None,
            arrival_procedures: None,
            terminal_points: None,
            published_holds: None,
            nominal_departures: BTreeMap::new(),
            decoded_terminals: BTreeMap::new(),
            nominal_arrivals: BTreeMap::new(),
            nominal_flights: BTreeMap::new(),
            flights: BTreeMap::new(),
            spoken_callsigns: BTreeMap::new(),
            terminal_routes: Vec::new(),
            environments: BTreeMap::new(),
            policy: policy(),
            fixed_gear: BTreeSet::new(),
            channels: BTreeMap::new(),
            orders: BTreeMap::new(),
            turnaround_inputs: Arc::new(BTreeMap::new()),
            stations: input.stations.clone(),
            regions: Arc::new(BTreeMap::new()),
            mora: Arc::new(Default::default()),
            simulator_root: root.clone(),
            reserved_stands: BTreeSet::new(),
            occupied_stand_cells: Arc::new(BTreeSet::new()),
            arrival_refusals: 0,
            warnings: Vec::new(),
            seeded_stations: BTreeMap::new(),
            advisories: BTreeMap::new(),
            local_spares: Vec::new(),
        };
        prepare_tower(&mut scene, &input, &handling);
        assert_eq!(scene.frames.len(), 3, "{:?}", scene.warnings);
        assert_eq!(scene.advisories.len(), 3);
        assert!(
            !scene.local_spares.is_empty(),
            "local departures need replacements"
        );
        let callsigns = scene
            .frames
            .iter()
            .map(|frame| frame.target.callsign.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(callsigns.len(), 3, "local aircraft need distinct callsigns");
        assert!(scene.advisories.values().all(|plan| plan.controlled));
        assert!(
            scene
                .frames
                .iter()
                .all(|frame| frame.target.com1_khz == Some(frequency))
        );
        // Drive the three local procedures through clearance and movement. The circuit must
        // reach a landing roll, both initial departures must finish, and spare departures must
        // take their place without rebuilding the scene.
        let circuit = scene
            .advisories
            .values()
            .find(|plan| !plan.departure)
            .map(|plan| plan.id)
            .expect("local circuit");
        let initial_departures = scene
            .advisories
            .values()
            .filter(|plan| plan.departure)
            .map(|plan| plan.id)
            .collect::<BTreeSet<_>>();
        let initial_spares = scene.local_spares.len();
        let mut runtime = Runtime {
            advisories: scene.advisories,
            world: scene.world,
            local_spares: scene.local_spares,
            ..Runtime::default()
        };
        for plan in runtime.advisories.values_mut() {
            plan.cleared = true;
        }
        let mut landed = false;
        for step in 1..=2_000 {
            advance(&mut runtime, &BTreeSet::new());
            runtime.world.advance(f64::from(step), false);
            let poses = runtime.world.snapshot();
            landed |= poses.iter().any(|pose| {
                pose.id == circuit
                    && matches!(pose.stage, openatc_core::traffic::motion::Stage::Rollout)
            });
            let removed = initial_departures
                .iter()
                .filter(|id| !runtime.advisories.contains_key(id))
                .count();
            if landed && removed == initial_departures.len() {
                break;
            }
            assert!(step < 2_000, "local traffic did not complete");
        }
        assert!(landed, "the local circuit never reached a landing roll");
        assert_eq!(
            initial_departures
                .iter()
                .filter(|id| !runtime.advisories.contains_key(id))
                .count(),
            initial_departures.len(),
            "both initial one-shot departures should be removed"
        );
        assert!(
            runtime.local_spares.len() < initial_spares,
            "a spare departure should have replaced a finished one"
        );
        assert!(runtime.advisories.contains_key(&circuit));
        let runway = longest_runway(&airport).expect("LFMN runway");
        let runway_meters = airport
            .runways
            .iter()
            .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)
            .map_or(0.0, |candidate| {
                (candidate.second.east - candidate.first.east)
                    .hypot(candidate.second.north - candidate.first.north)
            });
        let (kind, model, airline) =
            select_aircraft(&input, runway_meters, 7).expect("installed light aircraft");
        let callsign = input.profiles.airlines[&airline]
            .registration("LFMN", 7)
            .expect("LFMN registration");
        let aircraft = input
            .profiles
            .aircraft
            .get(&kind)
            .expect("aircraft profile");
        let installed = input
            .models
            .models
            .iter()
            .find(|candidate| candidate.id == model)
            .cloned()
            .expect("installed model");
        let field = Field {
            airport: "LFMN".into(),
            frequency_khz: frequency,
        };
        let mut terrain = super::scenery::Terrain::new(std::path::Path::new(&root)).unwrap();
        terrain.begin_check();
        for pattern in [
            Pattern::new(&airport, &runway, &callsign, field.clone()).unwrap(),
            Pattern::departure(&airport, &runway, &callsign, field).unwrap(),
        ] {
            let route = pattern.circuit().route();
            assert!(
                local_route_clear(&mut terrain, &route, aircraft.wingspan / 2.0 + 10.0).unwrap(),
                "local pattern lacks verified protection"
            );
            let frame = local_frame(
                &input,
                0x00d1_0001,
                &callsign,
                &airline,
                &kind,
                aircraft,
                installed.clone(),
                &route,
                frequency,
            )
            .expect("local frame");
            assert!(!frame.target.on_ground);
            assert_eq!(frame.target.com1_khz, Some(frequency));
        }
    }

    #[test]
    fn advisory_traffic_fits_inside_the_scene_cap() {
        assert_eq!(advisory_capacity(0), 2);
        assert_eq!(advisory_capacity(61), 2);
        assert_eq!(advisory_capacity(62), 1);
        assert_eq!(advisory_capacity(63), 0);
        assert_eq!(advisory_capacity(70), 0);
    }

    #[test]
    fn only_untowered_fields_near_the_player_are_selected() {
        let input = preparation(vec![
            station("KCTF", "CTAF", 122_900, 35.1),
            station("KTOW", "Tower", 118_300, 35.1),
            station("KTOW", "CTAF", 122_800, 35.1),
            station("KFAR", "CTAF", 122_700, 70.0),
        ]);
        assert_eq!(fields(&input, &policy()), vec!["KCTF".to_owned()]);
    }

    #[test]
    fn controlled_local_traffic_waits_for_its_tower_clearance() {
        let mut airport = openatc_core::airport::demo_airport();
        airport.icao = "KCTF".into();
        let input = preparation(vec![station("KCTF", "CTAF", 122_900, 35.0)]);
        let (mut plan, _, _) = plan(&input, &airport, 7, 0x00d1_0001, false, false).unwrap();
        plan.controlled = true;
        plan.cleared = false;
        plan.runway = "09".into();
        let id = plan.id;
        let mut runtime = Runtime::default();
        let generation = runtime.world.generation();
        assert!(
            runtime
                .world
                .insert_advisory(generation, id, plan.route(), 0)
        );
        runtime.advisories.insert(id, plan);
        advance(&mut runtime, &BTreeSet::new());
        let before = runtime
            .world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == id)
            .expect("local pose");
        assert!(before.awaiting_clearance);
        runtime.world.advance(1.0, false);
        let held = runtime
            .world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == id)
            .expect("held pose");
        assert_eq!(held.segment, before.segment);
        runtime.advisories.get_mut(&id).unwrap().cleared = true;
        advance(&mut runtime, &BTreeSet::new());
        runtime.world.advance(1.0, false);
        let moved = runtime
            .world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == id)
            .expect("cleared pose");
        assert!(
            moved.segment > before.segment || !moved.awaiting_clearance,
            "controlled traffic did not move after its clearance: {moved:?}"
        );
    }

    #[test]
    fn advisory_traffic_is_admitted_and_flown_without_a_controller() {
        let mut airport = openatc_core::airport::demo_airport();
        airport.icao = "KCTF".into();
        let input = preparation(vec![station("KCTF", "CTAF", 122_900, 35.0)]);
        let (plan, frame, kind) = plan(&input, &airport, 7, 0x00d0_0001, false, false).unwrap();
        assert_eq!(frame.target.id, 0x00d0_0001);
        assert_eq!(frame.target.com1_khz, Some(122_900));
        assert!(!frame.target.on_ground);
        assert!(!frame.target.callsign.is_empty());
        assert!(
            kind.is_some(),
            "advisory traffic keeps its propeller animation"
        );

        let mut runtime = Runtime::default();
        let generation = runtime.world.generation();
        assert!(
            runtime
                .world
                .insert_advisory(generation, plan.id, plan.route(), 0)
        );
        runtime.advisories.insert(plan.id, plan);
        // The published procedure is the authority, so the aircraft moves on its own.
        advance(&mut runtime, &BTreeSet::new());
        runtime.world.advance(0.0, false);
        runtime.world.advance(30.0, false);
        let pose = runtime
            .world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == 0x00d0_0001)
            .unwrap();
        assert!(pose.knots > 0.0);
        assert_eq!(pose.stage, openatc_core::traffic::motion::Stage::Descent);
        assert!(!pose.awaiting_clearance);
    }

    #[test]
    fn an_occupied_runway_is_never_landed_on() {
        let mut airport = openatc_core::airport::demo_airport();
        airport.icao = "KCTF".into();
        let input = preparation(vec![station("KCTF", "CTAF", 122_900, 35.0)]);
        let (plan, _, _) = plan(&input, &airport, 7, 0x00d0_0003, false, false).unwrap();
        let mut runtime = Runtime::default();
        let generation = runtime.world.generation();
        assert!(
            runtime
                .world
                .insert_advisory(generation, plan.id, plan.route(), 0)
        );
        runtime.advisories.insert(plan.id, plan.clone());
        // Fly to the final approach.
        let mut clock = 0.0;
        for _ in 0..600 {
            let pose = runtime
                .world
                .snapshot()
                .into_iter()
                .find(|pose| pose.id == plan.id)
                .unwrap();
            if pose.segment == 2 {
                break;
            }
            if pose.awaiting_clearance {
                assert!(runtime.world.authorise_advisory(
                    generation,
                    plan.id,
                    pose.segment,
                    &BTreeSet::new()
                ));
            }
            clock += 1.0;
            runtime.world.advance(clock, false);
        }
        let pose = runtime
            .world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == plan.id)
            .unwrap();
        assert_eq!(
            pose.segment, 2,
            "the aircraft is established on the final approach"
        );
        // An occupied runway turns the aircraft away instead of landing.
        let occupied = BTreeSet::from([plan.runway_resource().to_owned()]);
        advance(&mut runtime, &occupied);
        let turned = runtime
            .world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == plan.id)
            .unwrap();
        assert_eq!(turned.stage, openatc_core::traffic::motion::Stage::Climb);
        let published = runtime.advisories.get(&plan.id).unwrap();
        assert!(published.go_around.is_some());
        assert!(
            published
                .go_around
                .as_ref()
                .is_some_and(|advisory| advisory.text.contains("going around"))
        );
    }
    #[test]
    fn a_finished_straight_in_is_removed_instead_of_repeating() {
        let mut airport = openatc_core::airport::demo_airport();
        airport.icao = "KCTF".into();
        let input = preparation(vec![station("KCTF", "CTAF", 122_900, 35.0)]);
        let (plan, _, _) = plan(&input, &airport, 11, 0x00d0_0004, true, false).unwrap();
        assert!(plan.straight_in);
        assert!(plan.pattern.is_straight_in());
        let mut runtime = Runtime::default();
        let generation = runtime.world.generation();
        assert!(
            runtime
                .world
                .insert_advisory(generation, plan.id, plan.route(), 0)
        );
        runtime.advisories.insert(plan.id, plan.clone());
        for step in 1..=600 {
            advance(&mut runtime, &BTreeSet::new());
            runtime.world.advance(f64::from(step), false);
            let pose = runtime
                .world
                .snapshot()
                .into_iter()
                .find(|pose| pose.id == plan.id)
                .unwrap();
            if pose.complete {
                break;
            }
        }
        advance(&mut runtime, &BTreeSet::new());
        assert!(!runtime.advisories.contains_key(&plan.id));
        assert!(
            !runtime
                .world
                .snapshot()
                .iter()
                .any(|pose| pose.id == plan.id)
        );
    }

    #[test]
    fn a_finished_departure_is_removed_instead_of_repeating() {
        let mut airport = openatc_core::airport::demo_airport();
        airport.icao = "KCTF".into();
        let input = preparation(vec![station("KCTF", "CTAF", 122_900, 35.0)]);
        let (plan, _, _) = plan(&input, &airport, 13, 0x00d0_0005, false, true).unwrap();
        assert!(plan.departure);
        assert!(plan.pattern.is_departure());
        let mut runtime = Runtime::default();
        let generation = runtime.world.generation();
        assert!(
            runtime
                .world
                .insert_advisory(generation, plan.id, plan.route(), 0)
        );
        runtime.advisories.insert(plan.id, plan.clone());
        for step in 1..=600 {
            advance(&mut runtime, &BTreeSet::new());
            runtime.world.advance(f64::from(step), false);
            let pose = runtime
                .world
                .snapshot()
                .into_iter()
                .find(|pose| pose.id == plan.id)
                .unwrap();
            if pose.complete {
                break;
            }
        }
        advance(&mut runtime, &BTreeSet::new());
        assert!(!runtime.advisories.contains_key(&plan.id));
        assert!(
            !runtime
                .world
                .snapshot()
                .iter()
                .any(|pose| pose.id == plan.id)
        );
    }

    #[tokio::test]
    async fn completed_advisory_metadata_is_published_with_the_movement_sample() {
        let mut airport = openatc_core::airport::demo_airport();
        airport.icao = "KCTF".into();
        let input = preparation(vec![station("KCTF", "CTAF", 122_900, 35.0)]);
        let (plan, frame, _) = plan(&input, &airport, 13, 0x00d0_0005, false, true).unwrap();
        let id = plan.id;
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.generated.epoch = Some(state.session_generation);
        state.generated.policy = Some(policy());
        state.generated.airport = Some(Arc::new(airport));
        assert!(
            state
                .generated
                .world
                .insert_advisory(0, id, plan.route(), 0)
        );
        state.generated.advisories.insert(id, plan);
        state.generated.frames.push(frame);
        state.generated.simulation_job = Some(1);
        announce(&mut state);
        assert!(state.generated.advisories[&id].announced.is_empty());
        state.generated.simulation_job = None;
        for second in 0..=600 {
            advance(&mut state.generated, &BTreeSet::new());
            state.generated.world.advance(f64::from(second), false);
            if state.generated.world.snapshot()[0].complete {
                break;
            }
        }
        assert!(state.generated.world.snapshot()[0].complete);
        state.session.telemetry.simulation_seconds = Some(601.0);
        let shared = Arc::new(tokio::sync::RwLock::new(state));
        super::super::simulate(shared.clone()).await;
        let state = shared.read().await;
        assert!(!state.generated.advisories.contains_key(&id));
        assert!(
            !state
                .generated
                .frames
                .iter()
                .any(|frame| frame.target.id == id)
        );
        assert!(state.generated.world.current_segment(id).is_none());
    }

    #[test]
    fn a_cross_country_plan_carries_both_fields_and_is_removed_after_landing() {
        let mut origin = openatc_core::airport::demo_airport();
        origin.icao = "KCTF".into();
        let mut destination = origin.clone();
        destination.icao = "KDST".into();
        destination.reference_latitude += 0.25;
        let input = preparation(vec![
            station("KCTF", "CTAF", 122_900, 35.0),
            station("KDST", "CTAF", 123_000, 35.0),
        ]);
        let origin_runway = origin.runways[0].first_name.clone();
        let destination_runway = destination.runways[0].first_name.clone();
        let (plan, frame, _) = plan_cross(
            &input,
            &origin,
            &origin_runway,
            &destination,
            &destination_runway,
            17,
            0x00d0_0006,
        )
        .unwrap();
        assert!(plan.cross.is_some());
        assert_eq!(plan.field_for(0).airport, "KCTF");
        assert_eq!(plan.field_for(0).frequency_khz, 122_900);
        assert_eq!(plan.field_for(3).airport, "KDST");
        assert_eq!(plan.field_for(3).frequency_khz, 123_000);
        assert_eq!(frame.target.com1_khz, Some(122_900));
        let mut runtime = Runtime::default();
        let generation = runtime.world.generation();
        assert!(
            runtime
                .world
                .insert_advisory(generation, plan.id, plan.route(), 0)
        );
        runtime.advisories.insert(plan.id, plan.clone());
        for step in 1..=2000 {
            advance(&mut runtime, &BTreeSet::new());
            runtime.world.advance(f64::from(step), false);
            let pose = runtime
                .world
                .snapshot()
                .into_iter()
                .find(|pose| pose.id == plan.id)
                .unwrap();
            if pose.complete {
                break;
            }
        }
        advance(&mut runtime, &BTreeSet::new());
        assert!(!runtime.advisories.contains_key(&plan.id));
        assert!(
            !runtime
                .world
                .snapshot()
                .iter()
                .any(|pose| pose.id == plan.id)
        );
    }

    #[test]
    fn an_occupied_destination_turns_a_cross_country_into_a_local_go_around() {
        let mut origin = openatc_core::airport::demo_airport();
        origin.icao = "KCTF".into();
        let mut destination = origin.clone();
        destination.icao = "KDST".into();
        destination.reference_latitude += 0.25;
        let input = preparation(vec![
            station("KCTF", "CTAF", 122_900, 35.0),
            station("KDST", "CTAF", 123_000, 35.0),
        ]);
        let origin_runway = origin.runways[0].first_name.clone();
        let destination_runway = destination.runways[0].first_name.clone();
        let (plan, _, _) = plan_cross(
            &input,
            &origin,
            &origin_runway,
            &destination,
            &destination_runway,
            17,
            0x00d0_0007,
        )
        .unwrap();
        let landing = plan
            .cross
            .as_ref()
            .unwrap()
            .landing_runway_resource()
            .to_owned();
        let mut runtime = Runtime::default();
        let generation = runtime.world.generation();
        assert!(
            runtime
                .world
                .insert_advisory(generation, plan.id, plan.route(), 0)
        );
        runtime.advisories.insert(plan.id, plan.clone());
        for step in 1..=2000 {
            let pose = runtime
                .world
                .snapshot()
                .into_iter()
                .find(|pose| pose.id == plan.id)
                .unwrap();
            if pose.segment == 4 {
                break;
            }
            advance(&mut runtime, &BTreeSet::new());
            runtime.world.advance(f64::from(step), false);
        }
        let pose = runtime
            .world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == plan.id)
            .unwrap();
        assert_eq!(pose.segment, 4, "established on the destination final");
        advance(&mut runtime, &BTreeSet::from([landing]));
        let switched = runtime.advisories.get(&plan.id).unwrap();
        assert!(switched.cross.is_none());
        assert!(switched.pattern.runway_resource().contains("KDST"));
        assert!(switched.go_around.is_some());
        let pose = runtime
            .world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == plan.id)
            .unwrap();
        assert_eq!(pose.stage, openatc_core::traffic::motion::Stage::Climb);
    }

    #[test]
    fn a_finished_advisory_circuit_is_continued_from_the_same_point() {
        let mut airport = openatc_core::airport::demo_airport();
        airport.icao = "KCTF".into();
        let input = preparation(vec![station("KCTF", "CTAF", 122_900, 35.0)]);
        let (plan, _, _) = plan(&input, &airport, 7, 0x00d0_0002, false, false).unwrap();
        let mut runtime = Runtime::default();
        let generation = runtime.world.generation();
        assert!(
            runtime
                .world
                .insert_advisory(generation, plan.id, plan.route(), 0)
        );
        runtime.advisories.insert(plan.id, plan.clone());
        // Fly the whole circuit, granting each leg as the procedure reaches it.
        for step in 1..=600 {
            advance(&mut runtime, &BTreeSet::new());
            runtime.world.advance(f64::from(step), false);
            let pose = runtime
                .world
                .snapshot()
                .into_iter()
                .find(|pose| pose.id == plan.id)
                .unwrap();
            if pose.complete {
                break;
            }
        }
        let pose = runtime
            .world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == plan.id)
            .unwrap();
        assert!(pose.complete, "the circuit ends on the landing roll");
        // The next circuit starts where the landing roll ended.
        advance(&mut runtime, &BTreeSet::new());
        let continued = runtime
            .world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == plan.id)
            .unwrap();
        assert!(!continued.complete);
        assert_eq!(continued.segment, 0);
        assert_eq!(continued.stage, openatc_core::traffic::motion::Stage::Climb);
        let expected = plan
            .pattern
            .next_circuit()
            .unwrap()
            .route()
            .start_position();
        assert!((continued.position.latitude - expected.latitude).abs() < 1e-6);
        assert!((continued.position.longitude - expected.longitude).abs() < 1e-6);
    }
}
