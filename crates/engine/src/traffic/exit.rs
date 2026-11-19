//! Shared arrival exit selection.

use openatc_core::airport::Airport;

/// One selected arrival exit.
#[derive(Clone, Debug)]
pub(crate) struct ArrivalExitPlan {
    pub chosen: openatc_core::airport::ArrivalExit,
}

/// Choose the first usable exit that is clear of stopped traffic.
///
/// The departure entry is never used, an exit above the aircraft's safe
/// turn-off speed is skipped, and a stopped queue inside the exit throat moves
/// the plan to the next exit. A moving queue is assumed to clear.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub(crate) fn choose_arrival_exit(
    airport: &Airport,
    runway: &str,
    size: char,
    targets: &[openatc_core::state::TrafficTarget],
    along: f64,
    speed_knots: f64,
    minimum_rollout: f64,
    self_id: u32,
) -> Option<ArrivalExitPlan> {
    let mut viable = openatc_core::airport::arrival_exits(airport, runway, size)
        .into_iter()
        .filter(|exit| {
            if exit.entry || exit.along < minimum_rollout.max(along + 30.0) {
                return false;
            }
            let distance = (exit.along - along).max(0.0);
            distance >= 300.0 || speed_knots <= exit.maximum_knots + 5.0
        })
        .filter(|exit| !exit_blocked(airport, exit, targets, self_id));
    let chosen = viable.next()?;
    Some(ArrivalExitPlan { chosen })
}

fn exit_blocked(
    airport: &Airport,
    exit: &openatc_core::airport::ArrivalExit,
    targets: &[openatc_core::state::TrafficTarget],
    self_id: u32,
) -> bool {
    targets.iter().any(|target| {
        target.id != self_id
            && target.on_ground
            && target.latitude.is_finite()
            && target.longitude.is_finite()
            && {
                let point = openatc_core::airport::airport_point(
                    airport,
                    target.latitude,
                    target.longitude,
                );
                let gap = (point.east - exit.point.east).hypot(point.north - exit.point.north);
                gap < 150.0 && target.ground_speed_knots.unwrap_or(0.0) < 5.0
            }
    })
}
