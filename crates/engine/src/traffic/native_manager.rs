//! Native X-Plane AI aircraft management under OpenATC ATC control.
//!
//! This module manages X-Plane's native AI aircraft as participants in the OpenATC
//! ATC system. It tracks their state, issues simulated clearances, handles readbacks,
//! and integrates them with traffic separation and wake turbulence systems.

use crate::traffic::{separation, wake};
use crate::EngineState;
use openatc_core::airport::{Airport, calculate_taxi_route};
use openatc_core::state::{NativeAircraft, NativeClearance, NativePhase, Point, Telemetry, TrafficTarget};
use openatc_core::traffic::profiles::{Aircraft as Profile, Profiles};
use openatc_core::traffic::wake::departure_for_wake;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Maximum number of native AI aircraft X-Plane supports (excluding ownship).
const MAX_NATIVE_AI: usize = 19;

/// Time after which a readback is considered failed.
const READBACK_TIMEOUT: Duration = Duration::from_secs(45);

/// Minimum time between clearance updates for the same aircraft.
const CLEARANCE_COOLDOWN: Duration = Duration::from_secs(5);

/// Tracks a native X-Plane AI aircraft under ATC control.
#[derive(Clone, Debug)]
struct TrackedNativeAircraft {
    /// The native aircraft data.
    aircraft: NativeAircraft,
    /// When we last issued a clearance to this aircraft.
    last_clearance: Option<Instant>,
    /// Whether we've initialized tracking for this aircraft.
    initialized: bool,
    /// Assigned taxi route (taxiway names).
    taxi_route: Option<Vec<String>>,
    /// Taxi clearance for detailed route.
    taxi_clearance: Option<openatc_core::state::TaxiClearance>,
    /// Current target altitude for climb/descent.
    target_altitude: Option<i32>,
    /// Current target heading.
    target_heading: Option<f64>,
    /// Current target speed.
    target_speed: Option<i32>,
    /// Departure runway for this aircraft.
    departure_runway: Option<String>,
    /// Arrival runway for this aircraft.
    arrival_runway: Option<String>,
    /// Whether this is a departure or arrival.
    is_departure: bool,
    /// Pushback direction if applicable.
    pushback_direction: Option<f64>,
}

impl TrackedNativeAircraft {
    fn new(target: &TrafficTarget) -> Self {
        let mut aircraft = NativeAircraft {
            plane_index: 0, // Will be set when we know the index
            id: target.id,
            callsign: target.callsign.clone(),
            aircraft: target.aircraft.clone(),
            phase: NativePhase::Unknown,
            clearance: None,
            awaiting_readback: None,
            latitude: target.latitude,
            longitude: target.longitude,
            altitude_feet: target.altitude_feet,
            track_degrees: target.track_degrees,
            ground_speed_knots: target.ground_speed_knots.unwrap_or(0.0),
            on_ground: target.on_ground,
            ai_disabled: false,
            com1_khz: target.com1_khz,
            com2_khz: target.com2_khz,
            wake_category: target.wake_category,
            wingspan_meters: target.wingspan_meters,
            ..Default::default()
        };

        // Determine initial phase based on position/state
        aircraft.phase = Self::determine_phase(&aircraft);

        Self {
            aircraft,
            last_clearance: None,
            initialized: false,
            taxi_route: None,
            taxi_clearance: None,
            target_altitude: None,
            target_heading: None,
            target_speed: None,
            departure_runway: None,
            arrival_runway: None,
            is_departure: !target.on_ground, // Assume airborne = arrival, on ground = departure
            pushback_direction: None,
        }
    }

