//! Bounded traffic geometry shared by surface and approach monitoring.
pub mod advisory;
pub mod air;
pub mod animation;
pub mod coordinates;
pub mod csl;
pub mod ctaf;
pub mod curves;
pub mod holding;
pub mod motion;
pub mod procedures;
pub mod profiles;
pub mod resolved;
pub mod separation;
pub mod sequencing;
pub mod surface;
pub mod trajectory;
pub mod vertical;
pub mod wake;
pub mod wake_policy;
use crate::{
    airport::{Airport, airport_point},
    state::{Point, TaxiClearance, Telemetry, TrafficTarget},
};

/// Conservative plan-view extents from the simulator's reference point.
/// The reference point is not necessarily the centre of the fuselage.
#[must_use]
pub fn footprint(
    aircraft: &str,
    reported_span: Option<f64>,
    heading: f64,
    runway_heading: f64,
) -> (f64, f64) {
    let known = profiles::aircraft(aircraft);
    let span = reported_span
        .filter(|s| (1.0..=100.0).contains(s))
        .or_else(|| known.map(|a| a.wingspan))
        .unwrap_or(90.0);
    let length = known.map_or(90.0, |a| a.length);
    let angle = (heading - runway_heading).to_radians();
    (
        length * angle.cos().abs() + span / 2.0 * angle.sin().abs(),
        length * angle.sin().abs() + span / 2.0 * angle.cos().abs(),
    )
}

/// A target's signed distance along the runway and its centreline offset, in metres.
#[must_use]
pub fn runway_position(
    airport: &Airport,
    runway: &str,
    latitude: f64,
    longitude: f64,
) -> Option<(f64, f64, f64, f64)> {
    let r = airport
        .runways
        .iter()
        .find(|r| r.first_name == runway || r.second_name == runway)?;
    let (start, end) = if r.first_name == runway {
        (r.first, r.second)
    } else {
        (r.second, r.first)
    };
    let (dx, dn) = (end.east - start.east, end.north - start.north);
    let length = dx.hypot(dn);
    if length < 100.0 {
        return None;
    }
    let p = airport_point(airport, latitude, longitude);
    let (e, n) = (p.east - start.east, p.north - start.north);
    Some((
        (e * dx + n * dn) / length,
        (e * dn - n * dx) / length,
        length,
        dx.atan2(dn).to_degrees().rem_euclid(360.0),
    ))
}

/// Exact plan-view relation between one aircraft and a published runway strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunwayClearance {
    /// Distance along the runway from the named threshold, metres.
    pub along_meters: f64,
    /// Signed offset from the runway centreline, metres.
    pub offset_meters: f64,
    /// Aircraft half-extent across the runway, including wings and fuselage angle.
    pub lateral_half_meters: f64,
    /// Aircraft half-extent along the runway.
    pub fore_aft_meters: f64,
    /// Named runway length, metres.
    pub length_meters: f64,
    /// Published runway width, metres.
    pub width_meters: f64,
    /// Any part of the aircraft is on the runway strip, including wingtips.
    pub over_runway: bool,
}

/// Compare an aircraft footprint with the exact published runway width and length.
#[must_use]
pub fn runway_clearance(
    airport: &Airport,
    runway: &str,
    latitude: f64,
    longitude: f64,
    heading_degrees: f64,
    aircraft: &str,
    reported_span: Option<f64>,
    margin_meters: f64,
) -> Option<RunwayClearance> {
    let (along, across, length, runway_heading) =
        runway_position(airport, runway, latitude, longitude)?;
    let width = airport
        .runways
        .iter()
        .find(|r| r.first_name == runway || r.second_name == runway)?
        .width;
    let (fore_aft, lateral) = footprint(aircraft, reported_span, heading_degrees, runway_heading);
    let margin = margin_meters.max(0.0);
    let over_runway = along >= -fore_aft - margin
        && along <= length + fore_aft + margin
        && across.abs() < width / 2.0 + lateral + margin;
    Some(RunwayClearance {
        along_meters: along,
        offset_meters: across,
        lateral_half_meters: lateral,
        fore_aft_meters: fore_aft,
        length_meters: length,
        width_meters: width,
        over_runway,
    })
}

