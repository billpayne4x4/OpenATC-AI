//! Optional en-route offers. The pilot still requests and reads back any amendment.
use crate::{EngineState, Shared};
use axum::{
    Json,
    extract::State as AxumState,
    response::{IntoResponse, Response},
};
#[cfg(test)]
use openatc_core::aircraft::AircraftBehaviour;
use openatc_core::{
    apply::add_transmission,
    dialogue::say,
    state::{AtcOffer, PhaseCode, Request, SpeechTag},
    stations,
};
use serde_json::{Value, json};
use std::time::Instant;

#[derive(Default)]
pub struct Monitor {
    roll_generation: Option<u64>,
    pub(crate) chat_allowed: bool,
    shortcut_attempts: std::collections::BTreeSet<(usize, usize)>,
    last_roll: Option<Instant>,
    offered: Option<Instant>,
    previous_fix: String,
}

/// Enable optional offers after the first handoff; check eligibility before rolling.
pub fn roll_for_flight(state: &mut EngineState) {
    if state.initiative.roll_generation == Some(state.session_generation) {
        return;
    }
    state.initiative.roll_generation = Some(state.session_generation);
    state.initiative.chat_allowed = true;
    state.initiative.shortcut_attempts.clear();
    state.initiative.last_roll = None;
}

pub(crate) fn roll_percent(percent: u8) -> bool {
    percent >= 100 || (percent > 0 && rand::random::<f64>() * 100.0 < f64::from(percent))
}

fn candidate(state: &EngineState, index: usize) -> bool {
    let fixes = &state.session.plan.fixes;
    let next = state.flow.monitor.next_fix.max(1);
    let Some(fix) = fixes
        .get(index)
        .filter(|_| index > next && next < fixes.len())
    else {
        return false;
    };
    if fixes[next..=index].iter().any(|f| {
        f.minimum_altitude_feet.is_some()
            || f.maximum_altitude_feet.is_some()
            || f.minimum_speed_knots.is_some()
            || f.maximum_speed_knots.is_some()
    }) {
        return false;
    }
    let t = &state.session.telemetry;
    let own = [t.latitude, t.longitude];
    let target = [fix.latitude, fix.longitude];
    let direct = openatc_core::arrival::distance_nm(own, target);
    let Some(floor) = state.mora.corridor(own, target) else {
        return false;
    };
    if t.altitude_feet < f64::from(floor) || !(5.0..=100.0).contains(&direct) {
        return false;
    }
    let mut prior = own;
    let original: f64 = fixes[next..=index]
        .iter()
        .map(|f| {
            let at = [f.latitude, f.longitude];
            let distance = openatc_core::arrival::distance_nm(prior, at);
            prior = at;
            distance
        })
        .sum();
    let Some(arrival) = state.arrival_airport.as_ref() else {
        return false;
    };
    let remaining = direct
        + fixes[index..]
            .windows(2)
            .map(|w| {
                openatc_core::arrival::distance_nm(
                    [w[0].latitude, w[0].longitude],
                    [w[1].latitude, w[1].longitude],
                )
            })
            .sum::<f64>()
        + fixes.last().map_or(0.0, |f| {
            openatc_core::arrival::distance_nm(
                [f.latitude, f.longitude],
                [arrival.reference_latitude, arrival.reference_longitude],
            )
        });
    original - direct >= 2.0
        && t.altitude_feet
            <= arrival.elevation_feet
                + ((remaining - 10.0).max(0.0) * openatc_core::arrival::gradient(3.0)).max(2000.0)
}

