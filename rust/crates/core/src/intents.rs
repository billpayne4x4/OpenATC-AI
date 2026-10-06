//! Port of the C++ request interpretation (`core.cpp`): text intents plus the
//! small pure gates (crew role, transmit permission, discipline preset,
//! speech worth).
//!
//! Differential-tested against C++ on `tests/fixtures/intents.json`.

/// Catalogue of request buttons, mirroring C++ `requestDefinitions`.
/// Only titles matter here (intent resolution); UI grouping lives in C++.
const DEFINITIONS: [(&str, &str); 20] = [
    ("clearance", "request ifr clearance"),
    ("pushback", "request pushback"),
    ("taxi", "request taxi"),
    ("ready", "ready for departure"),
    ("gate", "taxi to parking"),
    ("progressive", "repeat taxi instructions"),
    ("radio_check", "radio check"),
    ("readback", "read back clearance"),
    ("repeat", "say again"),
    ("standby", "stand by"),
    ("unable", "unable"),
    ("frequency", "request frequency change"),
    ("position", "confirm position"),
    ("altitude", "request altitude..."),
    ("direct", "request direct-to..."),
    ("descent", "request descent..."),
    ("cancel_ifr", "cancel ifr"),
    ("approach", "brief arrival approach"),
    ("go_around", "going around"),
    ("emergency", "declare emergency"),
];

/// Parsed request: intent plus extracted slots.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Interpreted {
    /// Intent key (`altitude`, `conversation`, …).
    pub intent: String,
    /// Altitude in feet for altitude/descent intents.
    pub altitude_feet: i32,
    /// Waypoint for direct intents, uppercased.
    pub waypoint: String,
}

/// Parse pilot text. Mirrors C++ `interpretText` rule for rule, including
/// evaluation order (exacts, emergency substring, altitude/direct regexes,
/// catalogue titles) and the meters-to-feet rounding.
#[must_use]
pub fn interpret_text(text: &str) -> Interpreted {
    let normalized = text.to_ascii_lowercase();
    let mut request = Interpreted {
        intent: "conversation".to_owned(),
        ..Default::default()
    };
    match normalized.as_str() {
        "radio check" => "radio_check".clone_into(&mut request.intent),
        "say again" | "repeat" => "repeat".clone_into(&mut request.intent),
        "unable" => "unable".clone_into(&mut request.intent),
        "stand by" => "standby".clone_into(&mut request.intent),
        "request clearance" | "request ifr clearance" => {
            "clearance".clone_into(&mut request.intent);
        }
        "request taxi" => "taxi".clone_into(&mut request.intent),
        "request pushback" => "pushback".clone_into(&mut request.intent),
        "ready for departure" => "ready".clone_into(&mut request.intent),
        "going around" | "go around" => "go_around".clone_into(&mut request.intent),
        "cancel ifr" => "cancel_ifr".clone_into(&mut request.intent),
        "request altimeter" => "altimeter".clone_into(&mut request.intent),
        "request weather" => "weather".clone_into(&mut request.intent),
        _ => {}
    }
    if normalized.contains("mayday")
        || normalized.contains("pan pan")
        || normalized == "emergency"
        || normalized == "declare emergency"
    {
        "emergency".clone_into(&mut request.intent);
    }
    let altitude =
        regex::Regex::new(r"(?:request )?(?:altitude |climb |descend |descent )(?:to )?(fl\s*)?([0-9]{2,5})(?: feet| meters)?")
            .ok();
    if let Some(expression) = altitude
        && let Some(found) = expression.captures(&normalized)
        && let Some(digits) = found.get(2)
        && let Ok(value) = digits.as_str().parse::<i32>()
    {
        let fl = found.get(1).is_some();
        if normalized.contains("desc") {
            "descent".clone_into(&mut request.intent);
        } else {
            "altitude".clone_into(&mut request.intent);
        }
        request.altitude_feet = value * if fl { 100 } else { 1 };
        if normalized.contains("meter") && !fl {
            request.altitude_feet = super::meters_to_feet(request.altitude_feet);
        }
    }
    let direct = regex::Regex::new(r"(?:request )?direct(?: to)? ([a-z0-9]{2,10})").ok();
    if let Some(expression) = direct
        && let Some(found) = expression.captures(&normalized)
        && let Some(waypoint) = found.get(1)
    {
        "direct".clone_into(&mut request.intent);
        request.waypoint = waypoint.as_str().to_ascii_uppercase();
    }
    if request.intent == "conversation" {
        for (intent, title) in DEFINITIONS {
            if title == normalized {
                intent.clone_into(&mut request.intent);
                break;
            }
        }
    }
    request
}

/// Resolve the crew recipient. Mirrors C++ `resolveCrewRole`.
#[must_use]
pub fn resolve_crew_role(attendant_active: bool, ground_active: bool) -> String {
    if ground_active {
        "ground".to_owned()
    } else if attendant_active {
        "cabin".to_owned()
    } else {
        "atc".to_owned()
    }
}

