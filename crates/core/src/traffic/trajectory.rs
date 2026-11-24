//! Bounded airborne geometry and crossing levels prepared on planning workers.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use super::{
    curves,
    motion::Waypoint,
    vertical::{self, Crossing},
};
use crate::{
    arrival::{bearing, distance_nm},
    state::Point,
};

/// Retain the IAS requirement when vertical scheduling changes a crossing level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Airspeed {
    /// Indicated speed chosen within the published limits.
    pub indicated_knots: f64,
    /// Sampled airport pressure/temperature reference.
    pub atmosphere: super::air::Atmosphere,
    /// True direction from which the planning wind blows.
    pub wind_from: f64,
    /// Planning wind speed in knots.
    pub wind_knots: f64,
}
impl Airspeed {
    /// Resolve movement speed after the crossing height and track are known.
    pub fn ground_knots(&self, geometric_feet: f64, track: f64) -> Result<f64, String> {
        super::air::wind(
            self.atmosphere
                .true_knots(self.indicated_knots, geometric_feet)?,
            track,
            self.wind_from,
            self.wind_knots,
        )
        .map(|(ground, _)| ground)
    }
}

/// Published course/heading-to-intercept geometry before the actual turn is resolved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CourseIntercept {
    /// A fix on the following published course.
    pub target: [f64; 2],
    /// Following course, degrees true toward the target fix.
    pub target_course: f64,
    /// Published course to fly after the initial turn, degrees true.
    pub course: f64,
    /// A VI leg retains its prescribed true heading as the joint height/wind solve changes track.
    pub heading: Option<f64>,
    /// Maximum total distance of this intercept leg, nautical miles.
    pub maximum_nm: f64,
}

/// A resolved crossing after decoding its published leg and pressure reference.
#[derive(Clone)]
pub struct Fix {
    /// Latitude and longitude in degrees.
    pub coordinate: [f64; 2],
    /// Required true arrival course, including fly-over and course-to-fix legs.
    pub arrival_heading: Option<f64>,
    /// Crossing floor converted to geometric feet MSL.
    pub minimum_feet: f64,
    /// Crossing ceiling converted to geometric feet MSL.
    pub maximum_feet: f64,
    /// Desired geometric level before restrictions are applied.
    pub preferred_feet: f64,
    /// Ground speed after applying the aircraft envelope, wind and published IAS limit.
    pub ground_knots: f64,
    /// Original IAS and weather; None explicitly represents a fixed ground-speed path.
    pub airspeed: Option<Airspeed>,
    /// Published turn direction, when specified.
    pub required_turn: Option<curves::Turn>,
    /// Course-to-intercept geometry resolved again at the solved speed and turn radius.
    pub intercept: Option<CourseIntercept>,
}

/// Performance and sampling limits used during planning.
pub struct Envelope {
    /// Maximum bank for planned turns.
    pub bank_degrees: f64,
    /// Maximum angular turn rate in degrees per second.
    pub turn_rate_degrees_sec: f64,
    /// Maximum climb rate in feet per minute.
    pub climb_fpm: f64,
    /// Maximum descent rate in feet per minute.
    pub descent_fpm: f64,
    /// Maximum spacing between protection samples.
    pub spacing_meters: f64,
    /// Permitted curve length beyond the direct connection.
    pub maximum_detour_meters: f64,
}

/// A sampled connection ending at its crossing restriction.
#[derive(Clone)]
pub struct Leg {
    /// Geographic path with geometric heights.
    pub points: Vec<Waypoint>,
    /// Planned speed over the ground.
    pub ground_knots: f64,
    /// Duration at the planned speed.
    pub seconds: f64,
}

/// Solved geometry and levels awaiting an independent terrain/airspace check.
/// Its movement samples are unavailable until every connection passes that check.
#[derive(Clone)]
pub struct Draft {
    legs: Vec<Leg>,
    crossings: Vec<Fix>,
}
impl Draft {
    /// Retain solved virtual intercept positions without releasing movement samples.
    #[must_use]
    pub fn crossings(&self) -> &[Fix] {
        &self.crossings
    }
    /// Whether a late entry lies on the first leg ahead of its start; no samples or authority are released.
    #[must_use]
    pub fn first_leg_join(&self, coordinate: [f64; 2], tolerance_nm: f64) -> bool {
        self.legs
            .first()
            .is_some_and(|leg| forward_join_index(&leg.points, coordinate, tolerance_nm).is_some())
    }
    /// Planning diagnostics; this does not make movement samples available.
    #[must_use]
    pub fn leg_count(&self) -> usize {
        self.legs.len()
    }

    /// Additional per-leg floors for a replanning pass; this does not release movement samples.
    pub fn minimum_levels(
        &self,
        minimum: impl Fn(usize, Waypoint, Waypoint) -> Result<Option<f64>, String>,
    ) -> Result<Vec<Option<f64>>, String> {
        self.legs
            .iter()
            .enumerate()
            .map(|(index, leg)| {
                let mut floor: Option<f64> = None;
                for pair in leg.points.windows(2) {
                    if let Some(value) = minimum(index, pair[0], pair[1])? {
                        if !value.is_finite() {
                            return Err("Invalid corridor minimum".into());
                        }
                        floor = Some(floor.map_or(value, |previous| previous.max(value)));
                    }
                }
                Ok(floor)
            })
            .collect()
    }

    /// Endpoint heights used to construct clearances before releasing movement geometry.
    #[must_use]
    pub fn level_span(&self, index: usize) -> Option<(f64, f64)> {
        let leg = self.legs.get(index)?;
        Some((
            leg.points.first()?.altitude_feet,
            leg.points.last()?.altitude_feet,
        ))
    }

