//! Surface route monitoring, runway crossing continuation and holding-point handoffs.
use super::Shared;
use super::routes::bad_request;
use axum::{
    Json,
    extract::State as AxumState,
    response::{IntoResponse, Response},
};
use openatc_core::{
    apply::add_transmission,
    ops::update_flight_phase,
    state::{Request, SurfaceStage, Telemetry},
};
use serde_json::json;

pub async fn post_telemetry(
    AxumState(state): AxumState<Shared>,
    Json(telemetry): Json<Telemetry>,
) -> Response {
    let shared = state;
    let (settings, pilot_report, automatic_intent) = {
        let mut state = shared.write().await;
        if update_flight_phase(&mut state.session, &telemetry).is_err() {
            return bad_request("Invalid telemetry");
        }
        state.session.demo = false;
        if telemetry.radio_busy
            || telemetry.radio_sequence_seen.is_some_and(|seen| {
                state.session.transcript.iter().any(|entry| {
                    entry.sequence > seen && entry.speaker == "ATC" && !entry.background
                })
            })
        {
            return Json(json!({"accepted":true})).into_response();
        }
        super::flight::tick(&mut state);
        if state.session.taxi_clearance.approved
            && !state.session.taxi_clearance.crossing_runway.is_empty()
            && state.airport.as_ref().is_some_and(|a| {
                openatc_core::airport::crossing_vacated(
                    a,
                    &state.session.taxi_clearance,
                    &telemetry,
                )
            })
        {
            state.session.taxi_clearance.guidance_complete = true;
        }
        if state.session.taxi_clearance.approved
            && state.airport.as_ref().is_some_and(|a| {
                openatc_core::airport::taxi_hold_reached(
                    a,
                    &state.session.taxi_clearance,
                    &telemetry,
                )
            })
        {
            state.session.taxi_clearance.guidance_complete = true;
        }
        let crossing = state.session.taxi_clearance.clone();
        if crossing.stage() == SurfaceStage::CrossingComplete
            && telemetry.on_ground
            && !telemetry.paused
            && telemetry.radio_power
            && let Some(airport) = state.airport.as_ref()
        {
            let nearby = openatc_core::stations::nearby(&state.stations, &telemetry);
            if let Some(station) = openatc_core::stations::tuned(&nearby, telemetry.com1_khz)
                && station.airport == crossing.airport
                && openatc_core::stations::station_serves(station, "cross_runway")
                && let Ok(mut route) = openatc_core::airport::calculate_taxi_route(
                    airport,
                    &telemetry,
                    &crossing.destination,
                    crossing.to_parking,
                    'C',
                )
            {
                route.pending_readback = true;
                route.sequence = state.session.next_sequence;
                let controller_key = super::flight::station_key(station);
                let station_name = station.name.clone();
                let settings = state.settings.clone();
                let mut tag = super::routes::assign_airspace_controller(
                    &mut state,
                    &settings,
                    &controller_key,
                    "standard",
                );
                tag.position = station_name;
                let text = route.instructions.clone();
                state.session.taxi_clearance = route;
                add_transmission(&mut state.session, "ATC", &text, &tag);
                return Json(json!({"accepted":true})).into_response();
            }
        }
        let route = &state.session.taxi_clearance;
        let eligible = matches!(
            state.session.phase,
            openatc_core::state::PhaseCode::Taxi | openatc_core::state::PhaseCode::TaxiIn
        ) && route.stage() == SurfaceStage::AtHold
            && !route.to_parking
            && telemetry.on_ground
            && !telemetry.paused
            && telemetry.radio_power;
        if !eligible {
            return Json(json!({"accepted":true})).into_response();
        }
        if state
            .airport
            .as_ref()
            .is_none_or(|a| !openatc_core::airport::taxi_hold_reached(a, route, &telemetry))
        {
            return Json(json!({"accepted":true})).into_response();
        }
        let nearby = openatc_core::stations::nearby(&state.stations, &telemetry);
        let Some(station) = openatc_core::stations::tuned(&nearby, telemetry.com1_khz) else {
            return Json(json!({"accepted":true})).into_response();
        };
        if station.airport != route.airport
            || station.service == "ATIS"
            || station.service == "Unicom"
        {
            return Json(json!({"accepted":true})).into_response();
        }
        let retry = route.waiting_for_traffic
            && state.airport.as_ref().is_some_and(|a| {
                openatc_core::holding::blocked(
                    a,
                    &state.session.taxi_clearance.hold_short_runway,
                    &telemetry,
                )
                .is_none()
            });
        if route.holding_channel == telemetry.com1_khz && !retry {
            return Json(json!({"accepted":true})).into_response();
        }
        let needs_further = route.backtrack_required
            || (!route.hold_short_runway.is_empty()
                && route.hold_short_runway != state.session.plan.runway);
        let backtrack = route.backtrack_required;
        let tower = openatc_core::stations::station_serves(station, "ready");
        let controller_key = super::flight::station_key(station);
        let station_name = station.name.clone();
        let settings = state.settings.clone();
        let mut tag = super::routes::assign_airspace_controller(
            &mut state,
            &settings,
            &controller_key,
            "standard",
        );
        tag.position = station_name;
        state.session.taxi_clearance.holding_channel = telemetry.com1_khz;
        if tower && !needs_further && !retry {
            return Json(json!({"accepted":true})).into_response();
        }
        if backtrack && tower {
            let text = openatc_core::dialogue::say(
                "holding_report_ready",
                &[(
                    "runway",
                    state.session.taxi_clearance.hold_short_runway.clone(),
                )],
            );
            add_transmission(&mut state.session, "ATC", &text, &tag);
            return Json(json!({"accepted":true})).into_response();
        }
        (
            state.settings.clone(),
            openatc_core::dialogue::say(
                "pilot_holding_ready",
                &[("callsign", state.session.plan.callsign.clone())],
            ),
            if needs_further && !state.session.taxi_clearance.backtrack_required {
                "cross_runway"
            } else {
                "ready"
            },
        )
    };
    // Normal station-role and runway checks still govern this automatic request.
    let _ = super::routes::atc_request(
        &shared,
        &settings,
        Request {
            controller_initiated: true,
            intent: automatic_intent.into(),
            text: if automatic_intent == "cross_runway" {
                String::new()
            } else {
                pilot_report
            },
            ..Default::default()
        },
    )
    .await;
    Json(json!({"accepted":true})).into_response()
}
