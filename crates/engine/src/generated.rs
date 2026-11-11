//! Bounded worker preparation of model-compatible traffic. No simulator calls live here.

#![allow(clippy::default_trait_access)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use crate::{EngineState, Shared};
use openatc_core::{
    airport::Airport,
    state::{GeneratedTraffic, Point, TrafficTarget},
    traffic::{
        animation::{self, Animation},
        csl::Catalogue,
        profiles::{Category, Port, Profiles},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};
mod airborne;
mod airport_source;
mod assembly;
mod controller;
mod ctaf;
mod departure;
mod display;
mod enroute;
mod environment;
mod flight;
mod landing;
mod network;
mod policy;
mod population;
mod radio;
mod recovery;
mod routing;
mod runway;
pub(crate) mod scenery;
mod separation;
mod sequencing;
mod turnaround;
pub use radio::tick as radio_tick;

/// Stable apron identities, below the production allocator's `0xF00000` range and
/// the 24-bit TCAS limit. IDs above that limit are dropped by the traffic display.
#[cfg(test)]
const APRON_ID_BASE: u32 = 0x00A0_0000;
const APRON_MATCH_METERS: f64 = 15.0;

/// Process-wide generated-traffic diagnostics level, set from Settings each tick.
static TRAFFIC_LOG: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

pub(crate) fn set_traffic_log(level: openatc_settings::TrafficLog) {
    use openatc_settings::TrafficLog;
    let value = match level {
        TrafficLog::Off => 0,
        TrafficLog::Summary => 1,
        TrafficLog::Detail => 2,
    };
    TRAFFIC_LOG.store(value, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn traffic_log(level: openatc_settings::TrafficLog, message: impl AsRef<str>) {
    use openatc_settings::TrafficLog;
    let configured = TRAFFIC_LOG.load(std::sync::atomic::Ordering::Relaxed);
    let wanted = match level {
        TrafficLog::Off => 0,
        TrafficLog::Summary => 1,
        TrafficLog::Detail => 2,
    };
    if configured >= wanted && wanted > 0 {
        eprintln!("Generated traffic: {}", message.as_ref());
    }
}

#[derive(Default, Clone)]
#[allow(clippy::struct_excessive_bools)] // Preview and flight flags are independent runtime states.
pub struct Runtime {
    key: String,
    population: population::Population,
    queued_scene: Option<(Arc<Scene>, String, u64)>,
    apron_key: Option<String>,
    apron_ids: BTreeSet<u32>,
    apron_routes: BTreeMap<u32, Arc<openatc_core::traffic::motion::Route>>,
    apron_segments: BTreeMap<u32, usize>,
    apron_radio: BTreeSet<u32>,
    /// A gate departure has asked for its IFR clearance but has not read it back yet.
    delivery_pending: BTreeSet<u32>,
    /// A gate departure has read back its IFR clearance and may start and push back.
    delivery_cleared: BTreeSet<u32>,
    /// A ground aircraft was told to hold and its blocking aircraft, so it can be released.
    held: BTreeMap<u32, u32>,
    /// A re-plan has already been requested for the current airborne phase.
    airborne_reseeded: bool,
    preparation_job: Option<u64>,
    retry: Option<Instant>,
    /// Diagnostics verbosity, refreshed from settings every telemetry tick.
    log: openatc_settings::TrafficLog,
    /// A local scene is live; plan the wider network once and merge it.
    network_pending: bool,
    /// Ground poses already described in the detail log.
    detail_logged: BTreeSet<(u32, usize, bool)>,
    frames: Vec<GeneratedTraffic>,
    visible: BTreeSet<u32>,
    /// Frames this runtime last placed in the session traffic set.
    published_ids: BTreeSet<u32>,
    world: openatc_core::traffic::motion::World,
    pub(crate) airport: Option<Arc<Airport>>,
    pub(crate) destination: Option<Arc<Airport>>,
    airports: BTreeMap<String, Arc<Airport>>,
    network: Option<Arc<network::Plan>>,
    require_complete_flights: bool,
    departure_procedures: Option<Arc<openatc_core::traffic::procedures::Catalogue>>,
    arrival_procedures: Option<Arc<openatc_core::traffic::procedures::Catalogue>>,
    terminal_points: Option<Arc<openatc_core::traffic::coordinates::Points>>,
    published_holds: Option<Arc<openatc_core::traffic::holding::published::Catalogue>>,
    nominal_departures: BTreeMap<u32, Arc<departure::Surface>>,
    decoded_terminals: BTreeMap<u32, Arc<airborne::Decoded>>,
    nominal_arrivals: BTreeMap<u32, Arc<landing::Arrival>>,
    nominal_flights: BTreeMap<u32, Arc<assembly::Draft>>,
    spoken_callsigns: BTreeMap<String, String>,
    movement: BTreeMap<u32, animation::Handling>,
    previous_time: Option<f64>,
    observed_climbs: BTreeMap<u32, f64>,
    epoch: Option<u64>,
    pub status: String,
    policy: Option<Operations>,
    fixed_gear: BTreeSet<u32>,
    simulation_job: Option<u64>,
    channels: BTreeMap<String, animation::Channel>,
    orders: BTreeMap<u32, Vec<(usize, openatc_core::state::TaxiClearance)>>,
    approved_orders: BTreeSet<(u32, usize)>,
    departure_wake: BTreeMap<u32, f64>,
    /// Preserve the cancellation reason until the controller can transmit (true means wake).
    cancelled_takeoffs: BTreeMap<u32, bool>,
    /// Order in which departures reached the holding point, so the runway queue does not
    /// reorder itself when two taxiways join the runway at different distances.
    departure_order: BTreeMap<u32, u64>,
    departure_sequence: u64,
    cancelled_landings: BTreeSet<(u32, usize)>,
    exchange: Option<radio::Exchange>,
    /// A delivery clearance can proceed while Ground or Tower owns the primary exchange.
    delivery_exchange: Option<radio::Exchange>,
    last_speed_check: Option<f64>,
    last_speed_aircraft: u32,
    reported_crossings: BTreeSet<(u32, usize)>,
    reported_recoveries: BTreeSet<(u32, usize)>,
    warnings: Vec<String>,
    reported_holds: BTreeSet<(u32, usize)>,
    /// Last logged hold reason per clearance, so a changed cause is attributable without
    /// repeating the same line every tick.
    hold_reasons: BTreeMap<(u32, usize), String>,
    /// Authoritative phase and liveness record for the new frequency runtime.
    sessions: crate::traffic_runtime::SessionRegistry,
    health: crate::traffic_runtime::TrafficHealth,
    /// Last pose segment and position per session, used to detect real movement.
    session_segments: BTreeMap<u32, usize>,
    session_positions: BTreeMap<u32, (f64, f64)>,
    departure_waits: BTreeMap<u32, (String, Instant)>,
    /// Last crossing reissue per (aircraft, segment), so a blocked crossing cannot consume
    /// every controller exchange while it waits for traffic.
    reissued_crossings: BTreeMap<(u32, usize), Instant>,
    /// Last en-route position report on any frequency, and each flight's last report.
    last_airborne_report: Option<Instant>,
    airborne_reported: BTreeMap<u32, Instant>,
    tower_notified: BTreeSet<u32>,
    tower_ready: BTreeSet<u32>,
    flights: BTreeMap<u32, Arc<flight::Prepared>>,
    rejoins: BTreeMap<u32, flight::Rejoin>,
    rejoin_attempts: BTreeMap<(u32, usize), Arc<flight::Prepared>>,
    terminal_routes: Vec<routing::Terminal>,
    environments: BTreeMap<String, environment::Environment>,
    weather_observations: environment::Observations,
    active_flights: BTreeSet<u32>,
    /// Aircraft that began the scene already taxiing with their clearance in place.
    spawned_taxiing: BTreeSet<u32>,
    /// Aircraft created by the opening scene. Seeded traffic never waits for a clearance.
    seeded: BTreeSet<u32>,
    handoff_targets: BTreeMap<u32, usize>,
    pending_checkins: BTreeSet<(u32, usize)>,
    controllers: BTreeMap<u32, openatc_core::stations::Station>,
    sector_transfers: BTreeMap<(u32, usize), usize>,
    pending_sector_checkins:
        BTreeMap<u32, (Arc<flight::Prepared>, openatc_core::stations::Station)>,
    turnaround_inputs: Arc<BTreeMap<u32, turnaround::Input>>,
    turnarounds_attempted: BTreeSet<u32>,
    turnaround_job: Option<u64>,
    turnaround_pending: Option<turnaround::Pending>,
    turnaround_target: Option<u32>,
    stations: Arc<[openatc_core::stations::Station]>,
    regions: Arc<BTreeMap<String, openatc_core::regions::Region>>,
    mora: Arc<openatc_core::navigation::Mora>,
    simulator_root: String,
    reserved_stands: BTreeSet<String>,
    occupied_stand_cells: Arc<BTreeSet<String>>,
    surface_corridors: Arc<BTreeSet<String>>,
    /// Player position at the last survey. Travel beyond scene coverage starts a new survey.
    anchor: Option<[f64; 2]>,
    /// Bumped when travel invalidates the current area, so the scene key changes once.
    scene_revision: u64,
    /// Aircraft admitted directly on an airborne segment, with the station that owns it.
    seeded_stations: BTreeMap<u32, openatc_core::stations::Station>,
    /// Advisory traffic flying a published pattern at an untowered field.
    advisories: BTreeMap<u32, ctaf::Plan>,
    /// Prepared local departures that replace a completed one-shot departure.
    local_spares: Vec<ctaf::LocalTemplate>,
}

impl Runtime {
    pub(crate) fn operational_airports(&self) -> impl Iterator<Item = &Airport> {
        let mut seen = BTreeSet::new();
        self.airport
            .iter()
            .chain(self.destination.iter())
            .chain(self.airports.values())
            .chain(self.network.iter().flat_map(|plan| plan.airports.values()))
            .map(Arc::as_ref)
            .filter(move |airport| seen.insert(airport.icao.clone()))
    }
    /// Ask for one re-plan when the player becomes airborne so traffic is seeded on the
    /// procedure they are flying instead of only the surface traffic prepared at the gate.
    pub(crate) fn request_airborne_reseed(&mut self, on_ground: bool, departed: bool) {
        if on_ground {
            self.airborne_reseeded = false;
            return;
        }
        if !self.airborne_reseeded && departed {
            self.airborne_reseeded = true;
            self.key.clear();
            self.retry = None;
        }
    }

    fn log(&self, level: openatc_settings::TrafficLog, message: impl AsRef<str>) {
        traffic_log(level, message);
    }

    fn departure_ready(&self, id: u32) -> bool {
        (!self.require_complete_flights
            || self.flights.contains_key(&id)
            || self.apron_routes.contains_key(&id) && self.active_flights.contains(&id))
            && (self.active_flights.contains(&id)
                || self.policy.as_ref().is_none_or(|policy| {
                    self.world
                        .snapshot()
                        .iter()
                        .filter(|pose| {
                            self.active_flights.contains(&pose.id)
                                && !pose.complete
                                && !matches!(
                                    pose.stage,
                                    openatc_core::traffic::motion::Stage::Parked
                                        | openatc_core::traffic::motion::Stage::Turnaround
                                )
                        })
                        .count()
                        < policy.sequenced_flight_limit
                }))
    }
    pub(crate) fn observe_weather(&self, airport: &str, sample: &(serde_json::Value, Instant)) {
        if let Ok(mut observations) = self.weather_observations.write() {
            if !observations.contains_key(airport)
                && observations.len() >= 64
                && let Some(oldest) = observations
                    .iter()
                    .min_by_key(|(_, (_, at))| *at)
                    .map(|(id, _)| id.clone())
            {
                observations.remove(&oldest);
            }
            observations.insert(airport.to_owned(), sample.clone());
        }
    }
    fn departure_airport(&self, id: u32) -> Option<Arc<Airport>> {
        let name = &self.orders.get(&id)?.first()?.1.airport;
        self.airport
            .iter()
            .chain(self.destination.iter())
            .chain(self.airports.values())
            .chain(self.network.iter().flat_map(|plan| plan.airports.values()))
            .find(|airport| name.is_empty() || airport.icao == *name)
            .cloned()
    }
    pub(crate) fn observed_climbs(&mut self, rates: BTreeMap<u32, f64>) {
        self.observed_climbs = rates;
    }
    /// Expand only whole flight identities; TCAS and target addressing keep their short codes.
    pub(crate) fn spoken_text(&self, text: &str) -> String {
        let mut output = String::with_capacity(text.len());
        for part in text.split_inclusive(|c: char| !c.is_ascii_alphanumeric()) {
            let word = part.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
            output.push_str(self.spoken_callsigns.get(word).map_or(word, String::as_str));
            output.push_str(&part[word.len()..]);
        }
        output
    }
    fn crossing_blocked(&self, id: u32, index: usize, targets: &[TrafficTarget]) -> bool {
        let segment = self
            .flights
            .get(&id)
            .filter(|_| self.active_flights.contains(&id))
            .and_then(|flight| flight.route.segment(index))
            .or_else(|| self.world.current_segment(id));
        let Some(segment) = segment else {
            return true;
        };
        if segment.stage == openatc_core::traffic::motion::Stage::Takeoff
            && self
                .departure_wake
                .get(&id)
                .is_some_and(|seconds| *seconds > 0.0)
        {
            return true;
        }
        let landing_end = self
            .flights
            .get(&id)
            .and_then(|flight| flight.clearances.get(&index))
            .and_then(|clearance| clearance.landing_rollout);
        if !matches!(
            segment.stage,
            openatc_core::traffic::motion::Stage::Crossing
                | openatc_core::traffic::motion::Stage::LineUp
                | openatc_core::traffic::motion::Stage::Takeoff
        ) && landing_end.is_none()
        {
            return false;
        }
        let runway_segment = if landing_end.is_some() {
            self.flights
                .get(&id)
                .and_then(|flight| flight.route.segment(index + 1))
        } else {
            Some(segment)
        };
        let Some(runway_segment) = runway_segment else {
            return true;
        };
        let airport = self
            .orders
            .get(&id)
            .and_then(|orders| orders.iter().find(|(segment, _)| *segment == index))
            .and_then(|(_, order)| self.surface_airport(&order.airport))
            .or_else(|| {
                self.flights
                    .get(&id)
                    .and_then(|flight| flight.clearances.get(&index))
                    .and_then(|clearance| self.surface_airport(&clearance.station.airport))
            });
        let (Some(airport), Some(policy)) = (airport, self.policy.as_ref()) else {
            return true;
        };
        let mut runways = airport
            .runways
            .iter()
            .filter(|runway| {
                runway_segment.resources.contains(&format!(
                    "RUNWAY/{}/{}/{}",
                    airport.icao, runway.first_name, runway.second_name
                ))
            })
            .peekable();
        if runways.peek().is_none() {
            return true;
        }
        let seconds = (index..=landing_end.unwrap_or(index))
            .map(|step| {
                self.flights
                    .get(&id)
                    .and_then(|flight| flight.route.segment(step))
                    .unwrap_or(segment)
            })
            .map(|step| {
                step.points
                    .windows(2)
                    .map(|pair| {
                        openatc_core::arrival::distance_nm(
                            [pair[0].latitude, pair[0].longitude],
                            [pair[1].latitude, pair[1].longitude],
                        ) * 1852.0
                    })
                    .sum::<f64>()
                    / (step.knots.max(1.0) * 0.514_444)
            })
            .sum::<f64>()
            + policy.runway_clearance_buffer_seconds;
        let line_up =
            segment.stage == openatc_core::traffic::motion::Stage::LineUp && landing_end.is_none();
        let takeoff =
            segment.stage == openatc_core::traffic::motion::Stage::Takeoff && landing_end.is_none();
        runways.any(|runway| {
            // An aircraft established on a parallel runway's final is not traffic for this
            // runway. Surface occupancy still counts; only airborne parallel targets are
            // excluded.
            let relevant = targets.iter().filter(|target| {
                target.id != id
                    && (target.on_ground
                        || !separation::parallel_other_runway(
                            airport,
                            &runway.first_name,
                            target.latitude,
                            target.longitude,
                            target.track_degrees,
                        ))
            });
            if line_up {
                // Lining up needs the runway clear for the few seconds it takes to enter.
                // A distant final is handled after line-up, by the take-off clearance; a
                // short final still holds the departure at the line.
                openatc_core::holding::blocked_for_movement(
                    airport,
                    &runway.first_name,
                    relevant,
                    seconds,
                )
                .is_some()
            } else if takeoff {
                openatc_core::holding::blocked_for_movement(
                    airport,
                    &runway.first_name,
                    relevant,
                    seconds,
                )
                .is_some()
            } else {
                openatc_core::holding::blocked_during(
                    airport,
                    &runway.first_name,
                    relevant,
                    seconds,
                )
                .is_some()
            }
        })
    }
    fn surface_airport(&self, id: &str) -> Option<&Airport> {
        self.operational_airports()
            .find(|airport| id.is_empty() || airport.icao == id)
    }
}

fn next_job() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn preparation_started() -> &'static std::sync::Mutex<Option<Instant>> {
    static STARTED: std::sync::OnceLock<std::sync::Mutex<Option<Instant>>> =
        std::sync::OnceLock::new();
    STARTED.get_or_init(|| std::sync::Mutex::new(None))
}
fn finish_job(owner: &mut Option<u64>, job: u64) -> bool {
    if *owner != Some(job) {
        return false;
    }
    *owner = None;
    true
}

/// Match a published airport by position when installed navdata uses another identifier.
fn airport_code_map(root: &str, profiles: &Profiles) -> BTreeMap<String, String> {
    let path = std::path::Path::new(root).join("Custom Data/earth_aptmeta.dat");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    let mut positions: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for line in text.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 4 || fields[0].len() != 4 {
            continue;
        }
        let (Ok(latitude), Ok(longitude)) = (fields[2].parse::<f64>(), fields[3].parse::<f64>())
        else {
            continue;
        };
        if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
            continue;
        }
        positions.insert(fields[0].to_owned(), (latitude, longitude));
    }
    if positions.is_empty() {
        return BTreeMap::new();
    }
    let mut map = BTreeMap::new();
    for (published, hub) in &profiles.hubs {
        let (Some(latitude), Some(longitude)) = (hub.latitude, hub.longitude) else {
            continue;
        };
        let mut best: Option<(&String, f64)> = None;
        for (local, (local_latitude, local_longitude)) in &positions {
            let distance = (local_latitude - latitude)
                .abs()
                .hypot((local_longitude - longitude).abs());
            if best.is_none_or(|(_, current)| distance < current) {
                best = Some((local, distance));
            }
        }
        // Ten kilometres keeps the match on the airport itself rather than a nearby field.
        if let Some((local, distance)) = best
            && distance < 0.15
        {
            map.insert(local.clone(), published.clone());
        }
    }
    map
}

/// The published identifier for a simulator airport, when the two differ.
fn published_code(codes: &BTreeMap<String, String>, icao: &str) -> String {
    codes.get(icao).cloned().unwrap_or_else(|| icao.to_owned())
}

fn port(airport: &Airport) -> Option<Port> {
    Some(Port {
        icao: airport.icao.clone(),
        runway: airport
            .runways
            .iter()
            .map(|runway| {
                (runway.second.east - runway.first.east)
                    .hypot(runway.second.north - runway.first.north)
            })
            .reduce(f64::max)?,
        parking_size: airport
            .parking
            .iter()
            .filter(|stand| openatc_core::parking::fixed_wing_stand(stand))
            .filter_map(openatc_core::parking::stand_size)
            .max()?,
    })
}
fn runway_port(airport: &Airport, runway: &str, codes: &BTreeMap<String, String>) -> Option<Port> {
    let mut port = port(airport)?;
    let selected = airport
        .runways
        .iter()
        .find(|end| end.first_name == runway || end.second_name == runway)?;
    port.runway = (selected.second.east - selected.first.east)
        .hypot(selected.second.north - selected.first.north);
    port.icao = published_code(codes, &airport.icao);
    Some(port)
}

/// A port that carries the published airport identifier, so the world profile matches.
fn port_published(input: &Preparation, airport: &Airport) -> Option<Port> {
    let mut port = port(airport)?;
    port.icao = published_code(&input.airport_codes, &port.icao);
    Some(port)
}

fn airport(root: &str, id: &str) -> Result<Airport, String> {
    let mut airport = openatc_core::airport::load_airport_from_simulator(
        root,
        id,
        &|path| std::path::Path::new(path).is_file(),
        &|path| airport_source::read(path, id),
    )?;
    scenery::Terrain::new(std::path::Path::new(root))?.supplement_airport(&mut airport)?;
    Ok(airport)
}
fn position(airport: &Airport, point: Point) -> [f64; 2] {
    [
        airport.reference_latitude + point.north / 111_320.0,
        airport.reference_longitude
            + point.east / (111_320.0 * airport.reference_latitude.to_radians().cos()),
    ]
}
#[derive(Clone)]
struct Preparation {
    stations: Arc<[openatc_core::stations::Station]>,
    regions: BTreeMap<String, openatc_core::regions::Region>,
    simulator_root: String,
    origin: Airport,
    destination: Airport,
    profiles: Arc<Profiles>,
    models: Arc<Catalogue>,
    counts: [u8; 4],
    own: [f64; 2],
    observed: Vec<TrafficTarget>,
    reserved_stand_cells: BTreeSet<String>,
    reserved_surface_corridors: BTreeSet<String>,
    /// Stands the player already holds, withheld from the generated fleet.
    reserved_stands: BTreeSet<String>,
    /// Add local pattern traffic when the network released little traffic.
    local_pattern_traffic: bool,
    /// Start the first released flight on approach instead of at its stand.
    seed_single_arrival: bool,
    /// The player is airborne, so traffic is seeded around the procedure they are flying.
    player_airborne: bool,
    minimum_airborne_spawn_nm: f64,
    epoch: u64,
    /// Per-run seed for fleet, stand, procedure and startup-role choices.
    seed: u64,
    /// The world is still empty, so seed the opening mix of holding, taxiing and gate traffic.
    startup_cast: bool,
    /// Moving ground aircraft target, fixed for this session.
    taxi_target: usize,
    /// Use this runway even when the wind would select the other end (non-local alternate).
    force_runway: bool,
    /// Use this arrival runway even when the wind would select the other end.
    force_arrival_runway: bool,
    runway: String,
    arrival_runway: String,
    config: std::path::PathBuf,
    weather: BTreeMap<String, (serde_json::Value, Instant)>,
    weather_observations: Option<environment::Observations>,
    /// The simulator's own airport identifier for a published airport, when the installed
    /// navdata randomises airport codes.
    airport_codes: BTreeMap<String, String>,
}
impl Preparation {
    fn refresh_weather(&mut self) -> Result<(), String> {
        if let Some(observations) = &self.weather_observations {
            self.weather = observations
                .read()
                .map_err(|_| "Simulator weather observations are unavailable")?
                .clone();
        }
        Ok(())
    }
}
/// Upper bound on generated aircraft in one scene, including advisory traffic.
const MAXIMUM_AIRCRAFT: usize = 63;
/// Distance from the filed destination at which an airborne flight prepares its scene.
const ARRIVAL_PREPARE_NM: f64 = 90.0;

#[derive(Clone)]
struct Scene {
    require_complete_flights: bool,
    surface_corridors: Arc<BTreeSet<String>>,
    airports: BTreeMap<String, Arc<Airport>>,
    frames: Vec<GeneratedTraffic>,
    world: openatc_core::traffic::motion::World,
    movement: BTreeMap<u32, animation::Handling>,
    airport: Airport,
    destination: Airport,
    departure_procedures: Option<Arc<openatc_core::traffic::procedures::Catalogue>>,
    arrival_procedures: Option<Arc<openatc_core::traffic::procedures::Catalogue>>,
    terminal_points: Option<Arc<openatc_core::traffic::coordinates::Points>>,
    published_holds: Option<Arc<openatc_core::traffic::holding::published::Catalogue>>,
    nominal_departures: BTreeMap<u32, Arc<departure::Surface>>,
    decoded_terminals: BTreeMap<u32, Arc<airborne::Decoded>>,
    nominal_arrivals: BTreeMap<u32, Arc<landing::Arrival>>,
    nominal_flights: BTreeMap<u32, Arc<assembly::Draft>>,
    flights: BTreeMap<u32, Arc<flight::Prepared>>,
    spoken_callsigns: BTreeMap<String, String>,
    terminal_routes: Vec<routing::Terminal>,
    environments: BTreeMap<String, environment::Environment>,
    policy: Operations,
    fixed_gear: BTreeSet<u32>,
    channels: BTreeMap<String, animation::Channel>,
    orders: BTreeMap<u32, Vec<(usize, openatc_core::state::TaxiClearance)>>,
    turnaround_inputs: Arc<BTreeMap<u32, turnaround::Input>>,
    stations: Arc<[openatc_core::stations::Station]>,
    regions: Arc<BTreeMap<String, openatc_core::regions::Region>>,
    mora: Arc<openatc_core::navigation::Mora>,
    simulator_root: String,
    reserved_stands: BTreeSet<String>,
    occupied_stand_cells: Arc<BTreeSet<String>>,
    arrival_refusals: usize,
    warnings: Vec<String>,
    /// Aircraft admitted directly on an airborne segment, with the station that owns it.
    seeded_stations: BTreeMap<u32, openatc_core::stations::Station>,
    /// Advisory traffic flying a published pattern at an untowered field.
    advisories: BTreeMap<u32, ctaf::Plan>,
    /// Prepared local departures that replace a completed one-shot departure.
    local_spares: Vec<ctaf::LocalTemplate>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Operations {
    /// How many prepared flights the controller sequences at the same time.
    sequenced_flight_limit: usize,
    network_airport_limit: usize,
    network_candidate_limit: usize,
    network_radius_nm: f64,
    holding_turn_rate_degrees_sec: f64,
    contingency_holding_maximum_ias: f64,
    airborne_separation_nm: f64,
    airborne_terminal_separation_nm: f64,
    airborne_separation_low_feet: f64,
    airborne_separation_feet: f64,
    airborne_prediction_seconds: f64,
    version: u32,
    preparation_seconds_min: f64,
    preparation_seconds_max: f64,
    surface_sample_meters: f64,
    surface_segment_meters: f64,
    conflict_cell_meters: f64,
    stand_clearance_meters: f64,
    maximum_surface_segments: usize,
    radio_characters_per_second: f64,
    radio_gap_seconds: f64,
    runway_clearance_buffer_seconds: f64,
    pushback_knots: f64,
    pushback_radius_scale: f64,
    pushback_max_meters: f64,
    flight_clearance_lead_seconds: f64,
    terminal_candidate_limit: usize,
    terminal_intercept_limit_nm: f64,
    enroute_segment_nm: f64,
    airborne_sample_meters: f64,
    airborne_detour_meters: f64,
    airborne_turn_rate_degrees_sec: f64,
    runway_slope_limit_percent: f64,
    visible_aircraft_limit: usize,
    display_enter_nm: f64,
    display_leave_nm: f64,
    display_replacement_margin_nm: f64,
    same_direction_limit: usize,
    trailing_limit: usize,
    same_direction_cone_degrees: f64,
    same_direction_range_nm: f64,
    surface_monitor_radius_nm: f64,
    gate_connector_meters: f64,
    gate_connector_detour_meters: f64,
}
impl Operations {
    /// Vertical minimum for a pair whose higher aircraft is at `altitude_feet`.
    fn vertical_minimum(&self, altitude_feet: f64) -> f64 {
        if altitude_feet < 29_000.0 {
            self.airborne_separation_low_feet
        } else {
            self.airborne_separation_feet
        }
    }

    /// Use the radar minima of the airports a flight joins. An airport without its own
    /// table keeps these operation defaults, and the more conservative value wins.
    fn apply_scene_separation(&mut self, airports: [&str; 2]) {
        let found = airports.map(|icao| {
            openatc_core::traffic::wake_policy::read(|policy| policy.separation(icao).cloned())
        });
        self.combine_separation(found);
    }

