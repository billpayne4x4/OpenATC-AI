//! Advisory-only operations at fields without a Tower controller.
//!
//! A CTAF field is accepted only when no Tower service is published for the airport. The
//! pattern below therefore cannot issue or satisfy a controlled clearance; every leg is a
//! self-announced advisory. Towered fields keep their existing controlled-clearance path.
use crate::{
    airport::Airport,
    state::Point,
    stations::Station,
    traffic::motion::{Route, Segment, Stage, Waypoint},
};
use std::sync::Arc;

/// A published common traffic advisory frequency at an untowered field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// Airport identifier.
    pub airport: String,
    /// Advisory frequency in kHz.
    pub frequency_khz: i32,
}

fn tower_serves(airport: &str, stations: &[Station]) -> bool {
    stations.iter().any(|station| {
        station.airport == airport
            && (station.service == "Tower" || station.services.iter().any(|s| s == "Tower"))
    })
}

fn publishes_advisory(service: &str, name: &str) -> bool {
    let service = service.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    service.contains("ctaf")
        || service.contains("unicom")
        || name.contains("ctaf")
        || name.contains("unicom")
}

fn advisory_frequency(airport: &Airport, stations: &[Station]) -> Option<i32> {
    stations
        .iter()
        .filter(|station| station.airport == airport.icao)
        .filter(|station| publishes_advisory(&station.service, &station.name))
        .map(|station| station.khz)
        .chain(
            airport
                .frequencies
                .iter()
                .filter(|frequency| publishes_advisory(&frequency.service, &frequency.name))
                .map(|frequency| frequency.khz),
        )
        .find(|khz| (118_000..=136_975).contains(khz))
}

/// Airports that publish an advisory frequency and no Tower service.
///
/// A towered airport is never listed, so advisory traffic cannot be planned where a
/// controlled clearance is required.
#[must_use]
pub fn candidates(stations: &[Station]) -> Vec<String> {
    let mut airports = std::collections::BTreeSet::new();
    for station in stations {
        if !publishes_advisory(&station.service, &station.name)
            || tower_serves(&station.airport, stations)
            || station.airport.is_empty()
        {
            continue;
        }
        airports.insert(station.airport.clone());
    }
    airports.into_iter().collect()
}

/// Resolve the advisory frequency for one field.
///
/// This refuses a towered airport even when it also publishes a UNICOM frequency, so advisory
/// traffic can never appear where a controlled clearance is required.
pub fn field(airport: &Airport, stations: &[Station]) -> Result<Field, String> {
    if tower_serves(&airport.icao, stations) {
        return Err(format!(
            "{} is towered; controlled clearance is required",
            airport.icao
        ));
    }
    let frequency_khz = advisory_frequency(airport, stations)
        .ok_or_else(|| format!("{} publishes no CTAF or UNICOM frequency", airport.icao))?;
    Ok(Field {
        airport: airport.icao.clone(),
        frequency_khz,
    })
}

/// One self-announced leg of a visual pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct Advisory {
    /// Leg that produced the call.
    pub leg: &'static str,
    /// Spoken advisory, containing the callsign, field and leg.
    pub text: String,
}

/// The advisory made on one leg of a circuit.
#[derive(Clone, Debug, PartialEq)]
pub struct Leg {
    /// Route segment the aircraft is established on when the advisory is made.
    pub segment: usize,
    /// The advisory made on that leg.
    pub advisory: Advisory,
}

/// One circuit of a visual pattern: its geometry and the advisories made on it.
#[derive(Clone, Debug)]
pub struct Circuit {
    route: Arc<Route>,
    legs: Vec<Leg>,
}

impl Circuit {
    /// Build a circuit from an explicit route and advisory legs.
    ///
    /// Local surface departures use this to map the hold, takeoff and climb
    /// segments onto one procedure without inventing a second circuit.
    #[must_use]
    pub fn new(route: Arc<Route>, legs: Vec<Leg>) -> Self {
        Self { route, legs }
    }

    /// Geometry of this circuit.
    #[must_use]
    pub fn route(&self) -> Arc<Route> {
        self.route.clone()
    }
    /// Advisories made on this circuit, in circuit order.
    #[must_use]
    pub fn legs(&self) -> &[Leg] {
        &self.legs
    }
    /// The advisory made on one segment, when the circuit makes one there.
    #[must_use]
    pub fn advisory(&self, segment: usize) -> Option<&Advisory> {
        self.legs
            .iter()
            .find(|leg| leg.segment == segment)
            .map(|leg| &leg.advisory)
    }
}

/// Legs of a circuit and the segment each advisory is made on.
///
/// Every leg of the circuit carries its own call, so an aircraft announces the leg it is
/// established on.
const LEGS: [(&str, &str, usize); 4] = [
    ("downwind", "ctaf_downwind", 0),
    ("base", "ctaf_base", 1),
    ("final", "ctaf_final", 2),
    ("landing", "ctaf_landing", 3),
];

/// Legs of a cross-country flight and the segment each advisory is made on.
const CROSS_COUNTRY_LEGS: [(&str, &str, usize, bool); 5] = [
    ("departing", "ctaf_departing", 0, true),
    ("leaving", "ctaf_leaving", 1, true),
    ("five_miles", "ctaf_five_miles", 3, false),
    ("final", "ctaf_final", 4, false),
    ("landing", "ctaf_landing", 5, false),
];

