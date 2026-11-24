//! Route progress in simulator time. Clearances remain the controller's responsibility.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use crate::arrival::{bearing, destination, distance_nm};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

/// Ground speed instrumentation is opt-in; unconditional tracing floods the engine log.
static MOTION_DEBUG: OnceLock<bool> = OnceLock::new();

/// A movement ends at each clearance limit, even if the next segment shares its runway.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Occupying the departure stand.
    Parked,
    /// Tug movement out of the stand.
    Pushback,
    /// Cleared surface movement towards departure.
    TaxiOut,
    /// One specifically authorised runway crossing.
    Crossing,
    /// Specifically cleared runway entry and alignment before the takeoff roll.
    LineUp,
    /// Takeoff roll, ending at rotation.
    Takeoff,
    /// Published or cleared climb path.
    Climb,
    /// Cleared en-route legs.
    Cruise,
    /// A terrain-checked closed holding pattern with an explicit onward clearance.
    Holding,
    /// Cleared descent path.
    Descent,
    /// Instrument approach before touchdown.
    Approach,
    /// Validated missed-approach route.
    MissedApproach,
    /// Landing roll and runway exit.
    Rollout,
    /// Cleared surface movement to parking.
    TaxiIn,
    /// Stand occupancy before the next flight.
    Turnaround,
}
impl Stage {
    /// Landing lights are used for runway crossing, takeoff, approach and
    /// landing. Taxiing uses the separate taxi-light channel where the model
    /// supports it. Line-up keeps the landing lights off so aircraft on final
    /// are not dazzled.
    #[must_use]
    pub fn landing_lights(self) -> bool {
        matches!(
            self,
            Self::Crossing
                | Self::Takeoff
                | Self::Climb
                | Self::Approach
                | Self::MissedApproach
                | Self::Rollout
        )
    }

    /// Whether terrain contact is required when drawing this movement.
    #[must_use]
    pub fn on_ground(self) -> bool {
        matches!(
            self,
            Self::Parked
                | Self::Pushback
                | Self::TaxiOut
                | Self::Crossing
                | Self::LineUp
                | Self::Takeoff
                | Self::Rollout
                | Self::TaxiIn
                | Self::Turnaround
        )
    }
}

thread_local! {
    static SPEED_LOG: std::cell::RefCell<std::collections::BTreeMap<u32, (usize, f64)>> =
        const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
}

/// Report a ground speed target when the aircraft changes segment, so the log shows the planned
/// speed per segment without one line per simulation step.
fn speed_target_changed(id: u32, segment: usize, target: f64) -> bool {
    SPEED_LOG.with(|log| {
        let mut log = log.borrow_mut();
        let changed = log.get(&id).is_none_or(|(last, _)| *last != segment);
        if changed {
            log.insert(id, (segment, target));
        }
        changed
    })
}

/// A resolved geographic point, independent of the simulator's local origin.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct Waypoint {
    /// Degrees north.
    pub latitude: f64,
    /// Degrees east.
    pub longitude: f64,
    /// Feet MSL, including terrain elevation for surface paths.
    pub altitude_feet: f64,
}
impl Waypoint {
    fn coordinate(self) -> [f64; 2] {
        [self.latitude, self.longitude]
    }
    fn valid(self) -> bool {
        (-90.0..=90.0).contains(&self.latitude)
            && (-180.0..=180.0).contains(&self.longitude)
            && (-2000.0..=60000.0).contains(&self.altitude_feet)
    }
}

/// Coordinates and altitudes have already been resolved against scenery and navdata.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Segment {
    /// Operational movement represented by this segment.
    pub stage: Stage,
    /// Ordered continuous path; one point represents a stationary dwell.
    pub points: Vec<Waypoint>,
    /// Planned ground speed; terrain and procedure validation happens before compilation.
    pub knots: f64,
    /// Gate, taxi conflict or runway resources held until the entire segment is complete.
    /// A crossing segment must end beyond the far holding line, including tail clearance.
    pub resources: Vec<String>,
    /// Stationary preparation/turnaround time, in simulator seconds.
    pub dwell_seconds: f64,
}

#[derive(Clone, Debug)]
struct Leg {
    from: Waypoint,
    to: Waypoint,
    duration: f64,
    course: f64,
}

/// Reduce taxi speed targets upstream of turns, clearance limits and the final stop.
/// Geometry, resources and configured upper speed limits stay unchanged.
pub fn limit_surface_speeds(
    segments: &mut [Segment],
    braking: f64,
    stop_before: &BTreeSet<usize>,
) -> Result<(), String> {
    if !(0.1..=10.0).contains(&braking) {
        return Err("Invalid surface braking rate".into());
    }
    for index in (0..segments.len()).rev() {
        if !matches!(
            segments[index].stage,
            Stage::TaxiOut | Stage::TaxiIn | Stage::Pushback | Stage::Crossing
        ) {
            continue;
        }
        let next = if stop_before.contains(&(index + 1)) {
            0.0
        } else {
            segments.get(index + 1).map_or(0.0, |segment| segment.knots)
        };
        let distance = segments[index]
            .points
            .windows(2)
            .map(|pair| distance_nm(pair[0].coordinate(), pair[1].coordinate()) * 1852.0)
            .sum::<f64>();
        if distance == 0.0
            && segments[index].knots == 0.0
            && segments[index]
                .points
                .iter()
                .all(|point| Some(point) == segments[index].points.first())
        {
            continue;
        }
        if !distance.is_finite()
            || distance <= 0.0
            || !next.is_finite()
            || !segments[index].knots.is_finite()
        {
            return Err("Invalid surface speed transition geometry".into());
        }
        let allowed = (next.powi(2) + 2.0 * braking * distance / (1852.0 / 3600.0)).sqrt();
        // Leave room for roundoff when the compiled route checks v² - next² on short legs.
        segments[index].knots = segments[index]
            .knots
            .min(allowed * (1.0 - 32.0 * f64::EPSILON));
    }
    Ok(())
}

/// Compiled once on a worker; sampling never searches navdata or recomputes leg lengths.
#[derive(Clone, Debug)]
pub struct Route {
    segments: Vec<Segment>,
    legs: Vec<Vec<Leg>>,
    moving_seconds: Vec<f64>,
}
impl Route {
    /// Reject discontinuities, oversized geometry and infeasible vertical profiles.
    pub fn compile(
        segments: Vec<Segment>,
        climb_fpm: f64,
        descent_fpm: f64,
    ) -> Result<Self, String> {
        Self::compile_inner(segments, climb_fpm, descent_fpm, false)
    }

    /// Compile an advisory departure whose last segment intentionally ends off-field.
    ///
    /// The caller retires the aircraft when the route completes. Controlled traffic must use
    /// `compile`, which still requires a protected landing or holding continuation.
    pub fn compile_departure(
        segments: Vec<Segment>,
        climb_fpm: f64,
        descent_fpm: f64,
    ) -> Result<Self, String> {
        Self::compile_inner(segments, climb_fpm, descent_fpm, true)
    }

    fn compile_inner(
        segments: Vec<Segment>,
        climb_fpm: f64,
        descent_fpm: f64,
        allow_open_end: bool,
    ) -> Result<Self, String> {
        if segments.is_empty()
            || segments.len() > 512
            || !(100.0..=10000.0).contains(&climb_fpm)
            || !(100.0..=10000.0).contains(&descent_fpm)
        {
            return Err("Invalid traffic route or vertical performance".into());
        }
        let mut legs = Vec::with_capacity(segments.len());
        let mut count = 0;
        let mut previous: Option<Waypoint> = None;
        for segment in &segments {
            count += segment.points.len();
            if segment.points.is_empty()
                || count > 4096
                || segment.resources.len() > 32
                || segment
                    .resources
                    .iter()
                    .any(|r| r.is_empty() || r.len() > 128)
                || !(0.0..=86400.0).contains(&segment.dwell_seconds)
                || !(0.0..=600.0).contains(&segment.knots)
                || segment.points.iter().any(|p| !p.valid())
            {
                return Err("Traffic segment exceeds its geometry or memory limits".into());
            }
            let first = segment.points[0];
            if previous.is_some_and(|p| {
                distance_nm(p.coordinate(), first.coordinate()) > 0.001
                    || (p.altitude_feet - first.altitude_feet).abs() > 1.0
            }) {
                return Err("Traffic route contains a discontinuity".into());
            }
            if matches!(
                segment.stage,
                Stage::Crossing | Stage::LineUp | Stage::Takeoff | Stage::Rollout
            ) && segment.resources.is_empty()
            {
                return Err("Runway movement has no protected resource".into());
            }
            let mut compiled = Vec::new();
            for pair in segment.points.windows(2) {
                let (from, to) = (pair[0], pair[1]);
                let nm = distance_nm(from.coordinate(), to.coordinate());
                if nm < 0.00001 || segment.knots < 0.1 {
                    return Err("Moving traffic needs a nonzero leg and speed".into());
                }
                let seconds = nm / segment.knots * 3600.0;
                let vertical = (to.altitude_feet - from.altitude_feet) * 60.0 / seconds;
                // Sampled interpolation can exceed an exact rate by floating-point roundoff.
                let tolerance = climb_fpm.max(descent_fpm) * 1e-9;
                if vertical > climb_fpm + tolerance || -vertical > descent_fpm + tolerance {
                    return Err("Traffic route exceeds climb or descent performance".into());
                }
                compiled.push(Leg {
                    from,
                    to,
                    duration: seconds,
                    course: bearing(from.coordinate(), to.coordinate()),
                });
            }
            previous = segment.points.last().copied();
            if segment.stage == Stage::Holding
                && (segment.points.len() < 4
                    || distance_nm(first.coordinate(), previous.unwrap().coordinate()) > 0.001
                    || segment
                        .points
                        .iter()
                        .any(|point| (point.altitude_feet - first.altitude_feet).abs() > 1.0)
                    || compiled.iter().map(|leg| leg.duration).sum::<f64>() < 60.0)
            {
                return Err(
                    "Holding route must be a closed level pattern of at least one minute".into(),
                );
            }
            legs.push(compiled);
        }
        if !allow_open_end
            && !segments.last().unwrap().stage.on_ground()
            && segments.last().unwrap().stage != Stage::Holding
        {
            return Err(
                "Airborne route needs a protected landing or closed holding continuation".into(),
            );
        }
        let moving_seconds = legs
            .iter()
            .map(|segment| segment.iter().map(|leg| leg.duration).sum())
            .collect();
        Ok(Self {
            segments,
            legs,
            moving_seconds,
        })
    }
    /// Check nominal speed changes against the aircraft's acceleration and braking envelope.
    pub fn validate_rates(&self, acceleration: f64, braking: f64) -> Result<(), String> {
        if !(0.1..=10.0).contains(&acceleration) || !(0.1..=10.0).contains(&braking) {
            return Err("Invalid generated acceleration or braking rate".into());
        }
        for (index, pair) in self.segments.windows(2).enumerate() {
            let distance =
                self.legs[index].iter().map(|leg| leg.duration).sum::<f64>() * pair[0].knots;
            let slowing =
                (pair[0].knots.powi(2) - pair[1].knots.powi(2)).max(0.0) / (2.0 * braking);
            if distance + 8.0 * f64::EPSILON * distance.abs().max(slowing.abs()).max(1.0) < slowing
            {
                return Err(format!(
                    "Movement {index} ({:?}) needs {:.1} metres to slow from {:.1} to {:.1} knots, but has {:.1}",
                    pair[0].stage,
                    slowing * (1852.0 / 3600.0),
                    pair[0].knots,
                    pair[1].knots,
                    distance * (1852.0 / 3600.0)
                ));
            }
        }
        Ok(())
    }

    /// A fallback must have enough distance to brake at each of its internal speed limits.
    fn speed_changes_fit(&self, braking: f64) -> bool {
        self.segments.windows(2).enumerate().all(|(index, pair)| {
            let distance =
                self.legs[index].iter().map(|leg| leg.duration).sum::<f64>() * pair[0].knots;
            let slowing =
                (pair[0].knots.powi(2) - pair[1].knots.powi(2)).max(0.0) / (2.0 * braking);
            distance + 8.0 * f64::EPSILON * distance.abs().max(slowing.abs()).max(1.0) >= slowing
        })
    }
    fn recovery_speed_fits(&self, boundary: usize, recovery: &Route, braking: f64) -> bool {
        let Some(segment) = self.segments.get(boundary) else {
            return false;
        };
        let distance = self.legs[boundary]
            .iter()
            .map(|leg| leg.duration)
            .sum::<f64>()
            * segment.knots;
        let slowing =
            (segment.knots.powi(2) - recovery.segments[0].knots.powi(2)).max(0.0) / (2.0 * braking);
        recovery.speed_changes_fit(braking) && distance + 1e-8 >= slowing
    }
    #[must_use]
    /// Retrieve a controller's next clearance limit.
    pub fn segment(&self, index: usize) -> Option<&Segment> {
        self.segments.get(index)
    }
    /// Number of compiled movements, used by planners that inspect the route in order.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }
    /// Nominal duration of an explicit controller scope, before acceleration adjustments.
    #[must_use]
    pub fn nominal_seconds(&self, first: usize, last: usize) -> Option<f64> {
        if first > last || last >= self.legs.len() {
            return None;
        }
        Some(
            self.legs[first..=last]
                .iter()
                .flatten()
                .map(|leg| leg.duration)
                .sum(),
        )
    }
    /// Retain an already compiled suffix; this neither changes geometry nor grants movement.
    pub fn suffix(&self, first: usize) -> Result<Self, String> {
        if first >= self.segments.len() {
            return Err("Traffic route suffix has no movements".into());
        }
        Ok(Self {
            segments: self.segments[first..].to_vec(),
            legs: self.legs[first..].to_vec(),
            moving_seconds: self.moving_seconds[first..].to_vec(),
        })
    }
    /// Exact compiled endpoint, used when planning a continuous next phase.
    /// # Panics
    ///
    /// Panics if the route has no segments or a segment has no points.
    #[must_use]
    pub fn end_position(&self) -> Waypoint {
        *self.segments.last().unwrap().points.last().unwrap()
    }
    /// Exact compiled start point, used when planning a continuous next phase.
    /// # Panics
    ///
    /// Panics if the route has no segments or a segment has no points.
    #[must_use]
    pub fn start_position(&self) -> Waypoint {
        *self.segments.first().unwrap().points.first().unwrap()
    }
    /// Hold an unstarted aircraft at its stand for longer, spreading start-up requests apart.
    pub fn delay_initial_dwell(&mut self, extra_seconds: f64) -> bool {
        let Some(segment) = self.segments.first_mut() else {
            return false;
        };
        if !matches!(segment.stage, Stage::Parked | Stage::Turnaround)
            || !extra_seconds.is_finite()
            || extra_seconds < 0.0
        {
            return false;
        }
        let total = segment.dwell_seconds + extra_seconds;
        if !(0.0..=86400.0).contains(&total) {
            return false;
        }
        segment.dwell_seconds = total;
        true
    }
}

/// Immutable movement sample for conflict checks and drawing.
#[derive(Clone, Debug, Serialize)]
pub struct Pose {
    /// Stable traffic identity.
    pub id: u32,
    /// Geographic position.
    pub position: Waypoint,
    /// True ground-track degrees.
    pub heading: f64,
    /// Current ground speed.
    pub knots: f64,
    /// Current route movement.
    pub stage: Stage,
    /// Clearance-limit index, used to reject stale grants.
    pub segment: usize,
    /// This segment has not yet received movement permission.
    pub awaiting_clearance: bool,
    /// This route has reached its final clearance limit.
    pub complete: bool,
    /// The aircraft is following its separately checked recovery instead of the normal flight.
    pub recovering: bool,
    /// Original clearance limit that led into the current recovery route.
    pub recovery_from: Option<usize>,
    /// Nominal time remaining to this clearance limit, before any slower speed adjustment.
    pub seconds_to_limit: f64,
}

/// Constant acceleration to a target, followed by steady speed. Distances are knot-seconds.
struct SpeedChange {
    initial: f64,
    target: f64,
    rate: f64,
}
impl SpeedChange {
    fn new(initial: f64, target: f64, acceleration: f64, braking: f64) -> Self {
        Self {
            initial,
            target,
            rate: if target >= initial {
                acceleration
            } else {
                -braking
            },
        }
    }
    fn ramp_seconds(&self) -> f64 {
        ((self.target - self.initial) / self.rate).max(0.0)
    }
    fn sample(&self, seconds: f64) -> (f64, f64) {
        let ramp = seconds.min(self.ramp_seconds());
        let speed = self.initial + self.rate * ramp;
        let distance = f64::midpoint(self.initial, speed) * ramp + self.target * (seconds - ramp);
        (distance, speed.max(0.0))
    }
    fn seconds_to(&self, distance: f64) -> f64 {
        if distance <= 0.0 {
            return 0.0;
        }
        let ramp = self.ramp_seconds();
        let ramp_distance = f64::midpoint(self.initial, self.target) * ramp;
        if distance <= ramp_distance {
            let speed = (self.initial.powi(2) + 2.0 * self.rate * distance)
                .max(0.0)
                .sqrt();
            2.0 * distance / (self.initial + speed)
        } else {
            ramp + (distance - ramp_distance) / self.target
        }
    }
}

/// Accelerate, coast and brake while preserving an upper speed limit at the next boundary.
struct BoundarySpeed {
    first: SpeedChange,
    coast: f64,
    last: SpeedChange,
}
impl BoundarySpeed {
    fn new(
        initial: f64,
        maximum: f64,
        end: f64,
        distance: f64,
        acceleration: f64,
        braking: f64,
    ) -> Option<Self> {
        let end = end
            .min(maximum)
            .min((initial.powi(2) + 2.0 * acceleration * distance).sqrt());
        let peak = if initial > maximum {
            maximum
        } else {
            ((2.0 * acceleration * braking * distance
                + braking * initial.powi(2)
                + acceleration * end.powi(2))
                / (acceleration + braking))
                .sqrt()
                .min(maximum)
        };
        let first = SpeedChange::new(initial, peak, acceleration, braking);
        let last = SpeedChange::new(peak, end.min(peak), acceleration, braking);
        let remaining =
            distance - first.sample(first.ramp_seconds()).0 - last.sample(last.ramp_seconds()).0;
        if remaining < -1e-8 {
            return None;
        }
        Some(Self {
            first,
            coast: if peak > 0.0 {
                remaining.max(0.0) / peak
            } else {
                0.0
            },
            last,
        })
    }
    fn sample(&self, seconds: f64) -> (f64, f64) {
        let first_time = seconds.min(self.first.ramp_seconds());
        let (first_distance, first_speed) = self.first.sample(first_time);
        if (first_time - seconds).abs() <= f64::EPSILON {
            return (first_distance, first_speed);
        }
        let coast_time = (seconds - first_time).min(self.coast);
        let (last_distance, last_speed) = self.last.sample(seconds - first_time - coast_time);
        (
            first_distance + self.first.target * coast_time + last_distance,
            last_speed,
        )
    }
    fn seconds_to(&self, distance: f64) -> f64 {
        let first_time = self.first.ramp_seconds();
        let first_distance = self.first.sample(first_time).0;
        if distance <= first_distance {
            return self.first.seconds_to(distance);
        }
        let rest = distance - first_distance;
        let coast_distance = self.first.target * self.coast;
        if rest <= coast_distance {
            return first_time + rest / self.first.target;
        }
        first_time + self.coast + self.last.seconds_to(rest - coast_distance)
    }
}

#[derive(Clone)]
struct HoldExit {
    route: std::sync::Arc<Route>,
    recoveries: BTreeMap<usize, std::sync::Arc<Route>>,
    through: usize,
    defer_circuits: u32,
}