    /// Add clearance limits on the already solved path without releasing its movement samples.
    /// The caller selects assignable levels; original endpoint restrictions remain intact.
    /// # Panics
    ///
    /// Panics if the trajectory has no samples.
    pub fn split_limits(
        self,
        maximum_seconds: f64,
        eligible: impl Fn(usize) -> bool,
        level: impl Fn(usize, f64, f64, f64) -> Option<f64>,
    ) -> Result<(Self, Vec<usize>), String> {
        if !(60.0..=900.0).contains(&maximum_seconds) {
            return Err("Invalid airborne clearance duration budget".into());
        }
        let mut legs = Vec::new();
        let mut crossings = Vec::new();
        let mut source = Vec::new();
        let mut samples = 0;
        for (index, (leg, crossing)) in self.legs.into_iter().zip(self.crossings).enumerate() {
            let mut times = vec![0.0];
            for pair in leg.points.windows(2) {
                let seconds = distance_nm(
                    [pair[0].latitude, pair[0].longitude],
                    [pair[1].latitude, pair[1].longitude],
                ) / leg.ground_knots
                    * 3600.0;
                times.push(times.last().unwrap() + seconds);
            }
            let total = *times.last().ok_or("Empty airborne movement")?;
            if !eligible(index) || total <= maximum_seconds {
                samples += leg.points.len();
                legs.push(leg);
                crossings.push(crossing);
                source.push(index);
                continue;
            }
            let at = |time: f64| {
                let part = times
                    .partition_point(|value| *value <= time)
                    .saturating_sub(1)
                    .min(leg.points.len() - 2);
                let pair = &leg.points[part..=part + 1];
                let fraction = ((time - times[part]) / (times[part + 1] - times[part]).max(1e-12))
                    .clamp(0.0, 1.0);
                let coordinate = crate::arrival::destination(
                    [pair[0].latitude, pair[0].longitude],
                    bearing(
                        [pair[0].latitude, pair[0].longitude],
                        [pair[1].latitude, pair[1].longitude],
                    ),
                    distance_nm(
                        [pair[0].latitude, pair[0].longitude],
                        [pair[1].latitude, pair[1].longitude],
                    ) * fraction,
                );
                Waypoint {
                    latitude: coordinate[0],
                    longitude: coordinate[1],
                    altitude_feet: pair[0].altitude_feet
                        + (pair[1].altitude_feet - pair[0].altitude_feet) * fraction,
                }
            };
            let mut boundaries = vec![(0.0, leg.points[0])];
            while total - boundaries.last().unwrap().0 > maximum_seconds {
                let start = boundaries.last().unwrap().0;
                let earliest = start + maximum_seconds * 0.45;
                let latest = start + maximum_seconds * 0.9;
                let early = at(earliest).altitude_feet;
                let late = at(latest).altitude_feet;
                let assigned = level(index, early.min(late), early.max(late), late)
                    .filter(|value| {
                        value.is_finite()
                            && *value >= early.min(late) - 1e-6
                            && *value <= early.max(late) + 1e-6
                    })
                    .ok_or_else(|| {
                        format!(
                            "No assignable intermediate level within the clearance duration budget (leg {index}, {:.0}..{:.0} ft, preferred {:.0} ft)",
                            early.min(late),
                            early.max(late),
                            late
                        )
                    })?;
                let (time, point) = if (early - late).abs() < 1e-7 {
                    (latest, at(latest))
                } else {
                    let part = leg
                        .points
                        .windows(2)
                        .enumerate()
                        .find(|(part, pair)| {
                            (pair[1].altitude_feet - pair[0].altitude_feet).abs() > 1e-12
                                && assigned
                                    >= pair[0].altitude_feet.min(pair[1].altitude_feet) - 1e-8
                                && assigned
                                    <= pair[0].altitude_feet.max(pair[1].altitude_feet) + 1e-8
                                && times[*part + 1] >= earliest - 1e-6
                        })
                        .ok_or("Intermediate clearance level is not on its solved path")?;
                    let fraction = ((assigned - part.1[0].altitude_feet)
                        / (part.1[1].altitude_feet - part.1[0].altitude_feet))
                        .clamp(0.0, 1.0);
                    let time = times[part.0] + (times[part.0 + 1] - times[part.0]) * fraction;
                    let mut point = at(time);
                    point.altitude_feet = assigned;
                    (time, point)
                };
                if time <= start + 1e-6 || time - start > maximum_seconds || time >= total {
                    return Err("Intermediate clearance cannot fit its duration budget".into());
                }
                boundaries.push((time, point));
                if boundaries.len() + legs.len() > 128 {
                    return Err("Intermediate clearances exceed the crossing budget".into());
                }
            }
            boundaries.push((total, *leg.points.last().unwrap()));
            for (part, pair) in boundaries.windows(2).enumerate() {
                let mut points = vec![pair[0].1];
                points.extend(leg.points.iter().zip(&times).filter_map(|(point, time)| {
                    (*time > pair[0].0 + 1e-9 && *time < pair[1].0 - 1e-9).then_some(*point)
                }));
                points.push(pair[1].1);
                let requirement = if part + 2 == boundaries.len() {
                    crossing.clone()
                } else {
                    let before = points[points.len() - 2];
                    Fix {
                        coordinate: [pair[1].1.latitude, pair[1].1.longitude],
                        arrival_heading: Some(bearing(
                            [before.latitude, before.longitude],
                            [pair[1].1.latitude, pair[1].1.longitude],
                        )),
                        minimum_feet: pair[1].1.altitude_feet,
                        maximum_feet: pair[1].1.altitude_feet,
                        preferred_feet: pair[1].1.altitude_feet,
                        ground_knots: leg.ground_knots,
                        required_turn: None,
                        intercept: None,
                        airspeed: crossing.airspeed,
                    }
                };
                samples += points.len();
                legs.push(Leg {
                    points,
                    ground_knots: leg.ground_knots,
                    seconds: pair[1].0 - pair[0].0,
                });
                crossings.push(requirement);
                source.push(index);
            }
        }
        if legs.len() > 128 || samples > 4096 {
            return Err("Intermediate clearances exceed the airborne geometry budget".into());
        }
        Ok((Self { legs, crossings }, source))
    }

    /// Release movement samples only when the complete solved path is protected.
    pub fn protect(
        self,
        protected: impl Fn(Waypoint, Waypoint) -> bool,
    ) -> Result<Vec<Leg>, String> {
        if let Some(pair) = self
            .legs
            .iter()
            .flat_map(|leg| leg.points.windows(2))
            .find(|pair| !protected(pair[0], pair[1]))
        {
            return Err(format!(
                "Generated airborne path lacks verified corridor protection near {:.5}/{:.5} to {:.5}/{:.5}",
                pair[0].latitude, pair[0].longitude, pair[1].latitude, pair[1].longitude
            ));
        }
        Ok(self.legs)
    }
}

