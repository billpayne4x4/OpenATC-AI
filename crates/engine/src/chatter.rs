//! Audio-only airport chatter. No aircraft, models, routes or TCAS targets are created.

use crate::{EngineState, Shared};
use openatc_core::airport::Airport;
use openatc_core::apply::add_transmission;
use openatc_core::state::{Radio, SpeechTag, Telemetry};
use openatc_core::stations::{Station, reception_distance};
use openatc_core::traffic::profiles::Profiles;
use rand::Rng;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FIRST_DELAY_SECS: f64 = 4.0;
const GAP_MS: u32 = 700;

pub struct ChatterManager {
    sessions: BTreeMap<i32, Chatter>,
    airports: BTreeMap<String, Arc<Airport>>,
    loading: BTreeSet<String>,
    failed: BTreeMap<String, Instant>,
    next_global: Instant,
    next_warm: Instant,
}

impl Default for ChatterManager {
    fn default() -> Self {
        Self {
            sessions: BTreeMap::new(),
            airports: BTreeMap::new(),
            loading: BTreeSet::new(),
            failed: BTreeMap::new(),
            next_global: Instant::now(),
            next_warm: Instant::now(),
        }
    }
}

struct Chatter {
    frequency_khz: i32,
    airport: String,
    station: String,
    service: String,
    voice: String,
    radio: Radio,
    next_at: Instant,
    last_station_check: Instant,
    station_elevation: f64,
    seed: u64,
    runway: String,
    via: String,
    used_callsigns: BTreeSet<String>,
}

struct Line {
    speaker: String,
    text: String,
    position: String,
    gap_ms: u32,
}

