//! Editable traffic preferences and aircraft envelopes, independent of CSL paint names.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use super::csl::Catalogue;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Small bundled lookup for operational checks that cannot wait for an editable catalogue reload.
#[must_use]
pub fn aircraft(code: &str) -> Option<&'static Aircraft> {
    static DATA: std::sync::OnceLock<Option<Profiles>> = std::sync::OnceLock::new();
    let code = code.trim().to_ascii_uppercase();
    DATA.get_or_init(|| Profiles::builtin().ok())
        .as_ref()?
        .aircraft_for(&code)
}

/// Activity mix. These categories do not determine wake or approach category.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Mainline passenger operators.
    Airline,
    /// Regional passenger operators, including turboprops.
    Regional,
    /// Freight operators.
    Cargo,
    /// Private and business IFR aircraft.
    GeneralAviation,
    /// Military operators.
    Military,
}

/// Nominal movement envelope for generated aircraft, not an aircraft flight manual.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Aircraft {
    /// Optional managed speed range outside final approach; absent means no speed offer.
    #[serde(default)]
    pub spacing_speed: Option<SpacingSpeed>,
    /// Equivalent advertised model types using this same equipment and movement envelope.
    #[serde(default)]
    pub model_aliases: Vec<String>,
    /// Movement/animation envelope, separate from wake and approach categories.
    #[serde(default)]
    pub handling: Option<super::animation::Kind>,
    /// Fixed undercarriage remains extended in every phase.
    #[serde(default)]
    pub fixed_gear: bool,
    /// Human-readable type.
    pub name: String,
    /// Metres.
    pub wingspan: f64,
    /// Metres, used to keep the tail clear of intersections.
    pub length: f64,
    /// L, M, H or J; approach category is deliberately separate.
    pub wake: char,
    /// FAA consolidated wake category, separate from ICAO wake and approach category.
    #[serde(default)]
    pub cwt: Option<char>,
    /// ICAO approach category A through E.
    pub approach_category: char,
    /// Conservative dry-runway planning floor in metres.
    pub runway: f64,
    /// Nominal range in nautical miles; reserve is applied when selecting routes.
    pub range_nm: f64,
    /// Cruise true airspeed in knots.
    pub cruise_knots: f64,
    /// Maximum planned cruise altitude, feet MSL.
    pub ceiling: f64,
    /// Nominal final approach speed, knots.
    pub approach_knots: f64,
    /// Nominal climb rate, feet per minute.
    pub climb_fpm: f64,
    /// Nominal descent rate, feet per minute.
    pub descent_fpm: f64,
    /// Propeller animation is required when supported by the model.
    pub propeller: bool,
    /// Equipment assumed for this simulated aircraft.
    pub rnav: bool,
}

/// Nominal configured range for ATC spacing, not a stall-speed calculation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpacingSpeed {
    /// Lowest offered indicated speed in knots.
    pub minimum_knots: f64,
    /// Highest offered indicated speed in knots.
    pub maximum_knots: f64,
}

/// Geometry and timing of the limited automatic approach-speed workflow.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpacingPolicy {
    /// Reaction, deceleration and runway-planning margins; never separation minima.
    #[serde(default)]
    pub prediction: super::sequencing::Margins,
    /// Outer approach distance where speed offers begin.
    pub start_nm: f64,
    /// Maximum distance for this workflow.
    pub end_nm: f64,
    /// Resume published speeds before the final segment.
    pub release_nm: f64,
    /// Altitude ceiling of this terminal speed-offer workflow.
    pub maximum_altitude_feet: f64,
    /// Maximum lateral distance from the runway centreline.
    pub lateral_meters: f64,
    /// Maximum track error on an established approach.
    pub alignment_degrees: f64,
    /// Maximum altitude difference for a possible same-final encounter.
    pub vertical_feet: f64,
    /// Lower distance of the speed-adjustment window, not a separation minimum.
    pub gap_min_nm: f64,
    /// Upper distance of the speed-adjustment window.
    pub gap_max_nm: f64,
    /// Closing-speed threshold for an offer.
    pub closing_knots: f64,
    /// Offered speed increment and rounding step.
    pub speed_step_knots: f64,
    /// Ignore smaller changes.
    pub minimum_change_knots: f64,
}

impl Aircraft {
    /// Parking size derived from wingspan, never from wake or approach category.
    #[must_use]
    pub fn parking_size(&self) -> char {
        [15.0, 24.0, 36.0, 52.0, 65.0]
            .iter()
            .position(|limit| self.wingspan < *limit)
            .map_or('F', |n| char::from(b'A' + n as u8))
    }

    /// Check if this aircraft is a rotorcraft based on handling type.
    #[must_use]
    pub fn is_rotorcraft(&self) -> bool {
        matches!(
            self.handling,
            Some(crate::traffic::animation::Kind::Rotorcraft)
        )
    }
}

/// How an operator's partial route catalogue limits simulated city pairs.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoutePolicy {
    /// Use listed pairs exclusively when the operator has a route catalogue.
    #[default]
    Listed,
    /// Also permit simulated spokes through that operator's configured hubs.
    HubSpokes,
}

