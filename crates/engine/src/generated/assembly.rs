//! Join production departure, airborne and arrival plans before installing controller authority.
#[allow(unused_imports, clippy::wildcard_imports)]
use super::*;
use super::{Arc, BTreeMap, Operations, airborne, animation, departure, flight, landing};
use openatc_core::traffic::motion::{Route, Segment, Stage, Waypoint};

#[derive(Clone)]
pub(super) struct Draft {
    controller: Arc<super::controller::Flight>,
    departure: departure::Surface,
    airborne: airborne::Draft,
    missed: super::recovery::Draft,
    missed_variants: BTreeMap<usize, Arc<super::recovery::Draft>>,
    arrival: Arc<landing::Arrival>,
    airport: String,
    runway: String,
    climb_fpm: f64,
    descent_fpm: f64,
    acceleration: f64,
    braking: f64,
    holding: super::recovery::HoldingPolicy,
    obstacle_radius_meters: f64,
}

pub(super) struct Checked {
    controller: Option<Arc<super::controller::Flight>>,
    route: Arc<Route>,
    reports: BTreeMap<usize, flight::RunwayReport>,
    acceleration: f64,
    braking: f64,
    holding: Option<super::recovery::HoldingPolicy>,
    missed: Arc<Route>,
    missed_variants: BTreeMap<usize, Arc<super::recovery::Draft>>,
    map_index: usize,
    climb_fpm: f64,
    descent_fpm: f64,
    requirements: Vec<openatc_core::traffic::trajectory::Fix>,
    airspeeds: BTreeMap<usize, openatc_core::traffic::trajectory::Airspeed>,
    surface_start: usize,
    arrival_orders: Vec<(usize, openatc_core::state::TaxiClearance)>,
}

impl Draft {
    #[cfg(test)]
    pub fn origin(&self) -> &Airport {
        &self.controller.origin
    }
    pub fn weather_fresh(&self) -> bool {
        self.controller.departure_weather.fresh() && self.controller.arrival_weather.fresh()
    }
    /// Use installed navigation and physical scenery; unresolved low-level obstacles refuse release.
    pub fn production(
        self,
        terrain: &std::cell::RefCell<super::scenery::Terrain>,
        mora: &openatc_core::navigation::Mora,
    ) -> Result<flight::Prepared, String> {
        terrain.borrow_mut().begin_check();
        let origin = self.controller.departure_weather.clone();
        let destination = self.controller.arrival_weather.clone();
        let obstacle_radius = self.obstacle_radius_meters;
        let reason = std::cell::RefCell::new(None);
        let unresolved_detail = |terrain: &super::scenery::Terrain| {
            let classes = terrain.unresolved_classes();
            if classes.is_empty() {
                "object, polygon or network".to_owned()
            } else {
                classes
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        };
        let protected = |a: Waypoint, b: Waypoint| {
            // Duplicate route points carry no corridor and must not reject the flight.
            if (a.latitude - b.latitude).abs() < 1e-9 && (a.longitude - b.longitude).abs() < 1e-9 {
                return true;
            }
            if !origin.fresh() || !destination.fresh() {
                *reason.borrow_mut() =
                    Some("Weather expired during corridor protection".to_owned());
                return false;
            }
            let coordinates = |p: Waypoint| [p.latitude, p.longitude];
            if let Some(floor) = mora.corridor(coordinates(a), coordinates(b)) {
                let floor = f64::from(floor);
                let reference = origin.atmosphere.geometric(floor, false).and_then(|x| {
                    destination
                        .atmosphere
                        .geometric(floor, false)
                        .map(|y| x.max(y).max(floor))
                });
                if reference.is_ok_and(|floor| a.altitude_feet >= floor && b.altitude_feet >= floor)
                {
                    return true;
                }
            }
            let terrain_result = terrain.borrow_mut().terrain_clear(a, b, 0.0);
            let unresolved = {
                let terrain = terrain.borrow();
                terrain
                    .has_unresolved_obstacles()
                    .then(|| unresolved_detail(&terrain))
            };
            if let Some(detail) = unresolved {
                *reason.borrow_mut() = Some(format!(
                    "Low-level scenery corridor contains unresolved {detail} obstacles"
                ));
                return false;
            }
            match terrain_result {
                Ok(true) => {
                    let obstacles = terrain.borrow_mut().obstacles_clear(a, b, obstacle_radius);
                    match obstacles {
                        Ok(clear) => {
                            if !clear {
                                let unresolved = {
                                    let terrain = terrain.borrow();
                                    terrain
                                        .has_unresolved_obstacles()
                                        .then(|| unresolved_detail(&terrain))
                                };
                                if let Some(detail) = unresolved {
                                    *reason.borrow_mut() = Some(format!(
                                        "Low-level scenery corridor contains unresolved {detail} obstacles"
                                    ));
                                } else {
                                    *reason.borrow_mut() = Some(format!(
                                        "Physical obstacle intersects the route from {:.5}/{:.5} at {:.0} ft to {:.5}/{:.5} at {:.0} ft (aircraft radius {:.1} m)",
                                        a.latitude,
                                        a.longitude,
                                        a.altitude_feet,
                                        b.latitude,
                                        b.longitude,
                                        b.altitude_feet,
                                        obstacle_radius
                                    ));
                                }
                            }
                            clear
                        }
                        Err(error) => {
                            *reason.borrow_mut() = Some(error);
                            false
                        }
                    }
                }
                Ok(false) => {
                    *reason.borrow_mut() = Some(format!(
                        "Physical scenery intersects the route from {:.5}/{:.5} at {:.0} ft to {:.5}/{:.5} at {:.0} ft",
                        a.latitude,
                        a.longitude,
                        a.altitude_feet,
                        b.latitude,
                        b.longitude,
                        b.altitude_feet
                    ));
                    false
                }
                Err(error) => {
                    *reason.borrow_mut() = Some(error);
                    false
                }
            }
        };
        let checked = self
            .protect(protected)
            .map_err(|error| reason.borrow().clone().unwrap_or(error))?;
        let result = checked
            .finish_all_production(protected)
            .map_err(|error| reason.borrow().clone().unwrap_or(error))?;
        let unresolved = {
            let terrain = terrain.borrow();
            terrain
                .has_unresolved_obstacles()
                .then(|| unresolved_detail(&terrain))
        };
        if let Some(detail) = unresolved {
            return Err(format!(
                "Low-level scenery corridor contains unresolved {detail} obstacles"
            ));
        }
        Ok(result)
    }

    // The draft joins separately planned surface, airborne, arrival and controller products.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        departure: &departure::Surface,
        decoded: &airborne::Decoded,
        arrival: Arc<landing::Arrival>,
        airport: &str,
        runway: &str,
        aircraft: &openatc_core::traffic::profiles::Aircraft,
        handling: &animation::Handling,
        policy: &Operations,
        controller: Arc<super::controller::Flight>,
    ) -> Result<Self, String> {
        if departure.segments.len() != 2
            || departure.segments[0].stage != Stage::LineUp
            || departure.segments[1].stage != Stage::Takeoff
            || arrival.flare.stage != Stage::Approach
            || airport.trim().is_empty()
            || runway.trim().is_empty()
        {
            return Err(
                "Complete flight needs separate departure limits and a known arrival runway".into(),
            );
        }
        Ok(Self {
            obstacle_radius_meters: aircraft.wingspan.max(aircraft.length) / 2.0,
            controller,
            missed_variants: decoded.final_missed.clone(),
            departure: departure.clone(),
            airborne: decoded
                .nominal
                .clone()
                .ok_or("Nominal airborne plan is missing")?,
            missed: decoded
                .missed
                .clone()
                .ok_or("Published missed approach is missing")?,
            arrival,
            airport: airport.into(),
            runway: runway.into(),
            climb_fpm: aircraft.climb_fpm,
            descent_fpm: aircraft.descent_fpm,
            acceleration: handling.acceleration_knots_sec,
            braking: handling.braking_knots_sec,
            holding: super::recovery::HoldingPolicy {
                minimum_indicated_knots: aircraft.approach_knots,
                maximum_indicated_knots: policy.contingency_holding_maximum_ias,
                bank_degrees: handling.maximum_bank_degrees,
                turn_rate_degrees_sec: policy.holding_turn_rate_degrees_sec,
            },
        })
    }

