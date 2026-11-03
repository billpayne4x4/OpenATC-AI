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
    state::{Request, SurfaceStage, TakeoffState, Telemetry},
};
use serde_json::json;

/// Release a crossing notification if its request is cancelled before changing the route.
struct CrossingNotice {
    shared: Shared,
    generation: u64,
    sequence: u32,
    channel: i32,
}
impl Drop for CrossingNotice {
    fn drop(&mut self) {
        let shared = self.shared.clone();
        let (generation, sequence, channel) = (self.generation, self.sequence, self.channel);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let mut state = shared.write().await;
                let same_flight = state.session_generation == generation;
                let route = &mut state.session.taxi_clearance;
                if same_flight
                    && route.sequence == sequence
                    && route.holding_channel == channel
                    && route.stage() == SurfaceStage::AtHold
                    && !route.waiting_for_traffic
                {
                    route.holding_channel = 0;
                }
            });
        }
    }
}

/// After landing, the player is at the destination. Do not keep the departure
/// airport's taxi route as the active surface state.
fn sync_arrival_airport(state: &mut crate::EngineState) {
    if state.session.phase != openatc_core::state::PhaseCode::Landed {
        return;
    }
    let Some(arrival) = state.arrival_airport.as_ref() else {
        return;
    };
    if state
        .airport
        .as_ref()
        .is_some_and(|airport| airport.icao == arrival.icao)
    {
        return;
    }
    state.airport = state.arrival_airport.clone();
    state.session.taxi_clearance = Default::default();
}