/// One operating fleet. A regional subsidiary can have its own entry and callsign.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Airline {
    /// Simulated registration patterns by origin ICAO prefix; ? is a letter and # a digit.
    #[serde(default)]
    pub registrations: BTreeMap<String, Vec<String>>,
    /// Optional regional letter alphabets for registration placeholders.
    #[serde(default)]
    pub registration_alphabets: BTreeMap<String, String>,
    /// Display name.
    pub name: String,
    /// ICAO operating code, empty for private registrations.
    pub operator: String,
    /// Spoken telephony name.
    pub callsign: String,
    /// Traffic-mix category.
    pub category: Category,
    /// Hub spokes require a configured operator hub when a pair is not listed.
    #[serde(default)]
    pub route_policy: RoutePolicy,
    /// At least one end must be in this home area.
    pub home_prefixes: Vec<String>,
    /// Both ends must be in these ICAO prefix areas; an empty list means worldwide.
    pub serves: Vec<String>,
    /// ICAO aircraft types and relative weights, not actual fleet counts.
    pub fleet: BTreeMap<String, u32>,
    /// Explicit permitted marketing liveries. The operating callsign stays unchanged.
    #[serde(default)]
    pub liveries: Vec<String>,
}
impl Airline {
    /// Generate a fictional registration using the most specific editable regional pattern.
    #[must_use]
    pub fn registration(&self, airport: &str, seed: u64) -> Option<String> {
        let patterns = self
            .registrations
            .iter()
            .filter(|(prefix, _)| airport.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())?
            .1;
        let mut value = mix(seed);
        let alphabet = self
            .registration_alphabets
            .iter()
            .filter(|(prefix, _)| airport.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, alphabet)| alphabet.as_bytes());
        let pattern = &patterns[(value % patterns.len() as u64) as usize];
        Some(
            pattern
                .chars()
                .map(|character| {
                    value = mix(value);
                    match character {
                        '?' => alphabet.map_or_else(
                            || char::from(b'A' + (value % 26) as u8),
                            |alphabet| {
                                char::from(alphabet[(value % alphabet.len() as u64) as usize])
                            },
                        ),
                        '#' => char::from(b'0' + (value % 10) as u8),
                        literal => literal,
                    }
                })
                .collect(),
        )
    }
}

/// Airport-specific operator preferences, not market-share percentages.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Hub {
    /// Readable airport label.
    pub name: String,
    /// Keys refer to airline profile IDs, including regional fleets.
    pub airlines: BTreeMap<String, u32>,
    /// Published airport latitude, used to match a world airport to the simulator's own code.
    #[serde(default)]
    pub latitude: Option<f64>,
    /// Published airport longitude, used to match a world airport to the simulator's own code.
    #[serde(default)]
    pub longitude: Option<f64>,
}

