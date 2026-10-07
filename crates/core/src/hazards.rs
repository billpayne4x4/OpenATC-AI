//! Weather hazard checks and controller advisories.

use super::metar::{parse_metar, pressure_text};
use super::regions::Region;
use super::{UnitSystem, altitude_text, rounded};

/// Flight stage used by weather advisory checks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Phase {
    /// On stand, engines may be off.
    #[default]
    Parked = 0,
    /// IFR clearance received, pre-taxi.
    Clearance = 1,
    /// Moving under own power to the runway.
    Taxi = 2,
    /// Takeoff roll through initial climb.
    Departure = 3,
    /// Enroute cruise.
    Cruise = 4,
    /// Arrival phase (displayed as Descent).
    Arrival = 5,
    /// Approach phase, windshear advisories arm.
    Approach = 6,
    /// Wheels down, on the runway.
    Landed = 7,
    /// Pushback from stand.
    Pushback = 8,
    /// Taxi from runway to parking.
    TaxiIn = 9,
    /// Flight complete, state kept for review.
    Finished = 10,
}

/// Parse a phase name as the fixtures spell it.
#[must_use]
pub fn parse_phase(name: &str) -> Phase {
    match name {
        "Clearance" => Phase::Clearance,
        "Taxi" => Phase::Taxi,
        "Departure" => Phase::Departure,
        "Cruise" => Phase::Cruise,
        "Arrival" => Phase::Arrival,
        "Approach" => Phase::Approach,
        "Landed" => Phase::Landed,
        "Pushback" => Phase::Pushback,
        "TaxiIn" => Phase::TaxiIn,
        "Finished" => Phase::Finished,
        _ => Phase::Parked,
    }
}

/// Detected weather hazard.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WeatherHazard {
    /// Reporting station.
    pub station: String,
    /// Machine key (`thunderstorm`, `hail`, …).
    pub kind: String,
    /// Human phrase fragment.
    pub summary: String,
}

fn report_word(report: &serde_json::Value, key: &str) -> String {
    report
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned()
}

fn report_number(report: &serde_json::Value, key: &str) -> f64 {
    report
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0)
}

/// Evaluate METAR reports for hazards, applying each rule
/// for rule, including convective priority and approach-only windshear.
#[must_use]
pub fn evaluate_weather(
    reports: &serde_json::Value,
    departure: &str,
    destination: &str,
    alternate: &str,
    phase: Phase,
) -> Vec<WeatherHazard> {
    let mut hazards = Vec::new();
    let Some(reports) = reports.as_array() else {
        return hazards;
    };
    let windshear = regex::Regex::new(r"\bWS\b").ok();
    for report in reports {
        if !report.is_object() {
            continue;
        }
        let station = report_word(report, "icaoId");
        if station.is_empty() {
            continue;
        }
        if station != departure && station != destination && station != alternate {
            continue;
        }
        let terminal = station == destination || station == alternate;
        let present = report_word(report, "wxString");
        let raw = report_word(report, "rawOb");
        let category = report_word(report, "fltCat");
        if present.contains("TS") {
            hazards.push(WeatherHazard {
                station: station.clone(),
                kind: "thunderstorm".to_owned(),
                summary: "thunderstorm".to_owned(),
            });
        } else if present.contains("GR") {
            hazards.push(WeatherHazard {
                station: station.clone(),
                kind: "hail".to_owned(),
                summary: "hail".to_owned(),
            });
        } else if present.contains("FZ") {
            hazards.push(WeatherHazard {
                station: station.clone(),
                kind: "freezing_rain".to_owned(),
                summary: "freezing rain".to_owned(),
            });
        } else if present.contains("VA") {
            hazards.push(WeatherHazard {
                station: station.clone(),
                kind: "volcanic_ash".to_owned(),
                summary: "volcanic ash".to_owned(),
            });
        }
        if phase == Phase::Approach
            && windshear
                .as_ref()
                .is_some_and(|pattern| pattern.is_match(&raw))
        {
            hazards.push(WeatherHazard {
                station: station.clone(),
                kind: "windshear".to_owned(),
                summary: "windshear".to_owned(),
            });
        }
        if terminal && (category == "IFR" || category == "LIFR") {
            hazards.push(WeatherHazard {
                station: station.clone(),
                kind: "ifr".to_owned(),
                summary: format!("{category} conditions"),
            });
        }
    }
    hazards
}

