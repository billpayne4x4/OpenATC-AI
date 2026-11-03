//! Shared aircraft identity, state and behaviour.
//!
//! This module is the foundation for unifying pilot and traffic behaviour. The state and
//! behaviour trait are introduced first; migration of the existing phase logic follows.

use crate::atc_manager::AircraftKind;

/// Control mode for an aircraft. Traffic can execute instantly when unobstructed; the
/// pilot path may use AI when it shares a frequency with a controller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlMode {
    /// Pilot control.
    #[default]
    Pilot,
    /// Traffic control.
    Traffic,
}

/// Coarse aircraft phase shared by pilot and traffic.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum AircraftPhase {
    #[default]
    /// `Startup`.
    Startup,
    /// `Pushback`.
    Pushback,
    /// `IfrClearance`.
    IfrClearance,
    /// `Taxi`.
    Taxi,
    /// `Hold`.
    Hold,
    /// `Crossing`.
    Crossing,
    /// `LineUp`.
    LineUp,
    /// `Takeoff`.
    Takeoff,
    /// `Airborne`.
    Airborne,
    /// `Approach`.
    Approach,
    /// `Landing`.
    Landing,
    /// `TaxiIn`.
    TaxiIn,
    /// `AtGateIn`.
    AtGateIn,
    /// Flight complete.
    /// `Finished`.
    Finished,
    /// `Departure`.
    Departure,
    /// `Cruise`.
    Cruise,
    /// `Arrival`.
    Arrival,
    /// `Landed`.
    Landed,
    /// `Parked`.
    Parked,
}

/// Shared aircraft state used by both pilot and traffic.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AircraftState {
    /// Stable identity.
    pub id: u32,
    /// Radio callsign.
    pub callsign: String,
    /// Pilot or traffic.
    pub kind: AircraftKind,
    /// Control mode.
    pub control_mode: ControlMode,
    /// Current phase.
    pub phase: AircraftPhase,
    /// Tuned frequency in kHz, if any.
    pub frequency_khz: Option<i32>,
    /// Simulator seconds of the last progress.
    pub last_progress_seconds: f64,
    /// Number of recovery attempts.
    pub recoveries: u8,
}

impl AircraftState {
    /// Whether this aircraft is the player's own aircraft.
    #[must_use]
    pub fn is_pilot(&self) -> bool {
        self.kind == AircraftKind::Pilot
    }

    /// Whether this aircraft is simulated traffic.
    #[must_use]
    pub fn is_traffic(&self) -> bool {
        self.kind == AircraftKind::Traffic
    }
}

/// Why an aircraft is waiting on the surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HoldReason {
    /// Another aircraft is occupying the path ahead.
    Traffic { blocker: u32, gap_meters: u32 },
    /// A runway crossing or line-up needs a controller clearance.
    Runway { runway: String },
    /// The aircraft is waiting for a radio clearance.
    Clearance,
    /// The destination stand is occupied.
    Parking,
}

/// Deterministic surface movement decision owned by the aircraft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceDecision {
    /// Proceed along the assigned route.
    Continue,
    /// Follow the aircraft ahead at a safe gap.
    Follow { leader: u32, gap_meters: u32 },
    /// Hold for traffic or a runway clearance.
    Hold { reason: HoldReason },
}

/// Events that can be delivered to an aircraft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AircraftEvent {
    /// Startup complete.
    StartupComplete,
    /// Pushback complete.
    PushbackComplete,
    /// IFR clearance received.
    IfrClearanceReceived,
    /// Frequency changed.
    FrequencyChanged,
    /// Phase timeout.
    PhaseTimeout,
    /// Transition to arrival.
    Arrival,
    /// Transition to departure.
    Departure,
    /// Transition to approach.
    Approach,
    /// Transition to cruise.
    Cruise,
    /// Transition to landed.
    Landed,
    /// Transition to taxi.
    Taxi,
    /// Transition to taxi in.
    TaxiIn,
    /// Transition to finished.
    Finished,
    /// Transition to parked.
    Parked,
}

/// Outcome of handling an event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AircraftOutcome {
    /// No action taken.
    #[default]
    None,
    /// The aircraft advanced to a new phase.
    Advanced(AircraftPhase),
    /// The aircraft needs AI assistance.
    NeedsAi,
    /// The aircraft can proceed instantly.
    Instant,
}

/// Shared behaviour implemented by both pilot and traffic. Default methods are no-ops so
/// migration can happen incrementally.
pub trait AircraftBehaviour {
    /// Shared state for this aircraft.
    fn state(&self) -> &AircraftState;
    /// Mutable shared state for this aircraft.
    fn state_mut(&mut self) -> &mut AircraftState;

    /// Handle an aircraft event.
    fn on_event(&mut self, event: AircraftEvent) -> AircraftOutcome {
        let _ = event;
        AircraftOutcome::None
    }

    /// Advance the aircraft by the given number of seconds.
    fn advance(&mut self, _seconds: f64) -> AircraftOutcome {
        AircraftOutcome::None
    }

    /// Whether the aircraft can proceed without AI.
    fn can_proceed_instantly(&self) -> bool {
        self.state().control_mode == ControlMode::Traffic
    }

    /// Deterministic surface decision for the traffic around this aircraft.
    fn surface_decision(&self, _traffic: &[crate::state::TrafficTarget]) -> SurfaceDecision {
        SurfaceDecision::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dummy {
        state: AircraftState,
    }

    impl AircraftBehaviour for Dummy {
        fn state(&self) -> &AircraftState {
            &self.state
        }

        fn state_mut(&mut self) -> &mut AircraftState {
            &mut self.state
        }
    }

    #[test]
    fn traffic_can_proceed_instantly_by_default() {
        let mut dummy = Dummy {
            state: AircraftState {
                control_mode: ControlMode::Traffic,
                ..AircraftState::default()
            },
        };
        assert!(dummy.can_proceed_instantly());
        assert_eq!(
            dummy.on_event(AircraftEvent::StartupComplete),
            AircraftOutcome::None
        );
    }

    #[test]
    fn pilot_does_not_proceed_instantly_by_default() {
        let dummy = Dummy {
            state: AircraftState {
                control_mode: ControlMode::Pilot,
                ..AircraftState::default()
            },
        };
        assert!(!dummy.can_proceed_instantly());
    }
}