/// Position relative to the landing threshold, including a published displacement.
#[must_use]
pub fn approach_position(
    airport: &Airport,
    runway: &str,
    latitude: f64,
    longitude: f64,
) -> Option<(f64, f64, f64, f64)> {
    let mut position = runway_position(airport, runway, latitude, longitude)?;
    let r = airport
        .runways
        .iter()
        .find(|r| r.first_name == runway || r.second_name == runway)?;
    let displacement = if r.first_name == runway {
        r.first_displaced_meters
    } else {
        r.second_displaced_meters
    };
    position.0 -= displacement;
    position.2 -= displacement;
    Some(position)
}

/// Runway occupancy and traffic ahead, excluding a same-direction aircraft following ownship.
#[must_use]
pub fn landing_blocked(
    airport: &Airport,
    runway: &str,
    telemetry: &Telemetry,
) -> Option<&'static str> {
    let Some(own) = approach_position(airport, runway, telemetry.latitude, telemetry.longitude)
    else {
        return Some("holding_standby");
    };
    let Some(targets) = &telemetry.traffic else {
        return Some("holding_standby");
    };
    if wake::supported(&airport.icao) && own.0 < 0.0 {
        for target in targets.iter().take(126).filter(|t| {
            !t.on_ground && !crate::holding::on_other_parallel_runway(airport, runway, t)
        }) {
            let Some(lead_runway) = wake::approach_runway(airport, runway, target) else {
                continue;
            };
            let Some(lead) =
                approach_position(airport, lead_runway, target.latitude, target.longitude)
            else {
                continue;
            };
            let Some(relative) =
                approach_position(airport, runway, target.latitude, target.longitude)
            else {
                continue;
            };
            if relative.0 <= own.0
                || lead.0 > 1000.0
                || lead.1.abs() > 500.0
                || own.1.abs() > 500.0
                || crate::navigation::turn(target.track_degrees, own.3).abs() > 20.0
                || (target.altitude_feet - telemetry.altitude_feet).abs() > 2500.0
            {
                continue;
            }
            let required_wake =
                wake::arrival_for(&airport.icao, &target.aircraft, &telemetry.aircraft_icao)
                    .unwrap_or(10.0);
            let required = required_wake.max(if airport.icao.starts_with('Y') {
                5.0
            } else {
                3.0
            });
            let gap = (relative.0 - own.0).hypot(relative.1 - own.1) / 1852.0;
            let threshold_gap =
                target
                    .ground_speed_knots
                    .filter(|s| *s > 30.0)
                    .map_or(gap, |speed| {
                        let hours = (-lead.0 / 1852.0).max(0.0) / speed;
                        let along = relative.0
                            - lead.0
                            - own.0
                            - hours * telemetry.ground_speed_knots * 1852.0;
                        if along < 0.0 {
                            0.0
                        } else {
                            along.hypot(relative.1 - own.1) / 1852.0
                        }
                    });
            if gap < required || threshold_gap < required {
                return Some(if required_wake > 0.0 {
                    "holding_wake"
                } else {
                    "holding_traffic_final"
                });
            }
        }
    }
    crate::holding::blocked_targets(
        airport,
        runway,
        targets.iter().take(126).filter(|target| {
            if crate::holding::on_other_parallel_runway(airport, runway, target) {
                return false;
            }
            let Some(position) =
                approach_position(airport, runway, target.latitude, target.longitude)
            else {
                return true;
            };
            let same_direction =
                ((target.track_degrees - own.3 + 180.0).rem_euclid(360.0) - 180.0).abs() < 35.0;
            target.on_ground || !same_direction || position.0 >= own.0 - 50.0
        }),
    )
}

