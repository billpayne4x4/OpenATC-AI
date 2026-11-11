//! Nominal published missed approach from the MAP to its stated holding fix.

#![allow(clippy::default_trait_access)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use openatc_core::{
    arrival::distance_nm,
    traffic::{
        holding,
        motion::{Route, Segment, Stage, Waypoint},
        resolved::{self, Context},
        trajectory::{self, Envelope},
    },
};

#[derive(Clone)]
pub(super) struct HoldingPolicy {
    pub minimum_indicated_knots: f64,
    pub maximum_indicated_knots: f64,
    pub bank_degrees: f64,
    pub turn_rate_degrees_sec: f64,
}

/// A contingent clearance at an actual route boundary, with an aligned direct entry.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LimitHold {
    pub boundary: usize,
    pub position: Waypoint,
    pub inbound_true_degrees: f64,
    pub indicated_knots: f64,
    pub ground_knots: f64,
    pub leg_seconds: f64,
    pub bank_degrees: f64,
    pub turn_rate_degrees_sec: f64,
    reference: trajectory::Airspeed,
}
impl LimitHold {
    pub fn at(
        boundary: usize,
        segment: &Segment,
        fix: &trajectory::Fix,
        policy: &HoldingPolicy,
    ) -> Result<Self, String> {
        if !matches!(segment.stage, Stage::Climb | Stage::Cruise | Stage::Descent)
            || segment.points.len() < 2
            || !(40.0..=200.0).contains(&policy.maximum_indicated_knots)
            || !(40.0..=policy.maximum_indicated_knots).contains(&policy.minimum_indicated_knots)
        {
            return Err(
                "Clearance-limit hold has no supported aircraft or movement envelope".into(),
            );
        }
        let position = *segment.points.last().unwrap();
        let before = segment.points[segment.points.len() - 2];
        let coordinate = |point: Waypoint| [point.latitude, point.longitude];
        if distance_nm(coordinate(position), fix.coordinate) > 0.001 {
            return Err("Clearance-limit hold does not match its actual route crossing".into());
        }
        let airspeed = fix
            .airspeed
            .ok_or("Clearance-limit hold has no IAS reference")?;
        let indicated = airspeed.indicated_knots.min(policy.maximum_indicated_knots);
        if indicated < policy.minimum_indicated_knots {
            return Err("Clearance-limit holding speed is below the aircraft envelope".into());
        }
        let inbound = openatc_core::arrival::bearing(coordinate(before), coordinate(position));
        let level = airspeed
            .atmosphere
            .reported(position.altitude_feet, false)?;
        let result = Self {
            boundary,
            position,
            inbound_true_degrees: inbound,
            indicated_knots: indicated,
            ground_knots: trajectory::Airspeed {
                indicated_knots: indicated,
                ..airspeed
            }
            .ground_knots(position.altitude_feet, inbound)?,
            leg_seconds: if level > 14000.0 { 90.0 } else { 60.0 },
            bank_degrees: policy.bank_degrees.min(25.0),
            turn_rate_degrees_sec: policy.turn_rate_degrees_sec,
            reference: trajectory::Airspeed {
                indicated_knots: indicated,
                ..airspeed
            },
        };
        result.pattern().draft()?;
        Ok(result)
    }

    fn pattern(&self) -> holding::Pattern {
        holding::Pattern {
            fix: self.position,
            inbound_degrees: self.inbound_true_degrees,
            right: true,
            ground_knots: self.ground_knots,
            leg_seconds: self.leg_seconds,
            bank_degrees: self.bank_degrees,
            turn_rate_degrees_sec: self.turn_rate_degrees_sec,
        }
    }

    pub fn protect(
        &self,
        climb_fpm: f64,
        descent_fpm: f64,
        protected: impl Fn(Waypoint, Waypoint) -> bool,
    ) -> Result<Route, String> {
        Route::compile(
            vec![self.pattern().compile(protected)?],
            climb_fpm,
            descent_fpm,
        )
    }

    pub fn matches(&self, route: &Route) -> bool {
        let Some(segment) = route.segment(0) else {
            return false;
        };
        self.boundary < 512
            && self.position.altitude_feet.is_finite()
            && self.inbound_true_degrees.is_finite()
            && (0.0..360.0).contains(&self.inbound_true_degrees)
            && (40.0..=200.0).contains(&self.indicated_knots)
            && (self.reference.indicated_knots - self.indicated_knots).abs() <= f64::EPSILON
            && self
                .reference
                .ground_knots(self.position.altitude_feet, self.inbound_true_degrees)
                .is_ok_and(|ground| (ground - self.ground_knots).abs() < 1e-6)
            && segment.stage == Stage::Holding
            && route.segment(1).is_none()
            && segment.points.first() == Some(&self.position)
            && segment.points.last() == Some(&self.position)
            && segment
                .points
                .iter()
                .all(|p| (p.altitude_feet - self.position.altitude_feet).abs() < 1e-6)
            && (segment.knots - self.ground_knots).abs() < 1e-6
            && self
                .pattern()
                .draft()
                .is_ok_and(|draft| draft.matches(segment))
    }
}

#[derive(Clone)]
pub(super) struct Draft {
    /// Published MAP provenance, retained when a late path starts farther along the runway.
    #[allow(dead_code)]
    pub map: Waypoint,
    pub holding_fix: String,
    pub holding_feet: f64,
    path: trajectory::Draft,
    hold: holding::Draft,
    entry: Option<holding::EntryDraft>,
    climb_fpm: f64,
    descent_fpm: f64,
}
impl Draft {
    pub fn late_entry_available(&self, position: Waypoint) -> bool {
        self.path
            .first_leg_join([position.latitude, position.longitude], 0.001)
    }
    /// This checks both the missed path and the whole hold before returning a route.
    pub fn protect(self, protected: impl Fn(Waypoint, Waypoint) -> bool) -> Result<Route, String> {
        let mut segments = self
            .path
            .protect(&protected)?
            .into_iter()
            .map(|leg| Segment {
                stage: Stage::MissedApproach,
                points: leg.points,
                knots: leg.ground_knots,
                resources: Vec::new(),
                dwell_seconds: 0.0,
            })
            .collect::<Vec<_>>();
        if let Some(entry) = self.entry {
            segments.push(entry.protect(&protected)?);
        }
        segments.push(self.hold.protect(protected)?);
        Route::compile(segments, self.climb_fpm, self.descent_fpm)
    }
}

/// Retain the remaining final-approach lateral path before joining the published missed route.
/// A late departure from the landing path may join only the first missed leg ahead of the MAP.
pub(super) fn from_final(
    start: Waypoint,
    remaining_final: &[Segment],
    missed: &Route,
    climb_fpm: f64,
    descent_fpm: f64,
    protected: impl Fn(Waypoint, Waypoint) -> bool,
) -> Result<Route, String> {
    if ![start.latitude, start.longitude, start.altitude_feet]
        .iter()
        .all(|value| value.is_finite())
        || !(-90.0..=90.0).contains(&start.latitude)
        || !(-180.0..=180.0).contains(&start.longitude)
        || !(-3000.0..=65000.0).contains(&start.altitude_feet)
    {
        return Err("Final recovery position is invalid".into());
    }
    let mut suffix = (0..512)
        .map_while(|index| missed.segment(index).cloned())
        .collect::<Vec<_>>();
    if suffix.len() < 2
        || suffix[0].stage != Stage::MissedApproach
        || suffix
            .last()
            .is_none_or(|segment| segment.stage != Stage::Holding)
    {
        return Err("Final recovery needs a complete published missed approach and hold".into());
    }
    let coordinate = |point: Waypoint| [point.latitude, point.longitude];
    let map = suffix[0].points[0];
    let mut prefix = Vec::new();
    if !remaining_final.is_empty() {
        if distance_nm(coordinate(start), coordinate(remaining_final[0].points[0])) > 0.001
            || (start.altitude_feet - remaining_final[0].points[0].altitude_feet).abs() > 1.0
            || distance_nm(
                coordinate(*remaining_final.last().unwrap().points.last().unwrap()),
                coordinate(map),
            ) > 0.001
            || remaining_final
                .iter()
                .any(|segment| segment.stage != Stage::Approach || !segment.resources.is_empty())
        {
            return Err("Early missed approach does not retain its final path to the MAP".into());
        }
        for original in remaining_final {
            let mut segment = original.clone();
            segment.stage = Stage::MissedApproach;
            for point in &mut segment.points {
                point.altitude_feet = point.altitude_feet.max(start.altitude_feet);
            }
            prefix.push(segment);
        }
    } else if distance_nm(coordinate(start), coordinate(map)) > 0.001 {
        let first = &mut suffix[0];
        let cut = trajectory::forward_join_index(&first.points, coordinate(start), 0.001)
            .ok_or("Late missed approach cannot turn back to the MAP or skip a published leg")?;
        let end = *first.points.last().unwrap();
        if end.altitude_feet < start.altitude_feet {
            return Err("Late recovery would descend before its first missed fix".into());
        }
        let mut points = vec![start];
        points.extend(
            first
                .points
                .iter()
                .skip(cut)
                .copied()
                .filter(|point| distance_nm(coordinate(start), coordinate(*point)) > 1e-8),
        );
        let distances = points
            .windows(2)
            .map(|pair| distance_nm(coordinate(pair[0]), coordinate(pair[1])))
            .collect::<Vec<_>>();
        let total = distances.iter().sum::<f64>();
        if total <= 0.0 {
            return Err("Late recovery has no forward climb distance".into());
        }
        let mut flown = 0.0;
        for (point, distance) in points.iter_mut().skip(1).zip(distances) {
            flown += distance;
            point.altitude_feet =
                start.altitude_feet + (end.altitude_feet - start.altitude_feet) * flown / total;
        }
        *points.last_mut().unwrap() = end;
        first.points = points;
    }
    for segment in &mut suffix {
        if segment.points.last().unwrap().altitude_feet + 1e-6 < start.altitude_feet {
            return Err("Recovery needs a new cleared level above its published limit".into());
        }
        for point in &mut segment.points {
            point.altitude_feet = point.altitude_feet.max(start.altitude_feet);
        }
    }
    if prefix.is_empty() {
        suffix[0].points[0] = start;
    }
    prefix.extend(suffix);
    if prefix.iter().any(|segment| {
        segment
            .points
            .windows(2)
            .any(|pair| !protected(pair[0], pair[1]))
    }) {
        return Err("Modified final recovery lacks complete corridor protection".into());
    }
    Route::compile(prefix, climb_fpm, descent_fpm)
}

