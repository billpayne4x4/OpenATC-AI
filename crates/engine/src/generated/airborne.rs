//! Solve terminal and en-route crossings together before creating movement limits.

#![allow(clippy::default_trait_access)]
use openatc_core::traffic::{
    motion::{Segment, Stage, Waypoint},
    trajectory::{self, Envelope, Fix},
};

/// Aircraft-specific published inputs. They are not a flyable trajectory or a clearance.
#[derive(Clone)]
pub(super) struct Decoded {
    pub candidate: usize,
    pub nominal: Option<Draft>,
    pub missed: Option<super::recovery::Draft>,
    pub final_missed: std::collections::BTreeMap<usize, std::sync::Arc<super::recovery::Draft>>,
    pub missed_instruction: Option<super::flight::MissedInstruction>,
    pub amendment: Option<super::flight::RouteAmendment>,
    pub departure: Vec<Fix>,
    pub enroute: Vec<Fix>,
    pub arrival: Vec<Fix>,
    pub approach: Vec<Fix>,
}

pub(super) struct Request<'a> {
    pub candidate: usize,
    pub terminal: &'a super::routing::Terminal,
    pub rotation: Waypoint,
    pub origin: &'a openatc_core::airport::Airport,
    pub destination: &'a openatc_core::airport::Airport,
    pub points: &'a openatc_core::traffic::coordinates::Points,
    pub mora: &'a openatc_core::navigation::Mora,
    pub enroute_segment_nm: f64,
    pub departure_weather: &'a super::environment::Environment,
    pub arrival_weather: &'a super::environment::Environment,
    pub aircraft: &'a openatc_core::traffic::profiles::Aircraft,
    pub handling: &'a openatc_core::traffic::animation::Handling,
    pub intercept_limit_nm: f64,
    pub departure_floor_feet: f64,
    pub arrival_floor_feet: f64,
}

/// Decode the selected fleet's actual SID, STAR and approach on the preparation worker.
pub(super) fn resolve(input: &Request<'_>) -> Result<Decoded, String> {
    use openatc_core::traffic::resolved::{self, Context};
    if !input.departure_weather.fresh() || !input.arrival_weather.fresh() {
        return Err("Terminal planning needs fresh weather at both airports".into());
    }
    if ![input.departure_floor_feet, input.arrival_floor_feet]
        .iter()
        .all(|height| height.is_finite() && (-3000.0..=20000.0).contains(height))
    {
        return Err("Terminal planning has an invalid aerodrome vertical bound".into());
    }
    if !input.aircraft.rnav {
        return Err("Generated terminal planning needs supported navigation equipment".into());
    }
    let departure_ceiling = input
        .departure_weather
        .atmosphere
        .geometric(input.aircraft.ceiling, true)?;
    let arrival_ceiling = input
        .arrival_weather
        .atmosphere
        .geometric(input.aircraft.ceiling, true)?;
    let floor = f64::from(input.terminal.enroute_minimum_feet);
    if floor <= 0.0
        || input.departure_weather.atmosphere.geometric(floor, false)? > departure_ceiling
        || input.arrival_weather.atmosphere.geometric(floor, false)? > arrival_ceiling
    {
        return Err("Terminal connection exceeds this aircraft's ceiling".into());
    }
    let context =
        |airport, weather: &super::environment::Environment, floor, ceiling, speed| Context {
            airport,
            points: input.points,
            atmosphere: weather.atmosphere,
            variation: weather.variation,
            wind_from: weather.wind_from,
            wind_knots: weather.wind_knots,
            indicated_knots: speed,
            maximum_intercept_nm: input.intercept_limit_nm,
            climb_fpm: input.aircraft.climb_fpm,
            minimum_feet: floor,
            maximum_feet: ceiling,
            preferred_feet: ceiling,
        };
    let mut departure = resolved::boundary_crossings(
        input.rotation,
        &input.terminal.departure,
        &context(
            input.origin,
            input.departure_weather,
            input.departure_floor_feet,
            departure_ceiling,
            input
                .handling
                .rotation_knots
                .max(input.aircraft.approach_knots),
        ),
    )?;
    let end = departure.last().ok_or("Resolved departure is empty")?;
    if openatc_core::arrival::distance_nm(end.coordinate, input.terminal.enroute_from) >= 0.001 {
        return Err("Resolved SID does not reach its selected enroute connection".into());
    }
    let arrival_context = context(
        input.destination,
        input.arrival_weather,
        input.arrival_floor_feet,
        arrival_ceiling,
        input.aircraft.approach_knots,
    );
    let (mut arrival, manual) = resolved::amended_crossings(
        Waypoint {
            latitude: input.terminal.enroute_to[0],
            longitude: input.terminal.enroute_to[1],
            altitude_feet: arrival_ceiling,
        },
        &input.terminal.arrival,
        &arrival_context,
    )?;
    let entry = arrival.last().ok_or("Resolved arrival is empty")?.clone();
    // A manually terminated arrival never reaches the approach entry, so the approach is
    // decoded from its own published entry and the bridge is only flown under an amendment.
    let amended_target = if manual.is_none() {
        None
    } else {
        let leg = input
            .terminal
            .approach
            .first()
            .ok_or("Approach has no published entry leg")?;
        let position = openatc_core::traffic::coordinates::Points::runway(leg, input.destination)
            .or_else(|| input.points.resolve(leg, &input.destination.icao))
            .ok_or("Amended arrival has no resolvable approach entry fix")?;
        Some((leg, position.coordinate))
    };
    let mut approach = resolved::boundary_crossings(
        Waypoint {
            latitude: amended_target.map_or(entry.coordinate[0], |(_, target)| target[0]),
            longitude: amended_target.map_or(entry.coordinate[1], |(_, target)| target[1]),
            altitude_feet: entry.preferred_feet,
        },
        &input.terminal.approach,
        &arrival_context,
    )?;
    let first = approach.first().ok_or("Resolved approach is empty")?;
    if manual.is_none()
        && openatc_core::arrival::distance_nm(entry.coordinate, first.coordinate) >= 0.001
    {
        return Err("Resolved STAR and approach no longer share their published entry".into());
    }
    // Keep the published band across the amended bridge: the amendment changes the lateral
    // path only, so every retained crossing restriction stays in force until the approach's
    // own restrictions apply at the amended target.
    let amendment = match manual {
        None => None,
        Some(manual) => {
            let target = first.coordinate;
            if openatc_core::arrival::distance_nm(entry.coordinate, target) < 0.001 {
                return Err("Amended arrival does not reach its published approach entry".into());
            }
            arrival.push(Fix {
                intercept: None,
                airspeed: entry.airspeed,
                coordinate: target,
                arrival_heading: first.arrival_heading,
                minimum_feet: entry.minimum_feet,
                maximum_feet: entry.maximum_feet,
                preferred_feet: entry.preferred_feet,
                ground_knots: entry.ground_knots,
                required_turn: first.required_turn.or(entry.required_turn),
            });
            Some(super::flight::RouteAmendment {
                procedure: input.terminal.arrival_name.clone(),
                transition: input.terminal.arrival_transition.clone(),
                terminator: manual.terminator,
                manual_fix: manual.identifier,
                target: amended_target
                    .map(|(leg, _)| leg.identifier.clone())
                    .unwrap_or_default(),
                approach: input.terminal.approach_name.clone(),
                target_coordinate: target,
            })
        }
    };
    prefer_descent(&mut arrival, &mut approach);
    let budget = 128usize.saturating_sub(departure.len() + arrival.len() + approach.len());
    let enroute = super::enroute::crossings(super::enroute::Request {
        terminal: input.terminal,
        departure: &mut departure,
        arrival: &arrival,
        mora: input.mora,
        departure_weather: input.departure_weather,
        arrival_weather: input.arrival_weather,
        aircraft: input.aircraft,
        spacing_nm: input.enroute_segment_nm,
        budget,
    })?;
    if departure.len() + enroute.len() + arrival.len() + approach.len() > 128 {
        return Err(
            "Aircraft terminal crossings exceed the combined flight planning budget".into(),
        );
    }
    if !input.departure_weather.fresh() || !input.arrival_weather.fresh() {
        return Err("Terminal weather expired during preparation".into());
    }
    Ok(Decoded {
        candidate: input.candidate,
        nominal: None,
        missed: None,
        final_missed: Default::default(),
        missed_instruction: None,
        amendment,
        departure,
        enroute,
        arrival,
        approach,
    })
}

