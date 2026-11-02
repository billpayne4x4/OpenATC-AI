//! Final approach speed.

use openatc_core::traffic::profiles::Aircraft as Profile;

/// Nominal approach speed for a profile.
pub(crate) fn speed(profile: &Profile) -> f64 {
    profile.approach_knots * 0.514_444
}
