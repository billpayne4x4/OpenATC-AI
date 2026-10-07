//! Check accepted altitude, speed and frequency instructions against telemetry.
//! Route checks compare filed route strings rather than flown geometry.

use super::state::Telemetry;

/// What ATC expects the pilot to be doing right now.
#[derive(Clone, Debug, Default)]
pub struct Expectation {
    /// Cleared altitude feet, if any.
    pub altitude_feet: Option<i32>,
    /// Cleared direct-to waypoint (uppercased), if any.
    pub waypoint: Option<String>,
    /// Assigned speed knots, if any.
    pub speed_knots: Option<f64>,
    /// Handoff frequency kHz the pilot should have tuned, if any.
    pub expected_frequency_khz: Option<i32>,
    /// Ticks since the pilot accepted; complaints need grace (see [`GRACE_TICKS`]).
    pub ticks_since_accept: u32,
}

/// Ticks after accept before any complaint may fire.
pub const GRACE_TICKS: u32 = 3;

/// Altitude tolerance feet before it counts as a bust.
pub const ALTITUDE_TOLERANCE_FEET: f64 = 300.0;

/// Speed tolerance knots before it counts as non-compliance.
pub const SPEED_TOLERANCE_KNOTS: f64 = 20.0;

/// Check telemetry against the expectation. Returns the dotted speech id of
/// the complaint to read, or `None` when the pilot complies (or grace covers
/// the deviation). `plan_route` is the currently filed route string, used for
/// the direct-to check.
#[must_use]
pub fn check_compliance(
    telemetry: &Telemetry,
    plan_route: &str,
    expectation: &Expectation,
) -> Option<&'static str> {
    if expectation.ticks_since_accept < GRACE_TICKS {
        return None;
    }
    if let Some(cleared) = expectation.altitude_feet
        && !telemetry.on_ground
        && (telemetry.altitude_feet - f64::from(cleared)).abs() > ALTITUDE_TOLERANCE_FEET
    {
        return Some("complaint.altitude_bust");
    }
    if let Some(waypoint) = &expectation.waypoint
        && !waypoint.is_empty()
        && !plan_route.contains(waypoint.as_str())
    {
        return Some("complaint.off_route");
    }
    if let Some(speed) = expectation.speed_knots
        && (telemetry.ground_speed_knots - speed).abs() > SPEED_TOLERANCE_KNOTS
    {
        return Some("complaint.speed");
    }
    if let Some(frequency) = expectation.expected_frequency_khz
        && telemetry.com1_khz != 0
        && telemetry.com1_khz != frequency
    {
        return Some("complaint.frequency");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(altitude: f64) -> Telemetry {
        Telemetry {
            on_ground: false,
            altitude_feet: altitude,
            ground_speed_knots: 250.0,
            ..Default::default()
        }
    }

    #[test]
    fn grace_covers_fresh_accepts() {
        let expectation = Expectation {
            altitude_feet: Some(5000),
            ticks_since_accept: 0,
            ..Default::default()
        };
        assert_eq!(check_compliance(&level(9000.0), "", &expectation), None);
    }

    #[test]
    fn altitude_bust_fires_after_grace() {
        let expectation = Expectation {
            altitude_feet: Some(5000),
            ticks_since_accept: 5,
            ..Default::default()
        };
        assert_eq!(
            check_compliance(&level(9000.0), "", &expectation),
            Some("complaint.altitude_bust")
        );
        assert_eq!(check_compliance(&level(5100.0), "", &expectation), None);
    }

    #[test]
    fn ground_ops_never_bust_altitude() {
        let ground = Telemetry {
            on_ground: true,
            altitude_feet: 0.0,
            ..Default::default()
        };
        let expectation = Expectation {
            altitude_feet: Some(5000),
            ticks_since_accept: 9,
            ..Default::default()
        };
        assert_eq!(check_compliance(&ground, "", &expectation), None);
    }

    #[test]
    fn off_route_checks_filed_route() {
        let expectation = Expectation {
            waypoint: Some("PELIN".to_owned()),
            ticks_since_accept: 9,
            ..Default::default()
        };
        assert_eq!(
            check_compliance(&level(5000.0), "YMLT PELIN YMML", &expectation),
            None
        );
        assert_eq!(
            check_compliance(&level(5000.0), "YMLT WOL YMML", &expectation),
            Some("complaint.off_route")
        );
    }

    #[test]
    fn speed_non_compliance_fires() {
        let expectation = Expectation {
            speed_knots: Some(180.0),
            ticks_since_accept: 9,
            ..Default::default()
        };
        assert_eq!(
            check_compliance(&level(5000.0), "", &expectation),
            Some("complaint.speed")
        );
    }

    #[test]
    fn frequency_non_compliance_fires() {
        let tuned = Telemetry {
            com1_khz: 118_100,
            ..level(5000.0)
        };
        let untuned = Telemetry {
            com1_khz: 121_900,
            ..level(5000.0)
        };
        let expectation = Expectation {
            expected_frequency_khz: Some(118_100),
            ticks_since_accept: 9,
            ..Default::default()
        };
        assert_eq!(check_compliance(&tuned, "", &expectation), None);
        assert_eq!(
            check_compliance(&untuned, "", &expectation),
            Some("complaint.frequency")
        );
        // Silent radio (COM1 unread) never complains.
        let dark = Telemetry {
            com1_khz: 0,
            ..level(5000.0)
        };
        assert_eq!(check_compliance(&dark, "", &expectation), None);
    }
}