/// Plan toward the approach instead of preferring cruise height at every unrestricted arrival fix.
/// These are solver preferences; published bounds and subsequent clearance/protection checks remain authoritative.
fn prefer_descent(arrival: &mut [Fix], approach: &mut [Fix]) {
    let Some(last) = approach.last() else {
        return;
    };
    // The final approach leg frequently publishes no altitude of its own, so anchoring on its
    // preference holds the aircraft at its ceiling through the whole arrival and inflates every
    // turn radius. The binding published floor at the end of the approach is the real target;
    // the joint level and turn solver still raises the level wherever that descent is not
    // achievable, and no published limit is changed here.
    let floor = approach
        .iter()
        .map(|fix| fix.minimum_feet)
        .fold(f64::INFINITY, f64::min);
    let mut next = last.preferred_feet.min(floor);
    for fix in arrival.iter_mut().chain(approach.iter_mut()).rev() {
        next = next.clamp(fix.minimum_feet, fix.maximum_feet);
        fix.preferred_feet = next;
    }
}

/// Aircraft-specific production planning context; candidate attempts have no world side effects.
pub(super) struct Planner<'a> {
    pub regions: &'a std::collections::BTreeMap<String, openatc_core::regions::Region>,
    pub rotation: Waypoint,
    pub origin: &'a openatc_core::airport::Airport,
    pub destination: &'a openatc_core::airport::Airport,
    pub points: &'a openatc_core::traffic::coordinates::Points,
    pub mora: &'a openatc_core::navigation::Mora,
    pub departure_weather: &'a super::environment::Environment,
    pub arrival_weather: &'a super::environment::Environment,
    pub aircraft: &'a openatc_core::traffic::profiles::Aircraft,
    pub handling: &'a openatc_core::traffic::animation::Handling,
    pub policy: &'a super::Operations,
    pub departure_surface: &'a super::runway::Surface,
    pub arrival_surface: &'a super::runway::Surface,
    pub observed: &'a [openatc_core::state::TrafficTarget],
    pub reserved: &'a std::collections::BTreeSet<String>,
    pub occupied_stand_cells: &'a std::collections::BTreeSet<String>,
    pub parking_exclusions: &'a std::collections::BTreeSet<String>,
    pub preference: &'a openatc_core::parking::TrafficPreference,
    /// A category with no usable published arrival may join the approach directly.
    pub allow_direct: bool,
}
impl Planner<'_> {
    /// Select only after the published crossing connects through landing and parking.
    /// A decodable approach alone is insufficient to consume a flight's candidate slot.
    #[allow(clippy::float_cmp)]
    pub fn arrival(
        &self,
        candidates: &[super::routing::Terminal],
        usage: &std::collections::BTreeMap<usize, usize>,
    ) -> Result<(Decoded, super::landing::Arrival), String> {
        let departure_floor_feet = self.origin.elevation_feet.min(
            self.departure_weather.atmosphere.geometric(
                self.departure_surface.height(
                    self.departure_surface
                        .point(self.departure_surface.threshold),
                ),
                false,
            )?,
        );
        let arrival_floor_feet = self.destination.elevation_feet.min(
            self.arrival_weather.atmosphere.geometric(
                self.arrival_surface
                    .height(self.arrival_surface.point(self.arrival_surface.threshold)),
                false,
            )?,
        );
        let mut reason = "No terminal candidate is available".to_owned();
        let mut attempted = 0;
        let mut first_reason = None;
        // Terminal variants often share the same final crossing and runway exit.
        // Reuse identical landing searches only within this immutable aircraft/weather/stand request.
        let mut landings: Vec<(
            Waypoint,
            f64,
            trajectory::Airspeed,
            Result<super::landing::Arrival, String>,
        )> = Vec::new();
        let mut order = (0..candidates.len())
            .filter(|index| self.allow_direct || !candidates[*index].direct)
            .collect::<Vec<_>>();
        order.sort_by_key(|index| {
            (
                // Try published STARs first; the direct join is a fallback and should not
                // use up the candidate budget ahead of them.
                candidates[*index].direct,
                usage.get(index).copied().unwrap_or(0),
                *index,
            )
        });
        order.truncate(self.policy.terminal_candidate_limit);
        for candidate in order {
            let terminal = &candidates[candidate];
            attempted += 1;
            let attempt = (|| {
                let mut decoded = resolve(&Request {
                    candidate,
                    terminal,
                    rotation: self.rotation,
                    origin: self.origin,
                    destination: self.destination,
                    points: self.points,
                    mora: self.mora,
                    enroute_segment_nm: self.policy.enroute_segment_nm,
                    departure_weather: self.departure_weather,
                    arrival_weather: self.arrival_weather,
                    aircraft: self.aircraft,
                    handling: self.handling,
                    intercept_limit_nm: self.policy.terminal_intercept_limit_nm,
                    departure_floor_feet,
                    arrival_floor_feet,
                })?;
                let crossing = decoded
                    .approach
                    .last()
                    .ok_or("Approach has no threshold crossing")?;
                let angle = terminal
                    .approach
                    .last()
                    .and_then(|leg| leg.vertical_angle_degrees)
                    .ok_or("Approach has no published final descent angle")?;
                let threshold = Waypoint {
                    latitude: crossing.coordinate[0],
                    longitude: crossing.coordinate[1],
                    altitude_feet: crossing.preferred_feet,
                };
                let airspeed = crossing.airspeed.ok_or("Approach airspeed is unresolved")?;
                let arrival = if let Some((_, _, _, result)) =
                    landings.iter().find(|(point, descent, speed, _)| {
                        *point == threshold && *descent == angle && *speed == airspeed
                    }) {
                    result.clone()?
                } else {
                    let result = super::landing::prepare(&super::landing::Request {
                        airport: self.destination,
                        surface: self.arrival_surface,
                        crossing: threshold,
                        angle_degrees: angle,
                        airspeed,
                        aircraft: self.aircraft,
                        handling: self.handling,
                        policy: self.policy,
                        observed: self.observed,
                        reserved: self.reserved,
                        occupied_stand_cells: self.occupied_stand_cells,
                        parking_exclusions: self.parking_exclusions,
                        preference: self.preference,
                    });
                    landings.push((threshold, angle, airspeed, result.clone()));
                    result?
                };
                let departure_region =
                    openatc_core::regions::region_for(self.regions, &self.origin.icao);
                let arrival_region =
                    openatc_core::regions::region_for(self.regions, &self.destination.icao);
                decoded.nominal = Some(draft_scheduled(
                    self.rotation,
                    self.departure_surface.heading(),
                    self.arrival_surface.heading(),
                    vec![
                        Section {
                            stage: Stage::Climb,
                            crossings: decoded.departure.clone(),
                        },
                        Section {
                            stage: Stage::Cruise,
                            crossings: decoded.enroute.clone(),
                        },
                        Section {
                            stage: Stage::Descent,
                            crossings: decoded.arrival.clone(),
                        },
                        Section {
                            stage: Stage::Approach,
                            crossings: decoded.approach.clone(),
                        },
                    ],
                    &Envelope {
                        bank_degrees: self.handling.maximum_bank_degrees,
                        turn_rate_degrees_sec: self.policy.airborne_turn_rate_degrees_sec,
                        climb_fpm: self.aircraft.climb_fpm,
                        descent_fpm: self.aircraft.descent_fpm,
                        spacing_meters: self.policy.airborne_sample_meters,
                        maximum_detour_meters: self.policy.airborne_detour_meters,
                    },
                    Some((&departure_region, &arrival_region)),
                    Some((self.mora, self.departure_weather, self.arrival_weather)),
                )?);
                decoded.nominal = Some(decoded.nominal.take().unwrap().limit_scopes(
                    900.0
                        - self.policy.airborne_prediction_seconds
                        - self.policy.flight_clearance_lead_seconds,
                    &departure_region,
                    &arrival_region,
                )?);
                decoded.missed = Some(super::recovery::prepare(
                    self,
                    terminal,
                    decoded.approach.last().ok_or("Approach MAP is missing")?,
                    arrival_floor_feet,
                )?);
                let late = *arrival
                    .flare
                    .points
                    .last()
                    .ok_or("Flare has no late go-around position")?;
                let (variants, instruction) = super::recovery::final_variants(
                    self,
                    terminal,
                    decoded.approach.last().unwrap(),
                    arrival_floor_feet,
                    decoded.nominal.as_ref().unwrap(),
                    decoded.missed.as_ref().unwrap(),
                    late,
                )?;
                decoded.final_missed = variants;
                decoded.missed_instruction = instruction;
                if !self.departure_weather.fresh() || !self.arrival_weather.fresh() {
                    return Err("Terminal weather expired before arrival selection".to_owned());
                }
                Ok((decoded, arrival))
            })();
            match attempt {
                Ok(prepared) => return Ok(prepared),
                Err(error) => {
                    reason = format!(
                        "{}/{}/{}: {error}",
                        terminal.departure_name, terminal.arrival_name, terminal.approach_name
                    );
                    if first_reason.is_none() {
                        first_reason = Some(reason.clone());
                    }
                }
            }
        }
        if let Some(first) = first_reason
            && first != reason
        {
            reason = format!("{first}; last candidate: {reason}");
        }
        Err(format!(
            "No complete arrival among {attempted} terminal candidates: {reason}"
        ))
    }
}

