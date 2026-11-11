//! Editable deterministic operational speech, separate from AI example selection.
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Template {
    slots: Vec<String>,
    say: Vec<String>,
    #[serde(default)]
    match_any: Vec<String>,
    #[serde(default)]
    match_priority: i32,
    #[serde(default)]
    humour: bool,
    #[serde(default)]
    fallback: bool,
    #[serde(default)]
    when: BTreeMap<String, serde_json::Value>,
}
type Templates = BTreeMap<String, Template>;

/// Select a conversation response using editable matching and humour rules.
#[must_use]
pub fn conversation_response(prefix: &str, text: &str, humour: bool) -> Option<String> {
    matched_response(prefix, text, humour, &BTreeMap::new()).map(|key| say(&key, &[]))
}

/// Find a response whose editable conditions match observed facts.
///
/// # Panics
///
/// Panics if the built-in dialogue template set cannot be parsed.
#[must_use]
pub fn matched_response(
    prefix: &str,
    text: &str,
    humour: bool,
    facts: &BTreeMap<String, serde_json::Value>,
) -> Option<String> {
    let lock =
        TEMPLATES.get_or_init(|| RwLock::new(parse(DEFAULTS).expect("shipped dialogue TOML")));
    let templates = lock.read().ok()?;
    select_response(&templates, prefix, text, humour, facts)
}