    pub fn protect(
        self,
        protected: impl Fn(Waypoint, Waypoint) -> bool,
    ) -> Result<Checked, String> {
        let requirements = self.airborne.requirements().to_vec();
        let mut airspeeds = requirements
            .iter()
            .enumerate()
            .filter_map(|(index, fix)| {
                fix.airspeed
                    .map(|speed| (index + self.departure.segments.len(), speed))
            })
            .collect::<BTreeMap<_, _>>();
        if let Some(speed) = requirements.last().and_then(|fix| fix.airspeed) {
            airspeeds.insert(self.departure.segments.len() + requirements.len(), speed);
        }
        let airborne = self.airborne.protect(&protected)?;
        let map_index = self.departure.segments.len() + airborne.len() - 1;
        let missed = Arc::new(self.missed.protect(&protected)?);
        if self
            .arrival
            .flare
            .points
            .windows(2)
            .any(|pair| !protected(pair[0], pair[1]))
        {
            return Err("Generated flare lacks verified corridor protection".into());
        }
        let mut segments = self.departure.segments;
        segments.extend(airborne);
        segments.push(self.arrival.flare.clone());
        segments.extend(self.arrival.parking.exit.rollout.iter().cloned());
        let surface_start = segments.len();
        segments.extend(
            (0..512).map_while(|index| self.arrival.parking.surface.route.segment(index).cloned()),
        );
        if segments.len() > 512 {
            return Err("Complete generated flight exceeds the controller movement budget".into());
        }
        let mut reports = BTreeMap::new();
        for (index, pair) in segments.windows(2).enumerate() {
            if pair[0].stage == Stage::Rollout && pair[1].stage == Stage::TaxiIn {
                reports.insert(
                    index + 1,
                    flight::RunwayReport {
                        airport: self.airport.clone(),
                        runway: self.runway.clone(),
                    },
                );
            }
            if pair[0].stage == Stage::Crossing {
                let local = index
                    .checked_sub(surface_start)
                    .ok_or("Runway crossing is outside the arrival taxi route")?;
                let order = self
                    .arrival
                    .parking
                    .surface
                    .orders
                    .iter()
                    .find(|(segment, _)| *segment == local)
                    .map(|(_, order)| order)
                    .filter(|order| !order.crossing_runway.is_empty())
                    .ok_or("Arrival crossing needs its actual runway instruction")?;
                reports.insert(
                    index + 1,
                    flight::RunwayReport {
                        airport: order.airport.clone(),
                        runway: order.crossing_runway.clone(),
                    },
                );
            }
        }
        let route = Arc::new(Route::compile(segments, self.climb_fpm, self.descent_fpm)?);
        route.validate_rates(self.acceleration, self.braking)?;
        Ok(Checked {
            airspeeds,
            controller: Some(self.controller),
            route,
            reports,
            acceleration: self.acceleration,
            braking: self.braking,
            holding: Some(self.holding),
            missed,
            missed_variants: self
                .missed_variants
                .into_iter()
                .map(|(index, seed)| (index + 2, seed))
                .collect(),
            map_index,
            climb_fpm: self.climb_fpm,
            descent_fpm: self.descent_fpm,
            requirements,
            surface_start,
            arrival_orders: self.arrival.parking.surface.orders.clone(),
        })
    }
}

