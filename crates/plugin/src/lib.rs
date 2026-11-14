//! X-Plane plugin lifecycle, window, flight-loop callbacks and commands.
//! SDK calls run on the simulator thread; the companion engine handles dialogue.

#![allow(unsafe_code)]

mod arrival;
mod atis;
mod callouts;
mod crew;
mod debug_jump;
mod follow_me;
mod generated_traffic;
mod geometry;
mod gl;
mod legacy_gl;
mod panel;
mod render;
mod supervise;
mod taxi;
mod tcas;
mod telemetry;
mod traffic;
mod transponder;
mod weather;
mod window;

use openatc_ui::interface::Interface;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use xplane::{
    data::{DataRead, DataReadWrite, borrowed::DataRef},
    flight_loop::{FlightLoop, FlightLoopPhase, LoopResult},
    plugin::{Plugin, PluginInfo},
    xplane_plugin,
};

/// Shared plugin state behind the command handlers.
struct Shared {
    native_atis: atis::NativeAtis,
    native_radio: atis::NativeRadio,
    generated_traffic: generated_traffic::Renderer,
    tcas: tcas::Tcas,
    native_traffic: traffic::NativeTraffic,
    crew: crew::Runtime,
    callouts: callouts::Runtime,
    interface: Option<Interface>,
    window: Option<window::Floater>,
    window_state: Option<Box<window::WindowState>>,
    refs: Option<telemetry::Refs>,
    com1: Option<DataRef<i32, xplane::data::ReadWrite>>,
    com2: Option<DataRef<i32, xplane::data::ReadWrite>>,
    panel: panel::Panel,
    supervisor: supervise::Supervisor,
    last_frequency_sequence: u32,
    last_state_sequence: u32,
    /// Clearance sequence the copilot last flew or advised on.
    last_ap_seq: u32,
    last_reset_generation: u64,
    logged_notice: String,
    logged_role: String,
    enabled: bool,
    ground_arrows: taxi::GroundGuidance,
    holding_marker: taxi::GroundGuidance,
    traffic_holding: taxi::GroundGuidance,
    follow_me: follow_me::FollowMe,
    debug_cancel_pending: bool,
    debug_air_pending: Option<(&'static str, std::time::Instant)>,
    debug_air_target: Option<(openatc_core::debug_jump::Checkpoint, u64)>,
    arrival_sampler: arrival::TerrainSampler,
    weather_sampler: weather::Sampler,
}

/// The plugin object owned by the simulator host.
struct OpenAtc {
    shared: Arc<Mutex<Shared>>,
    flight_loop: Option<FlightLoop<LoopMarker>>,
    window_loop: Option<FlightLoop<LoopMarker>>,
    heartbeat: Option<supervise::Heartbeat>,
    commands: Vec<(
        xplane::command::Command,
        xplane::command::RegisteredCommandHandler,
    )>,
    #[allow(dead_code)]
    menu: Option<xplane::menu::Menu>,
}

/// Flight loop marker payload (state lives in Shared).
struct LoopMarker;

fn debug_log(text: &str) {
    if let Ok(line) = std::ffi::CString::new(text) {
        unsafe { xplane_sys::XPLMDebugString(line.as_ptr()) };
    }
}

/// Panel role, power and talk-to logging for one tick.
fn update_panel(
    shared: &Arc<Mutex<Shared>>,
    xpapi: &mut xplane::XPAPI,
    acf_path: &str,
    aircraft_dir: &std::path::Path,
) {
    let (attendant_override, ground_override) = {
        let locked = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(interface) = locked.interface.as_ref() else {
            return;
        };
        let settings = interface.engine.settings();
        (settings.attendant_ref.clone(), settings.ground_ref.clone())
    };
    let role_lines = {
        let mut locked = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut lines = locked.panel.refresh(
            &mut xpapi.data,
            acf_path,
            aircraft_dir,
            &attendant_override,
            &ground_override,
        );
        if !lines.is_empty() {
            let name = locked
                .panel
                .profile
                .as_ref()
                .map_or("sim/cockpit2/radios/actuators/com1_frequency_hz_833", |p| {
                    p.com1_active_ref.as_str()
                })
                .to_owned();
            locked.com1 = xpapi
                .data
                .find::<i32, _>(&name)
                .ok()
                .and_then(|r| r.writeable().ok());
            lines.push(format!(
                "OpenATC AI: COM1 {name} {}",
                if locked.com1.is_some() {
                    "writable"
                } else {
                    "unavailable"
                }
            ));
        }
        lines
    };
    for line in role_lines {
        debug_log(&line);
    }
    let mut locked = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let profile = crew::aircraft_profile(locked.panel.profile.as_ref().map(|p| &p.crew));
    locked.crew.bind(xpapi, acf_path, &profile);
    let selected = locked
        .crew
        .role
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let role = if selected.is_empty() {
        locked.panel.role()
    } else {
        selected
    };
    let has_role = !role.is_empty();
    let (radio, bus) = locked.panel.power();
    let spoken = if role.is_empty() {
        "ATC".to_owned()
    } else if role == "ground" {
        "GND".to_owned()
    } else {
        "CABIN".to_owned()
    };
    let profile = locked
        .panel
        .profile
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "Standard simulator controls".to_owned());
    let Some(interface) = locked.interface.as_mut() else {
        return;
    };
    interface.set_panel_role(&role, if has_role { "panel" } else { "" });
    interface.set_electrical_power(radio, bus);
    interface.aircraft_profile = profile;
    if spoken != locked.logged_role {
        locked.logged_role.clone_from(&spoken);
        let panel = if role.is_empty() {
            String::new()
        } else {
            " (cockpit panel)".to_owned()
        };
        debug_log(&format!("OpenATC AI: talking to {spoken}{panel}"));
    }
}

