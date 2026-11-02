//! go-around, missed approach and rejoin.

use crate::traffic::phase::TrafficPhase;

/// Phase implemented by this planner.
#[allow(dead_code)]
pub(crate) const PHASE: TrafficPhase = TrafficPhase::Recovery;
