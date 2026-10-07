//! Regional procedure settings and unit selection.

use super::UnitSystem;
use std::collections::BTreeMap;
use std::path::Path;

/// Local procedure for one departure region.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Region {
    /// Table key (`us`, `icao`, …).
    pub name: String,
    /// `qnh` or `altimeter`.
    pub pressure: String,
    /// `feet` or `meters`.
    pub altitude: String,
    /// `initial` (US) or `sid` (ICAO).
    pub clearance: String,
    /// Feet where flight levels take over.
    pub transition_feet: i32,
    /// Area control fallback kHz, if the region defines one. Absent means
    /// uncontrolled enroute airspace: nobody answers out there.
    pub center_khz: Option<i32>,
}

/// Default regional procedure table.
#[must_use]
pub fn builtin_region(icao: &str) -> Region {
    let icao_default = Region {
        name: "icao".to_owned(),
        pressure: "qnh".to_owned(),
        altitude: "feet".to_owned(),
        clearance: "sid".to_owned(),
        transition_feet: 10_000,
        center_khz: None,
    };
    if icao.is_empty() {
        return icao_default;
    }
    let prefix = icao.chars().next().unwrap_or(' ').to_ascii_uppercase();
    match prefix {
        'K' => Region {
            name: "us".to_owned(),
            pressure: "altimeter".to_owned(),
            clearance: "initial".to_owned(),
            transition_feet: 18_000,
            ..icao_default
        },
        'Z' => Region {
            name: "china".to_owned(),
            altitude: "meters".to_owned(),
            transition_feet: 20_000,
            ..icao_default
        },
        'U' => Region {
            name: "russia".to_owned(),
            altitude: "meters".to_owned(),
            transition_feet: 15_000,
            ..icao_default
        },
        'Y' => Region {
            name: "australia".to_owned(),
            ..icao_default
        },
        'E' | 'L' => Region {
            name: "europe".to_owned(),
            ..icao_default
        },
        _ => icao_default,
    }
}

/// Load and validate a regions file.
/// Returns prefix letter → region.
pub fn load_regions(path: &Path) -> Result<BTreeMap<String, Region>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("cannot open file: {error}"))?;
    let table: toml::Table = text
        .parse()
        .map_err(|error| format!("malformed file: {error}"))?;
    let regions = table
        .get("regions")
        .and_then(|value| value.as_table())
        .ok_or_else(|| "only [regions.NAME] sections allowed".to_owned())?;
    if table.keys().any(|key| key != "regions") {
        return Err("only [regions.NAME] sections allowed".to_owned());
    }
    let mut out = BTreeMap::new();
    for (name, body) in regions {
        let body = body
            .as_table()
            .ok_or_else(|| format!("bad section {name}"))?;
        for key in body.keys() {
            match key.as_str() {
                "prefixes" | "pressure" | "altitude" | "clearance" | "transition_feet"
                | "center_khz" => {}
                _ => return Err(format!("unknown regions key: {key}")),
            }
        }
        let get = |key: &str| {
            body.get(key)
                .and_then(|value| value.as_str())
                .ok_or_else(|| format!("region {name} misses {key}"))
        };
        let pressure = get("pressure")?.to_owned();
        let altitude = get("altitude")?.to_owned();
        let clearance = get("clearance")?.to_owned();
        if pressure != "qnh" && pressure != "altimeter" {
            return Err(format!("region {name} has bad pressure"));
        }
        if altitude != "feet" && altitude != "meters" {
            return Err(format!("region {name} has bad altitude"));
        }
        if clearance != "initial" && clearance != "sid" {
            return Err(format!("region {name} has bad clearance"));
        }
        let transition_feet: i32 = body
            .get("transition_feet")
            .and_then(toml::Value::as_integer)
            .ok_or_else(|| format!("region {name} misses transition_feet"))?
            .try_into()
            .map_err(|_| format!("region {name} has bad transition_feet"))?;
        if transition_feet <= 0 {
            return Err(format!("region {name} has bad transition_feet"));
        }
        let prefixes = body
            .get("prefixes")
            .and_then(|value| value.as_array())
            .ok_or_else(|| format!("region {name} misses prefixes"))?;
        let region = Region {
            name: name.to_owned(),
            pressure,
            altitude,
            clearance,
            transition_feet,
            center_khz: match body.get("center_khz") {
                None => None,
                Some(value) => {
                    let khz: i32 = value
                        .as_integer()
                        .and_then(|khz| khz.try_into().ok())
                        .ok_or_else(|| format!("region {name} has bad center_khz"))?;
                    if !(118_000..=136_990).contains(&khz) {
                        return Err(format!("region {name} has bad center_khz"));
                    }
                    Some(khz)
                }
            },
        };
        for prefix in prefixes {
            let letter = prefix
                .as_str()
                .filter(|text| text.len() == 1)
                .ok_or_else(|| format!("region {name} prefixes hold single letters"))?;
            out.insert(letter.to_owned(), region.clone());
        }
    }
    Ok(out)
}

