//! Optional reminders based on observed aircraft state, independent of request parsing.

#![allow(clippy::default_trait_access)]
use crate::EngineState;
use openatc_core::aircraft::AircraftBehaviour;
use openatc_core::{
    airport::{Runway, airport_point},
    state::{PhaseCode, Point},
    stations::{Station, nearby, tuned},
};
use std::time::{Duration, Instant};

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Monitor {
    previous_ground: Option<bool>,
    departure_seen: bool,
    takeoff_unapproved: bool,
    landing_unapproved: bool,
    runway_reported: bool,
    transponder: Reminder,
    taxi: Reminder,
    taxi_sequence: u32,
    taxi_station: String,
    taxi_movement: TaxiMovement,
    assistance: Option<TaxiAssistance>,
}

#[derive(Default)]
struct TaxiMovement {
    origin: Option<(f64, f64)>,
    rolling: bool,
    started: bool,
    joined_route: bool,
}

impl TaxiMovement {
    fn observe(&mut self, state: &EngineState) {
        if !taxi_monitoring(state) {
            *self = Self::default();
            return;
        }
        let t = &state.session.telemetry;
        let origin = *self.origin.get_or_insert((t.latitude, t.longitude));
        let moving = t.ground_speed_knots >= 1.0;
        // Speed alone can flicker at rest. Require displacement and two moving samples.
        self.started |= moving
            && self.rolling
            && openatc_core::ops::distance_nm(origin.0, origin.1, t.latitude, t.longitude) * 1852.0
                >= 5.0;
        self.rolling = moving;
        self.joined_route |= !off_route(state);
    }

    fn joining_route(&self, state: &EngineState) -> bool {
        if self.joined_route {
            return false;
        }
        let Some((latitude, longitude)) = self.origin else {
            return false;
        };
        let route = &state.session.taxi_clearance;
        let Some(first) = route.points.first() else {
            return false;
        };
        let Some(airport) = state
            .airport
            .as_ref()
            .filter(|airport| airport.icao == route.airport)
        else {
            return false;
        };
        let origin = airport_point(airport, latitude, longitude);
        let point = airport_point(
            airport,
            state.session.telemetry.latitude,
            state.session.telemetry.longitude,
        );
        // Scenery often leaves the apron between the stand and the first network node unmapped.
        // Keep the same corridor tolerance as the mapped route, without inventing a runway permission.
        segment_distance(point, origin, *first) <= 60.0
            && (point.east - first.east).hypot(point.north - first.north)
                <= (origin.east - first.east).hypot(origin.north - first.north) + 5.0
    }
}

struct TaxiAssistance {
    sequence: u32,
    route_sequence: u32,
    frequency: i32,
    station: String,
}

#[derive(Default)]
struct Reminder {
    key: String,
    since: Option<Instant>,
    last_call: Option<Instant>,
    calls: u8,
}

impl Reminder {
    fn observe(&mut self, key: String, now: Instant) {
        if self.key != key || key.is_empty() {
            self.key = key;
            self.since = Some(now);
            self.last_call = None;
            self.calls = 0;
        }
    }

    fn ready(&self, now: Instant, delay: u64) -> bool {
        !self.key.is_empty()
            && self
                .since
                .is_some_and(|since| now.duration_since(since) >= Duration::from_secs(delay))
            && self
                .last_call
                .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(60))
    }

    /// A bounded reminder: one repeat interval between calls, and a fixed number of calls.
    fn ready_repeating(&self, now: Instant, delay: u64, repeat: u64, limit: u8) -> bool {
        !self.key.is_empty()
            && self.calls < limit
            && self
                .since
                .is_some_and(|since| now.duration_since(since) >= Duration::from_secs(delay))
            && self
                .last_call
                .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(repeat))
    }
}

fn valid_squawk(code: i32) -> bool {
    (0..=7777).contains(&code) && format!("{code:04}").bytes().all(|digit| digit <= b'7')
}

fn transponder_issue(state: &EngineState) -> String {
    let t = &state.session.telemetry;
    let Some(clearance) = state.session.clearance.as_ref().filter(|c| c.acknowledged) else {
        return String::new();
    };
    if !state.settings.monitor_transponder || state.session.clearance_cancelled {
        return String::new();
    }
    // Emergency codes need their own handling, not a routine code correction.
    if matches!(t.transponder_code, Some(7500 | 7600 | 7700)) {
        return String::new();
    }
    if let (Some(actual), Ok(assigned)) = (t.transponder_code, clearance.squawk.parse::<i32>())
        && valid_squawk(actual)
        && valid_squawk(assigned)
        && actual != assigned
    {
        return format!("squawk:{assigned:04}");
    }
    if !t.on_ground
        && t.height_agl_feet > 300.0
        && matches!(t.transponder_mode, Some(0 | 1 | 2 | 4 | 5))
    {
        return "altitude".into();
    }
    String::new()
}

