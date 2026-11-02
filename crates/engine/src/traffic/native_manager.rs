//! Native X-Plane AI aircraft management under OpenATC ATC control.
//!
//! This module manages X-Plane's native AI aircraft as participants in the OpenATC
//! ATC system. It tracks their state, issues simulated clearances, handles readbacks,
//! and integrates them with traffic separation and wake turbulence systems.

use crate::traffic::separation;
use crate::EngineState;
use openatc_core::airport::Airport;
use openatc_core::state::{NativeAircraft, NativeClearance, NativePhase, Point, Telemetry, TrafficTarget};
use openatc_core::traffic::profiles::{Aircraft as Profile, Profiles};
use std::collections::HashMap;
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
    /// Current target altitude for climb/descent.
    target_altitude: Option<i32>,
    /// Current target heading.
    target_heading: Option<f64>,
    /// Current target speed.
    target_speed: Option<i32>,
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
            target_altitude: None,
            target_heading: None,
            target_speed: None,
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
            self.aircraft.entry(id).or_insert_with(|| {
                let mut t = TrackedNativeAircraft::new(target);
                t.aircraft.plane_index = plane_index;
                t
            });

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
                        self.issue_lineup_clearance(state, id);
                    }
                }
                NativePhase::Takeoff => {
                    if has_clearance {
                        self.issue_takeoff_clearance(state, id);
                    }
                }
                NativePhase::InitialClimb => {
                    if has_clearance {
                        self.issue_climb_clearance(state, id);
                    }
                }
                NativePhase::Enroute => {
                    // Monitor for descent/approach
                }
                NativePhase::Descent => {
                    if has_clearance {
                        self.issue_descent_clearance(state, id);
                    }
                }
                NativePhase::Approach => {
                    if has_clearance {
                        self.issue_approach_clearance(state, id);
                    }
                }
                NativePhase::Final => {
                    if has_clearance {
                        self.issue_landing_clearance(state, id);
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
        let sequence = self.next_sequence();
        // TODO: Calculate actual taxi route from airport data
        let taxi_route = vec!["A".to_string(), "B".to_string()]; // Placeholder
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.taxi_route = Some(taxi_route.clone());

        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Taxi,
            altitude_feet: None,
            heading: None,
            speed_knots: None,
            runway: None,
            taxi_route: Some(taxi_route),
            hold_short_of: None,
            sequence,
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, "Taxi via A, B, hold short of runway 09");
    }

    /// Issue pushback clearance.
    fn issue_pushback_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let sequence = self.next_sequence();
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Taxi,
            altitude_feet: None,
            heading: None,
            speed_knots: None,
            runway: None,
            taxi_route: None,
            hold_short_of: None,
            sequence,
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, "Pushback approved, face east");
    }

    /// Issue hold short clearance.
    fn issue_hold_short_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let sequence = self.next_sequence();
        let runway = "09"; // TODO: Determine actual runway
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::HoldShort,
            altitude_feet: None,
            heading: None,
            speed_knots: None,
            runway: Some(runway.to_string()),
            taxi_route: None,
            hold_short_of: Some(runway.to_string()),
            sequence,
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Hold short of runway {}", runway));
    }

    /// Check if aircraft needs to hold short.
    fn check_hold_short(&mut self, state: &mut EngineState, id: u32) {
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        // TODO: Check if approaching runway hold short line
        // For now, simulate based on phase transition
        if tracked.aircraft.phase == NativePhase::TaxiOut && tracked.aircraft.ground_speed_knots < 5.0 {
            tracked.aircraft.phase = NativePhase::HoldShort;
        }
    }

    /// Issue lineup clearance.
    fn issue_lineup_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let sequence = self.next_sequence();
        let runway = "09"; // TODO: Determine actual runway
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::LineUp,
            altitude_feet: None,
            heading: None,
            speed_knots: None,
            runway: Some(runway.to_string()),
            taxi_route: None,
            hold_short_of: None,
            sequence,
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Line up and wait runway {}", runway));
    }

    /// Issue takeoff clearance.
    fn issue_takeoff_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let sequence = self.next_sequence();
        let runway = "09"; // TODO: Determine actual runway
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Takeoff,
            altitude_feet: Some(5000),
            heading: None,
            speed_knots: None,
            runway: Some(runway.to_string()),
            taxi_route: None,
            hold_short_of: None,
            sequence,
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Cleared for takeoff runway {}, climb 5000", runway));
    }

    /// Issue climb clearance.
    fn issue_climb_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let sequence = self.next_sequence();
        let target_alt = 15000; // TODO: Based on flight plan
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.target_altitude = Some(target_alt);
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Climb,
            altitude_feet: Some(target_alt),
            heading: None,
            speed_knots: None,
            runway: None,
            taxi_route: None,
            hold_short_of: None,
            sequence,
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Climb and maintain {}", target_alt));
    }

    /// Issue descent clearance.
    fn issue_descent_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let sequence = self.next_sequence();
        let target_alt = 5000; // TODO: Based on approach
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.target_altitude = Some(target_alt);
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Descend,
            altitude_feet: Some(target_alt),
            heading: None,
            speed_knots: None,
            runway: None,
            taxi_route: None,
            hold_short_of: None,
            sequence,
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Descend and maintain {}", target_alt));
    }

    /// Issue approach clearance.
    fn issue_approach_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let sequence = self.next_sequence();
        let runway = "09"; // TODO: Determine actual runway
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Approach,
            altitude_feet: None,
            heading: None,
            speed_knots: None,
            runway: Some(runway.to_string()),
            taxi_route: None,
            hold_short_of: None,
            sequence,
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Cleared ILS runway {} approach", runway));
    }

    /// Issue landing clearance.
    fn issue_landing_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let sequence = self.next_sequence();
        let runway = "09"; // TODO: Determine actual runway
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Land,
            altitude_feet: None,
            heading: None,
            speed_knots: None,
            runway: Some(runway.to_string()),
            taxi_route: None,
            hold_short_of: None,
            sequence,
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, &format!("Cleared to land runway {}", runway));
    }

    /// Issue rollout clearance.
    fn issue_rollout_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let sequence = self.next_sequence();
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Taxi,
            altitude_feet: None,
            heading: None,
            speed_knots: None,
            runway: None,
            taxi_route: Some(vec!["Exit A".to_string()]),
            hold_short_of: None,
            sequence,
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, "Exit runway via A, contact ground");
    }

    /// Issue taxi-in clearance.
    fn issue_taxi_in_clearance(&mut self, state: &mut EngineState, id: u32) {
        if self.should_throttle_clearance(id) {
            return;
        }
        let sequence = self.next_sequence();
        let Some(tracked) = self.aircraft.get_mut(&id) else { return };
        tracked.aircraft.clearance = Some(NativeClearance {
            kind: openatc_core::state::ClearanceKind::Taxi,
            altitude_feet: None,
            heading: None,
            speed_knots: None,
            runway: None,
            taxi_route: Some(vec!["B".to_string(), "Gate 1".to_string()]),
            hold_short_of: None,
            sequence,
        });
        tracked.last_clearance = Some(Instant::now());

        self.add_atc_transmission(state, id, "Taxi via B to gate 1");
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