/// Copilot auto-tune plus sequence tracking for one tick.
fn update_tune(shared: &Arc<Mutex<Shared>>) {
    let (snapshot, notice, copilot_tunes, dev_mode) = {
        let mut locked = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(interface) = locked.interface.as_mut() else {
            return;
        };
        let settings = interface.engine.settings();
        (
            interface.engine.state(),
            interface.last_notice(),
            (settings.copilot_tunes || settings.auto_tune_handoff)
                && !interface.speech.busy()
                && !interface.speech.recording()
                && interface.speech_queue.is_empty()
                && !interface.auto_reply_pending,
            settings.dev_mode,
        )
    };
    let mut locked = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if snapshot.next_sequence < locked.last_state_sequence {
        locked.last_frequency_sequence = 0;
    }
    locked.last_state_sequence = snapshot.next_sequence;
    let handoff_radio = snapshot.handoff_radio();
    let last_frequency_sequence = locked.last_frequency_sequence;
    let binding = match handoff_radio {
        openatc_core::state::Radio::Com1 => &mut locked.com1,
        openatc_core::state::Radio::Com2 => &mut locked.com2,
    };
    if copilot_tunes
        && snapshot.frequency_sequence > last_frequency_sequence
        && (118_000..=136_990).contains(&snapshot.recommended_frequency_khz)
        && let Some(radio) = binding.as_mut()
    {
        radio.set(snapshot.recommended_frequency_khz);
        if radio.get() == snapshot.recommended_frequency_khz {
            locked.last_frequency_sequence = snapshot.frequency_sequence;
        }
    }
    // Error watch for dev mode.
    if dev_mode && !notice.is_empty() && notice != locked.logged_notice {
        locked.logged_notice.clone_from(&notice);
        debug_log(&format!("OpenATC AI error: {notice}"));
    }
    if notice.is_empty() {
        locked.logged_notice.clear();
    }
}

/// Copilot autopilot management for one tick. An acknowledged clearance plus
/// the setting on: with the autopilot engaged the copilot writes the altitude
/// preselect and announces it; with it off (or dials unreachable) the copilot
/// advises the pilot instead. Once per clearance sequence.
///
/// Heading and speed wait on sources: direct-to legs need magnetic variation
/// the core does not model yet, and no clearance carries a speed today.
fn update_autopilot(shared: &Arc<Mutex<Shared>>) {
    let job = {
        let locked = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(interface) = locked.interface.as_ref() else {
            return;
        };
        let settings = interface.engine.settings();
        if !settings.copilot_flies_ap
            || interface.speech.busy()
            || !interface.speech_queue.is_empty()
            || interface.auto_reply_pending
        {
            return;
        }
        let Some(clearance) = locked
            .interface
            .as_ref()
            .and_then(|interface| interface.engine.state().clearance.clone())
        else {
            return;
        };
        if !clearance.acknowledged || clearance.altitude_feet <= 0 {
            return;
        }
        if clearance.sequence == locked.last_ap_seq {
            return;
        }
        let engaged = locked
            .panel
            .ap_mode
            .as_ref()
            .is_some_and(|mode| mode.get() == 2);
        let dial = locked
            .panel
            .ap_altitude
            .as_ref()
            .map(xplane::data::DataRead::get);
        let callsign = locked
            .interface
            .as_ref()
            .map(|interface| interface.engine.state().plan.callsign.clone())
            .unwrap_or_default();
        (
            clearance.altitude_feet,
            clearance.sequence,
            engaged,
            dial,
            callsign,
        )
    };
    let (altitude, sequence, engaged, dial, callsign) = job;
    // Altitudes stay whole feet far below f32 precision limits.
    #[allow(clippy::cast_precision_loss)]
    let altitude_float = altitude as f32;
    let mut locked = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if sequence == locked.last_ap_seq {
        return;
    }
    locked.last_ap_seq = sequence;
    // Dial write first: the panel borrow ends before the interface borrow.
    let mut wrote = false;
    if engaged
        && let Some(current) = dial
        && (current - altitude_float).abs() >= 1.0
        && let Some(dial) = locked.panel.ap_altitude.as_mut()
    {
        dial.set(altitude_float);
        wrote = true;
    } else if engaged && dial.is_some() {
        return;
    }
    let Some(interface) = locked.interface.as_mut() else {
        return;
    };
    if !engaged {
        interface.notice = format!("Autopilot is off — set {altitude} yourself, {callsign}.");
        "Copilot advised: AP off".clone_into(&mut interface.last_action);
        return;
    }
    if dial.is_none() {
        "Copilot cannot reach the altitude dial.".clone_into(&mut interface.notice);
        "Copilot advised: no dial".clone_into(&mut interface.last_action);
        return;
    }
    if !wrote {
        return;
    }
    let entry = openatc_core::state::Transmission {
        radio: None,
        frequency: None,
        gap_ms: 0,
        information_only: false,
        pilot_reply: String::new(),
        background: false,
        speaker: "COPILOT".to_owned(),
        text: openatc_core::dialogue::say(
            "copilot_altitude_set",
            &[
                ("altitude", altitude.to_string()),
                ("callsign", callsign.to_owned()),
            ],
        ),
        sequence: 0,
        position: String::new(),
        voice: String::new(),
        delivery: "standard".to_owned(),
        speed: 1.0,
        urgent: false,
    };
    interface.speak_entry(&entry);
    interface.last_action = format!("Copilot set altitude {altitude}");
}

