//! X-Plane plugin lifecycle, window, flight-loop callbacks and commands.
//! SDK calls run on the simulator thread; the companion engine handles dialogue.

#![allow(unsafe_code)]

mod arrival;
mod geometry;
mod panel;
mod render;
mod supervise;
mod taxi;
mod telemetry;
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
    interface: Option<Interface>,
    window: Option<window::Floater>,
    window_state: Option<Box<window::WindowState>>,
    refs: Option<telemetry::Refs>,
    com1: Option<DataRef<i32, xplane::data::ReadWrite>>,
    panel: panel::Panel,
    supervisor: supervise::Supervisor,
    last_frequency_sequence: u32,
    last_state_sequence: u32,
    /// Clearance sequence the copilot last flew or advised on.
    last_ap_seq: u32,
    logged_notice: String,
    logged_role: String,
    enabled: bool,
    ground_arrows: taxi::GroundArrows,
    arrival_sampler: arrival::TerrainSampler,
    weather_sampler: weather::Sampler,
}

/// The plugin object owned by the simulator host.
struct OpenAtc {
    shared: Arc<Mutex<Shared>>,
    flight_loop: Option<FlightLoop<LoopMarker>>,
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
    let role = locked.panel.role();
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
            settings.copilot_tunes || settings.auto_tune_handoff,
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
    if copilot_tunes
        && snapshot.frequency_sequence > locked.last_frequency_sequence
        && (118_000..=136_990).contains(&snapshot.recommended_frequency_khz)
        && let Some(com1) = locked.com1.as_mut()
    {
        com1.set(snapshot.recommended_frequency_khz);
        locked.last_frequency_sequence = snapshot.frequency_sequence;
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
        if !settings.copilot_flies_ap {
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
    interface.tick();
    if interface.engine.connected() && interface.settings.simulator_root.is_empty() {
        let root = xpapi.paths.xplane_folder().to_string_lossy().into_owned();
        interface
            .engine
            .post("/simulator/root", serde_json::json!({"root": root}));
    }
    let requested_frequency = interface
        .requested_frequency_khz
        .take()
        .filter(|frequency| interface.radio_power && (118_000..=136_990).contains(frequency));
    // Send telemetry after processing the current UI requests.
    if let Some(telemetry) = &telemetry {
        interface.engine.post(
            "/telemetry",
            serde_json::to_value(telemetry).unwrap_or(serde_json::Value::Null),
        );
    }
    let state = interface.engine.state();
    let ground_enabled = interface.settings.show_ground_taxi_arrows;
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
    if let Some(frequency) = requested_frequency {
        if let Some(radio) = locked.com1.as_mut() {
            radio.set(frequency);
            if let Some(interface) = locked.interface.as_mut() {
                interface.last_action =
                    format!("Tuning COM1 to {:.3} MHz", f64::from(frequency) / 1000.0);
            }
        } else if let Some(interface) = locked.interface.as_mut() {
            "COM1 cannot be tuned from this aircraft's radio dataref."
                .clone_into(&mut interface.notice);
        }
    }
    drop(locked);
    update_tune(shared);
    update_autopilot(shared);
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
        // Everything X-Plane calls runs on the simulator main thread; the Arc
        // below shares ownership between callbacks, never threads.
        #[allow(clippy::arc_with_non_send_sync)]
        let shared = Arc::new(Mutex::new(Shared {
            interface: None,
            window: None,
            window_state: None,
            refs: None,
            com1: None,
            panel: panel::Panel::new(),
            supervisor: supervise::Supervisor::new(),
            last_frequency_sequence: 0,
            last_state_sequence: 0,
            last_ap_seq: 0,
            logged_notice: String::new(),
            logged_role: "init".to_owned(),
            enabled: false,
            ground_arrows: taxi::GroundArrows::new(),
            arrival_sampler: arrival::TerrainSampler::new(),
            weather_sampler: weather::Sampler::default(),
        }));
        Ok(Self {
            shared,
            flight_loop: None,
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
            for line in lines {
                debug_log(&line);
            }
            shared.com1 = xpapi
                .data
                .find("sim/cockpit2/radios/actuators/com1_frequency_hz_833")
                .ok()
                .and_then(|found: DataRef<i32, xplane::data::ReadOnly>| found.writeable().ok());
        }
        let mut interface = Interface::new("http://127.0.0.1:8087");
        interface.simulator_host = true;
        let system_path = xpapi.paths.xplane_folder().to_string_lossy().into_owned();
        interface
            .engine
            .post("/simulator/root", serde_json::json!({"root": system_path}));
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
        // Floating window owns the state box lifetime from here on.
        if let Some((window, state)) = window::Floater::open(window_state) {
            let mut shared = self
                .shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            shared.window = Some(window);
            shared.window_state = Some(state);
            shared.interface = Some(interface);
        } else {
            debug_log("OpenATC AI: window creation failed");
            return Ok(());
        }
        debug_log("OpenATC AI: enable commands");
        setup_commands(self, xpapi);
        debug_log("OpenATC AI: enable flight loop");
        // Half-second flight loop with the shared state.
        let tick_shared = self.shared.clone();
        let mut flight_loop = xpapi.new_flight_loop(
            FlightLoopPhase::BeforeFlightModel,
            move |xpapi: &mut xplane::XPAPI,
                  _state: &mut xplane::flight_loop::LoopState<LoopMarker>| {
                flight_tick(&tick_shared, xpapi);
                LoopResult::Seconds(0.5)
            },
            LoopMarker,
        );
        flight_loop.schedule_after(Duration::from_secs_f32(0.5));
        self.flight_loop = Some(flight_loop);
        self.heartbeat = Some(supervise::Heartbeat::start("http://127.0.0.1:8087"));
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .enabled = true;
        debug_log("OpenATC AI: enabled, flight loop running");
        Ok(())
    }

    fn disable(&mut self, _xpapi: &mut xplane::XPAPI) {
        self.flight_loop = None;
        self.heartbeat = None;
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
        shared.ground_arrows = taxi::GroundArrows::new();
        shared.arrival_sampler = arrival::TerrainSampler::new();
        shared.weather_sampler = weather::Sampler::default();
        if let Some(window) = shared.window.as_ref() {
            window.set_visible(false);
        }
        shared.interface = None;
    }

    fn receive_message(
        &mut self,
        _xpapi: &mut xplane::XPAPI,
        _from: i32,
        _message: xplane::message::MessageId,
        _param: *mut std::ffi::c_void,
    ) {
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

/// Command handler routing the five plugin commands to the interface.
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

impl xplane::command::CommandHandler for TalkHandler {
    fn command_begin(&mut self, _x: &mut xplane::XPAPI) -> xplane::command::CommandHandlerResult {
        match self.name.as_str() {
            "openatc/toggle_window" => {
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
            "openatc/talk" => {
                debug_log("OpenATC AI: openatc/talk recording");
                self.with_interface(|interface| interface.talk_push_to_talk(true, false));
                if let Ok(shared) = self.shared.lock()
                    && let Some(window) = shared.window.as_ref()
                {
                    window.set_visible(true);
                }
            }
            "openatc/talk_copilot" => {
                debug_log("OpenATC AI: openatc/talk_copilot recording");
                self.with_interface(|interface| interface.talk_push_to_talk(true, true));
                if let Ok(shared) = self.shared.lock()
                    && let Some(window) = shared.window.as_ref()
                {
                    window.set_visible(true);
                }
            }
            "openatc/mic_push_to_talk" => {
                debug_log("OpenATC AI: openatc/mic_push_to_talk recording");
                self.with_interface(|interface| interface.mic_push_to_talk(true));
            }
            "openatc/copy_error" => self.with_interface(|interface| {
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
            _ => {}
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
            && let Some(window) = shared.window.as_ref()
        {
            window.set_visible(!window.is_visible());
        }
    }
}

/// Register the five plugin commands plus the Plugins menu entry.
fn setup_commands(plugin: &mut OpenAtc, xpapi: &mut xplane::XPAPI) {
    // Commands: toggle, talk, copilot, mic, error copy.
    for (name, description) in [
        ("openatc/toggle_window", "Toggle OpenATC AI window"),
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
    ] {
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