fn on_pavement(runway: &Runway, point: Point) -> bool {
    let dx = runway.second.east - runway.first.east;
    let dy = runway.second.north - runway.first.north;
    let length = dx.hypot(dy);
    if length < 100.0 {
        return false;
    }
    let x = point.east - runway.first.east;
    let y = point.north - runway.first.north;
    let along = (x * dx + y * dy) / length;
    let across = (x * dy - y * dx).abs() / length;
    (0.0..=length).contains(&along) && across < runway.width / 2.0
}

fn matches_runway(runway: &Runway, name: &str) -> bool {
    !name.is_empty() && (runway.first_name == name || runway.second_name == name)
}

fn unauthorized_runway(state: &EngineState) -> Option<String> {
    let t = &state.session.telemetry;
    if !t.on_ground
        || state.session.clearance.is_none()
        || matches!(state.session.phase, PhaseCode::Landed | PhaseCode::Finished)
    {
        return None;
    }
    let airport = state.airport.as_ref()?;
    let point = airport_point(airport, t.latitude, t.longitude);
    let route = &state.session.taxi_clearance;
    airport
        .runways
        .iter()
        .find(|r| {
            on_pavement(r, point)
                && !(state.flow.takeoff_cleared
                    && airport.icao == state.session.plan.departure
                    && !state.session.has_departed
                    && matches_runway(r, &state.session.plan.runway))
                && !(state.flow.landing_cleared
                    && airport.icao == state.session.plan.destination
                    && state.session.phase == PhaseCode::Approach
                    && matches_runway(r, &state.session.plan.arrival_runway))
                && !(route.approved
                    && route.airport == airport.icao
                    && (matches_runway(r, &route.crossing_runway)
                        || route.runway_taxi && matches_runway(r, &route.destination)))
        })
        .map(|r| format!("{}/{}", r.first_name, r.second_name))
}

fn segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let dx = b.east - a.east;
    let dy = b.north - a.north;
    let length = dx * dx + dy * dy;
    let fraction = if length > 0.0 {
        (((p.east - a.east) * dx + (p.north - a.north) * dy) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.east - a.east - fraction * dx).hypot(p.north - a.north - fraction * dy)
}

fn taxi_monitoring(state: &EngineState) -> bool {
    let t = &state.session.telemetry;
    let route = &state.session.taxi_clearance;
    if !state.settings.monitor_taxi_route
        || !t.on_ground
        || !route.approved
        || route.pending_readback
        || route.guidance_complete
        || !route.crossing_runway.is_empty()
        || route.runway_taxi
        || route.points.len() < 2
        || !matches!(state.session.phase, PhaseCode::Taxi | PhaseCode::TaxiIn)
    {
        return false;
    }
    true
}

fn off_route(state: &EngineState) -> bool {
    if !taxi_monitoring(state) {
        return false;
    }
    let t = &state.session.telemetry;
    let route = &state.session.taxi_clearance;
    let Some(airport) = state.airport.as_ref().filter(|a| a.icao == route.airport) else {
        return false;
    };
    let point = airport_point(airport, t.latitude, t.longitude);
    route
        .points
        .windows(2)
        .all(|s| segment_distance(point, s[0], s[1]) > 60.0)
}

/// Observe every telemetry update, but speak only when the radio queue is clear.
pub fn tick(state: &mut EngineState, radio_available: bool) -> bool {
    tick_at(state, radio_available, Instant::now())
}

