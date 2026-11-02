//! Concurrent traffic encounters and radio calls, separate from clearance readbacks.
use super::{
    Duration, EngineState, Instant, Station, TrafficTarget, add_transmission, nearby, say, tuned,
};
use openatc_core::{
    state::{Request, SpeechTag, TrafficCall},
    traffic::advisory::{Encounter, encounter},
};

pub(super) struct Call {
    identity: String,
    station: String,
    at: Option<Instant>,
    clock: u8,
    urgent: bool,
    sighted: bool,
    clear_since: Option<Instant>,
    missing: bool,
}

fn slots(state: &EngineState, target: &TrafficTarget, e: Encounter) -> Vec<(&'static str, String)> {
    let direction = [
        "northbound",
        "northeast_bound",
        "eastbound",
        "southeast_bound",
        "southbound",
        "southwest_bound",
        "westbound",
        "northwest_bound",
    ][usize::from(e.direction)];
    let range = if e.distance_nm < 1.0 {
        say("traffic_range_less_than_mile", &[])
    } else {
        say(
            "traffic_range_miles",
            &[("distance", format!("{:.0}", e.distance_nm))],
        )
    };
    vec![
        ("callsign", state.session.plan.callsign.clone()),
        ("clock", e.clock.to_string()),
        ("range", range),
        ("movement", say(&format!("traffic_{direction}"), &[])),
        (
            "aircraft",
            if target.aircraft.is_empty() {
                say("traffic_type_unknown", &[])
            } else {
                target.aircraft.clone()
            },
        ),
        (
            "altitude",
            format!("{:.0}", (target.altitude_feet / 100.0).round() * 100.0),
        ),
    ]
}

fn refresh(state: &mut EngineState, station: &Station) -> Vec<(TrafficTarget, Encounter)> {
    let station_key = crate::flight::station_key(station);
    if state.session.telemetry.on_ground
        || !["Tower", "Departure", "Approach", "Center"].contains(&station.service.as_str())
    {
        state.traffic.advisories.clear();
        state.session.traffic_call = None;
        return vec![];
    }
    state
        .traffic
        .advisories
        .retain(|_, a| a.station == station_key);
    let targets = state.traffic_targets();
    let candidates: Vec<_> = targets
        .iter()
        .take(126)
        .filter(|t| t.id != 0)
        .filter_map(|t| {
            let climb = state.traffic.pilots.get(&t.id).and_then(|p| p.climb_fpm);
            Some((t.clone(), encounter(&state.session.telemetry, t, climb)?))
        })
        .collect();
    let enroute = station.service == "Center";
    for (id, active) in &mut state.traffic.advisories {
        let current = candidates
            .iter()
            .find(|(t, _)| t.id == *id && t.callsign == active.identity);
        if current.is_some_and(|(_, e)| e.warrants_advisory(enroute)) {
            active.clear_since = None;
        } else {
            active.clear_since.get_or_insert_with(Instant::now);
        }
        active.missing = current.is_none();
    }
    for (target, e) in &candidates {
        if !e.warrants_advisory(enroute) {
            continue;
        }
        if state
            .traffic
            .advisories
            .get(&target.id)
            .is_some_and(|a| a.identity != target.callsign)
        {
            state.traffic.advisories.remove(&target.id);
            if state
                .session
                .traffic_call
                .as_ref()
                .is_some_and(|c| c.target_id == target.id)
            {
                state.session.traffic_call = None;
            }
        }
        state
            .traffic
            .advisories
            .entry(target.id)
            .or_insert_with(|| Call {
                identity: target.callsign.clone(),
                station: station_key.clone(),
                at: None,
                clock: e.clock,
                urgent: false,
                sighted: false,
                clear_since: None,
                missing: false,
            });
    }
    // Expire unannounced observations without consuming airtime; cap remembered lost targets.
    state.traffic.advisories.retain(|_, a| {
        a.clear_since.is_none_or(|t| t.elapsed().as_secs() < 30)
            && (a.at.is_some() || a.clear_since.is_none())
    });
    while state.traffic.advisories.len() > 63 {
        let oldest = state
            .traffic
            .advisories
            .iter()
            .filter(|(_, a)| a.missing)
            .min_by_key(|(_, a)| a.clear_since)
            .map(|(id, _)| *id);
        if let Some(id) = oldest {
            state.traffic.advisories.remove(&id);
        } else {
            break;
        }
    }
    if state.session.traffic_call.as_ref().is_some_and(|c| {
        c.frequency_khz != station.khz || !state.traffic.advisories.contains_key(&c.target_id)
    }) {
        state.session.traffic_call = None;
    }
    candidates
}