/// One flight-loop tick: supervision, panel role, telemetry, auto-tune.
fn flight_tick(shared: &Arc<Mutex<Shared>>, xpapi: &mut xplane::XPAPI) {
    // Aircraft path and dirs for the profile lookup.
    let acf_path = xpapi.paths.acf_path(0).to_string_lossy().into_owned();
    let mut aircraft_dir = xpapi.paths.plugins_folder();
    aircraft_dir.push("OpenATC");
    aircraft_dir.push("aircraft");
    if let Ok(dir) = std::env::var("OPENATC_AIRCRAFT_DIR") {
        aircraft_dir = std::path::PathBuf::from(dir);
    }
    if !shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .enabled
    {
        return;
    }
    // Engine supervision with UI fault reporting.
    let fault = {
        let connected = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .interface
            .as_ref()
            .is_some_and(|interface| interface.engine.connected());
        let mut locked = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut lines = Vec::new();
        let fault = locked.supervisor.poll(connected, &mut lines);
        for line in lines {
            debug_log(&line);
        }
        fault
    };
    update_panel(shared, xpapi, &acf_path, &aircraft_dir);
    let mut telemetry = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .refs
        .as_ref()
        .map(telemetry::gather);
    let mut locked = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let debug_cancel_pending = locked.debug_cancel_pending;
    let debug_air_pending = locked.debug_air_pending;
    let receives_atis = locked
        .interface
        .as_ref()
        .is_some_and(|interface| interface.engine.connected() && interface.receives_atis);
    locked.native_atis.update(receives_atis);
    let connected = locked
        .interface
        .as_ref()
        .is_some_and(|interface| interface.engine.connected());
    // Another plugin may speak through the radio channel, for example a pushback helper, so the
    // native radio is only muted when the user asked for it.
    let mute_native_radio = locked
        .interface
        .as_ref()
        .is_none_or(|interface| interface.settings.mute_native_radio);
    locked.native_radio.update(connected && mute_native_radio);
    if let Some(code) = locked
        .interface
        .as_mut()
        .and_then(|interface| interface.requested_squawk.take())
    {
        let root = locked
            .interface
            .as_ref()
            .map(|interface| interface.settings.simulator_root.as_str())
            .unwrap_or("");
        let path =
            std::path::Path::new(root).join("Resources/plugins/OpenATC/aircraft/transponder.toml");
        let result = openatc_core::transponder::Controls::load(&path)
            .and_then(|controls| transponder::select(&mut xpapi.data, code, &controls));
        if let Some(interface) = locked.interface.as_mut() {
            interface.radio_notice = result.map_or_else(
                |error| error,
                |_| format!("Squawk {code:04} selected; mode selector unchanged."),
            );
        }
    }
    let requested_frequency = locked.interface.as_mut().and_then(|i| {
        i.requested_frequency_khz
            .take()
            .filter(|f| i.radio_power && (118_000..=136_990).contains(f))
            .map(|frequency| (frequency, i.requested_frequency_radio))
    });
    if let Some((frequency, selected)) = requested_frequency {
        let binding = match selected {
            openatc_core::state::Radio::Com1 => &mut locked.com1,
            openatc_core::state::Radio::Com2 => &mut locked.com2,
        };
        let observed = binding.as_mut().map(|radio| {
            radio.set(frequency);
            radio.get()
        });
        if observed == Some(frequency)
            && let Some(interface) = locked.interface.as_ref()
        {
            let state = interface.engine.state();
            // A manual tune only supersedes a handoff on that same radio.
            if state.handoff_radio() == selected {
                locked.last_frequency_sequence = state.frequency_sequence;
            }
        }
        if let Some(interface) = locked.interface.as_mut() {
            if observed == Some(frequency) {
                interface.radio_tuned(frequency);
            } else {
                interface.notice =
                    format!("The aircraft did not accept the selected {selected:?} frequency.");
            }
        }
    }
    // Use the same aircraft binding for tuning and reception.
    if let Some(t) = telemetry.as_mut()
        && let Some(radio) = locked.com1.as_ref()
    {
        t.com1_khz = radio.get();
    }
    if let Some(t) = telemetry.as_mut() {
        t.com2_khz = locked.com2.as_ref().map(|radio| radio.get()).or(t.com2_khz);
        t.wingspan_meters = locked
            .panel
            .profile
            .as_ref()
            .and_then(|p| p.wingspan_meters);
    }
    let reset_changed = locked
        .interface
        .as_ref()
        .is_some_and(|i| i.flight_reset_generation != locked.last_reset_generation);
    let Some(interface) = locked.interface.as_mut() else {
        return;
    };
    interface.set_engine_fault(&fault);
    if let Some(t) = telemetry.as_mut() {
        t.radio_power = interface.radio_power;
    }
    if let Some(telemetry) = &telemetry
        && telemetry.position_valid
    {
        interface.current_airport = taxi::current_airport(telemetry.latitude, telemetry.longitude);
    }
    if let Some(t) = &telemetry {
        let mut observed = t.clone();
        observed.radio_sequence_seen = Some(interface.last_speech_sequence);
        observed.radio_busy = interface.radio_busy(interface.engine.state().control_radio);
        interface.engine.telemetry(observed);
    }
    interface.tick();
    if interface.engine.connected() && interface.settings.simulator_root.is_empty() {
        let root = xpapi.paths.xplane_folder().to_string_lossy().into_owned();
        interface
            .engine
            .post("/simulator/root", serde_json::json!({"root": root}));
    }
    // Playback and recording occupy the radio until their queues drain.
    if let Some(telemetry) = telemetry.as_mut() {
        telemetry.radio_sequence_seen = Some(interface.last_speech_sequence);
        telemetry.radio_busy = interface.radio_busy(interface.engine.state().control_radio);
    }
    // Send telemetry after processing the current UI requests.
    if let Some(telemetry) = &telemetry {
        interface.engine.telemetry(telemetry.clone());
    }
    let state = interface.engine.state();
    if (debug_air_pending.is_some() || debug_cancel_pending)
        && (reset_changed || interface.reset_pending)
    {
        interface.debug_jump_busy = false;
        locked.debug_air_pending = None;
        locked.debug_air_target = None;
        locked.debug_cancel_pending = false;
        return;
    }
    if let Some((path, started)) = debug_air_pending {
        if path == "/debug/jump/observe" {
            let arrived = locked.debug_air_target.as_ref().is_some_and(|(target, _)| {
                telemetry
                    .as_ref()
                    .is_some_and(|t| debug_jump::arrived(target, t))
            });
            if arrived || started.elapsed().as_secs() > 150 {
                // Drop the shared borrow before accessing the interface again.
                let token = locked
                    .debug_air_target
                    .take()
                    .map(|(_, token)| token)
                    .unwrap_or(0);
                let interface = locked.interface.as_mut().unwrap();
                interface.engine.post(
                    "/debug/jump/finish",
                    serde_json::json!({"token":token,"moved":arrived,"telemetry":telemetry}),
                );
                interface.notice = if arrived {
                    "Checkpoint reached; paused. Review speed, thrust, gear/flaps, AP and active FMS leg. Select the appropriate controller and check in before resuming.".into()
                } else {
                    "Native initialization did not reach the checkpoint. ATC state was not moved; review aircraft position before resuming.".into()
                };
                interface.cancel_jump_speech();
                locked.debug_air_pending = Some(("/debug/jump/finish", std::time::Instant::now()));
            }
        } else if let Some(reply) = interface.engine.take_reply(path) {
            if !reply.success {
                interface.debug_jump_busy = false;
                interface.notice = format!(
                    "Checkpoint failed: {}. Review the aircraft and ATC state before resuming.",
                    reply.error
                );
                locked.debug_air_pending = None;
            } else if path == "/debug/jump/prepare" {
                let target = serde_json::from_value::<openatc_core::debug_jump::Checkpoint>(
                    reply.data["target"].clone(),
                );
                match target {
                    Ok(target) => {
                        let speed = f64::from(interface.debug_jump_speed);
                        let token = reply.data["token"].as_u64().unwrap_or(0);
                        // Scenery initialization may call plugin/window callbacks.
                        // Do not hold the UI mutex across the SDK load.
                        drop(locked);
                        let result = debug_jump::airborne(&target, speed);
                        let mut locked = shared
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let Some(interface) = locked.interface.as_mut() else {
                            return;
                        };
                        match result {
                            Ok(()) => {
                                interface.cancel_jump_speech();
                                interface.notice = "Loading checkpoint; waiting for paused native aircraft position…".into();
                                locked.debug_air_target = Some((target, token));
                                locked.debug_air_pending =
                                    Some(("/debug/jump/observe", std::time::Instant::now()));
                                locked.crew = crew::Runtime::default();
                                locked.callouts.reset();
                                locked.follow_me = follow_me::FollowMe::new();
                            }
                            Err(error) => {
                                interface.engine.post(
                                    "/debug/jump/finish",
                                    serde_json::json!({"token":token,"moved":false}),
                                );
                                interface.notice = error;
                                locked.debug_air_pending =
                                    Some(("/debug/jump/finish", std::time::Instant::now()));
                            }
                        }
                        return;
                    }
                    Err(error) => {
                        interface.engine.post(
                            "/debug/jump/finish",
                            serde_json::json!({"token":reply.data["token"],"moved":false}),
                        );
                        interface.notice = error.to_string();
                        locked.debug_air_pending =
                            Some(("/debug/jump/finish", std::time::Instant::now()));
                    }
                }
            } else {
                interface.debug_jump_busy = false;
                locked.debug_air_pending = None;
            }
        } else if started.elapsed().as_secs() > 35 {
            interface.debug_jump_busy = false;
            interface.notice = "Checkpoint response timed out. Keep the aircraft paused and review its position and ATC state.".into();
            locked.debug_air_pending = None;
        }
        return;
    }
    if let Some(kind) = interface
        .requested_debug_jump
        .take()
        .filter(|_| interface.settings.dev_mode)
    {
        if !["hold", "runway"].contains(&kind.as_str()) {
            if !interface.debug_jump_speed.is_finite() || interface.debug_jump_speed < 0.0 {
                interface.notice = "Checkpoint speed must be zero or a positive TAS.".into();
                return;
            }
            interface.debug_jump_busy = true;
            interface.cancel_jump_speech();
            interface
                .engine
                .post("/debug/jump/prepare", serde_json::json!({"kind":kind}));
            interface.notice = "Preparing route checkpoint…".into();
            locked.debug_air_pending = Some(("/debug/jump/prepare", std::time::Instant::now()));
            return;
        }
        let result = debug_jump::reposition(&kind, &state, &interface.airport);
        let pending = result.is_ok();
        if pending {
            interface
                .engine
                .post("/debug/cancel-actions", serde_json::json!({}));
        }
        interface.notice = result.map_or_else(
            |e| e,
            |()| "Debug jump complete; parking brake set. ATC permissions remain unchanged.".into(),
        );
        if pending {
            interface.cancel_jump_speech();
            locked.crew = crew::Runtime::default();
            locked.callouts.reset();
            locked.follow_me = follow_me::FollowMe::new();
        }
        locked.debug_cancel_pending = pending;
        return;
    }
    if debug_cancel_pending {
        if let Some(reply) = interface.engine.take_reply("/debug/cancel-actions") {
            if !reply.success {
                interface.notice = reply.error;
            }
            locked.debug_cancel_pending = false;
            locked.debug_air_pending = None;
        }
        return;
    }
    let ground_enabled = interface.settings.show_ground_taxi_arrows;
    let holding_enabled = interface.settings.show_holding_point;
    let traffic_holding_enabled = interface.settings.show_traffic_holding_points;
    let follow_me_enabled = interface.settings.show_follow_me;
    let suppress_native =
        interface.engine.connected() && interface.settings.suppress_native_traffic;
    let (native_enabled, native_count) = traffic::native_plan(
        suppress_native,
        !state.generated_traffic.is_empty(),
        interface.settings.manage_native_traffic,
        interface.settings.native_traffic_count,
    );
    let arrival_target = openatc_core::arrival::target(
        &interface.arrival,
        &state.plan.arrival_runway,
        &state.plan.approach,
    );
    let arrival_active =
        interface.page == 3 && interface.arrival.airport.icao == state.plan.destination;
    if let Some(target) = arrival_target {
        let Shared {
            interface,
            arrival_sampler,
            ..
        } = &mut *locked;
        if let Some(interface) = interface.as_mut() {
            arrival_sampler.update(
                arrival_active,
                &state.plan.destination,
                &target,
                telemetry.as_ref().unwrap_or(&state.telemetry),
                &mut interface.arrival_terrain,
            );
        }
    }
    {
        let Shared {
            interface,
            weather_sampler,
            ..
        } = &mut *locked;
        if let Some(interface) = interface {
            weather_sampler.update(interface);
        }
    }
    locked.ground_arrows.update(ground_enabled, &state);
    locked.holding_marker.update(holding_enabled, &state);
    locked
        .traffic_holding
        .update(traffic_holding_enabled, &state);
    locked.follow_me.update(follow_me_enabled, &state);
    if state.generated_traffic.is_empty() {
        locked.tcas.receive(&[]);
    }
    let status = locked
        .native_traffic
        .update(native_enabled, native_count)
        .to_owned();
    if let Some(interface) = locked.interface.as_mut() {
        interface.native_traffic_status = status;
    }
    if !state.generated_traffic.is_empty() {
        locked.tcas.receive(&state.generated_traffic);
    }
    let generation = locked
        .interface
        .as_ref()
        .map_or(0, |i| i.flight_reset_generation);
    if generation != locked.last_reset_generation {
        locked.last_reset_generation = generation;
        locked.debug_cancel_pending = false;
        locked.debug_air_pending = None;
        locked.debug_air_target = None;
        locked.crew = crew::Runtime::default();
        locked.last_frequency_sequence = 0;
        locked.last_state_sequence = 0;
        locked.last_ap_seq = 0;
        locked.ground_arrows = taxi::GroundGuidance::new();
        locked.holding_marker = taxi::GroundGuidance::new_holding_marker();
        locked.traffic_holding = taxi::GroundGuidance::new_traffic_holding();
        locked.follow_me = follow_me::FollowMe::new();
        locked.arrival_sampler = arrival::TerrainSampler::new();
        locked.weather_sampler = weather::Sampler::default();
    }
    if locked.interface.as_ref().is_some_and(|i| i.reset_pending) {
        return;
    }
    drop(locked);
    update_tune(shared);
    update_autopilot(shared);
    let mut locked = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let profile = crew::aircraft_profile(locked.panel.profile.as_ref().map(|p| &p.crew));
    let Shared {
        crew, interface, ..
    } = &mut *locked;
    if let Some(interface) = interface.as_ref() {
        crew.observe(&profile, &interface.engine);
        crew.tick(xpapi, &profile, &interface.engine);
    }
}

