//! X-Plane airspace polygons and supplemental areas.
//!
//! The simulator ships every published control area in
//! `Resources/default data/airspaces/airspace.txt`. This module keeps the
//! prohibited, restricted and danger areas and answers point queries.

use serde::Deserialize;
use std::path::Path;

/// One published airspace volume.
#[derive(Clone, Debug, Deserialize)]
pub struct Area {
    /// Published name.
    pub name: String,
    /// Airspace class or type (`P`, `R`, `D`, …).
    pub class: String,
    /// Lower limit in feet MSL.
    pub lower_ft: i32,
    /// Upper limit in feet MSL.
    pub upper_ft: i32,
    /// Polygon vertices as latitude, longitude.
    pub points: Vec<[f64; 2]>,
}

#[derive(Clone, Debug)]
struct Indexed {
    area: Area,
    south: f64,
    north: f64,
    west: f64,
    east: f64,
}

/// Airspace areas with a latitude/longitude bounding box for each polygon.
#[derive(Clone, Debug, Default)]
pub struct AirspaceIndex {
    areas: Vec<Indexed>,
}

impl AirspaceIndex {
    /// Load the simulator airspace file.
    ///
    /// # Errors
    ///
    /// A missing or unreadable file is reported; a malformed block is skipped.
    pub fn load(simulator_root: &Path) -> Result<Self, String> {
        let path = simulator_root
            .join("Resources")
            .join("default data")
            .join("airspaces")
            .join("airspace.txt");
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        Ok(Self::parse(&text))
    }

    /// Parse the X-Plane airspace format.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut index = Self::default();
        let mut current = None;
        for line in text.lines() {
            if let Some(class) = line.strip_prefix("AC ") {
                if let Some(area) = current.take() {
                    index.add_one(area);
                }
                current = Some(Area {
                    name: String::new(),
                    class: class.trim().to_owned(),
                    lower_ft: 0,
                    upper_ft: 60_000,
                    points: Vec::new(),
                });
                continue;
            }
            let Some(area) = current.as_mut() else {
                continue;
            };
            if let Some(name) = line.strip_prefix("AN ") {
                area.name = name.trim().to_owned();
            } else if let Some(limit) = line.strip_prefix("AL ") {
                area.lower_ft = parse_altitude(limit).unwrap_or(0);
            } else if let Some(limit) = line.strip_prefix("AH ") {
                area.upper_ft = parse_altitude(limit).unwrap_or(60_000);
            } else if let Some(point) = line.strip_prefix("DP ")
                && let Some(point) = parse_point(point)
            {
                area.points.push(point);
            }
        }
        if let Some(area) = current {
            index.add_one(area);
        }
        index
    }

    /// Add supplemental areas, keeping only valid polygons.
    pub fn add(&mut self, areas: impl IntoIterator<Item = Area>) {
        for area in areas {
            self.add_one(area);
        }
    }

    fn add_one(&mut self, area: Area) {
        if !matches!(area.class.as_str(), "P" | "R" | "D") || area.points.len() < 3 {
            return;
        }
        let mut south = f64::INFINITY;
        let mut north = f64::NEG_INFINITY;
        let mut west = f64::INFINITY;
        let mut east = f64::NEG_INFINITY;
        for [latitude, longitude] in &area.points {
            south = south.min(*latitude);
            north = north.max(*latitude);
            west = west.min(*longitude);
            east = east.max(*longitude);
        }
        self.areas.push(Indexed {
            area,
            south,
            north,
            west,
            east,
        });
    }

    /// Find the first area containing the position and altitude.
    #[must_use]
    pub fn find(
        &self,
        latitude: f64,
        longitude: f64,
        altitude_ft: f64,
        classes: &[String],
    ) -> Option<&Area> {
        self.areas.iter().find_map(|indexed| {
            if !classes
                .iter()
                .any(|class| class.eq_ignore_ascii_case(&indexed.area.class))
                || latitude < indexed.south
                || latitude > indexed.north
                || longitude < indexed.west
                || longitude > indexed.east
                || altitude_ft < f64::from(indexed.area.lower_ft)
                || altitude_ft > f64::from(indexed.area.upper_ft)
            {
                return None;
            }
            contains(&indexed.area.points, latitude, longitude).then_some(&indexed.area)
        })
    }
}