#[derive(Clone)]
pub(super) struct Section {
    pub stage: Stage,
    pub crossings: Vec<Fix>,
}

/// Joint solved levels and geometry awaiting verified corridor protection.
#[derive(Clone)]
pub(super) struct Draft {
    enroute_checked: bool,
    path: trajectory::Draft,
    stages: Vec<Stage>,
    crossings: Vec<Fix>,
}
impl Draft {
    fn limit_scopes(
        self,
        seconds: f64,
        departure: &openatc_core::regions::Region,
        arrival: &openatc_core::regions::Region,
    ) -> Result<Self, String> {
        let stages = self.stages;
        let crossings = self.crossings;
        let (path, source) = self.path.split_limits(
            seconds,
            |index| stages[index] != Stage::Approach,
            |index, lower, upper, preferred| {
                let region = if matches!(stages[index], Stage::Climb | Stage::Cruise) {
                    departure
                } else {
                    arrival
                };
                super::controller::select_level(&crossings[index], region, lower, upper, preferred)
            },
        )?;
        let crossings = path.crossings().to_vec();
        Ok(Self {
            path,
            crossings,
            stages: source.into_iter().map(|index| stages[index]).collect(),
            enroute_checked: self.enroute_checked,
        })
    }
    #[allow(dead_code)]
    pub fn enroute_checked(&self) -> bool {
        self.enroute_checked
    }

