//! Conservative runway protection using actual simulator targets.
use crate::{
    airport::{Airport, airport_point},
    state::Telemetry,
};
/// Spoken response ID when runway entry/takeoff is blocked.
#[must_use]
pub fn blocked(airport: &Airport, runway: &str, telemetry: &Telemetry) -> Option<&'static str> {
    let Some(targets) = &telemetry.traffic else {
        return Some("holding_standby");
    };
    blocked_targets(airport, runway, targets.iter().take(63))
}

/// Runway geometry used by the surface and final-approach checks.
struct RunwayFrame<'a> {
    runway: &'a crate::airport::Runway,
    start: crate::state::Point,
    dx: f64,
    dn: f64,
    length: f64,
    heading: f64,
}

fn runway_frame<'a>(airport: &'a Airport, runway: &str) -> Option<RunwayFrame<'a>> {
    let runway_data = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)?;
    let (start, end) = if runway_data.first_name == runway {
        (runway_data.first, runway_data.second)
    } else {
        (runway_data.second, runway_data.first)
    };
    let dx = end.east - start.east;
    let dn = end.north - start.north;
    let length = dx.hypot(dn);
    if length < 100.0 {
        return None;
    }
    Some(RunwayFrame {
        runway: runway_data,
        start,
        dx,
        dn,
        length,
        heading: dx.atan2(dn).to_degrees().rem_euclid(360.0),
    })
}

/// Physical runway occupancy, without treating a distant final as surface traffic.
pub fn blocked_surface<'a>(
    airport: &Airport,
    runway: &str,
    targets: impl Iterator<Item = &'a crate::state::TrafficTarget>,
) -> Option<&'static str> {
    let Some(frame) = runway_frame(airport, runway) else {
        return Some("holding_standby");
    };
    for target in targets {
        if !valid(target) {
            return Some("holding_standby");
        }
        if on_other_parallel_runway(airport, runway, target)
            || target.runway_occupancy == Some(false)
        {
            continue;
        }
        let p = airport_point(airport, target.latitude, target.longitude);
        let east = p.east - frame.start.east;
        let north = p.north - frame.start.north;
        let along = (east * frame.dx + north * frame.dn) / frame.length;
        let across = (east * frame.dn - north * frame.dx).abs() / frame.length;
        let agl = target.altitude_feet - airport.elevation_feet;
        let (fore_aft, lateral) = crate::traffic::footprint(
            &target.aircraft,
            target.wingspan_meters,
            target.track_degrees,
            frame.heading,
        );
        if (-fore_aft - 10.0..=frame.length + fore_aft + 10.0).contains(&along)
            && across < frame.runway.width / 2.0 + lateral + 10.0
            && (target.on_ground || agl < 150.0)
        {
            return Some("holding_runway_occupied");
        }
    }
    None
}

/// Along-runway position where a crossing route crosses the runway centreline.
#[must_use]
pub fn crossing_along(
    airport: &Airport,
    runway: &str,
    points: &[crate::state::Point],
) -> Option<f64> {
    let frame = runway_frame(airport, runway)?;
    for pair in points.windows(2) {
        let a = pair[0];
        let b = pair[1];
        let along = |p: crate::state::Point| {
            let east = p.east - frame.start.east;
            let north = p.north - frame.start.north;
            (east * frame.dx + north * frame.dn) / frame.length
        };
        let across = |p: crate::state::Point| {
            let east = p.east - frame.start.east;
            let north = p.north - frame.start.north;
            (east * frame.dn - north * frame.dx) / frame.length
        };
        let (along_a, across_a, along_b, across_b) = (along(a), across(a), along(b), across(b));
        if across_a * across_b <= 0.0 && (across_a - across_b).abs() > 1.0 {
            let fraction = across_a.abs() / (across_a.abs() + across_b.abs());
            return Some(along_a + (along_b - along_a) * fraction);
        }
    }
    None
}