    fn combine_separation(
        &mut self,
        found: [Option<openatc_core::traffic::wake_policy::Separation>; 2],
    ) {
        let base = (
            self.airborne_separation_nm,
            self.airborne_terminal_separation_nm,
            self.airborne_separation_low_feet,
            self.airborne_separation_feet,
        );
        let values = found.map(|separation| {
            separation.map_or(base, |separation| {
                (
                    separation.enroute_lateral_nm,
                    separation.terminal_lateral_nm,
                    separation.vertical_below_fl290_feet,
                    separation.vertical_fl290_and_above_feet,
                )
            })
        });
        let (enroute, terminal, low, high) = values.into_iter().fold(values[0], |a, b| {
            (a.0.max(b.0), a.1.max(b.1), a.2.max(b.2), a.3.max(b.3))
        });
        self.airborne_separation_nm = enroute;
        self.airborne_terminal_separation_nm = terminal;
        self.airborne_separation_low_feet = low;
        self.airborne_separation_feet = high;
    }
}

fn parse_operations(text: &str) -> Result<Operations, String> {
    if text.len() > 65536 {
        return Err("Generated operations policy exceeds 64 KiB".into());
    }
    let mut value: toml::Value =
        openatc_core::configuration::from_str(text).map_err(|error| error.to_string())?;
    let defaults: toml::Value =
        openatc_core::configuration::from_str(include_str!("../../../traffic/operations.toml"))
            .map_err(|error| error.to_string())?;
    let table = value
        .as_table_mut()
        .ok_or("Operations policy must be a table")?;
    for (key, value) in defaults.as_table().unwrap() {
        table.entry(key.clone()).or_insert_with(|| value.clone());
    }
    let policy: Operations = value
        .try_into()
        .map_err(|error: toml::de::Error| error.to_string())?;
    if !(1..=16).contains(&policy.sequenced_flight_limit)
        || !(2..=6).contains(&policy.network_airport_limit)
        || !(policy.network_airport_limit..=32).contains(&policy.network_candidate_limit)
        || !(25.0..=2000.0).contains(&policy.network_radius_nm)
        || !(1..=63).contains(&policy.visible_aircraft_limit)
        || !(1..=8).contains(&policy.same_direction_limit)
        || policy.trailing_limit > policy.same_direction_limit
        || !(10.0..=90.0).contains(&policy.same_direction_cone_degrees)
        || !(5.0..=50.0).contains(&policy.same_direction_range_nm)
        || !(5.0..=100.0).contains(&policy.display_enter_nm)
        || !(policy.display_enter_nm..=150.0).contains(&policy.display_leave_nm)
        || !(0.0..=10.0).contains(&policy.display_replacement_margin_nm)
        || !(1..=32).contains(&policy.terminal_candidate_limit)
        || !(1.0..=100.0).contains(&policy.terminal_intercept_limit_nm)
        || !(5.0..=50.0).contains(&policy.enroute_segment_nm)
        || !(10.0..=250.0).contains(&policy.airborne_sample_meters)
        || !(0.0..=10000.0).contains(&policy.airborne_detour_meters)
        || !(0.5..=3.0).contains(&policy.airborne_turn_rate_degrees_sec)
        || !(0.1..=10.0).contains(&policy.runway_slope_limit_percent)
        || !(1.0..=50.0).contains(&policy.surface_monitor_radius_nm)
        || !(1.0..=350.0).contains(&policy.gate_connector_meters)
        || !(0.0..=500.0).contains(&policy.gate_connector_detour_meters)
        || policy.version != 1
        || !(30.0..=300.0).contains(&policy.flight_clearance_lead_seconds)
        || !(0.5..=3.0).contains(&policy.holding_turn_rate_degrees_sec)
        || !(100.0..=200.0).contains(&policy.contingency_holding_maximum_ias)
        || !(3.0..=10.0).contains(&policy.airborne_separation_nm)
        || !(3.0..=10.0).contains(&policy.airborne_terminal_separation_nm)
        || !(1000.0..=5000.0).contains(&policy.airborne_separation_low_feet)
        || !(1000.0..=5000.0).contains(&policy.airborne_separation_feet)
        || !(60.0..=300.0).contains(&policy.airborne_prediction_seconds)
        || !(0.5..=5.0).contains(&policy.pushback_knots)
        || !(0.25..=1.0).contains(&policy.pushback_radius_scale)
        || !(20.0..=500.0).contains(&policy.pushback_max_meters)
        || !(5.0..=600.0).contains(&policy.preparation_seconds_min)
        || !(policy.preparation_seconds_min..=900.0).contains(&policy.preparation_seconds_max)
        || !(1.0..=10.0).contains(&policy.surface_sample_meters)
        || !(10.0..=50.0).contains(&policy.surface_segment_meters)
        || !(25.0..=100.0).contains(&policy.conflict_cell_meters)
        || !(30.0..=150.0).contains(&policy.stand_clearance_meters)
        || !(32..=512).contains(&policy.maximum_surface_segments)
        || !(8.0..=25.0).contains(&policy.radio_characters_per_second)
        || !(0.5..=10.0).contains(&policy.radio_gap_seconds)
        || !(10.0..=90.0).contains(&policy.runway_clearance_buffer_seconds)
    {
        return Err("Generated operations policy has invalid timing or distances".into());
    }
    Ok(policy)
}
fn cells(airport: &Airport, point: Point, radius: f64, size: f64) -> BTreeSet<String> {
    let mut cells = BTreeSet::new();
    let first_x = ((point.east - radius) / size).floor() as i32;
    let last_x = ((point.east + radius) / size).floor() as i32;
    let first_y = ((point.north - radius) / size).floor() as i32;
    let last_y = ((point.north + radius) / size).floor() as i32;
    for x in first_x..=last_x {
        for y in first_y..=last_y {
            cells.insert(format!("GROUND/{}/{x}/{y}", airport.icao));
        }
    }
    cells
}
#[derive(Clone)]
struct GroundPlan {
    route: openatc_core::traffic::motion::Route,
    orders: Vec<(usize, openatc_core::state::TaxiClearance)>,
}
// All twelve values describe one surface route; grouping them would only move the same
// parameters into a struct at each call site.
#[allow(clippy::too_many_arguments)]
fn ground_route(
    airport: &Airport,
    stand: Point,
    heading: f64,
    gate: bool,
    aircraft: &openatc_core::traffic::profiles::Aircraft,
    handling: &animation::Handling,
    runway: &str,
    seed: u64,
    policy: &Operations,
    to_parking: bool,
    observed: &[TrafficTarget],
    parked: bool,
) -> Result<GroundPlan, String> {
    let mut alternate = None;
    for attempt in 0..=4 {
        let selected = alternate.as_ref().unwrap_or(airport);
        let mut refused = None;
        let result = ground_route_once(
            selected,
            stand,
            heading,
            gate,
            aircraft,
            handling,
            runway,
            seed,
            policy,
            to_parking,
            observed,
            &mut refused,
            parked,
        );
        if result.is_ok() || attempt == 4 {
            return result;
        }
        let Some(point) = refused else {
            return result;
        };
        let Some(edge) =
            unsupported_surface_edge(selected, point, policy.surface_sample_meters * 2.0)
        else {
            return result;
        };
        // Retry existing taxi routes; never bridge the rejected pavement gap or alter a runway edge.
        alternate
            .get_or_insert_with(|| airport.clone())
            .edges
            .remove(edge);
    }
    unreachable!()
}

fn unsupported_surface_edge(airport: &Airport, point: Point, maximum: f64) -> Option<usize> {
    let nodes = airport
        .nodes
        .iter()
        .map(|node| (node.id, node.point))
        .collect::<BTreeMap<_, _>>();
    let mut candidates = airport
        .edges
        .iter()
        .enumerate()
        .filter(|(_, edge)| !edge.runway)
        .filter_map(|(index, edge)| {
            let (first, last) = (*nodes.get(&edge.first)?, *nodes.get(&edge.second)?);
            let delta = [last.east - first.east, last.north - first.north];
            let squared = delta[0] * delta[0] + delta[1] * delta[1];
            if squared <= 0.0001 {
                return None;
            }
            let fraction = (((point.east - first.east) * delta[0]
                + (point.north - first.north) * delta[1])
                / squared)
                .clamp(0.0, 1.0);
            let distance = (point.east - first.east - delta[0] * fraction)
                .hypot(point.north - first.north - delta[1] * fraction);
            (distance <= maximum).then_some((index, distance))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| a.1.total_cmp(&b.1));
    let first = *candidates.first()?;
    if candidates
        .get(1)
        .is_some_and(|next| (next.1 - first.1).abs() <= 0.01)
    {
        return None;
    }
    Some(first.0)
}

#[allow(clippy::too_many_arguments)]
fn ground_route_once(
    airport: &Airport,
    stand: Point,
    heading: f64,
    gate: bool,
    aircraft: &openatc_core::traffic::profiles::Aircraft,
    handling: &animation::Handling,
    runway: &str,
    seed: u64,
    policy: &Operations,
    to_parking: bool,
    observed: &[TrafficTarget],
    refused: &mut Option<Point>,
    parked: bool,
) -> Result<GroundPlan, String> {
    use openatc_core::traffic::motion::{Route, Segment, Stage, Waypoint};
    let start = position(airport, stand);
    let mut telemetry = openatc_core::state::Telemetry {
        latitude: start[0],
        longitude: start[1],
        position_valid: true,
        on_ground: true,
        heading_degrees: heading,
        traffic: Some(observed.to_vec()),
        ..Default::default()
    };
    let mut path = openatc_core::airport::calculate_taxi_route(
        airport,
        &telemetry,
        runway,
        to_parking,
        aircraft.parking_size(),
    )?;
    if path.points.is_empty() {
        return Err("No surface route".into());
    }
    let waypoint = |point: Point| {
        let coordinate = position(airport, point);
        Waypoint {
            latitude: coordinate[0],
            longitude: coordinate[1],
            altitude_feet: airport.elevation_feet + point.height / 0.3048,
        }
    };
    let mut segments = if parked {
        vec![Segment {
            stage: Stage::Parked,
            points: vec![waypoint(stand)],
            knots: 0.0,
            resources: cells(
                airport,
                stand,
                aircraft.wingspan.max(aircraft.length) / 2.0,
                policy.conflict_cell_meters,
            )
            .into_iter()
            .collect(),
            dwell_seconds: policy.preparation_seconds_min
                + (seed % 1000) as f64 / 1000.0
                    * (policy.preparation_seconds_max - policy.preparation_seconds_min),
        }]
    } else {
        Vec::new()
    };
    if to_parking {
        segments.clear();
    }
    let taxi_stage = if to_parking {
        Stage::TaxiIn
    } else {
        Stage::TaxiOut
    };
    let mut from = stand;
    if gate && !to_parking {
        let first = path
            .points
            .iter()
            .position(|point| (point.east - stand.east).hypot(point.north - stand.north) > 1.0);
        if let Some(index) = first {
            let entry = path.points[index];
            let nose = [heading.to_radians().sin(), heading.to_radians().cos()];
            if (entry.east - stand.east) * nose[0] + (entry.north - stand.north) * nose[1] < -1.0 {
                let next = path
                    .points
                    .iter()
                    .skip(index + 1)
                    .find(|point| (point.east - entry.east).hypot(point.north - entry.north) > 1.0)
                    .ok_or("Pushback needs a known onward taxi direction")?;
                let outgoing = (next.east - entry.east)
                    .atan2(next.north - entry.north)
                    .to_degrees()
                    .rem_euclid(360.0);
                let radius = (handling.taxi_turn_radius_meters * policy.pushback_radius_scale)
                    .max(
                        handling.wheelbase_meters
                            / handling.maximum_steering_degrees.to_radians().tan(),
                    )
                    .max(1.0);
                let endpoint = Point {
                    east: entry.east - radius * outgoing.to_radians().sin(),
                    north: entry.north - radius * outgoing.to_radians().cos(),
                    height: entry.height,
                };
                let curve = openatc_core::traffic::curves::connect(
                    openatc_core::traffic::curves::Pose {
                        point: stand,
                        heading: (heading + 180.0).rem_euclid(360.0),
                    },
                    openatc_core::traffic::curves::Pose {
                        point: endpoint,
                        heading: (outgoing + 180.0).rem_euclid(360.0),
                    },
                    &openatc_core::traffic::curves::Limits {
                        radius,
                        spacing: policy.surface_sample_meters,
                        length: policy.pushback_max_meters,
                        initial_turn: None,
                    },
                    |first, last| {
                        [first, last].iter().all(|sample| {
                            openatc_core::airport::on_pavement(airport, sample.point)
                                && !openatc_core::airport::within_runway(airport, &sample.point)
                        })
                    },
                )?;
                let mut protected = BTreeSet::new();
                for sample in &curve {
                    protected.extend(cells(
                        airport,
                        sample.point,
                        aircraft.wingspan.max(aircraft.length) / 2.0,
                        policy.conflict_cell_meters,
                    ));
                }
                segments.push(Segment {
                    stage: Stage::Pushback,
                    points: curve.iter().map(|sample| waypoint(sample.point)).collect(),
                    knots: policy.pushback_knots,
                    resources: protected.into_iter().collect(),
                    dwell_seconds: 0.0,
                });
                from = endpoint;
                let coordinate = position(airport, endpoint);
                telemetry.latitude = coordinate[0];
                telemetry.longitude = coordinate[1];
                telemetry.heading_degrees = outgoing;
                path = openatc_core::airport::calculate_taxi_route(
                    airport,
                    &telemetry,
                    runway,
                    false,
                    aircraft.parking_size(),
                )?;
            }
        }
    }
    let mut stage = taxi_stage;
    let mut crossing_resource = None;
    let mut crossed = BTreeSet::new();
    let mut orders = Vec::new();
    for _ in 0..16 {
        if to_parking && stage != Stage::Crossing && path.hold_short_runway.is_empty() {
            let target = airport
                .parking
                .iter()
                .find(|stand| stand.name == runway)
                .ok_or("Arrival stand disappeared")?;
            let last = *path
                .points
                .last()
                .ok_or("Arrival has no mapped taxi endpoint")?;
            if (last.east - target.point.east).hypot(last.north - target.point.north) > 0.01 {
                let previous = path.points.iter().rev().nth(1).copied().unwrap_or(from);
                let heading = (last.east - previous.east)
                    .atan2(last.north - previous.north)
                    .to_degrees()
                    .rem_euclid(360.0);
                let distance =
                    (last.east - target.point.east).hypot(last.north - target.point.north);
                if distance > policy.gate_connector_meters {
                    return Err("Arrival stand is too far from its mapped taxi route".into());
                }
                let curve = openatc_core::traffic::curves::connect(
                    openatc_core::traffic::curves::Pose {
                        point: last,
                        heading,
                    },
                    openatc_core::traffic::curves::Pose {
                        point: target.point,
                        heading: target.heading,
                    },
                    &openatc_core::traffic::curves::Limits {
                        radius: handling.taxi_turn_radius_meters,
                        spacing: policy.surface_sample_meters,
                        length: distance + policy.gate_connector_detour_meters,
                        initial_turn: None,
                    },
                    |a, b| {
                        [a, b].iter().all(|sample| {
                            openatc_core::airport::on_pavement(airport, sample.point)
                                && !openatc_core::airport::within_runway(airport, &sample.point)
                        })
                    },
                )?;
                path.points
                    .extend(curve.into_iter().skip(1).map(|sample| sample.point));
            }
        }
        let start_segment = segments.len();
        orders.push((start_segment, path.clone()));
        let raw: Vec<_> = std::iter::once(from)
            .chain(path.points.iter().copied())
            .collect();
        let stationary_hold = raw
            .iter()
            .all(|point| (point.east - from.east).hypot(point.north - from.north) < 0.01);
        if stationary_hold {
            if path.hold_short_runway.is_empty() || stage == Stage::Crossing {
                return Err("Surface clearance has no verified onward movement".into());
            }
            segments.push(Segment {
                stage,
                points: vec![waypoint(from)],
                knots: 0.0,
                resources: cells(
                    airport,
                    from,
                    aircraft.wingspan.max(aircraft.length) / 2.0,
                    policy.conflict_cell_meters,
                )
                .into_iter()
                .collect(),
                dwell_seconds: 0.0,
            });
        }
        let rounded = if stationary_hold {
            Vec::new()
        } else {
            openatc_core::traffic::surface::rounded(
                &raw,
                handling.taxi_turn_radius_meters,
                handling.wheelbase_meters / handling.maximum_steering_degrees.to_radians().tan(),
                policy.surface_sample_meters,
                |point| {
                    openatc_core::airport::on_pavement(airport, point)
                        && (stage == Stage::Crossing
                            || !openatc_core::airport::within_runway(airport, &point))
                },
            )?
        };
        let mut turning = false;
        for sample in &rounded {
            let to = &sample.point;
            let distance = (to.east - from.east).hypot(to.north - from.north);
            if distance < 0.01 {
                continue;
            }
            let samples = (distance / policy.surface_sample_meters).ceil() as usize;
            if samples > 4096 {
                return Err("Surface connector exceeds its sampling budget".into());
            }
            if let Some(point) = (0..=samples)
                .map(|index| {
                    let amount = index as f64 / samples as f64;
                    Point {
                        east: from.east + (to.east - from.east) * amount,
                        north: from.north + (to.north - from.north) * amount,
                        height: 0.0,
                    }
                })
                .find(|point| !openatc_core::airport::on_pavement(airport, *point))
            {
                *refused = Some(point);
                let coordinate = position(airport, point);
                return Err(format!(
                    "Unverified surface connector at {:.7}, {:.7}",
                    coordinate[0], coordinate[1]
                ));
            }
            let steps = (distance / policy.surface_segment_meters).ceil() as usize;
            let previous = from;
            for index in 1..=steps {
                if segments.len() >= 4096 {
                    return Err("Surface route exceeds segment budget".into());
                }
                let amount = index as f64 / steps as f64;
                let next = Point {
                    east: previous.east + (to.east - previous.east) * amount,
                    north: previous.north + (to.north - previous.north) * amount,
                    height: 0.0,
                };
                let radius = aircraft.wingspan.max(aircraft.length) / 2.0;
                let resources = cells(airport, from, radius, policy.conflict_cell_meters)
                    .union(&cells(airport, next, radius, policy.conflict_cell_meters))
                    .cloned()
                    .collect();
                segments.push(Segment {
                    stage,
                    points: vec![waypoint(from), waypoint(next)],
                    knots: if turning || sample.turning {
                        handling.taxi_turn_knots
                    } else {
                        handling.taxi_knots
                    },
                    resources,
                    dwell_seconds: 0.0,
                });
                from = next;
            }
            turning = sample.turning;
        }
        if matches!(stage, Stage::TaxiOut | Stage::TaxiIn) {
            let parts: Vec<_> = segments.drain(start_segment..).collect();
            let mut merged: Vec<Segment> = Vec::new();
            let mut length = 0.0;
            for part in parts {
                let distance = openatc_core::arrival::distance_nm(
                    [part.points[0].latitude, part.points[0].longitude],
                    [
                        part.points.last().unwrap().latitude,
                        part.points.last().unwrap().longitude,
                    ],
                ) * 1852.0;
                if let Some(previous) = merged.last_mut()
                    && (previous.knots - part.knots).abs() <= f64::EPSILON
                    && length + distance <= policy.surface_segment_meters
                {
                    let resources: BTreeSet<_> = previous
                        .resources
                        .iter()
                        .chain(&part.resources)
                        .cloned()
                        .collect();
                    if resources.len() <= 32 {
                        previous.points.extend(part.points.into_iter().skip(1));
                        previous.resources = resources.into_iter().collect();
                        length += distance;
                        continue;
                    }
                }
                length = distance;
                merged.push(part);
            }
            segments.extend(merged);
            if segments.len() > policy.maximum_surface_segments {
                return Err("Surface route exceeds segment budget".into());
            }
        }
        if stage == Stage::Crossing {
            let parts: Vec<_> = segments.drain(start_segment..).collect();
            let mut resources = BTreeSet::new();
            let mut points = Vec::new();
            for part in parts {
                if points.is_empty() {
                    points.push(part.points[0]);
                }
                points.extend(part.points.into_iter().skip(1));
                resources.extend(part.resources);
            }
            resources.insert(
                crossing_resource
                    .take()
                    .ok_or("Crossing has no runway resource")?,
            );
            segments.push(Segment {
                stage: Stage::Crossing,
                points,
                knots: handling.taxi_turn_knots,
                resources: resources.into_iter().collect(),
                dwell_seconds: 0.0,
            });
        } else if to_parking && path.hold_short_runway.is_empty() {
            segments.push(Segment {
                stage: Stage::Turnaround,
                points: vec![waypoint(from)],
                knots: 0.0,
                resources: cells(
                    airport,
                    from,
                    aircraft.wingspan.max(aircraft.length) / 2.0,
                    policy.conflict_cell_meters,
                )
                .into_iter()
                .collect(),
                dwell_seconds: policy.preparation_seconds_min,
            });
            openatc_core::traffic::motion::limit_surface_speeds(
                &mut segments,
                handling.braking_knots_sec,
                &orders.iter().map(|(index, _)| *index).collect(),
            )?;
            return Ok(GroundPlan {
                route: Route::compile(segments, aircraft.climb_fpm, aircraft.descent_fpm)?,
                orders,
            });
        } else if !to_parking && path.hold_short_runway == runway {
            openatc_core::traffic::motion::limit_surface_speeds(
                &mut segments,
                handling.braking_knots_sec,
                &orders.iter().map(|(index, _)| *index).collect(),
            )?;
            return Ok(GroundPlan {
                route: Route::compile(segments, aircraft.climb_fpm, aircraft.descent_fpm)?,
                orders,
            });
        }
        let coordinate = position(airport, from);
        telemetry.latitude = coordinate[0];
        telemetry.longitude = coordinate[1];
        if stage == Stage::Crossing {
            stage = taxi_stage;
            path = openatc_core::airport::calculate_taxi_route(
                airport,
                &telemetry,
                runway,
                to_parking,
                aircraft.parking_size(),
            )?;
        } else {
            let crossing = airport
                .runways
                .iter()
                .find(|r| {
                    r.first_name == path.hold_short_runway
                        || r.second_name == path.hold_short_runway
                })
                .ok_or("No protected crossing runway")?;
            let resource = format!(
                "RUNWAY/{}/{}/{}",
                airport.icao, crossing.first_name, crossing.second_name
            );
            if !crossed.insert(resource.clone()) {
                return Err("Surface route repeats a runway crossing".into());
            }
            crossing_resource = Some(resource);
            path = openatc_core::airport::calculate_crossing_route_for(
                airport,
                &telemetry,
                &path.hold_short_runway,
                aircraft,
            )
            .map_err(|error| {
                format!(
                    "{error} at {:.7},{:.7} for runway {} (departing {runway})",
                    telemetry.latitude, telemetry.longitude, path.hold_short_runway
                )
            })?;
            stage = Stage::Crossing;
        }
    }
    Err("Surface route exceeds crossing budget".into())
}
fn prepare_scene(
    mut input: Preparation,
    terrain: &Result<std::cell::RefCell<scenery::Terrain>, String>,
) -> Result<Scene, String> {
    let setup_started = std::time::Instant::now();
    input.refresh_weather()?;
    let mut environments = BTreeMap::new();
    for (airport, runway, forced) in [
        (&input.origin, &mut input.runway, input.force_runway),
        (
            &input.destination,
            &mut input.arrival_runway,
            input.force_arrival_runway,
        ),
    ] {
        if let Some(sample) = input.weather.get(&airport.icao)
            && let Ok(weather) = environment::Environment::from_sample(airport, sample)
        {
            let weather = input
                .weather_observations
                .as_ref()
                .map_or(weather.clone(), |observations| {
                    weather.follow(&airport.icao, observations, &sample.0)
                });
            if !forced {
                *runway = crate::radio::atis_runway(
                    &airport.runways,
                    runway,
                    weather.wind_from,
                    weather.wind_knots,
                );
            }
            environments.insert(airport.icao.clone(), weather);
        }
    }
    // Fleet eligibility uses the selected flow, not a longer runway elsewhere at the airport.
    let (Some(origin), Some(destination)) = (
        runway_port(&input.origin, &input.runway, &input.airport_codes),
        runway_port(
            &input.destination,
            &input.arrival_runway,
            &input.airport_codes,
        ),
    ) else {
        return Err(
            "Generated traffic needs selected runway and stand capacities at both airports".into(),
        );
    };
    let distance = openatc_core::arrival::distance_nm(
        [
            input.origin.reference_latitude,
            input.origin.reference_longitude,
        ],
        [
            input.destination.reference_latitude,
            input.destination.reference_longitude,
        ],
    );
    let mut used = BTreeSet::new();
    let mut result: Vec<GeneratedTraffic> = Vec::new();
    let classes = [
        Category::Airline,
        Category::Regional,
        Category::Cargo,
        Category::GeneralAviation,
    ];
    let mut seed = input.seed;
    for byte in origin.icao.bytes() {
        seed = seed.wrapping_mul(16_777_619) ^ u64::from(byte);
    }
    let (handling, mut policy, channels) = policy::load(&input.config)?;
    policy.apply_scene_separation([input.origin.icao.as_str(), input.destination.icao.as_str()]);
    let mut world = openatc_core::traffic::motion::World::default();
    let mut movement = BTreeMap::new();
    let mut fixed_gear = BTreeSet::new();
    let mut orders = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut terminal = |id: &str| {
        if input.simulator_root.is_empty() {
            return None;
        }
        match openatc_core::traffic::procedures::Catalogue::load(
            std::path::Path::new(&input.simulator_root),
            id,
        ) {
            Ok(catalogue) => Some(Arc::new(catalogue)),
            Err(error) => {
                warnings.push(format!("{id}: {error}"));
                None
            }
        }
    };
    let catalogue_started = std::time::Instant::now();
    let departure_procedures = terminal(&input.origin.icao);
    let arrival_procedures = terminal(&input.destination.icao);
    if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
        traffic_log(
            openatc_settings::TrafficLog::Summary,
            format!(
                "terminal catalogues {} and {} loaded in {:.2} s",
                input.origin.icao,
                input.destination.icao,
                catalogue_started.elapsed().as_secs_f64()
            ),
        );
    }
    let catalogues = departure_procedures
        .as_ref()
        .map(|catalogue| (input.origin.icao.as_str(), catalogue.as_ref()))
        .into_iter()
        .chain(
            arrival_procedures
                .as_ref()
                .map(|catalogue| (input.destination.icao.as_str(), catalogue.as_ref())),
        )
        .collect::<Vec<_>>();
    let points_started = std::time::Instant::now();
    let terminal_points = if catalogues.is_empty() {
        None
    } else {
        let mut points = openatc_core::traffic::coordinates::Points::requested(&catalogues);
        match points.load(std::path::Path::new(&input.simulator_root)) {
            Ok(()) => Some(Arc::new(points)),
            Err(error) => {
                warnings.push(error);
                None
            }
        }
    };
    if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
        traffic_log(
            openatc_settings::TrafficLog::Summary,
            format!(
                "terminal coordinates loaded in {:.2} s",
                points_started.elapsed().as_secs_f64()
            ),
        );
    }
    let mut shared_models = BTreeMap::new();
    let published_holds = terminal_points
        .as_ref()
        .and_then(|points| points.cycle.as_deref())
        .and_then(|cycle| {
            match openatc_core::traffic::holding::published::Catalogue::load(
                std::path::Path::new(&input.simulator_root),
                &catalogues,
                cycle,
            ) {
                Ok(holds) => Some(Arc::new(holds)),
                Err(error) => {
                    warnings.push(error);
                    None
                }
            }
        });
    let mora_started = std::time::Instant::now();
    let mora = Arc::new(openatc_core::navigation::Mora::load(&input.simulator_root));
    if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
        traffic_log(
            openatc_settings::TrafficLog::Summary,
            format!(
                "MORA loaded in {:.2} s",
                mora_started.elapsed().as_secs_f64()
            ),
        );
    }
    let controller_origin = Arc::new(input.origin.clone());
    let controller_destination = Arc::new(input.destination.clone());
    let controller_regions = Arc::new(input.regions.clone());
    let controller_stations = input.stations.clone();
    let candidates_started = std::time::Instant::now();
    let mut terminal_routes = match (&departure_procedures, &arrival_procedures, &terminal_points) {
        (Some(departures), Some(arrivals), Some(points)) => {
            match routing::candidates(&routing::Inputs {
                origin: &input.origin,
                destination: &input.destination,
                departure_runway: &input.runway,
                arrival_runway: &input.arrival_runway,
                departures,
                arrivals,
                points,
                mora: &mora,
                budget: policy.terminal_candidate_limit,
                direct: input.counts[3] > 0,
            }) {
                Ok(routes) => routes,
                Err(error) => {
                    warnings.push(error);
                    Vec::new()
                }
            }
        }
        _ => Vec::new(),
    };
    if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
        traffic_log(
            openatc_settings::TrafficLog::Summary,
            format!(
                "terminal candidates {} resolved in {:.2} s",
                terminal_routes.len(),
                candidates_started.elapsed().as_secs_f64()
            ),
        );
    }
    let surfaces_started = std::time::Instant::now();
    let mut terminal_surfaces = BTreeMap::new();
    for (airport, name) in [
        (&input.origin, &input.runway),
        (&input.destination, &input.arrival_runway),
    ] {
        let surface = (|| {
            let thresholds = if input.simulator_root.is_empty() {
                let runway = airport
                    .runways
                    .iter()
                    .find(|runway| runway.first_name == *name || runway.second_name == *name)
                    .ok_or("Runway surface needs a known runway end")?;
                vec![
                    openatc_core::arrival::Threshold {
                        runway: runway.first_name.clone(),
                        elevation_feet: airport.elevation_feet,
                    },
                    openatc_core::arrival::Threshold {
                        runway: runway.second_name.clone(),
                        elevation_feet: airport.elevation_feet,
                    },
                ]
            } else {
                openatc_core::arrival::ArrivalAirport::load(&input.simulator_root, airport.clone())?
                    .thresholds
            };
            runway::Surface::new(
                airport,
                name,
                &thresholds,
                policy.runway_slope_limit_percent,
            )
        })();
        match surface {
            Ok(surface) => {
                terminal_surfaces.insert(airport.icao.clone(), Arc::new(surface));
            }
            Err(error) => warnings.push(format!("{} runway {name}: {error}", airport.icao)),
        }
    }
    if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
        traffic_log(
            openatc_settings::TrafficLog::Summary,
            format!(
                "runway surfaces loaded in {:.2} s",
                surfaces_started.elapsed().as_secs_f64()
            ),
        );
    }
    let ga_start = terminal_routes.len();
    let ga_points = input
        .origin
        .parking
        .iter()
        .filter(|stand| stand.kind == "tie_down" || stand.operations.contains("general_aviation"))
        .map(|stand| stand.point)
        .collect::<Vec<_>>();
    let ga_points = if ga_points.is_empty() {
        input
            .origin
            .parking
            .iter()
            .map(|stand| stand.point)
            .collect()
    } else {
        ga_points
    };
    let plan_distance = |name: &str| {
        let Some(runway) = input
            .origin
            .runways
            .iter()
            .find(|runway| runway.first_name == name || runway.second_name == name)
        else {
            return f64::INFINITY;
        };
        ga_points
            .iter()
            .map(|point| {
                (point.east - runway.first.east)
                    .hypot(point.north - runway.first.north)
                    .min((point.east - runway.second.east).hypot(point.north - runway.second.north))
            })
            .fold(f64::INFINITY, f64::min)
    };
    let mut ga_departure = None;
    if input.counts[3] > 0
        && !input.simulator_root.is_empty()
        && let (Some(weather), Some(departures), Some(arrivals), Some(points)) = (
            environments.get(&input.origin.icao),
            departure_procedures.as_ref(),
            arrival_procedures.as_ref(),
            terminal_points.as_ref(),
        )
    {
        let alternate = same_flow_runways(&input.origin, &input.runway, weather)
            .into_iter()
            .filter(|name| *name != input.runway)
            .map(|name| (plan_distance(&name), name))
            .min_by(|first, second| first.0.total_cmp(&second.0));
        if let Some((_, name)) = alternate
            && let Ok(routes) = routing::candidates(&routing::Inputs {
                origin: &input.origin,
                destination: &input.destination,
                departure_runway: &name,
                arrival_runway: &input.arrival_runway,
                departures,
                arrivals,
                points,
                mora: &mora,
                budget: policy.terminal_candidate_limit,
                direct: true,
            })
            && let Ok(data) = openatc_core::arrival::ArrivalAirport::load(
                &input.simulator_root,
                input.origin.clone(),
            )
            && let Ok(surface) = runway::Surface::new(
                &input.origin,
                &name,
                &data.thresholds,
                policy.runway_slope_limit_percent,
            )
            && !routes.is_empty()
        {
            let end = ga_start + routes.len();
            terminal_routes.extend(routes);
            ga_departure = Some((name, Arc::new(surface), end));
        }
    }
    let mut plans = Vec::new();
    if let Some(surface) = terminal_surfaces.get(&input.origin.icao) {
        plans.push((input.runway.clone(), 0, ga_start, surface.clone()));
    }
    if let Some((name, surface, end)) = ga_departure {
        plans.push((name, ga_start, end, surface));
    }
    let mut controller_terminals = BTreeMap::new();
    let mut nominal_departures = BTreeMap::new();
    let mut decoded_terminals = BTreeMap::new();
    let mut nominal_arrivals = BTreeMap::new();
    let mut nominal_flights = BTreeMap::new();
    let mut flights = BTreeMap::new();
    let mut spoken_callsigns = BTreeMap::new();
    let mut reserved_arrival_stands = input.reserved_stands.clone();
    let mut occupied_arrival_cells = input.reserved_stand_cells.clone();
    let mut surface_corridors = input.reserved_surface_corridors.clone();
    let mut terminal_usage = BTreeMap::new();
    let mut turnaround_inputs = BTreeMap::new();
    let mut arrival_refusals = 0usize;
    let mut vacatable_cache = BTreeMap::<String, Result<(), String>>::new();
    for (category, count) in classes.into_iter().zip(input.counts) {
        let mut category_origin = origin.clone();
        if category == Category::GeneralAviation
            && let Some(longest) = plans
                .iter()
                .map(|(_, _, _, surface)| surface.length)
                .reduce(f64::max)
        {
            category_origin.runway = longest;
        }
        for _ in 0..count.min(63) {
            if result.len() >= 63 {
                break;
            }
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let Some(choice) = input.profiles.select_where(
                &category_origin,
                &destination,
                distance,
                category,
                &input.models,
                seed,
                |_, aircraft| {
                    let radius = aircraft.wingspan.max(aircraft.length) / 2.0;
                    input
                        .origin
                        .parking
                        .iter()
                        .enumerate()
                        .any(|(index, stand)| {
                            !used.contains(&index)
                                && openatc_core::parking::moving_supports(stand, aircraft)
                                && cells(
                                    &input.origin,
                                    stand.point,
                                    radius,
                                    policy.conflict_cell_meters,
                                )
                                .is_disjoint(&input.reserved_stand_cells)
                                && cells(
                                    &input.origin,
                                    stand.point,
                                    aircraft.wingspan.max(aircraft.length) / 2.0,
                                    policy.conflict_cell_meters,
                                )
                                .is_disjoint(&surface_corridors)
                        })
                        && input.destination.parking.iter().any(|stand| {
                            !reserved_arrival_stands.contains(&stand.name)
                                && openatc_core::parking::moving_supports(stand, aircraft)
                                && cells(
                                    &input.destination,
                                    stand.point,
                                    radius,
                                    policy.conflict_cell_meters,
                                )
                                .is_disjoint(&occupied_arrival_cells)
                                && cells(
                                    &input.destination,
                                    stand.point,
                                    radius,
                                    policy.conflict_cell_meters,
                                )
                                .is_disjoint(&surface_corridors)
                        })
                },
            ) else {
                warnings.push(format!(
                    "{category:?}: no compatible fleet/model for {} to {}",
                    origin.icao, destination.icao
                ));
                continue;
            };
            let Some(aircraft) = input.profiles.aircraft_for(&choice.aircraft) else {
                continue;
            };
            let Some(kind) = aircraft.handling else {
                continue;
            };
            let callsign = if choice.operator.is_empty() {
                let Some(registration) =
                    input.profiles.airlines[&choice.airline].registration(&input.origin.icao, seed)
                else {
                    warnings.push(format!(
                        "{}: no registration pattern for {}",
                        choice.airline, input.origin.icao
                    ));
                    continue;
                };
                registration
            } else {
                format!("{}{}", choice.operator, 100 + seed % 8900)
            };
            if result.iter().any(|frame| frame.target.callsign == callsign)
                || input
                    .observed
                    .iter()
                    .any(|target| target.callsign == callsign)
            {
                continue;
            }
            if !handling.contains_key(&kind) {
                continue;
            }
            let Some(model) = input
                .models
                .models
                .iter()
                .find(|model| model.id == choice.model)
            else {
                continue;
            };
            // A runway may only be committed to this arrival when the type can leave it
            // through a verified forward exit inside the runway-end envelope.
            if let (Some(surface), Some(environment)) = (
                terminal_surfaces.get(&input.destination.icao),
                environments.get(&input.destination.icao),
            ) {
                let outcome = vacatable_cache
                    .entry(choice.aircraft.clone())
                    .or_insert_with(|| {
                        landing::vacatable(
                            &input.destination,
                            &input.arrival_runway,
                            aircraft,
                            &handling[&kind],
                            &policy,
                            surface,
                            environment,
                        )
                    });
                if let Err(reason) = outcome {
                    warnings.push(format!(
                        "{} / {}: arrival runway {} cannot be vacated by {}: {reason}",
                        input.destination.icao, callsign, input.arrival_runway, choice.aircraft
                    ));
                    arrival_refusals += 1;
                    continue;
                }
            }
            let preference = openatc_core::parking::TrafficPreference {
                operator: choice.operator.clone(),
                operation: match category {
                    Category::Airline | Category::Regional => "airline",
                    Category::Cargo => "cargo",
                    Category::GeneralAviation => "general_aviation",
                }
                .into(),
            };
            let mut planned_stand = None;
            let mut planned_gate = false;
            let mut stands =
                input
                    .origin
                    .parking
                    .iter()
                    .enumerate()
                    .filter(|(index, stand)| {
                        if used.contains(index)
                            || !cells(
                                &input.origin,
                                stand.point,
                                aircraft.wingspan.max(aircraft.length) / 2.0,
                                policy.conflict_cell_meters,
                            )
                            .is_disjoint(&input.reserved_stand_cells)
                            || !cells(
                                &input.origin,
                                stand.point,
                                aircraft.wingspan.max(aircraft.length) / 2.0,
                                policy.conflict_cell_meters,
                            )
                            .is_disjoint(&surface_corridors)
                            || !openatc_core::parking::moving_supports(stand, aircraft)
                        {
                            return false;
                        }
                        let point = position(&input.origin, stand.point);
                        let safe = (aircraft.wingspan + aircraft.length)
                            .max(policy.stand_clearance_meters)
                            / 1852.0;
                        openatc_core::arrival::distance_nm(point, input.own) > safe
                            && input.observed.iter().filter(|target| target.on_ground).all(
                                |target| {
                                    openatc_core::arrival::distance_nm(
                                        point,
                                        [target.latitude, target.longitude],
                                    ) > safe
                                },
                            )
                            && result.iter().all(|frame| {
                                openatc_core::arrival::distance_nm(
                                    point,
                                    [frame.target.latitude, frame.target.longitude],
                                ) > safe
                            })
                    })
                    .collect::<Vec<_>>();
            stands.sort_by_key(|(index, stand)| {
                (preference.rank(stand, aircraft.parking_size()), *index)
            });
            stands.truncate(32);
            if stands.is_empty() {
                warnings.push(format!(
                    "{}: no free equipment-compatible stand for {}",
                    input.origin.icao, choice.aircraft
                ));
                continue;
            }
            for (index, stand) in stands {
                used.insert(index);
                let point = position(&input.origin, stand.point);
                if plans.is_empty() {
                    continue;
                }
                let plan_index = if category == Category::GeneralAviation {
                    plans
                        .iter()
                        .enumerate()
                        .min_by(|first, second| {
                            let distance = |plan: &(String, usize, usize, Arc<runway::Surface>)| {
                                let Some(runway) = input.origin.runways.iter().find(|runway| {
                                    runway.first_name == plan.0 || runway.second_name == plan.0
                                }) else {
                                    return f64::INFINITY;
                                };
                                (stand.point.east - runway.first.east)
                                    .hypot(stand.point.north - runway.first.north)
                                    .min(
                                        (stand.point.east - runway.second.east)
                                            .hypot(stand.point.north - runway.second.north),
                                    )
                            };
                            distance(first.1).total_cmp(&distance(second.1))
                        })
                        .map_or(0, |(index, _)| index)
                } else {
                    plans
                        .iter()
                        .position(|plan| plan.0 == input.runway)
                        .unwrap_or(0)
                };
                let (departure_runway, candidate_start, candidate_end, origin_surface) =
                    &plans[plan_index];
                let departure_candidates = &terminal_routes[*candidate_start..*candidate_end];
                let candidate_offset = *candidate_start;
                let Some(id) = (1..=126).map(|slot| 0x00f0_0000 + slot).find(|id| {
                    !input.observed.iter().any(|target| target.id == *id)
                        && !result.iter().any(|frame| frame.target.id == *id)
                }) else {
                    return Err("No unused generated traffic identity".into());
                };
                if aircraft.fixed_gear {
                    fixed_gear.insert(id);
                }
                let taxi_started = std::time::Instant::now();
                let planned = ground_route(
                    &input.origin,
                    stand.point,
                    stand.heading,
                    stand.kind == "gate",
                    aircraft,
                    &handling[&kind],
                    departure_runway.as_str(),
                    seed,
                    &policy,
                    false,
                    &input.observed,
                    true,
                );
                if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
                    traffic_log(
                        openatc_settings::TrafficLog::Summary,
                        format!(
                            "taxi route {} took {:.2} s",
                            callsign,
                            taxi_started.elapsed().as_secs_f64()
                        ),
                    );
                }
                if let Ok(route) = planned {
                    if (0..512)
                        .map_while(|index| route.route.segment(index))
                        .any(|segment| {
                            segment
                                .resources
                                .iter()
                                .any(|resource| input.reserved_stand_cells.contains(resource))
                        })
                    {
                        warnings.push(format!(
                            "{} / {}: departure taxi route crosses a committed arrival footprint",
                            input.origin.icao, stand.name
                        ));
                        continue;
                    }
                    let hold = route.route.end_position();
                    let departure_corridor = (0..512)
                        .map_while(|index| route.route.segment(index))
                        .flat_map(|segment| segment.resources.iter().cloned())
                        .collect::<BTreeSet<_>>();
                    if world.insert(world.generation(), id, Arc::new(route.route)) {
                        surface_corridors.extend(departure_corridor);
                        orders.insert(id, route.orders);
                        world.ground_rates(
                            id,
                            handling[&kind].acceleration_knots_sec,
                            handling[&kind].braking_knots_sec,
                        );
                        movement.insert(id, handling[&kind].clone());
                        let departure_started = std::time::Instant::now();
                        let departure_surface = (|| {
                            let environment = environments
                                .get(&input.origin.icao)
                                .ok_or("Departure surface needs fresh airport weather")?;
                            departure::plan(
                                &input.origin,
                                departure_runway.as_str(),
                                hold,
                                aircraft,
                                &handling[&kind],
                                environment,
                                origin_surface,
                                &policy,
                            )
                        })();
                        if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
                            traffic_log(
                                openatc_settings::TrafficLog::Summary,
                                format!(
                                    "departure surface {} took {:.2} s",
                                    callsign,
                                    departure_started.elapsed().as_secs_f64()
                                ),
                            );
                        }
                        match departure_surface {
                            Ok(surface) => {
                                let arrival_started = std::time::Instant::now();
                                let decoded = (|| {
                                    let points = terminal_points
                                        .as_ref()
                                        .ok_or("Terminal coordinates are unavailable".to_owned())?;
                                    let departure_weather = environments
                                        .get(&input.origin.icao)
                                        .ok_or("Departure weather is unavailable".to_owned())?;
                                    let arrival_weather = environments
                                        .get(&input.destination.icao)
                                        .ok_or("Arrival weather is unavailable".to_owned())?;
                                    let departure_surface = origin_surface.as_ref();
                                    let arrival_surface =
                                        terminal_surfaces.get(&input.destination.icao).ok_or(
                                            "Arrival runway surface is unavailable".to_owned(),
                                        )?;
                                    let usage_view = terminal_usage
                                        .iter()
                                        .filter(|(index, _)| {
                                            **index >= candidate_offset
                                                && **index
                                                    < candidate_offset + departure_candidates.len()
                                        })
                                        .map(|(index, count)| (*index - candidate_offset, *count))
                                        .collect::<BTreeMap<_, _>>();
                                    airborne::Planner {
                                        regions: &input.regions,
                                        rotation: surface.rotation,
                                        origin: &input.origin,
                                        destination: &input.destination,
                                        points,
                                        mora: &mora,
                                        departure_weather,
                                        arrival_weather,
                                        aircraft,
                                        handling: &handling[&kind],
                                        policy: &policy,
                                        departure_surface,
                                        arrival_surface,
                                        observed: &input.observed,
                                        reserved: &reserved_arrival_stands,
                                        occupied_stand_cells: &occupied_arrival_cells,
                                        parking_exclusions: &surface_corridors,
                                        preference: &preference,
                                        // Use a direct approach entry when no STAR joins the
                                        // available approach. This keeps the local airport
                                        // populated instead of rejecting the whole category.
                                        allow_direct: true,
                                    }
                                    .arrival(departure_candidates, &usage_view)
                                })();
                                if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
                                    traffic_log(
                                        openatc_settings::TrafficLog::Summary,
                                        format!(
                                            "arrival route {} took {:.2} s",
                                            callsign,
                                            arrival_started.elapsed().as_secs_f64()
                                        ),
                                    );
                                }
                                match decoded {
                                    Ok((mut decoded, arrival)) => {
                                        decoded.candidate += candidate_offset;
                                        planned_stand = Some(arrival.parking.stand.clone());
                                        planned_gate = input
                                            .destination
                                            .parking
                                            .iter()
                                            .find(|stand| stand.name == arrival.parking.stand)
                                            .is_some_and(|stand| stand.kind == "gate");
                                        let arrival = Arc::new(arrival);
                                        let joined = assembly::Draft::new(
                                            &surface,
                                            &decoded,
                                            arrival.clone(),
                                            &input.destination.icao,
                                            &input.arrival_runway,
                                            aircraft,
                                            &handling[&kind],
                                            &policy,
                                            Arc::new(controller::Flight {
                                                origin: controller_origin.clone(),
                                                destination: controller_destination.clone(),
                                                departure_runway: departure_runway.clone(),
                                                arrival_runway: input.arrival_runway.clone(),
                                                terminal: controller_terminals
                                                    .entry(decoded.candidate)
                                                    .or_insert_with(|| {
                                                        Arc::new(
                                                            terminal_routes[decoded.candidate]
                                                                .clone(),
                                                        )
                                                    })
                                                    .clone(),
                                                callsign: callsign.clone(),
                                                stations: input.stations.clone(),
                                                regions: controller_regions.clone(),
                                                departure_weather: environments[&input.origin.icao]
                                                    .clone(),
                                                arrival_weather: environments
                                                    [&input.destination.icao]
                                                    .clone(),
                                                missed: decoded.missed_instruction.clone(),
                                                amendment: decoded.amendment.clone(),
                                                scope_seconds: 900.0
                                                    - policy.airborne_prediction_seconds
                                                    - policy.flight_clearance_lead_seconds,
                                            }),
                                        );
                                        match joined {
                                            Ok(draft) => {
                                                reserved_arrival_stands
                                                    .insert(arrival.parking.stand.clone());
                                                surface_corridors.extend(
                                                    (0..512)
                                                        .map_while(|index| {
                                                            arrival
                                                                .parking
                                                                .surface
                                                                .route
                                                                .segment(index)
                                                        })
                                                        .flat_map(|segment| {
                                                            segment.resources.iter().cloned()
                                                        }),
                                                );
                                                if let Some(parked) = (0..512)
                                                    .map_while(|index| {
                                                        arrival.parking.surface.route.segment(index)
                                                    })
                                                    .last()
                                                {
                                                    occupied_arrival_cells
                                                        .extend(parked.resources.iter().cloned());
                                                }
                                                match terrain.as_ref() {
                                                    Ok(terrain) => {
                                                        let production_started =
                                                            std::time::Instant::now();
                                                        let produced = draft
                                                            .clone()
                                                            .production(terrain, &mora);
                                                        if std::env::var_os(
                                                            "OPENATC_TRAFFIC_PROFILE",
                                                        )
                                                        .is_some()
                                                        {
                                                            traffic_log(openatc_settings::TrafficLog::Summary, format!("corridor release {callsign} took {:.2} s", production_started.elapsed().as_secs_f64()));
                                                        }
                                                        match produced {
                                                            Ok(prepared) => {
                                                                flights.insert(id, Arc::new(prepared));
                                                            }
                                                            Err(error) => warnings.push(format!(
                                                                "{callsign}: corridor release: {error}"
                                                            )),
                                                        }
                                                    }
                                                    Err(error) => warnings.push(format!(
                                                        "{callsign}: scenery: {error}"
                                                    )),
                                                }
                                                nominal_flights.insert(id, Arc::new(draft));
                                                *terminal_usage
                                                    .entry(decoded.candidate)
                                                    .or_insert(0) += 1;
                                            }
                                            Err(error) => warnings.push(format!(
                                                "{callsign}: complete flight: {error}"
                                            )),
                                        }
                                        nominal_arrivals.insert(id, arrival);
                                        decoded_terminals.insert(id, Arc::new(decoded));
                                    }
                                    Err(error) => {
                                        if error.contains("No verified forward runway exit") {
                                            arrival_refusals += 1;
                                            warnings.push(format!(
                                                "{} / {} / {} {}: arrival runway {} cannot be vacated: {error}",
                                                input.origin.icao,
                                                stand.name,
                                                callsign,
                                                choice.aircraft,
                                                input.arrival_runway
                                            ));
                                        } else {
                                            warnings.push(format!(
                                                "{} / {} / {} {}: terminal inputs: {error}",
                                                input.origin.icao,
                                                stand.name,
                                                callsign,
                                                choice.aircraft
                                            ));
                                        }
                                    }
                                }
                                nominal_departures.insert(id, Arc::new(surface));
                            }
                            Err(error) => warnings.push(format!(
                                "{} / {}: departure surface: {error}",
                                input.origin.icao, stand.name
                            )),
                        }
                    } else {
                        warnings.push(format!(
                            "{} / {}: stand protection overlaps another generated aircraft",
                            input.origin.icao, stand.name
                        ));
                        fixed_gear.remove(&id);
                        continue;
                    }
                } else if let Err(error) = planned {
                    warnings.push(format!(
                        "{} / {} / {}: {error}",
                        input.origin.icao, stand.name, choice.aircraft
                    ));
                    fixed_gear.remove(&id);
                    continue;
                }
                if !choice.operator.is_empty()
                    && !choice.spoken_operator.trim().is_empty()
                    && let Some(number) = callsign.strip_prefix(&choice.operator)
                {
                    spoken_callsigns.insert(
                        callsign.clone(),
                        format!("{} {number}", choice.spoken_operator),
                    );
                }
                turnaround_inputs.insert(
                    id,
                    turnaround::Input {
                        profile: aircraft.clone(),
                        preference: preference.clone(),
                        general_aviation: category == Category::GeneralAviation,
                        gate: planned_gate,
                        origin: controller_origin.clone(),
                        destination: controller_destination.clone(),
                        origin_runway: input.runway.clone(),
                        destination_runway: input.arrival_runway.clone(),
                        arrival_stand: planned_stand.clone().unwrap_or_default(),
                        departure_procedures: arrival_procedures.clone(),
                        arrival_procedures: departure_procedures.clone(),
                        destination_surface: terminal_surfaces
                            .get(&input.destination.icao)
                            .cloned(),
                        origin_surface: terminal_surfaces.get(&input.origin.icao).cloned(),
                    },
                );
                result.push(GeneratedTraffic {
                    vertical_speed_fpm: 0.0,
                    body_heading_degrees: Some(stand.heading),
                    target: TrafficTarget {
                        id,
                        callsign,
                        name: input.profiles.airlines[&choice.airline].name.clone(),
                        aircraft: choice.aircraft,
                        latitude: point[0],
                        longitude: point[1],
                        altitude_feet: input.origin.elevation_feet,
                        track_degrees: stand.heading,
                        on_ground: true,
                        ground_speed_knots: Some(0.0),
                        wingspan_meters: Some(aircraft.wingspan),
                        ..Default::default()
                    },
                    model: shared_models
                        .entry(model.id.clone())
                        .or_insert_with(|| Arc::new(model.clone()))
                        .clone(),
                    animation: Animation::default(),
                });
                break;
            }
        }
    }
    if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
        traffic_log(
            openatc_settings::TrafficLog::Summary,
            format!(
                "{} flights planned in {:.2} s (scene total {:.2} s)",
                result.len(),
                setup_started.elapsed().as_secs_f64(),
                setup_started.elapsed().as_secs_f64()
            ),
        );
    }
    if result.is_empty() && input.counts.iter().any(|count| *count > 0) {
        return Err(if warnings.is_empty() {
            "No eligible generated aircraft".into()
        } else {
            warnings.into_iter().take(8).collect::<Vec<_>>().join("; ")
        });
    }
    if !decoded_terminals.is_empty() && environments.values().any(|weather| !weather.fresh()) {
        decoded_terminals.clear();
        nominal_arrivals.clear();
        nominal_flights.clear();
        flights.clear();
        warnings.push("Terminal weather expired before scene preparation completed".into());
    }
    for airport in [&input.origin, &input.destination] {
        if !environments.contains_key(&airport.icao) {
            warnings.push(format!(
                "{} needs fresh airport weather before an airborne flight can be planned",
                airport.icao
            ));
        }
    }
    let simulator_root = input.simulator_root.clone();
    Ok(Scene {
        surface_corridors: Arc::new(surface_corridors),
        require_complete_flights: !input.simulator_root.is_empty(),
        airports: [&input.origin, &input.destination]
            .into_iter()
            .map(|airport| (airport.icao.clone(), Arc::new(airport.clone())))
            .collect(),
        frames: result,
        world,
        movement,
        airport: input.origin,
        destination: input.destination,
        departure_procedures,
        arrival_procedures,
        terminal_points,
        published_holds,
        nominal_departures,
        decoded_terminals,
        nominal_arrivals,
        nominal_flights,
        flights,
        spoken_callsigns,
        terminal_routes,
        environments,
        policy,
        fixed_gear,
        channels,
        orders,
        turnaround_inputs: Arc::new(turnaround_inputs),
        stations: controller_stations,
        regions: controller_regions,
        mora,
        simulator_root,
        reserved_stands: reserved_arrival_stands,
        occupied_stand_cells: Arc::new(occupied_arrival_cells),
        arrival_refusals,
        warnings,
        seeded_stations: BTreeMap::new(),
        advisories: BTreeMap::new(),
        local_spares: Vec::new(),
    })
}