fn select_response(
    templates: &Templates,
    prefix: &str,
    text: &str,
    humour: bool,
    facts: &BTreeMap<String, serde_json::Value>,
) -> Option<String> {
    let words = |value: &str| {
        format!(
            " {} ",
            value
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|word| !word.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let text = words(text);
    templates
        .iter()
        .filter(|(key, template)| {
            key.starts_with(prefix)
                && (!template.humour || humour)
                && template.when.iter().all(|(fact, condition)| {
                    facts
                        .get(fact)
                        .is_some_and(|value| condition_matches(condition, value))
                })
        })
        .filter(|(_, template)| {
            template.fallback
                || (template.match_any.is_empty() && !template.when.is_empty())
                || template
                    .match_any
                    .iter()
                    .any(|phrase| !phrase.trim().is_empty() && text.contains(&words(phrase)))
        })
        .max_by_key(|(_, template)| (!template.fallback, template.match_priority))
        .map(|(key, _)| key.clone())
}

fn condition_matches(condition: &serde_json::Value, value: &serde_json::Value) -> bool {
    if let Some(options) = condition.as_array() {
        return options.contains(value);
    }
    if let Some(range) = condition.as_object() {
        let Some(number) = value.as_f64() else {
            return false;
        };
        return !range.is_empty()
            && range.iter().all(|(key, bound)| {
                bound.as_f64().is_some_and(|bound| match key.as_str() {
                    "min" => number >= bound,
                    "max" => number <= bound,
                    _ => false,
                })
            });
    }
    condition == value
}
const DEFAULTS: &str = include_str!("../../../speech/runtime/responses.toml");
static EDITED: AtomicBool = AtomicBool::new(false);
static PHRASE_CHOICES: OnceLock<Mutex<BTreeMap<String, usize>>> = OnceLock::new();
static PLACEHOLDERS: OnceLock<regex::Regex> = OnceLock::new();
static TEMPLATES: OnceLock<RwLock<Templates>> = OnceLock::new();

fn parse(text: &str) -> Result<Templates, String> {
    let templates: Templates = crate::configuration::from_str(text).map_err(|e| e.to_string())?;
    let placeholders = regex::Regex::new(r"\{([a-z_0-9]+)\}").map_err(|e| e.to_string())?;
    for (id, entry) in &templates {
        if entry.when.len() > 16
            || entry.match_any.len() > 32
            || entry
                .match_any
                .iter()
                .any(|phrase| phrase.trim().is_empty() || phrase.len() > 160)
            || entry.when.values().any(|condition| {
                condition.as_object().is_some_and(|range| {
                    range.is_empty()
                        || range.iter().any(|(key, value)| {
                            !["min", "max"].contains(&key.as_str()) || value.as_f64().is_none()
                        })
                }) || condition.as_array().is_some_and(Vec::is_empty)
            })
        {
            return Err(format!(
                "{id}: invalid or excessive response matching conditions"
            ));
        }
        if entry.say.is_empty() || entry.say.iter().any(|s| s.trim().is_empty()) {
            return Err(format!("{id}: say must contain non-empty phrases"));
        }
        for line in &entry.say {
            let used: std::collections::BTreeSet<_> = placeholders
                .captures_iter(line)
                .map(|c| c[1].to_owned())
                .collect();
            let declared: std::collections::BTreeSet<_> = entry.slots.iter().cloned().collect();
            if used != declared || placeholders.replace_all(line, "").contains(['{', '}']) {
                return Err(format!(
                    "{id}: each phrase must use exactly the declared placeholders"
                ));
            }
        }
    }
    Ok(templates)
}

/// Validate all required response IDs and slot contracts against the shipped TOML.
pub fn validate(path: &Path) -> Result<(), String> {
    let edited =
        parse(&std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?)?;
    let defaults = parse(DEFAULTS)?;
    for (id, required) in defaults {
        let Some(entry) = edited.get(&id) else {
            return Err(format!("missing response {id}"));
        };
        if entry.slots != required.slots {
            return Err(format!("{id}: required placeholders changed"));
        }
    }
    Ok(())
}

/// Load edits at engine startup; invalid libraries cannot silently fall back to code wording.
pub fn load(path: &Path) -> Result<(), String> {
    validate(path)?;
    let edited = parse(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)?;
    let lock = TEMPLATES.get_or_init(|| RwLock::new(BTreeMap::new()));
    *lock.write().map_err(|e| e.to_string())? = edited;
    EDITED.store(true, Ordering::Relaxed);
    Ok(())
}

fn next_phrase(choices: &mut BTreeMap<String, usize>, id: &str, count: usize) -> usize {
    let next = choices.entry(id.to_owned()).or_default();
    let pick = *next % count;
    *next = pick + 1;
    pick
}

/// Render one operational response; only the caller supplies operational facts.
///
/// # Panics
/// Panics only if the compiled-in TOML or placeholder pattern is invalid.
#[must_use]
pub fn say(id: &str, slots: &[(&str, String)]) -> String {
    let lock =
        TEMPLATES.get_or_init(|| RwLock::new(parse(DEFAULTS).expect("shipped dialogue TOML")));
    let Ok(templates) = lock.read() else {
        return String::new();
    };
    let Some(entry) = templates.get(id) else {
        return String::new();
    };
    let pick = if EDITED.load(Ordering::Relaxed) {
        let mut choices = PHRASE_CHOICES
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .expect("phrase choices");
        next_phrase(&mut choices, id, entry.say.len())
    } else {
        0
    };
    let Some(line) = entry.say.get(pick) else {
        return String::new();
    };
    if entry
        .slots
        .iter()
        .any(|name| !slots.iter().any(|(key, _)| key == name))
    {
        return String::new();
    }
    PLACEHOLDERS
        .get_or_init(|| regex::Regex::new(r"\{([a-z_0-9]+)\}").expect("placeholder pattern"))
        .replace_all(line, |captures: &regex::Captures<'_>| {
            slots
                .iter()
                .find(|(key, _)| *key == &captures[1])
                .map_or_else(String::new, |(_, value)| value.clone())
        })
        .into_owned()
}

/// All editable phrases for request matching or prompt examples.
///
/// # Panics
///
/// Panics if the built-in dialogue template set cannot be parsed.
#[must_use]
pub fn phrases(id: &str) -> Vec<String> {
    let lock =
        TEMPLATES.get_or_init(|| RwLock::new(parse(DEFAULTS).expect("shipped dialogue TOML")));
    lock.read()
        .ok()
        .and_then(|templates| templates.get(id).map(|entry| entry.say.clone()))
        .unwrap_or_default()
}

/// Normalised, space-delimited words used for deterministic clause classification.
fn clause_words(text: &str) -> String {
    format!(
        " {} ",
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    )
}

const CONTROL_CLAUSES: &[&str] = &[
    "taxi",
    "backtrack",
    "start",
    "start up",
    "pushback",
    "engine",
    "clearance",
    "cleared",
    "runway",
    "takeoff",
    "take off",
    "departure",
    "approach",
    "land",
    "landing",
    "altitude",
    "climb",
    "descend",
    "descent",
    "heading",
    "turn",
    "direct",
    "frequency",
    "squawk",
    "hold short",
    "cross",
    "crossing",
    "stand",
    "gate",
    "parking",
    "readback",
    "read back",
    "ready",
    "line up",
    "go around",
    "missed",
    "emergency",
    "mayday",
    "pan pan",
    "ifr",
    "vfr",
];

const SOCIAL_CLAUSES: &[&str] = &[
    "hello",
    "hi",
    "hey",
    "thanks",
    "thank you",
    "good morning",
    "good afternoon",
    "good evening",
    "good day",
    "goodbye",
    "bye",
    "cheers",
    "please",
    "welcome",
    "how are you",
    "joke",
    "chat",
    "lovely",
    "nice",
    "great",
    "sorry",
    "see you",
];

fn contains_clause(words: &str, clauses: &[&str]) -> bool {
    clauses
        .iter()
        .any(|clause| words.contains(&format!(" {clause} ")))
}

/// True when the transmission contains a clause that asks for or grants aircraft movement.
#[must_use]
pub fn has_control_clause(text: &str) -> bool {
    contains_clause(&clause_words(text), CONTROL_CLAUSES)
}

/// True when the transmission is purely social: it has a greeting or pleasantry and no control
/// clause. Pure banter must never be interpreted as a request for movement.
#[must_use]
pub fn social_only(text: &str) -> bool {
    let words = clause_words(text);
    contains_clause(&words, SOCIAL_CLAUSES) && !contains_clause(&words, CONTROL_CLAUSES)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editable_matching_conditions_priorities_and_humour_select_responses() {
        let templates = parse("[chat_default]\nslots=[]\nsay=['Hello']\nfallback=true\n[chat_cuppa]\nslots=[]\nsay=['Thanks for the cuppa']\nmatch_any=['cuppa']\nmatch_priority=20\nhumour=true\nwhen={phase=['cruise'],temperature={min=10,max=30}}\n").unwrap();
        let facts = BTreeMap::from([
            ("phase".into(), serde_json::json!("cruise")),
            ("temperature".into(), serde_json::json!(20)),
        ]);
        assert_eq!(
            select_response(&templates, "chat_", "Fancy a cuppa?", true, &facts).as_deref(),
            Some("chat_cuppa")
        );
        assert_eq!(
            select_response(&templates, "chat_", "Fancy a cuppa?", false, &facts).as_deref(),
            Some("chat_default")
        );
        assert_eq!(
            select_response(
                &templates,
                "chat_",
                "Fancy a cuppa?",
                true,
                &BTreeMap::new()
            )
            .as_deref(),
            Some("chat_default")
        );
        assert_eq!(
            select_response(&templates, "chat_", "cuppaboard", true, &facts).as_deref(),
            Some("chat_default")
        );
        assert!(
            parse("[chat]\nslots=[]\nsay=['Hello']\nwhen={phase={execute='anything'}}").is_err()
        );
        // Social pleasantries are recognised, but any movement clause keeps a transmission
        // out of the social-only class.
        assert!(social_only("Thanks, have a good day"));
        assert!(social_only("Hello there, how are you?"));
        assert!(!social_only("Thanks, request taxi via A"));
        assert!(!social_only("Good morning, cleared to land runway 27"));
        assert!(has_control_clause("Thanks, request taxi via A"));
        assert!(has_control_clause("ready for departure"));
        assert!(!has_control_clause("Thanks, have a good day"));
    }
    #[test]
    fn unrelated_responses_do_not_change_phrase_rotation() {
        let mut choices = BTreeMap::new();
        assert_eq!(next_phrase(&mut choices, "clearance", 3), 0);
        for _ in 0..5 {
            next_phrase(&mut choices, "radio_check", 4);
        }
        assert_eq!(next_phrase(&mut choices, "clearance", 3), 1);
        assert_eq!(next_phrase(&mut choices, "clearance", 3), 2);
        assert_eq!(next_phrase(&mut choices, "clearance", 3), 0);
    }
    #[test]
    fn shipped_templates_validate_and_reject_missing_facts() {
        let templates = parse(DEFAULTS).unwrap();
        assert!(templates.len() >= 120);
        assert!(parse("[example]\nslots=['runway']\nsay=['Taxi approved.']").is_err());
        assert!(
            parse("[example]\nslots=['runway']\nsay=['Hold short of {runway}.','Hold short.']")
                .is_err()
        );
        assert!(parse("[example]\nslots=[]\nsay=['{unknown}']").is_err());
    }
    #[test]
    fn ambient_traffic_chatter_and_hazard_phrases_render_from_the_shipped_toml() {
        let callsign = "N123AB";
        let runway = "27";
        let station = "Brisbane Ground";
        let via = "Alpha, Bravo";
        let simple = [
            ("callsign", callsign.to_owned()),
            ("runway", runway.to_owned()),
        ];
        let ready = [
            ("station", station.to_owned()),
            ("callsign", callsign.to_owned()),
        ];
        let taxi = [
            ("callsign", callsign.to_owned()),
            ("runway", runway.to_owned()),
            ("via", via.to_owned()),
        ];
        let final_call = [
            ("station", station.to_owned()),
            ("callsign", callsign.to_owned()),
            ("runway", runway.to_owned()),
        ];
        let handoff = [
            ("callsign", callsign.to_owned()),
            ("runway", runway.to_owned()),
            ("side", "left".to_owned()),
            ("taxiway", "Alpha".to_owned()),
            ("station", station.to_owned()),
            ("frequency", "121.700".to_owned()),
        ];
        let handoff_any = [
            ("callsign", callsign.to_owned()),
            ("runway", runway.to_owned()),
            ("station", station.to_owned()),
            ("frequency", "121.700".to_owned()),
        ];
        let cases: [(&str, &[(&str, String)], &[&str]); 22] = [
            ("ambient_hold_short", &simple, &[callsign, runway]),
            ("ambient_hold_short_readback", &simple, &[callsign, runway]),
            ("ambient_line_up", &simple, &[callsign, runway]),
            ("ambient_line_up_readback", &simple, &[callsign, runway]),
            ("ambient_takeoff", &simple, &[callsign, runway]),
            ("ambient_takeoff_readback", &simple, &[callsign, runway]),
            ("ambient_landing", &simple, &[callsign, runway]),
            ("ambient_landing_readback", &simple, &[callsign, runway]),
            ("chatter_ground_ready", &ready, &[station, callsign]),
            ("chatter_ground_taxi", &taxi, &[callsign, runway, via]),
            (
                "chatter_ground_taxi_readback",
                &taxi,
                &[callsign, runway, via],
            ),
            ("chatter_ground_taxi_direct", &simple, &[callsign, runway]),
            (
                "chatter_ground_taxi_direct_readback",
                &simple,
                &[callsign, runway],
            ),
            ("chatter_tower_departure", &ready, &[station, callsign]),
            ("chatter_tower_lineup", &simple, &[callsign, runway]),
            (
                "chatter_tower_lineup_readback",
                &simple,
                &[callsign, runway],
            ),
            (
                "chatter_tower_final",
                &final_call,
                &[station, callsign, runway],
            ),
            ("chatter_tower_landing", &simple, &[callsign, runway]),
            (
                "chatter_tower_landing_readback",
                &simple,
                &[callsign, runway],
            ),
            ("windshear_advise", &ready, &[callsign, station]),
            (
                "arrival_vacate_contact_ground",
                &handoff,
                &[callsign, runway, "left", "Alpha", station, "121.700"],
            ),
            (
                "arrival_vacate_any_contact_ground",
                &handoff_any,
                &[callsign, runway, station, "121.700"],
            ),
        ];
        for (key, slots, expected) in cases {
            let rendered = say(key, slots);
            assert!(!rendered.is_empty(), "{key} did not render");
            for value in expected {
                assert!(
                    rendered.contains(value),
                    "{key} dropped {value}: {rendered}"
                );
            }
            assert!(
                !rendered.contains('{'),
                "{key} left an unfilled placeholder: {rendered}"
            );
        }
    }

    #[test]
    fn response_values_do_not_interpolate_each_other() {
        let rendered = say(
            "copilot_repeat_instruction",
            &[
                ("instruction", "Keep {callsign} literal".into()),
                ("callsign", "VH-BIL".into()),
            ],
        );
        assert!(rendered.contains("Keep {callsign} literal"));
    }
}

/// Remove decorative emoji before sending a line to speech synthesis.
///
/// # Panics
///
/// Panics if the built-in emoji pattern cannot be compiled.
pub fn speech_text(text: &str) -> String {
    static EMOJI: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let emoji = EMOJI.get_or_init(|| {
        regex::Regex::new(
            r"[\p{Extended_Pictographic}\p{Emoji_Presentation}\x{FE0F}\x{200D}\x{20E3}]",
        )
        .expect("speech decoration pattern")
    });
    emoji.replace_all(text, "").trim().to_owned()
}

#[cfg(test)]
mod speech_text_tests {
    #[test]
    fn removes_decorations_without_losing_names_or_units() {
        assert_eq!(super::speech_text("Nora in Hobart. 😄"), "Nora in Hobart.");
        assert_eq!(
            super::speech_text("Harbour Café, 23°C, ready for departure."),
            "Harbour Café, 23°C, ready for departure."
        );
        assert_eq!(super::speech_text("Ready 👨‍✈️"), "Ready");
    }

    /// Arguments of every call to `marker` (which includes the opening parenthesis),
    /// stopping at its matching close so a scan cannot run into the next function.
    fn call_arguments<'a>(text: &'a str, marker: &str) -> Vec<&'a str> {
        let mut found = Vec::new();
        let mut rest = text;
        while let Some(index) = rest.find(marker) {
            let before = rest[..index].chars().next_back();
            let after = &rest[index + marker.len()..];
            rest = after;
            if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let mut depth = 1usize;
            let mut in_string = false;
            let mut escaped = false;
            let mut end = None;
            for (offset, c) in after.char_indices() {
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        in_string = false;
                    }
                    continue;
                }
                match c {
                    '"' => in_string = true,
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(offset);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(end) = end {
                found.push(&after[..end]);
            }
        }
        found
    }

    /// Phrase keys written as string literals in the Rust sources. `say` takes the key first;
    /// the traffic `atc` helper takes it before the slot list.
    fn literal_keys(text: &str) -> Vec<String> {
        let mut keys = Vec::new();
        for arguments in call_arguments(text, "say(") {
            let trimmed = arguments.trim_start();
            if !trimmed.starts_with('"') {
                continue;
            }
            let quoted = &trimmed[1..];
            if let Some(end) = quoted.find('"') {
                keys.push(quoted[..end].to_owned());
            }
        }
        for arguments in call_arguments(text, "atc(") {
            let head = arguments.split("&[").next().unwrap_or(arguments);
            let Some(start) = head.find('"') else {
                continue;
            };
            let quoted = &head[start + 1..];
            if let Some(end) = quoted.find('"') {
                keys.push(quoted[..end].to_owned());
            }
        }
        keys
    }

    #[test]
    fn every_literal_phrase_key_in_the_sources_has_speech() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut missing = Vec::new();
        let mut scanned = 0;
        let mut pending = vec![manifest.join("src"), manifest.join("../engine/src")];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for key in literal_keys(&text) {
                    if key.is_empty()
                        || !key
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    {
                        continue;
                    }
                    scanned += 1;
                    if super::phrases(&key).is_empty() {
                        missing.push(format!("{}: {key}", path.display()));
                    }
                }
            }
        }
        assert!(scanned > 100, "the scan found only {scanned} phrase keys");
        assert!(
            missing.is_empty(),
            "phrase keys without speech: {missing:?}"
        );
    }
}