impl Plugin for OpenAtc {
    type Error = xplane::data::borrowed::FindError;

    fn start(xpapi: &mut xplane::XPAPI) -> Result<Self, Self::Error> {
        let _ = xpapi;
        debug_log("OpenATC AI: Rust plugin start");
        if let Some(file) = supervise::plugin_file_path()
            && let Some(root) = std::path::Path::new(&file)
                .parent()
                .and_then(|p| p.parent())
            && let Err(error) =
                openatc_core::dialogue::load(&root.join("speech/runtime/responses.toml"))
        {
            debug_log(&format!("OpenATC AI: operational speech rejected: {error}"));
        }
        if let Some(file) = supervise::plugin_file_path()
            && let Some(root) = std::path::Path::new(&file)
                .parent()
                .and_then(std::path::Path::parent)
            && root.join("intents.toml").is_file()
            && let Err(error) = openatc_core::catalog::activate(&root.join("intents.toml"))
        {
            debug_log(&format!("OpenATC AI: intent catalogue rejected: {error}"));
        }
        // Everything X-Plane calls runs on the simulator main thread; the Arc
        // below shares ownership between callbacks, never threads.
        #[allow(clippy::arc_with_non_send_sync)]
        let shared = Arc::new(Mutex::new(Shared {
            generated_traffic: generated_traffic::Renderer::default(),
            tcas: tcas::Tcas::default(),
            native_traffic: traffic::NativeTraffic::default(),
            crew: crew::Runtime::default(),
            callouts: callouts::Runtime::default(),
            interface: None,
            native_atis: atis::NativeAtis::default(),
            native_radio: atis::NativeRadio::default(),
            window: None,
            window_state: None,
            refs: None,
            com1: None,
            com2: None,
            panel: panel::Panel::new(),
            supervisor: supervise::Supervisor::new(),
            last_frequency_sequence: 0,
            last_state_sequence: 0,
            last_ap_seq: 0,
            last_reset_generation: 0,
            logged_notice: String::new(),
            logged_role: "init".to_owned(),
            enabled: false,
            ground_arrows: taxi::GroundGuidance::new(),
            holding_marker: taxi::GroundGuidance::new_holding_marker(),
            traffic_holding: taxi::GroundGuidance::new_traffic_holding(),
            follow_me: follow_me::FollowMe::new(),
            debug_cancel_pending: false,
            debug_air_pending: None,
            debug_air_target: None,
            arrival_sampler: arrival::TerrainSampler::new(),
            weather_sampler: weather::Sampler::default(),
        }));
        Ok(Self {
            shared,
            flight_loop: None,
            window_loop: None,
            heartbeat: None,
            commands: Vec::new(),
            menu: None,
        })
    }