impl Scene {
    fn observe_prepared(&self, request: &mut Preparation) {
        request
            .reserved_surface_corridors
            .extend(self.surface_corridors.iter().cloned());
        request
            .observed
            .extend(self.frames.iter().map(|frame| frame.target.clone()));
        for frame in &self.frames {
            if let Some(parked) = self.world.current_segment(frame.target.id) {
                request
                    .reserved_stand_cells
                    .extend(parked.resources.iter().cloned());
            }
        }
        for (id, arrival) in &self.nominal_arrivals {
            request.reserved_surface_corridors.extend(
                (0..512)
                    .map_while(|index| arrival.parking.surface.route.segment(index))
                    .flat_map(|segment| segment.resources.iter().cloned()),
            );
            if let Some(segment) = (0..512)
                .map_while(|index| arrival.parking.surface.route.segment(index))
                .last()
            {
                request
                    .reserved_stand_cells
                    .extend(segment.resources.iter().cloned());
                if let (Some(frame), Some(point)) = (
                    self.frames.iter().find(|frame| frame.target.id == *id),
                    segment.points.last(),
                ) {
                    let mut parked = frame.target.clone();
                    parked.latitude = point.latitude;
                    parked.longitude = point.longitude;
                    parked.altitude_feet = point.altitude_feet;
                    parked.on_ground = true;
                    parked.ground_speed_knots = Some(0.0);
                    request.observed.push(parked);
                }
            }
        }
    }
    fn weather_fresh(&self) -> bool {
        self.environments
            .values()
            .all(environment::Environment::fresh)
            && self
                .nominal_flights
                .values()
                .all(|draft| draft.weather_fresh())
    }
    fn merge(mut self, mut other: Self) -> Result<Self, String> {
        if self
            .airports
            .keys()
            .chain(other.airports.keys())
            .collect::<BTreeSet<_>>()
            .len()
            > 8
        {
            return Err("Traffic network exceeds its airport budget".into());
        }
        let callsigns = self
            .frames
            .iter()
            .map(|frame| frame.target.callsign.to_ascii_uppercase())
            .collect::<BTreeSet<_>>();
        if other
            .frames
            .iter()
            .any(|frame| callsigns.contains(&frame.target.callsign.to_ascii_uppercase()))
        {
            return Err("Traffic network has duplicate aircraft callsigns".into());
        }
        if self.policy != other.policy
            || self.channels != other.channels
            || self.require_complete_flights != other.require_complete_flights
        {
            return Err("Traffic operation definitions changed during preparation".into());
        }
        let stale = !self.weather_fresh() || !other.weather_fresh();
        if !self.world.merge_prepared(other.world) {
            return Err("Prepared traffic directions have conflicting movement ownership".into());
        }
        let offset = self.terminal_routes.len();
        for decoded in other.decoded_terminals.values_mut() {
            Arc::make_mut(decoded).candidate += offset;
        }
        for (id, airport) in &other.airports {
            if self
                .airports
                .get(id)
                .is_some_and(|existing| existing != airport)
            {
                return Err("Airport scenery changed during traffic preparation".into());
            }
        }
        Arc::make_mut(&mut self.surface_corridors).extend(other.surface_corridors.iter().cloned());
        self.airports.extend(other.airports);
        self.frames.extend(other.frames);
        self.movement.extend(other.movement);
        self.nominal_departures.extend(other.nominal_departures);
        self.decoded_terminals.extend(other.decoded_terminals);
        self.nominal_arrivals.extend(other.nominal_arrivals);
        self.nominal_flights.extend(other.nominal_flights);
        self.flights.extend(other.flights);
        self.spoken_callsigns.extend(other.spoken_callsigns);
        self.terminal_routes.extend(other.terminal_routes);
        self.environments.extend(other.environments);
        self.fixed_gear.extend(other.fixed_gear);
        self.orders.extend(other.orders);
        Arc::make_mut(&mut self.turnaround_inputs).extend(
            other
                .turnaround_inputs
                .iter()
                .map(|(id, input)| (*id, input.clone())),
        );
        self.reserved_stands.extend(other.reserved_stands);
        Arc::make_mut(&mut self.occupied_stand_cells)
            .extend(other.occupied_stand_cells.iter().cloned());
        self.arrival_refusals += other.arrival_refusals;
        self.warnings.extend(other.warnings);
        self.seeded_stations.extend(other.seeded_stations);
        self.advisories.extend(other.advisories);
        self.local_spares.extend(other.local_spares);
        if stale || !self.weather_fresh() {
            self.decoded_terminals.clear();
            self.nominal_arrivals.clear();
            self.nominal_flights.clear();
            self.flights.clear();
            self.warnings.push(
                "Terminal weather changed or expired before traffic routes were merged".into(),
            );
        }
        Ok(self)
    }
}

impl Scene {
    /// Seed one aircraft ahead and one behind the player along the procedure they are flying.
    ///
    /// The route nearest the player identifies the SID/STAR: a flight that passes within a few
    /// miles and starts or ends at the local airport is on the same flow. Offsets are in
    /// nautical miles along that route, so the spacing survives corners.
    fn seed_player_sequence(
        &mut self,
        own: [f64; 2],
        airport: &Airport,
        ids: &[u32],
        seeded: &mut BTreeSet<u32>,
        target: usize,
    ) -> usize {
        use openatc_core::traffic::motion::Stage;
        let reference = [airport.reference_latitude, airport.reference_longitude];
        let mut candidates = Vec::new();
        for &id in ids {
            if seeded.contains(&id) {
                continue;
            }
            let Some(plan) = self.flights.get(&id) else {
                continue;
            };
            let route = &plan.route;
            let end = route.end_position();
            let ends_here =
                openatc_core::arrival::distance_nm([end.latitude, end.longitude], reference) < 3.0;
            if !ends_here {
                continue;
            }
            let Some((nearest, distance)) = route_nearest(route, own) else {
                continue;
            };
            if distance > 3.0 {
                continue;
            }
            candidates.push((id, distance, nearest));
        }
        candidates.sort_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        traffic_log(
            openatc_settings::TrafficLog::Detail,
            format!(
                "player sequence has {} candidates near {} (player {:.4}/{:.4})",
                candidates.len(),
                airport.icao,
                own[0],
                own[1]
            ),
        );
        let generation = self.world.generation();
        let mut placed = 0;
        for (slot, (id, _, nearest)) in candidates.into_iter().enumerate() {
            if placed >= 2 || seeded.len() >= target {
                break;
            }
            let Some(plan) = self.flights.get(&id).cloned() else {
                continue;
            };
            let forward = slot % 2 == 0;
            let offset = if forward { 4.0 } else { 5.0 };
            let Some(index) =
                route_offset_index(&plan.route, &plan.clearances, nearest, offset, forward)
            else {
                continue;
            };
            if self
                .world
                .seed_airborne(generation, id, plan.route.clone(), index)
            {
                seeded.insert(id);
                placed += 1;
                traffic_log(
                    openatc_settings::TrafficLog::Detail,
                    format!(
                        "seeded {id} at segment {index} {} of the player's procedure",
                        if forward { "ahead" } else { "behind" }
                    ),
                );
            } else if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
                traffic_log(
                    openatc_settings::TrafficLog::Summary,
                    format!(
                        "could not seed {id} {} the player (segment {index})",
                        if forward { "ahead of" } else { "behind" }
                    ),
                );
            }
            let _ = Stage::Approach;
        }
        placed
    }

    /// Admit a bounded share of the released flights at varied lifecycle points.
    ///
    /// A scene with several released flights starts some already on approach, some already
    /// taxiing out, and spreads the remaining start-up calls by extending their gate dwell. A
    /// lone released aircraft is left untouched so the full ground-to-parking path is still
    /// exercised. Seeded aircraft hold only the resources of the segment they occupy, and a
    /// later boundary still requires its own controller clearance.
    fn seed_population(
        &mut self,
        own: [f64; 2],
        seed: u64,
        startup_cast: bool,
        taxi_target: usize,
        allow_single: bool,
        observed: &[TrafficTarget],
        player_airborne: bool,
        minimum_airborne_spawn_nm: f64,
    ) {
        if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
            traffic_log(
                openatc_settings::TrafficLog::Summary,
                format!(
                    "seed_population airborne {player_airborne}, {} flights, {} airports",
                    self.flights.len(),
                    self.airports.len()
                ),
            );
        }
        if self.simulator_root.is_empty()
            || self.flights.len() < 2 && !(allow_single && self.flights.len() == 1)
        {
            return;
        }
        let target = if player_airborne {
            self.flights.len()
        } else {
            self.flights.len() - self.flights.len() / 4
        };
        let mut ids: Vec<u32> = self.flights.keys().copied().collect();
        ids.sort_by(|first, second| {
            let position = |id: u32| {
                self.flights
                    .get(&id)
                    .and_then(|plan| {
                        airborne_seed(&plan.route, true).and_then(|index| plan.route.segment(index))
                    })
                    .and_then(|segment| segment.points.first())
                    .map(|point| [point.latitude, point.longitude])
                    .unwrap_or(own)
            };
            openatc_core::arrival::distance_nm(own, position(*first))
                .total_cmp(&openatc_core::arrival::distance_nm(own, position(*second)))
                .then_with(|| first.cmp(second))
        });
        let generation = self.world.generation();
        let mut seeded = BTreeSet::new();
        if player_airborne
            && let Some(airport) = self
                .airports
                .values()
                .min_by(|left, right| {
                    openatc_core::arrival::distance_nm(
                        own,
                        [left.reference_latitude, left.reference_longitude],
                    )
                    .total_cmp(&openatc_core::arrival::distance_nm(
                        own,
                        [right.reference_latitude, right.reference_longitude],
                    ))
                })
                .filter(|airport| {
                    openatc_core::arrival::distance_nm(
                        own,
                        [airport.reference_latitude, airport.reference_longitude],
                    ) <= self.policy.display_enter_nm
                })
                .cloned()
        {
            // Put one aircraft ahead and one behind on the procedure the player is flying, so
            // the sequence and its speed constraints reflect the route.
            self.seed_player_sequence(own, &airport, &ids, &mut seeded, target);
        }
        // The opening cast is drawn from the run seed: a complete plan for every aircraft,
        // with the mix chosen per start. Two departures can be at the runway (one at the hold
        // and one queued directly behind), two already partway through their taxi, and one
        // arrival taxiing to its gate. Later refills start at gates so replacements call Ground
        // instead of appearing at a runway that is already occupied.
        let mut startup_ground: BTreeMap<
            u32,
            (
                Arc<openatc_core::traffic::motion::Route>,
                usize,
                f64,
                &'static str,
            ),
        > = BTreeMap::new();
        let mut keep_at_gate = BTreeSet::new();
        let mut startup_airborne = BTreeSet::new();
        if !player_airborne {
            let mut departures = ids
                .iter()
                .filter_map(|id| {
                    let prepared = self.flights.get(id)?;
                    let start = prepared.route.start_position();
                    let end = prepared.route.end_position();
                    let local_departure =
                        openatc_core::arrival::distance_nm(own, [start.latitude, start.longitude])
                            <= self.policy.display_enter_nm
                            && openatc_core::arrival::distance_nm(
                                own,
                                [end.latitude, end.longitude],
                            ) > self.policy.display_enter_nm;
                    if !local_departure {
                        return None;
                    }
                    let taxi = self
                        .world
                        .assigned_route(*id)
                        .unwrap_or_else(|| prepared.route.clone());
                    let outbound = taxi_out_segments(&taxi);
                    (!outbound.is_empty()).then_some((*id, taxi, outbound))
                })
                .collect::<Vec<_>>();
            let mut arrivals = ids
                .iter()
                .filter_map(|id| {
                    let prepared = self.flights.get(id)?;
                    let start = prepared.route.start_position();
                    let end = prepared.route.end_position();
                    let local_arrival =
                        openatc_core::arrival::distance_nm(own, [start.latitude, start.longitude])
                            > self.policy.display_enter_nm
                            && openatc_core::arrival::distance_nm(
                                own,
                                [end.latitude, end.longitude],
                            ) <= self.policy.display_enter_nm;
                    if !local_arrival {
                        return None;
                    }
                    // The prepared flight route ends at the landing rollout. The threshold
                    // to parking taxi-in lives in the nominal arrival, so prefer it here.
                    let taxi = self
                        .nominal_arrivals
                        .get(id)
                        .map(|arrival| Arc::new(arrival.parking.surface.route.clone()))
                        .or_else(|| self.world.assigned_route(*id))
                        .unwrap_or_else(|| prepared.route.clone());
                    let inbound = taxi_in_segments(&taxi);
                    (!inbound.is_empty()).then_some((*id, taxi, inbound))
                })
                .collect::<Vec<_>>();
            shuffle(&mut departures, seed ^ 0x5DEE_CE66_D001_7CB5);
            shuffle(&mut arrivals, seed ^ 0xA076_1D64_78BD_642F);
            let total = self.flights.len();
            let gate_target = (total / 8).clamp(1, 3).min(departures.len());
            let usable = departures.len().saturating_sub(gate_target);
            let moving_target = taxi_target
                .min(usable)
                .max(usize::from(startup_cast && taxi_target > 0 && usable > 0));
            let hold_target = if startup_cast && moving_target > 1 {
                (moving_target / 3).clamp(1, 2).min(moving_target)
            } else {
                0
            };
            let mid_target = moving_target.saturating_sub(hold_target);
            let approach_target = if arrivals.is_empty() {
                0
            } else {
                (total / 4).clamp(1, 4).min(arrivals.len())
            };
            let mut used = BTreeSet::new();
            let mut trial = self.world.clone();
            if startup_cast {
                // Hold point: the first aircraft is complete on the final taxi segment. The
                // second is queued as close behind it as the reserved taxi edges allow.
                for slot in 0..hold_target {
                    for (id, taxi, outbound) in &departures {
                        if used.contains(id) {
                            continue;
                        }
                        // Keep the seeded hold aircraft on the taxiway before the runway
                        // boundary. The last surface segment can end on the runway itself.
                        let last = outbound.last().copied();
                        let before = outbound
                            .len()
                            .checked_sub(2)
                            .and_then(|index| outbound.get(index))
                            .copied();
                        let indexes = if slot == 0 {
                            before.or(last).into_iter().collect::<Vec<_>>()
                        } else {
                            outbound.iter().rev().skip(2).copied().collect::<Vec<_>>()
                        };
                        let progress = if slot == 0 { 1.0 } else { 0.9 };
                        let role = if slot == 0 {
                            "holding point"
                        } else {
                            "holding queue"
                        };
                        for index in indexes {
                            if trial.seed_ground_progress(
                                generation,
                                *id,
                                taxi.clone(),
                                index,
                                progress,
                            ) {
                                startup_ground.insert(*id, (taxi.clone(), index, progress, role));
                                used.insert(*id);
                                break;
                            }
                        }
                        if used.contains(id) {
                            break;
                        }
                    }
                }
            }
            // Keep the airport moving on every pass, not only the first. Candidate segments
            // are tried from the middle outward so two routes on the same apron do not choose
            // the same protected edge.
            for slot in 0..mid_target {
                'mid: for (id, taxi, outbound) in &departures {
                    if used.contains(id) {
                        continue;
                    }
                    let mut candidates = outbound
                        .iter()
                        .copied()
                        .filter(|index| Some(*index) != outbound.last().copied())
                        .collect::<Vec<_>>();
                    let middle = candidates.len() / 2;
                    candidates.rotate_left(middle);
                    for index in candidates {
                        let progress = 0.45 + (slot as f64 * 0.08).min(0.4);
                        if trial.seed_ground_progress(
                            generation,
                            *id,
                            taxi.clone(),
                            index,
                            progress,
                        ) {
                            startup_ground.insert(*id, (taxi.clone(), index, progress, "mid taxi"));
                            used.insert(*id);
                            break 'mid;
                        }
                    }
                }
            }
            // Arrivals are seeded on approach at different distances. Taxi-in exists only
            // after a landing, never as an opening scene state.
            for (slot, (id, _, _)) in arrivals.iter().enumerate() {
                if slot >= approach_target {
                    break;
                }
                if used.contains(id) {
                    continue;
                }
                let Some(plan) = self.flights.get(id).cloned() else {
                    continue;
                };
                let target_nm = 5.0 + slot as f64 * 3.0;
                let Some((index, progress)) = approach_seed(&plan.route, target_nm, own) else {
                    continue;
                };
                if trial.seed_airborne_progress(
                    generation,
                    *id,
                    plan.route.clone(),
                    index,
                    progress,
                ) {
                    used.insert(*id);
                    startup_airborne.insert(*id);
                }
            }
            // Keep a few complete departures at their stands so Ground always has live calls to
            // answer, even when the opening cast is otherwise moving.
            for (id, _, _) in &departures {
                if keep_at_gate.len() >= gate_target {
                    break;
                }
                if !used.contains(id) {
                    keep_at_gate.insert(*id);
                }
            }
            if !startup_ground.is_empty()
                || !keep_at_gate.is_empty()
                || !startup_airborne.is_empty()
            {
                self.world = trial;
            }
            let holding = startup_ground
                .values()
                .filter(|(_, _, _, role)| *role == "holding point")
                .count();
            let queue = startup_ground
                .values()
                .filter(|(_, _, _, role)| *role == "holding queue")
                .count();
            let mid_taxi = startup_ground
                .values()
                .filter(|(_, _, _, role)| *role == "mid taxi")
                .count();
            let approach = startup_airborne.len();
            traffic_log(
                openatc_settings::TrafficLog::Summary,
                format!(
                    "traffic cast seed {seed:#018x}: {holding} holding, {queue} queued behind, {mid_taxi} mid taxi, {approach} on approach, {} gates/airborne; taxi target {taxi_target}, {} departures, {} arrivals",
                    self.flights
                        .len()
                        .saturating_sub(startup_ground.len() + approach),
                    departures.len(),
                    arrivals.len()
                ),
            );
        }
        for id in startup_ground.keys().copied().collect::<Vec<_>>() {
            let Some((route, index, _, role)) = startup_ground.get(&id) else {
                continue;
            };
            let prepared = self.flights.get(&id).cloned();
            if *role == "taxi to gate"
                || prepared
                    .as_ref()
                    .is_some_and(|prepared| Arc::ptr_eq(route, &prepared.route))
            {
                if let Some(prepared) = &prepared {
                    let station = prepared
                        .clearances
                        .range(..=*index)
                        .next_back()
                        .map(|(_, clearance)| clearance.station.clone())
                        .or_else(|| {
                            prepared
                                .clearances
                                .values()
                                .next()
                                .map(|c| c.station.clone())
                        });
                    if let Some(station) = station {
                        self.orders.remove(&id);
                        self.seeded_stations.insert(id, station);
                    }
                }
            }
            seeded.insert(id);
        }
        // Seed any local arrivals as needed after the planned departure positions. The counter
        // deliberately excludes parked local departures, which must get their own radio call.
        let mut taxi_seeded = 0usize;
        let mut local_arrival_slot = 0usize;
        for (ordinal, id) in ids.iter().copied().enumerate() {
            if seeded.len() >= target {
                break;
            }
            if seeded.contains(&id) || keep_at_gate.contains(&id) {
                continue;
            }
            let Some(prepared) = self.flights.get(&id).cloned() else {
                continue;
            };
            if !player_airborne && ordinal % 4 == 3 {
                continue;
            }
            let taxi = self
                .world
                .assigned_route(id)
                .unwrap_or_else(|| prepared.route.clone());
            let mut choices: Vec<(bool, usize, Arc<openatc_core::traffic::motion::Route>, bool)> =
                Vec::new();
            // Classify by the flight plan, not the ground route: a taxi route always ends at
            // the local holding point, so its end cannot distinguish a departure from an
            // arrival.
            let plan_start = prepared.route.start_position();
            let plan_end = prepared.route.end_position();
            let local_ground = !player_airborne
                && openatc_core::arrival::distance_nm(
                    own,
                    [plan_start.latitude, plan_start.longitude],
                ) <= self.policy.display_enter_nm
                && openatc_core::arrival::distance_nm(own, [plan_end.latitude, plan_end.longitude])
                    > self.policy.display_enter_nm;
            let local_arrival = !player_airborne
                && openatc_core::arrival::distance_nm(
                    own,
                    [plan_start.latitude, plan_start.longitude],
                ) > self.policy.display_enter_nm
                && openatc_core::arrival::distance_nm(own, [plan_end.latitude, plan_end.longitude])
                    <= self.policy.display_enter_nm;
            let prefer_approach =
                local_arrival || (!player_airborne && seeded_number(seed, ordinal as u64) & 1 == 0);
            let stages = if local_arrival {
                // Local arrivals start on final. A long descent or cruise seed is what made
                // approaching traffic appear 20-35 NM away.
                vec![openatc_core::traffic::motion::Stage::Approach]
            } else if prefer_approach {
                vec![
                    openatc_core::traffic::motion::Stage::Approach,
                    openatc_core::traffic::motion::Stage::Descent,
                    openatc_core::traffic::motion::Stage::Cruise,
                    openatc_core::traffic::motion::Stage::Climb,
                ]
            } else {
                vec![
                    openatc_core::traffic::motion::Stage::Cruise,
                    openatc_core::traffic::motion::Stage::Climb,
                    openatc_core::traffic::motion::Stage::Descent,
                    openatc_core::traffic::motion::Stage::Approach,
                ]
            };
            for stage in stages {
                let mut indices: Vec<_> = (0..prepared.route.segment_count())
                    .filter(|index| {
                        prepared
                            .route
                            .segment(*index)
                            .is_some_and(|segment| segment.stage == stage)
                    })
                    .collect();
                indices.sort_by(|left, right| {
                    let distance = |index| {
                        prepared
                            .route
                            .segment(index)
                            .and_then(|segment| segment.points.first())
                            .map_or(f64::INFINITY, |point| {
                                openatc_core::arrival::distance_nm(
                                    own,
                                    [point.latitude, point.longitude],
                                )
                            })
                    };
                    distance(*left).total_cmp(&distance(*right))
                });
                choices.extend(
                    indices
                        .into_iter()
                        .map(|index| (true, index, prepared.route.clone(), local_arrival)),
                );
            }
            if !player_airborne
                && local_ground
                && taxi_seeded < 3
                && let Some(index) = ground_seed(&taxi)
            {
                choices.push((false, index, taxi.clone(), false));
            }
            for (airborne, index, route, arrival_seed) in choices {
                if arrival_seed && local_arrival_slot >= 3 {
                    // Three spaced arrivals on final are enough for the local scene; a fourth
                    // would have to start on an earlier STAR leg far from the airport.
                    continue;
                }
                let Some(point) = route
                    .segment(index)
                    .and_then(|segment| segment.points.first())
                else {
                    continue;
                };
                // Place local arrivals at a measured distance along the approach path, not
                // a fraction of whichever long STAR segment happened to be chosen. The slot
                // spacing keeps two finals apart while the first arrival starts close in.
                let target_nm = 8.0 + (local_arrival_slot as f64 * 5.0).min(12.0);
                let (index, progress) = if airborne && arrival_seed {
                    approach_seed(route.as_ref(), target_nm, own).unwrap_or((index, 0.0))
                } else {
                    (index, 0.0)
                };
                let distance =
                    openatc_core::arrival::distance_nm(own, [point.latitude, point.longitude]);
                // A descending or enroute flight may start beyond the drawn radius, so a STAR
                // arrival is already established when the player reaches it. Ground traffic and
                // departures climbing away stay local.
                let far_seed = airborne
                    && player_airborne
                    && route.segment(index).is_some_and(|segment| {
                        !matches!(segment.stage, openatc_core::traffic::motion::Stage::Climb)
                    });
                let horizon = if far_seed {
                    self.policy.display_leave_nm.max(ARRIVAL_PREPARE_NM - 20.0)
                } else {
                    self.policy.display_enter_nm
                };
                if distance > horizon || airborne && distance < minimum_airborne_spawn_nm {
                    continue;
                }
                if airborne
                    && route.segment(index).is_some_and(|segment| {
                        segment.stage == openatc_core::traffic::motion::Stage::Approach
                    })
                    && prepared
                        .clearances
                        .iter()
                        .find(|(_, clearance)| clearance.landing_rollout.is_some())
                        .is_some_and(|(&flare, _)| {
                            index >= flare
                                || route.nominal_seconds(index, flare - 1).unwrap_or(0.0)
                                    < self.policy.flight_clearance_lead_seconds
                        })
                {
                    continue;
                }
                if airborne
                    && observed
                        .iter()
                        .chain(
                            self.frames
                                .iter()
                                .filter(|frame| frame.target.id != id)
                                .map(|frame| &frame.target),
                        )
                        .any(|target| {
                            !target.on_ground
                                && (point.altitude_feet - target.altitude_feet).abs()
                                    < self.policy.vertical_minimum(
                                        point.altitude_feet.max(target.altitude_feet),
                                    )
                                && openatc_core::arrival::distance_nm(
                                    [point.latitude, point.longitude],
                                    [target.latitude, target.longitude],
                                ) < self.policy.airborne_separation_nm + 0.01
                        })
                {
                    continue;
                }
                let Some(station) = prepared
                    .clearances
                    .range(..=index)
                    .next_back()
                    .map(|(_, clearance)| clearance.station.clone())
                    .or_else(|| {
                        prepared
                            .clearances
                            .values()
                            .next()
                            .map(|clearance| clearance.station.clone())
                    })
                else {
                    continue;
                };
                let mut world = self.world.clone();
                let placed = if airborne {
                    if progress > 0.0 {
                        world.seed_airborne_progress(generation, id, route.clone(), index, progress)
                    } else {
                        world.seed_airborne(generation, id, route.clone(), index)
                    }
                } else {
                    world.seed_ground(generation, id, route.clone(), index)
                };
                if !placed
                    || Arc::ptr_eq(&route, &prepared.route)
                        && prepared.recovery.iter().any(|(boundary, recovery)| {
                            !world.recovery(generation, id, *boundary, recovery.clone())
                        })
                {
                    continue;
                }
                if let Some(rates) = self.movement.get(&id)
                    && !world.ground_rates(
                        id,
                        rates.acceleration_knots_sec,
                        rates.braking_knots_sec,
                    )
                {
                    continue;
                }
                self.world = world;
                if !airborne {
                    taxi_seeded += 1;
                }
                if let Some(segment) = route.segment(index) {
                    let role = startup_ground
                        .get(&id)
                        .map_or("airborne", |(_, _, _, role)| *role);
                    traffic_log(
                        openatc_settings::TrafficLog::Detail,
                        format!(
                            "startup {id} {:?} at segment {index}, {distance:.1} NM ({role})",
                            segment.stage,
                        ),
                    );
                }
                self.sync_frame(id);
                if airborne && arrival_seed {
                    local_arrival_slot = local_arrival_slot.saturating_add(1);
                }
                if airborne
                    && arrival_seed
                    && let Some(frame) = self.frames.iter().find(|frame| frame.target.id == id)
                {
                    traffic_log(
                        openatc_settings::TrafficLog::Summary,
                        format!(
                            "arrival {id} seeded {:.1} NM from the player on final",
                            openatc_core::arrival::distance_nm(
                                own,
                                [frame.target.latitude, frame.target.longitude]
                            )
                        ),
                    );
                }
                if Arc::ptr_eq(&route, &prepared.route) {
                    self.orders.remove(&id);
                    self.seeded_stations.insert(id, station);
                }
                seeded.insert(id);
                break;
            }
        }
        for id in &seeded {
            self.sync_frame(*id);
        }
        // Remaining aircraft stay at their stands, but their first call is spread so the scene
        // does not release every start-up request in one burst.
        let mut gate = 0usize;
        for id in ids {
            if seeded.contains(&id) {
                continue;
            }
            self.world
                .stagger_initial_dwell(generation, id, gate as f64 * 3.0);
            gate += 1;
        }
    }
}

#[cfg(test)]
impl Scene {
    fn seed_airborne(&mut self, own: [f64; 2], allow_single: bool) {
        self.seed_population(own, 1, false, 2, allow_single, &[], false, 0.0);
    }
}

impl Scene {
    /// Move a frame onto the pose the movement world actually holds.
    fn sync_frame(&mut self, id: u32) {
        let Some(pose) = self.world.snapshot().into_iter().find(|pose| pose.id == id) else {
            return;
        };
        let Some(frame) = self.frames.iter_mut().find(|frame| frame.target.id == id) else {
            return;
        };
        frame.target.latitude = pose.position.latitude;
        frame.target.longitude = pose.position.longitude;
        frame.target.altitude_feet = pose.position.altitude_feet;
        frame.target.track_degrees = pose.heading;
        frame.target.on_ground = pose.stage.on_ground();
        frame.target.ground_speed_knots = Some(pose.knots);
        frame.body_heading_degrees = Some(pose.heading);
        frame.vertical_speed_fpm = 0.0;
    }
}

fn taxi_out_segments(route: &openatc_core::traffic::motion::Route) -> Vec<usize> {
    use openatc_core::traffic::motion::Stage;
    (0..route.segment_count())
        .filter(|index| {
            route
                .segment(*index)
                .is_some_and(|segment| segment.stage == Stage::TaxiOut && segment.points.len() >= 2)
        })
        .collect()
}

fn taxi_in_segments(route: &openatc_core::traffic::motion::Route) -> Vec<usize> {
    use openatc_core::traffic::motion::Stage;
    (0..route.segment_count())
        .filter(|index| {
            route
                .segment(*index)
                .is_some_and(|segment| segment.stage == Stage::TaxiIn && segment.points.len() >= 2)
        })
        .collect()
}

/// Point on the approach that is `target_nm` from `own`, measured as the simulator shows it.
/// Searches from the runway backwards so a procedure turn yields the inbound crossing rather
/// than the outbound one.
fn approach_seed(
    route: &openatc_core::traffic::motion::Route,
    target_nm: f64,
    own: [f64; 2],
) -> Option<(usize, f64)> {
    use openatc_core::traffic::motion::Stage;
    let distance = |point: &openatc_core::traffic::motion::Waypoint| {
        openatc_core::arrival::distance_nm(own, [point.latitude, point.longitude])
    };
    let approach = |index: usize| {
        route
            .segment(index)
            .is_some_and(|segment| segment.stage == Stage::Approach)
    };
    let mut closest: Option<(usize, f64, f64)> = None;
    for index in (0..route.segment_count()).rev() {
        if !approach(index) {
            continue;
        }
        let Some(segment) = route.segment(index) else {
            break;
        };
        if segment.points.len() < 2 {
            continue;
        }
        let lengths = segment
            .points
            .windows(2)
            .map(|pair| {
                openatc_core::arrival::distance_nm(
                    [pair[0].latitude, pair[0].longitude],
                    [pair[1].latitude, pair[1].longitude],
                )
            })
            .collect::<Vec<_>>();
        let total = lengths.iter().sum::<f64>();
        if total <= 0.0 {
            continue;
        }
        let mut along = 0.0;
        for (window, pair) in segment.points.windows(2).zip(lengths.iter()) {
            let start = distance(&window[0]);
            let end = distance(&window[1]);
            if (end - start).abs() > f64::EPSILON {
                let offset = (target_nm - start) / (end - start);
                if (0.0..=1.0).contains(&offset) {
                    return Some((index, ((along + offset * pair) / total).clamp(0.0, 0.99)));
                }
            }
            along += pair;
        }
        // Keep a fallback for a target that falls between published fixes.
        for (sample, point) in segment.points.iter().enumerate() {
            let error = (distance(point) - target_nm).abs();
            if closest.is_none_or(|(_, _, previous)| error < previous) {
                let progress = if sample == 0 {
                    0.0
                } else {
                    (lengths.iter().take(sample).sum::<f64>() / total).clamp(0.0, 0.99)
                };
                closest = Some((index, progress, error));
            }
        }
    }
    closest.map(|(index, progress, _)| (index, progress))
}

