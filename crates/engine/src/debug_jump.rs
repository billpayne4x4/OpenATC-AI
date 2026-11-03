//! Two-step developer jumps keep old telemetry from advancing the new checkpoint.

#![allow(clippy::default_trait_access)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use crate::{EngineState, Shared};
use axum::{
    Json,
    extract::State,
    response::{IntoResponse, Response},
};
use openatc_core::{debug_jump::Checkpoint, state::FlightPlan};
use serde_json::{Value, json};
use std::time::Instant;

pub struct Pending {
    target: Checkpoint,
    plan: FlightPlan,
    token: u64,
    created: Instant,
}

pub fn waiting(state: &mut EngineState) -> bool {
    if state.debug_jump.as_ref().is_some_and(|pending| {
        pending.created.elapsed().as_secs() >= 180
            || pending.token != state.session_generation
            || pending.plan != state.session.plan
    }) {
        state.debug_jump = None;
    }
    state.debug_jump.is_some()
}

pub async fn prepare(State(shared): State<Shared>, Json(body): Json<Value>) -> Response {
    let mut state = shared.write().await;
    if !state.settings.dev_mode {
        return crate::routes::bad_request("Developer mode is required.");
    }
    if waiting(&mut state) {
        return crate::routes::bad_request("A checkpoint jump is already pending.");
    }
    let session = &state.session;
    // Native initialization does not configure a third-party FMS or manufacture
    // a flight clearance. Aircraft setup still needs pilot review.
    if !session.telemetry.engines_running
        || !session.telemetry.position_valid
        || session
            .clearance
            .as_ref()
            .is_none_or(|clearance| !clearance.acknowledged)
    {
        return crate::routes::bad_request(
            "Airborne checkpoints require a powered aircraft and an acknowledged IFR clearance.",
        );
    }
    let mut target =
        match openatc_core::debug_jump::route(&session.plan, body["kind"].as_str().unwrap_or("")) {
            Ok(target) => target,
            Err(error) => return crate::routes::bad_request(error),
        };
    let airport = if target.kind == "sid" {
        &session.plan.departure
    } else {
        &session.plan.destination
    };
    let transition = openatc_core::regions::region_for(&state.regions, airport).transition_feet;
    let reported = session
        .telemetry
        .clearance_altitude_feet(target.altitude_feet.round() as i32, transition);
    target.geometric_feet =
        target.altitude_feet + (session.telemetry.altitude_feet - reported).clamp(-2000.0, 2000.0);
    state.session_generation = state.session_generation.wrapping_add(1);
    let token = state.session_generation;
    state.pending_copilot_reply = None;
    state.session.crew_actions.clear();
    state.crew.batch = None;
    state.crew.pushback = None;
    state.crew.checklist = None;
    state.crew.ground_taxi_origin = None;
    state.request_memory.cancel_pending();
    state.debug_jump = Some(Pending {
        target: target.clone(),
        plan: state.session.plan.clone(),
        token,
        created: Instant::now(),
    });
    Json(json!({"target":target,"token":token})).into_response()
}

