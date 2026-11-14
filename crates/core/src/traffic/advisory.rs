//! Relative motion for traffic calls. These thresholds trigger advice, not separation clearance.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use crate::state::{Telemetry, TrafficTarget};

/// Current relative position and the closest predicted horizontal encounter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Encounter {
    /// Clock position relative to the aircraft's heading, from one to twelve.
    pub clock: u8,
    /// Current horizontal range, nautical miles.
    pub distance_nm: f64,
    /// Signed altitude difference, traffic minus ownship.
    pub relative_altitude_feet: f64,
    /// Time to closest approach, bounded to the prediction horizon.
    pub seconds: f64,
    /// Predicted horizontal range at closest approach.
    pub closest_nm: f64,
    /// Predicted vertical difference at closest horizontal approach.
    pub vertical_feet: f64,
    /// Positive when range is decreasing.
    pub closing_knots: f64,
    /// Traffic's direction of travel, in 45-degree compass sectors.
    pub direction: u8,
}

/// Finite coordinates from a live surveillance target.
#[must_use]
pub fn valid(target: &TrafficTarget) -> bool {
    (-90.0..=90.0).contains(&target.latitude)
        && (-180.0..=180.0).contains(&target.longitude)
        && (-2000.0..=100_000.0).contains(&target.altitude_feet)
        && target.track_degrees.is_finite()
        && target
            .ground_speed_knots
            .is_none_or(|v| (0.0..=2000.0).contains(&v))
}

/// Predict constant-velocity relative motion for up to two minutes.
/// Target climb is optional because not every traffic provider exposes it.
#[must_use]
pub fn encounter(
    own: &Telemetry,
    other: &TrafficTarget,
    climb_fpm: Option<f64>,
) -> Option<Encounter> {
    if own.on_ground
        || other.on_ground
        || !own.position_valid
        || !valid(other)
        || !(-90.0..=90.0).contains(&own.latitude)
        || !(-180.0..=180.0).contains(&own.longitude)
        || !own.altitude_feet.is_finite()
        || !own.heading_degrees.is_finite()
        || !(0.0..=2000.0).contains(&own.ground_speed_knots)
        || !own.vertical_speed_fpm.is_finite()
    {
        return None;
    }
    let range =
        crate::ops::distance_nm(own.latitude, own.longitude, other.latitude, other.longitude);
    if range > 40.0 {
        return None;
    }
    let bearing = crate::arrival::bearing(
        [own.latitude, own.longitude],
        [other.latitude, other.longitude],
    );
    let (e, n) = (
        range * bearing.to_radians().sin(),
        range * bearing.to_radians().cos(),
    );
    let own_track = own.ground_track_degrees.unwrap_or(own.heading_degrees);
    if !own_track.is_finite() {
        return None;
    }
    let velocity = |speed: f64, degrees: f64| {
        (
            speed * degrees.to_radians().sin() / 3600.0,
            speed * degrees.to_radians().cos() / 3600.0,
        )
    };
    let (oe, on) = velocity(own.ground_speed_knots, own_track);
    let known_speed = other.ground_speed_knots.is_some();
    let (te, tn) = velocity(other.ground_speed_knots.unwrap_or(0.0), other.track_degrees);
    let (ve, vn) = (te - oe, tn - on);
    let norm = ve * ve + vn * vn;
    let dot = e * ve + n * vn;
    let seconds = if known_speed && norm > 1e-10 {
        (-dot / norm).clamp(0.0, 120.0)
    } else {
        0.0
    };
    let relative_altitude_feet = other.altitude_feet - own.altitude_feet;
    let target_climb = climb_fpm
        .filter(|v| v.is_finite() && v.abs() <= 15000.0)
        .unwrap_or(0.0);
    // A missing traffic climb rate must not hide our own climb towards its level.
    let vertical_feet =
        relative_altitude_feet + (target_climb - own.vertical_speed_fpm) * seconds / 60.0;
    let relative_bearing = (bearing - own.heading_degrees).rem_euclid(360.0);
    let hour = (relative_bearing / 30.0).round() as u8 % 12;
    Some(Encounter {
        clock: if hour == 0 { 12 } else { hour },
        distance_nm: range,
        relative_altitude_feet,
        seconds,
        closest_nm: (e + ve * seconds).hypot(n + vn * seconds),
        vertical_feet,
        closing_knots: if known_speed && range > 0.001 {
            -dot / range * 3600.0
        } else {
            0.0
        },
        direction: (other.track_degrees.rem_euclid(360.0) / 45.0).round() as u8 % 8,
    })
}