    fn enable(&mut self, xpapi: &mut xplane::XPAPI) -> Result<(), Self::Error> {
        debug_log("OpenATC AI: enable datarefs");
        {
            let mut shared = self
                .shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut lines = Vec::new();
            shared.refs = Some(telemetry::find_refs(&mut xpapi.data, &mut lines));
            shared.native_atis.bind(&mut xpapi.data);
            shared.native_radio.bind(&mut xpapi.data);
            for line in lines {
                debug_log(&line);
            }
            shared.com1 = xpapi
                .data
                .find("sim/cockpit2/radios/actuators/com1_frequency_hz_833")
                .ok()
                .and_then(|found: DataRef<i32, xplane::data::ReadOnly>| found.writeable().ok());
            shared.com2 = xpapi
                .data
                .find("sim/cockpit2/radios/actuators/com2_frequency_hz_833")
                .ok()
                .and_then(|found: DataRef<i32, xplane::data::ReadOnly>| found.writeable().ok());
        }
        let mut interface = Interface::new("http://127.0.0.1:8087");
        interface.simulator_host = true;
        let system_path = xpapi.paths.xplane_folder().to_string_lossy().into_owned();
        interface
            .engine
            .post("/simulator/root", serde_json::json!({"root": system_path}));
        let needs_window = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .window
            .is_none();
        // Disabled windows retain their renderer. Reuse its ImGui context.
        if needs_window {
            // Render backend behind the window refcon, drawing the interface.
            let draw_shared = self.shared.clone();
            let mut window_state = Box::new(window::WindowState::new());
            window_state.render.draw_fn = Some(Box::new(move |ui, frame| {
                if let Ok(mut shared) = draw_shared.lock()
                    && let Some(interface) = shared.interface.as_mut()
                {
                    interface.set_window_frame(frame);
                    return interface.draw(ui);
                }
                openatc_ui::interface::WindowActions::default()
            }));
            debug_log("OpenATC AI: enable window");
            // Keep callback state alive for the lifetime of its SDK window.
            if let Some((window, state)) = window::Floater::open(window_state) {
                let mut shared = self
                    .shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                shared.window = Some(window);
                shared.window_state = Some(state);
            } else {
                debug_log("OpenATC AI: window creation failed");
                return Ok(());
            }
        }
        {
            let mut shared = self
                .shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            shared.interface = Some(interface);
            shared.supervisor = supervise::Supervisor::new();
            shared.crew = crew::Runtime::default();
            shared.last_frequency_sequence = 0;
            shared.last_state_sequence = 0;
            shared.last_ap_seq = 0;
            shared.last_reset_generation = 0;
            if let Some(window) = shared.window.as_ref() {
                window.set_visible(true);
            }
        }
        debug_log("OpenATC AI: enable commands");
        if self.commands.is_empty() {
            setup_commands(self, xpapi);
        }
        debug_log("OpenATC AI: enable flight loop");
        // Radio and telemetry stay at 2 Hz; an active escort gets lightweight movement updates.
        let tick_shared = self.shared.clone();
        let mut last_logic = std::time::Instant::now();
        let mut flight_loop = xpapi.new_flight_loop(
            FlightLoopPhase::BeforeFlightModel,
            move |xpapi: &mut xplane::XPAPI,
                  _state: &mut xplane::flight_loop::LoopState<LoopMarker>| {
                if last_logic.elapsed() >= Duration::from_millis(500) {
                    flight_tick(&tick_shared, xpapi);
                    last_logic = std::time::Instant::now();
                }
                let mut shared = tick_shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                shared.follow_me.step();
                // The traffic rate is a user choice: a higher rate moves the traffic more
                // smoothly and spends more of the simulator's frame time.
                let traffic_rate = shared
                    .interface
                    .as_ref()
                    .map(|interface| interface.settings.traffic_rate)
                    .unwrap_or_default();
                // Consume immutable HTTP snapshots independently of the slower telemetry tick.
                if let Some(interface) = shared.interface.as_ref() {
                    let state = interface.engine.state();
                    let root = std::path::Path::new(&interface.settings.simulator_root)
                        .join("Resources/plugins/OpenATC/Resources/CSL");
                    shared.generated_traffic.receive(
                        &state.generated_traffic,
                        &root,
                        &state.generated_animation_channels,
                        state.telemetry.paused,
                        state.generated_traffic_clock,
                    );
                }
                shared.generated_traffic.step(traffic_rate);
                let Shared {
                    generated_traffic,
                    tcas,
                    ..
                } = &mut *shared;
                if tcas.active() {
                    // Do not release the whole override just because the renderer is between
                    // playout samples. Movement snapshots arrive every half second; an empty
                    // interpolation frame must leave the existing targets in place.
                    let frames = generated_traffic.frames();
                    if !frames.is_empty() {
                        tcas.receive(frames);
                    }
                    tcas.refresh();
                }
                if shared.tcas.active() {
                    return LoopResult::NextLoop;
                }
                if shared.follow_me.active()
                    || shared.generated_traffic.active()
                    || shared.callouts.active
                {
                    // Rendering gates its own cadence; TCAS and other frame services keep
                    // running independently of the selected traffic draw rate.
                    return LoopResult::NextLoop;
                }
                LoopResult::Seconds(0.5)
            },
            LoopMarker,
        );
        flight_loop.schedule_after(Duration::from_secs_f32(0.5));
        self.flight_loop = Some(flight_loop);
        let window_shared = self.shared.clone();
        let mut last_callout_sample = std::time::Instant::now();
        let mut window_loop = xpapi.new_flight_loop(
            FlightLoopPhase::BeforeFlightModel,
            move |xpapi: &mut xplane::XPAPI,
                  _state: &mut xplane::flight_loop::LoopState<LoopMarker>| {
                let mut shared = window_shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Shared {
                    window: Some(window),
                    window_state: Some(state),
                    ..
                } = &mut *shared
                {
                    window.apply_pending(state);
                }
                // Callouts sample only their own instruments, independently of radio logic.
                if last_callout_sample.elapsed() >= Duration::from_millis(20) {
                    last_callout_sample = std::time::Instant::now();
                    if let Some(telemetry) = shared.refs.as_ref().map(telemetry::gather_callouts) {
                        let aircraft = shared.panel.aircraft_path.clone();
                        let Shared {
                            interface,
                            callouts,
                            ..
                        } = &mut *shared;
                        if let Some(interface) = interface {
                            callouts.update(&mut xpapi.data, &aircraft, interface, &telemetry);
                        }
                    }
                }
                LoopResult::NextLoop
            },
            LoopMarker,
        );
        window_loop.schedule_immediate();
        self.window_loop = Some(window_loop);
        self.heartbeat = Some(supervise::Heartbeat::start("http://127.0.0.1:8087"));
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .enabled = true;
        debug_log("OpenATC AI: enabled, flight loop running");
        Ok(())
    }

