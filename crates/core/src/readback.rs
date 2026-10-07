//! Conservative spoken readback matching; never repairs incorrect operational values.
use crate::state::{Request, State};
fn word(token: &str) -> &str {
    match token {
        "alpha" | "alfa" => "a",
        "bravo" => "b",
        "charlie" => "c",
        "delta" => "d",
        "echo" => "e",
        "foxtrot" => "f",
        "golf" => "g",
        "hotel" => "h",
        "india" => "i",
        "juliet" | "juliett" => "j",
        "kilo" => "k",
        "lima" => "l",
        "mike" => "m",
        "november" => "n",
        "oscar" => "o",
        "papa" => "p",
        "quebec" => "q",
        "romeo" => "r",
        "sierra" => "s",
        "tango" => "t",
        "uniform" => "u",
        "victor" => "v",
        "whiskey" => "w",
        "xray" => "x",
        "yankee" => "y",
        "zulu" => "z",
        "zero" | "oh" => "0",
        "one" => "1",
        "two" => "2",
        "three" | "tree" => "3",
        "four" | "fower" => "4",
        "five" | "fife" => "5",
        "six" => "6",
        "seven" => "7",
        "eight" => "8",
        "nine" | "niner" => "9",
        _ => token,
    }
}
fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
fn compact(text: &str) -> String {
    tokens(text).iter().map(|s| word(s)).collect()
}
fn altitude(text: &str, expected: i32) -> bool {
    let t = tokens(text);
    for end in 0..t.len() {
        if t[end] != "feet" && t[end] != "foot" {
            continue;
        }
        let mut start = end;
        while start > 0
            && (word(&t[start - 1]).chars().all(|c| c.is_ascii_digit())
                || matches!(t[start - 1].as_str(), "thousand" | "hundred"))
        {
            start -= 1;
        }
        let mut value = 0;
        let mut run = String::new();
        for token in &t[start..end] {
            if token == "thousand" || token == "hundred" {
                value +=
                    run.parse::<i32>().unwrap_or(0) * if token == "thousand" { 1000 } else { 100 };
                run.clear();
            } else {
                run.push_str(word(token));
            }
        }
        value += run.parse::<i32>().unwrap_or(0);
        if value == expected {
            return true;
        }
    }
    false
}
/// Recognize pending spoken readbacks before general conversation classification.
/// A structured readback button already supplies an explicit clearance sequence.
pub fn prepare(state: &State, request: &mut Request) {
    if request.clearance_sequence != 0 {
        return;
    }
    let taxi = state.taxi_clearance.pending_readback;
    let clearance = state.clearance.as_ref();
    if !taxi && clearance.is_none_or(|c| c.acknowledged) {
        return;
    }
    let text = compact(&request.text);
    // The generic parser can interpret "initial altitude 5000" as an altitude
    // request. Pending readback evidence takes priority over that classification.
    let evidence = if taxi {
        text.contains("holdshort") || text.contains("taxivia")
    } else {
        [
            "cleared",
            "clearto",
            "route",
            "initialaltitude",
            "squawk",
            "feet",
            "meters",
        ]
        .iter()
        .filter(|word| text.contains(**word))
        .count()
            >= 2
    };
    if request.intent != "readback" && !evidence {
        return;
    }
    let spoken = compact(&request.text);
    let mut missing = Vec::new();
    if !spoken.contains(&compact(&state.plan.callsign)) {
        missing.push(crate::dialogue::say("readback_field_callsign", &[]));
    }
    if taxi {
        let hold = if state.taxi_clearance.hold_short_runway.is_empty() {
            &state.taxi_clearance.destination
        } else {
            &state.taxi_clearance.hold_short_runway
        };
        if !spoken.contains(&compact(hold)) {
            missing.push(crate::dialogue::say(
                "readback_field_assigned_runway_stand",
                &[],
            ));
        }
        if state.taxi_clearance.runway_taxi
            && (!spoken.contains("backtrack") || !spoken.contains("holdposition"))
        {
            missing.push(crate::dialogue::say("readback_field_runway_taxi", &[]));
        }
        if !state.taxi_clearance.hold_short_runway.is_empty() && !spoken.contains("holdshort") {
            missing.push(crate::dialogue::say(
                "readback_field_hold_short_instruction",
                &[],
            ));
        }
        // Validate structured route facts, never reverse-parse editable speech.
        for name in state
            .taxi_clearance
            .via
            .split_whitespace()
            .filter(|s| !matches!(*s, "and" | "then"))
        {
            if !spoken.contains(&compact(name)) {
                missing.push(crate::dialogue::say("readback_field_taxiway_route", &[]));
                break;
            }
        }
        request.clearance_sequence = state.taxi_clearance.sequence;
        request
            .waypoint
            .clone_from(&state.taxi_clearance.instructions);
    } else {
        let Some(c) = clearance else {
            return;
        };
        if !spoken.contains(&compact(&state.plan.destination)) {
            missing.push(crate::dialogue::say("readback_field_destination", &[]));
        }
        if !c
            .route
            .split_whitespace()
            .filter(|s| *s != "DCT")
            .all(|s| spoken.contains(&compact(s)))
        {
            missing.push(crate::dialogue::say("readback_field_route", &[]));
        }
        if !altitude(&request.text, c.altitude_feet) {
            missing.push(crate::dialogue::say("readback_field_altitude", &[]));
        }
        if !spoken.contains(&format!("squawk{}", c.squawk)) {
            missing.push(crate::dialogue::say("readback_field_squawk", &[]));
        }
        if !spoken.contains(&format!("runway{}", compact(&c.runway))) {
            missing.push(crate::dialogue::say("readback_field_runway", &[]));
        }
        request.clearance_sequence = c.sequence;
        request.altitude_feet = c.altitude_feet;
        request.waypoint.clone_from(&c.route);
    }
    "readback".clone_into(&mut request.intent);
    if !missing.is_empty() {
        request.waypoint = "READBACK ERROR:".to_owned()
            + &crate::dialogue::say(
                "readback_error_say_again_your_readback_i",
                &[
                    (
                        "clearance_kind",
                        (if taxi { "taxi" } else { "IFR" }).to_string(),
                    ),
                    ("missing_items", (missing.join(", ")).clone()),
                ],
            );
    }
}