fn transmit(
    state: &mut EngineState,
    station: &Station,
    key: &str,
    values: &[(&str, String)],
    urgent: bool,
) {
    let mut voice = crate::flight::tag(state, station);
    voice.urgent = urgent;
    if urgent {
        voice.delivery = "urgent".into();
    }
    add_transmission(&mut state.session, "ATC", &say(key, values), &voice);
    state
        .session
        .transcript
        .last_mut()
        .unwrap()
        .information_only = true;
}

fn announce(state: &mut EngineState, station: &Station, target: &TrafficTarget, e: Encounter) {
    let values = slots(state, target, e);
    transmit(
        state,
        station,
        if e.urgent() {
            "traffic_alert"
        } else {
            "traffic_advisory"
        },
        &values,
        e.urgent(),
    );
    let active = state.traffic.advisories.get_mut(&target.id).unwrap();
    active.at = Some(Instant::now());
    active.clock = e.clock;
    active.urgent = e.urgent();
    state.session.traffic_call = Some(TrafficCall {
        target_id: target.id,
        frequency_khz: station.khz,
        sighted: active.sighted,
    });
    state.traffic.last_advisory = Some(Instant::now());
}

/// Safety calls can interrupt speech, but cannot acknowledge or replace a clearance.
pub(crate) fn urgent(state: &mut EngineState) -> bool {
    let t = &state.session.telemetry;
    if state.session.demo || t.paused || !t.position_valid || !t.radio_power || t.on_ground {
        return false;
    }
    let stations = nearby(&state.stations, t);
    let Some(station) = state
        .session
        .control_frequency()
        .and_then(|frequency| tuned(&stations, frequency))
        .cloned()
    else {
        return false;
    };
    let candidates = refresh(state, &station);
    let candidate = candidates
        .iter()
        .filter(|(t, e)| {
            e.urgent()
                && state.traffic.advisories.get(&t.id).is_some_and(|a| {
                    !a.urgent || a.at.is_none_or(|at| at.elapsed().as_secs() >= 15)
                })
        })
        .min_by(|a, b| {
            a.1.seconds
                .total_cmp(&b.1.seconds)
                .then_with(|| a.1.distance_nm.total_cmp(&b.1.distance_nm))
        });
    let Some((target, e)) = candidate else {
        return false;
    };
    if state
        .traffic
        .last_urgent
        .is_some_and(|at| at.elapsed() < Duration::from_secs(3))
    {
        return false;
    }
    state.pending_copilot_reply = None;
    announce(state, &station, target, *e);
    state.traffic.last_urgent = Some(Instant::now());
    true
}

pub(super) fn tick(state: &mut EngineState, station: &Station) -> bool {
    let candidates = refresh(state, station);
    let enroute = station.service == "Center";
    let candidate = candidates
        .iter()
        .filter(|(t, e)| {
            e.warrants_advisory(enroute)
                && state.traffic.advisories.get(&t.id).is_some_and(|a| {
                    (e.urgent() && !a.urgent)
                        || a.at.is_none()
                        || (!a.sighted && a.at.is_some_and(|at| at.elapsed().as_secs() >= 30))
                })
        })
        .min_by(|a, b| {
            b.1.urgent()
                .cmp(&a.1.urgent())
                .then_with(|| {
                    state.traffic.advisories[&a.0.id]
                        .at
                        .cmp(&state.traffic.advisories[&b.0.id].at)
                })
                .then_with(|| a.1.seconds.total_cmp(&b.1.seconds))
        });
    if let Some((target, e)) = candidate
        && state
            .traffic
            .last_advisory
            .is_none_or(|at| at.elapsed().as_secs() >= 10)
    {
        announce(state, station, target, *e);
        return true;
    }
    let resolved = state
        .traffic
        .advisories
        .iter()
        .find(|(_, a)| {
            a.clear_since
                .is_some_and(|at| at.elapsed().as_secs() >= if a.missing { 8 } else { 5 })
        })
        .map(|(id, _)| *id);
    if let Some(id) = resolved {
        let a = state.traffic.advisories.remove(&id).unwrap();
        if state
            .session
            .traffic_call
            .as_ref()
            .is_some_and(|c| c.target_id == id)
        {
            state.session.traffic_call = None;
        }
        if a.at.is_some() && !a.sighted {
            transmit(
                state,
                station,
                if a.missing {
                    "traffic_no_longer_observed"
                } else {
                    "traffic_no_factor"
                },
                &[
                    ("callsign", state.session.plan.callsign.clone()),
                    ("clock", a.clock.to_string()),
                ],
                false,
            );
            return true;
        }
    }
    false
}