/// Build heading-continuous fly-over connectors; every sample needs corridor protection.
/// This does not grant flight or turn an unresolved procedure into a direct clearance.
pub fn compile(
    start: Waypoint,
    heading_degrees: f64,
    fixes: &[Fix],
    final_heading_degrees: f64,
    envelope: &Envelope,
    protected: impl Fn(Waypoint, Waypoint) -> bool,
) -> Result<Vec<Leg>, String> {
    draft(
        start,
        heading_degrees,
        fixes,
        final_heading_degrees,
        envelope,
    )?
    .protect(protected)
}

/// Solve bounded geometry, levels and nominal airspeed without granting movement.
pub fn draft(
    start: Waypoint,
    heading_degrees: f64,
    fixes: &[Fix],
    final_heading_degrees: f64,
    envelope: &Envelope,
) -> Result<Draft, String> {
    draft_selected(
        start,
        heading_degrees,
        fixes,
        final_heading_degrees,
        envelope,
        |_, lower, upper, preferred| Some(preferred.clamp(lower, upper)),
    )
}

/// Joint planning with a caller-supplied clearance-level selector.
pub fn draft_selected(
    start: Waypoint,
    heading_degrees: f64,
    fixes: &[Fix],
    final_heading_degrees: f64,
    envelope: &Envelope,
    select: impl Fn(usize, f64, f64, f64) -> Option<f64>,
) -> Result<Draft, String> {
    if fixes.is_empty() || fixes.len() > 128 {
        return Err("Generated airborne crossing budget is invalid".into());
    }
    let mut scheduled = fixes.to_vec();
    let mut previous = [start.latitude, start.longitude];
    // Preferred cruise heights are not a valid initial TAS estimate for the
    // first SID turn. Seed from the actual starting height, then solve the
    // complete level/speed/geometry profile to convergence below.
    for fix in &mut scheduled {
        if let Some(airspeed) = fix.airspeed {
            let track = fix
                .arrival_heading
                .unwrap_or_else(|| bearing(previous, fix.coordinate));
            let seed_height = start
                .altitude_feet
                .min(fix.preferred_feet)
                .max(fix.minimum_feet)
                .min(fix.maximum_feet);
            fix.ground_knots = airspeed.ground_knots(seed_height, track)?;
        }
        previous = fix.coordinate;
    }
    // A changed level changes TAS, turn radius and time available to climb or
    // descend. Rebuild all three together; never publish an unconverged path.
    let mut residual = (0.0f64, 0usize);
    for _ in 0..12 {
        let legs = compile_selected(
            start,
            heading_degrees,
            &scheduled,
            final_heading_degrees,
            envelope,
            &select,
        )?;
        let mut settled = true;
        residual = (0.0, 0);
        for (index, (fix, leg)) in scheduled.iter_mut().zip(&legs).enumerate() {
            let end = leg
                .points
                .last()
                .ok_or("Airborne crossing has no endpoint")?;
            if fix.intercept.is_some() {
                fix.coordinate = [end.latitude, end.longitude];
            }
            let Some(airspeed) = fix.airspeed else {
                continue;
            };
            let before = leg
                .points
                .iter()
                .rev()
                .nth(1)
                .ok_or("Airborne crossing has no incoming track")?;
            let track = bearing(
                [before.latitude, before.longitude],
                [end.latitude, end.longitude],
            );
            let ground = airspeed.ground_knots(end.altitude_feet, track)?;
            let error = (ground - fix.ground_knots).abs();
            if error > residual.0 {
                residual = (error, index);
            }
            settled &= error < 0.01;
            if let Some(heading) = fix.intercept.and_then(|intercept| intercept.heading) {
                let expected = super::air::heading_wind(
                    airspeed
                        .atmosphere
                        .true_knots(airspeed.indicated_knots, end.altitude_feet)?,
                    heading,
                    airspeed.wind_from,
                    airspeed.wind_knots,
                )?
                .1;
                settled &= crate::navigation::turn(track, expected).abs() < 0.01;
                fix.arrival_heading = Some(expected);
            }
            fix.ground_knots = ground;
            // Retain the last feasible level instead of alternating between adjacent flight levels.
            fix.preferred_feet = end.altitude_feet;
        }
        if settled {
            return Ok(Draft {
                legs,
                crossings: scheduled,
            });
        }
    }
    Err(format!(
        "Joint altitude and airspeed planning did not converge at crossing {} (remaining ground-speed error {:.3} knots)",
        residual.1 + 1,
        residual.0
    ))
}

/// Locate a point on this leg without returning to its start or skipping to a later leg.
#[must_use]
pub fn forward_join_index(
    points: &[Waypoint],
    coordinate: [f64; 2],
    tolerance_nm: f64,
) -> Option<usize> {
    if points.len() < 2
        || points.len() > 4096
        || points.iter().any(|point| {
            !point.latitude.is_finite()
                || !point.longitude.is_finite()
                || point.latitude.abs() > 90.0
                || point.longitude.abs() > 180.0
        })
        || !coordinate.iter().all(|value| value.is_finite())
        || coordinate[0].abs() > 90.0
        || coordinate[1].abs() > 180.0
        || !(0.0..=0.01).contains(&tolerance_nm)
    {
        return None;
    }
    let cosine = coordinate[0].to_radians().cos();
    if cosine.abs() < 0.01 {
        return None;
    }
    let project = |point: Waypoint| {
        [
            crate::navigation::turn(coordinate[1], point.longitude) * cosine * 60.0,
            (point.latitude - coordinate[0]) * 60.0,
        ]
    };
    points.windows(2).enumerate().find_map(|(index, pair)| {
        let a = project(pair[0]);
        let b = project(pair[1]);
        let d = [b[0] - a[0], b[1] - a[1]];
        let length = d[0] * d[0] + d[1] * d[1];
        if length <= 0.0 {
            return None;
        }
        let fraction = -(a[0] * d[0] + a[1] * d[1]) / length;
        if !(0.0..=1.0).contains(&fraction) {
            return None;
        }
        let error = (a[0] + fraction * d[0]).hypot(a[1] + fraction * d[1]);
        (error <= tolerance_nm).then_some(index + 1)
    })
}

#[cfg(test)]
fn compile_once(
    start: Waypoint,
    heading_degrees: f64,
    fixes: &[Fix],
    final_heading_degrees: f64,
    envelope: &Envelope,
) -> Result<Vec<Leg>, String> {
    compile_selected(
        start,
        heading_degrees,
        fixes,
        final_heading_degrees,
        envelope,
        &|_, lower, upper, preferred| Some(preferred.clamp(lower, upper)),
    )
}

