//! Local VFR circuit (traffic pattern) generation for GA and rotorcraft.
//!
//! References: FAA AIM 4-3-3, AC 90-66B. Propeller aircraft enter at 1000 ft AGL,
 //! turbine at 1500 ft AGL, rotorcraft at 500 ft AGL. Standard pattern is left-hand
 //! unless right traffic is published. Entry is 45 degrees to downwind at midfield.

use openatc_core::airport::Airport;
use openatc_core::arrival::{bearing, destination, distance_nm};
use openatc_core::traffic::animation::{Handling, Kind};
use openatc_core::traffic::motion::Waypoint;
use openatc_core::traffic::profiles::Aircraft as Profile;

/// Pattern altitude in feet AGL by aircraft handling type.
pub(crate) fn pattern_altitude_agl(handling: Kind) -> f64 {
    match handling {
        Kind::Piston => 1000.0,
        Kind::Turboprop => 1500.0,
        Kind::RegionalJet => 1500.0,
        Kind::Jet => 1500.0,
        Kind::HeavyJet => 1500.0,
        Kind::Rotorcraft => 500.0,
    }
}

/// Whether the pattern uses right-hand turns. Default left; airport-specific data can override.
pub(crate) fn right_traffic(_airport: &Airport, _runway: &str) -> bool {
    // Navdata/Chart Supplement would provide right-pattern data. Default to left.
    false
}

/// Turn direction for the pattern: +1 for left, -1 for right.
pub(crate) fn pattern_turn_direction(right: bool) -> f64 {
    if right { -1.0 } else { 1.0 }
}

/// Build the VFR traffic pattern waypoints around a runway.
///
/// Returns legs in order: departure, upwind, crosswind, downwind, base, final.
/// Each waypoint is at pattern altitude AGL except departure/upwind which climb through.
pub(crate) fn pattern_waypoints(
    airport: &Airport,
    runway: &str,
    profile: &Profile,
    _handling: &Handling,
) -> Option<Vec<Waypoint>> {
    let rw = airport.runways.iter().find(|r| {
        r.first_name == runway || r.second_name == runway
    })?;
    let (threshold, rollout_end) = (rw.first, rw.second);
    // Use the first end as the landing threshold for simplicity.
    let threshold_lat = airport.reference_latitude + threshold.north / 111_320.0;
    let threshold_lon = airport.reference_longitude
        + threshold.east / (111_320.0 * airport.reference_latitude.to_radians().cos().max(0.01));
    let runway_heading = bearing([threshold_lat, threshold_lon], [
        airport.reference_latitude + rollout_end.north / 111_320.0,
        airport.reference_longitude
            + rollout_end.east / (111_320.0 * airport.reference_latitude.to_radians().cos().max(0.01)),
    ]);
    let agl = pattern_altitude_agl(profile.handling.unwrap_or(Kind::Jet));
    let pattern_alt = airport.elevation_feet + agl;
    let turn_dir = pattern_turn_direction(right_traffic(airport, runway));
    let downwind_offset_nm = 0.5;
    let base_turn_nm = 0.5;
    let final_turn_nm = 0.25;

    let mut waypoints = Vec::new();

    // Departure: climb straight ahead along extended centerline.
    let departure = destination([threshold_lat, threshold_lon], runway_heading, 1.0 * 1852.0);
    waypoints.push(Waypoint {
        latitude: departure[0],
        longitude: departure[1],
        altitude_feet: pattern_alt,
    });

    // Upwind: continue straight ahead beyond departure end.
    let upwind = destination([threshold_lat, threshold_lon], runway_heading, 2.0 * 1852.0);
    waypoints.push(Waypoint {
        latitude: upwind[0],
        longitude: upwind[1],
        altitude_feet: pattern_alt,
    });

    // Crosswind: 90-degree turn from upwind.
    let crosswind_heading = (runway_heading + 90.0 * turn_dir).rem_euclid(360.0);
    let crosswind = destination([upwind[0], upwind[1]], crosswind_heading, downwind_offset_nm * 1852.0);
    waypoints.push(Waypoint {
        latitude: crosswind[0],
        longitude: crosswind[1],
        altitude_feet: pattern_alt,
    });

    // Downwind: parallel to runway, opposite direction.
    let downwind_heading = (runway_heading + 180.0).rem_euclid(360.0);
    let downwind = destination([crosswind[0], crosswind[1]], downwind_heading, 1.0 * 1852.0);
    waypoints.push(Waypoint {
        latitude: downwind[0],
        longitude: downwind[1],
        altitude_feet: pattern_alt,
    });

    // Base: 90-degree turn from downwind toward the runway.
    let base_heading = (downwind_heading + 90.0 * turn_dir).rem_euclid(360.0);
    let base = destination([downwind[0], downwind[1]], base_heading, base_turn_nm * 1852.0);
    waypoints.push(Waypoint {
        latitude: base[0],
        longitude: base[1],
        altitude_feet: pattern_alt,
    });

    // Final: aligned with runway heading, at least 1/4 mile from threshold.
    let final_wp = destination([threshold_lat, threshold_lon], runway_heading, final_turn_nm * 1852.0);
    waypoints.push(Waypoint {
        latitude: final_wp[0],
        longitude: final_wp[1],
        altitude_feet: pattern_alt,
    });

    Some(waypoints)
}