/// One published city pair for an operator.
///
/// The pair is unordered: a scheduled service is normally flown in both directions, so a
/// route is recorded once and matched from either end. The weight is a relative frequency
/// band inside the operator's own network, not a schedule and not a seat count.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// Profile id of the operating airline.
    pub airline: String,
    /// Origin airport ICAO.
    pub origin: String,
    /// Destination airport ICAO.
    pub destination: String,
    /// Relative frequency band for the pair.
    pub weight: u32,
    /// Aircraft types published for the pair, when the operator publishes equipment.
    #[serde(default)]
    pub equipment: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AircraftFile {
    version: u32,
    spacing: Option<SpacingPolicy>,
    aircraft: BTreeMap<String, Aircraft>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AirlineFile {
    version: u32,
    airline: BTreeMap<String, Airline>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HubFile {
    version: u32,
    airport: BTreeMap<String, Hub>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutesFile {
    version: u32,
    route: Vec<Route>,
}

/// Validated profile set, shared between traffic generation and diagnostics.
#[derive(Clone, Debug, Serialize)]
pub struct Profiles {
    /// Shared, editable approach-spacing thresholds.
    pub spacing: SpacingPolicy,
    /// ICAO type to movement envelope.
    pub aircraft: BTreeMap<String, Aircraft>,
    /// Stable profile ID to operating fleet.
    pub airlines: BTreeMap<String, Airline>,
    /// ICAO airport to operator preferences.
    pub hubs: BTreeMap<String, Hub>,
    /// Published operator city pairs, keyed by airline profile id.
    pub routes: BTreeMap<String, Vec<Route>>,
}

/// Only known runway and parking capacities may be used to generate a flight.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    /// ICAO airport identifier.
    pub icao: String,
    /// Available runway length for this fleet selection, metres.
    pub runway: f64,
    /// Largest known compatible parking size.
    pub parking_size: char,
}

/// A model-compatible choice. Stable seeds do not depend on render order.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Selection {
    /// Profile ID, independent of painted marketing airline.
    pub airline: String,
    /// Operating airline or empty for private aircraft.
    pub operator: String,
    /// Spoken operator name.
    pub spoken_operator: String,
    /// ICAO aircraft type.
    pub aircraft: String,
    /// CSL identity; renderer loads every part of this model.
    pub model: String,
    /// Selected painted operator, which may be an explicitly allowed partner.
    pub model_operator: String,
    /// Activity category for density controls.
    pub category: Category,
}

impl Profiles {
    /// Resolve only explicit model aliases; different variants need their own envelope.
    #[must_use]
    pub fn aircraft_for(&self, code: &str) -> Option<&Aircraft> {
        self.aircraft.get(code).or_else(|| {
            self.aircraft
                .values()
                .find(|aircraft| aircraft.model_aliases.iter().any(|alias| alias == code))
        })
    }
    /// Parse the three profile files together so unresolved fleet and hub references fail visibly.
    pub fn parse(aircraft: &str, airlines: &str, hubs: &str) -> Result<Self, String> {
        Self::parse_all(aircraft, airlines, hubs, None)
    }
    /// Parse the profile files and the published route graph together.
    ///
    /// Route data is optional here so a personal profile set without routes still loads; the
    /// bundled set always carries its routes.
    pub fn parse_all(
        aircraft: &str,
        airlines: &str,
        hubs: &str,
        routes: Option<&str>,
    ) -> Result<Self, String> {
        if [aircraft, airlines, hubs]
            .iter()
            .any(|s| s.len() > 1024 * 1024)
        {
            return Err("Traffic profile file exceeds 1 MiB".into());
        }
        let a: AircraftFile =
            crate::configuration::from_str(aircraft).map_err(|e| format!("aircraft.toml: {e}"))?;
        let l: AirlineFile =
            crate::configuration::from_str(airlines).map_err(|e| format!("airlines.toml: {e}"))?;
        let h: HubFile =
            crate::configuration::from_str(hubs).map_err(|e| format!("hubs.toml: {e}"))?;
        if [a.version, l.version, h.version] != [1; 3] {
            return Err("Traffic profiles require version = 1".into());
        }
        let mut published: BTreeMap<String, Vec<Route>> = BTreeMap::new();
        if let Some(text) = routes {
            if text.len() > 16 * 1024 * 1024 {
                return Err("Traffic route file exceeds 16 MiB".into());
            }
            let r: RoutesFile =
                crate::configuration::from_str(text).map_err(|e| format!("routes.toml: {e}"))?;
            if r.version != 1 {
                return Err("Traffic routes require version = 1".into());
            }
            if r.route.len() > 1_000_000 {
                return Err("Traffic route count exceeds its limit".into());
            }
            for route in r.route {
                published
                    .entry(route.airline.clone())
                    .or_default()
                    .push(route);
            }
        }
        let result = Self {
            spacing: if let Some(policy) = a.spacing {
                policy
            } else {
                let defaults: toml::Value = crate::configuration::from_str(include_str!(
                    "../../../../traffic/aircraft.toml"
                ))
                .map_err(|error| error.to_string())?;
                defaults["spacing"]
                    .clone()
                    .try_into()
                    .map_err(|error: toml::de::Error| error.to_string())?
            },
            aircraft: a.aircraft,
            airlines: l.airline,
            hubs: h.airport,
            routes: published,
        };
        result.validate()?;
        Ok(result)
    }

    /// Bundled profiles are also shipped as editable TOML files.
    pub fn builtin() -> Result<Self, String> {
        Self::parse_all(
            include_str!("../../../../traffic/aircraft.toml"),
            include_str!("../../../../traffic/airlines.toml"),
            include_str!("../../../../traffic/hubs.toml"),
            Some(include_str!("../../../../traffic/routes.toml")),
        )
    }

    fn validate(&self) -> Result<(), String> {
        let policy = &self.spacing;
        if !bounded(policy.release_nm, 3.0, 6.0)
            || !bounded(policy.start_nm, policy.release_nm + 1.0, 15.0)
            || !bounded(policy.end_nm, policy.start_nm + 1.0, 20.0)
            || !bounded(policy.maximum_altitude_feet, 1000.0, 10000.0)
            || !bounded(policy.lateral_meters, 100.0, 1000.0)
            || !bounded(policy.alignment_degrees, 5.0, 30.0)
            || !bounded(policy.vertical_feet, 500.0, 2000.0)
            || !bounded(policy.gap_min_nm, 2.0, 6.0)
            || !bounded(policy.gap_max_nm, policy.gap_min_nm + 1.0, 12.0)
            || !bounded(policy.closing_knots, 5.0, 50.0)
            || !bounded(policy.speed_step_knots, 5.0, 20.0)
            || policy.speed_step_knots % 5.0 != 0.0
            || !bounded(policy.minimum_change_knots, 1.0, policy.speed_step_knots)
            || !bounded(policy.prediction.reaction_seconds, 5.0, 120.0)
            || !bounded(policy.prediction.deceleration, 0.1, 3.0)
            || !bounded(policy.prediction.release_nm, 5.0, 15.0)
            || !bounded(policy.prediction.buffer_nm, 0.0, 3.0)
            || !bounded(policy.prediction.runway_seconds, 30.0, 300.0)
        {
            return Err("Invalid approach-spacing policy".into());
        }
        if self.aircraft.is_empty()
            || self.airlines.is_empty()
            || self.aircraft.len() > 512
            || self.airlines.len() > 2048
            || self.hubs.len() > 8192
        {
            return Err("Traffic profile counts are empty or exceed supported limits".into());
        }
        for (id, a) in &self.aircraft {
            if !code(id, 2, 4)
                || a.model_aliases.len() > 8
                || a.model_aliases.iter().any(|alias| {
                    !code(alias, 2, 4) || alias == id || self.aircraft.contains_key(alias)
                })
                || a.name.trim().is_empty()
                || !"LMHJ".contains(a.wake)
                || a.cwt.is_some_and(|c| !(('A'..='I').contains(&c)))
                || !"ABCDE".contains(a.approach_category)
                || !bounded(a.wingspan, 5.0, 90.0)
                || !bounded(a.length, 5.0, 90.0)
                || (a.runway != 0.0 && !bounded(a.runway, 300.0, 5000.0))
                || !bounded(a.range_nm, 50.0, 12000.0)
                || !bounded(a.cruise_knots, 70.0, 550.0)
                || !bounded(a.ceiling, 5000.0, 60000.0)
                || !bounded(a.approach_knots, 45.0, 180.0)
                || a.approach_knots >= a.cruise_knots
                || !bounded(a.climb_fpm, 200.0, 5000.0)
                || !bounded(a.descent_fpm, 200.0, 4000.0)
                || a.spacing_speed.as_ref().is_some_and(|speed| {
                    !bounded(speed.minimum_knots, a.approach_knots, a.cruise_knots)
                        || !bounded(
                            speed.maximum_knots,
                            speed.minimum_knots + 5.0,
                            a.cruise_knots.min(250.0),
                        )
                })
            {
                return Err(format!("Invalid aircraft envelope: {id}"));
            }
        }
        let mut aliases = BTreeSet::new();
        if self
            .aircraft
            .values()
            .flat_map(|aircraft| &aircraft.model_aliases)
            .any(|alias| !aliases.insert(alias))
        {
            return Err("Aircraft model aliases must be unique".into());
        }
        for (id, a) in &self.airlines {
            if id.is_empty()
                || id.len() > 40
                || a.name.trim().is_empty()
                || (a.category != Category::GeneralAviation
                    && (!code(&a.operator, 2, 3)
                        || a.callsign.trim().is_empty()
                        || a.home_prefixes.is_empty()))
                || a.home_prefixes
                    .iter()
                    .chain(&a.serves)
                    .any(|s| !code(s, 1, 4))
                || a.liveries.iter().any(|s| !code(s, 2, 3))
                || a.liveries.len() > 16
                || a.registrations.len() > 64
                || a.registration_alphabets.len() > 64
                || a.registration_alphabets.iter().any(|(prefix, alphabet)| {
                    (!prefix.is_empty() && !code(prefix, 1, 4))
                        || alphabet.is_empty()
                        || alphabet.len() > 26
                        || !alphabet.bytes().all(|byte| byte.is_ascii_uppercase())
                        || alphabet.bytes().collect::<BTreeSet<_>>().len() != alphabet.len()
                })
                || a.registrations.iter().any(|(prefix, patterns)| {
                    (!prefix.is_empty() && !code(prefix, 1, 4))
                        || patterns.is_empty()
                        || patterns.len() > 16
                        || patterns.iter().any(|pattern| {
                            !(3..=12).contains(&pattern.len())
                                || !pattern.bytes().all(|byte| {
                                    byte.is_ascii_uppercase()
                                        || byte.is_ascii_digit()
                                        || b"-?#".contains(&byte)
                                })
                        })
                })
                || a.fleet.is_empty()
                || a.fleet
                    .iter()
                    .any(|(t, w)| !self.aircraft.contains_key(t) || *w == 0 || *w > 1000)
            {
                return Err(format!(
                    "Invalid airline profile or unknown fleet type: {id}"
                ));
            }
        }
        for (id, h) in &self.hubs {
            let coordinates = match (h.latitude, h.longitude) {
                (Some(latitude), Some(longitude)) => {
                    bounded(latitude, -90.0, 90.0) && bounded(longitude, -180.0, 180.0)
                }
                (None, None) => true,
                _ => false,
            };
            if !code(id, 3, 4)
                || h.name.trim().is_empty()
                || h.airlines.is_empty()
                || !coordinates
                || h.airlines
                    .iter()
                    .any(|(id, w)| !self.airlines.contains_key(id) || *w == 0 || *w > 1000)
            {
                return Err(format!("Invalid hub preference or unknown airline: {id}"));
            }
        }
        for (id, routes) in &self.routes {
            if !self.airlines.contains_key(id) {
                return Err(format!("Published routes name an unknown airline: {id}"));
            }
            let mut seen = BTreeSet::new();
            for route in routes {
                if !code(&route.origin, 4, 4)
                    || !code(&route.destination, 4, 4)
                    || route.origin == route.destination
                    || !self.hubs.contains_key(&route.origin)
                    || !self.hubs.contains_key(&route.destination)
                    || route.weight == 0
                    || route.weight > 1000
                    || route.equipment.len() > 8
                    || route
                        .equipment
                        .iter()
                        .any(|kind| !self.aircraft.contains_key(kind))
                {
                    return Err(format!("Invalid published route for {id}"));
                }
                let mut ends = [route.origin.as_str(), route.destination.as_str()];
                ends.sort_unstable();
                if !seen.insert(ends) {
                    return Err(format!("Duplicate published route for {id}"));
                }
            }
        }
        Ok(())
    }

    /// Whether the operator has published route entries.
    #[must_use]
    pub fn has_published_network(&self, airline: &str) -> bool {
        self.routes
            .get(airline)
            .is_some_and(|routes| !routes.is_empty())
    }

    fn route_allowed(&self, airline: &str, origin: &str, destination: &str) -> bool {
        let Some(operator) = self.airlines.get(airline) else {
            return false;
        };
        if self
            .published_weight(airline, origin, destination)
            .is_some()
        {
            return true;
        }
        match operator.route_policy {
            RoutePolicy::Listed => !self.has_published_network(airline),
            RoutePolicy::HubSpokes => {
                self.hub_weight(airline, origin) > 0 || self.hub_weight(airline, destination) > 0
            }
        }
    }

    /// Published frequency band for a pair, if the operator flies it.
    #[must_use]
    pub fn published_weight(&self, airline: &str, origin: &str, destination: &str) -> Option<u32> {
        self.routes.get(airline)?.iter().find_map(|route| {
            let forward = route.origin == origin && route.destination == destination;
            let reverse = route.origin == destination && route.destination == origin;
            (forward || reverse).then_some(route.weight)
        })
    }

    /// Equipment published for a pair, when the operator publishes it.
    #[must_use]
    pub fn published_equipment(
        &self,
        airline: &str,
        origin: &str,
        destination: &str,
    ) -> Option<&[String]> {
        self.routes.get(airline)?.iter().find_map(|route| {
            let forward = route.origin == origin && route.destination == destination;
            let reverse = route.origin == destination && route.destination == origin;
            (forward || reverse).then_some(route.equipment.as_slice())
        })
    }

    /// Match both airports, route range and the installed model inventory before drawing lots.
    /// Empty model/type intersections return no flight; another airline's paint is never invented.
    #[must_use]
    pub fn select(
        &self,
        origin: &Port,
        destination: &Port,
        distance_nm: f64,
        category: Category,
        catalogue: &Catalogue,
        seed: u64,
    ) -> Option<Selection> {
        self.select_where(
            origin,
            destination,
            distance_nm,
            category,
            catalogue,
            seed,
            |_, _| true,
        )
    }

    /// Apply airport equipment and stand constraints before weighing the remaining fleets.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn select_where(
        &self,
        origin: &Port,
        destination: &Port,
        distance_nm: f64,
        category: Category,
        catalogue: &Catalogue,
        seed: u64,
        allowed: impl Fn(&str, &Aircraft) -> bool,
    ) -> Option<Selection> {
        if !distance_nm.is_finite()
            || distance_nm < 5.0
            || origin.icao == destination.icao
            || ![origin, destination].iter().all(|p| {
                code(&p.icao, 3, 4)
                    && bounded(p.runway, 300.0, 10000.0)
                    && "ABCDEF".contains(p.parking_size)
            })
        {
            return None;
        }
        let mut candidates = Vec::new();
        for (id, airline) in &self.airlines {
            if airline.category != category
                || !serves(airline, &origin.icao)
                || !serves(airline, &destination.icao)
            {
                continue;
            }
            // Listed networks remain exclusive; explicit hub-spoke profiles may add simulated
            // connections through a configured hub without claiming a published schedule.
            let published = self.published_weight(id, &origin.icao, &destination.icao);
            if !self.route_allowed(id, &origin.icao, &destination.icao) {
                continue;
            }
            let equipment = self.published_equipment(id, &origin.icao, &destination.icao);
            let oh = self.hub_weight(id, &origin.icao);
            let dh = self.hub_weight(id, &destination.icao);
            if category != Category::GeneralAviation
                && ![origin, destination].iter().any(|p| {
                    airline
                        .home_prefixes
                        .iter()
                        .any(|prefix| p.icao.starts_with(prefix))
                })
            {
                continue;
            }
            let mut fleet = Vec::new();
            for (kind, weight) in &airline.fleet {
                let aircraft = &self.aircraft[kind];
                if !allowed(kind, aircraft)
                    || distance_nm > aircraft.range_nm * 0.8
                    || [origin, destination].iter().any(|p| {
                        p.runway < aircraft.runway || p.parking_size < aircraft.parking_size()
                    })
                {
                    continue;
                }
                // Airline liveries take precedence over the generic same-type fallback.
                let paints: BTreeSet<_> = std::iter::once(airline.operator.as_str())
                    .chain(airline.liveries.iter().map(String::as_str))
                    .collect();
                let models: Vec<_> = catalogue
                    .models
                    .iter()
                    .filter_map(|m| {
                        let paint = m.matches.iter().find(|v| {
                            (v.aircraft == *kind || aircraft.model_aliases.contains(&v.aircraft))
                                && paints.contains(v.operator.as_str())
                                && !v.operator.is_empty()
                        })?;
                        Some((m, paint.operator.as_str(), paint.aircraft.as_str()))
                    })
                    .collect();
                let choice = if models.is_empty() {
                    std::iter::once(kind)
                        .chain(aircraft.model_aliases.iter())
                        .find_map(|code| {
                            catalogue
                                .choose(code, &airline.operator, "", mix(seed ^ stable_hash(kind)))
                                .map(|m| (m, "", code.as_str()))
                        })
                } else {
                    Some(models[(mix(seed ^ stable_hash(kind)) % models.len() as u64) as usize])
                };
                if let Some((model, paint, actual_type)) = choice {
                    // Equipment published for the pair is preferred, never exclusive.
                    let mut type_weight = u64::from(*weight);
                    if let Some(equipment) = equipment
                        && !equipment.is_empty()
                        && equipment.iter().any(|published| published == kind)
                    {
                        type_weight = type_weight.saturating_mul(4);
                    }
                    fleet.push((
                        type_weight,
                        Selection {
                            airline: id.clone(),
                            operator: airline.operator.clone(),
                            spoken_operator: airline.callsign.clone(),
                            aircraft: actual_type.into(),
                            model: model.id.clone(),
                            model_operator: paint.into(),
                            category,
                        },
                    ));
                }
            }
            if let Some(selected) = pick(&fleet, mix(seed ^ stable_hash(id))) {
                let weight = (u64::from(oh) + 2) * (u64::from(dh) + 2);
                // A published frequency band orders operators that share a hub preference.
                let published_factor = 1 + u64::from(published.unwrap_or(0)).min(100) / 10;
                candidates.push((weight * published_factor, selected.clone()));
            }
        }
        pick(&candidates, mix(seed)).cloned()
    }

    fn hub_weight(&self, airline: &str, airport: &str) -> u32 {
        self.hubs
            .get(airport)
            .and_then(|h| h.airlines.get(airline))
            .copied()
            .unwrap_or(0)
    }
}

