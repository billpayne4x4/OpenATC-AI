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
/// Preserve enabled scenery priority, then use global/default airport data.
#[must_use]
pub fn scenery_paths(root: &str) -> Vec<String> {
    let mut paths = Vec::new();
    if let Ok(text) = std::fs::read_to_string(format!("{root}/Custom Scenery/scenery_packs.ini")) {
        for line in text.lines() {
            if let Some(relative) = line.strip_prefix("SCENERY_PACK ") {
                let relative = relative.trim();
                let base = if relative == "*GLOBAL_AIRPORTS*" {
                    format!("{root}/Global Scenery/Global Airports")
                } else if Path::new(relative).is_relative() {
                    format!("{root}/{relative}")
                } else {
                    relative.to_owned()
                };
                paths.push(format!("{base}/Earth nav data/apt.dat"));
            }
        }
    }
    paths.push(format!(
        "{root}/Global Scenery/Global Airports/Earth nav data/apt.dat"
    ));
    paths.push(format!(
        "{root}/Resources/default scenery/default apt dat/Earth nav data/apt.dat"
    ));
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
            let fields: Vec<_> = line.split_whitespace().collect();
            let Some(record) = fields.first().and_then(|v| v.parse::<i32>().ok()) else {
                continue;
            };
            if [1, 16, 17, 99].contains(&record) {
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
    let overrides = std::env::var("OPENATC_RADIO_STATIONS_FILE")
        .unwrap_or_else(|_| format!("{root}/Resources/plugins/OpenATC/radio-stations.toml"));
    if Path::new(&overrides).is_file() {
        apply_combined(
            &mut stations,
            &std::fs::read_to_string(overrides).map_err(|e| e.to_string())?,
        )?;
    }
    Ok(stations)
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
/// Airport radio horizon and regional coverage networks. No terrain shielding claim.
#[must_use]
pub fn nearby(index: &[Station], t: &Telemetry) -> Vec<Station> {
    if !t.position_valid {
        return Vec::new();
    }
    let mut result = Vec::new();
    for station in index {
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
        let regional = matches!(station.service.as_str(), "Approach" | "Center")
            && !station.coverage.is_empty();
        if if regional {
            distance <= range
                || station
                    .coverage
                    .iter()
                    .any(|volume| volume.radio_distance_nm(t) <= cap)
        } else {
            distance <= range
        } {
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
/// Primary role capabilities; combined airport duties are assigned during indexing.
#[must_use]
pub fn serves(service: &str, intent: &str) -> bool {
    match intent {
        "clearance" => service == "Clearance",
        "start" | "pushback" | "start_pushback" | "taxi" | "gate" | "progressive" => {
            service == "Ground"
        }
        "cross_runway" => service == "Ground" || service == "Tower",
        "ready" | "backtrack" | "landing" => service == "Tower",
        "checkin" => ["Tower", "Approach", "Departure", "Center"].contains(&service),
        "altitude" | "direct" | "descent" | "cancel_ifr" | "route" | "deviation" | "hold"
        | "speed" | "divert" => ["Approach", "Departure", "Center"].contains(&service),
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
    let file: ServiceFile = toml::from_str(text).map_err(|e| e.to_string())?;
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

/// Published controller volume. Longitude is unwrapped around the aircraft.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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
    fn radio_distance_nm(&self, t: &Telemetry) -> f64 {
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
    pub fn contains(&self, t: &Telemetry) -> bool {
        if self.points.len() < 3 || t.altitude_feet < self.floor || t.altitude_feet > self.ceiling {
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
    let airports: BTreeSet<_> = stations.iter().map(|s| s.airport.clone()).collect();
    for airport in airports {
        let has = |role: &str| {
            stations
                .iter()
                .any(|s| s.airport == airport && s.service == role)
        };
        let clearance = if has("Clearance") {
            "Clearance"
        } else if has("Ground") {
            "Ground"
        } else {
            "Tower"
        };
        let ground = if has("Ground") { "Ground" } else { "Tower" };
        for station in stations.iter_mut().filter(|s| s.airport == airport) {
            for (primary, capability) in [(clearance, "Clearance"), (ground, "Ground")] {
                if station.service == primary && !station.services.iter().any(|s| s == capability) {
                    station.services.push(capability.to_owned());
                }
            }
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
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
