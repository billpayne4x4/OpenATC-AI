//! Published airport stations and a shared simulated reception model.
use crate::{
    airport::{Airport, load_airport},
    ops::distance_nm,
    state::Telemetry,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

/// Airport station used by both the radio UI and engine reception.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Station {
    /// Published regional reception volumes (MSL feet and latitude/longitude).
    pub coverage: Vec<Coverage>,
    /// Airport ICAO.
    pub airport: String,
    /// Airport name.
    pub airport_name: String,
    /// Published station name.
    pub name: String,
    /// Clearance, Ground, Tower, Approach, Departure, ATIS or Unicom.
    pub service: String,
    /// Explicit combined service capabilities (empty means primary service only).
    pub services: Vec<String>,
    /// Frequency/channel kHz.
    pub khz: i32,
    /// Airport location.
    pub latitude: f64,
    /// Airport location.
    pub longitude: f64,
    /// Airport surface elevation MSL.
    pub elevation_feet: f64,
    /// Distance from aircraft.
    pub distance_nm: f64,
    /// Within the simulated reception envelope.
    pub receivable: bool,
}
/// Normalise a simulator root for filesystem joins.
///
/// Windows folder fields often arrive with a trailing separator, and some pickers
/// return an extended-length prefix. Both break plain string concatenation.
#[must_use]
pub fn simulator_root(root: &str) -> std::path::PathBuf {
    let mut text = root.trim().trim_end_matches(['/', '\\']).to_owned();
    #[cfg(windows)]
    {
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            text = format!(r"\\{rest}");
        } else if let Some(rest) = text.strip_prefix(r"\\?\") {
            text = rest.to_owned();
        }
    }
    if text.ends_with(':') {
        text.push('/');
    }
    std::path::PathBuf::from(text)
}

/// Preserve enabled scenery priority, then use global/default airport data.
#[must_use]
pub fn scenery_paths(root: &str) -> Vec<String> {
    let root = simulator_root(root);
    let apt = |base: std::path::PathBuf| {
        base.join("Earth nav data")
            .join("apt.dat")
            .to_string_lossy()
            .into_owned()
    };
    let mut paths = Vec::new();
    if let Ok(text) = std::fs::read_to_string(root.join("Custom Scenery").join("scenery_packs.ini"))
    {
        let text = text.trim_start_matches('\u{feff}');
        for line in text.lines() {
            if let Some(relative) = line.strip_prefix("SCENERY_PACK ") {
                let relative = relative.trim();
                let base = if relative == "*GLOBAL_AIRPORTS*" {
                    root.join("Global Scenery").join("Global Airports")
                } else if Path::new(relative).is_relative() {
                    root.join(relative)
                } else {
                    std::path::PathBuf::from(relative)
                };
                paths.push(apt(base));
            }
        }
    }
    paths.push(apt(root.join("Global Scenery").join("Global Airports")));
    paths.push(apt(root
        .join("Resources")
        .join("default scenery")
        .join("default apt dat")));
    let mut seen = BTreeSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    paths
}
fn append_airport(text: &str, id: &str, seen: &mut BTreeSet<String>, stations: &mut Vec<Station>) {
    if id.is_empty() || !seen.insert(id.to_owned()) {
        return;
    }
    if let Ok(airport) = load_airport(text, id) {
        stations.extend(from_airport(&airport));
    }
}
/// Build once per simulator root; retain only lightweight airport records during parsing.
pub fn load_index(root: &str) -> Result<Vec<Station>, String> {
    let mut stations = Vec::new();
    let mut seen = BTreeSet::new();
    let mut read_any = false;
    for path in scenery_paths(root) {
        if !Path::new(&path).is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
        read_any = true;
        let mut id = String::new();
        let mut block = String::new();
        for line in text.lines() {
            let Some(record) = line
                .split_ascii_whitespace()
                .next()
                .and_then(|v| v.parse::<i32>().ok())
            else {
                continue;
            };
            if [1, 16, 17, 99].contains(&record) {
                let fields: Vec<_> = line.split_ascii_whitespace().collect();
                append_airport(&block, &id, &mut seen, &mut stations);
                block.clear();
                id.clear();
                if record != 99 && fields.len() > 4 {
                    fields[4].clone_into(&mut id);
                    block.push_str(line);
                    block.push('\n');
                }
            } else if record == 100
                || (50..=56).contains(&record)
                || (1050..=1056).contains(&record)
            {
                block.push_str(line);
                block.push('\n');
            }
        }
        append_airport(&block, &id, &mut seen, &mut stations);
    }
    if !read_any {
        return Err("No enabled airport scenery found".to_owned());
    }
    super::atc::extend_index(root, &mut stations)?;
    assign_airport_capabilities(&mut stations);
    let overrides = std::env::var("OPENATC_RADIO_STATIONS_FILE").unwrap_or_else(|_| {
        simulator_root(root)
            .join("Resources")
            .join("plugins")
            .join("OpenATC")
            .join("radio-stations.toml")
            .to_string_lossy()
            .into_owned()
    });
    if Path::new(&overrides).is_file() {
        apply_combined(
            &mut stations,
            &std::fs::read_to_string(overrides).map_err(|e| e.to_string())?,
        )?;
    }
    merge_shared_frequencies(&mut stations);
    Ok(stations)
}