/// Runway queue traffic is ignored by the taxi conflict check. The margin covers
/// aircraft holding short, lining up and just leaving the runway edge.
const RUNWAY_QUEUE_MARGIN_METERS: f64 = 60.0;

/// Predict a taxi collision over the next twenty seconds. Parallel traffic moving away is ignored.
#[must_use]
#[allow(clippy::many_single_char_names)]
pub fn taxi_conflict<'a>(airport: &Airport, telemetry: &'a Telemetry) -> Option<&'a TrafficTarget> {
    if !telemetry.on_ground {
        return None;
    }
    let own = airport_point(airport, telemetry.latitude, telemetry.longitude);
    if crate::airport::within_runway(airport, &own) {
        return None;
    }
    let speed = telemetry.ground_speed_knots.max(0.0) * 0.514_444;
    let heading = telemetry
        .ground_track_degrees
        .unwrap_or(telemetry.heading_degrees)
        .to_radians();
    telemetry
        .traffic
        .as_ref()?
        .iter()
        .take(126)
        .filter(|t| t.on_ground)
        .find(|target| {
            let p = airport_point(airport, target.latitude, target.longitude);
            // Runway traffic is handled by the crossing and takeoff clearance checks.
            // The margin also covers aircraft holding short or lining up at the edge.
            if crate::airport::within_runway(airport, &p)
                || crate::airport::runway_edge_distance(airport, &p)
                    .is_some_and(|distance| distance <= RUNWAY_QUEUE_MARGIN_METERS)
            {
                return false;
            }
            let (e, n) = (p.east - own.east, p.north - own.north);
            if e.hypot(n) > 120.0 {
                return false;
            }
            let ahead = e * heading.sin() + n * heading.cos();
            // Traffic behind is the follower's responsibility, not a reason to stop
            // the aircraft in front. Stopping here also produces false holds in a queue.
            if ahead <= 0.0 {
                return false;
            }
            let width = f64::midpoint(
                telemetry.wingspan_meters.unwrap_or(36.0),
                target.wingspan_meters.unwrap_or(36.0),
            ) + 12.0;
            let v = target.ground_speed_knots.unwrap_or(0.0) * 0.514_444;
            let h = target.track_degrees.to_radians();
            let (ve, vn) = (
                v * h.sin() - speed * heading.sin(),
                v * h.cos() - speed * heading.cos(),
            );
            let norm = ve * ve + vn * vn;
            let closing = e * ve + n * vn;
            let own_track = telemetry
                .ground_track_degrees
                .unwrap_or(telemetry.heading_degrees);
            let across = e * heading.cos() - n * heading.sin();
            let same_direction =
                ((target.track_degrees - own_track + 180.0).rem_euclid(360.0) - 180.0).abs() < 45.0;
            // Traffic ahead on the same route is a follow, not a collision: do not issue
            // a hold for it. Use track alignment when available, and fall back to a narrow
            // corridor ahead so noisy track data cannot make a follower stop.
            if same_direction || (e.hypot(n) < 300.0 && across.abs() < 50.0) {
                return false;
            }
            if norm < 0.01 {
                return e.hypot(n) < width;
            }
            let time = (-closing / norm).clamp(0.0, 20.0);
            closing < 0.0 && (e + ve * time).hypot(n + vn * time) < width
        })
}

