//! Initial climb and SID speed.

use openatc_core::traffic::animation::Handling;
use openatc_core::traffic::profiles::Aircraft as Profile;

/// Climb speed after rotation.
pub(crate) fn climb_speed(profile: &Profile, handling: &Handling) -> f64 {
    (profile.approach_knots + 10.0).max(handling.rotation_knots + 10.0) * 0.514_444
}