#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)]
struct Aircraft {
    assigned_route: std::sync::Arc<Route>,
    route: std::sync::Arc<Route>,
    segment: usize,
    elapsed: f64,
    authorised: bool,
    complete: bool,
    next_authorised: bool,
    airborne_authority: Option<(usize, usize)>,
    hold_exit: Option<HoldExit>,
    speed_limits: BTreeMap<usize, f64>,
    rates: Option<(f64, f64)>,
    speed: f64,
    holding: bool,
    recoveries: BTreeMap<usize, std::sync::Arc<Route>>,
    recovery_from: Option<usize>,
    surface_reservation: Option<(usize, BTreeSet<String>)>,
}
impl Aircraft {
    fn pose(&self, id: u32) -> Pose {
        let segment = &self.route.segments[self.segment];
        let mut elapsed = self.elapsed;
        let legs = &self.route.legs[self.segment];
        let mut position = segment.points[0];
        let mut heading = legs.first().map_or(0.0, |l| l.course);
        let mut moving = false;
        for (index, leg) in legs.iter().enumerate() {
            heading = leg.course;
            if elapsed >= leg.duration {
                elapsed -= leg.duration;
                position = leg.to;
                continue;
            }
            let fraction = elapsed / leg.duration;
            let incoming = if segment.stage == Stage::Holding && index == 0 {
                legs.last().map(|leg| leg.course)
            } else {
                index
                    .checked_sub(1)
                    .and_then(|index| legs.get(index))
                    .map(|leg| leg.course)
                    .or_else(|| {
                        self.segment
                            .checked_sub(1)
                            .and_then(|index| self.route.legs[index].last())
                            .map(|leg| leg.course)
                    })
            }
            .unwrap_or(leg.course);
            let outgoing = legs
                .get(index + 1)
                .map(|leg| leg.course)
                .or_else(|| {
                    if segment.stage == Stage::Holding {
                        legs.first().map(|leg| leg.course)
                    } else {
                        self.route
                            .legs
                            .get(self.segment + 1)
                            .and_then(|legs| legs.first())
                            .map(|leg| leg.course)
                    }
                })
                .unwrap_or(leg.course);
            let before = crate::navigation::turn(incoming, leg.course);
            let after = crate::navigation::turn(leg.course, outgoing);
            if before.abs() <= 15.0 && after.abs() <= 15.0 {
                let start = leg.course - before / 2.0;
                let end = leg.course + after / 2.0;
                heading =
                    (start + crate::navigation::turn(start, end) * fraction).rem_euclid(360.0);
            }
            if elapsed == 0.0 {
                position = leg.from;
                moving = self.authorised;
                break;
            }
            let coordinate = destination(
                leg.from.coordinate(),
                leg.course,
                segment.knots * elapsed / 3600.0,
            );
            position = Waypoint {
                latitude: coordinate[0],
                longitude: coordinate[1],
                altitude_feet: leg.from.altitude_feet
                    + (leg.to.altitude_feet - leg.from.altitude_feet) * fraction,
            };
            moving = self.authorised;
            break;
        }
        Pose {
            id,
            position,
            heading,
            knots: if moving {
                self.rates.map_or(segment.knots, |_| self.speed)
            } else {
                0.0
            },
            stage: segment.stage,
            segment: self.segment,
            awaiting_clearance: (!self.authorised || self.holding && self.speed == 0.0)
                && !self.complete,
            complete: self.complete,
            recovering: self.recovery_from.is_some(),
            recovery_from: self.recovery_from,
            seconds_to_limit: self.seconds_to_limit(),
        }
    }
    fn duration(&self) -> f64 {
        self.route.moving_seconds[self.segment] + self.route.segments[self.segment].dwell_seconds
    }
    fn seconds_to_limit(&self) -> f64 {
        let nominal = (self.duration() - self.elapsed).max(0.0);
        let segment = &self.route.segments[self.segment];
        let Some((acceleration, braking)) = self.rates else {
            return nominal;
        };
        if !self.authorised || !self.speed_limits.contains_key(&self.segment) {
            return nominal;
        }
        let cap = self
            .speed_limits
            .get(&self.segment)
            .copied()
            .unwrap_or(segment.knots);
        let next_authorised = self.next_authorised
            || self
                .airborne_authority
                .is_some_and(|(first, end)| (first..=end).contains(&(self.segment + 1)));
        let next_speed = self.route.segments.get(self.segment + 1).map_or(cap, |s| {
            self.speed_limits
                .get(&(self.segment + 1))
                .copied()
                .unwrap_or(s.knots)
        });
        let end = if self.route.segments[self.segment].stage == Stage::Takeoff {
            // A missing climb clearance must not brake the take-off roll. The aircraft
            // holds or recovers after rotation instead of accelerating and slowing on the
            // runway while Tower waits for final traffic.
            cap
        } else if next_authorised {
            next_speed
        } else {
            self.recoveries
                .get(&self.segment)
                .map_or(cap, |r| r.segments[0].knots)
        };
        BoundarySpeed::new(
            self.speed,
            cap,
            end,
            nominal * segment.knots,
            acceleration,
            braking,
        )
        .map_or(nominal, |profile| {
            profile.seconds_to(nominal * segment.knots)
        })
    }
}

/// The controller supplies whole-segment permission after checking traffic and wake rules.
/// A new segment needs its own grant or an already approved continuation.
#[derive(Default, Clone)]
pub struct World {
    generation: u64,
    aircraft: BTreeMap<u32, Aircraft>,
    owners: BTreeMap<String, u32>,
    /// Aircraft flying a published advisory procedure, which is their own authority.
    advisory: BTreeSet<u32>,
    clock: Option<f64>,
}

/// Result of accepting a simulator clock sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advance {
    /// First observation establishes the clock.
    Initial,
    /// Aircraft progress remains unchanged.
    Paused,
    /// An ordinary bounded time step was applied.
    Advanced,
    /// Clock discontinuity invalidated grants without moving aircraft.
    ClockChanged,
    /// The clock sample was not finite or was negative.
    Invalid,
}

impl World {
    /// Combine prepared scenes before either has a clock or movement authority, retaining occupied stands.
    pub fn merge_prepared(&mut self, other: Self) -> bool {
        let unstarted = |world: &Self| {
            world.clock.is_none()
                && world.owners.iter().all(|(resource, id)| {
                    world.aircraft.get(id).is_some_and(|aircraft| {
                        matches!(
                            aircraft.route.segments[0].stage,
                            Stage::Parked | Stage::Turnaround
                        ) && aircraft.route.segments[0].resources.contains(resource)
                    })
                })
                && world.aircraft.values().all(|aircraft| {
                    aircraft.segment == 0
                        && aircraft.elapsed == 0.0
                        && !aircraft.authorised
                        && !aircraft.next_authorised
                        && aircraft.airborne_authority.is_none()
                        && aircraft.hold_exit.is_none()
                        && aircraft.surface_reservation.is_none()
                        && aircraft.recovery_from.is_none()
                })
        };
        if self.generation != other.generation
            || self.aircraft.len() + other.aircraft.len() > 63
            || other
                .aircraft
                .keys()
                .any(|id| self.aircraft.contains_key(id))
            || other
                .owners
                .keys()
                .any(|resource| self.owners.contains_key(resource))
            || !unstarted(self)
            || !unstarted(&other)
        {
            return false;
        }
        self.aircraft.extend(other.aircraft);
        self.owners.extend(other.owners);
        self.advisory.extend(other.advisory);
        true
    }

    /// Add one freshly prepared aircraft to a running world without resetting its clock.
    /// Existing grants and resource owners are left intact; conflicting starts are refused.
    pub fn admit_prepared(&mut self, prepared: &Self, id: u32) -> bool {
        let Some(aircraft) = prepared.aircraft.get(&id) else {
            return false;
        };
        if self.generation != prepared.generation
            || prepared.clock.is_some()
            || self.aircraft.len() >= 63
            || self.aircraft.contains_key(&id)
            || aircraft.elapsed != 0.0
            || aircraft.complete
            || prepared
                .owners
                .iter()
                .any(|(resource, owner)| *owner == id && self.owners.contains_key(resource))
        {
            return false;
        }
        self.aircraft.insert(id, aircraft.clone());
        self.owners.extend(
            prepared
                .owners
                .iter()
                .filter(|(_, owner)| **owner == id)
                .map(|(resource, owner)| (resource.clone(), *owner)),
        );
        if prepared.advisory.contains(&id) {
            self.advisory.insert(id);
        }
        true
    }

    /// The assigned route, for preparing a varied start before the first simulation sample.
    #[must_use]
    pub fn assigned_route(&self, id: u32) -> Option<std::sync::Arc<Route>> {
        self.aircraft
            .get(&id)
            .map(|aircraft| aircraft.assigned_route.clone())
    }

    /// Keep a surface actor's exact position, clock and reservations when publishing a new scene.
    pub fn preserve_surface(&mut self, previous: &Self, id: u32) -> bool {
        let Some(aircraft) = previous.aircraft.get(&id) else {
            return false;
        };
        if self.generation != previous.generation
            || self.aircraft.len() >= 63
            || self.aircraft.contains_key(&id)
            || self
                .clock
                .is_some_and(|clock| Some(clock) != previous.clock)
            || aircraft.route.segments.iter().any(|segment| {
                !matches!(
                    segment.stage,
                    Stage::Parked
                        | Stage::Pushback
                        | Stage::TaxiOut
                        | Stage::Crossing
                        | Stage::TaxiIn
                        | Stage::Turnaround
                )
            })
            || previous
                .owners
                .iter()
                .any(|(resource, owner)| *owner == id && self.owners.contains_key(resource))
        {
            return false;
        }
        self.clock = previous.clock;
        self.aircraft.insert(id, aircraft.clone());
        self.owners.extend(
            previous
                .owners
                .iter()
                .filter(|(_, owner)| **owner == id)
                .map(|(resource, owner)| (resource.clone(), *owner)),
        );
        true
    }