/// True when the target is ahead on the same approved taxi route and travelling the same way.
///
/// A follower on the same taxiway does not need a controller hold; it only needs to maintain a
/// gap. The test uses route-space order and a narrow lateral corridor, not a radius around the
/// aircraft, so traffic on a nearby unrelated taxiway is ignored.
#[must_use]
pub fn ahead_on_same_taxi_route(
    airport: &Airport,
    route: &TaxiClearance,
    telemetry: &Telemetry,
    target: &TrafficTarget,
) -> bool {
    if route.points.len() < 2 || !telemetry.on_ground || !target.on_ground {
        return false;
    }
    let own = airport_point(airport, telemetry.latitude, telemetry.longitude);
    let goal = airport_point(airport, target.latitude, target.longitude);
    let project = |point: Point| -> Option<(usize, f64, f64, f64)> {
        let mut best: Option<(usize, f64, f64, f64, f64)> = None;
        for (index, pair) in route.points.windows(2).enumerate() {
            let a = pair[0];
            let b = pair[1];
            let dx = b.east - a.east;
            let dn = b.north - a.north;
            let length = dx.hypot(dn);
            if length < 0.5 {
                continue;
            }
            let ux = dx / length;
            let un = dn / length;
            let along = (point.east - a.east) * ux + (point.north - a.north) * un;
            let across = ((point.east - a.east) * un - (point.north - a.north) * ux).abs();
            let clamped = along.clamp(0.0, length);
            let score = (along - clamped).abs() + across;
            if best.is_none_or(|(_, _, _, _, best_score)| score < best_score) {
                best = Some((
                    index,
                    clamped,
                    across,
                    ux.atan2(un).to_degrees().rem_euclid(360.0),
                    score,
                ));
            }
        }
        best.map(|(index, along, across, heading, _)| (index, along, across, heading))
    };
    let Some((own_segment, own_along, _, _)) = project(own) else {
        return false;
    };
    let Some((target_segment, target_along, target_across, route_heading)) = project(goal) else {
        return false;
    };
    if target_across > 45.0 || target_segment < own_segment {
        return false;
    }
    if target_segment == own_segment && target_along <= own_along {
        return false;
    }
    let gap = (own.east - goal.east).hypot(own.north - goal.north);
    if gap > 150.0 {
        return false;
    }
    let same_direction =
        ((target.track_degrees - route_heading + 180.0).rem_euclid(360.0) - 180.0).abs() < 45.0;
    same_direction
}