/// Construct the pilot's complete reply from the active clearance, never radio chatter.
#[must_use]
pub fn auto_reply(state: &State) -> Option<Request> {
    let last = state
        .transcript
        .iter()
        .rev()
        .find(|entry| entry.speaker == "ATC" && !entry.background)?;
    if last.position.is_empty() {
        return None;
    }
    let mut request = Request {
        intent: "readback".to_owned(),
        ..Default::default()
    };
    if state.taxi_clearance.pending_readback {
        let taxi = &state.taxi_clearance;
        "taxi".clone_into(&mut request.readback_kind);
        request.clearance_sequence = taxi.sequence;
        request.waypoint.clone_from(&taxi.instructions);
        request.text = if taxi.runway_taxi {
            crate::dialogue::say(
                "pilot_runway_taxi_readback",
                &[
                    ("entry_runway", taxi.entry_runway.clone()),
                    ("runway", taxi.destination.clone()),
                    ("callsign", state.plan.callsign.clone()),
                ],
            )
        } else if taxi.via.is_empty() && !taxi.hold_short_runway.is_empty() {
            crate::dialogue::say(
                "pilot_taxi_holding_point_readback",
                &[
                    ("runway", taxi.hold_short_runway.clone()),
                    ("callsign", state.plan.callsign.clone()),
                ],
            )
        } else if !taxi.hold_short_runway.is_empty() {
            crate::dialogue::say(
                "pilot_taxi_hold_readback",
                &[
                    ("via", taxi.via.clone()),
                    ("runway", taxi.hold_short_runway.clone()),
                    ("callsign", state.plan.callsign.clone()),
                ],
            )
        } else {
            crate::dialogue::say(
                "pilot_taxi_readback",
                &[
                    ("instructions", taxi.instructions.clone()),
                    ("callsign", state.plan.callsign.clone()),
                ],
            )
        };
    } else {
        let Some(clearance) = state.clearance.as_ref().filter(|c| !c.acknowledged) else {
            if last.pilot_reply.is_empty() {
                return None;
            }
            return Some(Request {
                intent: "acknowledge".to_owned(),
                text: last.pilot_reply.clone(),
                clearance_sequence: last.sequence,
                ..Default::default()
            });
        };
        request.clearance_sequence = clearance.sequence;
        "ifr".clone_into(&mut request.readback_kind);
        request.altitude_feet = clearance.altitude_feet;
        request.waypoint.clone_from(&clearance.route);
        request.text = crate::dialogue::say(
            "pilot_ifr_readback",
            &[
                ("destination", state.plan.destination.clone()),
                ("route", clearance.route.clone()),
                ("altitude", clearance.altitude_feet.to_string()),
                ("squawk", clearance.squawk.clone()),
                ("runway", clearance.runway.clone()),
                ("callsign", state.plan.callsign.clone()),
            ],
        );
    }
    Some(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_wrong_altitude_and_matches_phonetics() {
        let mut s = State::default();
        s.plan.callsign = "C-GTLT".into();
        s.plan.destination = "VTBS".into();
        s.clearance = Some(crate::state::Clearance {
            altitude_feet: 5000,
            route: "CHU1C CMP W21 NOBER NORT2D".into(),
            runway: "31".into(),
            squawk: "2105".into(),
            sequence: 2,
            ..Default::default()
        });
        let text = "Charlie Golf Tango Lima Tango cleared Victor Tango Bravo Sierra CHU1C CMP W21 NOBER NORT2D five thousand feet squawk two one zero five runway three one";
        let mut r = Request {
            text: text.into(),
            intent: "conversation".into(),
            ..Default::default()
        };
        prepare(&s, &mut r);
        assert_eq!(r.intent, "readback");
        assert!(!r.waypoint.starts_with("READBACK ERROR"));
        let mut wrong = Request {
            text: text.replace("five thousand", "five hundred"),
            intent: "conversation".into(),
            ..Default::default()
        };
        prepare(&s, &mut wrong);
        assert!(wrong.waypoint.contains("altitude"));
    }
    #[test]
    fn typed_clearance_and_garbled_readback_override_generic_parser() {
        let mut state = State::default();
        state.plan.callsign = "C-GTLT".into();
        state.plan.destination = "VTBS".into();
        state.clearance = Some(crate::state::Clearance {
            altitude_feet: 5000,
            route: "CHU1C CMP W21 NOBER NORT2D".into(),
            runway: "31".into(),
            squawk: "2105".into(),
            sequence: 2,
            ..Default::default()
        });
        let typed = "Cleared to VTBS, route CHU1C CMP W21 NOBER NORT2D, initial altitude 5000 feet, squawk 2105, Runway 31, C-GTLT";
        let parsed = crate::identify::interpret_text(typed);
        let mut request = Request {
            intent: parsed.intent,
            altitude_feet: parsed.altitude_feet,
            waypoint: parsed.waypoint,
            text: typed.into(),
            ..Default::default()
        };
        assert_ne!(
            request.intent, "readback",
            "reproduce the generic-parser conflict"
        );
        prepare(&state, &mut request);
        assert_eq!(request.intent, "readback");
        assert_eq!(request.waypoint, state.clearance.as_ref().unwrap().route);
        let garbled = "Charlie Gough, 10-go, clear to 30 BS, route 21 CCMP, W21, no bird, nor 2D, initial altitude 5000 feet, or 2.105 runway 31 foot departure.";
        let parsed = crate::identify::interpret_text(garbled);
        let mut request = Request {
            intent: parsed.intent,
            altitude_feet: parsed.altitude_feet,
            waypoint: parsed.waypoint,
            text: garbled.into(),
            ..Default::default()
        };
        prepare(&state, &mut request);
        assert_eq!(request.intent, "readback");
        assert!(request.waypoint.starts_with("READBACK ERROR:"));
        assert!(request.waypoint.contains("callsign"));
        let mut taxi = Request {
            intent: "conversation".into(),
            text: "Your Quest Taxi 2 runway 31".into(),
            ..Default::default()
        };
        prepare(&state, &mut taxi);
        assert_eq!(taxi.intent, "conversation", "taxi is not a readback");
    }
}