fn compile_selected(
    start: Waypoint,
    heading_degrees: f64,
    fixes: &[Fix],
    final_heading_degrees: f64,
    envelope: &Envelope,
    select: &impl Fn(usize, f64, f64, f64) -> Option<f64>,
) -> Result<Vec<Leg>, String> {
    if fixes.is_empty()
        || fixes.len() > 128
        || ![heading_degrees, final_heading_degrees]
            .iter()
            .all(|v| v.is_finite())
        || !(5.0..=30.0).contains(&envelope.bank_degrees)
        || !(0.5..=3.0).contains(&envelope.turn_rate_degrees_sec)
        || !(10.0..=250.0).contains(&envelope.spacing_meters)
        || !(0.0..=100_000.0).contains(&envelope.maximum_detour_meters)
        || ![start.latitude, start.longitude, start.altitude_feet]
            .iter()
            .all(|v| v.is_finite())
        || start.latitude.abs() >= 85.0
        || start.longitude.abs() > 180.0
    {
        return Err("Invalid generated airborne envelope".into());
    }
    let origin = [start.latitude, start.longitude];
    let projection = |coordinate: [f64; 2]| Point {
        east: crate::navigation::turn(origin[1], coordinate[1])
            * 111_320.0
            * origin[0].to_radians().cos(),
        north: (coordinate[0] - origin[0]) * 111_320.0,
        height: 0.0,
    };
    let geographic = |point: Point, altitude_feet: f64| Waypoint {
        latitude: origin[0] + point.north / 111_320.0,
        longitude: (origin[1] + point.east / (111_320.0 * origin[0].to_radians().cos()) + 180.0)
            .rem_euclid(360.0)
            - 180.0,
        altitude_feet,
    };
    let mut from = curves::Pose {
        point: projection(origin),
        heading: heading_degrees,
    };
    let mut geometry = Vec::with_capacity(fixes.len());
    let mut crossings = Vec::with_capacity(fixes.len());
    let mut count = 0;
    for (index, fix) in fixes.iter().enumerate() {
        if !fix.coordinate.iter().all(|value| value.is_finite())
            || fix.coordinate[0].abs() >= 85.0
            || fix.coordinate[1].abs() > 180.0
            || !(40.0..=550.0).contains(&fix.ground_knots)
        {
            return Err(format!("Invalid airborne crossing at {index}"));
        }
        let speed = fix.ground_knots * (1852.0 / 3600.0);
        let radius = (speed.powi(2) / (9.80665 * envelope.bank_degrees.to_radians().tan()))
            .max(speed / envelope.turn_rate_degrees_sec.to_radians());
        let heading = fix.arrival_heading.unwrap_or_else(|| {
            // A fix without a published arrival course is a fly-by waypoint. Cross it on
            // the inbound course and let the next connection make the turn, instead of
            // forcing the outbound course before the fix.
            let from_point = geographic(from.point, 0.0);
            let inbound = bearing([from_point.latitude, from_point.longitude], fix.coordinate);
            if inbound.is_finite() {
                inbound
            } else {
                from.heading
            }
        });
        if !heading.is_finite() {
            return Err(format!("Invalid arrival course at {index}"));
        }
        let to = curves::Pose {
            point: projection(fix.coordinate),
            heading,
        };
        let length = (to.point.east - from.point.east).hypot(to.point.north - from.point.north);
        let directed_turn = match fix.required_turn {
            Some(curves::Turn::Right) => (heading - from.heading).rem_euclid(360.0),
            Some(curves::Turn::Left) => (from.heading - heading).rem_euclid(360.0),
            None => 0.0,
        };
        // A coded reversal can need more room than the ordinary straight-leg detour allowance.
        let detour = if directed_turn > 180.0 {
            envelope
                .maximum_detour_meters
                .max(std::f64::consts::TAU * radius)
        } else {
            envelope.maximum_detour_meters
        };
        let samples = if let Some(mut intercept) = fix.intercept {
            if let Some(heading) = intercept.heading {
                let airspeed = fix.airspeed.ok_or("Heading intercept needs a retained airspeed and wind reference")?;
                let track = super::air::heading_wind(
                    airspeed.atmosphere.true_knots(airspeed.indicated_knots, fix.preferred_feet)?,
                    heading, airspeed.wind_from, airspeed.wind_knots,
                )?.1.to_radians();
                // True track is measured at the solved intercept, not on the projection's origin parallel.
                intercept.course = (track.sin() * origin[0].to_radians().cos()
                    / fix.coordinate[0].to_radians().cos())
                    .atan2(track.cos()).to_degrees().rem_euclid(360.0);
            }
            intercept_curve(from, intercept, fix.required_turn, radius, envelope.spacing_meters, &projection)
        } else { curves::connect(
            from,
            to,
            &curves::Limits {
                radius,
                spacing: envelope.spacing_meters,
                    length: length + detour,
                initial_turn: fix.required_turn,
            },
            |_, _| true,
        ) }
        .map_err(|error| {
            let from_point = geographic(from.point, 0.0);
            let bearing = bearing(
                [from_point.latitude, from_point.longitude],
                fix.coordinate,
            );
            format!("Airborne crossing {index} from {:.5}/{:.5} to {:.5}/{:.5} (bearing {bearing:.1}), radius {radius:.0} m, length {length:.0} m, headings {:.1}/{heading:.1}, ground speed {:.1} kt, preferred {:.0} ft within {:.0}..{:.0} ft, turn {:?}: {error}", from_point.latitude, from_point.longitude, fix.coordinate[0], fix.coordinate[1], from.heading, fix.ground_knots, fix.preferred_feet, fix.minimum_feet, fix.maximum_feet, fix.required_turn)
        })?;
        let endpoint = samples.last().ok_or("Airborne connection is empty")?;
        let to = curves::Pose {
            point: endpoint.point,
            heading: endpoint.heading,
        };
        count += samples.len();
        if count > 4096 {
            return Err("Generated airborne route exceeds its sample budget".into());
        }
        let meters: f64 = samples
            .windows(2)
            .map(|pair| {
                let a = geographic(pair[0].point, 0.0);
                let b = geographic(pair[1].point, 0.0);
                distance_nm([a.latitude, a.longitude], [b.latitude, b.longitude]) * 1852.0
            })
            .sum();
        let seconds = meters / speed;
        crossings.push(Crossing {
            minimum_feet: fix.minimum_feet,
            maximum_feet: fix.maximum_feet,
            preferred_feet: fix.preferred_feet,
            seconds,
        });
        geometry.push((samples, meters));
        from = to;
    }
    let levels = vertical::levels_selected(
        start.altitude_feet,
        &crossings,
        envelope.climb_fpm,
        envelope.descent_fpm,
        select,
    )?;
    let mut altitude = start.altitude_feet;
    let mut result: Vec<Leg> = Vec::with_capacity(fixes.len());
    for (((samples, meters), crossing), level) in geometry.into_iter().zip(crossings).zip(levels) {
        let mut along = 0.0;
        let mut points = Vec::with_capacity(samples.len());
        for (index, sample) in samples.iter().enumerate() {
            if index > 0 {
                let previous = geographic(samples[index - 1].point, 0.0);
                let current = geographic(sample.point, 0.0);
                along += distance_nm(
                    [previous.latitude, previous.longitude],
                    [current.latitude, current.longitude],
                ) * 1852.0;
            }
            points.push(geographic(
                sample.point,
                altitude + (level - altitude) * (along / meters).clamp(0.0, 1.0),
            ));
        }
        // Preserve known joins through the projection round trip.
        points[0] = result
            .last()
            .and_then(|leg| leg.points.last())
            .copied()
            .unwrap_or(start);
        let fix = &fixes[result.len()];
        let coordinate = if fix.intercept.is_some() {
            let endpoint = points.last().unwrap();
            [endpoint.latitude, endpoint.longitude]
        } else {
            fix.coordinate
        };
        *points.last_mut().unwrap() = Waypoint {
            latitude: coordinate[0],
            longitude: coordinate[1],
            altitude_feet: level,
        };
        let nm: f64 = points
            .windows(2)
            .map(|pair| {
                distance_nm(
                    [pair[0].latitude, pair[0].longitude],
                    [pair[1].latitude, pair[1].longitude],
                )
            })
            .sum();
        let seconds = nm / fixes[result.len()].ground_knots * 3600.0;
        let rate = (level - altitude) * 60.0 / seconds;
        if rate > envelope.climb_fpm + 1.0 || -rate > envelope.descent_fpm + 1.0 {
            return Err("Geographic projection exceeds the vertical envelope".into());
        }
        altitude = level;
        result.push(Leg {
            points,
            ground_knots: fixes[result.len()].ground_knots,
            seconds: crossing.seconds,
        });
    }
    Ok(result)
}