/// Transmit permission for a role under power state. Mirrors C++ `canTransmit`.
#[must_use]
pub fn can_transmit(role: &str, radio_power: bool, bus_power: bool) -> bool {
    if role == "copilot" {
        return true;
    }
    if role == "cabin" || role == "ground" {
        return bus_power;
    }
    radio_power
}

/// Discipline toggles, mirroring C++ `Realism` field-for-field (including the
/// flat-bool layout) so preset matching agrees exactly.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Realism {
    /// Verbatim readback required.
    pub strict_readbacks: bool,
    /// Tuned frequency required.
    pub require_frequency: bool,
    /// Callsign required.
    pub require_callsign: bool,
    /// Standard phraseology required.
    pub strict_phraseology: bool,
    /// Corrections explain the rule.
    pub teaching_corrections: bool,
    /// Mayday practice flows enabled.
    pub practice_emergencies: bool,
}

/// Preset name for a toggle state; anything else is custom.
/// Mirrors C++ `disciplinePreset`.
#[must_use]
pub fn discipline_preset(realism: Realism, congestion: &str) -> String {
    let quiet = !realism.strict_readbacks
        && !realism.require_frequency
        && !realism.require_callsign
        && !realism.strict_phraseology
        && !realism.teaching_corrections
        && !realism.practice_emergencies
        && congestion == "off";
    if quiet {
        return "relaxed".to_owned();
    }
    let standard = realism.strict_readbacks
        && !realism.require_frequency
        && !realism.require_callsign
        && realism.strict_phraseology
        && realism.teaching_corrections
        && !realism.practice_emergencies
        && congestion == "quiet";
    if standard {
        return "standard".to_owned();
    }
    let real = realism.strict_readbacks
        && realism.require_frequency
        && realism.require_callsign
        && realism.strict_phraseology
        && !realism.teaching_corrections
        && congestion == "busy"
        && realism.practice_emergencies;
    if real {
        return "real".to_owned();
    }
    "custom".to_owned()
}

/// Whether a PTT recording is worth transmitting. Mirrors C++ `speechWorthSending`.
#[must_use]
pub fn speech_worth_sending(seconds: f64, peak_level: f64, text: &str) -> bool {
    if seconds < 0.5 || peak_level < 0.02 {
        return false;
    }
    let trimmed = text.trim_matches([' ', '\t', '\r', '\n', '"', '\'']);
    !trimmed.is_empty() && trimmed != "[BLANK_AUDIO]"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn matches_shared_fixtures() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/intents.json");
        let cases: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut count = 0;
        for case in cases["interpret"].as_array().unwrap() {
            let request = interpret_text(case["text"].as_str().unwrap());
            assert_eq!(
                request.intent,
                case["intent"].as_str().unwrap(),
                "intent for {}",
                case["text"].as_str().unwrap()
            );
            assert_eq!(
                request.altitude_feet,
                i32::try_from(case["altitudeFeet"].as_i64().unwrap()).unwrap(),
                "altitude for {}",
                case["text"].as_str().unwrap()
            );
            assert_eq!(
                request.waypoint,
                case["waypoint"].as_str().unwrap(),
                "waypoint for {}",
                case["text"].as_str().unwrap()
            );
            count += 1;
        }
        assert_eq!(count, 21, "interpret cases run");
        for case in cases["roles"].as_array().unwrap() {
            assert_eq!(
                resolve_crew_role(
                    case["attendant"].as_bool().unwrap(),
                    case["ground"].as_bool().unwrap()
                ),
                case["expected"].as_str().unwrap()
            );
            count += 1;
        }
        for case in cases["gates"].as_array().unwrap() {
            assert_eq!(
                can_transmit(
                    case["role"].as_str().unwrap(),
                    case["radio"].as_bool().unwrap(),
                    case["bus"].as_bool().unwrap()
                ),
                case["expected"].as_bool().unwrap()
            );
            count += 1;
        }
        for case in cases["presets"].as_array().unwrap() {
            let realism = Realism {
                strict_readbacks: case["strictReadbacks"].as_bool().unwrap(),
                require_frequency: case["requireFrequency"].as_bool().unwrap(),
                require_callsign: case["requireCallsign"].as_bool().unwrap(),
                strict_phraseology: case["strictPhraseology"].as_bool().unwrap(),
                teaching_corrections: case["teachingCorrections"].as_bool().unwrap(),
                practice_emergencies: case["practiceEmergencies"].as_bool().unwrap(),
            };
            assert_eq!(
                discipline_preset(realism, case["congestion"].as_str().unwrap()),
                case["expected"].as_str().unwrap()
            );
            count += 1;
        }
        for case in cases["worthSending"].as_array().unwrap() {
            assert_eq!(
                speech_worth_sending(
                    case["seconds"].as_f64().unwrap(),
                    case["peak"].as_f64().unwrap(),
                    case["text"].as_str().unwrap()
                ),
                case["expected"].as_bool().unwrap()
            );
            count += 1;
        }
        assert_eq!(count, 39, "all fixtures run");
    }
}