pub async fn finish(State(shared): State<Shared>, Json(body): Json<Value>) -> Response {
    let mut state = shared.write().await;
    if !state.settings.dev_mode
        || !waiting(&mut state)
        || state
            .debug_jump
            .as_ref()
            .is_none_or(|pending| Some(pending.token) != body["token"].as_u64())
    {
        return crate::routes::bad_request(
            "Checkpoint expired or flight changed; ATC state was not moved.",
        );
    }
    let observed = if body["moved"] == true {
        let Ok(telemetry) =
            serde_json::from_value::<openatc_core::state::Telemetry>(body["telemetry"].clone())
        else {
            return crate::routes::bad_request("Native checkpoint telemetry is required.");
        };
        let target = &state.debug_jump.as_ref().unwrap().target;
        if !telemetry.position_valid
            || !telemetry.paused
            || telemetry.on_ground
            || openatc_core::ops::distance_nm(
                target.latitude,
                target.longitude,
                telemetry.latitude,
                telemetry.longitude,
            ) >= 0.5
            || (telemetry.altitude_feet - target.geometric_feet).abs() >= 200.0
        {
            return crate::routes::bad_request(
                "Native aircraft has not reached the paused checkpoint.",
            );
        }
        let mut check = state.session.clone();
        if let Err(error) = openatc_core::ops::update_flight_phase(&mut check, &telemetry) {
            return crate::routes::bad_request(error);
        }
        Some(telemetry)
    } else {
        None
    };
    let pending = state.debug_jump.take().unwrap();
    if body["moved"] != true {
        return Json(json!({"accepted":true})).into_response();
    }
    let target = pending.target;
    state.flow = Default::default();
    state.flow.announce_approach = matches!(target.phase, openatc_core::state::PhaseCode::Arrival);
    state.flow.monitor.next_fix = target.next_fix;
    state.flow.monitor.checked_fix = target.next_fix.saturating_sub(1);
    let route = state.session.clearance.as_ref().unwrap().route.clone();
    state.flow.monitor.route = route;
    state.parking = Default::default();
    state.initiative = Default::default();
    state.traffic = Default::default();
    state.session.phase = target.phase;
    state.session.candidate_phase = target.phase;
    state.session.phase_evidence = 0;
    state.session.has_departed = true;
    state.session.clearance_cancelled = false;
    state.session.atc_offer = None;
    state.session.traffic_call = None;
    state.session.departure_wake_seconds = 0.0;
    state.session.arrival_wake_conflict = false;
    state.session.taxi_clearance = Default::default();
    state.session.ground_interphone_disconnected = true;
    state.session.recommended_frequency_khz = 0;
    state.session.frequency_sequence = 0;
    // Keep history, but old readbacks must not block instructions at the new location.
    for entry in &mut state.session.transcript {
        entry.pilot_reply.clear();
    }
    if let Some(clearance) = &mut state.session.clearance {
        clearance.altitude_feet = target.altitude_feet.round() as i32;
        clearance.readback_scope.clear();
        clearance.acknowledged = true;
    }
    state.session.telemetry = observed.unwrap();
    Json(json!({"accepted":true,"nextFix":target.next_fix})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::state::{Clearance, RouteFix};
    use std::sync::Arc;
    use tokio::sync::RwLock;
    #[tokio::test]
    async fn surface_jump_does_not_leave_a_busy_copilot_or_pushback() {
        let mut state = crate::test_state();
        state.settings.dev_mode = true;
        state.pending_copilot_reply = Some(crate::routes::PreparedReply {
            generation: 0,
            sequence: 1,
            frequency: 121_705,
            request: Default::default(),
        });
        state.crew.pushback = Some(openatc_core::crew::PushbackRequest {
            distance_metres: 20.0,
            turn_degrees: 0.0,
        });
        state.crew.checklist = Some(("departure".into(), 1, false));
        let shared = Arc::new(RwLock::new(state));
        assert!(
            crate::routes::debug_cancel_actions(State(shared.clone()))
                .await
                .status()
                .is_success()
        );
        let state = shared.read().await;
        assert!(state.pending_copilot_reply.is_none());
        assert!(state.crew.pushback.is_none());
        assert!(state.crew.checklist.is_none());
    }
    #[tokio::test]
    async fn jump_transaction_cancels_stale_work_and_never_grants_approach_permission() {
        let mut state = crate::test_state();
        state.settings.dev_mode = true;
        state.session.telemetry.position_valid = true;
        state.session.telemetry.engines_running = true;
        state.session.telemetry.on_ground = false;
        state.session.clearance = Some(Clearance {
            altitude_feet: 18000,
            route: "FILED".into(),
            departure_readback: String::new(),
            as_filed: true,
            runway: "04R".into(),
            squawk: "1234".into(),
            readback_scope: String::new(),
            readback_speed: None,
            readback_altitude: None,
            acknowledged: true,
            sequence: 1,
            frequency_khz: 118_675,
        });
        state.session.plan.fixes = (0..3)
            .map(|i| RouteFix {
                latitude: 44.0 + f64::from(i),
                longitude: 8.0,
                altitude_feet: 9000.0,
                constraint_source: "STAR TEST".into(),
                path_terminator: "TF".into(),
                ..Default::default()
            })
            .collect();
        let shared = Arc::new(RwLock::new(state));
        assert_eq!(
            prepare(State(shared.clone()), Json(json!({"kind":"star"})))
                .await
                .status(),
            200
        );
        let token = shared.read().await.session_generation;
        assert!(waiting(&mut *shared.write().await));
        assert_eq!(
            finish(
                State(shared.clone()),
                Json(json!({"token":token+1,"moved":true}))
            )
            .await
            .status(),
            400
        );
        let mut observed = shared.read().await.session.telemetry.clone();
        let target = shared
            .read()
            .await
            .debug_jump
            .as_ref()
            .unwrap()
            .target
            .clone();
        observed.latitude = target.latitude;
        observed.longitude = target.longitude;
        observed.altitude_feet = target.geometric_feet;
        // Cockpit instruments may not refresh during a paused native initialization.
        observed.indicated_altitude_feet = Some(18000.0);
        observed.paused = true;
        assert_eq!(
            finish(
                State(shared.clone()),
                Json(json!({"token":token,"moved":true}))
            )
            .await
            .status(),
            400
        );
        assert_eq!(
            finish(
                State(shared.clone()),
                Json(json!({"token":token,"moved":true,"telemetry":observed}))
            )
            .await
            .status(),
            200
        );
        let state = shared.read().await;
        assert_eq!(state.flow.monitor.next_fix, 1);
        assert_eq!(state.session.phase, openatc_core::state::PhaseCode::Arrival);
        assert!(!state.flow.approach_cleared && !state.flow.landing_cleared);
        assert!(state.session.telemetry.paused);
        drop(state);
        assert_eq!(
            prepare(State(shared.clone()), Json(json!({"kind":"star"})))
                .await
                .status(),
            200
        );
        shared.write().await.session.plan.destination = "OTHER".into();
        assert_eq!(
            finish(
                State(shared.clone()),
                Json(json!({"token":token+1,"moved":true}))
            )
            .await
            .status(),
            400
        );
        assert!(shared.read().await.debug_jump.is_none());
    }
}