impl ChatterManager {
    /// Load airport contexts away from the telemetry write lock.
    pub async fn warm_airports(shared: Shared) {
        {
            let state = shared.read().await;
            if Instant::now() < state.chatter.next_warm {
                return;
            }
        }
        {
            let mut state = shared.write().await;
            state.chatter.next_warm = Instant::now() + Duration::from_secs(2);
        }
        let (root, needed) = {
            let state = shared.read().await;
            if !state.settings.airport_chatter {
                return;
            }
            let telemetry = &state.session.telemetry;
            if telemetry.on_ground || telemetry.paused || !telemetry.position_valid {
                return;
            }
            let root = state.settings.simulator_root.clone();
            if root.is_empty() {
                return;
            }
            let needed = tuned_frequencies(telemetry)
                .into_iter()
                .filter_map(|frequency| station_for(&state, frequency, telemetry))
                .filter_map(|station| {
                    let airport = station.airport.clone();
                    (!airport.is_empty()
                        && state
                            .airport
                            .as_ref()
                            .is_none_or(|loaded| loaded.icao != airport)
                        && !state.chatter.airports.contains_key(&airport)
                        && !state.chatter.loading.contains(&airport)
                        && !state
                            .chatter
                            .failed
                            .get(&airport)
                            .is_some_and(|at| at.elapsed() < Duration::from_secs(60)))
                    .then_some(airport)
                })
                .collect::<BTreeSet<_>>();
            (root, needed)
        };
        for icao in needed {
            {
                let mut state = shared.write().await;
                if state.chatter.airports.contains_key(&icao)
                    || !state.chatter.loading.insert(icao.clone())
                {
                    continue;
                }
            }
            let shared = shared.clone();
            let root = root.clone();
            tokio::spawn(async move {
                let load_root = root.clone();
                let load_icao = icao.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let exists = |path: &str| std::path::Path::new(path).exists();
                    let read = |path: &str| {
                        std::fs::read_to_string(path).map_err(|error| error.to_string())
                    };
                    openatc_core::airport::load_airport_from_simulator(
                        &load_root, &load_icao, &exists, &read,
                    )
                })
                .await
                .unwrap_or_else(|error| Err(error.to_string()));
                let mut state = shared.write().await;
                state.chatter.finish_load(&icao, result);
            });
        }
    }

    fn finish_load(&mut self, icao: &str, result: Result<Airport, String>) {
        self.loading.remove(icao);
        match result {
            Ok(airport) => {
                self.airports.insert(icao.to_owned(), Arc::new(airport));
                self.failed.remove(icao);
                if self.airports.len() > 8
                    && let Some(key) = self
                        .airports
                        .keys()
                        .find(|key| {
                            !self
                                .sessions
                                .values()
                                .any(|session| session.airport == **key)
                        })
                        .cloned()
                {
                    self.airports.remove(&key);
                }
            }
            Err(_) => {
                self.failed.insert(icao.to_owned(), Instant::now());
            }
        }
    }

    /// Advance active chatter sessions and clear stations the player has left.
    pub fn tick(&mut self, state: &mut EngineState) {
        let now = Instant::now();
        let telemetry = state.session.telemetry.clone();
        if !state.settings.airport_chatter || telemetry.paused || !telemetry.position_valid {
            self.clear(state);
            return;
        }
        let tuned = tuned_frequencies(&telemetry);
        let mut active = BTreeSet::new();
        for frequency in tuned {
            let radio = radio_for(&telemetry, frequency);
            let mut keep = false;
            if let Some(session) = self.sessions.get(&frequency) {
                if session.last_station_check.elapsed() < Duration::from_secs(5) {
                    keep = true;
                } else if let Some(station) = station_for(state, frequency, &telemetry) {
                    if station.airport == session.airport
                        && service_for(&station) == session.service
                    {
                        if let Some(session) = self.sessions.get_mut(&frequency) {
                            session.last_station_check = now;
                            session.station_elevation = station.elevation_feet;
                            session.radio =
                                preferred_radio(radio, state.session.control_radio, session.radio);
                        }
                        keep = true;
                    }
                }
            }
            if keep {
                if let Some(session) = self.sessions.get_mut(&frequency) {
                    session.radio =
                        preferred_radio(radio, state.session.control_radio, session.radio);
                }
                active.insert(frequency);
                continue;
            }
            self.sessions.remove(&frequency);
            let Some(station) = station_for(state, frequency, &telemetry) else {
                continue;
            };
            if let Some(loaded) = &state.airport
                && loaded.icao == station.airport
            {
                self.airports
                    .entry(station.airport.clone())
                    .or_insert_with(|| Arc::new(loaded.clone()));
            }
            let Some(airport) = self.airports.get(&station.airport).cloned() else {
                continue;
            };
            let (runway, via) = airport_voice(&airport, state);
            self.sessions.insert(
                frequency,
                Chatter {
                    frequency_khz: frequency,
                    airport: station.airport.clone(),
                    station: station_label(&station, &airport),
                    service: service_for(&station),
                    voice: crate::flight::tag(state, &station).voice,
                    radio,
                    next_at: now + Duration::from_secs_f64(FIRST_DELAY_SECS),
                    last_station_check: now,
                    station_elevation: station.elevation_feet,
                    seed: rand::rng().random(),
                    runway,
                    via,
                    used_callsigns: BTreeSet::new(),
                },
            );
            active.insert(frequency);
        }
        self.sessions
            .retain(|frequency, _| active.contains(frequency));
        state.session.chatter_frequencies = active;
        if telemetry.radio_busy || now < self.next_global {
            return;
        }
        let control = state.session.control_radio;
        let Some(frequency) = self
            .sessions
            .values()
            .filter(|session| session.next_at <= now)
            .min_by_key(|session| radio_priority(session.radio, control))
            .map(|session| session.frequency_khz)
        else {
            return;
        };
        let Some(airport) = self
            .sessions
            .get(&frequency)
            .and_then(|session| self.airports.get(&session.airport))
            .cloned()
        else {
            return;
        };
        let Some(profiles) = state.traffic_profiles.current() else {
            return;
        };
        let Some(session) = self.sessions.get_mut(&frequency) else {
            return;
        };
        let Some(lines) = exchange(state, &airport, session, &profiles) else {
            if let Some(session) = self.sessions.get_mut(&frequency) {
                session.next_at = now + interval(state, session.seed);
            }
            return;
        };
        for line in lines {
            let voice = if line.speaker == "ATC" {
                session.voice.clone()
            } else {
                crate::traffic::pilot_voice_for_callsign(state, &line.speaker)
            };
            let tag = SpeechTag {
                radio: Some(session.radio),
                frequency: Some(frequency),
                gap_ms: line.gap_ms,
                chatter: true,
                position: line.position,
                voice,
                delivery: "standard".to_owned(),
                speed: 1.0,
                ..Default::default()
            };
            add_transmission(&mut state.session, &line.speaker, &line.text, &tag);
            if let Some(entry) = state.session.transcript.last_mut() {
                entry.background = true;
            }
        }
        if let Some(session) = self.sessions.get_mut(&frequency) {
            session.seed = session
                .seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1);
            session.next_at = now + interval(state, session.seed);
        }
        self.next_global = now + Duration::from_secs(18);
    }

    fn clear(&mut self, state: &mut EngineState) {
        self.sessions.clear();
        state.session.chatter_frequencies.clear();
    }
}