/// Legs of a departure and the segment each advisory is made on.
const DEPARTURE_LEGS: [(&str, &str, usize); 2] = [
    ("departing", "ctaf_departing", 0),
    ("leaving", "ctaf_leaving", 1),
];

/// Legs of a straight-in arrival and the segment each advisory is made on.
const STRAIGHT_IN_LEGS: [(&str, &str, usize); 3] = [
    ("five_miles", "ctaf_five_miles", 0),
    ("final", "ctaf_final", 1),
    ("landing", "ctaf_landing", 2),
];

/// Climb and descent performance used to check the pattern geometry.
const CLIMB_FPM: f64 = 1_000.0;
const DESCENT_FPM: f64 = 1_000.0;

/// Distance from the runway centreline to the downwind leg.
const OFFSET_METERS: f64 = 1_500.0;
/// Distance from the threshold to the final approach fix.
const FINAL_METERS: f64 = 3_000.0;
/// Distance from the threshold to the start of a straight-in arrival.
const STRAIGHT_IN_METERS: f64 = 9_000.0;
/// Distance from the threshold to the short final fix.
const SHORT_FINAL_METERS: f64 = 3_000.0;

/// Length of the straight climb ahead of the runway after a landing.
const UPWIND_METERS: f64 = 3_200.0;
/// Lead of the crosswind leg beyond the end of the climb.
const CROSSWIND_METERS: f64 = 1_500.0;
/// Height above the field of a visual pattern.
const PATTERN_FEET: f64 = 1_000.0;

/// Local geometry of a right-hand circuit, in metres along and right of the runway.
///
/// The downwind is flown on the right of the runway and against the landing direction, so
/// the aircraft turns base towards the runway and lands on the runway heading.
struct Geometry {
    reference_latitude: f64,
    reference_longitude: f64,
    /// Threshold of the runway the circuit uses, in the airport's local plane.
    origin: Point,
    /// Unit vector along the runway.
    along: (f64, f64),
    /// Unit vector to the right of the runway.
    right: (f64, f64),
    /// Runway length in metres.
    length: f64,
    /// Field elevation in feet.
    elevation: f64,
    /// Pattern altitude in feet.
    pattern_altitude: f64,
    /// Runway resource held during the landing roll.
    runway_resource: String,
}

impl Geometry {
    /// A point on the pattern, measured along and right of the runway.
    fn at(&self, along: f64, across: f64, altitude: f64) -> Waypoint {
        let east = self.origin.east + self.along.0 * along + self.right.0 * across;
        let north = self.origin.north + self.along.1 * along + self.right.1 * across;
        let latitude = self.reference_latitude + north / 111_320.0;
        let longitude = self.reference_longitude
            + east
                / (111_320.0
                    * (self.reference_latitude * std::f64::consts::PI / 180.0)
                        .cos()
                        .max(0.01));
        Waypoint {
            latitude,
            longitude,
            altitude_feet: altitude,
        }
    }
    /// The distance along and right of the runway of a point.
    fn local(&self, point: Waypoint) -> (f64, f64) {
        let north = (point.latitude - self.reference_latitude) * 111_320.0 - self.origin.north;
        let east = (point.longitude - self.reference_longitude)
            * 111_320.0
            * (self.reference_latitude * std::f64::consts::PI / 180.0)
                .cos()
                .max(0.01)
            - self.origin.east;
        (
            east * self.along.0 + north * self.along.1,
            east * self.right.0 + north * self.right.1,
        )
    }
    /// The point a given distance ahead of another point, on the same track.
    fn ahead(&self, point: Waypoint, distance: f64, altitude: f64) -> Waypoint {
        let (along, across) = self.local(point);
        self.at(along + distance, across, altitude)
    }
    /// End of the landing roll.
    fn rollout_end(&self) -> Waypoint {
        self.at(self.length.min(900.0), 0.0, self.elevation)
    }
    /// Top of the straight climb ahead of the runway.
    fn upwind_end(&self, from: &Waypoint) -> Waypoint {
        self.ahead(*from, UPWIND_METERS, self.pattern_altitude)
    }
    /// Start of the downwind leg.
    fn downwind_start(&self, from: &Waypoint) -> Waypoint {
        let (along, _) = self.local(self.upwind_end(from));
        self.at(
            along + CROSSWIND_METERS,
            OFFSET_METERS,
            self.pattern_altitude,
        )
    }
    /// End of the downwind leg, past the threshold on the right of the runway.
    fn downwind_end(&self) -> Waypoint {
        self.at(-FINAL_METERS, OFFSET_METERS, self.pattern_altitude)
    }
    /// Start of the final approach, on the extended centreline.
    fn base_end(&self) -> Waypoint {
        self.at(-FINAL_METERS, 0.0, self.pattern_altitude)
    }
    /// Threshold of the runway.
    fn threshold(&self) -> Waypoint {
        self.at(0.0, 0.0, self.elevation)
    }
    /// The circuit itself: downwind, base, final and the landing roll.
    fn segments(&self, downwind_start: Waypoint) -> Vec<Segment> {
        vec![
            Segment {
                stage: Stage::Descent,
                points: vec![downwind_start, self.downwind_end()],
                knots: 90.0,
                resources: vec![],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::Descent,
                points: vec![self.downwind_end(), self.base_end()],
                knots: 90.0,
                resources: vec![],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::Approach,
                points: vec![self.base_end(), self.threshold()],
                knots: 85.0,
                resources: vec![],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::Rollout,
                points: vec![self.threshold(), self.rollout_end()],
                knots: 70.0,
                resources: vec![self.runway_resource.clone()],
                dwell_seconds: 0.0,
            },
        ]
    }
}