    fn disable(&mut self, _xpapi: &mut xplane::XPAPI) {
        self.window_loop = None;
        self.flight_loop = None;
        self.heartbeat = None;
        let final_settings = {
            let shared = self
                .shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            shared
                .interface
                .as_ref()
                .filter(|interface| interface.settings_loaded)
                .map(|interface| {
                    let mut settings = interface.settings.clone();
                    settings.transcript_fraction = interface.transcript_fraction;
                    (
                        interface.engine.endpoint(),
                        serde_json::to_value(&settings).unwrap_or(serde_json::Value::Null),
                    )
                })
        };
        // Flush the last drag/splitter change before stopping the companion; the
        // normal debounce may not have elapsed when the pilot exits immediately.
        if let Some((endpoint, settings)) = final_settings {
            let _ = openatc_http::post_json(
                &endpoint,
                "/settings",
                &settings,
                Duration::from_millis(250),
            );
        }
        // Only our local companion accepts this session's PID. Independent
        // engines reject it; the remote AI/STT/TTS server is never contacted.
        let _ = openatc_http::post_json(
            "http://127.0.0.1:8087",
            "/plugin/shutdown",
            &serde_json::json!({"parentPid": std::process::id()}),
            Duration::from_millis(250),
        );
        let mut shared = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shared.enabled = false;
        shared.callouts = callouts::Runtime::default();
        shared.native_atis.update(false);
        shared.native_radio.update(false);
        shared.generated_traffic = generated_traffic::Renderer::default();
        shared.tcas = tcas::Tcas::default();
        shared.native_traffic = traffic::NativeTraffic::default();
        shared.ground_arrows = taxi::GroundGuidance::new();
        shared.holding_marker = taxi::GroundGuidance::new_holding_marker();
        shared.traffic_holding = taxi::GroundGuidance::new_traffic_holding();
        shared.follow_me = follow_me::FollowMe::new();
        shared.arrival_sampler = arrival::TerrainSampler::new();
        shared.weather_sampler = weather::Sampler::default();
        if let Some(window) = shared.window.as_ref() {
            window.set_visible(false);
        }
        shared.interface = None;
        shared.crew = crew::Runtime::default();
        shared.refs = None;
        shared.com1 = None;
        shared.com2 = None;
    }

