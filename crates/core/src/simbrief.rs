//! Port of the `SimBrief` OFP normalizer: JSON in, flight plan out. Lenient
//! field coercion, strict number parsing, then the same plan validation the
//! UI runs.

use super::ops::validate_flight_plan;
use super::state::{FlightPlan, RouteFix};

/// Truncate toward zero, same as the original cast. `SimBrief` numbers are small sane
/// magnitudes, so this is exact in practice.
#[allow(clippy::cast_possible_truncation)]
fn trunc_int(value: f64) -> i32 {
    value as i32
}

/// String field with fallback; numbers stringify, anything else falls back.
fn string_value(object: &serde_json::Value, key: &str, fallback: &str) -> String {
    let Some(value) = object.get(key) else {
        return fallback.to_owned();
    };
    if value.is_null() {
        return fallback.to_owned();
    }
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    if value.is_number() {
        return value.to_string();
    }
    fallback.to_owned()
}

/// Strict number field: whole string must parse finite, else an error naming
/// the field. Empty means the fallback.
fn number_value(object: &serde_json::Value, key: &str, fallback: f64) -> Result<f64, String> {
    let text = string_value(object, key, "");
    if text.is_empty() {
        return Ok(fallback);
    }
    match text.parse::<f64>() {
        Ok(value) if value.is_finite() => Ok(value),
        _ => Err(format!("Invalid SimBrief field: {key}")),
    }
}