pub fn tick(state: &mut EngineState) {
    if !state.settings.atc_shortcut_offers
        || state.settings.atc_shortcut_chance_percent == 0
        || state.session.emergency_declared
        || state.session.phase != PhaseCode::Cruise
    {
        state.session.atc_offer = None;
        return;
    }
    if state
        .session
        .atc_offer
        .as_ref()
        .is_some_and(|o| Some(o.frequency_khz) != state.session.control_frequency())
        || state
            .initiative
            .offered
            .is_some_and(|at| at.elapsed().as_secs() > 90)
    {
        state.session.atc_offer = None;
    }
    if !crate::traffic::ready(state)
        || state.session.telemetry.on_ground
        || state.session.telemetry.vertical_speed_fpm.abs() > 200.0
        || state
            .session
            .clearance
            .as_ref()
            .is_none_or(|c| !c.acknowledged)
        || state.flow.monitor.heading.is_some()
        || state.flow.monitor.deviation.is_some()
        || state.session.atc_offer.is_some()
        || state
            .initiative
            .last_roll
            .is_some_and(|at| at.elapsed().as_secs() < 300)
    {
        return;
    }
    let stations = stations::nearby(&state.stations, &state.session.telemetry);
    let Some(station) = state
        .session
        .control_frequency()
        .and_then(|frequency| stations::tuned(&stations, frequency))
        .filter(|s| ["Center", "Departure", "Approach"].contains(&s.service.as_str()))
    else {
        return;
    };
    let next = state.flow.monitor.next_fix.max(1);
    let target = (next + 1..state.session.plan.fixes.len().min(next + 4)).find(|&i| {
        state.session.plan.fixes[i].identifier != state.initiative.previous_fix
            && !state.initiative.shortcut_attempts.contains(&(next, i))
            && candidate(state, i)
    });
    let Some(target) = target else {
        return;
    };
    state.initiative.shortcut_attempts.insert((next, target));
    state.initiative.last_roll = Some(Instant::now());
    if !roll_percent(state.settings.atc_shortcut_chance_percent) {
        return;
    }
    let waypoint = state.session.plan.fixes[target].identifier.clone();
    state.initiative.previous_fix.clone_from(&waypoint);
    state.initiative.offered = Some(Instant::now());
    let offer = AtcOffer {
        waypoint,
        sequence: state.session.next_sequence,
        frequency_khz: station.khz,
    };
    let voice = crate::flight::tag(state, station);
    let text = say(
        "atc_offer_direct",
        &[
            ("callsign", state.session.plan.callsign.clone()),
            ("waypoint", offer.waypoint.clone()),
        ],
    );
    add_transmission(&mut state.session, "ATC", &text, &voice);
    state.session.atc_offer = Some(offer);
}

