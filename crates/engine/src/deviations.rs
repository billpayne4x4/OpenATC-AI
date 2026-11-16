//! Deviation monitoring, ATC warnings, diversion planning and the optional
//! "number for you to call" exchange.

use crate::EngineState;
use openatc_core::airspace::AirspaceIndex;
use openatc_core::apply::add_transmission;
use openatc_core::deviations::{
    DEFAULT_POLICY, DeviationContext, DeviationRule, Finding, Mode, Policy, Scope, default_rules,
};
use openatc_core::dialogue::say;
use openatc_core::state::SpeechTag;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Deviation monitoring state owned by the engine.
pub struct Manager {
    text: String,
    policies: BTreeMap<String, Policy>,
    rules: Vec<Box<dyn DeviationRule>>,
    airspace: Option<AirspaceIndex>,
    airspace_attempted: bool,
    custom_dirs: Vec<PathBuf>,
    strikes: BTreeMap<&'static str, u8>,
    last: BTreeMap<&'static str, Instant>,
    recovery: Option<String>,
    ground_pending: bool,
    ground_done: bool,
    divert_pending: bool,
    diverted: bool,
    number_sent: bool,
    generation: u64,
}

impl Default for Manager {
    fn default() -> Self {
        Self::from_text(DEFAULT_POLICY.to_owned(), Vec::new())
    }
}

impl Manager {
    /// Load a player-edited policy, falling back to the shipped file.
    #[must_use]
    pub fn load(config_dir: &Path, exe_dir: &Path) -> Self {
        let mut custom_dirs = vec![
            config_dir.join("custom-restrictions"),
            exe_dir.join("../custom-restrictions"),
            exe_dir.join("custom-restrictions"),
        ];
        custom_dirs.sort();
        custom_dirs.dedup();
        for path in [
            config_dir.join("deviations.toml"),
            exe_dir.join("../deviations.toml"),
            exe_dir.join("deviations.toml"),
        ] {
            if let Ok(text) = std::fs::read_to_string(path) {
                return Self::from_text(text, custom_dirs);
            }
        }
        Self::from_text(DEFAULT_POLICY.to_owned(), custom_dirs)
    }

    fn from_text(text: String, custom_dirs: Vec<PathBuf>) -> Self {
        Self {
            text,
            policies: BTreeMap::new(),
            rules: default_rules(),
            airspace: None,
            airspace_attempted: false,
            custom_dirs,
            strikes: BTreeMap::new(),
            last: BTreeMap::new(),
            recovery: None,
            ground_pending: false,
            ground_done: false,
            divert_pending: false,
            diverted: false,
            number_sent: false,
            generation: 0,
        }
    }

    /// Run one telemetry tick; returns true when a radio call was added.
    pub fn tick(&mut self, state: &mut EngineState) -> bool {
        self.reset_if_needed(state.session_generation);
        let region = region(state);
        let policy = self.policy(&region);
        if policy.mode == Mode::Off || state.session.telemetry.radio_busy {
            return false;
        }
        state.flow.warning.limit = policy.settings.flight_warnings;
        state.flow.monitor.warning.limit = policy.settings.flight_warnings;
        if self.ground_pending && !self.ground_done {
            return self.ground_consequence(state, &policy);
        }
        if self.divert_pending && !self.diverted {
            return self.divert(state, &policy);
        }
        if self.recovery.is_some() {
            return self.recover(state);
        }
        self.ensure_airspace(state);
        let finding = {
            let context = DeviationContext {
                telemetry: &state.session.telemetry,
                plan: &state.session.plan,
                taxi: &state.session.taxi_clearance,
                airport: state.airport.as_ref(),
                airspace: self.airspace.as_ref(),
                phase: state.session.phase,
                next_fix: state.flow.monitor.next_fix,
                vector_active: state.flow.monitor.heading.is_some(),
                direct_active: state.flow.monitor.direct_origin.is_some(),
                settings: &policy.settings,
            };
            self.rules
                .iter()
                .filter(|rule| policy.rule(rule.id()))
                .find_map(|rule| rule.evaluate(&context))
        };
        finding.is_some_and(|finding| self.handle(state, &policy, finding))
    }

    fn reset_if_needed(&mut self, generation: u64) {
        if self.generation == generation {
            return;
        }
        self.generation = generation;
        self.strikes.clear();
        self.last.clear();
        self.recovery = None;
        self.ground_pending = false;
        self.ground_done = false;
        self.divert_pending = false;
        self.diverted = false;
        self.number_sent = false;
    }