/// One advisory of a cross-country flight, with the field that hears it.
#[derive(Clone, Debug, PartialEq)]
pub struct CrossCountryLeg {
    /// Segment on which the call is made.
    pub segment: usize,
    /// Spoken advisory.
    pub advisory: Advisory,
    /// Field whose frequency carries the call.
    pub field: Field,
}

/// A VFR flight from one untowered field to another.
///
/// The route starts on the departure runway, climbs and cruises, then joins the destination
/// five-mile final and lands. Calls are split between the two CTAF frequencies: the departure
/// and leaving calls belong to the origin, the approach and landing calls to the destination.
pub struct CrossCountry {
    callsign: String,
    origin: Field,
    destination: Field,
    origin_runway: String,
    destination_runway: String,
    route: Arc<Route>,
    legs: Vec<CrossCountryLeg>,
    destination_pattern: Arc<Pattern>,
}

impl CrossCountry {
    /// Build a cross-country route between two untowered fields.
    pub fn new(
        origin_airport: &Airport,
        origin_runway: &str,
        destination_airport: &Airport,
        destination_runway: &str,
        callsign: &str,
        origin: Field,
        destination: Field,
    ) -> Result<Self, String> {
        if origin.airport != origin_airport.icao || destination.airport != destination_airport.icao
        {
            return Err("CTAF field belongs to another airport".into());
        }
        if origin_airport.icao == destination_airport.icao {
            return Err("a cross-country flight needs two different fields".into());
        }
        if callsign.trim().is_empty() {
            return Err("CTAF traffic needs a callsign".into());
        }
        let from = Pattern::geometry(origin_airport, origin_runway)?;
        let to = Pattern::geometry(destination_airport, destination_runway)?;
        let cruise = from.elevation.max(to.elevation) + 3_500.0;
        let upwind = from.at(from.length + 1_500.0, 0.0, from.pattern_altitude);
        let midpoint = Waypoint {
            latitude: f64::midpoint(
                origin_airport.reference_latitude,
                destination_airport.reference_latitude,
            ),
            longitude: f64::midpoint(
                origin_airport.reference_longitude,
                destination_airport.reference_longitude,
            ),
            altitude_feet: cruise,
        };
        let five_miles = to.at(-9_000.0, 0.0, to.pattern_altitude);
        let short_final = to.at(-3_000.0, 0.0, to.pattern_altitude - 300.0);
        let threshold = to.threshold();
        let rollout = to.rollout_end();
        let destination_pattern = Arc::new(Pattern::new(
            destination_airport,
            destination_runway,
            callsign,
            destination.clone(),
        )?);
        let route = Route::compile(
            vec![
                Segment {
                    stage: Stage::Climb,
                    points: vec![from.threshold(), upwind],
                    knots: 75.0,
                    resources: vec![from.runway_resource.clone()],
                    dwell_seconds: 0.0,
                },
                Segment {
                    stage: Stage::Climb,
                    points: vec![upwind, midpoint],
                    knots: 95.0,
                    resources: vec![],
                    dwell_seconds: 0.0,
                },
                Segment {
                    stage: Stage::Descent,
                    points: vec![midpoint, five_miles],
                    knots: 95.0,
                    resources: vec![],
                    dwell_seconds: 0.0,
                },
                Segment {
                    stage: Stage::Approach,
                    points: vec![five_miles, short_final],
                    knots: 85.0,
                    resources: vec![],
                    dwell_seconds: 0.0,
                },
                Segment {
                    stage: Stage::Approach,
                    points: vec![short_final, threshold],
                    knots: 80.0,
                    resources: vec![],
                    dwell_seconds: 0.0,
                },
                Segment {
                    stage: Stage::Rollout,
                    points: vec![threshold, rollout],
                    knots: 70.0,
                    resources: vec![to.runway_resource.clone()],
                    dwell_seconds: 0.0,
                },
            ],
            CLIMB_FPM,
            DESCENT_FPM,
        )
        .map_err(|error| format!("cross-country geometry: {error}"))?;
        let callsign = callsign.trim();
        let legs = CROSS_COUNTRY_LEGS
            .iter()
            .map(|(leg, key, segment, at_origin)| {
                let (field, airport, runway) = if *at_origin {
                    (&origin, origin_airport.icao.as_str(), origin_runway)
                } else {
                    (
                        &destination,
                        destination_airport.icao.as_str(),
                        destination_runway,
                    )
                };
                CrossCountryLeg {
                    segment: *segment,
                    advisory: Advisory {
                        leg,
                        text: crate::dialogue::say(
                            key,
                            &[
                                ("airport", airport.to_owned()),
                                ("callsign", callsign.to_owned()),
                                ("runway", runway.to_owned()),
                            ],
                        ),
                    },
                    field: field.clone(),
                }
            })
            .collect();
        Ok(Self {
            callsign: callsign.to_owned(),
            origin,
            destination,
            origin_runway: origin_runway.to_owned(),
            destination_runway: destination_runway.to_owned(),
            route: Arc::new(route),
            legs,
            destination_pattern,
        })
    }