/// Combine station entries that share an airport and frequency.
///
/// Published data can list the same transmitter twice, for example a Tower and a
/// PRM monitor. Keeping both makes the tuned station depend on list order and can
/// leave the controller role wrong.
fn merge_shared_frequencies(stations: &mut Vec<Station>) {
    let priority = |service: &str| match service {
        "Tower" => 0,
        "Approach" => 1,
        "Departure" => 2,
        "Ground" => 3,
        "Clearance" | "Delivery" => 4,
        "ATIS" => 5,
        "Unicom" => 6,
        _ => 7,
    };
    let mut merged: Vec<Station> = Vec::new();
    for station in stations.drain(..) {
        let Some(index) = merged.iter().position(|existing| {
            existing.airport == station.airport && existing.khz == station.khz
        }) else {
            merged.push(station);
            continue;
        };
        // ATIS and Unicom are distinct services even when they share a channel.
        if ["ATIS", "Unicom"].contains(&station.service.as_str())
            || ["ATIS", "Unicom"].contains(&merged[index].service.as_str())
        {
            merged.push(station);
            continue;
        }
        let existing = &mut merged[index];
        for service in
            std::iter::once(station.service.clone()).chain(station.services.iter().cloned())
        {
            if !existing.services.contains(&service) {
                existing.services.push(service);
            }
        }
        if !existing.services.contains(&existing.service) {
            existing.services.push(existing.service.clone());
        }
        if priority(&station.service) < priority(&existing.service) {
            existing.service = station.service;
            existing.name = station.name;
            existing.airport_name = station.airport_name;
        }
    }
    *stations = merged;
}
/// Convert a loaded airport (also used for deterministic fixtures).
#[must_use]
pub fn from_airport(a: &Airport) -> Vec<Station> {
    a.frequencies
        .iter()
        .map(|f| Station {
            airport: a.icao.clone(),
            airport_name: a.name.clone(),
            name: if f.name.trim().is_empty() {
                format!("{} {}", a.name, f.service)
            } else {
                f.name.clone()
            },
            service: f.service.clone(),
            khz: f.khz,
            latitude: a.reference_latitude,
            longitude: a.reference_longitude,
            elevation_feet: a.elevation_feet,
            ..Station::default()
        })
        .collect()
}
/// Test radio reception without cloning the station's coverage geometry.
#[must_use]
pub fn reception_distance(station: &Station, t: &Telemetry) -> Option<f64> {
    if !t.position_valid {
        return None;
    }
    let distance = distance_nm(t.latitude, t.longitude, station.latitude, station.longitude);
    let cap = match station.service.as_str() {
        "Tower" => 50.,
        "ATIS" => 100.,
        "Approach" | "Departure" | "Center" => 150.,
        _ => 25.,
    };
    let height = (t.altitude_feet - station.elevation_feet).max(0.);
    let horizon = 1.23 * (height.sqrt() + 50_f64.sqrt());
    // Airport surface stations remain usable around their movement area.
    let range = horizon.max(10.).min(cap);
    let regional =
        matches!(station.service.as_str(), "Approach" | "Center") && !station.coverage.is_empty();
    let receivable = if regional {
        distance <= range
            || station
                .coverage
                .iter()
                .any(|volume| volume.radio_distance_nm(t) <= cap)
    } else {
        distance <= range
    };
    receivable.then_some(distance)
}