impl Checked {
    #[allow(dead_code)]
    pub fn production_contracts(&self) -> Result<BTreeMap<usize, flight::Clearance>, String> {
        let controller = self
            .controller
            .as_ref()
            .ok_or("Production flight has no retained controller input")?;
        self.controller_contracts(&controller.context())
    }

    pub fn finish_production(
        self,
        recovery: impl Fn(usize, &Segment) -> Result<Arc<Route>, String>,
        protected: impl Fn(Waypoint, Waypoint) -> bool,
    ) -> Result<flight::Prepared, String> {
        let controller = self
            .controller
            .clone()
            .ok_or("Production flight has no retained controller input")?;
        self.finish_controlled(&controller.context(), recovery, protected)
    }

    pub fn controller_contracts(
        &self,
        context: &super::controller::Context<'_>,
    ) -> Result<BTreeMap<usize, flight::Clearance>, String> {
        let mut contracts = super::controller::contracts(
            &self.route,
            self.map_index,
            &self.requirements,
            self.surface_start,
            &self.arrival_orders,
            context,
        )?;
        if self.holding.is_some() {
            super::controller::holding_contracts(
                &self.route,
                &self.requirements,
                &self.holding_limits()?,
                context,
                &mut contracts,
            )?;
        }
        Ok(contracts)
    }

    fn holding_limits(&self) -> Result<BTreeMap<usize, super::recovery::LimitHold>, String> {
        let policy = self
            .holding
            .as_ref()
            .ok_or("Flight has no holding envelope")?;
        let mut limits = BTreeMap::new();
        for index in 2..=self.map_index {
            let segment = self.route.segment(index).ok_or("Missing airborne limit")?;
            if !matches!(segment.stage, Stage::Climb | Stage::Cruise | Stage::Descent) {
                continue;
            }
            let fix = self
                .requirements
                .get(index - 2)
                .ok_or("Holding limit has no IAS reference")?;
            limits.insert(
                index,
                super::recovery::LimitHold::at(index, segment, fix, policy)?,
            );
        }
        Ok(limits)
    }

    /// Construct every airborne fallback before releasing a production flight.
    pub fn all_recoveries(
        &self,
        protected: impl Fn(Waypoint, Waypoint) -> bool,
    ) -> Result<BTreeMap<usize, Arc<Route>>, String> {
        let mut routes = self
            .final_recoveries(&protected)
            .into_iter()
            .map(|(index, result)| result.map(|route| (index, route)))
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let mut probe = openatc_core::traffic::motion::World::default();
        if !probe.insert(0, 1, self.route.clone())
            || !probe.ground_rates(1, self.acceleration, self.braking)
        {
            return Err("Holding construction could not register the nominal flight".into());
        }
        for (index, hold) in self.holding_limits()? {
            let route = Arc::new(hold.protect(self.climb_fpm, self.descent_fpm, &protected)?);
            if !hold.matches(&route) || !probe.recovery(0, 1, index, route.clone()) {
                return Err(format!(
                    "Holding recovery cannot meet the speed or entry at movement {index}"
                ));
            }
            routes.insert(index, route);
        }
        Ok(routes)
    }

    pub fn finish_all_production(
        self,
        protected: impl Fn(Waypoint, Waypoint) -> bool,
    ) -> Result<flight::Prepared, String> {
        let routes = self.all_recoveries(&protected)?;
        self.finish_production(
            |index, _| {
                routes
                    .get(&index)
                    .cloned()
                    .ok_or_else(|| format!("Missing production recovery {index}"))
            },
            protected,
        )
    }

    pub fn finish_controlled(
        self,
        context: &super::controller::Context<'_>,
        recovery: impl Fn(usize, &Segment) -> Result<Arc<Route>, String>,
        protected: impl Fn(Waypoint, Waypoint) -> bool,
    ) -> Result<flight::Prepared, String> {
        let contracts = self.controller_contracts(context)?;
        let mut plan = self.finish(
            |index, _| {
                contracts
                    .get(&index)
                    .cloned()
                    .ok_or_else(|| format!("Missing constructed controller contract {index}"))
            },
            recovery,
            protected,
        )?;
        plan.amendment = context.amendment.cloned();
        Ok(plan)
    }

