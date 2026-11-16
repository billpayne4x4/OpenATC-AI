//! Follow-me movement along an acknowledged surface route.
#[cfg(test)]
use crate::aircraft::AircraftBehaviour;
use crate::state::{PhaseCode, Point, State};

/// Cached route and vehicle progress in metres along it.
#[derive(Default)]
pub struct Escort {
    points: Vec<Point>,
    lengths: Vec<f64>,
    progress: f64,
    target: f64,
    sequence: u32,
    airport: String,
    /// Projection used by the approved route.
    pub latitude: f64,
    /// Projection used by the approved route.
    pub longitude: f64,
}

impl Escort {
    /// Update the target from the aircraft position. Returns whether an escort is available.
    pub fn update(&mut self, state: &State) -> bool {
        let route = &state.taxi_clearance;
        let t = &state.telemetry;
        if !t.on_ground
            || !t.position_valid
            || !matches!(state.phase, PhaseCode::Taxi | PhaseCode::TaxiIn)
            || route.points.len() < 2
            || route.points.len() > 8192
        {
            self.clear();
            return false;
        }
        if route.pending_readback || !route.approved || route.traffic_hold || route.parking_hold {
            self.target = self.progress;
            if route.airport != self.airport {
                self.clear();
            }
            return !self.points.is_empty();
        }
        let ownship = Point {
            east: (t.longitude - route.reference_longitude)
                * 111_320.0
                * route.reference_latitude.to_radians().cos(),
            north: (t.latitude - route.reference_latitude) * 111_320.0,
            height: 0.0,
        };
        let old_position = self.position().map(|v| v.0);
        let changed = self.sequence != route.sequence
            || self.airport != route.airport
            || self.points.is_empty();
        if changed {
            let same_origin = (self.latitude - route.reference_latitude).abs() <= f64::EPSILON
                && (self.longitude - route.reference_longitude).abs() <= f64::EPSILON
                && self.airport == route.airport;
            self.points.clone_from(&route.points);
            self.lengths.clear();
            self.lengths.push(0.0);
            for pair in self.points.windows(2) {
                let distance = (pair[1].east - pair[0].east).hypot(pair[1].north - pair[0].north);
                if !distance.is_finite() {
                    self.clear();
                    return false;
                }
                self.lengths
                    .push(self.lengths.last().copied().unwrap_or(0.0) + distance);
            }
            self.latitude = route.reference_latitude;
            self.longitude = route.reference_longitude;
            self.sequence = route.sequence;
            self.airport.clone_from(&route.airport);
            self.progress = old_position
                .filter(|_| same_origin)
                .and_then(|p| self.project(p).filter(|v| v.1 < 20.0))
                .map_or_else(
                    || self.project(ownship).map_or(0.0, |v| v.0 + 30.0),
                    |v| v.0,
                );
        }
        let Some((pilot_progress, distance)) = self.project(ownship) else {
            self.clear();
            return false;
        };
        if distance > 100.0 {
            self.clear();
            return false;
        }
        let limit = (self.lengths.last().copied().unwrap_or(0.0) - 6.0).max(0.0);
        self.progress = self.progress.min(limit);
        self.target = if t.paused {
            self.progress
        } else {
            (pilot_progress + 45.0).min(limit).max(self.progress)
        };
        true
    }

    /// Advance at no more than 15 knots. A stalled frame cannot jump the car down the route.
    pub fn advance(&mut self, seconds: f64) -> Option<(Point, f64)> {
        if seconds.is_finite() {
            self.progress = (self.progress + seconds.clamp(0.0, 0.1) * 7.7167).min(self.target);
        }
        self.position()
    }

    /// Drop the route after reset, takeoff or reaching parking.
    pub fn clear(&mut self) {
        self.points.clear();
        self.lengths.clear();
        self.progress = 0.0;
        self.target = 0.0;
    }

