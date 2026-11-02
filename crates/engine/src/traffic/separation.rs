//! Physical ground separation shared by every surface phase.

use openatc_core::state::TrafficTarget;

/// Required centre-to-centre gap for two aircraft nose-to-tail.
#[must_use]
pub(crate) fn required_gap(own_length: f64, other_length: f64) -> f64 {
    ((own_length + other_length) / 2.0 + 15.0).clamp(45.0, 80.0)
}

/// Closest aircraft physically ahead on the taxiway.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ahead {
    /// Target identity.
    pub(crate) id: u32,
    /// Centre-to-centre gap in metres.
    pub(crate) gap: f64,
    /// Gap needed at taxi speed.
    pub(crate) required: f64,
    /// Leader ground speed in metres per second.
    pub(crate) speed: f64,
}

/// Find the closest ground target ahead of this aircraft.
#[must_use]
pub(crate) fn closest_ahead(
    latitude: f64,
    longitude: f64,
    heading: f64,
    own_length: f64,
    own_id: u32,
    traffic: &[TrafficTarget],
) -> Option<Ahead> {
    let mut closest: Option<Ahead> = None;
    for other in traffic
        .iter()
        .filter(|target| target.id != own_id && target.on_ground)
    {
        if !other.latitude.is_finite() || !other.longitude.is_finite() {
            continue;
        }
        if other.runway_occupancy == Some(true) {
            continue;
        }
        let other_length = openatc_core::traffic::profiles::aircraft(&other.aircraft)
            .map_or(40.0, |profile| profile.length);
        let required = required_gap(own_length, other_length);
        let gap = openatc_core::arrival::distance_nm(
            [latitude, longitude],
            [other.latitude, other.longitude],
        ) * 1852.0;
        let bearing = openatc_core::arrival::bearing(
            [latitude, longitude],
            [other.latitude, other.longitude],
        );
        let relative = (bearing - heading).rem_euclid(360.0);
        if relative >= 45.0 && relative <= 315.0 {
            continue;
        }
        let candidate = Ahead {
            id: other.id,
            gap,
            required,
            speed: other.ground_speed_knots.unwrap_or(0.0) * 0.514_444,
        };
        if closest.is_none_or(|current| candidate.gap < current.gap) {
            closest = Some(candidate);
        }
    }
    closest
}

/// Speed a follower may use behind the leader without closing the gap too quickly.
#[must_use]
pub(crate) fn follow_speed(leader: Ahead, taxi_speed: f64) -> f64 {
    let stop = leader.required * 0.55;
    if leader.gap <= stop {
        0.0
    } else if leader.speed < 0.1 {
        if leader.gap < leader.required {
            0.0
        } else {
            f64::INFINITY
        }
    } else {
        (leader.speed + (leader.gap - stop) * 0.4).min(taxi_speed)
    }
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
    fn followers_keep_moving_behind_a_moving_taxi_leader() {
        let required = required_gap(37.0, 37.0);
        let leader = Ahead {
            id: 2,
            gap: required + 10.0,
            required,
            speed: 5.0,
        };
        assert!(follow_speed(leader, 8.0) > 5.0);
        let close = Ahead {
            gap: required * 0.7,
            ..leader
        };
        let speed = follow_speed(close, 8.0);
        assert!(speed > 0.0 && speed <= 8.0);
        let stopped = Ahead {
            speed: 0.0,
            gap: required - 1.0,
            ..leader
        };
        assert_eq!(follow_speed(stopped, 8.0), 0.0);
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