/// A sighting report does not accept visual separation or another pending clearance.
pub(crate) fn reply(state: &mut EngineState, station: &Station, request: &Request) -> Option<bool> {
    if !["traffic_sighted", "traffic_not_sighted"].contains(&request.intent.as_str()) {
        return None;
    }
    let id = state
        .session
        .traffic_call
        .as_ref()
        .filter(|c| c.frequency_khz == station.khz)
        .map(|c| c.target_id);
    let accepted = !state.session.telemetry.on_ground
        && id.is_some_and(|id| {
            state
                .traffic
                .advisories
                .get(&id)
                .is_some_and(|a| a.station == crate::flight::station_key(station))
        });
    if !request.controller_initiated {
        let call = state.session.plan.callsign.clone();
        add_transmission(
            &mut state.session,
            &call,
            &request.text,
            &SpeechTag {
                radio: Some(request.radio),
                ..Default::default()
            },
        );
    }
    let key = if accepted {
        let sighted = request.intent == "traffic_sighted";
        state.session.traffic_call.as_mut().unwrap().sighted = sighted;
        state
            .traffic
            .advisories
            .get_mut(&id.unwrap())
            .unwrap()
            .sighted = sighted;
        if sighted {
            "traffic_sighting_acknowledged"
        } else {
            "traffic_keep_looking"
        }
    } else {
        "traffic_no_active_advisory"
    };
    transmit(
        state,
        station,
        key,
        &[("callsign", state.session.plan.callsign.clone())],
        false,
    );
    if let Some(reply) = state.session.transcript.last_mut() {
        reply.radio = Some(request.radio);
    }
    Some(accepted)
}
#[cfg(test)]
mod tests {
    use super::super::atc;
    use super::*;
    #[test]
    fn urgent_second_encounter_interrupts_audio_without_consuming_readback() {
        let (mut s, station) = setup();
        s.stations = vec![station.clone()];
        s.session.demo = false;
        assert!(tick(&mut s, &station));
        atc(
            &mut s,
            &station,
            "traffic_reduce_speed",
            &[("callsign", "VH-BIL".into()), ("speed", "180".into())],
            "pilot_speed_readback",
        );
        let pending = openatc_core::readback::auto_reply(&s.session).unwrap();
        s.pending_copilot_reply = Some(crate::routes::PreparedReply {
            generation: s.session_generation,
            sequence: s.session.next_sequence,
            frequency: s.session.telemetry.com1_khz,
            request: pending.clone(),
        });
        s.session.telemetry.radio_busy = true;
        let mut second = s.session.telemetry.traffic.as_ref().unwrap()[0].clone();
        second.id = 25;
        second.callsign = "QFA123".into();
        second.latitude = 0.5 / 60.0;
        s.session.telemetry.traffic.as_mut().unwrap().push(second);
        assert!(urgent(&mut s));
        assert_eq!(s.session.traffic_call.as_ref().unwrap().target_id, 25);
        assert_eq!(s.traffic.advisories.len(), 2);
        assert!(s.pending_copilot_reply.is_none());
        let warning = s.session.transcript.last().unwrap();
        assert!(warning.urgent && warning.information_only);
        assert_eq!(
            openatc_core::readback::auto_reply(&s.session).unwrap().text,
            pending.text
        );
        assert!(!urgent(&mut s), "Urgent calls must not flood the radio");
        s.session.telemetry.paused = true;
        s.traffic.last_urgent = None;
        s.traffic.advisories.get_mut(&25).unwrap().urgent = false;
        assert!(!urgent(&mut s));
    }