    /// Callsign making the advisories.
    #[must_use]
    pub fn callsign(&self) -> &str {
        &self.callsign
    }
    /// Origin field.
    #[must_use]
    pub fn origin_field(&self) -> &Field {
        &self.origin
    }
    /// Destination field.
    #[must_use]
    pub fn destination_field(&self) -> &Field {
        &self.destination
    }
    /// Origin runway.
    #[must_use]
    pub fn origin_runway(&self) -> &str {
        &self.origin_runway
    }
    /// Destination runway.
    #[must_use]
    pub fn destination_runway(&self) -> &str {
        &self.destination_runway
    }
    /// Flight geometry. It ends on the destination landing roll.
    #[must_use]
    pub fn route(&self) -> Arc<Route> {
        self.route.clone()
    }
    /// Advisories and the field each one is made on.
    #[must_use]
    pub fn legs(&self) -> &[CrossCountryLeg] {
        &self.legs
    }
    /// Runway resource held during the destination landing roll.
    #[must_use]
    pub fn landing_runway_resource(&self) -> &str {
        self.destination_pattern.runway_resource()
    }

    /// Circuit flown when the destination runway is occupied at the final.
    pub fn go_around_circuit(&self, from: Waypoint) -> Result<Circuit, String> {
        self.destination_pattern.go_around_circuit(from)
    }

    /// Destination pattern, used when the arrival must continue locally.
    #[must_use]
    pub fn destination_pattern(&self) -> Arc<Pattern> {
        self.destination_pattern.clone()
    }

    /// The advisory made on a segment, with the field that hears it.
    #[must_use]
    pub fn advisory(&self, segment: usize) -> Option<(&Advisory, &Field)> {
        self.legs
            .iter()
            .find(|leg| leg.segment == segment)
            .map(|leg| (&leg.advisory, &leg.field))
    }
}

/// A right-hand visual circuit for one aircraft at an untowered field.
pub struct Pattern {
    field: Field,
    callsign: String,
    runway: String,
    geometry: Geometry,
    circuit: Circuit,
    advisories: Vec<Advisory>,
    straight_in: bool,
    departure: bool,
}

impl Pattern {
    /// Runway geometry shared by the circuit and the straight-in arrival.
    fn geometry(airport: &Airport, runway_name: &str) -> Result<Geometry, String> {
        let runway = airport
            .runways
            .iter()
            .find(|runway| runway.first_name == runway_name || runway.second_name == runway_name)
            .ok_or("CTAF pattern runway is unavailable")?;
        let (start, end) = if runway.first_name == runway_name {
            (runway.first, runway.second)
        } else {
            (runway.second, runway.first)
        };
        let (east, north) = (end.east - start.east, end.north - start.north);
        let length = east.hypot(north);
        if !length.is_finite() || length < 300.0 {
            return Err("CTAF pattern runway is too short for a circuit".into());
        }
        let (along_east, along_north) = (east / length, north / length);
        // Right normal in the local east/north plane.
        let (right_east, right_north) = (along_north, -along_east);
        Ok(Geometry {
            reference_latitude: airport.reference_latitude,
            reference_longitude: airport.reference_longitude,
            origin: start,
            along: (along_east, along_north),
            right: (right_east, right_north),
            length,
            elevation: airport.elevation_feet,
            pattern_altitude: airport.elevation_feet + PATTERN_FEET,
            runway_resource: format!(
                "RUNWAY/{}/{}/{}",
                airport.icao, runway.first_name, runway.second_name
            ),
        })
    }

    /// Build a right-hand circuit ending in a landing rollout on the named runway.
    ///
    /// The route is advisory traffic geometry only. It grants no movement and carries no
    /// controlled-clearance contract.
    pub fn new(
        airport: &Airport,
        runway_name: &str,
        callsign: &str,
        field: Field,
    ) -> Result<Self, String> {
        if field.airport != airport.icao {
            return Err("CTAF field belongs to another airport".into());
        }
        if callsign.trim().is_empty() {
            return Err("CTAF traffic needs a callsign".into());
        }
        let geometry = Self::geometry(airport, runway_name)?;
        // The first circuit starts on the downwind, abeam the far end of the runway.
        let entry = geometry.at(geometry.length, OFFSET_METERS, geometry.pattern_altitude);
        let circuit = Self::compile(
            geometry.segments(entry),
            0,
            callsign.trim(),
            &field,
            runway_name,
            &LEGS,
            false,
        )?;
        let advisories = circuit
            .legs()
            .iter()
            .map(|leg| leg.advisory.clone())
            .collect::<Vec<_>>();
        Ok(Self {
            field,
            callsign: callsign.trim().to_owned(),
            runway: runway_name.to_owned(),
            geometry,
            circuit,
            advisories,
            straight_in: false,
            departure: false,
        })
    }