fn tuned_frequencies(telemetry: &Telemetry) -> Vec<i32> {
    let mut frequencies = [Radio::Com1, Radio::Com2]
        .into_iter()
        .filter_map(|radio| telemetry.frequency(radio))
        .filter(|frequency| *frequency > 0)
        .collect::<Vec<_>>();
    frequencies.sort_unstable();
    frequencies.dedup();
    frequencies
}

fn role(station: &Station, service: &str) -> bool {
    station.service == service || station.services.iter().any(|value| value == service)
}

fn service_for(station: &Station) -> String {
    if station.service == "Ground" || role(station, "Ground") && !role(station, "Tower") {
        "Ground".to_owned()
    } else {
        "Tower".to_owned()
    }
}

fn station_for(state: &EngineState, frequency: i32, telemetry: &Telemetry) -> Option<Station> {
    state
        .stations
        .iter()
        .filter(|station| station.khz == frequency)
        .filter(|station| role(station, "Ground") || role(station, "Tower"))
        .filter_map(|station| {
            reception_distance(station, telemetry).map(|distance| (distance, station))
        })
        .min_by(|(left, _), (right, _)| left.total_cmp(right))
        .map(|(_, station)| station.clone())
}

fn radio_for(telemetry: &Telemetry, frequency: i32) -> Radio {
    if telemetry.frequency(Radio::Com1) == Some(frequency) {
        Radio::Com1
    } else {
        Radio::Com2
    }
}

fn preferred_radio(candidate: Radio, control: Radio, current: Radio) -> Radio {
    if candidate == control {
        candidate
    } else if current == control {
        current
    } else {
        candidate
    }
}

fn radio_priority(radio: Radio, control: Radio) -> u8 {
    if radio == control {
        0
    } else if radio == Radio::Com1 {
        1
    } else {
        2
    }
}

fn station_label(station: &Station, airport: &Airport) -> String {
    let name = station.name.trim();
    if name.is_empty() {
        format!("{} {}", airport.name, station.service)
    } else {
        name.to_owned()
    }
}

fn airport_voice(airport: &Airport, state: &EngineState) -> (String, String) {
    let Some(runway) = active_runway(airport, state) else {
        return (String::new(), String::new());
    };
    (runway.clone(), taxi_via(airport, &runway))
}

fn active_runway(airport: &Airport, state: &EngineState) -> Option<String> {
    let planned = if airport.icao == state.session.plan.destination
        && !state.session.plan.arrival_runway.is_empty()
    {
        &state.session.plan.arrival_runway
    } else {
        &state.session.plan.runway
    };
    if !planned.is_empty()
        && airport
            .runways
            .iter()
            .any(|runway| runway.first_name == *planned || runway.second_name == *planned)
    {
        return Some(planned.clone());
    }
    airport
        .runways
        .iter()
        .max_by(|left, right| runway_length(left).total_cmp(&runway_length(right)))
        .map(|runway| runway.first_name.clone())
}

fn runway_length(runway: &openatc_core::airport::Runway) -> f64 {
    (runway.second.east - runway.first.east).hypot(runway.second.north - runway.first.north)
}

fn taxi_via(airport: &Airport, runway: &str) -> String {
    for stand in airport
        .parking
        .iter()
        .filter(|stand| {
            !stand.name.is_empty() && stand.size.chars().next().is_some_and(|size| size >= 'C')
        })
        .take(8)
    {
        let telemetry = Telemetry {
            position_valid: true,
            on_ground: true,
            latitude: airport.reference_latitude + stand.point.north / 111_320.0,
            longitude: airport.reference_longitude
                + stand.point.east
                    / (111_320.0 * (airport.reference_latitude.to_radians().cos()).max(0.01)),
            ..Default::default()
        };
        let Ok(route) =
            openatc_core::airport::calculate_taxi_route(airport, &telemetry, runway, false, 'C')
        else {
            continue;
        };
        if !route.via.is_empty() {
            return route.via;
        }
    }
    String::new()
}

