//! Round taxi corners only where the installed pavement supports the turn.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use crate::state::Point;

/// A sampled surface path with a lower speed on curved portions.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    /// Local airport coordinate.
    pub point: Point,
    /// The planner should use its turn speed here.
    pub turning: bool,
}

/// Preserve both clearance endpoints and reject turns that need missing pavement.
/// # Panics
///
/// Panics if the clearance has no endpoints.
#[allow(clippy::many_single_char_names)]
pub fn rounded(
    points: &[Point],
    preferred_radius: f64,
    minimum_radius: f64,
    spacing: f64,
    supported: impl Fn(Point) -> bool,
) -> Result<Vec<Sample>, String> {
    // Installed taxi graphs repeat a node where an edge meets the runway or another edge.
    // A repeated point leaves a zero-length outgoing segment, and no corner can fit in it.
    // Half a metre is well below any real taxi segment and only collapses those duplicates.
    const MINIMUM_SEPARATION_METERS: f64 = 0.5;
    if points.len() < 2
        || points.len() > 4096
        || !(1.0..=100.0).contains(&preferred_radius)
        || !(0.5..=preferred_radius).contains(&minimum_radius)
        || !(1.0..=10.0).contains(&spacing)
        || points
            .iter()
            .any(|point| !point.east.is_finite() || !point.north.is_finite())
    {
        return Err("Invalid surface turn geometry".into());
    }
    let mut clean = Vec::new();
    for point in points {
        if clean.last().is_none_or(|last: &Point| {
            (point.east - last.east).hypot(point.north - last.north) > MINIMUM_SEPARATION_METERS
        }) {
            clean.push(*point);
        }
    }
    if clean.len() < 2 {
        return Err("Surface path has no movement".into());
    }
    let mut output = vec![Sample {
        point: clean[0],
        turning: false,
    }];
    for triple in clean.windows(3) {
        let (a, b, c) = (triple[0], triple[1], triple[2]);
        let before = (b.east - a.east).hypot(b.north - a.north);
        let after = (c.east - b.east).hypot(c.north - b.north);
        let u = [(b.east - a.east) / before, (b.north - a.north) / before];
        let v = [(c.east - b.east) / after, (c.north - b.north) / after];
        let angle = (u[0] * v[0] + u[1] * v[1]).clamp(-1.0, 1.0).acos();
        if angle.to_degrees() < 1.0 {
            output.push(Sample {
                point: b,
                turning: false,
            });
            continue;
        }
        if angle.to_degrees() >= 170.0 {
            return Err("Surface route requires an unsupported reversal".into());
        }
        let trim = (preferred_radius * (angle / 2.0).tan())
            .min(before * 0.45)
            .min(after * 0.45);
        let radius = trim / (angle / 2.0).tan();
        if radius < minimum_radius {
            return Err("Taxi turn is too tight for the aircraft".into());
        }
        let sign = (u[0] * v[1] - u[1] * v[0]).signum();
        let start = Point {
            east: b.east - u[0] * trim,
            north: b.north - u[1] * trim,
            height: b.height,
        };
        let centre = Point {
            east: start.east - u[1] * radius * sign,
            north: start.north + u[0] * radius * sign,
            height: b.height,
        };
        let initial = (start.north - centre.north).atan2(start.east - centre.east);
        let count = (radius * angle / spacing)
            .ceil()
            .max((angle.to_degrees() / 5.0).ceil()) as usize;
        for index in 0..=count {
            if output.len() >= 4096 {
                return Err("Rounded surface route exceeds the point budget".into());
            }
            let phase = initial + sign * angle * index as f64 / count as f64;
            let point = Point {
                east: centre.east + radius * phase.cos(),
                north: centre.north + radius * phase.sin(),
                height: b.height,
            };
            if !supported(point) {
                return Err("Taxi turn needs unverified pavement".into());
            }
            output.push(Sample {
                point,
                turning: true,
            });
        }
    }
    output.push(Sample {
        point: *clean.last().unwrap(),
        turning: false,
    });
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_corner_keeps_clearance_endpoints_and_requires_room_for_the_aircraft() {
        let points = [
            Point {
                east: -100.0,
                north: 0.0,
                height: 0.0,
            },
            Point::default(),
            Point {
                east: 0.0,
                north: 100.0,
                height: 0.0,
            },
        ];
        let path = rounded(&points, 30.0, 10.0, 5.0, |_| true).unwrap();
        assert_eq!(path.first().unwrap().point, points[0]);
        assert_eq!(path.last().unwrap().point, points[2]);
        assert!(path.iter().any(|sample| sample.turning));
        assert!(
            !path
                .iter()
                .any(|sample| sample.point.east.abs() < 0.001 && sample.point.north.abs() < 0.001)
        );
        assert!(
            rounded(&points, 30.0, 10.0, 5.0, |point| point.east < -20.0
                || point.north > 20.0)
            .is_err()
        );
        assert!(rounded(&points, 30.0, 20.0, 5.0, |_| true).is_ok());
        let short = [
            points[0],
            points[1],
            Point {
                north: 10.0,
                ..Default::default()
            },
        ];
        assert!(rounded(&short, 30.0, 10.0, 5.0, |_| true).is_err());
    }

    #[test]
    fn repeated_taxi_nodes_do_not_create_an_impossible_corner() {
        let points = [
            Point {
                east: -100.0,
                north: 0.0,
                height: 0.0,
            },
            Point::default(),
            Point {
                east: 0.0,
                north: 0.05,
                height: 0.0,
            },
            Point {
                east: 0.0,
                north: 100.0,
                height: 0.0,
            },
        ];
        let path = rounded(&points, 30.0, 10.0, 5.0, |_| true)
            .expect("a repeated node must not hide the real corner");
        assert_eq!(path.first().unwrap().point, points[0]);
        assert_eq!(path.last().unwrap().point, points[3]);
        assert!(path.iter().any(|sample| sample.turning));
    }
}