fn tick_at(state: &mut EngineState, radio_available: bool, now: Instant) -> bool {
    let mut monitor = std::mem::take(&mut state.flow.discipline);
    let t = &state.session.telemetry;
    if !state.settings.monitor_transponder
        && !state.settings.monitor_runway_permissions
        && !state.settings.monitor_taxi_route
    {
        state.flow.discipline = Monitor {
            previous_ground: Some(t.on_ground),
            departure_seen: state.session.has_departed,
            ..Default::default()
        };
        return false;
    }
    if state.session.demo || t.paused || !t.position_valid {
        monitor.previous_ground = None;
        monitor.transponder = Reminder::default();
        monitor.taxi = Reminder::default();
        if !monitor.taxi_movement.started {
            monitor.taxi_movement = TaxiMovement::default();
        }
        state.flow.discipline = monitor;
        return false;
    }
    if state.settings.monitor_runway_permissions {
        if monitor.previous_ground == Some(true) && !t.on_ground && !monitor.departure_seen {
            monitor.takeoff_unapproved =
                !state.flow.takeoff_cleared && !state.session.plan.departure.is_empty();
        }
        if !t.on_ground {
            monitor.departure_seen = true;
        }
        if monitor.previous_ground == Some(false) && t.on_ground && state.session.has_departed {
            monitor.landing_unapproved = !state.flow.landing_cleared;
        }
    } else {
        monitor.takeoff_unapproved = false;
        monitor.landing_unapproved = false;
        monitor.runway_reported = false;
    }
    monitor.previous_ground = Some(t.on_ground);
    monitor.transponder.observe(transponder_issue(state), now);
    let route = &state.session.taxi_clearance;
    if monitor.taxi_sequence != route.sequence {
        monitor.taxi_sequence = route.sequence;
        monitor.taxi = Reminder::default();
        monitor.taxi_movement = TaxiMovement::default();
        monitor.assistance = None;
        monitor.taxi_station = state
            .session
            .transcript
            .iter()
            .find(|e| e.sequence == route.sequence && e.speaker == "ATC")
            .map_or_else(String::new, |e| e.position.clone());
    }
    monitor.taxi_movement.observe(state);
    monitor.taxi.observe(
        if monitor.taxi_movement.started
            && off_route(state)
            && !monitor.taxi_movement.joining_route(state)
        {
            "off_route".into()
        } else {
            String::new()
        },
        now,
    );
    let runway = state
        .settings
        .monitor_runway_permissions
        .then(|| unauthorized_runway(state))
        .flatten();
    if runway.is_none() {
        monitor.runway_reported = false;
    }
    let channel: Vec<_> = state
        .stations
        .iter()
        .filter(|s| Some(s.khz) == state.session.control_frequency())
        .cloned()
        .collect();
    let stations = nearby(&channel, t);
    let station = state
        .session
        .control_frequency()
        .and_then(|frequency| tuned(&stations, frequency))
        .filter(|s| !["Unicom", "ATIS"].contains(&s.service.as_str()));
    let mut message = None;
    if radio_available
        && t.radio_power
        && let Some(station) = station
    {
        let on_airport_channel = state
            .airport
            .as_ref()
            .is_some_and(|a| a.icao == station.airport)
            && ["Tower", "Ground"].contains(&station.service.as_str());
        if state.settings.monitor_runway_permissions
            && on_airport_channel
            && t.on_ground
            && t.ground_speed_knots < 40.0
            && !monitor.runway_reported
            && let Some(runway) = runway
        {
            monitor.runway_reported = true;
            message = Some(("monitor_runway_entry", vec![("runway", runway)]));
        } else if monitor.takeoff_unapproved && !t.on_ground && t.height_agl_feet > 50.0 {
            monitor.takeoff_unapproved = false;
            message = Some(("monitor_takeoff_without_clearance", vec![]));
        } else if monitor.landing_unapproved
            && on_airport_channel
            && t.on_ground
            && state.session.phase == PhaseCode::Landed
            && t.ground_speed_knots < 40.0
        {
            monitor.landing_unapproved = false;
            message = Some(("monitor_landing_without_clearance", vec![]));
        } else if !waiting_for_reply(state) {
            if monitor.transponder.ready_repeating(now, 30, 120, 2) {
                monitor.transponder.last_call = Some(now);
                monitor.transponder.calls = monitor.transponder.calls.saturating_add(1);
                message = Some(
                    if let Some(code) = monitor.transponder.key.strip_prefix("squawk:") {
                        ("monitor_squawk", vec![("squawk", code.to_owned())])
                    } else {
                        ("monitor_squawk_altitude", vec![])
                    },
                );
            } else if monitor.taxi.ready(now, 15)
                && station.name == monitor.taxi_station
                && on_airport_channel
            {
                monitor.taxi.last_call = Some(now);
                message = Some(("monitor_taxi_position", vec![]));
            }
        }
    }
    state.flow.discipline = monitor;
    if let (Some((phrase, mut slots)), Some(station)) = (message, station.cloned()) {
        slots.push(("callsign", state.session.plan.callsign.clone()));
        transmit(state, &station, phrase, &slots);
        true
    } else {
        false
    }
}

fn waiting_for_reply(state: &EngineState) -> bool {
    state
        .session
        .transcript
        .iter()
        .rev()
        .find(|e| e.is_controller_instruction())
        .is_some_and(|e| !e.pilot_reply.is_empty())
        || state.session.taxi_clearance.pending_readback
}