    fn policy(&mut self, region: &str) -> Policy {
        if let Some(policy) = self.policies.get(region) {
            return policy.clone();
        }
        let policy = Policy::load(&self.text, region).unwrap_or_else(|error| {
            eprintln!("Deviation policy: {error}");
            Policy::load(DEFAULT_POLICY, region).unwrap_or_else(|_| Policy {
                mode: Mode::Realistic,
                settings: Default::default(),
                rules: BTreeMap::new(),
                number: Default::default(),
            })
        });
        self.policies.insert(region.to_owned(), policy.clone());
        policy
    }

    fn ensure_airspace(&mut self, state: &EngineState) {
        if self.airspace.is_some() || self.airspace_attempted {
            return;
        }
        self.airspace_attempted = true;
        let root = state.settings.simulator_root.trim();
        let mut index = if root.is_empty() {
            AirspaceIndex::default()
        } else {
            AirspaceIndex::load(Path::new(root)).unwrap_or_else(|error| {
                eprintln!("Deviation airspace: {error}");
                AirspaceIndex::default()
            })
        };
        if let Ok(areas) = Policy::airspace(&self.text) {
            index.add(areas);
        }
        index.add(custom_areas(&self.custom_dirs));
        self.airspace = Some(index);
    }

    fn handle(&mut self, state: &mut EngineState, policy: &Policy, finding: Finding) -> bool {
        let now = Instant::now();
        if self.last.get(finding.rule).is_some_and(|last| {
            now.duration_since(*last) < Duration::from_secs(policy.settings.cooldown_seconds)
        }) {
            return false;
        }
        let callsign = state.session.plan.callsign.clone();
        let mut slots = vec![("callsign", callsign)];
        for (key, value) in &finding.facts {
            slots.push((key.as_str(), value.clone()));
        }
        let reply = pilot_reply_for(finding.phrase);
        if !send(state, finding.phrase, &slots, reply) {
            return false;
        }
        self.last.insert(finding.rule, now);
        let runway_entry = finding.rule == "hold_short_passed"
            && policy.settings.runway_entry_immediate
            && aircraft_on_runway(state);
        let weight = if runway_entry {
            policy.limit(finding.scope)
        } else if finding.serious() {
            if policy.settings.serious_immediate {
                policy.limit(finding.scope)
            } else {
                policy.settings.serious_strikes.max(1)
            }
        } else {
            1
        };
        let count = self.strikes.entry(finding.rule).or_default();
        *count = count.saturating_add(weight);
        let limit = policy.limit(finding.scope);
        if *count >= limit {
            match finding.scope {
                Scope::Ground => self.ground_pending = true,
                Scope::Flight => self.divert_pending = true,
            }
        } else if finding.rule == "hold_short_passed"
            && !runway_entry
            && let Some(runway) = finding.facts.get("runway")
            && runway_clear(state, runway)
        {
            self.recovery = Some(runway.clone());
        }
        true
    }

    fn ground_consequence(&mut self, state: &mut EngineState, policy: &Policy) -> bool {
        let callsign = state.session.plan.callsign.clone();
        let slots = [("callsign", callsign.clone())];
        let sent = send(
            state,
            "deviation_hold_position",
            &slots,
            Some("pilot_hold_position_readback"),
        );
        state.flow.takeoff_cleared = false;
        state.session.taxi_clearance.takeoff_state = openatc_core::state::TakeoffState::Idle;
        self.send_number(state, policy, callsign);
        self.ground_done = true;
        self.ground_pending = false;
        sent
    }

    fn send_number(&mut self, state: &mut EngineState, policy: &Policy, callsign: String) {
        if !policy.number.enabled || self.number_sent || policy.number.number.trim().is_empty() {
            return;
        }
        let number = number_words(&policy.number.number);
        let first = [("callsign", callsign.clone()), ("number", number.clone())];
        send(
            state,
            "atc_number_for_you",
            &first,
            Some("pilot_ready_for_number"),
        );
        let second = [("number", number), ("callsign", callsign)];
        send(
            state,
            "atc_number_for_you_number",
            &second,
            Some("pilot_number_readback"),
        );
        self.number_sent = true;
    }

    fn recover(&mut self, state: &mut EngineState) -> bool {
        let Some(runway) = self.recovery.as_ref() else {
            return false;
        };
        if !runway_clear(state, runway) {
            return false;
        }
        let callsign = state.session.plan.callsign.clone();
        let slots = [("callsign", callsign), ("runway", runway.clone())];
        let on_runway = aircraft_on_runway(state);
        let (phrase, reply) = if on_runway {
            ("deviation_vacate", "pilot_vacate_readback")
        } else {
            ("deviation_continue", "pilot_continue_readback")
        };
        let sent = send(state, phrase, &slots, Some(reply));
        if sent {
            state.session.taxi_clearance.guidance_complete = true;
            state.session.taxi_clearance.hold_short_passed = true;
            state.session.taxi_clearance.holding_channel = 0;
            self.strikes.insert("hold_short_passed", 0);
            self.recovery = None;
        }
        sent
    }