/// Airport radio horizon and regional coverage networks. No terrain shielding claim.
#[must_use]
pub fn nearby(index: &[Station], t: &Telemetry) -> Vec<Station> {
    if !t.position_valid {
        return Vec::new();
    }
    let mut result = Vec::new();
    for station in index {
        if let Some(distance) = reception_distance(station, t) {
            let mut s = station.clone();
            s.distance_nm = distance;
            s.receivable = true;
            result.push(s);
        }
    }
    let priority = |service: &str| {
        if t.on_ground {
            match service {
                "Clearance" => 0,
                "Ground" => 1,
                "Tower" => 2,
                "ATIS" => 3,
                "Departure" => 4,
                "Approach" => 5,
                _ => 6,
            }
        } else {
            match service {
                "Approach" => 0,
                "Departure" => 1,
                "Tower" => 2,
                "ATIS" => 3,
                "Ground" => 4,
                "Clearance" => 5,
                _ => 6,
            }
        }
    };
    result.sort_by(|a, b| {
        a.distance_nm
            .total_cmp(&b.distance_nm)
            .then(a.airport.cmp(&b.airport))
            .then(priority(&a.service).cmp(&priority(&b.service)))
            .then(a.service.cmp(&b.service))
    });
    result
}
/// Resolve frequency reuse: nearest receivable airport wins, then deterministic service order.
#[must_use]
pub fn tuned(stations: &[Station], khz: i32) -> Option<&Station> {
    stations.iter().find(|s| s.receivable && s.khz == khz)
}