/// Runway protection for a crossing. An aircraft that has passed the crossing point
/// and is still moving in the landing direction does not block the crossing behind it.
pub fn blocked_for_crossing<'a>(
    airport: &Airport,
    runway: &str,
    crossing_along: f64,
    targets: impl Iterator<Item = &'a crate::state::TrafficTarget> + Clone,
    seconds: f64,
) -> Option<&'static str> {
    if let Some(reason) = blocked_final_timing(airport, runway, targets.clone(), seconds) {
        return Some(reason);
    }
    let Some(frame) = runway_frame(airport, runway) else {
        return Some("holding_standby");
    };
    for target in targets {
        if !valid(target) {
            return Some("holding_standby");
        }
        if on_other_parallel_runway(airport, runway, target)
            || target.runway_occupancy == Some(false)
        {
            continue;
        }
        let p = airport_point(airport, target.latitude, target.longitude);
        let east = p.east - frame.start.east;
        let north = p.north - frame.start.north;
        let along = (east * frame.dx + north * frame.dn) / frame.length;
        let across = (east * frame.dn - north * frame.dx).abs() / frame.length;
        let agl = target.altitude_feet - airport.elevation_feet;
        let (fore_aft, lateral) = crate::traffic::footprint(
            &target.aircraft,
            target.wingspan_meters,
            target.track_degrees,
            frame.heading,
        );
        if !(-fore_aft - 10.0..=frame.length + fore_aft + 10.0).contains(&along)
            || across >= frame.runway.width / 2.0 + lateral + 10.0
            || (!target.on_ground && agl >= 150.0)
        {
            continue;
        }
        let heading_difference =
            (target.track_degrees - frame.heading + 180.0).rem_euclid(360.0) - 180.0;
        let moving_away =
            target.ground_speed_knots.unwrap_or(0.0) > 1.0 && heading_difference.abs() < 45.0;
        let behind = along >= crossing_along + fore_aft + 10.0;
        if behind && moving_away {
            continue;
        }
        return Some("holding_runway_occupied");
    }
    let _ = seconds;
    None
}

fn valid(target: &crate::state::TrafficTarget) -> bool {
    [
        target.latitude,
        target.longitude,
        target.altitude_feet,
        target.track_degrees,
    ]
    .iter()
    .all(|value| value.is_finite())
        && target.latitude.abs() <= 90.0
        && target.longitude.abs() <= 180.0
}

/// A final-approach target inside the fixed protection envelope. Used for landing
/// and crossing clearances.
fn blocked_final<'a>(
    airport: &Airport,
    runway: &str,
    targets: impl Iterator<Item = &'a crate::state::TrafficTarget>,
) -> Option<&'static str> {
    let Some(frame) = runway_frame(airport, runway) else {
        return Some("holding_standby");
    };
    for target in targets {
        if !valid(target) {
            return Some("holding_standby");
        }
        if on_other_parallel_runway(airport, runway, target) {
            continue;
        }
        let p = airport_point(airport, target.latitude, target.longitude);
        // Ground traffic on a parallel taxiway is not runway occupancy even when it sits
        // inside the wider footprint used to protect a low final.
        if target.on_ground && !crate::airport::within_runway(airport, &p) {
            continue;
        }
        let east = p.east - frame.start.east;
        let north = p.north - frame.start.north;
        let along = (east * frame.dx + north * frame.dn) / frame.length;
        let across = (east * frame.dn - north * frame.dx).abs() / frame.length;
        let agl = target.altitude_feet - airport.elevation_feet;
        let heading_difference =
            (target.track_degrees - frame.heading + 180.0).rem_euclid(360.0) - 180.0;
        // Protect both approach ends of the same physical runway.
        let inbound = (along < 0.0 && heading_difference.abs() < 35.0)
            || (along > frame.length && heading_difference.abs() > 145.0);
        let distance = if along < 0.0 {
            -along
        } else {
            along - frame.length
        };
        if !target.on_ground
            && inbound
            && distance < 9260.0
            && across < 150.0 + distance * 0.08
            && (-100.0..2500.0).contains(&agl)
        {
            return Some("holding_traffic_final");
        }
    }
    None
}

