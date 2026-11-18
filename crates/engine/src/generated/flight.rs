//! Install a complete checked flight at the departure hold without granting movement.

#![allow(clippy::default_trait_access)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
#[allow(unused_imports, clippy::wildcard_imports)]
use super::*;
use super::{Arc, BTreeMap, BTreeSet, EngineState, GeneratedTraffic, Instant, Runtime, radio};
use openatc_core::stations::Station;
use openatc_core::traffic::motion::{Route, Stage};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct MissedInstruction {
    pub fix: String,
    pub geometric_feet: f64,
    pub reported_feet: i32,
    pub standard: bool,
}

/// An explicit controller amendment replacing a published manual termination.
///
/// A manual termination publishes no flyable geometry, so the onward lateral path exists only
/// as an issued instruction with its read-back. The published procedure, its terminator and its
/// manual transition point are retained here so the amendment can be checked against the source.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RouteAmendment {
    /// Published procedure whose manual termination is amended.
    pub procedure: String,
    /// Published transition of that procedure.
    pub transition: String,
    /// Path terminator that ends the published computable geometry.
    pub terminator: String,
    /// Published manual transition point closing that geometry.
    pub manual_fix: String,
    /// Explicitly amended target fix the controller directs the flight to.
    pub target: String,
    /// Approach procedure the amended target belongs to.
    pub approach: String,
    /// Coordinate of the amended target, so the flown path can be checked against it.
    pub target_coordinate: [f64; 2],
}

#[derive(Clone)]
pub(super) struct SectorTransfer {
    /// Nominal seconds remaining in this movement when the receiving sector begins.
    pub remaining_seconds: f64,
    pub station: Station,
}

#[derive(Clone)]
pub(super) struct Clearance {
    pub station: Station,
    /// Coordinated receiver sectors along this same movement; no additional movement grant.
    pub transfers: Vec<SectorTransfer>,
    /// Neighboring sectors accepting the separately cleared fallback under its limit controller.
    pub coordinated_recovery: Vec<Station>,
    pub instruction: String,
    pub readback: String,
    /// Last initial-climb limit explicitly included in the takeoff instruction.
    pub initial_climb: Option<usize>,
    /// Last taxi segment covered by this one instruction; never spans a runway crossing.
    pub taxi_through: Option<usize>,
    /// Last airborne limit explicitly covered by this instruction and readback.
    pub airborne_through: Option<usize>,
    /// Last rollout movement explicitly covered by a landing clearance on final.
    pub landing_rollout: Option<usize>,
    /// An amended missed level explicitly included in the approach instruction and readback.
    pub missed: Option<MissedInstruction>,
    /// An explicit lateral amendment included in this clearance's instruction and readback.
    pub amended_route: Option<RouteAmendment>,
    /// Contingent hold at the end of this clearance's explicitly granted scope.
    pub holding: Option<super::recovery::LimitHold>,
}

impl Clearance {
    fn stations(&self) -> impl Iterator<Item = &Station> {
        std::iter::once(&self.station)
            .chain(self.transfers.iter().map(|transfer| &transfer.station))
    }
    pub fn station_after(&self, transfers: usize) -> &Station {
        transfers
            .checked_sub(1)
            .and_then(|index| self.transfers.get(index))
            .map_or(&self.station, |transfer| &transfer.station)
    }
    pub fn limit_station(&self) -> &Station {
        self.station_after(self.transfers.len())
    }
}

pub(super) fn on_station(
    runtime: &Runtime,
    id: u32,
    frequency: Option<i32>,
    station: &Station,
) -> bool {
    frequency == Some(station.khz)
        && runtime.controllers.get(&id).is_none_or(|current| {
            current.khz == station.khz
                && current
                    .name
                    .trim()
                    .eq_ignore_ascii_case(station.name.trim())
        })
}

fn sector_exchange(
    runtime: &mut Runtime,
    pose: &openatc_core::traffic::motion::Pose,
    frame: &GeneratedTraffic,
    prepared: &Arc<Prepared>,
) -> Option<radio::Exchange> {
    if let Some((plan, receiver)) = runtime.pending_sector_checkins.remove(&pose.id)
        && Arc::ptr_eq(&plan, prepared)
        && frame.target.com1_khz == Some(receiver.khz)
    {
        let slots = [
            ("callsign", frame.target.callsign.clone()),
            ("station", receiver.name.clone()),
        ];
        return Some(radio::Exchange {
            speed: None,
            id: pose.id,
            segment: pose.segment,
            station: receiver,
            callsign: frame.target.callsign.clone(),
            messages: vec![
                (
                    false,
                    openatc_core::dialogue::say("pilot_station_checkin", &slots),
                ),
                (true, openatc_core::dialogue::say("flight_checkin", &slots)),
            ],
            next: 0,
            due: Instant::now(),
            grant: false,
            tune_after: None,
            sector_transfer: None,
            recovery_from: None,
            rejoin: false,
        });
    }
    if pose.awaiting_clearance || pose.complete || pose.recovering {
        return None;
    }
    let clearance = prepared.clearances.get(&pose.segment)?;
    let count = runtime
        .sector_transfers
        .get(&(pose.id, pose.segment))
        .copied()
        .unwrap_or(0);
    let transfer = clearance.transfers.get(count)?;
    let lead = runtime.policy.as_ref()?.flight_clearance_lead_seconds;
    if pose.seconds_to_limit > transfer.remaining_seconds + lead {
        return None;
    }
    let sender = clearance.station_after(count);
    if !on_station(runtime, pose.id, frame.target.com1_khz, sender) {
        return None;
    }
    let slots = [
        ("callsign", frame.target.callsign.clone()),
        ("station", transfer.station.name.clone()),
        (
            "frequency",
            format!("{:.3}", f64::from(transfer.station.khz) / 1000.0),
        ),
    ];
    Some(radio::Exchange {
        speed: None,
        id: pose.id,
        segment: pose.segment,
        station: sender.clone(),
        callsign: frame.target.callsign.clone(),
        messages: vec![
            (true, openatc_core::dialogue::say("flight_handoff", &slots)),
            (
                false,
                openatc_core::dialogue::say("pilot_frequency_readback", &slots),
            ),
        ],
        next: 0,
        due: Instant::now(),
        grant: false,
        tune_after: Some(transfer.station.clone()),
        sector_transfer: Some((prepared.clone(), count + 1)),
        recovery_from: None,
        rejoin: false,
    })
}

/// The runway actually used, retained independently of controller wording.
#[derive(Clone)]
pub(super) struct RunwayReport {
    pub airport: String,
    pub runway: String,
}

#[derive(Clone)]
pub(super) struct Prepared {
    /// Conservative floor across the retained arrival restrictions; None refuses speed amendments.
    pub spacing_floor: Option<f64>,
    pub route: Arc<Route>,
    pub airspeeds: BTreeMap<usize, openatc_core::traffic::trajectory::Airspeed>,
    pub recovery: BTreeMap<usize, Arc<Route>>,
    pub clearances: BTreeMap<usize, Clearance>,
    /// Keyed by the first segment after the runway exit/crossing.
    pub vacated_reports: BTreeMap<usize, RunwayReport>,
    /// Lateral amendment this flight was planned with; its clearance must carry the directive.
    pub amendment: Option<RouteAmendment>,
}

impl Prepared {
    /// Resume the existing checked route at its own contingent holding fix.
    /// A missed-approach hold requires a newly planned approach rather than this suffix.
    pub fn resume_after_limit(&self, source: usize) -> Result<Self, String> {
        let held = self
            .clearances
            .get(&source)
            .and_then(|c| c.holding.as_ref())
            .filter(|hold| hold.boundary == source)
            .ok_or("Flight has no contingent hold at this limit")?;
        if self
            .recovery
            .get(&source)
            .is_none_or(|route| !held.matches(route))
        {
            return Err("Contingent holding geometry no longer matches its clearance".into());
        }
        let first = source.checked_add(1).ok_or("Onward index overflow")?;
        let route = Arc::new(self.route.suffix(first)?);
        if route
            .segment(0)
            .is_none_or(|s| s.points.first() != Some(&held.position))
        {
            return Err("Onward route does not start at its cleared holding fix".into());
        }
        let mut clearances = BTreeMap::new();
        for (&index, original) in self.clearances.range(first..) {
            let mut clearance = original.clone();
            clearance.initial_climb = None;
            clearance.airborne_through = clearance
                .airborne_through
                .map(|end| {
                    end.checked_sub(first)
                        .ok_or("Invalid onward airborne scope")
                })
                .transpose()?;
            clearance.taxi_through = clearance
                .taxi_through
                .map(|end| end.checked_sub(first).ok_or("Invalid onward taxi scope"))
                .transpose()?;
            clearance.landing_rollout = clearance
                .landing_rollout
                .map(|end| end.checked_sub(first).ok_or("Invalid onward landing scope"))
                .transpose()?;
            if let Some(hold) = &mut clearance.holding {
                hold.boundary = hold
                    .boundary
                    .checked_sub(first)
                    .ok_or("Invalid onward holding scope")?;
            }
            clearances.insert(index - first, clearance);
        }
        // The first remaining movement may have inherited a whole-SID or airborne clearance.
        let inherited = self
            .clearances
            .range(..first)
            .rev()
            .find_map(|(&start, clearance)| {
                clearance
                    .airborne_through
                    .filter(|end| (start..=*end).contains(&first))
                    .or_else(|| {
                        clearance
                            .initial_climb
                            .filter(|end| (2..=*end).contains(&first))
                    })
            });
        if let Some(end) = inherited.filter(|end| *end > first) {
            clearances
                .get_mut(&0)
                .ok_or("Onward flight has no first clearance")?
                .airborne_through = Some(end - first);
        }
        let result = Self {
            spacing_floor: self.spacing_floor,
            route,
            // A resumed suffix inherits the amended target it was already flying to.
            amendment: self.amendment.clone(),
            airspeeds: self
                .airspeeds
                .range(first..)
                .map(|(&index, speed)| (index - first, *speed))
                .collect(),
            clearances,
            recovery: self
                .recovery
                .range(first..)
                .map(|(&index, route)| (index - first, route.clone()))
                .collect(),
            vacated_reports: self
                .vacated_reports
                .range(first..)
                .map(|(&index, report)| (index - first, report.clone()))
                .collect(),
        };
        result.validate_onward()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self
            .route
            .segment(0)
            .is_none_or(|segment| segment.stage != Stage::LineUp)
            || self
                .route
                .segment(1)
                .is_none_or(|segment| segment.stage != Stage::Takeoff)
        {
            return Err(
                "Generated flight must begin with separate lineup and takeoff limits".into(),
            );
        }
        if self
            .route
            .segment(2)
            .is_none_or(|segment| segment.stage != Stage::Climb)
            || self
                .clearances
                .get(&1)
                .is_none_or(|clearance| clearance.initial_climb.is_none_or(|end| end < 2))
            || self
                .clearances
                .iter()
                .any(|(index, clearance)| *index != 1 && clearance.initial_climb.is_some())
        {
            return Err("Takeoff needs an explicit initial-climb permission".into());
        }
        self.validate_route(false)
    }

    pub fn validate_onward(&self) -> Result<(), String> {
        if self.route.segment(0).is_none_or(|segment| {
            segment.stage.on_ground()
                || matches!(segment.stage, Stage::Holding)
                || !segment.resources.is_empty()
        }) || self
            .clearances
            .values()
            .any(|clearance| clearance.initial_climb.is_some())
        {
            return Err("Onward flight must start with a separately cleared airborne path".into());
        }
        self.validate_route(true)
    }