/// A short answer belongs only to the current controller's current question.
pub fn assistance_answer(
    state: &EngineState,
    request: &openatc_core::state::Request,
) -> Option<bool> {
    let offer = state.flow.discipline.assistance.as_ref()?;
    if request.role != "atc"
        || !matches!(request.intent.as_str(), "conversation" | "progressive")
        || !taxi_monitoring(state)
        || !state.session.telemetry.position_valid
        || state.session.telemetry.paused
        || !state.session.telemetry.radio_power
        || offer.route_sequence != state.session.taxi_clearance.sequence
        || Some(offer.frequency) != state.session.control_frequency()
        || waiting_for_reply(state)
        || state
            .session
            .transcript
            .iter()
            .rev()
            .find(|e| e.is_controller_instruction())
            .is_none_or(|e| e.sequence != offer.sequence)
    {
        return None;
    }
    let stations = nearby(&state.stations, &state.session.telemetry);
    let station = tuned(&stations, offer.frequency)?;
    if crate::flight::station_key(station) != offer.station
        || station.airport != state.session.taxi_clearance.airport
    {
        return None;
    }
    if request.intent == "progressive" {
        return Some(true);
    }
    let text = request
        .text
        .to_lowercase()
        .replace(&station.name.to_lowercase(), "")
        .replace(&state.session.plan.callsign.to_lowercase(), "");
    let words = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    match words.as_str() {
        "yes" | "yes please" | "affirm" | "affirmative" | "affirmative please" | "stopped"
        | "holding position" => Some(true),
        "no" | "negative" | "no thanks" | "no thank you" => Some(false),
        _ => None,
    }
}

pub fn answer_assistance(
    state: &mut EngineState,
    station: &Station,
    request: &openatc_core::state::Request,
) -> Option<bool> {
    let accept = assistance_answer(state, request)?;
    let mut replacement = None;
    let phrase = if !accept {
        "taxi_assistance_declined"
    } else if state.session.telemetry.ground_speed_knots > 1.0
        || state.session.taxi_clearance.traffic_hold
        || state.session.taxi_clearance.parking_hold
    {
        "taxi_assistance_stop"
    } else {
        let old = &state.session.taxi_clearance;
        replacement = state
            .airport
            .as_ref()
            .filter(|a| a.icao == old.airport)
            .and_then(|a| {
                openatc_core::airport::calculate_taxi_route(
                    a,
                    &state.session.telemetry,
                    &old.destination,
                    old.to_parking,
                    openatc_core::parking::aircraft_size(&state.session).unwrap_or('F'),
                )
                .ok()
            });
        if replacement.is_some() {
            "taxi_assistance_revised"
        } else {
            "taxi_assistance_unavailable"
        }
    };
    let callsign = state.session.plan.callsign.clone();
    let pilot = openatc_core::state::SpeechTag {
        radio: Some(request.radio),
        voice: state.settings.pilot_voice.clone(),
        speed: state.settings.pilot_speed,
        ..Default::default()
    };
    openatc_core::apply::add_transmission(&mut state.session, &callsign, &request.text, &pilot);
    let mut slots = vec![("callsign", callsign)];
    if let Some(mut route) = replacement {
        route.sequence = state.session.next_sequence;
        route.pending_readback = true;
        slots.push(("instructions", route.instructions.clone()));
        state.session.taxi_clearance = route;
    }
    state.flow.discipline.assistance = None;
    transmit(state, station, phrase, &slots);
    if let Some(reply) = state.session.transcript.last_mut() {
        reply.radio = Some(request.radio);
    }
    if phrase == "taxi_assistance_stop" {
        state.flow.discipline.assistance = Some(TaxiAssistance {
            sequence: state.session.next_sequence - 1,
            route_sequence: state.session.taxi_clearance.sequence,
            frequency: station.khz,
            station: crate::flight::station_key(station),
        });
    }
    Some(phrase != "taxi_assistance_unavailable")
}

