//! Port of the C++ unit formatters (`core.cpp`): display/speech text for
//! altitudes, speeds, distances and climb rates, plus unit resolution.
//!
//! Differential-tested against the C++ implementation on the shared fixtures
//! in `tests/fixtures/units.json` — both sides must agree byte-for-byte.
//! Rounding matches C++ `lround` (half away from zero); the `%.1f` climb
//! format avoids exact-half values in fixtures where `format!` (half to even)
//! and `snprintf` could disagree.

pub mod hazards;
pub mod intents;
pub mod metar;
pub mod profiles;
pub mod regions;

/// Effective display/speech units. Hybrid flies feet aloft.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UnitSystem {
    /// Feet, knots, nautical miles, feet per minute.
    #[default]
    Imperial,
    /// Meters, km/h, kilometers, meters per second.
    Metric,
    /// Feet aloft; pressure and visibility stay regional.
    Hybrid,
}

/// Resolve the effective system from the `units` setting and departure ICAO.
/// Mirrors `resolveUnits`: unknown preferences fall back to imperial.
#[must_use]
pub fn resolve_units(preference: &str, departure: &str) -> UnitSystem {
    if preference == "metric" {
        return UnitSystem::Metric;
    }
    if preference == "region" && !departure.is_empty() {
        match departure.chars().next().unwrap_or(' ').to_ascii_uppercase() {
            'K' => return UnitSystem::Imperial,
            'Z' | 'U' => return UnitSystem::Metric,
            _ => return UnitSystem::Hybrid,
        }
    }
    UnitSystem::Imperial
}

/// Feet to meters.
#[must_use]
pub fn feet_to_meters(feet: f64) -> f64 {
    feet * 0.3048
}

/// Whole meters to feet, rounded to 100-foot steps (validation granularity).
/// Operation order mirrors the C++ (`meters / 0.3048 / 100`) bit-for-bit.
#[must_use]
pub fn meters_to_feet(meters: i32) -> i32 {
    let feet = rounded(f64::from(meters) / 0.3048 / 100.0) * 100;
    i32::try_from(feet).unwrap_or(i32::MAX)
}

fn plural(value: i64, one: &str, many: &str) -> String {
    format!(
        "{value} {}",
        if value == 1 || value == -1 { one } else { many }
    )
}

/// Round half away from zero to whole units, mirroring C++ `lround`.
/// The single cast below is exact: inputs are rounded aviation magnitudes,
// far inside `i64` range, exactly like the `static_cast<long>` it mirrors.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn rounded(value: f64) -> i64 {
    value.round() as i64
}

/// Altitude text: `32000 ft` / `32000 feet` / `9750 m` / `9750 meters`.
#[must_use]
pub fn altitude_text(feet: f64, units: UnitSystem, speech: bool) -> String {
    if units != UnitSystem::Metric {
        let whole = rounded(feet);
        if speech {
            return plural(whole, "foot", "feet");
        }
        return format!("{whole} ft");
    }
    let meters = rounded(feet * 0.3048 / 10.0) * 10;
    if speech {
        return plural(meters, "meter", "meters");
    }
    format!("{meters} m")
}

/// Speed text: `440 kt` / `440 knots` / `815 km/h` / `815 kilometers per hour`.
#[must_use]
pub fn speed_text(knots: f64, units: UnitSystem, speech: bool) -> String {
    if units != UnitSystem::Metric {
        let whole = rounded(knots);
        if speech {
            return plural(whole, "knot", "knots");
        }
        return format!("{whole} kt");
    }
    let kmh = rounded(knots * 1.852);
    if speech {
        return plural(kmh, "kilometer per hour", "kilometers per hour");
    }
    format!("{kmh} km/h")
}

