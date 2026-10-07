//! METAR parsing and regional pressure formatting.

use super::regions::Region;
use super::rounded;

/// Parsed surface weather observation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Weather {
    /// Raw METAR text.
    pub raw: String,
    /// Wind group as reported, or `Unavailable`.
    pub wind: String,
    /// Visibility group as reported, or `Unavailable`.
    pub visibility: String,
    /// Cloud group as reported, or `Unavailable`.
    pub clouds: String,
    /// QNH in hectopascals.
    pub qnh: Option<i32>,
    /// Altimeter in hundredths of an inch of mercury.
    pub altimeter: Option<i32>,
}

/// Parse supported observation groups from a METAR.
#[must_use]
pub fn parse_metar(raw: &str) -> Weather {
    let mut weather = Weather {
        raw: raw.to_owned(),
        wind: "Unavailable".to_owned(),
        visibility: "Unavailable".to_owned(),
        clouds: "Unavailable".to_owned(),
        ..Default::default()
    };
    let group = |pattern: &str| {
        regex::Regex::new(pattern)
            .ok()
            .as_ref()
            .and_then(|expression| expression.find(raw))
            .map(|found| found.as_str().to_owned())
    };
    let number = |pattern: &str| {
        regex::Regex::new(pattern)
            .ok()
            .as_ref()
            .and_then(|expression| expression.captures(raw))
            .and_then(|found| found.get(1))
            .and_then(|digits| digits.as_str().parse::<i32>().ok())
    };
    if let Some(qnh) = number(r"\bQ([0-9]{4})\b") {
        weather.qnh = Some(qnh);
    }
    if let Some(altimeter) = number(r"\bA([0-9]{4})\b") {
        weather.altimeter = Some(altimeter);
        if weather.qnh.is_none() {
            // Values stay near 30 inHg: the widening conversion cannot overflow.
            weather.qnh = Some(
                i32::try_from(rounded(f64::from(altimeter) * 0.338_638_9)).unwrap_or(i32::MAX),
            );
        }
    }
    if let Some(wind) = group(r"\b(?:[0-9]{3}|VRB)[0-9]{2,3}(?:G[0-9]{2,3})?KT\b") {
        weather.wind = wind;
    }
    if raw.contains("CAVOK") {
        weather.visibility = String::from("10 km or more");
        weather.clouds = String::from("CAVOK");
    } else {
        if let Some(visibility) = group(r"\b[0-9]{1,2}(?:/[0-9])?SM\b|\b9999\b") {
            weather.visibility = visibility;
        }
        if let Some(clouds) = group(r"\b(?:FEW|SCT|BKN|OVC)[0-9]{3}(?:CB|TCU)?\b") {
            weather.clouds = clouds;
        }
    }
    weather
}

/// Format pressure in the regional units, including
/// the cross-unit derivation when the report carries only one value.
#[must_use]
pub fn pressure_text(weather: &Weather, region: &Region, speech: bool) -> String {
    let mut qnh = weather.qnh;
    let mut altimeter = weather.altimeter;
    if let (None, Some(alt)) = (qnh, altimeter) {
        // Values stay near 1000 hPa: the widening conversion cannot overflow.
        qnh = Some(i32::try_from(rounded(f64::from(alt) * 0.338_638_9)).unwrap_or(i32::MAX));
    }
    if altimeter.is_none()
        && let Some(qnh) = qnh
    {
        altimeter = Some(i32::try_from(rounded(f64::from(qnh) * 2.953)).unwrap_or(i32::MAX));
    }
    if region.pressure == "altimeter" {
        if let Some(altimeter) = altimeter {
            if speech {
                return format!("Altimeter {altimeter}");
            }
            return format!("{}.{:02} inHg", altimeter / 100, altimeter % 100);
        }
    } else if let Some(qnh) = qnh {
        return format!("QNH {qnh}");
    }
    if speech {
        "pressure unknown".to_owned()
    } else {
        "pressure --".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::super::regions::builtin_region;
    use super::*;
    use std::path::PathBuf;

    fn opt(value: &serde_json::Value) -> Option<i32> {
        if value.is_null() {
            None
        } else {
            Some(i32::try_from(value.as_i64().unwrap()).unwrap())
        }
    }

    #[test]
    fn matches_shared_fixtures() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/metar.json");
        let cases: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut count = 0;
        for case in cases["parses"].as_array().unwrap() {
            let weather = parse_metar(case["raw"].as_str().unwrap());
            assert_eq!(weather.wind, case["wind"].as_str().unwrap(), "wind");
            assert_eq!(
                weather.visibility,
                case["visibility"].as_str().unwrap(),
                "visibility"
            );
            assert_eq!(weather.clouds, case["clouds"].as_str().unwrap(), "clouds");
            assert_eq!(weather.qnh, opt(&case["qnh"]), "qnh");
            assert_eq!(weather.altimeter, opt(&case["altimeter"]), "altimeter");
            count += 1;
        }
        assert_eq!(count, 5, "parses run");
        for case in cases["pressures"].as_array().unwrap() {
            let weather = parse_metar(case["raw"].as_str().unwrap());
            let mut region = builtin_region("");
            if case["region"].as_str().unwrap() == "us" {
                region.pressure = "altimeter".to_owned();
            }
            assert_eq!(
                pressure_text(&weather, &region, case["speech"].as_bool().unwrap()),
                case["expected"].as_str().unwrap(),
                "pressure"
            );
            count += 1;
        }
        assert_eq!(count, 12, "all fixtures run");
    }
}