/// Magnitude and side of the surface crosswind, using true wind and true runway bearing.
#[must_use]
pub fn crosswind(wind_degrees: f64, wind_knots: f64, runway_degrees: f64) -> Option<(f64, bool)> {
    if ![wind_degrees, wind_knots, runway_degrees]
        .iter()
        .all(|v| v.is_finite())
        || !(0.0..=200.0).contains(&wind_knots)
    {
        return None;
    }
    let component = wind_knots * (wind_degrees - runway_degrees).to_radians().sin();
    Some((component.abs(), component > 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_approach_pairs_use_wake_or_ordinary_spacing_as_appropriate() {
        use crate::{airport::Runway, state::Point};
        let a = Airport {
            icao: "KTEST".into(),
            runways: vec![Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                width: 45.0,
                second: Point {
                    east: 3000.0,
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut t = Telemetry {
            aircraft_icao: "P28A".into(),
            on_ground: false,
            longitude: -5.0 / 60.0,
            altitude_feet: 1500.0,
            ground_speed_knots: 90.0,
            traffic: Some(vec![TrafficTarget {
                id: 2,
                aircraft: "A320".into(),
                longitude: -2.0 / 60.0,
                on_ground: false,
                track_degrees: 90.0,
                altitude_feet: 600.0,
                ground_speed_knots: Some(140.0),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(landing_blocked(&a, "09", &t), Some("holding_wake"));
        t.aircraft_icao = "A320".into();
        t.traffic.as_mut().unwrap()[0].aircraft = "P28A".into();
        t.traffic.as_mut().unwrap()[0].longitude = -3.0 / 60.0;
        assert_eq!(landing_blocked(&a, "09", &t), Some("holding_traffic_final"));
    }
    use crate::{airport::Runway, state::Point};
    #[test]
    fn follower_does_not_block_landing_but_runway_traffic_does() {
        let a = Airport {
            runways: vec![Runway {
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
        let mut t = Telemetry {
            longitude: -0.03,
            on_ground: false,
            traffic: Some(vec![TrafficTarget {
                longitude: -0.06,
                altitude_feet: 1500.0,
                track_degrees: 90.0,
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(landing_blocked(&a, "09", &t), None);
        let follower = t.traffic.as_ref().unwrap()[0].clone();
        t.traffic = Some(
            (0..63)
                .map(|id| TrafficTarget {
                    id: id + 1,
                    ..follower.clone()
                })
                .collect(),
        );
        t.traffic.as_mut().unwrap().push(TrafficTarget {
            id: 0x00f0_0001,
            longitude: 0.01,
            on_ground: true,
            ..Default::default()
        });
        assert_eq!(
            landing_blocked(&a, "09", &t),
            Some("holding_runway_occupied")
        );
        assert_eq!(crosswind(180.0, 20.0, 90.0), Some((20.0, true)));
    }
    #[test]
    fn runway_clearance_uses_exact_width_and_wing_span() {
        use crate::state::Point;
        let airport = Airport {
            reference_latitude: 0.0,
            reference_longitude: 0.0,
            runways: vec![crate::airport::Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                first: Point {
                    east: -1200.0,
                    north: 0.0,
                    height: 0.0,
                },
                second: Point {
                    east: 1200.0,
                    north: 0.0,
                    height: 0.0,
                },
                width: 45.0,
                ..Default::default()
            }],
            ..Default::default()
        };
        let over =
            runway_clearance(&airport, "09", 0.00027, 0.0, 90.0, "A320", Some(36.0), 0.0).unwrap();
        assert_eq!(over.width_meters, 45.0);
        assert_eq!(over.lateral_half_meters, 18.0);
        assert!(
            over.over_runway,
            "a wingtip 30 m from the centreline is over a 45 m runway"
        );
        let clear =
            runway_clearance(&airport, "09", 0.0005, 0.0, 90.0, "A320", Some(36.0), 0.0).unwrap();
        assert!(!clear.over_runway);
        assert!(clear.offset_meters.abs() > 45.0);
    }

    #[test]
    fn taxi_crossing_traffic_is_protected_but_diverging_traffic_is_not() {
        let a = Airport::default();
        let mut t = Telemetry {
            on_ground: true,
            ground_speed_knots: 10.0,
            heading_degrees: 0.0,
            traffic: Some(vec![TrafficTarget {
                id: 4,
                latitude: 0.0005,
                longitude: 0.0006,
                on_ground: true,
                track_degrees: 270.0,
                ground_speed_knots: Some(10.0),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(taxi_conflict(&a, &t).unwrap().id, 4);
        t.traffic.as_mut().unwrap()[0].track_degrees = 90.0;
        assert!(taxi_conflict(&a, &t).is_none());
    }

    #[test]
    fn taxi_conflict_ignores_traffic_behind_the_aircraft() {
        let a = Airport::default();
        let t = Telemetry {
            on_ground: true,
            ground_speed_knots: 10.0,
            heading_degrees: 0.0,
            traffic: Some(vec![TrafficTarget {
                id: 4,
                latitude: -0.0005,
                longitude: 0.0002,
                on_ground: true,
                track_degrees: 0.0,
                ground_speed_knots: Some(10.0),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert!(taxi_conflict(&a, &t).is_none());
    }

    #[test]
    fn taxi_conflict_ignores_runway_queue_traffic() {
        use crate::airport::Runway;
        use crate::state::Point;
        let a = Airport {
            runways: vec![Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                first: Point::default(),
                second: Point {
                    east: 2_000.0,
                    ..Default::default()
                },
                width: 45.0,
                ..Default::default()
            }],
            ..Default::default()
        };
        let t = Telemetry {
            on_ground: true,
            ground_speed_knots: 10.0,
            heading_degrees: 0.0,
            latitude: -0.0008,
            longitude: 0.001,
            traffic: Some(vec![TrafficTarget {
                id: 4,
                latitude: 0.0,
                longitude: 0.001,
                on_ground: true,
                track_degrees: 90.0,
                ground_speed_knots: Some(10.0),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert!(taxi_conflict(&a, &t).is_none());
    }
}
