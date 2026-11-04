//! Taxi-out, hold and taxi conflict decisions.

use openatc_core::airport::Airport;
use openatc_core::state::{Point, TrafficTarget};
use openatc_core::traffic::animation::Handling;
use openatc_core::traffic::motion::Waypoint;

/// Round taxiway corners with the aircraft's turn geometry.
///
/// The raw node path is kept when a fillet cannot fit on the pavement, so a
/// tight airport corner never removes an aircraft from the surface.
pub(crate) fn round_taxi_path(
    airport: &Airport,
    points: &[Point],
    handling: &Handling,
) -> Vec<Point> {
    let minimum = handling.wheelbase_meters
        / handling
            .maximum_steering_degrees
            .to_radians()
            .tan()
            .max(0.1);
    openatc_core::traffic::surface::rounded(
        points,
        handling.taxi_turn_radius_meters,
        minimum,
        5.0,
        |point| openatc_core::airport::on_pavement(airport, point),
    )
    .map(|samples| samples.into_iter().map(|sample| sample.point).collect())
    .unwrap_or_else(|_| points.to_vec())
}

/// Taxi speed near the current position.
///
/// The target follows a braking curve into each corner and accelerates out of it.
/// The motion controller applies the actual speed change, so the target itself does
/// not need a fixed look-ahead distance.
pub(crate) fn taxi_speed(
    points: &[Waypoint],
    cumulative: &[f64],
    distance: f64,
    straight: f64,
    turn: f64,
    braking: f64,
) -> f64 {
    if points.len() < 2 || cumulative.len() != points.len() {
        return straight;
    }
    let straight = straight.max(0.0);
    let turn = turn.clamp(0.0, straight);
    let braking = braking.max(0.02);
    let mut target = straight;
    for corner in 1..points.len().saturating_sub(1) {
        if corner_turn(points, corner) <= 15.0 {
            continue;
        }
        let Some(at) = cumulative.get(corner) else {
            continue;
        };
        let distance_to_corner = (at - distance).abs();
        let allowed = (turn * turn + 2.0 * braking * distance_to_corner).sqrt();
        target = target.min(allowed.max(turn));
    }
    target
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

/// Distance along the taxi route to the player when the player is ahead on it.
///
/// The route projection catches a player around a corner that the straight-line
/// follower check cannot see.
pub(crate) fn player_gap_ahead(
    points: &[Waypoint],
    cumulative: &[f64],
    distance: f64,
    airport: &Airport,
    player: &TrafficTarget,
    own_length: f64,
) -> Option<f64> {
    if !player.on_ground
        || player.runway_occupancy == Some(true)
        || !player.latitude.is_finite()
        || !player.longitude.is_finite()
    {
        return None;
    }
    let player_length = openatc_core::traffic::profiles::aircraft(&player.aircraft)
        .map_or(40.0, |profile| profile.length);
    let required = crate::traffic::separation::player_gap(own_length, player_length);
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
        if range > distance + 2.0 && range - distance < required + 30.0 && across < 35.0 {
            return Some(range - distance);
        }
    }
    None
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
        let braking = 1.5;
        assert_eq!(
            taxi_speed(
                &points,
                &cumulative,
                corner - 200.0,
                straight,
                turn,
                braking
            ),
            straight
        );
        let approaching = taxi_speed(&points, &cumulative, corner - 20.0, straight, turn, braking);
        assert!(approaching > turn && approaching < straight);
        assert!(
            (taxi_speed(&points, &cumulative, corner, straight, turn, braking) - turn).abs() < 0.01
        );
        let leaving = taxi_speed(&points, &cumulative, corner + 20.0, straight, turn, braking);
        assert!(leaving > turn && leaving <= straight);
        assert_eq!(
            taxi_speed(
                &points,
                &cumulative,
                corner + 200.0,
                straight,
                turn,
                braking
            ),
            straight
        );
    }
    #[test]
    fn taxiway_corners_are_rounded_inside_pavement() {
        let mut airport = openatc_core::airport::demo_airport();
        if let Some(runway) = airport.runways.first_mut() {
            runway.width = 300.0;
        }
        let points = vec![
            Point {
                east: 0.0,
                north: 0.0,
                height: 0.0,
            },
            Point {
                east: 100.0,
                north: 0.0,
                height: 0.0,
            },
            Point {
                east: 100.0,
                north: 100.0,
                height: 0.0,
            },
        ];
        let catalogue = openatc_core::traffic::animation::builtin().expect("bundled handling");
        let handling = catalogue
            .get(&openatc_core::traffic::animation::Kind::Jet)
            .expect("jet handling");
        let rounded = round_taxi_path(&airport, &points, handling);
        assert!(rounded.len() > 3, "{rounded:?}");
        assert_eq!(rounded.first(), points.first());
        assert_eq!(rounded.last(), points.last());
    }
}