// Five-digit COM records omit the last 5: 11802 means 118.025 MHz.
// Modern CHAN records already contain all six digits and must stay unchanged.
pub(crate) fn legacy_com_khz(frequency: i32) -> i32 {
    frequency.saturating_mul(10) + if frequency.rem_euclid(5) == 2 { 5 } else { 0 }
}
/// Primary role capabilities; combined airport duties are assigned during indexing.
#[must_use]
pub fn serves(service: &str, intent: &str) -> bool {
    match intent {
        "traffic_sighted" | "traffic_not_sighted" => {
            ["Tower", "Departure", "Approach", "Center"].contains(&service)
        }
        "clearance" => service == "Clearance" || service == "Delivery",
        "departure_runway" => ["Clearance", "Ground", "Tower"].contains(&service),
        "arrival_runway" | "change_approach" => {
            ["Approach", "Departure", "Center", "Tower"].contains(&service)
        }
        "start" | "pushback" | "start_pushback" | "taxi" | "gate" | "progressive" => {
            service == "Ground"
        }
        "cross_runway" | "vacated" => service == "Ground" || service == "Tower",
        "ready" | "backtrack" | "landing" => service == "Tower",
        "resume_course" | "altitude" | "direct" | "descent" | "cancel_ifr" | "route"
        | "deviation" | "hold" | "speed" | "divert" => {
            ["Approach", "Departure", "Center"].contains(&service)
        }
        "approach" | "runway" => ["Approach", "Center"].contains(&service),
        "go_around" | "visual" | "localizer" => ["Tower", "Approach"].contains(&service),
        _ => [
            "Clearance",
            "Ground",
            "Tower",
            "Approach",
            "Departure",
            "Center",
        ]
        .contains(&service),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceFile {
    #[serde(default)]
    combined: Vec<Combined>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Combined {
    airport: String,
    khz: i32,
    services: Vec<String>,
}
/// Add capabilities only to a real published station; no synthetic frequencies.
pub fn apply_combined(stations: &mut [Station], text: &str) -> Result<(), String> {
    let file: ServiceFile = crate::configuration::from_str(text).map_err(|e| e.to_string())?;
    for entry in file.combined {
        if entry.services.is_empty()
            || entry.services.iter().any(|s| {
                !["Clearance", "Ground", "Tower", "Approach", "Departure"].contains(&s.as_str())
            })
        {
            return Err("Combined services must use published controller roles".to_owned());
        }
        let matches: Vec<_> = stations
            .iter_mut()
            .filter(|s| s.airport == entry.airport && s.khz == entry.khz)
            .collect();
        if matches.is_empty() {
            return Err(format!(
                "No published station {} {}",
                entry.airport, entry.khz
            ));
        }
        for station in matches {
            if station.service == "ATIS" || station.service == "Unicom" {
                return Err("ATIS/Unicom cannot provide controller services".to_owned());
            }
            station.services.clone_from(&entry.services);
            if !station.services.contains(&station.service) {
                station.services.push(station.service.clone());
            }
        }
    }
    Ok(())
}
/// Capability check includes explicitly configured combined service roles.
#[must_use]
pub fn station_serves(station: &Station, intent: &str) -> bool {
    serves(&station.service, intent) || station.services.iter().any(|s| serves(s, intent))
}

/// Whether a station holds one literal controller role, including configured combined duties.
#[must_use]
pub fn has_role(station: &Station, role: &str) -> bool {
    station.service == role || station.services.iter().any(|service| service == role)
}

/// Select the published station that provides `intent` at `airport`.
///
/// The aircraft's tuned frequency wins when it has the capability. Otherwise the nearest
/// published station is used. Pilot routing and generated traffic share this resolver so a
/// combined service, a renamed Delivery facility or a second Ground frequency cannot select
/// different stations on the two paths.
#[must_use]
pub fn station_for_intent<'a>(
    stations: &'a [Station],
    airport: &str,
    intent: &str,
    tuned_khz: Option<i32>,
    position: [f64; 2],
) -> Option<&'a Station> {
    let mut candidates = stations
        .iter()
        .filter(|station| station.airport == airport && station_serves(station, intent))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    if let Some(khz) = tuned_khz
        && let Some(station) = candidates.iter().find(|station| station.khz == khz)
    {
        return Some(station);
    }
    candidates.sort_by(|left, right| {
        distance_nm(position[0], position[1], left.latitude, left.longitude)
            .total_cmp(&distance_nm(
                position[0],
                position[1],
                right.latitude,
                right.longitude,
            ))
            .then_with(|| left.khz.cmp(&right.khz))
            .then_with(|| left.name.cmp(&right.name))
    });
    candidates.first().copied()
}

/// Published controller volume. Longitude is unwrapped around the aircraft.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Coverage {
    /// Lower MSL altitude in feet.
    pub floor: f64,
    /// Upper MSL altitude in feet.
    pub ceiling: f64,
    /// Polygon vertices, latitude then longitude.
    pub points: Vec<[f64; 2]>,
}
impl Coverage {
    /// Radio discovery uses horizontal coverage, not the controller's assigned
    /// altitude band. Airport aircraft can tune approach before entering it.
    #[must_use]
    pub fn radio_distance_nm(&self, t: &Telemetry) -> f64 {
        if self.points.len() < 3 {
            return f64::INFINITY;
        }
        let horizontal = Telemetry {
            altitude_feet: self.floor.midpoint(self.ceiling),
            ..t.clone()
        };
        if self.contains(&horizontal) {
            return 0.;
        }
        let origin = self.points[0][1];
        let longitude = |v: f64| (v - origin + 180.).rem_euclid(360.) - 180.;
        let aircraft_x = longitude(t.longitude);
        let cosine = t.latitude.to_radians().cos();
        let project = |p: [f64; 2]| {
            [
                (longitude(p[1]) - aircraft_x) * cosine * 60.,
                (p[0] - t.latitude) * 60.,
            ]
        };
        let mut previous = project(self.points[self.points.len() - 1]);
        let mut distance = f64::INFINITY;
        for point in &self.points {
            let current = project(*point);
            let dx = current[0] - previous[0];
            let dy = current[1] - previous[1];
            let length = dx * dx + dy * dy;
            let along = if length > 0. {
                (-(previous[0] * dx + previous[1] * dy) / length).clamp(0., 1.)
            } else {
                0.
            };
            distance = distance.min((previous[0] + along * dx).hypot(previous[1] + along * dy));
            previous = current;
        }
        distance
    }
    /// Whether telemetry lies inside the published horizontal and altitude bounds.
    #[must_use]
    pub fn contains(&self, t: &Telemetry) -> bool {
        if self.points.len() < 3
            || !self.floor.is_finite()
            || !self.ceiling.is_finite()
            || self.floor > self.ceiling
            || ![t.latitude, t.longitude, t.altitude_feet]
                .iter()
                .all(|value| value.is_finite())
            || t.altitude_feet < self.floor
            || t.altitude_feet > self.ceiling
        {
            return false;
        }
        let origin = self.points[0][1];
        let longitude = |value: f64| (value - origin + 180.).rem_euclid(360.) - 180.;
        let aircraft_x = longitude(t.longitude);
        let mut inside = false;
        let mut previous = self.points[self.points.len() - 1];
        for current in &self.points {
            let x1 = longitude(previous[1]);
            let x2 = longitude(current[1]);
            // Adjacent sectors share their boundary. Keep roundoff at an exact edge
            // from creating an unassigned sliver between otherwise identical polygons.
            let dx = x2 - x1;
            let dy = current[0] - previous[0];
            let length2 = dx * dx + dy * dy;
            let along = if length2 > 0.0 {
                (((aircraft_x - x1) * dx + (t.latitude - previous[0]) * dy) / length2)
                    .clamp(0.0, 1.0)
            } else {
                0.0
            };
            if (aircraft_x - x1 - along * dx).hypot(t.latitude - previous[0] - along * dy) <= 1e-10
            {
                return true;
            }
            if (previous[0] > t.latitude) != (current[0] > t.latitude)
                && aircraft_x
                    < (x2 - x1) * (t.latitude - previous[0]) / (current[0] - previous[0]) + x1
            {
                inside = !inside;
            }
            previous = *current;
        }
        inside
    }
}