fn intercept_curve(
    from: curves::Pose,
    intercept: CourseIntercept,
    required: Option<curves::Turn>,
    radius: f64,
    spacing: f64,
    project: &impl Fn([f64; 2]) -> Point,
) -> Result<Vec<curves::Sample>, String> {
    if !intercept.target.iter().all(|value| value.is_finite())
        || intercept.target[0].abs() >= 85.0
        || intercept.target[1].abs() > 180.0
        || !intercept.course.is_finite()
        || !intercept.target_course.is_finite()
        || !(1.0..=100.0).contains(&intercept.maximum_nm)
    {
        return Err("Invalid published course intercept".into());
    }
    let turn = match required {
        Some(curves::Turn::Right) => (intercept.course - from.heading).rem_euclid(360.0),
        Some(curves::Turn::Left) => -(from.heading - intercept.course).rem_euclid(360.0),
        None => crate::navigation::turn(from.heading, intercept.course),
    };
    let sign = if turn < 0.0 { -1.0 } else { 1.0 };
    let heading = from.heading.to_radians();
    let centre = [
        from.point.east + sign * radius * heading.cos(),
        from.point.north - sign * radius * heading.sin(),
    ];
    let at = |fraction: f64| {
        let heading = from.heading + turn * fraction;
        curves::Sample {
            point: Point {
                east: centre[0] - sign * radius * heading.to_radians().cos(),
                north: centre[1] + sign * radius * heading.to_radians().sin(),
                height: from.point.height,
            },
            heading: heading.rem_euclid(360.0),
        }
    };
    let end = if turn.abs() < 1e-8 {
        curves::Sample {
            point: from.point,
            heading: intercept.course.rem_euclid(360.0),
        }
    } else {
        at(1.0)
    };
    let fix = project(intercept.target);
    let delta = [fix.east - end.point.east, fix.north - end.point.north];
    let u = [
        intercept.course.to_radians().sin(),
        intercept.course.to_radians().cos(),
    ];
    let v = [
        intercept.target_course.to_radians().sin(),
        intercept.target_course.to_radians().cos(),
    ];
    let cross = |a: [f64; 2], b: [f64; 2]| a[0] * b[1] - a[1] * b[0];
    let divisor = cross(u, v);
    if divisor.abs() < 0.001 {
        return Err("Published intercept courses are parallel".into());
    }
    let along = cross(delta, v) / divisor;
    let remaining = cross(u, delta) / divisor;
    let arc = radius * turn.to_radians().abs();
    if !along.is_finite()
        || !remaining.is_finite()
        || along < -0.01
        || remaining < -0.01
        || arc + along.max(0.0) > intercept.maximum_nm * 1852.0
    {
        return Err("Published intercept cannot be reached after its bank-limited turn".into());
    }
    let arc_steps = (arc / spacing.min(radius * 2.0_f64.to_radians())).ceil() as usize;
    let straight_steps = (along.max(0.0) / spacing).ceil() as usize;
    if arc_steps + straight_steps > 8191 || arc + along.max(0.0) < 0.01 {
        return Err("Published intercept exceeds its geometry budget".into());
    }
    let mut samples = vec![curves::Sample {
        point: from.point,
        heading: from.heading,
    }];
    for index in 1..=arc_steps {
        samples.push(at(index as f64 / arc_steps as f64));
    }
    for index in 1..=straight_steps {
        let distance = along.max(0.0) * index as f64 / straight_steps as f64;
        samples.push(curves::Sample {
            point: Point {
                east: end.point.east + u[0] * distance,
                north: end.point.north + u[1] * distance,
                height: end.point.height,
            },
            heading: intercept.course.rem_euclid(360.0),
        });
    }
    Ok(samples)
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn late_join_stays_on_the_first_leg_and_handles_the_dateline() {
        let point = |latitude, longitude| Waypoint {
            latitude,
            longitude,
            altitude_feet: 1000.0,
        };
        let first = [point(0.0, 0.0), point(0.0, 0.1)];
        assert_eq!(forward_join_index(&first, [0.0, 0.05], 0.001), Some(1));
        for coordinate in [[0.0, -0.001], [0.0, 0.2], [0.0001, 0.05], [f64::NAN, 0.05]] {
            assert!(forward_join_index(&first, coordinate, 0.001).is_none());
        }
        assert!(forward_join_index(&first, [0.0, 0.05], f64::NAN).is_none());
        assert!(forward_join_index(&first, [0.0, 0.05], 1.0).is_none());
        let across = [point(0.0, 179.99), point(0.0, -179.99)];
        assert_eq!(forward_join_index(&across, [0.0, -180.0], 0.001), Some(1));
        let broken = [first[0], point(f64::NAN, 0.01), first[1]];
        assert!(forward_join_index(&broken, [0.0, 0.05], 0.001).is_none());
    }

    #[test]
    fn intermediate_limits_bound_duration_without_losing_the_published_crossing() {
        let start = Waypoint {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 10000.0,
        };
        let fix = Fix {
            coordinate: [0.0, 1.0],
            arrival_heading: Some(90.0),
            minimum_feet: 7000.0,
            maximum_feet: 7000.0,
            preferred_feet: 7000.0,
            ground_knots: 180.0,
            required_turn: None,
            intercept: None,
            airspeed: None,
        };
        let envelope = Envelope {
            bank_degrees: 20.0,
            turn_rate_degrees_sec: 3.0,
            climb_fpm: 1000.0,
            descent_fpm: 1000.0,
            spacing_meters: 250.0,
            maximum_detour_meters: 1000.0,
        };
        let original = draft(start, 90.0, std::slice::from_ref(&fix), 90.0, &envelope).unwrap();
        let grid = |_: usize, lower: f64, upper: f64, preferred: f64| {
            let low = (lower / 100.0).ceil();
            let high = (upper / 100.0).floor();
            (low <= high).then(|| (preferred / 100.0).round().clamp(low, high) * 100.0)
        };
        let (split, source) = original
            .clone()
            .split_limits(300.0, |_| true, grid)
            .unwrap();
        assert!(split.leg_count() > 1 && source.iter().all(|index| *index == 0));
        assert_eq!(split.crossings.last().unwrap().coordinate, fix.coordinate);
        assert_eq!(
            split.crossings.last().unwrap().minimum_feet,
            fix.minimum_feet
        );
        assert_eq!(
            split.crossings.last().unwrap().maximum_feet,
            fix.maximum_feet
        );
        assert!(
            split.crossings[..split.crossings.len() - 1]
                .iter()
                .all(|crossing| {
                    crossing.minimum_feet == crossing.maximum_feet
                        && crossing.minimum_feet > fix.maximum_feet
                        && (crossing.minimum_feet % 100.0).abs() < 1e-8
                })
        );
        assert!(split.legs.iter().all(|leg| leg.seconds <= 300.0 + 1e-6));
        for pair in split.legs.windows(2) {
            assert_eq!(pair[0].points.last(), pair[1].points.first());
        }
        assert!(split.clone().protect(|_, _| false).is_err());
        let released = split
            .protect(|a, b| {
                [a, b].iter().all(|point| {
                    point.latitude.abs() < 1e-6
                        && (6999.999..=10000.001).contains(&point.altitude_feet)
                })
            })
            .unwrap();
        let seconds = released.iter().map(|leg| leg.seconds).sum::<f64>();
        let actual = distance_nm([0.0, 0.0], [0.0, 1.0]) / 180.0 * 3600.0;
        assert!((seconds - actual).abs() < 1e-6);
        let (unchanged, _) = original
            .clone()
            .split_limits(
                60.0,
                |_| false,
                |_, _, _, _| panic!("Excluded approach cannot acquire a new limit"),
            )
            .unwrap();
        assert_eq!(unchanged.leg_count(), 1);
        assert!(
            original
                .clone()
                .split_limits(300.0, |_| true, |_, _, _, _| None)
                .is_err()
        );
        assert!(original.split_limits(1.0, |_| true, grid).is_err());
    }

    #[test]
    fn fly_by_waypoint_is_crossed_on_the_inbound_course() {
        let start = Waypoint {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 5000.0,
        };
        let fix = |coordinate| Fix {
            coordinate,
            arrival_heading: None,
            minimum_feet: 5000.0,
            maximum_feet: 5000.0,
            preferred_feet: 5000.0,
            ground_knots: 200.0,
            required_turn: None,
            intercept: None,
            airspeed: None,
        };
        let fixes = [fix([0.05, 0.0]), fix([0.1, 0.1])];
        let envelope = Envelope {
            bank_degrees: 25.0,
            turn_rate_degrees_sec: 3.0,
            climb_fpm: 2000.0,
            descent_fpm: 2000.0,
            spacing_meters: 250.0,
            maximum_detour_meters: 5000.0,
        };
        let draft = draft(start, 0.0, &fixes, 45.0, &envelope)
            .expect("a fly-by turn must not force the outbound course early");
        assert_eq!(draft.legs[0].points.last().unwrap().latitude, 0.05);
        assert!(
            draft.legs[0]
                .points
                .iter()
                .take(draft.legs[0].points.len() - 1)
                .all(|point| point.longitude.abs() < 1e-9),
            "the path turned toward the next leg before the fly-by fix"
        );
        let length = draft
            .legs
            .iter()
            .flat_map(|leg| leg.points.windows(2))
            .map(|pair| {
                distance_nm(
                    [pair[0].latitude, pair[0].longitude],
                    [pair[1].latitude, pair[1].longitude],
                )
            })
            .sum::<f64>();
        assert!(length < 20.0, "fly-by path took {length:.1} NM");
    }

    #[test]
    fn published_long_way_reversal_retains_its_turn_hand_and_radius() {
        let start = Waypoint {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 3000.0,
        };
        let coordinate = crate::arrival::destination([0.0, 0.0], 299.0, 10423.0 / 1852.0);
        let fix = Fix {
            coordinate,
            arrival_heading: Some(299.0),
            minimum_feet: 3000.0,
            maximum_feet: 3000.0,
            preferred_feet: 3000.0,
            ground_knots: 150.0,
            required_turn: Some(curves::Turn::Right),
            intercept: None,
            airspeed: None,
        };
        let envelope = Envelope {
            bank_degrees: 20.0,
            turn_rate_degrees_sec: 3.0,
            climb_fpm: 1000.0,
            descent_fpm: 1000.0,
            spacing_meters: 100.0,
            maximum_detour_meters: 1000.0,
        };
        let solved = draft(start, 89.0, &[fix], 299.0, &envelope).unwrap();
        let points = &solved.legs[0].points;
        let mut previous = 89.0;
        let mut right = 0.0;
        let mut left = 0.0;
        for pair in points.windows(2) {
            let course = bearing(
                [pair[0].latitude, pair[0].longitude],
                [pair[1].latitude, pair[1].longitude],
            );
            let turn = crate::navigation::turn(previous, course);
            right += turn.max(0.0);
            left += (-turn).max(0.0);
            previous = course;
        }
        assert!(
            right > 180.0 && left < 90.0,
            "The coded right reversal must not become the shorter left turn"
        );
        let speed = 150.0 * 1852.0 / 3600.0;
        let radius = (speed * speed / (9.80665 * 20.0f64.to_radians().tan()))
            .max(speed / 3.0f64.to_radians());
        let length = solved.legs[0].seconds * speed;
        assert!(length <= 10423.0 + std::f64::consts::TAU * radius + 1.0);
        assert!(solved.protect(|_, _| false).is_err());
    }

    #[test]
    fn course_intercept_turns_before_resolving_its_virtual_crossing() {
        let start = Waypoint {
            latitude: 43.0,
            longitude: 7.0,
            altitude_feet: 3000.0,
        };
        let geographic = |east, north| {
            [
                start.latitude + north / 111_320.0,
                start.longitude + east / (111_320.0 * start.latitude.to_radians().cos()),
            ]
        };
        let u = [146.0f64.to_radians().sin(), 146.0f64.to_radians().cos()];
        let v = [116.0f64.to_radians().sin(), 116.0f64.to_radians().cos()];
        let hint = geographic(u[0] * 2659.0, u[1] * 2659.0);
        let target = geographic(
            u[0] * 2659.0 + v[0] * 28.76 * 1852.0,
            u[1] * 2659.0 + v[1] * 28.76 * 1852.0,
        );
        let fix = Fix {
            coordinate: hint,
            arrival_heading: Some(146.0),
            minimum_feet: 3000.0,
            maximum_feet: 3000.0,
            preferred_feet: 3000.0,
            ground_knots: 130.0,
            airspeed: None,
            required_turn: Some(curves::Turn::Right),
            intercept: Some(CourseIntercept {
                target,
                target_course: 116.0,
                course: 146.0,
                heading: None,
                maximum_nm: 20.0,
            }),
        };
        let envelope = Envelope {
            bank_degrees: 20.0,
            turn_rate_degrees_sec: 3.0,
            climb_fpm: 1000.0,
            descent_fpm: 1000.0,
            spacing_meters: 150.0,
            maximum_detour_meters: 5000.0,
        };
        let solved = draft(start, 45.0, std::slice::from_ref(&fix), 146.0, &envelope).unwrap();
        assert!(solved.clone().protect(|_, _| false).is_err());
        let crossing = solved.crossings()[0].coordinate;
        assert!(distance_nm(hint, crossing) * 1852.0 > 500.0);
        let leg = &solved.legs[0];
        assert_eq!(
            [
                leg.points.last().unwrap().latitude,
                leg.points.last().unwrap().longitude
            ],
            crossing
        );
        let course = bearing(crossing, target);
        assert!(crate::navigation::turn(course, 116.0).abs() < 0.5);
        assert!(leg.seconds * leg.ground_knots * 1852.0 / 3600.0 < 10000.0);
        let first = bearing(
            [start.latitude, start.longitude],
            [leg.points[1].latitude, leg.points[1].longitude],
        );
        assert!(crate::navigation::turn(45.0, first).abs() < 1.1);
        assert_eq!(fix.coordinate, hint);
        let mut short = fix.clone();
        short.intercept.as_mut().unwrap().maximum_nm = 1.0;
        assert!(draft(start, 45.0, &[short], 146.0, &envelope).is_err());
        let mut parallel = fix.clone();
        parallel.intercept.as_mut().unwrap().target_course = 146.0;
        assert!(draft(start, 45.0, &[parallel], 146.0, &envelope).is_err());
    }

    #[test]
    fn coupled_airspeed_and_discrete_levels_keep_a_feasible_crossing() {
        let airspeed = Airspeed {
            indicated_knots: 150.0,
            atmosphere: super::super::air::Atmosphere::surface(1013.25, 15.0, 0.0).unwrap(),
            wind_from: 0.0,
            wind_knots: 0.0,
        };
        let start = Waypoint {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 0.0,
        };
        let envelope = Envelope {
            bank_degrees: 20.0,
            turn_rate_degrees_sec: 3.0,
            climb_fpm: 1000.0,
            descent_fpm: 1000.0,
            spacing_meters: 150.0,
            maximum_detour_meters: 100.0,
        };
        for tenth in 90..=110 {
            let fix = Fix {
                coordinate: [f64::from(tenth) / 600.0, 0.0],
                arrival_heading: Some(0.0),
                minimum_feet: 2500.0,
                maximum_feet: 10000.0,
                preferred_feet: 10000.0,
                ground_knots: 150.0,
                airspeed: Some(airspeed),
                required_turn: None,
                intercept: None,
            };
            let draft = draft_selected(
                start,
                0.0,
                std::slice::from_ref(&fix),
                0.0,
                &envelope,
                |_, lower, upper, preferred| {
                    let low = (lower / 100.0).ceil() * 100.0;
                    let high = (upper / 100.0).floor() * 100.0;
                    (low <= high).then(|| ((preferred / 100.0).floor() * 100.0).clamp(low, high))
                },
            )
            .unwrap();
            let leg = &draft.legs[0];
            let end = leg.points.last().unwrap();
            assert!((2500.0..=10000.0).contains(&end.altitude_feet));
            assert!((end.altitude_feet / 100.0 - (end.altitude_feet / 100.0).round()).abs() < 1e-6);
            assert!(
                (leg.ground_knots - airspeed.ground_knots(end.altitude_feet, 0.0).unwrap()).abs()
                    < 0.011
            );
            assert_eq!(fix.preferred_feet, 10000.0);
            assert!(end.altitude_feet / (leg.seconds / 60.0) <= 1001.0);
        }
    }

    #[test]
    fn solved_levels_preserve_piston_turboprop_and_jet_indicated_speeds() {
        let atmosphere = super::super::air::Atmosphere::surface(1013.25, 15.0, 0.0).unwrap();
        let start = Waypoint {
            latitude: 44.0,
            longitude: 8.0,
            altitude_feet: 18000.0,
        };
        for (indicated, climb, descent) in [
            (65.0, 650.0, 500.0),
            (110.0, 1500.0, 1200.0),
            (170.0, 2500.0, 1000.0),
        ] {
            let airspeed = Airspeed {
                indicated_knots: indicated,
                atmosphere,
                wind_from: 280.0,
                wind_knots: 12.0,
            };
            let fixes = [
                Fix {
                    coordinate: [44.3, 8.0],
                    arrival_heading: Some(0.0),
                    minimum_feet: 0.0,
                    maximum_feet: 18000.0,
                    preferred_feet: 18000.0,
                    ground_knots: airspeed.ground_knots(18000.0, 0.0).unwrap(),
                    airspeed: Some(airspeed),
                    required_turn: None,
                    intercept: None,
                },
                Fix {
                    coordinate: [44.3, 8.08],
                    arrival_heading: Some(90.0),
                    minimum_feet: 12000.0,
                    maximum_feet: 12000.0,
                    preferred_feet: 12000.0,
                    ground_knots: airspeed.ground_knots(12000.0, 90.0).unwrap(),
                    airspeed: Some(airspeed),
                    required_turn: None,
                    intercept: None,
                },
            ];
            let envelope = Envelope {
                bank_degrees: 20.0,
                turn_rate_degrees_sec: 3.0,
                climb_fpm: climb,
                descent_fpm: descent,
                spacing_meters: 150.0,
                maximum_detour_meters: 20000.0,
            };
            let fixed_speed = compile_once(start, 0.0, &fixes, 90.0, &envelope).unwrap();
            let draft = draft(start, 0.0, &fixes, 90.0, &envelope).unwrap();
            assert_eq!(draft.leg_count(), fixes.len());
            assert!(draft.clone().protect(|_, _| false).is_err());
            assert!(
                draft
                    .protect(|a, b| a.altitude_feet >= 10000.0 && b.altitude_feet >= 10000.0)
                    .is_ok()
            );
            let end = fixed_speed[0].points.last().unwrap();
            let before = fixed_speed[0].points.iter().rev().nth(1).unwrap();
            let required = airspeed
                .ground_knots(
                    end.altitude_feet,
                    bearing(
                        [before.latitude, before.longitude],
                        [end.latitude, end.longitude],
                    ),
                )
                .unwrap();
            assert!(
                (fixed_speed[0].ground_knots - required).abs() > 1.0,
                "Preferred-level speed must reproduce the old IAS mismatch"
            );
            let legs = compile(start, 0.0, &fixes, 90.0, &envelope, |a, b| {
                a.altitude_feet >= 10000.0 && b.altitude_feet >= 10000.0
            })
            .unwrap();
            assert!(
                legs[0].points.last().unwrap().altitude_feet < 17900.0,
                "{indicated}: downstream limit must force earlier descent"
            );
            assert!(
                (legs[0].ground_knots - fixes[0].ground_knots).abs() > 1.0,
                "Fixed preferred-level ground speed must change"
            );
            assert_eq!(legs[0].points[0], start);
            assert!(
                legs.windows(2)
                    .all(|pair| pair[0].points.last() == pair[1].points.first())
            );
            for (leg, fix) in legs.iter().zip(&fixes) {
                let last = leg.points.last().unwrap();
                assert_eq!([last.latitude, last.longitude], fix.coordinate);
            }
            for leg in &legs {
                let end = leg.points.last().unwrap();
                let before = leg.points.iter().rev().nth(1).unwrap();
                let expected = airspeed
                    .ground_knots(
                        end.altitude_feet,
                        bearing(
                            [before.latitude, before.longitude],
                            [end.latitude, end.longitude],
                        ),
                    )
                    .unwrap();
                assert!(
                    (leg.ground_knots - expected).abs() < 0.011,
                    "{indicated}: IAS changed when the vertical solution changed"
                );
            }
            assert!(compile(start, 0.0, &fixes, 90.0, &envelope, |_, _| false).is_err());
        }
    }
    #[test]
    fn crossing_ceiling_shapes_a_continuous_turning_descent() {
        let start = Waypoint {
            latitude: 44.0,
            longitude: 8.0,
            altitude_feet: 18000.0,
        };
        let fixes = [
            Fix {
                airspeed: None,
                coordinate: [44.2, 8.0],
                arrival_heading: None,
                minimum_feet: 0.0,
                maximum_feet: 18000.0,
                preferred_feet: 18000.0,
                ground_knots: 200.0,
                required_turn: None,
                intercept: None,
            },
            Fix {
                airspeed: None,
                coordinate: [44.2, 8.1],
                arrival_heading: None,
                minimum_feet: 12000.0,
                maximum_feet: 12000.0,
                preferred_feet: 12000.0,
                ground_knots: 180.0,
                required_turn: None,
                intercept: None,
            },
        ];
        let envelope = Envelope {
            bank_degrees: 20.0,
            turn_rate_degrees_sec: 3.0,
            climb_fpm: 1500.0,
            descent_fpm: 2000.0,
            spacing_meters: 150.0,
            maximum_detour_meters: 20000.0,
        };
        let path = compile(start, 0.0, &fixes, 90.0, &envelope, |a, b| {
            a.altitude_feet >= 10000.0 && b.altitude_feet >= 10000.0
        })
        .unwrap();
        assert!(path[0].points.last().unwrap().altitude_feet < 18000.0);
        assert_eq!(path[0].points.last(), path[1].points.first());
        assert!((path[1].points.last().unwrap().altitude_feet - 12000.0).abs() < 0.01);
        assert!(compile(start, 0.0, &fixes, 90.0, &envelope, |_, _| false).is_err());
    }
}