impl Encounter {
    /// Early warning only for traffic that is closing inside the protected volume.
    /// En-route calls start farther out than terminal calls; these are simulator heuristics.
    #[must_use]
    pub fn warrants_advisory(self, enroute: bool) -> bool {
        let horizontal = if enroute { 5.0 } else { 3.0 };
        let predicted = self.closing_knots > 5.0
            && self.seconds > 0.0
            && self.seconds <= 120.0
            && self.closest_nm < horizontal
            && self.vertical_feet.abs() < 1_000.0;
        let immediate = self.distance_nm < 0.75 && self.relative_altitude_feet.abs() < 500.0;
        predicted || immediate
    }

    /// An immediate proximity warning without an unvalidated avoidance heading.
    #[must_use]
    pub fn urgent(self) -> bool {
        (self.distance_nm < 1.0 && self.relative_altitude_feet.abs() < 500.0)
            || (self.closing_knots > 20.0
                && self.seconds > 0.0
                && self.seconds < 30.0
                && self.closest_nm < 0.5
                && self.vertical_feet.abs() < 500.0)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    fn own() -> Telemetry {
        Telemetry {
            latitude: 0.0,
            longitude: 0.0,
            position_valid: true,
            altitude_feet: 5000.0,
            on_ground: false,
            heading_degrees: 0.0,
            ground_speed_knots: 150.0,
            ..Default::default()
        }
    }
    fn target() -> TrafficTarget {
        TrafficTarget {
            latitude: 8.0 / 60.0,
            altitude_feet: 5000.0,
            ground_speed_knots: Some(150.0),
            track_degrees: 180.0,
            ..Default::default()
        }
    }
    #[test]
    fn head_on_is_called_before_a_fixed_three_mile_radius() {
        let e = encounter(&own(), &target(), Some(0.0)).unwrap();
        assert_eq!(e.clock, 12);
        assert!((90.0..100.0).contains(&e.seconds));
        assert!(e.closest_nm < 0.01 && e.warrants_advisory(false));
        assert!(!e.urgent());
    }
    #[test]
    fn own_climb_is_considered_without_a_traffic_climb_rate() {
        let mut own = own();
        own.vertical_speed_fpm = 2000.0;
        let mut other = target();
        other.altitude_feet = 8200.0;
        let predicted = encounter(&own, &other, None).unwrap();
        assert!(predicted.relative_altitude_feet > 3000.0);
        assert!(predicted.vertical_feet.abs() < 100.0);
        assert!(predicted.warrants_advisory(false));
        own.vertical_speed_fpm = 0.0;
        assert!(
            !encounter(&own, &other, None)
                .unwrap()
                .warrants_advisory(false)
        );
    }
    #[test]
    fn clock_uses_heading_not_track_and_handles_north_wrap() {
        let mut t = own();
        t.heading_degrees = 90.0;
        t.ground_track_degrees = Some(0.0);
        assert_eq!(encounter(&t, &target(), None).unwrap().clock, 9);
        t.heading_degrees = 359.0;
        assert_eq!(encounter(&t, &target(), None).unwrap().clock, 12);
    }
    #[test]
    fn vertical_crossing_and_divergence_change_the_advice() {
        let mut t = target();
        t.altitude_feet = 8000.0;
        assert!(
            !encounter(&own(), &t, Some(0.0))
                .unwrap()
                .warrants_advisory(false)
        );
        assert!(
            encounter(&own(), &t, Some(-1800.0))
                .unwrap()
                .warrants_advisory(false)
        );
        t.altitude_feet = 5000.0;
        t.track_degrees = 0.0;
        t.ground_speed_knots = Some(200.0);
        assert!(
            !encounter(&own(), &t, None)
                .unwrap()
                .warrants_advisory(false)
        );
    }
    #[test]
    fn bad_ground_or_unknown_motion_cannot_invent_a_collision() {
        let mut t = target();
        t.ground_speed_knots = None;
        let e = encounter(&own(), &t, None).unwrap();
        assert_eq!(e.seconds, 0.0);
        assert!(!e.warrants_advisory(false));
        t.longitude = f64::NAN;
        assert!(encounter(&own(), &t, None).is_none());
        t = target();
        t.on_ground = true;
        assert!(encounter(&own(), &t, None).is_none());
    }
}