    /// Build a straight-in arrival from about five miles final to the landing roll.
    ///
    /// The arrival carries the same advisory authority as the circuit, uses the runway only
    /// for its landing roll and never repeats.
    pub fn straight_in(
        airport: &Airport,
        runway_name: &str,
        callsign: &str,
        field: Field,
    ) -> Result<Self, String> {
        if field.airport != airport.icao {
            return Err("CTAF field belongs to another airport".into());
        }
        if callsign.trim().is_empty() {
            return Err("CTAF traffic needs a callsign".into());
        }
        let geometry = Self::geometry(airport, runway_name)?;
        let fix = geometry.at(-STRAIGHT_IN_METERS, 0.0, geometry.pattern_altitude);
        let short_final = geometry.at(-SHORT_FINAL_METERS, 0.0, geometry.pattern_altitude - 300.0);
        let segments = vec![
            Segment {
                stage: Stage::Approach,
                points: vec![fix, short_final],
                knots: 85.0,
                resources: vec![],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::Approach,
                points: vec![short_final, geometry.threshold()],
                knots: 80.0,
                resources: vec![],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::Rollout,
                points: vec![geometry.threshold(), geometry.rollout_end()],
                knots: 70.0,
                resources: vec![geometry.runway_resource.clone()],
                dwell_seconds: 0.0,
            },
        ];
        let circuit = Self::compile(
            segments,
            0,
            callsign.trim(),
            &field,
            runway_name,
            &STRAIGHT_IN_LEGS,
            false,
        )?;
        let advisories = circuit
            .legs()
            .iter()
            .map(|leg| leg.advisory.clone())
            .collect();
        Ok(Self {
            field,
            callsign: callsign.trim().to_owned(),
            runway: runway_name.to_owned(),
            geometry,
            circuit,
            advisories,
            straight_in: true,
            departure: false,
        })
    }

    /// Build a departure from the runway threshold and out of the pattern.
    ///
    /// The first segment holds the runway resource while the aircraft climbs along it; the
    /// resource is released when the aircraft turns out. The departure ends after one pass.
    pub fn departure(
        airport: &Airport,
        runway_name: &str,
        callsign: &str,
        field: Field,
    ) -> Result<Self, String> {
        if field.airport != airport.icao {
            return Err("CTAF field belongs to another airport".into());
        }
        if callsign.trim().is_empty() {
            return Err("CTAF traffic needs a callsign".into());
        }
        let geometry = Self::geometry(airport, runway_name)?;
        let upwind = geometry.at(geometry.length + 1_500.0, 0.0, geometry.pattern_altitude);
        let turnout = geometry.at(
            geometry.length + 6_000.0,
            2_000.0,
            geometry.pattern_altitude + 500.0,
        );
        let segments = vec![
            Segment {
                stage: Stage::Climb,
                points: vec![geometry.threshold(), upwind],
                knots: 75.0,
                resources: vec![geometry.runway_resource.clone()],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::Climb,
                points: vec![upwind, turnout],
                knots: 85.0,
                resources: vec![],
                dwell_seconds: 0.0,
            },
        ];
        let circuit = Self::compile(
            segments,
            0,
            callsign.trim(),
            &field,
            runway_name,
            &DEPARTURE_LEGS,
            true,
        )?;
        let advisories = circuit
            .legs()
            .iter()
            .map(|leg| leg.advisory.clone())
            .collect();
        Ok(Self {
            field,
            callsign: callsign.trim().to_owned(),
            runway: runway_name.to_owned(),
            geometry,
            circuit,
            advisories,
            straight_in: false,
            departure: true,
        })
    }

    /// Compile a circuit and the advisories made on it.
    fn compile(
        segments: Vec<Segment>,
        offset: usize,
        callsign: &str,
        field: &Field,
        runway_name: &str,
        legs: &[(&'static str, &'static str, usize)],
        departure: bool,
    ) -> Result<Circuit, String> {
        let route = if departure {
            Route::compile_departure(segments, CLIMB_FPM, DESCENT_FPM)
        } else {
            Route::compile(segments, CLIMB_FPM, DESCENT_FPM)
        }
        .map_err(|error| format!("CTAF pattern geometry: {error}"))?;
        let legs = legs
            .iter()
            .map(|(leg, key, segment)| Leg {
                segment: segment + offset,
                advisory: Advisory {
                    leg,
                    text: crate::dialogue::say(
                        key,
                        &[
                            ("airport", field.airport.clone()),
                            ("callsign", callsign.to_owned()),
                            ("runway", runway_name.to_owned()),
                        ],
                    ),
                },
            })
            .collect::<Vec<_>>();
        Ok(Circuit {
            route: Arc::new(route),
            legs,
        })
    }

    /// Advisory frequency this pattern uses.
    #[must_use]
    pub fn field(&self) -> &Field {
        &self.field
    }

    /// Runway used by the circuit.
    #[must_use]
    pub fn runway(&self) -> &str {
        &self.runway
    }

    /// The runway resource the pattern holds during the landing roll.
    #[must_use]
    pub fn runway_resource(&self) -> &str {
        &self.geometry.runway_resource
    }