/// Check a bounded traffic view without copying telemetry or aircraft lists.
pub fn blocked_targets<'a>(
    airport: &Airport,
    runway: &str,
    targets: impl Iterator<Item = &'a crate::state::TrafficTarget> + Clone,
) -> Option<&'static str> {
    blocked_surface(airport, runway, targets.clone())
        .or_else(|| blocked_final(airport, runway, targets))
}

/// Time the movement needs against a final-approach target, rather than a fixed radius.
fn blocked_final_timing<'a>(
    airport: &Airport,
    runway: &str,
    targets: impl Iterator<Item = &'a crate::state::TrafficTarget>,
    seconds: f64,
) -> Option<&'static str> {
    for target in targets.filter(|target| !target.on_ground) {
        if on_other_parallel_runway(airport, runway, target) {
            continue;
        }
        let Some((along, across, length, heading)) =
            crate::traffic::runway_position(airport, runway, target.latitude, target.longitude)
        else {
            return Some("holding_standby");
        };
        let turn = crate::navigation::turn(heading, target.track_degrees).abs();
        let inbound = along < 0.0 && turn < 35.0 || along > length && turn > 145.0;
        let distance = if along < 0.0 { -along } else { along - length };
        if !inbound
            || across.abs() > 150.0 + distance * 0.08
            || !(-100.0..=5000.0).contains(&(target.altitude_feet - airport.elevation_feet))
        {
            continue;
        }
        let Some(speed) = target
            .ground_speed_knots
            .filter(|speed| (20.0..=600.0).contains(speed))
        else {
            if distance <= seconds * 600.0 * 0.514_444 {
                return Some("holding_standby");
            }
            continue;
        };
        if distance / (speed * 0.514_444) <= seconds {
            return Some("holding_traffic_final");
        }
    }
    None
}

/// A take-off or crossing only needs the runway clear while it moves. A distant final that
/// cannot reach the threshold before the movement clears does not block the clearance.
pub fn blocked_for_movement<'a>(
    airport: &Airport,
    runway: &str,
    targets: impl Iterator<Item = &'a crate::state::TrafficTarget> + Clone,
    seconds: f64,
) -> Option<&'static str> {
    if !seconds.is_finite() || !(0.0..=600.0).contains(&seconds) {
        return Some("holding_standby");
    }
    blocked_surface(airport, runway, targets.clone())
        .or_else(|| blocked_final_timing(airport, runway, targets, seconds))
}

/// Protect the time needed for a particular runway movement, not just a fixed radius.
pub fn blocked_during<'a>(
    airport: &Airport,
    runway: &str,
    targets: impl Iterator<Item = &'a crate::state::TrafficTarget> + Clone,
    seconds: f64,
) -> Option<&'static str> {
    if !seconds.is_finite() || !(0.0..=600.0).contains(&seconds) {
        return Some("holding_standby");
    }
    if let Some(reason) = blocked_targets(airport, runway, targets.clone()) {
        return Some(reason);
    }
    blocked_final_timing(airport, runway, targets, seconds)
}

/// True when the target is established on a different parallel runway.
pub(crate) fn on_other_parallel_runway(
    airport: &Airport,
    runway: &str,
    target: &crate::state::TrafficTarget,
) -> bool {
    for candidate in &airport.runways {
        for (name, start, end) in [
            (&candidate.first_name, candidate.first, candidate.second),
            (&candidate.second_name, candidate.second, candidate.first),
        ] {
            if name == runway {
                continue;
            }
            let heading = runway_heading(start, end);
            if crate::navigation::turn(target.track_degrees, heading).abs() > 25.0 {
                continue;
            }
            let Some((along, across, length, _)) =
                crate::traffic::runway_position(airport, name, target.latitude, target.longitude)
            else {
                continue;
            };
            if across.abs() > 200.0 || along < -20.0 * 1852.0 || along > length + 20.0 * 1852.0 {
                continue;
            }
            if parallel_runways(airport, runway, name) {
                return true;
            }
        }
    }
    false
}