    fn divert(&mut self, state: &mut EngineState, policy: &Policy) -> bool {
        let Some((icao, name)) = choose_airport(state, policy) else {
            self.diverted = true;
            self.divert_pending = false;
            return false;
        };
        let callsign = state.session.plan.callsign.clone();
        let slots = [("callsign", callsign.clone()), ("airport", name)];
        let sent = send(
            state,
            "deviation_divert",
            &slots,
            Some("pilot_divert_readback"),
        );
        state.session.plan.destination.clone_from(&icao);
        state.session.plan.route = format!("DCT {icao}");
        state.session.plan.arrival_runway.clear();
        state.session.plan.star.clear();
        state.session.plan.star_transition.clear();
        state.session.plan.approach.clear();
        state.session.plan.fixes.clear();
        state.arrival_airport = None;
        state.arrival_profile = None;
        state.arrival_load_error.clear();
        self.send_number(state, policy, callsign);
        self.diverted = true;
        self.divert_pending = false;
        sent
    }
}

/// Run the manager without holding two mutable borrows of the engine state.
pub fn tick(state: &mut EngineState) -> bool {
    let mut manager = std::mem::take(&mut state.deviations);
    let result = manager.tick(state);
    state.deviations = manager;
    result
}

fn aircraft_on_runway(state: &EngineState) -> bool {
    state.airport.as_ref().is_some_and(|airport| {
        let point = openatc_core::airport::airport_point(
            airport,
            state.session.telemetry.latitude,
            state.session.telemetry.longitude,
        );
        openatc_core::airport::within_runway(airport, &point)
    })
}

fn custom_areas(directories: &[PathBuf]) -> Vec<openatc_core::airspace::Area> {
    let mut areas = Vec::new();
    for directory in directories {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        let mut paths: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "toml")
            })
            .collect();
        paths.sort();
        for path in paths {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            match Policy::airspace(&text) {
                Ok(found) => areas.extend(found),
                Err(error) => eprintln!("Custom restrictions {}: {error}", path.display()),
            }
        }
    }
    areas
}

fn region(state: &EngineState) -> String {
    let telemetry = &state.session.telemetry;
    let nearest = telemetry
        .position_valid
        .then(|| {
            state
                .stations
                .iter()
                .filter(|station| {
                    station.service == "Tower"
                        || station.services.iter().any(|service| service == "Tower")
                })
                .min_by(|left, right| {
                    let distance = |station: &openatc_core::stations::Station| {
                        openatc_core::arrival::distance_nm(
                            [telemetry.latitude, telemetry.longitude],
                            [station.latitude, station.longitude],
                        )
                    };
                    distance(left).total_cmp(&distance(right))
                })
                .map(|station| station.airport.clone())
        })
        .flatten();
    let icao = nearest
        .filter(|icao| !icao.is_empty())
        .unwrap_or_else(|| state.session.plan.departure.trim().to_owned());
    openatc_core::regions::region_for(&state.regions, &icao).name
}

fn pilot_reply_for(phrase: &str) -> Option<&'static str> {
    match phrase {
        "deviation_hold_short" => Some("pilot_hold_short_readback"),
        "deviation_airspace" => Some("pilot_airspace_readback"),
        "deviation_off_plan" => Some("pilot_off_plan_readback"),
        "deviation_hold_position" => Some("pilot_hold_position_readback"),
        "deviation_continue" => Some("pilot_continue_readback"),
        "deviation_vacate" => Some("pilot_vacate_readback"),
        "deviation_divert" => Some("pilot_divert_readback"),
        _ => None,
    }
}

fn send(
    state: &mut EngineState,
    phrase: &str,
    slots: &[(&str, String)],
    reply: Option<&str>,
) -> bool {
    let text = say(phrase, slots);
    if text.is_empty() {
        return false;
    }
    let tag = tag(state);
    add_transmission(&mut state.session, "ATC", &text, &tag);
    if state.settings.copilot_replies
        && let Some(reply) = reply
        && let Some(entry) = state.session.transcript.last_mut()
    {
        let reply = say(reply, slots);
        if !reply.is_empty() {
            entry.pilot_reply = reply;
        }
    }
    true
}

fn tag(state: &mut EngineState) -> SpeechTag {
    let channel = state.session.control_frequency().unwrap_or(0);
    let nearby = openatc_core::stations::nearby(&state.stations, &state.session.telemetry);
    if let Some(station) = openatc_core::stations::tuned(&nearby, channel) {
        crate::flight::tag(state, station)
    } else {
        SpeechTag {
            radio: Some(state.session.control_radio),
            frequency: state.session.control_frequency(),
            position: "ATC".to_owned(),
            ..Default::default()
        }
    }
}

