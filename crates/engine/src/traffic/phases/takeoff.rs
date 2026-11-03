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
    let taxi = handling.taxi_knots.max(1.0);
    let acceleration = handling.acceleration_knots_sec.max(0.1);
    let required = ((handling.rotation_knots.powi(2) - taxi.powi(2)).max(0.0)
        / (2.0 * acceleration))
        * 0.514_444;
    required.clamp(300.0, (length - 150.0).max(300.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::airport::{Airport, Runway};
    use openatc_core::state::Point;

    fn airport() -> Airport {
        Airport {
            runways: vec![Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                first: Point::default(),
                second: Point {
                    east: 4_000.0,
                    ..Default::default()
                },
                width: 45.0,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn heavier_classes_use_more_runway_to_reach_rotation() {
        let handling = openatc_core::traffic::animation::builtin().expect("handling");
        let airport = airport();
        let piston = rotation_meters(
            handling
                .get(&openatc_core::traffic::animation::Kind::Piston)
                .expect("piston"),
            &airport,
            "09",
        );
        let jet = rotation_meters(
            handling
                .get(&openatc_core::traffic::animation::Kind::Jet)
                .expect("jet"),
            &airport,
            "09",
        );
        let heavy = rotation_meters(
            handling
                .get(&openatc_core::traffic::animation::Kind::HeavyJet)
                .expect("heavy"),
            &airport,
            "09",
        );
        assert!(piston < jet, "{piston} < {jet}");
        assert!(jet < heavy, "{jet} < {heavy}");
        assert!(heavy < 3_900.0, "heavy rotation must fit the runway");
    }
}