    pub fn approach_levels(&self) -> Vec<(usize, f64, f64)> {
        self.stages
            .iter()
            .enumerate()
            .filter_map(|(index, stage)| {
                if *stage != Stage::Approach {
                    return None;
                }
                self.path
                    .level_span(index)
                    .map(|(start, end)| (index, start, end))
            })
            .collect()
    }

    pub fn requirements(&self) -> &[Fix] {
        &self.crossings
    }
    pub fn leg_count(&self) -> usize {
        self.path.leg_count()
    }
    pub fn protect(
        self,
        protected: impl Fn(Waypoint, Waypoint) -> bool,
    ) -> Result<Vec<Segment>, String> {
        Ok(self
            .path
            .protect(protected)?
            .into_iter()
            .zip(self.stages)
            .map(|(leg, stage)| Segment {
                stage,
                points: leg.points,
                knots: leg.ground_knots,
                resources: Vec::new(),
                dwell_seconds: 0.0,
            })
            .collect())
    }
}

#[allow(dead_code)]
pub(super) fn compile(
    rotation: Waypoint,
    departure_heading: f64,
    runway_heading: f64,
    sections: Vec<Section>,
    envelope: &Envelope,
    protected: impl Fn(Waypoint, Waypoint) -> bool,
) -> Result<Vec<Segment>, String> {
    draft(
        rotation,
        departure_heading,
        runway_heading,
        sections,
        envelope,
    )?
    .protect(protected)
}

#[allow(dead_code)]
pub(super) fn draft(
    rotation: Waypoint,
    departure_heading: f64,
    runway_heading: f64,
    sections: Vec<Section>,
    envelope: &Envelope,
) -> Result<Draft, String> {
    draft_scheduled(
        rotation,
        departure_heading,
        runway_heading,
        sections,
        envelope,
        None,
        None,
    )
}