pub async fn post_telemetry(
    AxumState(state): AxumState<Shared>,
    Json(telemetry): Json<Telemetry>,
) -> Response {
    let shared = state;
    crate::chatter::ChatterManager::warm_airports(shared.clone()).await;
    // Load the CSL catalogue once so the ambient traffic layer can spawn models.
    let needs_models = {
        let state = shared.read().await;
        state.ambient.needs_models() && !state.settings.simulator_root.is_empty()
    };
    if needs_models {
        let shared = shared.clone();
        tokio::spawn(async move {
            let (root, workers, installer) = {
                let state = shared.read().await;
                (
                    state.settings.simulator_root.clone(),
                    state.workers.clone(),
                    state.model_installer.clone(),
                )
            };
            if let Ok(models) = installer.models(&root, &workers).await {
                let mut state = shared.write().await;
                state.ambient.set_models(models);
            }
        });
    }
    let (
        settings,
        pilot_report,
        automatic_intent,
        generation,
        route_sequence,
        channel,
        before,
        radio,
    ) = {
        let mut state = shared.write().await;
        if crate::debug_jump::waiting(&mut state) {
            return Json(json!({"accepted":true})).into_response();
        }
        let previous_receivers = [
            state
                .session
                .telemetry
                .frequency(openatc_core::state::Radio::Com1),
            state
                .session
                .telemetry
                .frequency(openatc_core::state::Radio::Com2),
        ];
        let was_powered = state.session.telemetry.radio_power;
        if let Err(error) = update_flight_phase(&mut state.session, &telemetry) {
            return bad_request(&error);
        }
        sync_arrival_airport(&mut state);
        crate::atc::sync_ownship(&mut state);
        crate::radio::observe_atis_tuning(&mut state, previous_receivers, was_powered);
        // Re-plan once when the player becomes airborne, so the traffic is seeded on the
        // procedure they are actually flying: one aircraft ahead and one behind, rather than
        // only the surface traffic prepared while they were at the gate.
        let departed = state.session.has_departed;
        state
            .generated
            .request_airborne_reseed(telemetry.on_ground, departed);
        // Resolve the local airport from the position the simulator already sends. Without this
        // the engine only learned the airport from the interface's per-frame update, so traffic
        // did not start until the window was opened.
        if state.airport.is_none()
            && telemetry.position_valid
            && !state.settings.simulator_root.is_empty()
        {
            let own = [telemetry.latitude, telemetry.longitude];
            let nearest = state
                .stations
                .iter()
                .filter(|station| {
                    station.service == "Tower"
                        || station.services.iter().any(|service| service == "Tower")
                })
                .min_by(|left, right| {
                    let distance = |station: &openatc_core::stations::Station| {
                        openatc_core::arrival::distance_nm(
                            own,
                            [station.latitude, station.longitude],
                        )
                    };
                    distance(left).total_cmp(&distance(right))
                })
                .map(|station| station.airport.clone());
            if let Some(icao) = nearest {
                let root = state.settings.simulator_root.clone();
                let exists = |path: &str| std::path::Path::new(path).exists();
                let read =
                    |path: &str| std::fs::read_to_string(path).map_err(|error| error.to_string());
                if let Ok(airport) =
                    openatc_core::airport::load_airport_from_simulator(&root, &icao, &exists, &read)
                {
                    eprintln!("OpenATC engine: local airport resolved from telemetry: {icao}");
                    state.airport = Some(airport);
                }
            }
        }
        let channel = state.session.control_frequency().unwrap_or(0);
        if state
            .pending_copilot_reply
            .as_ref()
            .is_some_and(|p| Some(p.frequency) != telemetry.frequency(p.request.radio))
        {
            state.pending_copilot_reply = None;
        }
        state.session.demo = false;
        super::generated::step(&mut state);
        let mut ambient = std::mem::take(&mut state.ambient);
        ambient.tick(&mut state);
        state.ambient = ambient;
        let mut chatter = std::mem::take(&mut state.chatter);
        chatter.tick(&mut state);
        state.chatter = chatter;
        if state.settings.generated_traffic {
            let worker_state = shared.clone();
            tokio::spawn(async move {
                super::generated::prepare(worker_state.clone()).await;
                super::generated::simulate(worker_state.clone()).await;
                super::generated::prepare_turnaround(worker_state).await;
            });
        }
        super::crew::update_ground_connection(&mut state);
        super::cabin::tick(&mut state);
        super::missions::clock(&mut state);
        super::traffic::observe(&mut state);
        if super::flight::urgent(&mut state) || super::traffic::advisories::urgent(&mut state) {
            return Json(json!({"accepted":true})).into_response();
        }
        let radio_busy = telemetry.radio_busy
            || telemetry.radio_sequence_seen.is_some_and(|seen| {
                state.session.transcript.iter().any(|entry| {
                    entry.sequence > seen
                        && entry.speaker == "ATC"
                        && !entry.background
                        && entry.radio.unwrap_or_default() == state.session.control_radio
                })
            });
        super::routes::reconcile_pending_reply(&mut state);
        if super::discipline::tick(&mut state, !radio_busy) || radio_busy {
            return Json(json!({"accepted":true})).into_response();
        }
        let sequence = state.session.next_sequence;
        super::flight::tick(&mut state);
        if state.session.next_sequence != sequence {
            return Json(json!({"accepted":true})).into_response();
        }
        super::cabin::follow_up(&mut state);
        if state.session.next_sequence != sequence {
            return Json(json!({"accepted":true})).into_response();
        }
        super::parking::tick(&mut state);
        if state.session.next_sequence != sequence || state.session.taxi_clearance.parking_hold {
            return Json(json!({"accepted":true})).into_response();
        }
        super::traffic::tick(&mut state);
        if state.session.next_sequence != sequence || state.session.taxi_clearance.traffic_hold {
            return Json(json!({"accepted":true})).into_response();
        }
        super::generated::radio_tick(&mut state);
        if state.session.next_sequence != sequence {
            return Json(json!({"accepted":true})).into_response();
        }
        super::initiative::tick(&mut state);
        if state.session.next_sequence != sequence {
            return Json(json!({"accepted":true})).into_response();
        }
        if state
            .session
            .clearance
            .as_ref()
            .is_none_or(|c| c.acknowledged)
            && !state.session.taxi_clearance.pending_readback
        {
            super::missions::tick(&mut state);
            if state.session.next_sequence != sequence {
                return Json(json!({"accepted":true})).into_response();
            }
        }
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
            && !state.settings.copilot_replies
            && telemetry.on_ground
            && !telemetry.paused
            && telemetry.radio_power
            && let Some(airport) = state.airport.as_ref()
        {
            let nearby = openatc_core::stations::nearby(&state.stations, &telemetry);
            if let Some(station) = openatc_core::stations::tuned(&nearby, channel)
                && station.airport == crossing.airport
                && openatc_core::stations::station_serves(station, "cross_runway")
                && let Ok(mut route) = openatc_core::airport::calculate_taxi_route(
                    airport,
                    &telemetry,
                    &crossing.destination,
                    crossing.to_parking,
                    openatc_core::parking::aircraft_size(&state.session).unwrap_or('F'),
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
        super::flight::takeoff_tick(&mut state);
        if state.session.next_sequence != sequence {
            return Json(json!({"accepted":true})).into_response();
        }
        let route = &state.session.taxi_clearance;
        let eligible = matches!(
            state.session.phase,
            openatc_core::state::PhaseCode::Taxi | openatc_core::state::PhaseCode::TaxiIn
        ) && route.stage() == SurfaceStage::AtHold
            && route.takeoff_state == TakeoffState::Idle
            && (!route.to_parking || !route.hold_short_runway.is_empty())
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
        let Some(station) = openatc_core::stations::tuned(&nearby, channel) else {
            return Json(json!({"accepted":true})).into_response();
        };
        if station.airport != route.airport
            || station.service == "ATIS"
            || station.service == "Unicom"
        {
            return Json(json!({"accepted":true})).into_response();
        }
        let retry = route.waiting_for_traffic
            && (route.hold_short_runway != state.session.plan.runway
                || state.session.departure_wake_seconds <= 0.0)
            && state.airport.as_ref().is_some_and(|a| {
                let telemetry = state.telemetry_with_traffic();
                let Some(targets) = telemetry.traffic.as_deref() else {
                    return false;
                };
                let runway = &state.session.taxi_clearance.hold_short_runway;
                if route.waiting_seconds > 0.0 {
                    let size = openatc_core::parking::aircraft_size(&state.session).unwrap_or('F');
                    if let Ok(crossing) =
                        openatc_core::airport::calculate_crossing_route(a, &telemetry, runway, size)
                        && let Some(along) =
                            openatc_core::holding::crossing_along(a, runway, &crossing.points)
                    {
                        return openatc_core::holding::blocked_for_crossing(
                            a,
                            runway,
                            along,
                            targets.iter(),
                            route.waiting_seconds,
                        )
                        .is_none();
                    }
                    openatc_core::holding::blocked_for_movement(
                        a,
                        runway,
                        targets.iter(),
                        route.waiting_seconds,
                    )
                    .is_none()
                } else {
                    openatc_core::holding::blocked(a, runway, &telemetry).is_none()
                }
            });
        if route.holding_channel == channel && !retry {
            return Json(json!({"accepted":true})).into_response();
        }
        let needs_further = route.to_parking
            || route.backtrack_required
            || (!route.hold_short_runway.is_empty()
                && route.hold_short_runway != state.session.plan.runway);
        let backtrack = route.backtrack_required;
        let tower = openatc_core::stations::station_serves(station, "ready");
        if !tower && !needs_further {
            // A frequency transfer needs no new runway clearance or scenery reload.
            if let Some(next) = nearby
                .iter()
                .find(|candidate| {
                    candidate.airport == route.airport
                        && candidate.receivable
                        && openatc_core::stations::station_serves(candidate, "ready")
                })
                .cloned()
            {
                let current = station.clone();
                let tag = super::flight::tag(&mut state, &current);
                let frequency = format!("{:.3}", f64::from(next.khz) / 1000.0);
                let slots = [
                    ("callsign", state.session.plan.callsign.clone()),
                    ("station", next.name),
                    ("frequency", frequency),
                ];
                let text = openatc_core::dialogue::say("flight_handoff", &slots);
                state.session.recommended_frequency_khz = next.khz;
                state.session.frequency_sequence = state.session.next_sequence;
                add_transmission(&mut state.session, "ATC", &text, &tag);
                if let Some(entry) = state.session.transcript.last_mut() {
                    entry.pilot_reply =
                        openatc_core::dialogue::say("pilot_frequency_readback", &slots);
                }
                state.session.taxi_clearance.holding_channel = channel;
            }
            return Json(json!({"accepted":true})).into_response();
        }
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
        if tower && !needs_further && !retry {
            if state.session.taxi_clearance.holding_channel != channel {
                let slots = [
                    ("callsign", state.session.plan.callsign.clone()),
                    ("runway", state.session.plan.runway.clone()),
                ];
                let text = openatc_core::dialogue::say("flight_report_ready", &slots);
                add_transmission(&mut state.session, "ATC", &text, &tag);
                if let Some(entry) = state.session.transcript.last_mut() {
                    entry.information_only = true;
                }
                state.session.taxi_clearance.holding_channel = channel;
            }
            return Json(json!({"accepted":true})).into_response();
        }
        state.session.taxi_clearance.holding_channel = channel;
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
            state.session_generation,
            state.session.taxi_clearance.sequence,
            channel,
            state.session.next_sequence,
            state.session.control_radio,
        )
    };
    let _notice = (automatic_intent == "cross_runway").then(|| CrossingNotice {
        shared: shared.clone(),
        generation,
        sequence: route_sequence,
        channel,
    });
    // Normal station-role and runway checks still govern this automatic request.
    let response = super::routes::atc_request(
        &shared,
        &settings,
        Request {
            radio,
            controller_initiated: true,
            clearance_sequence: route_sequence,
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
    let mut state = shared.write().await;
    if state.session_generation == generation
        && state.session.control_radio == radio
        && state.session.taxi_clearance.sequence == route_sequence
        && state.session.taxi_clearance.holding_channel == channel
        && (response.status().is_client_error()
            || response.status().is_server_error()
            || state.session.next_sequence == before)
    {
        // A failed request must not consume the holding-point notification.
        state.session.taxi_clearance.holding_channel = 0;
    }
    Json(json!({"accepted":true})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::aircraft::AircraftBehaviour;
    use openatc_core::{
        airport::Airport,
        state::{Point, TaxiClearance},
        stations::Station,
    };

    fn fixture() -> crate::EngineState {
        let mut state = crate::test_state();
        state
            .session
            .on_event(openatc_core::aircraft::AircraftEvent::Taxi);
        state.session.plan.departure = "TEST".into();
        state.session.plan.runway = "09".into();
        state.session.plan.callsign = "C-GTLT".into();
        state.airport = Some(Airport {
            icao: "TEST".into(),
            ..Default::default()
        });
        state.session.taxi_clearance = TaxiClearance {
            airport: "TEST".into(),
            approved: true,
            hold_short_runway: "09".into(),
            points: vec![Point::default()],
            sequence: 42,
            ..Default::default()
        };
        state.stations = vec![
            Station {
                airport: "TEST".into(),
                name: "Test Ground".into(),
                service: "Ground".into(),
                khz: 121_400,
                ..Default::default()
            },
            Station {
                airport: "TEST".into(),
                name: "Test Tower".into(),
                service: "Tower".into(),
                khz: 118_700,
                ..Default::default()
            },
        ];
        state
    }

    fn telemetry() -> Telemetry {
        Telemetry {
            position_valid: true,
            on_ground: true,
            radio_power: true,
            com1_khz: 121_400,
            ground_speed_knots: 3.0,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn cancelled_crossing_notice_retries_without_erasing_a_new_route_or_traffic_hold() {
        for protected in 0..3 {
            let mut state = fixture();
            state.session.taxi_clearance.guidance_complete = true;
            state.session.taxi_clearance.holding_channel = 121_400;
            let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
            let copy = shared.clone();
            let (ready, waiting) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let _notice = CrossingNotice {
                    shared: copy,
                    generation: 0,
                    sequence: 42,
                    channel: 121_400,
                };
                let _ = ready.send(());
                std::future::pending::<()>().await;
            });
            waiting.await.unwrap();
            if protected == 1 {
                shared.write().await.session.taxi_clearance.sequence = 43;
            }
            if protected == 2 {
                shared
                    .write()
                    .await
                    .session
                    .taxi_clearance
                    .waiting_for_traffic = true;
            }
            task.abort();
            let _ = task.await;
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                shared.read().await.session.taxi_clearance.holding_channel,
                if protected == 0 { 0 } else { 121_400 }
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires installed X-Plane scenery"]
    async fn nice_reported_04l_hold_issues_crossing_without_voice_discovery_or_scanning() {
        let root = std::env::var("OPENATC_SIM_ROOT").unwrap();
        let airport = openatc_core::airport::load_airport_from_simulator(
            &root,
            "LFMN",
            &|p| std::path::Path::new(p).is_file(),
            &|p| std::fs::read_to_string(p).map_err(|e| e.to_string()),
        )
        .unwrap();
        let mut state = fixture();
        state.settings.simulator_root = root.clone();
        state.station_root = root;
        state.settings.ai_enabled = false;
        state.settings.llm_phrase_variety = false;
        state.settings.congestion = openatc_settings::Congestion::Busy;
        state.settings.tts_url = "http://127.0.0.1:1".into();
        state.session.plan.departure = "LFMN".into();
        state.session.plan.runway = "04R".into();
        state.session.plan.aircraft = "A20N".into();
        let t = Telemetry {
            latitude: 43.652_637,
            longitude: 7.203_133,
            com1_khz: 121_705,
            ground_speed_knots: 0.0,
            traffic: Some(vec![]),
            ..telemetry()
        };
        state.session.taxi_clearance = openatc_core::airport::calculate_taxi_route(
            &airport,
            &Telemetry {
                latitude: 43.655_753,
                longitude: 7.202_489,
                ..t.clone()
            },
            "04R",
            false,
            'C',
        )
        .unwrap();
        assert_eq!(state.session.taxi_clearance.hold_short_runway, "04L");
        state.session.taxi_clearance.approved = true;
        state.session.taxi_clearance.pending_readback = false;
        state.session.taxi_clearance.sequence = 19;
        state.stations = openatc_core::stations::from_airport(&airport);
        state.airport = Some(airport);
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            post_telemetry(AxumState(shared.clone()), Json(t.clone())),
        )
        .await
        .expect("Automatic crossing must not wait on voice discovery or artificial congestion");
        let state = shared.read().await;
        assert_eq!(state.session.taxi_clearance.crossing_runway, "04L");
        assert!(state.session.taxi_clearance.pending_readback);
        assert!(
            state
                .session
                .transcript
                .iter()
                .any(|entry| entry.text.contains("04L")
                    && entry.text.to_lowercase().contains("cross"))
        );
        drop(state);
        let count = shared.read().await.session.transcript.len();
        post_telemetry(AxumState(shared.clone()), Json(t)).await;
        assert_eq!(
            shared.read().await.session.transcript.len(),
            count,
            "Wait for crossing readback before another instruction"
        );
    }

    #[tokio::test]
    async fn an_unheard_other_radio_call_does_not_block_the_ground_handoff() {
        let mut state = fixture();
        add_transmission(
            &mut state.session,
            "ATC",
            "Other station report",
            &openatc_core::state::SpeechTag {
                radio: Some(openatc_core::state::Radio::Com2),
                ..Default::default()
            },
        );
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        post_telemetry(
            AxumState(shared.clone()),
            Json(Telemetry {
                radio_sequence_seen: Some(0),
                ..telemetry()
            }),
        )
        .await;
        assert_eq!(
            shared.read().await.session.recommended_frequency_khz,
            118_700
        );
    }

    #[tokio::test]
    async fn handoff_waits_for_readback_and_radio_then_transfers_once_while_moving() {
        let mut state = fixture();
        state.session.taxi_clearance.pending_readback = true;
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        let t = telemetry();
        post_telemetry(AxumState(shared.clone()), Json(t.clone())).await;
        assert!(shared.read().await.session.transcript.is_empty());
        shared.write().await.session.taxi_clearance.pending_readback = false;
        post_telemetry(
            AxumState(shared.clone()),
            Json(Telemetry {
                radio_busy: true,
                ..t.clone()
            }),
        )
        .await;
        assert_eq!(
            shared.read().await.session.taxi_clearance.holding_channel,
            0
        );
        post_telemetry(AxumState(shared.clone()), Json(t.clone())).await;
        {
            let state = shared.read().await;
            assert_eq!(state.session.recommended_frequency_khz, 118_700);
            assert_eq!(state.session.transcript.len(), 1);
            let entry = &state.session.transcript[0];
            assert!(entry.text.contains("Test Tower") && entry.text.contains("118.700"));
            assert!(!entry.pilot_reply.is_empty());
            assert_eq!(state.session.frequency_sequence, entry.sequence);
        }
        post_telemetry(AxumState(shared.clone()), Json(t.clone())).await;
        post_telemetry(
            AxumState(shared.clone()),
            Json(Telemetry {
                com1_khz: 118_700,
                ..t
            }),
        )
        .await;
        {
            let state = shared.read().await;
            assert_eq!(
                state.session.transcript.len(),
                2,
                "Tower should prompt for ready, not clear departure"
            );
            assert!(
                state
                    .session
                    .transcript
                    .last()
                    .unwrap()
                    .text
                    .to_lowercase()
                    .contains("report ready")
            );
            assert_eq!(
                state.session.taxi_clearance.takeoff_state,
                TakeoffState::Idle
            );
        }
    }

    #[test]
    fn landing_switches_surface_state_to_the_arrival_airport() {
        let mut state = fixture();
        state.session.phase = openatc_core::state::PhaseCode::Landed;
        state.arrival_airport = Some(Airport {
            icao: "LIMC".into(),
            ..Default::default()
        });
        state.session.taxi_clearance.airport = "TEST".into();
        state.session.taxi_clearance.approved = true;
        sync_arrival_airport(&mut state);
        assert_eq!(
            state.airport.as_ref().map(|a| a.icao.as_str()),
            Some("LIMC")
        );
        assert!(!state.session.taxi_clearance.approved);
        assert!(state.session.taxi_clearance.airport.is_empty());
    }

    #[tokio::test]
    async fn the_hold_prompts_report_ready_without_clearing() {
        for auto_respond in [true, false] {
            let mut state = fixture();
            state.settings.copilot_auto_respond = auto_respond;
            state.settings.simulator_root = "test".into();
            state.station_root = "test".into();
            state.session.taxi_clearance.guidance_complete = true;
            state.session.taxi_clearance.takeoff_state = TakeoffState::Idle;
            let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
            post_telemetry(
                AxumState(shared.clone()),
                Json(Telemetry {
                    com1_khz: 118_700,
                    traffic: Some(Vec::new()),
                    ..telemetry()
                }),
            )
            .await;
            let state = shared.read().await;
            assert_eq!(
                state.session.taxi_clearance.takeoff_state,
                TakeoffState::Idle,
                "reaching the hold must not start the takeoff sequence"
            );
            assert_eq!(state.session.taxi_clearance.holding_channel, 118_700);
            assert!(
                state
                    .session
                    .transcript
                    .iter()
                    .any(|entry| entry.text.to_lowercase().contains("report ready")),
                "the hold should prompt report ready"
            );
        }
    }

    #[tokio::test]
    async fn missing_tower_can_be_discovered_on_the_next_update() {
        let mut state = fixture();
        let tower = state.stations.pop().unwrap();
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        post_telemetry(AxumState(shared.clone()), Json(telemetry())).await;
        assert_eq!(
            shared.read().await.session.taxi_clearance.holding_channel,
            0
        );
        shared.write().await.stations.push(tower);
        post_telemetry(AxumState(shared.clone()), Json(telemetry())).await;
        assert_eq!(
            shared.read().await.session.recommended_frequency_khz,
            118_700
        );
    }

    #[tokio::test]
    #[ignore = "requires installed X-Plane scenery"]
    async fn klax_reported_hold_transfers_from_ground_to_published_tower() {
        let root = std::env::var("OPENATC_SIM_ROOT").unwrap();
        let airport = openatc_core::airport::load_airport_from_simulator(
            &root,
            "KLAX",
            &|p| std::path::Path::new(p).exists(),
            &|p| std::fs::read_to_string(p).map_err(|e| e.to_string()),
        )
        .unwrap();
        let t = Telemetry {
            latitude: 33.938_318,
            longitude: -118.383_084,
            ground_speed_knots: 0.19,
            ..telemetry()
        };
        let mut state = fixture();
        state.session.plan.departure = "KLAX".into();
        state.session.plan.runway = "25L".into();
        state.session.taxi_clearance = openatc_core::airport::calculate_taxi_route(
            &airport,
            &Telemetry {
                latitude: 33.9389,
                ..t.clone()
            },
            "25L",
            false,
            'C',
        )
        .unwrap();
        assert_eq!(state.session.taxi_clearance.hold_short_runway, "25L");
        state.session.taxi_clearance.approved = true;
        state.stations = openatc_core::stations::from_airport(&airport);
        let tower = state
            .stations
            .iter()
            .find(|s| s.service == "Tower")
            .unwrap()
            .clone();
        let expected = tower.khz;
        state.airport = Some(airport);
        let shared = std::sync::Arc::new(tokio::sync::RwLock::new(state));
        post_telemetry(AxumState(shared.clone()), Json(t)).await;
        let state = shared.read().await;
        assert_eq!(state.session.recommended_frequency_khz, expected);
        assert!(
            state
                .session
                .transcript
                .last()
                .unwrap()
                .text
                .contains(&tower.name)
        );
    }
}