fn exchange(
    state: &EngineState,
    airport: &Airport,
    session: &mut Chatter,
    profiles: &Profiles,
) -> Option<Vec<Line>> {
    if session.runway.is_empty() {
        return None;
    }
    let callsign = callsign(state, airport, profiles, session)?;
    session.used_callsigns.insert(callsign.clone());
    if session.used_callsigns.len() > 64 {
        session.used_callsigns.clear();
    }
    let station = session.station.clone();
    let runway = session.runway.clone();
    let via = session.via.clone();
    let lines = if session.service == "Ground" {
        let pilot = openatc_core::dialogue::say(
            "chatter_ground_ready",
            &[("station", station.clone()), ("callsign", callsign.clone())],
        );
        let (clearance, readback) = if via.is_empty() {
            (
                openatc_core::dialogue::say(
                    "chatter_ground_taxi_direct",
                    &[("callsign", callsign.clone()), ("runway", runway.clone())],
                ),
                openatc_core::dialogue::say(
                    "chatter_ground_taxi_direct_readback",
                    &[("callsign", callsign.clone()), ("runway", runway.clone())],
                ),
            )
        } else {
            (
                openatc_core::dialogue::say(
                    "chatter_ground_taxi",
                    &[
                        ("callsign", callsign.clone()),
                        ("runway", runway.clone()),
                        ("via", via.clone()),
                    ],
                ),
                openatc_core::dialogue::say(
                    "chatter_ground_taxi_readback",
                    &[
                        ("callsign", callsign.clone()),
                        ("runway", runway.clone()),
                        ("via", via.clone()),
                    ],
                ),
            )
        };
        vec![
            Line {
                speaker: callsign.clone(),
                text: pilot,
                position: "Chatter".to_owned(),
                gap_ms: GAP_MS,
            },
            Line {
                speaker: "ATC".to_owned(),
                text: clearance,
                position: "Ground".to_owned(),
                gap_ms: GAP_MS,
            },
            Line {
                speaker: callsign.clone(),
                text: readback,
                position: "Chatter".to_owned(),
                gap_ms: 0,
            },
        ]
    } else if rand::rng().random_bool(0.5) {
        let pilot = openatc_core::dialogue::say(
            "chatter_tower_departure",
            &[("station", station.clone()), ("callsign", callsign.clone())],
        );
        let clearance = openatc_core::dialogue::say(
            "chatter_tower_lineup",
            &[("callsign", callsign.clone()), ("runway", runway.clone())],
        );
        let readback = openatc_core::dialogue::say(
            "chatter_tower_lineup_readback",
            &[("callsign", callsign.clone()), ("runway", runway.clone())],
        );
        vec![
            Line {
                speaker: callsign.clone(),
                text: pilot,
                position: "Chatter".to_owned(),
                gap_ms: GAP_MS,
            },
            Line {
                speaker: "ATC".to_owned(),
                text: clearance,
                position: "Tower".to_owned(),
                gap_ms: GAP_MS,
            },
            Line {
                speaker: callsign.clone(),
                text: readback,
                position: "Chatter".to_owned(),
                gap_ms: 0,
            },
        ]
    } else {
        let pilot = openatc_core::dialogue::say(
            "chatter_tower_final",
            &[
                ("station", station.clone()),
                ("callsign", callsign.clone()),
                ("runway", runway.clone()),
            ],
        );
        let clearance = openatc_core::dialogue::say(
            "chatter_tower_landing",
            &[("callsign", callsign.clone()), ("runway", runway.clone())],
        );
        let readback = openatc_core::dialogue::say(
            "chatter_tower_landing_readback",
            &[("callsign", callsign.clone()), ("runway", runway.clone())],
        );
        vec![
            Line {
                speaker: callsign.clone(),
                text: pilot,
                position: "Chatter".to_owned(),
                gap_ms: GAP_MS,
            },
            Line {
                speaker: "ATC".to_owned(),
                text: clearance,
                position: "Tower".to_owned(),
                gap_ms: GAP_MS,
            },
            Line {
                speaker: callsign.clone(),
                text: readback,
                position: "Chatter".to_owned(),
                gap_ms: 0,
            },
        ]
    };
    Some(lines)
}