pub async fn answer(AxumState(shared): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    let mut s = shared.write().await;
    let Some(offer) = s
        .session
        .atc_offer
        .clone()
        .filter(|o| body["sequence"].as_u64() == Some(u64::from(o.sequence)))
    else {
        return crate::routes::bad_request(openatc_core::dialogue::say(
            "notice_that_controller_offer_is_no_longer_available_119a06cd",
            &[],
        ));
    };
    let accept = body["accept"].as_bool();
    if accept.is_none()
        || !crate::traffic::ready(&s)
        || !s.settings.atc_shortcut_offers
        || s.session.control_frequency() != Some(offer.frequency_khz)
        || s.session.phase != PhaseCode::Cruise
        || s.initiative
            .offered
            .is_none_or(|t| t.elapsed().as_secs() > 90)
    {
        return crate::routes::bad_request(openatc_core::dialogue::say(
            "notice_wait_for_a_clear_radio_and_a_current_offer_from_this_con_5d31a8c7",
            &[],
        ));
    }
    let stations = stations::nearby(&s.stations, &s.session.telemetry);
    let Some(station) = stations::tuned(&stations, offer.frequency_khz) else {
        return crate::routes::bad_request(openatc_core::dialogue::say(
            "notice_controller_unavailable_842f8e10",
            &[],
        ));
    };
    let call = s.session.plan.callsign.clone();
    if accept == Some(false) {
        s.session.atc_offer = None;
        let text = say("pilot_decline_shortcut", &[("callsign", call.clone())]);
        let mut pilot_tag = SpeechTag::default();
        pilot_tag.radio = s.session.receiving_radio(offer.frequency_khz);
        pilot_tag.frequency = Some(offer.frequency_khz);
        add_transmission(&mut s.session, &call, &text, &pilot_tag);
        let voice = crate::flight::tag(&mut s, station);
        add_transmission(
            &mut s.session,
            "ATC",
            &say("atc_shortcut_declined", &[("callsign", call)]),
            &voice,
        );
        return Json(json!({"accepted":true})).into_response();
    }
    let Some(index) = s
        .session
        .plan
        .fixes
        .iter()
        .position(|f| f.identifier == offer.waypoint)
    else {
        return crate::routes::bad_request(openatc_core::dialogue::say(
            "notice_the_route_has_changed_e54a6e49",
            &[],
        ));
    };
    if !candidate(&s, index) {
        s.session.atc_offer = None;
        return crate::routes::bad_request(openatc_core::dialogue::say(
            "notice_the_shortcut_no_longer_meets_route_or_descent_limits_a371d2d4",
            &[],
        ));
    }
    let text = say(
        "pilot_accept_shortcut",
        &[
            ("station", station.name.clone()),
            ("callsign", call),
            ("waypoint", offer.waypoint.clone()),
        ],
    );
    s.session.atc_offer = None;
    drop(s);
    crate::routes::post_request(
        AxumState(shared),
        Json(json!(Request {
            intent: "direct".into(),
            waypoint: offer.waypoint,
            text,
            role: "atc".into(),
            ..Default::default()
        })),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opportunity_probability_uses_percent_and_flight_reset_clears_attempts() {
        assert!(!roll_percent(0));
        assert!(roll_percent(100));
        let mut state = crate::test_state();
        roll_for_flight(&mut state);
        state.initiative.shortcut_attempts.insert((1, 2));
        roll_for_flight(&mut state);
        assert!(state.initiative.shortcut_attempts.contains(&(1, 2)));
        state.session_generation += 1;
        roll_for_flight(&mut state);
        assert!(state.initiative.shortcut_attempts.is_empty());
    }

    #[tokio::test]
    async fn optional_shortcut_needs_terrain_restrictions_and_pilot_acceptance() {
        let mut s = crate::test_state();
        s.settings.atc_shortcut_offers = true;
        s.settings.atc_shortcut_chance_percent = 100;
        s.session
            .on_event(openatc_core::aircraft::AircraftEvent::Cruise);
        s.session.telemetry = openatc_core::state::Telemetry {
            latitude: 0.1,
            longitude: 0.1,
            altitude_feet: 5000.0,
            on_ground: false,
            position_valid: true,
            com1_khz: 123_100,
            ..Default::default()
        };
        s.session.clearance = Some(openatc_core::state::Clearance {
            acknowledged: true,
            ..Default::default()
        });
        s.session.plan.fixes = [
            (0.0, 0.0, "START"),
            (0.3, 0.1, "BEND"),
            (0.1, 0.7, "SHORT"),
            (0.1, 3.0, "END"),
        ]
        .into_iter()
        .map(
            |(latitude, longitude, identifier)| openatc_core::state::RouteFix {
                latitude,
                longitude,
                identifier: identifier.into(),
                ..Default::default()
            },
        )
        .collect();
        s.arrival_airport = Some(openatc_core::airport::Airport {
            reference_latitude: 0.1,
            reference_longitude: 3.1,
            ..Default::default()
        });
        s.stations = vec![stations::Station {
            service: "Center".into(),
            name: "Test Center".into(),
            khz: 123_100,
            latitude: 0.1,
            longitude: 0.1,
            ..Default::default()
        }];
        assert!(!candidate(&s, 2));
        s.mora.0.insert((0, 0), 1000);
        assert!(candidate(&s, 2));
        s.session.plan.fixes[1].minimum_altitude_feet = Some(4000);
        assert!(!candidate(&s, 2));
        s.session.plan.fixes[1].minimum_altitude_feet = None;
        s.session.plan.fixes[1].minimum_speed_knots = Some(210.0);
        assert!(!candidate(&s, 2));
        s.session.plan.fixes[1].minimum_speed_knots = None;
        let prior = s.session.clearance.clone();
        tick(&mut s);
        let offer = s.session.atc_offer.clone().expect("offer");
        assert_eq!(s.session.clearance, prior);
        assert!(s.session.transcript.last().unwrap().pilot_reply.is_empty());
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(s));
        assert!(
            answer(
                AxumState(shared.clone()),
                Json(json!({"sequence":offer.sequence,"accept":false}))
            )
            .await
            .status()
            .is_success()
        );
        assert!(shared.read().await.session.atc_offer.is_none());
        assert!(
            !answer(
                AxumState(shared.clone()),
                Json(json!({"sequence":offer.sequence,"accept":true}))
            )
            .await
            .status()
            .is_success()
        );
    }
}
