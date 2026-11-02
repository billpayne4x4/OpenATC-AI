//! Traffic phase names shared by the manager and the phase planners.

use openatc_core::traffic::motion::Stage;

/// One stage of a traffic aircraft's life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum TrafficPhase {
    /// Parked at a stand.
    AtGate,
    /// Pushed back from the stand.
    Pushback,
    /// Taxiing out to the departure runway.
    TaxiOut,
    /// Holding short of a runway.
    Hold,
    /// Crossing one runway.
    Crossing,
    /// Lined up and waiting for takeoff clearance.
    LineUp,
    /// Takeoff roll.
    Takeoff,
    /// Initial climb after rotation.
    InitialClimb,
    /// Following the SID.
    Sid,
    /// Climbing to cruise.
    Climb,
    /// En-route cruise.
    Cruise,
    /// Airborne holding pattern.
    AirborneHold,
    /// Descending towards the STAR.
    Descent,
    /// Following the STAR.
    Star,
    /// Instrument approach before the final segment.
    Approach,
    /// Established on final approach.
    Final,
    /// Landing flare and touchdown.
    Landing,
    /// Landing roll and runway exit.
    Rollout,
    /// Leaving the runway.
    RunwayExit,
    /// Taxiing to the arrival stand.
    TaxiIn,
    /// Arrived at the stand.
    Gate,
    /// Local VFR circuit.
    Pattern,
    /// Go-around or missed approach.
    Recovery,
}

impl TrafficPhase {
    /// Map the shared movement stage to a traffic phase.
    #[must_use]
    pub(crate) fn from_stage(stage: Stage) -> Self {
        match stage {
            Stage::Parked => Self::AtGate,
            Stage::Pushback => Self::Pushback,
            Stage::TaxiOut => Self::TaxiOut,
            Stage::Crossing => Self::Crossing,
            Stage::LineUp => Self::LineUp,
            Stage::Takeoff => Self::Takeoff,
            Stage::Climb => Self::Climb,
            Stage::Cruise => Self::Cruise,
            Stage::Holding => Self::AirborneHold,
            Stage::Descent => Self::Descent,
            Stage::Approach => Self::Approach,
            Stage::MissedApproach => Self::Recovery,
            Stage::Rollout => Self::Rollout,
            Stage::TaxiIn => Self::TaxiIn,
            Stage::Turnaround => Self::Gate,
        }
    }

    /// Stable label for diagnostics.
    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::AtGate => "AtGate",
            Self::Pushback => "Pushback",
            Self::TaxiOut => "TaxiOut",
            Self::Hold => "Hold",
            Self::Crossing => "Crossing",
            Self::LineUp => "LineUp",
            Self::Takeoff => "Takeoff",
            Self::InitialClimb => "InitialClimb",
            Self::Sid => "Sid",
            Self::Climb => "Climb",
            Self::Cruise => "Cruise",
            Self::AirborneHold => "AirborneHold",
            Self::Descent => "Descent",
            Self::Star => "Star",
            Self::Approach => "Approach",
            Self::Final => "Final",
            Self::Landing => "Landing",
            Self::Rollout => "Rollout",
            Self::RunwayExit => "RunwayExit",
            Self::TaxiIn => "TaxiIn",
            Self::Gate => "Gate",
            Self::Pattern => "Pattern",
            Self::Recovery => "Recovery",
        }
    }
}