    /// Visit a bounded prediction using only current movement grants and registered recoveries.
    /// The live world, its clock and its resource owners remain unchanged.
    pub fn forecast(
        &self,
        seconds: f64,
        mut visit: impl FnMut(f64, &[Pose]) -> bool,
    ) -> Result<bool, String> {
        if !(0.0..=900.0).contains(&seconds) {
            return Err("Traffic prediction exceeds its time budget".into());
        }
        let mut predicted = self.clone();
        let start = self.clock.unwrap_or(0.0);
        if self.clock.is_none() {
            predicted.advance(start, false);
        }
        for step in 0..=seconds.ceil() as u32 {
            let elapsed = f64::from(step).min(seconds);
            if step > 0 && predicted.advance(start + elapsed, false) != Advance::Advanced {
                return Err("Traffic prediction encountered an invalid clock".into());
            }
            if !visit(elapsed, &predicted.snapshot()) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Install the next planned phase at a stopped clearance limit, without granting movement.
    pub fn continue_surface(
        &mut self,
        generation: u64,
        id: u32,
        route: std::sync::Arc<Route>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        if aircraft
            .rates
            .is_some_and(|(_, braking)| !route.speed_changes_fit(braking))
        {
            return false;
        }
        let current = aircraft.pose(id);
        // Keep only the resources of the segment the aircraft is physically on. Older
        // segments may still name the runway after an arrival or a completed taxi; those
        // entries must not survive into the next departure.
        let keep: BTreeSet<String> = aircraft
            .route
            .segments
            .get(aircraft.segment)
            .map(|segment| segment.resources.iter().cloned().collect())
            .unwrap_or_default();
        let first = route.segments[0].points[0];
        if !aircraft.complete
            || !current.stage.on_ground()
            || current.knots > 0.0
            || !route.segments[0].stage.on_ground()
            || distance_nm(current.position.coordinate(), first.coordinate()) > 0.001
            || (current.position.altitude_feet - first.altitude_feet).abs() > 1.0
        {
            return false;
        }
        aircraft.speed_limits.clear();
        aircraft.assigned_route = route.clone();
        aircraft.route = route;
        aircraft.segment = 0;
        aircraft.elapsed = 0.0;
        aircraft.authorised = false;
        aircraft.next_authorised = false;
        aircraft.airborne_authority = None;
        aircraft.hold_exit = None;
        aircraft.complete = false;
        aircraft.holding = false;
        aircraft.surface_reservation = None;
        aircraft.recoveries.clear();
        aircraft.recovery_from = None;
        // A replaced route must not inherit surface owners from the old arrival or taxi
        // route. A completed arrival can still name the runway in `owners` after turning
        // around; keeping that entry blocks every following departure from lining up.
        self.owners
            .retain(|resource, owner| *owner != id || keep.contains(resource));
        true
    }
    /// Identify the reserved surface traffic preventing the next taxi clearance.
    #[must_use]
    pub fn surface_blocker(&self, id: u32) -> Option<u32> {
        let blocker = self.raw_surface_blocker(id)?;
        // Converging taxi routes can each hold a resource the other needs. Break the
        // resulting two-aircraft deadlock by yielding to traffic on the right.
        if self.raw_surface_blocker(blocker) == Some(id) {
            if self.yields_to_right(id, blocker) {
                return Some(blocker);
            }
            return None;
        }
        Some(blocker)
    }

    fn raw_surface_blocker(&self, id: u32) -> Option<u32> {
        let aircraft = self.aircraft.get(&id)?;
        let stage = aircraft.route.segments[aircraft.segment].stage;
        aircraft
            .route
            .segments
            .iter()
            .skip(aircraft.segment)
            .take_while(|segment| segment.stage == stage)
            .flat_map(|segment| &segment.resources)
            .filter_map(|resource| self.owners.get(resource).copied())
            .find(|owner| *owner != id && !self.behind(id, *owner))
    }

    /// True when the other aircraft is on this aircraft's right, so this aircraft yields.
    fn yields_to_right(&self, id: u32, other: u32) -> bool {
        let (Some(a), Some(b)) = (self.aircraft.get(&id), self.aircraft.get(&other)) else {
            return false;
        };
        let pa = a.pose(id);
        let pb = b.pose(other);
        let dlon = (pb.position.longitude - pa.position.longitude).to_radians()
            * pa.position.latitude.to_radians().cos();
        let dlat = (pb.position.latitude - pa.position.latitude).to_radians();
        if dlon.hypot(dlat) < 1.0e-9 {
            return id < other;
        }
        let bearing = dlon.atan2(dlat).to_degrees().rem_euclid(360.0);
        let relative = crate::navigation::turn(pa.heading, bearing);
        if relative.abs() < 1.0 || (relative.abs() - 180.0).abs() < 1.0 {
            return id < other;
        }
        relative > 0.0
    }

    /// True when the other aircraft is behind this one on the same movement. A leader ignores
    /// resources held by a follower; otherwise two adjacent aircraft block each other and both
    /// are told to hold.
    fn behind(&self, id: u32, other: u32) -> bool {
        let (Some(a), Some(b)) = (self.aircraft.get(&id), self.aircraft.get(&other)) else {
            return false;
        };
        // Only aircraft following the same assigned route can safely be ordered by progress.
        // Opposite or unrelated routes must still block the reservation.
        if !std::sync::Arc::ptr_eq(&a.assigned_route, &b.assigned_route)
            || a.route.segments[a.segment].stage != b.route.segments[b.segment].stage
        {
            return false;
        }
        let end_distance = |aircraft: &Aircraft| {
            let pose = aircraft.pose(if std::ptr::eq(aircraft, a) { id } else { other });
            let end = aircraft.route.end_position();
            distance_nm(
                [pose.position.latitude, pose.position.longitude],
                [end.latitude, end.longitude],
            ) * 1852.0
        };
        let own = end_distance(a);
        let other_remaining = end_distance(b);
        if !own.is_finite() || !other_remaining.is_finite() {
            return false;
        }
        if (own - other_remaining).abs() <= 15.0 {
            return id < other;
        }
        other_remaining > own
    }

    /// True when both aircraft are moving through the same ground stage over shared taxiway
    /// edges. Taxi routes are compiled per flight, so the protected edges identify following
    /// traffic even though the route pointers differ.
    fn companion(&self, id: u32, other: u32) -> bool {
        let (Some(a), Some(b)) = (self.aircraft.get(&id), self.aircraft.get(&other)) else {
            return false;
        };
        let stage = a.route.segments[a.segment].stage;
        if !matches!(stage, Stage::TaxiOut | Stage::TaxiIn | Stage::Pushback)
            || stage != b.route.segments[b.segment].stage
        {
            return false;
        }
        let a_remaining = a
            .route
            .segments
            .iter()
            .skip(a.segment)
            .take_while(|segment| segment.stage == stage)
            .flat_map(|segment| &segment.resources)
            .collect::<BTreeSet<_>>();
        let b_remaining = b
            .route
            .segments
            .iter()
            .skip(b.segment)
            .take_while(|segment| segment.stage == stage)
            .flat_map(|segment| &segment.resources)
            .collect::<BTreeSet<_>>();
        !a_remaining.is_disjoint(&b_remaining)
    }
    /// Inspect the current clearance limit without exposing mutable route state.
    #[must_use]
    pub fn current_segment(&self, id: u32) -> Option<&Segment> {
        let aircraft = self.aircraft.get(&id)?;
        aircraft.route.segments.get(aircraft.segment)
    }
    /// Resources one aircraft currently holds, including those retained while standing.
    #[must_use]
    pub fn owned_resources(&self, id: u32) -> BTreeSet<String> {
        self.owners
            .iter()
            .filter(|(_, owner)| **owner == id)
            .map(|(resource, _)| resource.clone())
            .collect()
    }
    /// Current owner of one protected resource.
    #[must_use]
    pub fn resource_owner(&self, resource: &str) -> Option<u32> {
        self.owners.get(resource).copied()
    }
    /// Flight generation attached to work sent to a CPU worker.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Remove the previous flight and invalidate outstanding work.
    pub fn reset(&mut self) {
        *self = Self {
            generation: self.generation.wrapping_add(1),
            ..Self::default()
        };
    }

    /// Withdraw all movement authority from a stalled aircraft without deleting it.
    /// The health monitor uses this after the retrigger stage; a fresh grant is required.
    pub fn release_movement(&mut self, id: u32) -> bool {
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        aircraft.authorised = false;
        aircraft.next_authorised = false;
        aircraft.airborne_authority = None;
        aircraft.surface_reservation = None;
        aircraft.holding = false;
        self.owners.retain(|_, owner| *owner != id);
        true
    }

    /// Remove one finished aircraft and release every surface resource it owns.
    pub fn remove(&mut self, id: u32) -> bool {
        self.owners.retain(|_, owner| *owner != id);
        self.advisory.remove(&id);
        self.aircraft.remove(&id).is_some()
    }
    /// Admit a compiled route within the active-aircraft cap.
    pub fn insert(&mut self, generation: u64, id: u32, route: std::sync::Arc<Route>) -> bool {
        if generation != self.generation
            || id == 0
            || self.aircraft.len() >= 63
            || self.aircraft.contains_key(&id)
        {
            return false;
        }
        let initial = &route.segments[0];
        if matches!(initial.stage, Stage::Parked | Stage::Turnaround) {
            if initial
                .resources
                .iter()
                .any(|resource| self.owners.contains_key(resource))
            {
                return false;
            }
            for resource in &initial.resources {
                self.owners.insert(resource.clone(), id);
            }
        }
        let initial_speed = if initial.stage.on_ground() {
            0.0
        } else {
            initial.knots
        };
        self.aircraft.insert(
            id,
            Aircraft {
                assigned_route: route.clone(),
                route,
                segment: 0,
                elapsed: 0.0,
                authorised: false,
                complete: false,
                next_authorised: false,
                airborne_authority: None,
                hold_exit: None,
                speed_limits: BTreeMap::new(),
                rates: None,
                speed: initial_speed,
                holding: false,
                recoveries: BTreeMap::new(),
                recovery_from: None,
                surface_reservation: None,
            },
        );
        true
    }
    /// Whether the aircraft has movement authority for its current segment.
    #[must_use]
    pub fn is_authorised(&self, id: u32) -> bool {
        self.aircraft
            .get(&id)
            .is_some_and(|aircraft| aircraft.authorised)
    }

    /// Clear inherited movement authority without changing the aircraft's position.
    pub fn clear_authority(&mut self, id: u32) {
        if let Some(aircraft) = self.aircraft.get_mut(&id) {
            aircraft.authorised = false;
            aircraft.next_authorised = false;
            aircraft.complete = false;
            aircraft.airborne_authority = None;
            aircraft.surface_reservation = None;
        }
    }

    /// Move an inserted aircraft to a different surface segment without advancing time.
    ///
    /// Used when dressing the airport at startup so taxiing traffic can begin between a
    /// stand and the active runway instead of all starting at the same route end.
    pub fn place_on_segment(&mut self, id: u32, segment: usize) -> bool {
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        if segment >= aircraft.route.segment_count()
            || !aircraft.route.segments[segment].stage.on_ground()
        {
            return false;
        }
        self.owners.retain(|_, owner| *owner != id);
        aircraft.segment = segment;
        aircraft.elapsed = 0.0;
        aircraft.authorised = false;
        aircraft.complete = false;
        aircraft.next_authorised = false;
        aircraft.airborne_authority = None;
        aircraft.hold_exit = None;
        aircraft.holding = false;
        aircraft.surface_reservation = None;
        aircraft.speed = 0.0;
        true
    }

    /// Admit an aircraft that is already established on an airborne segment of its route.
    ///
    /// The seeded aircraft owns no surface resources and starts with authority only for its
    /// current segment, exactly as if the preceding segment had just been completed. Later
    /// boundaries still need their own clearance.
    pub fn seed_airborne(
        &mut self,
        generation: u64,
        id: u32,
        route: std::sync::Arc<Route>,
        segment: usize,
    ) -> bool {
        self.admit_airborne(generation, id, route, segment, true, 0.0)
    }

    /// Admit an aircraft partway along an airborne segment, used to start arrivals on final
    /// rather than at the first fix of a long approach leg.
    pub fn seed_airborne_progress(
        &mut self,
        generation: u64,
        id: u32,
        route: std::sync::Arc<Route>,
        segment: usize,
        progress: f64,
    ) -> bool {
        if !progress.is_finite() || !(0.0..=1.0).contains(&progress) {
            return false;
        }
        self.admit_airborne(generation, id, route, segment, true, progress)
    }

    /// Admit an aircraft on an airborne segment, with or without movement permission.
    fn admit_airborne(
        &mut self,
        generation: u64,
        id: u32,
        route: std::sync::Arc<Route>,
        segment: usize,
        authorised: bool,
        progress: f64,
    ) -> bool {
        if generation != self.generation
            || id == 0
            || (self.aircraft.len() >= 63 && !self.aircraft.contains_key(&id))
        {
            return false;
        }
        if let Some(existing) = self.aircraft.get(&id)
            && !(existing.segment == 0
                && existing.elapsed == 0.0
                && !existing.authorised
                && !existing.complete
                && !existing.next_authorised
                && existing.airborne_authority.is_none()
                && existing.hold_exit.is_none()
                && existing.surface_reservation.is_none()
                && existing.recovery_from.is_none())
        {
            return false;
        }
        let Some(initial) = route.segments.get(segment) else {
            return false;
        };
        if initial.stage.on_ground() || initial.points.len() < 2 {
            return false;
        }
        if initial
            .resources
            .iter()
            .any(|resource| self.owners.contains_key(resource))
        {
            return false;
        }
        let speed = initial.knots;
        // The aircraft is no longer standing on its planned stand; release whatever the
        // unstarted surface route held so another flight can use it.
        self.owners.retain(|_, owner| *owner != id);
        let rates = self.aircraft.get(&id).and_then(|aircraft| aircraft.rates);
        let duration = route.moving_seconds[segment] + initial.dwell_seconds;
        self.aircraft.insert(
            id,
            Aircraft {
                assigned_route: route.clone(),
                route,
                segment,
                elapsed: (duration * progress).min(duration),
                authorised,
                complete: false,
                next_authorised: false,
                airborne_authority: None,
                hold_exit: None,
                speed_limits: BTreeMap::new(),
                rates,
                speed,
                holding: false,
                recoveries: BTreeMap::new(),
                recovery_from: None,
                surface_reservation: None,
            },
        );
        true
    }

    /// Insert a local procedure that starts on the surface and later needs
    /// advisory authorisation for its takeoff and climb.
    pub fn insert_local(
        &mut self,
        generation: u64,
        id: u32,
        route: std::sync::Arc<Route>,
        segment: usize,
    ) -> bool {
        if !self.insert(generation, id, route) {
            return false;
        }
        if segment > 0 && !self.place_on_segment(id, segment) {
            self.remove(id);
            return false;
        }
        self.advisory.insert(id);
        true
    }

    /// Admit an aircraft that flies a published advisory procedure with no controller behind it.
    ///
    /// The procedure is the authority for this traffic, so the aircraft is admitted without
    /// movement permission and owns no surface resource. Only the segments of the admitted
    /// route can ever be authorised, and the published runway is held for the landing roll
    /// alone.
    pub fn insert_advisory(
        &mut self,
        generation: u64,
        id: u32,
        route: std::sync::Arc<Route>,
        segment: usize,
    ) -> bool {
        if self.aircraft.contains_key(&id) && !self.advisory.contains(&id) {
            return false;
        }
        if !self.admit_airborne(generation, id, route, segment, false, 0.0) {
            return false;
        }
        self.advisory.insert(id);
        true
    }

    /// Whether an aircraft flies a published advisory procedure.
    #[must_use]
    pub fn is_advisory(&self, id: u32) -> bool {
        self.advisory.contains(&id)
    }

    /// Grant the next segment of a published advisory procedure.
    ///
    /// The procedure is the authority, so the grant follows the admitted route. A taxi, a
    /// runway entry and a segment beyond the route are refused: an advisory procedure never
    /// authorises controlled movement.
    /// # Panics
    ///
    /// Panics if the route segment index is outside the validated range.
    pub fn authorise_advisory(
        &mut self,
        generation: u64,
        id: u32,
        segment: usize,
        blocked: &BTreeSet<String>,
    ) -> bool {
        if generation != self.generation || !self.advisory.contains(&id) {
            return false;
        }
        let Some(aircraft) = self.aircraft.get(&id) else {
            return false;
        };
        if aircraft.segment != segment {
            return false;
        }
        let Some(current) = aircraft.route.segments.get(segment) else {
            return false;
        };
        if !matches!(
            current.stage,
            Stage::Pushback
                | Stage::TaxiOut
                | Stage::TaxiIn
                | Stage::Holding
                | Stage::LineUp
                | Stage::Takeoff
                | Stage::Climb
                | Stage::Cruise
                | Stage::Descent
                | Stage::Approach
                | Stage::Rollout
        ) {
            return false;
        }
        if !self.reserve(generation, id, segment, blocked) {
            return false;
        }
        let aircraft = self.aircraft.get_mut(&id).unwrap();
        aircraft.authorised = true;
        aircraft.holding = false;
        true
    }

    /// Install the next advisory circuit at a completed boundary, without granting movement.
    ///
    /// The procedure continues from the exact point the previous circuit ended, so the
    /// aircraft is never repositioned.
    pub fn continue_advisory(
        &mut self,
        generation: u64,
        id: u32,
        route: std::sync::Arc<Route>,
    ) -> bool {
        let Some(aircraft) = self.aircraft.get(&id) else {
            return false;
        };
        let current = aircraft.pose(id);
        if !aircraft.complete || current.knots > 0.0 {
            return false;
        }
        self.redirect_advisory(generation, id, route)
    }

    /// Redirect an advisory aircraft onto a new circuit from its present position.
    ///
    /// The procedure continues from the exact point the aircraft reached, so it is never
    /// repositioned, and its movement permission follows it onto the new circuit.
    /// # Panics
    ///
    /// Panics if the route segment index is outside the validated range.
    pub fn redirect_advisory(
        &mut self,
        generation: u64,
        id: u32,
        route: std::sync::Arc<Route>,
    ) -> bool {
        if generation != self.generation || !self.advisory.contains(&id) {
            return false;
        }
        let Some(aircraft) = self.aircraft.get(&id) else {
            return false;
        };
        let current = aircraft.pose(id);
        let Some(first) = route.segments.first() else {
            return false;
        };
        if first.stage.on_ground()
            || distance_nm(current.position.coordinate(), first.points[0].coordinate()) > 0.001
            || (current.position.altitude_feet - first.points[0].altitude_feet).abs() > 1.0
        {
            return false;
        }
        let authorised = aircraft.authorised;
        let aircraft = self.aircraft.get_mut(&id).unwrap();
        aircraft.speed_limits.clear();
        aircraft.assigned_route = route.clone();
        aircraft.route = route;
        aircraft.segment = 0;
        aircraft.elapsed = 0.0;
        aircraft.authorised = authorised;
        aircraft.next_authorised = false;
        aircraft.airborne_authority = None;
        aircraft.hold_exit = None;
        aircraft.complete = false;
        aircraft.holding = false;
        aircraft.surface_reservation = None;
        aircraft.recoveries.clear();
        aircraft.recovery_from = None;
        true
    }
    /// Admit an aircraft already rolling on a surface segment of its route.
    ///
    /// The current segment is the only movement granted. Its protected cells are taken so no
    /// other aircraft can be admitted into them, and the next boundary still requires a
    /// controller clearance. A started aircraft is never reseeded or teleported.
    pub fn seed_ground(
        &mut self,
        generation: u64,
        id: u32,
        route: std::sync::Arc<Route>,
        segment: usize,
    ) -> bool {
        self.seed_ground_progress(generation, id, route, segment, 0.0)
    }

    /// Admit an aircraft partway through a surface segment, or completed on it.
    ///
    /// A progress of zero starts at the segment boundary, which is the normal seed. Values
    /// between zero and one place the aircraft along the segment so a scene can open with
    /// traffic already taxiing. A progress of one completes the segment and leaves the aircraft
    /// stopped at its end; that is used for an aircraft already at a runway holding point.
    pub fn seed_ground_progress(
        &mut self,
        generation: u64,
        id: u32,
        route: std::sync::Arc<Route>,
        segment: usize,
        progress: f64,
    ) -> bool {
        if generation != self.generation
            || id == 0
            || !progress.is_finite()
            || !(0.0..=1.0).contains(&progress)
            || (self.aircraft.len() >= 63 && !self.aircraft.contains_key(&id))
        {
            return false;
        }
        if let Some(existing) = self.aircraft.get(&id)
            && !(existing.segment == 0
                && existing.elapsed == 0.0
                && !existing.authorised
                && !existing.complete
                && !existing.next_authorised
                && existing.airborne_authority.is_none()
                && existing.hold_exit.is_none()
                && existing.surface_reservation.is_none()
                && existing.recovery_from.is_none())
        {
            return false;
        }
        let Some(initial) = route.segments.get(segment) else {
            return false;
        };
        if !initial.stage.on_ground()
            || !matches!(
                initial.stage,
                Stage::Pushback | Stage::TaxiOut | Stage::TaxiIn | Stage::Crossing
            )
            || initial.points.len() < 2
        {
            return false;
        }
        if initial
            .resources
            .iter()
            .any(|resource| self.owners.contains_key(resource))
        {
            return false;
        }
        let complete = progress >= 1.0;
        let elapsed = if complete {
            route.moving_seconds[segment] + initial.dwell_seconds
        } else {
            route.moving_seconds[segment].max(0.0) * progress
        };
        let speed = initial.knots;
        self.owners.retain(|_, owner| *owner != id);
        let rates = self.aircraft.get(&id).and_then(|aircraft| aircraft.rates);
        for resource in &initial.resources {
            self.owners.insert(resource.clone(), id);
        }
        self.aircraft.insert(
            id,
            Aircraft {
                assigned_route: route.clone(),
                route,
                segment,
                elapsed,
                authorised: !complete,
                complete,
                next_authorised: false,
                airborne_authority: None,
                hold_exit: None,
                speed_limits: BTreeMap::new(),
                rates,
                speed: if complete { 0.0 } else { speed },
                holding: false,
                recoveries: BTreeMap::new(),
                recovery_from: None,
                surface_reservation: None,
            },
        );
        true
    }
    /// Spread the first start-up call of an unstarted aircraft without moving it.
    pub fn stagger_initial_dwell(&mut self, generation: u64, id: u32, extra_seconds: f64) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        if aircraft.segment != 0
            || aircraft.elapsed != 0.0
            || aircraft.authorised
            || aircraft.complete
        {
            return false;
        }
        let mut route = (*aircraft.route).clone();
        if !route.delay_initial_dwell(extra_seconds) {
            return false;
        }
        aircraft.route = std::sync::Arc::new(route.clone());
        aircraft.assigned_route = std::sync::Arc::new(route);
        true
    }
    /// Returns false for stale, conflicting or already completed instructions.
    /// # Panics
    ///
    /// Panics if the route segment index is outside the validated range.
    pub fn authorise(
        &mut self,
        generation: u64,
        id: u32,
        segment: usize,
        blocked: &BTreeSet<String>,
    ) -> bool {
        let Some(aircraft) = self.aircraft.get(&id) else {
            return false;
        };
        let stage = aircraft.route.segments[aircraft.segment].stage;
        if !stage.on_ground()
            && stage != Stage::Holding
            && !aircraft.recoveries.contains_key(&aircraft.segment)
            && !aircraft.next_authorised
        {
            return false;
        }
        if !self.reserve(generation, id, segment, blocked) {
            return false;
        }
        let aircraft = self.aircraft.get_mut(&id).unwrap();
        aircraft.authorised = true;
        aircraft.holding = false;
        true
    }
    /// Register a checked recovery before granting a movement that could wait airborne.
    /// Terrain, procedure and separation validation belongs to the controller planner.
    pub fn recovery(
        &mut self,
        generation: u64,
        id: u32,
        boundary: usize,
        route: std::sync::Arc<Route>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        if !Self::recovery_matches(&aircraft.route, boundary, &route) {
            return false;
        }
        if let Some((_, braking)) = aircraft.rates {
            if !route.speed_changes_fit(braking) {
                return false;
            }
            let segment = &aircraft.route.segments[boundary];
            let duration = aircraft.route.legs[boundary]
                .iter()
                .map(|leg| leg.duration)
                .sum::<f64>();
            let (speed, remaining) = if boundary == aircraft.segment {
                (
                    aircraft.speed,
                    (duration - aircraft.elapsed).max(0.0) * segment.knots,
                )
            } else {
                (segment.knots, duration * segment.knots)
            };
            let slowing =
                (speed.powi(2) - route.segments[0].knots.powi(2)).max(0.0) / (2.0 * braking);
            if remaining + 1e-8 < slowing {
                return false;
            }
        }
        aircraft.recoveries.insert(boundary, route);
        true
    }
    fn recovery_matches(original: &Route, boundary: usize, route: &Route) -> bool {
        let Some(current) = original.segments.get(boundary) else {
            return false;
        };
        if current.stage.on_ground() || current.stage == Stage::Holding {
            return false;
        }
        let last = current.points.last().unwrap();
        let first = route.segments[0].points[0];
        let incoming = original.legs[boundary].last().map_or(0.0, |leg| leg.course);
        let outgoing = route.legs[0].first().map_or(incoming, |leg| leg.course);
        if distance_nm(last.coordinate(), first.coordinate()) > 0.001
            || (last.altitude_feet - first.altitude_feet).abs() > 1.0
            || crate::navigation::turn(incoming, outgoing).abs() > 30.0
            || route.segments.last().unwrap().stage != Stage::Holding
            || route
                .segments
                .iter()
                .any(|segment| segment.stage.on_ground() || !segment.resources.is_empty())
        {
            return false;
        }
        true
    }
    /// Apply an approved airborne reroute without granting its later runway or taxi movements.
    pub fn replace_airborne(
        &mut self,
        generation: u64,
        id: u32,
        route: std::sync::Arc<Route>,
        recoveries: BTreeMap<usize, std::sync::Arc<Route>>,
        blocked: &BTreeSet<String>,
    ) -> bool {
        if generation != self.generation
            || route.segments[0].stage.on_ground()
            || route.segments[0].stage != Stage::Holding && !recoveries.contains_key(&0)
            || recoveries
                .iter()
                .any(|(boundary, recovery)| !Self::recovery_matches(&route, *boundary, recovery))
        {
            return false;
        }
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        if !aircraft.authorised
            || aircraft.complete
            || aircraft.route.segments[aircraft.segment].stage.on_ground()
        {
            return false;
        }
        if let Some((_, braking)) = aircraft.rates
            && recoveries.iter().any(|(boundary, recovery)| {
                !route.recovery_speed_fits(*boundary, recovery, braking)
            })
        {
            return false;
        }
        let current = aircraft.pose(id);
        let first = &route.segments[0];
        let outgoing = route.legs[0]
            .first()
            .map_or(current.heading, |leg| leg.course);
        if distance_nm(current.position.coordinate(), first.points[0].coordinate()) > 0.001
            || (current.position.altitude_feet - first.points[0].altitude_feet).abs() > 1.0
            || crate::navigation::turn(current.heading, outgoing).abs() > 30.0
            || first.knots + 0.1 < aircraft.speed
            || first.resources.iter().any(|resource| {
                blocked.contains(resource)
                    || self.owners.get(resource).is_some_and(|owner| *owner != id)
            })
        {
            return false;
        }
        self.owners.retain(|_, owner| *owner != id);
        for resource in &first.resources {
            self.owners.insert(resource.clone(), id);
        }
        aircraft.speed_limits.clear();
        aircraft.assigned_route = route.clone();
        aircraft.route = route;
        aircraft.segment = 0;
        aircraft.elapsed = 0.0;
        aircraft.authorised = true;
        aircraft.next_authorised = false;
        aircraft.airborne_authority = None;
        aircraft.hold_exit = None;
        aircraft.complete = false;
        aircraft.holding = false;
        aircraft.surface_reservation = None;
        aircraft.recovery_from = None;
        aircraft.recoveries = recoveries;
        true
    }
    /// Queue a checked onward flight for the next crossing of the holding fix.
    /// The aircraft completes its current circuit rather than jumping to the new path.
    pub fn leave_hold_at_fix(
        &mut self,
        generation: u64,
        id: u32,
        route: std::sync::Arc<Route>,
        recoveries: BTreeMap<usize, std::sync::Arc<Route>>,
        through: usize,
    ) -> bool {
        self.queue_hold_exit(generation, id, route, recoveries, through)
            .is_ok()
    }

    /// Validate and queue an onward route, retaining the reason for a refused join.
    pub fn queue_hold_exit(
        &mut self,
        generation: u64,
        id: u32,
        route: std::sync::Arc<Route>,
        recoveries: BTreeMap<usize, std::sync::Arc<Route>>,
        through: usize,
    ) -> Result<(), String> {
        if generation != self.generation {
            return Err("Holding exit belongs to another generation".into());
        }
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return Err("Holding aircraft no longer exists".into());
        };
        let current = &aircraft.route.segments[aircraft.segment];
        if current.stage != Stage::Holding
            || !aircraft.authorised
            || aircraft.complete
            || aircraft.hold_exit.is_some()
            || through >= route.segments.len()
            || !matches!(
                route.segments[0].stage,
                Stage::Climb
                    | Stage::Cruise
                    | Stage::Descent
                    | Stage::Approach
                    | Stage::MissedApproach
            )
            || route.segments[..=through]
                .iter()
                .enumerate()
                .any(|(index, segment)| {
                    segment.stage != route.segments[0].stage
                        || !segment.resources.is_empty()
                        || !recoveries.contains_key(&index)
                })
            || route.segments.iter().enumerate().any(|(index, segment)| {
                !segment.stage.on_ground()
                    && segment.stage != Stage::Holding
                    && !recoveries.contains_key(&index)
            })
            || recoveries
                .iter()
                .any(|(boundary, recovery)| !Self::recovery_matches(&route, *boundary, recovery))
        {
            return Err(
                "Holding exit lacks an active hold and complete movement/recovery contracts".into(),
            );
        }
        let anchor = current.points[0];
        let first = route.segments[0].points[0];
        let incoming = aircraft.route.legs[aircraft.segment]
            .last()
            .map_or(0.0, |leg| leg.course);
        let outgoing = route.legs[0].first().map_or(incoming, |leg| leg.course);
        if distance_nm(anchor.coordinate(), first.coordinate()) > 0.001
            || (anchor.altitude_feet - first.altitude_feet).abs() > 1.0
            || crate::navigation::turn(incoming, outgoing).abs() > 30.0
        {
            return Err(format!(
                "Holding exit join differs by {:.3} NM / {:.1} feet / {:.1} degrees; speeds {:.1} to {:.1} knots",
                distance_nm(anchor.coordinate(), first.coordinate()),
                anchor.altitude_feet - first.altitude_feet,
                crate::navigation::turn(incoming, outgoing),
                current.knots,
                route.segments[0].knots
            ));
        }
        let mut defer_circuits = 0;
        if route.segments[0].knots + 0.1 < aircraft.speed.max(current.knots) {
            let braking = aircraft
                .rates
                .ok_or("Slower holding exit requires a braking envelope")?
                .1;
            let slowing = (aircraft.speed.max(current.knots).powi(2)
                - route.segments[0].knots.powi(2))
            .max(0.0)
                / (2.0 * braking);
            let circuit = aircraft.duration() * current.knots;
            if slowing > circuit + 1e-8 {
                return Err("Holding circuit is too short to brake for the onward route".into());
            }
            if slowing > (aircraft.duration() - aircraft.elapsed) * current.knots {
                defer_circuits = 1;
            }
        }
        if let Some((_, braking)) = aircraft.rates {
            if recoveries.iter().any(|(boundary, recovery)| {
                !route.recovery_speed_fits(*boundary, recovery, braking)
            }) {
                return Err("Holding exit cannot brake for an onward recovery".into());
            }
            for index in 0..through {
                let segment = &route.segments[index];
                let distance = route.legs[index]
                    .iter()
                    .map(|leg| leg.duration)
                    .sum::<f64>()
                    * segment.knots;
                let slowing = (segment.knots.powi(2) - route.segments[index + 1].knots.powi(2))
                    .max(0.0)
                    / (2.0 * braking);
                if distance + 1e-8 < slowing {
                    return Err("Holding exit cannot brake within its granted scope".into());
                }
            }
        }
        aircraft.hold_exit = Some(HoldExit {
            route,
            recoveries,
            through,
            defer_circuits,
        });
        Ok(())
    }