/// Deterministic value for a seed and an ordinal, used to vary choices without a dependency.
fn seeded_number(seed: u64, value: u64) -> u64 {
    let mut z = seed ^ value.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn shuffle<T>(items: &mut [T], seed: u64) {
    let mut state = seed;
    for index in (1..items.len()).rev() {
        state = seeded_number(state, index as u64);
        let other = (state % (index as u64 + 1)) as usize;
        items.swap(index, other);
    }
}

fn airborne_seed(
    route: &openatc_core::traffic::motion::Route,
    prefer_approach: bool,
) -> Option<usize> {
    use openatc_core::traffic::motion::Stage;
    let mut stages = vec![Stage::Approach, Stage::Descent, Stage::Cruise, Stage::Climb];
    if !prefer_approach {
        stages.rotate_left(2);
    }
    stages.into_iter().find_map(|stage| {
        (0..route.segment_count()).find(|index| {
            route
                .segment(*index)
                .is_some_and(|segment| segment.stage == stage && segment.points.len() >= 2)
        })
    })
}

fn ground_seed(route: &openatc_core::traffic::motion::Route) -> Option<usize> {
    use openatc_core::traffic::motion::Stage;
    // Start departures at the last taxi segment (the runway queue) rather than at the gate.
    let departure = (0..route.segment_count()).rev().find(|index| {
        route
            .segment(*index)
            .is_some_and(|segment| segment.stage == Stage::TaxiOut && segment.points.len() >= 2)
    });
    // Arrivals start at the runway exit and taxi in.
    let arrival = (0..route.segment_count()).find(|index| {
        route
            .segment(*index)
            .is_some_and(|segment| segment.stage == Stage::TaxiIn && segment.points.len() >= 2)
    });
    departure.or(arrival)
}

/// Nearest route segment to a point, with the distance in nautical miles.
fn route_nearest(
    route: &openatc_core::traffic::motion::Route,
    point: [f64; 2],
) -> Option<(usize, f64)> {
    let mut best: Option<(usize, f64)> = None;
    for index in 0..route.segment_count() {
        let Some(segment) = route.segment(index) else {
            break;
        };
        for sample in &segment.points {
            let distance =
                openatc_core::arrival::distance_nm(point, [sample.latitude, sample.longitude]);
            if best.is_none_or(|(_, previous)| distance < previous) {
                best = Some((index, distance));
            }
        }
    }
    best
}

/// Segment reached by travelling an offset along the route, restricted to airborne stages.
fn route_offset_index(
    route: &openatc_core::traffic::motion::Route,
    clearances: &BTreeMap<usize, flight::Clearance>,
    nearest: usize,
    offset_nm: f64,
    forward: bool,
) -> Option<usize> {
    use openatc_core::traffic::motion::Stage;
    let mut travelled = 0.0;
    let mut index = nearest;
    loop {
        let segment = route.segment(index)?;
        let airborne = segment.points.len() >= 2
            && matches!(
                segment.stage,
                Stage::Climb | Stage::Cruise | Stage::Descent | Stage::Approach
            );
        // Only a clearance boundary keeps the seeded aircraft moving: it can request the
        // chapter it is about to enter instead of stopping at an ungranted boundary.
        if airborne && travelled >= offset_nm && clearances.contains_key(&index) {
            return Some(index);
        }
        let next = if forward {
            index.checked_add(1)?
        } else {
            index.checked_sub(1)?
        };
        if next >= route.segment_count() {
            return None;
        }
        travelled += segment
            .points
            .windows(2)
            .map(|pair| {
                openatc_core::arrival::distance_nm(
                    [pair[0].latitude, pair[0].longitude],
                    [pair[1].latitude, pair[1].longitude],
                )
            })
            .sum::<f64>();
        index = next;
    }
}

fn prepare_bidirectional(mut input: Preparation) -> Result<Scene, String> {
    let seed_single_arrival = input.seed_single_arrival;
    let minimum_airborne_spawn_nm = input.minimum_airborne_spawn_nm;
    let mut inbound = input.clone();
    std::mem::swap(&mut inbound.origin, &mut inbound.destination);
    std::mem::swap(&mut inbound.runway, &mut inbound.arrival_runway);
    let mut remaining = 63;
    for index in 0..4 {
        let count = input.counts[index].min(remaining);
        remaining -= count;
        input.counts[index] = count.div_ceil(2);
        inbound.counts[index] = count / 2;
    }
    let own = input.own;
    let seed = input.seed;
    let startup_cast = input.startup_cast;
    let taxi_target = input.taxi_target;
    let observed = input.observed.clone();
    let player_airborne = input.player_airborne;
    let first = prepare_direction(input);
    if inbound.counts.iter().all(|count| *count == 0) {
        let allow_single = seed_single_arrival;
        return first.map(|mut scene| {
            scene.seed_population(
                own,
                seed,
                startup_cast,
                taxi_target,
                allow_single,
                &observed,
                player_airborne,
                minimum_airborne_spawn_nm,
            );
            scene
        });
    }
    if let Ok(scene) = &first {
        scene.observe_prepared(&mut inbound);
    }
    let second = prepare_direction(inbound);
    let mut result = match (first, second) {
        (Ok(scene), Ok(reverse)) => scene.merge(reverse),
        (Ok(mut scene), Err(error)) | (Err(error), Ok(mut scene)) => {
            scene.warnings.push(format!(
                "Other traffic direction could not be prepared: {error}"
            ));
            Ok(scene)
        }
        (Err(first), Err(second)) => Err(format!(
            "Neither traffic direction could be prepared: {first}; {second}"
        )),
    };
    if let Ok(scene) = &mut result {
        scene.seed_population(
            own,
            seed,
            startup_cast,
            taxi_target,
            seed_single_arrival,
            &observed,
            player_airborne,
            minimum_airborne_spawn_nm,
        );
    }
    result
}

fn same_flow_runways(
    airport: &Airport,
    preferred: &str,
    weather: &environment::Environment,
) -> Vec<String> {
    let selected = crate::radio::atis_runway(
        &airport.runways,
        preferred,
        weather.wind_from,
        weather.wind_knots,
    );
    let mut ends = Vec::new();
    for runway in &airport.runways {
        let heading = (runway.second.east - runway.first.east)
            .atan2(runway.second.north - runway.first.north)
            .to_degrees()
            .rem_euclid(360.0);
        ends.extend([
            (runway.first_name.clone(), heading),
            (
                runway.second_name.clone(),
                (heading + 180.0).rem_euclid(360.0),
            ),
        ]);
    }
    let Some((_, heading)) = ends.iter().find(|(name, _)| *name == selected) else {
        return Vec::new();
    };
    std::iter::once(selected.clone())
        .chain(
            ends.iter()
                .filter(|(name, course)| {
                    *name != selected
                        && openatc_core::navigation::turn(*heading, *course).abs() <= 15.0
                        && crate::radio::atis_runway(
                            &airport.runways,
                            name,
                            weather.wind_from,
                            weather.wind_knots,
                        ) == *name
                })
                .map(|(name, _)| name.clone())
                .take(3),
        )
        .collect()
}

fn profile_category(profiles: &Profiles, name: &str) -> Option<Category> {
    let categories = profiles
        .airlines
        .values()
        .filter(|operator| operator.name == name)
        .map(|operator| operator.category)
        .collect::<BTreeSet<_>>();
    (categories.len() == 1).then(|| *categories.first().unwrap())
}

fn complete_mix_score(
    scene: &Scene,
    profiles: &Profiles,
    requested: &BTreeSet<Category>,
) -> (usize, usize, usize) {
    let represented = scene
        .frames
        .iter()
        .filter(|frame| {
            if scene.require_complete_flights {
                scene.flights.contains_key(&frame.target.id)
            } else {
                scene.nominal_flights.contains_key(&frame.target.id)
            }
        })
        .filter_map(|frame| profile_category(profiles, &frame.target.name))
        .filter(|category| requested.contains(category))
        .collect::<BTreeSet<_>>();
    (
        represented.len(),
        if scene.require_complete_flights {
            scene.flights.len()
        } else {
            scene.nominal_flights.len()
        },
        usize::from(scene.arrival_refusals == 0),
    )
}

fn reciprocal_runway(preferred: &str, airport: &Airport) -> Option<String> {
    let runway = airport
        .runways
        .iter()
        .find(|runway| runway.first_name == preferred || runway.second_name == preferred)?;
    let other = if runway.first_name == preferred {
        runway.second_name.clone()
    } else {
        runway.first_name.clone()
    };
    (!other.is_empty() && other != preferred).then_some(other)
}

fn prepare_direction(input: Preparation) -> Result<Scene, String> {
    let terrain = scenery::Terrain::new(std::path::Path::new(&input.simulator_root))
        .map(std::cell::RefCell::new);
    prepare_direction_with_terrain(input, &terrain, true)
}

fn prepare_direction_with_terrain(
    mut input: Preparation,
    terrain: &Result<std::cell::RefCell<scenery::Terrain>, String>,
    _try_reciprocal: bool,
) -> Result<Scene, String> {
    let requested = [
        Category::Airline,
        Category::Regional,
        Category::Cargo,
        Category::GeneralAviation,
    ]
    .into_iter()
    .zip(input.counts)
    .filter_map(|(category, count)| (count > 0).then_some(category))
    .collect::<BTreeSet<_>>();
    let profiles = input.profiles.clone();
    let score = |result: &Result<Scene, String>| {
        result.as_ref().map_or((0, 0, 0), |scene| {
            complete_mix_score(scene, &profiles, &requested)
        })
    };
    let started = std::time::Instant::now();
    let mut best = prepare_scene(input.clone(), terrain);
    let initial = score(&best);
    if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
        traffic_log(
            openatc_settings::TrafficLog::Summary,
            format!(
                "initial scene attempt finished in {:.1} s; {} released",
                started.elapsed().as_secs_f64(),
                initial.1
            ),
        );
    }
    // The ATIS runway owns the player's airport. A scene that only produced arrivals elsewhere
    // is not enough: the local airport needs departures on the ground. If none were released,
    // try alternate arrival/departure ends at the non-local airport while keeping the player's
    // runway fixed.
    let local_departures = best.as_ref().map_or(0, |scene| {
        scene
            .flights
            .values()
            .filter(|plan| {
                let start = plan.route.start_position();
                openatc_core::arrival::distance_nm(input.own, [start.latitude, start.longitude])
                    <= 5.0
            })
            .count()
    });
    if input.simulator_root.is_empty() || (initial.1 > 0 && local_departures > 0) {
        return best;
    }
    input.refresh_weather()?;
    let weather = |airport: &Airport| {
        input.weather.get(&airport.icao).and_then(|sample| {
            let weather = environment::Environment::from_sample(airport, sample).ok()?;
            Some(
                input
                    .weather_observations
                    .as_ref()
                    .map_or(weather.clone(), |observations| {
                        weather.follow(&airport.icao, observations, &sample.0)
                    }),
            )
        })
    };
    let (Some(departure_weather), Some(arrival_weather)) =
        (weather(&input.origin), weather(&input.destination))
    else {
        return best;
    };
    let local_origin = openatc_core::arrival::distance_nm(
        input.own,
        [
            input.origin.reference_latitude,
            input.origin.reference_longitude,
        ],
    ) <= openatc_core::arrival::distance_nm(
        input.own,
        [
            input.destination.reference_latitude,
            input.destination.reference_longitude,
        ],
    );
    let mut departures = vec![input.runway.clone()];
    let mut arrivals = vec![input.arrival_runway.clone()];
    if local_origin {
        // The player's departure runway is fixed. The destination may use its same-flow
        // parallels or the reciprocal end; only the local airport is visible.
        if let Some(reciprocal) = reciprocal_runway(&input.arrival_runway, &input.destination) {
            arrivals.push(reciprocal);
        }
        for runway in same_flow_runways(&input.destination, &input.arrival_runway, &arrival_weather)
        {
            if !arrivals.contains(&runway) {
                arrivals.push(runway);
            }
        }
    } else {
        // The player's arrival runway stays on the ATIS flow. A same-flow parallel is allowed
        // if the primary runway has no usable exit, but the reciprocal end is never used.
        for runway in same_flow_runways(&input.destination, &input.arrival_runway, &arrival_weather)
        {
            if !arrivals.contains(&runway) {
                arrivals.push(runway);
            }
        }
        if let Some(reciprocal) = reciprocal_runway(&input.runway, &input.origin) {
            departures.push(reciprocal);
        }
        for runway in same_flow_runways(&input.origin, &input.runway, &departure_weather) {
            if !departures.contains(&runway) {
                departures.push(runway);
            }
        }
    }
    departures.dedup();
    arrivals.dedup();
    // The alternates only refine the mix. Four pairs cover both ends of each airport's flow
    // without a full re-preparation per candidate.
    for departure in departures.iter().take(3) {
        for arrival in arrivals.iter().take(3) {
            if !departure_weather.fresh() || !arrival_weather.fresh() {
                return best;
            }
            if departures.first() == Some(departure) && arrivals.first() == Some(arrival) {
                continue;
            }
            let mut alternative = input.clone();
            alternative.force_runway = *departure != input.runway;
            alternative.force_arrival_runway = *arrival != input.arrival_runway;
            alternative.runway.clone_from(departure);
            alternative.arrival_runway.clone_from(arrival);
            let trial_started = std::time::Instant::now();
            let trial = prepare_scene(alternative, terrain);
            if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
                traffic_log(
                    openatc_settings::TrafficLog::Summary,
                    format!(
                        "alternate {departure}/{arrival} attempted in {:.1} s; {} released",
                        trial_started.elapsed().as_secs_f64(),
                        score(&trial).1
                    ),
                );
            }
            if score(&trial) > score(&best) {
                best = trial;
                if let Ok(scene) = &mut best {
                    scene.warnings.push(format!("Generated route uses departure runway {departure} and arrival runway {arrival} to accommodate more of the requested traffic mix"));
                }
                if score(&best).0 == requested.len() {
                    return best;
                }
                continue;
            }
            if let Ok(scene) = &mut best {
                match trial {
                    Ok(trial) => {
                        // Retain later stand failures as well as the first ones, within a fixed diagnostic budget.
                        let tail = trial.warnings.len().saturating_sub(3);
                        scene.warnings.extend(
                            trial
                                .warnings
                                .into_iter()
                                .enumerate()
                                .filter(|(index, _)| *index < 3 || *index >= tail)
                                .map(|(_, reason)| {
                                    format!("Alternate runways {departure}/{arrival}: {reason}")
                                }),
                        );
                    }
                    Err(error) => scene
                        .warnings
                        .push(format!("Alternate runways {departure}/{arrival}: {error}")),
                }
            }
        }
    }
    best
}

fn scene_key(state: &EngineState) -> String {
    // The airport is resolved after the first telemetry sample. Starting from the planned
    // departure keeps that resolution from invalidating the first survey.
    let airport = state
        .airport
        .as_ref()
        .map(|airport| airport.icao.as_str())
        .filter(|icao| !icao.is_empty())
        .unwrap_or(state.session.plan.departure.as_str());
    format!(
        "{}|{}|{}|{}|{}|{}|{:?}|{}|{}",
        state.settings.simulator_root,
        airport,
        state.session.plan.departure,
        state.session.plan.destination,
        state.session.plan.runway,
        state.session.plan.arrival_runway,
        [
            state.settings.airline_traffic,
            state.settings.regional_traffic,
            state.settings.cargo_traffic,
            state.settings.ga_traffic
        ],
        state.session_generation,
        state.generated.scene_revision
    )
}

/// Validate preview/surface jobs against the base scene key, without the tier suffix.
fn preparation_is_current(state: &EngineState, epoch: u64, base_key: &str) -> bool {
    state.session_generation == epoch
        && state.settings.generated_traffic
        && scene_key(state) == base_key
}

/// How far the player may travel from the last surveyed area before its airports no longer
/// cover the scene and a fresh survey is warranted.
fn scene_coverage_nm(state: &EngineState) -> f64 {
    state
        .generated
        .policy
        .as_ref()
        .map_or(50.0, |policy| policy.surface_monitor_radius_nm)
        .max(5.0)
}

/// True when the player has left the area covered by the last survey.
fn travel_outside_coverage(anchor: Option<[f64; 2]>, own: [f64; 2], coverage_nm: f64) -> bool {
    anchor.is_some_and(|anchor| {
        let distance = openatc_core::arrival::distance_nm(anchor, own);
        distance.is_finite() && distance > coverage_nm
    })
}

/// What generated traffic is doing on one runway resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunwayUse {
    Clear,
    LineUp,
    Departing,
    Landing,
}

impl Runtime {
    /// Report whether generated traffic occupies this runway, and how. A landing aircraft takes
    /// priority over a departure, so a following aircraft is told to hold short rather than line up.
    pub(super) fn runway_use(&self, resource: &str) -> RunwayUse {
        let mut line_up = false;
        let mut departing = false;
        for pose in self.world.snapshot() {
            let Some(flight) = self.flights.get(&pose.id) else {
                continue;
            };
            let Some(segment) = flight.route.segment(pose.segment) else {
                continue;
            };
            if !segment.resources.iter().any(|held| held == resource) {
                continue;
            }
            match pose.stage {
                openatc_core::traffic::motion::Stage::Approach
                | openatc_core::traffic::motion::Stage::MissedApproach
                | openatc_core::traffic::motion::Stage::Rollout => return RunwayUse::Landing,
                openatc_core::traffic::motion::Stage::LineUp => line_up = true,
                openatc_core::traffic::motion::Stage::Takeoff => departing = true,
                _ => {}
            }
        }
        if line_up {
            RunwayUse::LineUp
        } else if departing {
            RunwayUse::Departing
        } else {
            RunwayUse::Clear
        }
    }
}

fn planning_observations(telemetry: &openatc_core::state::Telemetry) -> Vec<TrafficTarget> {
    let mut observed = telemetry
        .traffic
        .as_deref()
        .unwrap_or_default()
        .iter()
        .take(126)
        .cloned()
        .collect::<Vec<_>>();
    if telemetry.position_valid {
        let own = TrafficTarget {
            id: 0,
            aircraft: telemetry.aircraft_icao.clone(),
            latitude: telemetry.latitude,
            longitude: telemetry.longitude,
            altitude_feet: telemetry.altitude_feet,
            track_degrees: telemetry
                .ground_track_degrees
                .unwrap_or(telemetry.heading_degrees),
            ground_speed_knots: Some(telemetry.ground_speed_knots),
            wingspan_meters: telemetry.wingspan_meters,
            on_ground: telemetry.on_ground,
            ..Default::default()
        };
        if openatc_core::traffic::advisory::valid(&own) {
            observed.push(own);
        }
    }
    observed
}

enum PreparedJob {
    Survey(Arc<network::Plan>),
    Scene(Box<Scene>),
}

/// Human-facing callsign for transcript text while the tag keeps the compact flight identity.
pub(super) fn display_callsign(target: &TrafficTarget) -> String {
    if target.name.is_empty() {
        return target.callsign.clone();
    }
    let digits = target
        .callsign
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    if digits.is_empty() {
        target.name.clone()
    } else {
        format!("{} {digits}", target.name)
    }
}

pub async fn prepare(shared: Shared) {
    // Traffic is planned traffic. A preview layer without flight plans cannot be cleared,
    // blocks the planned queue and has no radio, so it is not published at all.
    if !shared.read().await.settings.generated_traffic {
        return;
    }
    if let Err(error) = crate::radio::ensure_index(&shared).await {
        eprintln!("Generated traffic index failed: {error}");
        shared.write().await.generated.status = error;
        return;
    }
    let (
        job,
        key,
        base_key,
        epoch,
        seed,
        startup_cast,
        taxi_target,
        population_seed,
        minimum_airborne_spawn_nm,
        reservations,
        settings,
        plan,
        profiles,
        models,
        config,
        workers,
        own,
        observed,
        player_airborne,
        local_only,
        weather,
        weather_observations,
        network,
        regions,
        stations,
    ) = {
        let mut state = shared.write().await;
        if !state.settings.generated_traffic || !state.session.telemetry.position_valid {
            return;
        }
        // Only complete scenes publish. Every aircraft carries a cleared flight plan.
        // On the ground the airport itself is the subject: plan the player's pair first and keep
        // the cast dense there. One following pass expands to the surrounding network and
        // merges its SID/STAR traffic without replacing the local scene.
        let local_only = state.session.telemetry.on_ground && !state.generated.network_pending;
        let mut base_key = scene_key(&state);
        let mut key = base_key.clone();
        let own_now = [
            state.session.telemetry.latitude,
            state.session.telemetry.longitude,
        ];
        let travelled =
            travel_outside_coverage(state.generated.anchor, own_now, scene_coverage_nm(&state));
        population::maintain(&mut state);
        if state.generated.population.taxi_target == 0 {
            let minimum = usize::from(state.settings.traffic_taxi_min);
            let maximum = usize::from(
                state
                    .settings
                    .traffic_taxi_max
                    .max(state.settings.traffic_taxi_min),
            );
            let span = (maximum.saturating_sub(minimum) + 1).max(1) as u64;
            state.generated.population.taxi_target = (minimum
                + (seeded_number(state.traffic_seed ^ 0x7A11_1A6E, 0x51) % span) as usize)
                .min(63);
        }
        if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
            traffic_log(
                openatc_settings::TrafficLog::Summary,
                format!(
                    "prepare sees on_ground {} -> player_airborne {}",
                    state.session.telemetry.on_ground, !state.session.telemetry.on_ground
                ),
            );
        }
        if travelled {
            state.generated.scene_revision = state.generated.scene_revision.wrapping_add(1);
            state.generated.network = None;
            state.generated.anchor = Some(own_now);
            state.generated.population.refill_blocked_until = None;
            base_key = scene_key(&state);
            key.clone_from(&base_key);
        }
        let replenishing = state.generated.key == key && population::needs_refill(&state.generated);
        if state.generated.preparation_job.is_some()
            || state.generated.simulation_job.is_some()
            || state.generated.turnaround_job.is_some()
            || state.generated.turnaround_pending.is_some()
            || (state.generated.key == key && !replenishing && !state.generated.network_pending)
            || state
                .generated
                .retry
                .is_some_and(|time| time.elapsed() < Duration::from_secs(10))
            || state
                .generated
                .population
                .refill_blocked_until
                .is_some_and(|until| Instant::now() < until)
        {
            return;
        }
        for (airport, sample) in &state.sim_weather {
            state.generated.observe_weather(airport, sample);
        }
        let network = state
            .generated
            .network
            .clone()
            .filter(|plan| plan.key == key);
        state.generated.status = if network.is_some() {
            if replenishing {
                "Replenishing generated traffic from installed airport data".into()
            } else {
                "Preparing controlled traffic routes from installed airport data".into()
            }
        } else {
            "Selecting traffic airports and compatible fleets".into()
        };
        let job = next_job();
        population::reservations(&mut state.generated);
        state.generated.preparation_job = Some(job);
        state.generated.population.revision = state.generated.population.revision.wrapping_add(1);
        state.generated.retry = Some(Instant::now());
        traffic_log(
            openatc_settings::TrafficLog::Summary,
            format!(
                "preparation job started ({})",
                if network.is_some() { "scene" } else { "survey" }
            ),
        );
        if let Ok(mut started) = preparation_started().lock() {
            *started = Some(Instant::now());
        }
        (
            job,
            key,
            base_key,
            state.session_generation,
            state.traffic_seed,
            !state.generated.population.started,
            state.generated.population.taxi_target,
            state.generated.population.revision,
            if state.generated.population.started {
                state
                    .generated
                    .policy
                    .as_ref()
                    .map_or(20.0, |policy| policy.display_enter_nm / 2.0)
            } else {
                0.0
            },
            (
                (*state.generated.occupied_stand_cells).clone(),
                (*state.generated.surface_corridors).clone(),
                state.generated.reserved_stands.clone(),
            ),
            state.settings.clone(),
            state.session.plan.clone(),
            state.traffic_profiles.clone(),
            state.model_installer.clone(),
            state.config_dir.clone(),
            state.workers.clone(),
            [
                state.session.telemetry.latitude,
                state.session.telemetry.longitude,
            ],
            {
                let mut observed = planning_observations(&state.session.telemetry);
                for frame in &state.generated.frames {
                    observed.retain(|target| target.id != frame.target.id);
                    observed.push(frame.target.clone());
                }
                observed
            },
            !state.session.telemetry.on_ground,
            local_only,
            [
                &state.session.plan.departure,
                &state.session.plan.destination,
            ]
            .into_iter()
            .filter_map(|id| {
                state
                    .sim_weather
                    .get(id)
                    .map(|sample| (id.clone(), sample.clone()))
            })
            .collect(),
            state.generated.weather_observations.clone(),
            network,
            state.regions.clone(),
            Arc::<[openatc_core::stations::Station]>::from(state.stations.clone()),
        )
    };
    let survey_key = key.clone();
    traffic_log(
        openatc_settings::TrafficLog::Summary,
        format!(
            "traffic seed {seed:#018x}; {} background CPU workers",
            workers.count()
        ),
    );
    let result = async {
        let profiles = profiles.get(&config, &workers, false).await?;
        let models = models.models(&settings.simulator_root, &workers).await?;
        let counts = [
            settings.airline_traffic,
            settings.regional_traffic,
            settings.cargo_traffic,
            settings.ga_traffic,
        ]
        .map(|count| count);
        workers
            .run(move || {
                let (_, policy, _) = policy::load(&config)?;
                let (origin, destination, runway, arrival_runway) = network::endpoints(
                    &settings.simulator_root,
                    &plan,
                    &stations,
                    own,
                    &policy,
                    player_airborne,
                )?;
                let airport_codes = airport_code_map(&settings.simulator_root, &profiles);
                let input = Preparation {
                    stations,
                    regions,
                    simulator_root: settings.simulator_root.clone(),
                    origin,
                    destination,
                    profiles,
                    models,
                    counts,
                    own,
                    observed,
                    epoch: epoch.wrapping_add(population_seed),
                    seed,
                    startup_cast,
                    taxi_target,
                    force_runway: false,
                    force_arrival_runway: false,
                    runway,
                    arrival_runway,
                    config,
                    weather,
                    weather_observations: Some(weather_observations),
                    reserved_stand_cells: reservations.0,
                    reserved_surface_corridors: reservations.1,
                    reserved_stands: reservations
                        .2
                        .into_iter()
                        .chain((!plan.arrival_stand.is_empty()).then(|| plan.arrival_stand.clone()))
                        .collect(),
                    // Keep the airport alive when the route catalogue cannot release a
                    // complete city pair. The local pattern adds departures and a circuit
                    // arrival only when the scene has fewer than two released flights.
                    local_pattern_traffic: true,
                    seed_single_arrival: settings.start_first_flight_on_approach,
                    player_airborne,
                    minimum_airborne_spawn_nm,
                    airport_codes,
                };
                match network {
                    None => network::survey(&input, survey_key)
                        .map(|plan| PreparedJob::Survey(Arc::new(plan))),
                    Some(_) if input.counts.iter().all(|count| *count == 0) => {
                        prepare_bidirectional(input)
                            .map(|scene| PreparedJob::Scene(Box::new(scene)))
                    }
                    Some(plan) => network::prepare(input, &plan, local_only)
                        .map(|scene| PreparedJob::Scene(Box::new(scene))),
                }
            })
            .await?
    }
    .await;
    let mut state = shared.write().await;
    if !finish_job(&mut state.generated.preparation_job, job) {
        return;
    }
    if !preparation_is_current(&state, epoch, &base_key) {
        return;
    }
    match result {
        Ok(PreparedJob::Survey(plan)) => {
            state.generated.network = Some(plan);
            state.generated.anchor = Some(own);
            state.generated.status =
                "Traffic airports selected; waiting for simulator airport weather".into();
            traffic_log(
                openatc_settings::TrafficLog::Summary,
                format!("{}", state.generated.status),
            );
        }
        Ok(PreparedJob::Scene(scene)) if scene.weather_fresh() => {
            if let Ok(mut started) = preparation_started().lock()
                && let Some(started) = started.take()
            {
                traffic_log(
                    openatc_settings::TrafficLog::Summary,
                    format!(
                        "scene prepared in {:.1} s; {} complete flights, {} released, {} warnings; scenery cache {} hits / {} misses; {}",
                        started.elapsed().as_secs_f64(),
                        scene.nominal_flights.len(),
                        scene.flights.len(),
                        scene.warnings.len(),
                        scenery::cache::progress().hits,
                        scenery::cache::progress().misses,
                        scene
                            .warnings
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" | ")
                    ),
                );
            }
            if state.generated.epoch == Some(epoch) && !state.generated.frames.is_empty() {
                state.generated.queued_scene = Some((Arc::new(*scene), key, epoch));
                population::publish_pending(&mut state);
            } else {
                publish_scene(&mut state.generated, *scene, key, epoch);
            }
            state.generated.network_pending = local_only;
            traffic_log(
                openatc_settings::TrafficLog::Summary,
                format!("{}", state.generated.status),
            );
        }
        Ok(PreparedJob::Scene(_)) => {
            state.generated.status =
                "Simulator weather changed before traffic publication; preparation will retry"
                    .into();
            traffic_log(
                openatc_settings::TrafficLog::Summary,
                format!("{}", state.generated.status),
            );
        }
        Err(error) => {
            state.generated.status.clone_from(&error);
            eprintln!("Generated traffic preparation failed: {error}");
        }
    }
}

fn frames_overlap(first: &GeneratedTraffic, second: &GeneratedTraffic) -> bool {
    let latitude = (first.target.latitude - second.target.latitude).abs() * 111_320.0;
    let longitude = (first.target.longitude - second.target.longitude).abs()
        * 111_320.0
        * first.target.latitude.to_radians().cos().max(0.01);
    latitude.hypot(longitude) <= APRON_MATCH_METERS
}

/// Keep the apron visual for stands that the production scene only fills with another parked
/// aircraft. Released flights still take their stand so their movement and controller identity
/// remain visible.
fn merge_apron_frames(
    scene_frames: Vec<GeneratedTraffic>,
    released: &BTreeSet<u32>,
    apron: Vec<GeneratedTraffic>,
) -> Vec<GeneratedTraffic> {
    let mut frames = Vec::with_capacity(scene_frames.len() + apron.len());
    for frame in scene_frames {
        // The local Tower layer is generated independently of the released network flights and
        // must not be removed just because a parked apron aircraft happens to sit underneath it.
        if !ctaf::is_local(frame.target.id)
            && !released.contains(&frame.target.id)
            && apron
                .iter()
                .any(|apron_frame| frames_overlap(apron_frame, &frame))
        {
            continue;
        }
        frames.push(frame);
    }
    for frame in apron {
        if !frames
            .iter()
            .any(|existing| frames_overlap(existing, &frame))
        {
            frames.push(frame);
        }
    }
    frames
}

fn replaces_airport(runtime: &Runtime, airport: &Airport) -> bool {
    !airport.icao.is_empty()
        && runtime
            .airport
            .as_ref()
            .is_some_and(|current| !current.icao.is_empty() && current.icao != airport.icao)
}

