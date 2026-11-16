//! Validated surface paths. A path can only be built through the validator, so
//! movement code cannot opt out of the pavement checks.

use crate::airport::{Airport, taxi_point_on_pavement, within_runway};
use crate::state::Point;

/// Why a surface path was rejected.
#[derive(Clone, Debug, PartialEq)]
pub enum SurfaceError {
    /// No points were supplied.
    Empty,
    /// A sampled point is outside the rendered pavement.
    OffPavement {
        /// East offset of the rejected sample, in metres.
        east: f64,
        /// North offset of the rejected sample, in metres.
        north: f64,
    },
    /// A path crosses runway pavement without the matching clearance.
    UnauthorizedRunway {
        /// East offset of the rejected sample, in metres.
        east: f64,
        /// North offset of the rejected sample, in metres.
        north: f64,
    },
}

/// A surface path whose samples have all been validated.
#[derive(Clone, Debug)]
pub struct SurfacePath(Vec<Point>);

impl SurfacePath {
    /// Validated points.
    #[must_use]
    pub fn points(&self) -> &[Point] {
        &self.0
    }

    /// Take the validated points.
    #[must_use]
    pub fn into_points(self) -> Vec<Point> {
        self.0
    }
}

/// Pavement and runway checks for one loaded airport.
pub struct SurfaceModel<'a> {
    airport: &'a Airport,
}

impl<'a> SurfaceModel<'a> {
    /// Build a surface model for an airport.
    #[must_use]
    pub fn new(airport: &'a Airport) -> Self {
        Self { airport }
    }

    /// Validate a ground path, sampling every metre.
    ///
    /// `allow_runway` is only true for a line-up, takeoff, crossing or landing
    /// movement that already holds the matching runway clearance.
    pub fn validate(
        &self,
        points: &[Point],
        allow_runway: bool,
    ) -> Result<SurfacePath, SurfaceError> {
        if points.is_empty() {
            return Err(SurfaceError::Empty);
        }
        for pair in points.windows(2) {
            let length = (pair[1].east - pair[0].east).hypot(pair[1].north - pair[0].north);
            let steps = (length / 1.0).ceil().max(1.0) as usize;
            for step in 0..=steps {
                let fraction = step as f64 / steps as f64;
                let point = Point {
                    east: pair[0].east + (pair[1].east - pair[0].east) * fraction,
                    north: pair[0].north + (pair[1].north - pair[0].north) * fraction,
                    height: pair[0].height,
                };
                self.check(point, allow_runway)?;
            }
        }
        if points.len() == 1 {
            self.check(points[0], allow_runway)?;
        }
        Ok(SurfacePath(points.to_vec()))
    }

    fn check(&self, point: Point, allow_runway: bool) -> Result<(), SurfaceError> {
        if !taxi_point_on_pavement(self.airport, point) {
            return Err(SurfaceError::OffPavement {
                east: point.east,
                north: point.north,
            });
        }
        if !allow_runway && within_runway(self.airport, &point) {
            return Err(SurfaceError::UnauthorizedRunway {
                east: point.east,
                north: point.north,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn airport() -> Airport {
        let point = |east, north| Point {
            east,
            north,
            height: 0.0,
        };
        Airport {
            pavement_triangles: vec![
                [point(0.0, 0.0), point(100.0, 0.0), point(100.0, 100.0)],
                [point(0.0, 0.0), point(100.0, 100.0), point(0.0, 100.0)],
            ],
            ..Default::default()
        }
    }

    #[test]
    fn a_path_off_pavement_is_rejected() {
        let airport = airport();
        let model = SurfaceModel::new(&airport);
        let inside = [
            Point {
                east: 10.0,
                north: 10.0,
                height: 0.0,
            },
            Point {
                east: 90.0,
                north: 90.0,
                height: 0.0,
            },
        ];
        assert!(model.validate(&inside, false).is_ok());
        let outside = [
            Point {
                east: 10.0,
                north: 10.0,
                height: 0.0,
            },
            Point {
                east: 150.0,
                north: 50.0,
                height: 0.0,
            },
        ];
        assert!(matches!(
            model.validate(&outside, false),
            Err(SurfaceError::OffPavement { .. })
        ));
    }

    #[test]
    fn runway_pavement_needs_the_matching_clearance() {
        let mut airport = airport();
        airport.runways = vec![crate::airport::Runway {
            first_name: "09".into(),
            second_name: "27".into(),
            first: Point {
                east: 0.0,
                north: 50.0,
                height: 0.0,
            },
            second: Point {
                east: 200.0,
                north: 50.0,
                height: 0.0,
            },
            width: 45.0,
            ..Default::default()
        }];
        let model = SurfaceModel::new(&airport);
        let path = [
            Point {
                east: 10.0,
                north: 50.0,
                height: 0.0,
            },
            Point {
                east: 90.0,
                north: 50.0,
                height: 0.0,
            },
        ];
        assert!(matches!(
            model.validate(&path, false),
            Err(SurfaceError::UnauthorizedRunway { .. })
        ));
        assert!(model.validate(&path, true).is_ok());
    }
}
