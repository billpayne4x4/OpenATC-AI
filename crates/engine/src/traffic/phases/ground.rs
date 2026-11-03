//! Taxi-out, hold and taxi conflict decisions.

use openatc_core::airport::Airport;
use openatc_core::state::TrafficTarget;
use openatc_core::traffic::motion::Waypoint;

/// Distance before a corner where the turn speed begins.
const TURN_LOOKAHEAD_METERS: f64 = 40.0;
/// Distance after a corner before accelerating back to taxi speed.
const TURN_LOOKBEHIND_METERS: f64 = 15.0;

/// Taxi speed near the current position.
///
/// A long leg ending in a turn must not be driven at turn speed for its whole
/// length. The aircraft keeps taxi speed until it is close to the corner, slows
/// through the corner itself, and accelerates once clear of it.
pub(crate) fn taxi_speed(
    points: &[Waypoint],
    cumulative: &[f64],
    distance: f64,
    straight: f64,
    turn: f64,
) -> f64 {
    if points.len() < 2 || cumulative.len() != points.len() {
        return straight;
    }
    let index = cumulative
        .iter()
        .position(|value| *value >= distance)
        .unwrap_or(points.len().saturating_sub(1))
        .max(1);
    if index >= 2
        && distance - cumulative[index - 1] <= TURN_LOOKBEHIND_METERS
        && corner_turn(points, index - 1) > 15.0
    {
        return turn;
    }
    for corner in index..points.len().saturating_sub(1) {
        let Some(ahead) = cumulative.get(corner) else {
            break;
        };
        if ahead - distance > TURN_LOOKAHEAD_METERS {
            break;
        }
        if corner_turn(points, corner) > 15.0 {
            return turn;
        }
    }
    straight
}

fn corner_turn(points: &[Waypoint], index: usize) -> f64 {
    let Some(previous) = index.checked_sub(1).and_then(|i| points.get(i)) else {
        return 0.0;
    };
    let Some(current) = points.get(index) else {
        return 0.0;
    };
    let Some(next) = points.get(index + 1) else {
        return 0.0;
    };
    let first = openatc_core::arrival::bearing(
        [previous.latitude, previous.longitude],
        [current.latitude, current.longitude],
    );
    let second = openatc_core::arrival::bearing(
        [current.latitude, current.longitude],
        [next.latitude, next.longitude],
    );
    openatc_core::navigation::turn(first, second).abs()
}

/// Whether the player is ahead on a taxi route, or close in front of the aircraft.
pub(crate) fn player_ahead_on_route(
    points: &[Waypoint],
    cumulative: &[f64],
    distance: f64,
    airport: &Airport,
    player: &TrafficTarget,
) -> bool {
    if !player.on_ground || !player.latitude.is_finite() || !player.longitude.is_finite() {
        return false;
    }
    let goal = openatc_core::airport::airport_point(airport, player.latitude, player.longitude);
    for (index, pair) in points.windows(2).enumerate() {
        let end = cumulative.get(index + 1).copied().unwrap_or(0.0);
        if end <= distance + 2.0 {
            continue;
        }
        let a = openatc_core::airport::airport_point(airport, pair[0].latitude, pair[0].longitude);
        let b = openatc_core::airport::airport_point(airport, pair[1].latitude, pair[1].longitude);
        let (dx, dn) = (b.east - a.east, b.north - a.north);
        let length = dx.hypot(dn);
        if length < 0.5 {
            continue;
        }
        let along = ((goal.east - a.east) * dx + (goal.north - a.north) * dn) / length;
        if !(0.0..=length).contains(&along) {
            continue;
        }
        let across = ((goal.east - a.east) * dn - (goal.north - a.north) * dx).abs() / length;
        let range = cumulative[index] + along;
        if range > distance + 2.0 && range - distance < 80.0 && across < 35.0 {
            return true;
        }
    }
    let (own_lat, own_lon, heading) = position_heading(points, cumulative, distance);
    let own = openatc_core::airport::airport_point(airport, own_lat, own_lon);
    let (east, north) = (goal.east - own.east, goal.north - own.north);
    let gap = east.hypot(north);
    let ahead = east * heading.to_radians().sin() + north * heading.to_radians().cos();
    gap < 45.0 && ahead > -5.0
}

/// Position and heading at one distance along a taxi route.
fn position_heading(points: &[Waypoint], cumulative: &[f64], distance: f64) -> (f64, f64, f64) {
    let index = cumulative
        .iter()
        .position(|value| *value >= distance)
        .unwrap_or(points.len().saturating_sub(1))
        .max(1);
    let Some(start) = points.get(index.saturating_sub(1)) else {
        return (0.0, 0.0, 0.0);
    };
    let Some(end) = points.get(index) else {
        return (start.latitude, start.longitude, 0.0);
    };
    let span = cumulative[index] - cumulative[index.saturating_sub(1)];
    let fraction = if span > 0.0 {
        ((distance - cumulative[index.saturating_sub(1)]) / span).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let heading = openatc_core::arrival::bearing(
        [start.latitude, start.longitude],
        [end.latitude, end.longitude],
    );
    (
        start.latitude + (end.latitude - start.latitude) * fraction,
        start.longitude + (end.longitude - start.longitude) * fraction,
        heading,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> (Vec<Waypoint>, Vec<f64>) {
        let points = vec![
            Waypoint {
                latitude: 0.0,
                longitude: 0.0,
                altitude_feet: 0.0,
            },
            Waypoint {
                latitude: 0.0,
                longitude: 0.01,
                altitude_feet: 0.0,
            },
            Waypoint {
                latitude: 0.01,
                longitude: 0.01,
                altitude_feet: 0.0,
            },
        ];
        let mut cumulative = vec![0.0];
        for pair in points.windows(2) {
            let leg = openatc_core::arrival::distance_nm(
                [pair[0].latitude, pair[0].longitude],
                [pair[1].latitude, pair[1].longitude],
            ) * 1_852.0;
            cumulative.push(cumulative.last().copied().unwrap_or(0.0) + leg);
        }
        (points, cumulative)
    }

    #[test]
    fn a_long_leg_keeps_taxi_speed_until_the_corner() {
        let (points, cumulative) = route();
        let straight = 11.3;
        let turn = 6.2;
        let corner = cumulative[1];
        assert_eq!(
            taxi_speed(&points, &cumulative, corner - 200.0, straight, turn),
            straight
        );
        assert_eq!(
            taxi_speed(&points, &cumulative, corner - 20.0, straight, turn),
            turn
        );
        assert_eq!(
            taxi_speed(&points, &cumulative, corner + 5.0, straight, turn),
            turn
        );
        assert_eq!(
            taxi_speed(&points, &cumulative, corner + 30.0, straight, turn),
            straight
        );
    }
}