    /// Include any extra circuit and the actual braking time before a queued holding exit.
    #[must_use]
    pub fn holding_exit_seconds(&self, id: u32) -> Option<f64> {
        let aircraft = self.aircraft.get(&id)?;
        let exit = aircraft.hold_exit.as_ref()?;
        let segment = &aircraft.route.segments[aircraft.segment];
        let remaining = (aircraft.duration() - aircraft.elapsed).max(0.0);
        let Some((acceleration, braking)) = aircraft.rates else {
            return Some(remaining);
        };
        let (delay, distance, speed) = if exit.defer_circuits > 0 {
            let pending = SpeedChange::new(aircraft.speed, segment.knots, acceleration, braking);
            let delay = pending.seconds_to(remaining * segment.knots);
            (
                delay,
                aircraft.duration() * segment.knots,
                pending.sample(delay).1,
            )
        } else {
            (0.0, remaining * segment.knots, aircraft.speed)
        };
        let profile = BoundarySpeed::new(
            speed,
            segment.knots,
            exit.route.segments[0].knots,
            distance,
            acceleration,
            braking,
        )?;
        Some(delay + profile.seconds_to(distance))
    }

    /// Withdraw a departure grant before the roll; never stop an aircraft already taking off.
    pub fn withhold_takeoff(&mut self, generation: u64, id: u32) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        let current = &aircraft.route.segments[aircraft.segment];
        if current.stage == Stage::Takeoff && aircraft.speed <= 0.0 && aircraft.elapsed <= 0.0 {
            aircraft.authorised = false;
        } else if current.stage == Stage::LineUp
            && aircraft
                .route
                .segments
                .get(aircraft.segment + 1)
                .is_some_and(|segment| segment.stage == Stage::Takeoff)
        {
            // Keep completing the lineup, but withdraw its onward takeoff permission.
        } else {
            return false;
        }
        aircraft.next_authorised = false;
        aircraft.airborne_authority = None;
        true
    }

    /// Withdraw an upcoming limit without interrupting the current airborne segment.
    pub fn withdraw_continuation(&mut self, generation: u64, id: u32, next: usize) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        if aircraft.complete
            || aircraft.segment.checked_add(1) != Some(next)
            || aircraft.route.segments.get(next).is_none()
        {
            return false;
        }
        aircraft.next_authorised = false;
        aircraft.airborne_authority = None;
        let current = &aircraft.route.segments[aircraft.segment].resources;
        self.owners.retain(|resource, owner| {
            *owner != id
                || current.contains(resource)
                || aircraft
                    .surface_reservation
                    .as_ref()
                    .is_some_and(|(_, reserved)| reserved.contains(resource))
        });
        true
    }

    /// Cancel an onward clearance while retaining the current holding circuit.
    pub fn cancel_hold_exit(&mut self, generation: u64, id: u32) -> bool {
        if generation != self.generation {
            return false;
        }
        self.aircraft
            .get_mut(&id)
            .is_some_and(|aircraft| aircraft.hold_exit.take().is_some())
    }

    /// Route identity survives worker clones and distinguishes a queued flight from its predecessor.
    #[must_use]
    pub fn follows(&self, id: u32, route: &std::sync::Arc<Route>) -> bool {
        self.aircraft
            .get(&id)
            .is_some_and(|aircraft| std::sync::Arc::ptr_eq(&aircraft.assigned_route, route))
    }

    /// Replace a hold only after the controller approves a complete onward path.
    /// # Panics
    ///
    /// Panics if the replacement route has no segments.
    pub fn leave_hold(&mut self, generation: u64, id: u32, route: std::sync::Arc<Route>) -> bool {
        if generation != self.generation
            || route.segments.last().unwrap().stage != Stage::Holding
            || route
                .segments
                .iter()
                .any(|segment| segment.stage.on_ground() || !segment.resources.is_empty())
        {
            return false;
        }
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        if aircraft.route.segments[aircraft.segment].stage != Stage::Holding {
            return false;
        }
        let current = aircraft.pose(id);
        let first = route.segments[0].points[0];
        let outgoing = route.legs[0]
            .first()
            .map_or(current.heading, |leg| leg.course);
        if route.segments[0].knots + 0.1 < current.knots
            || distance_nm(current.position.coordinate(), first.coordinate()) > 0.001
            || (current.position.altitude_feet - first.altitude_feet).abs() > 1.0
            || crate::navigation::turn(current.heading, outgoing).abs() > 30.0
        {
            return false;
        }
        aircraft.recovery_from.get_or_insert(aircraft.segment);
        aircraft.route = route;
        aircraft.segment = 0;
        aircraft.elapsed = 0.0;
        aircraft.authorised = true;
        aircraft.next_authorised = false;
        aircraft.airborne_authority = None;
        aircraft.hold_exit = None;
        aircraft.complete = false;
        aircraft.recoveries.clear();
        self.owners.retain(|_, owner| *owner != id);
        true
    }
    /// A separately approved onward segment may be reserved before reaching its boundary.
    pub fn authorise_continuation(
        &mut self,
        generation: u64,
        id: u32,
        segment: usize,
        blocked: &BTreeSet<String>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        if segment != aircraft.segment + 1 || !aircraft.authorised || aircraft.holding {
            return false;
        }
        let Some(next) = aircraft.route.segments.get(segment) else {
            return false;
        };
        if let Some((_, braking)) = aircraft.rates {
            let remaining = (aircraft.duration() - aircraft.elapsed)
                * aircraft.route.segments[aircraft.segment].knots
                * (1852.0 / 3600.0);
            let slowing_distance = (aircraft.speed.powi(2) - next.knots.powi(2)).max(0.0)
                * (1852.0 / 3600.0)
                / (2.0 * braking);
            let tolerance = if aircraft.route.segments[aircraft.segment].stage.on_ground() {
                0.02
            } else {
                8.0 * f64::EPSILON * remaining.abs().max(slowing_distance.abs()).max(1.0)
            };
            if remaining + tolerance < slowing_distance {
                return false;
            }
        }
        if !next.stage.on_ground()
            && next.stage != Stage::Holding
            && !aircraft.recoveries.contains_key(&segment)
        {
            return false;
        }
        if next.resources.iter().any(|resource| {
            blocked.contains(resource)
                || self.owners.get(resource).is_some_and(|owner| *owner != id)
        }) {
            return false;
        }
        for resource in &next.resources {
            self.owners.insert(resource.clone(), id);
        }
        aircraft.next_authorised = true;
        true
    }
    /// Grant the explicit airborne limits covered by one accepted readback.
    /// Landing, holding and surface permissions cannot be included. A takeoff clearance
    /// issued from the line-up is the exception: the intervening takeoff segment belongs to
    /// the same clearance and is granted with its initial climb.
    /// # Panics
    ///
    /// Panics if the cleared segment index is outside the route.
    pub fn authorise_airborne_until(
        &mut self,
        generation: u64,
        id: u32,
        first: usize,
        end: usize,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(aircraft) = self.aircraft.get(&id) else {
            return false;
        };
        let current = aircraft.segment;
        let after_takeoff = first == current + 2
            && aircraft
                .route
                .segments
                .get(current)
                .is_some_and(|segment| segment.stage == Stage::LineUp)
            && aircraft
                .route
                .segments
                .get(current + 1)
                .is_some_and(|segment| segment.stage == Stage::Takeoff);
        if aircraft.complete
            || aircraft.recovery_from.is_some()
            || first < current
            || first > current + 1 + usize::from(after_takeoff)
            || end < first
            || end >= aircraft.route.segments.len()
        {
            return false;
        }
        let stage = aircraft.route.segments[first].stage;
        if !matches!(
            stage,
            Stage::Climb | Stage::Cruise | Stage::Descent | Stage::Approach | Stage::MissedApproach
        ) || aircraft.route.segments[first..=end]
            .iter()
            .enumerate()
            .any(|(offset, segment)| {
                segment.stage != stage
                    || !segment.resources.is_empty()
                    || !aircraft.recoveries.contains_key(&(first + offset))
            })
        {
            return false;
        }
        if let Some((_, braking)) = aircraft.rates {
            for index in first..end {
                let current = &aircraft.route.segments[index];
                let distance = aircraft.route.legs[index]
                    .iter()
                    .map(|leg| leg.duration)
                    .sum::<f64>()
                    * current.knots;
                let slowing = (current.knots.powi(2)
                    - aircraft.route.segments[index + 1].knots.powi(2))
                .max(0.0)
                    / (2.0 * braking);
                if distance + 1e-8 < slowing {
                    return false;
                }
            }
        }
        let current = aircraft.segment;
        // A takeoff readback can include the initial SID before the roll starts.
        // Runway entry and the takeoff roll still need their own permission.
        let waiting_takeoff = first == current + 1
            && !aircraft.authorised
            && aircraft.route.segments[current].stage == Stage::Takeoff;
        let accepted = if after_takeoff {
            // Reserve the line-up exit and the takeoff roll before recording the climb.
            self.authorise(generation, id, current, &BTreeSet::new())
                && self.authorise_continuation(generation, id, current + 1, &BTreeSet::new())
        } else if waiting_takeoff {
            true
        } else if first == current {
            self.authorise(generation, id, first, &BTreeSet::new())
        } else {
            self.authorise_continuation(generation, id, first, &BTreeSet::new())
        };
        if !accepted {
            return false;
        }
        let aircraft = self.aircraft.get_mut(&id).unwrap();
        aircraft.airborne_authority = Some((first, end));
        if first == current {
            aircraft.next_authorised = false;
        }
        true
    }

    /// Protect a clearance while its readback is pending, without moving the aircraft.
    /// # Panics
    ///
    /// Panics if the route has no segments or the segment index is outside the route.
    pub fn reserve_surface_route(
        &mut self,
        generation: u64,
        id: u32,
        segment: usize,
        blocked: &BTreeSet<String>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(aircraft) = self.aircraft.get(&id) else {
            return false;
        };
        if aircraft.segment != segment || aircraft.complete {
            return false;
        }
        let stage = aircraft.route.segments[segment].stage;
        if !matches!(stage, Stage::TaxiOut | Stage::TaxiIn | Stage::Pushback) {
            return self.reserve(generation, id, segment, blocked);
        }
        let end = aircraft
            .route
            .segments
            .iter()
            .enumerate()
            .skip(segment)
            .take_while(|(_, next)| next.stage == stage)
            .map(|(index, _)| index)
            .last()
            .unwrap();
        self.reserve_surface_until(generation, id, segment, end, blocked)
    }

    /// Protect only the contiguous surface movements covered by one clearance, without moving.
    pub fn reserve_surface_until(
        &mut self,
        generation: u64,
        id: u32,
        segment: usize,
        end: usize,
        blocked: &BTreeSet<String>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(aircraft) = self.aircraft.get(&id) else {
            return false;
        };
        if aircraft.segment != segment
            || aircraft.complete
            || end < segment
            || end >= aircraft.route.segments.len()
        {
            return false;
        }
        let stage = aircraft.route.segments[segment].stage;
        if !matches!(stage, Stage::TaxiOut | Stage::TaxiIn | Stage::Pushback)
            || aircraft.route.segments[segment..=end]
                .iter()
                .any(|next| next.stage != stage)
        {
            return false;
        }
        // A follower sharing the taxiway with a leader ahead is cleared up to the cell before
        // the leader and released again at that boundary. Holding it instead leaves the
        // follower parked until it requests taxi again.
        let external = (segment..=end).find(|index| {
            aircraft.route.segments[*index]
                .resources
                .iter()
                .any(|resource| blocked.contains(resource))
        });
        let owned = (segment..=end).find(|index| {
            aircraft.route.segments[*index]
                .resources
                .iter()
                .any(|resource| self.owners.get(resource).is_some_and(|owner| *owner != id))
        });
        let limit = match (external, owned) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(index), None) | (None, Some(index)) => Some(index),
            (None, None) => None,
        };
        let end = match limit {
            None => end,
            Some(index) => {
                if index == segment || external == Some(index) {
                    return false;
                }
                let conflict =
                    aircraft.route.segments[index]
                        .resources
                        .iter()
                        .find_map(|resource| {
                            self.owners
                                .get(resource)
                                .copied()
                                .filter(|owner| *owner != id)
                        });
                let Some(conflict) = conflict else {
                    return false;
                };
                if !self.companion(id, conflict) {
                    return false;
                }
                index - 1
            }
        };
        let resources: BTreeSet<_> = aircraft.route.segments[segment..=end]
            .iter()
            .flat_map(|next| next.resources.iter().cloned())
            .collect();
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        for resource in &resources {
            self.owners.insert(resource.clone(), id);
        }
        aircraft.surface_reservation = Some((end, resources));
        true
    }

    /// Reserve one protected segment without granting movement.
    pub fn reserve(
        &mut self,
        generation: u64,
        id: u32,
        segment: usize,
        blocked: &BTreeSet<String>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(a) = self.aircraft.get(&id) else {
            return false;
        };
        if a.segment != segment || a.complete {
            return false;
        }
        let resources = a.route.segments[segment].resources.clone();
        if resources.iter().any(|resource| {
            blocked.contains(resource)
                || self
                    .owners
                    .get(resource)
                    .is_some_and(|owner| *owner != id && !self.behind(id, *owner))
        }) {
            return false;
        }
        let Some(a) = self.aircraft.get_mut(&id) else {
            return false;
        };
        for resource in &resources {
            self.owners.insert(resource.clone(), id);
        }
        let _ = a;
        true
    }
    /// Withdraw an unexecuted surface grant while retaining the aircraft's occupied cells.
    pub fn cancel_reservation(
        &mut self,
        generation: u64,
        id: u32,
        occupied: &BTreeSet<String>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        if !aircraft.route.segments[aircraft.segment].stage.on_ground() || aircraft.speed > 0.0 {
            return false;
        }
        aircraft.authorised = false;
        aircraft.next_authorised = false;
        aircraft.airborne_authority = None;
        aircraft.hold_exit = None;
        aircraft.surface_reservation = None;
        self.owners
            .retain(|resource, owner| *owner != id || occupied.contains(resource));
        for resource in occupied {
            self.owners.entry(resource.clone()).or_insert(id);
        }
        true
    }
    /// Brake surface traffic when a new observed obstruction enters its reserved path.
    /// Only known blocking cells suspend movement. A missing observation feed says nothing about
    /// the airport, so it cannot stop an aircraft that holds its own clearance.
    pub fn recheck_surface(&mut self, blocked: &BTreeSet<String>) {
        for (id, aircraft) in &mut self.aircraft {
            let current = &aircraft.route.segments[aircraft.segment];
            if !matches!(
                current.stage,
                Stage::TaxiOut | Stage::TaxiIn | Stage::Pushback
            ) {
                continue;
            }
            aircraft.holding = current
                .resources
                .iter()
                .any(|resource| blocked.contains(resource));
            if aircraft.holding
                || aircraft
                    .route
                    .segments
                    .get(aircraft.segment + 1)
                    .is_some_and(|next| {
                        next.resources
                            .iter()
                            .any(|resource| blocked.contains(resource))
                    })
            {
                aircraft.next_authorised = false;
                aircraft.airborne_authority = None;
                aircraft.hold_exit = None;
                self.owners.retain(|resource, owner| {
                    *owner != *id
                        || current.resources.contains(resource)
                        || aircraft
                            .surface_reservation
                            .as_ref()
                            .is_some_and(|(_, reserved)| reserved.contains(resource))
                });
            }
        }
    }
    /// Apply a read-back pre-final speed restriction along existing protected geometry.
    /// This grants no movement authority and never changes final, hold or recovery speeds.
    pub fn restrict_arrival_speed(
        &mut self,
        generation: u64,
        id: u32,
        limits: &BTreeMap<usize, f64>,
    ) -> bool {
        if generation != self.generation || limits.is_empty() || limits.len() > 512 {
            return false;
        }
        let Some(a) = self.aircraft.get_mut(&id) else {
            return false;
        };
        if a.recovery_from.is_some() || a.complete || a.rates.is_none()
            || limits.keys().next().copied() != Some(a.segment)
            || limits.iter().enumerate().any(|(offset, (&index, &speed))| {
                index != a.segment + offset || a.route.segments.get(index).is_none_or(|segment| {
                    !matches!(segment.stage, Stage::Cruise | Stage::Descent)
                        || !(40.0..=segment.knots).contains(&speed)
                        // Retain the checked speed contract at a clearance-limit hold.
                        || a.recoveries.get(&index).is_some_and(|route| speed < route.segments[0].knots)
                })
            }) { return false; }
        a.speed_limits.clone_from(limits);
        true
    }

    /// Whether the current pre-final segment has an accepted speed restriction.
    #[must_use]
    pub fn arrival_speed_restricted(&self, id: u32) -> bool {
        self.aircraft
            .get(&id)
            .is_some_and(|a| a.recovery_from.is_none() && a.speed_limits.contains_key(&a.segment))
    }

    /// Set per-aircraft acceleration and braking from a validated profile.
    pub fn ground_rates(&mut self, id: u32, acceleration: f64, braking: f64) -> bool {
        if !(0.1..=10.0).contains(&acceleration) || !(0.1..=10.0).contains(&braking) {
            return false;
        }
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        if aircraft.authorised
            && !aircraft.route.segments[aircraft.segment].stage.on_ground()
            && aircraft.rates != Some((acceleration, braking))
        {
            return false;
        }
        if aircraft.recoveries.iter().any(|(boundary, recovery)| {
            !aircraft
                .route
                .recovery_speed_fits(*boundary, recovery, braking)
        }) {
            return false;
        }
        aircraft.rates = Some((acceleration, braking));
        true
    }
    /// Reserve one following taxi leg to avoid stopping at every graph node.
    /// Runway entry, crossing and takeoff require their own controller grant.
    pub fn reserve_next_taxi(
        &mut self,
        generation: u64,
        id: u32,
        blocked: &BTreeSet<String>,
    ) -> bool {
        if self.generation != generation {
            return false;
        }
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        let next = aircraft.segment + 1;
        let Some(segment) = aircraft.route.segments.get(next) else {
            return false;
        };
        if !aircraft.authorised
            || aircraft.holding
            || !matches!(segment.stage, Stage::TaxiOut | Stage::TaxiIn)
            || !matches!(
                aircraft.route.segments[aircraft.segment].stage,
                Stage::TaxiOut | Stage::TaxiIn
            )
            || segment.resources.iter().any(|resource| {
                blocked.contains(resource)
                    || self.owners.get(resource).is_some_and(|owner| *owner != id)
            })
        {
            return false;
        }
        for resource in &segment.resources {
            self.owners.insert(resource.clone(), id);
        }
        aircraft.next_authorised = true;
        true
    }
    /// Extend a stopped taxi movement when the queue ahead has moved.
    /// A follower does not need a new radio clearance merely because it reached the end of a
    /// reservation that was shortened behind the leader, or stopped at the start of a shared
    /// cell that the leader has now released.
    pub fn resume_taxi(
        &mut self,
        generation: u64,
        id: u32,
        blocked: &BTreeSet<String>,
        current_approved: bool,
        next_approved: bool,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(aircraft) = self.aircraft.get(&id) else {
            return false;
        };
        if aircraft.complete
            || aircraft.recovery_from.is_some()
            || aircraft.holding
            || !matches!(
                aircraft.route.segments[aircraft.segment].stage,
                Stage::TaxiOut | Stage::TaxiIn
            )
        {
            return false;
        }
        let current = aircraft.segment;
        let at_end = aircraft.elapsed + 1e-6 >= aircraft.duration();
        let rolling_into_segment = current > 0 && aircraft.elapsed <= 1e-6;
        if !at_end && !rolling_into_segment {
            return false;
        }
        let Some(next_segment) = aircraft.route.segments.get(current + 1) else {
            return false;
        };
        let next_is_taxi = matches!(next_segment.stage, Stage::TaxiOut | Stage::TaxiIn);
        if at_end && !next_is_taxi {
            return false;
        }
        let current_resources = aircraft.route.segments[current].resources.clone();
        let next_resources = next_segment.resources.clone();
        let blocked_resources = |resources: &[String]| {
            resources.iter().any(|resource| {
                blocked.contains(resource)
                    || self.owners.get(resource).is_some_and(|owner| *owner != id)
            })
        };
        if rolling_into_segment && (!current_approved || blocked_resources(&current_resources)) {
            return false;
        }
        if at_end && (!next_approved || blocked_resources(&next_resources)) {
            return false;
        }
        let Some(aircraft) = self.aircraft.get_mut(&id) else {
            return false;
        };
        if rolling_into_segment {
            for resource in &current_resources {
                self.owners.insert(resource.clone(), id);
            }
            aircraft.authorised = true;
        }
        if at_end {
            for resource in &next_resources {
                self.owners.insert(resource.clone(), id);
            }
            aircraft.authorised = true;
            aircraft.next_authorised = true;
        }
        true
    }

    /// Pausing and large clock changes cannot move traffic through a clearance limit.
    /// # Panics
    ///
    /// Panics if a segment duration or sample count is outside the validated range.
    pub fn advance(&mut self, seconds: f64, paused: bool) -> Advance {
        if !seconds.is_finite() || seconds < 0.0 {
            return Advance::Invalid;
        }
        let previous = self.clock.replace(seconds);
        if paused {
            return Advance::Paused;
        }
        let Some(previous) = previous else {
            return Advance::Initial;
        };
        let elapsed = seconds - previous;
        if !(0.0..=30.0).contains(&elapsed) {
            // Keep aircraft and occupied resources; require the controller to reconsider each grant.
            for aircraft in self.aircraft.values_mut() {
                if aircraft.route.segments[aircraft.segment].stage.on_ground() {
                    aircraft.authorised = false;
                    aircraft.speed = 0.0;
                }
                aircraft.next_authorised = false;
                aircraft.airborne_authority = None;
                aircraft.hold_exit = None;
            }
            return Advance::ClockChanged;
        }
        let steps = (elapsed / 0.5).ceil().max(1.0) as usize;
        let elapsed = elapsed / steps as f64;
        for _ in 0..steps {
            for (id, aircraft) in &mut self.aircraft {
                if !aircraft.authorised || aircraft.complete {
                    continue;
                }
                let mut pending_seconds = elapsed;
                for _ in 0..512 {
                    if pending_seconds <= 0.0 || !aircraft.authorised || aircraft.complete {
                        break;
                    }
                    let segment = &aircraft.route.segments[aircraft.segment];
                    let next_authorised = aircraft.next_authorised
                        || aircraft.airborne_authority.is_some_and(|(first, end)| {
                            (first..=end).contains(&(aircraft.segment + 1))
                        });
                    let cruise_limit = aircraft
                        .speed_limits
                        .get(&aircraft.segment)
                        .copied()
                        .unwrap_or(segment.knots);
                    let duration = aircraft.duration();
                    let remaining_seconds = (duration - aircraft.elapsed).max(0.0);
                    let consumed = if let Some((acceleration, braking)) = aircraft.rates
                        && segment.knots > 0.0
                    {
                        let remaining = (aircraft.duration() - aircraft.elapsed)
                            * segment.knots
                            * (1852.0 / 3600.0);
                        let holding_exit = segment.stage == Stage::Holding
                            && aircraft
                                .hold_exit
                                .as_ref()
                                .is_some_and(|exit| exit.defer_circuits == 0);
                        let next_speed = if holding_exit {
                            aircraft
                                .hold_exit
                                .as_ref()
                                .map(|exit| exit.route.segments[0].knots)
                        } else if next_authorised || aircraft.recovery_from.is_some() {
                            aircraft
                                .route
                                .segments
                                .get(aircraft.segment + 1)
                                .map(|segment| {
                                    aircraft
                                        .speed_limits
                                        .get(&(aircraft.segment + 1))
                                        .copied()
                                        .unwrap_or(segment.knots)
                                })
                        } else if segment.stage == Stage::Takeoff {
                            // Keep the take-off roll smooth while the climb clearance is
                            // pending; do not brake toward the recovery speed.
                            Some(segment.knots)
                        } else {
                            aircraft
                                .recoveries
                                .get(&aircraft.segment)
                                .map(|route| route.segments[0].knots)
                        };
                        let distance_to_limit = remaining
                            + if next_authorised {
                                let index = aircraft.segment + 1;
                                aircraft.route.moving_seconds[index]
                                    * aircraft.route.segments[index].knots
                                    * (1852.0 / 3600.0)
                            } else {
                                0.0
                            };
                        let nonstop = aircraft.recovery_from.is_some()
                            || segment.stage == Stage::Holding
                            || !segment.stage.on_ground()
                                && aircraft.recoveries.contains_key(&aircraft.segment);
                        let target = if aircraft.holding && segment.stage.on_ground() {
                            0.0
                        } else {
                            let limit = if nonstop {
                                cruise_limit
                            } else {
                                (2.0 * braking * (1852.0 / 3600.0) * distance_to_limit).sqrt()
                                    / (1852.0 / 3600.0)
                            };
                            let next_limit = next_speed.map_or(segment.knots, |speed| {
                                (speed.powi(2) + 2.0 * braking * remaining / (1852.0 / 3600.0))
                                    .sqrt()
                            });
                            cruise_limit.min(limit).min(next_limit)
                        };
                        let change =
                            SpeedChange::new(aircraft.speed, target, acceleration, braking);
                        if segment.stage.on_ground()
                            && speed_target_changed(*id, aircraft.segment, target)
                            && *MOTION_DEBUG
                                .get_or_init(|| std::env::var_os("OPENATC_MOTION_DEBUG").is_some())
                        {
                            eprintln!(
                                "MOTION t={:?} {id} seg {} {:?} elapsed {:.1}/{:.1} speed {:.1} target {:.1} knots {:.1} next {:?} next_auth {} remaining {:.1} auth {} holding {}",
                                self.clock,
                                aircraft.segment,
                                segment.stage,
                                aircraft.elapsed,
                                aircraft.duration(),
                                aircraft.speed,
                                target,
                                segment.knots,
                                next_speed,
                                next_authorised,
                                remaining,
                                aircraft.authorised,
                                aircraft.holding
                            );
                        }
                        let to_boundary = remaining_seconds * segment.knots;
                        let boundary = if !segment.stage.on_ground()
                            && (segment.stage != Stage::Holding || holding_exit)
                        {
                            next_speed.and_then(|end| {
                                BoundarySpeed::new(
                                    aircraft.speed,
                                    cruise_limit,
                                    end,
                                    to_boundary,
                                    acceleration,
                                    braking,
                                )
                            })
                        } else {
                            None
                        };
                        let sample = |seconds| {
                            boundary.as_ref().map_or_else(
                                || change.sample(seconds),
                                |boundary| boundary.sample(seconds),
                            )
                        };
                        let (distance, speed) = sample(pending_seconds);
                        if distance < to_boundary
                            && !(remaining < 0.02 && segment.stage.on_ground())
                        {
                            aircraft.elapsed += distance / segment.knots;
                            aircraft.speed = speed;
                            break;
                        }
                        let consumed = if distance < to_boundary {
                            0.0
                        } else {
                            boundary
                                .as_ref()
                                .map_or_else(
                                    || change.seconds_to(to_boundary),
                                    |boundary| boundary.seconds_to(to_boundary),
                                )
                                .clamp(0.0, pending_seconds)
                        };
                        aircraft.speed = sample(consumed).1;
                        aircraft.elapsed = duration;
                        consumed
                    } else {
                        let consumed = pending_seconds.min(remaining_seconds);
                        aircraft.elapsed = (aircraft.elapsed + consumed).min(duration);
                        consumed
                    };
                    pending_seconds = (pending_seconds - consumed).max(0.0);
                    if aircraft.elapsed < duration {
                        break;
                    }
                    if segment.stage == Stage::Holding {
                        aircraft.elapsed = 0.0;
                        if aircraft
                            .hold_exit
                            .as_ref()
                            .is_some_and(|exit| exit.defer_circuits > 0)
                        {
                            aircraft.hold_exit.as_mut().unwrap().defer_circuits -= 1;
                        } else if let Some(exit) = aircraft.hold_exit.take() {
                            self.owners.retain(|_, owner| *owner != *id);
                            aircraft.speed_limits.clear();
                            aircraft.assigned_route = exit.route.clone();
                            aircraft.route = exit.route;
                            aircraft.recoveries = exit.recoveries;
                            aircraft.segment = 0;
                            aircraft.next_authorised = false;
                            aircraft.airborne_authority = Some((0, exit.through));
                            aircraft.recovery_from = None;
                            aircraft.holding = false;
                            aircraft.surface_reservation = None;
                        }
                        continue;
                    }
                    if aircraft.elapsed >= aircraft.duration() {
                        if !next_authorised
                            && !segment.stage.on_ground()
                            && let Some(recovery) = aircraft.recoveries.remove(&aircraft.segment)
                        {
                            self.owners.retain(|_, owner| *owner != *id);
                            aircraft.airborne_authority = None;
                            aircraft.hold_exit = None;
                            aircraft.recovery_from = Some(aircraft.segment);
                            aircraft.speed_limits.clear();
                            aircraft.route = recovery;
                            aircraft.segment = 0;
                            aircraft.elapsed = 0.0;
                            aircraft.authorised = true;
                            aircraft.complete = false;
                            aircraft.recoveries.clear();
                            continue;
                        }
                        let next = aircraft.segment + 1;
                        // Retain shared resources until the segment that actually leaves them completes.
                        let retained = aircraft
                            .route
                            .segments
                            .get(next)
                            .map(|s| &s.resources)
                            .or_else(|| {
                                aircraft.route.segments[aircraft.segment]
                                    .stage
                                    .on_ground()
                                    .then_some(&aircraft.route.segments[aircraft.segment].resources)
                            });
                        self.owners.retain(|resource, owner| {
                            *owner != *id
                                || retained.is_some_and(|r| r.contains(resource))
                                || aircraft.surface_reservation.as_ref().is_some_and(
                                    |(end, resources)| next <= *end && resources.contains(resource),
                                )
                        });
                        if aircraft
                            .surface_reservation
                            .as_ref()
                            .is_some_and(|(end, _)| next > *end)
                        {
                            aircraft.surface_reservation = None;
                        }
                        aircraft.authorised = next_authorised || aircraft.recovery_from.is_some();
                        aircraft.next_authorised = false;

                        if next == aircraft.route.segments.len() {
                            aircraft.complete = true;
                        } else {
                            aircraft.segment = next;
                            aircraft.elapsed = 0.0;
                        }
                    }
                }
            }
        }
        Advance::Advanced
    }
    #[must_use]
    /// Publish at most 63 complete poses without exposing mutable movement state.
    pub fn snapshot(&self) -> Vec<Pose> {
        self.aircraft.iter().map(|(id, a)| a.pose(*id)).collect()
    }
    /// Density changes retire completed flights; they never remove active runway traffic.
    pub fn retire(&mut self, id: u32) -> bool {
        if !self.aircraft.get(&id).is_some_and(|a| a.complete) {
            return false;
        }
        self.aircraft.remove(&id);
        self.owners.retain(|_, owner| *owner != id);
        true
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    fn point(meters: f64, altitude: f64) -> Waypoint {
        Waypoint {
            latitude: 0.0,
            longitude: meters / 111_320.0,
            altitude_feet: altitude,
        }
    }
    fn crossing() -> std::sync::Arc<Route> {
        std::sync::Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Crossing,
                        points: vec![point(0.0, 0.0), point(100.0, 0.0)],
                        knots: 10.0,
                        resources: vec!["TEST/09-27".into()],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::TaxiIn,
                        points: vec![point(100.0, 0.0), point(200.0, 0.0)],
                        knots: 10.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                ],
                1000.0,
                1000.0,
            )
            .unwrap(),
        )
    }
    #[test]
    fn compiled_durations_preserve_sample_sums_suffix_and_dwell() {
        let mut segments = crossing().segments.clone();
        segments[0].points = (0..=100).map(|i| point(f64::from(i), 0.0)).collect();
        segments[0].dwell_seconds = 7.5;
        let route = Route::compile(segments, 1000.0, 1000.0).unwrap();
        for (index, legs) in route.legs.iter().enumerate() {
            assert_eq!(
                route.moving_seconds[index],
                legs.iter().map(|leg| leg.duration).sum::<f64>()
            );
        }
        let suffix = route.suffix(1).unwrap();
        assert_eq!(suffix.moving_seconds, route.moving_seconds[1..]);
        let mut world = World::default();
        assert!(world.insert(0, 1, std::sync::Arc::new(route.clone())));
        assert_eq!(world.aircraft[&1].duration(), route.moving_seconds[0] + 7.5);
        assert_eq!(
            route.nominal_seconds(0, 1),
            Some(route.legs.iter().flatten().map(|leg| leg.duration).sum())
        );
    }
    #[test]
    fn prepared_world_merge_retains_identities_and_rejects_old_grants_or_clocks() {
        let prepared = |id| {
            let mut world = World::default();
            assert!(world.insert(0, id, crossing()));
            world
        };
        let mut world = prepared(1);
        assert!(!world.merge_prepared(prepared(1)));
        let mut granted = prepared(2);
        assert!(granted.authorise(0, 2, 0, &BTreeSet::new()));
        assert!(!world.merge_prepared(granted));
        let mut clocked = prepared(2);
        clocked.advance(0.0, false);
        assert!(!world.merge_prepared(clocked));
        assert_eq!(world.snapshot().len(), 1);
        assert!(world.merge_prepared(prepared(2)));
        assert_eq!(
            world
                .snapshot()
                .iter()
                .map(|pose| pose.id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        world.advance(0.0, false);
        world.advance(10.0, false);
        assert!(world.snapshot().iter().all(|pose| pose.knots == 0.0));
        assert!(!world.merge_prepared(prepared(3)));
        let mut full = World::default();
        for id in 1..=63 {
            assert!(full.insert(0, id, crossing()));
        }
        assert!(!full.merge_prepared(prepared(64)));
        assert_eq!(full.snapshot().len(), 63);
    }

    #[test]
    fn live_surface_transfer_keeps_progress_clock_and_reservations() {
        let mut previous = World::default();
        assert!(previous.insert(0, 7, crossing()));
        previous.advance(1000.0, false);
        assert!(previous.authorise(0, 7, 0, &BTreeSet::new()));
        previous.advance(1005.0, false);
        let before = previous.snapshot()[0].clone();
        let mut next = World::default();
        assert!(next.preserve_surface(&previous, 7));
        assert_eq!(next.snapshot()[0].position, before.position);
        assert_eq!(next.snapshot()[0].knots, before.knots);
        assert_eq!(next.resource_owner("TEST/09-27"), Some(7));
        next.advance(1006.0, false);
        assert!(next.snapshot()[0].position.longitude > before.position.longitude);
        let mut conflicting = World::default();
        assert!(conflicting.insert(0, 8, crossing()));
        assert!(conflicting.authorise(0, 8, 0, &BTreeSet::new()));
        assert!(!conflicting.preserve_surface(&previous, 7));
        assert_eq!(conflicting.snapshot().len(), 1);
        assert_eq!(conflicting.resource_owner("TEST/09-27"), Some(8));
        let mut stale = World::default();
        stale.reset();
        assert!(!stale.preserve_surface(&previous, 7));
    }

    #[test]
    fn incremental_admission_preserves_a_running_clock_and_refuses_conflicting_owners() {
        let mut world = World::default();
        assert!(world.insert(0, 7, crossing()));
        world.advance(1000.0, false);
        assert!(world.authorise(0, 7, 0, &BTreeSet::new()));
        world.advance(1005.0, false);
        let before = world.snapshot()[0].clone();
        let mut prepared = World::default();
        assert!(prepared.insert(0, 8, crossing()));
        assert!(prepared.authorise(0, 8, 0, &BTreeSet::new()));
        assert!(!world.admit_prepared(&prepared, 8));
        let stand = std::sync::Arc::new(
            Route::compile(
                vec![Segment {
                    stage: Stage::Parked,
                    points: vec![point(1000.0, 0.0)],
                    knots: 0.0,
                    resources: vec!["FREE STAND".into()],
                    dwell_seconds: 5.0,
                }],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        assert!(prepared.insert(0, 9, stand));
        assert!(world.admit_prepared(&prepared, 9));
        assert!(!world.admit_prepared(&prepared, 9));
        assert_eq!(world.resource_owner("FREE STAND"), Some(9));
        assert_eq!(
            world
                .snapshot()
                .iter()
                .find(|pose| pose.id == 7)
                .unwrap()
                .position,
            before.position
        );
        assert_eq!(world.advance(1006.0, false), Advance::Advanced);
        assert!(
            world
                .snapshot()
                .iter()
                .find(|pose| pose.id == 7)
                .unwrap()
                .position
                .longitude
                > before.position.longitude
        );
        prepared.advance(0.0, false);
        world.remove(9);
        assert!(
            !world.admit_prepared(&prepared, 9),
            "A sampled source cannot introduce a second clock"
        );
    }

    #[test]
    fn bounded_surface_reservation_protects_the_clearance_without_granting_it() {
        let segment = |index, resource: &str| Segment {
            stage: Stage::TaxiIn,
            points: vec![
                point(f64::from(index) * 100.0, 0.0),
                point(f64::from(index + 1) * 100.0, 0.0),
            ],
            knots: 10.0,
            resources: vec![resource.to_owned()],
            dwell_seconds: 0.0,
        };
        let route = std::sync::Arc::new(
            Route::compile(
                vec![segment(0, "A"), segment(1, "B"), segment(2, "C")],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        let mut world = World::default();
        assert!(world.insert(0, 1, route));
        assert!(!world.reserve_surface_until(0, 1, 0, 3, &BTreeSet::new()));
        assert!(world.reserve_surface_until(0, 1, 0, 1, &BTreeSet::new()));
        assert!(world.snapshot()[0].awaiting_clearance);
        for (id, resource, expected) in [(2, "C", true), (3, "B", false)] {
            let route = std::sync::Arc::new(
                Route::compile(vec![segment(0, resource)], 1000.0, 1000.0).unwrap(),
            );
            assert!(world.insert(0, id, route));
            assert_eq!(
                world.reserve(0, id, 0, &BTreeSet::new()),
                expected,
                "Reservation extended beyond, or lost part of, its clearance"
            );
        }
        let before = world.snapshot()[0].position;
        world.advance(0.0, false);
        world.advance(10.0, false);
        assert_eq!(
            world.snapshot()[0].position,
            before,
            "A reservation moved an aircraft before readback"
        );
    }

    #[test]
    fn replacing_a_finished_route_releases_stale_surface_owners() {
        let segment = |stage, from, to, resource: &str| Segment {
            stage,
            points: vec![point(from, 0.0), point(to, 0.0)],
            knots: 10.0,
            resources: vec![resource.to_owned()],
            dwell_seconds: 0.0,
        };
        let arrival = std::sync::Arc::new(
            Route::compile(
                vec![segment(Stage::TaxiIn, 0.0, 100.0, "STAND")],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        let mut world = World::default();
        assert!(world.insert(0, 1, arrival));
        assert!(world.reserve(0, 1, 0, &BTreeSet::new()));
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        world.advance(0.0, false);
        world.advance(30.0, false);
        assert!(world.snapshot()[0].complete);
        // A completed arrival can leave an older segment's runway entry behind. The
        // aircraft is on its stand, so that entry must not survive the next route.
        world.owners.insert("RUNWAY/LFMN/04R/22L".into(), 1);
        let departure = std::sync::Arc::new(
            Route::compile(
                vec![segment(Stage::TaxiOut, 100.0, 200.0, "TAXI/A")],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        assert!(world.continue_surface(0, 1, departure));
        assert!(
            !world.owners.contains_key("RUNWAY/LFMN/04R/22L"),
            "the replaced arrival route must not leave the runway owned"
        );
        assert_eq!(
            world.owners.get("STAND"),
            Some(&1),
            "the cell the aircraft is standing on stays reserved"
        );
        let follower = std::sync::Arc::new(
            Route::compile(
                vec![segment(Stage::TaxiOut, 0.0, 100.0, "RUNWAY/LFMN/04R/22L")],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        assert!(world.insert(0, 2, follower));
        assert!(
            world.reserve(0, 2, 0, &BTreeSet::new()),
            "the next departure must be able to reserve the runway"
        );
    }

    #[test]
    fn taxi_corridor_waits_before_entering_a_shared_lane() {
        let route = |start: &str| {
            std::sync::Arc::new(
                Route::compile(
                    vec![
                        Segment {
                            stage: Stage::TaxiOut,
                            points: vec![point(0.0, 0.0), point(100.0, 0.0)],
                            knots: 10.0,
                            resources: vec![start.into()],
                            dwell_seconds: 0.0,
                        },
                        Segment {
                            stage: Stage::TaxiOut,
                            points: vec![point(100.0, 0.0), point(200.0, 0.0)],
                            knots: 10.0,
                            resources: vec!["SHARED".into()],
                            dwell_seconds: 0.0,
                        },
                        Segment {
                            stage: Stage::Crossing,
                            points: vec![point(200.0, 0.0), point(300.0, 0.0)],
                            knots: 10.0,
                            resources: vec!["RUNWAY".into()],
                            dwell_seconds: 0.0,
                        },
                    ],
                    1000.0,
                    1000.0,
                )
                .unwrap(),
            )
        };
        let mut world = World::default();
        assert!(world.insert(0, 1, route("RAMP1")));
        assert!(world.insert(0, 2, route("RAMP2")));
        assert!(world.reserve_surface_route(0, 1, 0, &BTreeSet::new()));
        // The follower is cleared up to the merge, but not into the shared lane.
        assert!(world.reserve_surface_route(0, 2, 0, &BTreeSet::new()));
        assert_eq!(world.owners.get("RAMP2"), Some(&2));
        assert_eq!(world.owners.get("SHARED"), Some(&1));
        assert!(!world.owners.contains_key("RUNWAY"));
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        assert!(world.reserve_next_taxi(0, 1, &BTreeSet::new()));
        world.advance(0.0, false);
        world.advance(25.0, false);
        assert_eq!(world.snapshot()[0].segment, 1);
        assert_eq!(world.owners.get("SHARED"), Some(&1));
        world.advance(50.0, false);
        assert_eq!(world.snapshot()[0].stage, Stage::Crossing);
        assert!(world.snapshot()[0].awaiting_clearance);
        assert!(world.reserve_surface_route(0, 2, 0, &BTreeSet::new()));
    }
    #[test]
    fn short_surface_braking_limits_survive_compilation_roundoff() {
        for length in 2..100 {
            let metres = f64::from(length) / 100.0;
            let mut segments = vec![
                Segment {
                    stage: Stage::TaxiIn,
                    points: vec![point(0.0, 0.0), point(metres, 0.0)],
                    knots: 15.0,
                    resources: vec![],
                    dwell_seconds: 0.0,
                },
                Segment {
                    stage: Stage::TaxiIn,
                    points: vec![point(metres, 0.0), point(metres + 100.0, 0.0)],
                    knots: 8.0,
                    resources: vec![],
                    dwell_seconds: 0.0,
                },
            ];
            limit_surface_speeds(&mut segments, 2.0, &BTreeSet::new()).unwrap();
            Route::compile(segments, 1000.0, 1000.0)
                .unwrap()
                .validate_rates(2.0, 2.0)
                .unwrap();
        }
    }

    #[test]
    fn surface_speed_planning_brakes_before_short_turns_and_parking() {
        let mut segments = vec![
            Segment {
                stage: Stage::TaxiIn,
                points: vec![point(0.0, 0.0), point(25.0, 0.0)],
                knots: 15.0,
                resources: vec!["A".into()],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::TaxiIn,
                points: vec![point(25.0, 0.0), point(30.0, 0.0)],
                knots: 15.0,
                resources: vec!["B".into()],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::TaxiIn,
                points: vec![point(30.0, 0.0), point(35.0, 0.0)],
                knots: 8.0,
                resources: vec!["C".into()],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::Turnaround,
                points: vec![point(35.0, 0.0)],
                knots: 0.0,
                resources: vec!["STAND".into()],
                dwell_seconds: 0.0,
            },
        ];
        let original = segments.clone();
        assert!(
            Route::compile(original.clone(), 1000.0, 1000.0)
                .unwrap()
                .validate_rates(3.0, 3.0)
                .is_err()
        );
        limit_surface_speeds(&mut segments, 3.0, &BTreeSet::from([2])).unwrap();
        for (before, after) in original.iter().zip(&segments) {
            assert_eq!(before.points, after.points);
            assert_eq!(before.resources, after.resources);
            assert!(after.knots <= before.knots);
        }
        let route = std::sync::Arc::new(Route::compile(segments, 1000.0, 1000.0).unwrap());
        route.validate_rates(3.0, 3.0).unwrap();
        let mut world = World::default();
        assert!(world.insert(0, 1, route));
        assert!(world.ground_rates(1, 3.0, 3.0));
        world.advance(0.0, false);
        for step in 1..400 {
            let pose = world.snapshot()[0].clone();
            if pose.awaiting_clearance {
                assert!(world.authorise(0, 1, pose.segment, &BTreeSet::new()));
            }
            if !pose.awaiting_clearance && pose.segment < 3 {
                world.authorise_continuation(0, 1, pose.segment + 1, &BTreeSet::new());
            }
            world.advance(f64::from(step) * 0.25, false);
            if world.snapshot()[0].complete {
                break;
            }
        }
        let pose = &world.snapshot()[0];
        assert_eq!(pose.stage, Stage::Turnaround);
        assert!(pose.complete);
        assert_eq!(pose.position, point(35.0, 0.0));
        let mut hold = vec![Segment {
            stage: Stage::TaxiOut,
            points: vec![point(0.0, 0.0)],
            knots: 0.0,
            resources: vec![],
            dwell_seconds: 0.0,
        }];
        limit_surface_speeds(&mut hold, 3.0, &BTreeSet::new()).unwrap();
    }

    #[test]
    fn wake_withdrawal_stops_an_unstarted_roll_without_stopping_a_moving_departure() {
        let mut world = World::default();
        let route = std::sync::Arc::new(
            Route::compile(
                vec![Segment {
                    stage: Stage::Takeoff,
                    points: vec![point(0.0, 0.0), point(2000.0, 0.0)],
                    knots: 80.0,
                    resources: vec!["RUNWAY/TEST/09/27".into()],
                    dwell_seconds: 0.0,
                }],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        assert!(world.insert(0, 1, route));
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        assert!(!world.withhold_takeoff(1, 1));
        assert!(world.withhold_takeoff(0, 1));
        world.advance(0.0, false);
        world.advance(1.0, false);
        let stopped = world.snapshot()[0].clone();
        assert_eq!(stopped.position, point(0.0, 0.0));
        assert!(stopped.awaiting_clearance);
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        world.advance(2.0, false);
        let rolling = world.snapshot()[0].clone();
        assert!(rolling.knots > 0.0);
        assert!(!world.withhold_takeoff(0, 1));
        world.advance(3.0, false);
        assert!(world.snapshot()[0].position.longitude > rolling.position.longitude);
    }

    #[test]
    fn departure_phase_keeps_the_hold_until_a_new_controller_grant() {
        let mut world = World::default();
        let taxi = std::sync::Arc::new(
            Route::compile(
                vec![Segment {
                    stage: Stage::TaxiOut,
                    points: vec![point(0.0, 0.0), point(100.0, 0.0)],
                    knots: 10.0,
                    resources: vec!["HOLD".into()],
                    dwell_seconds: 0.0,
                }],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        let departure = |start| {
            std::sync::Arc::new(
                Route::compile(
                    vec![Segment {
                        stage: Stage::Takeoff,
                        points: vec![point(start, 0.0), point(1000.0, 0.0)],
                        knots: 60.0,
                        resources: vec!["RUNWAY".into()],
                        dwell_seconds: 0.0,
                    }],
                    1000.0,
                    1000.0,
                )
                .unwrap(),
            )
        };
        world.insert(0, 1, taxi);
        assert!(!world.continue_surface(0, 1, departure(100.0)));
        world.authorise(0, 1, 0, &BTreeSet::new());
        world.advance(0.0, false);
        world.advance(25.0, false);
        assert!(!world.continue_surface(0, 1, departure(200.0)));
        assert!(world.continue_surface(0, 1, departure(100.0)));
        assert_eq!(world.owners.get("HOLD"), Some(&1));
        assert!(!world.owners.contains_key("RUNWAY"));
        world.advance(26.0, false);
        assert_eq!(world.snapshot()[0].position, point(100.0, 0.0));
        assert!(world.snapshot()[0].awaiting_clearance);
        assert!(!world.authorise(0, 1, 0, &BTreeSet::from(["RUNWAY".into()])));
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        world.advance(27.0, false);
        assert!(world.snapshot()[0].position.longitude > point(100.0, 0.0).longitude);
    }
    #[test]
    fn airborne_reroute_requires_recovery_and_keeps_landing_and_taxi_grants_separate() {
        let hold = |fix: Waypoint, heading| {
            let segment = crate::traffic::holding::Pattern {
                fix,
                inbound_degrees: heading,
                right: true,
                ground_knots: 100.0,
                leg_seconds: 60.0,
                bank_degrees: 25.0,
                turn_rate_degrees_sec: 3.0,
            }
            .compile(|_, _| true)
            .unwrap();
            std::sync::Arc::new(Route::compile(vec![segment], 1000.0, 1000.0).unwrap())
        };
        let fix = Waypoint {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 1500.0,
        };
        let mut world = World::default();
        world.insert(0, 1, hold(fix, 90.0));
        world.ground_rates(1, 1.0, 5.0);
        world.authorise(0, 1, 0, &BTreeSet::new());
        world.advance(0.0, false);
        world.advance(1.0, false);
        let current = world.snapshot()[0].clone();
        let next = |from: Waypoint, meters, height| {
            let coordinate = destination(from.coordinate(), current.heading, meters / 1852.0);
            Waypoint {
                latitude: coordinate[0],
                longitude: coordinate[1],
                altitude_feet: height,
            }
        };
        let touchdown = next(current.position, 1852.0, 1000.0);
        let exit = next(touchdown, 1000.0, 1000.0);
        let gate = next(exit, 200.0, 1000.0);
        let route = std::sync::Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Approach,
                        points: vec![current.position, touchdown],
                        knots: 100.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Rollout,
                        points: vec![touchdown, exit],
                        knots: 100.0,
                        resources: vec!["RUNWAY".into()],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::TaxiIn,
                        points: vec![exit, gate],
                        knots: 10.0,
                        resources: vec!["TAXI".into()],
                        dwell_seconds: 0.0,
                    },
                ],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        assert!(!world.replace_airborne(0, 1, route.clone(), BTreeMap::new(), &BTreeSet::new()));
        assert_eq!(world.snapshot()[0].stage, Stage::Holding);
        let recovery = BTreeMap::from([(0, hold(touchdown, current.heading))]);
        let blocked = BTreeSet::from(["RUNWAY".into()]);
        assert!(world.replace_airborne(0, 1, route, recovery, &blocked));
        assert!(!world.owners.contains_key("RUNWAY"));
        assert!(!world.authorise_continuation(0, 1, 1, &blocked));
        let mut cleared = world.clone();
        assert!(cleared.authorise_continuation(0, 1, 1, &BTreeSet::new()));
        for second in 2..=100 {
            world.advance(f64::from(second), false);
            cleared.advance(f64::from(second), false);
        }
        let refused = &world.snapshot()[0];
        assert_eq!(refused.stage, Stage::Holding);
        assert!(!refused.awaiting_clearance);
        assert!(!refused.stage.on_ground());
        let landed = &cleared.snapshot()[0];
        assert_eq!(landed.stage, Stage::TaxiIn);
        assert!(landed.awaiting_clearance);
        assert_eq!(landed.position, exit);
        assert!(!cleared.owners.contains_key("RUNWAY"));
    }

    #[test]
    fn speed_changes_carry_real_time_across_a_clearance_boundary() {
        let start = Waypoint {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 5000.0,
        };
        let point = |nm| {
            let coordinate = destination(start.coordinate(), 90.0, nm);
            Waypoint {
                latitude: coordinate[0],
                longitude: coordinate[1],
                ..start
            }
        };
        let entry = point(160.0 * 0.2 / 3600.0);
        let end = point(2.0);
        let hold = |fix, knots| {
            crate::traffic::holding::Pattern {
                fix,
                inbound_degrees: 90.0,
                right: true,
                ground_knots: knots,
                leg_seconds: 60.0,
                bank_degrees: 25.0,
                turn_rate_degrees_sec: 3.0,
            }
            .compile(|_, _| true)
            .unwrap()
        };
        let segment = |from, to, knots| Segment {
            stage: Stage::Cruise,
            points: vec![from, to],
            knots,
            resources: vec![],
            dwell_seconds: 0.0,
        };
        let route = std::sync::Arc::new(
            Route::compile(
                vec![
                    segment(start, entry, 160.0),
                    segment(entry, end, 100.0),
                    hold(end, 100.0),
                ],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        let mut world = World::default();
        assert!(world.insert(0, 1, route));
        assert!(world.ground_rates(1, 0.1, 0.1));
        for (boundary, fix, knots) in [(0, entry, 160.0), (1, end, 100.0)] {
            let recovery = std::sync::Arc::new(
                Route::compile(vec![hold(fix, knots)], 1000.0, 1000.0).unwrap(),
            );
            assert!(world.recovery(0, 1, boundary, recovery));
        }
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        // Represent a speed carried from a preceding slower phase.
        world.aircraft.get_mut(&1).unwrap().speed = 80.0;
        assert!(world.authorise_continuation(0, 1, 1, &BTreeSet::new()));
        world.advance(0.0, false);
        world.advance(0.5, false);
        let pose = &world.snapshot()[0];
        assert_eq!(pose.segment, 1);
        assert!((pose.knots - 80.05).abs() < 1e-8);
        let expected_nm = (80.0 * 0.5 + 0.5 * 0.1 * 0.5_f64.powi(2)) / 3600.0;
        let actual_nm = distance_nm(start.coordinate(), pose.position.coordinate());
        assert!(
            (actual_nm - expected_nm).abs() < 1e-7,
            "Travelled {actual_nm}, expected {expected_nm} NM"
        );
    }

    #[test]
    fn approach_speed_is_reached_before_the_slower_segment_begins() {
        let start = Waypoint {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 5000.0,
        };
        let point = |nm| {
            let coordinate = destination(start.coordinate(), 90.0, nm);
            Waypoint {
                latitude: coordinate[0],
                longitude: coordinate[1],
                ..start
            }
        };
        // The braking event falls between half-second movement samples.
        let entry = point(15333.3 / 3600.0);
        let end = point(12.0);
        let hold = |fix, knots| {
            crate::traffic::holding::Pattern {
                fix,
                inbound_degrees: 90.0,
                right: true,
                ground_knots: knots,
                leg_seconds: 60.0,
                bank_degrees: 25.0,
                turn_rate_degrees_sec: 3.0,
            }
            .compile(|_, _| true)
            .unwrap()
        };
        let segment = |from, to, knots| Segment {
            stage: Stage::Approach,
            points: vec![from, to],
            knots,
            resources: vec![],
            dwell_seconds: 0.0,
        };
        let route = std::sync::Arc::new(
            Route::compile(
                vec![
                    segment(start, entry, 200.0),
                    segment(entry, end, 100.0),
                    hold(end, 100.0),
                ],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        for step in [0.5, 0.1, 0.02] {
            let mut world = World::default();
            assert!(world.insert(0, 1, route.clone()));
            assert!(world.ground_rates(1, 1.0, 1.0));
            for (boundary, fix, knots) in [(0, entry, 200.0), (1, end, 100.0)] {
                let recovery = std::sync::Arc::new(
                    Route::compile(vec![hold(fix, knots)], 1000.0, 1000.0).unwrap(),
                );
                assert!(world.recovery(0, 1, boundary, recovery));
            }
            assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
            assert!(world.authorise_continuation(0, 1, 1, &BTreeSet::new()));
            world.advance(0.0, false);
            let mut entered = false;
            let mut before = 200.0;
            for tick in 1..=6000 {
                world.advance(f64::from(tick) * step, false);
                let pose = &world.snapshot()[0];
                assert!((before - pose.knots).abs() <= step + 1e-7);
                if pose.segment == 1 {
                    assert!(
                        pose.knots <= 100.0 + 1e-7,
                        "{step}: entered at {} knots",
                        pose.knots
                    );
                    entered = true;
                    break;
                }
                before = pose.knots;
            }
            assert!(entered);
        }
    }

    #[test]
    fn arrival_speed_amendment_is_smooth_and_preserves_final_and_recovery() {
        let point = |nm: f64| {
            let p = destination([0.0, 0.0], 90.0, nm);
            Waypoint {
                latitude: p[0],
                longitude: p[1],
                altitude_feet: 6000.0,
            }
        };
        let hold = |fix| {
            crate::traffic::holding::Pattern {
                fix,
                inbound_degrees: 90.0,
                right: true,
                ground_knots: 140.0,
                leg_seconds: 60.0,
                bank_degrees: 25.0,
                turn_rate_degrees_sec: 3.0,
            }
            .compile(|_, _| true)
            .unwrap()
        };
        let route = std::sync::Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Descent,
                        points: vec![point(0.0), point(10.0)],
                        knots: 220.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Approach,
                        points: vec![point(10.0), point(15.0)],
                        knots: 140.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    hold(point(15.0)),
                ],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        let mut world = World::default();
        assert!(world.insert(0, 1, route));
        assert!(world.ground_rates(1, 1.0, 0.5));
        for (index, end) in [(0, 10.0), (1, 15.0)] {
            assert!(world.recovery(
                0,
                1,
                index,
                std::sync::Arc::new(
                    Route::compile(vec![hold(point(end))], 1000.0, 1000.0).unwrap()
                )
            ));
        }
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        world.advance(0.0, false);
        assert!(!world.restrict_arrival_speed(1, 1, &BTreeMap::from([(0, 180.0)])));
        assert!(
            !world.restrict_arrival_speed(0, 1, &BTreeMap::from([(0, 130.0)])),
            "recovery speed contract"
        );
        assert!(
            !world.restrict_arrival_speed(0, 1, &BTreeMap::from([(0, 180.0), (1, 120.0)])),
            "no final speed amendment"
        );
        assert!(world.restrict_arrival_speed(0, 1, &BTreeMap::from([(0, 180.0)])));
        assert_eq!(world.snapshot()[0].knots, 220.0);
        world.advance(1.0, false);
        assert!((world.snapshot()[0].knots - 219.5).abs() < 1e-6);
        let still = world.snapshot()[0].position;
        world.advance(20.0, true);
        assert_eq!(world.snapshot()[0].position, still);
        assert!(world.authorise_continuation(0, 1, 1, &BTreeSet::new()));
        for second in 21..600 {
            world.advance(f64::from(second), false);
            if world.snapshot()[0].segment == 1 {
                break;
            }
        }
        let final_pose = &world.snapshot()[0];
        assert_eq!(final_pose.stage, Stage::Approach);
        assert!((final_pose.knots - 140.0).abs() < 0.1);
        assert!(!world.arrival_speed_restricted(1));
    }

    #[test]
    fn airborne_spawn_and_late_speed_clearance_keep_the_recovery_flyable() {
        let start = Waypoint {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 5000.0,
        };
        let entry = Waypoint {
            longitude: 0.1,
            ..start
        };
        let end = Waypoint {
            longitude: 0.3,
            altitude_feet: 3000.0,
            ..start
        };
        let hold = |fix, knots| {
            crate::traffic::holding::Pattern {
                fix,
                inbound_degrees: 90.0,
                right: true,
                ground_knots: knots,
                leg_seconds: 60.0,
                bank_degrees: 25.0,
                turn_rate_degrees_sec: 3.0,
            }
            .compile(|_, _| true)
            .unwrap()
        };
        let route = std::sync::Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Cruise,
                        points: vec![start, entry],
                        knots: 200.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Descent,
                        points: vec![entry, end],
                        knots: 100.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    hold(end, 100.0),
                ],
                2000.0,
                2000.0,
            )
            .unwrap(),
        );
        let prepare = || {
            let mut world = World::default();
            assert!(world.insert(0, 1, route.clone()));
            assert!(world.ground_rates(1, 1.0, 1.0));
            for (boundary, fix, knots) in [(0, entry, 200.0), (1, end, 100.0)] {
                let recovery = std::sync::Arc::new(
                    Route::compile(vec![hold(fix, knots)], 2000.0, 2000.0).unwrap(),
                );
                assert!(world.recovery(0, 1, boundary, recovery));
            }
            assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
            assert_eq!(world.snapshot()[0].knots, 200.0);
            world.advance(0.0, false);
            world
        };
        let mut early = prepare();
        assert!(early.authorise_continuation(0, 1, 1, &BTreeSet::new()));
        let mut entered = false;
        for second in 1..=180 {
            early.advance(f64::from(second), false);
            let pose = &early.snapshot()[0];
            if pose.stage == Stage::Descent {
                assert!(
                    pose.knots <= 101.0,
                    "Descent entered before slowing: {}",
                    pose.knots
                );
                entered = true;
                break;
            }
        }
        assert!(entered);
        let mut late = prepare();
        let duration = late.aircraft[&1].duration();
        for second in 1..=duration.floor() as u32 {
            late.advance(f64::from(second), false);
        }
        assert!(!late.authorise_continuation(0, 1, 1, &BTreeSet::new()));
        let slow_recovery =
            std::sync::Arc::new(Route::compile(vec![hold(entry, 100.0)], 2000.0, 2000.0).unwrap());
        assert!(!late.recovery(0, 1, 0, slow_recovery.clone()));
        assert_eq!(late.aircraft[&1].recoveries[&0].segments[0].knots, 200.0);
        let mut alternative = prepare();
        assert!(alternative.recovery(0, 1, 0, slow_recovery));
        for second in 1..=200 {
            alternative.advance(f64::from(second), false);
        }
        assert!(alternative.snapshot()[0].recovering);
        assert!(alternative.snapshot()[0].knots <= 100.0 + 1e-7);
        late.advance(duration + 5.0, false);
        let pose = &late.snapshot()[0];
        assert_eq!(pose.stage, Stage::Holding);
        assert!(!pose.awaiting_clearance);
        assert!(pose.knots > 190.0);
    }

    #[test]
    fn recovery_speed_limits_and_hold_departures_cannot_jump_to_a_slower_speed() {
        let hold = |fix, speed| {
            crate::traffic::holding::Pattern {
                fix,
                inbound_degrees: 90.0,
                right: true,
                ground_knots: speed,
                leg_seconds: 60.0,
                bank_degrees: 25.0,
                turn_rate_degrees_sec: 3.0,
            }
            .compile(|_, _| true)
            .unwrap()
        };
        let start = point(0.0, 5000.0);
        let boundary = point(1000.0, 5000.0);
        let route = std::sync::Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Cruise,
                        points: vec![start, boundary],
                        knots: 200.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    hold(boundary, 200.0),
                ],
                2000.0,
                2000.0,
            )
            .unwrap(),
        );
        let mut world = World::default();
        assert!(world.insert(0, 1, route.clone()));
        assert!(world.ground_rates(1, 1.0, 1.0));
        let valid = std::sync::Arc::new(
            Route::compile(vec![hold(boundary, 200.0)], 2000.0, 2000.0).unwrap(),
        );
        assert!(world.recovery(0, 1, 0, valid.clone()));
        let short = point(1010.0, 5000.0);
        let invalid = std::sync::Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::MissedApproach,
                        points: vec![boundary, short],
                        knots: 200.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    hold(short, 100.0),
                ],
                2000.0,
                2000.0,
            )
            .unwrap(),
        );
        assert!(!world.recovery(0, 1, 0, invalid.clone()));
        assert!(std::sync::Arc::ptr_eq(
            &world.aircraft[&1].recoveries[&0],
            &valid
        ));
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        assert!(!world.replace_airborne(
            0,
            1,
            route.clone(),
            BTreeMap::from([(0, invalid)]),
            &BTreeSet::new()
        ));
        assert!(std::sync::Arc::ptr_eq(&world.aircraft[&1].route, &route));
        assert!(!world.ground_rates(1, 0.1, 0.1));
        assert_eq!(world.aircraft[&1].rates, Some((1.0, 1.0)));
        world.advance(0.0, false);
        world.advance(20.0, false);
        let current = world.snapshot()[0].clone();
        assert!(current.recovering && current.stage == Stage::Holding);
        assert_eq!(current.recovery_from, Some(0));
        let slow = std::sync::Arc::new(
            Route::compile(vec![hold(current.position, 50.0)], 2000.0, 2000.0).unwrap(),
        );
        assert!(!world.leave_hold(0, 1, slow));
        assert_eq!(world.snapshot()[0].position, current.position);
        assert_eq!(world.snapshot()[0].knots, current.knots);
    }

    #[test]
    fn slower_onward_route_brakes_in_the_hold_and_defers_a_late_exit() {
        use std::sync::Arc;
        let hold = |fix, speed| {
            crate::traffic::holding::Pattern {
                fix,
                inbound_degrees: 90.0,
                right: true,
                ground_knots: speed,
                leg_seconds: 60.0,
                bank_degrees: 25.0,
                turn_rate_degrees_sec: 3.0,
            }
            .compile(|_, _| true)
            .unwrap()
        };
        let fix = point(0.0, 5000.0);
        let end = point(8000.0, 5000.0);
        let holding = Arc::new(Route::compile(vec![hold(fix, 175.0)], 2000.0, 2000.0).unwrap());
        let onward = Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Cruise,
                        points: vec![fix, end],
                        knots: 166.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    hold(end, 166.0),
                ],
                2000.0,
                2000.0,
            )
            .unwrap(),
        );
        let fallback = Arc::new(Route::compile(vec![hold(end, 166.0)], 2000.0, 2000.0).unwrap());
        for late in [false, true] {
            let mut world = World::default();
            assert!(world.insert(0, 1, holding.clone()));
            assert!(world.ground_rates(1, 1.0, 1.0));
            assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
            world.advance(0.0, false);
            let duration = world.aircraft[&1].duration();
            if late {
                world.aircraft.get_mut(&1).unwrap().elapsed = duration - 0.01;
            }
            let before = world.snapshot()[0].clone();
            assert!(
                world
                    .queue_hold_exit(
                        0,
                        1,
                        onward.clone(),
                        BTreeMap::from([(0, fallback.clone())]),
                        0
                    )
                    .is_ok()
            );
            assert_eq!(world.snapshot()[0].position, before.position);
            assert_eq!(world.snapshot()[0].knots, before.knots);
            assert_eq!(
                world.aircraft[&1]
                    .hold_exit
                    .as_ref()
                    .unwrap()
                    .defer_circuits,
                u32::from(late)
            );
            let predicted = world.holding_exit_seconds(1).unwrap();
            if late {
                assert!(predicted > duration);
            }
            let mut previous = before;
            let mut left = false;
            for step in 1..2000 {
                let time = f64::from(step) * 0.5;
                world.advance(time, false);
                let current = world.snapshot()[0].clone();
                assert!((current.knots - previous.knots).abs() <= 0.500_001);
                assert!(
                    distance_nm(
                        previous.position.coordinate(),
                        current.position.coordinate()
                    ) <= 176.0 * 0.5 / 3600.0
                );
                if world.follows(1, &onward) {
                    assert!(current.knots <= 166.1);
                    assert!((time - predicted).abs() <= 0.501);
                    left = true;
                    break;
                }
                previous = current;
            }
            assert!(left, "Aircraft must leave after braking at its cleared fix");
        }
    }

    #[test]
    fn one_airborne_authority_covers_only_its_explicit_limits() {
        let hold = |fix| {
            crate::traffic::holding::Pattern {
                fix,
                inbound_degrees: 90.0,
                right: true,
                ground_knots: 100.0,
                leg_seconds: 60.0,
                bank_degrees: 25.0,
                turn_rate_degrees_sec: 3.0,
            }
            .compile(|_, _| true)
            .unwrap()
        };
        let mut segments = (0..3)
            .map(|index| Segment {
                stage: Stage::Cruise,
                points: vec![
                    point(f64::from(index) * 20.0, 5000.0),
                    point(f64::from(index + 1) * 20.0, 5000.0),
                ],
                knots: 100.0,
                resources: vec![],
                dwell_seconds: 0.0,
            })
            .collect::<Vec<_>>();
        segments.push(hold(point(60.0, 5000.0)));
        let mut world = World::default();
        assert!(world.insert(
            0,
            1,
            std::sync::Arc::new(Route::compile(segments, 1000.0, 1000.0).unwrap())
        ));
        assert!(world.ground_rates(1, 1.0, 1.0));
        for boundary in 0..3 {
            let recovery = std::sync::Arc::new(
                Route::compile(
                    vec![hold(point((boundary + 1) as f64 * 20.0, 5000.0))],
                    1000.0,
                    1000.0,
                )
                .unwrap(),
            );
            assert!(world.recovery(0, 1, boundary, recovery));
        }
        assert!(!world.authorise_airborne_until(1, 1, 0, 2));
        assert!(!world.authorise_airborne_until(0, 1, 0, 3));
        assert!(world.snapshot()[0].awaiting_clearance);
        let mut missing = world.clone();
        missing.aircraft.get_mut(&1).unwrap().recoveries.remove(&1);
        assert!(!missing.authorise_airborne_until(0, 1, 0, 2));
        assert!(missing.snapshot()[0].awaiting_clearance);
        let mut narrowed = world.clone();
        assert!(narrowed.authorise_airborne_until(0, 1, 0, 2));
        assert!(narrowed.authorise_continuation(0, 1, 1, &BTreeSet::new()));
        assert!(narrowed.authorise_airborne_until(0, 1, 0, 0));
        narrowed.advance(0.0, false);
        narrowed.advance(0.5, false);
        assert_eq!(narrowed.snapshot()[0].recovery_from, Some(0));
        let mut shortened = world.clone();
        assert!(shortened.authorise_airborne_until(0, 1, 0, 1));
        shortened.advance(0.0, false);
        shortened.advance(1.0, false);
        assert_eq!(shortened.snapshot()[0].recovery_from, Some(1));
        assert!(world.authorise_airborne_until(0, 1, 0, 2));
        let mut clock = world.clone();
        clock.advance(0.0, false);
        assert_eq!(clock.advance(1000.0, false), Advance::ClockChanged);
        clock.advance(1000.5, false);
        assert_eq!(clock.snapshot()[0].recovery_from, Some(0));
        world.advance(0.0, false);
        world.advance(1.0, false);
        assert_eq!(world.snapshot()[0].segment, 2);
        assert!(!world.snapshot()[0].recovering);
        world.advance(2.0, false);
        assert_eq!(world.snapshot()[0].recovery_from, Some(2));
    }

    #[test]
    fn initial_climb_span_waits_for_the_separate_takeoff_permission() {
        let hold = |fix| {
            crate::traffic::holding::Pattern {
                fix,
                inbound_degrees: 90.0,
                right: true,
                ground_knots: 100.0,
                leg_seconds: 60.0,
                bank_degrees: 25.0,
                turn_rate_degrees_sec: 3.0,
            }
            .compile(|_, _| true)
            .unwrap()
        };
        let mut segments = (0..3)
            .map(|index| Segment {
                stage: if index == 0 {
                    Stage::Takeoff
                } else {
                    Stage::Climb
                },
                points: vec![
                    point(f64::from(index) * 20.0, 5000.0),
                    point(f64::from(index + 1) * 20.0, 5000.0),
                ],
                knots: 100.0,
                resources: if index == 0 {
                    vec!["RUNWAY".into()]
                } else {
                    vec![]
                },
                dwell_seconds: 0.0,
            })
            .collect::<Vec<_>>();
        segments.push(hold(point(60.0, 5000.0)));
        let mut world = World::default();
        assert!(world.insert(
            0,
            1,
            std::sync::Arc::new(Route::compile(segments, 1000.0, 1000.0).unwrap())
        ));
        for boundary in 1..3 {
            assert!(
                world.recovery(
                    0,
                    1,
                    boundary,
                    std::sync::Arc::new(
                        Route::compile(
                            vec![hold(point((boundary + 1) as f64 * 20.0, 5000.0))],
                            1000.0,
                            1000.0
                        )
                        .unwrap()
                    )
                )
            );
        }
        assert!(world.authorise_airborne_until(0, 1, 1, 2));
        let stopped = world.snapshot()[0].position;
        world.advance(0.0, false);
        world.advance(1.0, false);
        assert_eq!(world.snapshot()[0].position, stopped);
        assert!(world.snapshot()[0].awaiting_clearance);
        assert!(!world.authorise(0, 1, 0, &BTreeSet::from(["RUNWAY".into()])));
        world.advance(2.0, false);
        assert_eq!(world.snapshot()[0].position, stopped);
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        world.advance(3.0, false);
        assert_eq!(world.snapshot()[0].segment, 2);
        assert!(!world.snapshot()[0].recovering);
        world.advance(4.0, false);
        assert_eq!(world.snapshot()[0].recovery_from, Some(2));
    }

    #[test]
    fn initial_climb_after_line_up_covers_the_takeoff_segment() {
        let hold = |fix| {
            crate::traffic::holding::Pattern {
                fix,
                inbound_degrees: 90.0,
                right: true,
                ground_knots: 100.0,
                leg_seconds: 60.0,
                bank_degrees: 25.0,
                turn_rate_degrees_sec: 3.0,
            }
            .compile(|_, _| true)
            .unwrap()
        };
        let mut segments = [Stage::LineUp, Stage::Takeoff, Stage::Climb, Stage::Climb]
            .iter()
            .enumerate()
            .map(|(index, stage)| Segment {
                stage: *stage,
                points: vec![
                    point(index as f64 * 40.0, 5000.0),
                    point((index + 1) as f64 * 40.0, 5000.0),
                ],
                knots: if *stage == Stage::LineUp { 10.0 } else { 100.0 },
                resources: if matches!(*stage, Stage::LineUp | Stage::Takeoff) {
                    vec!["RUNWAY".into()]
                } else {
                    vec![]
                },
                dwell_seconds: 0.0,
            })
            .collect::<Vec<_>>();
        segments.push(hold(point(160.0, 5000.0)));
        let mut world = World::default();
        assert!(world.insert(
            0,
            1,
            std::sync::Arc::new(Route::compile(segments, 1000.0, 1000.0).unwrap())
        ));
        for boundary in 2..4 {
            assert!(
                world.recovery(
                    0,
                    1,
                    boundary,
                    std::sync::Arc::new(
                        Route::compile(
                            vec![hold(point((boundary + 1) as f64 * 40.0, 5000.0))],
                            1000.0,
                            1000.0
                        )
                        .unwrap()
                    )
                )
            );
        }
        assert!(world.authorise_airborne_until(0, 1, 2, 3));
        assert!(!world.snapshot()[0].awaiting_clearance);
        world.advance(0.0, false);
        for step in 1..=20 {
            world.advance(f64::from(step) * 0.5, false);
        }
        assert!(world.snapshot()[0].segment >= 2);
        assert!(!world.snapshot()[0].recovering);
    }

    #[test]
    fn a_taxi_follower_resumes_when_the_queue_ahead_moves() {
        let route = |first: &str, second: &str, north: f64| {
            std::sync::Arc::new(
                Route::compile(
                    vec![
                        Segment {
                            stage: Stage::TaxiOut,
                            points: vec![point(0.0, north), point(100.0, north)],
                            knots: 10.0,
                            resources: vec![first.into()],
                            dwell_seconds: 0.0,
                        },
                        Segment {
                            stage: Stage::TaxiOut,
                            points: vec![point(100.0, north), point(200.0, north)],
                            knots: 10.0,
                            resources: vec![second.into()],
                            dwell_seconds: 0.0,
                        },
                        Segment {
                            stage: Stage::Turnaround,
                            points: vec![point(200.0, north)],
                            knots: 0.0,
                            resources: vec![],
                            dwell_seconds: 0.0,
                        },
                    ],
                    1000.0,
                    1000.0,
                )
                .unwrap(),
            )
        };
        let mut world = World::default();
        assert!(world.insert(0, 1, route("SHARED", "L1", 0.0)));
        assert!(world.insert(0, 2, route("F0", "SHARED", -20.0)));
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        assert!(world.authorise(0, 2, 0, &BTreeSet::new()));
        world.advance(0.0, false);
        world.advance(25.0, false);
        assert_eq!(world.snapshot()[0].segment, 1);
        let follower = world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == 2)
            .unwrap();
        assert_eq!(follower.segment, 1);
        assert!(follower.awaiting_clearance);
        let start = follower.position.longitude;
        assert!(world.resume_taxi(0, 2, &BTreeSet::new(), true, true));
        world.advance(26.0, false);
        let moved = world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == 2)
            .unwrap();
        assert!(moved.position.longitude > start);
    }

    #[test]
    fn the_runway_resource_is_released_when_the_takeoff_roll_completes() {
        let mut world = World::default();
        assert!(
            world.insert(
                0,
                1,
                std::sync::Arc::new(
                    Route::compile(
                        vec![
                            Segment {
                                stage: Stage::Takeoff,
                                points: vec![point(0.0, 0.0), point(800.0, 0.0)],
                                knots: 100.0,
                                resources: vec!["RUNWAY".into()],
                                dwell_seconds: 0.0,
                            },
                            Segment {
                                stage: Stage::Climb,
                                points: vec![point(800.0, 0.0), point(12000.0, 1000.0)],
                                knots: 150.0,
                                resources: vec![],
                                dwell_seconds: 0.0,
                            },
                            Segment {
                                stage: Stage::Climb,
                                points: vec![point(12000.0, 1000.0), point(24000.0, 2000.0)],
                                knots: 150.0,
                                resources: vec![],
                                dwell_seconds: 0.0,
                            },
                            crate::traffic::holding::Pattern {
                                fix: point(24000.0, 2000.0),
                                inbound_degrees: 270.0,
                                right: true,
                                ground_knots: 150.0,
                                leg_seconds: 60.0,
                                bank_degrees: 25.0,
                                turn_rate_degrees_sec: 3.0,
                            }
                            .compile(|_, _| true)
                            .unwrap(),
                        ],
                        1000.0,
                        1000.0,
                    )
                    .unwrap()
                )
            )
        );
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        world.advance(0.0, false);
        assert_eq!(world.owners.get("RUNWAY"), Some(&1));
        world.aircraft.get_mut(&1).unwrap().next_authorised = true;
        for step in 1..=40 {
            world.advance(f64::from(step) * 0.5, false);
        }
        assert_eq!(world.snapshot()[0].stage, Stage::Climb);
        assert!(!world.owners.contains_key("RUNWAY"));
    }

    #[test]
    fn crossing_requires_a_grant_and_continuation_requires_another() {
        let mut w = World::default();
        assert!(w.insert(0, 1, crossing()));
        assert!(w.insert(0, 2, crossing()));
        assert_eq!(w.advance(100.0, false), Advance::Initial);
        w.advance(110.0, false);
        assert_eq!(w.snapshot()[0].position, point(0.0, 0.0));
        assert!(!w.authorise(0, 1, 0, &BTreeSet::from(["TEST/09-27".into()])));
        assert!(w.authorise(0, 1, 0, &BTreeSet::new()));
        assert!(!w.authorise(0, 2, 0, &BTreeSet::new()));
        w.advance(135.0, false);
        let pose = &w.snapshot()[0];
        assert_eq!(pose.segment, 1);
        assert!(pose.awaiting_clearance);
        assert_eq!(pose.position, point(100.0, 0.0));
        assert!(w.authorise(0, 2, 0, &BTreeSet::new()));
        assert!(!w.authorise(0, 1, 0, &BTreeSet::new()));
        assert!(!w.retire(1));
    }
    #[test]
    fn pause_clock_jump_and_reset_never_apply_old_work() {
        let mut w = World::default();
        w.insert(0, 1, crossing());
        w.authorise(0, 1, 0, &BTreeSet::new());
        w.advance(100.0, false);
        w.advance(110.0, true);
        assert_eq!(w.snapshot()[0].position, point(0.0, 0.0));
        assert_eq!(w.advance(1000.0, false), Advance::ClockChanged);
        w.advance(1001.0, false);
        assert_eq!(w.snapshot()[0].position, point(0.0, 0.0));
        w.reset();
        assert!(!w.insert(0, 2, crossing()));
        assert!(!w.authorise(0, 1, 0, &BTreeSet::new()));
        assert!(w.snapshot().is_empty());
    }
    #[test]
    fn taxi_brakes_for_new_obstruction_and_does_not_reserve_a_crossing() {
        let route = Route::compile(
            vec![
                Segment {
                    stage: Stage::TaxiOut,
                    points: vec![point(0.0, 0.0), point(100.0, 0.0)],
                    knots: 10.0,
                    resources: vec!["A".into()],
                    dwell_seconds: 0.0,
                },
                Segment {
                    stage: Stage::TaxiOut,
                    points: vec![point(100.0, 0.0), point(200.0, 0.0)],
                    knots: 10.0,
                    resources: vec!["B".into()],
                    dwell_seconds: 0.0,
                },
                Segment {
                    stage: Stage::Crossing,
                    points: vec![point(200.0, 0.0), point(300.0, 0.0)],
                    knots: 10.0,
                    resources: vec!["RUNWAY".into()],
                    dwell_seconds: 0.0,
                },
            ],
            1000.0,
            1000.0,
        )
        .unwrap();
        let mut world = World::default();
        assert!(world.insert(0, 1, std::sync::Arc::new(route)));
        assert!(world.ground_rates(1, 2.0, 2.0));
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        assert!(world.reserve_next_taxi(0, 1, &BTreeSet::new()));
        world.advance(0.0, false);
        for second in 1..=5 {
            world.advance(f64::from(second), false);
        }
        let before = world.snapshot()[0].clone();
        assert_eq!(before.knots, 10.0);
        world.recheck_surface(&BTreeSet::from(["A".into()]));
        world.advance(6.0, false);
        let braking = world.snapshot()[0].clone();
        assert_eq!(braking.knots, 8.0);
        assert!(braking.position.longitude > before.position.longitude);
        for second in 7..=12 {
            world.advance(f64::from(second), false);
        }
        assert_eq!(world.snapshot()[0].knots, 0.0);
        let stopped = world.snapshot()[0].position;
        world.advance(13.0, false);
        assert_eq!(world.snapshot()[0].position, stopped);
        world.recheck_surface(&BTreeSet::new());
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        assert!(world.reserve_next_taxi(0, 1, &BTreeSet::new()));
        let mut continued_at_speed = false;
        for second in 14..=80 {
            world.advance(f64::from(second), false);
            let pose = world.snapshot()[0].clone();
            if pose.segment == 1 {
                continued_at_speed |= pose.knots > 5.0;
            }
            assert!(!world.reserve_next_taxi(0, 1, &BTreeSet::new()) || pose.segment == 0);
            if pose.segment == 2 {
                assert!(pose.awaiting_clearance);
                assert_eq!(pose.position, point(200.0, 0.0));
                break;
            }
        }
        assert!(continued_at_speed);
        assert_eq!(world.snapshot()[0].segment, 2);
    }
    #[test]
    fn occupied_final_holding_point_is_not_released_until_retirement() {
        let route = std::sync::Arc::new(
            Route::compile(
                vec![Segment {
                    stage: Stage::TaxiOut,
                    points: vec![point(0.0, 0.0), point(10.0, 0.0)],
                    knots: 10.0,
                    resources: vec!["HOLD".into()],
                    dwell_seconds: 0.0,
                }],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        let mut world = World::default();
        world.insert(0, 1, route.clone());
        world.insert(0, 2, route);
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        world.advance(0.0, false);
        world.advance(10.0, false);
        assert!(world.snapshot()[0].complete);
        assert!(!world.authorise(0, 2, 0, &BTreeSet::new()));
        assert!(world.retire(1));
        assert!(world.authorise(0, 2, 0, &BTreeSet::new()));
    }
    #[test]
    fn an_unavailable_landing_uses_its_cleared_hold_instead_of_stopping_airborne() {
        let holding = |anchor: Waypoint, course: f64| {
            let mut points = Vec::new();
            for index in 0..=36 {
                let angle = (f64::from(index) * 10.0).to_radians();
                let along = 500.0 * angle.sin();
                let right = 500.0 * (1.0 - angle.cos());
                let radians = course.to_radians();
                points.push(Waypoint {
                    latitude: anchor.latitude
                        + (along * radians.cos() - right * radians.sin()) / 111_320.0,
                    longitude: anchor.longitude
                        + (along * radians.sin() + right * radians.cos()) / 111_320.0,
                    altitude_feet: anchor.altitude_feet,
                });
            }
            std::sync::Arc::new(
                Route::compile(
                    vec![Segment {
                        stage: Stage::Holding,
                        points,
                        knots: 80.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    }],
                    1000.0,
                    1000.0,
                )
                .unwrap(),
            )
        };
        let route = std::sync::Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Cruise,
                        points: vec![point(0.0, 5000.0), point(1000.0, 5000.0)],
                        knots: 100.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Rollout,
                        points: vec![point(1000.0, 5000.0), point(1010.0, 5000.0)],
                        knots: 10.0,
                        resources: vec!["RW".into()],
                        dwell_seconds: 0.0,
                    },
                ],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        let mut world = World::default();
        world.insert(0, 1, route);
        assert!(
            !world.authorise(0, 1, 0, &BTreeSet::new()),
            "An airborne clearance needs a recovery"
        );
        assert!(world.recovery(0, 1, 0, holding(point(1000.0, 5000.0), 90.0)));
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        assert!(!world.authorise_continuation(0, 1, 1, &BTreeSet::from(["RW".into()])));
        world.advance(0.0, false);
        world.advance(20.0, false);
        let first = world.snapshot()[0].clone();
        assert_eq!(first.stage, Stage::Holding);
        world.advance(40.0, false);
        let second = world.snapshot()[0].clone();
        assert_ne!(first.position, second.position);
        assert_eq!(second.knots, 80.0);
        assert!(!second.complete && !second.awaiting_clearance);
        assert!(!world.leave_hold(1, 1, holding(second.position, second.heading)));
        assert!(!world.leave_hold(0, 1, holding(point(0.0, 5000.0), second.heading)));
        assert!(world.leave_hold(0, 1, holding(second.position, second.heading)));
        world.advance(60.0, false);
        assert_ne!(world.snapshot()[0].position, second.position);
        world.advance(1000.0, false);
        let clock_changed = world.snapshot()[0].position;
        world.advance(1001.0, false);
        assert_ne!(world.snapshot()[0].position, clock_changed);
    }
    #[test]
    fn airborne_seeding_starts_established_without_surface_resources() {
        let airborne = std::sync::Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Cruise,
                        points: vec![point(0.0, 5000.0), point(10_000.0, 5000.0)],
                        knots: 200.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Descent,
                        points: vec![point(10_000.0, 5000.0), point(20_000.0, 4000.0)],
                        knots: 180.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Rollout,
                        points: vec![point(20_000.0, 4000.0), point(21_000.0, 4000.0)],
                        knots: 100.0,
                        resources: vec!["RW".into()],
                        dwell_seconds: 0.0,
                    },
                ],
                1000.0,
                3000.0,
            )
            .unwrap(),
        );
        // A ground segment is not an airborne seed.
        let mut world = World::default();
        assert!(!world.seed_airborne(0, 1, crossing(), 0));
        assert!(world.snapshot().is_empty());
        // An airborne segment starts moving with authority for that segment only.
        let seeded_route = airborne.clone();
        assert!(world.insert(0, 2, crossing()));
        assert!(world.ground_rates(2, 1.0, 2.0));
        assert!(world.seed_airborne(0, 2, airborne, 0));
        assert!(
            world.ground_rates(2, 1.0, 2.0),
            "Keep the prepared aircraft's movement envelope"
        );
        assert!(
            !world.ground_rates(2, 2.0, 4.0),
            "An airborne seed cannot change its envelope"
        );
        let seeded = world.snapshot();
        assert_eq!(seeded.len(), 1);
        assert_eq!(seeded[0].id, 2);
        assert_eq!(seeded[0].stage, Stage::Cruise);
        assert!(!seeded[0].complete && !seeded[0].awaiting_clearance);
        assert!(world.owned_resources(2).is_empty());
        world.advance(0.0, false);
        world.advance(5.0, false);
        let moved = world.snapshot()[0].clone();
        assert_ne!(moved.position, seeded[0].position);
        assert_eq!(moved.segment, 0);
        // The next segment still needs its own grant; bounded clock steps keep movement valid.
        for step in 1..=9 {
            world.advance(5.0 + f64::from(step) * 25.0, false);
        }
        let limit = world.snapshot()[0].clone();
        assert_eq!(limit.segment, 1);
        assert!(limit.awaiting_clearance);
        // A started aircraft is never reseeded or teleported.
        assert!(!world.seed_airborne(0, 2, seeded_route, 0));
    }
    #[test]
    fn ground_seeding_starts_taxiing_owns_its_cells_and_refuses_occupancy() {
        let route = || {
            std::sync::Arc::new(
                Route::compile(
                    vec![
                        Segment {
                            stage: Stage::Parked,
                            points: vec![point(0.0, 0.0)],
                            knots: 0.0,
                            resources: vec!["STAND".into()],
                            dwell_seconds: 30.0,
                        },
                        Segment {
                            stage: Stage::TaxiOut,
                            points: vec![point(0.0, 0.0), point(100.0, 0.0)],
                            knots: 10.0,
                            resources: vec!["A".into()],
                            dwell_seconds: 0.0,
                        },
                        Segment {
                            stage: Stage::TaxiOut,
                            points: vec![point(100.0, 0.0), point(200.0, 0.0)],
                            knots: 10.0,
                            resources: vec!["B".into()],
                            dwell_seconds: 0.0,
                        },
                    ],
                    1000.0,
                    1000.0,
                )
                .unwrap(),
            )
        };
        let mut world = World::default();
        let first = route();
        assert!(world.insert(0, 1, first.clone()));
        assert_eq!(world.owned_resources(1), BTreeSet::from(["STAND".into()]));
        assert!(world.seed_ground(0, 1, first.clone(), 1));
        assert_eq!(world.owned_resources(1), BTreeSet::from(["A".into()]));
        let pose = &world.snapshot()[0];
        assert_eq!(pose.stage, Stage::TaxiOut);
        assert!(!pose.awaiting_clearance && pose.knots == 10.0);
        // A started aircraft keeps its own position and cannot be seeded again.
        assert!(!world.seed_ground(0, 1, first, 2));
        // The seeded cells are occupied, so another aircraft cannot be injected into them.
        let second = route();
        assert!(world.insert(0, 2, second.clone()));
        assert!(!world.seed_ground(0, 2, second, 1));
        // A stand dwell can be delayed without moving the aircraft.
        assert_eq!(world.aircraft[&2].duration(), 30.0);
        assert!(world.stagger_initial_dwell(0, 2, 45.0));
        assert_eq!(world.aircraft[&2].duration(), 75.0);
        assert!(!world.stagger_initial_dwell(0, 1, 10.0));
        assert!(!world.stagger_initial_dwell(0, 2, -1.0));
    }
    #[test]
    fn progress_seeding_places_traffic_mid_taxi_and_completed_at_the_hold() {
        let route = || {
            std::sync::Arc::new(
                Route::compile(
                    vec![
                        Segment {
                            stage: Stage::Parked,
                            points: vec![point(0.0, 0.0)],
                            knots: 0.0,
                            resources: vec!["STAND".into()],
                            dwell_seconds: 30.0,
                        },
                        Segment {
                            stage: Stage::TaxiOut,
                            points: vec![point(0.0, 0.0), point(100.0, 0.0)],
                            knots: 10.0,
                            resources: vec!["A".into()],
                            dwell_seconds: 0.0,
                        },
                        Segment {
                            stage: Stage::TaxiOut,
                            points: vec![point(100.0, 0.0), point(200.0, 0.0)],
                            knots: 10.0,
                            resources: vec!["B".into()],
                            dwell_seconds: 0.0,
                        },
                    ],
                    1000.0,
                    1000.0,
                )
                .unwrap(),
            )
        };
        let mut rolling = World::default();
        let inbound = route();
        assert!(rolling.insert(0, 1, inbound.clone()));
        assert!(rolling.seed_ground_progress(0, 1, inbound, 1, 0.5));
        let pose = &rolling.snapshot()[0];
        assert_eq!(pose.stage, Stage::TaxiOut);
        assert!(!pose.complete && !pose.awaiting_clearance);
        assert_eq!(pose.knots, 10.0);
        assert!(pose.position.longitude > 0.0 && pose.position.longitude < 100.0 / 111_320.0);

        let mut holding = World::default();
        let outbound = route();
        assert!(holding.insert(0, 2, outbound.clone()));
        assert!(holding.seed_ground_progress(0, 2, outbound, 2, 1.0));
        let pose = &holding.snapshot()[0];
        assert_eq!(pose.stage, Stage::TaxiOut);
        assert!(pose.complete);
        assert_eq!(pose.knots, 0.0);
        assert_eq!(holding.owned_resources(2), BTreeSet::from(["B".into()]));
        assert!(!holding.seed_ground_progress(0, 2, route(), 2, 0.5));
        assert!(!holding.seed_ground_progress(0, 3, route(), 2, 1.1));
    }

    #[test]
    fn impossible_climb_and_route_gaps_are_rejected() {
        let route = Segment {
            stage: Stage::Climb,
            points: vec![point(0.0, 0.0), point(100.0, 10000.0)],
            knots: 150.0,
            resources: vec![],
            dwell_seconds: 0.0,
        };
        assert!(
            Route::compile(vec![route], 2000.0, 2000.0)
                .unwrap_err()
                .contains("performance")
        );
        let mut segments = crossing().segments.clone();
        segments[1].points[0] = point(500.0, 0.0);
        assert!(
            Route::compile(segments, 2000.0, 2000.0)
                .unwrap_err()
                .contains("discontinuity")
        );
    }
    fn advisory() -> std::sync::Arc<Route> {
        std::sync::Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Descent,
                        points: vec![point(0.0, 1000.0), point(2000.0, 1000.0)],
                        knots: 90.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Approach,
                        points: vec![point(2000.0, 1000.0), point(7000.0, 0.0)],
                        knots: 85.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Rollout,
                        points: vec![point(7000.0, 0.0), point(7900.0, 0.0)],
                        knots: 70.0,
                        resources: vec!["RUNWAY/TEST/09/27".into()],
                        dwell_seconds: 0.0,
                    },
                ],
                1000.0,
                1000.0,
            )
            .unwrap(),
        )
    }
    #[test]
    fn an_advisory_procedure_moves_only_along_its_own_route() {
        let mut world = World::default();
        assert!(world.insert_advisory(0, 1, advisory(), 0));
        assert!(world.is_advisory(1));
        // A controlled aircraft can never be adopted as advisory traffic.
        assert!(world.insert(0, 2, crossing()));
        assert!(!world.insert_advisory(0, 2, advisory(), 0));
        // The procedure is the authority, so the first segment moves without a controller.
        assert!(world.authorise_advisory(0, 1, 0, &BTreeSet::new()));
        world.advance(0.0, false);
        world.advance(30.0, false);
        let pose = world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == 1)
            .unwrap();
        assert_eq!(pose.stage, Stage::Descent);
        assert!(pose.knots > 0.0);
        // Controlled traffic is never granted by an advisory procedure.
        assert!(!world.authorise_advisory(0, 2, 0, &BTreeSet::new()));
    }
    #[test]
    fn an_advisory_procedure_stops_at_a_boundary_until_the_next_segment_is_granted() {
        let mut world = World::default();
        assert!(world.insert_advisory(0, 1, advisory(), 0));
        assert!(world.authorise_advisory(0, 1, 0, &BTreeSet::new()));
        world.advance(0.0, false);
        world.advance(20.0, false);
        world.advance(40.0, false);
        world.advance(60.0, false);
        let pose = world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == 1)
            .unwrap();
        assert_eq!(pose.segment, 1);
        assert!(pose.awaiting_clearance);
        assert_eq!(pose.knots, 0.0);
        // The next segment is granted by the procedure, not by a controller clearance.
        assert!(world.authorise_advisory(0, 1, 1, &BTreeSet::new()));
        world.advance(61.0, false);
        world.advance(90.0, false);
        let moved = world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == 1)
            .unwrap();
        assert_eq!(moved.segment, 1);
        assert!(moved.knots > 0.0);
    }
    #[test]
    fn an_advisory_circuit_continues_from_the_point_the_last_one_ended() {
        let mut world = World::default();
        let route = advisory();
        assert!(world.insert_advisory(0, 1, route.clone(), 0));
        let mut clock = 0.0;
        // Fly the whole circuit, granting each leg as the procedure reaches it.
        for _ in 0..600 {
            let pose = world
                .snapshot()
                .into_iter()
                .find(|pose| pose.id == 1)
                .unwrap();
            if pose.complete {
                break;
            }
            if pose.awaiting_clearance {
                assert!(world.authorise_advisory(0, 1, pose.segment, &BTreeSet::new()));
            }
            clock += 1.0;
            world.advance(clock, false);
        }
        let pose = world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == 1)
            .unwrap();
        assert!(pose.complete);
        // The continuation starts where the landing roll ended and climbs away from it.
        let next = std::sync::Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::Climb,
                        points: vec![point(7900.0, 0.0), point(12900.0, 1000.0)],
                        knots: 70.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Descent,
                        points: vec![point(12900.0, 1000.0), point(20000.0, 1000.0)],
                        knots: 90.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Approach,
                        points: vec![point(20000.0, 1000.0), point(25000.0, 0.0)],
                        knots: 85.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    Segment {
                        stage: Stage::Rollout,
                        points: vec![point(25000.0, 0.0), point(25900.0, 0.0)],
                        knots: 70.0,
                        resources: vec!["RUNWAY/TEST/09/27".into()],
                        dwell_seconds: 0.0,
                    },
                ],
                1000.0,
                1000.0,
            )
            .unwrap(),
        );
        assert!(world.continue_advisory(0, 1, next.clone()));
        let resumed = world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == 1)
            .unwrap();
        assert!(!resumed.complete);
        assert_eq!(resumed.segment, 0);
        assert!(resumed.awaiting_clearance);
        assert!((resumed.position.longitude - next.start_position().longitude).abs() < 1e-9);
        assert!(world.authorise_advisory(0, 1, 0, &BTreeSet::new()));
        clock += 1.0;
        world.advance(clock, false);
        clock += 20.0;
        world.advance(clock, false);
        let climbing = world
            .snapshot()
            .into_iter()
            .find(|pose| pose.id == 1)
            .unwrap();
        assert_eq!(climbing.stage, Stage::Climb);
        assert!(climbing.position.altitude_feet > 0.0);
    }
}