    fn project(&self, p: Point) -> Option<(f64, f64)> {
        self.points
            .windows(2)
            .enumerate()
            .map(|(i, s)| {
                let dx = s[1].east - s[0].east;
                let dy = s[1].north - s[0].north;
                let length = dx.hypot(dy);
                let fraction = if length > 0.01 {
                    (((p.east - s[0].east) * dx + (p.north - s[0].north) * dy) / (length * length))
                        .clamp(0.0, 1.0)
                } else {
                    0.0
                };
                (
                    self.lengths[i] + fraction * length,
                    (p.east - s[0].east - fraction * dx)
                        .hypot(p.north - s[0].north - fraction * dy),
                )
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    fn position(&self) -> Option<(Point, f64)> {
        if self.points.len() < 2 {
            return None;
        }
        let i = self
            .lengths
            .partition_point(|d| *d <= self.progress)
            .saturating_sub(1)
            .min(self.points.len() - 2);
        let (a, b) = (self.points[i], self.points[i + 1]);
        let length = self.lengths[i + 1] - self.lengths[i];
        let f = if length > 0.01 {
            ((self.progress - self.lengths[i]) / length).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Some((
            Point {
                east: a.east + (b.east - a.east) * f,
                north: a.north + (b.north - a.north) * f,
                height: 0.0,
            },
            (b.east - a.east)
                .atan2(b.north - a.north)
                .to_degrees()
                .rem_euclid(360.0),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn route() -> State {
        let mut state = State {
            phase: PhaseCode::Taxi,
            ..State::default()
        };
        state.telemetry.position_valid = true;
        state.taxi_clearance.approved = true;
        state.taxi_clearance.airport = "TEST".into();
        state.taxi_clearance.sequence = 1;
        state.taxi_clearance.points = vec![
            Point::default(),
            Point {
                north: 100.0,
                ..Default::default()
            },
            Point {
                east: 100.0,
                north: 100.0,
                height: 0.0,
            },
        ];
        state
    }
    #[test]
    fn follows_bend_and_stops_before_clearance_limit() {
        let mut state = route();
        let mut car = Escort::default();
        assert!(car.update(&state));
        state.telemetry.latitude = 100.0 / 111_320.0;
        state.telemetry.longitude = 100.0 / 111_320.0;
        assert!(car.update(&state));
        for _ in 0..300 {
            let (p, _) = car.advance(0.1).unwrap();
            assert!(p.east.abs() < 0.001 || (p.north - 100.0).abs() < 0.001);
            assert!(p.east <= 94.0);
        }
        assert!((car.position().unwrap().0.east - 94.0).abs() < 0.001);
    }
    #[test]
    fn readback_pause_and_arrival_crossing_keep_the_same_clearance_limit() {
        let mut state = route();
        state.on_event(crate::aircraft::AircraftEvent::TaxiIn);
        state.taxi_clearance.to_parking = true;
        let mut car = Escort::default();
        state.taxi_clearance.pending_readback = true;
        assert!(!car.update(&state));
        state.taxi_clearance.pending_readback = false;
        assert!(car.update(&state));
        state.telemetry.paused = true;
        car.update(&state);
        let before = car.position().unwrap();
        assert_eq!(car.advance(10.0).unwrap(), before);
        state.taxi_clearance.pending_readback = true;
        state.taxi_clearance.approved = false;
        assert!(car.update(&state));
        assert_eq!(car.advance(0.1).unwrap(), before);
        state.on_event(crate::aircraft::AircraftEvent::Finished);
        assert!(!car.update(&state));
        assert!(car.advance(0.1).is_none());
    }
    #[test]
    fn speed_and_distance_remain_bounded() {
        let mut car = Escort::default();
        let mut state = route();
        car.update(&state);
        let start = car.position().unwrap().0.north;
        let next = car.advance(100.0).unwrap().0.north;
        assert!(next - start < 0.8);
        state.telemetry.longitude = 0.1;
        assert!(!car.update(&state));
    }
}
