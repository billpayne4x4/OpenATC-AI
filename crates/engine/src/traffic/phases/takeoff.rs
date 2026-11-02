//! Line-up and takeoff roll decisions.

use openatc_core::airport::Airport;
use openatc_core::traffic::animation::Handling;

/// Speed through the takeoff roll. A short runway borrows extra acceleration so
/// Vr is still met at the rotation point.
pub(crate) fn roll_speed(
    handling: &Handling,
    distance: f64,
    runway_start: f64,
    rotation_end: f64,
    taxi: f64,
) -> f64 {
    let rotation = handling.rotation_knots * 0.514_444;
    let roll = (rotation_end - runway_start).max(1.0);
    let required = (rotation.powi(2) - taxi.powi(2)).max(0.0) / (2.0 * roll);
    let acceleration = required.max(handling.acceleration_knots_sec * 0.514_444);
    let rolled = (distance - runway_start).max(0.0);
    (taxi.powi(2) + 2.0 * acceleration * rolled)
        .sqrt()
        .min(rotation)
}

/// Distance from the threshold needed to accelerate to the rotation speed.
pub(crate) fn rotation_meters(handling: &Handling, airport: &Airport, runway: &str) -> f64 {
    let length = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)
        .map_or(2_000.0, |runway| {
            (runway.second.east - runway.first.east).hypot(runway.second.north - runway.first.north)
        });
    let taxi = handling.taxi_turn_knots.max(1.0);
    let acceleration = handling.acceleration_knots_sec.max(0.1);
    let required = ((handling.rotation_knots.powi(2) - taxi.powi(2)).max(0.0)
        / (2.0 * acceleration))
        * 0.514_444;
    required.clamp(300.0, (length - 150.0).max(300.0))
}