    fn validate_route(&self, onward: bool) -> Result<(), String> {
        for (&index, speed) in &self.airspeeds {
            let segment = self
                .route
                .segment(index)
                .filter(|segment| !segment.stage.on_ground())
                .ok_or("Retained airspeed does not belong to an airborne movement")?;
            if segment.points.windows(2).any(|pair| {
                let track = openatc_core::arrival::bearing(
                    [pair[0].latitude, pair[0].longitude],
                    [pair[1].latitude, pair[1].longitude],
                );
                speed.ground_knots(pair[1].altitude_feet, track).is_err()
            }) {
                return Err("Retained airspeed or planning wind is invalid".into());
            }
        }
        let mut stages = Vec::new();
        let mut missed_instruction: Option<&MissedInstruction> = None;
        let mut amended_route: Option<&RouteAmendment> = None;
        for index in 0..512 {
            let Some(segment) = self.route.segment(index) else {
                break;
            };
            stages.push(segment.stage);
            if segment.stage != Stage::Turnaround {
                let clearance = self
                    .clearances
                    .get(&index)
                    .ok_or_else(|| format!("No generated clearance for segment {index}"))?;
                if !(118_000..=136_990).contains(&clearance.station.khz)
                    || clearance.instruction.trim().is_empty()
                    || clearance.instruction.len() > 4096
                    || clearance.readback.trim().is_empty()
                    || clearance.readback.len() > 4096
                {
                    return Err(format!("Invalid generated clearance at segment {index}"));
                }
                let mut remaining = self.route.nominal_seconds(index, index).unwrap_or(0.0);
                if clearance.coordinated_recovery.len() > 32
                    || clearance.coordinated_recovery.iter().any(|station| {
                        !(118_000..=136_990).contains(&station.khz)
                            || station.coverage.is_empty()
                            || !(station.service == "Center"
                                || station.services.iter().any(|service| service == "Center"))
                    })
                    || !clearance.coordinated_recovery.is_empty() && segment.stage.on_ground()
                {
                    return Err(format!(
                        "Invalid recovery controller coordination at segment {index}"
                    ));
                }
                for transfer in &clearance.transfers {
                    if !matches!(segment.stage, Stage::Climb | Stage::Cruise | Stage::Descent)
                        || !transfer.remaining_seconds.is_finite()
                        || transfer.remaining_seconds <= 0.0
                        || transfer.remaining_seconds >= remaining
                        || !(118_000..=136_990).contains(&transfer.station.khz)
                        || transfer.station.coverage.is_empty()
                        || !(transfer.station.service == "Center"
                            || transfer
                                .station
                                .services
                                .iter()
                                .any(|service| service == "Center"))
                        || clearance.initial_climb.is_some()
                        || clearance.airborne_through.is_some()
                    {
                        return Err(format!(
                            "Invalid coordinated controller transfer at segment {index}"
                        ));
                    }
                    remaining = transfer.remaining_seconds;
                }
                let services: &[&str] = match segment.stage {
                    Stage::LineUp | Stage::Takeoff | Stage::Rollout => &["Tower"],
                    Stage::TaxiIn | Stage::Crossing => &["Ground", "Tower"],
                    Stage::Climb => &["Departure", "Approach", "Center"],
                    Stage::Cruise | Stage::Descent | Stage::Holding => &["Approach", "Center"],
                    Stage::Approach | Stage::MissedApproach => &["Approach", "Tower"],
                    _ => &[],
                };
                if !services.contains(&clearance.station.service.as_str())
                    && !clearance
                        .station
                        .services
                        .iter()
                        .any(|service| services.contains(&service.as_str()))
                {
                    return Err(format!(
                        "Wrong controller service for generated segment {index}"
                    ));
                }
            }
            if let Some(instruction) = self
                .clearances
                .get(&index)
                .and_then(|clearance| clearance.missed.as_ref())
            {
                if segment.stage != Stage::Approach
                    || instruction.fix.is_empty()
                    || instruction.fix.len() > 16
                    || !instruction
                        .fix
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-')
                    || !instruction.geometric_feet.is_finite()
                    || !(-3000.0..=65000.0).contains(&instruction.geometric_feet)
                    || !(-2000..=60000).contains(&instruction.reported_feet)
                {
                    return Err("Invalid amended missed-approach contract".into());
                }
                missed_instruction = Some(instruction);
            }
            // An amended route replaces a published manual termination, so the instruction and
            // read-back must both name the amended target before that movement can be flown.
            if let Some(amendment) = self
                .clearances
                .get(&index)
                .and_then(|clearance| clearance.amended_route.as_ref())
            {
                let names = |text: &str| {
                    amendment.target.len() > 16
                        || !amendment
                            .target
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-')
                        || !text.contains(&amendment.target)
                };
                let clearance = &self.clearances[&index];
                if segment.stage != Stage::Approach
                    || amendment.procedure.trim().is_empty()
                    || amendment.approach.trim().is_empty()
                    || !openatc_core::traffic::resolved::MANUAL_TERMINATORS
                        .contains(&amendment.terminator.as_str())
                    || names(&clearance.instruction)
                    || names(&clearance.readback)
                {
                    return Err("Invalid amended route contract".into());
                }
                amended_route = Some(amendment);
            }
            if let Some(hold) = self.clearances.get(&index).and_then(|c| c.holding.as_ref()) {
                let clearance = &self.clearances[&index];
                let expected = if index == 1 && segment.stage == Stage::Takeoff {
                    clearance.initial_climb
                } else if matches!(segment.stage, Stage::Climb | Stage::Cruise | Stage::Descent) {
                    self.clearances
                        .get(&1)
                        .and_then(|c| c.initial_climb)
                        .filter(|end| (2..=*end).contains(&index))
                        .or_else(|| {
                            self.clearances
                                .range(..=index)
                                .rev()
                                .find_map(|(&start, c)| {
                                    c.airborne_through
                                        .filter(|end| (start..=*end).contains(&index))
                                })
                        })
                        .or(Some(index))
                } else {
                    None
                };
                if expected != Some(hold.boundary)
                    || hold.boundary < index
                    || self.route.segment(hold.boundary).is_none_or(|limit| {
                        !matches!(limit.stage, Stage::Climb | Stage::Cruise | Stage::Descent)
                            || limit.points.last() != Some(&hold.position)
                    })
                    || self
                        .recovery
                        .get(&hold.boundary)
                        .is_none_or(|route| !hold.matches(route))
                {
                    return Err("Contingent holding contract does not match its clearance limit and protected recovery".into());
                }
            }
            if segment.stage != Stage::Approach {
                missed_instruction = None;
                amended_route = None;
            }
            if let Some(amendment) = amended_route {
                // The flown path must actually arrive at the amended target before the
                // cleared movement; an issued instruction alone grants no lateral deviation.
                let reached = (0..=index).any(|step| {
                    self.route.segment(step).is_some_and(|route| {
                        route.points.iter().any(|point| {
                            openatc_core::arrival::distance_nm(
                                [point.latitude, point.longitude],
                                amendment.target_coordinate,
                            ) < 0.001
                        })
                    })
                });
                if !reached {
                    return Err("Amended route does not reach its cleared target".into());
                }
            }
            if let Some(instruction) = missed_instruction {
                if segment
                    .points
                    .iter()
                    .any(|point| point.altitude_feet > instruction.geometric_feet + 1.0)
                {
                    return Err("Amended missed level is below its approach entry".into());
                }
                let recovery = self
                    .recovery
                    .get(&index)
                    .ok_or("Amended missed clearance lacks a recovery")?;
                if recovery
                    .segment(0)
                    .is_none_or(|segment| segment.stage != Stage::MissedApproach)
                {
                    return Err("Amended missed clearance has no published missed path".into());
                }
                let hold = (0..512)
                    .map_while(|step| recovery.segment(step))
                    .last()
                    .filter(|segment| segment.stage == Stage::Holding)
                    .ok_or("Amended missed clearance has no holding continuation")?;
                if hold
                    .points
                    .iter()
                    .any(|point| (point.altitude_feet - instruction.geometric_feet).abs() > 1.0)
                {
                    return Err("Missed recovery does not match its amended cleared level".into());
                }
            }
            if !segment.stage.on_ground()
                && segment.stage != Stage::Holding
                && !self.recovery.contains_key(&index)
            {
                return Err(format!(
                    "Generated flight has no recovery at segment {index}"
                ));
            }
            if (index > 1 || onward)
                && matches!(
                    segment.stage,
                    Stage::Parked
                        | Stage::TaxiOut
                        | Stage::Pushback
                        | Stage::LineUp
                        | Stage::Takeoff
                )
            {
                return Err("Departure movements reappear after the takeoff segment".into());
            }
        }
        // A planned amendment only exists as a cleared, read-back lateral change, so the
        // approach it belongs to cannot be flown without its directive, and no clearance
        // may announce a different amended target.
        if self.amendment.is_some() {
            let approach = |index: usize| {
                self.route
                    .segment(index)
                    .is_some_and(|segment| segment.stage == Stage::Approach)
            };
            for clearance in self.clearances.values() {
                if clearance.amended_route.as_ref().is_some_and(|directive| {
                    directive != self.amendment.as_ref().expect("checked above")
                }) {
                    return Err("Approach clearance announces a different amended target".into());
                }
            }
            if !self
                .clearances
                .iter()
                .any(|(index, clearance)| approach(*index) && clearance.amended_route.is_some())
            {
                return Err(
                    "Amended approach clearance does not carry its planned directive".into(),
                );
            }
        }
        for (&start, clearance) in &self.clearances {
            let Some(end) = clearance.taxi_through else {
                continue;
            };
            if end <= start || end >= stages.len() || stages[start] != Stage::TaxiIn {
                return Err("Taxi clearance has an invalid movement limit".into());
            }
            for (index, stage) in stages.iter().enumerate().take(end + 1).skip(start + 1) {
                let next = self
                    .clearances
                    .get(&index)
                    .ok_or("Taxi clearance has a missing movement")?;
                if *stage != Stage::TaxiIn
                    || self.vacated_reports.contains_key(&index)
                    || next.taxi_through.is_some()
                    || next.station.airport != clearance.station.airport
                    || next.station.khz != clearance.station.khz
                    || next.station.service != clearance.station.service
                    || next.station.name != clearance.station.name
                    || next.missed != clearance.missed
                    || next.holding != clearance.holding
                    || next.instruction != clearance.instruction
                    || next.readback != clearance.readback
                {
                    return Err(
                        "Taxi clearance crosses a separate permission or controller limit".into(),
                    );
                }
            }
        }
        let spans = self
            .clearances
            .iter()
            .filter_map(|(&start, clearance)| {
                clearance.airborne_through.map(|end| (start, end, false))
            })
            .chain(
                self.clearances
                    .get(&1)
                    .and_then(|clearance| clearance.initial_climb)
                    .filter(|end| *end > 2)
                    .map(|end| (2, end, true)),
            );
        for (start, end, initial) in spans {
            let clearance = self
                .clearances
                .get(&start)
                .ok_or("Airborne clearance has no starting movement")?;
            if end <= start
                || end >= stages.len()
                || clearance.taxi_through.is_some()
                || initial && clearance.airborne_through.is_some()
                || self
                    .route
                    .segment(start)
                    .is_none_or(|segment| !segment.resources.is_empty())
                || !matches!(
                    stages[start],
                    Stage::Climb
                        | Stage::Cruise
                        | Stage::Descent
                        | Stage::Approach
                        | Stage::MissedApproach
                )
            {
                return Err("Airborne clearance has an invalid movement limit".into());
            }
            for index in start + 1..=end {
                let next = self
                    .clearances
                    .get(&index)
                    .ok_or("Airborne clearance has a missing movement")?;
                if stages[index] != stages[start]
                    || !next.transfers.is_empty()
                    || !clearance.transfers.is_empty()
                    || next.airborne_through.is_some()
                    || next.landing_rollout.is_some()
                    || next.taxi_through.is_some()
                    || next.initial_climb.is_some()
                    || self.vacated_reports.contains_key(&index)
                    || self
                        .route
                        .segment(index)
                        .is_none_or(|segment| !segment.resources.is_empty())
                    || next.station.airport != clearance.station.airport
                    || next.station.khz != clearance.station.khz
                    || next.station.service != clearance.station.service
                    || next.station.name != clearance.station.name
                    || next.missed != clearance.missed
                    || next.holding != clearance.holding
                    || next.instruction != clearance.instruction
                    || next.readback != clearance.readback
                {
                    return Err(
                        "Airborne clearance crosses a separate permission or controller limit"
                            .into(),
                    );
                }
            }
        }
        for (&start, clearance) in &self.clearances {
            let Some(end) = clearance.landing_rollout else {
                continue;
            };
            if stages.get(start) != Some(&Stage::Approach)
                || end <= start
                || end >= stages.len()
                || clearance.initial_climb.is_some()
                || clearance.airborne_through.is_some()
                || clearance.taxi_through.is_some()
                || !(clearance.station.service == "Tower"
                    || clearance
                        .station
                        .services
                        .iter()
                        .any(|service| service == "Tower"))
                || self.route.segment(start).is_none_or(|segment| {
                    segment.resources.iter().any(|resource| {
                        !resource.starts_with("RUNWAY/")
                            || self
                                .route
                                .segment(start + 1)
                                .is_none_or(|rollout| !rollout.resources.contains(resource))
                    })
                })
            {
                return Err("Landing clearance has an invalid final or rollout limit".into());
            }
            for (index, stage) in stages.iter().enumerate().take(end + 1).skip(start + 1) {
                let next = self
                    .clearances
                    .get(&index)
                    .ok_or("Landing clearance lacks its rollout contract")?;
                if *stage != Stage::Rollout
                    || next.landing_rollout.is_some()
                    || next.initial_climb.is_some()
                    || next.taxi_through.is_some()
                    || next.airborne_through.is_some()
                    || next.station.airport != clearance.station.airport
                    || next.station.khz != clearance.station.khz
                    || next.station.service != clearance.station.service
                    || next.station.name != clearance.station.name
                    || next.missed != clearance.missed
                    || next.instruction != clearance.instruction
                    || next.readback != clearance.readback
                    || self.vacated_reports.contains_key(&index)
                {
                    return Err(
                        "Landing clearance crosses a separate runway or controller limit".into(),
                    );
                }
                let resources = &self.route.segment(index).unwrap().resources;
                let first = &self.route.segment(start + 1).unwrap().resources;
                if !resources
                    .iter()
                    .any(|resource| resource.starts_with("RUNWAY/"))
                    || !first
                        .iter()
                        .filter(|resource| resource.starts_with("RUNWAY/"))
                        .all(|resource| resources.contains(resource))
                {
                    return Err("Landing rollout does not retain its protected runway".into());
                }
            }
            if stages.get(end + 1) == Some(&Stage::Rollout) {
                return Err("Landing clearance must include its complete rollout".into());
            }
        }
        for (index, pair) in stages.windows(2).enumerate() {
            if pair[0] == Stage::Crossing
                && !matches!(pair[1], Stage::TaxiIn | Stage::Crossing | Stage::Turnaround)
            {
                return Err("Runway crossing has no surface exit limit".into());
            }
            if matches!(pair[0], Stage::Rollout | Stage::Crossing)
                && matches!(pair[1], Stage::TaxiIn | Stage::Crossing | Stage::Turnaround)
            {
                let report = self
                    .vacated_reports
                    .get(&(index + 1))
                    .ok_or("Runway exit needs a typed vacated report")?;
                if report.runway.trim().is_empty()
                    || report.airport.trim().is_empty()
                    || self
                        .clearances
                        .get(&index)
                        .is_none_or(|clearance| clearance.station.airport != report.airport)
                {
                    return Err("Runway report does not match its surface controller".into());
                }
            }
        }
        if self.vacated_reports.keys().any(|&index| {
            index == 0
                || index >= stages.len()
                || !matches!(stages[index - 1], Stage::Rollout | Stage::Crossing)
                || !matches!(
                    stages[index],
                    Stage::TaxiIn | Stage::Crossing | Stage::Turnaround
                )
        }) {
            return Err("Runway report is not at a completed surface runway movement".into());
        }
        let approach = stages
            .iter()
            .position(|stage| *stage == Stage::Approach)
            .ok_or("Generated flight has no approach")?;
        let rollout = stages
            .iter()
            .position(|stage| *stage == Stage::Rollout)
            .ok_or("Generated flight has no landing roll")?;
        let taxi = stages
            .iter()
            .position(|stage| *stage == Stage::TaxiIn)
            .ok_or("Generated flight has no taxi to parking")?;
        if (!onward && approach <= 1)
            || rollout <= approach
            || taxi <= rollout
            || stages.last() != Some(&Stage::Turnaround)
        {
            return Err("Generated arrival phases are incomplete or out of order".into());
        }
        if self
            .recovery
            .keys()
            .any(|index| self.route.segment(*index).is_none())
        {
            return Err("Generated recovery refers to a missing phase".into());
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(super) struct Rejoin {
    previous: Arc<Prepared>,
    plan: Arc<Prepared>,
    source: usize,
    accepted: bool,
}

/// Reuse a protected onward route only after the aircraft has reported its contingent hold.
pub(super) fn prepare_limit_rejoins(runtime: &mut Runtime) {
    let poses = runtime.world.snapshot();
    let held = poses
        .iter()
        .filter_map(|pose| pose.recovery_from.map(|source| (pose.id, source)))
        .collect::<BTreeSet<_>>();
    runtime.rejoin_attempts.retain(|key, owner| {
        held.contains(key)
            && runtime
                .flights
                .get(&key.0)
                .is_some_and(|current| Arc::ptr_eq(current, owner))
    });
    let candidates = poses
        .into_iter()
        .filter_map(|pose| {
            let source = pose.recovery_from?;
            let previous = runtime.flights.get(&pose.id)?;
            (pose.stage == Stage::Holding
                && runtime.active_flights.contains(&pose.id)
                && runtime.reported_recoveries.contains(&(pose.id, source))
                && !runtime.rejoins.contains_key(&pose.id)
                && previous
                    .clearances
                    .get(&source)
                    .and_then(|c| c.holding.as_ref())
                    .is_some_and(|hold| hold.boundary == source)
                && !runtime
                    .rejoin_attempts
                    .get(&(pose.id, source))
                    .is_some_and(|attempt| Arc::ptr_eq(attempt, previous)))
            .then(|| (pose.id, source, previous.clone()))
        })
        .collect::<Vec<_>>();
    for (id, source, previous) in candidates {
        let result = previous
            .resume_after_limit(source)
            .and_then(|prepared| offer_rejoin(runtime, id, prepared));
        if let Err(error) = result {
            runtime.rejoin_attempts.insert((id, source), previous);
            if runtime.warnings.len() < 128 {
                runtime
                    .warnings
                    .push(format!("Traffic {id} remains holding: {error}"));
            }
        }
    }
}

#[cfg(test)]
pub(super) fn rejoin_refusal(
    runtime: &Runtime,
    telemetry: &openatc_core::state::Telemetry,
    id: u32,
) -> Option<String> {
    let rejoin = runtime.rejoins.get(&id)?;
    super::separation::rejoin_check(runtime, telemetry, id, &rejoin.plan).err()
}

pub(super) fn offer_rejoin(
    runtime: &mut Runtime,
    id: u32,
    prepared: Prepared,
) -> Result<(), String> {
    prepared.validate_onward()?;
    validate_reports(runtime, &prepared)?;
    let pose = runtime
        .world
        .snapshot()
        .into_iter()
        .find(|pose| pose.id == id)
        .ok_or("Aircraft no longer exists")?;
    let source = pose
        .recovery_from
        .filter(|_| pose.stage == Stage::Holding)
        .ok_or("Aircraft is not following its recovery hold")?;
    let previous = runtime
        .flights
        .get(&id)
        .ok_or("Original flight is unavailable")?;
    let prior_missed = previous
        .clearances
        .range(..=source)
        .rev()
        .find_map(|(_, clearance)| clearance.missed.as_ref());
    if prior_missed.is_some() {
        let first_approach = (0..512).find(|index| {
            prepared
                .route
                .segment(*index)
                .is_some_and(|segment| segment.stage == Stage::Approach)
        });
        if first_approach
            .and_then(|index| prepared.clearances.get(&index))
            .is_none_or(|clearance| clearance.missed.is_none())
        {
            return Err(
                "Onward approach must explicitly retain or replace its amended missed clearance"
                    .into(),
            );
        }
    }
    if runtime.rejoins.contains_key(&id)
        || !runtime.active_flights.contains(&id)
        || runtime
            .frames
            .iter()
            .find(|frame| frame.target.id == id)
            .is_none_or(|frame| {
                !previous
                    .clearances
                    .values()
                    .flat_map(Clearance::stations)
                    .any(|station| on_station(runtime, id, frame.target.com1_khz, station))
            })
    {
        return Err(
            "Onward clearance needs a known current controller and a free request slot".into(),
        );
    }
    let through = prepared.clearances[&0].airborne_through.unwrap_or(0);
    let mut probe = runtime.world.clone();
    probe
        .queue_hold_exit(
            probe.generation(),
            id,
            prepared.route.clone(),
            prepared.recovery.clone(),
            through,
        )
        .map_err(|error| format!("Onward flight cannot leave its holding fix: {error}"))?;
    runtime.rejoins.insert(
        id,
        Rejoin {
            previous: runtime
                .flights
                .get(&id)
                .ok_or("Original flight is unavailable")?
                .clone(),
            plan: Arc::new(prepared),
            source,
            accepted: false,
        },
    );
    Ok(())
}

pub(super) fn rejoin_blocked(
    runtime: &Runtime,
    telemetry: &openatc_core::state::Telemetry,
    id: u32,
) -> bool {
    runtime.rejoins.get(&id).is_none_or(|rejoin| {
        super::separation::rejoin_blocked(runtime, telemetry, id, &rejoin.plan)
    })
}

pub(super) fn accept_rejoin(runtime: &mut Runtime, id: u32, source: usize) {
    let Some(rejoin) = runtime.rejoins.get(&id) else {
        return;
    };
    if rejoin.source != source || rejoin.accepted {
        return;
    }
    if runtime
        .frames
        .iter()
        .find(|frame| frame.target.id == id)
        .is_none_or(|frame| frame.target.com1_khz != Some(rejoin.plan.clearances[&0].station.khz))
    {
        return;
    }
    let through = rejoin.plan.clearances[&0].airborne_through.unwrap_or(0);
    if !runtime.world.leave_hold_at_fix(
        runtime.world.generation(),
        id,
        rejoin.plan.route.clone(),
        rejoin.plan.recovery.clone(),
        through,
    ) {
        runtime.rejoins.remove(&id);
        return;
    }
    runtime.rejoins.get_mut(&id).unwrap().accepted = true;
}

fn synchronise_rejoins(runtime: &mut Runtime) {
    let completed = runtime
        .rejoins
        .iter()
        .filter(|(id, rejoin)| rejoin.accepted && runtime.world.follows(**id, &rejoin.plan.route))
        .map(|(&id, rejoin)| (id, rejoin.plan.clone()))
        .collect::<Vec<_>>();
    for (id, plan) in completed {
        runtime
            .approved_orders
            .retain(|(aircraft, _)| *aircraft != id);
        for index in 0..=plan.clearances[&0].airborne_through.unwrap_or(0) {
            runtime.approved_orders.insert((id, index));
        }
        runtime
            .pending_checkins
            .retain(|(aircraft, _)| *aircraft != id);
        runtime
            .reported_crossings
            .retain(|(aircraft, _)| *aircraft != id);
        runtime
            .reported_recoveries
            .retain(|(aircraft, _)| *aircraft != id);
        runtime.handoff_targets.insert(id, 0);
        runtime
            .sector_transfers
            .retain(|(aircraft, _), _| *aircraft != id);
        runtime.pending_sector_checkins.remove(&id);
        runtime.flights.insert(id, plan);
        runtime.rejoins.remove(&id);
    }
}

/// A discontinuous simulator clock invalidates outstanding airborne movement grants.
pub(super) fn clock_changed(runtime: &mut Runtime) {
    runtime.rejoins.clear();
    runtime.rejoin_attempts.clear();
    runtime.cancelled_landings.clear();
    runtime.pending_sector_checkins.clear();
    if runtime
        .exchange
        .as_ref()
        .is_some_and(|exchange| exchange.rejoin || exchange.sector_transfer.is_some())
    {
        runtime.exchange = None;
    }
    let airborne: BTreeSet<_> = runtime
        .world
        .snapshot()
        .into_iter()
        .filter(|pose| !pose.stage.on_ground() && !pose.recovering)
        .map(|pose| pose.id)
        .collect();
    runtime
        .approved_orders
        .retain(|(id, _)| !airborne.contains(id));
    if runtime
        .exchange
        .as_ref()
        .is_some_and(|exchange| exchange.grant && airborne.contains(&exchange.id))
    {
        runtime.exchange = None;
    }
}

/// A fallback changes route indices; outstanding normal-route messages and grants are stale.
pub(super) fn synchronise_recoveries(runtime: &mut Runtime) {
    synchronise_rejoins(runtime);
    let poses = runtime.world.snapshot();
    runtime.cancelled_landings.retain(|(id, start)| {
        runtime.active_flights.contains(id)
            && poses
                .iter()
                .any(|pose| pose.id == *id && !pose.recovering && pose.segment <= *start)
    });
    let stale = runtime
        .rejoins
        .iter()
        .filter_map(|(&id, rejoin)| {
            let valid = runtime.active_flights.contains(&id)
                && runtime
                    .flights
                    .get(&id)
                    .is_some_and(|flight| Arc::ptr_eq(flight, &rejoin.previous))
                && runtime.frames.iter().any(|frame| frame.target.id == id)
                && poses.iter().any(|pose| {
                    pose.id == id
                        && pose.stage == Stage::Holding
                        && !pose.complete
                        && pose.recovery_from == Some(rejoin.source)
                });
            (!valid).then_some(id)
        })
        .collect::<Vec<_>>();
    for id in stale {
        runtime.rejoins.remove(&id);
        runtime
            .world
            .cancel_hold_exit(runtime.world.generation(), id);
        if runtime
            .exchange
            .as_ref()
            .is_some_and(|exchange| exchange.id == id && exchange.rejoin)
        {
            runtime.exchange = None;
        }
    }
    let recovering: BTreeMap<_, _> = runtime
        .world
        .snapshot()
        .into_iter()
        .filter_map(|pose| pose.recovery_from.map(|source| (pose.id, source)))
        .collect();
    runtime
        .approved_orders
        .retain(|(id, _)| !recovering.contains_key(id));
    runtime
        .pending_checkins
        .retain(|(id, _)| !recovering.contains_key(id));
    runtime
        .pending_sector_checkins
        .retain(|id, _| !recovering.contains_key(id));
    runtime
        .handoff_targets
        .retain(|id, _| !recovering.contains_key(id));
    if runtime.exchange.as_ref().is_some_and(|exchange| {
        recovering.get(&exchange.id).is_some_and(|source| {
            exchange.recovery_from != Some(*source)
                || exchange.grant
                    && !(exchange.rejoin
                        && runtime
                            .rejoins
                            .get(&exchange.id)
                            .is_some_and(|rejoin| rejoin.source == *source && !rejoin.accepted))
                || exchange.tune_after.is_some() && !exchange.rejoin
        }) || exchange.recovery_from.is_some() && !recovering.contains_key(&exchange.id)
    }) {
        runtime.exchange = None;
    }
}

pub(super) fn acknowledge(runtime: &mut Runtime, id: u32, segment: usize) {
    if !runtime.active_flights.contains(&id)
        || !runtime.approved_orders.contains(&(id, segment))
        || runtime
            .world
            .snapshot()
            .iter()
            .any(|pose| pose.id == id && pose.recovering)
    {
        return;
    }
    let Some(clearance) = runtime
        .flights
        .get(&id)
        .and_then(|flight| flight.clearances.get(&segment))
    else {
        return;
    };
    if let Some(end) = clearance.airborne_through {
        if !runtime
            .world
            .authorise_airborne_until(runtime.world.generation(), id, segment, end)
        {
            runtime.approved_orders.remove(&(id, segment));
            return;
        }
        for index in segment + 1..=end {
            runtime.approved_orders.insert((id, index));
        }
    }
    if segment == 1
        && let Some(end) = clearance.initial_climb
    {
        if end > 2
            && !runtime
                .world
                .authorise_airborne_until(runtime.world.generation(), id, 2, end)
        {
            let callsign = runtime
                .frames
                .iter()
                .find(|frame| frame.target.id == id)
                .map_or_else(|| id.to_string(), |frame| frame.target.callsign.clone());
            runtime.log(
                openatc_settings::TrafficLog::Summary,
                format!(
                    "{callsign} takeoff readback could not record the initial climb through segment {end}"
                ),
            );
            runtime.approved_orders.remove(&(id, segment));
            return;
        }
        for index in 2..=end {
            runtime.approved_orders.insert((id, index));
        }
    }
    if let Some(end) = clearance.landing_rollout {
        for index in segment + 1..=end {
            runtime.approved_orders.insert((id, index));
        }
    }
    if let Some(end) = clearance.taxi_through {
        for index in segment + 1..=end {
            runtime.approved_orders.insert((id, index));
        }
    }
}

pub(super) fn revoke_landing(runtime: &mut Runtime, id: u32, start: usize) {
    let Some(end) = runtime
        .flights
        .get(&id)
        .and_then(|plan| plan.clearances.get(&start))
        .and_then(|clearance| clearance.landing_rollout)
    else {
        return;
    };
    for index in start..=end {
        runtime.approved_orders.remove(&(id, index));
    }
    let generation = runtime.world.generation();
    runtime.world.withdraw_continuation(generation, id, start);
    runtime
        .world
        .withdraw_continuation(generation, id, start + 1);
}

pub(super) fn validate_reports(runtime: &Runtime, prepared: &Prepared) -> Result<(), String> {
    for (index, report) in &prepared.vacated_reports {
        let airport = runtime
            .surface_airport(&report.airport)
            .ok_or("Runway report airport is unavailable")?;
        let runway = airport
            .runways
            .iter()
            .find(|runway| {
                runway.first_name == report.runway || runway.second_name == report.runway
            })
            .ok_or("Runway report names an unknown runway")?;
        let movement = prepared
            .route
            .segment(index - 1)
            .ok_or("Runway report movement is unavailable")?;
        if !movement.resources.contains(&format!(
            "RUNWAY/{}/{}/{}",
            airport.icao, runway.first_name, runway.second_name
        )) {
            return Err("Runway report movement does not protect the named runway".into());
        }
    }
    Ok(())
}

/// Validate on a clone so a bad recovery cannot leave an aircraft stuck in a half-installed flight.
pub(super) fn install(runtime: &mut Runtime, id: u32, prepared: &Prepared) -> Result<(), String> {
    prepared.validate()?;
    validate_reports(runtime, prepared)?;
    let pose = runtime
        .world
        .snapshot()
        .into_iter()
        .find(|pose| pose.id == id)
        .ok_or("Generated aircraft no longer exists")?;
    if pose.stage != Stage::TaxiOut || !pose.complete || !runtime.tower_ready.contains(&id) {
        return Err("Generated aircraft has not completed taxi and Tower check-in".into());
    }
    let mut world = runtime.world.clone();
    let generation = world.generation();
    if !world.continue_surface(generation, id, prepared.route.clone()) {
        return Err("Generated departure is discontinuous at the holding point".into());
    }
    for (boundary, route) in &prepared.recovery {
        if !world.recovery(generation, id, *boundary, route.clone()) {
            return Err(format!(
                "Generated recovery is discontinuous at segment {boundary}"
            ));
        }
    }
    runtime.world = world;
    runtime
        .sector_transfers
        .retain(|(aircraft, _), _| *aircraft != id);
    runtime.pending_sector_checkins.remove(&id);
    runtime.flights.insert(id, Arc::new(prepared.clone()));
    runtime
        .approved_orders
        .retain(|(aircraft, _)| *aircraft != id);
    runtime
        .reported_crossings
        .retain(|(aircraft, _)| *aircraft != id);
    runtime
        .reported_holds
        .retain(|(aircraft, _)| *aircraft != id);
    runtime
        .hold_reasons
        .retain(|(aircraft, _), _| *aircraft != id);
    runtime
        .pending_checkins
        .retain(|(aircraft, _)| *aircraft != id);
    runtime
        .reported_recoveries
        .retain(|(aircraft, _)| *aircraft != id);
    runtime.rejoins.remove(&id);
    runtime
        .rejoin_attempts
        .retain(|(aircraft, _), _| *aircraft != id);
    runtime.cancelled_takeoffs.remove(&id);
    runtime
        .cancelled_landings
        .retain(|(aircraft, _)| *aircraft != id);
    runtime.orders.remove(&id);
    runtime.active_flights.insert(id);
    runtime.handoff_targets.insert(id, 0);
    Ok(())
}

pub(super) fn exchange(state: &mut EngineState) -> Option<radio::Exchange> {
    exchange_filtered(state, false, None)
}

/// Active aircraft already talking to Tower. Called before the general Ground taxi queue so a
/// departure at the hold cannot be starved by surface clearances.
pub(super) fn ground_exchange(state: &mut EngineState) -> Option<radio::Exchange> {
    exchange_filtered(state, true, None)
}

/// The Tower pass restricted to one aircraft, used when Tower answers its ready call.
pub(super) fn ground_exchange_for(state: &mut EngineState, id: u32) -> Option<radio::Exchange> {
    exchange_filtered(state, true, Some(id))
}

/// True when a generated aircraft has reached the holding point geometry. This is the
/// same marker/endpoint/hold-line test the player uses, so an aircraft rolling over the
/// line is recognized without waiting for the motion world to mark the segment complete.
pub(super) fn at_departure_hold(
    runtime: &Runtime,
    pose: &openatc_core::traffic::motion::Pose,
) -> bool {
    if pose.stage != Stage::TaxiOut {
        return false;
    }
    let Some(airport) = runtime.departure_airport(pose.id) else {
        return false;
    };
    let Some(order) = runtime
        .orders
        .get(&pose.id)
        .and_then(|orders| orders.iter().find(|(segment, _)| *segment == pose.segment))
        .map(|(_, order)| order)
    else {
        return false;
    };
    let here = openatc_core::airport::airport_point(
        &airport,
        pose.position.latitude,
        pose.position.longitude,
    );
    openatc_core::airport::taxi_hold_geometry_reached(&airport, order, here)
}

/// Record the order in which departures reached the holding point.
///
/// Distance to the threshold is not a queue order when two taxiways join the runway at
/// different points: a later aircraft on the nearer taxiway would otherwise jump ahead of
/// one already waiting. Aircraft present at the first observation are ordered by id so the
/// queue is stable before any of them talks to Tower.
fn note_departure_order(runtime: &mut Runtime) {
    let holding: BTreeSet<u32> = runtime
        .world
        .snapshot()
        .into_iter()
        .filter(|pose| {
            at_departure_hold(runtime, pose)
                || (pose.complete
                    && runtime
                        .world
                        .assigned_route(pose.id)
                        .is_some_and(|route| pose.segment + 1 == route.segment_count()))
        })
        .map(|pose| pose.id)
        .collect();
    // Only aircraft still waiting at a holding point keep an order. A later turnaround at
    // the same identity joins the queue at the back.
    runtime.departure_order.retain(|id, _| holding.contains(id));
    for id in holding {
        if !runtime.departure_order.contains_key(&id) {
            runtime
                .departure_order
                .insert(id, runtime.departure_sequence);
            runtime.departure_sequence = runtime.departure_sequence.saturating_add(1);
        }
    }
}

/// Aircraft ahead on the same runway, including a complete taxi-out still at the hold.
/// A departure already lined up or rolling keeps the runway; aircraft still at their holding
/// points keep the order in which they arrived.
pub(super) fn lineup_ahead(
    runtime: &mut Runtime,
    id: u32,
    airport: &Airport,
    runway: &str,
) -> Option<u32> {
    note_departure_order(runtime);
    let threshold = airport
        .runways
        .iter()
        .find(|candidate| candidate.first_name == runway || candidate.second_name == runway)
        .map(|candidate| {
            if candidate.first_name == runway {
                candidate.first
            } else {
                candidate.second
            }
        })?;
    let distance = |pose: &openatc_core::traffic::motion::Pose| {
        let point = openatc_core::airport::airport_point(
            airport,
            pose.position.latitude,
            pose.position.longitude,
        );
        (point.east - threshold.east).hypot(point.north - threshold.north)
    };
    let rank = |pose: &openatc_core::traffic::motion::Pose| {
        if matches!(pose.stage, Stage::LineUp | Stage::Takeoff) {
            2
        } else {
            i32::from(pose.stage == Stage::TaxiOut && pose.complete)
        }
    };
    let own_pose = runtime
        .world
        .snapshot()
        .into_iter()
        .find(|pose| pose.id == id);
    let own = own_pose.as_ref().map_or(f64::INFINITY, &distance);
    let own_rank = own_pose.as_ref().map_or(0, &rank);
    let own_order = runtime.departure_order.get(&id).copied();
    runtime.world.snapshot().into_iter().find_map(|pose| {
        if pose.id == id || rank(&pose) == 0 {
            return None;
        }
        if pose.stage == Stage::Takeoff && pose.knots > 0.1 {
            return None;
        }
        let same_runway = runtime
            .flights
            .get(&pose.id)
            .and_then(|plan| plan.route.segment(pose.segment))
            .is_some_and(|segment| {
                segment.resources.iter().any(|resource| {
                    let Some(rest) = resource.strip_prefix("RUNWAY/") else {
                        return false;
                    };
                    let mut parts = rest.split('/');
                    parts.next() == Some(&airport.icao) && parts.next() == Some(runway)
                })
            })
            || runtime.orders.get(&pose.id).is_some_and(|orders| {
                orders
                    .last()
                    .is_some_and(|(_, order)| order.hold_short_runway == runway)
            });
        if !same_runway {
            return None;
        }
        let other_rank = rank(&pose);
        if other_rank != own_rank {
            return (other_rank > own_rank).then_some(pose.id);
        }
        if other_rank == 1
            && let (Some(own_order), Some(other_order)) =
                (own_order, runtime.departure_order.get(&pose.id).copied())
        {
            return (other_order < own_order).then_some(pose.id);
        }
        let other = distance(&pose);
        (other + 1.0 < own || ((other - own).abs() <= 1.0 && pose.id < id)).then_some(pose.id)
    })
}

/// Clear a departure for takeoff directly from the hold when the runway is clear.
///
/// The normal contract chain has separate line-up and take-off clearances. A busy Tower
/// should not force two calls when the runway is already available.
pub(super) fn takeoff_exchange_for(state: &mut EngineState, id: u32) -> Option<radio::Exchange> {
    let pose = state
        .generated
        .world
        .snapshot()
        .into_iter()
        .find(|pose| pose.id == id)?;
    if pose.stage != Stage::LineUp || !pose.awaiting_clearance {
        return None;
    }
    let prepared = state.generated.flights.get(&id).cloned()?;
    let frame = state
        .generated
        .frames
        .iter()
        .find(|frame| frame.target.id == id)?
        .clone();
    let index = pose.segment + 1;
    let takeoff = prepared.clearances.get(&index)?.clone();
    if prepared.route.segment(index)?.stage != Stage::Takeoff
        || !on_station(
            &state.generated,
            id,
            frame.target.com1_khz,
            &takeoff.station,
        )
    {
        return None;
    }
    let airport = state
        .generated
        .surface_airport(&takeoff.station.airport)
        .cloned()?;
    let runway = prepared
        .route
        .segment(index)?
        .resources
        .iter()
        .find_map(|resource| {
            resource
                .strip_prefix("RUNWAY/")
                .and_then(|rest| rest.split('/').nth(1))
                .map(ToOwned::to_owned)
        })?;
    if lineup_ahead(&mut state.generated, id, &airport, &runway).is_some()
        || state
            .generated
            .runway_use(&format!("RUNWAY/{}/{runway}", airport.icao))
            != super::RunwayUse::Clear
        || state
            .generated
            .departure_wake
            .get(&id)
            .is_some_and(|seconds| *seconds > 0.0)
    {
        return None;
    }
    let mut blocked = super::external_surface_cells(&state.generated, &state.session.telemetry);
    let telemetry = &state.session.telemetry;
    if telemetry.on_ground
        && let Some(policy) = state.generated.policy.as_ref()
    {
        let profile = openatc_core::traffic::profiles::aircraft(&telemetry.aircraft_icao);
        let span = telemetry
            .wingspan_meters
            .or_else(|| profile.map(|profile| profile.wingspan))
            .unwrap_or(36.0);
        let point =
            openatc_core::airport::airport_point(&airport, telemetry.latitude, telemetry.longitude);
        let at_stand = airport.parking.iter().any(|stand| {
            (point.east - stand.point.east).hypot(point.north - stand.point.north) <= 50.0
        });
        let on_taxiway = !at_stand && openatc_core::airport::on_taxiway(&airport, &point, 20.0);
        if let Some(clearance) = openatc_core::traffic::runway_clearance(
            &airport,
            &runway,
            telemetry.latitude,
            telemetry.longitude,
            telemetry.heading_degrees,
            &telemetry.aircraft_icao,
            Some(span),
            0.0,
        ) && !clearance.over_runway
            && !on_taxiway
        {
            let player_cells =
                super::cells(&airport, point, span / 2.0, policy.conflict_cell_meters);
            for segment_index in pose.segment..=index {
                let Some(segment) = prepared.route.segment(segment_index) else {
                    continue;
                };
                for resource in &segment.resources {
                    if player_cells.contains(resource) {
                        blocked.remove(resource);
                    }
                }
            }
        }
    }
    // Use the same conflict checks as the readback grant. Issuing a clearance that the
    // next tick immediately cancels is worse than holding or lining up.
    let targets = super::external_targets(&state.generated, &state.session.telemetry);
    if state.generated.crossing_blocked(id, index, &targets)
        || super::separation::blocked(
            &state.generated,
            &state.session.telemetry,
            id,
            index,
            &takeoff,
        )
    {
        return None;
    }
    let generation = state.generated.world.generation();
    let mut probe = state.generated.world.clone();
    if !probe.authorise(generation, id, pose.segment, &blocked)
        || !probe.authorise_continuation(generation, id, index, &blocked)
    {
        return None;
    }
    state.generated.approved_orders.insert((id, pose.segment));
    state.generated.log(
        openatc_settings::TrafficLog::Summary,
        format!(
            "{} cleared for takeoff directly from the hold on runway {runway}",
            frame.target.callsign
        ),
    );
    Some(radio::Exchange {
        speed: None,
        id,
        segment: index,
        station: takeoff.station,
        callsign: frame.target.callsign,
        messages: vec![(true, takeoff.instruction), (false, takeoff.readback)],
        next: 0,
        due: Instant::now(),
        grant: true,
        tune_after: None,
        sector_transfer: None,
        recovery_from: None,
        rejoin: false,
    })
}

fn exchange_filtered(
    state: &mut EngineState,
    ground_only: bool,
    only: Option<u32>,
) -> Option<radio::Exchange> {
    let blocked = super::external_surface_cells(&state.generated, &state.session.telemetry);
    let profiles = state.traffic_profiles.current();
    let targets = super::external_targets(&state.generated, &state.session.telemetry);
    let mut poses = state.generated.world.snapshot();
    poses.sort_by(|left, right| {
        let deadline = |pose: &openatc_core::traffic::motion::Pose| {
            if pose.awaiting_clearance {
                0.0
            } else {
                pose.seconds_to_limit
            }
        };
        let ground =
            |pose: &openatc_core::traffic::motion::Pose| i32::from(!pose.stage.on_ground());
        ground(left)
            .cmp(&ground(right))
            .then_with(|| deadline(left).total_cmp(&deadline(right)))
            .then_with(|| left.id.cmp(&right.id))
    });
    for pose in poses {
        if (ground_only && !pose.stage.on_ground())
            || only.is_some_and(|only| only != pose.id)
            || !state.generated.active_flights.contains(&pose.id)
        {
            continue;
        }
        let Some(prepared) = state.generated.flights.get(&pose.id).cloned() else {
            continue;
        };
        let Some(frame) = state
            .generated
            .frames
            .iter()
            .find(|frame| frame.target.id == pose.id)
            .cloned()
        else {
            continue;
        };
        if pose.recovering {
            let Some(source) = pose.recovery_from else {
                continue;
            };
            // Once the missed route has started, a delayed cancellation cannot say to continue approach.
            state
                .generated
                .cancelled_landings
                .retain(|(id, _)| *id != pose.id);
            if pose.stage == Stage::Holding
                && let Some(rejoin) = state.generated.rejoins.get(&pose.id)
                && !rejoin.accepted
                && rejoin.source == source
                && !super::separation::rejoin_blocked(
                    &state.generated,
                    &state.session.telemetry,
                    pose.id,
                    &rejoin.plan,
                )
            {
                let clearance = &rejoin.plan.clearances[&0];
                if !on_station(
                    &state.generated,
                    pose.id,
                    frame.target.com1_khz,
                    &clearance.station,
                ) {
                    // Recovery may have changed the clearance chain; hand off from the current owner.
                    let old = rejoin
                        .previous
                        .clearances
                        .values()
                        .flat_map(Clearance::stations)
                        .find(|station| {
                            on_station(&state.generated, pose.id, frame.target.com1_khz, station)
                        })
                        .cloned()
                        .or_else(|| state.generated.controllers.get(&pose.id).cloned());
                    let Some(old) = old else {
                        continue;
                    };
                    let slots = [
                        ("callsign", frame.target.callsign.clone()),
                        ("station", clearance.station.name.clone()),
                        (
                            "frequency",
                            format!("{:.3}", f64::from(clearance.station.khz) / 1000.0),
                        ),
                    ];
                    return Some(radio::Exchange {
                        speed: None,
                        id: pose.id,
                        segment: 0,
                        station: old.clone(),
                        callsign: frame.target.callsign.clone(),
                        messages: vec![
                            (true, openatc_core::dialogue::say("flight_handoff", &slots)),
                            (
                                false,
                                openatc_core::dialogue::say("pilot_frequency_readback", &slots),
                            ),
                        ],
                        next: 0,
                        due: Instant::now(),
                        grant: false,
                        tune_after: Some(clearance.station.clone()),
                        sector_transfer: None,
                        recovery_from: Some(source),
                        rejoin: true,
                    });
                }
                return Some(radio::Exchange {
                    speed: None,
                    id: pose.id,
                    segment: 0,
                    station: clearance.station.clone(),
                    callsign: frame.target.callsign.clone(),
                    messages: vec![
                        (true, clearance.instruction.clone()),
                        (false, clearance.readback.clone()),
                    ],
                    next: 0,
                    due: Instant::now(),
                    grant: true,
                    tune_after: None,
                    sector_transfer: None,
                    recovery_from: Some(source),
                    rejoin: true,
                });
            }
            if pose.stage == Stage::Holding
                && let Some(clearance) = prepared.clearances.get(&source)
                && clearance
                    .holding
                    .as_ref()
                    .is_some_and(|hold| hold.boundary == source)
                && !state
                    .generated
                    .reported_recoveries
                    .contains(&(pose.id, source))
            {
                let station = prepared
                    .clearances
                    .range(..=source)
                    .rev()
                    .flat_map(|(_, contract)| {
                        std::iter::once(&contract.station)
                            .chain(contract.transfers.iter().map(|transfer| &transfer.station))
                    })
                    .find(|station| {
                        on_station(&state.generated, pose.id, frame.target.com1_khz, station)
                    })
                    .cloned();
                if let Some(station) = station {
                    state
                        .generated
                        .reported_recoveries
                        .insert((pose.id, source));
                    return Some(radio::Exchange {
                        speed: None,
                        id: pose.id,
                        segment: pose.segment,
                        station,
                        callsign: frame.target.callsign.clone(),
                        messages: vec![(
                            false,
                            openatc_core::dialogue::say(
                                "generated_pilot_limit_holding",
                                &[("callsign", frame.target.callsign.clone())],
                            ),
                        )],
                        next: 0,
                        due: Instant::now(),
                        grant: false,
                        tune_after: None,
                        sector_transfer: None,
                        recovery_from: Some(source),
                        rejoin: false,
                    });
                }
            }
            if pose.stage != Stage::MissedApproach
                || prepared
                    .route
                    .segment(source)
                    .is_none_or(|segment| segment.stage != Stage::Approach)
                || prepared
                    .recovery
                    .get(&source)
                    .and_then(|route| route.segment(0))
                    .is_none_or(|segment| segment.stage != Stage::MissedApproach)
                || state
                    .generated
                    .reported_recoveries
                    .contains(&(pose.id, source))
            {
                continue;
            }
            // Report on the aircraft's tuned frequency; a pending handoff cannot tune it here.
            let Some(station) = prepared
                .clearances
                .range(..=source)
                .rev()
                .map(|(_, clearance)| &clearance.station)
                .chain(
                    prepared
                        .clearances
                        .range(source + 1..)
                        .map(|(_, clearance)| &clearance.station),
                )
                .find(|station| Some(station.khz) == frame.target.com1_khz)
                .cloned()
            else {
                continue;
            };
            let exchange = radio::Exchange {
                speed: None,
                id: pose.id,
                segment: pose.segment,
                station,
                callsign: frame.target.callsign.clone(),
                messages: vec![(
                    false,
                    openatc_core::dialogue::say(
                        "pilot_go_around_blocked",
                        &[("callsign", frame.target.callsign.clone())],
                    ),
                )],
                next: 0,
                due: Instant::now(),
                grant: false,
                tune_after: None,
                sector_transfer: None,
                recovery_from: Some(source),
                rejoin: false,
            };
            state
                .generated
                .reported_recoveries
                .insert((pose.id, source));
            return Some(exchange);
        }
        if let Some((_, start)) = state
            .generated
            .cancelled_landings
            .iter()
            .find(|(id, _)| *id == pose.id)
            .copied()
        {
            state.generated.cancelled_landings.remove(&(pose.id, start));
            if let Some(clearance) = prepared.clearances.get(&start) {
                return Some(radio::hold_landing(
                    radio::Exchange {
                        speed: None,
                        id: pose.id,
                        segment: start,
                        station: clearance.station.clone(),
                        callsign: frame.target.callsign.clone(),
                        messages: vec![],
                        next: 0,
                        due: Instant::now(),
                        grant: false,
                        tune_after: None,
                        sector_transfer: None,
                        recovery_from: None,
                        rejoin: false,
                    },
                    true,
                ));
            }
        }
        if state.generated.cancelled_takeoffs.contains_key(&pose.id) {
            let Some(clearance) = prepared.clearances.get(&1) else {
                continue;
            };
            if Some(clearance.station.khz) != frame.target.com1_khz {
                continue;
            }
            let wake = *state.generated.cancelled_takeoffs.get(&pose.id).unwrap();
            return Some(radio::hold_takeoff(
                radio::Exchange {
                    speed: None,
                    id: pose.id,
                    segment: 1,
                    station: clearance.station.clone(),
                    callsign: frame.target.callsign.clone(),
                    messages: vec![],
                    next: 0,
                    due: Instant::now(),
                    grant: false,
                    tune_after: None,
                    sector_transfer: None,
                    recovery_from: None,
                    rejoin: false,
                },
                true,
                wake,
            ));
        }
        if let Some(report) = prepared.vacated_reports.get(&pose.segment) {
            let previous = pose.segment - 1;
            if !state
                .generated
                .reported_crossings
                .contains(&(pose.id, previous))
            {
                let Some(airport) = state.generated.surface_airport(&report.airport) else {
                    continue;
                };
                let Some(clearance) = prepared.clearances.get(&previous) else {
                    continue;
                };
                let observed = openatc_core::state::Telemetry {
                    aircraft_icao: frame.target.aircraft.clone(),
                    latitude: pose.position.latitude,
                    longitude: pose.position.longitude,
                    altitude_feet: pose.position.altitude_feet,
                    position_valid: true,
                    on_ground: pose.stage.on_ground(),
                    heading_degrees: pose.heading,
                    wingspan_meters: frame.target.wingspan_meters,
                    ..Default::default()
                };
                if !observed.on_ground
                    || frame.target.com1_khz != Some(clearance.station.khz)
                    || !airport.runways.iter().any(|runway| {
                        runway.first_name == report.runway || runway.second_name == report.runway
                    })
                    || profiles
                        .as_ref()
                        .and_then(|profiles| profiles.aircraft_for(&frame.target.aircraft))
                        .map_or_else(
                            || {
                                openatc_core::airport::aircraft_on_named_runway(
                                    airport,
                                    &report.runway,
                                    &observed,
                                )
                            },
                            |profile| {
                                openatc_core::airport::aircraft_on_named_runway_for(
                                    airport,
                                    &report.runway,
                                    &observed,
                                    profile,
                                )
                            },
                        )
                {
                    continue;
                }
                let exchange = radio::Exchange {
                    speed: None,
                    id: pose.id,
                    segment: pose.segment,
                    station: clearance.station.clone(),
                    callsign: frame.target.callsign.clone(),
                    messages: vec![(
                        false,
                        openatc_core::dialogue::say(
                            "pilot_runway_vacated",
                            &[
                                ("callsign", frame.target.callsign.clone()),
                                ("runway", report.runway.clone()),
                            ],
                        ),
                    )],
                    next: 0,
                    due: Instant::now(),
                    grant: false,
                    tune_after: None,
                    sector_transfer: None,
                    recovery_from: None,
                    rejoin: false,
                };
                state
                    .generated
                    .reported_crossings
                    .insert((pose.id, previous));
                return Some(exchange);
            }
        }
        if pose.complete {
            continue;
        }
        if let Some(index) = state
            .generated
            .pending_checkins
            .iter()
            .find_map(|(id, index)| (*id == pose.id).then_some(*index))
            && let Some(clearance) = prepared.clearances.get(&index)
            && frame.target.com1_khz == Some(clearance.station.khz)
        {
            let slots = [
                ("station", clearance.station.name.clone()),
                ("callsign", frame.target.callsign.clone()),
            ];
            let exchange = radio::Exchange {
                speed: None,
                id: pose.id,
                segment: index,
                station: clearance.station.clone(),
                callsign: frame.target.callsign.clone(),
                messages: vec![
                    (
                        false,
                        openatc_core::dialogue::say("pilot_station_checkin", &slots),
                    ),
                    (true, openatc_core::dialogue::say("flight_checkin", &slots)),
                ],
                next: 0,
                due: Instant::now(),
                grant: false,
                tune_after: None,
                sector_transfer: None,
                recovery_from: None,
                rejoin: false,
            };
            state.generated.pending_checkins.remove(&(pose.id, index));
            return Some(exchange);
        }
        if let Some(exchange) = sector_exchange(&mut state.generated, &pose, &frame, &prepared) {
            return Some(exchange);
        }
        let current_handoff = state
            .generated
            .handoff_targets
            .get(&pose.id)
            .is_none_or(|index| *index < pose.segment)
            && prepared
                .clearances
                .get(&pose.segment)
                .is_some_and(|clearance| {
                    !on_station(
                        &state.generated,
                        pose.id,
                        frame.target.com1_khz,
                        clearance.station_after(
                            state
                                .generated
                                .sector_transfers
                                .get(&(pose.id, pose.segment))
                                .copied()
                                .unwrap_or(0),
                        ),
                    )
                });
        let index = if current_handoff || pose.awaiting_clearance {
            pose.segment
        } else if !pose.stage.on_ground() || pose.stage == Stage::Takeoff {
            if pose.seconds_to_limit
                > state
                    .generated
                    .policy
                    .as_ref()?
                    .flight_clearance_lead_seconds
            {
                continue;
            }
            pose.segment + 1
        } else {
            continue;
        };
        let Some(mut selected) = prepared.clearances.get(&index).cloned() else {
            continue;
        };
        selected.station = selected
            .station_after(
                state
                    .generated
                    .sector_transfers
                    .get(&(pose.id, index))
                    .copied()
                    .unwrap_or(0),
            )
            .clone();
        let clearance = &selected;
        if prepared
            .route
            .segment(index)
            .is_some_and(|segment| matches!(segment.stage, Stage::LineUp | Stage::Takeoff))
            && let Some(segment) = prepared.route.segment(index)
            && let Some(runway) = segment.resources.iter().find_map(|resource| {
                resource
                    .strip_prefix("RUNWAY/")
                    .and_then(|rest| rest.split('/').nth(1))
            })
            && let Some(airport) = state
                .generated
                .surface_airport(&clearance.station.airport)
                .cloned()
            && lineup_ahead(&mut state.generated, pose.id, &airport, runway).is_some()
        {
            continue;
        }
        let crossing = state.generated.crossing_blocked(pose.id, index, &targets);
        let airborne_blocked = super::separation::blocked(
            &state.generated,
            &state.session.telemetry,
            pose.id,
            index,
            clearance,
        );
        if crossing || airborne_blocked {
            let reason = format!("crossing {crossing}, airborne {airborne_blocked}");
            if state.generated.hold_reasons.get(&(pose.id, index)) != Some(&reason) {
                state
                    .generated
                    .hold_reasons
                    .insert((pose.id, index), reason.clone());
                let mut nearby = Vec::new();
                for other in state
                    .generated
                    .world
                    .snapshot()
                    .into_iter()
                    .filter(|other| other.id != pose.id && !other.stage.on_ground())
                {
                    let distance = openatc_core::arrival::distance_nm(
                        [pose.position.latitude, pose.position.longitude],
                        [other.position.latitude, other.position.longitude],
                    );
                    if distance <= 10.0 {
                        let callsign = state
                            .generated
                            .frames
                            .iter()
                            .find(|frame| frame.target.id == other.id)
                            .map_or_else(
                                || other.id.to_string(),
                                |frame| frame.target.callsign.clone(),
                            );
                        nearby.push(format!(
                            "{callsign} {:?}/{} {distance:.1}NM {:.0}ft",
                            other.stage,
                            other.segment,
                            other.position.altitude_feet - pose.position.altitude_feet
                        ));
                    }
                }
                state.generated.log(
                    openatc_settings::TrafficLog::Summary,
                    format!(
                        "{} clearance withheld at segment {index}: {reason}; nearby {nearby:?}",
                        frame.target.callsign
                    ),
                );
            }
            let revoked = state.generated.approved_orders.remove(&(pose.id, index));
            if clearance.landing_rollout.is_some() {
                if revoked {
                    revoke_landing(&mut state.generated, pose.id, index);
                }
                if revoked || state.generated.reported_holds.insert((pose.id, index)) {
                    return Some(radio::hold_landing(
                        radio::Exchange {
                            speed: None,
                            id: pose.id,
                            segment: index,
                            station: clearance.station.clone(),
                            callsign: frame.target.callsign.clone(),
                            messages: vec![],
                            next: 0,
                            due: Instant::now(),
                            grant: false,
                            tune_after: None,
                            sector_transfer: None,
                            recovery_from: None,
                            rejoin: false,
                        },
                        revoked,
                    ));
                }
                continue;
            }
            if prepared
                .route
                .segment(index)
                .is_some_and(|segment| segment.stage == Stage::Takeoff)
            {
                // An aircraft already lined up and waiting does not need another hold
                // transmission. It only hears from Tower when it is cleared or a take-off
                // clearance is actually cancelled.
                if !revoked && matches!(pose.stage, Stage::LineUp | Stage::Takeoff) {
                    continue;
                }
                if revoked || state.generated.reported_holds.insert((pose.id, index)) {
                    return Some(radio::hold_takeoff(
                        radio::Exchange {
                            speed: None,
                            id: pose.id,
                            segment: index,
                            station: clearance.station.clone(),
                            callsign: frame.target.callsign.clone(),
                            messages: vec![],
                            next: 0,
                            due: Instant::now(),
                            grant: false,
                            tune_after: None,
                            sector_transfer: None,
                            recovery_from: None,
                            rejoin: false,
                        },
                        revoked,
                        state
                            .generated
                            .departure_wake
                            .get(&pose.id)
                            .is_some_and(|seconds| *seconds > 0.0),
                    ));
                }
                continue;
            }
            if airborne_blocked
                && prepared
                    .route
                    .segment(index)
                    .is_some_and(|segment| !segment.stage.on_ground())
            {
                if revoked || state.generated.reported_holds.insert((pose.id, index)) {
                    return Some(radio::hold_airborne(radio::Exchange {
                        speed: None,
                        id: pose.id,
                        segment: index,
                        station: clearance.station.clone(),
                        callsign: frame.target.callsign.clone(),
                        messages: vec![],
                        next: 0,
                        due: Instant::now(),
                        grant: false,
                        tune_after: None,
                        sector_transfer: None,
                        recovery_from: None,
                        rejoin: false,
                    }));
                }
                continue;
            }
            if revoked {
                let runway = prepared
                    .vacated_reports
                    .get(&(index + 1))
                    .map(|report| report.runway.as_str());
                return Some(radio::cancel_crossing(
                    radio::Exchange {
                        speed: None,
                        id: pose.id,
                        segment: index,
                        station: clearance.station.clone(),
                        callsign: frame.target.callsign.clone(),
                        messages: vec![],
                        next: 0,
                        due: Instant::now(),
                        grant: false,
                        tune_after: None,
                        sector_transfer: None,
                        recovery_from: None,
                        rejoin: false,
                    },
                    runway,
                ));
            }
            continue;
        }
        state.generated.hold_reasons.remove(&(pose.id, index));
        if !current_handoff && state.generated.approved_orders.contains(&(pose.id, index)) {
            continue;
        }
        let mut probe = state.generated.world.clone();
        let mut available = if current_handoff {
            true
        } else if index == pose.segment {
            probe.authorise(probe.generation(), pose.id, index, &blocked)
        } else {
            probe.authorise_continuation(probe.generation(), pose.id, index, &blocked)
        };
        // Runway clearance uses the exact published width and the player's wing footprint.
        // The coarse ground grid is not allowed to deny a line-up when the player is merely
        // observing from the grass beside the runway.
        if !available
            && index == pose.segment
            && matches!(pose.stage, Stage::LineUp | Stage::Takeoff)
            && state.session.telemetry.on_ground
            && let (Some(segment), Some(airport), Some(policy)) = (
                prepared.route.segment(index),
                state.generated.airport.as_deref(),
                state.generated.policy.as_ref(),
            )
            && let Some(runway) = segment.resources.iter().find_map(|resource| {
                resource
                    .strip_prefix("RUNWAY/")
                    .and_then(|rest| rest.split('/').nth(1))
                    .map(ToOwned::to_owned)
            })
        {
            let telemetry = &state.session.telemetry;
            let profile = openatc_core::traffic::profiles::aircraft(&telemetry.aircraft_icao);
            let span = telemetry
                .wingspan_meters
                .or_else(|| profile.map(|profile| profile.wingspan))
                .unwrap_or(36.0);
            let point = openatc_core::airport::airport_point(
                airport,
                telemetry.latitude,
                telemetry.longitude,
            );
            let at_stand = airport.parking.iter().any(|stand| {
                (point.east - stand.point.east).hypot(point.north - stand.point.north) <= 50.0
            });
            let on_taxiway = !at_stand && openatc_core::airport::on_taxiway(airport, &point, 20.0);
            let runway_clearance = openatc_core::traffic::runway_clearance(
                airport,
                &runway,
                telemetry.latitude,
                telemetry.longitude,
                telemetry.heading_degrees,
                &telemetry.aircraft_icao,
                Some(span),
                0.0,
            );
            if runway_clearance.is_some_and(|clearance| !clearance.over_runway) && !on_taxiway {
                let clearance = runway_clearance.unwrap();
                let player_cells =
                    super::cells(airport, point, span / 2.0, policy.conflict_cell_meters);
                let player_blocked = segment
                    .resources
                    .iter()
                    .filter(|resource| {
                        blocked.contains(*resource) && player_cells.contains(*resource)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if !player_blocked.is_empty() {
                    let mut relaxed = blocked.clone();
                    for resource in &player_blocked {
                        relaxed.remove(resource);
                    }
                    if probe.authorise(probe.generation(), pose.id, index, &relaxed) {
                        available = true;
                        state.generated.log(
                            openatc_settings::TrafficLog::Summary,
                            format!(
                                "{} line-up clear of runway {runway}: offset {:.1} m, lateral {:.1} m, half-width {:.1} m, span {span:.1} m",
                                frame.target.callsign,
                                clearance.offset_meters.abs(),
                                clearance.lateral_half_meters,
                                clearance.width_meters / 2.0
                            ),
                        );
                    }
                }
            } else if let Some(clearance) = runway_clearance {
                state.generated.log(
                    openatc_settings::TrafficLog::Summary,
                    format!(
                        "{} held by player near runway {runway}: over_runway {}, on_taxiway {on_taxiway}, offset {:.1} m, lateral {:.1} m, half-width {:.1} m",
                        frame.target.callsign,
                        clearance.over_runway,
                        clearance.offset_meters.abs(),
                        clearance.lateral_half_meters,
                        clearance.width_meters / 2.0
                    ),
                );
            }
        }
        if !available {
            if pose.awaiting_clearance
                && matches!(pose.stage, Stage::LineUp | Stage::Takeoff)
                && state.generated.reported_holds.insert((pose.id, index))
            {
                let segment = prepared.route.segment(index);
                let owners = segment
                    .map(|segment| {
                        segment
                            .resources
                            .iter()
                            .filter_map(|resource| {
                                state
                                    .generated
                                    .world
                                    .resource_owner(resource)
                                    .map(|owner| (resource.clone(), owner))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let blocked_here = segment
                    .map(|segment| {
                        segment
                            .resources
                            .iter()
                            .filter(|resource| blocked.contains(*resource))
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let hold = segment
                    .and_then(|segment| segment.points.first())
                    .map(|point| [point.latitude, point.longitude]);
                let observed = super::external_targets(&state.generated, &state.session.telemetry)
                    .into_iter()
                    .filter(|target| target.on_ground)
                    .filter_map(|target| {
                        let distance = hold.map(|hold| {
                            openatc_core::arrival::distance_nm(
                                hold,
                                [target.latitude, target.longitude],
                            )
                        })?;
                        (distance <= 0.2).then_some((
                            target.id,
                            target.callsign,
                            distance,
                            target.track_degrees,
                        ))
                    })
                    .collect::<Vec<_>>();
                let player = hold.map(|hold| {
                    openatc_core::arrival::distance_nm(
                        hold,
                        [
                            state.session.telemetry.latitude,
                            state.session.telemetry.longitude,
                        ],
                    )
                });
                let wake = state
                    .generated
                    .departure_wake
                    .get(&pose.id)
                    .is_some_and(|seconds| *seconds > 0.0);
                state.generated.log(
                    openatc_settings::TrafficLog::Summary,
                    format!(
                        "{} takeoff held at segment {index} ({:?}): crossing {crossing}, airborne {airborne_blocked}, owners {owners:?}, blocked {blocked_here:?}, observed {observed:?}, player {player:?}, wake {wake}",
                        frame.target.callsign, pose.stage
                    ),
                );
                return Some(radio::hold_takeoff(
                    radio::Exchange {
                        speed: None,
                        id: pose.id,
                        segment: index,
                        station: clearance.station.clone(),
                        callsign: frame.target.callsign.clone(),
                        messages: vec![],
                        next: 0,
                        due: Instant::now(),
                        grant: false,
                        tune_after: None,
                        sector_transfer: None,
                        recovery_from: None,
                        rejoin: false,
                    },
                    false,
                    wake,
                ));
            }
            if pose.awaiting_clearance
                && prepared
                    .route
                    .segment(index)
                    .is_some_and(|segment| segment.stage == Stage::TaxiIn)
                && on_station(
                    &state.generated,
                    pose.id,
                    frame.target.com1_khz,
                    &clearance.station,
                )
                && state.generated.reported_holds.insert((pose.id, index))
            {
                return Some(radio::cancel_crossing(
                    radio::Exchange {
                        speed: None,
                        id: pose.id,
                        segment: index,
                        station: clearance.station.clone(),
                        callsign: frame.target.callsign.clone(),
                        messages: vec![],
                        next: 0,
                        due: Instant::now(),
                        grant: false,
                        tune_after: None,
                        sector_transfer: None,
                        recovery_from: None,
                        rejoin: false,
                    },
                    None,
                ));
            }
            continue;
        }
        let Some(frame) = state
            .generated
            .frames
            .iter()
            .find(|frame| frame.target.id == pose.id)
        else {
            continue;
        };
        if !on_station(
            &state.generated,
            pose.id,
            frame.target.com1_khz,
            &clearance.station,
        ) {
            let Some(old) = prepared
                .clearances
                .range(..=pose.segment)
                .rev()
                .flat_map(|(_, clearance)| {
                    std::iter::once(&clearance.station)
                        .chain(clearance.transfers.iter().map(|transfer| &transfer.station))
                })
                .find(|station| {
                    on_station(&state.generated, pose.id, frame.target.com1_khz, station)
                })
                .or_else(|| {
                    state
                        .stations
                        .iter()
                        .find(|station| Some(station.khz) == frame.target.com1_khz)
                })
                .cloned()
            else {
                continue;
            };
            let slots = [
                ("callsign", frame.target.callsign.clone()),
                ("station", clearance.station.name.clone()),
                (
                    "frequency",
                    format!("{:.3}", f64::from(clearance.station.khz) / 1000.0),
                ),
            ];
            return Some(radio::Exchange {
                speed: None,
                id: pose.id,
                segment: index,
                station: old,
                callsign: frame.target.callsign.clone(),
                messages: vec![
                    (true, openatc_core::dialogue::say("flight_handoff", &slots)),
                    (
                        false,
                        openatc_core::dialogue::say("pilot_frequency_readback", &slots),
                    ),
                ],
                next: 0,
                due: Instant::now(),
                grant: false,
                tune_after: Some(clearance.station.clone()),
                sector_transfer: None,
                recovery_from: None,
                rejoin: false,
            });
        }
        if let Some(end) = clearance.taxi_through
            && !state.generated.world.reserve_surface_until(
                state.generated.world.generation(),
                pose.id,
                index,
                end,
                &blocked,
            )
        {
            continue;
        }
        return Some(radio::Exchange {
            speed: None,
            id: pose.id,
            segment: index,
            station: clearance.station.clone(),
            callsign: frame.target.callsign.clone(),
            messages: vec![
                (true, clearance.instruction.clone()),
                (false, clearance.readback.clone()),
            ],
            next: 0,
            due: Instant::now(),
            grant: true,
            tune_after: None,
            sector_transfer: None,
            recovery_from: None,
            rejoin: false,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::traffic::motion::{Segment, Waypoint};

    fn point(east: f64, altitude: f64) -> Waypoint {
        Waypoint {
            latitude: 0.0,
            longitude: east / 111_320.0,
            altitude_feet: altitude,
        }
    }
    fn fixture() -> (EngineState, Prepared) {
        let mut state = crate::test_state();
        state.settings.generated_traffic = true;
        state.settings.hear_traffic_radio = false;
        state.session.telemetry.traffic = Some(vec![]);
        state.session.telemetry.position_valid = true;
        state.session.telemetry.longitude = -0.02;
        state.session.telemetry.on_ground = true;
        let phases = [
            Stage::LineUp,
            Stage::Takeoff,
            Stage::Climb,
            Stage::Cruise,
            Stage::Descent,
            Stage::Approach,
            Stage::Rollout,
            Stage::TaxiIn,
            Stage::Turnaround,
        ];
        let vertices = [
            point(100.0, 0.0),
            point(200.0, 0.0),
            point(1000.0, 0.0),
            point(5000.0, 1000.0),
            point(8000.0, 1000.0),
            point(11000.0, 500.0),
            point(13000.0, 0.0),
            point(14000.0, 0.0),
            point(14500.0, 0.0),
        ];
        let mut clearances = BTreeMap::new();
        let mut recovery = BTreeMap::new();
        let segments = phases
            .iter()
            .enumerate()
            .map(|(index, stage)| {
                let end = vertices[(index + 1).min(vertices.len() - 1)];
                if *stage != Stage::Turnaround {
                    let (service, khz) = match stage {
                        Stage::LineUp | Stage::Takeoff | Stage::Approach | Stage::Rollout => {
                            ("Tower", 120_950)
                        }
                        Stage::Climb | Stage::Descent => ("Approach", 124_500),
                        Stage::Cruise => ("Center", 118_675),
                        Stage::TaxiIn => ("Ground", 121_705),
                        _ => unreachable!(),
                    };
                    let station = Station {
                        name: format!("Test {service}"),
                        service: service.into(),
                        airport: "TEST".into(),
                        khz,
                        ..Default::default()
                    };
                    state.stations.push(station.clone());
                    clearances.insert(
                        index,
                        Clearance {
                            amended_route: None,
                            station,
                            transfers: Vec::new(),
                            coordinated_recovery: Vec::new(),
                            instruction: format!("Controller phase {index}."),
                            readback: format!("Pilot phase {index}."),
                            initial_climb: (index == 1).then_some(2),
                            taxi_through: None,
                            airborne_through: None,
                            landing_rollout: None,
                            missed: None,
                            holding: None,
                        },
                    );
                }
                if !stage.on_ground() {
                    let mut north = end;
                    north.latitude += 1000.0 / 111_320.0;
                    let mut east = end;
                    east.longitude += 1000.0 / 111_320.0;
                    let mut corner = east;
                    corner.latitude = north.latitude;
                    recovery.insert(
                        index,
                        Arc::new(
                            Route::compile(
                                vec![Segment {
                                    stage: Stage::Holding,
                                    points: vec![end, east, corner, north, end],
                                    knots: 80.0,
                                    resources: vec![],
                                    dwell_seconds: 0.0,
                                }],
                                4000.0,
                                4000.0,
                            )
                            .unwrap(),
                        ),
                    );
                }
                Segment {
                    stage: *stage,
                    points: if *stage == Stage::Turnaround {
                        vec![end]
                    } else {
                        vec![vertices[index], end]
                    },
                    knots: if matches!(stage, Stage::LineUp | Stage::TaxiIn) {
                        10.0
                    } else if *stage == Stage::Turnaround {
                        0.0
                    } else {
                        80.0
                    },
                    resources: if matches!(stage, Stage::LineUp | Stage::Takeoff | Stage::Rollout) {
                        vec!["RUNWAY/TEST/09/27".into()]
                    } else if stage.on_ground() {
                        vec![format!("PHASE/{index}")]
                    } else {
                        vec![]
                    },
                    dwell_seconds: 0.0,
                }
            })
            .collect();
        let prepared = Prepared {
            spacing_floor: None,
            amendment: None,
            airspeeds: BTreeMap::new(),
            route: Arc::new(Route::compile(segments, 4000.0, 4000.0).unwrap()),
            recovery,
            clearances,
            vacated_reports: BTreeMap::from([(
                7,
                RunwayReport {
                    airport: "TEST".into(),
                    runway: "09".into(),
                },
            )]),
        };
        let mut runtime = Runtime {
            epoch: Some(state.session_generation),
            policy: Some(
                parse_operations(include_str!("../../../../traffic/operations.toml")).unwrap(),
            ),
            ..Default::default()
        };
        runtime.airport = Some(Arc::new(Airport {
            icao: "TEST".into(),
            runways: vec![openatc_core::airport::Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                width: 45.0,
                first: Point {
                    east: 0.0,
                    ..Default::default()
                },
                second: Point {
                    east: 13500.0,
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        }));
        runtime.world.insert(
            0,
            1,
            Arc::new(
                Route::compile(
                    vec![Segment {
                        stage: Stage::TaxiOut,
                        points: vec![point(0.0, 0.0), vertices[0]],
                        knots: 10.0,
                        resources: vec!["HOLD".into()],
                        dwell_seconds: 0.0,
                    }],
                    4000.0,
                    4000.0,
                )
                .unwrap(),
            ),
        );
        runtime.world.authorise(0, 1, 0, &BTreeSet::new());
        runtime.world.advance(0.0, false);
        runtime.world.advance(30.0, false);
        runtime.tower_ready.insert(1);
        runtime.frames.push(GeneratedTraffic {
            target: TrafficTarget {
                id: 1,
                callsign: "TEST123".into(),
                com1_khz: Some(120_950),
                ..Default::default()
            },
            vertical_speed_fpm: 0.0,
            body_heading_degrees: None,
            model: Arc::new(openatc_core::traffic::csl::Model {
                id: "fixture".into(),
                objects: vec![],
                vertical_offset: None,
                matches: vec![],
            }),
            animation: Animation::default(),
        });
        state.generated = runtime;
        (state, prepared)
    }

    #[test]
    fn completed_taxi_handoff_precedes_gate_calls_and_reaches_takeoff() {
        for combined in [false, true] {
            let (mut state, mut prepared) = fixture();
            state.generated.tower_ready.clear();
            let ground = Station {
                airport: "TEST".into(),
                name: "Test Ground".into(),
                service: "Ground".into(),
                khz: 121_705,
                ..Default::default()
            };
            state.stations.push(ground.clone());
            if combined {
                let departure = Station {
                    airport: "TEST".into(),
                    name: "Test combined Departure".into(),
                    service: "Departure".into(),
                    services: vec!["Tower".into()],
                    khz: 119_100,
                    ..Default::default()
                };
                for index in [0, 1] {
                    prepared.clearances.get_mut(&index).unwrap().station = departure.clone();
                }
                state.stations.push(departure);
            }
            let departure_khz = prepared.clearances[&0].station.khz;
            state.generated.frames[0].target.com1_khz = Some(ground.khz);
            state
                .generated
                .flights
                .insert(1, Arc::new(prepared.clone()));
            let order = openatc_core::state::TaxiClearance {
                airport: "TEST".into(),
                hold_short_runway: "09".into(),
                ..Default::default()
            };
            state.generated.orders.insert(1, vec![(0, order.clone())]);
            // A gate call is continuously eligible while the leading aircraft awaits Tower.
            let gate = Arc::new(
                Route::compile(
                    vec![Segment {
                        stage: Stage::Parked,
                        points: vec![point(-500.0, 0.0)],
                        knots: 0.0,
                        resources: vec!["GATE/2".into()],
                        dwell_seconds: 5.0,
                    }],
                    4000.0,
                    4000.0,
                )
                .unwrap(),
            );
            assert!(state.generated.world.insert(0, 2, gate));
            let mut frame = state.generated.frames[0].clone();
            frame.target.id = 2;
            frame.target.callsign = "TEST456".into();
            state.generated.frames.push(frame);
            state.generated.flights.insert(2, Arc::new(prepared));
            state.generated.orders.insert(2, vec![(0, order)]);
            radio::tick(&mut state);
            let exchange = state
                .generated
                .exchange
                .as_ref()
                .expect("Ground must hand off the queued departure");
            assert_eq!(
                exchange.id, 1,
                "A gate call starved the departure holding point"
            );
            assert_eq!(exchange.station.khz, ground.khz);
            assert_eq!(exchange.tune_after.as_ref().unwrap().khz, departure_khz);
            assert_eq!(state.generated.frames[0].target.com1_khz, Some(ground.khz));
            assert!(!state.generated.active_flights.contains(&1));
            let mut takeoff = false;
            for second in 31..180 {
                if let Some(exchange) = &mut state.generated.exchange {
                    exchange.due = Instant::now();
                }
                radio::tick(&mut state);
                state.session.telemetry.simulation_seconds = Some(f64::from(second));
                super::super::advance(&mut state.generated, &state.session.telemetry);
                let pose = state
                    .generated
                    .world
                    .snapshot()
                    .into_iter()
                    .find(|pose| pose.id == 1)
                    .unwrap();
                if pose.stage == Stage::Takeoff && pose.knots > 0.0 {
                    assert!(state.generated.tower_ready.contains(&1));
                    assert!(state.generated.approved_orders.contains(&(1, 1)));
                    takeoff = true;
                    break;
                }
            }
            assert!(
                takeoff,
                "Departure never progressed beyond its holding point: {:?}",
                state.generated.world.snapshot()
            );
        }
    }

    #[test]
    fn airborne_body_heading_keeps_wind_drift_out_of_tcas_ground_track() {
        let (mut state, mut prepared) = fixture();
        let speed = openatc_core::traffic::trajectory::Airspeed {
            indicated_knots: 150.0,
            atmosphere: openatc_core::traffic::air::Atmosphere::surface(1013.25, 15.0, 0.0)
                .unwrap(),
            wind_from: 0.0,
            wind_knots: 20.0,
        };
        prepared.airspeeds = (2..=5).map(|index| (index, speed)).collect();
        let mut invalid = prepared.clone();
        invalid.airspeeds.insert(1, speed);
        assert!(install(&mut state.generated, 1, &invalid).is_err());
        assert!(state.generated.active_flights.is_empty());
        install(&mut state.generated, 1, &prepared).unwrap();
        let mut saw_airborne = false;
        let mut saw_takeoff = false;
        for second in 31..1000 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            state.session.telemetry.simulation_seconds = Some(f64::from(second));
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            let frame = &state.generated.frames[0];
            saw_takeoff |= pose.stage == Stage::Takeoff;
            let observer = openatc_core::state::Telemetry {
                latitude: frame.target.latitude,
                longitude: frame.target.longitude,
                position_valid: true,
                ..Default::default()
            };
            let visible = super::super::display::select(
                &state.generated.frames,
                &observer,
                &BTreeSet::new(),
                state.generated.policy.as_ref().unwrap(),
            );
            assert!(
                visible.iter().any(|drawn| drawn.target.id == 1),
                "Aircraft disappeared during {:?}",
                pose.stage
            );
            if pose.knots > 0.0 {
                assert!(
                    openatc_core::navigation::turn(pose.heading, frame.target.track_degrees).abs()
                        < 1e-9
                );
                let body = frame.body_heading_degrees.unwrap();
                if pose.stage.on_ground() {
                    assert!(openatc_core::navigation::turn(pose.heading, body).abs() < 1e-9);
                } else {
                    let expected = openatc_core::traffic::air::heading_from_ground(
                        pose.knots,
                        pose.heading,
                        0.0,
                        20.0,
                    )
                    .unwrap();
                    assert!(openatc_core::navigation::turn(expected, body).abs() < 1e-9);
                    assert!(openatc_core::navigation::turn(pose.heading, body).abs() > 1.0);
                    saw_airborne = true;
                }
            }
            if pose.complete && pose.stage == Stage::Turnaround {
                break;
            }
        }
        assert!(saw_takeoff && saw_airborne);
        assert_eq!(state.generated.world.snapshot()[0].stage, Stage::Turnaround);
        assert!(state.generated.world.snapshot()[0].complete);
    }

    #[test]
    fn the_second_taxiway_does_not_jump_the_holding_queue() {
        let route = |start: f64, end: f64| {
            Arc::new(
                Route::compile(
                    vec![Segment {
                        stage: Stage::TaxiOut,
                        points: vec![point(start, 0.0), point(end, 0.0)],
                        knots: 10.0,
                        resources: vec![],
                        dwell_seconds: 0.0,
                    }],
                    4000.0,
                    4000.0,
                )
                .unwrap(),
            )
        };
        let mut world = openatc_core::traffic::motion::World::default();
        assert!(world.insert(0, 1, route(1000.0, 0.0)));
        assert!(world.insert(0, 2, route(500.0, -500.0)));
        assert!(world.authorise(0, 1, 0, &BTreeSet::new()));
        world.advance(0.0, false);
        for step in 1..=300 {
            world.advance(f64::from(step), false);
        }
        assert!(
            world
                .snapshot()
                .iter()
                .any(|pose| pose.id == 1 && pose.complete)
        );
        let mut runtime = Runtime {
            world,
            ..Default::default()
        };
        let airport = Airport {
            icao: "KTEST".into(),
            runways: vec![openatc_core::airport::Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                width: 45.0,
                first: Point {
                    east: -1000.0,
                    ..Default::default()
                },
                second: Point {
                    east: 2000.0,
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        for id in [1, 2] {
            runtime.orders.insert(
                id,
                vec![(
                    0,
                    openatc_core::state::TaxiClearance {
                        hold_short_runway: "09".into(),
                        ..Default::default()
                    },
                )],
            );
        }
        assert_eq!(lineup_ahead(&mut runtime, 1, &airport, "09"), None);
        assert!(runtime.world.authorise(0, 2, 0, &BTreeSet::new()));
        for step in 301..=600 {
            runtime.world.advance(f64::from(step), false);
        }
        assert!(
            runtime
                .world
                .snapshot()
                .iter()
                .any(|pose| pose.id == 2 && pose.complete)
        );
        assert_eq!(lineup_ahead(&mut runtime, 2, &airport, "09"), Some(1));
    }

    #[test]
    fn a_queued_generated_follower_never_withholds_the_departure_ahead() {
        let (mut state, prepared) = fixture();
        state.session.telemetry.traffic = Some(Vec::new());
        let model = state.generated.frames[0].model.clone();
        state.generated.frames.push(GeneratedTraffic {
            target: TrafficTarget {
                id: 2,
                callsign: "FOLLOW123".into(),
                aircraft: String::new(),
                latitude: 30.0 / 111_320.0,
                longitude: -100.0 / 111_320.0,
                altitude_feet: 0.0,
                track_degrees: 90.0,
                on_ground: true,
                ground_speed_knots: Some(0.0),
                wingspan_meters: Some(30.0),
                ..Default::default()
            },
            vertical_speed_fpm: 0.0,
            body_heading_degrees: Some(90.0),
            model,
            animation: Animation::default(),
        });
        install(&mut state.generated, 1, &prepared).unwrap();
        let all = super::super::observed_targets(&state.generated, &state.session.telemetry);
        let external = super::super::external_targets(&state.generated, &state.session.telemetry);
        assert!(
            state.generated.crossing_blocked(1, 1, &all),
            "fixture must put the follower inside the runway protection envelope"
        );
        assert!(!state.generated.crossing_blocked(1, 1, &external));
        let mut airborne = false;
        for second in 31..900 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            state.session.telemetry.simulation_seconds = Some(f64::from(second));
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            airborne |= pose.stage == Stage::Climb && !pose.awaiting_clearance;
            if airborne {
                break;
            }
        }
        assert!(
            airborne,
            "departure was withheld by the queued generated aircraft: poses={:?}, holds={:?}",
            state.generated.world.snapshot(),
            state.generated.reported_holds
        );
        assert!(!state.generated.cancelled_takeoffs.contains_key(&1));
    }

    #[test]
    fn surface_movement_is_not_gated_on_the_tcas_feed() {
        let (mut state, prepared) = fixture();
        state.session.telemetry.traffic = None;
        install(&mut state.generated, 1, &prepared).unwrap();
        let mut moved = false;
        for second in 31..300 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            state.session.telemetry.simulation_seconds = Some(f64::from(second));
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            moved |= pose.stage == Stage::Takeoff;
            if moved {
                break;
            }
        }
        assert!(
            moved,
            "the aircraft never left the line-up without a TCAS feed: {:?}",
            state.generated.world.snapshot()
        );
    }

    #[test]
    fn sector_contact_waits_for_readback_and_checkin_without_adding_movement_authority() {
        for replace_route in [false, true] {
            let (mut state, mut prepared) = fixture();
            let sender = prepared.clearances[&3].station.clone();
            let receiver = Station {
                name: "Next Center".into(),
                coverage: vec![openatc_core::stations::Coverage {
                    floor: 0.0,
                    ceiling: 50000.0,
                    points: vec![[-1.0, -1.0], [-1.0, 1.0], [1.0, 1.0], [1.0, -1.0]],
                }],
                ..sender.clone()
            };
            let remaining = prepared.route.nominal_seconds(3, 3).unwrap() / 2.0;
            prepared
                .clearances
                .get_mut(&3)
                .unwrap()
                .transfers
                .push(SectorTransfer {
                    remaining_seconds: remaining,
                    station: receiver.clone(),
                });
            install(&mut state.generated, 1, &prepared).unwrap();
            let generation = state.generated.world.generation();
            for second in 31..500 {
                let pose = state.generated.world.snapshot()[0].clone();
                if pose.awaiting_clearance && pose.segment <= 3 {
                    assert!(state.generated.world.authorise(
                        generation,
                        1,
                        pose.segment,
                        &BTreeSet::new()
                    ));
                }
                if pose.segment < 3 {
                    let _ = state.generated.world.authorise_continuation(
                        generation,
                        1,
                        pose.segment + 1,
                        &BTreeSet::new(),
                    );
                }
                state.generated.world.advance(f64::from(second), false);
                let pose = state.generated.world.snapshot()[0].clone();
                if pose.segment == 3 && !pose.awaiting_clearance {
                    break;
                }
            }
            assert_eq!(state.generated.world.snapshot()[0].stage, Stage::Cruise);
            state.generated.frames[0].target.com1_khz = Some(sender.khz);
            state.generated.controllers.insert(1, sender.clone());
            state.generated.approved_orders.insert((1, 3));
            let authority = state.generated.approved_orders.clone();
            let contact = exchange(&mut state).unwrap();
            assert_eq!(contact.station.name, sender.name);
            assert!(!contact.grant && contact.sector_transfer.is_some());
            state.generated.exchange = Some(contact);
            radio::tick(&mut state);
            assert_eq!(state.generated.controllers[&1].name, sender.name);
            if replace_route {
                state
                    .generated
                    .flights
                    .insert(1, Arc::new(prepared.clone()));
            }
            for _ in 0..2 {
                if let Some(exchange) = &mut state.generated.exchange {
                    exchange.due = Instant::now();
                }
                radio::tick(&mut state);
            }
            assert_eq!(state.generated.approved_orders, authority);
            if replace_route {
                assert_eq!(state.generated.controllers[&1].name, sender.name);
                assert!(!state.generated.sector_transfers.contains_key(&(1, 3)));
                assert!(state.generated.pending_sector_checkins.is_empty());
            } else {
                assert_eq!(state.generated.controllers[&1].name, receiver.name);
                assert_eq!(state.generated.frames[0].target.com1_khz, Some(sender.khz));
                assert_eq!(state.generated.sector_transfers[&(1, 3)], 1);
                let checkin = exchange(&mut state).unwrap();
                assert_eq!(checkin.station.name, receiver.name);
                assert!(!checkin.messages[0].0 && checkin.messages[1].0);
                assert!(!checkin.grant && checkin.tune_after.is_none());
                assert!(state.generated.pending_sector_checkins.is_empty());
            }
        }
    }

    #[test]
    fn queued_flight_cannot_apply_its_takeoff_contract_to_a_surface_readback() {
        let (mut state, prepared) = fixture();
        state.generated.flights.insert(1, Arc::new(prepared));
        assert!(!state.generated.active_flights.contains(&1));
        state.generated.exchange = Some(radio::Exchange {
            speed: None,
            id: 1,
            segment: 1,
            station: state.stations[0].clone(),
            callsign: "TEST123".into(),
            messages: vec![],
            next: 0,
            due: Instant::now(),
            grant: true,
            tune_after: None,
            sector_transfer: None,
            recovery_from: None,
            rejoin: false,
        });
        radio::tick(&mut state);
        assert!(state.generated.approved_orders.contains(&(1, 1)));
        assert!(!state.generated.active_flights.contains(&1));
        assert!(state.generated.exchange.is_none());
        assert!(state.generated.world.snapshot()[0].complete);
    }

    #[tokio::test]
    async fn worker_clock_reset_cannot_restore_old_airborne_grants() {
        let (mut state, prepared) = fixture();
        install(&mut state.generated, 1, &prepared).unwrap();
        let generation = state.generated.world.generation();
        assert!(
            state
                .generated
                .world
                .authorise(generation, 1, 0, &BTreeSet::new())
        );
        let mut reached = false;
        for second in 31..600 {
            let pose = state.generated.world.snapshot()[0].clone();
            if pose.awaiting_clearance && pose.segment < 3 {
                assert!(state.generated.world.authorise(
                    generation,
                    1,
                    pose.segment,
                    &BTreeSet::new()
                ));
            }
            if !pose.awaiting_clearance && pose.segment < 3 {
                assert!(state.generated.world.authorise_continuation(
                    generation,
                    1,
                    pose.segment + 1,
                    &BTreeSet::new()
                ));
            }
            state.generated.world.advance(f64::from(second), false);
            if state.generated.world.snapshot()[0].segment == 3 {
                reached = true;
                break;
            }
        }
        assert!(reached);
        state
            .generated
            .approved_orders
            .extend([(1, 3), (1, 4), (2, 0)]);
        state.generated.exchange = Some(radio::Exchange {
            speed: None,
            id: 1,
            segment: 4,
            station: prepared.clearances[&4].station.clone(),
            callsign: "TEST123".into(),
            messages: vec![],
            next: 0,
            due: Instant::now(),
            grant: true,
            tune_after: None,
            sector_transfer: None,
            recovery_from: None,
            rejoin: false,
        });
        state.session.telemetry.simulation_seconds = Some(10000.0);
        let shared = Arc::new(tokio::sync::RwLock::new(state));
        super::super::simulate(shared.clone()).await;
        let state = shared.read().await;
        assert_eq!(state.generated.approved_orders, BTreeSet::from([(2, 0)]));
        assert!(state.generated.exchange.is_none());
        assert_eq!(state.generated.world.snapshot()[0].segment, 3);
        assert!(state.generated.simulation_job.is_none());
    }

    #[test]
    fn one_airborne_readback_covers_its_path_without_duplicate_radio_calls() {
        for slot in [2, 3] {
            let (mut state, old) = fixture();
            let remap = |index| if index > slot { index + 2 } else { index };
            let mut clearances = old
                .clearances
                .iter()
                .map(|(index, value)| (remap(*index), value.clone()))
                .collect::<BTreeMap<_, _>>();
            clearances.insert(slot + 1, old.clearances[&slot].clone());
            clearances.insert(slot + 2, old.clearances[&slot].clone());
            if slot == 2 {
                clearances.get_mut(&1).unwrap().initial_climb = Some(4);
            } else {
                clearances.get_mut(&slot).unwrap().airborne_through = Some(slot + 2);
            }
            let mut recovery = old
                .recovery
                .iter()
                .filter(|(index, _)| **index != slot)
                .map(|(index, value)| (remap(*index), value.clone()))
                .collect::<BTreeMap<_, _>>();
            let mut segments = Vec::new();
            for index in 0..512 {
                let Some(segment) = old.route.segment(index) else {
                    break;
                };
                if index != slot {
                    segments.push(segment.clone());
                    continue;
                }
                for part in 0..3 {
                    let start = segment.points[0];
                    let end = *segment.points.last().unwrap();
                    let at = |fraction: f64| Waypoint {
                        latitude: start.latitude + (end.latitude - start.latitude) * fraction,
                        longitude: start.longitude + (end.longitude - start.longitude) * fraction,
                        altitude_feet: start.altitude_feet
                            + (end.altitude_feet - start.altitude_feet) * fraction,
                    };
                    let from = at(f64::from(part) / 3.0);
                    let to = at(f64::from(part + 1) / 3.0);
                    segments.push(Segment {
                        points: vec![from, to],
                        ..segment.clone()
                    });
                    let hold = openatc_core::traffic::holding::Pattern {
                        fix: to,
                        inbound_degrees: 90.0,
                        right: true,
                        ground_knots: 80.0,
                        leg_seconds: 60.0,
                        bank_degrees: 25.0,
                        turn_rate_degrees_sec: 3.0,
                    }
                    .compile(|_, _| true)
                    .unwrap();
                    recovery.insert(
                        slot + part as usize,
                        Arc::new(Route::compile(vec![hold], 4000.0, 4000.0).unwrap()),
                    );
                }
            }
            let prepared = Prepared {
                spacing_floor: None,
                amendment: None,
                airspeeds: old
                    .airspeeds
                    .iter()
                    .map(|(&index, speed)| (remap(index), *speed))
                    .collect(),
                route: Arc::new(Route::compile(segments, 4000.0, 4000.0).unwrap()),
                clearances,
                recovery,
                vacated_reports: old
                    .vacated_reports
                    .iter()
                    .map(|(index, value)| (remap(*index), value.clone()))
                    .collect(),
            };
            let mut invalid = prepared.clone();
            if slot == 2 {
                invalid.clearances.get_mut(&1).unwrap().initial_climb = Some(5);
            } else {
                invalid.clearances.get_mut(&slot).unwrap().airborne_through = Some(slot + 3);
            }
            assert!(install(&mut state.generated, 1, &invalid).is_err());
            assert!(state.generated.active_flights.is_empty());
            let mut invalid = prepared.clone();
            invalid.clearances.get_mut(&(slot + 1)).unwrap().station.khz = 123_450;
            assert!(install(&mut state.generated, 1, &invalid).is_err());
            assert!(state.generated.active_flights.is_empty());
            install(&mut state.generated, 1, &prepared).unwrap();
            state.settings.hear_traffic_radio = true;
            state.session.telemetry.com1_khz = prepared.clearances[&slot].station.khz;
            let mut crossed = BTreeSet::new();
            for second in 31..1000 {
                if let Some(exchange) = &mut state.generated.exchange {
                    exchange.due = Instant::now();
                }
                super::super::radio_tick(&mut state);
                state.session.telemetry.simulation_seconds = Some(f64::from(second));
                super::super::advance(&mut state.generated, &state.session.telemetry);
                let pose = &state.generated.world.snapshot()[0];
                assert!(
                    !pose.recovering,
                    "Accepted span unexpectedly fell back at {}",
                    pose.segment
                );
                crossed.insert(pose.segment);
                if pose.stage == Stage::Turnaround && pose.complete {
                    break;
                }
            }
            assert!((slot..=slot + 2).all(|index| crossed.contains(&index)));
            assert_eq!(state.generated.world.snapshot()[0].stage, Stage::Turnaround);
            assert!(state.generated.world.snapshot()[0].complete);
            for expected in [
                format!("Controller phase {slot}."),
                format!("Pilot phase {slot}."),
            ] {
                assert_eq!(
                    state
                        .session
                        .transcript
                        .iter()
                        .filter(|entry| entry.text == expected)
                        .count(),
                    usize::from(slot == 3),
                    "{expected}"
                );
            }
        }
    }

    #[test]
    fn one_taxi_readback_covers_only_its_explicit_contiguous_route() {
        let (mut state, mut prepared) = fixture();
        let mut segments: Vec<_> = (0..32)
            .map_while(|index| prepared.route.segment(index).cloned())
            .collect();
        segments[7].points = vec![point(14000.0, 0.0), point(14250.0, 0.0)];
        segments.insert(
            8,
            Segment {
                stage: Stage::TaxiIn,
                points: vec![point(14250.0, 0.0), point(14500.0, 0.0)],
                knots: 10.0,
                resources: vec!["PHASE/8".into()],
                dwell_seconds: 0.0,
            },
        );
        prepared.route = Arc::new(Route::compile(segments, 4000.0, 4000.0).unwrap());
        prepared
            .clearances
            .insert(8, prepared.clearances[&7].clone());
        prepared.clearances.get_mut(&7).unwrap().taxi_through = Some(8);
        prepared.validate().unwrap();
        let mut invalid = prepared.clone();
        invalid.clearances.get_mut(&7).unwrap().taxi_through = Some(9);
        assert!(
            invalid.validate().is_err(),
            "Taxi permission cannot include parking completion"
        );
        invalid = prepared.clone();
        invalid.clearances.get_mut(&8).unwrap().station.khz = 121_950;
        assert!(
            invalid.validate().is_err(),
            "Taxi permission cannot bypass a controller change"
        );
        invalid = prepared.clone();
        invalid.clearances.get_mut(&8).unwrap().instruction = "A separate taxi instruction".into();
        assert!(
            invalid.validate().is_err(),
            "Taxi permission cannot absorb another instruction"
        );
        install(&mut state.generated, 1, &prepared).unwrap();
        acknowledge(&mut state.generated, 1, 7);
        assert!(
            !state.generated.approved_orders.contains(&(1, 8)),
            "Preparing a readback must not grant its route"
        );
        let mut taxi_seen = false;
        let mut corridor_reserved = false;
        for second in 31..1100 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            if state
                .generated
                .exchange
                .as_ref()
                .is_some_and(|exchange| exchange.segment == 7 && exchange.grant)
            {
                corridor_reserved = true;
                assert!(!state.generated.approved_orders.contains(&(1, 7)));
                assert!(state.generated.world.snapshot()[0].awaiting_clearance);
                let mut probe = state.generated.world.clone();
                let other = Route::compile(
                    vec![Segment {
                        stage: Stage::TaxiIn,
                        points: vec![point(14250.0, 0.0), point(14500.0, 0.0)],
                        knots: 10.0,
                        resources: vec!["PHASE/8".into()],
                        dwell_seconds: 0.0,
                    }],
                    4000.0,
                    4000.0,
                )
                .unwrap();
                assert!(probe.insert(0, 2, Arc::new(other)));
                assert!(
                    !probe.reserve(0, 2, 0, &BTreeSet::new()),
                    "Full taxi corridor must be reserved while its readback is pending"
                );
            }
            if state
                .generated
                .exchange
                .as_ref()
                .is_some_and(|exchange| exchange.segment == 8 && exchange.grant)
            {
                panic!("A covered taxi edge generated a redundant clearance");
            }
            state.session.telemetry.simulation_seconds = Some(f64::from(second));
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            if pose.segment == 8 {
                taxi_seen = true;
                assert!(state.generated.approved_orders.contains(&(1, 7)));
                assert!(state.generated.approved_orders.contains(&(1, 8)));
            }
            if pose.complete && pose.stage == Stage::Turnaround {
                break;
            }
        }
        let pose = &state.generated.world.snapshot()[0];
        assert!(corridor_reserved && taxi_seen && pose.complete && pose.stage == Stage::Turnaround);
    }

    #[test]
    fn landing_permission_checks_player_and_native_runway_occupants() {
        let (mut state, prepared) = fixture();
        install(&mut state.generated, 1, &prepared).unwrap();
        for second in 31..900 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            state.session.telemetry.simulation_seconds = Some(f64::from(second));
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            if pose.stage == Stage::Approach
                && pose.seconds_to_limit
                    <= state
                        .generated
                        .policy
                        .as_ref()
                        .unwrap()
                        .flight_clearance_lead_seconds
            {
                break;
            }
        }
        assert_eq!(state.generated.world.snapshot()[0].stage, Stage::Approach);
        assert!(!state.generated.approved_orders.contains(&(1, 6)));
        state.generated.exchange = None;
        let occupant = TrafficTarget {
            id: 88,
            aircraft: "B77W".into(),
            callsign: "ARR88".into(),
            longitude: 12700.0 / 111_320.0,
            track_degrees: 90.0,
            on_ground: true,
            ..Default::default()
        };
        state.session.telemetry.traffic = Some(vec![occupant.clone()]);
        let blocked =
            super::super::external_surface_cells(&state.generated, &state.session.telemetry);
        assert!(blocked.contains("RUNWAY/TEST/09/27"));
        assert!(
            !state
                .generated
                .world
                .clone()
                .authorise_continuation(0, 1, 6, &blocked)
        );
        assert!(
            exchange(&mut state).is_none(),
            "Native runway occupant must prevent landing permission"
        );
        state.session.telemetry.traffic = Some(vec![]);
        assert!(
            exchange(&mut state).is_some_and(|exchange| exchange.segment == 6 && exchange.grant)
        );
        state.session.telemetry.longitude = occupant.longitude;
        state.session.telemetry.heading_degrees = occupant.track_degrees;
        assert!(
            exchange(&mut state).is_none(),
            "Player runway occupant must prevent landing permission"
        );
    }

    #[test]
    fn active_taxi_edges_cannot_inherit_a_previous_segment_permission() {
        let (mut state, _) = fixture();
        state.generated.world = Default::default();
        let route = Route::compile(
            (0..2)
                .map(|index| Segment {
                    stage: Stage::TaxiIn,
                    points: vec![
                        point(f64::from(index) * 100.0, 0.0),
                        point(f64::from(index + 1) * 100.0, 0.0),
                    ],
                    knots: 10.0,
                    resources: vec![],
                    dwell_seconds: 0.0,
                })
                .collect(),
            4000.0,
            4000.0,
        )
        .unwrap();
        assert!(state.generated.world.insert(0, 1, Arc::new(route)));
        state.generated.active_flights.insert(1);
        state.generated.approved_orders.insert((1, 0));
        for second in 0..80 {
            state.session.telemetry.simulation_seconds = Some(f64::from(second));
            super::super::advance(&mut state.generated, &state.session.telemetry);
        }
        let pose = &state.generated.world.snapshot()[0];
        assert_eq!(pose.segment, 1);
        assert!(pose.awaiting_clearance);
        assert_eq!(pose.position, point(100.0, 0.0));
    }

    #[test]
    fn complete_arrival_crossing_waits_for_readback_and_resumes_without_legacy_orders() {
        let (mut state, mut prepared) = fixture();
        let mut segments: Vec<_> = (0..32)
            .map_while(|index| prepared.route.segment(index).cloned())
            .collect();
        segments.pop();
        segments.extend([
            Segment {
                stage: Stage::Crossing,
                points: vec![point(14500.0, 0.0), point(15000.0, 0.0)],
                knots: 10.0,
                resources: vec!["RUNWAY/TEST/18/36".into()],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::TaxiIn,
                points: vec![point(15000.0, 0.0), point(15500.0, 0.0)],
                knots: 10.0,
                resources: vec![],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::Turnaround,
                points: vec![point(15500.0, 0.0)],
                knots: 0.0,
                resources: vec![],
                dwell_seconds: 0.0,
            },
        ]);
        prepared.route = Arc::new(Route::compile(segments, 4000.0, 4000.0).unwrap());
        for index in [8, 9] {
            prepared
                .clearances
                .insert(index, prepared.clearances[&7].clone());
        }
        let mut crossing_span = prepared.clone();
        crossing_span.clearances.get_mut(&7).unwrap().taxi_through = Some(8);
        assert!(
            crossing_span
                .validate()
                .unwrap_err()
                .contains("permission or controller limit"),
            "A taxi clearance cannot authorise a runway crossing"
        );
        prepared.vacated_reports.insert(
            9,
            RunwayReport {
                airport: "TEST".into(),
                runway: "18".into(),
            },
        );
        Arc::make_mut(state.generated.airport.as_mut().unwrap())
            .runways
            .push(openatc_core::airport::Runway {
                first_name: "18".into(),
                second_name: "36".into(),
                width: 45.0,
                first: Point {
                    east: 14750.0,
                    north: 1000.0,
                    height: 0.0,
                },
                second: Point {
                    east: 14750.0,
                    north: -1000.0,
                    height: 0.0,
                },
                ..Default::default()
            });
        install(&mut state.generated, 1, &prepared).unwrap();
        assert!(!state.generated.orders.contains_key(&1));
        for second in 31..1200 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            state.session.telemetry.simulation_seconds = Some(f64::from(second));
            super::super::advance(&mut state.generated, &state.session.telemetry);
            if state.generated.world.snapshot()[0].segment == 8 {
                break;
            }
        }
        let stopped = state.generated.world.snapshot()[0].clone();
        assert_eq!(stopped.segment, 8);
        assert!(stopped.awaiting_clearance);
        for _ in 0..8 {
            state.session.telemetry.simulation_seconds =
                Some(state.session.telemetry.simulation_seconds.unwrap() + 1.0);
            super::super::advance(&mut state.generated, &state.session.telemetry);
            assert_eq!(
                state.generated.world.snapshot()[0].position,
                stopped.position
            );
        }
        assert!(!state.generated.approved_orders.contains(&(1, 8)));
        let final_traffic = TrafficTarget {
            id: 88,
            aircraft: "A320".into(),
            callsign: "ARR88".into(),
            latitude: 1500.0 / 111_320.0,
            longitude: 14750.0 / 111_320.0,
            altitude_feet: 200.0,
            track_degrees: 180.0,
            ground_speed_knots: Some(120.0),
            on_ground: false,
            ..Default::default()
        };
        state.session.telemetry.traffic = Some(vec![final_traffic.clone()]);
        for _ in 0..8 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
        }
        assert!(
            !state.generated.approved_orders.contains(&(1, 8)),
            "Predicted arrival must block the crossing instruction"
        );
        state.session.telemetry.traffic = Some(vec![]);
        // Let the controller and pilot speak, then introduce final traffic before accepting the readback.
        for _ in 0..8 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            if state.generated.exchange.as_ref().is_some_and(|exchange| {
                exchange.segment == 8 && exchange.grant && exchange.next == exchange.messages.len()
            }) {
                break;
            }
        }
        assert!(
            state
                .generated
                .exchange
                .as_ref()
                .is_some_and(|exchange| exchange.segment == 8 && exchange.grant)
        );
        state.session.telemetry.traffic = Some(vec![final_traffic.clone()]);
        state.generated.exchange.as_mut().unwrap().due = Instant::now();
        super::super::radio_tick(&mut state);
        assert!(!state.generated.approved_orders.contains(&(1, 8)));
        assert!(
            !state.generated.exchange.as_ref().unwrap().grant,
            "A new conflict must replace the grant with cancellation/hold"
        );
        state.session.telemetry.traffic = Some(vec![]);
        for _ in 0..12 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            if state.generated.approved_orders.contains(&(1, 8)) {
                break;
            }
        }
        assert!(state.generated.approved_orders.contains(&(1, 8)));
        state.session.telemetry.traffic = Some(vec![final_traffic]);
        state.session.telemetry.simulation_seconds =
            Some(state.session.telemetry.simulation_seconds.unwrap() + 1.0);
        super::super::advance(&mut state.generated, &state.session.telemetry);
        assert_eq!(
            state.generated.world.snapshot()[0].position,
            stopped.position
        );
        assert!(state.generated.world.snapshot()[0].awaiting_clearance);
        super::super::radio_tick(&mut state);
        assert!(
            !state.generated.approved_orders.contains(&(1, 8)),
            "A new conflict before entry must revoke an accepted crossing"
        );
        assert!(!state.generated.exchange.as_ref().unwrap().grant);
        state.session.telemetry.traffic = Some(vec![]);
        for _ in 0..12 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            if state.generated.approved_orders.contains(&(1, 8)) {
                break;
            }
        }
        assert!(state.generated.approved_orders.contains(&(1, 8)));
        for _ in 0..300 {
            state.session.telemetry.simulation_seconds =
                Some(state.session.telemetry.simulation_seconds.unwrap() + 1.0);
            super::super::advance(&mut state.generated, &state.session.telemetry);
            if state.generated.world.snapshot()[0].segment == 9 {
                break;
            }
        }
        let pose = &state.generated.world.snapshot()[0];
        assert_eq!(
            pose.segment, 9,
            "Acknowledged complete-flight crossing remained blocked without legacy orders"
        );
        assert!(pose.awaiting_clearance);
        assert!(!state.generated.approved_orders.contains(&(1, 9)));
        super::super::radio_tick(&mut state);
        assert!(state.generated.reported_crossings.contains(&(1, 8)));
    }

    #[test]
    fn custom_profile_dimensions_do_not_use_an_unknown_aircraft_fallback() {
        let (state, _) = fixture();
        let airport = state.generated.airport.as_ref().unwrap();
        let observed = openatc_core::state::Telemetry {
            aircraft_icao: "CUSTOM".into(),
            latitude: 60.0 / 111_320.0,
            longitude: 12700.0 / 111_320.0,
            heading_degrees: 90.0,
            position_valid: true,
            on_ground: true,
            ..Default::default()
        };
        let mut aircraft = openatc_core::traffic::profiles::aircraft("C172")
            .unwrap()
            .clone();
        aircraft.length = 20.0;
        aircraft.wingspan = 12.0;
        assert!(openatc_core::airport::aircraft_on_runway(
            airport, &observed
        ));
        assert!(!openatc_core::airport::aircraft_on_runway_for(
            airport, &observed, &aircraft
        ));
        aircraft.length = f64::NAN;
        assert!(openatc_core::airport::aircraft_on_runway_for(
            airport, &observed, &aircraft
        ));
    }
    #[test]
    fn runway_vacated_waits_for_observed_clearance_before_ground_handoff() {
        let (mut state, prepared) = fixture();
        install(&mut state.generated, 1, &prepared).unwrap();
        Arc::make_mut(state.generated.airport.as_mut().unwrap()).runways[0]
            .second
            .east = 15000.0;
        for second in 31..900 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            state.session.telemetry.simulation_seconds = Some(f64::from(second));
            super::super::advance(&mut state.generated, &state.session.telemetry);
            if state.generated.world.snapshot()[0].stage == Stage::TaxiIn {
                break;
            }
        }
        assert_eq!(state.generated.world.snapshot()[0].stage, Stage::TaxiIn);
        let stopped = state.generated.world.snapshot()[0].position;
        for offset in 1..=8 {
            super::super::radio_tick(&mut state);
            let seconds = state.session.telemetry.simulation_seconds.unwrap();
            state.session.telemetry.simulation_seconds = Some(seconds + f64::from(offset));
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            assert!(
                pose.awaiting_clearance,
                "Taxi-in moved without its readback"
            );
            assert_eq!(pose.position, stopped);
        }
        assert!(!state.generated.reported_crossings.contains(&(1, 6)));
        assert_eq!(state.generated.frames[0].target.com1_khz, Some(120_950));
        assert!(!state.generated.approved_orders.contains(&(1, 7)));
        Arc::make_mut(state.generated.airport.as_mut().unwrap()).runways[0]
            .second
            .east = 13500.0;
        super::super::radio_tick(&mut state);
        assert!(state.generated.reported_crossings.contains(&(1, 6)));
        let exchange = state.generated.exchange.as_ref().unwrap();
        assert_eq!(exchange.station.khz, 120_950);
        assert!(!exchange.grant && exchange.tune_after.is_none());
    }
    #[test]
    fn actual_missed_path_reports_once_without_granting_or_retuning_the_old_flight() {
        let (mut state, mut prepared) = fixture();
        let start = prepared
            .route
            .segment(5)
            .unwrap()
            .points
            .last()
            .copied()
            .unwrap();
        let hold = point(15000.0, 1000.0);
        let pattern = openatc_core::traffic::holding::Pattern {
            fix: hold,
            inbound_degrees: 90.0,
            right: true,
            ground_knots: 80.0,
            leg_seconds: 60.0,
            bank_degrees: 25.0,
            turn_rate_degrees_sec: 3.0,
        }
        .compile(|_, _| true)
        .unwrap();
        prepared.recovery.insert(
            5,
            Arc::new(
                Route::compile(
                    vec![
                        Segment {
                            stage: Stage::MissedApproach,
                            points: vec![start, hold],
                            knots: 80.0,
                            resources: vec![],
                            dwell_seconds: 0.0,
                        },
                        pattern,
                    ],
                    4000.0,
                    4000.0,
                )
                .unwrap(),
            ),
        );
        install(&mut state.generated, 1, &prepared).unwrap();
        let generation = state.generated.world.generation();
        assert!(
            state
                .generated
                .world
                .authorise(generation, 1, 0, &BTreeSet::new())
        );
        let mut reached = false;
        for second in 31..700 {
            let pose = state.generated.world.snapshot()[0].clone();
            if pose.awaiting_clearance && pose.segment < 5 {
                assert!(state.generated.world.authorise(
                    generation,
                    1,
                    pose.segment,
                    &BTreeSet::new()
                ));
            }
            if !pose.awaiting_clearance && pose.segment < 5 {
                assert!(state.generated.world.authorise_continuation(
                    generation,
                    1,
                    pose.segment + 1,
                    &BTreeSet::new()
                ));
            }
            state.generated.world.advance(f64::from(second), false);
            let pose = &state.generated.world.snapshot()[0];
            if pose.recovering {
                assert_eq!(pose.recovery_from, Some(5));
                assert_eq!(pose.stage, Stage::MissedApproach);
                reached = true;
                break;
            }
        }
        assert!(reached);
        state.settings.hear_traffic_radio = true;
        state.session.telemetry.com1_khz = 120_950;
        state.generated.exchange = Some(radio::Exchange {
            speed: None,
            id: 1,
            segment: 6,
            station: prepared.clearances[&6].station.clone(),
            callsign: "TEST123".into(),
            messages: vec![],
            next: 0,
            due: Instant::now(),
            grant: true,
            tune_after: Some(Station {
                khz: 118_675,
                ..Default::default()
            }),
            sector_transfer: None,
            recovery_from: None,
            rejoin: false,
        });
        state.generated.pending_checkins.insert((1, 6));
        state.generated.approved_orders.insert((1, 6));
        super::super::radio_tick(&mut state);
        let report = state.generated.exchange.as_ref().unwrap();
        assert_eq!(report.recovery_from, Some(5));
        assert_eq!(report.next, 1);
        assert!(!report.grant && report.tune_after.is_none());
        let entry = state.session.transcript.last().unwrap();
        assert!(entry.background && entry.text.to_lowercase().contains("going around"));
        let count = state.session.transcript.len();
        state.generated.exchange.as_mut().unwrap().due = Instant::now();
        super::super::radio_tick(&mut state);
        assert!(state.generated.exchange.is_none());
        for _ in 0..3 {
            super::super::radio_tick(&mut state);
        }
        assert_eq!(state.session.transcript.len(), count);
        assert_eq!(
            state.generated.reported_recoveries,
            BTreeSet::from([(1, 5)])
        );
        assert!(state.generated.approved_orders.is_empty());
        assert!(state.generated.pending_checkins.is_empty());
        assert_eq!(state.generated.frames[0].target.com1_khz, Some(120_950));
        assert!(state.generated.world.snapshot()[0].recovering);
    }

    #[test]
    fn cleared_limit_hold_reports_once_without_reviving_stale_onward_grants() {
        use openatc_core::traffic::{
            air::Atmosphere,
            trajectory::{Airspeed, Fix},
        };
        let (mut state, mut prepared) = fixture();
        let segment = prepared.route.segment(3).unwrap();
        let end = *segment.points.last().unwrap();
        let atmosphere = Atmosphere::surface(1013.25, 15.0, 0.0).unwrap();
        let reference = Airspeed {
            indicated_knots: 80.0 / atmosphere.true_knots(100.0, end.altitude_feet).unwrap()
                * 100.0,
            atmosphere,
            wind_from: 0.0,
            wind_knots: 0.0,
        };
        let fix = Fix {
            coordinate: [end.latitude, end.longitude],
            arrival_heading: Some(90.0),
            minimum_feet: end.altitude_feet,
            maximum_feet: end.altitude_feet,
            preferred_feet: end.altitude_feet,
            ground_knots: 80.0,
            airspeed: Some(reference),
            required_turn: None,
            intercept: None,
        };
        let hold = super::super::recovery::LimitHold::at(
            3,
            segment,
            &fix,
            &super::super::recovery::HoldingPolicy {
                minimum_indicated_knots: 40.0,
                maximum_indicated_knots: 200.0,
                bank_degrees: 25.0,
                turn_rate_degrees_sec: 3.0,
            },
        )
        .unwrap();
        prepared.recovery.insert(
            3,
            Arc::new(hold.protect(4000.0, 4000.0, |_, _| true).unwrap()),
        );
        prepared.clearances.get_mut(&3).unwrap().holding = Some(hold.clone());
        prepared.validate().unwrap();
        let mut wrong_scope = prepared.clone();
        wrong_scope.clearances.get_mut(&2).unwrap().holding = Some(hold);
        assert!(wrong_scope.validate().is_err());
        install(&mut state.generated, 1, &prepared).unwrap();
        let generation = state.generated.world.generation();
        assert!(
            state
                .generated
                .world
                .authorise(generation, 1, 0, &BTreeSet::new())
        );
        let mut reached = false;
        let mut now = 30.0;
        for second in 31..900 {
            let pose = state.generated.world.snapshot()[0].clone();
            if pose.awaiting_clearance && pose.segment < 3 {
                assert!(state.generated.world.authorise(
                    generation,
                    1,
                    pose.segment,
                    &BTreeSet::new()
                ));
            }
            if !pose.awaiting_clearance && pose.segment < 3 {
                assert!(state.generated.world.authorise_continuation(
                    generation,
                    1,
                    pose.segment + 1,
                    &BTreeSet::new()
                ));
            }
            now = f64::from(second);
            state.generated.world.advance(now, false);
            if state.generated.world.snapshot()[0].recovering {
                reached = true;
                break;
            }
        }
        assert!(reached);
        state.settings.hear_traffic_radio = true;
        state.session.telemetry.com1_khz = 118_675;
        state.generated.frames[0].target.com1_khz = Some(118_675);
        state.generated.approved_orders.insert((1, 4));
        state.generated.pending_checkins.insert((1, 4));
        super::super::radio_tick(&mut state);
        let exchange = state.generated.exchange.as_ref().unwrap();
        assert_eq!(exchange.recovery_from, Some(3));
        assert!(!exchange.grant && !exchange.rejoin && exchange.tune_after.is_none());
        let report = state.session.transcript.last().unwrap();
        assert!(report.background && report.text.contains("established in the hold"));
        state.generated.exchange.as_mut().unwrap().due = Instant::now();
        super::super::radio_tick(&mut state);
        for _ in 0..3 {
            super::super::radio_tick(&mut state);
        }
        assert_eq!(
            state
                .session
                .transcript
                .iter()
                .filter(|entry| entry.text.contains("established in the hold"))
                .count(),
            1
        );
        assert!(state.generated.rejoins.contains_key(&1));
        assert_eq!(state.generated.frames[0].target.com1_khz, Some(118_675));
        assert!(state.generated.approved_orders.is_empty());
        assert!(state.generated.pending_checkins.is_empty());
        assert!(state.generated.world.snapshot()[0].recovering);
        for _ in 0..1600 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            now += 0.5;
            state.session.telemetry.simulation_seconds = Some(now);
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            if pose.stage == Stage::Turnaround && pose.complete {
                break;
            }
        }
        let pose = &state.generated.world.snapshot()[0];
        assert_eq!(pose.stage, Stage::Turnaround, "{pose:?}");
        assert!(pose.complete);
        assert!(state.generated.rejoins.is_empty());
        assert_eq!(
            state
                .session
                .transcript
                .iter()
                .filter(|entry| entry.text.contains("established in the hold"))
                .count(),
            1
        );
    }

    #[test]
    fn fallback_discards_pending_normal_readbacks_grants_and_handoffs() {
        for next in [0, 1, 2] {
            let (mut state, prepared) = fixture();
            install(&mut state.generated, 1, &prepared).unwrap();
            let generation = state.generated.world.generation();
            assert!(
                state
                    .generated
                    .world
                    .authorise(generation, 1, 0, &BTreeSet::new())
            );
            let mut reached = false;
            for second in 31..600 {
                let pose = state.generated.world.snapshot()[0].clone();
                if pose.awaiting_clearance && pose.segment < 2 {
                    assert!(state.generated.world.authorise(
                        generation,
                        1,
                        pose.segment,
                        &BTreeSet::new()
                    ));
                }
                if pose.segment == 1 && !pose.awaiting_clearance {
                    assert!(state.generated.world.authorise_continuation(
                        generation,
                        1,
                        2,
                        &BTreeSet::new()
                    ));
                }
                state.generated.world.advance(f64::from(second), false);
                let pose = &state.generated.world.snapshot()[0];
                if pose.recovering {
                    assert_eq!(pose.recovery_from, Some(2));
                    reached = true;
                    break;
                }
            }
            assert!(reached, "Aircraft never entered the registered fallback");
            let messages = vec![
                (true, "Stale instruction".into()),
                (false, "Stale readback".into()),
            ];
            state.generated.exchange = Some(radio::Exchange {
                speed: None,
                id: 1,
                segment: 3,
                station: prepared.clearances[&3].station.clone(),
                callsign: "TEST123".into(),
                messages,
                next,
                due: Instant::now(),
                grant: true,
                tune_after: Some(Station {
                    khz: 118_675,
                    ..Default::default()
                }),
                sector_transfer: None,
                recovery_from: None,
                rejoin: false,
            });
            state
                .generated
                .approved_orders
                .extend([(1, 3), (1, 4), (2, 7)]);
            state.generated.pending_checkins.extend([(1, 3), (2, 7)]);
            state.generated.handoff_targets.extend([(1, 3), (2, 7)]);
            let frequency = state.generated.frames[0].target.com1_khz;
            let transcripts = state.session.transcript.len();
            state.session.telemetry.paused = true;
            super::super::radio_tick(&mut state);
            assert!(state.generated.exchange.is_none());
            assert_eq!(state.generated.approved_orders, BTreeSet::from([(2, 7)]));
            assert_eq!(state.generated.pending_checkins, BTreeSet::from([(2, 7)]));
            assert_eq!(state.generated.handoff_targets, BTreeMap::from([(2, 7)]));
            assert_eq!(state.generated.frames[0].target.com1_khz, frequency);
            assert_eq!(state.session.transcript.len(), transcripts);
            assert!(state.generated.active_flights.contains(&1));
            assert!(state.generated.flights.contains_key(&1));
            state.session.telemetry.paused = false;
            super::super::radio_tick(&mut state);
            assert!(state.generated.exchange.is_none());
            let pose = &state.generated.world.snapshot()[0];
            assert!(pose.recovering && !pose.awaiting_clearance);
        }
    }

    fn wake_fixture() -> EngineState {
        let (mut state, mut prepared) = fixture();
        let old = prepared.route.clone();
        let segments = (0..9)
            .map(|index| {
                let mut segment = old.segment(index).unwrap().clone();
                if index == 1 {
                    segment.resources = vec!["RUNWAY/KTEST/09/27".into()];
                }
                for resource in &mut segment.resources {
                    *resource = resource.replace("RUNWAY/TEST/", "RUNWAY/KTEST/");
                }
                segment
            })
            .collect();
        prepared.route = Arc::new(Route::compile(segments, 4000.0, 4000.0).unwrap());
        for clearance in prepared.clearances.values_mut() {
            clearance.station.airport = "KTEST".into();
        }
        prepared.vacated_reports.get_mut(&7).unwrap().airport = "KTEST".into();
        let airport = Arc::make_mut(state.generated.airport.as_mut().unwrap());
        airport.icao = "KTEST".into();
        airport.runways[0].first.east = 0.0;
        airport.runways[0].second.east = 3000.0;
        state.session.plan.departure = "KTEST".into();
        state.session.plan.runway = "09".into();
        state.generated.frames[0].target.aircraft = "C172".into();
        install(&mut state.generated, 1, &prepared).unwrap();
        state
    }

    #[test]
    fn an_active_wake_interval_lines_up_instead_of_holding_short() {
        let mut state = wake_fixture();
        state.generated.departure_wake.insert(1, 120.0);
        assert!(takeoff_exchange_for(&mut state, 1).is_none());
        let exchange = ground_exchange_for(&mut state, 1).expect("line up while wake runs");
        assert!(exchange.grant);
        assert_eq!(exchange.segment, 0, "line up, not takeoff");
        assert!(exchange.messages[0].1.contains("phase 0"));
    }

    #[test]
    fn a_clear_runway_clears_takeoff_directly_from_the_hold() {
        let mut state = wake_fixture();
        state.session.telemetry.aircraft_icao = "C172".into();
        state.session.telemetry.wingspan_meters = Some(11.0);
        state.session.telemetry.heading_degrees = 90.0;
        let pose = state.generated.world.snapshot()[0].clone();
        assert_eq!(pose.stage, Stage::LineUp);
        assert!(pose.awaiting_clearance);
        let exchange = takeoff_exchange_for(&mut state, 1).expect("clear runway");
        assert!(exchange.grant);
        assert_eq!(exchange.segment, 1, "must clear takeoff, not line up");
        assert!(exchange.messages[0].1.contains("phase 1"));
    }

    #[test]
    fn a_rolling_takeoff_is_not_cancelled_by_late_traffic() {
        let mut state = wake_fixture();
        state
            .generated
            .policy
            .as_mut()
            .unwrap()
            .runway_clearance_buffer_seconds = 120.0;
        let exchange = takeoff_exchange_for(&mut state, 1).expect("clear runway");
        state.generated.exchange = Some(exchange);
        let mut now = 30.0;
        for _ in 0..40 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            if state.generated.approved_orders.contains(&(1, 1)) {
                break;
            }
            now += 1.0;
            state.session.telemetry.simulation_seconds = Some(now);
            super::super::advance(&mut state.generated, &state.session.telemetry);
        }
        assert!(state.generated.approved_orders.contains(&(1, 1)));
        let mut rolling = false;
        for _ in 0..120 {
            now += 1.0;
            state.session.telemetry.simulation_seconds = Some(now);
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            if pose.stage == Stage::Takeoff && pose.knots > 20.0 {
                rolling = true;
                break;
            }
        }
        assert!(rolling, "the departure did not start its roll");
        let incoming = TrafficTarget {
            id: 88,
            callsign: "ARR88".into(),
            aircraft: "A320".into(),
            latitude: 0.0,
            longitude: -0.02,
            altitude_feet: 500.0,
            track_degrees: 90.0,
            ground_speed_knots: Some(200.0),
            on_ground: false,
            ..Default::default()
        };
        state.session.telemetry.traffic = Some(vec![incoming]);
        for _ in 0..5 {
            now += 1.0;
            state.session.telemetry.simulation_seconds = Some(now);
            super::super::advance(&mut state.generated, &state.session.telemetry);
        }
        assert!(
            state.generated.approved_orders.contains(&(1, 1)),
            "a rolling take-off must keep its clearance"
        );
        assert!(!state.generated.cancelled_takeoffs.contains_key(&1));
    }

    #[test]
    fn a_direct_takeoff_readback_keeps_the_clearance() {
        let mut state = wake_fixture();
        state.settings.hear_traffic_radio = true;
        state.session.telemetry.aircraft_icao = "C172".into();
        state.session.telemetry.wingspan_meters = Some(11.0);
        state.session.telemetry.heading_degrees = 90.0;
        state.session.telemetry.com1_khz = 120_950;
        let exchange = takeoff_exchange_for(&mut state, 1).expect("clear runway");
        assert!(exchange.grant);
        state.generated.exchange = Some(exchange);
        let mut now = 30.0;
        for _ in 0..20 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            now += 0.5;
            state.session.telemetry.simulation_seconds = Some(now);
            super::super::advance(&mut state.generated, &state.session.telemetry);
        }
        assert!(state.generated.approved_orders.contains(&(1, 1)));
        assert!(state.generated.approved_orders.contains(&(1, 2)));
        assert!(!state.generated.cancelled_takeoffs.contains_key(&1));
    }

    #[test]
    fn a_cleared_direct_takeoff_is_not_cancelled_by_the_grounded_player() {
        let mut state = wake_fixture();
        state.generated.approved_orders.insert((1, 1));
        super::super::withhold_conflicting_departures(
            &mut state.generated,
            &state.session.telemetry,
        );
        assert!(state.generated.approved_orders.contains(&(1, 1)));
        assert!(!state.generated.cancelled_takeoffs.contains_key(&1));
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn crossing_airborne_traffic_withholds_takeoff_and_requires_a_fresh_readback_after_clearing() {
        for arrival_stage in 0..3 {
            let mut state = wake_fixture();
            state.settings.hear_traffic_radio = true;
            state.session.telemetry.com1_khz = 120_950;
            let mut now = 30.0;
            let mut ready = false;
            for _ in 0..100 {
                let pose = &state.generated.world.snapshot()[0];
                if pose.stage == Stage::Takeoff
                    && pose.awaiting_clearance
                    && state.generated.exchange.is_none()
                {
                    ready = true;
                    break;
                }
                if let Some(exchange) = &mut state.generated.exchange {
                    exchange.due = Instant::now();
                }
                super::super::radio_tick(&mut state);
                now += 0.5;
                state.session.telemetry.simulation_seconds = Some(now);
                super::super::advance(&mut state.generated, &state.session.telemetry);
            }
            assert!(ready);
            if arrival_stage > 0 {
                super::super::radio_tick(&mut state);
                let exchange = state.generated.exchange.as_ref().unwrap();
                assert!(exchange.grant && exchange.segment == 1 && exchange.next == 1);
            }
            if arrival_stage == 2 {
                for _ in 0..3 {
                    state.generated.exchange.as_mut().unwrap().due = Instant::now();
                    super::super::radio_tick(&mut state);
                    if state.generated.approved_orders.contains(&(1, 1)) {
                        break;
                    }
                }
                assert!(state.generated.approved_orders.contains(&(1, 1)));
                assert!(state.generated.approved_orders.contains(&(1, 2)));
            }
            state.session.telemetry.traffic = Some(vec![TrafficTarget {
                id: 88,
                callsign: "CROSS88".into(),
                aircraft: "C172".into(),
                latitude: 3.0 / 60.0,
                longitude: 2.5 / 60.0,
                altitude_feet: 1000.0,
                ground_speed_knots: Some(120.0),
                track_degrees: 180.0,
                on_ground: false,
                ..Default::default()
            }]);
            assert!(
                !state.generated.crossing_blocked(
                    1,
                    1,
                    &super::super::observed_targets(&state.generated, &state.session.telemetry)
                ),
                "This aircraft crosses the departure corridor beyond the runway monitor"
            );
            if arrival_stage == 2 {
                now += 0.5;
                state.session.telemetry.simulation_seconds = Some(now);
                super::super::advance(&mut state.generated, &state.session.telemetry);
                assert!(state.generated.cancelled_takeoffs.contains_key(&1));
                assert!(!state.generated.approved_orders.contains(&(1, 1)));
                assert!(!state.generated.approved_orders.contains(&(1, 2)));
            }
            for _ in 0..15 {
                if let Some(exchange) = &mut state.generated.exchange {
                    exchange.due = Instant::now();
                }
                super::super::radio_tick(&mut state);
                now += 0.5;
                state.session.telemetry.simulation_seconds = Some(now);
                super::super::advance(&mut state.generated, &state.session.telemetry);
                let pose = &state.generated.world.snapshot()[0];
                assert_eq!(pose.stage, Stage::Takeoff);
                assert_eq!(pose.knots, 0.0);
                assert!(!state.generated.approved_orders.contains(&(1, 1)));
                assert!(!state.generated.approved_orders.contains(&(1, 2)));
            }
            state.session.telemetry.traffic = Some(vec![]);
            for _ in 0..2000 {
                if let Some(exchange) = &mut state.generated.exchange {
                    exchange.due = Instant::now();
                }
                super::super::radio_tick(&mut state);
                now += 0.5;
                state.session.telemetry.simulation_seconds = Some(now);
                super::super::advance(&mut state.generated, &state.session.telemetry);
                let pose = &state.generated.world.snapshot()[0];
                assert!(!pose.recovering);
                if pose.stage == Stage::Turnaround && pose.complete {
                    break;
                }
            }
            let pose = &state.generated.world.snapshot()[0];
            assert_eq!(pose.stage, Stage::Turnaround, "{pose:?}");
            assert!(pose.complete);
            let takeoff_calls = state
                .session
                .transcript
                .iter()
                .filter(|entry| entry.text.contains("Controller phase 1."))
                .count();
            assert_eq!(takeoff_calls, if arrival_stage > 0 { 2 } else { 1 });
        }
    }

    #[test]
    fn generated_departures_wait_for_observed_native_and_player_wake() {
        for player_leads in [false, true] {
            let mut state = wake_fixture();
            let heavy = TrafficTarget {
                id: 88,
                callsign: "HEAVY88".into(),
                aircraft: "B77W".into(),
                ground_speed_knots: Some(180.0),
                longitude: 0.01,
                track_degrees: 90.0,
                on_ground: true,
                ..Default::default()
            };
            if player_leads {
                state.session.telemetry.aircraft_icao = "B77W".into();
                state.session.telemetry.longitude = 0.01;
                state.session.telemetry.heading_degrees = 90.0;
                state.session.telemetry.on_ground = true;
                state.session.telemetry.traffic = Some(vec![]);
            } else {
                state.session.telemetry.traffic = Some(vec![heavy]);
            }
            state.session.telemetry.simulation_seconds = Some(31.0);
            crate::traffic::observe(&mut state);
            if player_leads {
                state.session.telemetry.on_ground = false;
            } else {
                state.session.telemetry.traffic.as_mut().unwrap()[0].on_ground = false;
            }
            state.session.telemetry.simulation_seconds = Some(32.0);
            crate::traffic::observe(&mut state);
            assert_eq!(state.generated.departure_wake.get(&1), Some(&180.0));
            // The observed leader leaves the runway while its wake interval remains active.
            if player_leads {
                state.session.telemetry.longitude = 0.2;
                state.session.telemetry.altitude_feet = 4000.0;
            } else {
                let leader = &mut state.session.telemetry.traffic.as_mut().unwrap()[0];
                leader.longitude = 0.2;
                leader.altitude_feet = 4000.0;
            }
            for second in 33..1000 {
                state.session.telemetry.simulation_seconds = Some(f64::from(second));
                crate::traffic::observe(&mut state);
                if let Some(exchange) = &mut state.generated.exchange {
                    exchange.due = Instant::now();
                }
                super::super::radio_tick(&mut state);
                super::super::advance(&mut state.generated, &state.session.telemetry);
                let pose = &state.generated.world.snapshot()[0];
                if second < 212 {
                    assert!(
                        matches!(pose.stage, Stage::LineUp | Stage::Takeoff)
                            && (pose.stage != Stage::Takeoff || pose.knots == 0.0),
                        "early roll at {second}: {pose:?}"
                    );
                }
                assert!(!pose.recovering, "{pose:?}");
                if pose.stage == Stage::Turnaround && pose.complete {
                    break;
                }
            }
            let pose = &state.generated.world.snapshot()[0];
            assert_eq!(pose.stage, Stage::Turnaround, "{pose:?}");
            assert!(pose.complete);
        }
    }

    #[test]
    fn wake_arriving_during_takeoff_readback_cancels_the_grant() {
        let mut state = wake_fixture();
        let mut now = 30.0;
        let mut pending = false;
        for _ in 0..100 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            if state.generated.exchange.as_ref().is_some_and(|exchange| {
                exchange.segment == 1 && exchange.grant && exchange.next == exchange.messages.len()
            }) {
                pending = true;
                break;
            }
            now += 1.0;
            state.session.telemetry.simulation_seconds = Some(now);
            super::super::advance(&mut state.generated, &state.session.telemetry);
        }
        assert!(pending);
        assert!(!state.generated.approved_orders.contains(&(1, 1)));
        let heavy = TrafficTarget {
            id: 88,
            aircraft: "B77W".into(),
            callsign: "HEAVY88".into(),
            longitude: 0.01,
            track_degrees: 90.0,
            on_ground: true,
            ..Default::default()
        };
        state.session.telemetry.traffic = Some(vec![heavy]);
        now += 1.0;
        state.session.telemetry.simulation_seconds = Some(now);
        crate::traffic::observe(&mut state);
        now += 1.0;
        state.session.telemetry.simulation_seconds = Some(now);
        state.session.telemetry.traffic.as_mut().unwrap()[0].on_ground = false;
        crate::traffic::observe(&mut state);
        assert_eq!(state.generated.departure_wake.get(&1), Some(&180.0));
        state.generated.exchange.as_mut().unwrap().due = Instant::now();
        super::super::radio_tick(&mut state);
        let cancellation = state.generated.exchange.as_ref().unwrap();
        assert!(!cancellation.grant);
        assert!(
            cancellation.messages[0]
                .1
                .contains("cancel takeoff clearance")
        );
        assert!(!state.generated.approved_orders.contains(&(1, 1)));
        for _ in 0..40 {
            now += 1.0;
            state.session.telemetry.simulation_seconds = Some(now);
            crate::traffic::observe(&mut state);
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            assert!(
                pose.stage != Stage::Takeoff || pose.knots == 0.0,
                "{pose:?}"
            );
        }
    }

    #[test]
    fn expired_wake_cannot_restore_an_old_takeoff_grant_while_radio_is_busy() {
        let mut state = wake_fixture();
        let mut now = 30.0;
        for _ in 0..100 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            if state.generated.approved_orders.contains(&(1, 1)) {
                break;
            }
            now += 1.0;
            state.session.telemetry.simulation_seconds = Some(now);
            super::super::advance(&mut state.generated, &state.session.telemetry);
        }
        assert!(state.generated.approved_orders.contains(&(1, 1)));
        state.session.telemetry.traffic = Some(vec![TrafficTarget {
            id: 88,
            callsign: "HEAVY88".into(),
            aircraft: "B77W".into(),
            ground_speed_knots: Some(180.0),
            longitude: 0.01,
            track_degrees: 90.0,
            on_ground: true,
            ..Default::default()
        }]);
        state.session.telemetry.com1_khz = 120_950;
        state.session.telemetry.radio_busy = true;
        now += 1.0;
        state.session.telemetry.simulation_seconds = Some(now);
        crate::traffic::observe(&mut state);
        state.session.telemetry.traffic.as_mut().unwrap()[0].on_ground = false;
        now += 1.0;
        state.session.telemetry.simulation_seconds = Some(now);
        crate::traffic::observe(&mut state);
        assert!(state.generated.cancelled_takeoffs.contains_key(&1));
        assert!(!state.generated.approved_orders.contains(&(1, 1)));
        assert!(!state.generated.approved_orders.contains(&(1, 2)));
        for _ in 0..220 {
            now += 1.0;
            state.session.telemetry.simulation_seconds = Some(now);
            crate::traffic::observe(&mut state);
            super::super::radio_tick(&mut state);
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            assert!(
                pose.stage != Stage::Takeoff || pose.knots == 0.0,
                "{pose:?}"
            );
            assert!(state.generated.cancelled_takeoffs.contains_key(&1));
        }
        assert!(!state.generated.departure_wake.contains_key(&1));
        state.session.telemetry.radio_busy = false;
        super::super::radio_tick(&mut state);
        let cancellation = state.generated.exchange.as_ref().unwrap();
        assert!(!cancellation.grant);
        assert!(
            cancellation.messages[0]
                .1
                .contains("cancel takeoff clearance")
        );
        // Wake expiry does not clear an aircraft still inside the departure corridor.
        for _ in 0..20 {
            now += 1.0;
            state.session.telemetry.simulation_seconds = Some(now);
            crate::traffic::observe(&mut state);
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            assert!(
                pose.stage != Stage::Takeoff || pose.knots == 0.0,
                "{pose:?}"
            );
        }
        let leader = &mut state.session.telemetry.traffic.as_mut().unwrap()[0];
        leader.longitude = 0.2;
        leader.altitude_feet = 4000.0;
        for _ in 0..900 {
            now += 1.0;
            state.session.telemetry.simulation_seconds = Some(now);
            crate::traffic::observe(&mut state);
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            assert!(!pose.recovering);
            if pose.stage == Stage::Turnaround && pose.complete {
                break;
            }
        }
        let pose = &state.generated.world.snapshot()[0];
        assert_eq!(pose.stage, Stage::Turnaround, "{pose:?}");
        assert!(pose.complete);
    }

    #[test]
    fn a_distant_final_allows_line_up_but_a_short_final_holds() {
        let state = wake_fixture();
        let target = |longitude: f64, callsign: &str| TrafficTarget {
            id: 88,
            callsign: callsign.into(),
            aircraft: "A320".into(),
            longitude,
            altitude_feet: 2000.0,
            track_degrees: 90.0,
            ground_speed_knots: Some(200.0),
            on_ground: false,
            ..Default::default()
        };
        assert!(!state.generated.crossing_blocked(
            1,
            0,
            std::slice::from_ref(&target(-0.1, "FAR88"))
        ));
        assert!(state.generated.crossing_blocked(
            1,
            0,
            std::slice::from_ref(&target(-0.02, "NEAR88"))
        ));
    }

    #[test]
    fn a_parallel_runway_final_does_not_withhold_the_departure() {
        let mut state = wake_fixture();
        state
            .generated
            .policy
            .as_mut()
            .unwrap()
            .runway_clearance_buffer_seconds = 180.0;
        let airport = Arc::make_mut(state.generated.airport.as_mut().unwrap());
        airport.runways.push(openatc_core::airport::Runway {
            first_name: "09L".into(),
            second_name: "27R".into(),
            width: 45.0,
            first: Point {
                east: 0.0,
                north: 800.0,
                ..Default::default()
            },
            second: Point {
                east: 3000.0,
                north: 800.0,
                ..Default::default()
            },
            ..Default::default()
        });
        let target = |north: f64, callsign: &str| TrafficTarget {
            id: 88,
            callsign: callsign.into(),
            aircraft: "A320".into(),
            latitude: north / 111_320.0,
            longitude: -0.1,
            altitude_feet: 2000.0,
            track_degrees: 90.0,
            ground_speed_knots: Some(200.0),
            on_ground: false,
            ..Default::default()
        };
        assert!(state.generated.crossing_blocked(
            1,
            1,
            std::slice::from_ref(&target(0.0, "SAME88"))
        ));
        assert!(!state.generated.crossing_blocked(
            1,
            1,
            std::slice::from_ref(&target(800.0, "PAR88"))
        ));
        state.session.telemetry.traffic = Some(vec![target(800.0, "PAR88")]);
        assert!(takeoff_exchange_for(&mut state, 1).is_some());
        // 4.2 NM final on the parallel runway, the distance at which the old build
        // withheld AFR7246 while HOP3870 was still turning onto 04L.
        let mut medium = target(800.0, "MED88");
        medium.longitude = -0.07;
        medium.altitude_feet = 1487.0;
        state.session.telemetry.traffic = Some(vec![medium]);
        assert!(
            takeoff_exchange_for(&mut state, 1).is_some(),
            "a parallel final outside the short-final radius must not withhold take-off"
        );
        // A real localizer is wider than 200 m at 1.5 NM. A parallel arrival offset
        // within the corridor must still be recognised, or a short final on 09L
        // cancels a 09 departure after the clearance.
        let mut offset = target(1250.0, "OFFSET88");
        offset.longitude = -0.025;
        offset.altitude_feet = 612.0;
        state.session.telemetry.traffic = Some(vec![offset]);
        assert!(
            takeoff_exchange_for(&mut state, 1).is_some(),
            "an offset parallel final must not withhold take-off"
        );
        // A departure on 09 is independent of a short final to the parallel 09L. The
        // arrival is established on a different strip, and a go-around remains the
        // arrival's responsibility; it must not cancel the departure.
        let mut parallel_short = target(800.0, "PARSHORT88");
        parallel_short.longitude = -0.02;
        parallel_short.altitude_feet = 500.0;
        state.session.telemetry.traffic = Some(vec![parallel_short]);
        assert!(
            takeoff_exchange_for(&mut state, 1).is_some(),
            "a short final on the parallel runway must not withhold take-off"
        );
        // The same runway's short final still blocks.
        let mut same_short = target(0.0, "SAMSHORT88");
        same_short.longitude = -0.02;
        same_short.altitude_feet = 500.0;
        state.session.telemetry.traffic = Some(vec![same_short]);
        assert!(
            takeoff_exchange_for(&mut state, 1).is_none(),
            "a short final on the departure runway must withhold take-off"
        );
    }

    #[test]
    fn approaching_traffic_cancels_takeoff_before_roll_and_requires_a_fresh_readback() {
        for after_readback in [false, true] {
            let mut state = wake_fixture();
            state
                .generated
                .policy
                .as_mut()
                .unwrap()
                .runway_clearance_buffer_seconds = 120.0;
            let incoming = TrafficTarget {
                id: 88,
                callsign: "ARR88".into(),
                aircraft: "A320".into(),
                longitude: -0.1,
                altitude_feet: 2000.0,
                track_degrees: 90.0,
                ground_speed_knots: Some(200.0),
                on_ground: false,
                ..Default::default()
            };
            let airport = state.generated.airport.as_ref().unwrap();
            assert!(
                openatc_core::holding::blocked_targets(airport, "09", std::iter::once(&incoming))
                    .is_none(),
                "Arrival must be outside the fixed final radius"
            );
            assert!(
                state
                    .generated
                    .crossing_blocked(1, 1, std::slice::from_ref(&incoming))
            );
            let mut now = 30.0;
            let mut ready = false;
            for _ in 0..100 {
                if let Some(exchange) = &mut state.generated.exchange {
                    exchange.due = Instant::now();
                }
                super::super::radio_tick(&mut state);
                ready = if after_readback {
                    state.generated.approved_orders.contains(&(1, 1))
                } else {
                    state.generated.exchange.as_ref().is_some_and(|exchange| {
                        exchange.segment == 1
                            && exchange.grant
                            && exchange.next == exchange.messages.len()
                    })
                };
                if ready {
                    break;
                }
                now += 1.0;
                state.session.telemetry.simulation_seconds = Some(now);
                super::super::advance(&mut state.generated, &state.session.telemetry);
            }
            assert!(ready);
            state.session.telemetry.traffic = Some(vec![incoming]);
            if after_readback {
                state.session.telemetry.radio_busy = true;
                now += 1.0;
                state.session.telemetry.simulation_seconds = Some(now);
                super::super::advance(&mut state.generated, &state.session.telemetry);
                assert_eq!(state.generated.cancelled_takeoffs.get(&1), Some(&false));
                assert!(!state.generated.approved_orders.contains(&(1, 2)));
                // Even after the traffic clears, the old grant must not restart a departure.
                state.session.telemetry.traffic = Some(vec![]);
                for _ in 0..80 {
                    now += 1.0;
                    state.session.telemetry.simulation_seconds = Some(now);
                    super::super::radio_tick(&mut state);
                    super::super::advance(&mut state.generated, &state.session.telemetry);
                    let pose = &state.generated.world.snapshot()[0];
                    assert!(
                        pose.stage != Stage::Takeoff || pose.knots == 0.0,
                        "{pose:?}"
                    );
                }
                state.session.telemetry.radio_busy = false;
            } else {
                state.generated.exchange.as_mut().unwrap().due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            let cancellation = state.generated.exchange.as_ref().unwrap();
            assert!(!cancellation.grant);
            assert!(
                cancellation.messages[0]
                    .1
                    .contains("cancel takeoff clearance")
            );
            assert!(cancellation.messages[0].1.contains("traffic"));
            assert!(!cancellation.messages[0].1.contains("wake"));
            assert!(!state.generated.approved_orders.contains(&(1, 1)));
            state.session.telemetry.traffic = Some(vec![]);
            for _ in 0..1200 {
                now += 1.0;
                state.session.telemetry.simulation_seconds = Some(now);
                if let Some(exchange) = &mut state.generated.exchange {
                    exchange.due = Instant::now();
                }
                super::super::radio_tick(&mut state);
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
        }
    }

    #[test]
    fn departure_and_combined_controllers_work_through_the_full_flight() {
        for combined in [false, true] {
            let (mut state, mut prepared) = fixture();
            let station = &mut prepared.clearances.get_mut(&2).unwrap().station;
            station.service = if combined { "Tower" } else { "Departure" }.into();
            station.name = if combined {
                "Test Tower/Departure"
            } else {
                "Test Departure"
            }
            .into();
            station.services = if combined {
                vec!["Tower".into(), "Departure".into()]
            } else {
                vec![]
            };
            if combined {
                station.khz = 120_950;
            }
            state.stations.push(station.clone());
            install(&mut state.generated, 1, &prepared).unwrap();
            for second in 31..900 {
                if let Some(exchange) = &mut state.generated.exchange {
                    exchange.due = Instant::now();
                }
                super::super::radio_tick(&mut state);
                state.session.telemetry.simulation_seconds = Some(f64::from(second));
                super::super::advance(&mut state.generated, &state.session.telemetry);
                let pose = &state.generated.world.snapshot()[0];
                assert!(!pose.recovering, "{combined}: {pose:?}");
                if pose.stage == Stage::Turnaround && pose.complete {
                    break;
                }
            }
            let pose = &state.generated.world.snapshot()[0];
            assert_eq!(pose.stage, Stage::Turnaround);
            assert!(pose.complete);
            let mut invalid = prepared.clone();
            let station = &mut invalid.clearances.get_mut(&2).unwrap().station;
            station.service = "Ground".into();
            station.services.clear();
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn recovery_hold_rejoins_after_readback_at_fix_then_reaches_parking() {
        let (mut state, mut prepared) = fixture();
        let segment = prepared.route.segment(4).unwrap();
        let fix = *segment.points.last().unwrap();
        let atmosphere =
            openatc_core::traffic::air::Atmosphere::surface(1013.25, 15.0, 0.0).unwrap();
        let requirement = openatc_core::traffic::trajectory::Fix {
            coordinate: [fix.latitude, fix.longitude],
            arrival_heading: Some(90.0),
            minimum_feet: fix.altitude_feet,
            maximum_feet: fix.altitude_feet,
            preferred_feet: fix.altitude_feet,
            ground_knots: 80.0,
            required_turn: None,
            intercept: None,
            airspeed: Some(openatc_core::traffic::trajectory::Airspeed {
                indicated_knots: 80.0 / atmosphere.true_knots(100.0, fix.altitude_feet).unwrap()
                    * 100.0,
                atmosphere,
                wind_from: 0.0,
                wind_knots: 0.0,
            }),
        };
        let hold = super::super::recovery::LimitHold::at(
            4,
            segment,
            &requirement,
            &super::super::recovery::HoldingPolicy {
                minimum_indicated_knots: 40.0,
                maximum_indicated_knots: 200.0,
                bank_degrees: 25.0,
                turn_rate_degrees_sec: 3.0,
            },
        )
        .unwrap();
        prepared.recovery.insert(
            4,
            Arc::new(hold.protect(4000.0, 4000.0, |_, _| true).unwrap()),
        );
        prepared.clearances.get_mut(&4).unwrap().holding = Some(hold);
        install(&mut state.generated, 1, &prepared).unwrap();
        let mut now = 30.0;
        for _ in 0..700 {
            let pose = state.generated.world.snapshot()[0].clone();
            if pose.recovering {
                break;
            }
            if pose.awaiting_clearance && pose.segment < 5 {
                assert!(
                    state
                        .generated
                        .world
                        .authorise(0, 1, pose.segment, &BTreeSet::new())
                );
            }
            if !pose.awaiting_clearance && pose.segment < 4 {
                assert!(state.generated.world.authorise_continuation(
                    0,
                    1,
                    pose.segment + 1,
                    &BTreeSet::new()
                ));
            }
            now += 1.0;
            state.generated.world.advance(now, false);
        }
        assert_eq!(state.generated.world.snapshot()[0].recovery_from, Some(4));
        assert_eq!(state.generated.world.snapshot()[0].stage, Stage::Holding);
        now += 15.0;
        state.generated.world.advance(now, false);
        let before = state.generated.world.snapshot()[0].position;
        state.generated.frames[0].target.com1_khz = Some(124_500);
        state.session.telemetry.com1_khz = 124_500;
        state.settings.hear_traffic_radio = true;
        let onward = prepared.resume_after_limit(4).unwrap();
        assert_eq!(onward.clearances[&0].station.khz, 120_950);
        let mut broken = onward.clone();
        broken.recovery.clear();
        assert!(offer_rejoin(&mut state.generated, 1, broken).is_err());
        assert!(state.generated.rejoins.is_empty());
        offer_rejoin(&mut state.generated, 1, onward.clone()).unwrap();
        assert!(!state.generated.world.follows(1, &onward.route));
        assert_eq!(state.generated.world.snapshot()[0].position, before);
        for _ in 0..12 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            if state.generated.rejoins[&1].accepted {
                break;
            }
        }
        assert!(state.generated.rejoins[&1].accepted);
        assert_eq!(state.generated.world.snapshot()[0].stage, Stage::Holding);
        assert_eq!(state.generated.world.snapshot()[0].position, before);
        assert!(Arc::ptr_eq(
            &state.generated.flights[&1].route,
            &prepared.route
        ));
        for invalidation in 0..3 {
            let mut retired = state.generated.clone();
            match invalidation {
                0 => {
                    retired.active_flights.remove(&1);
                }
                1 => {
                    retired.frames.clear();
                }
                _ => {
                    let replacement = (*retired.flights[&1]).clone();
                    retired.flights.insert(1, Arc::new(replacement));
                }
            }
            synchronise_recoveries(&mut retired);
            assert!(retired.rejoins.is_empty());
            for step in 1..=300 {
                retired.world.advance(now + f64::from(step), false);
            }
            assert!(!retired.world.follows(1, &onward.route));
            assert_eq!(retired.world.snapshot()[0].stage, Stage::Holding);
        }
        let mut reset = state.generated.clone();
        assert_eq!(
            reset.world.advance(now - 1.0, false),
            openatc_core::traffic::motion::Advance::ClockChanged
        );
        clock_changed(&mut reset);
        assert!(reset.rejoins.is_empty());
        for step in 0..300 {
            reset.world.advance(now + f64::from(step), false);
            synchronise_recoveries(&mut reset);
        }
        assert!(!reset.world.follows(1, &onward.route));
        assert_eq!(reset.world.snapshot()[0].stage, Stage::Holding);
        assert_eq!(reset.world.snapshot()[0].recovery_from, Some(4));
        let mut activated = false;
        for _ in 0..900 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            super::super::radio_tick(&mut state);
            now += 1.0;
            state.session.telemetry.simulation_seconds = Some(now);
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            if !pose.recovering {
                activated = true;
            }
            if pose.stage == Stage::Turnaround && pose.complete {
                break;
            }
        }
        assert!(activated);
        assert!(state.generated.rejoins.is_empty());
        assert!(Arc::ptr_eq(
            &state.generated.flights[&1].route,
            &onward.route
        ));
        let pose = &state.generated.world.snapshot()[0];
        assert_eq!(pose.stage, Stage::Turnaround, "{pose:?}");
        assert!(pose.complete);
    }

    #[test]
    fn complete_flight_permissions_reach_parking_and_bad_recovery_is_atomic() {
        let (mut state, prepared) = fixture();
        let mut missing_report = prepared.clone();
        missing_report.vacated_reports.clear();
        assert!(install(&mut state.generated, 1, &missing_report).is_err());
        let mut wrong_controller = prepared.clone();
        wrong_controller
            .vacated_reports
            .get_mut(&7)
            .unwrap()
            .airport = "OTHER".into();
        assert!(install(&mut state.generated, 1, &wrong_controller).is_err());
        let mut unknown_runway = prepared.clone();
        unknown_runway.vacated_reports.get_mut(&7).unwrap().runway = "99".into();
        assert!(install(&mut state.generated, 1, &unknown_runway).is_err());
        let mut broken = prepared.clone();
        broken.recovery.remove(&2);
        assert!(install(&mut state.generated, 1, &broken).is_err());
        assert!(state.generated.flights.is_empty());
        assert!(state.generated.active_flights.is_empty());
        assert_eq!(state.generated.world.snapshot()[0].stage, Stage::TaxiOut);
        assert!(state.generated.world.snapshot()[0].complete);
        install(&mut state.generated, 1, &prepared).unwrap();
        assert!(state.generated.world.snapshot()[0].awaiting_clearance);
        assert!(state.generated.flights.contains_key(&1));
        state.settings.hear_traffic_radio = true;
        state.session.telemetry.com1_khz = 120_950;
        let mut report_text = None;
        let mut vacated_reports = 0;
        let mut seen = BTreeSet::new();
        let mut frequencies = BTreeSet::new();
        let mut checkins = BTreeSet::new();
        for second in 31..900 {
            if let Some(exchange) = &mut state.generated.exchange {
                exchange.due = Instant::now();
            }
            let report_seen = state.generated.reported_crossings.contains(&(1, 6));
            super::super::radio_tick(&mut state);
            if !report_seen && state.generated.reported_crossings.contains(&(1, 6)) {
                vacated_reports += 1;
                report_text = Some(
                    state.generated.exchange.as_ref().unwrap().messages[0]
                        .1
                        .clone(),
                );
                assert_eq!(
                    state.generated.frames[0].target.com1_khz,
                    Some(120_950),
                    "Vacated report must precede Ground transfer"
                );
                assert!(
                    !state.generated.approved_orders.contains(&(1, 7)),
                    "A vacated report cannot grant taxi-in"
                );
            }
            if let Some(exchange) = &state.generated.exchange
                && !exchange.grant
                && exchange.tune_after.is_none()
                && exchange
                    .messages
                    .first()
                    .is_some_and(|(controller, _)| !controller)
            {
                checkins.insert(exchange.station.khz);
            }
            state.session.telemetry.simulation_seconds = Some(f64::from(second));
            super::super::advance(&mut state.generated, &state.session.telemetry);
            let pose = &state.generated.world.snapshot()[0];
            seen.insert(format!("{:?}", pose.stage));
            frequencies.insert(state.generated.frames[0].target.com1_khz.unwrap());
            assert!(
                !pose.recovering,
                "Normal flight entered recovery at {:?}",
                pose.stage
            );
            if pose.stage == Stage::Turnaround && pose.complete {
                break;
            }
        }
        let pose = &state.generated.world.snapshot()[0];
        assert_eq!(pose.stage, Stage::Turnaround);
        assert_eq!(vacated_reports, 1);
        let report_text = report_text.unwrap();
        let heard: Vec<_> = state
            .session
            .transcript
            .iter()
            .filter(|entry| entry.speaker == "TEST123" && entry.text == report_text)
            .collect();
        assert_eq!(heard.len(), 1, "Runway vacated must be heard exactly once");
        assert!(heard[0].background);
        assert_eq!(heard[0].radio, Some(openatc_core::state::Radio::Com1));
        assert!(
            pose.complete,
            "Generated flight did not reach parking: {pose:?}"
        );
        assert!(frequencies.contains(&124_500));
        assert!(frequencies.contains(&118_675));
        assert!(frequencies.contains(&121_705));
        for station in [124_500, 118_675, 120_950, 121_705] {
            assert!(
                checkins.contains(&station),
                "Missing generated check-in on {station}"
            );
        }
        for stage in [
            "Takeoff", "Climb", "Cruise", "Descent", "Approach", "Rollout", "TaxiIn",
        ] {
            assert!(seen.contains(stage), "Skipped generated phase {stage}");
        }
    }
}
