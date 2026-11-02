//! runway exit and taxi to the next hold.

use crate::traffic::phase::TrafficPhase;

/// Phase implemented by this planner.
#[allow(dead_code)]
pub(crate) const PHASE: TrafficPhase = TrafficPhase::RunwayExit;