/// Build a 45-degree entry to downwind from midfield.
pub(crate) fn pattern_entry_waypoints(
    airport: &Airport,
    runway: &str,
    profile: &Profile,
    _arrival_bearing_deg: f64,
) -> Option<Vec<Waypoint>> {
    let rw = airport.runways.iter().find(|r| {
        r.first_name == runway || r.second_name == runway
    })?;
    let threshold = rw.first;
    let threshold_lat = airport.reference_latitude + threshold.north / 111_320.0;
    let threshold_lon = airport.reference_longitude
        + threshold.east / (111_320.0 * airport.reference_latitude.to_radians().cos().max(0.01));
    let agl = pattern_altitude_agl(profile.handling.unwrap_or(Kind::Jet));
    let pattern_alt = airport.elevation_feet + agl;
    let midfield_lat = threshold_lat;
    let midfield_lon = threshold_lon;

    let mut waypoints = Vec::new();

    // Cross midfield at pattern altitude + 500 ft, then descend and enter at 45 degrees.
    let crossing_alt = pattern_alt + 500.0;
    waypoints.push(Waypoint {
        latitude: midfield_lat,
        longitude: midfield_lon,
        altitude_feet: crossing_alt,
    });

    // 45-degree entry to downwind.
    let downwind_heading = (bearing([threshold_lat, threshold_lon], [
        airport.reference_latitude + rw.second.north / 111_320.0,
        airport.reference_longitude
            + rw.second.east / (111_320.0 * airport.reference_latitude.to_radians().cos().max(0.01)),
    ]) + 180.0)
        .rem_euclid(360.0);
    let entry_heading = (downwind_heading + 45.0 * -1.0).rem_euclid(360.0); // left traffic entry from downwind side
    let entry = destination([midfield_lat, midfield_lon], entry_heading, 1.0 * 1852.0);
    waypoints.push(Waypoint {
        latitude: entry[0],
        longitude: entry[1],
        altitude_feet: pattern_alt,
    });

    Some(waypoints)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::airport::{Airport, Runway};
    use openatc_core::state::Point;
    use openatc_core::traffic::animation::builtin;

    fn test_airport() -> Airport {
        Airport {
            icao: "KSQL".into(),
            reference_latitude: 37.51,
            reference_longitude: -122.28,
            elevation_feet: 25.0,
            runways: vec![Runway {
                first_name: "30L".into(),
                second_name: "12R".into(),
            first: Point { east: 0.0, north: 0.0, height: 0.0 },
            second: Point { east: 2000.0, north: 1000.0, height: 0.0 },
                width: 45.0,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn pattern_altitude_by_handling_type() {
        assert_eq!(pattern_altitude_agl(Kind::Piston), 1000.0);
        assert_eq!(pattern_altitude_agl(Kind::Rotorcraft), 500.0);
        assert_eq!(pattern_altitude_agl(Kind::Jet), 1500.0);
    }

    #[test]
    fn pattern_waypoints_returns_six_legs() {
        let airport = test_airport();
        let profile = openatc_core::traffic::profiles::aircraft("C172").expect("C172");
        let waypoints = pattern_waypoints(
            &airport,
            "30L",
            profile,
            &openatc_core::traffic::animation::builtin()
                .expect("handling")
                .get(&Kind::Piston)
                .expect("piston")
                .clone(),
        );
        assert!(waypoints.is_some());
        let legs = waypoints.unwrap();
        assert_eq!(legs.len(), 6);
        // All pattern legs should be at pattern altitude (elevation + 1000 ft AGL for piston).
        for leg in &legs {
            assert!(
                (leg.altitude_feet - 1025.0).abs() < 1.0,
                "leg altitude {}",
                leg.altitude_feet
            );
        }
    }

    #[test]
    fn pattern_entry_returns_two_waypoints() {
        let airport = test_airport();
        let profile = openatc_core::traffic::profiles::aircraft("C172").expect("C172");
        let entry = pattern_entry_waypoints(&airport, "30L", profile, 90.0);
        assert!(entry.is_some());
        let legs = entry.unwrap();
        assert_eq!(legs.len(), 2);
    }
}