fn draft_scheduled(
    rotation: Waypoint,
    departure_heading: f64,
    runway_heading: f64,
    sections: Vec<Section>,
    envelope: &Envelope,
    regions: Option<(
        &openatc_core::regions::Region,
        &openatc_core::regions::Region,
    )>,
    terrain: Option<(
        &openatc_core::navigation::Mora,
        &super::environment::Environment,
        &super::environment::Environment,
    )>,
) -> Result<Draft, String> {
    if sections.len() != 4
        || sections.iter().map(|section| section.stage).ne([
            Stage::Climb,
            Stage::Cruise,
            Stage::Descent,
            Stage::Approach,
        ])
        || sections
            .iter()
            .enumerate()
            .any(|(index, section)| index != 1 && section.crossings.is_empty())
        || sections[1].crossings.is_empty()
            && !sections[0]
                .crossings
                .last()
                .zip(sections[2].crossings.first())
                .is_some_and(|(last, first)| {
                    openatc_core::arrival::distance_nm(last.coordinate, first.coordinate) < 0.001
                })
    {
        return Err(
            "Airborne plan needs ordered departure, en-route, arrival and approach sections".into(),
        );
    }
    let count: usize = sections.iter().map(|section| section.crossings.len()).sum();
    if count > 128 {
        return Err("Combined airborne crossings exceed the planning budget".into());
    }
    let mut stages = Vec::with_capacity(count);
    let mut crossings: Vec<Fix> = Vec::with_capacity(count);
    for section in sections {
        for crossing in section.crossings {
            if let Some(previous) = crossings.last_mut()
                && openatc_core::arrival::distance_nm(previous.coordinate, crossing.coordinate)
                    < 0.001
            {
                merge(previous, &crossing)?;
            } else {
                stages.push(section.stage);
                crossings.push(crossing);
            }
        }
    }
    for _ in 0..12 {
        let path = trajectory::draft_selected(
            rotation,
            departure_heading,
            &crossings,
            runway_heading,
            envelope,
            |index, lower, upper, preferred| {
                let Some((departure, arrival)) = regions else {
                    return Some(preferred.clamp(lower, upper));
                };
                if stages[index] == Stage::Approach {
                    // The approach clearance retains published vertical restrictions;
                    // its sampled crossings are not separate assigned flight levels.
                    return Some(preferred.clamp(lower, upper));
                }
                let region = if matches!(stages[index], Stage::Climb | Stage::Cruise) {
                    departure
                } else {
                    arrival
                };
                super::controller::select_level(&crossings[index], region, lower, upper, preferred)
            },
        )?;
        crossings = path.crossings().to_vec();
        let Some((mora, departure_weather, arrival_weather)) = terrain else {
            return Ok(Draft {
                path,
                stages,
                crossings,
                enroute_checked: false,
            });
        };
        if !departure_weather.fresh() || !arrival_weather.fresh() {
            return Err("Enroute protection weather expired".into());
        }
        let floors = path.minimum_levels(|index, from, to| {
            if stages[index] != Stage::Cruise {
                return Ok(None);
            }
            let published = mora
                .corridor([from.latitude, from.longitude], [to.latitude, to.longitude])
                .filter(|floor| *floor > 0)
                .ok_or("Curved enroute corridor has unknown grid MORA")?;
            let published = f64::from(published);
            let floor = published
                .max(departure_weather.atmosphere.geometric(published, false)?)
                .max(arrival_weather.atmosphere.geometric(published, false)?);
            Ok((from.altitude_feet.min(to.altitude_feet) + 1e-6 < floor).then_some(floor))
        })?;
        if floors.iter().all(Option::is_none) {
            return Ok(Draft {
                path,
                stages,
                crossings,
                enroute_checked: true,
            });
        }
        for (index, floor) in floors.into_iter().enumerate() {
            let Some(floor) = floor else {
                continue;
            };
            if index == 0 {
                return Err("Enroute minimum conflicts with the fixed departure position".into());
            }
            for crossing in &mut crossings[index - 1..=index] {
                crossing.minimum_feet = crossing.minimum_feet.max(floor);
                if crossing.minimum_feet > crossing.maximum_feet {
                    return Err(
                        "Curved enroute MORA conflicts with a published crossing ceiling".into(),
                    );
                }
            }
        }
    }
    Err("Curved enroute level and corridor planning did not converge".into())
}