    #[test]
    fn routine_encounters_take_turns_and_reused_slots_do_not_inherit_sightings() {
        let (mut s, station) = setup();
        let mut second = s.session.telemetry.traffic.as_ref().unwrap()[0].clone();
        second.id = 25;
        second.callsign = "QFA123".into();
        s.session.telemetry.traffic.as_mut().unwrap().push(second);
        assert!(tick(&mut s, &station));
        assert_eq!(s.session.traffic_call.as_ref().unwrap().target_id, 24);
        assert!(!tick(&mut s, &station));
        s.traffic.last_advisory = None;
        assert!(tick(&mut s, &station));
        assert_eq!(s.session.traffic_call.as_ref().unwrap().target_id, 25);
        assert_eq!(
            reply(
                &mut s,
                &station,
                &Request {
                    intent: "traffic_sighted".into(),
                    ..Default::default()
                }
            ),
            Some(true)
        );
        s.session.telemetry.traffic.as_mut().unwrap()[1].callsign = "JST321".into();
        s.traffic.last_advisory = None;
        assert!(tick(&mut s, &station));
        assert!(!s.session.traffic_call.as_ref().unwrap().sighted);
    }
    fn setup() -> (EngineState, Station) {
        let mut s = crate::test_state();
        s.session.telemetry = openatc_core::state::Telemetry {
            position_valid: true,
            on_ground: false,
            altitude_feet: 5000.0,
            ground_speed_knots: 150.0,
            com1_khz: 125_000,
            traffic: Some(vec![TrafficTarget {
                id: 24,
                callsign: "VH-BIL".into(),
                aircraft: "PA28".into(),
                latitude: 8.0 / 60.0,
                altitude_feet: 5000.0,
                track_degrees: 180.0,
                ground_speed_knots: Some(150.0),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let station = Station {
            service: "Approach".into(),
            name: "Approach".into(),
            khz: 125_000,
            ..Default::default()
        };
        (s, station)
    }
    #[test]
    fn advisory_report_and_clear_scan_complete_without_chatter_enabled() {
        let (mut s, station) = setup();
        s.settings.hear_traffic_radio = false;
        assert!(!s.settings.hear_traffic_radio);
        assert!(tick(&mut s, &station));
        assert_eq!(s.session.traffic_call.as_ref().unwrap().target_id, 24);
        assert!(
            s.session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("12 o'clock")
        );
        assert!(s.session.transcript.last().unwrap().pilot_reply.is_empty());
        let r = Request {
            intent: "traffic_not_sighted".into(),
            text: "Negative contact".into(),
            ..Default::default()
        };
        assert_eq!(reply(&mut s, &station, &r), Some(true));
        s.session.telemetry.traffic.as_mut().unwrap()[0].altitude_feet = 10000.0;
        assert!(!tick(&mut s, &station));
        s.traffic.advisories.get_mut(&24).unwrap().clear_since =
            Some(Instant::now().checked_sub(Duration::from_secs(6)).unwrap());
        assert!(tick(&mut s, &station));
        assert!(
            s.session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("no factor")
        );
        assert!(s.session.traffic_call.is_none());
    }
    #[test]
    fn missing_feed_is_not_reported_clear_and_wrong_station_cannot_acknowledge() {
        let (mut s, station) = setup();
        assert!(tick(&mut s, &station));
        let mut other = station.clone();
        other.khz = 126_000;
        let r = Request {
            intent: "traffic_sighted".into(),
            ..Default::default()
        };
        assert_eq!(reply(&mut s, &other, &r), Some(false));
        assert!(!s.session.traffic_call.as_ref().unwrap().sighted);
        s.session.telemetry.traffic = None;
        assert!(!tick(&mut s, &station));
        s.traffic.advisories.get_mut(&24).unwrap().clear_since =
            Some(Instant::now().checked_sub(Duration::from_secs(9)).unwrap());
        assert!(tick(&mut s, &station));
        assert!(
            s.session
                .transcript
                .last()
                .unwrap()
                .text
                .contains("no longer observed")
        );
    }

    #[test]
    fn sighting_does_not_remove_a_later_instruction_readback() {
        let (mut s, station) = setup();
        assert!(tick(&mut s, &station));
        atc(
            &mut s,
            &station,
            "traffic_reduce_speed",
            &[("callsign", "VH-BIL".into()), ("speed", "180".into())],
            "pilot_speed_readback",
        );
        let sequence = s.session.transcript.last().unwrap().sequence;
        let r = Request {
            intent: "traffic_sighted".into(),
            text: "Traffic in sight".into(),
            ..Default::default()
        };
        assert_eq!(reply(&mut s, &station, &r), Some(true));
        assert!(s.session.traffic_call.as_ref().unwrap().sighted);
        let pending = s
            .session
            .transcript
            .iter()
            .find(|t| t.sequence == sequence)
            .unwrap();
        assert!(!pending.pilot_reply.is_empty());
        assert_eq!(
            s.session
                .transcript
                .iter()
                .rev()
                .find(|t| t.is_controller_instruction())
                .unwrap()
                .sequence,
            sequence
        );
    }
}