/// Distance text: `214 NM` / `214 miles` / `396 km` / `396 kilometers`.
#[must_use]
pub fn distance_text(nm: f64, units: UnitSystem, speech: bool) -> String {
    if units != UnitSystem::Metric {
        let whole = rounded(nm);
        if speech {
            return plural(whole, "mile", "miles");
        }
        return format!("{whole} NM");
    }
    let km = rounded(nm * 1.852);
    if speech {
        return plural(km, "kilometer", "kilometers");
    }
    format!("{km} km")
}

/// Climb-rate text: `500 fpm` / `500 feet per minute` / `2.5 m/s`.
#[must_use]
pub fn climb_rate_text(fpm: f64, units: UnitSystem, speech: bool) -> String {
    if units != UnitSystem::Metric {
        let whole = rounded(fpm);
        if speech {
            return plural(whole, "foot per minute", "feet per minute");
        }
        return format!("{whole} fpm");
    }
    let ms = (fpm * 0.00508 * 10.0).round() / 10.0;
    if speech {
        // Exact compare mirrors the C++ (`ms == 1`): `ms` is already rounded
        // to one decimal, so 1.0 and -1.0 occur exactly, never approximately.
        #[allow(clippy::float_cmp)]
        let word = if ms == 1.0 || ms == -1.0 {
            "meter per second"
        } else {
            "meters per second"
        };
        return format!("{ms:.1} {word}");
    }
    format!("{ms:.1} m/s")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn parse_units(name: &str) -> UnitSystem {
        match name {
            "metric" => UnitSystem::Metric,
            "hybrid" => UnitSystem::Hybrid,
            _ => UnitSystem::Imperial,
        }
    }

    #[test]
    fn matches_shared_fixtures() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/units.json");
        let cases: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut count = 0;
        for case in cases.as_array().unwrap() {
            let actual = match case["fn"].as_str().unwrap() {
                "resolveUnits" => match resolve_units(
                    case["preference"].as_str().unwrap(),
                    case["departure"].as_str().unwrap(),
                ) {
                    UnitSystem::Imperial => "imperial",
                    UnitSystem::Metric => "metric",
                    UnitSystem::Hybrid => "hybrid",
                }
                .to_owned(),
                "altitudeText" => altitude_text(
                    case["feet"].as_f64().unwrap(),
                    parse_units(case["units"].as_str().unwrap()),
                    case["speech"].as_bool().unwrap(),
                ),
                "speedText" => speed_text(
                    case["knots"].as_f64().unwrap(),
                    parse_units(case["units"].as_str().unwrap()),
                    case["speech"].as_bool().unwrap(),
                ),
                "distanceText" => distance_text(
                    case["nm"].as_f64().unwrap(),
                    parse_units(case["units"].as_str().unwrap()),
                    case["speech"].as_bool().unwrap(),
                ),
                "climbRateText" => climb_rate_text(
                    case["fpm"].as_f64().unwrap(),
                    parse_units(case["units"].as_str().unwrap()),
                    case["speech"].as_bool().unwrap(),
                ),
                "feetToMeters" => {
                    let value = feet_to_meters(case["feet"].as_f64().unwrap());
                    let expected = case["expected"].as_f64().unwrap();
                    assert!(
                        (value - expected).abs() < 1e-9,
                        "feetToMeters {}",
                        case["feet"]
                    );
                    count += 1;
                    continue;
                }
                "metersToFeet" => {
                    let value =
                        meters_to_feet(i32::try_from(case["meters"].as_i64().unwrap()).unwrap());
                    assert_eq!(
                        value,
                        i32::try_from(case["expected"].as_i64().unwrap()).unwrap(),
                        "metersToFeet"
                    );
                    count += 1;
                    continue;
                }
                unknown => panic!("unknown fixture fn {unknown}"),
            };
            assert_eq!(
                actual,
                case["expected"].as_str().unwrap(),
                "fixture {}",
                case["fn"].as_str().unwrap()
            );
            count += 1;
        }
        assert!(count >= 29, "fixtures loaded: {count}");
    }
}
