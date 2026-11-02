//! Landing flare, touchdown and rollout decisions.

use openatc_core::traffic::animation::Handling;
use openatc_core::traffic::profiles::Aircraft as Profile;

/// Speed during the landing roll, braking harder only when the exit is close.
pub(crate) fn rollout_speed(
    handling: &Handling,
    distance: f64,
    ground_start: f64,
    runway_end: f64,
    taxi: f64,
    touchdown: f64,
) -> f64 {
    let roll = (runway_end - ground_start).max(1.0);
    let required = ((touchdown.powi(2) - taxi.powi(2)).max(0.0) / (2.0 * roll)).max(0.1);
    let braking = (handling.braking_knots_sec * 0.514_444).max(required);
    let rolled = (distance - ground_start).max(0.0);
    (touchdown.powi(2) - 2.0 * braking * rolled)
        .max(taxi.powi(2))
        .sqrt()
}

/// Touchdown speed for a profile.
pub(crate) fn touchdown_speed(profile: &Profile) -> f64 {
    (profile.approach_knots - 5.0).max(45.0) * 0.514_444
}