    fn determine_phase(aircraft: &NativeAircraft) -> NativePhase {
        if aircraft.on_ground {
            if aircraft.ground_speed_knots < 1.0 {
                // Check if at gate/parking vs holding short
                NativePhase::AtGate
            } else if aircraft.ground_speed_knots < 30.0 {
                NativePhase::TaxiOut
            } else {
                NativePhase::Takeoff
            }
        } else {
            if aircraft.altitude_feet < 3000.0 && aircraft.ground_speed_knots < 200.0 {
                NativePhase::Approach
            } else if aircraft.altitude_feet < 10000.0 {
                NativePhase::InitialClimb
            } else {
                NativePhase::Enroute
            }
        }
    }

    fn update_from_target(&mut self, target: &TrafficTarget, plane_index: u8) {
        let was_on_ground = self.aircraft.on_ground;
        
        self.aircraft.plane_index = plane_index;
        self.aircraft.latitude = target.latitude;
        self.aircraft.longitude = target.longitude;
        self.aircraft.altitude_feet = target.altitude_feet;
        self.aircraft.track_degrees = target.track_degrees;
        self.aircraft.ground_speed_knots = target.ground_speed_knots.unwrap_or(0.0);
        self.aircraft.on_ground = target.on_ground;
        self.aircraft.com1_khz = target.com1_khz;
        self.aircraft.com2_khz = target.com2_khz;
        self.aircraft.wake_category = target.wake_category;
        self.aircraft.wingspan_meters = target.wingspan_meters;

        // Update phase based on new state
        self.aircraft.phase = Self::determine_phase(&self.aircraft);

        // Track takeoff/landing transitions
        if was_on_ground && !target.on_ground {
            // Takeoff occurred
            self.is_departure = true;
        } else if !was_on_ground && target.on_ground {
            // Landing occurred
            self.is_departure = false;
            self.aircraft.phase = NativePhase::Rollout;
        }
    }
}

/// Manages native X-Plane AI aircraft as ATC participants.
#[derive(Default)]
pub struct NativeAircraftManager {
    /// Tracked native aircraft, keyed by Mode S ID.
    aircraft: HashMap<u32, TrackedNativeAircraft>,
    /// Next sequence number for clearances.
    next_sequence: u32,
    /// Whether native AI participation is enabled.
    enabled: bool,
    /// Whether we currently own the AI planes (via XPLMAcquirePlanes).
    owns_planes: bool,
    /// Airport profiles for aircraft performance data.
    profiles: Option<Arc<Profiles>>,
    /// Cached airport data for taxi routing.
    airport_cache: Option<Arc<Airport>>,
    /// Cached airport ICAO.
    airport_icao: Option<String>,
}