fn serves(airline: &Airline, airport: &str) -> bool {
    airline.serves.is_empty() || airline.serves.iter().any(|p| airport.starts_with(p))
}
fn bounded(v: f64, low: f64, high: f64) -> bool {
    v.is_finite() && (low..=high).contains(&v)
}
fn code(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}
fn pick<T>(values: &[(u64, T)], seed: u64) -> Option<&T> {
    let sum: u64 = values.iter().map(|(w, _)| *w).sum();
    if sum == 0 {
        return None;
    }
    let mut draw = seed % sum;
    for (weight, value) in values {
        if draw < *weight {
            return Some(value);
        }
        draw -= *weight;
    }
    None
}
/// Stable mixer; results do not depend on Rust's hash-map seed or operating system.
#[must_use]
pub fn mix(mut v: u64) -> u64 {
    v = v.wrapping_add(0x9e37_79b9_7f4a_7c15);
    v = (v ^ (v >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    v = (v ^ (v >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    v ^ (v >> 31)
}
/// FNV-1a string seed, stable across application restarts.
#[must_use]
pub fn stable_hash(value: &str) -> u64 {
    value.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_routes_bind_an_operator_to_the_pairs_it_publishes() {
        let profiles = Profiles::builtin().unwrap();
        assert!(profiles.has_published_network("AFR"));
        assert!(profiles.published_weight("AFR", "LFPG", "LFMN").is_some());
        // A published pair is matched from either end.
        assert_eq!(
            profiles.published_weight("AFR", "LFPG", "LFMN"),
            profiles.published_weight("AFR", "LFMN", "LFPG")
        );
        assert!(profiles.published_weight("AFR", "LFPG", "EGLL").is_none());
        assert!(
            profiles
                .published_equipment("CCM", "LFBO", "LFKJ")
                .is_some()
        );
    }

    #[test]
    fn cargo_hub_spokes_do_not_turn_partial_european_routes_into_a_global_exclusion() {
        let mut profiles = Profiles::builtin().unwrap();
        assert!(profiles.has_published_network("FDX"));
        assert!(profiles.published_weight("FDX", "KMEM", "KSDF").is_none());
        assert!(profiles.route_allowed("FDX", "KMEM", "KSDF"));
        assert!(profiles.route_allowed("FDX", "KSDF", "KMEM"));
        assert!(profiles.route_allowed("UPS", "KMEM", "KSDF"));
        assert!(!profiles.route_allowed("FDX", "KAAA", "KBBB"));
        assert!(!profiles.route_allowed("UPS", "KAAA", "KBBB"));
        assert!(!profiles.route_allowed("AFR", "LFPG", "EGLL"));
        profiles.airlines.get_mut("FDX").unwrap().route_policy = RoutePolicy::Listed;
        assert!(!profiles.route_allowed("FDX", "KMEM", "KSDF"));

        profiles.airlines.get_mut("FDX").unwrap().route_policy = RoutePolicy::HubSpokes;
        profiles.airlines.retain(|id, _| id == "FDX");
        let catalogue = models(&profiles);
        let choice = profiles
            .select_where(
                &port("KMEM"),
                &port("KSDF"),
                330.0,
                Category::Cargo,
                &catalogue,
                1,
                |_, _| true,
            )
            .unwrap();
        assert_eq!(choice.operator, "FDX");
        assert_eq!(choice.category, Category::Cargo);
        assert!(
            profiles
                .select_where(
                    &port("KMEM"),
                    &port("KSDF"),
                    330.0,
                    Category::Cargo,
                    &catalogue,
                    1,
                    |_, _| false
                )
                .is_none()
        );
        assert!(
            profiles
                .select(
                    &port("KMEM"),
                    &port("KSDF"),
                    330.0,
                    Category::Cargo,
                    &Catalogue {
                        models: vec![],
                        warnings: vec![]
                    },
                    1
                )
                .is_none()
        );
        assert!(
            profiles
                .select(
                    &port("KMEM"),
                    &port("KSDF"),
                    20_000.0,
                    Category::Cargo,
                    &catalogue,
                    1
                )
                .is_none()
        );
        profiles.airlines.get_mut("FDX").unwrap().serves = vec!["LF".into()];
        assert!(
            profiles
                .select(
                    &port("KMEM"),
                    &port("KSDF"),
                    330.0,
                    Category::Cargo,
                    &catalogue,
                    1
                )
                .is_none()
        );
        profiles
            .hubs
            .get_mut("KMEM")
            .unwrap()
            .airlines
            .remove("FDX");
        assert!(!profiles.route_allowed("FDX", "KMEM", "KSDF"));
    }

    #[test]
    fn a_route_must_name_a_known_operator_airport_and_equipment() {
        let aircraft = include_str!("../../../../traffic/aircraft.toml");
        let airlines = include_str!("../../../../traffic/airlines.toml");
        let hubs = include_str!("../../../../traffic/hubs.toml");
        let unknown_airline = r#"version = 1

[[route]]
airline = "ZZZ"
origin = "LFPG"
destination = "LFMN"
weight = 10
"#;
        assert!(Profiles::parse_all(aircraft, airlines, hubs, Some(unknown_airline)).is_err());
        let unknown_airport = r#"version = 1

[[route]]
airline = "AFR"
origin = "LFPG"
destination = "ZZZZ"
weight = 10
"#;
        assert!(Profiles::parse_all(aircraft, airlines, hubs, Some(unknown_airport)).is_err());
        let unknown_equipment = r#"version = 1

[[route]]
airline = "AFR"
origin = "LFPG"
destination = "LFMN"
weight = 10
equipment = ["ZZZZ"]
"#;
        assert!(Profiles::parse_all(aircraft, airlines, hubs, Some(unknown_equipment)).is_err());
        let duplicate = r#"version = 1

[[route]]
airline = "AFR"
origin = "LFPG"
destination = "LFMN"
weight = 10

[[route]]
airline = "AFR"
origin = "LFMN"
destination = "LFPG"
weight = 20
"#;
        assert!(Profiles::parse_all(aircraft, airlines, hubs, Some(duplicate)).is_err());
    }

    #[test]
    fn the_bundled_route_graph_is_loaded_with_the_profiles() {
        let profiles = Profiles::builtin().unwrap();
        assert!(!profiles.routes.is_empty());
        for (airline, routes) in &profiles.routes {
            assert!(
                profiles.airlines.contains_key(airline),
                "published routes name an unknown operator: {airline}"
            );
            for route in routes {
                assert!(profiles.hubs.contains_key(&route.origin));
                assert!(profiles.hubs.contains_key(&route.destination));
            }
        }
    }
    #[test]
    fn actual_stand_constraints_filter_the_fleet_before_selection() {
        let profiles = Profiles::builtin().unwrap();
        let catalogue = models(&profiles);
        for seed in 0..32 {
            let selection = profiles
                .select_where(
                    &port("KLAX"),
                    &port("KSFO"),
                    300.0,
                    Category::GeneralAviation,
                    &catalogue,
                    seed,
                    |_, aircraft| !aircraft.propeller,
                )
                .unwrap();
            assert!(selection.operator.is_empty());
            assert!(
                !profiles
                    .aircraft_for(&selection.aircraft)
                    .unwrap()
                    .propeller
            );
        }
        assert!(
            profiles
                .select_where(
                    &port("KLAX"),
                    &port("KSFO"),
                    300.0,
                    Category::GeneralAviation,
                    &catalogue,
                    1,
                    |_, _| false
                )
                .is_none()
        );
    }

    #[test]
    fn managed_spacing_requires_valid_profile_speeds_and_standard_increments() {
        let mut profiles = Profiles::builtin().unwrap();
        assert!(
            profiles
                .aircraft
                .values()
                .all(|aircraft| aircraft.spacing_speed.is_some())
        );
        profiles.spacing.speed_step_knots = 7.0;
        assert!(profiles.validate().is_err());
        profiles.spacing.speed_step_knots = 5.0;
        profiles
            .aircraft
            .get_mut("C172")
            .unwrap()
            .spacing_speed
            .as_mut()
            .unwrap()
            .minimum_knots = 40.0;
        assert!(profiles.validate().is_err());
        profiles.aircraft.get_mut("C172").unwrap().spacing_speed = None;
        assert!(profiles.validate().is_ok());
    }
    use crate::traffic::csl::{Match, Model};
    #[test]
    fn private_registrations_and_piper_models_use_editable_profiles() {
        let profiles = Profiles::builtin().unwrap();
        let ga = &profiles.airlines["GA"];
        let french = ga.registration("LFMD", 123).unwrap();
        assert!(french.starts_with("F-"));
        assert_eq!(french.len(), 6);
        assert_eq!(french, ga.registration("LFMD", 123).unwrap());
        assert!(ga.registration("YSSY", 123).unwrap().starts_with("VH-"));
        let catalogue = Catalogue {
            models: vec![Model {
                id: "PIPER".into(),
                objects: vec![],
                vertical_offset: None,
                matches: vec![Match {
                    aircraft: "P28A".into(),
                    operator: String::new(),
                    livery: String::new(),
                }],
            }],
            warnings: vec![],
        };
        let port = Port {
            icao: "LFMD".into(),
            runway: 1500.0,
            parking_size: 'B',
        };
        let destination = Port {
            icao: "LFMN".into(),
            ..port.clone()
        };
        let choice = profiles
            .select(
                &port,
                &destination,
                20.0,
                Category::GeneralAviation,
                &catalogue,
                123,
            )
            .unwrap();
        assert_eq!(choice.aircraft, "P28A");
        assert!(profiles.aircraft_for(&choice.aircraft).unwrap().fixed_gear);
    }
    fn models(profiles: &Profiles) -> Catalogue {
        Catalogue {
            models: profiles
                .airlines
                .values()
                .flat_map(|a| {
                    a.fleet.keys().map(move |t| Model {
                        id: format!("{}/{t}", a.operator),
                        objects: vec![],
                        vertical_offset: None,
                        matches: vec![Match {
                            aircraft: t.clone(),
                            operator: a.operator.clone(),
                            livery: String::new(),
                        }],
                    })
                })
                .collect(),
            warnings: vec![],
        }
    }
    fn port(id: &str) -> Port {
        Port {
            icao: id.into(),
            runway: 4000.0,
            parking_size: 'F',
        }
    }
    #[test]
    fn profiles_resolve_all_references_and_preserve_type_categories() {
        let p = Profiles::builtin().unwrap();
        assert!(p.airlines.len() >= 20 && p.hubs.len() >= 30 && p.aircraft.len() >= 30);
        assert_eq!(p.aircraft["A320"].parking_size(), 'C');
        assert_eq!(p.aircraft["A388"].wake, 'J');
        assert!(p.aircraft["AT76"].propeller);
        assert_eq!(p.airlines["SIA"].fleet.get("B78X"), Some(&15));
        assert!(!p.airlines["SIA"].fleet.contains_key("B789"));
    }
    #[test]
    fn selection_uses_both_ends_hubs_suitability_and_available_paint() {
        let p = Profiles::builtin().unwrap();
        let c = models(&p);
        let mut local = 0;
        for seed in 0..500 {
            let s = p
                .select(
                    &port("YSSY"),
                    &port("YMML"),
                    400.0,
                    Category::Airline,
                    &c,
                    seed,
                )
                .unwrap();
            assert!(matches!(s.operator.as_str(), "QFA" | "JST" | "VOZ"));
            local += usize::from(s.operator == "QFA");
            assert_eq!(
                s,
                p.select(
                    &port("YSSY"),
                    &port("YMML"),
                    400.0,
                    Category::Airline,
                    &c,
                    seed
                )
                .unwrap()
            );
        }
        assert!(local > 200);
        let small = Port {
            icao: "KSMO".into(),
            runway: 1000.0,
            parking_size: 'A',
        };
        assert!(
            p.select(&small, &port("KLAX"), 20.0, Category::Airline, &c, 1)
                .is_none()
        );
        assert!(
            p.select(
                &small,
                &port("KLGB"),
                40.0,
                Category::GeneralAviation,
                &c,
                1
            )
            .is_some()
        );
        assert!(
            p.select(
                &port("KLAX"),
                &port("KSNA"),
                40.0,
                Category::Airline,
                &Catalogue::default(),
                1
            )
            .is_none()
        );
        assert!(
            p.select(
                &port("KLAX"),
                &port("KSNA"),
                f64::NAN,
                Category::Airline,
                &c,
                1
            )
            .is_none()
        );
        let distant = p
            .select(
                &port("WSSS"),
                &port("YSSY"),
                3400.0,
                Category::Airline,
                &c,
                5,
            )
            .unwrap();
        assert!(p.aircraft[&distant.aircraft].range_nm * 0.8 >= 3400.0);
    }
    #[test]
    fn malformed_or_unresolved_edits_fail_instead_of_silently_changing_traffic() {
        let a = include_str!("../../../../traffic/aircraft.toml");
        let l = include_str!("../../../../traffic/airlines.toml");
        let h = include_str!("../../../../traffic/hubs.toml");
        assert!(Profiles::parse(a, &l.replace("B78X = 15", "ZZZZ = 15"), h).is_err());
        assert!(Profiles::parse(&a.replacen("wingspan =", "wingpsan =", 1), l, h).is_err());
        assert!(Profiles::parse(a, l, &h.replacen("version = 1", "version = 2", 1)).is_err());
    }
}