    /// Whether this arrival is a straight-in rather than a repeating circuit.
    #[must_use]
    pub fn is_straight_in(&self) -> bool {
        self.straight_in
    }

    /// Whether this procedure is a departure rather than an arrival.
    #[must_use]
    pub fn is_departure(&self) -> bool {
        self.departure
    }

    /// Whether the procedure ends after one pass and must not repeat.
    #[must_use]
    pub fn ends_after_one_pass(&self) -> bool {
        self.straight_in || self.departure
    }

    /// Callsign making the advisories.
    #[must_use]
    pub fn callsign(&self) -> &str {
        &self.callsign
    }

    /// Geometry of the circuit. It carries no controlled clearance or takeoff grant.
    #[must_use]
    pub fn route(&self) -> Arc<Route> {
        self.circuit.route()
    }

    /// The first circuit, with the advisories made on it.
    #[must_use]
    pub fn circuit(&self) -> &Circuit {
        &self.circuit
    }

    /// One advisory per leg, in circuit order. Wording never contains a clearance.
    #[must_use]
    pub fn advisories(&self) -> &[Advisory] {
        &self.advisories
    }

    /// The circuit that follows the one just flown.
    ///
    /// The aircraft climbs from the end of the landing roll, turns onto the downwind and
    /// flies the same circuit again. The continuation is the same geometry every time, so a
    /// pattern can be repeated without a controller.
    pub fn next_circuit(&self) -> Result<Circuit, String> {
        if self.ends_after_one_pass() {
            return Err("a one-pass procedure does not repeat".into());
        }
        self.circuit_from(self.geometry.rollout_end())
    }

    /// The circuit that follows a go-around from the position the aircraft abandoned.
    ///
    /// The aircraft climbs away from the runway and rejoins the downwind, so an occupied
    /// runway is never used.
    pub fn go_around_circuit(&self, from: Waypoint) -> Result<Circuit, String> {
        self.circuit_from(from)
    }

    /// Climb from a position back to the downwind, then fly the circuit.
    fn circuit_from(&self, entry: Waypoint) -> Result<Circuit, String> {
        let geometry = &self.geometry;
        let climb_end = geometry.upwind_end(&entry);
        let downwind_start = geometry.downwind_start(&entry);
        let mut segments = vec![
            Segment {
                stage: Stage::Climb,
                points: vec![entry, climb_end],
                knots: 70.0,
                resources: vec![],
                dwell_seconds: 0.0,
            },
            Segment {
                stage: Stage::Descent,
                points: vec![climb_end, downwind_start],
                knots: 90.0,
                resources: vec![],
                dwell_seconds: 0.0,
            },
        ];
        segments.extend(geometry.segments(downwind_start));
        Self::compile(
            segments,
            2,
            self.callsign.trim(),
            &self.field,
            self.runway.as_str(),
            &LEGS,
            false,
        )
    }

