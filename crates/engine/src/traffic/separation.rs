//! Physical ground separation shared by every surface phase.

use openatc_core::state::TrafficTarget;

/// Required centre-to-centre gap for two aircraft nose-to-tail.
#[must_use]
pub(crate) fn required_gap(own_length: f64, other_length: f64) -> f64 {
    ((own_length + other_length) / 2.0 + 15.0).clamp(45.0, 80.0)
}

/// True when another ground target is physically ahead inside the protected gap.
///
/// Compares current positions and headings. Route distance is not a reliable ordering
/// signal when aircraft join the same taxiway from different nodes.
#[must_use]
pub(crate) fn ahead_conflict(
    latitude: f64,
    longitude: f64,
    heading: f64,
    own_length: f64,
    own_id: u32,
    traffic: &[TrafficTarget],
) -> bool {
    for other in traffic
        .iter()
        .filter(|target| target.id != own_id && target.on_ground)
    {
        if !other.latitude.is_finite() || !other.longitude.is_finite() {
            continue;
        }
        let other_length = openatc_core::traffic::profiles::aircraft(&other.aircraft)
            .map_or(40.0, |profile| profile.length);
        let gap = openatc_core::arrival::distance_nm(
            [latitude, longitude],
            [other.latitude, other.longitude],
        ) * 1852.0;
        if gap >= required_gap(own_length, other_length) {
            continue;
        }
        let bearing = openatc_core::arrival::bearing(
            [latitude, longitude],
            [other.latitude, other.longitude],
        );
        let relative = (bearing - heading).rem_euclid(360.0);
        if relative >= 45.0 && relative <= 315.0 {
            continue;
        }
        let track_difference = openatc_core::navigation::turn(heading, other.track_degrees).abs();
        // Same-direction traffic is a follow: the aircraft behind always yields.
        // Converging or head-on traffic is resolved once by ID so both cannot stop.
        if track_difference >= 45.0 && own_id < other.id {
            continue;
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: u32, longitude: f64, track: f64) -> TrafficTarget {
        TrafficTarget {
            id,
            aircraft: "A320".into(),
            latitude: 0.0,
            longitude,
            track_degrees: track,
            on_ground: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_follower_stops_behind_a_leader_on_the_same_taxiway() {
        assert!(ahead_conflict(
            0.0,
            0.0,
            90.0,
            37.0,
            1,
            &[target(2, 0.0002, 90.0)]
        ));
        assert!(!ahead_conflict(
            0.0,
            0.0,
            90.0,
            37.0,
            1,
            &[target(2, -0.0002, 90.0)]
        ));
    }

    #[test]
    fn converging_traffic_yields_once_instead_of_both_stopping() {
        let other = target(2, 0.0002, 180.0);
        assert!(!ahead_conflict(0.0, 0.0, 90.0, 37.0, 1, &[other.clone()]));
        assert!(ahead_conflict(0.0, 0.0, 90.0, 37.0, 3, &[other]));
    }

    #[test]
    fn required_gap_uses_aircraft_lengths() {
        assert!(required_gap(37.0, 27.0) > 45.0);
        assert!(required_gap(8.0, 8.0) >= 45.0);
        assert!(required_gap(74.0, 74.0) <= 80.0);
    }
}