fn runway_clear(state: &EngineState, runway: &str) -> bool {
    let Some(airport) = state.airport.as_ref() else {
        return false;
    };
    if runway.is_empty() {
        return false;
    }
    let telemetry = state.telemetry_with_traffic();
    openatc_core::holding::blocked(airport, runway, &telemetry).is_none()
}

fn choose_airport(state: &EngineState, policy: &Policy) -> Option<(String, String)> {
    let current = state
        .airport
        .as_ref()
        .map(|airport| airport.icao.as_str())
        .unwrap_or_default();
    let alternate = state.session.plan.alternate.trim();
    if !alternate.is_empty() && alternate != current {
        return Some((alternate.to_owned(), station_name(state, alternate)));
    }
    let range = policy.settings.diversion_range_nm.max(1.0);
    let origin = [
        state.session.telemetry.latitude,
        state.session.telemetry.longitude,
    ];
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    for station in &state.stations {
        if station.airport.is_empty()
            || station.airport == current
            || !seen.insert(station.airport.clone())
        {
            continue;
        }
        let distance =
            openatc_core::arrival::distance_nm(origin, [station.latitude, station.longitude]);
        if distance <= range {
            candidates.push((
                distance,
                station.airport.clone(),
                station.airport_name.clone(),
            ));
        }
    }
    candidates.sort_by(|left, right| left.0.total_cmp(&right.0));
    let required = required_runway_m(state);
    let root = state.settings.simulator_root.as_str();
    let exists = |path: &str| Path::new(path).exists();
    let read = |path: &str| std::fs::read_to_string(path).map_err(|error| error.to_string());
    for (_, icao, name) in &candidates {
        if root.is_empty() {
            break;
        }
        if let Ok(airport) =
            openatc_core::airport::load_airport_from_simulator(root, icao, &exists, &read)
            && longest_runway_m(&airport) >= required
        {
            return Some((icao.clone(), non_empty(name, icao)));
        }
    }
    if let Some((_, icao, name)) = candidates.first() {
        return Some((icao.clone(), non_empty(name, icao)));
    }
    let destination = state.session.plan.destination.trim();
    if !destination.is_empty() && destination != current {
        return Some((destination.to_owned(), station_name(state, destination)));
    }
    None
}

fn non_empty(name: &str, icao: &str) -> String {
    if name.trim().is_empty() {
        icao.to_owned()
    } else {
        name.to_owned()
    }
}

fn station_name(state: &EngineState, icao: &str) -> String {
    state
        .stations
        .iter()
        .find(|station| station.airport == icao && !station.airport_name.trim().is_empty())
        .map_or_else(|| icao.to_owned(), |station| station.airport_name.clone())
}

fn required_runway_m(state: &EngineState) -> f64 {
    match openatc_core::parking::aircraft_size(&state.session) {
        Some('A') => 800.0,
        Some('B') => 1200.0,
        Some('C') => 1800.0,
        Some('D') => 2200.0,
        Some('E') => 2800.0,
        Some('F') => 3200.0,
        _ => 1500.0,
    }
}

fn longest_runway_m(airport: &openatc_core::airport::Airport) -> f64 {
    airport
        .runways
        .iter()
        .map(|runway| {
            (runway.second.east - runway.first.east).hypot(runway.second.north - runway.first.north)
        })
        .fold(0.0, f64::max)
}

fn number_words(value: &str) -> String {
    let words: Vec<_> = value
        .chars()
        .filter_map(|character| match character {
            '0' => Some("zero"),
            '1' => Some("one"),
            '2' => Some("two"),
            '3' => Some("three"),
            '4' => Some("four"),
            '5' => Some("five"),
            '6' => Some("six"),
            '7' => Some("seven"),
            '8' => Some("eight"),
            '9' => Some("nine"),
            _ => None,
        })
        .collect();
    if words.is_empty() {
        value.to_owned()
    } else {
        words.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_is_spoken_digit_by_digit() {
        assert_eq!(
            number_words("5318008"),
            "five three one eight zero zero eight"
        );
    }

    #[test]
    fn custom_restrictions_read_valid_files_and_skip_bad_ones() {
        let directory = std::env::temp_dir().join(format!(
            "openatc-custom-restrictions-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("valid.toml"),
            r#"
[[airspace]]
name = "Test area"
class = "R"
lower_ft = 0
upper_ft = 1000
points = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0]]
"#,
        )
        .unwrap();
        std::fs::write(directory.join("broken.toml"), "not valid toml").unwrap();
        let areas = custom_areas(&[directory.clone()]);
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].name, "Test area");
        std::fs::remove_dir_all(directory).ok();
    }
}
