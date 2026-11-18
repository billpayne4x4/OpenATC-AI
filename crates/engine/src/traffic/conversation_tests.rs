#![allow(clippy::default_trait_access)]
use super::*;
use openatc_core::aircraft::AircraftBehaviour;

fn prepared(aircraft: &str) -> EngineState {
    let mut state = crate::test_state();
    state.settings.talk_to_traffic = true;
    state.settings.informal_pilot_chat = true;
    state.settings.pilot_chat_humour = true;
    state.settings.pilots_call_me = true;
    state
        .session
        .on_event(openatc_core::aircraft::AircraftEvent::Cruise);
    state.session.telemetry.position_valid = true;
    state.session.telemetry.radio_power = true;
    state.session.telemetry.altitude_feet = 10000.0;
    state.session.telemetry.com1_khz = 123_450;
    state.traffic.pilots.insert(
        42,
        Pilot {
            target: TrafficTarget {
                id: 42,
                callsign: "QFA42".into(),
                aircraft: aircraft.into(),
                on_ground: false,
                altitude_feet: 10000.0,
                ground_speed_knots: Some(150.0),
                com1_khz: Some(123_450),
                ..Default::default()
            },
            seen: Instant::now(),
            landing: None,
            voice: "test-voice".into(),
            climb_fpm: None,
            conversation: Default::default(),
        },
    );
    state
}

#[tokio::test]
async fn airline_turboprop_and_ga_pilots_reply_and_remember_their_conversation() {
    for aircraft in ["A320", "AT76", "PA28"] {
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(prepared(aircraft)));
        let response = conversation::request(
            shared.clone(),
            json!({"id":42,"message":"You deserve a beer after that crosswind landing"}),
        )
        .await;
        assert!(response.status().is_success());
        let state = shared.read().await;
        let reply = state.session.transcript.last().unwrap();
        assert_eq!(reply.speaker, "QFA42");
        assert_eq!(reply.voice, "test-voice");
        assert!(reply.text.contains("after the flight"));
        assert!(state.session.clearance.is_none());
        assert_eq!(state.traffic.pilots[&42].conversation.len(), 1);
        drop(state);
        assert!(
            conversation::request(
                shared.clone(),
                json!({"id":42,"message":"Thanks, have a good flight"})
            )
            .await
            .status()
            .is_success()
        );
        assert_eq!(
            shared.read().await.traffic.pilots[&42].conversation.len(),
            2
        );
    }
}

#[tokio::test]
async fn casual_chat_respects_settings_critical_phases_and_operational_replies() {
    for reason in [
        "disabled",
        "humour_off",
        "emergency",
        "busy",
        "takeoff",
        "crossing",
        "readback",
        "clearance_pending",
        "handoff",
        "stale",
        "frequency",
    ] {
        let mut state = prepared("AT76");
        match reason {
            "disabled" => state.settings.informal_pilot_chat = false,
            "humour_off" => state.settings.pilot_chat_humour = false,
            "emergency" => state.session.emergency_declared = true,
            "busy" => state.session.telemetry.radio_busy = true,
            "takeoff" => state.session.phase = PhaseCode::Departure,
            "crossing" => state.session.taxi_clearance.crossing_runway = "09".into(),
            "clearance_pending" => {
                state.session.clearance = Some(openatc_core::state::Clearance {
                    acknowledged: false,
                    ..Default::default()
                });
            }
            "handoff" => state.session.frequency_sequence = 1,
            "readback" => {
                add_transmission(
                    &mut state.session,
                    "ATC",
                    "Maintain 10000 feet.",
                    &SpeechTag::default(),
                );
                state.session.transcript.last_mut().unwrap().pilot_reply =
                    "Maintaining 10000 feet.".into();
            }
            "stale" => {
                state.traffic.pilots.get_mut(&42).unwrap().seen =
                    Instant::now().checked_sub(Duration::from_secs(31)).unwrap();
            }
            "frequency" => {
                state.traffic.pilots.get_mut(&42).unwrap().target.com1_khz = Some(122_800);
            }
            _ => unreachable!(),
        }
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        let result = conversation::request(
            shared.clone(),
            json!({"id":42,"message":"You deserve a beer"}),
        )
        .await;
        if reason == "humour_off" {
            assert!(result.status().is_success());
            assert!(
                !shared
                    .read()
                    .await
                    .session
                    .transcript
                    .last()
                    .unwrap()
                    .text
                    .contains("beer")
            );
        } else {
            assert!(!result.status().is_success(), "{reason}");
        }
    }
}