    /// Advisory when the runway is still occupied at the final call. The pilot goes around;
    /// no landing authority is created by this message.
    #[must_use]
    pub fn go_around(&self) -> Advisory {
        Advisory {
            leg: "final",
            text: crate::dialogue::say(
                "ctaf_go_around",
                &[
                    ("airport", self.field.airport.clone()),
                    ("callsign", self.callsign.clone()),
                    ("runway", self.runway.clone()),
                ],
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field_airport() -> Airport {
        let mut airport = crate::airport::demo_airport();
        airport.icao = "KCTF".into();
        airport.frequencies = vec![crate::airport::Frequency {
            service: "CTAF".into(),
            name: "CTAF".into(),
            khz: 122_900,
        }];
        airport
    }

    fn station(service: &str, khz: i32) -> Station {
        Station {
            airport: "KCTF".into(),
            service: service.into(),
            khz,
            ..Default::default()
        }
    }

    #[test]
    fn advisory_fields_refuse_any_towered_airport() {
        let airport = field_airport();
        let towered = [station("Tower", 118_300), station("Unicom", 122_800)];
        assert!(
            field(&airport, &towered).is_err(),
            "a towered field must never expose CTAF operations"
        );
        assert!(field(&airport, &[]).is_ok());
        let untowered = [station("Unicom", 122_800)];
        assert_eq!(field(&airport, &untowered).unwrap().frequency_khz, 122_800);
    }

    #[test]
    fn advisory_field_requires_a_published_frequency() {
        let mut airport = field_airport();
        airport.frequencies.clear();
        assert!(field(&airport, &[]).is_err());
        airport.frequencies.push(crate::airport::Frequency {
            service: "Unicom".into(),
            name: "Unicom".into(),
            khz: 122_700,
        });
        assert_eq!(field(&airport, &[]).unwrap().frequency_khz, 122_700);
    }

    #[test]
    fn only_untowered_fields_with_an_advisory_frequency_are_candidates() {
        let untowered = station("Unicom", 122_800);
        let towered = Station {
            airport: "KTOW".into(),
            service: "Tower".into(),
            khz: 118_300,
            ..Default::default()
        };
        let towered_advisory = Station {
            airport: "KTOW".into(),
            service: "CTAF".into(),
            khz: 122_900,
            ..Default::default()
        };
        let other = Station {
            airport: "KOTH".into(),
            service: "Ground".into(),
            khz: 121_900,
            ..Default::default()
        };
        let stations = [untowered.clone(), towered, towered_advisory, other];
        assert_eq!(candidates(&stations), vec!["KCTF".to_owned()]);
        assert!(candidates(&[]).is_empty());
    }

    #[test]
    fn a_cross_country_splits_calls_between_the_two_frequencies() {
        let origin = field_airport();
        let mut destination = field_airport();
        destination.icao = "KDST".into();
        destination.reference_latitude += 0.25;
        destination.frequencies = vec![crate::airport::Frequency {
            service: "CTAF".into(),
            name: "CTAF".into(),
            khz: 123_000,
        }];
        let origin_field = field(&origin, &[]).unwrap();
        let destination_field = field(&destination, &[]).unwrap();
        let origin_runway = origin.runways[0].first_name.clone();
        let destination_runway = destination.runways[0].first_name.clone();
        let cross = CrossCountry::new(
            &origin,
            &origin_runway,
            &destination,
            &destination_runway,
            "N12345",
            origin_field,
            destination_field,
        )
        .unwrap();
        assert_eq!(cross.route().segment_count(), 6);
        assert_eq!(cross.legs().len(), 5);
        assert_eq!(cross.legs()[0].field.frequency_khz, 122_900);
        assert!(cross.legs()[0].advisory.text.contains("KCTF"));
        assert_eq!(cross.legs()[2].field.frequency_khz, 123_000);
        assert_eq!(cross.legs()[2].field.airport, "KDST");
        assert!(cross.legs()[2].advisory.text.contains("KDST"));
        assert!(cross.legs()[2].advisory.text.contains("five miles final"));
        assert!(cross.legs()[4].advisory.text.contains("vacated"));
        assert!(cross.landing_runway_resource().contains("KDST"));
        let from = Waypoint {
            latitude: destination.reference_latitude,
            longitude: destination.reference_longitude,
            altitude_feet: destination.elevation_feet,
        };
        assert!(cross.go_around_circuit(from).is_ok());
        for leg in cross.legs() {
            let lower = leg.advisory.text.to_lowercase();
            assert!(!lower.contains("cleared"));
            assert!(!lower.contains("clearance"));
        }
    }

    #[test]
    fn a_pattern_announces_every_leg_and_never_grants_a_clearance() {
        let airport = field_airport();
        let field = field(&airport, &[]).unwrap();
        let runway = airport.runways[0].first_name.clone();
        let pattern = Pattern::new(&airport, &runway, "N12345", field).unwrap();
        assert_eq!(pattern.route().segment_count(), 4);
        assert_eq!(pattern.advisories().len(), 4);
        for advisory in pattern.advisories() {
            assert!(advisory.text.contains("N12345"));
            assert!(advisory.text.contains("KCTF"));
            assert!(advisory.text.contains(&runway));
            let lower = advisory.text.to_lowercase();
            assert!(!lower.contains("cleared"));
            assert!(!lower.contains("takeoff clearance"));
            assert!(!lower.contains("landing clearance"));
        }
        assert_eq!(pattern.advisories()[0].leg, "downwind");
        assert_eq!(pattern.advisories()[3].leg, "landing");
        let expected = [
            format!("KCTF traffic, N12345, entering downwind runway {runway}, KCTF"),
            format!("KCTF traffic, N12345, turning base runway {runway}, KCTF"),
            format!("KCTF traffic, N12345, final runway {runway}, KCTF"),
            format!("KCTF traffic, N12345, runway {runway} vacated, KCTF"),
        ];
        for (advisory, expected) in pattern.advisories().iter().zip(expected) {
            assert_eq!(advisory.text, expected);
        }
        let go_around = pattern.go_around();
        assert_eq!(
            go_around.text,
            format!("KCTF traffic, N12345, going around runway {runway}, KCTF")
        );
        assert!(!go_around.text.to_lowercase().contains("cleared"));
    }

    #[test]
    fn a_departure_announces_the_runway_and_leaving_the_pattern() {
        let airport = field_airport();
        let field = field(&airport, &[]).unwrap();
        let runway = airport.runways[0].first_name.clone();
        let pattern = Pattern::departure(&airport, &runway, "N12345", field).unwrap();
        assert_eq!(pattern.route().segment_count(), 2);
        assert_eq!(pattern.advisories().len(), 2);
        assert_eq!(
            pattern.advisories()[0].text,
            format!("KCTF traffic, N12345, departing runway {runway}, KCTF")
        );
        assert_eq!(
            pattern.advisories()[1].text,
            "KCTF traffic, N12345, leaving the pattern, KCTF"
        );
        assert!(pattern.is_departure());
        assert!(pattern.ends_after_one_pass());
        assert!(pattern.next_circuit().is_err());
        for advisory in pattern.advisories() {
            let lower = advisory.text.to_lowercase();
            assert!(!lower.contains("cleared"));
            assert!(!lower.contains("clearance"));
        }
    }

    #[test]
    fn a_straight_in_announces_five_miles_final_and_vacated() {
        let airport = field_airport();
        let field = field(&airport, &[]).unwrap();
        let runway = airport.runways[0].first_name.clone();
        let pattern = Pattern::straight_in(&airport, &runway, "N12345", field).unwrap();
        assert_eq!(pattern.route().segment_count(), 3);
        assert_eq!(pattern.advisories().len(), 3);
        assert_eq!(
            pattern.advisories()[0].text,
            format!("KCTF traffic, N12345, five miles final runway {runway}, KCTF")
        );
        assert_eq!(
            pattern.advisories()[2].text,
            format!("KCTF traffic, N12345, runway {runway} vacated, KCTF")
        );
        assert!(pattern.is_straight_in());
        assert!(pattern.next_circuit().is_err());
        for advisory in pattern.advisories() {
            let lower = advisory.text.to_lowercase();
            assert!(!lower.contains("cleared"));
            assert!(!lower.contains("clearance"));
        }
    }

    #[test]
    fn a_seeded_pattern_moves_on_its_first_advisory_leg_without_a_controller() {
        use crate::traffic::motion::World;
        let airport = field_airport();
        let field = field(&airport, &[]).unwrap();
        let runway = airport.runways[0].first_name.clone();
        let pattern = Pattern::new(&airport, &runway, "N12345", field).unwrap();
        let mut world = World::default();
        assert!(world.seed_airborne(0, 7, pattern.route(), 0));
        let start = world.snapshot()[0].clone();
        world.advance(0.0, false);
        world.advance(20.0, false);
        let moved = world.snapshot()[0].clone();
        assert_eq!(moved.stage, Stage::Descent);
        assert_ne!(moved.position, start.position);
        assert!(!moved.complete);
    }

    #[test]
    fn the_downwind_is_flown_against_the_landing_direction() {
        let airport = field_airport();
        let field = field(&airport, &[]).unwrap();
        let runway = airport.runways[0].first_name.clone();
        let pattern = Pattern::new(&airport, &runway, "N12345", field).unwrap();
        let route = pattern.route();
        let downwind = route.segment(0).unwrap();
        let final_approach = route.segment(2).unwrap();
        // The downwind ends past the threshold, on the right of the runway.
        let downwind_end = downwind.points[1];
        let landing_heading = crate::arrival::bearing(
            [
                final_approach.points[0].latitude,
                final_approach.points[0].longitude,
            ],
            [
                final_approach.points[1].latitude,
                final_approach.points[1].longitude,
            ],
        );
        let downwind_heading = crate::arrival::bearing(
            [downwind.points[0].latitude, downwind.points[0].longitude],
            [downwind_end.latitude, downwind_end.longitude],
        );
        let difference = (landing_heading - downwind_heading).abs();
        assert!(
            difference > 120.0 && difference < 240.0,
            "the downwind is flown against the landing direction, not with it"
        );
    }

    #[test]
    fn a_continuation_rejoins_the_downwind_from_the_end_of_the_landing_roll() {
        let airport = field_airport();
        let field = field(&airport, &[]).unwrap();
        let runway = airport.runways[0].first_name.clone();
        let pattern = Pattern::new(&airport, &runway, "N12345", field).unwrap();
        let first = pattern.circuit().clone();
        let next = pattern.next_circuit().unwrap();
        assert_eq!(next.route().segment_count(), 6);
        let last = first.route().end_position();
        let start = next.route().start_position();
        assert!(
            (last.latitude - start.latitude).abs() < 1e-6
                && (last.longitude - start.longitude).abs() < 1e-6
                && (last.altitude_feet - start.altitude_feet).abs() < 1.0,
            "the continuation starts where the landing roll ended"
        );
        // The continuation climbs away from the runway before it rejoins the downwind.
        let route = next.route();
        assert_eq!(route.segment(0).unwrap().stage, Stage::Climb);
        assert_eq!(route.segment(5).unwrap().stage, Stage::Rollout);
        // The same four advisories are made on the continuation, on the shifted segments.
        assert_eq!(next.legs().len(), 4);
        for (leg, original) in next.legs().iter().zip(first.legs()) {
            assert_eq!(leg.advisory.text, original.advisory.text);
            assert_eq!(leg.segment, original.segment + 2);
        }
        assert_eq!(next.advisory(2).unwrap().leg, "downwind");
        assert_eq!(next.advisory(5).unwrap().leg, "landing");
        // The continuation is the same geometry every time.
        let again = pattern.next_circuit().unwrap();
        assert_eq!(again.route().segment_count(), next.route().segment_count());
    }

    #[test]
    fn a_go_around_climbs_away_from_the_runway_and_never_uses_it() {
        let airport = field_airport();
        let field = field(&airport, &[]).unwrap();
        let runway = airport.runways[0].first_name.clone();
        let pattern = Pattern::new(&airport, &runway, "N12345", field).unwrap();
        let threshold = pattern.route().end_position();
        let circuit = pattern.go_around_circuit(threshold).unwrap();
        assert_eq!(circuit.route().segment_count(), 6);
        // The first leg climbs, so the aircraft leaves the runway before it rejoins.
        let route = circuit.route();
        let climb = route.segment(0).unwrap();
        assert_eq!(climb.stage, Stage::Climb);
        assert!(climb.points[1].altitude_feet > climb.points[0].altitude_feet);
        // The landing roll is still the last leg of the circuit.
        let rollout = route.segment(5).unwrap();
        assert_eq!(rollout.stage, Stage::Rollout);
    }
}