/// Prepare each final boundary at its actual entry height, using one explicit missed level.
/// These remain opaque drafts until both protection and the approach clearance are complete.
pub(super) type Variants = (
    std::collections::BTreeMap<usize, std::sync::Arc<Draft>>,
    Option<super::flight::MissedInstruction>,
);

pub(super) fn final_variants(
    planner: &super::airborne::Planner<'_>,
    terminal: &super::routing::Terminal,
    map: &trajectory::Fix,
    minimum_feet: f64,
    nominal: &super::airborne::Draft,
    published: &Draft,
    late: Waypoint,
) -> Result<Variants, String> {
    let mut levels = nominal.approach_levels();
    if levels.is_empty() {
        return Err("Final recovery has no approach limits".into());
    }
    let maximum = levels
        .iter()
        .map(|(_, start, end)| start.max(*end))
        .fold(published.holding_feet, f64::max);
    let weather = planner.arrival_weather;
    let region = openatc_core::regions::region_for(planner.regions, &planner.destination.icao);
    let standard =
        weather.atmosphere.reported(maximum, false)? >= f64::from(region.transition_feet);
    let step = if !standard && region.altitude == "meters" {
        50.0 / 0.3048
    } else {
        100.0
    };
    let reported = (weather.atmosphere.reported(maximum, standard)? / step - 1e-8).ceil() * step;
    let target = weather
        .atmosphere
        .geometric(reported, standard)?
        .max(maximum);
    let amended = target > published.holding_feet + 1.0;
    let floor = if amended {
        target
    } else {
        published.holding_feet
    };
    let instruction = amended.then(|| super::flight::MissedInstruction {
        fix: published.holding_fix.clone(),
        geometric_feet: floor,
        reported_feet: reported.round() as i32,
        standard,
    });
    let map_index = nominal.leg_count() - 1;
    // Refuse an unusable balked-landing path before constructing the earlier final recoveries.
    levels.sort_by_key(|(index, _, _)| (*index != map_index, *index));
    let mut variants = std::collections::BTreeMap::new();
    let mut by_height: std::collections::BTreeMap<u64, std::sync::Arc<Draft>> = Default::default();
    let mut late_seed = None;
    for (index, _, end) in levels {
        let seed = if let Some(existing) = by_height.get(&end.to_bits()) {
            existing.clone()
        } else {
            let mut entry = map.clone();
            entry.preferred_feet = end;
            let seed = std::sync::Arc::new(prepare_with_clearance(
                planner,
                terminal,
                &entry,
                minimum_feet,
                Some(floor),
                instruction.as_ref(),
            )?);
            if (seed.holding_feet - floor).abs() > 1.0 {
                return Err("Final recovery cannot retain its cleared holding level".into());
            }
            by_height.insert(end.to_bits(), seed.clone());
            seed
        };
        if index == map_index {
            if terminal
                .missed
                .first()
                .is_some_and(|leg| matches!(leg.terminator.as_str(), "CA" | "VA"))
            {
                // Re-solve the altitude termination from the actual balked-landing position.
                // Fixed published legs still require the original forward join.
                late_seed = Some(std::sync::Arc::new(prepare_start(
                    planner,
                    terminal,
                    map,
                    minimum_feet,
                    Some(floor),
                    instruction.as_ref(),
                    Some(late),
                )?));
            } else if seed.late_entry_available(late) {
                late_seed = Some(seed.clone());
            } else {
                return Err(
                    "Landing path cannot join its first published missed leg after the threshold"
                        .into(),
                );
            }
        }
        variants.insert(index, seed);
    }
    let late_seed = late_seed.ok_or("Final recovery lost its late-entry variant")?;
    variants.insert(nominal.leg_count(), late_seed);
    Ok((variants, instruction))
}

/// Reuse an explicit crossing level only when the hold immediately repeats the same published fix.
/// A bare ceiling or a restriction at another fix cannot supply the holding floor.
fn holding_restriction(
    hold: &openatc_core::traffic::procedures::Leg,
    prefix: &[openatc_core::traffic::procedures::Leg],
) -> Option<openatc_core::traffic::procedures::Leg> {
    if hold.altitude_one.is_some() {
        return Some(hold.clone());
    }
    if hold.altitude_two.is_some()
        || hold.altitude_one_is_flight_level.is_some()
        || hold.altitude_two_is_flight_level.is_some()
        || !hold.altitude_description.trim().is_empty()
    {
        return None;
    }
    let incoming = prefix.last()?;
    if !matches!(incoming.terminator.as_str(), "IF" | "TF" | "DF" | "CF")
        || incoming.identifier.is_empty()
        || incoming.identifier != hold.identifier
        || incoming.region != hold.region
        || incoming.section != hold.section
        || incoming.subsection != hold.subsection
        || incoming.altitude_one.is_none()
        || incoming.altitude_one_is_flight_level.is_none()
        || incoming.altitude_two.is_some()
        || incoming.altitude_two_is_flight_level.is_some()
        || !matches!(incoming.altitude_description.trim(), "" | "+")
    {
        return None;
    }
    let mut resolved = hold.clone();
    resolved
        .altitude_description
        .clone_from(&incoming.altitude_description);
    resolved.altitude_one = incoming.altitude_one;
    resolved.altitude_one_is_flight_level = incoming.altitude_one_is_flight_level;
    Some(resolved)
}

/// This seed starts at the published MAP, not at an arbitrary early or late go-around position.
pub(super) fn prepare(
    planner: &super::airborne::Planner<'_>,
    terminal: &super::routing::Terminal,
    map: &trajectory::Fix,
    minimum_feet: f64,
) -> Result<Draft, String> {
    prepare_with_holding_floor(planner, terminal, map, minimum_feet, None)
}

/// Re-solve an amended missed level against the original published restrictions.
/// The resulting draft still requires protection and an explicit amended clearance.
pub(super) fn prepare_with_holding_floor(
    planner: &super::airborne::Planner<'_>,
    terminal: &super::routing::Terminal,
    map: &trajectory::Fix,
    minimum_feet: f64,
    holding_floor: Option<f64>,
) -> Result<Draft, String> {
    prepare_with_clearance(planner, terminal, map, minimum_feet, holding_floor, None)
}