fn transmit(state: &mut EngineState, station: &Station, phrase: &str, slots: &[(&str, String)]) {
    if phrase == "monitor_landing_without_clearance" {
        state.flow.vacate_reported = true;
    }
    let mut tag = crate::flight::tag(state, station);
    if phrase == "monitor_runway_entry" {
        tag.urgent = true;
        tag.delivery = "urgent".into();
    }
    let text = openatc_core::dialogue::say(phrase, slots);
    openatc_core::apply::add_transmission(&mut state.session, "ATC", &text, &tag);
    if phrase == "monitor_taxi_position" {
        state.flow.discipline.assistance = Some(TaxiAssistance {
            sequence: state.session.next_sequence - 1,
            route_sequence: state.session.taxi_clearance.sequence,
            frequency: station.khz,
            station: crate::flight::station_key(station),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::{
        airport::Airport,
        apply::RunwaySequence,
        state::{Clearance, SpeechTag, TaxiClearance},
    };

    fn state() -> EngineState {
        let mut s = crate::test_state();
        s.session.demo = false;
        s.session.plan.departure = "TEST".into();
        s.session.plan.runway = "09".into();
        s.session.plan.callsign = "TEST1".into();
        s.session
            .on_event(openatc_core::aircraft::AircraftEvent::Taxi);
        s.session.telemetry.com1_khz = 121_900;
        s.session.telemetry.position_valid = true;
        s.session.telemetry.latitude = 0.001;
        s.session.telemetry.longitude = 0.005;
        s.session.clearance = Some(Clearance {
            squawk: "2105".into(),
            acknowledged: true,
            ..Default::default()
        });
        s.stations = vec![Station {
            airport: "TEST".into(),
            service: "Ground".into(),
            name: "Test Ground".into(),
            khz: 121_900,
            ..Default::default()
        }];
        s.airport = Some(Airport {
            icao: "TEST".into(),
            runways: vec![Runway {
                first_name: "09".into(),
                second_name: "27".into(),
                first: Point::default(),
                second: Point {
                    east: 2000.0,
                    ..Default::default()
                },
                width: 45.0,
                ..Default::default()
            }],
            ..Default::default()
        });
        s
    }

    #[test]
    #[ignore = "requires installed X-Plane scenery"]
    fn nice_reported_taxi_position_geometry() {
        let root = std::env::var("OPENATC_SIM_ROOT").unwrap();
        let airport = openatc_core::airport::load_airport_from_simulator(
            &root,
            "LFMN",
            &|p| std::path::Path::new(p).is_file(),
            &|p| std::fs::read_to_string(p).map_err(|e| e.to_string()),
        )
        .unwrap();
        for (latitude, longitude) in [(43.652_675, 7.205_204), (43.655_753, 7.202_489)] {
            let point = airport_point(&airport, latitude, longitude);
            for runway in &airport.runways {
                let dx = runway.second.east - runway.first.east;
                let dn = runway.second.north - runway.first.north;
                let cross = ((point.east - runway.first.east) * dn
                    - (point.north - runway.first.north) * dx)
                    .abs()
                    / dx.hypot(dn);
                eprintln!(
                    "{latitude},{longitude}: {}/{} offset={cross:.1}m half-width={:.1}m on_pavement={}",
                    runway.first_name,
                    runway.second_name,
                    runway.width / 2.0,
                    on_pavement(runway, point)
                );
            }
            let t = openatc_core::state::Telemetry {
                latitude,
                longitude,
                on_ground: true,
                position_valid: true,
                ..Default::default()
            };
            let route =
                openatc_core::airport::calculate_taxi_route(&airport, &t, "04R", false, 'C');
            eprintln!(
                "Taxi result: {:?}",
                route
                    .as_ref()
                    .map(|route| (&route.instructions, &route.hold_short_runway))
            );
            if longitude > 7.205 {
                assert_eq!(
                    openatc_core::airport::runway_at_point(&airport, &point)
                        .unwrap()
                        .first_name,
                    "04L"
                );
                assert!(route.is_err());
            } else {
                assert!(openatc_core::airport::runway_at_point(&airport, &point).is_none());
                assert_eq!(route.unwrap().hold_short_runway, "04L");
            }
        }
        let mut s = state();
        s.session.plan.departure = "LFMN".into();
        s.session.plan.runway = "04R".into();
        s.session
            .on_event(openatc_core::aircraft::AircraftEvent::PushbackComplete);
        s.session.telemetry.latitude = 43.652_675;
        s.session.telemetry.longitude = 7.205_204;
        s.session.telemetry.on_ground = true;
        let request = openatc_core::state::Request {
            intent: "taxi".into(),
            ..Default::default()
        };
        let realism = Default::default();
        let tag = SpeechTag::default();
        let region = openatc_core::regions::region_for(&s.regions, "LFMN");
        let ctx = openatc_core::apply::RequestContext {
            squawk: "1234",
            runway_taxi_authorized: false,
            request: &request,
            airport: Some(&airport),
            atc_tag: &tag,
            pilot_tag: &tag,
            realism: &realism,
            units: openatc_core::UnitSystem::Imperial,
            region: &region,
            traffic: None,
            runway_sequence: RunwaySequence::Clear,
        };
        let outcome = openatc_core::apply::apply_request(&mut s.session, &ctx);
        assert!(!outcome.accepted);
        assert!(outcome.message.contains("04L/22R"), "{}", outcome.message);
        assert!(!outcome.message.to_lowercase().contains("backtrack"));
    }

    #[test]
    fn squawk_waits_for_adjustment_and_radio_then_stops_when_corrected() {
        let mut s = state();
        s.settings.monitor_transponder = true;
        s.session.telemetry.transponder_code = Some(1200);
        let now = Instant::now();
        assert!(!tick_at(&mut s, true, now));
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(29)));
        assert!(!tick_at(&mut s, false, now + Duration::from_secs(35)));
        assert!(tick_at(&mut s, true, now + Duration::from_secs(36)));
        assert!(s.session.transcript.last().unwrap().text.contains("2105"));
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(60)));
        s.session.telemetry.transponder_code = Some(2105);
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(120)));
    }

    #[test]
    fn transponder_reminders_are_bounded_and_spaced() {
        let mut s = state();
        s.settings.monitor_transponder = true;
        s.session.telemetry.transponder_code = Some(1200);
        let now = Instant::now();
        assert!(!tick_at(&mut s, true, now));
        assert!(tick_at(&mut s, true, now + Duration::from_secs(31)));
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(100)));
        assert!(tick_at(&mut s, true, now + Duration::from_secs(151)));
        for second in [300, 400, 500] {
            assert!(!tick_at(&mut s, true, now + Duration::from_secs(second)));
        }
        assert_eq!(
            s.session
                .transcript
                .iter()
                .filter(|entry| entry.text.contains("2105"))
                .count(),
            2
        );
        // Correcting the code ends the reminder; a new mismatch starts a fresh bounded count.
        s.session.telemetry.transponder_code = Some(2105);
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(600)));
        s.session.telemetry.transponder_code = Some(1200);
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(700)));
        assert!(tick_at(&mut s, true, now + Duration::from_secs(731)));
    }

    #[test]
    fn unavailable_invalid_and_emergency_codes_are_not_routine_mismatches() {
        let mut s = state();
        s.settings.monitor_transponder = true;
        for code in [
            None,
            Some(-1),
            Some(1280),
            Some(7500),
            Some(7600),
            Some(7700),
        ] {
            s.session.telemetry.transponder_code = code;
            assert!(transponder_issue(&s).is_empty());
        }
        s.session.telemetry.transponder_code = Some(2105);
        s.session.telemetry.transponder_mode = Some(1);
        assert!(transponder_issue(&s).is_empty());
        s.session.telemetry.on_ground = false;
        s.session.telemetry.height_agl_feet = 500.0;
        assert_eq!(transponder_issue(&s), "altitude");
        s.session.telemetry.transponder_mode = Some(7);
        assert!(transponder_issue(&s).is_empty());
    }

    #[test]
    fn paused_time_does_not_count_towards_transponder_warning() {
        let mut s = state();
        s.settings.monitor_transponder = true;
        s.session.telemetry.transponder_code = Some(1200);
        let now = Instant::now();
        assert!(!tick_at(&mut s, true, now));
        s.session.telemetry.paused = true;
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(20)));
        s.session.telemetry.paused = false;
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(100)));
        assert!(tick_at(&mut s, true, now + Duration::from_secs(131)));
    }

    #[test]
    fn runway_entry_requires_permission_for_that_physical_runway() {
        let mut s = state();
        s.settings.monitor_runway_permissions = true;
        let now = Instant::now();
        assert!(!tick_at(&mut s, true, now));
        s.session.telemetry.latitude = 0.0;
        s.session.taxi_clearance = TaxiClearance {
            approved: true,
            airport: "TEST".into(),
            crossing_runway: "18".into(),
            ..Default::default()
        };
        assert!(tick_at(&mut s, true, now));
        assert!(s.session.transcript.last().unwrap().text.contains("09/27"));
        assert!(!tick_at(&mut s, true, now));
        s.session.taxi_clearance.crossing_runway = "27".into();
        assert!(unauthorized_runway(&s).is_none());
        s.session.taxi_clearance.crossing_runway.clear();
        s.flow.takeoff_cleared = true;
        assert!(unauthorized_runway(&s).is_none());
    }

    #[test]
    fn takeoff_warning_survives_busy_radio_and_does_not_command_an_abort() {
        let mut s = state();
        s.settings.monitor_runway_permissions = true;
        let now = Instant::now();
        assert!(!tick_at(&mut s, true, now));
        s.session.telemetry.latitude = 0.0;
        s.session.telemetry.ground_speed_knots = 100.0;
        assert!(!tick_at(&mut s, true, now));
        s.session.telemetry.on_ground = false;
        s.session.telemetry.height_agl_feet = 300.0;
        s.session.has_departed = true;
        assert!(!tick_at(&mut s, false, now));
        assert!(tick_at(&mut s, true, now));
        let text = &s.session.transcript.last().unwrap().text;
        assert!(text.contains("intentions"));
        assert!(!text.contains("stop"));
        assert!(!tick_at(&mut s, true, now));
    }

    #[test]
    fn landing_warning_waits_for_rollout_and_respects_clearance() {
        for cleared in [false, true] {
            let mut s = state();
            s.settings.monitor_runway_permissions = true;
            s.flow.landing_cleared = cleared;
            s.session.telemetry.on_ground = false;
            s.session.has_departed = true;
            let now = Instant::now();
            assert!(!tick_at(&mut s, true, now));
            s.session.telemetry.on_ground = true;
            s.session
                .on_event(openatc_core::aircraft::AircraftEvent::Landed);
            s.session.telemetry.ground_speed_knots = 80.0;
            assert!(!tick_at(&mut s, true, now));
            s.session.telemetry.ground_speed_knots = 25.0;
            assert_eq!(tick_at(&mut s, true, now), !cleared);
        }
    }

    fn taxi_state() -> EngineState {
        let mut s = state();
        s.settings.monitor_taxi_route = true;
        s.session.taxi_clearance = TaxiClearance {
            sequence: 10,
            airport: "TEST".into(),
            approved: true,
            points: vec![
                Point::default(),
                Point {
                    east: 2000.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        s.session.next_sequence = 10;
        openatc_core::apply::add_transmission(
            &mut s.session,
            "ATC",
            "Taxi via Alpha",
            &SpeechTag {
                position: "Test Ground".into(),
                ..Default::default()
            },
        );
        s
    }

    #[test]
    fn apron_join_does_not_warn_until_the_aircraft_leaves_its_connector() {
        let mut state = taxi_state();
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Taxi);
        state.session.telemetry.longitude = 0.0;
        state.session.telemetry.latitude = 300.0 / 111_320.0;
        let mut movement = TaxiMovement::default();
        movement.observe(&state);
        state.session.telemetry.ground_speed_knots = 5.0;
        for north in [280.0, 250.0, 150.0, 70.0] {
            state.session.telemetry.latitude = north / 111_320.0;
            movement.observe(&state);
            assert!(off_route(&state));
            assert!(movement.joining_route(&state));
        }
        state.session.telemetry.longitude = 100.0 / 111_320.0;
        assert!(!movement.joining_route(&state));
        state.session.telemetry.longitude = 0.0;
        state.session.telemetry.latitude = 30.0 / 111_320.0;
        movement.observe(&state);
        assert!(movement.joined_route);
        state.session.telemetry.latitude = 150.0 / 111_320.0;
        assert!(off_route(&state));
        assert!(!movement.joining_route(&state));
    }

    #[test]
    fn parked_aircraft_and_small_speed_fluctuations_do_not_trigger_taxi_warnings() {
        for phase in [PhaseCode::Taxi, PhaseCode::TaxiIn] {
            let mut s = taxi_state();
            s.session.phase = phase;
            let now = Instant::now();
            for (seconds, speed) in [
                (0, 0.0),
                (16, 0.0),
                (80, 0.2),
                (160, 1.5),
                (180, 1.5),
                (240, 0.0),
            ] {
                s.session.telemetry.ground_speed_knots = speed;
                assert!(!tick_at(&mut s, true, now + Duration::from_secs(seconds)));
            }
            assert_eq!(s.session.transcript.len(), 1);
        }
    }

    #[test]
    fn taxi_assistance_waits_for_motion_and_stays_with_issuing_controller() {
        let mut s = taxi_state();
        let now = Instant::now();
        assert!(!tick_at(&mut s, true, now));
        s.session.telemetry.ground_speed_knots = 3.0;
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(20)));
        s.session.telemetry.longitude += 0.0001;
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(25)));
        s.session.telemetry.ground_speed_knots = 0.0;
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(39)));
        assert!(!tick_at(&mut s, false, now + Duration::from_secs(41)));
        assert!(tick_at(&mut s, true, now + Duration::from_secs(42)));
        s.stations[0].name = "Other Ground".into();
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(120)));
        s.session.telemetry.latitude = 0.0001;
        assert!(!off_route(&s));
    }

    #[test]
    fn pre_readback_motion_does_not_arm_a_new_taxi_route() {
        let mut s = taxi_state();
        let now = Instant::now();
        s.session.taxi_clearance.pending_readback = true;
        s.session.telemetry.ground_speed_knots = 4.0;
        assert!(!tick_at(&mut s, true, now));
        s.session.telemetry.longitude += 0.001;
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(30)));
        s.session.taxi_clearance.pending_readback = false;
        s.session.telemetry.ground_speed_knots = 0.0;
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(60)));
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(90)));
        s.session.telemetry.ground_speed_knots = 3.0;
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(91)));
        s.session.telemetry.longitude += 0.0001;
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(95)));
        assert!(s.flow.discipline.taxi_movement.started);
        s.session.taxi_clearance.sequence += 1;
        s.session.telemetry.ground_speed_knots = 0.0;
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(130)));
        assert!(!tick_at(&mut s, true, now + Duration::from_secs(200)));
    }

    #[tokio::test]
    async fn typed_yes_uses_the_pending_question_without_an_ai_call() {
        for (airport_id, phase, latitude, longitude, destination) in [
            ("TEST", PhaseCode::Taxi, 0.006, 0.002, "09R"),
            ("DEST", PhaseCode::TaxiIn, 0.001, 4.012, "Gate C1"),
        ] {
            let mut s = taxi_state();
            let airport = openatc_core::airport::load_airport(
                include_str!("../../../xtask/fixtures/flight-apt.dat"),
                airport_id,
            )
            .unwrap();
            s.settings.simulator_root = "synthetic".into();
            s.station_root = s.settings.simulator_root.clone();
            s.settings.ai_enabled = true;
            s.settings.ai_url = "http://127.0.0.1:1".into();
            s.settings.ai_model = "must-not-be-called".into();
            s.settings.congestion = openatc_settings::Congestion::Off;
            s.voices_source = s.settings.tts_url.clone();
            s.voices_refreshed = Some(Instant::now());
            s.session.phase = phase;
            s.session.plan.aircraft = "A320".into();
            s.session.telemetry.latitude = latitude;
            s.session.telemetry.longitude = longitude;
            s.session.telemetry.traffic = Some(vec![]);
            s.session.taxi_clearance.airport = airport_id.into();
            s.session.taxi_clearance.destination = destination.into();
            s.session.taxi_clearance.to_parking = phase == PhaseCode::TaxiIn;
            s.airport = Some(airport);
            s.stations[0].airport = airport_id.into();
            s.stations[0].latitude = latitude;
            s.stations[0].longitude = longitude;
            let station = s.stations[0].clone();
            transmit(
                &mut s,
                &station,
                "monitor_taxi_position",
                &[("callsign", "TEST1".into())],
            );
            let shared = std::sync::Arc::new(tokio::sync::RwLock::new(s));
            let response = crate::routes::post_request(
                axum::extract::State(shared.clone()),
                axum::Json(serde_json::json!({"text":"yes","role":"atc"})),
            )
            .await;
            assert!(response.status().is_success());
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(result["result"]["accepted"], true, "{result}");
            let s = shared.read().await;
            let route = &s.session.taxi_clearance;
            assert!(route.pending_readback);
            assert!(!route.approved);
            assert_eq!(route.hold_short_runway, "09L");
            assert!(route.crossing_runway.is_empty());
            assert_eq!(route.destination, destination);
            assert_eq!(
                route.sequence,
                s.session.transcript.last().unwrap().sequence
            );
            assert_eq!(
                s.session
                    .transcript
                    .iter()
                    .filter(|e| e.text == "yes")
                    .count(),
                1
            );
            assert!(openatc_core::readback::auto_reply(&s.session).is_some());
        }
    }

    #[test]
    fn short_answers_cannot_accept_old_questions_or_clearances() {
        let mut s = taxi_state();
        let station = s.stations[0].clone();
        let mut request = openatc_core::state::Request {
            role: "atc".into(),
            intent: "conversation".into(),
            text: "yes".into(),
            ..Default::default()
        };
        assert_eq!(assistance_answer(&s, &request), None);
        transmit(
            &mut s,
            &station,
            "monitor_taxi_position",
            &[("callsign", "TEST1".into())],
        );
        for text in ["yes", "Yes, please!", "Test Ground, TEST1, affirmative."] {
            request.text = text.into();
            assert_eq!(assistance_answer(&s, &request), Some(true));
        }
        request.text = "no thanks".into();
        assert_eq!(assistance_answer(&s, &request), Some(false));
        request.text = "yes, cross runway 09".into();
        assert_eq!(assistance_answer(&s, &request), None);
        request.text = "yes".into();
        s.session.telemetry.com1_khz = 118_700;
        assert_eq!(assistance_answer(&s, &request), None);
        s.session.telemetry.com1_khz = station.khz;
        s.session.taxi_clearance.pending_readback = true;
        assert_eq!(assistance_answer(&s, &request), None);
        s.session.taxi_clearance.pending_readback = false;
        s.session.taxi_clearance.sequence += 1;
        assert_eq!(assistance_answer(&s, &request), None);
        s.session.taxi_clearance.sequence -= 1;
        transmit(
            &mut s,
            &station,
            "monitor_squawk",
            &[("callsign", "TEST1".into()), ("squawk", "2105".into())],
        );
        assert_eq!(assistance_answer(&s, &request), None);
    }
}