fn publish_scene(runtime: &mut Runtime, mut scene: Scene, key: String, epoch: u64) {
    // Never replace a live scene with fewer planned aircraft. The population and replenishment
    // passes add traffic; publishing a smaller scene over a larger one deletes aircraft the
    // player is already watching, including the one that was about to ask for departure.
    // A scene for another airport is different: the old traffic belongs to the departure airport
    // and must not stay visible after the player has landed elsewhere.
    let replacing_airport = replaces_airport(runtime, &scene.airport);
    if !replacing_airport
        && !runtime.flights.is_empty()
        && scene.flights.len() < runtime.flights.len()
    {
        runtime.status =
            "Smaller replacement scene skipped; the live traffic owns the world".into();
        return;
    }

    let previous = runtime.clone();
    let live_apron: BTreeSet<_> = if replacing_airport {
        BTreeSet::new()
    } else {
        previous
            .apron_routes
            .keys()
            .copied()
            .filter(|id| previous.world.current_segment(*id).is_some())
            .collect()
    };
    let mut deferred = BTreeSet::new();
    for id in &live_apron {
        for resource in previous.world.owned_resources(*id) {
            if let Some(owner) = scene.world.resource_owner(&resource) {
                deferred.insert(owner);
            }
        }
        if let Some(apron) = previous.frames.iter().find(|frame| frame.target.id == *id) {
            deferred.extend(
                scene
                    .frames
                    .iter()
                    .filter(|frame| frames_overlap(frame, apron))
                    .map(|frame| frame.target.id),
            );
        }
    }
    let available = MAXIMUM_AIRCRAFT.saturating_sub(live_apron.len());
    let candidates: Vec<_> = scene
        .world
        .snapshot()
        .into_iter()
        .map(|pose| pose.id)
        .filter(|id| !deferred.contains(id))
        .collect();
    deferred.extend(candidates.into_iter().skip(available));
    for id in &deferred {
        scene.world.remove(*id);
    }
    scene
        .frames
        .retain(|frame| !deferred.contains(&frame.target.id));
    scene.flights.retain(|id, _| !deferred.contains(id));
    for id in &live_apron {
        if !scene.world.preserve_surface(&previous.world, *id) {
            runtime.status =
                "Traffic publication deferred while taxi reservations remain active".into();
            return;
        }
    }
    if !scene.warnings.is_empty() {
        let corridor = scene
            .warnings
            .iter()
            .filter(|warning| warning.contains("corridor release"))
            .take(6)
            .cloned()
            .collect::<Vec<_>>();
        let sample = if corridor.is_empty() {
            scene.warnings.iter().take(8).cloned().collect::<Vec<_>>()
        } else {
            corridor
        }
        .join(" | ");
        eprintln!("Generated traffic warnings: {sample}");
    }
    let published_ids = runtime.published_ids.clone();
    let weather_observations = runtime.weather_observations.clone();
    let network = runtime.network.clone();
    let anchor = runtime.anchor;
    let scene_revision = runtime.scene_revision;
    let apron_key = if replacing_airport {
        None
    } else {
        runtime.apron_key.clone()
    };
    let apron_ids = if replacing_airport {
        BTreeSet::new()
    } else {
        runtime.apron_ids.clone()
    };
    let apron = runtime
        .frames
        .iter()
        .filter(|frame| apron_ids.contains(&frame.target.id))
        .cloned()
        .collect::<Vec<_>>();
    *runtime = Runtime::default();
    runtime.published_ids = published_ids;
    runtime.population = previous.population.clone();
    runtime.population.started |= !scene.flights.is_empty();
    runtime.weather_observations = weather_observations;
    runtime.network = network;
    runtime.anchor = anchor;
    runtime.scene_revision = scene_revision;
    runtime.apron_key = apron_key;
    runtime.apron_ids = apron_ids;
    runtime.status = format!(
        "{} generated surface aircraft prepared; {} terminal candidates, {} aircraft-specific terminal inputs, {} joined arrivals, {} complete nominal flights, {} released flights; rejected corridors are listed in warnings",
        scene.frames.len(),
        scene.terminal_routes.len(),
        scene.decoded_terminals.len(),
        scene.nominal_arrivals.len(),
        scene.nominal_flights.len(),
        scene.flights.len()
    );
    runtime.terminal_routes = scene.terminal_routes;
    runtime.environments = scene.environments;
    runtime.key = key;
    let released = scene.flights.keys().copied().collect::<BTreeSet<_>>();
    // Surface previews keep their parked actors. A complete scene publishes only released
    // flights and independently prepared local traffic, with matching movement ownership.
    if scene.require_complete_flights {
        scene.frames.retain(|frame| {
            released.contains(&frame.target.id) || ctaf::is_generated(frame.target.id)
        });
    }
    runtime.frames = merge_apron_frames(scene.frames, &released, apron);
    let published = runtime
        .frames
        .iter()
        .map(|frame| frame.target.id)
        .collect::<BTreeSet<_>>();
    for pose in scene.world.snapshot() {
        if !published.contains(&pose.id) {
            scene.world.remove(pose.id);
        }
    }
    scene.movement.retain(|id, _| published.contains(id));
    scene.orders.retain(|id, _| published.contains(id));
    Arc::make_mut(&mut scene.turnaround_inputs).retain(|id, _| published.contains(id));
    scene.seeded_stations.retain(|id, _| published.contains(id));
    scene.advisories.retain(|id, _| published.contains(id));
    scene.fixed_gear.retain(|id| published.contains(id));
    scene.flights.retain(|id, _| published.contains(id));
    runtime.seeded_stations = scene.seeded_stations;
    runtime.advisories = scene.advisories;
    runtime.local_spares = scene.local_spares;
    for (id, station) in &runtime.seeded_stations {
        runtime.active_flights.insert(*id);
        runtime.controllers.insert(*id, station.clone());
        runtime.handoff_targets.insert(*id, 0);
        if let Some(frame) = runtime
            .frames
            .iter_mut()
            .find(|frame| frame.target.id == *id)
        {
            frame.target.com1_khz = Some(station.khz);
        }
    }
    runtime.visible.clear();
    // A simulation job started before this scene would finish with a pre-scene world and
    // overwrite these aircraft. Drop it; the next job starts from the published world.
    runtime.simulation_job = None;
    runtime.world = scene.world;
    for id in runtime.seeded_stations.keys().copied().collect::<Vec<_>>() {
        if let Some(pose) = runtime
            .world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == id)
        {
            runtime.handoff_targets.insert(id, pose.segment);
        }
    }
    runtime.movement = scene.movement;
    runtime.require_complete_flights = scene.require_complete_flights;
    runtime.airports = scene.airports;
    runtime.airport = Some(Arc::new(scene.airport));
    runtime.destination = Some(Arc::new(scene.destination));
    runtime.departure_procedures = scene.departure_procedures;
    runtime.arrival_procedures = scene.arrival_procedures;
    runtime.terminal_points = scene.terminal_points;
    runtime.published_holds = scene.published_holds;
    runtime.nominal_departures = scene.nominal_departures;
    runtime.decoded_terminals = scene.decoded_terminals;
    runtime.nominal_arrivals = scene.nominal_arrivals;
    runtime.nominal_flights = scene.nominal_flights;
    runtime.flights = scene.flights;
    // A new scene must not inherit the previous scene's active-flight ids. Otherwise the
    // controller skips the new aircraft and they sit at the gate or pushback forever.
    runtime.active_flights.clear();
    runtime.approved_orders.clear();
    runtime
        .active_flights
        .extend(runtime.flights.keys().copied());
    runtime
        .active_flights
        .extend(runtime.apron_ids.iter().copied());
    let previous_ids: BTreeSet<u32> = previous
        .frames
        .iter()
        .map(|frame| frame.target.id)
        .chain(previous.active_flights.iter().copied())
        .chain(previous.approved_orders.iter().map(|(id, _)| *id))
        .collect();
    runtime.seeded = runtime
        .frames
        .iter()
        .map(|frame| frame.target.id)
        .filter(|id| {
            !runtime.apron_ids.contains(id)
                && (!previous_ids.contains(id) || ctaf::is_local(*id) || ctaf::is_generated(*id))
        })
        .collect();
    let published_ids: Vec<u32> = runtime.frames.iter().map(|frame| frame.target.id).collect();
    for id in published_ids {
        runtime.world.clear_authority(id);
    }
    // Startup dressing is departure traffic only. A TaxiIn aircraft exists only after a
    // landing, so remove any arrival that the scene seeded already taxiing to a stand.
    let seeded_taxi_in: BTreeSet<u32> = runtime
        .world
        .snapshot()
        .into_iter()
        .filter(|pose| {
            if pose.stage == openatc_core::traffic::motion::Stage::TaxiIn {
                return true;
            }
            // Never start a ground aircraft on the runway. A taxi-out hold seed can land on
            // the runway boundary depending on the route split.
            pose.stage.on_ground()
                && !runtime.apron_ids.contains(&pose.id)
                && runtime.airport.as_deref().is_some_and(|airport| {
                    let point = openatc_core::airport::airport_point(
                        airport,
                        pose.position.latitude,
                        pose.position.longitude,
                    );
                    openatc_core::airport::within_runway(airport, &point)
                })
        })
        .map(|pose| pose.id)
        .collect();
    for id in seeded_taxi_in {
        runtime.world.remove(id);
        runtime.frames.retain(|frame| frame.target.id != id);
        runtime.flights.remove(&id);
        runtime.orders.remove(&id);
        runtime.movement.remove(&id);
        Arc::make_mut(&mut runtime.turnaround_inputs).remove(&id);
        runtime.seeded_stations.remove(&id);
        runtime.advisories.remove(&id);
        runtime.fixed_gear.remove(&id);
        runtime
            .approved_orders
            .retain(|(aircraft, _)| *aircraft != id);
    }
    // A generated flight that begins the scene already taxiing has, by definition, already
    // received its taxi clearance. Approve its surface route now; runway crossings, line-up
    // and takeoff still require their own grants.
    runtime.spawned_taxiing.clear();
    for pose in runtime.world.snapshot() {
        if pose.id != 0
            && matches!(
                pose.stage,
                openatc_core::traffic::motion::Stage::TaxiOut
                    | openatc_core::traffic::motion::Stage::TaxiIn
            )
        {
            runtime.spawned_taxiing.insert(pose.id);
        }
    }
    for id in &runtime.spawned_taxiing {
        let Some(flight) = runtime.flights.get(id) else {
            continue;
        };
        for index in 0..flight.route.segment_count() {
            let Some(segment) = flight.route.segment(index) else {
                break;
            };
            if matches!(
                segment.stage,
                openatc_core::traffic::motion::Stage::TaxiOut
                    | openatc_core::traffic::motion::Stage::TaxiIn
            ) {
                runtime.approved_orders.insert((*id, index));
            }
        }
    }
    // Spread the initial taxiing cast between its stands and the active runway. Without this,
    // every ground flight starts at the same route end and the airport looks like a car park.
    let mut spawn_ids: Vec<u32> = runtime
        .seeded
        .iter()
        .copied()
        .filter(|id| !runtime.apron_ids.contains(id))
        .collect();
    spawn_ids.sort_unstable();
    let spawn_count = spawn_ids.len();
    for (position, id) in spawn_ids.into_iter().enumerate() {
        let Some(surface) = runtime.flights.get(&id).map(|flight| {
            (0..flight.route.segment_count())
                .filter(|index| {
                    flight.route.segment(*index).is_some_and(|segment| {
                        matches!(
                            segment.stage,
                            openatc_core::traffic::motion::Stage::TaxiOut
                                | openatc_core::traffic::motion::Stage::TaxiIn
                        )
                    })
                })
                .collect::<Vec<_>>()
        }) else {
            continue;
        };
        if surface.is_empty() {
            continue;
        }
        let fraction = (position + 1) as f64 / (spawn_count + 1) as f64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let index = ((fraction * surface.len() as f64) as usize).min(surface.len() - 1);
        runtime.world.place_on_segment(id, surface[index]);
    }
    let poses = runtime.world.snapshot();
    for frame in &mut runtime.frames {
        if let Some(pose) = poses.iter().find(|pose| pose.id == frame.target.id) {
            frame.target.latitude = pose.position.latitude;
            frame.target.longitude = pose.position.longitude;
        }
    }
    let roster = runtime
        .world
        .snapshot()
        .iter()
        .map(|pose| {
            format!(
                "{}:{:?}{}{}",
                pose.id,
                pose.stage,
                if runtime.flights.contains_key(&pose.id) {
                    "*"
                } else {
                    ""
                },
                if runtime.apron_ids.contains(&pose.id) {
                    "(apron)"
                } else {
                    ""
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    runtime.log(
        openatc_settings::TrafficLog::Summary,
        format!(
            "published scene has {} flights, {} poses, {} frames; {roster}",
            runtime.flights.len(),
            runtime.world.snapshot().len(),
            runtime.frames.len()
        ),
    );
    runtime.spoken_callsigns = scene.spoken_callsigns;
    runtime.policy = Some(scene.policy);
    runtime.fixed_gear = scene.fixed_gear;
    runtime.channels = scene.channels;
    runtime.orders = scene.orders;
    runtime.turnaround_inputs = scene.turnaround_inputs;
    runtime.stations = scene.stations;
    runtime.regions = scene.regions;
    runtime.mora = scene.mora;
    runtime.simulator_root = scene.simulator_root;
    runtime.reserved_stands = scene.reserved_stands;
    runtime.occupied_stand_cells = scene.occupied_stand_cells;
    runtime.surface_corridors = scene.surface_corridors;
    runtime.turnarounds_attempted = BTreeSet::new();
    runtime.turnaround_job = None;
    runtime.turnaround_pending = None;
    runtime.turnaround_target = None;
    runtime.warnings = scene.warnings;
    runtime.previous_time = None;
    runtime.epoch = Some(epoch);
    runtime.simulation_job = None;
    for id in live_apron {
        if let Some(route) = previous.apron_routes.get(&id) {
            runtime.apron_routes.insert(id, route.clone());
        }
        if let Some(count) = previous.apron_segments.get(&id) {
            runtime.apron_segments.insert(id, *count);
        }
        if let Some(movement) = previous.movement.get(&id) {
            runtime.movement.insert(id, movement.clone());
        }
        if let Some(orders) = previous.orders.get(&id) {
            runtime.orders.insert(id, orders.clone());
        }
        if let Some(controller) = previous.controllers.get(&id) {
            runtime.controllers.insert(id, controller.clone());
        }
        if previous.active_flights.contains(&id) {
            runtime.active_flights.insert(id);
        }
        if previous.apron_radio.contains(&id) {
            runtime.apron_radio.insert(id);
        }
        if previous.tower_ready.contains(&id) {
            runtime.tower_ready.insert(id);
        }
        if previous.tower_notified.contains(&id) {
            runtime.tower_notified.insert(id);
        }
        runtime.approved_orders.extend(
            previous
                .approved_orders
                .iter()
                .filter(|(aircraft, _)| *aircraft == id)
                .copied(),
        );
        runtime.reported_crossings.extend(
            previous
                .reported_crossings
                .iter()
                .filter(|(aircraft, _)| *aircraft == id)
                .copied(),
        );
    }
    if !runtime.apron_routes.is_empty() {
        runtime.previous_time = previous.previous_time;
        runtime.exchange = previous
            .exchange
            .filter(|exchange| runtime.apron_routes.contains_key(&exchange.id));
    }
    runtime
        .apron_ids
        .retain(|id| runtime.frames.iter().any(|frame| frame.target.id == *id));
}

fn external_surface_cells(
    runtime: &Runtime,
    telemetry: &openatc_core::state::Telemetry,
) -> BTreeSet<String> {
    let Some(policy) = &runtime.policy else {
        return BTreeSet::new();
    };
    let mut blocked = BTreeSet::new();
    let mut add = |target: &TrafficTarget| {
        for airport in runtime.operational_airports() {
            if openatc_core::arrival::distance_nm(
                [airport.reference_latitude, airport.reference_longitude],
                [target.latitude, target.longitude],
            ) > policy.surface_monitor_radius_nm
            {
                continue;
            }
            let wingspan = target
                .wingspan_meters
                .or_else(|| {
                    openatc_core::traffic::profiles::aircraft(&target.aircraft)
                        .map(|profile| profile.wingspan)
                })
                .unwrap_or(36.0);
            blocked.extend(cells(
                airport,
                openatc_core::airport::airport_point(airport, target.latitude, target.longitude),
                wingspan / 2.0,
                policy.conflict_cell_meters,
            ));
            // Runway phases reserve the runway itself, not only its taxi conflict cells.
            for runway in &airport.runways {
                if !target.on_ground
                    && separation::parallel_other_runway(
                        airport,
                        &runway.first_name,
                        target.latitude,
                        target.longitude,
                        target.track_degrees,
                    )
                {
                    continue;
                }
                if openatc_core::holding::blocked_targets(
                    airport,
                    &runway.first_name,
                    std::iter::once(target),
                )
                .is_some()
                {
                    blocked.insert(format!(
                        "RUNWAY/{}/{}/{}",
                        airport.icao, runway.first_name, runway.second_name
                    ));
                }
            }
        }
    };
    if telemetry.on_ground {
        add(&TrafficTarget {
            id: 0,
            aircraft: telemetry.aircraft_icao.clone(),
            latitude: telemetry.latitude,
            longitude: telemetry.longitude,
            altitude_feet: telemetry.altitude_feet,
            track_degrees: telemetry.heading_degrees,
            on_ground: true,
            wingspan_meters: telemetry.wingspan_meters,
            ..Default::default()
        });
    }
    if let Some(observed) = &telemetry.traffic {
        for target in observed
            .iter()
            .filter(|target| target.on_ground && !runtime.movement.contains_key(&target.id))
        {
            add(target);
        }
    }
    blocked
}

#[allow(dead_code)]
fn observed_targets(
    runtime: &Runtime,
    telemetry: &openatc_core::state::Telemetry,
) -> Vec<TrafficTarget> {
    let mut targets = external_targets(runtime, telemetry);
    targets.extend(runtime.frames.iter().map(|frame| frame.target.clone()));
    targets
}

/// Traffic the controller did not generate itself: the simulator's observations and the player.
///
/// Generated aircraft are not included because their conflicts are resolved by the movement
/// world's resource ownership. Treating a queued generated aircraft as observed runway occupancy
/// would let a departure hold itself behind the queue it is leading.
fn external_targets(
    runtime: &Runtime,
    telemetry: &openatc_core::state::Telemetry,
) -> Vec<TrafficTarget> {
    let ids: BTreeSet<_> = runtime.frames.iter().map(|frame| frame.target.id).collect();
    let mut targets: Vec<_> = telemetry
        .traffic
        .as_ref()
        .into_iter()
        .flatten()
        .take(63)
        .filter(|target| !ids.contains(&target.id))
        .cloned()
        .collect();
    targets.push(TrafficTarget {
        id: 0,
        aircraft: telemetry.aircraft_icao.clone(),
        latitude: telemetry.latitude,
        longitude: telemetry.longitude,
        altitude_feet: telemetry.altitude_feet,
        track_degrees: telemetry.heading_degrees,
        on_ground: telemetry.on_ground,
        ground_speed_knots: Some(telemetry.ground_speed_knots),
        wingspan_meters: telemetry.wingspan_meters,
        ..Default::default()
    });
    targets
}

fn session_phase(
    stage: openatc_core::traffic::motion::Stage,
) -> Option<crate::traffic_runtime::Phase> {
    use crate::traffic_runtime::Phase;
    use openatc_core::traffic::motion::Stage;
    Some(match stage {
        Stage::Parked => Phase::AtGate,
        Stage::Pushback => Phase::Pushback,
        Stage::TaxiOut => Phase::Taxi,
        Stage::Crossing => Phase::Crossing,
        Stage::LineUp => Phase::LineUp,
        Stage::Takeoff => Phase::Takeoff,
        Stage::Climb | Stage::Cruise | Stage::Descent | Stage::Holding => Phase::Airborne,
        Stage::Approach | Stage::MissedApproach => Phase::Approach,
        Stage::Rollout => Phase::Landing,
        Stage::TaxiIn => Phase::TaxiIn,
        Stage::Turnaround => Phase::AtGateIn,
    })
}

fn update_sessions(runtime: &mut Runtime, seconds: f64) {
    let poses = runtime.world.snapshot();
    let ids: BTreeSet<u32> = runtime
        .frames
        .iter()
        .filter(|frame| poses.iter().any(|pose| pose.id == frame.target.id))
        .map(|frame| frame.target.id)
        .collect();
    runtime.sessions.retain(&ids);
    runtime.session_positions.retain(|id, _| ids.contains(id));
    for frame in &runtime.frames {
        let Some(pose) = poses.iter().find(|pose| pose.id == frame.target.id) else {
            continue;
        };
        let Some(phase) = session_phase(pose.stage) else {
            continue;
        };
        let frequency = frame.target.com1_khz.unwrap_or(0);
        if !runtime.sessions.contains(pose.id) {
            runtime.sessions.add(pose.id, frequency, phase, seconds);
        } else {
            runtime.sessions.phase(pose.id, phase, seconds);
            // A speed above zero is not progress: a stopped crossing can still vibrate or
            // creep on the spot. Only a new segment or a real position change resets health.
            let moved =
                runtime
                    .session_positions
                    .get(&pose.id)
                    .is_none_or(|(latitude, longitude)| {
                        openatc_core::arrival::distance_nm(
                            [*latitude, *longitude],
                            [pose.position.latitude, pose.position.longitude],
                        ) * 1852.0
                            > 1.0
                    });
            if runtime.session_segments.get(&pose.id) != Some(&pose.segment) || moved {
                runtime.sessions.progress(pose.id, seconds);
            }
        }
        runtime.session_segments.insert(pose.id, pose.segment);
        runtime
            .session_positions
            .insert(pose.id, (pose.position.latitude, pose.position.longitude));
    }
    // The selector runtime still owns movement grants. Retiring a session here removed a
    // legitimate flight that the old crossing path later released, so health actions are
    // logged only until the frequency runtime owns the grant.
    let actions = runtime.health.check(&mut runtime.sessions, seconds);
    for action in actions {
        match action {
            crate::traffic_runtime::HealthAction::Retrigger(id) => runtime.log(
                openatc_settings::TrafficLog::Summary,
                format!("traffic health: session {id} needs a phase retrigger"),
            ),
            crate::traffic_runtime::HealthAction::Release(id) => {
                runtime.world.release_movement(id);
                runtime
                    .approved_orders
                    .retain(|(aircraft, _)| *aircraft != id);
                runtime.log(
                    openatc_settings::TrafficLog::Summary,
                    format!("traffic health: released movement authority for stalled session {id}"),
                );
            }
            crate::traffic_runtime::HealthAction::Retire(id) => runtime.log(
                openatc_settings::TrafficLog::Summary,
                format!("traffic health: session {id} is beyond recovery"),
            ),
        }
    }
}

/// True when the player's powered radio is tuned to the aircraft's controller frequency.
fn generated_on_player_frequency(
    runtime: &Runtime,
    telemetry: &openatc_core::state::Telemetry,
    id: u32,
) -> bool {
    let Some(frame) = runtime.frames.iter().find(|frame| frame.target.id == id) else {
        return false;
    };
    let Some(frequency) = frame.target.com1_khz else {
        return false;
    };
    telemetry.radio_power
        && (telemetry.com1_khz == frequency || telemetry.com2_khz == Some(frequency))
}

fn advance(runtime: &mut Runtime, telemetry: &openatc_core::state::Telemetry) -> bool {
    let mut clock_changed = false;
    if let (Some(_airport), Some(seconds), Some(_policy)) = (
        &runtime.airport,
        telemetry.simulation_seconds,
        &runtime.policy,
    ) {
        withhold_conflicting_departures(runtime, telemetry);
        withhold_conflicting_landings(runtime, telemetry);
        let blocked = external_surface_cells(runtime, telemetry);
        runtime.world.recheck_surface(&blocked);
        // Authorisation follows the aircraft's own clearance. The simulator's TCAS feed only
        // supplies additional observed conflicts; its absence says nothing about the generated
        // flight, and generated conflicts are owned by the movement world.
        let targets = external_targets(runtime, telemetry);
        let generation = runtime.world.generation();
        // A pose without a frame, flight plan or apron assignment has no controller or
        // lifecycle. It must not retain runway or taxi resources forever.
        let controlled: BTreeSet<u32> = runtime
            .frames
            .iter()
            .map(|frame| frame.target.id)
            .chain(runtime.flights.keys().copied())
            .chain(runtime.apron_ids.iter().copied())
            .chain(runtime.active_flights.iter().copied())
            .collect();
        let orphans: Vec<u32> = runtime
            .world
            .snapshot()
            .into_iter()
            .map(|pose| pose.id)
            .filter(|id| !controlled.contains(id))
            .collect();
        for id in orphans {
            runtime.world.remove(id);
            runtime.log(
                openatc_settings::TrafficLog::Summary,
                format!("removed orphan generated pose {id} with no frame, flight plan or station assignment"),
            );
        }
        // Advisory traffic at an untowered field is flown by the published procedure, so it
        // never enters the controlled clearance path below.
        ctaf::advance(runtime, &blocked);
        for pose in runtime.world.snapshot() {
            // Explain every stopped taxiing aircraft once: its assigned route, the plan's next
            // boundary and the clearance state are what decide whether it can ask for departure.
            if pose.stage == openatc_core::traffic::motion::Stage::TaxiOut && pose.knots < 1.0 {
                if runtime.detail_logged.len() > 512 {
                    runtime.detail_logged.clear();
                }
                if runtime
                    .detail_logged
                    .insert((pose.id, pose.segment, pose.complete))
                {
                    let assigned = runtime.world.assigned_route(pose.id);
                    let (segments, end) =
                        assigned.as_ref().map_or((0, "none".to_owned()), |route| {
                            let end = route.end_position();
                            (
                                route.segment_count(),
                                format!("{:.6}/{:.6}", end.latitude, end.longitude),
                            )
                        });
                    let next = runtime
                        .flights
                        .get(&pose.id)
                        .and_then(|plan| plan.route.segment(pose.segment + 1))
                        .map_or_else(
                            || "none".to_owned(),
                            |segment| format!("{:?}", segment.stage),
                        );
                    runtime.log(
                        openatc_settings::TrafficLog::Detail,
                        format!(
                            "hold detail {} seg {}/{} complete {} assigned-end {} plan-next {} active {} tower-ready {} approved {} awaiting {} knots {:.1}",
                            pose.id,
                            pose.segment,
                            segments,
                            pose.complete,
                            end,
                            next,
                            runtime.active_flights.contains(&pose.id),
                            runtime.tower_ready.contains(&pose.id),
                            runtime.approved_orders.contains(&(pose.id, pose.segment)),
                            pose.awaiting_clearance,
                            pose.knots,
                        ),
                    );
                }
            }
            if runtime.apron_ids.contains(&pose.id)
                && !runtime.flights.contains_key(&pose.id)
                && !runtime.active_flights.contains(&pose.id)
            {
                continue;
            }
            if runtime.world.is_advisory(pose.id) {
                continue;
            }
            if pose.recovering {
                continue;
            }
            // A queue follower extends its movement as the aircraft ahead releases cells.
            // It should not wait for the leader to stop and request a fresh taxi clearance.
            if runtime.active_flights.contains(&pose.id)
                && matches!(
                    pose.stage,
                    openatc_core::traffic::motion::Stage::TaxiOut
                        | openatc_core::traffic::motion::Stage::TaxiIn
                )
                && pose.knots < 1.0
            {
                let current_approved = runtime.approved_orders.contains(&(pose.id, pose.segment));
                let next_approved = runtime
                    .approved_orders
                    .contains(&(pose.id, pose.segment + 1));
                runtime.world.resume_taxi(
                    generation,
                    pose.id,
                    &blocked,
                    current_approved,
                    next_approved,
                );
            }
            // A generated flight that is already taxiing started with its clearance. Approve
            // ordinary taxi segments as they are reached so a spawned taxiing aircraft never
            // sits in `awaiting clearance`. Runway crossings, line-up and takeoff still need
            // their own grants.
            if runtime.seeded.contains(&pose.id)
                && matches!(
                    pose.stage,
                    openatc_core::traffic::motion::Stage::TaxiOut
                        | openatc_core::traffic::motion::Stage::TaxiIn
                )
                && !runtime.approved_orders.contains(&(pose.id, pose.segment))
            {
                runtime.approved_orders.insert((pose.id, pose.segment));
            }
            if pose.awaiting_clearance {
                // Ambient seeded traffic has no flight plan or controller; it follows its
                // route and exposes itself to ATC only when the player is on frequency.
                if pose.id != 0
                    && runtime.seeded.contains(&pose.id)
                    && !runtime.flights.contains_key(&pose.id)
                {
                    runtime
                        .world
                        .authorise(generation, pose.id, pose.segment, &blocked);
                    continue;
                }
                let gate = matches!(
                    pose.stage,
                    openatc_core::traffic::motion::Stage::Parked
                        | openatc_core::traffic::motion::Stage::Turnaround
                );
                // Seeded aircraft never wait for a clearance on ordinary taxi or airborne
                // segments. Only gate traffic and runway entry remain controlled.
                if pose.id != 0
                    && runtime.seeded.contains(&pose.id)
                    && !gate
                    && !matches!(
                        pose.stage,
                        openatc_core::traffic::motion::Stage::LineUp
                            | openatc_core::traffic::motion::Stage::Takeoff
                            | openatc_core::traffic::motion::Stage::Crossing
                    )
                {
                    runtime
                        .world
                        .authorise(generation, pose.id, pose.segment, &blocked);
                    continue;
                }
                // A gate aircraft only needs an IFR clearance while the player is tuned to
                // its clearance frequency. Otherwise it advances on its own.
                if pose.id != 0
                    && gate
                    && !generated_on_player_frequency(runtime, telemetry, pose.id)
                {
                    runtime
                        .world
                        .authorise(generation, pose.id, pose.segment, &blocked);
                    continue;
                }
                // Complete flights carry their own clearances after legacy ground orders are removed.
                if runtime.active_flights.contains(&pose.id)
                    && pose.stage != openatc_core::traffic::motion::Stage::Turnaround
                    && !runtime.approved_orders.contains(&(pose.id, pose.segment))
                {
                    continue;
                }
                if !matches!(
                    pose.stage,
                    openatc_core::traffic::motion::Stage::Parked
                        | openatc_core::traffic::motion::Stage::Turnaround
                        | openatc_core::traffic::motion::Stage::TaxiOut
                        | openatc_core::traffic::motion::Stage::TaxiIn
                        | openatc_core::traffic::motion::Stage::Crossing
                ) && !runtime.approved_orders.contains(&(pose.id, pose.segment))
                {
                    continue;
                }
                if runtime.orders.get(&pose.id).is_some_and(|orders| {
                    orders.iter().any(|(segment, _)| *segment == pose.segment)
                }) && !runtime.approved_orders.contains(&(pose.id, pose.segment))
                {
                    continue;
                }
                if runtime.crossing_blocked(pose.id, pose.segment, &targets) {
                    continue;
                }
                // A landing aircraft keeps the runway, so a departure queued to line up waits at
                // the holding point instead of entering. Departures already lined up continue.
                if pose.stage == openatc_core::traffic::motion::Stage::LineUp
                    && runtime
                        .flights
                        .get(&pose.id)
                        .and_then(|flight| flight.route.segment(pose.segment))
                        .and_then(|segment| {
                            segment
                                .resources
                                .iter()
                                .find(|resource| resource.starts_with("RUNWAY/"))
                        })
                        .is_some_and(|key| matches!(runtime.runway_use(key), RunwayUse::Landing))
                {
                    continue;
                }
                runtime
                    .world
                    .authorise(generation, pose.id, pose.segment, &blocked);
            }
            if !runtime.active_flights.contains(&pose.id) {
                runtime
                    .world
                    .reserve_next_taxi(generation, pose.id, &blocked);
            }
            if runtime.active_flights.contains(&pose.id)
                && runtime
                    .approved_orders
                    .contains(&(pose.id, pose.segment + 1))
                && !runtime.crossing_blocked(pose.id, pose.segment + 1, &targets)
            {
                runtime.world.authorise_continuation(
                    generation,
                    pose.id,
                    pose.segment + 1,
                    &blocked,
                );
            }
        }
        clock_changed = runtime.world.advance(seconds, telemetry.paused)
            == openatc_core::traffic::motion::Advance::ClockChanged;
        if clock_changed {
            flight::clock_changed(runtime);
        }
        flight::synchronise_recoveries(runtime);
        update_sessions(runtime, seconds);
        let elapsed = runtime
            .previous_time
            .replace(seconds)
            .map_or(0.0, |previous| {
                let elapsed = seconds - previous;
                if (0.0..=30.0).contains(&elapsed) {
                    elapsed
                } else {
                    0.0
                }
            });
        for pose in runtime.world.snapshot() {
            let Some(frame) = runtime
                .frames
                .iter_mut()
                .find(|frame| frame.target.id == pose.id)
            else {
                continue;
            };
            frame.target.latitude = pose.position.latitude;
            frame.target.longitude = pose.position.longitude;
            frame.vertical_speed_fpm = if elapsed > 0.0 {
                (pose.position.altitude_feet - frame.target.altitude_feet) * 60.0 / elapsed
            } else {
                0.0
            };
            frame.target.altitude_feet = pose.position.altitude_feet;
            let heading = if pose.stage.on_ground() {
                pose.heading
            } else {
                runtime
                    .flights
                    .get(&pose.id)
                    .and_then(|flight| {
                        flight
                            .airspeeds
                            .get(&pose.recovery_from.unwrap_or(pose.segment))
                    })
                    .and_then(|speed| {
                        openatc_core::traffic::air::heading_from_ground(
                            pose.knots,
                            pose.heading,
                            speed.wind_from,
                            speed.wind_knots,
                        )
                        .ok()
                    })
                    .unwrap_or(pose.heading)
            };
            let body = (heading
                + if pose.stage == openatc_core::traffic::motion::Stage::Pushback {
                    180.0
                } else {
                    0.0
                })
            .rem_euclid(360.0);
            let turn = if pose.knots > 0.0 {
                openatc_core::navigation::turn(
                    frame
                        .body_heading_degrees
                        .unwrap_or(frame.target.track_degrees),
                    body,
                )
            } else {
                0.0
            };
            if pose.knots > 0.0 {
                frame.target.track_degrees = pose.heading;
                frame.body_heading_degrees = Some(body);
            }
            frame.target.ground_speed_knots = Some(pose.knots);
            frame.target.on_ground = pose.stage.on_ground();
            let Some(handling) = runtime.movement.get(&pose.id) else {
                continue;
            };
            // A seeded aircraft is already moving when it is first drawn, so its engines must
            // already be running. Without this the propeller and beacon animate up from a
            // cold start while the aircraft taxis.
            if pose.stage.on_ground() && pose.knots > 0.0 && frame.animation.engine < 0.5 {
                frame.animation.engine = 1.0;
                frame.animation.propeller_rpm = handling.propeller_rpm;
                frame.animation.beacon = true;
                frame.animation.navigation_lights = true;
            }
            let steps = elapsed.ceil().clamp(1.0, 30.0) as usize;
            for _ in 0..steps {
                frame.animation.advance(
                    animation::Input {
                        stage: pose.stage,
                        knots: pose.knots,
                        turn_degrees_sec: if elapsed > 0.0 { turn / elapsed } else { 0.0 },
                        vertical_fpm: frame.vertical_speed_fpm,
                        engines_running: !matches!(
                            pose.stage,
                            openatc_core::traffic::motion::Stage::Parked
                                | openatc_core::traffic::motion::Stage::Turnaround
                        ) && (pose.stage
                            != openatc_core::traffic::motion::Stage::Pushback
                            || runtime.approved_orders.contains(&(pose.id, pose.segment))),
                        fixed_gear: runtime.fixed_gear.contains(&pose.id),
                        landing_lights: pose.stage.landing_lights(),
                    },
                    handling,
                    if telemetry.paused {
                        0.0
                    } else {
                        elapsed / steps as f64
                    },
                );
            }
        }
    }
    clock_changed
}

/// Refresh per-aircraft departure delays before the controller or movement worker can grant a roll.
pub(crate) fn refresh_departure_wake(state: &mut EngineState) {
    openatc_core::traffic::wake_policy::set_standard(&state.settings.wake_standard);
    let mut delays = BTreeMap::new();
    let waiting = state
        .generated
        .world
        .snapshot()
        .into_iter()
        .filter(|pose| {
            !pose.recovering
                && (pose.stage == openatc_core::traffic::motion::Stage::LineUp
                    || pose.stage == openatc_core::traffic::motion::Stage::Takeoff
                        && pose.knots <= 0.0)
        })
        .map(|pose| pose.id)
        .collect::<BTreeSet<_>>();
    for (&id, flight) in &state.generated.flights {
        if !waiting.contains(&id) || !state.generated.active_flights.contains(&id) {
            continue;
        }
        let Some(segment) = flight
            .route
            .segment(1)
            .filter(|segment| segment.stage == openatc_core::traffic::motion::Stage::Takeoff)
        else {
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
        let Some(airport) = flight
            .clearances
            .get(&1)
            .and_then(|clearance| state.generated.surface_airport(&clearance.station.airport))
        else {
            continue;
        };
        let Some(start) = segment.points.first() else {
            continue;
        };
        let heading = segment.points.last().map_or(0.0, |end| {
            openatc_core::arrival::bearing(
                [start.latitude, start.longitude],
                [end.latitude, end.longitude],
            )
        });
        let Some(runway) = airport
            .runways
            .iter()
            .filter(|runway| {
                segment.resources.contains(&format!(
                    "RUNWAY/{}/{}/{}",
                    airport.icao, runway.first_name, runway.second_name
                ))
            })
            .filter_map(|runway| {
                let position = openatc_core::traffic::runway_position(
                    airport,
                    &runway.first_name,
                    start.latitude,
                    start.longitude,
                )?;
                let angle = openatc_core::navigation::turn(position.3, heading).abs();
                let error = angle.min((180.0 - angle).abs());
                (error < 20.0).then_some((runway, error))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(runway, _)| runway)
        else {
            continue;
        };
        let first = openatc_core::traffic::runway_position(
            airport,
            &runway.first_name,
            start.latitude,
            start.longitude,
        )
        .is_some_and(|position| openatc_core::navigation::turn(position.3, heading).abs() < 90.0);
        let name = if first {
            &runway.first_name
        } else {
            &runway.second_name
        };
        let remaining = crate::traffic::departure_wake_delay(
            state,
            id,
            airport,
            name,
            &frame.target.aircraft,
            start.latitude,
            start.longitude,
        );
        if remaining > 0.0 {
            delays.insert(id, remaining);
        }
    }
    state.generated.departure_wake = delays;
    withhold_conflicting_departures(&mut state.generated, &state.session.telemetry);
}

fn withhold_conflicting_departures(
    runtime: &mut Runtime,
    telemetry: &openatc_core::state::Telemetry,
) {
    let targets = external_targets(runtime, telemetry);
    let waiting = runtime
        .world
        .snapshot()
        .into_iter()
        .filter(|pose| {
            // Once the roll is established, a late arrival conflict must not cancel the
            // take-off. The movement world already cleared the runway before the roll; the
            // arrival or go-around is sequenced around the departing aircraft instead.
            let rolling =
                pose.stage == openatc_core::traffic::motion::Stage::Takeoff && pose.knots > 20.0;
            !pose.recovering
                && !rolling
                && runtime.active_flights.contains(&pose.id)
                && matches!(
                    pose.stage,
                    openatc_core::traffic::motion::Stage::LineUp
                        | openatc_core::traffic::motion::Stage::Takeoff
                )
        })
        .map(|pose| pose.id)
        .collect::<Vec<_>>();
    for id in waiting {
        // Surface occupancy includes the player. Airborne prediction must still check
        // every other aircraft when the player is parked.
        let airborne_blocked = runtime.approved_orders.contains(&(id, 1))
            && runtime
                .flights
                .get(&id)
                .and_then(|plan| plan.clearances.get(&1))
                .is_some_and(|clearance| separation::blocked(runtime, telemetry, id, 1, clearance));
        let crossing = runtime.crossing_blocked(id, 1, &targets);
        if !crossing && !airborne_blocked {
            continue;
        }
        if runtime
            .world
            .withhold_takeoff(runtime.world.generation(), id)
            && runtime.approved_orders.remove(&(id, 1))
        {
            let callsign = runtime
                .frames
                .iter()
                .find(|frame| frame.target.id == id)
                .map_or_else(|| id.to_string(), |frame| frame.target.callsign.clone());
            runtime.log(
                openatc_settings::TrafficLog::Summary,
                format!(
                    "{callsign} takeoff cancelled: crossing {crossing}, airborne {airborne_blocked}"
                ),
            );
            runtime.cancelled_takeoffs.insert(
                id,
                runtime
                    .departure_wake
                    .get(&id)
                    .is_some_and(|seconds| *seconds > 0.0),
            );
            if let Some(end) = runtime
                .flights
                .get(&id)
                .and_then(|flight| flight.clearances.get(&1))
                .and_then(|clearance| clearance.initial_climb)
            {
                for index in 2..=end {
                    runtime.approved_orders.remove(&(id, index));
                }
            }
        }
    }
}

fn withhold_conflicting_landings(
    runtime: &mut Runtime,
    telemetry: &openatc_core::state::Telemetry,
) {
    let targets = external_targets(runtime, telemetry);
    let mut revoke = Vec::new();
    for pose in runtime
        .world
        .snapshot()
        .into_iter()
        .filter(|pose| !pose.recovering && runtime.active_flights.contains(&pose.id))
    {
        let Some(plan) = runtime.flights.get(&pose.id) else {
            continue;
        };
        for (&start, contract) in &plan.clearances {
            if contract.landing_rollout.is_some()
                && runtime.approved_orders.contains(&(pose.id, start))
                && pose.segment <= start
            {
                let crossing = runtime.crossing_blocked(pose.id, start, &targets);
                let airborne = separation::blocked(runtime, telemetry, pose.id, start, contract);
                if crossing || airborne {
                    revoke.push((pose.id, start, crossing, airborne));
                }
            }
        }
    }
    for (id, start, crossing, airborne) in revoke {
        if runtime.cancelled_landings.insert((id, start)) {
            let callsign = runtime
                .frames
                .iter()
                .find(|frame| frame.target.id == id)
                .map_or_else(|| id.to_string(), |frame| frame.target.callsign.clone());
            runtime.log(
                openatc_settings::TrafficLog::Summary,
                format!(
                    "{callsign} landing clearance withdrawn: crossing {crossing}, airborne {airborne}"
                ),
            );
        }
        flight::revoke_landing(runtime, id, start);
    }
}

/// Validated CSL animation channel names shared by every traffic layer.
#[must_use]
pub fn animation_channels(config: &std::path::Path) -> BTreeMap<String, animation::Channel> {
    policy::channels(config)
}

/// Validated animation rates shared by every traffic layer.
#[must_use]
pub fn animation_handling(
    config: &std::path::Path,
) -> BTreeMap<animation::Kind, animation::Handling> {
    policy::load(config)
        .map(|(handling, _, _)| handling)
        .unwrap_or_else(|_| animation::builtin().unwrap_or_default())
}

/// Publish the most recent complete worker sample without waiting for simulation.
pub fn step(state: &mut EngineState) {
    let plan_key = format!(
        "{}/{}/{}",
        state.session_generation, state.session.plan.departure, state.session.plan.destination
    );
    if state.settings.auto_generated_traffic
        && state.session.ifr
        && !state.session.demo
        && !state.session.plan.departure.is_empty()
        && !state.session.plan.destination.is_empty()
        && state.auto_generated_plan != plan_key
    {
        state.auto_generated_plan = plan_key;
        state.settings.generated_traffic = true;
    }
    state.generated.log = state.settings.traffic_log;
    set_traffic_log(state.settings.traffic_log);
    let generated_ids = state.generated.published_ids.clone();
    if !state.settings.generated_traffic {
        state.generated = Runtime::default();
    }
    if state.settings.generated_traffic {
        let telemetry = state.session.telemetry.clone();
        let destination = state.arrival_airport.as_ref().map(|airport| {
            (
                airport.icao.clone(),
                airport.reference_latitude,
                airport.reference_longitude,
            )
        });
        population::request_nearby_scene(&mut state.generated, &telemetry, true, destination);
    }
    if !state.settings.generated_traffic || state.generated.epoch != Some(state.session_generation)
    {
        if state.settings.generated_traffic {
            state.session.generated_traffic.clear();
            state.session.generated_traffic_debug.clear();
            state.generated.visible.clear();
            state.generated.published_ids.clear();
        } else {
            // Generated traffic is off, but ambient traffic still owns its frames.
            state
                .session
                .generated_traffic
                .retain(|frame| !generated_ids.contains(&frame.target.id));
            state
                .session
                .generated_traffic_debug
                .retain(|debug| !generated_ids.contains(&debug.id));
        }
        state.session.generated_traffic_clock = None;
        state.session.generated_animation_channels.clear();
        state.session.generated_traffic_diagnostics.clear();
        return;
    }
    let mut visible = state
        .generated
        .policy
        .as_ref()
        .map(|policy| {
            display::select(
                &state.generated.frames,
                &state.session.telemetry,
                &state.generated.visible,
                policy,
            )
        })
        .unwrap_or_default();
    let current_segments: std::collections::BTreeMap<_, _> = state
        .generated
        .world
        .snapshot()
        .into_iter()
        .map(|pose| (pose.id, pose.segment))
        .collect();
    // The next hold is the start of the next runway crossing, or the end of the surface route
    // at the departure runway. Separate from the rendered frames because only the map uses it.
    let holding_points: std::collections::BTreeMap<u32, [f64; 2]> = visible
        .iter()
        .filter(|frame| frame.target.on_ground)
        .filter_map(|frame| {
            let route = state.generated.world.assigned_route(frame.target.id)?;
            let from = current_segments
                .get(&frame.target.id)
                .copied()
                .unwrap_or_default();
            let mut hold = None;
            for index in from..route.segment_count() {
                let segment = route.segment(index)?;
                if !segment.stage.on_ground() {
                    break;
                }
                if segment.stage == openatc_core::traffic::motion::Stage::Crossing {
                    hold = segment.points.first().copied();
                    break;
                }
            }
            let point = hold.unwrap_or_else(|| route.end_position());
            Some((frame.target.id, [point.latitude, point.longitude]))
        })
        .collect();
    state.session.generated_holding_points = holding_points;
    for frame in &mut visible {
        // A blank COM field made prepared ground traffic look as if it had no radio.
        if frame.target.on_ground && frame.target.com1_khz.is_none() {
            let own = [frame.target.latitude, frame.target.longitude];
            frame.target.com1_khz = state
                .stations
                .iter()
                .filter(|station| station.service == "Ground" || station.service == "Tower")
                .min_by(|left, right| {
                    let distance = |station: &openatc_core::stations::Station| {
                        openatc_core::arrival::distance_nm(
                            own,
                            [station.latitude, station.longitude],
                        )
                    };
                    distance(left).total_cmp(&distance(right))
                })
                .map(|station| station.khz);
        }
    }
    let published_ids: BTreeSet<u32> = generated_ids
        .iter()
        .copied()
        .chain(visible.iter().map(|frame| frame.target.id))
        .collect();
    state.generated.visible = visible.iter().map(|frame| frame.target.id).collect();
    state.generated.published_ids = state.generated.visible.clone();
    let poses = state.generated.world.snapshot();
    let debug: Vec<_> = visible
        .iter()
        .map(|frame| {
            let pose = poses.iter().find(|pose| pose.id == frame.target.id);
            let blocker = state.generated.world.surface_blocker(frame.target.id);
            let reason = if let Some(blocker) = blocker {
                format!("blocked by {blocker}")
            } else if let Some(exchange) = state
                .generated
                .exchange
                .as_ref()
                .filter(|exchange| exchange.id == frame.target.id)
            {
                if exchange.grant {
                    format!("clearing {:?}", pose.map(|pose| pose.stage))
                } else {
                    "holding: controller exchange".to_owned()
                }
            } else if pose.is_some_and(|pose| pose.awaiting_clearance) {
                "awaiting clearance".to_owned()
            } else if pose.is_some_and(|pose| pose.knots < 0.5) {
                "stopped".to_owned()
            } else {
                "moving".to_owned()
            };
            openatc_core::state::TrafficDebug {
                id: frame.target.id,
                callsign: frame.target.callsign.clone(),
                stage: pose
                    .map_or_else(|| "unknown".to_owned(), |pose| format!("{:?}", pose.stage)),
                reason,
                blocker,
                awaiting_clearance: pose.is_some_and(|pose| pose.awaiting_clearance),
                hold: None,
            }
        })
        .collect();
    state
        .session
        .generated_traffic_debug
        .retain(|debug| !published_ids.contains(&debug.id));
    state.session.generated_traffic_debug.extend(debug);
    if state.session.generated_traffic_debug.len() > 126 {
        state.session.generated_traffic_debug.truncate(126);
    }
    state
        .session
        .generated_traffic
        .retain(|frame| !published_ids.contains(&frame.target.id));
    state.session.generated_traffic.extend(visible);
    state
        .session
        .generated_traffic
        .sort_by_key(|frame| frame.target.id);
    state.session.generated_traffic_clock = state
        .session
        .telemetry
        .simulation_seconds
        .filter(|time| time.is_finite())
        .map(|time| (state.session_generation, time))
        .or_else(|| {
            state
                .generated
                .previous_time
                .map(|time| (state.session_generation, time))
        });
    state
        .session
        .generated_animation_channels
        .clone_from(&state.generated.channels);
    state
        .session
        .generated_traffic_diagnostics
        .clone_from(&state.generated.warnings);
    if let Some(observed) = &mut state.session.telemetry.traffic {
        let ids: BTreeSet<_> = state
            .generated
            .frames
            .iter()
            .map(|frame| frame.target.id)
            .collect();
        let callsigns: BTreeSet<String> = state
            .generated
            .frames
            .iter()
            .map(|frame| openatc_core::ops::callsign_key(&frame.target.callsign))
            .filter(|callsign| !callsign.is_empty())
            .collect();
        observed.retain(|target| {
            !ids.contains(&target.id)
                && !callsigns.contains(&openatc_core::ops::callsign_key(&target.callsign))
        });
        observed.extend(
            state
                .generated
                .frames
                .iter()
                .map(|frame| frame.target.clone()),
        );
    }
}

fn publish_revocations(current: &mut Runtime, worker: &Runtime, revoked: &BTreeSet<(u32, usize)>) {
    for &(id, index) in revoked {
        let same_plan = current
            .flights
            .get(&id)
            .zip(worker.flights.get(&id))
            .is_some_and(|(a, b)| Arc::ptr_eq(a, b));
        if !same_plan {
            continue;
        }
        current.approved_orders.remove(&(id, index));
        if let Some(&wake) = worker.cancelled_takeoffs.get(&id) {
            current.cancelled_takeoffs.insert(id, wake);
        }
        if worker.cancelled_landings.contains(&(id, index)) {
            current.cancelled_landings.insert((id, index));
        }
    }
}

/// At most one simulation job is outstanding; later telemetry replaces the next input.
pub async fn simulate(shared: Shared) {
    let (job, mut runtime, telemetry, workers, key, epoch) = {
        let mut state = shared.write().await;
        turnaround::publish_pending(&mut state);
        // Keep apron traffic moving while the full scene is prepared.
        let apron = !state.generated.apron_ids.is_empty();
        if !state.settings.generated_traffic
            || state.generated.simulation_job.is_some()
            || (!apron && state.generated.epoch != Some(state.session_generation))
        {
            return;
        }
        let job = next_job();
        state.generated.simulation_job = Some(job);
        (
            job,
            state.generated.clone(),
            state.session.telemetry.clone(),
            state.workers.clone(),
            state.generated.key.clone(),
            state.session_generation,
        )
    };
    let started = std::time::Instant::now();
    let queued = runtime.world.snapshot().len();
    let result = workers
        .run(move || {
            let approved = runtime.approved_orders.clone();
            let clock_changed = advance(&mut runtime, &telemetry);
            let revoked = approved
                .difference(&runtime.approved_orders)
                .copied()
                .collect::<BTreeSet<_>>();
            (runtime, clock_changed, revoked)
        })
        .await;
    let took = started.elapsed();
    if took > std::time::Duration::from_millis(400) {
        traffic_log(
            openatc_settings::TrafficLog::Summary,
            format!(
                "simulation job took {:.0} ms for {queued} poses",
                took.as_secs_f64() * 1000.0
            ),
        );
    }
    let mut state = shared.write().await;
    if !finish_job(&mut state.generated.simulation_job, job) {
        return;
    }
    if state.session_generation != epoch || state.generated.key != key {
        return;
    }
    if !state.settings.generated_traffic {
        return;
    }
    match result {
        Ok((runtime, clock_changed, revoked)) => {
            publish_revocations(&mut state.generated, &runtime, &revoked);
            state.generated.world = runtime.world;
            state.generated.frames = runtime.frames;
            state.generated.advisories = runtime.advisories;
            state.generated.local_spares = runtime.local_spares;
            state.generated.movement = runtime.movement;
            state.generated.previous_time = runtime.previous_time;
            if clock_changed {
                flight::clock_changed(&mut state.generated);
            }
            flight::synchronise_recoveries(&mut state.generated);
            refresh_departure_wake(&mut state);
            turnaround::publish_pending(&mut state);
            population::publish_pending(&mut state);
            population::maintain(&mut state);
            let telemetry = state.session.telemetry.clone();
            withhold_conflicting_landings(&mut state.generated, &telemetry);
            // The controller must not wait for the simulation job to be idle. A job that
            // outlives the telemetry interval leaves `simulation_job` set across the next
            // handler tick, so the departure gate and every clearance request were skipped for
            // as long as the movement stayed slow. Each published world now gets a radio turn,
            // which is exactly the cadence the traffic moves at.
            if !clock_changed {
                radio_tick(&mut state);
            }
            step(&mut state);
        }
        Err(error) => state.generated.status = error,
    }
}

/// Plan at most one parked arrival's onward departure away from the simulator thread.
/// A completed turnaround leaves the world unchanged until its replacement flight is installed.
pub(crate) async fn prepare_turnaround(shared: Shared) {
    turnaround::prepare(shared).await;
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use openatc_core::traffic::motion::{Route, Segment, Stage, Waypoint};

    #[test]
    fn scene_identity_follows_the_active_airport() {
        let mut state = crate::test_state();
        state.airport = Some(Airport {
            icao: "LFMN".into(),
            ..Default::default()
        });
        let departure = scene_key(&state);
        state.airport = Some(Airport {
            icao: "LIMC".into(),
            ..Default::default()
        });
        assert_ne!(departure, scene_key(&state));
    }

    #[test]
    fn scene_key_is_stable_when_the_departure_airport_resolves() {
        let mut state = crate::test_state();
        state.session.plan.departure = "LFMN".into();
        state.airport = None;
        let before = scene_key(&state);
        state.airport = Some(Airport {
            icao: "LFMN".into(),
            ..Default::default()
        });
        assert_eq!(before, scene_key(&state));
    }

    #[test]
    fn a_scene_for_another_airport_replaces_the_live_world() {
        let mut runtime = Runtime::default();
        runtime.airport = Some(Arc::new(Airport {
            icao: "LFMN".into(),
            ..Default::default()
        }));
        assert!(replaces_airport(
            &runtime,
            &Airport {
                icao: "LIMC".into(),
                ..Default::default()
            }
        ));
        assert!(!replaces_airport(
            &runtime,
            &Airport {
                icao: "LFMN".into(),
                ..Default::default()
            }
        ));
    }

    #[test]
    fn generated_traffic_auto_enables_once_per_plan() {
        let mut state = crate::test_state();
        state.settings.generated_traffic = false;
        state.settings.auto_generated_traffic = true;
        state.session.demo = false;
        state.session.ifr = true;
        state.session.plan.departure = "TEST".into();
        state.session.plan.destination = "DEST".into();
        step(&mut state);
        assert!(state.settings.generated_traffic);
        state.settings.generated_traffic = false;
        step(&mut state);
        assert!(
            !state.settings.generated_traffic,
            "a manual off must survive the same plan"
        );
    }

    #[test]
    fn fleet_selection_uses_the_selected_runway_and_published_airport_code() {
        use openatc_core::traffic::csl::{Match, Model};
        let mut airport = openatc_core::airport::demo_airport();
        airport.icao = "LOCL".into();
        let mut short = airport.runways[0].clone();
        short.first_name = "09L".into();
        short.second_name = "27R".into();
        short.second.east = short.first.east + 2800.0;
        short.second.north = short.first.north;
        let mut long = short.clone();
        long.first_name = "09R".into();
        long.second_name = "27L".into();
        long.second.east = long.first.east + 6000.0;
        airport.runways = vec![short, long];
        for stand in &mut airport.parking {
            stand.size = "F".into();
        }
        let codes = BTreeMap::from([("LOCL".into(), "KMEM".into())]);
        let selected = runway_port(&airport, "27R", &codes).unwrap();
        assert_eq!(selected.icao, "KMEM");
        assert_eq!(selected.runway, 2800.0);
        assert_eq!(port(&airport).unwrap().runway, 6000.0);
        assert!(runway_port(&airport, "MISSING", &codes).is_none());
        let mut profiles = Profiles::builtin().unwrap();
        profiles.airlines.retain(|name, _| name == "UPS");
        let ups = profiles.airlines.get_mut("UPS").unwrap();
        ups.fleet = BTreeMap::from([("B752".into(), 1), ("B744".into(), 1000)]);
        let models = Catalogue {
            models: ["B752", "B744"]
                .map(|aircraft| Model {
                    id: aircraft.into(),
                    objects: vec![],
                    vertical_offset: None,
                    matches: vec![Match {
                        aircraft: aircraft.into(),
                        operator: "UPS".into(),
                        livery: String::new(),
                    }],
                })
                .to_vec(),
            warnings: vec![],
        };
        let destination = Port {
            icao: "KSDF".into(),
            runway: 6000.0,
            parking_size: 'F',
        };
        let mut saw_heavy = false;
        for seed in 0..32 {
            let choice = profiles
                .select(
                    &selected,
                    &destination,
                    330.0,
                    Category::Cargo,
                    &models,
                    seed,
                )
                .unwrap();
            assert_eq!(choice.aircraft, "B752");
            let inactive = Port {
                runway: 6000.0,
                ..selected.clone()
            };
            saw_heavy |= profiles
                .select(
                    &inactive,
                    &destination,
                    330.0,
                    Category::Cargo,
                    &models,
                    seed,
                )
                .unwrap()
                .aircraft
                == "B744";
        }
        assert!(
            saw_heavy,
            "the unused long runway previously admitted the heavy fleet"
        );
        let no_spoke = BTreeMap::from([("LOCL".into(), "KAAA".into())]);
        assert_eq!(
            runway_port(&airport, "09L", &no_spoke).unwrap().icao,
            "KAAA"
        );
    }

    #[test]
    fn seeding_uses_approach_without_taxi_in() {
        let point = |east: f64, altitude: f64| Waypoint {
            latitude: 0.0,
            longitude: east / 111_320.0,
            altitude_feet: altitude,
        };
        let route = |id: u32| {
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Parked,
                        points: vec![point(0.0, 0.0)],
                        knots: 0.0,
                        resources: vec![format!("STAND/{id}")],
                        dwell_seconds: 30.0,
                    },
                    Segment {
                        stage: Stage::TaxiOut,
                        points: vec![point(0.0, 0.0), point(200.0, 0.0)],
                        knots: 10.0,
                        resources: vec![format!("TAXI/{id}")],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Climb,
                        points: vec![point(200.0, 0.0), point(1200.0, 0.0)],
                        knots: 150.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Cruise,
                        points: vec![point(1200.0, 0.0), point(2200.0, 0.0)],
                        knots: 200.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Approach,
                        points: vec![point(2200.0, 0.0), point(2700.0, 0.0)],
                        knots: 140.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Rollout,
                        points: vec![point(2700.0, 0.0), point(3000.0, 0.0)],
                        knots: 100.0,
                        resources: vec![format!("RUNWAY/{id}")],
                        dwell_seconds: 0.0,
                    },
                ],
                2000.0,
                2000.0,
            )
            .unwrap()
        };
        let station = |index: usize| openatc_core::stations::Station {
            name: format!("Test {index}"),
            service: "Ground".into(),
            airport: "TEST".into(),
            khz: 121_700,
            ..Default::default()
        };
        let prepared = |id: u32| {
            let route = std::sync::Arc::new(route(id));
            let clearances = (0..route.segment_count())
                .map(|index| {
                    (
                        index,
                        flight::Clearance {
                            station: station(index),
                            transfers: Vec::new(),
                            coordinated_recovery: Vec::new(),
                            instruction: format!("Phase {index}"),
                            readback: format!("Phase {index}"),
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
            std::sync::Arc::new(flight::Prepared {
                spacing_floor: None,
                route: route.clone(),
                airspeeds: BTreeMap::new(),
                recovery: BTreeMap::new(),
                clearances,
                vacated_reports: BTreeMap::new(),
                amendment: None,
            })
        };
        let model = std::sync::Arc::new(openatc_core::traffic::csl::Model {
            id: "fixture".into(),
            objects: Vec::new(),
            vertical_offset: None,
            matches: Vec::new(),
        });
        let mut scene = Scene {
            require_complete_flights: true,
            surface_corridors: Arc::new(BTreeSet::new()),
            airports: BTreeMap::new(),
            frames: Vec::new(),
            world: Default::default(),
            movement: BTreeMap::new(),
            airport: Airport::default(),
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
            policy: parse_operations(include_str!("../../../traffic/operations.toml")).unwrap(),
            fixed_gear: BTreeSet::new(),
            channels: BTreeMap::new(),
            orders: BTreeMap::new(),
            turnaround_inputs: Arc::new(BTreeMap::new()),
            stations: Arc::from(Vec::new()),
            regions: Arc::new(BTreeMap::new()),
            mora: Arc::new(Default::default()),
            simulator_root: "test".into(),
            reserved_stands: BTreeSet::new(),
            occupied_stand_cells: Arc::new(BTreeSet::new()),
            arrival_refusals: 0,
            warnings: Vec::new(),
            seeded_stations: BTreeMap::new(),
            advisories: BTreeMap::new(),
            local_spares: Vec::new(),
        };
        let generation = scene.world.generation();
        for id in 1..=4 {
            let mut prepared = (*prepared(id)).clone();
            if id == 1 {
                let mut segments: Vec<_> = (0..prepared.route.segment_count())
                    .map(|i| prepared.route.segment(i).unwrap().clone())
                    .collect();
                for segment in &mut segments[..3] {
                    for point in &mut segment.points {
                        point.latitude = 40.0;
                    }
                }
                segments[3].points[0].latitude = 40.0;
                prepared.route = Arc::new(Route::compile(segments, 2000.0, 2000.0).unwrap());
            }
            let prepared = Arc::new(prepared);
            assert!(scene.world.insert(generation, id, prepared.route.clone()));
            scene.frames.push(GeneratedTraffic {
                target: TrafficTarget {
                    id,
                    callsign: format!("TEST{id}"),
                    aircraft: "A20N".into(),
                    latitude: prepared.route.start_position().latitude,
                    on_ground: true,
                    ..Default::default()
                },
                vertical_speed_fpm: 0.0,
                body_heading_degrees: None,
                model: model.clone(),
                animation: Animation::default(),
            });
            scene.flights.insert(id, prepared);
        }
        let mut distant = scene.clone();
        distant.seed_airborne([80.0, 0.0], true);
        assert!(
            distant.seeded_stations.is_empty(),
            "Do not seed unseen flights at distant airports"
        );
        scene.seed_airborne([0.0, 0.0], false);
        assert_eq!(
            scene
                .world
                .snapshot()
                .iter()
                .find(|pose| pose.id == 1)
                .unwrap()
                .stage,
            Stage::Approach,
            "Choose the nearby arrival even when it departed from a distant stand"
        );
        assert_eq!(scene.seeded_stations.len(), 1, "one ground departure seed");
        let seeded: Vec<_> = scene
            .world
            .snapshot()
            .into_iter()
            .filter(|pose| pose.stage != Stage::Parked)
            .collect();
        assert!(
            seeded.iter().any(|pose| pose.stage == Stage::Approach),
            "one seeded aircraft should already be on approach: {seeded:?}"
        );
        assert!(
            !seeded.iter().any(|pose| pose.stage == Stage::TaxiIn),
            "taxi-in is never a startup state: {seeded:?}"
        );
        // A failed corridor must not leave an invisible stand owner in the live world.
        scene.flights.remove(&4);
        assert_eq!(scene.world.resource_owner("STAND/4"), Some(4));
        let mut production = Runtime::default();
        publish_scene(&mut production, scene, "complete".into(), 1);
        assert!(!production.frames.is_empty());
        assert_eq!(production.world.snapshot().len(), production.frames.len());
        assert_eq!(production.world.resource_owner("STAND/4"), None);
        assert!(
            production
                .frames
                .iter()
                .all(|frame| production.world.current_segment(frame.target.id).is_some())
        );
        assert_eq!(production.seeded_stations.len(), 1);
    }

    #[test]
    fn opening_cast_uses_the_configured_taxi_target() {
        use openatc_core::traffic::motion::{Route, Segment, Stage, Waypoint};
        let point = |east: f64, altitude: f64| Waypoint {
            latitude: 43.0,
            longitude: 7.0 + east,
            altitude_feet: altitude,
        };
        let station = openatc_core::stations::Station {
            name: "Test Ground".into(),
            service: "Ground".into(),
            airport: "TEST".into(),
            khz: 121_700,
            ..Default::default()
        };
        let prepared = |id: u32, arrival: bool| {
            let segments = if arrival {
                vec![
                    Segment {
                        stage: Stage::TaxiIn,
                        points: vec![point(2.0, 0.0), point(1.0, 0.0)],
                        knots: 12.0,
                        resources: vec![format!("IN/{id}/A")],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::TaxiIn,
                        points: vec![point(1.0, 0.0), point(0.0, 0.0)],
                        knots: 12.0,
                        resources: vec![format!("IN/{id}/B")],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Parked,
                        points: vec![point(0.0, 0.0)],
                        knots: 0.0,
                        resources: vec![format!("STAND/{id}")],
                        dwell_seconds: 10.0,
                    },
                ]
            } else {
                vec![
                    Segment {
                        stage: Stage::Parked,
                        points: vec![point(0.0, 0.0)],
                        knots: 0.0,
                        resources: vec![format!("STAND/{id}")],
                        dwell_seconds: 10.0,
                    },
                    Segment {
                        stage: Stage::TaxiOut,
                        points: vec![point(0.0, 0.0), point(2.0, 0.0)],
                        knots: 12.0,
                        resources: vec![format!("OUT/{id}")],
                        dwell_seconds: 0.0,
                    },
                ]
            };
            let route = Arc::new(Route::compile(segments, 4_000.0, 4_000.0).unwrap());
            let clearances = (0..route.segment_count())
                .map(|index| {
                    (
                        index,
                        flight::Clearance {
                            station: station.clone(),
                            transfers: Vec::new(),
                            coordinated_recovery: Vec::new(),
                            instruction: format!("Phase {index}"),
                            readback: format!("Phase {index}"),
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
            Arc::new(flight::Prepared {
                spacing_floor: None,
                route,
                airspeeds: BTreeMap::new(),
                recovery: BTreeMap::new(),
                clearances,
                vacated_reports: BTreeMap::new(),
                amendment: None,
            })
        };
        let taxi_route = |id: u32| {
            let mut segments = vec![Segment {
                stage: Stage::Parked,
                points: vec![point(0.0, 0.0)],
                knots: 0.0,
                resources: vec![format!("STAND/{id}")],
                dwell_seconds: 10.0,
            }];
            for step in 0..4 {
                segments.push(Segment {
                    stage: Stage::TaxiOut,
                    points: vec![
                        point(step as f64 * 0.005, 0.0),
                        point((step + 1) as f64 * 0.005, 0.0),
                    ],
                    knots: 12.0,
                    resources: vec![format!("OUT/{id}/{step}")],
                    dwell_seconds: 0.0,
                });
            }
            Arc::new(Route::compile(segments, 4_000.0, 4_000.0).unwrap())
        };
        let model = Arc::new(openatc_core::traffic::csl::Model {
            id: "fixture".into(),
            objects: Vec::new(),
            matches: Vec::new(),
            vertical_offset: None,
        });
        let mut scene = Scene {
            require_complete_flights: true,
            surface_corridors: Arc::new(BTreeSet::new()),
            airports: BTreeMap::new(),
            frames: Vec::new(),
            world: Default::default(),
            movement: BTreeMap::new(),
            airport: Airport::default(),
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
            policy: parse_operations(include_str!("../../../traffic/operations.toml")).unwrap(),
            fixed_gear: BTreeSet::new(),
            channels: BTreeMap::new(),
            orders: BTreeMap::new(),
            turnaround_inputs: Arc::new(BTreeMap::new()),
            stations: Arc::from(Vec::new()),
            regions: Arc::new(BTreeMap::new()),
            mora: Arc::new(Default::default()),
            simulator_root: "test".into(),
            reserved_stands: BTreeSet::new(),
            occupied_stand_cells: Arc::new(BTreeSet::new()),
            arrival_refusals: 0,
            warnings: Vec::new(),
            seeded_stations: BTreeMap::new(),
            advisories: BTreeMap::new(),
            local_spares: Vec::new(),
        };
        let generation = scene.world.generation();
        for id in 1..=8 {
            let prepared = prepared(id, false);
            assert!(scene.world.insert(generation, id, taxi_route(id)));
            scene.orders.insert(
                id,
                vec![(
                    0,
                    openatc_core::state::TaxiClearance {
                        airport: "TEST".into(),
                        hold_short_runway: "09".into(),
                        ..Default::default()
                    },
                )],
            );
            scene.frames.push(GeneratedTraffic {
                target: TrafficTarget {
                    id,
                    callsign: format!("DEP{id}"),
                    aircraft: "A20N".into(),
                    latitude: 43.0,
                    longitude: 7.0,
                    on_ground: true,
                    ..Default::default()
                },
                vertical_speed_fpm: 0.0,
                body_heading_degrees: None,
                model: model.clone(),
                animation: Animation::default(),
            });
            scene.flights.insert(id, prepared);
        }
        for id in 9..=11 {
            scene.frames.push(GeneratedTraffic {
                target: TrafficTarget {
                    id,
                    callsign: format!("ARR{id}"),
                    aircraft: "A20N".into(),
                    latitude: 43.0,
                    longitude: 7.0,
                    on_ground: true,
                    ..Default::default()
                },
                vertical_speed_fpm: 0.0,
                body_heading_degrees: None,
                model: model.clone(),
                animation: Animation::default(),
            });
            scene.flights.insert(id, prepared(id, true));
        }
        scene.seed_population([43.0, 7.0], 7, true, 4, false, &[], false, 0.0);
        let poses = scene.world.snapshot();
        let outbound: Vec<_> = poses
            .iter()
            .filter(|pose| pose.stage == Stage::TaxiOut)
            .collect();
        assert_eq!(
            outbound.iter().filter(|pose| pose.complete).count(),
            1,
            "exactly one aircraft is on the hold line: {outbound:?}"
        );
        assert!(
            outbound.iter().filter(|pose| !pose.complete).count() >= 3,
            "a queued aircraft and two mid-taxi aircraft are behind it: {outbound:?}"
        );
        assert!(
            !poses.iter().any(|pose| pose.stage == Stage::TaxiIn),
            "taxi-in is never a startup state: {poses:?}"
        );
        assert!(
            poses.iter().any(|pose| pose.stage == Stage::Parked),
            "some departures must remain at their gates to call Ground: {poses:?}"
        );
        assert!(
            scene
                .seeded_stations
                .keys()
                .all(|id| !(9..=11).contains(id)),
            "arrivals must not be seeded on the ground"
        );
        assert!(
            (1..=8).any(|id| scene.orders.contains_key(&id)),
            "gate and hold departures keep their taxi clearance"
        );
    }

    #[test]
    fn runway_use_gives_a_landing_aircraft_priority_over_a_departure() {
        use openatc_core::traffic::motion::{Route, Segment, Stage, Waypoint};
        let key = "RUNWAY/TEST/09/27";
        let route = |stage| {
            Arc::new(
                Route::compile(
                    vec![Segment {
                        stage,
                        points: vec![Waypoint {
                            latitude: 0.0,
                            longitude: 0.0,
                            altitude_feet: 0.0,
                        }],
                        knots: 0.0,
                        resources: vec![key.into()],
                        dwell_seconds: 0.0,
                    }],
                    1500.0,
                    1500.0,
                )
                .unwrap(),
            )
        };
        let prepared = |route: Arc<Route>| {
            Arc::new(flight::Prepared {
                spacing_floor: None,
                route,
                airspeeds: BTreeMap::new(),
                recovery: BTreeMap::new(),
                clearances: BTreeMap::new(),
                vacated_reports: BTreeMap::new(),
                amendment: None,
            })
        };
        let mut runtime = Runtime::default();
        assert_eq!(runtime.runway_use(key), RunwayUse::Clear);
        // An aircraft holding in position means the next aircraft holds short.
        let id = 1;
        runtime
            .world
            .insert(runtime.world.generation(), id, route(Stage::LineUp));
        runtime.flights.insert(id, prepared(route(Stage::LineUp)));
        assert_eq!(runtime.runway_use(key), RunwayUse::LineUp);
        // A landing aircraft takes priority, so the following aircraft holds short instead.
        let mut landing = Runtime::default();
        landing
            .world
            .insert(landing.world.generation(), 2, route(Stage::Rollout));
        landing.flights.insert(2, prepared(route(Stage::Rollout)));
        assert_eq!(landing.runway_use(key), RunwayUse::Landing);
        // The same traffic does not occupy a different runway.
        assert_eq!(landing.runway_use("RUNWAY/TEST/27/09"), RunwayUse::Clear);
    }

    #[test]
    fn travel_beyond_the_surveyed_area_requests_a_new_survey() {
        // No survey yet, or movement inside the covered radius, reuses the current airports.
        assert!(!travel_outside_coverage(None, [43.65, 7.20], 50.0));
        assert!(!travel_outside_coverage(
            Some([43.65, 7.20]),
            [43.80, 7.20],
            50.0
        ));
        // A position beyond the coverage radius starts a fresh survey.
        assert!(travel_outside_coverage(
            Some([43.65, 7.20]),
            [44.65, 7.20],
            50.0
        ));
        // Coordinates at the boundary remain covered.
        assert!(!travel_outside_coverage(
            Some([43.65, 7.20]),
            [44.45, 7.20],
            50.0
        ));
    }
    #[tokio::test]
    async fn registered_airports_keep_surface_ownership_and_receive_weather_requests() {
        use axum::{Json, extract::State};
        let mut state = crate::test_state();
        state.station_root = state.settings.simulator_root.clone();
        let mut first = openatc_core::airport::demo_airport();
        first.icao = "FIRST".into();
        let mut third = first.clone();
        third.icao = "THIRD".into();
        third.reference_latitude += 2.0;
        let target = TrafficTarget {
            id: 2,
            on_ground: true,
            wingspan_meters: Some(12.0),
            latitude: third.reference_latitude,
            longitude: third.reference_longitude,
            ..Default::default()
        };
        state.generated.policy =
            Some(parse_operations(include_str!("../../../traffic/operations.toml")).unwrap());
        state.generated.airport = Some(Arc::new(first.clone()));
        state.generated.airports = [first, third.clone()]
            .into_iter()
            .map(|airport| (airport.icao.clone(), Arc::new(airport)))
            .collect();
        state.generated.orders.insert(
            3,
            vec![(
                0,
                openatc_core::state::TaxiClearance {
                    airport: "THIRD".into(),
                    ..Default::default()
                },
            )],
        );
        assert_eq!(state.generated.departure_airport(3).unwrap().icao, "THIRD");
        assert_eq!(state.generated.operational_airports().count(), 2);
        let mut telemetry = state.session.telemetry.clone();
        telemetry.on_ground = false;
        telemetry.traffic = Some(vec![target.clone()]);
        let blocked = external_surface_cells(&state.generated, &telemetry);
        let third_cells = cells(&third, Point::default(), 6.0, 50.0);
        assert!(third_cells.is_subset(&blocked));
        assert!(!blocked.iter().any(|resource| resource.contains("FIRST")));
        let shared = Arc::new(tokio::sync::RwLock::new(state));
        let snapshot = crate::routes::full_state(State(shared.clone())).await.0;
        assert!(
            snapshot["weatherAirports"]
                .as_array()
                .unwrap()
                .iter()
                .any(|airport| airport["airport"] == "THIRD"
                    && airport["latitude"] == third.reference_latitude)
        );
        let response = crate::radio::weather(State(shared.clone()), Json(serde_json::json!({
            "airport":"THIRD", "latitude":third.reference_latitude, "longitude":third.reference_longitude,
            "sampleAltitudeFeet":third.elevation_feet, "pressureHpa":1013.25, "temperatureC":15.0,
            "dewpointC":10.0, "visibilityMeters":10000.0, "magneticVariationDegrees":0.0,
            "windDegrees":0.0, "windKnots":0.0
        }))).await;
        assert!(response.status().is_success());
        assert!(shared.read().await.sim_weather.contains_key("THIRD"));
    }

    #[tokio::test]
    async fn accepted_simulator_weather_updates_the_planning_watch() {
        use axum::{Json, extract::State};
        let mut state = crate::test_state();
        state.station_root = state.settings.simulator_root.clone();
        let airport = Airport {
            icao: "TEST".into(),
            reference_latitude: 43.0,
            reference_longitude: 7.0,
            elevation_feet: 25.0,
            ..Default::default()
        };
        state.airport = Some(airport.clone());
        let observations = state.generated.weather_observations.clone();
        let shared = Arc::new(tokio::sync::RwLock::new(state));
        let sample = serde_json::json!({
            "airport":"TEST", "latitude":43.0, "longitude":7.0,
            "sampleAltitudeFeet":25.0, "pressureHpa":1013.25, "temperatureC":15.0,
            "dewpointC":10.0, "visibilityMeters":10000.0,
            "magneticVariationDegrees":2.0, "windDegrees":220.0, "windKnots":12.0
        });
        assert!(
            crate::radio::weather(State(shared.clone()), Json(sample.clone()))
                .await
                .status()
                .is_success()
        );
        let retained = observations.read().unwrap()["TEST"].clone();
        let weather = environment::Environment::from_sample(&airport, &retained)
            .unwrap()
            .follow("TEST", &observations, &sample);
        assert!(weather.fresh());
        let mut rejected = sample.clone();
        rejected["pressureHpa"] = serde_json::json!(500.0);
        assert!(
            !crate::radio::weather(State(shared.clone()), Json(rejected))
                .await
                .status()
                .is_success()
        );
        assert_eq!(observations.read().unwrap()["TEST"], retained);
        let mut changed = sample;
        changed["windKnots"] = serde_json::json!(15.0);
        assert!(
            crate::radio::weather(State(shared), Json(changed))
                .await
                .status()
                .is_success()
        );
        assert!(!weather.fresh());
    }

    #[test]
    fn traffic_mix_coverage_does_not_guess_between_ambiguous_operator_names() {
        let mut profiles = Profiles::builtin().unwrap();
        let name = profiles
            .airlines
            .values()
            .find(|operator| operator.category == Category::Airline)
            .unwrap()
            .name
            .clone();
        assert_eq!(profile_category(&profiles, &name), Some(Category::Airline));
        let mut other = profiles
            .airlines
            .values()
            .find(|operator| operator.category == Category::Regional)
            .unwrap()
            .clone();
        other.name = name.clone();
        profiles.airlines.insert("TESTCAT".into(), other);
        assert_eq!(profile_category(&profiles, &name), None);
        assert_eq!(profile_category(&profiles, "Missing operator"), None);
    }

    #[test]
    fn surface_retries_refuse_ambiguous_edges_and_preserve_runway_links() {
        let point = |east, north| Point {
            east,
            north,
            height: 0.0,
        };
        let mut airport = Airport {
            nodes: vec![
                openatc_core::airport::TaxiNode {
                    id: 1,
                    point: point(0.0, 0.0),
                },
                openatc_core::airport::TaxiNode {
                    id: 2,
                    point: point(100.0, 0.0),
                },
                openatc_core::airport::TaxiNode {
                    id: 3,
                    point: point(0.0, 20.0),
                },
                openatc_core::airport::TaxiNode {
                    id: 4,
                    point: point(100.0, 20.0),
                },
            ],
            edges: vec![
                openatc_core::airport::TaxiEdge {
                    first: 1,
                    second: 2,
                    runway: true,
                    ..Default::default()
                },
                openatc_core::airport::TaxiEdge {
                    first: 3,
                    second: 4,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            unsupported_surface_edge(&airport, point(50.0, 0.0), 2.0),
            None
        );
        assert_eq!(
            unsupported_surface_edge(&airport, point(50.0, 19.0), 2.0),
            Some(1)
        );
        airport.edges.push(airport.edges[1].clone());
        assert_eq!(
            unsupported_surface_edge(&airport, point(50.0, 19.0), 2.0),
            None
        );
    }

    #[test]
    fn arrival_surface_monitoring_uses_both_airports() {
        let mut runtime = Runtime {
            airport: Some(Arc::new(Airport {
                icao: "ORIG".into(),
                reference_latitude: 40.0,
                reference_longitude: 0.0,
                ..Default::default()
            })),
            ..Default::default()
        };
        runtime.destination = Some(Arc::new(Airport {
            icao: "DEST".into(),
            reference_latitude: 45.0,
            reference_longitude: 8.0,
            ..Default::default()
        }));
        runtime.policy =
            Some(parse_operations(include_str!("../../../traffic/operations.toml")).unwrap());
        assert_eq!(runtime.surface_airport("DEST").unwrap().icao, "DEST");
        assert_eq!(runtime.surface_airport("ORIG").unwrap().icao, "ORIG");
        assert!(runtime.surface_airport("UNKNOWN").is_none());
        let telemetry = openatc_core::state::Telemetry {
            latitude: 45.0,
            longitude: 8.0,
            on_ground: true,
            traffic: Some(vec![]),
            ..Default::default()
        };
        let blocked = external_surface_cells(&runtime, &telemetry);
        let expected = cells(
            runtime.surface_airport("DEST").unwrap(),
            Point::default(),
            40.0,
            50.0,
        );
        assert!(!expected.is_empty());
        assert!(expected.is_subset(&blocked));
        let origin = cells(
            runtime.surface_airport("ORIG").unwrap(),
            Point::default(),
            40.0,
            50.0,
        );
        assert!(origin.is_disjoint(&blocked));
    }
    #[test]
    fn pushback_waits_for_the_completed_radio_exchange() {
        use openatc_core::traffic::motion::{Route, Segment, Stage, Waypoint};
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.generated.epoch = Some(state.session_generation);
        state.settings.hear_traffic_radio = true;
        state.generated.policy =
            Some(parse_operations(include_str!("../../../traffic/operations.toml")).unwrap());
        state.generated.airport = Some(Arc::new(Airport {
            icao: "OTHER".into(),
            ..Default::default()
        }));
        state.generated.destination = Some(Arc::new(Airport {
            icao: "TEST".into(),
            ..Default::default()
        }));
        state.generated.orders.insert(
            7,
            vec![(
                1,
                openatc_core::state::TaxiClearance {
                    airport: "TEST".into(),
                    ..Default::default()
                },
            )],
        );
        state.stations.push(openatc_core::stations::Station {
            airport: "TEST".into(),
            name: "Test Ground".into(),
            service: "Ground".into(),
            khz: 121_700,
            ..Default::default()
        });
        state.session.telemetry.com1_khz = 121_700;
        state.session.telemetry.com2_khz = Some(121_700);
        state.session.control_radio = openatc_core::state::Radio::Com2;
        state.session.telemetry.radio_power = true;
        state.generated.frames.push(GeneratedTraffic {
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
            target: TrafficTarget {
                id: 7,
                callsign: "TEST7".into(),
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
        let point = Waypoint {
            latitude: 43.0,
            longitude: 7.0,
            altitude_feet: 10.0,
        };
        assert!(
            state.generated.world.insert(
                0,
                7,
                Route::compile(
                    vec![Segment {
                        stage: Stage::Pushback,
                        points: vec![
                            point,
                            Waypoint {
                                latitude: 43.0001,
                                ..point
                            }
                        ],
                        knots: 3.0,
                        resources: vec!["PUSHBACK".into()],
                        dwell_seconds: 0.0,
                    }],
                    100.0,
                    100.0
                )
                .unwrap()
                .into()
            )
        );
        state.generated.require_complete_flights = true;
        for _ in 0..4 {
            radio_tick(&mut state);
        }
        assert!(state.generated.exchange.is_none());
        assert!(state.session.transcript.is_empty());
        assert!(!state.generated.approved_orders.contains(&(7, 0)));
        assert_eq!(state.generated.world.snapshot()[0].knots, 0.0);
        // The remaining fixture exercises the ground-only exchange independently.
        state.generated.require_complete_flights = false;
        for _ in 0..3 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            radio_tick(&mut state);
            assert!(!state.generated.approved_orders.contains(&(7, 0)));
            assert_eq!(state.generated.world.snapshot()[0].knots, 0.0);
        }
        state.generated.exchange.as_mut().unwrap().due = Instant::now();
        radio_tick(&mut state);
        assert!(state.generated.approved_orders.contains(&(7, 0)));
        assert_eq!(state.session.transcript.len(), 3);
        assert!(
            state
                .session
                .transcript
                .iter()
                .all(|entry| entry.radio == Some(openatc_core::state::Radio::Com2))
        );
        assert!(state.session.transcript[0].text.contains("pushback"));
        assert!(state.session.transcript[2].text.contains("ready to taxi"));
    }
    #[tokio::test]
    async fn reset_while_worker_is_queued_does_not_block_or_replace_the_new_scene() {
        let mut engine = crate::test_state();
        engine.session.telemetry.position_valid = true;
        engine.settings.generated_traffic = true;
        let frame = |id| GeneratedTraffic {
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
            target: TrafficTarget {
                id,
                ..Default::default()
            },
            model: Arc::new(openatc_core::traffic::csl::Model {
                id: "TEST".into(),
                objects: vec![],
                vertical_offset: None,
                matches: vec![],
            }),
            animation: Animation::default(),
        };
        engine.generated.epoch = Some(engine.session_generation);
        engine.generated.key = "old".into();
        engine.generated.frames = vec![frame(1)];
        let workers = engine.workers.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let blocker = tokio::spawn(async move {
            workers
                .run(move || {
                    let _ = started.send(());
                    let _ = wait.recv();
                })
                .await
                .unwrap();
        });
        ready.await.unwrap();
        let shared = Arc::new(tokio::sync::RwLock::new(engine));
        let old = tokio::spawn(simulate(shared.clone()));
        tokio::time::timeout(Duration::from_secs(2), async {
            while shared.read().await.generated.simulation_job.is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let old_job = shared.read().await.generated.simulation_job;
        {
            let mut engine = shared.write().await;
            engine.session_generation += 1;
            engine.generated = Runtime::default();
            engine.generated.policy =
                Some(parse_operations(include_str!("../../../traffic/operations.toml")).unwrap());
            engine.generated.epoch = Some(engine.session_generation);
            engine.generated.key = "new".into();
            engine.generated.frames = vec![frame(2)];
        }
        let new = tokio::spawn(simulate(shared.clone()));
        tokio::time::timeout(Duration::from_secs(2), async {
            while shared.read().await.generated.simulation_job.is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_ne!(old_job, shared.read().await.generated.simulation_job);
        release.send(()).unwrap();
        blocker.await.unwrap();
        old.await.unwrap();
        new.await.unwrap();
        let engine = shared.read().await;
        assert!(engine.generated.simulation_job.is_none());
        assert_eq!(engine.generated.frames[0].target.id, 2);
        assert_eq!(engine.session.generated_traffic[0].target.id, 2);
    }
    #[test]
    #[ignore = "requires installed scenery and Bluebell"]
    fn installed_bidirectional_routes_keep_airport_ownership_through_controller_flights() {
        installed_bidirectional_lifecycles([2, 0, 0, 0]);
    }

    #[test]
    #[ignore = "requires installed scenery and Bluebell"]
    fn installed_mixed_bidirectional_lifecycles_reach_parking() {
        installed_bidirectional_lifecycles([2, 2, 0, 0]);
    }

    fn installed_bidirectional_lifecycles(counts: [u8; 4]) {
        let expected = counts
            .iter()
            .map(|count| usize::from(*count))
            .sum::<usize>();
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let stations: Arc<[openatc_core::stations::Station]> =
            openatc_core::stations::load_index(&root).unwrap().into();
        // Installed scenery planning can outlive a single weather sample. Keep the fixture's
        // same conditions fresh, just as repeated simulator observations do in production.
        let weather: BTreeMap<String, (serde_json::Value, Instant)> = ["LFMN", "LIMC"]
            .into_iter()
            .map(|id| {
                let airport = airport(&root, id).unwrap();
                (
                    id.into(),
                    (
                        serde_json::json!({
                            "airport": id,
                            "latitude": airport.reference_latitude,
                            "longitude": airport.reference_longitude,
                            "sampleAltitudeFeet": airport.elevation_feet,
                            "pressureHpa": 1013.25,
                            "temperatureC": 15.0,
                            "magneticVariationDegrees": 0.0,
                            "windDegrees": 0.0,
                            "windKnots": 0.0,
                        }),
                        Instant::now(),
                    ),
                )
            })
            .collect();
        let observations: environment::Observations =
            Arc::new(std::sync::RwLock::new(weather.clone()));
        struct RefreshWeather {
            stop: Arc<std::sync::atomic::AtomicBool>,
            worker: Option<std::thread::JoinHandle<()>>,
        }
        impl Drop for RefreshWeather {
            fn drop(&mut self) {
                self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
                if let Some(worker) = self.worker.take() {
                    worker.join().unwrap();
                }
            }
        }
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _refresh = RefreshWeather {
            stop: stop.clone(),
            worker: Some({
                let observations = observations.clone();
                std::thread::spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        for (_, time) in observations.write().unwrap().values_mut() {
                            *time = Instant::now();
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                })
            }),
        };
        let mut input = Preparation {
            weather_observations: Some(observations),
            reserved_stand_cells: BTreeSet::new(),
            local_pattern_traffic: false,
            seed_single_arrival: false,
            player_airborne: false,
            minimum_airborne_spawn_nm: 0.0,
            airport_codes: BTreeMap::new(),
            reserved_surface_corridors: BTreeSet::new(),
            reserved_stands: BTreeSet::new(),
            stations: stations.clone(),
            regions: openatc_core::regions::load_regions(
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../regions.toml"),
            )
            .unwrap(),
            simulator_root: root.clone(),
            origin: airport(&root, "LFMN").unwrap(),
            destination: airport(&root, "LIMC").unwrap(),
            profiles: Arc::new(Profiles::builtin().unwrap()),
            models: Arc::new(
                Catalogue::scan(
                    &std::path::Path::new(&root).join("Resources/plugins/OpenATC/Resources/CSL"),
                )
                .unwrap(),
            ),
            counts,
            own: [43.655_764, 7.202_480],
            observed: vec![TrafficTarget {
                id: 0,
                latitude: 43.655_764,
                longitude: 7.202_480,
                on_ground: true,
                ..Default::default()
            }],
            epoch: 1,
            seed: 1,
            startup_cast: false,
            taxi_target: 4,
            force_runway: false,
            force_arrival_runway: false,
            runway: "04R".into(),
            arrival_runway: "35R".into(),
            config: std::path::PathBuf::new(),
            weather,
        };
        // This fixture proves the full gate-to-gate lifecycle. Startup phase seeding is
        // covered separately and must not remove the departure orders under this assertion.
        let mut inbound = input.clone();
        std::mem::swap(&mut inbound.origin, &mut inbound.destination);
        std::mem::swap(&mut inbound.runway, &mut inbound.arrival_runway);
        for index in 0..4 {
            input.counts[index] = counts[index].div_ceil(2);
            inbound.counts[index] = counts[index] / 2;
        }
        let scene = prepare_direction(input).unwrap();
        scene.observe_prepared(&mut inbound);
        let mut scene = scene.merge(prepare_direction(inbound).unwrap()).unwrap();
        assert_eq!(scene.frames.len(), expected, "{:?}", scene.warnings);
        assert_eq!(
            scene.nominal_flights.len(),
            expected,
            "{:?}",
            scene.warnings
        );
        for (&id, draft) in &scene.nominal_flights {
            let decoded = &scene.decoded_terminals[&id];
            assert!(decoded.candidate < scene.terminal_routes.len());
            // This branch checks geometry/controller integration; installed obstacle release stays separate.
            let prepared = draft
                .as_ref()
                .clone()
                .protect(|_, _| true)
                .unwrap()
                .finish_all_production(|_, _| true)
                .unwrap();
            let departure = &scene.orders[&id][0].1.airport;
            assert_eq!(&prepared.clearances[&0].station.airport, departure);
            assert!(
                prepared
                    .clearances
                    .values()
                    .any(|contract| contract.station.service == "Ground"
                        && contract.station.airport != *departure)
            );
            scene.flights.insert(id, Arc::new(prepared));
        }
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.stations = stations.to_vec();
        publish_scene(
            &mut state.generated,
            scene,
            "bidirectional-fixture".into(),
            state.session_generation,
        );
        state.session.telemetry.position_valid = true;
        state.session.telemetry.on_ground = true;
        state.session.telemetry.latitude = 43.655_764;
        state.session.telemetry.longitude = 7.202_480;
        state.session.telemetry.traffic = Some(vec![]);
        let mut stages = BTreeMap::<u32, Vec<openatc_core::traffic::motion::Stage>>::new();
        let mut stopped_since = None;
        for second in 0..8000 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            if let Some(exchange) = &mut state.generated.delivery_exchange {
                exchange.due = Instant::now();
            }
            radio_tick(&mut state);
            state.session.telemetry.simulation_seconds = Some(f64::from(second));
            advance(&mut state.generated, &state.session.telemetry);
            for pose in state.generated.world.snapshot() {
                let visited = stages.entry(pose.id).or_default();
                if !visited.contains(&pose.stage) {
                    visited.push(pose.stage);
                }
            }
            if state.generated.world.snapshot().iter().all(|pose| {
                pose.complete && pose.stage == openatc_core::traffic::motion::Stage::Turnaround
            }) {
                break;
            }
            let poses = state.generated.world.snapshot();
            if poses.iter().any(|pose| {
                pose.awaiting_clearance
                    && pose.stage == openatc_core::traffic::motion::Stage::TaxiIn
            }) && poses.iter().all(|pose| {
                pose.complete
                    || pose.awaiting_clearance
                        && pose.stage == openatc_core::traffic::motion::Stage::TaxiIn
            }) && state.generated.exchange.is_none()
            {
                let since = *stopped_since.get_or_insert(second);
                if second - since > 300 {
                    break;
                }
            } else {
                stopped_since = None;
            }
        }
        for pose in state
            .generated
            .world
            .snapshot()
            .into_iter()
            .filter(|pose| !pose.complete)
        {
            if let Some(plan) = state.generated.flights.get(&pose.id) {
                if let Some(contract) = plan.clearances.get(&pose.segment) {
                    eprintln!(
                        "Earlier gates: takeoff cancellation {}, landing cancellation {}, vacated {:?}",
                        state.generated.cancelled_takeoffs.contains_key(&pose.id),
                        state
                            .generated
                            .cancelled_landings
                            .iter()
                            .any(|(id, _)| *id == pose.id),
                        plan.vacated_reports.get(&pose.segment).map(|report| (
                            &report.airport,
                            &report.runway,
                            state
                                .generated
                                .reported_crossings
                                .contains(&(pose.id, pose.segment - 1)),
                            plan.clearances.get(&(pose.segment - 1)).map(|previous| (
                                previous.station.khz,
                                previous.station.name.as_str()
                            ))
                        ))
                    );
                    let blocked =
                        external_surface_cells(&state.generated, &state.session.telemetry);
                    let mut probe = state.generated.world.clone();
                    let grant =
                        probe.authorise(probe.generation(), pose.id, pose.segment, &blocked);
                    let reservation = contract.taxi_through.map(|end| {
                        (
                            end,
                            probe.reserve_surface_until(
                                probe.generation(),
                                pose.id,
                                pose.segment,
                                end,
                                &blocked,
                            ),
                        )
                    });
                    let frame = state
                        .generated
                        .frames
                        .iter()
                        .find(|frame| frame.target.id == pose.id)
                        .unwrap();
                    eprintln!(
                        "Surface {} {}: blocker {:?}, grant {}, reservation {:?}, approved {}, tuned {:?}, expected {}/{}, controller {:?}, handoff {:?}, checkins {:?}",
                        pose.id,
                        frame.target.aircraft,
                        state.generated.world.surface_blocker(pose.id),
                        grant,
                        reservation,
                        state
                            .generated
                            .approved_orders
                            .contains(&(pose.id, pose.segment)),
                        frame.target.com1_khz,
                        contract.station.name,
                        contract.station.khz,
                        state
                            .generated
                            .controllers
                            .get(&pose.id)
                            .map(|station| (&station.name, station.khz)),
                        state.generated.handoff_targets.get(&pose.id),
                        state.generated.pending_checkins
                    );
                }
                eprintln!(
                    "Onward refusal: {:?}",
                    flight::rejoin_refusal(&state.generated, &state.session.telemetry, pose.id)
                );
                eprintln!(
                    "Flight {} current {:?} at segment {}, {} contracts; approved count {}; next exchange {:?}",
                    pose.id,
                    pose.stage,
                    pose.segment,
                    plan.clearances.len(),
                    state.generated.approved_orders.len(),
                    state
                        .generated
                        .exchange
                        .as_ref()
                        .map(|exchange| (exchange.next, exchange.grant))
                );
            }
        }
        assert!(
            state
                .generated
                .world
                .snapshot()
                .iter()
                .all(|pose| pose.complete
                    && pose.stage == openatc_core::traffic::motion::Stage::Turnaround),
            "{:?}; {:?}",
            state.generated.world.snapshot(),
            state.generated.warnings
        );
        assert_eq!(state.generated.world.snapshot().len(), expected);
        assert_eq!(stages.len(), expected);
        for visited in stages.values() {
            use openatc_core::traffic::motion::Stage;
            for required in [
                Stage::TaxiOut,
                Stage::Takeoff,
                Stage::Climb,
                Stage::Cruise,
                Stage::Descent,
                Stage::Approach,
                Stage::Rollout,
                Stage::TaxiIn,
                Stage::Turnaround,
            ] {
                assert!(
                    visited.contains(&required),
                    "Missing {required:?} from {visited:?}"
                );
            }
        }
    }

    #[test]
    fn runway_retries_preserve_the_wind_compatible_flow() {
        let mut airport = openatc_core::airport::demo_airport();
        let original = airport.runways[0].clone();
        let mut parallel = original.clone();
        parallel.first_name = "09L".into();
        parallel.second_name = "27R".into();
        let mut cross = original.clone();
        cross.first_name = "18".into();
        cross.second_name = "36".into();
        cross.second.east = cross.first.east;
        cross.second.north = cross.first.north + 2000.0;
        airport.runways = vec![original, parallel, cross];
        let sample = serde_json::json!({
            "airport":airport.icao, "latitude":airport.reference_latitude,
            "longitude":airport.reference_longitude, "sampleAltitudeFeet":airport.elevation_feet,
            "pressureHpa":1013.25, "temperatureC":15.0, "magneticVariationDegrees":0.0,
            "windDegrees":90.0, "windKnots":12.0
        });
        let weather =
            environment::Environment::from_sample(&airport, &(sample, Instant::now())).unwrap();
        assert_eq!(
            same_flow_runways(&airport, "09", &weather),
            vec!["09", "09L"]
        );
        assert_eq!(
            same_flow_runways(&airport, "27", &weather),
            vec!["09", "09L"]
        );
    }

    #[tokio::test]
    #[ignore = "requires installed scenery and Bluebell"]
    async fn installed_preparation_worker_revalidates_live_weather() {
        installed_network_lifecycles(false, InstalledNetwork::Filed).await;
    }

    #[tokio::test]
    #[ignore = "requires installed scenery and Bluebell"]
    async fn installed_network_airline_regional_lifecycles_reach_parking() {
        installed_network_lifecycles(true, InstalledNetwork::Filed).await;
    }

    #[tokio::test]
    #[ignore = "requires installed scenery and Bluebell"]
    async fn installed_unfiled_player_network_lifecycles_reach_parking() {
        installed_network_lifecycles(true, InstalledNetwork::Unfiled).await;
    }

    #[test]
    #[ignore = "requires installed airport scenery"]
    fn installed_cargo_ground_routes_reach_the_holding_point() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let memphis = airport(&root, "KMEM").unwrap();
        let profiles = Profiles::builtin().unwrap();
        let handling = animation::builtin().unwrap();
        let policy = parse_operations(include_str!("../../../traffic/operations.toml")).unwrap();
        eprintln!(
            "KMEM source {}; {} pavement contours, {} faces, {} parking stands",
            memphis.source,
            memphis.pavements.len(),
            memphis.pavement_triangles.len(),
            memphis.parking.len()
        );
        let mut missing = Vec::new();
        for code in ["B738", "B752", "C172"] {
            let aircraft = profiles.aircraft_for(code).unwrap();
            let accepted = connected_stands(&memphis, aircraft, &handling, &policy, "36L");
            eprintln!("KMEM {code}: {accepted} connected stands");
            if accepted == 0 {
                missing.push(code);
            }
        }
        assert!(missing.is_empty(), "No connected stands for {missing:?}");
        for code in ["B763", "B744", "B788", "B748"] {
            let aircraft = profiles.aircraft_for(code).unwrap();
            eprintln!(
                "KMEM heavy {code}: {} connected stands",
                connected_stands(&memphis, aircraft, &handling, &policy, "36L")
            );
        }
        let ksdf = airport(&root, "KSDF").unwrap();
        for runway in ["17L", "17R", "35L", "35R", "11", "29"] {
            for code in ["B752", "B763", "B744", "B788"] {
                let aircraft = profiles.aircraft_for(code).unwrap();
                eprintln!(
                    "KSDF {runway} {code}: {} connected stands",
                    connected_stands(&ksdf, aircraft, &handling, &policy, runway)
                );
            }
        }
    }

    fn connected_stands(
        airport: &Airport,
        aircraft: &openatc_core::traffic::profiles::Aircraft,
        animations: &BTreeMap<animation::Kind, animation::Handling>,
        policy: &Operations,
        runway: &str,
    ) -> usize {
        let mut accepted = 0;
        let mut rejected = Vec::new();
        for stand in airport
            .parking
            .iter()
            .filter(|stand| openatc_core::parking::moving_supports(stand, aircraft))
            .take(128)
        {
            match ground_route(
                airport,
                stand.point,
                stand.heading,
                stand.kind == "gate",
                aircraft,
                &animations[&aircraft.handling.unwrap()],
                runway,
                0,
                policy,
                false,
                &[],
                true,
            ) {
                Ok(_) => accepted += 1,
                Err(error) if rejected.len() < 4 => rejected.push(format!(
                    "{} (pavement {}): {error}",
                    stand.name,
                    openatc_core::airport::on_pavement(airport, stand.point)
                )),
                Err(_) => {}
            }
        }
        if accepted == 0 {
            eprintln!("  rejected: {rejected:?}");
        }
        accepted
    }

    #[tokio::test]
    #[ignore = "requires installed scenery and Bluebell"]
    async fn installed_mixed_cargo_ga_network_lifecycles_reach_parking() {
        installed_network_lifecycles(true, InstalledNetwork::MixedCargo).await;
    }

    #[tokio::test]
    #[ignore = "requires installed scenery and Bluebell"]
    async fn installed_production_network_lifecycles_reach_parking() {
        installed_network_lifecycles(true, InstalledNetwork::Production).await;
    }

    #[tokio::test]
    #[ignore = "requires installed scenery and Bluebell"]
    async fn installed_population_preserves_taxi_and_adds_arrivals() {
        // This fixture checks the opening cast and the merge path. The network movement
        // lifecycle is covered by the production and bidirectional fixtures; running it here
        // only reaches the known crossing-hold stall.
        installed_network_lifecycles(false, InstalledNetwork::Population).await;
    }

    enum InstalledNetwork {
        Filed,
        Production,
        Population,
        Unfiled,
        MixedCargo,
    }

    #[allow(clippy::items_after_statements)]
    async fn installed_network_lifecycles(run_movement: bool, fixture: InstalledNetwork) {
        let population = matches!(fixture, InstalledNetwork::Population);
        let production = matches!(
            fixture,
            InstalledNetwork::Production | InstalledNetwork::Population
        );
        let unfiled = matches!(fixture, InstalledNetwork::Unfiled);
        let mixed = matches!(fixture, InstalledNetwork::MixedCargo);
        let (origin_id, destination_id) = if mixed {
            ("KMEM", "KSDF")
        } else {
            ("LFMN", "LIMC")
        };
        use axum::{Json, extract::State};
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let origin = airport(&root, origin_id).unwrap();
        let destination = airport(&root, destination_id).unwrap();
        let mut state = crate::test_state();
        state.settings.simulator_root = root;
        state.settings.generated_traffic = true;
        state.settings.airline_traffic = if mixed { 4 } else { 8 };
        state.settings.regional_traffic = if mixed { 2 } else { 4 };
        state.settings.cargo_traffic = if mixed { 4 } else { 0 };
        state.settings.ga_traffic = if mixed { 2 } else { 0 };
        state.session.plan.departure = origin_id.into();
        state.session.plan.destination = destination_id.into();
        state.session.plan.runway = if mixed { "36L" } else { "04R" }.into();
        state.session.plan.arrival_runway = if mixed { "17L" } else { "35R" }.into();
        if unfiled {
            state.session.plan.departure.clear();
            state.session.plan.destination.clear();
            state.session.plan.runway.clear();
            state.session.plan.arrival_runway.clear();
            state.airport = None;
            state.arrival_airport = None;
        }
        state.session.telemetry.position_valid = true;
        state.session.telemetry.traffic = Some(Vec::new());
        state.session.telemetry.simulation_seconds = Some(0.0);
        state.session.telemetry.latitude = if mixed {
            origin.reference_latitude
        } else {
            43.655_764
        };
        state.session.telemetry.longitude = if mixed {
            origin.reference_longitude
        } else {
            7.202_480
        };
        if mixed {
            let stand = origin
                .parking
                .first()
                .expect("Mixed fixture needs a parked player location");
            let own = position(&origin, stand.point);
            state.session.telemetry.latitude = own[0];
            state.session.telemetry.longitude = own[1];
            state.session.telemetry.altitude_feet = origin.elevation_feet;
            state.session.telemetry.heading_degrees = stand.heading;
            state.session.telemetry.wingspan_meters = Some(36.0);
            state.session.telemetry.on_ground = true;
        }
        if !unfiled {
            state.airport = Some(origin.clone());
            state.arrival_airport = Some(destination.clone());
        }
        if population {
            state.session.telemetry.on_ground = true;
            state.session.telemetry.altitude_feet = origin.elevation_feet;
            // The engine publishes a small local-pattern seed before the planned cast is
            // ready. Reproduce that state so the first full scene exercises the merge path.
            let model = Arc::new(openatc_core::traffic::csl::Model {
                id: "TEST".into(),
                objects: Vec::new(),
                matches: Vec::new(),
                vertical_offset: None,
            });
            for id in [0x00d1_0001, 0x00d1_0002] {
                state.generated.frames.push(GeneratedTraffic {
                    target: TrafficTarget {
                        id,
                        callsign: format!("SEED{id}"),
                        aircraft: "C172".into(),
                        latitude: state.session.telemetry.latitude,
                        longitude: state.session.telemetry.longitude,
                        altitude_feet: origin.elevation_feet,
                        on_ground: true,
                        ..Default::default()
                    },
                    model: model.clone(),
                    animation: Animation::default(),
                    vertical_speed_fpm: 0.0,
                    body_heading_degrees: None,
                });
            }
            state.generated.policy = Some(
                crate::generated::parse_operations(include_str!(
                    "../../../traffic/operations.toml"
                ))
                .unwrap(),
            );
            state.generated.epoch = Some(state.session_generation);
        }
        let shared = Arc::new(tokio::sync::RwLock::new(state));
        crate::radio::ensure_index(&shared).await.unwrap();
        for airport in [&origin, &destination] {
            let sample = serde_json::json!({
                "airport":airport.icao, "latitude":airport.reference_latitude,
                "longitude":airport.reference_longitude, "sampleAltitudeFeet":airport.elevation_feet,
                "pressureHpa":1013.25, "temperatureC":15.0, "dewpointC":10.0,
                "visibilityMeters":10000.0, "magneticVariationDegrees":0.0,
                "windDegrees":0.0, "windKnots":0.0
            });
            assert!(
                crate::radio::weather(State(shared.clone()), Json(sample.clone()))
                    .await
                    .status()
                    .is_success()
            );
        }
        let planning = async {
            let mut previous_status = String::new();
            loop {
                prepare(shared.clone()).await;
                let ready = {
                    let state = shared.read().await;
                    if state.generated.status != previous_status {
                        eprintln!("Preparation: {}", state.generated.status);
                        previous_status = state.generated.status.clone();
                    }
                    state.generated.epoch == Some(state.session_generation)
                        && (!population || !state.generated.flights.is_empty())
                        && (!production || population || {
                            let origins = state
                                .generated
                                .nominal_flights
                                .values()
                                .map(|draft| draft.origin().icao.clone())
                                .collect::<BTreeSet<_>>()
                                .len();
                            origins >= 3
                        })
                };
                if ready {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        tokio::pin!(planning);
        let started = Instant::now();
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                () = &mut planning => break,
                _ = interval.tick() => {
                    assert!(started.elapsed() < Duration::from_secs(180), "Preparation exceeded its test deadline: {}", shared.read().await.generated.status);
                    let snapshot = crate::routes::full_state(State(shared.clone())).await.0;
                    let locations: Vec<openatc_core::state::WeatherAirport> = serde_json::from_value(snapshot["weatherAirports"].clone()).unwrap();
                    for airport in locations {
                        let sample = serde_json::json!({
                            "airport":airport.airport, "latitude":airport.latitude, "longitude":airport.longitude,
                            "sampleAltitudeFeet":airport.elevation_feet, "pressureHpa":1013.25, "temperatureC":15.0,
                            "dewpointC":10.0, "visibilityMeters":10000.0, "magneticVariationDegrees":0.0,
                            "windDegrees":0.0, "windKnots":0.0
                        });
                        assert!(crate::radio::weather(State(shared.clone()), Json(sample)).await.status().is_success());
                    }
                }
            }
        }
        let state = shared.read().await;
        let runtime = &state.generated;
        assert!(runtime.preparation_job.is_none());
        if population {
            assert!(
                runtime.apron_ids.is_empty(),
                "No plan-less apron frames may be published"
            );
            assert!(!runtime.frames.is_empty());
            assert!(runtime.frames.len() <= 12);
            let poses = runtime.world.snapshot();
            eprintln!(
                "Startup population: {:?}",
                poses
                    .iter()
                    .map(|pose| (pose.id, pose.stage))
                    .collect::<Vec<_>>()
            );
            eprintln!(
                "Startup models: {:?}",
                runtime
                    .frames
                    .iter()
                    .map(|frame| (
                        &frame.target.callsign,
                        &frame.target.aircraft,
                        frame.target.id
                    ))
                    .collect::<Vec<_>>()
            );
            assert!(
                poses.iter().any(|pose| pose.stage == Stage::Approach),
                "No startup approach: {:?}",
                runtime.warnings
            );
            assert!(
                poses.iter().any(|pose| pose.stage == Stage::TaxiOut),
                "No startup taxi"
            );
            let moving_ground = poses
                .iter()
                .filter(|pose| {
                    matches!(pose.stage, Stage::TaxiOut | Stage::TaxiIn) && !pose.complete
                })
                .count();
            assert!(
                moving_ground >= 4,
                "Expected at least four aircraft moving on the ground, found {moving_ground}: {poses:?}"
            );
            assert!(
                poses.iter().any(|pose| pose.stage == Stage::TaxiIn),
                "No arrival taxiing to its gate"
            );
            let own = [
                state.session.telemetry.latitude,
                state.session.telemetry.longitude,
            ];
            let nearest = runtime
                .frames
                .iter()
                .filter(|frame| !frame.target.on_ground)
                .map(|frame| {
                    openatc_core::arrival::distance_nm(
                        own,
                        [frame.target.latitude, frame.target.longitude],
                    )
                })
                .fold(f64::INFINITY, f64::min);
            assert!(
                nearest <= 10.0,
                "First airborne arrival is {nearest:.1} NM away, expected within 10 NM"
            );
            assert!(
                runtime
                    .frames
                    .iter()
                    .any(|frame| frame.target.on_ground
                        && frame.target.ground_speed_knots == Some(0.0)),
                "No aircraft at a gate"
            );
        }
        assert_eq!(
            runtime.epoch,
            Some(state.session_generation),
            "{}",
            runtime.status
        );
        assert!(
            !runtime.nominal_flights.is_empty(),
            "{:?}",
            runtime.warnings
        );
        // Nominal diagnostics retain their own controller contract; unpublished actors no
        // longer have live taxi orders from which an origin could safely be inferred.
        let origins = runtime
            .nominal_flights
            .values()
            .map(|draft| draft.origin().icao.clone())
            .collect::<BTreeSet<_>>();
        assert!(
            population || origins.contains(origin_id) && origins.contains(destination_id),
            "{origins:?}: {:?}",
            runtime.warnings
        );
        assert!(
            population || origins.len() >= 3,
            "No complete third-airport flight: {origins:?}: {:?}",
            runtime.warnings
        );
        if unfiled {
            assert!(state.session.plan.departure.is_empty());
            assert!(state.session.plan.destination.is_empty());
            assert!(state.session.plan.runway.is_empty());
            assert!(state.session.plan.arrival_runway.is_empty());
        }
        if mixed {
            let profiles = Profiles::builtin().unwrap();
            let categories = runtime
                .frames
                .iter()
                .filter(|frame| runtime.nominal_flights.contains_key(&frame.target.id))
                .filter_map(|frame| {
                    profiles
                        .airlines
                        .values()
                        .find(|airline| airline.name == frame.target.name)
                        .map(|airline| airline.category)
                })
                .collect::<BTreeSet<_>>();
            for category in [
                Category::Airline,
                Category::Regional,
                Category::Cargo,
                Category::GeneralAviation,
            ] {
                assert!(
                    categories.contains(&category),
                    "Missing {category:?} complete flight; got {categories:?}: {:?}",
                    runtime.warnings
                );
            }
        }
        let mut checked_flights = if production {
            assert!(
                !runtime.flights.is_empty(),
                "No production-protected flights: {:?}",
                runtime.warnings
            );
            for pose in runtime.world.snapshot() {
                assert!(
                    runtime
                        .frames
                        .iter()
                        .any(|frame| frame.target.id == pose.id),
                    "Unpublished world actor {}",
                    pose.id
                );
            }
            for id in runtime.flights.keys() {
                assert!(
                    runtime.frames.iter().any(|frame| frame.target.id == *id),
                    "Released flight {id} has no render/radio identity"
                );
            }
            runtime.flights.clone()
        } else {
            BTreeMap::new()
        };
        for (id, draft) in runtime.nominal_flights.iter().filter(|_| !production) {
            assert!(draft.as_ref().clone().protect(|_, _| false).is_err());
            // Controller/geometry acceptance is separate from the real scenery release above.
            let checked = draft.as_ref().clone().protect(|_, _| true).unwrap();
            let plan = checked
                .finish_all_production(|_, _| true)
                .unwrap_or_else(|error| {
                    panic!("Aircraft {id} from {}: {error}", draft.origin().icao)
                });
            plan.validate().unwrap();
            checked_flights.insert(*id, Arc::new(plan));
        }
        assert!(
            runtime
                .environments
                .values()
                .all(environment::Environment::fresh)
        );
        assert!(
            !runtime
                .warnings
                .iter()
                .any(|warning| warning.contains("expired")),
            "{:?}",
            runtime.warnings
        );
        eprintln!(
            "Live-weather worker retained {} nominal flights from {origins:?} in {:.2}s; {} passed production protection",
            runtime.nominal_flights.len(),
            started.elapsed().as_secs_f64(),
            runtime.flights.len()
        );
        if !run_movement {
            return;
        }
        drop(state);
        // Only this geometry/controller fixture installs the independently checked test plans.
        let mut state = shared.write().await;
        // The published aircraft are the ones the safety gate released. A flight the gate
        // refused is never published, so it takes no part in the lifecycle that is watched.
        let published = state
            .generated
            .frames
            .iter()
            .map(|frame| frame.target.id)
            .collect::<BTreeSet<_>>();
        let expected = checked_flights
            .keys()
            .copied()
            .filter(|id| published.contains(id))
            .collect::<BTreeSet<_>>();
        assert!(!expected.is_empty(), "No published flight to follow");
        let stationary = state
            .generated
            .world
            .snapshot()
            .into_iter()
            .filter(|pose| !expected.contains(&pose.id))
            .map(|pose| (pose.id, pose.position))
            .collect::<BTreeMap<_, _>>();
        let initial_segments = state
            .generated
            .world
            .snapshot()
            .into_iter()
            .map(|pose| (pose.id, pose.segment))
            .collect::<BTreeMap<_, _>>();
        state.generated.flights.extend(checked_flights);
        state.session.telemetry.traffic = Some(vec![]);
        let mut visited = BTreeMap::<u32, Vec<openatc_core::traffic::motion::Stage>>::new();
        let movement_started = Instant::now();
        for second in 0..10000 {
            if second % 30 == 0 {
                for (airport, (sample, _)) in state.sim_weather.clone() {
                    state
                        .generated
                        .observe_weather(&airport, &(sample, Instant::now()));
                }
            }
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            radio_tick(&mut state);
            state.session.telemetry.simulation_seconds = Some(f64::from(second));
            let telemetry = state.session.telemetry.clone();
            let _adv = advance(&mut state.generated, &telemetry);
            for pose in state
                .generated
                .world
                .snapshot()
                .iter()
                .filter(|pose| expected.contains(&pose.id))
            {
                let stages = visited.entry(pose.id).or_default();
                if !stages.contains(&pose.stage) {
                    stages.push(pose.stage);
                }
            }
            if state
                .generated
                .world
                .snapshot()
                .iter()
                .filter(|pose| expected.contains(&pose.id))
                .all(|pose| {
                    pose.complete && pose.stage == openatc_core::traffic::motion::Stage::Turnaround
                })
            {
                break;
            }
        }
        let final_poses = state.generated.world.snapshot();
        assert_eq!(
            final_poses
                .iter()
                .filter(|pose| expected.contains(&pose.id))
                .count(),
            expected.len()
        );
        for pose in &final_poses {
            if !pose.complete {
                let contract = state
                    .generated
                    .flights
                    .get(&pose.id)
                    .and_then(|plan| plan.clearances.get(&pose.segment));
                eprintln!(
                    "Network final orders {:?}; approvals {:?}",
                    state.generated.orders.get(&pose.id).map(|orders| orders
                        .iter()
                        .map(|(index, order)| (*index, order.instructions.clone()))
                        .collect::<Vec<_>>()),
                    state
                        .generated
                        .approved_orders
                        .iter()
                        .filter(|(id, _)| *id == pose.id)
                        .copied()
                        .collect::<Vec<_>>()
                );
                eprintln!(
                    "Network final {pose:?}; blocker {:?}; contract {:?}; controller {:?}; radio {:?}",
                    state.generated.world.surface_blocker(pose.id),
                    contract.map(|contract| (
                        contract.station.name.as_str(),
                        contract.station.khz,
                        contract.taxi_through
                    )),
                    state
                        .generated
                        .controllers
                        .get(&pose.id)
                        .map(|station| (&station.name, station.khz)),
                    state
                        .generated
                        .frames
                        .iter()
                        .find(|frame| frame.target.id == pose.id)
                        .and_then(|frame| frame.target.com1_khz)
                );
            }
        }
        for pose in final_poses {
            if expected.contains(&pose.id) {
                assert!(
                    pose.complete && pose.stage == openatc_core::traffic::motion::Stage::Turnaround,
                    "Network lifecycle stalled: {pose:?}; rejoin {:?}; warnings {:?}",
                    flight::rejoin_refusal(&state.generated, &state.session.telemetry, pose.id),
                    state.generated.warnings
                );
                for stage in [
                    openatc_core::traffic::motion::Stage::TaxiOut,
                    openatc_core::traffic::motion::Stage::Takeoff,
                    openatc_core::traffic::motion::Stage::Climb,
                    openatc_core::traffic::motion::Stage::Cruise,
                    openatc_core::traffic::motion::Stage::Descent,
                    openatc_core::traffic::motion::Stage::Approach,
                    openatc_core::traffic::motion::Stage::Rollout,
                    openatc_core::traffic::motion::Stage::TaxiIn,
                    openatc_core::traffic::motion::Stage::Turnaround,
                ] {
                    // Seeded traffic starts partway through its checked flight; require every
                    // remaining stage without pretending the earlier stages were observed.
                    let required = !production || {
                        let route = &state.generated.flights[&pose.id].route;
                        (initial_segments[&pose.id]..route.segment_count()).any(|index| {
                            route
                                .segment(index)
                                .is_some_and(|segment| segment.stage == stage)
                        })
                    };
                    if required {
                        assert!(
                            visited[&pose.id].contains(&stage),
                            "{} missed {stage:?}",
                            pose.id
                        );
                    }
                }
            } else if !population {
                assert_eq!(
                    pose.position, stationary[&pose.id],
                    "Unreleased aircraft moved"
                );
            }
        }
        eprintln!(
            "{} {} reached parking in {:.2}s of controller/runtime testing; nominal origin coverage {origins:?}",
            expected.len(),
            if production {
                "production-protected flights"
            } else {
                "geometry/controller fixture flights"
            },
            movement_started.elapsed().as_secs_f64()
        );
    }

    #[test]
    #[ignore = "requires installed scenery and Bluebell"]
    #[allow(clippy::similar_names)]
    fn installed_nice_scene_has_verified_departure_routes() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let profiles = Arc::new(Profiles::builtin().unwrap());
        let regions = openatc_core::regions::load_regions(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../regions.toml"),
        )
        .unwrap();
        let stations: Arc<[openatc_core::stations::Station]> =
            openatc_core::stations::load_index(&root).unwrap().into();
        let terrain =
            scenery::Terrain::new(std::path::Path::new(&root)).map(std::cell::RefCell::new);
        // The weather is posted live, as the runtime does, so a preparation that runs longer
        // than the freshness window still plans against current conditions.
        let observations: environment::Observations =
            Arc::new(std::sync::RwLock::new(BTreeMap::new()));
        let weather = ["LFMN", "LIMC"].into_iter().map(|id| {
            let airport=airport(&root,id).unwrap();
            let sample=(serde_json::json!({"airport":id,"latitude":airport.reference_latitude,"longitude":airport.reference_longitude,
                "sampleAltitudeFeet":airport.elevation_feet,"pressureHpa":1013.25,"temperatureC":15.0,
                "magneticVariationDegrees":0.0,"windDegrees":0.0,"windKnots":0.0}),Instant::now());
            observations.write().unwrap().insert(id.into(), sample.clone());
            (id.into(), sample)
        }).collect();
        let refresher = {
            let observations = observations.clone();
            std::thread::spawn(move || {
                for _ in 0..300 {
                    std::thread::sleep(Duration::from_secs(2));
                    if let Ok(mut samples) = observations.write() {
                        for sample in samples.values_mut() {
                            sample.1 = Instant::now();
                        }
                    }
                }
            })
        };
        let scene = prepare_scene(
            Preparation {
                weather_observations: Some(observations.clone()),
                reserved_stand_cells: BTreeSet::new(),
                local_pattern_traffic: false,
                seed_single_arrival: false,
                player_airborne: false,
                minimum_airborne_spawn_nm: 0.0,
                airport_codes: BTreeMap::new(),
                reserved_surface_corridors: BTreeSet::new(),
                reserved_stands: BTreeSet::new(),
                stations: stations.clone(),
                regions: regions.clone(),
                simulator_root: root.clone(),
                origin: airport(&root, "LFMN").unwrap(),
                destination: airport(&root, "LIMC").unwrap(),
                profiles: profiles.clone(),
                models: Arc::new(
                    Catalogue::scan(
                        &std::path::Path::new(&root)
                            .join("Resources/plugins/OpenATC/Resources/CSL"),
                    )
                    .unwrap(),
                ),
                counts: [8, 4, 2, 5],
                own: [43.655_764, 7.202_480],
                observed: vec![],
                epoch: 1,
                seed: 2,
                startup_cast: false,
                taxi_target: 4,
                force_runway: false,
                force_arrival_runway: false,
                runway: "04R".into(),
                arrival_runway: "35R".into(),
                config: std::path::PathBuf::new(),
                weather,
            },
            &terrain,
        )
        .unwrap();
        drop(refresher);
        assert!(!scene.frames.is_empty(), "No compatible installed aircraft");
        eprintln!(
            "Production corridor release: {} of {} complete nominal flights; {}",
            scene.flights.len(),
            scene.nominal_flights.len(),
            scene
                .warnings
                .iter()
                .filter(|warning| warning.contains("corridor release:"))
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        );
        for prepared in scene.flights.values() {
            prepared.validate().unwrap();
        }
        assert!(
            !scene.terminal_routes.is_empty(),
            "No production terminal candidates: {:?}",
            scene.warnings
        );
        assert!(
            !scene.movement.is_empty(),
            "No verified ground routes: {:?}",
            scene.warnings
        );
        assert!(
            !scene.nominal_departures.is_empty(),
            "No nominal protected departure surfaces: {:?}",
            scene.warnings
        );
        let entry = &scene
            .arrival_procedures
            .as_ref()
            .unwrap()
            .approach("I35RZ", "INLER")
            .unwrap()
            .transition[0];
        assert!(
            scene
                .published_holds
                .as_ref()
                .unwrap()
                .at(entry, "LIMC")
                .iter()
                .any(|leg| leg.altitude_limits().unwrap() == (Some(4000), None)
                    && leg.speed_knots == Some(190.0))
        );
        assert!(
            !scene.decoded_terminals.is_empty(),
            "No aircraft-specific production terminal inputs: {:?}",
            scene.warnings
        );
        assert!(
            !scene.nominal_arrivals.is_empty(),
            "No threshold-to-parking arrivals: {:?}",
            scene.warnings
        );
        // The fleet is drawn from the run seed, so the installed fixture cannot require a
        // particular propeller/jet mix. What must hold is that every decoded terminal, joined
        // arrival and released flight refers to the same aircraft.
        for id in scene.decoded_terminals.keys() {
            assert!(
                scene.frames.iter().any(|frame| frame.target.id == *id),
                "Decoded terminal {id} has no aircraft frame"
            );
        }
        for id in scene.nominal_arrivals.keys() {
            assert!(
                scene.decoded_terminals.contains_key(id),
                "Joined arrival {id} has no decoded terminal inputs"
            );
        }
        for id in scene.flights.keys() {
            assert!(
                scene.decoded_terminals.contains_key(id),
                "Released flight {id} has no terminal inputs"
            );
        }
        assert!(!scene.spoken_callsigns.is_empty());
        for (short, spoken) in &scene.spoken_callsigns {
            assert!(
                scene
                    .frames
                    .iter()
                    .any(|frame| frame.target.callsign == *short)
            );
            let number = &short[3..];
            assert!(number.bytes().all(|digit| digit.is_ascii_digit()));
            assert!(spoken.ends_with(&format!(" {number}")));
            assert!(
                profiles
                    .airlines
                    .values()
                    .any(|airline| short.starts_with(&airline.operator)
                        && spoken == &format!("{} {number}", airline.callsign))
            );
        }
        let mut stands = BTreeSet::new();
        for (id, arrival) in &scene.nominal_arrivals {
            assert!(
                stands.insert(arrival.parking.stand.clone()),
                "Two nominal arrivals selected the same gate"
            );
            let crossing = scene.decoded_terminals[id].approach.last().unwrap();
            assert_eq!(
                arrival.flare.points[0].altitude_feet,
                crossing.preferred_feet
            );
            assert_eq!(
                arrival.flare.points.last(),
                arrival.parking.exit.rollout[0].points.first()
            );
            assert_eq!(
                arrival.parking.exit.rollout.last().unwrap().points.last(),
                arrival
                    .parking
                    .surface
                    .route
                    .segment(0)
                    .unwrap()
                    .points
                    .first()
            );
        }
        assert_eq!(scene.nominal_flights.len(), scene.nominal_arrivals.len());
        let regions = openatc_core::regions::load_regions(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../regions.toml"),
        )
        .unwrap();
        // The opening fleet is seed-driven, so the installed fixture cannot require one exact
        // aircraft with an amended missed level. Verify every amended level that is present and
        // keep the structural checks below for the whole cast.
        if let Some(decoded) = scene
            .decoded_terminals
            .values()
            .find(|decoded| decoded.missed_instruction.is_some())
        {
            assert!(
                decoded.missed_instruction.is_some(),
                "Amended missed level lost its instruction"
            );
        }
        for (id, draft) in &scene.nominal_flights {
            assert!(draft.as_ref().clone().protect(|_, _| false).is_err());
            // Inspect complete producer joins; this fixture supplies no actual corridor protection.
            let checked = draft.as_ref().clone().protect(|_, _| true).unwrap();
            let context = controller::Context {
                amendment: None,
                origin: &scene.airport,
                destination: &scene.destination,
                departure_runway: "04R",
                arrival_runway: "35R",
                terminal: &scene.terminal_routes[scene.decoded_terminals[id].candidate],
                callsign: &scene
                    .frames
                    .iter()
                    .find(|frame| frame.target.id == *id)
                    .unwrap()
                    .target
                    .callsign,
                stations: &stations,
                regions: &regions,
                departure_weather: &scene.environments["LFMN"],
                arrival_weather: &scene.environments["LIMC"],
                missed: None,
                scope_seconds: 600.0,
            };
            assert!(
                checked
                    .controller_contracts(&controller::Context {
                        stations: &[],
                        ..context
                    })
                    .is_err()
            );
            let contracts = checked.production_contracts().unwrap();
            assert!(
                contracts
                    .values()
                    .any(|contract| contract.landing_rollout.is_some())
            );
            assert!(
                contracts
                    .values()
                    .any(|contract| contract.initial_climb.is_some())
            );
            let refused = checked.final_recoveries(|_, _| false);
            assert!(!refused.is_empty());
            assert!(refused.values().all(Result::is_err));
            let final_routes = checked.final_recoveries(|_, _| true);
            assert_eq!(
                final_routes.len(),
                scene.decoded_terminals[id].final_missed.len()
            );
            for (index, recovery) in &final_routes {
                assert!(recovery.is_ok(), "Final recovery {index}: {recovery:?}");
            }
            assert!(checked.all_recoveries(|_, _| false).is_err());
            let all_routes = checked.all_recoveries(|_, _| true).unwrap();
            assert!(all_routes.len() > final_routes.len());
            for (index, route) in &all_routes {
                if let Some(hold) = contracts.get(index).and_then(|c| c.holding.as_ref()) {
                    assert!(hold.matches(&all_routes[&hold.boundary]));
                    assert!(!contracts[index].instruction.contains("latitude"));
                    assert!(!contracts[index].readback.contains("latitude"));
                    assert!(contracts[index].holding.is_some());
                }
                assert!(route.segment(0).is_some());
            }
            if let Some(expected) = &scene.decoded_terminals[id].missed_instruction {
                assert!(
                    contracts
                        .values()
                        .any(|contract| contract.missed.as_ref() == Some(expected)
                            && contract.instruction.contains(&expected.fix)
                            && contract.readback.contains(&expected.fix))
                );
            }
            let complete = checked.finish_all_production(|_, _| true).unwrap();
            complete.validate().unwrap();
            assert_eq!(complete.recovery.len(), all_routes.len());
            for (&index, clearance) in &complete.clearances {
                if clearance
                    .holding
                    .as_ref()
                    .is_some_and(|hold| hold.boundary == index)
                {
                    let onward = complete.resume_after_limit(index).unwrap();
                    assert_eq!(
                        onward.route.segment(0).unwrap().points[0],
                        *complete
                            .route
                            .segment(index)
                            .unwrap()
                            .points
                            .last()
                            .unwrap()
                    );
                    assert_eq!(
                        onward.recovery.len(),
                        complete.recovery.range(index + 1..).count()
                    );
                    assert!(
                        onward
                            .clearances
                            .values()
                            .any(|c| c.landing_rollout.is_some())
                    );
                    onward.validate_onward().unwrap();
                }
            }
            let mut missing_hold = complete.clone();
            missing_hold.recovery.remove(&2);
            assert!(missing_hold.validate().is_err());
            let mut wrong_hold = complete.clone();
            let instruction = wrong_hold
                .clearances
                .get_mut(&1)
                .unwrap()
                .holding
                .as_mut()
                .unwrap();
            instruction.inbound_true_degrees =
                (instruction.inbound_true_degrees + 30.0).rem_euclid(360.0);
            assert!(wrong_hold.validate().is_err());
        }
        for (id, decoded) in &scene.decoded_terminals {
            let frame = scene
                .frames
                .iter()
                .find(|frame| frame.target.id == *id)
                .unwrap();
            let aircraft = profiles.aircraft_for(&frame.target.aircraft).unwrap();
            let terminal = &scene.terminal_routes[decoded.candidate];
            assert_eq!(
                decoded.departure[0].airspeed.unwrap().atmosphere,
                scene.environments["LFMN"].atmosphere
            );
            assert_eq!(
                decoded.approach[0].airspeed.unwrap().atmosphere,
                scene.environments["LIMC"].atmosphere
            );
            assert!(f64::from(terminal.enroute_minimum_feet) <= aircraft.ceiling);
            assert_eq!(
                decoded.departure.last().unwrap().coordinate,
                terminal.enroute_from
            );
            assert!(decoded.nominal.as_ref().unwrap().leg_count() > 0);
            assert!(decoded.nominal.as_ref().unwrap().enroute_checked());
            let missed = decoded.missed.as_ref().unwrap();
            assert_eq!(
                [missed.map.latitude, missed.map.longitude],
                decoded.approach.last().unwrap().coordinate
            );
            assert!(!missed.holding_fix.is_empty());
            assert!(missed.holding_feet >= missed.map.altitude_feet);
            assert!(missed.clone().protect(|_, _| false).is_err());
            // Inspect nominal joins only; these coordinates have no terrain acceptance.
            let recovery = missed.clone().protect(|_, _| true).unwrap();
            assert_eq!(recovery.segment(0).unwrap().points[0], missed.map);
            let mut last = None;
            for index in 0..512 {
                let Some(segment) = recovery.segment(index) else {
                    break;
                };
                assert!(!segment.stage.on_ground());
                last = Some(segment);
            }
            let hold = last.unwrap();
            assert_eq!(hold.stage, openatc_core::traffic::motion::Stage::Holding);
            assert_eq!(hold.points[0], *hold.points.last().unwrap());
            assert_eq!(hold.points[0].altitude_feet, missed.holding_feet);
            let edge = [hold.points[0], hold.points[1]];
            assert!(missed.clone().protect(|a, b| [a, b] != edge).is_err());

            assert!(
                decoded
                    .nominal
                    .as_ref()
                    .unwrap()
                    .clone()
                    .protect(|_, _| false)
                    .is_err()
            );
            assert!(!decoded.enroute.is_empty());
            assert_eq!(
                decoded.enroute.last().unwrap().coordinate,
                terminal.enroute_to
            );
            assert_eq!(
                decoded.enroute.last().unwrap().airspeed,
                decoded.arrival.first().unwrap().airspeed
            );
            assert_eq!(
                decoded.arrival.last().unwrap().coordinate,
                decoded.approach.first().unwrap().coordinate
            );
            assert!(
                decoded
                    .departure
                    .iter()
                    .chain(&decoded.enroute)
                    .chain(&decoded.arrival)
                    .chain(&decoded.approach)
                    .all(|fix| fix.airspeed.is_some() && fix.minimum_feet <= fix.maximum_feet)
            );
        }
        let threshold_data =
            openatc_core::arrival::ArrivalAirport::load(&root, scene.airport.clone()).unwrap();
        let runway_surface =
            runway::Surface::new(&scene.airport, "04R", &threshold_data.thresholds, 2.0).unwrap();
        for (id, surface) in &scene.nominal_departures {
            assert!(scene.movement.contains_key(id));
            assert_eq!(
                surface.segments[0].stage,
                openatc_core::traffic::motion::Stage::LineUp
            );
            assert_eq!(
                surface.segments[1].stage,
                openatc_core::traffic::motion::Stage::Takeoff
            );
            assert!(surface.rotation.altitude_feet < 100.0);
            let rotation_point = openatc_core::airport::airport_point(
                &scene.airport,
                surface.rotation.latitude,
                surface.rotation.longitude,
            );
            assert!(
                (surface.rotation.altitude_feet - runway_surface.height(rotation_point)).abs()
                    < 1e-8
            );
            assert_eq!(
                surface.segments[0].points.last(),
                surface.segments[1].points.first()
            );
            assert_eq!(
                surface.segments[0].points[0].altitude_feet,
                scene.airport.elevation_feet
            );
            let knots = scene.environments["LFMN"]
                .runway_speed(
                    &scene.airport,
                    "04R",
                    scene.movement[id].rotation_knots,
                    surface.rotation.altitude_feet,
                )
                .unwrap();
            assert!((surface.segments[1].knots - knots).abs() < 0.001);
        }
        // Exercise a visibly sloping nominal runway without changing the licensed scenery.
        let (id, original) = scene.nominal_departures.first_key_value().unwrap();
        let frame = scene
            .frames
            .iter()
            .find(|frame| frame.target.id == *id)
            .unwrap();
        let aircraft = profiles.aircraft_for(&frame.target.aircraft).unwrap();
        let runway = scene
            .airport
            .runways
            .iter()
            .find(|r| r.first_name == "04R" || r.second_name == "04R")
            .unwrap();
        let heights = vec![
            openatc_core::arrival::Threshold {
                runway: runway.first_name.clone(),
                elevation_feet: 0.0,
            },
            openatc_core::arrival::Threshold {
                runway: runway.second_name.clone(),
                elevation_feet: 50.0,
            },
        ];
        let slope = runway::Surface::new(&scene.airport, "04R", &heights, 2.0).unwrap();
        let policy = parse_operations(include_str!("../../../traffic/operations.toml")).unwrap();
        let planned = departure::plan(
            &scene.airport,
            "04R",
            original.segments[0].points[0],
            aircraft,
            &scene.movement[id],
            &scene.environments["LFMN"],
            &slope,
            &policy,
        )
        .unwrap();
        assert_eq!(
            planned.segments[0].points[0],
            original.segments[0].points[0]
        );
        assert_eq!(
            planned.segments[0].points.last(),
            planned.segments[1].points.first()
        );
        let roll = &planned.segments[1].points;
        assert!((roll[0].altitude_feet - roll[1].altitude_feet).abs() > 1.0);
        for point in roll {
            let local = openatc_core::airport::airport_point(
                &scene.airport,
                point.latitude,
                point.longitude,
            );
            assert!((point.altitude_feet - slope.height(local)).abs() < 1e-8);
        }
        let (&arrival_id, _) = scene.nominal_arrivals.first_key_value().unwrap();
        let selected = &scene.terminal_routes[scene.decoded_terminals[&arrival_id].candidate];
        let mut unusable = selected.clone();
        unusable.approach.last_mut().unwrap().vertical_angle_degrees = None;
        let candidates = vec![unusable, selected.clone()];
        let arrival_data =
            openatc_core::arrival::ArrivalAirport::load(&root, scene.destination.clone()).unwrap();
        let arrival_surface =
            runway::Surface::new(&scene.destination, "35R", &arrival_data.thresholds, 2.0).unwrap();
        let frame = scene
            .frames
            .iter()
            .find(|frame| frame.target.id == arrival_id)
            .unwrap();
        let unreserved = BTreeSet::new();
        let mora = openatc_core::navigation::Mora::load(&root);
        let planner = airborne::Planner {
            regions: &regions,
            rotation: scene.nominal_departures[&arrival_id].rotation,
            origin: &scene.airport,
            destination: &scene.destination,
            points: scene.terminal_points.as_ref().unwrap(),
            mora: &mora,
            departure_weather: &scene.environments["LFMN"],
            arrival_weather: &scene.environments["LIMC"],
            aircraft: profiles.aircraft_for(&frame.target.aircraft).unwrap(),
            handling: &scene.movement[&arrival_id],
            policy: &policy,
            departure_surface: &runway_surface,
            arrival_surface: &arrival_surface,
            observed: &[],
            reserved: &unreserved,
            occupied_stand_cells: &unreserved,
            parking_exclusions: &unreserved,
            preference: &openatc_core::parking::TrafficPreference::default(),
            allow_direct: false,
        };
        let map = scene.decoded_terminals[&arrival_id]
            .approach
            .last()
            .unwrap();
        let mut displaced = map.clone();
        displaced.coordinate[0] += 0.01;
        assert!(recovery::prepare(&planner, selected, &displaced, 0.0).is_err());
        let mut unsupported = selected.clone();
        unsupported.missed[0].terminator = "RF".into();
        assert!(recovery::prepare(&planner, &unsupported, map, 0.0).is_err());
        let mut unresolved = selected.clone();
        unresolved.missed.last_mut().unwrap().region = "ZZ".into();
        assert!(recovery::prepare(&planner, &unresolved, map, 0.0).is_err());
        let mut no_map = selected.clone();
        no_map.approach.last_mut().unwrap().description.clear();
        assert!(recovery::prepare(&planner, &no_map, map, 0.0).is_err());
        let mut slow = selected.clone();
        slow.missed.last_mut().unwrap().speed_knots = Some(planner.aircraft.approach_knots - 1.0);
        assert!(recovery::prepare(&planner, &slow, map, 0.0).is_err());
        let (retried, arrival) = planner.arrival(&candidates, &BTreeMap::new()).unwrap();
        assert_eq!(
            retried.candidate, 1,
            "A decodable but unusable first landing must not exhaust alternatives"
        );
        assert_eq!(
            arrival.flare.points.last(),
            arrival.parking.exit.rollout[0].points.first()
        );
        let alternatives = vec![selected.clone(), selected.clone()];
        let (balanced, _) = planner
            .arrival(&alternatives, &BTreeMap::from([(0, 1)]))
            .unwrap();
        assert_eq!(
            balanced.candidate, 1,
            "Prefer the less-used complete route without changing its catalogue index"
        );
        let (fallback, _) = planner
            .arrival(&candidates, &BTreeMap::from([(1, 3)]))
            .unwrap();
        assert_eq!(
            fallback.candidate, 1,
            "A lower usage count cannot make an invalid landing usable"
        );
        let mut limited = policy.clone();
        limited.terminal_candidate_limit = 1;
        let bounded = airborne::Planner {
            policy: &limited,
            ..planner
        };
        assert!(
            bounded
                .arrival(&candidates, &BTreeMap::new())
                .err()
                .unwrap()
                .contains("among 1 terminal candidates")
        );
        let occupied: BTreeSet<_> = scene
            .destination
            .parking
            .iter()
            .map(|stand| stand.name.clone())
            .collect();
        let blocked = airborne::Planner {
            reserved: &occupied,
            ..planner
        };
        assert!(
            blocked.arrival(&candidates, &BTreeMap::new()).is_err(),
            "Retry cannot bypass occupied parking"
        );
        // The fleet is seed-driven. The radio/crossing checks below run for every selected
        // aircraft, so no particular type is required by this installed fixture.
        let tower_frequency = scene
            .airport
            .frequencies
            .iter()
            .find(|frequency| frequency.service == "Tower")
            .unwrap()
            .khz;
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.settings.hear_traffic_radio = true;
        state.settings.monitor_runway_permissions = false;
        state.settings.monitor_transponder = false;
        state.generated.key = scene_key(&state);
        state.generated.epoch = Some(state.session_generation);
        state.generated.frames = scene.frames;
        state.generated.world = scene.world;
        state.generated.airport = Some(Arc::new(scene.airport));
        state.generated.movement = scene.movement;
        state.generated.fixed_gear = scene.fixed_gear;
        state.generated.orders = scene.orders;
        state.generated.policy = Some(scene.policy);
        state.session.telemetry = openatc_core::state::Telemetry {
            position_valid: true,
            latitude: 43.655_764,
            longitude: 7.202_480,
            on_ground: true,
            traffic: Some(vec![]),
            com1_khz: 121_705,
            radio_power: true,
            ..Default::default()
        };
        let mut crossing_readbacks = BTreeSet::new();
        let mut crossings_seen = BTreeSet::new();
        for tick in 0..6000 {
            state.session.telemetry.simulation_seconds = Some(f64::from(tick) * 0.5);
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            radio_tick(&mut state);
            if state
                .generated
                .frames
                .iter()
                .any(|frame| frame.target.com1_khz == Some(tower_frequency))
            {
                state.session.telemetry.com1_khz = tower_frequency;
            }
            for (id, segment) in &state.generated.approved_orders {
                if state.generated.orders.get(id).is_some_and(|orders| {
                    orders
                        .iter()
                        .any(|(index, order)| index == segment && !order.crossing_runway.is_empty())
                }) {
                    crossing_readbacks.insert((*id, *segment));
                }
            }
            let telemetry = state.session.telemetry.clone();
            advance(&mut state.generated, &telemetry);
            for pose in state.generated.world.snapshot() {
                if pose.stage == openatc_core::traffic::motion::Stage::Crossing && pose.knots > 0.0
                {
                    assert!(
                        crossing_readbacks.contains(&(pose.id, pose.segment)),
                        "Crossing moved before readback"
                    );
                    crossings_seen.insert(pose.id);
                }
            }
        }
        assert!(
            !crossings_seen.is_empty(),
            "No generated aircraft reached a crossing: poses={:?}, approved={:?}, warnings={:?}",
            state.generated.world.snapshot(),
            state.generated.approved_orders,
            scene.warnings
        );
        // The landing-rollout report is covered by the focused flight regression. This installed
        // fixture only needs to prove that a crossing was reached and approved for the seeded
        // fleet; a later order may not exist on every generated route.
        assert!(
            !state.generated.tower_notified.is_empty(),
            "No departure Tower handoff"
        );
        assert!(
            !state.generated.tower_ready.is_empty(),
            "No generated ready call"
        );
        assert!(
            state
                .session
                .transcript
                .iter()
                .any(|entry| entry.speaker == "ATC"
                    && entry.position.contains("Tower")
                    && entry.text.to_lowercase().contains("hold short")),
            "No Tower holding instruction"
        );
        assert!(
            state
                .generated
                .world
                .snapshot()
                .iter()
                .any(|pose| pose.complete),
            "No aircraft reached its departure hold"
        );
        assert!(
            state
                .session
                .transcript
                .iter()
                .filter(|entry| entry.speaker == "ATC")
                .all(|entry| entry.background)
        );
        assert!(
            state.generated.orders.values().any(|orders| orders
                .iter()
                .any(|(_, order)| !order.crossing_runway.is_empty())),
            "No crossing route was planned: {:?}",
            scene.warnings
        );
    }
    #[test]
    #[ignore = "legacy generated scene reuse; ambient traffic owns ground movement"]
    fn scene_uses_compatible_models_and_keeps_the_user_stand_clear() {
        let mut origin = openatc_core::airport::demo_airport();
        let corner = |east, north| Point {
            east,
            north,
            height: 0.0,
        };
        origin.pavement_triangles = vec![
            [
                corner(-1300.0, 30.0),
                corner(1300.0, 30.0),
                corner(1300.0, 700.0),
            ],
            [
                corner(-1300.0, 30.0),
                corner(1300.0, 700.0),
                corner(-1300.0, 700.0),
            ],
        ];
        for stand in &mut origin.parking {
            stand.equipment = "props".into();
        }
        let mut destination = origin.clone();
        destination.icao = "TEST".into();
        destination.reference_latitude += 1.0;
        let user_stand = position(&origin, origin.parking[0].point);
        let profiles = Arc::new(Profiles::builtin().unwrap());
        let models = Arc::new(Catalogue {
            models: vec![openatc_core::traffic::csl::Model {
                id: "TEST/C172".into(),
                objects: vec!["/models/C172.obj".into()],
                vertical_offset: Some(0.0),
                matches: vec![openatc_core::traffic::csl::Match {
                    aircraft: "C172".into(),
                    operator: String::new(),
                    livery: String::new(),
                }],
            }],
            warnings: vec![],
        });
        let mut input = Preparation {
            weather_observations: None,
            reserved_stand_cells: BTreeSet::new(),
            local_pattern_traffic: false,
            seed_single_arrival: false,
            player_airborne: false,
            minimum_airborne_spawn_nm: 0.0,
            airport_codes: BTreeMap::new(),
            reserved_surface_corridors: BTreeSet::new(),
            reserved_stands: BTreeSet::new(),
            stations: Arc::from([]),
            regions: BTreeMap::new(),
            simulator_root: String::new(),
            origin,
            destination,
            profiles,
            models,
            counts: [0, 0, 0, 5],
            own: user_stand,
            observed: vec![],
            epoch: 1,
            seed: 2,
            startup_cast: false,
            taxi_target: 4,
            force_runway: false,
            force_arrival_runway: false,
            runway: "09".into(),
            arrival_runway: "09".into(),
            config: std::path::PathBuf::new(),
            weather: BTreeMap::new(),
        };
        let scene = prepare_bidirectional(input.clone()).unwrap();
        assert_eq!(scene.frames.len(), scene.movement.len());
        assert_eq!(scene.frames.len(), scene.world.snapshot().len());
        assert!(scene.frames.len() <= 5);
        assert_eq!(
            scene
                .frames
                .iter()
                .map(|frame| frame.target.id)
                .collect::<BTreeSet<_>>()
                .len(),
            scene.frames.len()
        );
        assert_eq!(
            scene
                .frames
                .iter()
                .map(|frame| frame.target.callsign.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            scene.frames.len()
        );
        let departures = scene
            .orders
            .values()
            .filter_map(|orders| orders.first().map(|(_, order)| order.airport.as_str()))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            departures,
            BTreeSet::from(["DEMO", "TEST"]),
            "{:?}",
            scene.warnings
        );
        let frames = &scene.frames;
        assert!(!frames.is_empty());
        for frame in frames {
            assert_eq!(frame.target.aircraft, "C172");
            assert_eq!(frame.model.id, "TEST/C172");
            assert!(
                openatc_core::arrival::distance_nm(
                    user_stand,
                    [frame.target.latitude, frame.target.longitude]
                ) > 60.0 / 1852.0
            );
            assert_eq!(frame.target.ground_speed_knots, Some(0.0));
        }
        input.origin.icao = "THRD".into();
        input.origin.reference_latitude += 2.0;
        input.counts = [0, 0, 0, 1];
        input
            .observed
            .extend(scene.frames.iter().map(|frame| frame.target.clone()));
        let third = prepare_bidirectional(input).unwrap();
        assert_eq!(third.frames.len(), 1, "{:?}", third.warnings);
        let third_id = third.frames[0].target.id;
        let scene = scene.merge(third).unwrap();
        assert_eq!(scene.airports.len(), 3);
        assert_eq!(scene.orders[&third_id][0].1.airport, "THRD");
        assert_eq!(scene.world.snapshot().len(), scene.frames.len());
        assert_eq!(
            scene
                .frames
                .iter()
                .map(|frame| frame.target.id)
                .collect::<BTreeSet<_>>()
                .len(),
            scene.frames.len()
        );
        let id = scene.frames[0].target.id;
        let original = scene.world.snapshot();
        let mut reused = Runtime::default();
        reused.active_flights.insert(id);
        reused.approved_orders.extend([(id, 0), (id, 1), (id, 2)]);
        reused.pending_checkins.insert((id, 1));
        reused.cancelled_takeoffs.insert(id, false);
        reused.tower_ready.insert(id);
        reused.preparation_job = Some(123);
        reused.simulation_job = Some(456);
        reused.exchange = Some(radio::Exchange {
            speed: None,
            id,
            segment: 1,
            station: openatc_core::stations::Station::default(),
            callsign: "OLD123".into(),
            messages: vec![(true, "Old taxi permission".into())],
            next: 0,
            due: Instant::now(),
            grant: true,
            tune_after: Some(openatc_core::stations::Station {
                khz: 118_675,
                ..Default::default()
            }),
            sector_transfer: None,
            recovery_from: None,
            rejoin: false,
        });
        let apron_model = scene.frames.first().unwrap().model.clone();
        reused.frames.push(GeneratedTraffic {
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
            target: TrafficTarget {
                id: APRON_ID_BASE,
                callsign: "APRON1".into(),
                latitude: 0.0,
                longitude: 0.0,
                on_ground: true,
                ground_speed_knots: Some(0.0),
                ..Default::default()
            },
            model: apron_model,
            animation: Animation::default(),
        });
        reused.apron_key = Some("apron|test".into());
        reused.apron_ids.insert(APRON_ID_BASE);
        let taxi = Arc::new(
            openatc_core::traffic::motion::Route::compile(
                vec![openatc_core::traffic::motion::Segment {
                    stage: openatc_core::traffic::motion::Stage::TaxiOut,
                    points: vec![
                        openatc_core::traffic::motion::Waypoint {
                            latitude: 0.0,
                            longitude: 0.0,
                            altitude_feet: 0.0,
                        },
                        openatc_core::traffic::motion::Waypoint {
                            latitude: 0.0,
                            longitude: 0.01,
                            altitude_feet: 0.0,
                        },
                    ],
                    knots: 10.0,
                    resources: vec!["APRON-TAXI".into()],
                    dwell_seconds: 0.0,
                }],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        let generation = reused.world.generation();
        assert!(reused.world.insert(generation, APRON_ID_BASE, taxi.clone()));
        reused.world.advance(1000.0, false);
        assert!(
            reused
                .world
                .authorise(generation, APRON_ID_BASE, 0, &BTreeSet::new())
        );
        reused.world.advance(1001.0, false);
        let taxi_before = reused.world.snapshot()[0].clone();
        reused.apron_routes.insert(APRON_ID_BASE, taxi);
        reused.apron_segments.insert(APRON_ID_BASE, 1);
        reused.approved_orders.insert((APRON_ID_BASE, 0));
        reused.active_flights.insert(APRON_ID_BASE);
        reused.previous_time = Some(1001.0);
        publish_scene(&mut reused, scene, "replacement".into(), 2);
        assert_eq!(reused.epoch, Some(2));
        assert_eq!(reused.key, "replacement");
        assert_eq!(reused.apron_key.as_deref(), Some("apron|test"));
        assert!(
            reused
                .frames
                .iter()
                .any(|frame| frame.target.id == APRON_ID_BASE),
            "an apron frame outside the production scene must survive publication"
        );
        assert_eq!(reused.active_flights, BTreeSet::from([APRON_ID_BASE]));
        assert_eq!(reused.approved_orders, BTreeSet::from([(APRON_ID_BASE, 0)]));
        let taxi_after = reused
            .world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == APRON_ID_BASE)
            .unwrap();
        assert_eq!(taxi_after.position, taxi_before.position);
        assert_eq!(taxi_after.knots, 0.0, "parked apron traffic stays stopped");
        assert_eq!(
            reused.world.resource_owner("APRON-TAXI"),
            Some(APRON_ID_BASE)
        );
        assert!(reused.pending_checkins.is_empty() && reused.cancelled_takeoffs.is_empty());
        assert!(reused.tower_ready.is_empty() && reused.exchange.is_none());
        assert!(!finish_job(&mut reused.preparation_job, 123));
        assert!(!finish_job(&mut reused.simulation_job, 456));
        let mut live = openatc_core::state::Telemetry {
            position_valid: true,
            on_ground: true,
            traffic: Some(vec![]),
            ..Default::default()
        };
        for step in 0..=180 {
            live.simulation_seconds = Some(f64::from(step) * 0.5);
            advance(&mut reused, &live);
        }
        for before in original.into_iter().filter(|pose| pose.id != APRON_ID_BASE) {
            let after = reused
                .world
                .snapshot()
                .into_iter()
                .find(|pose| pose.id == before.id)
                .unwrap();
            assert_eq!(
                after.position, before.position,
                "Reused ID moved without a fresh controller exchange"
            );
        }
    }
    #[test]
    #[ignore = "requires installed scenery and Bluebell"]
    fn installed_cannes_piston_scene_uses_propeller_stands() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let origin = airport(&root, "LFMD").unwrap();
        let runway = origin
            .runways
            .iter()
            .max_by(|a, b| {
                (a.second.east - a.first.east)
                    .hypot(a.second.north - a.first.north)
                    .total_cmp(
                        &(b.second.east - b.first.east).hypot(b.second.north - b.first.north),
                    )
            })
            .unwrap()
            .first_name
            .clone();
        let terrain =
            scenery::Terrain::new(std::path::Path::new(&root)).map(std::cell::RefCell::new);
        let scene = prepare_scene(
            Preparation {
                weather_observations: None,
                reserved_stand_cells: BTreeSet::new(),
                local_pattern_traffic: false,
                seed_single_arrival: false,
                player_airborne: false,
                minimum_airborne_spawn_nm: 0.0,
                airport_codes: BTreeMap::new(),
                reserved_surface_corridors: BTreeSet::new(),
                reserved_stands: BTreeSet::new(),
                stations: Arc::from([]),
                regions: BTreeMap::new(),
                simulator_root: root.clone(),
                own: [origin.reference_latitude, origin.reference_longitude],
                origin,
                destination: airport(&root, "LFMN").unwrap(),
                profiles: Arc::new(Profiles::builtin().unwrap()),
                models: Arc::new(
                    Catalogue::scan(
                        &std::path::Path::new(&root)
                            .join("Resources/plugins/OpenATC/Resources/CSL"),
                    )
                    .unwrap(),
                ),
                counts: [0, 0, 0, 4],
                observed: vec![],
                epoch: 1,
                seed: 1,
                startup_cast: false,
                taxi_target: 4,
                force_runway: false,
                force_arrival_runway: false,
                runway,
                arrival_runway: "04R".into(),
                config: std::path::PathBuf::new(),
                weather: BTreeMap::new(),
            },
            &terrain,
        )
        .unwrap();
        assert!(!scene.frames.is_empty());
        assert!(
            scene
                .frames
                .iter()
                .any(|frame| frame.target.aircraft == "C172")
        );
        assert_eq!(scene.frames.len(), scene.movement.len());
        assert!(scene.frames.iter().all(|frame| {
            let point = openatc_core::airport::airport_point(
                &scene.airport,
                frame.target.latitude,
                frame.target.longitude,
            );
            scene.airport.parking.iter().any(|stand| {
                stand
                    .equipment
                    .split('|')
                    .any(|equipment| equipment == "props")
                    && (stand.point.east - point.east).hypot(stand.point.north - point.north) < 1.0
            })
        }));
    }
    #[test]
    fn reset_never_republishes_an_old_generated_scene() {
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.generated.epoch = Some(1);
        state.session_generation = 2;
        step(&mut state);
        assert!(state.session.generated_traffic.is_empty());
    }

    #[test]
    fn display_callsign_keeps_the_code_for_the_tag_and_spells_the_alias_in_text() {
        let target = TrafficTarget {
            callsign: "BAW123".into(),
            name: "Speedbird".into(),
            ..Default::default()
        };
        assert_eq!(display_callsign(&target), "Speedbird 123");
        let unknown = TrafficTarget {
            callsign: "ZZZ9".into(),
            ..Default::default()
        };
        assert_eq!(display_callsign(&unknown), "ZZZ9");
    }
    #[test]
    fn opening_cast_replaces_seed_frames_before_category_limits() {
        use openatc_core::traffic::motion::{Route, Segment, Stage, Waypoint};
        let point = Waypoint {
            latitude: 43.0,
            longitude: 7.0,
            altitude_feet: 10.0,
        };
        let model = Arc::new(openatc_core::traffic::csl::Model {
            id: "fixture".into(),
            objects: Vec::new(),
            matches: Vec::new(),
            vertical_offset: None,
        });
        let frame = |id: u32| GeneratedTraffic {
            target: TrafficTarget {
                id,
                callsign: format!("TEST{id}"),
                aircraft: "C172".into(),
                latitude: 43.0,
                longitude: 7.0,
                on_ground: true,
                ..Default::default()
            },
            model: model.clone(),
            animation: Animation::default(),
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
        };
        let station = openatc_core::stations::Station {
            name: "Test Ground".into(),
            service: "Ground".into(),
            airport: "TEST".into(),
            khz: 121_700,
            ..Default::default()
        };
        let mut scene = Scene {
            require_complete_flights: true,
            surface_corridors: Arc::new(BTreeSet::new()),
            airports: BTreeMap::new(),
            frames: Vec::new(),
            world: Default::default(),
            movement: BTreeMap::new(),
            airport: Airport::default(),
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
            policy: parse_operations(include_str!("../../../traffic/operations.toml")).unwrap(),
            fixed_gear: BTreeSet::new(),
            channels: BTreeMap::new(),
            orders: BTreeMap::new(),
            turnaround_inputs: Arc::new(BTreeMap::new()),
            stations: Arc::from(Vec::new()),
            regions: Arc::new(BTreeMap::new()),
            mora: Arc::new(Default::default()),
            simulator_root: "test".into(),
            reserved_stands: BTreeSet::new(),
            occupied_stand_cells: Arc::new(BTreeSet::new()),
            arrival_refusals: 0,
            warnings: Vec::new(),
            seeded_stations: BTreeMap::new(),
            advisories: BTreeMap::new(),
            local_spares: Vec::new(),
        };
        let generation = scene.world.generation();
        for id in [0x00d1_0011, 0x00d1_0012, 0x00d1_0013, 0x00f1_0001] {
            let route = Arc::new(
                Route::compile(
                    vec![
                        Segment {
                            stage: Stage::Parked,
                            points: vec![point],
                            knots: 0.0,
                            resources: vec![format!("STAND/{id}")],
                            dwell_seconds: 10.0,
                        },
                        Segment {
                            stage: Stage::TaxiOut,
                            points: vec![
                                point,
                                Waypoint {
                                    latitude: point.latitude + 0.0001,
                                    ..point
                                },
                            ],
                            knots: 10.0,
                            resources: vec![format!("TAXI/{id}")],
                            dwell_seconds: 0.0,
                        },
                    ],
                    1000.0,
                    1000.0,
                )
                .unwrap(),
            );
            assert!(scene.world.insert(generation, id, route.clone()));
            let clearances = (0..route.segment_count())
                .map(|index| {
                    (
                        index,
                        flight::Clearance {
                            station: station.clone(),
                            transfers: Vec::new(),
                            coordinated_recovery: Vec::new(),
                            instruction: format!("Phase {index}"),
                            readback: format!("Phase {index}"),
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
            scene.flights.insert(
                id,
                Arc::new(flight::Prepared {
                    spacing_floor: None,
                    route,
                    airspeeds: BTreeMap::new(),
                    recovery: BTreeMap::new(),
                    clearances,
                    vacated_reports: BTreeMap::new(),
                    amendment: None,
                }),
            );
            scene.frames.push(frame(id));
        }
        let mut runtime = Runtime::default();
        runtime.population.target = 12;
        runtime.frames.push(frame(0x00d1_0001));
        runtime.frames.push(frame(0x00d1_0002));
        let telemetry = openatc_core::state::Telemetry {
            position_valid: true,
            latitude: 43.0,
            longitude: 7.0,
            on_ground: true,
            ..Default::default()
        };
        population::merge(
            &mut runtime,
            scene,
            "test".into(),
            0,
            &telemetry,
            &openatc_core::traffic::profiles::Profiles::parse(
                include_str!("../../../traffic/aircraft.toml"),
                include_str!("../../../traffic/airlines.toml"),
                include_str!("../../../traffic/hubs.toml"),
            )
            .unwrap(),
            [0, 0, 0, 0],
        );
        assert_eq!(
            runtime
                .frames
                .iter()
                .map(|frame| frame.target.id)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([0x00d1_0011, 0x00d1_0012, 0x00d1_0013, 0x00f1_0001]),
            "the opening cast must replace the seed frames even when every category cap is zero"
        );
    }
}

/// Read-only generated-traffic state for diagnosing stalled aircraft.
pub async fn debug_generated(
    axum::extract::State(shared): axum::extract::State<Shared>,
) -> axum::Json<serde_json::Value> {
    use serde_json::json;
    let state = shared.read().await;
    let runtime = &state.generated;
    let poses: Vec<serde_json::Value> = runtime
        .world
        .snapshot()
        .into_iter()
        .map(|pose| {
            json!({
                "id": pose.id,
                "stage": format!("{:?}", pose.stage),
                "segment": pose.segment,
                "awaitingClearance": pose.awaiting_clearance,
                "complete": pose.complete,
                "knots": pose.knots,
                "latitude": pose.position.latitude,
                "longitude": pose.position.longitude,
            })
        })
        .collect();
    let sessions: Vec<serde_json::Value> = runtime
        .sessions
        .sessions()
        .map(|(&id, session)| {
            json!({
                "id": id,
                "frequency": session.frequency,
                "phase": format!("{:?}", session.phase),
                "lastProgress": session.last_progress,
            })
        })
        .collect();
    let exchange = runtime.exchange.as_ref().map(|exchange| {
        json!({
            "id": exchange.id,
            "callsign": exchange.callsign,
            "segment": exchange.segment,
            "grant": exchange.grant,
            "next": exchange.next,
            "messages": exchange.messages,
        })
    });
    let delivery_exchange = runtime.delivery_exchange.as_ref().map(|exchange| {
        json!({
            "id": exchange.id,
            "callsign": exchange.callsign,
            "segment": exchange.segment,
            "grant": exchange.grant,
            "next": exchange.next,
        })
    });
    axum::Json(json!({
        "epoch": runtime.epoch,
        "sessionGeneration": state.session_generation,
        "frames": runtime.frames.len(),
        "flights": runtime.flights.len(),
        "sessions": sessions,
        "poses": poses,
        "approvedOrders": runtime.approved_orders.iter().collect::<Vec<_>>(),
        "reportedHolds": runtime.reported_holds.iter().collect::<Vec<_>>(),
        "exchange": exchange,
        "deliveryExchange": delivery_exchange,
        "departureWaits": runtime.departure_waits.iter().map(|(id, (reason, at))| json!({"id": id, "reason": reason, "forSeconds": at.elapsed().as_secs_f64()})).collect::<Vec<_>>(),
        "towerReady": runtime.tower_ready.iter().collect::<Vec<_>>(),
        "towerNotified": runtime.tower_notified.iter().collect::<Vec<_>>(),
        "activeFlights": runtime.active_flights.iter().collect::<Vec<_>>(),
        "held": runtime.held.iter().map(|(id, blocker)| json!({"id": id, "blocker": blocker})).collect::<Vec<_>>(),
        "surfaceBlockers": runtime.world.snapshot().into_iter().filter_map(|pose| runtime.world.surface_blocker(pose.id).map(|blocker| json!({"id": pose.id, "blocker": blocker}))).collect::<Vec<_>>(),
        "networkPending": runtime.network_pending,
        "orders": runtime.orders.iter().map(|(id, orders)| json!({"id": id, "segments": orders.iter().map(|(segment, _)| *segment).collect::<Vec<_>>()})).collect::<Vec<_>>(),
    }))
}