/// `%.2g`-style visibility formatting for the realistic value domain.
fn sig2(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    if value >= 10.0 {
        return format!("{}", rounded(value));
    }
    let text = if value >= 1.0 {
        format!("{:.1}", (value * 10.0).round() / 10.0)
    } else {
        format!("{:.2}", (value * 100.0).round() / 100.0)
    };
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// Format the controller advisory;
/// `%.0f`/`%.2g` roundings avoid exact-half fixture values where `format!`
/// (half to even) and `snprintf` could disagree.
#[must_use]
pub fn advisory_text(
    callsign: &str,
    report: &serde_json::Value,
    hazard: &WeatherHazard,
    region: &Region,
    units: UnitSystem,
) -> String {
    let station = hazard.station.clone();
    let wind = {
        let direction = report_number(report, "wdir");
        let speed = report_number(report, "wspd");
        let gust = report_number(report, "wgst");
        if speed <= 0.0 {
            "wind calm".to_owned()
        } else {
            let base = if report_word(report, "wdir") == "VRB" || direction <= 0.0 {
                format!("variable at {speed:.0}")
            } else {
                format!("{direction:.0} at {speed:.0}")
            };
            if gust > speed {
                format!("wind {base} gusting {gust:.0} knots")
            } else {
                format!("wind {base} knots")
            }
        }
    };
    let visibility = {
        let visib = report_number(report, "visib");
        if units == UnitSystem::Metric {
            let meters = rounded(visib * 1609.0 / 100.0) * 100;
            let word = if meters == 1 || meters == -1 {
                "meter"
            } else {
                "meters"
            };
            format!("{meters} {word}")
        } else {
            format!("{} miles", sig2(visib))
        }
    };
    let pressure = pressure_text(&parse_metar(&report_word(report, "rawOb")), region, true);
    if hazard.kind == "windshear" {
        return format!("{callsign}, windshear reported at {station}. Advise intentions.");
    }
    if hazard.kind == "ifr" {
        let mut ceiling = String::new();
        if let Some(clouds) = report.get("clouds").and_then(|value| value.as_array()) {
            let mut lowest = 0.0;
            let mut cover = String::new();
            for layer in clouds {
                let kind = layer
                    .get("cover")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let base = layer
                    .get("base")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                if (kind == "BKN" || kind == "OVC") && (lowest == 0.0 || base < lowest) {
                    lowest = base;
                    cover = String::from(if kind == "OVC" { "overcast" } else { "broken" });
                }
            }
            if lowest > 0.0 {
                ceiling = format!(", ceiling {} {cover}", altitude_text(lowest, units, true));
            }
        }
        return format!(
            "{callsign}, {station} is now {}{ceiling}, {pressure}. Advise intentions.",
            hazard.summary
        );
    }
    format!(
        "{callsign}, {station} weather: {}, {wind}, visibility {visibility}, {pressure}. Advise intentions.",
        hazard.summary
    )
}

#[cfg(test)]
mod tests {
    use super::super::regions::builtin_region;
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn matches_shared_fixtures() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/hazards.json");
        let cases: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut count = 0;
        for case in cases["evaluations"].as_array().unwrap() {
            let hazards = evaluate_weather(
                &case["reports"],
                case["departure"].as_str().unwrap(),
                case["destination"].as_str().unwrap(),
                case["alternate"].as_str().unwrap_or_default(),
                parse_phase(case["phase"].as_str().unwrap()),
            );
            let kinds: Vec<&str> = hazards.iter().map(|hazard| hazard.kind.as_str()).collect();
            let expected: Vec<&str> = case["expected"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect();
            assert_eq!(
                kinds,
                expected,
                "hazards for {}",
                case["name"].as_str().unwrap()
            );
            count += 1;
        }
        assert!(count >= 6, "evaluations run: {count}");
        for case in cases["advisories"].as_array().unwrap() {
            let report = &case["report"];
            let hazard = WeatherHazard {
                station: case["station"].as_str().unwrap().to_owned(),
                kind: case["kind"].as_str().unwrap().to_owned(),
                summary: case["summary"].as_str().unwrap().to_owned(),
            };
            let mut region = builtin_region("");
            if case["region"].as_str().unwrap() == "us" {
                region.pressure = "altimeter".to_owned();
            }
            let units = match case["units"].as_str().unwrap() {
                "metric" => UnitSystem::Metric,
                _ => UnitSystem::Imperial,
            };
            assert_eq!(
                advisory_text("VH-BIL", report, &hazard, &region, units),
                case["expected"].as_str().unwrap(),
                "advisory {}",
                case["name"].as_str().unwrap()
            );
            count += 1;
        }
        assert!(count >= 10, "all fixtures run: {count}");
    }
}