fn runway_heading(start: crate::state::Point, end: crate::state::Point) -> f64 {
    (end.east - start.east)
        .atan2(end.north - start.north)
        .to_degrees()
        .rem_euclid(360.0)
}

fn runway_endpoints(
    airport: &Airport,
    name: &str,
) -> Option<(crate::state::Point, crate::state::Point)> {
    airport.runways.iter().find_map(|runway| {
        if runway.first_name == name {
            Some((runway.first, runway.second))
        } else if runway.second_name == name {
            Some((runway.second, runway.first))
        } else {
            None
        }
    })
}

fn parallel_runways(airport: &Airport, first: &str, second: &str) -> bool {
    if first == second {
        return false;
    }
    let (Some((start, end)), Some((other_start, other_end))) = (
        runway_endpoints(airport, first),
        runway_endpoints(airport, second),
    ) else {
        return false;
    };
    let angle = crate::navigation::turn(
        runway_heading(start, end),
        runway_heading(other_start, other_end),
    )
    .abs();
    if angle.min((180.0 - angle).abs()) >= 20.0 {
        return false;
    }
    let dx = end.east - start.east;
    let dn = end.north - start.north;
    let length = dx.hypot(dn);
    if length < 100.0 {
        return false;
    }
    let across = |point: crate::state::Point| {
        ((point.east - start.east) * dn - (point.north - start.north) * dx) / length
    };
    let near = across(other_start);
    let far = across(other_end);
    near * far > 0.0 && near.abs() > 250.0 && far.abs() > 250.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Point, TrafficTarget};
    #[test]
    fn a_crossing_route_reports_the_runway_intersection() {
        let airport = crate::airport::demo_airport();
        let points = [
            Point {
                east: -200.0,
                north: -50.0,
                height: 0.0,
            },
            Point {
                east: -200.0,
                north: 50.0,
                height: 0.0,
            },
        ];
        let along = crossing_along(&airport, "09", &points).expect("crossing point");
        assert!((along - 1_000.0).abs() < 1.0, "{along}");
    }

    #[test]
    fn a_landed_aircraft_past_the_crossing_does_not_block_it() {
        let airport = crate::airport::demo_airport();
        let target = |east: f64, speed: f64| TrafficTarget {
            latitude: 0.0,
            longitude: east / 111_320.0,
            altitude_feet: 0.0,
            track_degrees: 90.0,
            ground_speed_knots: Some(speed),
            on_ground: true,
            ..Default::default()
        };
        let past = target(-200.0, 20.0);
        assert!(
            blocked_for_crossing(&airport, "09", 500.0, std::iter::once(&past), 60.0).is_none()
        );
        let before = target(-700.0, 20.0);
        assert_eq!(
            blocked_for_crossing(&airport, "09", 500.0, std::iter::once(&before), 60.0),
            Some("holding_runway_occupied")
        );
        let stopped = target(-200.0, 0.0);
        assert_eq!(
            blocked_for_crossing(&airport, "09", 500.0, std::iter::once(&stopped), 60.0),
            Some("holding_runway_occupied")
        );
    }

    #[test]
    fn lining_up_needs_surface_occupancy_but_not_a_distant_final() {
        let airport = crate::airport::demo_airport();
        let mut target = TrafficTarget {
            latitude: 0.0,
            longitude: -0.18,
            altitude_feet: 1200.0,
            track_degrees: 90.0,
            ground_speed_knots: Some(200.0),
            on_ground: false,
            ..Default::default()
        };
        assert!(blocked_surface(&airport, "09", std::iter::once(&target)).is_none());
        assert_eq!(
            blocked_for_movement(&airport, "09", std::iter::once(&target), 240.0),
            Some("holding_traffic_final")
        );
        assert!(blocked_for_movement(&airport, "09", std::iter::once(&target), 60.0).is_none());
        target.on_ground = true;
        target.longitude = 0.0;
        assert_eq!(
            blocked_surface(&airport, "09", std::iter::once(&target)),
            Some("holding_runway_occupied")
        );
        assert_eq!(
            blocked_for_movement(&airport, "09", std::iter::once(&target), 60.0),
            Some("holding_runway_occupied")
        );
    }

    #[test]
    fn a_takeoff_waits_for_a_close_final_but_not_a_distant_one() {
        let airport = crate::airport::demo_airport();
        let mut target = TrafficTarget {
            latitude: 0.0,
            longitude: -5.0 / 60.0,
            altitude_feet: 1200.0,
            track_degrees: 90.0,
            ground_speed_knots: Some(140.0),
            on_ground: false,
            ..Default::default()
        };
        assert!(blocked_for_movement(&airport, "09", std::iter::once(&target), 90.0).is_none());
        target.longitude = -3.0 / 60.0;
        assert_eq!(
            blocked_for_movement(&airport, "09", std::iter::once(&target), 90.0),
            Some("holding_traffic_final")
        );
    }

    #[test]
    fn an_aircraft_holding_short_is_not_reported_as_runway_occupancy() {
        let airport = crate::airport::demo_airport();
        let mut target = TrafficTarget {
            latitude: 0.0,
            longitude: -0.010_8,
            altitude_feet: airport.elevation_feet,
            track_degrees: 90.0,
            on_ground: true,
            runway_occupancy: Some(false),
            ..Default::default()
        };
        assert!(blocked_surface(&airport, "09", std::iter::once(&target)).is_none());
        target.runway_occupancy = Some(true);
        assert_eq!(
            blocked_surface(&airport, "09", std::iter::once(&target)),
            Some("holding_runway_occupied")
        );
    }

    #[test]
    fn a_final_on_the_parallel_runway_does_not_block_the_departure_runway() {
        use crate::airport::{Airport, Runway};
        let airport = Airport {
            icao: "LFMN".into(),
            runways: vec![
                Runway {
                    first_name: "04L".into(),
                    second_name: "22R".into(),
                    width: 45.0,
                    second: Point {
                        east: 3000.0,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Runway {
                    first_name: "04R".into(),
                    second_name: "22L".into(),
                    width: 45.0,
                    first: Point {
                        north: 300.0,
                        ..Default::default()
                    },
                    second: Point {
                        east: 3000.0,
                        north: 300.0,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let target = TrafficTarget {
            latitude: 0.0,
            longitude: -1.8 / 60.0,
            altitude_feet: 1200.0,
            track_degrees: 90.0,
            ground_speed_knots: Some(140.0),
            on_ground: false,
            ..Default::default()
        };
        let mut distant = target.clone();
        distant.longitude = -5.0 / 60.0;
        assert!(blocked_for_movement(&airport, "04R", std::iter::once(&distant), 120.0).is_none());
        assert_eq!(
            blocked_for_movement(&airport, "04L", std::iter::once(&target), 120.0),
            Some("holding_traffic_final")
        );
    }

    #[test]
    fn a_long_crossing_protects_an_arrival_outside_the_fixed_final_radius() {
        let airport = crate::airport::demo_airport();
        let mut traffic = TrafficTarget {
            latitude: 0.0,
            longitude: -0.18,
            altitude_feet: 1200.0,
            track_degrees: 90.0,
            ground_speed_knots: Some(200.0),
            on_ground: false,
            ..Default::default()
        };
        assert!(blocked_targets(&airport, "09", std::iter::once(&traffic)).is_none());
        assert_eq!(
            blocked_during(&airport, "09", std::iter::once(&traffic), 240.0),
            Some("holding_traffic_final")
        );
        assert!(blocked_during(&airport, "09", std::iter::once(&traffic), 60.0).is_none());
        traffic.ground_speed_knots = None;
        assert_eq!(
            blocked_during(&airport, "09", std::iter::once(&traffic), 240.0),
            Some("holding_standby")
        );
        traffic.longitude = 0.18;
        traffic.track_degrees = 270.0;
        traffic.ground_speed_knots = Some(200.0);
        assert_eq!(
            blocked_during(&airport, "09", std::iter::once(&traffic), 240.0),
            Some("holding_traffic_final")
        );
    }
    #[test]
    fn crossing_tail_and_arrival_exit_stay_protected_beyond_the_painted_line() {
        let airport = Airport {
            runways: vec![crate::airport::Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                width: 45.0,
                second: Point {
                    east: 2000.0,
                    ..Default::default()
                },
                ..Default::default()
            }],
            taxi_hold_lines: vec![[
                Point {
                    east: 900.0,
                    north: 75.0,
                    height: 0.0,
                },
                Point {
                    east: 1100.0,
                    north: 75.0,
                    height: 0.0,
                },
            ]],
            ..Default::default()
        };
        let route = crate::state::TaxiClearance {
            crossing_runway: "09".into(),
            points: vec![
                Point {
                    east: 1000.0,
                    north: -100.0,
                    height: 0.0,
                },
                Point {
                    east: 1000.0,
                    north: 130.0,
                    height: 0.0,
                },
            ],
            ..Default::default()
        };
        let mut t = Telemetry {
            aircraft_icao: "A320".into(),
            longitude: 1000.0 / 111_320.0,
            latitude: 100.0 / 111_320.0,
            heading_degrees: 0.0,
            on_ground: true,
            position_valid: true,
            ..Default::default()
        };
        assert!(!crate::airport::crossing_vacated(&airport, &route, &t));
        assert!(crate::airport::aircraft_on_runway(&airport, &t));
        t.latitude = 125.0 / 111_320.0;
        assert!(crate::airport::crossing_vacated(&airport, &route, &t));
        assert!(!crate::airport::aircraft_on_runway(&airport, &t));
        let target = TrafficTarget {
            aircraft: "B77W".into(),
            longitude: 1000.0 / 111_320.0,
            latitude: 65.0 / 111_320.0,
            on_ground: true,
            track_degrees: 0.0,
            ..Default::default()
        };
        assert_eq!(
            blocked_targets(&airport, "09", std::iter::once(&target)),
            Some("holding_runway_occupied")
        );
    }
    #[test]
    fn distinguishes_clear_runway_incoming_occupied_and_unavailable() {
        let a = Airport {
            runways: vec![crate::airport::Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                first: Point::default(),
                second: Point {
                    east: 2000.0,
                    ..Default::default()
                },
                width: 45.0,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut t = Telemetry::default();
        assert_eq!(blocked(&a, "09", &t), Some("holding_standby"));
        t.traffic = Some(vec![]);
        assert_eq!(blocked(&a, "09", &t), None);
        let target = TrafficTarget {
            longitude: -0.02,
            altitude_feet: 500.0,
            track_degrees: 90.0,
            ..Default::default()
        };
        t.traffic = Some(vec![target]);
        assert_eq!(blocked(&a, "09", &t), Some("holding_traffic_final"));
        t.traffic.as_mut().unwrap()[0].track_degrees = 270.0;
        assert_eq!(blocked(&a, "09", &t), None);
        t.traffic = Some(vec![TrafficTarget {
            longitude: 0.01,
            on_ground: true,
            ..Default::default()
        }]);
        assert_eq!(blocked(&a, "09", &t), Some("holding_runway_occupied"));
    }
}
