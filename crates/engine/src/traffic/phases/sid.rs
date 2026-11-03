//! Initial climb and SID speed.

use openatc_core::traffic::animation::Handling;
use openatc_core::traffic::profiles::Aircraft as Profile;

/// Nominal climb speed as a fraction of cruise speed.
///
/// Approach and rotation speeds are too low for a clean climb, especially for
/// jets. Using a fraction of cruise speed keeps the climb gradient realistic
/// while light aircraft remain limited by their approach-derived minimum.
const CLIMB_CRUISE_FRACTION: f64 = 0.55;

/// Lowest safe climb speed derived from approach and rotation speed.
pub(crate) fn minimum_climb_speed(profile: &Profile, handling: &Handling) -> f64 {
    (profile.approach_knots + 10.0).max(handling.rotation_knots + 10.0) * 0.514_444
}

/// Climb speed after rotation.
pub(crate) fn climb_speed(profile: &Profile, handling: &Handling) -> f64 {
    minimum_climb_speed(profile, handling)
        .max(profile.cruise_knots * CLIMB_CRUISE_FRACTION * 0.514_444)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jet_climb_speed_uses_a_clean_climb_speed_not_approach_speed() {
        let handling = openatc_core::traffic::animation::builtin().expect("handling");
        let jet = handling
            .get(&openatc_core::traffic::animation::Kind::Jet)
            .expect("jet handling");
        let a320 = openatc_core::traffic::profiles::aircraft("A320").expect("A320");
        let knots = climb_speed(a320, jet) / 0.514_444;
        assert!(
            knots > 200.0,
            "a jet must not climb at approach speed: {knots}"
        );
        let gradient = a320.climb_fpm * 60.0 / knots;
        assert!(
            (450.0..=650.0).contains(&gradient),
            "jet climb gradient {gradient} ft/NM"
        );
    }

    #[test]
    fn light_aircraft_keep_their_approach_derived_climb_speed() {
        let handling = openatc_core::traffic::animation::builtin().expect("handling");
        let piston = handling
            .get(&openatc_core::traffic::animation::Kind::Piston)
            .expect("piston handling");
        let c172 = openatc_core::traffic::profiles::aircraft("C172").expect("C172");
        let knots = climb_speed(c172, piston) / 0.514_444;
        assert!((65.0..=90.0).contains(&knots), "C172 climb speed {knots}");
    }
}
