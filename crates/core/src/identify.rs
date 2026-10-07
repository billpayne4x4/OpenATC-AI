//! Public request parser: pilot text, intent and extracted values.

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

/// Parse pilot text into intent plus slots. Delegates to `parse`.
#[must_use]
pub fn interpret_text(text: &str) -> Interpreted {
    super::parse::interpret(text)
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
    }
}