impl NativeAircraftManager {
    /// Create a new NativeAircraftManager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable or disable native AI participation.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.aircraft.clear();
        }
    }

    /// Set whether we own the planes (for disabling X-Plane AI).
    pub fn set_owns_planes(&mut self, owns: bool) {
        self.owns_planes = owns;
    }

    /// Set the aircraft profiles for performance lookups.
    pub fn set_profiles(&mut self, profiles: Arc<Profiles>) {
        self.profiles = Some(profiles);
    }

    /// Set the airport data for taxi routing and runway determination.
    pub fn set_airport(&mut self, airport: Arc<Airport>) {
        self.airport_icao = Some(airport.icao.clone());
        self.airport_cache = Some(airport);
    }

    /// Clear the airport cache.
    pub fn clear_airport(&mut self) {
        self.airport_cache = None;
        self.airport_icao = None;
    }

    /// Process telemetry update and manage native aircraft.
    pub fn tick(&mut self, state: &mut crate::EngineState, telemetry: &Telemetry) {
        if !self.enabled || !state.settings.native_ai_participation {
            return;
        }

        // Update ownership state from telemetry
        self.owns_planes = telemetry.native_ai_owned;

        // Get native traffic from telemetry
        let native_targets = telemetry.traffic.as_deref().unwrap_or(&[]);

        // Update or create tracked aircraft
        let mut seen_ids = std::collections::BTreeSet::new();
        for (index, target) in native_targets.iter().enumerate() {
            if target.id == 0 {
                continue; // Skip ownship
            }
            seen_ids.insert(target.id);

            let plane_index = (index + 1) as u8; // 1-based, 0 is ownship
            if plane_index > MAX_NATIVE_AI as u8 {
                continue;
            }

            let id = target.id;
            let is_new = !self.aircraft.contains_key(&id);
            self.aircraft.entry(id).or_insert_with(|| {
                let mut t = TrackedNativeAircraft::new(target);
                t.aircraft.plane_index = plane_index;
                t
            });

            // Assign departure/arrival runways for new aircraft
            if is_new {
                if let Some(tracked) = self.aircraft.get_mut(&id) {
                    if tracked.is_departure {
                        tracked.departure_runway = self.get_departure_runway(state);
                    } else {
                        tracked.arrival_runway = self.get_arrival_runway(state);
                    }
                }
            }

            // Update tracked aircraft (mutable borrow ends at end of block)
            {
                if let Some(tracked) = self.aircraft.get_mut(&id) {
                    tracked.update_from_target(target, plane_index);

                    // If we own the planes, mark AI as disabled for this aircraft
                    if self.owns_planes && !tracked.aircraft.ai_disabled {
                        tracked.aircraft.ai_disabled = true;
                        // XPLMDisableAIForPlane is called by the plugin
                    }
                }
            }

            // Manage ATC interaction based on phase
            self.manage_atc_interaction(state, id);
        }

        // Remove aircraft no longer in simulator
        self.aircraft.retain(|id, _| seen_ids.contains(id));

        // Update state with native aircraft for UI/ATC
        state.session.native_aircraft = self.aircraft.values().map(|t| t.aircraft.clone()).collect();
    }

    /// Manage ATC interaction for a native aircraft based on its phase.
    fn manage_atc_interaction(&mut self, state: &mut EngineState, id: u32) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64();

        // Check readback timeout
        if let Some(tracked) = self.aircraft.get_mut(&id) {
            if let Some(readback) = &tracked.aircraft.awaiting_readback {
                if now > readback.deadline {
                    tracked.aircraft.awaiting_readback = None;
                    tracked.aircraft.clearance = None;
                }
            }
        }

        // Get phase and other data (borrow ends here)
        let phase = self.aircraft.get(&id).map(|t| t.aircraft.phase.clone());
        let initialized = self.aircraft.get(&id).map(|t| t.initialized).unwrap_or(false);
        let has_clearance = self.aircraft.get(&id).map(|t| t.aircraft.clearance.is_none()).unwrap_or(false);

        // Issue clearances based on phase (re-acquire mutable borrow)
        if let Some(phase) = phase {
            match phase {
                NativePhase::AtGate => {
                    if has_clearance && initialized {
                        self.issue_taxi_clearance(state, id);
                    }
                }
                NativePhase::Pushback => {
                    if has_clearance {
                        self.issue_pushback_clearance(state, id);
                    }
                }
                NativePhase::TaxiOut => {
                    if has_clearance {
                        self.issue_taxi_clearance(state, id);
                    }
                    self.check_hold_short(state, id);
                }
                NativePhase::HoldShort => {
                    if has_clearance {
                        self.issue_hold_short_clearance(state, id);
                    }
                }
                NativePhase::LineUp => {
                    if has_clearance {
                        // Check wake turbulence delay before lineup
                        if let Some(tracked) = self.aircraft.get(&id) {
                            if let Some(airport) = self.airport_cache.as_ref() {
                                if let Some(runway) = &tracked.departure_runway {
                                    let wake_delay = state.traffic.wake.departure_delay(
                                        tracked.aircraft.id,
                                        airport,
                                        runway,
                                        &tracked.aircraft.aircraft,
                                        tracked.aircraft.latitude,
                                        tracked.aircraft.longitude,
                                    );
                                    if wake_delay <= 0.0 {
                                        self.issue_lineup_clearance(state, id);
                                    }
                                } else {
                                    self.issue_lineup_clearance(state, id);
                                }
                            } else {
                                self.issue_lineup_clearance(state, id);
                            }
                        }
                    }
                }
                NativePhase::Takeoff => {
                    if has_clearance {
                        // Check wake turbulence delay and separation before takeoff
                        if let Some(tracked) = self.aircraft.get(&id) {
                            if let Some(airport) = self.airport_cache.as_ref() {
                                if let Some(runway) = &tracked.departure_runway {
                                    let wake_delay = state.traffic.wake.departure_delay(
                                        tracked.aircraft.id,
                                        airport,
                                        runway,
                                        &tracked.aircraft.aircraft,
                                        tracked.aircraft.latitude,
                                        tracked.aircraft.longitude,
                                    );
                                    if wake_delay <= 0.0 {
                                        self.issue_takeoff_clearance(state, id);
                                    }
                                } else {
                                    self.issue_takeoff_clearance(state, id);
                                }
                            } else {
                                self.issue_takeoff_clearance(state, id);
                            }
                        }
                    }
                }
                NativePhase::InitialClimb => {
                    if has_clearance {
                        self.issue_climb_clearance(state, id);
                    }
                }
                NativePhase::Enroute => {
                    // Monitor for descent/approach based on flight plan
                    if let Some(tracked) = self.aircraft.get_mut(&id) {
                        // Check if we should start descent based on distance to destination
                        if let Some(arrival_airport) = state.arrival_airport.as_ref() {
                            let distance_nm = openatc_core::arrival::distance_nm(
                                [tracked.aircraft.latitude, tracked.aircraft.longitude],
                                [arrival_airport.reference_latitude, arrival_airport.reference_longitude],
                            );
                            // Start descent ~100-150 NM out for jets, 50 NM for props
                            let descent_distance = if tracked.aircraft.aircraft.starts_with('B') || tracked.aircraft.aircraft.starts_with('A') {
                                120.0 // Jets
                            } else {
                                60.0 // Props
                            };
                            
                            if distance_nm <= descent_distance && tracked.aircraft.phase == NativePhase::Enroute {
                                tracked.aircraft.phase = NativePhase::Descent;
                            }
                        }
                    }
                }
                NativePhase::Descent => {
                    if has_clearance {
                        self.issue_descent_clearance(state, id);
                    }
                }
                NativePhase::Approach => {
                    if has_clearance {
                        // Check arrival wake conflict
                        let conflict = if let Some(tracked) = self.aircraft.get(&id) {
                            if let Some(airport) = self.airport_cache.as_ref() {
                                if let Some(runway) = &tracked.arrival_runway {
                                    let mut wake_monitor = state.traffic.wake.clone();
                                    let targets = state.traffic_targets();
                                    wake_monitor.arrival_conflict(
                                        &state.session.telemetry,
                                        &targets,
                                        Some(airport),
                                        runway,
                                        0.0,
                                    )
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        
                        if !conflict {
                            self.issue_approach_clearance(state, id);
                        }
                    }
                }
                NativePhase::Final => {
                    if has_clearance {
                        // Check arrival wake conflict
                        let conflict = if let Some(tracked) = self.aircraft.get(&id) {
                            if let Some(airport) = self.airport_cache.as_ref() {
                                if let Some(runway) = &tracked.arrival_runway {
                                    let mut wake_monitor = state.traffic.wake.clone();
                                    let targets = state.traffic_targets();
                                    wake_monitor.arrival_conflict(
                                        &state.session.telemetry,
                                        &targets,
                                        Some(airport),
                                        runway,
                                        0.0,
                                    )
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        
                        if !conflict {
                            self.issue_landing_clearance(state, id);
                        }
                    }
                }
                NativePhase::Landing => {
                    // Transition to rollout
                }
                NativePhase::Rollout => {
                    if has_clearance {
                        self.issue_rollout_clearance(state, id);
                    }
                }
                NativePhase::TaxiIn => {
                    if has_clearance {
                        self.issue_taxi_in_clearance(state, id);
                    }
                }
                NativePhase::AtGateArrival => {
                    // Flight complete
                }
                NativePhase::Unknown => {
                    if let Some(tracked) = self.aircraft.get_mut(&id) {
                        tracked.initialized = true;
                    }
                }
            }

            // Process readback if clearance exists and not acknowledged
            if let Some(tracked) = self.aircraft.get_mut(&id) {
                if let Some(clearance) = &tracked.aircraft.clearance {
                    if tracked.aircraft.awaiting_readback.is_none() {
                        tracked.aircraft.awaiting_readback = Some(openatc_core::state::ReadbackExpectation {
                            clearance_sequence: clearance.sequence,
                            expected_items: Self::expected_readback_items(clearance),
                            deadline: now + READBACK_TIMEOUT.as_secs_f64(),
                        });
                    }
                }
            }
        }
    }

    /// Determine expected readback items for a clearance.
    fn expected_readback_items(clearance: &NativeClearance) -> Vec<openatc_core::state::ReadbackItem> {
        let mut items = Vec::new();
        match clearance.kind {
            openatc_core::state::ClearanceKind::Taxi => {
                items.push(openatc_core::state::ReadbackItem::TaxiRoute);
                if clearance.hold_short_of.is_some() {
                    items.push(openatc_core::state::ReadbackItem::HoldShort);
                }
            }
            openatc_core::state::ClearanceKind::HoldShort => {
                items.push(openatc_core::state::ReadbackItem::HoldShort);
            }
            openatc_core::state::ClearanceKind::LineUp => {
                items.push(openatc_core::state::ReadbackItem::Runway);
            }
            openatc_core::state::ClearanceKind::Takeoff => {
                items.push(openatc_core::state::ReadbackItem::Runway);
            }
            openatc_core::state::ClearanceKind::Climb | openatc_core::state::ClearanceKind::Descend => {
                if clearance.altitude_feet.is_some() {
                    items.push(openatc_core::state::ReadbackItem::Altitude);
                }
            }
            openatc_core::state::ClearanceKind::Heading => {
                items.push(openatc_core::state::ReadbackItem::Heading);
            }
            openatc_core::state::ClearanceKind::Speed => {
                items.push(openatc_core::state::ReadbackItem::Speed);
            }
            openatc_core::state::ClearanceKind::Approach => {
                items.push(openatc_core::state::ReadbackItem::Runway);
            }
            openatc_core::state::ClearanceKind::Land => {
                items.push(openatc_core::state::ReadbackItem::Runway);
            }
            openatc_core::state::ClearanceKind::CrossRunway => {
                items.push(openatc_core::state::ReadbackItem::Runway);
            }
        }
        items
    }

    /// Issue taxi clearance.
    fn issue_taxi_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        
        let Some(airport) = self.airport_cache.as_ref() else { return };
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        
        // Check ground separation - hold if traffic ahead
        let traffic_targets = state.traffic_targets();
        let profile = self.profiles
            .as_ref()
            .and_then(|p| p.aircraft(&tracked.aircraft.aircraft));
        let own_length = profile.map(|p| p.length).unwrap_or(30.0);
        
        // Check for traffic ahead on taxi route
        let conflict = separation::closest_ahead(
            tracked.aircraft.latitude,
            tracked.aircraft.longitude,
            tracked.aircraft.track_degrees,
            own_length,
            tracked.aircraft.id,
            &traffic_targets,
            |_| Some(100.0), // Default queue gap
        );
        
        if conflict.is_some() {
            // Traffic ahead - don't issue taxi clearance yet
            return;
        }
        
        // Determine destination based on phase
        let (destination, to_parking) = if tracked.is_departure {
            // Departure: taxi to runway hold short
            (tracked.departure_runway.clone().unwrap_or_else(|| "09".to_string()), false)
        } else {
            // Arrival: taxi to parking
            ("Gate 1".to_string(), true)
        };
        
        // Get aircraft size from profile
        let aircraft_size = self.profiles
            .as_ref()
            .and_then(|p| p.aircraft(&tracked.aircraft.aircraft))
            .map(|p| p.parking_size)
            .unwrap_or('C');
        
        // Calculate taxi route using airport data
        let telemetry = &state.session.telemetry;
        if let Ok(taxi_clearance) = calculate_taxi_route(
            airport,
            telemetry,
            &destination,
            to_parking,
            aircraft_size,
        ) {
            let taxi_route: Vec<String> = taxi_clearance.points
                .iter()
                .map(|p| p.name.clone())
                .filter(|n| !n.is_empty())
                .collect();
            
            let hold_short = taxi_clearance.hold_short_runway.clone();
            let instructions = taxi_clearance.instructions.clone();
            
            tracked.taxi_route = Some(taxi_route.clone());
            tracked.taxi_clearance = Some(taxi_clearance);
            
            tracked.aircraft.clearance = Some(NativeClearance {
                kind: openatc_core::state::ClearanceKind::Taxi,
                altitude_feet: None,
                heading: None,
                speed_knots: None,
                runway: tracked.departure_runway.clone(),
                taxi_route: Some(taxi_route),
                hold_short_of: if !hold_short.is_empty() { Some(hold_short) } else { None },
                sequence: self.next_sequence(),
            });
            tracked.last_clearance = Some(Instant::now());
            
            self.add_atc_transmission(state, id, &instructions);
        }
    }

    /// Issue pushback clearance.
    fn issue_pushback_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        
        // Calculate pushback direction based on aircraft heading and parking position
        // For now, use the aircraft's current heading + 180 (face opposite direction)
        let pushback_heading = (tracked.aircraft.track_degrees + 180.0) % 360.0;
        tracked.pushback_direction = Some(pushback_heading);
        
        let cardinal = heading_to_cardinal(pushback_heading);
        
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Taxi,
            altitude_feet: None,
            heading: Some(pushback_heading),
            speed_knots: None,
            runway: None,
            taxi_route: None,
            hold_short_of: None,
            sequence: self.next_sequence(),
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Pushback approved, face {}", cardinal));
    }

    /// Convert heading to cardinal direction.
    fn heading_to_cardinal(heading: f64) -> String {
        let dirs = ["north", "north-east", "east", "south-east", "south", "south-west", "west", "north-west"];
        let idx = ((heading + 22.5) / 45.0).floor() as usize % 8;
        dirs[idx].to_string()
    }

    /// Get the departure runway for this aircraft based on airport assignment.
    fn get_departure_runway(&self, state: &EngineState) -> Option<String> {
        if let Some(airport) = self.airport_cache.as_ref() {
            if let Some(assignment) = state.airports.assignment(
                &airport.icao,
                &state.session.plan.runway,
                &state.session.plan.arrival_runway,
                state.session.telemetry.magnetic_heading_degrees.unwrap_or(0.0),
                state.session.telemetry.wingspan_meters.unwrap_or(10.0) * 1.94384, // Approximate wind knots from wingspan
                0.0, // Minimum length - we'll use airport default
            ) {
                return Some(assignment.departure);
            }
        }
        None
    }

    /// Get the arrival runway for this aircraft based on airport assignment.
    fn get_arrival_runway(&self, state: &EngineState) -> Option<String> {
        if let Some(airport) = self.airport_cache.as_ref() {
            if let Some(assignment) = state.airports.assignment(
                &airport.icao,
                &state.session.plan.runway,
                &state.session.plan.arrival_runway,
                state.session.telemetry.magnetic_heading_degrees.unwrap_or(0.0),
                state.session.telemetry.wingspan_meters.unwrap_or(10.0) * 1.94384,
                0.0,
            ) {
                return Some(assignment.arrival);
            }
        }
        None
    }

    /// Issue hold short clearance.
    fn issue_hold_short_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let runway = self.get_departure_runway(state).unwrap_or_else(|| "09".to_string());
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::HoldShort,
            altitude_feet: None,
            heading: None,
            speed_knots: None,
            runway: Some(runway.clone()),
            taxi_route: None,
            hold_short_of: Some(runway.clone()),
            sequence: self.next_sequence(),
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Hold short of runway {}", runway));
    }

    /// Check if aircraft needs to hold short.
    fn check_hold_short(&mut self, state: &mut EngineState, id: u32) {
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        // Check if taxi clearance has hold short and we're approaching it
        if let Some(taxi) = &tracked.taxi_clearance {
            if !taxi.hold_short_runway.is_empty() {
                // Check distance to hold short point using airport data
                if let Some(airport) = self.airport_cache.as_ref() {
                    // Simplified: if speed is low and we have a hold short, transition to hold short phase
                    if tracked.aircraft.ground_speed_knots < 5.0 {
                        tracked.aircraft.phase = NativePhase::HoldShort;
                    }
                }
            }
        }
    }

    /// Issue lineup clearance.
    fn issue_lineup_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let runway = self.get_departure_runway(state).unwrap_or_else(|| "09".to_string());
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::LineUp,
            altitude_feet: None,
            heading: None,
            speed_knots: None,
            runway: Some(runway.clone()),
            taxi_route: None,
            hold_short_of: None,
            sequence: self.next_sequence(),
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Line up and wait runway {}", runway));
    }

    /// Issue takeoff clearance.
    fn issue_takeoff_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let runway = self.get_departure_runway(state).unwrap_or_else(|| "09".to_string());
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        
        // Get initial climb altitude from flight plan or default
        let initial_alt = state.session.plan.initial_altitude_feet.max(3000) as i32;
        
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Takeoff,
            altitude_feet: Some(initial_alt),
            heading: None,
            speed_knots: None,
            runway: Some(runway.clone()),
            taxi_route: None,
            hold_short_of: None,
            sequence: self.next_sequence(),
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Cleared for takeoff runway {}, climb {}", runway, initial_alt));
    }

    /// Issue climb clearance.
    fn issue_climb_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        
        // Get cruise altitude from flight plan
        let cruise_alt = state.session.plan.cruise_feet.max(state.session.plan.initial_altitude_feet) as i32;
        tracked.target_altitude = Some(cruise_alt);
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Climb,
            altitude_feet: Some(cruise_alt),
            heading: None,
            speed_knots: None,
            runway: None,
            taxi_route: None,
            hold_short_of: None,
            sequence: self.next_sequence(),
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Climb and maintain {}", cruise_alt));
    }

    /// Issue descent clearance.
    fn issue_descent_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        
        // For arrival, descend to approach altitude
        let approach_alt = 5000; // Could be based on approach plate
        tracked.target_altitude = Some(approach_alt);
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Descend,
            altitude_feet: Some(approach_alt),
            heading: None,
            speed_knots: None,
            runway: None,
            taxi_route: None,
            hold_short_of: None,
            sequence: self.next_sequence(),
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Descend and maintain {}", approach_alt));
    }

    /// Issue approach clearance.
    fn issue_approach_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let runway = self.get_arrival_runway(state).unwrap_or_else(|| "09".to_string());
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        
        // Get approach type from flight plan
        let approach_type = state.session.plan.approach.clone().unwrap_or_else(|| "ILS".to_string());
        
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Approach,
            altitude_feet: None,
            heading: None,
            speed_knots: None,
            runway: Some(runway.clone()),
            taxi_route: None,
            hold_short_of: None,
            sequence: self.next_sequence(),
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Cleared {} runway {} approach", approach_type, runway));
    }

    /// Issue landing clearance.
    fn issue_landing_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let runway = self.get_arrival_runway(state).unwrap_or_else(|| "09".to_string());
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Land,
            altitude_feet: None,
            heading: None,
            speed_knots: None,
            runway: Some(runway.clone()),
            taxi_route: None,
            hold_short_of: None,
            sequence: self.next_sequence(),
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Cleared to land runway {}", runway));
    }

    /// Issue rollout clearance.
    fn issue_rollout_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        
        // Get exit taxiway from taxi clearance if available
        let exit_taxi = tracked.taxi_clearance.as_ref()
            .and_then(|t| t.points.first())
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Exit A".to_string());
        
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Taxi,
            altitude_feet: None,
            heading: None,
            speed_knots: None,
            runway: None,
            taxi_route: Some(vec![exit_taxi.clone()]),
            hold_short_of: None,
            sequence: self.next_sequence(),
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Exit runway via {}, contact ground", exit_taxi));
    }

    /// Issue taxi-in clearance.
    fn issue_taxi_in_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let Some(airport) = self.airport_cache.as_ref() else { return };
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        
        // Calculate taxi route to parking
        let telemetry = &state.session.telemetry;
        let aircraft_size = self.profiles
            .as_ref()
            .and_then(|p| p.aircraft(&tracked.aircraft.aircraft))
            .map(|p| p.parking_size)
            .unwrap_or('C');
        
        if let Ok(taxi_clearance) = calculate_taxi_route(
            airport,
            telemetry,
            "Gate 1",
            true,
            aircraft_size,
        ) {
            let taxi_route: Vec<String> = taxi_clearance.points
                .iter()
                .map(|p| p.name.clone())
                .filter(|n| !n.is_empty())
                .collect();
            
            let instructions = taxi_clearance.instructions.clone();
            
            tracked.taxi_route = Some(taxi_route.clone());
            tracked.taxi_clearance = Some(taxi_clearance);
            
            tracked.aircraft.clearance = Some(NativeClearance {
                kind: openatc_core::state::ClearanceKind::Taxi,
                altitude_feet: None,
                heading: None,
                speed_knots: None,
                runway: None,
                taxi_route: Some(taxi_route),
                hold_short_of: None,
                sequence: self.next_sequence(),
            });
            tracked.last_clearance = Some(Instant::now());

            self.add_atc_transmission(state, id, &instructions);
        }
    }

    /// Check if we should throttle clearances for this aircraft.
    fn should_throttle_clearance(&self, id: u32) -> bool {
        self.aircraft.get(&id).is_some_and(|tracked| {
            tracked.last_clearance.is_some_and(|last| last.elapsed() < CLEARANCE_COOLDOWN)
        })
    }

    /// Get next sequence number.
    fn next_sequence(&mut self) -> u32 {
        let seq = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        seq.max(1)
    }

    /// Add ATC transmission to transcript.
    fn add_atc_transmission(&self, state: &mut EngineState, id: u32, text: &str) {
        use openatc_core::apply::add_transmission;
        use openatc_core::state::{Radio, SpeechTag};

        let Some(tracked) = self.aircraft.get(&id) else { return };
        // Determine which radio the aircraft is on
        let radio = tracked.aircraft.com1_khz.map_or(Radio::Com1, |khz| {
            if state.session.receiving_radio(khz).is_some() {
                Radio::Com1
            } else {
                Radio::Com2
            }
        });

        let tag = SpeechTag {
            radio: Some(radio),
            voice: String::new(),
            delivery: "standard".to_string(),
            speed: 1.0,
            position: tracked.aircraft.callsign.clone(),
            ..Default::default()
        };

        add_transmission(&mut state.session, "ATC", text, &tag);
    }

    /// Get all tracked native aircraft as NativeAircraft for state.
    pub fn get_native_aircraft(&self) -> Vec<NativeAircraft> {
        self.aircraft.values().map(|t| t.aircraft.clone()).collect()
    }

    /// Get count of tracked aircraft.
    pub fn len(&self) -> usize {
        self.aircraft.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.aircraft.is_empty()
    }
}

use std::sync::Arc;