/// Look up the region for an ICAO: prefix table, then the `icao` entry,
/// then use the built-in fallback.
#[must_use]
pub fn region_for(table: &BTreeMap<String, Region>, icao: &str) -> Region {
    if !icao.is_empty() {
        let prefix = icao
            .chars()
            .next()
            .unwrap_or(' ')
            .to_ascii_uppercase()
            .to_string();
        if let Some(region) = table.get(&prefix) {
            return region.clone();
        }
    }
    if let Some(region) = table.values().find(|region| region.name == "icao") {
        return region.clone();
    }
    builtin_region(icao)
}

/// Select the effective units for a region.
#[must_use]
pub fn units_for_region(region: &Region) -> UnitSystem {
    if region.altitude == "meters" {
        return UnitSystem::Metric;
    }
    if region.pressure == "altimeter" {
        return UnitSystem::Imperial;
    }
    UnitSystem::Hybrid
}

/// Local procedure note included in model prompts.
#[must_use]
pub fn region_notes(region: &Region, units: UnitSystem) -> String {
    let altitude = if units == UnitSystem::Metric {
        "meters"
    } else {
        "feet"
    };
    let pressure = if region.pressure == "altimeter" {
        "altimeter in inches of mercury"
    } else {
        "QNH in hectopascals"
    };
    format!(
        "Local procedure ({}): {pressure}; altitudes in {altitude}; transition altitude {} feet.",
        region.name, region.transition_feet
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
    }

    fn unit_name(units: UnitSystem) -> &'static str {
        match units {
            UnitSystem::Imperial => "imperial",
            UnitSystem::Metric => "metric",
            UnitSystem::Hybrid => "hybrid",
        }
    }

    #[test]
    fn matches_shared_fixtures() {
        let dir = fixtures();
        let cases: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("regions.json")).unwrap())
                .unwrap();
        let table = load_regions(&dir.join(cases["toml"].as_str().unwrap())).unwrap();
        let mut count = 0;
        for case in cases["lookups"].as_array().unwrap() {
            let region = region_for(&table, case["icao"].as_str().unwrap());
            assert_eq!(region.name, case["name"].as_str().unwrap(), "name");
            assert_eq!(
                region.pressure,
                case["pressure"].as_str().unwrap(),
                "pressure"
            );
            assert_eq!(
                region.altitude,
                case["altitude"].as_str().unwrap(),
                "altitude"
            );
            assert_eq!(
                region.clearance,
                case["clearance"].as_str().unwrap(),
                "clearance"
            );
            assert_eq!(
                region.transition_feet,
                i32::try_from(case["transition_feet"].as_i64().unwrap()).unwrap(),
                "transition"
            );
            assert_eq!(
                unit_name(units_for_region(&region)),
                case["units"].as_str().unwrap(),
                "units"
            );
            count += 1;
        }
        assert_eq!(count, 5, "lookups run");
        for (index, text) in cases["invalid"].as_array().unwrap().iter().enumerate() {
            let path = std::env::temp_dir().join(format!("openatc_regions_bad_{index}.toml"));
            std::fs::write(&path, text.as_str().unwrap()).unwrap();
            assert!(
                load_regions(&path).is_err(),
                "invalid fixture {index} must reject"
            );
            let _ = std::fs::remove_file(&path);
            count += 1;
        }
        assert_eq!(count, 10, "all fixtures run");
    }

    #[test]
    fn builtin_matches_fixtures() {
        let dir = fixtures();
        let cases: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("regions.json")).unwrap())
                .unwrap();
        let mut count = 0;
        for case in cases["builtin"].as_array().unwrap() {
            let region = builtin_region(case["icao"].as_str().unwrap());
            assert_eq!(region.name, case["name"].as_str().unwrap(), "name");
            assert_eq!(
                region.pressure,
                case["pressure"].as_str().unwrap(),
                "pressure"
            );
            assert_eq!(
                region.altitude,
                case["altitude"].as_str().unwrap(),
                "altitude"
            );
            assert_eq!(
                region.clearance,
                case["clearance"].as_str().unwrap(),
                "clearance"
            );
            assert_eq!(
                region.transition_feet,
                i32::try_from(case["transition_feet"].as_i64().unwrap()).unwrap(),
                "transition"
            );
            count += 1;
        }
        assert_eq!(count, 4, "builtin fixtures run");
    }
}