fn callsign(
    state: &EngineState,
    airport: &Airport,
    profiles: &Profiles,
    session: &Chatter,
) -> Option<String> {
    let candidates = profiles
        .airlines
        .values()
        .filter(|airline| !airline.operator.is_empty())
        .filter(|airline| {
            airline.serves.is_empty()
                || airline
                    .serves
                    .iter()
                    .any(|prefix| airport.icao.starts_with(prefix))
        })
        .filter(|airline| {
            airline.home_prefixes.is_empty()
                || airline
                    .home_prefixes
                    .iter()
                    .any(|prefix| airport.icao.starts_with(prefix))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    let player = state.session.plan.callsign.to_uppercase();
    for _ in 0..8 {
        let airline = candidates[rand::rng().random_range(0..candidates.len())];
        let value = format!(
            "{}{}",
            airline.operator,
            100 + rand::rng().random_range(0..8900)
        );
        if value == player || session.used_callsigns.contains(&value) {
            continue;
        }
        return Some(value);
    }
    None
}

fn interval(state: &EngineState, seed: u64) -> Duration {
    let density = [
        state.settings.airline_traffic,
        state.settings.regional_traffic,
        state.settings.cargo_traffic,
        state.settings.ga_traffic,
    ]
    .into_iter()
    .map(f64::from)
    .sum::<f64>();
    let mut base = (55.0 - density * 0.7).clamp(12.0, 60.0);
    base *= match state.settings.congestion {
        openatc_settings::Congestion::Quiet => 1.25,
        openatc_settings::Congestion::Busy => 0.75,
        openatc_settings::Congestion::Off => 1.0,
    };
    let jitter = 0.7 + (mix(seed) % 601) as f64 / 1000.0;
    Duration::from_secs_f64(base * jitter)
}

fn mix(value: u64) -> u64 {
    let mut value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::airport::{Parking, Runway, TaxiEdge, TaxiNode};
    use openatc_core::state::Point;

    fn point(east: f64, north: f64) -> Point {
        Point {
            east,
            north,
            height: 0.0,
        }
    }

    fn airport() -> Airport {
        Airport {
            icao: "YSSY".to_owned(),
            name: "Sydney".to_owned(),
            elevation_feet: 0.0,
            runways: vec![Runway {
                first_name: "09".to_owned(),
                second_name: "27".to_owned(),
                first: point(0.0, 0.0),
                second: point(2_000.0, 0.0),
                width: 45.0,
                ..Default::default()
            }],
            parking: vec![Parking {
                name: "1".to_owned(),
                size: "D".to_owned(),
                point: point(-300.0, -200.0),
                ..Default::default()
            }],
            nodes: vec![
                TaxiNode {
                    id: 1,
                    point: point(-300.0, -200.0),
                },
                TaxiNode {
                    id: 2,
                    point: point(0.0, -200.0),
                },
                TaxiNode {
                    id: 3,
                    point: point(0.0, -20.0),
                },
            ],
            edges: vec![
                TaxiEdge {
                    first: 1,
                    second: 2,
                    name: "Alpha".to_owned(),
                    runway: false,
                    one_way: false,
                    active_runways: String::new(),
                    size: 'D',
                },
                TaxiEdge {
                    first: 2,
                    second: 3,
                    name: "Bravo".to_owned(),
                    runway: false,
                    one_way: false,
                    active_runways: String::new(),
                    size: 'D',
                },
            ],
            ..Default::default()
        }
    }

    fn fixture(frequency: i32, altitude: f64) -> crate::EngineState {
        let mut state = crate::test_state();
        state.settings.airport_chatter = true;
        state.session.telemetry = Telemetry {
            position_valid: true,
            latitude: 0.0,
            longitude: 0.0,
            altitude_feet: altitude,
            on_ground: false,
            com1_khz: frequency,
            ..Default::default()
        };
        state.stations = vec![Station {
            airport: "YSSY".to_owned(),
            airport_name: "Sydney".to_owned(),
            name: "Sydney Ground".to_owned(),
            service: "Ground".to_owned(),
            khz: frequency,
            latitude: 0.0,
            longitude: 0.0,
            elevation_feet: 0.0,
            ..Default::default()
        }];
        state
    }

    #[test]
    fn destination_chatter_uses_the_arrival_runway() {
        let airport = airport();
        let mut state = crate::test_state();
        state.session.plan.destination = "YSSY".into();
        state.session.plan.runway = "09".into();
        state.session.plan.arrival_runway = "27".into();
        assert_eq!(active_runway(&airport, &state).as_deref(), Some("27"));
    }

    #[test]
    fn chatter_uses_the_installed_runway_and_taxiways() {
        let airport = airport();
        assert_eq!(
            active_runway(&airport, &crate::test_state()).as_deref(),
            Some("09")
        );
        assert!(taxi_via(&airport, "09").contains("Alpha"));
        let mut chatter = Chatter {
            frequency_khz: 121_900,
            airport: "YSSY".to_owned(),
            station: "Sydney Ground".to_owned(),
            service: "Ground".to_owned(),
            voice: "test-voice".to_owned(),
            radio: Radio::Com1,
            next_at: Instant::now(),
            last_station_check: Instant::now(),
            station_elevation: 0.0,
            seed: 7,
            runway: "09".to_owned(),
            via: "Alpha, Bravo".to_owned(),
            used_callsigns: BTreeSet::new(),
        };
        let mut state = fixture(121_900, 5_000.0);
        state.session.plan.callsign = "QFA123".to_owned();
        let profiles = crate::traffic_profiles::Store::default().current().unwrap();
        let lines = exchange(&state, &airport, &mut chatter, &profiles).unwrap();
        assert!(lines[0].text.contains("Sydney Ground"));
        assert!(lines[1].text.contains("runway 09"));
        assert!(lines[1].text.contains("Alpha, Bravo"));
        assert!(lines[2].text.contains("runway 09"));
    }

    #[test]
    fn chatter_sessions_follow_tuned_frequencies_and_disappear_on_tune_away() {
        let mut manager = ChatterManager::default();
        let mut state = fixture(121_900, 5_000.0);
        manager
            .airports
            .insert("YSSY".to_owned(), Arc::new(airport()));
        manager.tick(&mut state);
        assert_eq!(manager.sessions.len(), 1);
        assert!(state.session.chatter_frequencies.contains(&121_900));
        state.session.telemetry.com1_khz = 118_000;
        manager.tick(&mut state);
        assert!(manager.sessions.is_empty());
        assert!(state.session.chatter_frequencies.is_empty());
        assert!(state.session.generated_traffic.is_empty());
    }

    #[test]
    fn chatter_emits_background_lines_on_the_tuned_radio() {
        let mut manager = ChatterManager::default();
        let mut state = fixture(121_900, 5_000.0);
        manager
            .airports
            .insert("YSSY".to_owned(), Arc::new(airport()));
        manager.tick(&mut state);
        manager
            .sessions
            .get_mut(&121_900)
            .expect("chatter session")
            .next_at = Instant::now();
        manager.next_global = Instant::now() - Duration::from_secs(1);
        manager.tick(&mut state);
        let lines = state
            .session
            .transcript
            .iter()
            .filter(|entry| entry.background && entry.frequency == Some(121_900))
            .collect::<Vec<_>>();
        assert!(!lines.is_empty());
        assert!(lines.iter().all(|entry| entry.radio == Some(Radio::Com1)));
        assert!(state.session.generated_traffic.is_empty());
    }

    #[test]
    fn chatter_runs_on_the_ground_and_two_radios_hold_two_sessions() {
        let mut manager = ChatterManager::default();
        let mut state = fixture(121_900, 400.0);
        manager
            .airports
            .insert("YSSY".to_owned(), Arc::new(airport()));
        manager.tick(&mut state);
        assert_eq!(manager.sessions.len(), 1);

        let mut state = fixture(121_900, 5_000.0);
        state.session.telemetry.com2_khz = Some(118_000);
        state.stations.push(Station {
            airport: "YSSY".to_owned(),
            airport_name: "Sydney".to_owned(),
            name: "Sydney Tower".to_owned(),
            service: "Tower".to_owned(),
            khz: 118_000,
            latitude: 0.0,
            longitude: 0.0,
            elevation_feet: 0.0,
            ..Default::default()
        });
        manager
            .airports
            .insert("YSSY".to_owned(), Arc::new(airport()));
        manager.tick(&mut state);
        assert_eq!(manager.sessions.len(), 2);
        assert!(state.session.chatter_frequencies.contains(&121_900));
        assert!(state.session.chatter_frequencies.contains(&118_000));
    }
}