    fn receive_message(
        &mut self,
        _xpapi: &mut xplane::XPAPI,
        _from: i32,
        message: xplane::message::MessageId,
        _param: *mut std::ffi::c_void,
    ) {
        if message == xplane::message::MessageId::ReleasePlanes {
            let mut shared = self
                .shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            shared.native_traffic.yield_to_provider();
            shared.tcas.yield_to_provider();
        }
    }

    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: String::from("OpenATC AI"),
            signature: String::from("org.openatc.development"),
            description: String::from(
                "OpenATC AI development UI and simulator controller interface",
            ),
        }
    }
}

/// Route registered simulator commands to the interface.
struct TalkHandler {
    shared: Arc<Mutex<Shared>>,
    name: String,
}

impl TalkHandler {
    fn with_interface(&self, work: impl FnOnce(&mut Interface)) {
        if let Ok(mut shared) = self.shared.lock()
            && shared.enabled
            && let Some(interface) = shared.interface.as_mut()
        {
            work(interface);
        }
    }
}

impl TalkHandler {
    fn action(&self) -> Option<Action> {
        action_for(&self.name)
    }
}

impl xplane::command::CommandHandler for TalkHandler {
    fn command_begin(&mut self, _x: &mut xplane::XPAPI) -> xplane::command::CommandHandlerResult {
        match self.action() {
            Some(Action::ToggleWindow) => {
                if let Ok(mut shared) = self.shared.lock() {
                    let opening = shared
                        .window
                        .as_ref()
                        .is_some_and(|window| !window.is_visible());
                    if opening && let Some(interface) = shared.interface.as_mut() {
                        interface.expand();
                    }
                    if let Some(window) = shared.window.as_ref() {
                        window.set_visible(opening);
                    }
                }
            }
            Some(Action::Talk) => {
                debug_log("OpenATC AI: openatc/talk recording");
                self.with_interface(|interface| interface.talk_push_to_talk(true, false));
                if let Ok(shared) = self.shared.lock()
                    && let Some(window) = shared.window.as_ref()
                {
                    window.set_visible(true);
                }
            }
            Some(Action::TalkCopilot) => {
                debug_log("OpenATC AI: openatc/talk_copilot recording");
                self.with_interface(|interface| interface.talk_push_to_talk(true, true));
                if let Ok(shared) = self.shared.lock()
                    && let Some(window) = shared.window.as_ref()
                {
                    window.set_visible(true);
                }
            }
            Some(Action::Transmit) => {
                let radio = if self.name == "openatc/transmit_com2" {
                    openatc_core::state::Radio::Com2
                } else {
                    openatc_core::state::Radio::Com1
                };
                self.with_interface(|interface| interface.radio_push_to_talk(true, radio));
            }
            Some(Action::MicPushToTalk) => {
                debug_log("OpenATC AI: openatc/mic_push_to_talk recording");
                self.with_interface(|interface| interface.mic_push_to_talk(true));
            }
            Some(Action::CopyError) => self.with_interface(|interface| {
                let notice = interface.last_notice();
                debug_log(&format!(
                    "OpenATC AI error report: {}",
                    if notice.is_empty() {
                        "no current error"
                    } else {
                        &notice
                    }
                ));
            }),
            // Every registered binding maps to an action; an unknown name cannot reach here
            // because setup only registers the commands listed in COMMANDS.
            None => {}
        }
        xplane::command::CommandHandlerResult::AllowXPlaneProcessing
    }

