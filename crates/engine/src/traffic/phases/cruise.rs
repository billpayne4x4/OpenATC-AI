//! en-route cruise, speed and handoffs.

use crate::traffic::phase::TrafficPhase;

/// Phase implemented by this planner.
#[allow(dead_code)]
pub(crate) const PHASE: TrafficPhase = TrafficPhase::Cruise;