fn section(document: &serde_json::Value, key: &str) -> serde_json::Value {
    document
        .get(key)
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// Append an airport endpoint fix when it carries coordinates.
fn push_airport_fix(fixes: &mut Vec<RouteFix>, airport: &serde_json::Value) -> Result<(), String> {
    let latitude = string_value(airport, "pos_lat", "");
    let longitude = string_value(airport, "pos_long", "");
    if !latitude.is_empty() && !longitude.is_empty() {
        fixes.push(RouteFix {
            identifier: string_value(airport, "icao_code", ""),
            latitude: number_value(airport, "pos_lat", 0.0)?,
            longitude: number_value(airport, "pos_long", 0.0)?,
            altitude_feet: number_value(airport, "elevation", 0.0)?,
        });
    }
    Ok(())
}

/// Collect the navlog fixes, skipping entries without coordinates. A lone
/// object counts as a one-item list, like over there.
fn navlog_fixes(document: &serde_json::Value) -> Result<Vec<RouteFix>, String> {
    let navlog = section(document, "navlog");
    let mut fixes = if navlog.is_object() {
        navlog
            .get("fix")
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    };
    if fixes.is_object() {
        fixes = serde_json::Value::Array(vec![fixes]);
    }
    let mut route = Vec::new();
    if let Some(items) = fixes.as_array() {
        for fix in items {
            if string_value(fix, "pos_lat", "").is_empty()
                || string_value(fix, "pos_long", "").is_empty()
            {
                continue;
            }
            route.push(RouteFix {
                identifier: string_value(fix, "ident", ""),
                latitude: number_value(fix, "pos_lat", 0.0)?,
                longitude: number_value(fix, "pos_long", 0.0)?,
                altitude_feet: number_value(fix, "altitude_feet", 0.0)?,
            });
        }
    }
    Ok(route)
}

/// Normalize one `SimBrief` OFP document into a flight plan. Errors carry the
/// consistent validation messages.
pub fn parse_simbrief(document: &serde_json::Value) -> Result<FlightPlan, String> {
    if !document.is_object()
        || document.get("origin").is_none()
        || document.get("destination").is_none()
        || document.get("general").is_none()
    {
        return Err("SimBrief did not return a flight plan. Generate an OFP first.".to_owned());
    }
    let origin = section(document, "origin");
    let destination = section(document, "destination");
    let general = section(document, "general");
    let aircraft = section(document, "aircraft");
    let weights = section(document, "weights");
    let fuel = section(document, "fuel");
    let params = section(document, "params");
    let registration = string_value(&aircraft, "reg", "");
    let mut callsign =
        string_value(&general, "icao_airline", "") + &string_value(&general, "flight_number", "");
    if callsign.is_empty() {
        callsign = if registration.is_empty() {
            "OPENATC".to_owned()
        } else {
            registration.clone()
        };
    }
    let mut alternate = section(document, "alternate");
    if let Some(first) = alternate.as_array().and_then(|items| items.first()) {
        alternate = first.clone();
    }
    let units = string_value(&params, "units", "kgs");
    let kilograms = if units == "lbs" || units == "LBS" {
        0.453_592_37
    } else {
        1.0
    };
    let mut fixes = Vec::new();
    push_airport_fix(&mut fixes, &origin)?;
    fixes.extend(navlog_fixes(document)?);
    push_airport_fix(&mut fixes, &destination)?;
    let plan = FlightPlan {
        departure: string_value(&origin, "icao_code", ""),
        destination: string_value(&destination, "icao_code", ""),
        runway: string_value(&origin, "plan_rwy", ""),
        arrival_runway: string_value(&destination, "plan_rwy", ""),
        route: string_value(&general, "route", ""),
        cruise_feet: trunc_int(number_value(&general, "initial_altitude", 32000.0)?),
        callsign,
        registration,
        aircraft: string_value(&aircraft, "icaocode", "A20N"),
        sid: string_value(&general, "sid_ident", ""),
        star: string_value(&general, "star_ident", ""),
        sid_transition: string_value(&general, "sid_trans", ""),
        star_transition: string_value(&general, "star_trans", ""),
        approach: string_value(&general, "appr_ident", ""),
        cost_index: trunc_int(number_value(&general, "costindex", 5.0)?),
        airac: string_value(&params, "airac", ""),
        source: "SimBrief".to_owned(),
        alternate: string_value(&alternate, "icao_code", ""),
        block_fuel_kg: number_value(&fuel, "plan_ramp", 0.0)? * kilograms,
        trip_fuel_kg: number_value(&fuel, "enroute_burn", 0.0)? * kilograms,
        reserve_fuel_kg: (number_value(&fuel, "reserve", 0.0)?
            + number_value(&fuel, "contingency", 0.0)?)
            * kilograms,
        alternate_fuel_kg: number_value(&fuel, "alternate_burn", 0.0)? * kilograms,
        taxi_fuel_kg: number_value(&fuel, "taxi", 0.0)? * kilograms,
        passengers: trunc_int(number_value(&weights, "pax_count", 0.0)?),
        payload_kg: number_value(&weights, "payload", 0.0)? * kilograms,
        cargo_kg: number_value(&weights, "cargo", 0.0)? * kilograms,
        zero_fuel_weight_kg: number_value(&weights, "est_zfw", 0.0)? * kilograms,
        takeoff_weight_kg: number_value(&weights, "est_tow", 0.0)? * kilograms,
        landing_weight_kg: number_value(&weights, "est_ldw", 0.0)? * kilograms,
        estimated_minutes: number_value(&section(document, "times"), "est_time_enroute", 0.0)?
            / 60.0,
        fixes,
        ..Default::default()
    };
    validate_flight_plan(&plan)?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures() -> serde_json::Value {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/simbrief.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn documents_match_shared_fixtures() {
        let cases = fixtures();
        let documents = cases["documents"].as_array().unwrap();
        assert!(!documents.is_empty(), "simbrief fixtures present");
        for case in documents {
            let name = case["name"].as_str().unwrap_or("");
            let result = parse_simbrief(&case["document"]);
            if let Some(error) = case.get("error").and_then(serde_json::Value::as_str) {
                assert_eq!(result.unwrap_err(), error, "error {name}");
                continue;
            }
            let plan = result.unwrap();
            let want = &case["expected"];
            for (key, target) in [
                ("departure", &plan.departure),
                ("destination", &plan.destination),
                ("runway", &plan.runway),
                ("arrivalRunway", &plan.arrival_runway),
                ("route", &plan.route),
                ("callsign", &plan.callsign),
                ("registration", &plan.registration),
                ("aircraft", &plan.aircraft),
                ("sid", &plan.sid),
                ("star", &plan.star),
                ("approach", &plan.approach),
                ("alternate", &plan.alternate),
                ("source", &plan.source),
            ] {
                if let Some(value) = want.get(key).and_then(serde_json::Value::as_str) {
                    assert_eq!(target, value, "{key} {name}");
                }
            }
            for (key, target) in [
                ("cruiseFeet", plan.cruise_feet),
                ("costIndex", plan.cost_index),
                ("passengers", plan.passengers),
            ] {
                if let Some(value) = want.get(key).and_then(serde_json::Value::as_i64) {
                    assert_eq!(target, i32::try_from(value).unwrap_or(0), "{key} {name}");
                }
            }
            for (key, target) in [
                ("blockFuelKg", plan.block_fuel_kg),
                ("tripFuelKg", plan.trip_fuel_kg),
                ("reserveFuelKg", plan.reserve_fuel_kg),
            ] {
                if let Some(value) = want.get(key).and_then(serde_json::Value::as_f64) {
                    assert!((target - value).abs() < 1e-6, "{key} {name}");
                }
            }
            if let Some(fixes) = want.get("fixes").and_then(serde_json::Value::as_array) {
                assert_eq!(plan.fixes.len(), fixes.len(), "fix count {name}");
                for (got, want_fix) in plan.fixes.iter().zip(fixes) {
                    assert_eq!(
                        got.identifier,
                        want_fix.as_str().unwrap_or(""),
                        "fix ident {name}"
                    );
                }
            }
        }
    }
}