/// Match the simulator's consolidated airport duties: Delivery, otherwise Ground,
/// otherwise Tower delivers clearance. Tower handles ground duties if no Ground
/// is published. Separate published services keep their own responsibilities.
pub fn assign_airport_capabilities(stations: &mut [Station]) {
    let mut roles = std::collections::BTreeMap::<String, (bool, bool)>::new();
    for station in stations.iter() {
        let entry = roles.entry(station.airport.clone()).or_default();
        entry.0 |= station.service == "Clearance" || station.service == "Delivery";
        entry.1 |= station.service == "Ground";
    }
    for station in stations {
        let (has_clearance, has_ground) = roles[&station.airport];
        let clearance = if has_clearance {
            "Clearance"
        } else if has_ground {
            "Ground"
        } else {
            "Tower"
        };
        let ground = if has_ground { "Ground" } else { "Tower" };
        for (primary, capability) in [(clearance, "Clearance"), (ground, "Ground")] {
            if station.service == primary && !station.services.iter().any(|s| s == capability) {
                station.services.push(capability.to_owned());
            }
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    #[test]
    fn adjacent_controller_edges_are_closed_but_real_gaps_remain_outside() {
        let west = Coverage {
            floor: 0.0,
            ceiling: 60000.0,
            points: vec![[-1.0, -1.0], [-1.0, 0.0], [1.0, 0.0], [1.0, -1.0]],
        };
        let east = Coverage {
            points: vec![[-1.0, 0.0], [-1.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
            ..west.clone()
        };
        let point = Telemetry {
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: 30000.0,
            ..Default::default()
        };
        assert!(west.contains(&point) && east.contains(&point));
        assert!(!west.contains(&Telemetry {
            longitude: 0.000_001,
            ..point.clone()
        }));
        assert!(!east.contains(&Telemetry {
            longitude: -0.000_001,
            ..point.clone()
        }));
        assert!(!east.contains(&Telemetry {
            altitude_feet: f64::NAN,
            ..point
        }));
    }
    #[test]
    fn regional_volume_wraps_dateline_without_covering_opposite_hemisphere() {
        let volume = Coverage {
            floor: 100.,
            ceiling: 10000.,
            points: vec![[10., 179.], [12., 179.], [12., -179.], [10., -179.]],
        };
        let t = Telemetry {
            latitude: 11.,
            longitude: -179.5,
            altitude_feet: 500.,
            ..Telemetry::default()
        };
        assert!(volume.contains(&t));
        let ground = Telemetry {
            altitude_feet: 0.,
            ..t.clone()
        };
        assert!(volume.radio_distance_nm(&ground).abs() < f64::EPSILON);
        assert!(
            volume.radio_distance_nm(&Telemetry {
                longitude: 178.5,
                ..ground.clone()
            }) < 40.
        );
        assert!(
            volume.radio_distance_nm(&Telemetry {
                longitude: 0.,
                ..ground
            }) > 1000.
        );
        assert!(!volume.contains(&Telemetry {
            longitude: 0.,
            ..t.clone()
        }));
        assert!(!volume.contains(&Telemetry {
            altitude_feet: 20000.,
            ..t
        }));
    }
}

/// Airport markers are separate from frequencies so uncontrolled fields appear too.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AirportMarker {
    /// Simulator airport identifier.
    pub id: String,
    /// Airport name from the scenery header.
    pub name: String,
    /// Latitude of the first published runway or pad.
    pub latitude: f64,
    /// Longitude of the first published runway or pad.
    pub longitude: f64,
    /// Heliports can be hidden to keep regional views readable.
    #[serde(default)]
    pub heliport: bool,
    /// Scenery boundaries and paved contours used only for airport ownership.
    #[serde(skip)]
    pub footprints: Vec<Vec<[f64; 2]>>,
    /// Latitude/longitude bounds to avoid scanning distant polygons.
    #[serde(skip)]
    pub bounds: Option<[f64; 4]>,
}

/// Index enabled airport scenery in priority order, including fields without radios.
/// # Panics
///
/// Panics if an airport record is shorter than the documented field layout.
pub fn map_airports(root: &str) -> Result<Vec<AirportMarker>, String> {
    use std::io::BufRead;
    let mut markers = Vec::new();
    let mut seen = BTreeSet::new();
    for path in scenery_paths(root) {
        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };
        let mut airport: Option<AirportMarker> = None;
        let mut positioned = false;
        let mut contour = Vec::new();
        let mut surface = false;
        for line in std::io::BufReader::new(file).lines() {
            let line = line.map_err(|e| e.to_string())?;
            let f: Vec<_> = line.split_whitespace().collect();
            let Some(record) = f.first().and_then(|v| v.parse::<i32>().ok()) else {
                continue;
            };
            if [1, 16, 17, 99].contains(&record) {
                if positioned && let Some(marker) = airport.take() {
                    markers.push(marker);
                }
                positioned = false;
                surface = false;
                contour.clear();
                airport = if f.len() > 5 && record != 99 && seen.insert(f[4].to_owned()) {
                    Some(AirportMarker {
                        id: f[4].into(),
                        name: f[5..].join(" "),
                        heliport: record == 17,
                        ..Default::default()
                    })
                } else {
                    None
                };
            } else if airport.is_some() {
                if matches!(record, 110 | 130) {
                    surface = true;
                    contour.clear();
                } else if surface && (111..=114).contains(&record) {
                    if let Some(point) = f
                        .get(1)
                        .zip(f.get(2))
                        .and_then(|(a, b)| a.parse::<f64>().ok().zip(b.parse::<f64>().ok()))
                        && point.0.is_finite()
                        && point.1.is_finite()
                    {
                        contour.push([point.0, point.1]);
                    }
                    if matches!(record, 113 | 114) {
                        if contour.len() >= 3 {
                            airport
                                .as_mut()
                                .unwrap()
                                .footprints
                                .push(std::mem::take(&mut contour));
                        }
                        contour.clear();
                    }
                } else {
                    surface = false;
                }
                if record == 100
                    && let Some(polygon) = runway_footprint(&f)
                {
                    airport.as_mut().unwrap().footprints.push(polygon);
                }
                if positioned {
                    continue;
                }

                let coordinate = match record {
                    100 => f.get(9).zip(f.get(10)),
                    101 => f.get(4).zip(f.get(5)),
                    102 => f.get(2).zip(f.get(3)),
                    _ => None,
                }
                .and_then(|(a, b)| a.parse::<f64>().ok().zip(b.parse::<f64>().ok()));
                if let Some((latitude, longitude)) = coordinate.filter(|(a, b)| {
                    a.is_finite() && b.is_finite() && a.abs() <= 90. && b.abs() <= 180.
                }) {
                    let marker = airport.as_mut().unwrap();
                    marker.latitude = latitude;
                    marker.longitude = longitude;
                    positioned = true;
                }
            }
        }
        if positioned && let Some(marker) = airport {
            markers.push(marker);
        }
    }
    for marker in &mut markers {
        marker.bounds = marker.footprints.iter().flatten().fold(None, |bounds, p| {
            Some(bounds.map_or([p[0], p[1], p[0], p[1]], |b: [f64; 4]| {
                [
                    b[0].min(p[0]),
                    b[1].min(p[1]),
                    b[2].max(p[0]),
                    b[3].max(p[1]),
                ]
            }))
        });
    }
    Ok(markers)
}

fn runway_footprint(fields: &[&str]) -> Option<Vec<[f64; 2]>> {
    let number = |i: usize| fields.get(i)?.parse::<f64>().ok();
    let width = number(1)?;
    let a = [number(9)?, number(10)?];
    let b = [number(18)?, number(19)?];
    if ![width, a[0], a[1], b[0], b[1]]
        .iter()
        .all(|v| v.is_finite())
        || width <= 0.0
    {
        return None;
    }
    let scale = f64::midpoint(a[0], b[0]).to_radians().cos().abs().max(0.01);
    let east = (b[1] - a[1]) * scale;
    let north = b[0] - a[0];
    let length = east.hypot(north);
    if length < 1e-8 {
        return None;
    }
    let offset = width * 0.5 / 111_320.0;
    let dlat = -east / length * offset;
    let dlon = north / length * offset / scale;
    Some(vec![
        [a[0] + dlat, a[1] + dlon],
        [b[0] + dlat, b[1] + dlon],
        [b[0] - dlat, b[1] - dlon],
        [a[0] - dlat, a[1] - dlon],
    ])
}

/// Locate the airport containing the aircraft, rather than the nearest reference point.
#[must_use]
pub fn occupied_airport<'a>(
    airports: &'a [AirportMarker],
    telemetry: &Telemetry,
) -> Option<&'a AirportMarker> {
    if !telemetry.on_ground || !telemetry.position_valid {
        return None;
    }
    airports
        .iter()
        .filter(|a| {
            a.bounds.is_none_or(|b| {
                telemetry.latitude >= b[0]
                    && telemetry.longitude >= b[1]
                    && telemetry.latitude <= b[2]
                    && telemetry.longitude <= b[3]
            })
        })
        .filter(|airport| {
            airport.footprints.iter().any(|polygon| {
                point_in_footprint([telemetry.latitude, telemetry.longitude], polygon)
            })
        })
        .min_by(|a, b| {
            a.heliport.cmp(&b.heliport).then_with(|| {
                distance_nm(
                    telemetry.latitude,
                    telemetry.longitude,
                    a.latitude,
                    a.longitude,
                )
                .total_cmp(&distance_nm(
                    telemetry.latitude,
                    telemetry.longitude,
                    b.latitude,
                    b.longitude,
                ))
            })
        })
}
fn point_in_footprint(point: [f64; 2], polygon: &[[f64; 2]]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];
    for &current in polygon {
        if (current[0] > point[0]) != (previous[0] > point[0])
            && point[1]
                < (previous[1] - current[1]) * (point[0] - current[0]) / (previous[0] - current[0])
                    + current[1]
        {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    #[test]
    fn property_beats_nearby_reference_and_never_claims_airborne_ownership() {
        let field = AirportMarker {
            id: "FIELD".into(),
            latitude: 1.0,
            longitude: 1.0,
            footprints: vec![vec![[1., 1.], [1., 1.04], [1.04, 1.04], [1.04, 1.]]],
            ..Default::default()
        };
        let nearby = AirportMarker {
            id: "PAD".into(),
            latitude: 1.035,
            longitude: 1.035,
            heliport: true,
            ..Default::default()
        };
        let fields = vec![nearby, field];
        let mut t = Telemetry {
            on_ground: true,
            position_valid: true,
            latitude: 1.03,
            longitude: 1.03,
            ..Default::default()
        };
        assert_eq!(occupied_airport(&fields, &t).unwrap().id, "FIELD");
        t.on_ground = false;
        assert!(occupied_airport(&fields, &t).is_none());
    }
}

#[cfg(test)]
mod station_selection_tests {
    use super::*;
    fn station(service: &str, khz: i32, latitude: f64) -> Station {
        Station {
            airport: "TEST".into(),
            name: format!("{service} {khz}"),
            service: service.into(),
            khz,
            latitude,
            longitude: 0.0,
            ..Default::default()
        }
    }
    #[test]
    fn a_combined_ground_duty_is_found_on_the_tower_station() {
        let mut tower = station("Tower", 118_700, 0.0);
        tower.services.push("Ground".into());
        let stations = [tower];
        let found = station_for_intent(&stations, "TEST", "taxi", None, [0.0, 0.0]).unwrap();
        assert_eq!(found.khz, 118_700);
        assert_eq!(found.service, "Tower");
    }
    #[test]
    fn the_tuned_frequency_wins_over_a_nearer_station() {
        let stations = [
            station("Ground", 121_700, 0.0),
            station("Ground", 121_900, 0.5),
        ];
        let found =
            station_for_intent(&stations, "TEST", "taxi", Some(121_900), [0.0, 0.0]).unwrap();
        assert_eq!(found.khz, 121_900);
    }
    #[test]
    fn delivery_is_a_clearance_service() {
        let mut delivery = station("Delivery", 121_900, 0.0);
        delivery.name = "Test Delivery".into();
        let stations = [delivery];
        let found = station_for_intent(&stations, "TEST", "clearance", None, [0.0, 0.0]).unwrap();
        assert_eq!(found.khz, 121_900);
    }
    #[test]
    fn no_ground_station_does_not_fall_back_to_tower() {
        let stations = [station("Tower", 118_700, 0.0)];
        assert!(station_for_intent(&stations, "TEST", "taxi", None, [0.0, 0.0]).is_none());
        assert!(station_for_intent(&stations, "TEST", "clearance", None, [0.0, 0.0]).is_none());
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn simulator_root_drops_trailing_separators_and_keeps_drive_roots() {
        assert_eq!(
            simulator_root("C:/X-Plane 12/").to_string_lossy(),
            "C:/X-Plane 12"
        );
        assert_eq!(
            simulator_root(r"C:\X-Plane 12\\").to_string_lossy(),
            r"C:\X-Plane 12"
        );
        assert_eq!(simulator_root("C:").to_string_lossy(), "C:/");
    }

    #[test]
    fn a_shared_frequency_keeps_one_station_with_both_services() {
        let mut stations = vec![
            Station {
                airport: "YSSY".into(),
                name: "PRM W (Monitor)".into(),
                service: "Approach".into(),
                khz: 119_450,
                ..Default::default()
            },
            Station {
                airport: "YSSY".into(),
                name: "SYDNEY Tower".into(),
                service: "Tower".into(),
                khz: 119_450,
                ..Default::default()
            },
        ];
        merge_shared_frequencies(&mut stations);
        assert_eq!(stations.len(), 1);
        assert_eq!(stations[0].service, "Tower");
        assert_eq!(stations[0].name, "SYDNEY Tower");
        assert!(stations[0].services.contains(&"Approach".to_owned()));
    }

    #[test]
    fn scenery_paths_ignore_a_byte_order_mark_and_a_trailing_root_separator() {
        let root =
            std::env::temp_dir().join(format!("openatc-scenery-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pack = root.join("Custom Scenery").join("TestPack");
        std::fs::create_dir_all(pack.join("Earth nav data")).unwrap();
        std::fs::write(pack.join("Earth nav data").join("apt.dat"), "I\n").unwrap();
        std::fs::write(
            root.join("Custom Scenery").join("scenery_packs.ini"),
            "\u{feff}SCENERY_PACK Custom Scenery/TestPack\n",
        )
        .unwrap();
        let root_text = format!("{}/", root.to_string_lossy());
        let paths = scenery_paths(&root_text);
        assert!(
            paths
                .iter()
                .any(|path| path.contains("TestPack") && path.ends_with("apt.dat")),
            "{paths:?}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