    fn command_continue(
        &mut self,
        _x: &mut xplane::XPAPI,
    ) -> xplane::command::CommandHandlerResult {
        xplane::command::CommandHandlerResult::AllowXPlaneProcessing
    }

    fn command_end(&mut self, _x: &mut xplane::XPAPI) -> xplane::command::CommandHandlerResult {
        match self.name.as_str() {
            "openatc/talk" => {
                self.with_interface(|interface| interface.talk_push_to_talk(false, false));
            }
            "openatc/talk_copilot" => {
                self.with_interface(|interface| interface.talk_push_to_talk(false, true));
            }
            "openatc/transmit_com1" | "openatc/transmit_com2" => {
                let radio = if self.name == "openatc/transmit_com2" {
                    openatc_core::state::Radio::Com2
                } else {
                    openatc_core::state::Radio::Com1
                };
                self.with_interface(|interface| interface.radio_push_to_talk(false, radio));
            }
            "openatc/mic_push_to_talk" => {
                self.with_interface(|interface| interface.mic_push_to_talk(false));
            }
            _ => {}
        }
        xplane::command::CommandHandlerResult::AllowXPlaneProcessing
    }
}

/// Menu item toggling the floating window.
struct ToggleItem {
    shared: Arc<Mutex<Shared>>,
}

impl xplane::menu::ClickHandler for ToggleItem {
    fn item_clicked(&mut self, _x: &mut xplane::XPAPI, _item: &xplane::menu::ActionItem) {
        if let Ok(shared) = self.shared.lock()
            && shared.enabled
            && let Some(window) = shared.window.as_ref()
        {
            window.set_visible(!window.is_visible());
        }
    }
}

/// What a registered binding does. Both halves of a push-to-talk command share one action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    ToggleWindow,
    Talk,
    TalkCopilot,
    Transmit,
    MicPushToTalk,
    CopyError,
}

fn action_for(name: &str) -> Option<Action> {
    Some(match name {
        "openatc/toggle_window" => Action::ToggleWindow,
        "openatc/talk" => Action::Talk,
        "openatc/talk_copilot" => Action::TalkCopilot,
        "openatc/transmit_com1" | "openatc/transmit_com2" => Action::Transmit,
        "openatc/mic_push_to_talk" => Action::MicPushToTalk,
        "openatc/copy_error" => Action::CopyError,
        _ => return None,
    })
}

/// Every simulator binding this plugin registers, with its user-facing description.
const COMMANDS: [(&str, &str); 7] = [
    ("openatc/toggle_window", "Toggle OpenATC AI window"),
    (
        "openatc/transmit_com1",
        "Transmit COM1 — hold to speak, release to transmit",
    ),
    (
        "openatc/transmit_com2",
        "Transmit COM2 — hold to speak, release to transmit",
    ),
    (
        "openatc/talk",
        "Hold to talk on the radio, release to transmit",
    ),
    (
        "openatc/talk_copilot",
        "Hold to talk to the copilot, release to transmit",
    ),
    (
        "openatc/mic_push_to_talk",
        "Hold to record microphone, release to transcribe",
    ),
    ("openatc/copy_error", "Write the current error to Log.txt"),
];

/// Register radio, crew and window bindings plus the Plugins menu entry.
fn setup_commands(plugin: &mut OpenAtc, xpapi: &mut xplane::XPAPI) {
    for (name, description) in COMMANDS {
        if let Ok(mut command) = xpapi.command.try_new(name, description) {
            let handler = TalkHandler {
                shared: plugin.shared.clone(),
                name: name.to_owned(),
            };
            let registered = command.handle(handler, true);
            plugin.commands.push((command, registered));
        }
    }
    // Plugins menu entry toggling the window.
    if let Ok(menu) = xpapi.menu.new_menu("OpenATC AI") {
        if let Ok(item) = xpapi.menu.new_action_item(
            "Show / hide",
            ToggleItem {
                shared: plugin.shared.clone(),
            },
        ) {
            menu.add_child(item);
        }
        if menu.add_to_plugins_menu().is_ok() {
            plugin.menu = Some(menu);
        }
    }
}

xplane_plugin!(OpenAtc);

#[cfg(test)]
mod binding_tests {
    use super::{COMMANDS, action_for};

    #[test]
    fn every_registered_binding_has_a_handler() {
        // A binding that is registered but not handled would silently do nothing when a user
        // pressed it, so the two lists must not be able to drift apart.
        for (name, description) in COMMANDS {
            assert!(
                action_for(name).is_some(),
                "{name} is registered as \"{description}\" but has no handler"
            );
            assert!(!name.is_empty() && !description.is_empty());
        }
        assert_eq!(
            COMMANDS.len(),
            7,
            "bindings changed; update the documentation"
        );
        assert!(action_for("openatc/not_a_binding").is_none());
    }

    #[test]
    fn both_radio_transmit_bindings_share_one_action_and_keep_their_own_receiver() {
        assert_eq!(
            action_for("openatc/transmit_com1"),
            action_for("openatc/transmit_com2")
        );
        assert_ne!(
            action_for("openatc/transmit_com1"),
            action_for("openatc/talk")
        );
    }
}