#[test]
fn pilots_can_initiate_only_after_the_configured_quiet_interval() {
    let mut state = prepared("PA28");
    assert!(!conversation::initiate(&mut state));
    state.initiative.chat_allowed = true;
    state.settings.pilot_chat_chance_percent = 100;
    state.traffic.last_chat = Some(
        Instant::now()
            .checked_sub(Duration::from_secs(301))
            .unwrap(),
    );
    assert!(
        !conversation::initiate(&mut state),
        "no reason even at 100 percent"
    );
    let target = &state.traffic.pilots[&42].target;
    state.sim_weather.insert("TEST".into(), (json!({"latitude":target.latitude, "longitude":target.longitude, "windKnots":25.0, "visibilityMeters":10000.0}), Instant::now()));
    state
        .traffic
        .policy
        .as_mut()
        .unwrap()
        .initiation_probability = 1.0;
    state.traffic.last_chat = Some(
        Instant::now()
            .checked_sub(Duration::from_secs(301))
            .unwrap(),
    );
    assert!(conversation::initiate(&mut state));
    assert!(
        state
            .session
            .transcript
            .last()
            .unwrap()
            .text
            .contains(&state.session.plan.callsign)
    );
    assert!(!conversation::initiate(&mut state));
    state.traffic.last_chat = Some(
        Instant::now()
            .checked_sub(Duration::from_secs(301))
            .unwrap(),
    );
    state.traffic.last_radio = None;
    assert!(
        !conversation::initiate(&mut state),
        "the same reason is not another dice roll"
    );
    state.sim_weather.get_mut("TEST").unwrap().0["windKnots"] = json!(0.0);
    assert!(!conversation::initiate(&mut state));
    state.sim_weather.get_mut("TEST").unwrap().0["windKnots"] = json!(25.0);
    assert!(
        conversation::initiate(&mut state),
        "a new weather concern is a new opportunity"
    );
    state.traffic.last_chat = Some(
        Instant::now()
            .checked_sub(Duration::from_secs(301))
            .unwrap(),
    );
    state.session.emergency_declared = true;
    assert!(!conversation::initiate(&mut state));
}

#[tokio::test]
async fn model_pilot_replies_to_free_text_without_gaining_operational_authority() {
    use axum::{Router, routing::post};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/v1/chat/completions", post(|Json(body): Json<Value>| async move {
            assert!(body["messages"][0]["content"].as_str().unwrap().contains("QFA42"));
            Json(json!({"choices":[{"message":{"content":"Ha, thanks. We'll save that for after the flight!"}}]}))
        }))).await.unwrap();
    });
    let mut state = prepared("A320");
    state.settings.ai_enabled = true;
    state.settings.ai_model = "test".into();
    state.settings.ai_url = format!("http://{address}");
    let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
    assert!(
        conversation::request(
            shared.clone(),
            json!({"id":42,"message":"You deserve a beer after that crosswind landing"})
        )
        .await
        .status()
        .is_success()
    );
    let state = shared.read().await;
    assert!(
        state
            .session
            .transcript
            .last()
            .unwrap()
            .text
            .starts_with("Ha, thanks")
    );
    assert!(state.session.crew_actions.is_empty());
    assert!(state.session.clearance.is_none());
    assert_eq!(state.session.phase, PhaseCode::Cruise);
    server.abort();
}
