//! Crew roles, transmission eligibility and realism presets.

/// Resolve the crew recipient.
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

/// Check whether a role can transmit with the current radio power.
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

/// Realism controls, including the
/// flat-bool layout) so preset matching agrees exactly.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug)]
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

impl Default for Realism {
    /// Defaults require strict readbacks and phraseology, with teaching
    /// corrections on; frequency, callsign and emergency practice off.
    fn default() -> Self {
        Self {
            strict_readbacks: true,
            require_frequency: false,
            require_callsign: false,
            strict_phraseology: true,
            teaching_corrections: true,
            practice_emergencies: false,
        }
    }
}

/// Preset name for a toggle state; anything else is custom.
/// Apply a named realism preset.
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
        && realism.require_frequency
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

/// Check whether a PTT recording contains enough speech to transmit.
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
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/intents.json");
        let cases: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut count = 0;
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
        assert_eq!(count, 18, "all fixtures run");
    }
}