    pub fn final_recoveries(
        &self,
        protected: impl Fn(Waypoint, Waypoint) -> bool,
    ) -> BTreeMap<usize, Result<Arc<Route>, String>> {
        let mut result = BTreeMap::new();
        for index in 0..=self.map_index + 1 {
            let Some(segment) = self
                .route
                .segment(index)
                .filter(|segment| segment.stage == Stage::Approach)
            else {
                continue;
            };
            let start = *segment.points.last().unwrap();
            let remaining = if index < self.map_index {
                (index + 1..=self.map_index)
                    .map(|next| self.route.segment(next).unwrap().clone())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let checked = (|| {
                let seed = if self.missed_variants.is_empty() {
                    self.missed.clone()
                } else {
                    Arc::new(
                        self.missed_variants
                            .get(&index)
                            .ok_or("Final recovery variant is missing")?
                            .as_ref()
                            .clone()
                            .protect(&protected)?,
                    )
                };
                let route = Arc::new(super::recovery::from_final(
                    start,
                    &remaining,
                    &seed,
                    self.climb_fpm,
                    self.descent_fpm,
                    &protected,
                )?);
                let mut probe = openatc_core::traffic::motion::World::default();
                if !probe.insert(0, 1, self.route.clone())
                    || !probe.ground_rates(1, self.acceleration, self.braking)
                    || !probe.recovery(0, 1, index, route.clone())
                {
                    return Err(format!("Final recovery cannot join movement {index}"));
                }
                Ok(route)
            })();
            result.insert(index, checked);
        }
        result
    }

    /// Controller and recovery providers see only a completely checked nominal route.
    pub fn finish(
        self,
        clearance: impl Fn(usize, &Segment) -> Result<flight::Clearance, String>,
        recovery: impl Fn(usize, &Segment) -> Result<Arc<Route>, String>,
        protected: impl Fn(Waypoint, Waypoint) -> bool,
    ) -> Result<flight::Prepared, String> {
        let mut clearances = BTreeMap::new();
        let mut recoveries = BTreeMap::new();
        for index in 0..512 {
            let Some(segment) = self.route.segment(index) else {
                break;
            };
            if segment.stage != Stage::Turnaround {
                clearances.insert(index, clearance(index, segment)?);
            }
            if !segment.stage.on_ground() && segment.stage != Stage::Holding {
                let route = recovery(index, segment)?;
                for step in 0..512 {
                    let Some(segment) = route.segment(step) else {
                        break;
                    };
                    if segment
                        .points
                        .windows(2)
                        .any(|pair| !protected(pair[0], pair[1]))
                    {
                        return Err(format!(
                            "Unprotected generated recovery at movement {index}"
                        ));
                    }
                }
                recoveries.insert(index, route);
            }
        }
        let mut plan = flight::Prepared {
            spacing_floor: self.controller.as_ref().map(|c| {
                c.terminal
                    .arrival
                    .iter()
                    .chain(&c.terminal.approach)
                    .filter(|leg| leg.speed_description == "+")
                    .filter_map(|leg| leg.speed_knots)
                    .fold(0.0, f64::max)
            }),
            airspeeds: self.airspeeds,
            route: self.route,
            clearances,
            recovery: recoveries,
            vacated_reports: self.reports,
            amendment: None,
        };
        if let Some(expected) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.missed.as_ref())
        {
            for index in 0..=self.map_index {
                if plan
                    .route
                    .segment(index)
                    .is_some_and(|segment| segment.stage == Stage::Approach)
                    && plan
                        .clearances
                        .get(&index)
                        .and_then(|clearance| clearance.missed.as_ref())
                        != Some(expected)
                {
                    return Err(
                        "Final recovery amendment is missing from its approach clearance".into(),
                    );
                }
            }
        }
        plan.validate()?;
        if plan
            .clearances
            .get(&(self.map_index + 1))
            .is_none_or(|clearance| clearance.landing_rollout.is_none())
        {
            return Err(
                "Complete flight needs a landing clearance before its flare and rollout".into(),
            );
        }
        let mut probe = openatc_core::traffic::motion::World::default();
        if !probe.insert(0, 1, plan.route.clone()) {
            return Err("Complete generated flight could not be checked".into());
        }
        if !probe.ground_rates(1, self.acceleration, self.braking) {
            return Err("Generated performance envelope is invalid".into());
        }
        for (&index, recovery) in &plan.recovery {
            if !probe.recovery(0, 1, index, recovery.clone()) {
                return Err(format!("Invalid generated recovery at movement {index}"));
            }
            if let Some(controller) = self.controller.as_ref() {
                let clearance = plan
                    .clearances
                    .get_mut(&index)
                    .ok_or("Recovery has no controller contract")?;
                if matches!(
                    plan.route.segment(index).unwrap().stage,
                    Stage::Climb | Stage::Cruise | Stage::Descent
                ) && clearance.holding.is_none()
                {
                    return Err(format!(
                        "Production movement {index} has no contingent holding clearance"
                    ));
                }
                if !super::controller::covers_recovery(clearance.limit_station(), recovery) {
                    clearance.coordinated_recovery = super::controller::recovery_coordination(
                        clearance.limit_station(),
                        recovery,
                        &controller.stations,
                    )
                    .map_err(|error| {
                        format!(
                            "Recovery at movement {index} leaves its controller coverage: {error}"
                        )
                    })?;
                }
            }
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::{
        state::{GeneratedTraffic, TrafficTarget},
        stations::Station,
    };

    #[test]
    fn assembled_protected_route_and_recoveries_drive_the_controller_to_parking() {
        let point = |metres, altitude| Waypoint {
            latitude: 0.0,
            longitude: metres / 111_320.0,
            altitude_feet: altitude,
        };
        let coordinates = [
            point(0.0, 0.0),
            point(200.0, 0.0),
            point(1000.0, 0.0),
            point(5000.0, 1000.0),
            point(8000.0, 1000.0),
            point(10000.0, 500.0),
            point(12000.0, 100.0),
            point(13000.0, 0.0),
            point(14000.0, 0.0),
            point(14200.0, 0.0),
            point(14500.0, 0.0),
        ];
        let stages = [
            Stage::LineUp,
            Stage::Takeoff,
            Stage::Climb,
            Stage::Cruise,
            Stage::Descent,
            Stage::Approach,
            Stage::Approach,
            Stage::Rollout,
            Stage::Rollout,
            Stage::TaxiIn,
            Stage::Turnaround,
        ];
        let segments = stages
            .iter()
            .enumerate()
            .map(|(index, stage)| Segment {
                stage: *stage,
                points: if index == 10 {
                    vec![coordinates[index]]
                } else {
                    vec![coordinates[index], coordinates[index + 1]]
                },
                knots: if *stage == Stage::Turnaround {
                    0.0
                } else if matches!(stage, Stage::LineUp | Stage::TaxiIn) || index == 8 {
                    10.0
                } else {
                    100.0
                },
                resources: match index {
                    0..=1 => vec!["RUNWAY/TEST/09/27".into()],
                    6..=8 => vec!["RUNWAY/DEST/09/27".into()],
                    10 => vec!["STAND/DEST/1".into()],
                    _ => vec![],
                },
                dwell_seconds: 0.0,
            })
            .collect::<Vec<_>>();
        let route = Arc::new(Route::compile(segments, 1500.0, 1500.0).unwrap());
        route.validate_rates(3.0, 3.0).unwrap();
        let hold = |fix| {
            openatc_core::traffic::holding::Pattern {
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
        let missed_fix = point(16000.0, 1000.0);
        let missed = Arc::new(
            Route::compile(
                vec![
                    Segment {
                        stage: Stage::MissedApproach,
                        points: vec![coordinates[6], missed_fix],
                        knots: 100.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    },
                    hold(missed_fix),
                ],
                1500.0,
                1500.0,
            )
            .unwrap(),
        );
        let airport = |id: &str, start, end| {
            Arc::new(Airport {
                icao: id.into(),
                runways: vec![openatc_core::airport::Runway {
                    first_name: "09".into(),
                    second_name: "27".into(),
                    width: 45.0,
                    first: Point {
                        east: start,
                        ..Default::default()
                    },
                    second: Point {
                        east: end,
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                ..Default::default()
            })
        };
        let origin = airport("TEST", 0.0, 1000.0);
        let destination = airport("DEST", 13000.0, 14000.0);
        let weather = |airport: &Airport| {
            environment::Environment::from_sample(
                airport,
                &(
                    serde_json::json!({"airport":airport.icao,"latitude":0.0,"longitude":0.0,
                "sampleAltitudeFeet":0.0,"pressureHpa":1013.25,"temperatureC":15.0,
                "magneticVariationDegrees":0.0,"windDegrees":0.0,"windKnots":0.0}),
                    Instant::now(),
                ),
            )
            .unwrap()
        };
        let departure_weather = weather(&origin);
        let arrival_weather = weather(&destination);
        let requirements: Vec<openatc_core::traffic::trajectory::Fix> = (2..=5)
            .map(|index| {
                let end = *route.segment(index).unwrap().points.last().unwrap();
                let atmosphere = departure_weather.atmosphere;
                openatc_core::traffic::trajectory::Fix {
                    coordinate: [end.latitude, end.longitude],
                    arrival_heading: Some(90.0),
                    minimum_feet: end.altitude_feet,
                    maximum_feet: end.altitude_feet,
                    preferred_feet: end.altitude_feet,
                    ground_knots: 100.0,
                    required_turn: None,
                    intercept: None,
                    airspeed: Some(openatc_core::traffic::trajectory::Airspeed {
                        atmosphere,
                        indicated_knots: 10000.0
                            / atmosphere.true_knots(100.0, end.altitude_feet).unwrap(),
                        wind_from: 0.0,
                        wind_knots: 0.0,
                    }),
                }
            })
            .collect();
        let build_checked = || -> Checked {
            Checked {
                airspeeds: BTreeMap::new(),
                controller: None,
                route: route.clone(),
                reports: BTreeMap::from([(
                    9,
                    flight::RunwayReport {
                        airport: "DEST".into(),
                        runway: "09".into(),
                    },
                )]),
                acceleration: 3.0,
                braking: 3.0,
                holding: None,
                missed: missed.clone(),
                missed_variants: BTreeMap::new(),
                map_index: 5,
                climb_fpm: 1500.0,
                descent_fpm: 1500.0,
                requirements: requirements.clone(),
                surface_start: 9,
                arrival_orders: vec![(
                    0,
                    openatc_core::state::TaxiClearance {
                        airport: "DEST".into(),
                        instructions: "Taxi to stand 1 via A.".into(),
                        via: "A".into(),
                        destination: "1".into(),
                        ..Default::default()
                    },
                )],
            }
        };
        let checked = build_checked();
        let final_recoveries = checked.final_recoveries(|_, _| true);
        assert!(final_recoveries.values().all(Result::is_ok));
        let stations = [
            ("TEST", "Tower", 120_950),
            ("TEST", "Departure", 124_500),
            ("TEST", "Center", 118_675),
            ("DEST", "Approach", 125_500),
            ("DEST", "Tower", 120_900),
            ("DEST", "Ground", 121_705),
        ]
        .into_iter()
        .map(|(airport, service, khz)| Station {
            airport: airport.into(),
            service: service.into(),
            name: format!("{airport} {service}"),
            khz,
            coverage: if service == "Center" {
                vec![openatc_core::stations::Coverage {
                    floor: 0.0,
                    ceiling: 20000.0,
                    points: vec![[-0.2, -0.2], [-0.2, 0.2], [0.2, 0.2], [0.2, -0.2]],
                }]
            } else {
                vec![]
            },
            ..Default::default()
        })
        .collect::<Vec<_>>();
        let terminal = routing::Terminal {
            arrival_manual: None,
            direct: false,
            enroute_from: [0.0, 0.0],
            enroute_to: [0.0, 0.1],
            enroute_minimum_feet: 1000,
            departure_name: "TEST1A".into(),
            departure: vec![],
            arrival_name: "TEST2A".into(),
            arrival_transition: String::new(),
            arrival: vec![],
            approach_name: "I09".into(),
            approach_transition: String::new(),
            approach: vec![],
            missed: vec![],
        };
        let regions = BTreeMap::new();
        let context = super::super::controller::Context {
            amendment: None,
            origin: &origin,
            destination: &destination,
            departure_runway: "09",
            arrival_runway: "09",
            terminal: &terminal,
            callsign: "TEST123",
            stations: &stations,
            regions: &regions,
            departure_weather: &departure_weather,
            arrival_weather: &arrival_weather,
            missed: None,
            scope_seconds: 600.0,
        };
        let mut hold_fields = vec![""; 38];
        hold_fields[0] = "010";
        hold_fields[1] = "I";
        hold_fields[2] = "I09";
        hold_fields[4] = "HOLD";
        hold_fields[5] = "ZZ";
        hold_fields[6] = "P";
        hold_fields[7] = "C";
        hold_fields[8] = "EE H";
        hold_fields[9] = "R";
        hold_fields[11] = "HM";
        hold_fields[20] = "0900";
        hold_fields[21] = "T010";
        hold_fields[22] = "+";
        hold_fields[23] = "01000";
        let catalogue = openatc_core::traffic::procedures::Catalogue::parse(&format!(
            "APPCH:{};",
            hold_fields.join(",")
        ))
        .unwrap();
        let mut amended_terminal = terminal.clone();
        amended_terminal.missed = catalogue.0.values().next().unwrap().clone();
        let missed = flight::MissedInstruction {
            fix: "HOLD".into(),
            geometric_feet: 1000.0,
            reported_feet: 1000,
            standard: false,
        };
        let amended_context = super::super::controller::Context {
            terminal: &amended_terminal,
            missed: Some(&missed),
            ..context
        };
        let bad_reference = flight::MissedInstruction {
            reported_feet: 2000,
            ..missed.clone()
        };
        assert!(
            checked
                .controller_contracts(&super::super::controller::Context {
                    missed: Some(&bad_reference),
                    ..amended_context
                })
                .is_err()
        );
        let mut amended_route_terminal = terminal.clone();
        let mut manual_fields = vec![""; 38];
        manual_fields[0] = "010";
        manual_fields[1] = "S";
        manual_fields[2] = "TEST2A";
        manual_fields[4] = "COPEN";
        manual_fields[11] = "FM";
        let manual_leg = openatc_core::traffic::procedures::Catalogue::parse(&format!(
            "STAR:{};",
            manual_fields.join(",")
        ))
        .unwrap()
        .0
        .values()
        .next()
        .unwrap()
        .clone();
        amended_route_terminal.arrival_manual =
            Some(openatc_core::traffic::resolved::ManualTermination {
                terminator: "FM".into(),
                identifier: "COPEN".into(),
            });
        amended_route_terminal.arrival = manual_leg;
        let mut entry_fields = vec![""; 38];
        entry_fields[0] = "010";
        entry_fields[1] = "A";
        entry_fields[2] = "I09";
        entry_fields[4] = "KELLA";
        entry_fields[11] = "IF";
        amended_route_terminal.approach = openatc_core::traffic::procedures::Catalogue::parse(
            &format!("APPCH:{};", entry_fields.join(",")),
        )
        .unwrap()
        .0
        .values()
        .next()
        .unwrap()
        .clone();
        let amendment = flight::RouteAmendment {
            procedure: "TEST2A".into(),
            transition: String::new(),
            terminator: "FM".into(),
            manual_fix: "COPEN".into(),
            target: "KELLA".into(),
            approach: "I09".into(),
            target_coordinate: [coordinates[5].latitude, coordinates[5].longitude],
        };
        let route_context = super::super::controller::Context {
            terminal: &amended_route_terminal,
            amendment: Some(&amendment),
            ..context
        };
        let unamended = super::super::controller::Context {
            terminal: &terminal,
            ..route_context
        };
        let amended_checked = build_checked();
        assert!(amended_checked.controller_contracts(&unamended).is_err());
        let route_plan = amended_checked
            .finish_controlled(
                &route_context,
                |index, segment| {
                    if let Some(result) = final_recoveries.get(&index) {
                        return result.clone();
                    }
                    Ok(Arc::new(Route::compile(
                        vec![hold(*segment.points.last().unwrap())],
                        1500.0,
                        1500.0,
                    )?))
                },
                |_, _| true,
            )
            .unwrap();
        let amended = route_plan.clearances[&5].amended_route.as_ref().unwrap();
        assert_eq!(amended, &amendment);
        // The lateral change is announced in the instruction and confirmed in the read-back.
        assert!(route_plan.clearances[&5].instruction.contains("KELLA"));
        assert!(route_plan.clearances[&5].readback.contains("KELLA"));
        for invalid in 0..5 {
            let mut broken = route_plan.clone();
            match invalid {
                0 => {
                    broken
                        .clearances
                        .get_mut(&5)
                        .unwrap()
                        .amended_route
                        .as_mut()
                        .unwrap()
                        .target_coordinate = [1.0, 1.0];
                }
                4 => broken.clearances.get_mut(&5).unwrap().amended_route = None,
                1 => {
                    broken.clearances.get_mut(&5).unwrap().instruction = "Descend as filed.".into();
                }
                2 => {
                    broken.clearances.get_mut(&5).unwrap().readback = "Roger, I09.".into();
                }
                _ => {
                    broken
                        .clearances
                        .get_mut(&5)
                        .unwrap()
                        .amended_route
                        .as_mut()
                        .unwrap()
                        .terminator = "ZZ".into();
                }
            }
            assert!(broken.validate().is_err(), "case {invalid} accepted");
        }
        // A procedure with no published manual termination cannot be amended at all.
        assert!(
            route_plan.validate().is_ok(),
            "the issued amendment must validate"
        );
        let plan = checked
            .finish_controlled(
                &amended_context,
                |index, segment| {
                    if let Some(result) = final_recoveries.get(&index) {
                        return result.clone();
                    }
                    Ok(Arc::new(Route::compile(
                        vec![hold(*segment.points.last().unwrap())],
                        1500.0,
                        1500.0,
                    )?))
                },
                |_, _| true,
            )
            .unwrap();
        assert!(plan.clearances[&5].instruction.contains("HOLD"));
        assert!(plan.clearances[&5].readback.contains("1000 feet"));
        assert_eq!(plan.clearances[&5].missed.as_ref(), Some(&missed));
        for invalid in 0..6 {
            let mut broken = plan.clone();
            match invalid {
                0 => broken.clearances.get_mut(&6).unwrap().landing_rollout = Some(9),
                1 => broken.clearances.get_mut(&7).unwrap().station.khz = 121_705,
                2 => broken.clearances.get_mut(&8).unwrap().readback = "Different clearance".into(),
                3 => broken.clearances.get_mut(&5).unwrap().airborne_through = Some(6),
                4 => {
                    broken
                        .clearances
                        .get_mut(&5)
                        .unwrap()
                        .missed
                        .as_mut()
                        .unwrap()
                        .geometric_feet = 2000.0;
                }
                _ => broken
                    .clearances
                    .get_mut(&5)
                    .unwrap()
                    .missed
                    .as_mut()
                    .unwrap()
                    .fix
                    .clear(),
            }
            assert!(broken.validate().is_err());
        }
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.settings.hear_traffic_radio = false;
        state.session.telemetry.position_valid = true;
        state.session.telemetry.longitude = -0.02;
        state.session.telemetry.traffic = Some(vec![]);
        state.generated.epoch = Some(state.session_generation);
        state.generated.policy =
            Some(parse_operations(include_str!("../../../../traffic/operations.toml")).unwrap());
        state.generated.airport = Some(origin);
        state.generated.destination = Some(destination);
        let taxi = Arc::new(
            Route::compile(
                vec![Segment {
                    stage: Stage::TaxiOut,
                    points: vec![point(-100.0, 0.0), coordinates[0]],
                    knots: 10.0,
                    resources: vec![],
                    dwell_seconds: 0.0,
                }],
                1500.0,
                1500.0,
            )
            .unwrap(),
        );
        assert!(state.generated.world.insert(0, 1, taxi));
        assert!(state.generated.world.authorise(0, 1, 0, &BTreeSet::new()));
        state.generated.world.advance(0.0, false);
        state.generated.world.advance(30.0, false);
        state.generated.frames.push(GeneratedTraffic {
            target: TrafficTarget {
                id: 1,
                callsign: "TEST123".into(),
                com1_khz: Some(120_950),
                ..Default::default()
            },
            model: Arc::new(openatc_core::traffic::csl::Model {
                id: "fixture".into(),
                objects: vec![],
                vertical_offset: None,
                matches: vec![],
            }),
            animation: Animation::default(),
            body_heading_degrees: None,
            vertical_speed_fpm: 0.0,
        });
        state.generated.tower_ready.insert(1);
        flight::install(&mut state.generated, 1, &plan).unwrap();
        let mut checked_conflict = false;
        let mut landing_instructions = 0;
        for second in 31..1000 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            if state.generated.exchange.as_ref().is_some_and(|exchange| {
                exchange.segment == 6 && exchange.grant && exchange.next == 1
            }) {
                landing_instructions += 1;
            }
            if !checked_conflict && state.generated.approved_orders.contains(&(1, 6)) {
                checked_conflict = true;
                assert!((7..=8).all(|index| state.generated.approved_orders.contains(&(1, index))));
                for airborne in [false, true] {
                    let mut blocked = crate::test_state();
                    blocked.generated = state.generated.clone();
                    blocked.session = state.session.clone();
                    blocked.settings = state.settings.clone();
                    blocked.session.telemetry.traffic = Some(vec![TrafficTarget {
                        id: 88,
                        latitude: if airborne { 0.03 } else { 0.0 },
                        longitude: if airborne { 11000.0 } else { 13500.0 } / 111_320.0,
                        altitude_feet: if airborne { 500.0 } else { 0.0 },
                        ground_speed_knots: Some(if airborne { 180.0 } else { 0.0 }),
                        on_ground: !airborne,
                        track_degrees: if airborne { 180.0 } else { 90.0 },
                        aircraft: "A320".into(),
                        ..Default::default()
                    }]);
                    if airborne {
                        blocked
                            .generated
                            .observed_climbs(BTreeMap::from([(88, 0.0)]));
                        assert!(!blocked.generated.crossing_blocked(
                            1,
                            6,
                            &super::super::observed_targets(
                                &blocked.generated,
                                &blocked.session.telemetry
                            )
                        ));
                    }
                    blocked.session.telemetry.radio_busy = true;
                    let before = blocked.generated.approved_orders.clone();
                    let mut worker = blocked.generated.clone();
                    blocked.session.telemetry.simulation_seconds = Some(f64::from(second));
                    super::super::advance(&mut worker, &blocked.session.telemetry);
                    let revoked = before
                        .difference(&worker.approved_orders)
                        .copied()
                        .collect::<BTreeSet<_>>();
                    assert!((6..=8).all(|index| revoked.contains(&(1, index))));
                    // The observation may clear before the worker publishes; its revocation must survive.
                    blocked.session.telemetry.traffic = Some(vec![]);
                    super::super::publish_revocations(&mut blocked.generated, &worker, &revoked);
                    blocked.generated.world = worker.world;
                    assert!(blocked.generated.cancelled_landings.contains(&(1, 6)));
                    assert!(
                        (6..=8)
                            .all(|index| !blocked.generated.approved_orders.contains(&(1, index)))
                    );
                    for step in 1..180 {
                        blocked.session.telemetry.simulation_seconds =
                            Some(f64::from(second + step));
                        super::super::advance(&mut blocked.generated, &blocked.session.telemetry);
                        let pose = &blocked.generated.world.snapshot()[0];
                        assert!(
                            !matches!(
                                pose.stage,
                                Stage::Rollout | Stage::TaxiIn | Stage::Turnaround
                            ),
                            "{pose:?}"
                        );
                        if pose.recovering {
                            break;
                        }
                    }
                    assert!(blocked.generated.world.snapshot()[0].recovering);
                    let report = flight::exchange(&mut blocked).expect("missed approach report");
                    assert_eq!(
                        report.recovery_from,
                        blocked.generated.world.snapshot()[0].recovery_from
                    );
                    assert!(!report.grant);
                    assert!(
                        report
                            .messages
                            .iter()
                            .any(|(_, text)| text.to_lowercase().contains("going around")),
                        "{:?}",
                        report.messages
                    );
                    assert!(
                        !blocked
                            .generated
                            .cancelled_landings
                            .iter()
                            .any(|(id, _)| *id == 1)
                    );
                    for _ in 0..400 {
                        blocked.session.telemetry.simulation_seconds =
                            Some(blocked.session.telemetry.simulation_seconds.unwrap() + 1.0);
                        super::super::advance(&mut blocked.generated, &blocked.session.telemetry);
                    }
                    assert_eq!(blocked.generated.world.snapshot()[0].stage, Stage::Holding);
                    let mut onward_clearances = BTreeMap::new();
                    for old in 5..10 {
                        let mut clearance = plan.clearances[&old].clone();
                        clearance.landing_rollout = clearance.landing_rollout.map(|end| end - 5);
                        clearance.missed = None;
                        onward_clearances.insert(old - 5, clearance);
                    }
                    let onward = flight::Prepared {
                        spacing_floor: None,
                        airspeeds: plan
                            .airspeeds
                            .range(5..)
                            .map(|(&index, speed)| (index - 5, *speed))
                            .collect(),
                        route: Arc::new(
                            Route::compile(
                                (5..=10)
                                    .map(|index| plan.route.segment(index).unwrap().clone())
                                    .collect(),
                                1500.0,
                                1500.0,
                            )
                            .unwrap(),
                        ),
                        clearances: onward_clearances,
                        recovery: plan
                            .recovery
                            .range(5..=6)
                            .map(|(index, route)| (index - 5, route.clone()))
                            .collect(),
                        vacated_reports: BTreeMap::from([(4, plan.vacated_reports[&9].clone())]),
                        amendment: None,
                    };
                    onward.validate_onward().unwrap();
                    let error =
                        flight::offer_rejoin(&mut blocked.generated, 1, onward).unwrap_err();
                    assert!(error.contains("retain or replace"), "{error}");
                    assert!(blocked.generated.rejoins.is_empty());
                }
            }
            state.session.telemetry.simulation_seconds = Some(f64::from(second));
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            assert!(!pose.recovering, "{pose:?}");
            if pose.stage == Stage::Turnaround && pose.complete {
                break;
            }
        }
        let pose = &state.generated.world.snapshot()[0];
        assert_eq!(pose.stage, Stage::Turnaround, "{pose:?}");
        assert!(pose.complete);
        assert!(checked_conflict);
        assert_eq!(landing_instructions, 1);
    }
}