fn prepare_with_clearance(
    planner: &super::airborne::Planner<'_>,
    terminal: &super::routing::Terminal,
    map: &trajectory::Fix,
    minimum_feet: f64,
    holding_floor: Option<f64>,
    amendment: Option<&super::flight::MissedInstruction>,
) -> Result<Draft, String> {
    prepare_start(
        planner,
        terminal,
        map,
        minimum_feet,
        holding_floor,
        amendment,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_start(
    planner: &super::airborne::Planner<'_>,
    terminal: &super::routing::Terminal,
    map: &trajectory::Fix,
    minimum_feet: f64,
    holding_floor: Option<f64>,
    amendment: Option<&super::flight::MissedInstruction>,
    late: Option<Waypoint>,
) -> Result<Draft, String> {
    if holding_floor
        .is_some_and(|level| !level.is_finite() || !(-3000.0..=65000.0).contains(&level))
    {
        return Err("Amended missed-approach holding level is invalid".into());
    }
    let map_leg = terminal
        .approach
        .last()
        .filter(|leg| leg.missed_point())
        .ok_or("Approach has no explicit published missed-approach point")?;
    let position = openatc_core::traffic::coordinates::Points::runway(map_leg, planner.destination)
        .or_else(|| planner.points.resolve(map_leg, &planner.destination.icao))
        .ok_or("Published missed-approach point is unresolved")?;
    if distance_nm(map.coordinate, position.coordinate) >= 0.001 {
        return Err("Nominal missed approach must start at its published MAP".into());
    }
    let (published, prefix) = terminal
        .missed
        .split_last()
        .ok_or("Missed approach is missing")?;
    if published.terminator != "HM" {
        return prepare_fallback(
            planner,
            terminal,
            map,
            minimum_feet,
            holding_floor,
            amendment,
            late,
        );
    }
    if published.terminator != "HM"
        || prefix.is_empty()
        || prefix.iter().any(|leg| leg.terminator == "HM")
    {
        return Err(
            "Missed approach needs a complete path and an explicit terminal hold level".into(),
        );
    }
    let published = holding_restriction(published, prefix)
        .ok_or("Missed approach holding level has no explicit same-fix restriction")?;
    let published = &published;
    let weather = planner.arrival_weather;
    if !weather.fresh() {
        return Err("Missed approach weather is stale".into());
    }
    let published_map = Waypoint {
        latitude: map.coordinate[0],
        longitude: map.coordinate[1],
        altitude_feet: map.preferred_feet,
    };
    let start = if let Some(start) = late {
        if !prefix
            .first()
            .is_some_and(|leg| matches!(leg.terminator.as_str(), "CA" | "VA"))
        {
            return Err(
                "Late missed approach needs a separately planned first altitude leg".into(),
            );
        }
        let local = openatc_core::airport::airport_point(
            planner.destination,
            start.latitude,
            start.longitude,
        );
        let along = planner.arrival_surface.along(local);
        let centre = planner.arrival_surface.point(along);
        let height = planner.arrival_surface.height(local);
        if [start.latitude, start.longitude, start.altitude_feet, along]
            .iter()
            .any(|value| !value.is_finite())
            || along <= planner.arrival_surface.threshold
            || along >= planner.arrival_surface.length
            || (local.east - centre.east).hypot(local.north - centre.north) > 1.852
            || start.altitude_feet < height - 1.0
            || start.altitude_feet > published_map.altitude_feet + 1.0
        {
            return Err("Late missed approach start is outside its checked runway envelope".into());
        }
        start
    } else {
        published_map
    };
    let map_speed = map.airspeed.ok_or("Missed approach has no MAP airspeed")?;
    if map_speed.atmosphere != weather.atmosphere
        || (map_speed.wind_from - weather.wind_from).abs() > f64::EPSILON
        || (map_speed.wind_knots - weather.wind_knots).abs() > f64::EPSILON
    {
        return Err("Missed approach weather does not match its MAP inputs".into());
    }
    let indicated = published
        .speed_knots
        .map_or(map_speed.indicated_knots, |limit| {
            map_speed.indicated_knots.min(limit)
        });
    if indicated < planner.aircraft.approach_knots {
        return Err("Published hold speed is below the fleet approach envelope".into());
    }
    let context = Context {
        airport: planner.destination,
        points: planner.points,
        atmosphere: weather.atmosphere,
        variation: weather.variation,
        wind_from: weather.wind_from,
        wind_knots: weather.wind_knots,
        indicated_knots: indicated,
        maximum_intercept_nm: planner.policy.terminal_intercept_limit_nm,
        climb_fpm: planner.aircraft.climb_fpm,
        minimum_feet,
        maximum_feet: weather
            .atmosphere
            .geometric(planner.aircraft.ceiling, true)?,
        preferred_feet: start.altitude_feet,
    };
    let (low, high) = resolved::altitude_bounds(published, &context)?;
    let level = low
        .max(start.altitude_feet)
        .max(holding_floor.unwrap_or(low));
    let explicit = amendment.is_some_and(|amendment| {
        amendment.fix == published.identifier
            && (amendment.geometric_feet - level).abs() <= 1.0
            && weather
                .atmosphere
                .reported(level, amendment.standard)
                .is_ok_and(|reported| (reported - f64::from(amendment.reported_feet)).abs() <= 1.0)
    });
    if level > context.maximum_feet || level > high && !explicit {
        return Err("MAP altitude conflicts with the published holding level".into());
    }
    let published_standard = published
        .altitude_one_is_flight_level
        .ok_or("Holding pressure reference is unresolved")?;
    if published.altitude_two.is_some()
        && published.altitude_two_is_flight_level != Some(published_standard)
    {
        return Err("Mixed holding pressure references need an explicit clearance solution".into());
    }
    if amendment.is_some() && !explicit {
        return Err("Amended missed level does not match its named holding instruction".into());
    }
    let standard = amendment.map_or(published_standard, |amendment| amendment.standard);
    let cleared_leg = |leg: &openatc_core::traffic::procedures::Leg| {
        let mut leg = leg.clone();
        if explicit {
            let amendment = amendment.unwrap();
            leg.altitude_description.clear();
            leg.altitude_one = Some(amendment.reported_feet);
            leg.altitude_one_is_flight_level = Some(amendment.standard);
            leg.altitude_two = None;
            leg.altitude_two_is_flight_level = None;
        }
        leg
    };
    let mut position = planner
        .points
        .resolve(published, &planner.destination.icao)
        .ok_or("Missed approach holding fix is unresolved")?;
    position.magnetic_variation_degrees = Some(
        planner
            .points
            .course_variation(published, &planner.destination.icao)
            .unwrap_or(weather.variation),
    );
    let course = (published.course.ok_or("Missed holding course is missing")?
        + position.magnetic_variation_degrees.unwrap())
    .rem_euclid(360.0);
    let airspeed = trajectory::Airspeed {
        indicated_knots: indicated,
        ..map_speed
    };
    let pattern = holding::Pattern::published(
        &cleared_leg(published),
        position,
        &holding::Clearance {
            level_feet: weather.atmosphere.reported(level, standard)?.round() as i32,
            geometric_feet: level,
            indicated_knots: indicated,
            ground_knots: airspeed.ground_knots(level, course)?,
            bank_degrees: planner.handling.maximum_bank_degrees,
            turn_rate_degrees_sec: planner.policy.holding_turn_rate_degrees_sec,
        },
    )?;
    // Only the named holding fix receives the explicitly amended level. Other crossings keep their limits.
    let prefix = prefix
        .iter()
        .map(|leg| {
            if explicit
                && leg.identifier == published.identifier
                && planner
                    .points
                    .resolve(leg, &planner.destination.icao)
                    .is_some_and(|point| distance_nm(point.coordinate, position.coordinate) < 0.001)
            {
                cleared_leg(leg)
            } else {
                leg.clone()
            }
        })
        .collect::<Vec<_>>();
    for leg in &prefix {
        let (_, ceiling) = resolved::altitude_bounds(leg, &context)?;
        if ceiling < start.altitude_feet {
            return Err("Missed entry exceeds a published intermediate ceiling".into());
        }
    }
    let mut fixes = resolved::boundary_crossings(start, &prefix, &context)?;
    for fix in &mut fixes {
        fix.minimum_feet = fix.minimum_feet.max(start.altitude_feet);
        if fix.minimum_feet > fix.maximum_feet {
            return Err("Missed approach would descend below its entry level".into());
        }
        fix.preferred_feet = fix.preferred_feet.max(start.altitude_feet);
    }
    let last = fixes
        .last_mut()
        .ok_or("Missed approach has no holding entry")?;
    if distance_nm(last.coordinate, position.coordinate) >= 0.001 {
        return Err("Missed approach does not reach its named holding fix".into());
    }
    if last.airspeed != Some(airspeed) || level < last.minimum_feet || level > last.maximum_feet {
        return Err(format!(
            "Missed approach requires a separate holding entry or speed transition (incoming course {:?}, holding course {course:.2}, matching IAS {}, holding level {level:.1} within {:.1}..{:.1})",
            last.arrival_heading,
            last.airspeed == Some(airspeed),
            last.minimum_feet,
            last.maximum_feet
        ));
    }
    // The HM level applies to the incoming leg. Its turn direction belongs to
    // the racetrack and must not replace a preceding missed-approach turn.
    last.minimum_feet = level;
    last.maximum_feet = level;
    last.preferred_feet = level;
    let incoming = last.arrival_heading.unwrap_or(course);
    let entry = pattern.direct_entry(incoming)?;
    let final_heading = if entry.is_some() { incoming } else { course };
    last.arrival_heading = Some(final_heading);
    last.ground_knots = airspeed.ground_knots(level, final_heading)?;
    let path = trajectory::draft(
        start,
        planner.arrival_surface.heading(),
        &fixes,
        final_heading,
        &Envelope {
            bank_degrees: planner.handling.maximum_bank_degrees,
            turn_rate_degrees_sec: planner.policy.airborne_turn_rate_degrees_sec,
            climb_fpm: planner.aircraft.climb_fpm,
            descent_fpm: planner.aircraft.descent_fpm,
            spacing_meters: planner.policy.airborne_sample_meters,
            maximum_detour_meters: planner.policy.airborne_detour_meters,
        },
    )?;
    let hold = pattern.draft()?;
    if !weather.fresh() {
        return Err("Missed approach weather expired during preparation".into());
    }
    Ok(Draft {
        map: published_map,
        holding_fix: published.identifier.clone(),
        holding_feet: level,
        path,
        hold,
        entry,
        climb_fpm: planner.aircraft.climb_fpm,
        descent_fpm: planner.aircraft.descent_fpm,
    })
}

/// Missed approaches that end in a climb, a direct-to-fix or a vector have no published
/// hold. Fly the published path to its level, then hold at the end of it. The hold is an
/// engine fallback, not a published procedure: right turns, one-minute legs, and the
/// published level.
#[allow(clippy::too_many_arguments)]
fn prepare_fallback(
    planner: &super::airborne::Planner<'_>,
    terminal: &super::routing::Terminal,
    map: &trajectory::Fix,
    minimum_feet: f64,
    holding_floor: Option<f64>,
    amendment: Option<&super::flight::MissedInstruction>,
    late: Option<Waypoint>,
) -> Result<Draft, String> {
    if amendment.is_some() {
        return Err("Amended missed level needs a published hold".into());
    }
    let missed = &terminal.missed;
    let last = missed
        .last()
        .filter(|leg| matches!(leg.terminator.as_str(), "CA" | "DF" | "VM"))
        .ok_or("Missed approach ends without a published hold or climb")?;
    if missed.iter().any(|leg| leg.terminator == "HM") {
        return Err("Missed approach mixes a published hold with a climb".into());
    }
    let weather = planner.arrival_weather;
    if !weather.fresh() {
        return Err("Missed approach weather is stale".into());
    }
    let published_map = Waypoint {
        latitude: map.coordinate[0],
        longitude: map.coordinate[1],
        altitude_feet: map.preferred_feet,
    };
    let start = if let Some(start) = late {
        let local = openatc_core::airport::airport_point(
            planner.destination,
            start.latitude,
            start.longitude,
        );
        let along = planner.arrival_surface.along(local);
        let centre = planner.arrival_surface.point(along);
        let height = planner.arrival_surface.height(local);
        if [start.latitude, start.longitude, start.altitude_feet, along]
            .iter()
            .any(|value| !value.is_finite())
            || along <= planner.arrival_surface.threshold
            || along >= planner.arrival_surface.length
            || (local.east - centre.east).hypot(local.north - centre.north) > 1.852
            || start.altitude_feet < height - 1.0
            || start.altitude_feet > published_map.altitude_feet + 1.0
        {
            return Err("Late missed approach start is outside its checked runway envelope".into());
        }
        start
    } else {
        published_map
    };
    let map_speed = map.airspeed.ok_or("Missed approach has no MAP airspeed")?;
    if map_speed.atmosphere != weather.atmosphere
        || (map_speed.wind_from - weather.wind_from).abs() > f64::EPSILON
        || (map_speed.wind_knots - weather.wind_knots).abs() > f64::EPSILON
    {
        return Err("Missed approach weather does not match its MAP inputs".into());
    }
    let indicated = map_speed.indicated_knots;
    let context = Context {
        airport: planner.destination,
        points: planner.points,
        atmosphere: weather.atmosphere,
        variation: weather.variation,
        wind_from: weather.wind_from,
        wind_knots: weather.wind_knots,
        indicated_knots: indicated,
        maximum_intercept_nm: planner.policy.terminal_intercept_limit_nm,
        climb_fpm: planner.aircraft.climb_fpm,
        minimum_feet,
        maximum_feet: weather
            .atmosphere
            .geometric(planner.aircraft.ceiling, true)?,
        preferred_feet: start.altitude_feet,
    };
    // A missed approach can change course between climb legs. The fallback climbs
    // straight on the first course to the final missing level instead of forcing that
    // turn, then holds at the end of the climb.
    let mut path_legs = missed.to_vec();
    if let Some(first) = missed.first().filter(|leg| leg.terminator == "CA") {
        let mut leg = first.clone();
        leg.altitude_description
            .clone_from(&last.altitude_description);
        leg.altitude_one = last.altitude_one;
        leg.altitude_one_is_flight_level = last.altitude_one_is_flight_level;
        leg.altitude_two = None;
        leg.altitude_two_is_flight_level = None;
        leg.course = leg.course.or(last.course);
        path_legs = vec![leg];
    }
    let path_course = path_legs.first().and_then(|leg| leg.course);
    let mut fixes = resolved::boundary_crossings(start, &path_legs, &context)?;
    let (low, high) = resolved::altitude_bounds(last, &context)?;
    let level = low
        .max(start.altitude_feet)
        .max(holding_floor.unwrap_or(low));
    if level > context.maximum_feet || level > high {
        return Err("Missed approach level conflicts with its published climb".into());
    }
    let reported_level = weather.atmosphere.reported(level, false)?.round() as i32;
    let variation = weather.variation;
    let course = fixes
        .last()
        .and_then(|end| {
            end.arrival_heading
                .or_else(|| path_course.map(|course| (course + variation).rem_euclid(360.0)))
                .or_else(|| {
                    last.course
                        .map(|course| (course + variation).rem_euclid(360.0))
                })
        })
        .ok_or("Missed approach has no inbound course")?;
    // A short climb ends too close to the MAP to turn into the hold. Extend the level
    // segment along the inbound course to give the turn room; this is part of the same
    // engine fallback, not a published leg.
    let distance = fixes.last().map_or(0.0, |end| {
        distance_nm(
            [start.latitude, start.longitude],
            [end.coordinate[0], end.coordinate[1]],
        )
    });
    if distance < 5.0
        && let Some(end) = fixes.last()
    {
        let extra = 5.0 - distance;
        let radians = course.to_radians();
        let latitude = end.coordinate[0] + extra / 60.0 * radians.cos();
        let longitude = end.coordinate[1]
            + extra / 60.0 * radians.sin() / end.coordinate[0].to_radians().cos().max(0.05);
        let extension_speed = trajectory::Airspeed {
            indicated_knots: indicated,
            ..map_speed
        };
        fixes.push(trajectory::Fix {
            coordinate: [latitude, longitude],
            arrival_heading: Some(course),
            minimum_feet: level,
            maximum_feet: level,
            preferred_feet: level,
            ground_knots: extension_speed.ground_knots(level, course)?,
            airspeed: Some(extension_speed),
            required_turn: None,
            intercept: None,
        });
    }
    let end = fixes
        .last_mut()
        .ok_or("Missed approach has no path to a hold point")?;
    end.minimum_feet = level;
    end.maximum_feet = level;
    end.preferred_feet = level;
    let identifier = if last.terminator == "DF" && !last.identifier.is_empty() {
        last.identifier.clone()
    } else {
        terminal
            .approach
            .last()
            .filter(|leg| !leg.identifier.is_empty())
            .map_or_else(|| "MA".into(), |leg| leg.identifier.clone())
    };
    let mut hold_leg = last.clone();
    hold_leg.identifier.clone_from(&identifier);
    hold_leg.terminator = "HM".into();
    hold_leg.turn = "R".into();
    hold_leg.course = Some((course - variation).rem_euclid(360.0));
    hold_leg.altitude_description.clear();
    hold_leg.altitude_one = Some(reported_level);
    hold_leg.altitude_one_is_flight_level = Some(false);
    hold_leg.altitude_two = None;
    hold_leg.altitude_two_is_flight_level = None;
    hold_leg.speed_description = "-".into();
    hold_leg.speed_knots = None;
    hold_leg.distance_time = "T010".into();
    let position = openatc_core::traffic::coordinates::Position {
        coordinate: end.coordinate,
        elevation_feet: None,
        magnetic_variation_degrees: Some(variation),
    };
    let airspeed = trajectory::Airspeed {
        indicated_knots: indicated,
        ..map_speed
    };
    let pattern = holding::Pattern::published(
        &hold_leg,
        position,
        &holding::Clearance {
            level_feet: reported_level,
            geometric_feet: level,
            indicated_knots: indicated,
            ground_knots: airspeed.ground_knots(level, course)?,
            bank_degrees: planner.handling.maximum_bank_degrees,
            turn_rate_degrees_sec: planner.policy.holding_turn_rate_degrees_sec,
        },
    )?;
    let incoming = end.arrival_heading.unwrap_or(course);
    let entry = pattern.direct_entry(incoming)?;
    let final_heading = if entry.is_some() { incoming } else { course };
    let last_fix = fixes.last_mut().expect("checked missed path");
    last_fix.arrival_heading = Some(final_heading);
    last_fix.ground_knots = airspeed.ground_knots(level, final_heading)?;
    let path = trajectory::draft(
        start,
        planner.arrival_surface.heading(),
        &fixes,
        final_heading,
        &Envelope {
            bank_degrees: planner.handling.maximum_bank_degrees,
            turn_rate_degrees_sec: planner.policy.airborne_turn_rate_degrees_sec,
            climb_fpm: planner.aircraft.climb_fpm,
            descent_fpm: planner.aircraft.descent_fpm,
            spacing_meters: planner.policy.airborne_sample_meters,
            maximum_detour_meters: planner.policy.airborne_detour_meters,
        },
    )?;
    let hold = pattern.draft()?;
    if !weather.fresh() {
        return Err("Missed approach weather expired during preparation".into());
    }
    Ok(Draft {
        map: published_map,
        holding_fix: identifier,
        holding_feet: level,
        path,
        hold,
        entry,
        climb_fpm: planner.aircraft.climb_fpm,
        descent_fpm: planner.aircraft.descent_fpm,
    })
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use openatc_core::{
        airport::{Airport, Runway},
        arrival::Threshold,
        state::Point,
        traffic::{
            animation, coordinates::Points, procedures::Catalogue, profiles, trajectory::Fix,
        },
    };
    use std::{
        collections::BTreeSet,
        time::{Instant, SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn limit_hold_brakes_before_entry_and_keeps_flying_without_onward_authority() {
        use openatc_core::traffic::{air::Atmosphere, motion::World, trajectory::Airspeed};
        use std::sync::Arc;
        let atmosphere = Atmosphere::surface(1013.25, 15.0, 0.0).unwrap();
        let reference = Airspeed {
            atmosphere,
            indicated_knots: 270.0,
            wind_from: 0.0,
            wind_knots: 0.0,
        };
        let point = |longitude| Waypoint {
            latitude: 0.0,
            longitude,
            altitude_feet: 35000.0,
        };
        let segment = Segment {
            stage: Stage::Cruise,
            points: vec![point(0.0), point(0.2)],
            knots: reference.ground_knots(35000.0, 90.0).unwrap(),
            resources: vec![],
            dwell_seconds: 0.0,
        };
        let fix = Fix {
            coordinate: [0.0, 0.2],
            arrival_heading: Some(90.0),
            minimum_feet: 35000.0,
            maximum_feet: 35000.0,
            preferred_feet: 35000.0,
            ground_knots: segment.knots,
            airspeed: Some(reference),
            required_turn: None,
            intercept: None,
        };
        let policy = HoldingPolicy {
            minimum_indicated_knots: 140.0,
            maximum_indicated_knots: 200.0,
            bank_degrees: 30.0,
            turn_rate_degrees_sec: 3.0,
        };
        let hold = LimitHold::at(0, &segment, &fix, &policy).unwrap();
        assert_eq!(hold.indicated_knots, 200.0);
        assert_eq!(hold.leg_seconds, 90.0);
        assert_eq!(hold.bank_degrees, 25.0);
        assert!(
            hold.ground_knots > 300.0,
            "The holding limit is IAS, not ground speed"
        );
        assert!(hold.protect(2000.0, 2000.0, |_, _| false).is_err());
        let route = Arc::new(hold.protect(2000.0, 2000.0, |_, _| true).unwrap());
        assert!(hold.matches(&route));
        let mut changed = hold.clone();
        changed.indicated_knots = 190.0;
        assert!(!changed.matches(&route));
        changed = hold.clone();
        changed.leg_seconds = 60.0;
        assert!(!changed.matches(&route));
        changed = hold.clone();
        changed.inbound_true_degrees = 180.0;
        assert!(!changed.matches(&route));
        let mut fast_pattern = hold.pattern();
        fast_pattern.ground_knots = segment.knots;
        let fast_hold = fast_pattern.compile(|_, _| true).unwrap();
        let nominal = Arc::new(
            Route::compile(vec![segment.clone(), fast_hold.clone()], 2000.0, 2000.0).unwrap(),
        );
        let mut world = World::default();
        assert!(world.insert(0, 1, nominal));
        assert!(world.ground_rates(1, 3.0, 3.0));
        assert!(world.recovery(0, 1, 0, route.clone()));
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        world.advance(0.0, false);
        let mut previous = world.snapshot()[0].clone();
        let mut entered = false;
        let mut holding_steps = 0;
        for step in 1..=2400 {
            world.advance(f64::from(step) * 0.5, false);
            let pose = world.snapshot()[0].clone();
            let travelled = distance_nm(
                [previous.position.latitude, previous.position.longitude],
                [pose.position.latitude, pose.position.longitude],
            );
            assert!(travelled < segment.knots * 0.5 / 3600.0 + 0.0001);
            assert!(!pose.complete);
            if pose.recovering {
                entered = true;
                holding_steps += 1;
                assert_eq!(pose.stage, Stage::Holding);
                assert_eq!(pose.recovery_from, Some(0));
                assert!((pose.position.altitude_feet - 35000.0).abs() < 1e-6);
                assert!((pose.knots - hold.ground_knots).abs() < 0.01);
            }
            previous = pose;
        }
        assert!(entered && holding_steps > 1000);
        let mut short = segment;
        short.points[0].longitude = 0.199;
        let mut impossible = World::default();
        assert!(impossible.insert(
            0,
            1,
            Arc::new(Route::compile(vec![short, fast_hold], 2000.0, 2000.0).unwrap())
        ));
        assert!(impossible.ground_rates(1, 3.0, 3.0));
        assert!(!impossible.recovery(0, 1, 0, route));
    }

    #[test]
    fn final_recovery_keeps_the_map_path_and_checks_a_late_forward_join() {
        let point = |longitude, altitude| Waypoint {
            latitude: 0.0,
            longitude,
            altitude_feet: altitude,
        };
        let map = point(0.0, 100.0);
        let fix = point(0.01, 1000.0);
        let hold = holding::Pattern {
            fix,
            inbound_degrees: 90.0,
            right: true,
            ground_knots: 100.0,
            leg_seconds: 60.0,
            bank_degrees: 25.0,
            turn_rate_degrees_sec: 3.0,
        }
        .compile(|_, _| true)
        .unwrap();
        let published = Route::compile(
            vec![
                Segment {
                    stage: Stage::MissedApproach,
                    points: vec![map, fix],
                    knots: 100.0,
                    resources: vec![],
                    dwell_seconds: 0.0,
                },
                hold,
            ],
            5000.0,
            3000.0,
        )
        .unwrap();
        let start = point(-0.01, 500.0);
        let final_path = vec![Segment {
            stage: Stage::Approach,
            points: vec![start, map],
            knots: 100.0,
            resources: vec![],
            dwell_seconds: 0.0,
        }];
        let early =
            from_final(start, &final_path, &published, 5000.0, 3000.0, |_, _| true).unwrap();
        assert_eq!(early.segment(0).unwrap().points[0], start);
        let at_map = *early.segment(0).unwrap().points.last().unwrap();
        assert_eq!(at_map.longitude, map.longitude);
        assert_eq!(at_map.altitude_feet, start.altitude_feet);
        assert_eq!(early.segment(1).unwrap().points[0], at_map);
        let late_start = point(0.002, 0.0);
        let late = from_final(late_start, &[], &published, 5000.0, 3000.0, |_, _| true).unwrap();
        let climb = late.segment(0).unwrap();
        assert_eq!(climb.points[0], late_start);
        assert_eq!(*climb.points.last().unwrap(), fix);
        assert!(
            climb
                .points
                .iter()
                .all(|point| point.longitude >= late_start.longitude)
        );
        assert!(from_final(late_start, &[], &published, 5000.0, 3000.0, |_, _| false).is_err());
        assert!(
            from_final(
                point(0.009, 0.0),
                &[],
                &published,
                5000.0,
                3000.0,
                |_, _| true
            )
            .is_err()
        );
        assert!(
            from_final(point(0.02, 0.0), &[], &published, 5000.0, 3000.0, |_, _| {
                true
            })
            .is_err()
        );
        assert!(
            from_final(
                Waypoint {
                    latitude: 0.01,
                    ..late_start
                },
                &[],
                &published,
                5000.0,
                3000.0,
                |_, _| true
            )
            .is_err()
        );
        assert!(
            from_final(
                Waypoint {
                    altitude_feet: f64::NAN,
                    ..start
                },
                &final_path,
                &published,
                5000.0,
                3000.0,
                |_, _| true
            )
            .is_err()
        );
        let high = point(-0.01, 2000.0);
        let high_final = vec![Segment {
            points: vec![high, map],
            ..final_path[0].clone()
        }];
        assert!(from_final(high, &high_final, &published, 5000.0, 3000.0, |_, _| true).is_err());
        let mut wrong = final_path.clone();
        wrong[0].resources.push("RUNWAY".into());
        assert!(from_final(start, &wrong, &published, 5000.0, 3000.0, |_, _| true).is_err());
    }

    #[test]
    fn published_missed_seed_requires_its_map_fix_level_and_protected_hold() {
        let record = |sequence: &str, id: &str, kind: &str, altitude: &str, description: &str| {
            let mut fields = vec![""; 38];
            fields[0] = sequence;
            fields[1] = "I";
            fields[2] = "TEST";
            fields[4] = id;
            fields[5] = "ZZ";
            fields[6] = "P";
            fields[7] = if id.starts_with("RW") { "G" } else { "C" };
            fields[8] = description;
            fields[11] = kind;
            fields[20] = "0000";
            fields[22] = "+";
            fields[23] = altitude;
            format!("APPCH:{};", fields.join(","))
        };
        let text = [
            record("010", "RW36", "CF", "00050", "G  M"),
            record("020", "RW36", "FA", "01000", "G M "),
            record("030", "HOLD", "TF", "02000", "E   "),
            record("040", "HOLD", "HM", "02000", "VE H"),
        ]
        .join("\n");
        let mut catalogue = Catalogue::parse(&text).unwrap();
        let legs = catalogue.0.values_mut().next().unwrap();
        let hold = legs.last_mut().unwrap();
        hold.turn = "R".into();
        hold.distance_time = "T010".into();
        hold.speed_knots = Some(100.0);
        hold.speed_description = "-".into();
        let airport = Airport {
            icao: "TEST".into(),
            runways: vec![Runway {
                first_name: "36".into(),
                second_name: "18".into(),
                width: 45.0,
                second: Point {
                    north: 3000.0,
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut points = Points::requested(&[("TEST", &catalogue)]);
        let root = std::env::temp_dir().join(format!(
            "openatc-missed-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let custom = root.join("Custom Data");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(
            custom.join("earth_fix.dat"),
            "I\n1200 Version - data cycle 2610\n0.1 0.0 HOLD TEST ZZ\n99\n",
        )
        .unwrap();
        std::fs::write(
            custom.join("earth_nav.dat"),
            "I\n1200 Version - data cycle 2610\n99\n",
        )
        .unwrap();
        points.load(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        let legs = catalogue.0.values().next().unwrap();
        let terminal = super::super::routing::Terminal {
            arrival_manual: None,
            direct: false,
            enroute_from: [0.0, 0.0],
            enroute_to: [0.0, 0.0],
            enroute_minimum_feet: 1000,
            departure_name: String::new(),
            departure: vec![],
            arrival_name: String::new(),
            arrival_transition: String::new(),
            arrival: vec![],
            approach_name: "TEST".into(),
            approach_transition: String::new(),
            approach: vec![legs[0].clone()],
            missed: legs[1..].to_vec(),
        };
        let weather=super::super::environment::Environment::from_sample(&airport,&(
            serde_json::json!({"airport":"TEST","latitude":0.0,"longitude":0.0,"sampleAltitudeFeet":0.0,
                "pressureHpa":1013.25,"temperatureC":15.0,"magneticVariationDegrees":0.0,"windDegrees":0.0,"windKnots":0.0}),Instant::now(),
        )).unwrap();
        let heights = [
            Threshold {
                runway: "36".into(),
                elevation_feet: 0.0,
            },
            Threshold {
                runway: "18".into(),
                elevation_feet: 0.0,
            },
        ];
        let surface = super::super::runway::Surface::new(&airport, "36", &heights, 2.0).unwrap();
        let policy =
            super::super::parse_operations(include_str!("../../../../traffic/operations.toml"))
                .unwrap();
        let handling = animation::builtin().unwrap();
        let mora = Default::default();
        let reserved = BTreeSet::new();
        let planner = super::super::airborne::Planner {
            regions: &Default::default(),
            rotation: Waypoint {
                latitude: 0.0,
                longitude: 0.0,
                altitude_feet: 50.0,
            },
            origin: &airport,
            destination: &airport,
            points: &points,
            mora: &mora,
            departure_weather: &weather,
            arrival_weather: &weather,
            aircraft: profiles::aircraft("C172").unwrap(),
            handling: &handling[&animation::Kind::Piston],
            policy: &policy,
            departure_surface: &surface,
            arrival_surface: &surface,
            observed: &[],
            reserved: &reserved,
            occupied_stand_cells: &BTreeSet::new(),
            parking_exclusions: &BTreeSet::new(),
            preference: &openatc_core::parking::TrafficPreference::default(),
            allow_direct: false,
        };
        let speed = trajectory::Airspeed {
            indicated_knots: planner.aircraft.approach_knots,
            atmosphere: weather.atmosphere,
            wind_from: 0.0,
            wind_knots: 0.0,
        };
        let map = Fix {
            coordinate: [0.0, 0.0],
            arrival_heading: Some(0.0),
            minimum_feet: 50.0,
            maximum_feet: 50.0,
            preferred_feet: 50.0,
            ground_knots: speed.ground_knots(50.0, 0.0).unwrap(),
            airspeed: Some(speed),
            required_turn: None,
            intercept: None,
        };
        let draft = prepare(&planner, &terminal, &map, 0.0).unwrap();
        assert_eq!(draft.holding_fix, "HOLD");
        assert!(draft.clone().protect(|_, _| false).is_err());
        // Geometry-only inspection; no terrain or obstacle protection is asserted.
        let route = draft.clone().protect(|_, _| true).unwrap();
        assert_eq!(route.segment(0).unwrap().stage, Stage::MissedApproach);
        assert_eq!(route.segment(0).unwrap().points[0], draft.map);
        let hold = (0..512)
            .map_while(|index| route.segment(index))
            .last()
            .unwrap();
        assert_eq!(hold.stage, Stage::Holding);
        assert_eq!(
            hold.points[0].altitude_feet,
            weather.atmosphere.geometric(2000.0, false).unwrap()
        );
        let mut repeated = terminal.clone();
        let last = repeated.missed.last_mut().unwrap();
        last.altitude_one = None;
        last.altitude_one_is_flight_level = None;
        last.altitude_description.clear();
        let repeated_draft = prepare(&planner, &repeated, &map, 0.0).unwrap();
        assert_eq!(repeated_draft.holding_feet, draft.holding_feet);
        assert_eq!(repeated_draft.holding_fix, draft.holding_fix);
        assert!(repeated_draft.protect(|_, _| false).is_err());
        assert!(repeated.missed.last().unwrap().altitude_one.is_none());
        let before_hold = repeated.missed.len() - 2;
        for change in 0..5 {
            let mut invalid = repeated.clone();
            let incoming = &mut invalid.missed[before_hold];
            match change {
                0 => incoming.identifier = "OTHER".into(),
                1 => incoming.region = "XX".into(),
                2 => incoming.altitude_description = "-".into(),
                3 => incoming.altitude_one_is_flight_level = None,
                _ => incoming.altitude_one = None,
            }
            assert!(prepare(&planner, &invalid, &map, 0.0).is_err());
        }
        let mut late_terminal = terminal.clone();
        late_terminal.missed[0].terminator = "CA".into();
        let point = planner
            .arrival_surface
            .point(planner.arrival_surface.threshold + 300.0);
        let coordinate = super::super::position(planner.destination, point);
        let late = Waypoint {
            latitude: coordinate[0],
            longitude: coordinate[1],
            altitude_feet: planner.arrival_surface.height(point),
        };
        assert!(prepare_start(&planner, &terminal, &map, 0.0, None, None, Some(late)).is_err());
        let late_draft =
            prepare_start(&planner, &late_terminal, &map, 0.0, None, None, Some(late)).unwrap();
        assert_eq!(late_draft.map, draft.map);
        assert!(late_draft.clone().protect(|_, _| false).is_err());
        let late_route = late_draft.protect(|_, _| true).unwrap();
        assert_eq!(late_route.segment(0).unwrap().points[0], late);
        assert_eq!(
            late_route
                .segment(0)
                .unwrap()
                .points
                .last()
                .unwrap()
                .altitude_feet,
            weather.atmosphere.geometric(1000.0, false).unwrap()
        );
        let wrong = Waypoint {
            longitude: late.longitude + 0.01,
            ..late
        };
        assert!(
            prepare_start(&planner, &late_terminal, &map, 0.0, None, None, Some(wrong)).is_err()
        );
        for (along, offset) in [
            (planner.arrival_surface.threshold - 1.0, 0.0),
            (planner.arrival_surface.length + 1.0, 0.0),
            (planner.arrival_surface.threshold + 300.0, 1000.0),
            (planner.arrival_surface.threshold + 300.0, -5.0),
        ] {
            let point = planner.arrival_surface.point(along);
            let coordinate = super::super::position(planner.destination, point);
            let invalid = Waypoint {
                latitude: coordinate[0],
                longitude: coordinate[1],
                altitude_feet: planner.arrival_surface.height(point) + offset,
            };
            assert!(
                prepare_start(
                    &planner,
                    &late_terminal,
                    &map,
                    0.0,
                    None,
                    None,
                    Some(invalid)
                )
                .is_err()
            );
        }
        late_terminal.missed[0].terminator = "VA".into();
        let heading =
            prepare_start(&planner, &late_terminal, &map, 0.0, None, None, Some(late)).unwrap();
        assert!(heading.protect(|_, _| false).is_err());

        let mut elevated = map.clone();
        elevated.preferred_feet = weather.atmosphere.geometric(2500.0, false).unwrap();
        let target = weather.atmosphere.geometric(3000.0, false).unwrap();
        let amended =
            prepare_with_holding_floor(&planner, &terminal, &elevated, 0.0, Some(target)).unwrap();
        assert!((amended.holding_feet - target).abs() < 1e-6);
        assert!(amended.clone().protect(|_, _| false).is_err());
        let alternate = amended.protect(|_, _| true).unwrap();
        assert_eq!(
            alternate.segment(0).unwrap().points[0].altitude_feet,
            elevated.preferred_feet
        );
        assert!(
            (0..512)
                .map_while(|index| alternate.segment(index))
                .flat_map(|segment| &segment.points)
                .all(|point| point.altitude_feet + 1e-6 >= elevated.preferred_feet)
        );
        let mut limited = terminal.clone();
        let last = limited.missed.last_mut().unwrap();
        last.altitude_description = "-".into();
        last.altitude_one = Some(2200);
        assert!(
            prepare_with_holding_floor(&planner, &limited, &elevated, 0.0, Some(target)).is_err()
        );
        let mut exact = terminal.clone();
        for leg in &mut exact.missed {
            if leg.identifier == "HOLD" {
                leg.altitude_description.clear();
                leg.altitude_one = Some(2000);
            }
        }
        assert!(
            prepare_with_holding_floor(&planner, &exact, &elevated, 0.0, Some(target)).is_err()
        );
        let instruction = super::super::flight::MissedInstruction {
            fix: "HOLD".into(),
            geometric_feet: target,
            reported_feet: 3000,
            standard: false,
        };
        let cleared = prepare_with_clearance(
            &planner,
            &exact,
            &elevated,
            0.0,
            Some(target),
            Some(&instruction),
        )
        .unwrap();
        assert!((cleared.holding_feet - target).abs() < 1.0);
        assert!(cleared.protect(|_, _| false).is_err());
        assert_eq!(exact.missed.last().unwrap().altitude_one, Some(2000));
        let mut wrong = instruction.clone();
        wrong.fix = "OTHER".into();
        assert!(
            prepare_with_clearance(&planner, &exact, &elevated, 0.0, Some(target), Some(&wrong))
                .is_err()
        );
        wrong = instruction.clone();
        wrong.reported_feet += 100;
        assert!(
            prepare_with_clearance(&planner, &exact, &elevated, 0.0, Some(target), Some(&wrong))
                .is_err()
        );
        let mut ceiling = terminal.clone();
        ceiling.missed[0].altitude_description = "-".into();
        ceiling.missed[0].altitude_one = Some(1000);
        assert!(
            prepare_with_holding_floor(&planner, &ceiling, &elevated, 0.0, Some(target)).is_err()
        );
        assert!(
            prepare_with_clearance(
                &planner,
                &ceiling,
                &elevated,
                0.0,
                Some(target),
                Some(&instruction)
            )
            .is_err()
        );
        assert!(
            prepare_with_holding_floor(&planner, &terminal, &map, 0.0, Some(f64::NAN)).is_err()
        );
        let edge = [hold.points[0], hold.points[1]];
        assert!(draft.protect(|a, b| [a, b] != edge).is_err());
        let mut early = map.clone();
        early.coordinate[0] -= 0.01;
        assert!(prepare(&planner, &terminal, &early, 0.0).is_err());
        let mut bad = terminal.clone();
        bad.missed[0].terminator = "RF".into();
        assert!(prepare(&planner, &bad, &map, 0.0).is_err());
        bad = terminal.clone();
        bad.missed.last_mut().unwrap().region = "XX".into();
        assert!(prepare(&planner, &bad, &map, 0.0).is_err());
        bad = terminal.clone();
        bad.missed.last_mut().unwrap().speed_knots = Some(planner.aircraft.approach_knots - 1.0);
        assert!(prepare(&planner, &bad, &map, 0.0).is_err());

        // Sydney-style missed approach: a climb on course to its level, with no published
        // hold. The fallback holds at the end of the climb.
        let mut climb = terminal.clone();
        let mut ca = terminal.missed.last().unwrap().clone();
        ca.identifier.clear();
        ca.terminator = "CA".into();
        ca.altitude_description = "+".into();
        ca.altitude_one = Some(3000);
        ca.altitude_one_is_flight_level = Some(false);
        ca.altitude_two = None;
        ca.altitude_two_is_flight_level = None;
        ca.course = Some(0.0);
        ca.speed_description.clear();
        ca.speed_knots = None;
        climb.missed = vec![terminal.missed[0].clone(), ca];
        let fallback = prepare(&planner, &climb, &map, 0.0).expect("fallback hold");
        assert_eq!(fallback.holding_fix, "RW36");
        assert!(
            (fallback.holding_feet - weather.atmosphere.geometric(3000.0, false).unwrap()).abs()
                < 1e-6
        );
        assert!(fallback.clone().protect(|_, _| false).is_err());
        let route = fallback.clone().protect(|_, _| true).unwrap();
        assert_eq!(route.segment(0).unwrap().stage, Stage::MissedApproach);
        let hold = (0..512)
            .map_while(|index| route.segment(index))
            .last()
            .unwrap();
        assert_eq!(hold.stage, Stage::Holding);
        assert_eq!(
            hold.points[0].altitude_feet,
            weather.atmosphere.geometric(3000.0, false).unwrap()
        );
    }

    /// Uses the installed simulator data, so it is ignored by default. Run with
    /// `cargo test -p openatc-engine -- --ignored sydney_climb`.
    #[test]
    #[ignore = "needs installed X-Plane navdata"]
    fn sydney_ils_missed_approaches_produce_a_fallback_hold() {
        use openatc_core::airport::load_airport_from_simulator;
        use std::path::Path;
        let root = std::env::var("OPENATC_SIM_ROOT")
            .unwrap_or_else(|_| "/home/bill/.local/share/Steam/steamapps/common/X-Plane 12".into());
        let root_path = Path::new(&root);
        if !root_path.join("Custom Data/CIFP/YSSY.dat").is_file() {
            eprintln!("skipping: no YSSY CIFP under {}", root_path.display());
            return;
        }
        let airport = load_airport_from_simulator(
            &root,
            "YSSY",
            &|path| Path::new(path).is_file(),
            &|path| std::fs::read_to_string(path).map_err(|error| error.to_string()),
        )
        .expect("YSSY airport");
        let catalogue = Catalogue::load(root_path, "YSSY").expect("YSSY procedures");
        let mut points = Points::requested(&[("YSSY", &catalogue)]);
        points.load(root_path).expect("YSSY fixes");
        let weather = super::super::environment::Environment::from_sample(
            &airport,
            &(
                serde_json::json!({
                    "airport": "YSSY", "latitude": airport.reference_latitude,
                    "longitude": airport.reference_longitude,
                    "sampleAltitudeFeet": airport.elevation_feet,
                    "pressureHpa": 1013.25, "temperatureC": 15.0,
                    "magneticVariationDegrees": 12.0,
                    "windDegrees": 0.0, "windKnots": 0.0
                }),
                Instant::now(),
            ),
        )
        .expect("YSSY weather");
        let heights = airport
            .runways
            .iter()
            .flat_map(|runway| {
                [
                    Threshold {
                        runway: runway.first_name.clone(),
                        elevation_feet: airport.elevation_feet,
                    },
                    Threshold {
                        runway: runway.second_name.clone(),
                        elevation_feet: airport.elevation_feet,
                    },
                ]
            })
            .collect::<Vec<_>>();
        let policy =
            super::super::parse_operations(include_str!("../../../../traffic/operations.toml"))
                .unwrap();
        let handling = animation::builtin().unwrap();
        let profiles = profiles::Profiles::builtin().unwrap();
        let mora = Default::default();
        let reserved = BTreeSet::new();
        let variation = weather.variation;
        let mut failures = Vec::new();
        for name in ["I07", "I16R", "I25", "I34L", "I34R"] {
            let approach = match catalogue.approach(name, "") {
                Ok(approach) => approach,
                Err(error) => {
                    failures.push(format!("{name}: {error}"));
                    continue;
                }
            };
            let Some(map_leg) = approach.landing.last() else {
                failures.push(format!("{name}: no missed-approach point"));
                continue;
            };
            let runway = map_leg.identifier.trim_start_matches("RW").to_owned();
            let Some(map_position) = Points::runway(map_leg, &airport) else {
                failures.push(format!("{name}: runway {runway} threshold unresolved"));
                continue;
            };
            let surface = match super::super::runway::Surface::new(&airport, &runway, &heights, 2.0)
            {
                Ok(surface) => surface,
                Err(error) => {
                    failures.push(format!("{name}: runway {runway}: {error}"));
                    continue;
                }
            };
            let Some(aircraft) = profiles::aircraft("A320") else {
                failures.push("A320 profile".into());
                break;
            };
            let planner = super::super::airborne::Planner {
                regions: &Default::default(),
                rotation: Waypoint {
                    latitude: airport.reference_latitude,
                    longitude: airport.reference_longitude,
                    altitude_feet: airport.elevation_feet,
                },
                origin: &airport,
                destination: &airport,
                points: &points,
                mora: &mora,
                departure_weather: &weather,
                arrival_weather: &weather,
                aircraft,
                handling: &handling[&animation::Kind::Jet],
                policy: &policy,
                departure_surface: &surface,
                arrival_surface: &surface,
                observed: &[],
                reserved: &reserved,
                occupied_stand_cells: &BTreeSet::new(),
                parking_exclusions: &BTreeSet::new(),
                preference: &openatc_core::parking::TrafficPreference::default(),
                allow_direct: false,
            };
            let heading = map_leg
                .course
                .map_or(0.0, |course| (course + variation).rem_euclid(360.0));
            let speed = trajectory::Airspeed {
                indicated_knots: planner.aircraft.approach_knots,
                atmosphere: weather.atmosphere,
                wind_from: 0.0,
                wind_knots: 0.0,
            };
            let map = Fix {
                coordinate: map_position.coordinate,
                arrival_heading: Some(heading),
                minimum_feet: airport.elevation_feet,
                maximum_feet: airport.elevation_feet,
                preferred_feet: airport.elevation_feet,
                ground_knots: speed.ground_knots(airport.elevation_feet, heading).unwrap(),
                airspeed: Some(speed),
                required_turn: None,
                intercept: None,
            };
            let terminal = super::super::routing::Terminal {
                arrival_manual: None,
                direct: false,
                enroute_from: [0.0, 0.0],
                enroute_to: [0.0, 0.0],
                enroute_minimum_feet: 1000,
                departure_name: String::new(),
                departure: vec![],
                arrival_name: String::new(),
                arrival_transition: String::new(),
                arrival: vec![],
                approach_name: name.into(),
                approach_transition: String::new(),
                approach: approach.landing.to_vec(),
                missed: approach.missed.to_vec(),
            };
            match prepare(&planner, &terminal, &map, 0.0) {
                Ok(draft) => {
                    println!(
                        "{name} runway {runway}: hold={} level={:.0}",
                        draft.holding_fix, draft.holding_feet
                    );
                    if let Err(error) = draft.protect(|_, _| true) {
                        failures.push(format!("{name}: {error}"));
                    }
                }
                Err(error) => failures.push(format!("{name}: {error}")),
            }
        }
        assert!(failures.is_empty(), "{failures:?}");
    }
    /// Exercises the fallback across installed airports. Ignored by default; run with
    /// `cargo test -p openatc-engine -- --ignored other_airports --nocapture`.
    #[test]
    #[ignore = "needs installed X-Plane navdata"]
    fn other_airports_climb_missed_approaches_produce_a_fallback_hold() {
        use openatc_core::airport::load_airport_from_simulator;
        use std::path::Path;
        let root = std::env::var("OPENATC_SIM_ROOT")
            .unwrap_or_else(|_| "/home/bill/.local/share/Steam/steamapps/common/X-Plane 12".into());
        let root_path = Path::new(&root);
        let airports = [
            "YSSY", "YMML", "YBBN", "YSCB", "YPPH", "NZAA", "LIMC", "LFMN", "EGLL", "LFPG", "KLAX",
            "KJFK", "KSEA", "KORD", "VHHH", "RJTT", "WSSS", "OMDB",
        ];
        let policy =
            super::super::parse_operations(include_str!("../../../../traffic/operations.toml"))
                .unwrap();
        let handling = animation::builtin().unwrap();
        let profiles = profiles::Profiles::builtin().unwrap();
        let mora = Default::default();
        let reserved = BTreeSet::new();
        let mut failures = Vec::new();
        let mut tested = 0;
        for icao in airports {
            let airport = match load_airport_from_simulator(
                &root,
                icao,
                &|path| Path::new(path).is_file(),
                &|path| std::fs::read_to_string(path).map_err(|error| error.to_string()),
            ) {
                Ok(airport) => airport,
                Err(error) => {
                    println!("{icao}: skipped: {error}");
                    continue;
                }
            };
            let catalogue = match Catalogue::load(root_path, icao) {
                Ok(catalogue) if !catalogue.0.is_empty() => catalogue,
                Ok(_) => {
                    println!("{icao}: skipped: no procedures");
                    continue;
                }
                Err(error) => {
                    println!("{icao}: skipped: {error}");
                    continue;
                }
            };
            let mut points = Points::requested(&[(icao, &catalogue)]);
            if let Err(error) = points.load(root_path) {
                println!("{icao}: skipped: {error}");
                continue;
            }
            let weather = match super::super::environment::Environment::from_sample(
                &airport,
                &(
                    serde_json::json!({
                        "airport": icao, "latitude": airport.reference_latitude,
                        "longitude": airport.reference_longitude,
                        "sampleAltitudeFeet": airport.elevation_feet,
                        "pressureHpa": 1013.25, "temperatureC": 15.0,
                        "magneticVariationDegrees": 0.0,
                        "windDegrees": 0.0, "windKnots": 0.0
                    }),
                    Instant::now(),
                ),
            ) {
                Ok(weather) => weather,
                Err(error) => {
                    failures.push(format!("{icao}: weather: {error}"));
                    continue;
                }
            };
            let heights = airport
                .runways
                .iter()
                .flat_map(|runway| {
                    [
                        Threshold {
                            runway: runway.first_name.clone(),
                            elevation_feet: airport.elevation_feet,
                        },
                        Threshold {
                            runway: runway.second_name.clone(),
                            elevation_feet: airport.elevation_feet,
                        },
                    ]
                })
                .collect::<Vec<_>>();
            let names: Vec<String> = catalogue
                .0
                .keys()
                .filter(|key| {
                    key.kind == "APPCH"
                        && key.transition.is_empty()
                        && key.route_type != "A"
                        && key.route_type != "Z"
                        && key.name.starts_with('I')
                })
                .map(|key| key.name.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            for name in names.into_iter().take(4) {
                let Ok(approach) = catalogue.approach(&name, "") else {
                    continue;
                };
                let Some(map_leg) = approach.landing.last() else {
                    continue;
                };
                if !map_leg.missed_point() {
                    continue;
                }
                let fallback = approach
                    .missed
                    .last()
                    .is_some_and(|leg| matches!(leg.terminator.as_str(), "CA" | "DF" | "VM"))
                    && !approach.missed.iter().any(|leg| leg.terminator == "HM");
                if !fallback {
                    continue;
                }
                let runway = map_leg.identifier.trim_start_matches("RW").to_owned();
                let Some(map_position) = Points::runway(map_leg, &airport) else {
                    failures.push(format!(
                        "{icao} {name}: runway {runway} threshold unresolved"
                    ));
                    continue;
                };
                let surface =
                    match super::super::runway::Surface::new(&airport, &runway, &heights, 2.0) {
                        Ok(surface) => surface,
                        Err(error) => {
                            failures.push(format!("{icao} {name}: runway {runway}: {error}"));
                            continue;
                        }
                    };
                let Some(aircraft) = profiles::aircraft("A320") else {
                    failures.push("A320 profile".into());
                    break;
                };
                let planner = super::super::airborne::Planner {
                    regions: &Default::default(),
                    rotation: Waypoint {
                        latitude: airport.reference_latitude,
                        longitude: airport.reference_longitude,
                        altitude_feet: airport.elevation_feet,
                    },
                    origin: &airport,
                    destination: &airport,
                    points: &points,
                    mora: &mora,
                    departure_weather: &weather,
                    arrival_weather: &weather,
                    aircraft,
                    handling: &handling[&animation::Kind::Jet],
                    policy: &policy,
                    departure_surface: &surface,
                    arrival_surface: &surface,
                    observed: &[],
                    reserved: &reserved,
                    occupied_stand_cells: &BTreeSet::new(),
                    parking_exclusions: &BTreeSet::new(),
                    preference: &openatc_core::parking::TrafficPreference::default(),
                    allow_direct: false,
                };
                let heading = map_leg
                    .course
                    .map_or(0.0, |course| course.rem_euclid(360.0));
                let speed = trajectory::Airspeed {
                    indicated_knots: planner.aircraft.approach_knots,
                    atmosphere: weather.atmosphere,
                    wind_from: 0.0,
                    wind_knots: 0.0,
                };
                let map = Fix {
                    coordinate: map_position.coordinate,
                    arrival_heading: Some(heading),
                    minimum_feet: airport.elevation_feet,
                    maximum_feet: airport.elevation_feet,
                    preferred_feet: airport.elevation_feet,
                    ground_knots: speed.ground_knots(airport.elevation_feet, heading).unwrap(),
                    airspeed: Some(speed),
                    required_turn: None,
                    intercept: None,
                };
                let terminal = super::super::routing::Terminal {
                    arrival_manual: None,
                    direct: false,
                    enroute_from: [0.0, 0.0],
                    enroute_to: [0.0, 0.0],
                    enroute_minimum_feet: 1000,
                    departure_name: String::new(),
                    departure: vec![],
                    arrival_name: String::new(),
                    arrival_transition: String::new(),
                    arrival: vec![],
                    approach_name: name.clone(),
                    approach_transition: String::new(),
                    approach: approach.landing.to_vec(),
                    missed: approach.missed.to_vec(),
                };
                match prepare(&planner, &terminal, &map, 0.0) {
                    Ok(draft) => {
                        tested += 1;
                        println!(
                            "{icao} {name} runway {runway}: hold={} level={:.0}",
                            draft.holding_fix, draft.holding_feet
                        );
                        if let Err(error) = draft.protect(|_, _| true) {
                            failures.push(format!("{icao} {name}: {error}"));
                        }
                    }
                    Err(error) if error.contains("uses unsupported") => {
                        println!("{icao} {name}: skipped: {error}");
                    }
                    Err(error) => failures.push(format!("{icao} {name}: {error}")),
                }
            }
        }
        println!("tested {tested} fallback approaches across the installed airports");
        assert!(
            tested >= 15,
            "only {tested} fallback approaches were exercised"
        );
        assert!(failures.is_empty(), "{failures:?}");
    }
}