fn parse_point(text: &str) -> Option<[f64; 2]> {
    let parts: Vec<_> = text.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let latitude = parse_dms(parts[0])?;
    let longitude = parse_dms(parts[2])?;
    let latitude = if parts[1].eq_ignore_ascii_case("S") {
        -latitude
    } else {
        latitude
    };
    let longitude = if parts[3].eq_ignore_ascii_case("W") {
        -longitude
    } else {
        longitude
    };
    Some([latitude, longitude])
}

fn parse_dms(text: &str) -> Option<f64> {
    let mut parts = text.split(':');
    let degrees: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next().unwrap_or("0").parse().ok()?;
    Some(degrees + minutes / 60.0 + seconds / 3600.0)
}

fn parse_altitude(text: &str) -> Option<i32> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("GND") || text.eq_ignore_ascii_case("SFC") {
        return Some(0);
    }
    if text.eq_ignore_ascii_case("UNL") || text.eq_ignore_ascii_case("UNLTD") {
        return Some(60_000);
    }
    if let Some(level) = text.strip_prefix("FL") {
        return level.trim().parse::<i32>().ok().map(|level| level * 100);
    }
    let number = text
        .trim_end_matches(|c: char| c.is_ascii_alphabetic() || c.is_whitespace())
        .trim();
    number.parse::<f64>().ok().map(|value| value as i32)
}

fn contains(points: &[[f64; 2]], latitude: f64, longitude: f64) -> bool {
    let mut inside = false;
    let mut previous = points.len() - 1;
    for current in 0..points.len() {
        let [lat_a, lon_a] = points[current];
        let [lat_b, lon_b] = points[previous];
        if (lat_a > latitude) != (lat_b > latitude)
            && longitude < (lon_b - lon_a) * (latitude - lat_a) / (lat_b - lat_a) + lon_a
        {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
AC P
AN P-56 (P-56)
AL GND
AH 19700 MSL
DP 38:50:00 N 077:00:00 W
DP 38:50:00 N 077:10:00 W
DP 39:00:00 N 077:10:00 W
DP 39:00:00 N 077:00:00 W

AC C
AN NOT RESTRICTED
AL GND
AH 10000 MSL
DP 38:00:00 N 077:00:00 W
DP 38:00:00 N 077:10:00 W
DP 38:10:00 N 077:10:00 W
";

    #[test]
    fn finds_a_prohibited_area_and_ignores_other_classes() {
        let index = AirspaceIndex::parse(SAMPLE);
        let classes = vec!["P".to_owned(), "R".to_owned()];
        assert_eq!(
            index
                .find(38.9, -77.05, 1000.0, &classes)
                .map(|area| area.name.as_str()),
            Some("P-56 (P-56)")
        );
        assert!(index.find(38.9, -77.05, 25_000.0, &classes).is_none());
        assert!(index.find(38.05, -77.05, 1000.0, &classes).is_none());
    }

    #[test]
    fn supplemental_areas_are_available() {
        let mut index = AirspaceIndex::default();
        index.add([Area {
            name: "Parliament House".to_owned(),
            class: "R".to_owned(),
            lower_ft: 0,
            upper_ft: 4500,
            points: vec![
                [-35.30, 149.12],
                [-35.30, 149.13],
                [-35.32, 149.13],
                [-35.32, 149.12],
            ],
        }]);
        assert!(
            index
                .find(-35.31, 149.125, 1000.0, &["R".to_owned()])
                .is_some()
        );
    }

    #[test]
    fn dms_and_flight_levels_parse() {
        assert!((parse_dms("51:43:18").unwrap() - 51.721_666).abs() < 0.000_1);
        assert_eq!(parse_altitude("19500 MSL"), Some(19_500));
        assert_eq!(parse_altitude("FL195"), Some(19_500));
        assert_eq!(parse_altitude("GND"), Some(0));
    }
}