pub(super) fn merge(previous: &mut Fix, next: &Fix) -> Result<(), String> {
    if previous.intercept.is_some() || next.intercept.is_some() {
        return Err("Coincident course intercept needs a separate movement limit".into());
    }
    let minimum = previous.minimum_feet.max(next.minimum_feet);
    let maximum = previous.maximum_feet.min(next.maximum_feet);
    if minimum > maximum {
        return Err("Coincident flight sections have conflicting crossing levels".into());
    }
    if previous
        .arrival_heading
        .zip(next.arrival_heading)
        .is_some_and(|(a, b)| openatc_core::navigation::turn(a, b).abs() > 1.0)
        || previous
            .required_turn
            .zip(next.required_turn)
            .is_some_and(|(a, b)| a != b)
    {
        return Err("Coincident flight sections have conflicting courses or turns".into());
    }
    // Do not discard a minimum-speed restriction by selecting the slower of two joins.
    // A speed change at the same point needs an upstream, aircraft-specific deceleration plan.
    // The two sections meet at one point but are planned from their own airport samples, so
    // only the intended speed has to agree; the joined crossing keeps the sample of the leg
    // it terminates, and the arrival crossings after it keep theirs.
    match (previous.airspeed, next.airspeed) {
        (Some(a), Some(b)) => {
            if (a.indicated_knots - b.indicated_knots).abs() > 1e-6 {
                return Err(
                    "Coincident flight sections change airspeed without a deceleration plan".into(),
                );
            }
        }
        (None, None) if (previous.ground_knots - next.ground_knots).abs() <= 1e-6 => {}
        (Some(_), None) | (None, Some(_)) => {
            return Err(
                "Coincident flight sections disagree on whether the path has an airspeed".into(),
            );
        }
        (None, None) => {
            return Err("Coincident flight sections have incompatible ground speeds".into());
        }
    }
    previous.minimum_feet = minimum;
    previous.maximum_feet = maximum;
    previous.preferred_feet = next.preferred_feet.clamp(minimum, maximum);
    previous.arrival_heading = previous.arrival_heading.or(next.arrival_heading);
    previous.required_turn = previous.required_turn.or(next.required_turn);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn coincident_sections_join_on_speed_intent_and_still_refuse_a_real_speed_change() {
        let atmosphere = |pressure| {
            openatc_core::traffic::air::Atmosphere::surface(pressure, 15.0, 0.0)
                .expect("surface atmosphere")
        };
        let crossing = |pressure, indicated, ground| Fix {
            coordinate: [36.0, -86.0],
            arrival_heading: Some(270.0),
            minimum_feet: 2000.0,
            maximum_feet: 20000.0,
            preferred_feet: 6000.0,
            ground_knots: ground,
            airspeed: Some(openatc_core::traffic::trajectory::Airspeed {
                indicated_knots: indicated,
                atmosphere: atmosphere(pressure),
                wind_from: 0.0,
                wind_knots: 0.0,
            }),
            required_turn: None,
            intercept: None,
        };
        // Each section is planned from its own airport sample, so the join almost never sees
        // identical weather. That is not a speed change and must not refuse the connection.
        let mut departure = crossing(1013.25, 250.0, 260.0);
        let arrival = crossing(1010.0, 250.0, 258.0);
        assert!(merge(&mut departure, &arrival).is_ok());
        assert_eq!(departure.minimum_feet, 2000.0);
        assert!(departure.airspeed.is_some());
        // A genuine speed change at the same point still needs an upstream deceleration plan.
        let mut departure = crossing(1013.25, 250.0, 260.0);
        let faster = crossing(1010.0, 280.0, 290.0);
        assert!(
            merge(&mut departure, &faster)
                .unwrap_err()
                .contains("change airspeed without a deceleration plan")
        );
        // One section having a real airspeed and the other none is not a joinable pair.
        let mut departure = crossing(1013.25, 250.0, 260.0);
        let mut fixed = crossing(1010.0, 250.0, 258.0);
        fixed.airspeed = None;
        assert!(merge(&mut departure, &fixed).is_err());
    }
    #[test]
    fn arrival_preferences_follow_the_approach_without_changing_published_limits() {
        let fix = |minimum, maximum, preferred| Fix {
            coordinate: [1.0, 2.0],
            arrival_heading: Some(90.0),
            minimum_feet: minimum,
            maximum_feet: maximum,
            preferred_feet: preferred,
            ground_knots: 150.0,
            airspeed: None,
            required_turn: None,
            intercept: None,
        };
        let mut arrival = vec![fix(8000.0, 42000.0, 42000.0), fix(3000.0, 10000.0, 10000.0)];
        let mut approach = vec![fix(3000.0, 3000.0, 3000.0), fix(50.0, 50.0, 50.0)];
        let before = arrival.iter().chain(&approach).cloned().collect::<Vec<_>>();
        prefer_descent(&mut arrival, &mut approach);
        assert_eq!(
            arrival
                .iter()
                .chain(&approach)
                .map(|fix| fix.preferred_feet)
                .collect::<Vec<_>>(),
            vec![8000.0, 3000.0, 3000.0, 50.0]
        );
        for (old, new) in before.iter().zip(arrival.iter().chain(&approach)) {
            assert_eq!(
                (old.minimum_feet, old.maximum_feet),
                (new.minimum_feet, new.maximum_feet)
            );
            assert_eq!(old.coordinate, new.coordinate);
            assert_eq!(old.ground_knots, new.ground_knots);
            assert_eq!(old.arrival_heading, new.arrival_heading);
        }
        // A lower published band remains intact even when the next fix requires a climb.
        arrival[1].maximum_feet = 2000.0;
        arrival[1].minimum_feet = 2000.0;
        prefer_descent(&mut arrival, &mut approach);
        assert_eq!(arrival[1].preferred_feet, 2000.0);
        assert_eq!(approach[0].preferred_feet, 3000.0);
    }

    #[test]
    fn fleet_terminal_inputs_keep_restrictions_and_lower_runway_thresholds() {
        use openatc_core::{
            airport::{Airport, Runway},
            state::Point,
            traffic::{coordinates::Points, procedures::Catalogue},
        };
        let leg = |kind: &str, runway: &str, altitude: &str| {
            let mut fields = vec![""; 38];
            fields[0] = "010";
            fields[1] = "1";
            fields[2] = "TEST";
            fields[11] = kind;
            fields[20] = "0000";
            fields[22] = " ";
            fields[23] = altitude;
            let mut leg = Catalogue::parse(&format!("SID:{};", fields.join(",")))
                .unwrap()
                .0
                .into_values()
                .next()
                .unwrap()
                .remove(0);
            leg.identifier = format!("RW{runway}");
            leg.section = "P".into();
            leg.subsection = "G".into();
            leg
        };
        let origin = Airport {
            icao: "ORIG".into(),
            runways: vec![Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                width: 45.0,
                first: Point {
                    north: 1000.0,
                    ..Default::default()
                },
                second: Point {
                    north: 1500.0,
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        let destination = Airport {
            icao: "DEST".into(),
            reference_latitude: 1.0,
            elevation_feet: 100.0,
            ..origin.clone()
        };
        let weather = |airport: &Airport| {
            super::super::environment::Environment::from_sample(airport,&(
            serde_json::json!({"airport":airport.icao,"latitude":airport.reference_latitude,"longitude":airport.reference_longitude,
                "sampleAltitudeFeet":airport.elevation_feet,"pressureHpa":1013.25,"temperatureC":15.0,
                "magneticVariationDegrees":0.0,"windDegrees":0.0,"windKnots":0.0}),std::time::Instant::now())).unwrap()
        };
        let departure_weather = weather(&origin);
        let arrival_weather = weather(&destination);
        let departure = leg("TF", "09", "02000");
        let arrival = leg("IF", "09", "04000");
        let final_fix = leg("TF", "27", "00050");
        let points = Points::default();
        let mora = openatc_core::navigation::Mora(
            [(0, 0), (1, 0)]
                .into_iter()
                .map(|cell| (cell, 1000))
                .collect(),
        );
        let terminal = super::super::routing::Terminal {
            arrival_manual: None,
            direct: false,
            enroute_from: Points::runway(&departure, &origin).unwrap().coordinate,
            enroute_to: Points::runway(&arrival, &destination).unwrap().coordinate,
            enroute_minimum_feet: 1000,
            departure_name: "TEST".into(),
            departure: vec![departure],
            arrival_name: "TEST".into(),
            arrival_transition: String::new(),
            arrival: vec![arrival.clone()],
            approach_name: "TEST".into(),
            approach_transition: String::new(),
            approach: vec![arrival, final_fix],
            missed: vec![],
        };
        let aircraft = openatc_core::traffic::profiles::aircraft("C172")
            .unwrap()
            .clone();
        let handling = &openatc_core::traffic::animation::builtin().unwrap()
            [&openatc_core::traffic::animation::Kind::Piston];
        let request = |aircraft, terminal, floor| Request {
            candidate: 0,
            terminal,
            rotation: Waypoint {
                latitude: 0.0,
                longitude: 0.0,
                altitude_feet: 0.0,
            },
            origin: &origin,
            destination: &destination,
            points: &points,
            mora: &mora,
            enroute_segment_nm: 20.0,
            departure_weather: &departure_weather,
            arrival_weather: &arrival_weather,
            aircraft,
            handling,
            intercept_limit_nm: 20.0,
            departure_floor_feet: 0.0,
            arrival_floor_feet: floor,
        };
        assert!(
            resolve(&request(&aircraft, &terminal, destination.elevation_feet)).is_err(),
            "Airport maximum elevation is above this runway's approach crossing"
        );
        let decoded = resolve(&request(&aircraft, &terminal, 0.0)).unwrap();
        let expected = arrival_weather.atmosphere.geometric(4000.0, false).unwrap();
        assert_eq!(decoded.arrival.last().unwrap().maximum_feet, expected);
        assert_eq!(decoded.approach.first().unwrap().maximum_feet, expected);
        assert_eq!(
            decoded
                .approach
                .last()
                .unwrap()
                .airspeed
                .unwrap()
                .indicated_knots,
            aircraft.approach_knots
        );
        let mut too_low = aircraft.clone();
        too_low.ceiling = 500.0;
        assert!(
            resolve(&request(&too_low, &terminal, 0.0))
                .err()
                .unwrap()
                .contains("ceiling")
        );
        let mut unsupported = terminal.clone();
        unsupported.departure[0].terminator = "RF".into();
        assert!(resolve(&request(&aircraft, &unsupported, 0.0)).is_err());
    }

    fn inputs() -> (Waypoint, Vec<Section>, Envelope) {
        let start = Waypoint {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 0.0,
        };
        let sections = [
            (Stage::Climb, 0.1, 0.0, 6000.0, 6000.0),
            (Stage::Cruise, 0.2, 0.0, 6000.0, 6000.0),
            (Stage::Descent, 0.3, 0.0, 1000.0, 1000.0),
            (Stage::Approach, 0.4, 0.0, 0.0, 0.0),
        ]
        .into_iter()
        .map(
            |(stage, latitude, minimum_feet, maximum_feet, preferred_feet)| Section {
                stage,
                crossings: vec![Fix {
                    airspeed: None,
                    coordinate: [latitude, 0.0],
                    arrival_heading: Some(0.0),
                    minimum_feet,
                    maximum_feet,
                    preferred_feet,
                    ground_knots: 120.0,
                    required_turn: None,
                    intercept: None,
                }],
            },
        )
        .collect();
        (
            start,
            sections,
            Envelope {
                bank_degrees: 20.0,
                turn_rate_degrees_sec: 3.0,
                climb_fpm: 2000.0,
                descent_fpm: 1000.0,
                spacing_meters: 250.0,
                maximum_detour_meters: 100.0,
            },
        )
    }

    #[test]
    fn shared_sid_star_boundary_needs_no_invented_cruise_leg() {
        let (start, mut sections, envelope) = inputs();
        sections[1].crossings.clear();
        let boundary = sections[0].crossings[0].clone();
        sections[2].crossings.insert(0, boundary);
        let segments = compile(start, 0.0, 0.0, sections.clone(), &envelope, |_, _| true).unwrap();
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.stage)
                .collect::<Vec<_>>(),
            vec![Stage::Climb, Stage::Descent, Stage::Approach]
        );
        let mut conflicting = sections.clone();
        conflicting[2].crossings[0].minimum_feet = 7000.0;
        assert!(draft(start, 0.0, 0.0, conflicting, &envelope).is_err());
        let mut different_speed = sections.clone();
        different_speed[2].crossings[0].ground_knots += 1.0;
        assert!(draft(start, 0.0, 0.0, different_speed, &envelope).is_err());
        sections[2].crossings[0].coordinate[0] += 0.01;
        assert!(draft(start, 0.0, 0.0, sections, &envelope).is_err());
    }

    #[test]
    fn actual_enroute_edges_raise_the_profile_without_relaxing_published_ceilings() {
        use openatc_core::{airport::Airport, navigation::Mora};
        let airport = Airport {
            icao: "TEST".into(),
            ..Default::default()
        };
        let weather = super::super::environment::Environment::from_sample(&airport, &(
            serde_json::json!({"airport":"TEST","latitude":0.0,"longitude":0.0,"sampleAltitudeFeet":0.0,
                "pressureHpa":1013.25,"temperatureC":15.0,"magneticVariationDegrees":0.0,
                "windDegrees":0.0,"windKnots":0.0}), std::time::Instant::now())).unwrap();
        let data = || {
            let (start, mut sections, envelope) = inputs();
            sections[0].crossings[0].preferred_feet = 3000.0;
            sections[1].crossings[0].preferred_feet = 3000.0;
            sections[2].crossings[0].coordinate[0] = 0.5;
            sections[3].crossings[0].coordinate[0] = 0.6;
            (start, sections, envelope)
        };
        let mora = Mora(
            (-1..=1)
                .flat_map(|latitude| (-1..=1).map(move |longitude| ((latitude, longitude), 5000)))
                .collect(),
        );
        let (start, sections, envelope) = data();
        let draft = draft_scheduled(
            start,
            0.0,
            0.0,
            sections,
            &envelope,
            None,
            Some((&mora, &weather, &weather)),
        )
        .unwrap();
        assert!(draft.enroute_checked());
        assert!(draft.requirements()[0].minimum_feet >= 5000.0);
        assert!(draft.requirements()[1].minimum_feet >= 5000.0);
        assert_eq!(draft.requirements()[0].maximum_feet, 6000.0);
        assert_eq!(draft.requirements()[2].maximum_feet, 1000.0);
        assert!(
            draft.clone().protect(|_, _| false).is_err(),
            "An enroute check cannot release terminal geometry"
        );
        let segments = draft.protect(|_, _| true).unwrap();
        assert!(
            segments[1]
                .points
                .iter()
                .all(|point| point.altitude_feet >= 5000.0)
        );
        let (start, mut sections, envelope) = data();
        sections[0].crossings[0].maximum_feet = 4000.0;
        assert!(
            draft_scheduled(
                start,
                0.0,
                0.0,
                sections,
                &envelope,
                None,
                Some((&mora, &weather, &weather))
            )
            .err()
            .unwrap()
            .contains("ceiling")
        );
        let (start, sections, envelope) = data();
        assert!(
            draft_scheduled(
                start,
                0.0,
                0.0,
                sections,
                &envelope,
                None,
                Some((&Mora::default(), &weather, &weather))
            )
            .is_err()
        );
    }

    #[test]
    fn arrival_ceiling_shapes_the_preceding_enroute_section() {
        let (start, sections, envelope) = inputs();
        let segments = compile(start, 0.0, 0.0, sections, &envelope, |a, b| {
            a.longitude.abs() < 0.001 && b.longitude.abs() < 0.001
        })
        .unwrap();
        assert_eq!(segments.len(), 4);
        let touchdown = *segments.last().unwrap().points.last().unwrap();
        let vacated = Waypoint {
            latitude: touchdown.latitude + 0.01,
            ..touchdown
        };
        let parked = Waypoint {
            longitude: touchdown.longitude + 0.01,
            ..vacated
        };
        let mut complete = segments.clone();
        complete.extend([
            Segment {
                stage: Stage::Rollout,
                points: vec![touchdown, vacated],
                knots: 40.0,
                resources: vec!["RUNWAY/TEST/01/19".into()],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::TaxiIn,
                points: vec![vacated, parked],
                knots: 5.0,
                resources: vec![],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::Turnaround,
                points: vec![parked],
                knots: 0.0,
                resources: vec![],
                dwell_seconds: 120.0,
            },
        ]);
        openatc_core::traffic::motion::Route::compile(
            complete,
            envelope.climb_fpm,
            envelope.descent_fpm,
        )
        .unwrap();

        let cruise_end = segments[1].points.last().unwrap().altitude_feet;
        assert!(
            cruise_end < 6000.0,
            "Cruise retained an unreachable later ceiling"
        );
        assert!(segments[2].points.last().unwrap().altitude_feet <= 1000.0);
        assert_eq!(segments[3].points.last().unwrap().altitude_feet, 0.0);
        assert!(
            segments
                .windows(2)
                .all(|pair| pair[0].points.last() == pair[1].points.first())
        );
    }

    #[test]
    fn a_repeated_approach_entry_limits_the_incoming_star_leg() {
        let (start, mut sections, envelope) = inputs();
        let mut initial = sections[2].crossings[0].clone();
        initial.maximum_feet = 500.0;
        initial.preferred_feet = 500.0;
        sections[3].crossings.insert(0, initial);
        let segments = compile(start, 0.0, 0.0, sections, &envelope, |_, _| true).unwrap();
        assert_eq!(segments.len(), 4);
        assert_eq!(segments[2].points.last().unwrap().altitude_feet, 500.0);
        let (start, mut sections, envelope) = inputs();
        let mut initial = sections[2].crossings[0].clone();
        initial.ground_knots = 80.0;
        sections[3].crossings.insert(0, initial);
        assert!(compile(start, 0.0, 0.0, sections, &envelope, |_, _| true).is_err());
    }

    #[test]
    fn compatible_ias_survives_a_lower_star_approach_join() {
        use openatc_core::traffic::{air::Atmosphere, trajectory::Airspeed};
        let (start, mut sections, envelope) = inputs();
        let speed = Airspeed {
            indicated_knots: 120.0,
            atmosphere: Atmosphere::surface(1013.25, 15.0, 0.0).unwrap(),
            wind_from: 0.0,
            wind_knots: 10.0,
        };
        sections[2].crossings[0].airspeed = Some(speed);
        sections[2].crossings[0].ground_knots = speed.ground_knots(1000.0, 0.0).unwrap();
        let mut entry = sections[2].crossings[0].clone();
        entry.maximum_feet = 500.0;
        entry.preferred_feet = 500.0;
        entry.ground_knots = speed.ground_knots(500.0, 0.0).unwrap();
        sections[3].crossings.insert(0, entry);
        let path = compile(start, 0.0, 0.0, sections, &envelope, |a, b| {
            a.longitude.abs() < 0.001 && b.longitude.abs() < 0.001
        })
        .unwrap();
        assert_eq!(path[2].points.last().unwrap().altitude_feet, 500.0);
        assert!((path[2].knots - speed.ground_knots(500.0, 0.0).unwrap()).abs() < 0.01);
    }
    #[test]
    fn unprotected_or_incomplete_flights_do_not_create_movement() {
        let (start, sections, envelope) = inputs();
        assert!(
            compile(start, 0.0, 0.0, sections, &envelope, |_, b| b.latitude
                < 0.25)
            .is_err()
        );
        let (start, mut sections, envelope) = inputs();
        sections.pop();
        assert!(compile(start, 0.0, 0.0, sections, &envelope, |_, _| true).is_err());
    }
}
