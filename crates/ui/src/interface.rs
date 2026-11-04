//! UI state, asynchronous replies and frame updates.

// Layout tests compare exact fixture coordinates.
#![allow(clippy::float_cmp)]
// Layout limits bound the numeric casts below.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use crate::client::EngineClient;
use crate::widgets;
use openatc_audio::Speech;
use openatc_core::airport::Airport;
use openatc_core::arrival::{ArrivalAirport, TerrainProfile};
use openatc_core::identify::interpret_text;
use openatc_core::intents::{can_transmit, resolve_crew_role};
use openatc_core::state::{FlightPlan, State, Transmission};
use openatc_settings::Settings;

/// True when a transmission belongs to the station tuned on its radio.
pub(crate) fn on_tuned_frequency(
    entry: &Transmission,
    state: &State,
    radio: openatc_core::state::Radio,
) -> bool {
    entry.radio.unwrap_or_default() == radio
        && entry
            .frequency
            .is_none_or(|frequency| state.telemetry.frequency(radio) == Some(frequency))
}

/// A handoff is read back when a crew or pilot transmission follows it, even
/// if the instruction marker has not cleared yet.
fn handoff_readback_spoken(state: &State, sequence: u32) -> bool {
    state.transcript.iter().any(|entry| {
        entry.sequence > sequence
            && (entry.speaker == "COPILOT" || entry.speaker == state.plan.callsign)
    })
}

/// True when the entry's station is tuned on the radio it names.
pub(crate) fn entry_frequency_tuned(entry: &Transmission, state: &State) -> bool {
    entry.frequency.is_none_or(|frequency| {
        state.telemetry.frequency(entry.radio.unwrap_or_default()) == Some(frequency)
    })
}

/// Local notice shown under the transcript (power refusals and the like).
pub struct LocalNotice {
    /// Speaker tag, usually SYSTEM.
    pub speaker: String,
    /// Notice text.
    pub text: String,
}

/// Stable page indices used by saved navigation and the desktop preview.
pub const PAGES: [&str; 11] = [
    "ATC",
    "Flight Plan",
    "Taxi",
    "Arrival",
    "Airports",
    "Settings",
    "Radios",
    "Map",
    "Traffic",
    "Missions",
    "ATC COM2",
];

#[derive(Clone, Copy, Debug)]
/// In-sim window placement and pop-out state.
pub struct WindowFrame {
    /// Native floating-window bounds, when available.
    pub native_bounds: Option<[i32; 4]>,
    /// Top-left corner in screen points.
    pub position: [f32; 2],
    /// Window size in screen points.
    pub size: [f32; 2],
    /// Whether the window may leave the simulator.
    pub can_pop_out: bool,
    /// Whether the window currently floats outside.
    pub popped_out: bool,
}

#[derive(Clone, Copy, Debug, Default)]
/// One-frame window requests collected while drawing.
pub struct WindowActions {
    /// Restore saved native bounds once settings arrive.
    pub restore_bounds: Option<[i32; 4]>,
    /// Close button pressed.
    pub close: bool,
    /// Pop-out toggle pressed.
    pub toggle_pop_out: bool,
    /// Keep simulator shortcuts out while a widget owns keyboard input.
    pub capture_keyboard: bool,
    /// Window drag distance this frame.
    pub drag_delta: [f32; 2],
    /// Bottom-right resize grip movement in UI pixels.
    pub resize_delta: [f32; 2],
    /// Requested content height, if any.
    pub height: Option<f32>,
    /// Minimum window size.
    pub minimum_size: [f32; 2],
    /// Interface scale override.
    pub ui_scale: f32,
}

/// State shared by the interface pages and frame updates.
#[allow(clippy::struct_excessive_bools)]
pub struct Interface {
    /// Radio selected for ATC requests, retained while browsing other pages.
    pub active_radio: openatc_core::state::Radio,
    /// Radio captured when push-to-talk starts.
    pub ptt_radio: openatc_core::state::Radio,
    /// Cached COM2 display snapshot, rebuilt only when engine telemetry changes.
    com2_view: Option<(std::sync::Arc<State>, std::sync::Arc<State>)>,
    pub(crate) settings_section: usize,
    pub(crate) setup_server_url: String,
    pub(crate) traffic_profiles: serde_json::Value,
    /// Native simulator traffic ownership and active count.
    pub native_traffic_status: String,
    pub(crate) traffic_selected: u32,
    pub(crate) traffic_notice: String,
    pub(crate) scenery_status: serde_json::Value,
    pub(crate) scenery_checked: f64,
    pub(crate) scenery_pending: bool,
    pub(crate) setup_status: serde_json::Value,
    pub(crate) setup_checked: f64,
    pub(crate) setup_pending: bool,
    pub(crate) setup_overlay: bool,
    pub(crate) setup_seen_run: u64,
    pub(crate) traffic_message: String,
    /// Quick request just sent; blocks a double-click until the state catches up.
    pub(crate) pending_request: Option<(String, std::time::Instant)>,
    pub(crate) model_install_status: serde_json::Value,
    pub(crate) model_install_checked: f64,
    pub(crate) model_install_pending: bool,
    pub(crate) missions: crate::missions::Browser,
    /// Engine connection plus snapshot cache.
    pub engine: EngineClient,
    /// Airport containing the aircraft according to scenery boundaries or pavement.
    pub occupied_airport: String,
    /// Receivable published stations around the aircraft.
    pub nearby_stations: Vec<openatc_core::stations::Station>,
    /// Station/weather reception status.
    pub radio_notice: String,
    /// Takeoff speed and cached audio diagnostics.
    pub callout_status: String,
    /// Voice names returned by the configured TTS service.
    pub available_voices: Vec<openatc_core::voices::Voice>,
    /// Voice catalogue request is pending.
    pub voices_loading: bool,
    /// Voice API progress or error message.
    pub voices_notice: String,
    voices_origin: String,
    /// Search text for voice names and languages.
    pub voice_filter: String,
    /// English text shared by voice auditions.
    pub voice_preview_text: String,
    /// Controller polygons around the current map viewport.
    pub map_stations: Vec<openatc_core::stations::Station>,
    /// Airport markers around the current map viewport.
    pub map_airports: Vec<openatc_core::stations::AirportMarker>,
    /// Show published volumes outside the aircraft altitude band.
    pub map_all_altitudes: bool,
    /// Show airport markers and their hover details.
    pub map_airports_visible: bool,
    /// Traffic visibility is independent on the airport and sector maps.
    pub taxi_traffic_visible: bool,
    /// Show observed traffic on the developer sector map.
    pub map_traffic_visible: bool,
    /// Show the filed plan on the developer sector map.
    pub map_plan_visible: bool,
    /// Include heliports among the airport markers.
    pub map_heliports: bool,
    /// Map catalogue status.
    pub map_notice: String,
    map_checked: f64,
    map_loading: bool,
    /// Debug sector map scale, in nautical miles from the aircraft.
    pub radio_map_range: f32,
    /// Debug map follows the aircraft until dragged.
    pub radio_map_center: Option<[f64; 2]>,
    /// Arrival stand name or terminal search.
    pub parking_filter: String,
    /// Search configured aircraft crew controls.
    pub crew_control_filter: String,
    /// Station index/discovery request is pending.
    pub stations_loading: bool,
    stations_checked: f64,
    atis_checked: f64,
    atis_frequency: i32,
    atis_radio: openatc_core::state::Radio,
    atis_airport: String,
    atis_pending: bool,
    /// The native host suppresses its own ATIS while this broadcast is selected.
    pub receives_atis: bool,
    /// Audio capture and playback.
    pub speech: Speech,
    pub(crate) speech_com2: Speech,
    /// Open page index.
    pub page: usize,
    /// Settings snapshot loaded from the engine.
    pub settings_loaded: bool,
    /// Draft flight plan loaded from the engine.
    pub plan_loaded: bool,
    /// Operator settings mirror.
    pub settings: Settings,
    /// Dispatch draft being edited.
    pub draft: FlightPlan,
    /// Airport under inspection.
    pub airport: Airport,
    pub(crate) map_geometry: crate::maps::Geometry,
    /// Destination geometry and installed approach reference.
    pub arrival: openatc_core::arrival::ArrivalAirport,
    /// Main-thread scenery samples for the destination profile.
    pub arrival_terrain: openatc_core::arrival::TerrainProfile,
    /// Destination request in flight.
    pub arrival_loading: bool,
    /// Last requested destination.
    pub arrival_requested: String,
    /// Destination loading status.
    pub arrival_notice: String,
    /// Last destination retry time.
    pub arrival_retry_at: f64,
    /// Nearby airport services, independent of the airport search map.
    pub local_airport: Airport,
    /// Local airport request in flight.
    pub local_loading: bool,
    /// Last nearby airport requested.
    pub local_requested: String,
    /// Local airport status.
    pub local_notice: String,
    /// Message box text.
    pub message: String,
    /// Request filter text.
    pub search: String,
    /// Waypoint for direct-to modal.
    pub requested_waypoint: String,
    /// Airport browser ICAO.
    pub airport_code: String,
    /// Taxi map airport selection; starts with departure/current airport.
    pub taxi_arrival: bool,
    /// Prefer the current simulator airport on first opening.
    pub taxi_use_current: bool,
    /// Current simulator airport supplied by the plugin.
    pub current_airport: String,
    /// Last requested automatic taxi airport.
    pub taxi_requested: String,
    /// Engine refusal line.
    pub notice: String,
    /// Plan page notice.
    pub plan_notice: String,
    /// Own the desktop clipboard selection until plugin unload.
    pub clipboard: Option<arboard::Clipboard>,
    /// Clearance waiting on Planning submission.
    pub pending_clearance: Option<openatc_core::state::Request>,
    /// Airport page notice.
    pub airport_notice: String,
    /// Settings page notice.
    pub settings_notice: String,
    /// Last saved settings JSON for change detection.
    pub last_settings_json: String,
    /// When settings last changed.
    pub settings_changed_at: f64,
    /// Last user interaction time.
    pub last_interaction: f64,
    /// Last tuned COM1 frequency in kHz, for stopping speech on change.
    pub last_com1_khz: i32,
    /// Last tuned COM2 frequency in kHz, for stopping speech on change.
    pub last_com2_khz: Option<i32>,
    /// Plan already auto-enabled for generated traffic.
    auto_generated_plan: String,
    /// Open request modal intent.
    pub modal_intent: String,
    /// Open request modal title.
    pub modal_title: String,
    /// Requested altitude feet for the modal.
    pub requested_altitude: i32,
    /// Voice pool popup visible.
    pub show_voice_popup: bool,
    /// Cockpit panel role (call-light routing).
    pub panel_role: String,
    /// Panel role source.
    pub panel_source: String,
    /// Engine fault line.
    pub engine_fault: String,
    /// Radio power state.
    pub radio_power: bool,
    /// Matched aircraft profile reported by the simulator plugin.
    pub aircraft_profile: String,
    /// Bus power state.
    pub bus_power: bool,
    /// Local notices under the transcript.
    pub local_notices: Vec<LocalNotice>,
    /// Last power refusal kind, to log once per outage.
    pub last_power_refusal: String,
    /// Voice health line.
    pub voice_health: String,
    /// AI server version reported by the engine.
    pub ai_version: String,
    /// Engine version reported by the latest state snapshot.
    pub engine_version: String,
    /// Last voice check time.
    pub last_voice_check: f64,
    /// Last action line for dev mode.
    pub last_action: String,
    /// Prevent duplicate automatic replies while the command is in flight.
    pub auto_reply_pending: bool,
    /// Radio associated with the automatic reply currently in flight.
    auto_reply_radio: openatc_core::state::Radio,
    /// Copilot transmission is queued; its decision waits until playback finishes.
    pub copilot_reply_prepared: bool,
    /// Prepared transmission that must reach the local audio queue before commit.
    pub copilot_prepared_sequence: u32,
    /// Last controller instruction submitted for an automatic readback.
    pub last_copilot_reply_sequence: u32,
    /// Last handoff followed by an automatic controller check-in.
    pub last_checkin_sequence: u32,
    /// Push-to-talk armed.
    pub ptt_armed: bool,
    /// Push-to-talk role override.
    pub ptt_role: String,
    /// Record start time.
    pub record_start: f64,
    /// Record peak level.
    pub record_peak: f32,
    /// Map zoom.
    pub zoom: f32,
    /// Map pan.
    pub pan: [f32; 2],
    /// `SimBrief` import running.
    pub importing: bool,
    /// Airport load running.
    pub loading_airport: bool,
    /// Map layer toggles.
    pub show_runways: bool,
    /// Navaid layer toggle.
    pub show_navaids: bool,
    /// 3D orbit view toggle.
    pub orbit_view: bool,
    /// Demo buildings toggle.
    pub show_buildings: bool,
    /// Orbit yaw radians.
    pub yaw: f32,
    /// Orbit pitch radians.
    pub pitch: f32,
    /// Last transcript sequence spoken.
    pub last_transcript_sequence: u32,
    pub(crate) transcript_scroll: f32,
    pub(crate) transcript_follow_latest: bool,
    pub(crate) transcript_layout: Option<([f32; 2], bool)>,
    /// Last transcript sequence queued for speech.
    pub last_speech_sequence: u32,
    /// Last handoff sequence already applied to the radio.
    last_handoff_tuned: u32,
    /// Pending speech entries.
    pub speech_queue: std::collections::VecDeque<Transmission>,
    /// Background chatter currently playing: radio, frequency and gap after it.
    chatter_playing: Option<(openatc_core::state::Radio, i32, u32)>,
    /// Earliest next background chatter line per radio (COM1, COM2).
    chatter_gate_until: [f64; 2],
    /// Panel window offset in screen points. Stays zero on desktop; the
    /// simulator host sets it so content lands inside the floating window.
    pub window_offset: [f32; 2],
    /// Last measured panel size.
    pub window_size: [f32; 2],
    /// Communications-only compact layout.
    pub compact: bool,
    /// Transcript share of the ATC page.
    pub transcript_fraction: f32,
    pending_window_bounds: Option<[i32; 4]>,
    pending_mode_height: Option<f32>,
    /// Frequency picked on the radio page, if any.
    pub requested_frequency_khz: Option<i32>,
    /// Squawk selected from a clearance or transcript link.
    pub requested_squawk: Option<i32>,
    /// Radio receiving a manual frequency selection.
    pub requested_frequency_radio: openatc_core::state::Radio,
    /// Debug-only simulator reposition request.
    pub requested_debug_jump: Option<String>,
    /// Requested airborne true airspeed; zero preserves the current TAS.
    pub debug_jump_speed: f32,
    /// Suspend automatic speech/readbacks while a native checkpoint is loading.
    pub debug_jump_busy: bool,
    /// True when the plugin (not desktop) hosts the panel.
    pub simulator_host: bool,
    /// Background opacity when faded.
    pub background_opacity: f32,
    /// Reset confirmation armed.
    pub confirm_reset: bool,
    /// Session reset awaiting an engine response.
    pub reset_pending: bool,
    /// Reset failure shown beside the retry controls.
    pub reset_error: String,
    #[cfg(test)]
    pub(crate) reset_button_bounds: [[f32; 4]; 2],
    #[cfg(test)]
    pub(crate) toolbar_bounds: [[f32; 4]; 12],
    #[cfg(test)]
    pub(crate) collapse_bounds: [f32; 4],
    #[cfg(test)]
    pub(crate) plan_callsign_bounds: [f32; 4],
    #[cfg(test)]
    pub(crate) toolbar_drag_bounds: [f32; 4],
    /// Notifies the simulator host to clear flight-local callbacks and guidance.
    pub flight_reset_generation: u64,
    can_pop_out: bool,
    popped_out: bool,
    expanded_height: f32,
    manual_compact: bool,
    window_actions: WindowActions,
    pending_settings_json: String,
}

impl Interface {
    /// Open a settings section without relying on its position in the window.
    pub fn show_settings_section(&mut self, section: usize) {
        self.settings_section = section.min(10);
        self.set_page(5);
    }
    /// Refresh controller choices after an aircraft-confirmed manual tune.
    pub fn radio_tuned(&mut self, frequency: i32) {
        self.last_action = format!(
            "{} tuned to {:.3} MHz",
            if self.requested_frequency_radio == openatc_core::state::Radio::Com2 {
                "COM2"
            } else {
                "COM1"
            },
            f64::from(frequency) / 1000.0
        );
        self.search.clear();
        self.stations_checked = 0.0;
    }
    /// Connect to the engine at the endpoint.
    #[must_use]
    pub fn new(endpoint: &str) -> Self {
        Self {
            settings_section: 0,
            setup_server_url: "http://127.0.0.1:8099".into(),
            traffic_profiles: serde_json::Value::Null,
            native_traffic_status: String::new(),
            traffic_selected: 0,
            traffic_notice: String::new(),
            scenery_status: serde_json::Value::Null,
            scenery_checked: -1.0,
            scenery_pending: false,
            setup_status: serde_json::Value::Null,
            setup_checked: -1.0,
            setup_pending: false,
            setup_overlay: false,
            setup_seen_run: 0,
            traffic_message: String::new(),
            pending_request: None,
            model_install_status: serde_json::Value::Null,
            model_install_checked: 0.0,
            model_install_pending: false,
            missions: crate::missions::Browser::default(),
            engine: EngineClient::new(endpoint),
            occupied_airport: String::new(),
            nearby_stations: Vec::new(),
            radio_notice: String::new(),
            callout_status: String::new(),
            available_voices: Vec::new(),
            voices_loading: false,
            voices_notice: String::new(),
            voices_origin: String::new(),
            voice_filter: String::new(),
            voice_preview_text: String::new(),
            map_stations: Vec::new(),
            map_airports: Vec::new(),
            map_all_altitudes: false,
            map_airports_visible: true,
            taxi_traffic_visible: true,
            map_traffic_visible: true,
            map_plan_visible: false,
            map_heliports: false,
            map_notice: String::new(),
            map_checked: 0.0,
            map_loading: false,
            radio_map_range: 80.0,
            radio_map_center: None,
            parking_filter: String::new(),
            crew_control_filter: String::new(),
            stations_loading: false,
            stations_checked: 0.0,
            atis_checked: 0.0,
            atis_frequency: 0,
            atis_radio: openatc_core::state::Radio::Com1,
            atis_airport: String::new(),
            atis_pending: false,
            receives_atis: false,
            speech: Speech::default(),
            speech_com2: Speech::default(),
            page: 0,
            active_radio: openatc_core::state::Radio::Com1,
            ptt_radio: openatc_core::state::Radio::Com1,
            com2_view: None,
            settings_loaded: false,
            plan_loaded: false,
            settings: Settings::default(),
            draft: FlightPlan::default(),
            airport: Airport::default(),
            map_geometry: crate::maps::Geometry::default(),
            arrival: ArrivalAirport::default(),
            arrival_terrain: TerrainProfile::default(),
            arrival_loading: false,
            arrival_requested: String::new(),
            arrival_notice: String::new(),
            arrival_retry_at: 0.0,
            local_airport: Airport::default(),
            local_loading: false,
            local_requested: String::new(),
            local_notice: String::new(),
            message: String::new(),
            search: String::new(),
            requested_waypoint: String::new(),
            airport_code: String::new(),
            taxi_arrival: false,
            taxi_use_current: true,
            current_airport: String::new(),
            taxi_requested: String::new(),
            notice: String::new(),
            plan_notice: String::new(),
            pending_clearance: None,
            clipboard: None,
            airport_notice: String::new(),
            settings_notice: String::new(),
            last_settings_json: String::new(),
            settings_changed_at: 0.0,
            last_interaction: 0.0,
            last_com1_khz: 0,
            last_com2_khz: None,
            auto_generated_plan: String::new(),
            modal_intent: String::new(),
            modal_title: String::new(),
            requested_altitude: 32000,
            show_voice_popup: false,
            panel_role: String::new(),
            panel_source: String::new(),
            engine_fault: String::new(),
            radio_power: true,
            aircraft_profile: String::new(),
            bus_power: true,
            local_notices: Vec::new(),
            last_power_refusal: String::new(),
            voice_health: String::new(),
            ai_version: String::new(),
            engine_version: String::new(),
            last_voice_check: 0.0,
            last_action: String::new(),
            auto_reply_pending: false,
            auto_reply_radio: openatc_core::state::Radio::Com1,
            copilot_reply_prepared: false,
            copilot_prepared_sequence: 0,
            last_copilot_reply_sequence: 0,
            last_checkin_sequence: 0,
            ptt_armed: false,
            ptt_role: String::new(),
            record_start: 0.0,
            record_peak: 0.0,
            zoom: 1.0,
            pan: [0.0, 0.0],
            importing: false,
            loading_airport: false,
            show_runways: true,
            show_navaids: true,
            orbit_view: false,
            show_buildings: true,
            yaw: 0.35,
            pitch: 0.85,
            last_transcript_sequence: 0,
            transcript_scroll: 0.0,
            transcript_follow_latest: true,
            transcript_layout: None,
            last_speech_sequence: 0,
            last_handoff_tuned: 0,
            speech_queue: std::collections::VecDeque::new(),
            chatter_playing: None,
            chatter_gate_until: [0.0; 2],
            window_offset: [0.0, 0.0],
            window_size: [960.0, 760.0],
            compact: false,
            transcript_fraction: 0.57,
            pending_window_bounds: None,
            pending_mode_height: None,
            requested_frequency_khz: None,
            requested_squawk: None,
            requested_frequency_radio: openatc_core::state::Radio::Com1,
            requested_debug_jump: None,
            debug_jump_speed: 0.0,
            debug_jump_busy: false,
            simulator_host: false,
            background_opacity: 0.96,
            confirm_reset: false,
            reset_pending: false,
            reset_error: String::new(),
            #[cfg(test)]
            reset_button_bounds: [[0.0; 4]; 2],
            #[cfg(test)]
            toolbar_bounds: [[0.0; 4]; 12],
            #[cfg(test)]
            collapse_bounds: [0.0; 4],
            #[cfg(test)]
            plan_callsign_bounds: [0.0; 4],
            #[cfg(test)]
            toolbar_drag_bounds: [0.0; 4],
            flight_reset_generation: 0,
            can_pop_out: false,
            popped_out: false,
            expanded_height: 760.0,
            manual_compact: false,
            window_actions: WindowActions::default(),
            pending_settings_json: String::new(),
        }
    }

    /// Crew role from the panel call lights.
    #[must_use]
    pub fn crew_role(&self) -> String {
        resolve_crew_role(self.panel_role == "cabin", self.panel_role == "ground")
    }

    /// Send a request with power gating, logging one SYSTEM line per outage.
    pub fn send(&mut self, request: &openatc_core::state::Request) {
        self.send_on_radio(request, self.active_radio);
    }

    fn send_on_radio(
        &mut self,
        request: &openatc_core::state::Request,
        radio: openatc_core::state::Radio,
    ) {
        let mut request = request.clone();
        request.radio = radio;
        let request = &request;
        if self.reset_pending {
            return;
        }
        if request.role == "ground"
            && (self.engine.state().ground_interphone_disconnected
                || !self.engine.state().telemetry.on_ground)
        {
            self.notice = openatc_core::dialogue::say("crew_ground_interphone_disconnected", &[]);
            return;
        }
        if request.role != "copilot"
            && !can_transmit(&request.role, self.radio_power, self.bus_power)
        {
            let radio = request.role != "cabin" && request.role != "ground";
            let reason = if radio { "radio" } else { "bus" };
            self.notice = if radio {
                "NO RADIO POWER — the RMP has no power.".to_owned()
            } else {
                "NO ELECTRICAL POWER — the interphone is dead.".to_owned()
            };
            self.last_action = format!("Transmit refused ({reason} power off)");
            if self.last_power_refusal != reason {
                reason.clone_into(&mut self.last_power_refusal);
                self.local_notices.push(LocalNotice {
                    speaker: "SYSTEM".to_owned(),
                    text: if radio {
                        "No radio power — turn the aircraft battery and radios on, then transmit again.".to_owned()
                    } else {
                        "No electrical power — the cabin interphone is dead. Restore power, then try again.".to_owned()
                    },
                });
                while self.local_notices.len() > 10 {
                    self.local_notices.remove(0);
                }
            }
            return;
        }
        if request.role != "copilot" {
            self.last_power_refusal.clear();
        }
        if !matches!(request.role.as_str(), "copilot" | "cabin" | "ground")
            && matches!(
                self.engine.state().phase,
                openatc_core::state::PhaseCode::Parked | openatc_core::state::PhaseCode::Finished
            )
            && (request.intent == "clearance"
                || interpret_text(&request.text).intent == "clearance")
        {
            if let Some(message) = openatc_core::ops::flight_plan_clearance_feedback(&self.draft) {
                self.notice = message;
                self.plan_notice.clone_from(&self.notice);
                let mut body = serde_json::to_value(request).unwrap_or(serde_json::Value::Null);
                body["intent"] = serde_json::json!("clearance");
                body["planningFlightPlan"] =
                    serde_json::to_value(&self.draft).unwrap_or(serde_json::Value::Null);
                body["planningCallsign"] =
                    serde_json::json!(self.transmission_callsign(&self.engine.state()));
                self.engine.post("/request", body);
                return;
            }
            if self.pending_clearance.is_some() {
                return;
            }
            let mut clearance = request.clone();
            "clearance".clone_into(&mut clearance.intent);
            self.pending_clearance = Some(clearance);
            self.engine.post(
                "/plan",
                serde_json::to_value(&self.draft).unwrap_or(serde_json::Value::Null),
            );
            "Submitting the Planning flight for clearance…".clone_into(&mut self.notice);
            return;
        }
        // Radio traffic needs a tuned controller. Crew interphone calls do not.
        if !matches!(request.role.as_str(), "copilot" | "cabin" | "ground")
            && !self.station_reachable(radio)
        {
            return;
        }
        let mut request = request.clone();
        if !matches!(request.role.as_str(), "copilot" | "cabin" | "ground")
            && !request.controller_initiated
            && !request.text.is_empty()
        {
            let state = self.engine.state();
            let nearby = openatc_core::stations::nearby(&self.nearby_stations, &state.telemetry);
            if let Some(station) = state
                .telemetry
                .frequency(request.radio)
                .and_then(|frequency| openatc_core::stations::tuned(&nearby, frequency))
            {
                request.text = format_typed_transmission(
                    &station.name,
                    self.transmission_callsign(&state),
                    &request.text,
                );
            }
        }
        let mut body = serde_json::to_value(&request).unwrap_or(serde_json::Value::Null);
        if !matches!(request.role.as_str(), "copilot" | "cabin" | "ground")
            && let Some(callsign) =
                openatc_core::ops::planning_callsign(&self.engine.state(), &self.draft.callsign)
        {
            body["planningCallsign"] = serde_json::json!(callsign);
        }
        self.engine.post("/request", body);
    }

    /// Before filing, radio checks use the identity being reviewed in Planning.
    pub fn transmission_callsign<'a>(&'a self, state: &'a State) -> &'a str {
        openatc_core::ops::planning_callsign(state, &self.draft.callsign)
            .unwrap_or(&state.plan.callsign)
    }

    /// Transmit the message box to ATC.
    pub fn transmit_box(&mut self) -> bool {
        if !self.engine.connected() {
            "Transmit blocked: engine down".clone_into(&mut self.last_action);
            return false;
        }
        if self.message.is_empty() {
            "Transmit blocked: empty box".clone_into(&mut self.last_action);
            return false;
        }
        let parsed = interpret_text(&self.message);
        let mut full = openatc_core::state::Request {
            intent: parsed.intent,
            text: self.message.clone(),
            altitude_feet: parsed.altitude_feet,
            waypoint: parsed.waypoint,
            ..Default::default()
        };
        full.role.clone_from(&self.crew_role());
        if full.role == "atc" {
            let state = self.engine.state();
            let nearby = openatc_core::stations::nearby(&self.nearby_stations, &state.telemetry);
            if let Some(station) = state
                .telemetry
                .frequency(self.active_radio)
                .and_then(|frequency| openatc_core::stations::tuned(&nearby, frequency))
            {
                full.text = format_typed_transmission(
                    &station.name,
                    if full.intent == "clearance" {
                        &self.draft.callsign
                    } else {
                        self.transmission_callsign(&state)
                    },
                    &full.text,
                );
            }
        }
        self.notice.clear();
        self.send(&full);
        self.message.clear();
        self.last_action = format!("Transmit sent ({})", full.role);
        true
    }

    /// Talk to the copilot instead of the radio.
    pub fn transmit_box_to_copilot(&mut self) -> bool {
        if !self.settings.copilot_button {
            "Talk blocked: copilot button off".clone_into(&mut self.last_action);
            return false;
        }
        if !self.engine.connected() {
            "Talk blocked: engine down".clone_into(&mut self.last_action);
            return false;
        }
        if self.message.is_empty() {
            "Talk blocked: empty box".clone_into(&mut self.last_action);
            return false;
        }
        let parsed = interpret_text(&self.message);
        let request = openatc_core::state::Request {
            intent: parsed.intent,
            text: self.message.clone(),
            altitude_feet: parsed.altitude_feet,
            waypoint: parsed.waypoint,
            role: "copilot".to_owned(),
            ..Default::default()
        };
        self.notice.clear();
        self.send(&request);
        self.message.clear();
        "Talk sent (copilot)".clone_into(&mut self.last_action);
        true
    }

    /// Push settings to the engine for validation and save.
    pub fn save_settings(&mut self) {
        if !self.settings_loaded || !self.engine.connected() {
            "Connect to the engine before saving settings.".clone_into(&mut self.settings_notice);
            return;
        }
        let body = serde_json::to_value(&self.settings).unwrap_or(serde_json::Value::Null);
        self.engine.post("/settings", body);
        self.last_settings_json = serde_json::to_string(&self.settings).unwrap_or_default();
        "Saving settings...".clone_into(&mut self.settings_notice);
    }

    /// Controller station that can accept a transmission on this radio, if any.
    fn tuned_controller(
        &self,
        radio: openatc_core::state::Radio,
    ) -> Option<openatc_core::stations::Station> {
        let state = self.engine.state();
        let nearby = openatc_core::stations::nearby(&self.nearby_stations, &state.telemetry);
        state
            .telemetry
            .frequency(radio)
            .and_then(|frequency| openatc_core::stations::tuned(&nearby, frequency))
            .filter(|station| !matches!(station.service.as_str(), "ATIS" | "Unicom"))
            .cloned()
    }

    /// Refuse radio speech unless a controller is tuned, before any STT work starts.
    fn station_reachable(&mut self, radio: openatc_core::state::Radio) -> bool {
        if self.tuned_controller(radio).is_some() {
            return true;
        }
        self.ptt_armed = false;
        let frequency = self.engine.state().telemetry.frequency(radio).unwrap_or(0);
        self.notice = if frequency > 0 {
            format!(
                "Not on station — {:.3} MHz is not a receivable controller frequency.",
                f64::from(frequency) / 1000.0
            )
        } else {
            "Not on station — select a controller frequency before transmitting.".to_owned()
        };
        "Transmit refused (not on station)".clone_into(&mut self.last_action);
        false
    }

    /// Start recording for a radio or interphone role.
    fn start_talking(&mut self, role: &str, radio: openatc_core::state::Radio) {
        self.expand();
        let background = self.chatter_playing.is_some();
        self.stop_all_chatter();
        if (!background && self.speech.busy())
            || self.speech.recording()
            || self.speech.monitoring()
        {
            "Mic busy — wait for the current job".clone_into(&mut self.last_action);
            return;
        }
        self.ptt_armed = true;
        self.ptt_radio = radio;
        role.clone_into(&mut self.ptt_role);
        self.record_start = now_seconds();
        self.record_peak = 0.0;
        self.speech.start_recording(false);
        self.last_action = if role == "copilot" {
            "PTT recording (copilot)".to_owned()
        } else {
            "PTT recording".to_owned()
        };
    }

    /// Push-to-talk for the message box mic: record on press, transcribe on release.
    pub fn mic_push_to_talk(&mut self, down: bool) {
        if self.reset_pending {
            return;
        }
        if down {
            if !self.station_reachable(self.active_radio) {
                return;
            }
            self.expand();
        }
        self.ptt_armed = false;
        if down {
            let background = self.chatter_playing.is_some();
            self.stop_all_chatter();
            if (!background && self.speech.busy())
                || self.speech.recording()
                || self.speech.monitoring()
            {
                "Mic busy — wait for the current job".clone_into(&mut self.last_action);
                return;
            }
            self.speech.start_recording(false);
        } else if self.speech.recording() {
            let endpoint = self.engine.endpoint();
            self.speech.transcribe(&endpoint);
        }
    }

    /// Push-to-talk for Talk/Transmit: record on press, transcribe on release.
    pub fn talk_push_to_talk(&mut self, down: bool, copilot: bool) {
        if self.reset_pending {
            return;
        }
        if down {
            let role = if copilot {
                "copilot".to_owned()
            } else {
                self.crew_role()
            };
            if role == "atc" && !self.station_reachable(self.active_radio) {
                return;
            }
            self.start_talking(&role, self.active_radio);
        } else if self.speech.recording() {
            let endpoint = self.engine.endpoint();
            self.speech.transcribe(&endpoint);
        } else {
            self.ptt_armed = false;
        }
    }

    /// Radio bindings bypass the crew interphone selection.
    pub fn radio_push_to_talk(&mut self, down: bool, radio: openatc_core::state::Radio) {
        if down {
            if !self.station_reachable(radio) {
                return;
            }
            self.start_talking("atc", radio);
        } else if self.ptt_armed && self.ptt_role == "atc" && self.ptt_radio == radio {
            self.talk_push_to_talk(false, false);
        }
    }

    /// Current refusal or simulator-data error for reports.
    #[must_use]
    pub fn last_notice(&self) -> String {
        if self.notice.is_empty() {
            self.engine.telemetry_error().unwrap_or_default()
        } else {
            self.notice.clone()
        }
    }

    /// Drain engine replies into page notices.
    pub fn poll_replies(&mut self) {
        if let Some(reply) = self.engine.take_reply("/simbrief") {
            self.importing = false;
            if reply.success {
                match serde_json::from_value::<FlightPlan>(reply.data) {
                    Ok(draft) => {
                        self.draft = draft;
                        self.plan_loaded = true;
                        "Downloaded latest OFP. Review Planning, then request IFR clearance; this flight will be submitted automatically."
                            .clone_into(&mut self.plan_notice);
                    }
                    Err(error) => self.plan_notice = error.to_string(),
                }
            } else {
                reply.error.clone_into(&mut self.plan_notice);
            }
        }
        if let Some(reply) = self.engine.take_reply("/plan") {
            if let Some(request) = self.pending_clearance.take() {
                if reply.success {
                    self.engine.post(
                        "/request",
                        serde_json::to_value(request).unwrap_or(serde_json::Value::Null),
                    );
                } else {
                    self.notice.clone_from(&reply.error);
                }
            }
            self.plan_notice = if reply.success {
                let notice = reply
                    .data
                    .get("plan")
                    .and_then(|plan| plan.get("procedureNotice"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if notice.is_empty() {
                    "Flight plan accepted by OpenATC AI.".to_owned()
                } else {
                    self.draft.procedure_notice.clone_from(&notice);
                    notice
                }
            } else {
                reply.error
            };
        }
        self.poll_replies_rest();
    }

    /// Remaining reply drains: settings, voice, session, request.
    fn poll_replies_rest(&mut self) {
        if let Some(reply) = self.engine.take_reply("/airport/arrival") {
            self.arrival_loading = false;
            if reply.success {
                match serde_json::from_value::<openatc_core::arrival::ArrivalAirport>(reply.data) {
                    Ok(data) => {
                        self.arrival = data;
                        self.arrival_notice.clear();
                    }
                    Err(error) => self.arrival_notice = error.to_string(),
                }
            } else {
                self.arrival_notice = reply.error;
            }
        }
        if let Some(reply) = self.engine.take_reply("/airport/local") {
            self.local_loading = false;
            if reply.success {
                match serde_json::from_value::<Airport>(reply.data) {
                    Ok(airport) => {
                        if self.airport.icao.is_empty() && !self.loading_airport {
                            self.set_airport(airport.clone());
                            self.airport_code.clone_from(&airport.icao);
                            self.airport_notice =
                                format!("Loaded {} from installed scenery.", airport.icao);
                        }
                        self.local_airport = airport;
                        self.local_notice.clear();
                    }
                    Err(error) => self.local_notice = error.to_string(),
                }
            } else {
                self.local_notice = reply.error;
            }
        }

        if let Some(reply) = self.engine.take_reply("/airport/load") {
            self.loading_airport = false;
            if reply.success {
                match serde_json::from_value::<Airport>(reply.data) {
                    Ok(airport) => {
                        self.set_airport(airport);
                        self.zoom = 1.0;
                        self.pan = [0.0, 0.0];
                        self.airport_notice =
                            format!("Loaded {} from installed scenery.", self.airport.icao);
                    }
                    Err(error) => self.airport_notice = error.to_string(),
                }
            } else {
                reply.error.clone_into(&mut self.airport_notice);
            }
        }
        if let Some(reply) = self.engine.take_reply("/settings") {
            self.settings_notice = if reply.success {
                "Settings saved".to_owned()
            } else {
                reply.error
            };
        }
        if let Some(reply) = self.engine.take_reply("/voice-health") {
            if reply.success {
                let stt = reply
                    .data
                    .get("stt")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let tts = reply
                    .data
                    .get("tts")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                self.voice_health = if stt && tts {
                    "Voice: ready".to_owned()
                } else if !stt && !tts {
                    "Voice: STT + TTS down".to_owned()
                } else if !stt {
                    "Voice: STT down".to_owned()
                } else {
                    "Voice: TTS down".to_owned()
                };
                if let Some(version) = reply
                    .data
                    .get("aiVersion")
                    .and_then(serde_json::Value::as_str)
                {
                    version.clone_into(&mut self.ai_version);
                }
                if let Some(version) = reply
                    .data
                    .get("engineVersion")
                    .and_then(serde_json::Value::as_str)
                {
                    version.clone_into(&mut self.engine_version);
                }
            } else {
                "Voice: unreachable".clone_into(&mut self.voice_health);
            }
        }
        if let Some(reply) = self.engine.take_reply("/session/reset") {
            if reply.success {
                self.engine.accept_reset(&reply.data);
                let mut fresh = Self::new(&self.engine.endpoint());
                // Keep the existing transport, settings and window placement.
                std::mem::swap(&mut fresh.engine, &mut self.engine);
                fresh.settings = self.settings.clone();
                fresh.settings_loaded = self.settings_loaded;
                fresh
                    .last_settings_json
                    .clone_from(&self.last_settings_json);
                fresh.simulator_host = self.simulator_host;
                fresh.preserve_window_layout(self);
                fresh.current_airport.clone_from(&self.current_airport);
                fresh.flight_reset_generation = self.flight_reset_generation;
                fresh.clipboard = self.clipboard.take();
                fresh.plan_notice = "New flight ready.".into();
                *self = fresh;
            } else {
                self.reset_pending = false;
                self.confirm_reset = true;
                self.reset_error = reply.error;
                self.notice.clone_from(&self.reset_error);
            }
        }
        for endpoint in ["/request/copilot-prepare", "/request/copilot-checkin"] {
            if let Some(reply) = self.engine.take_reply(endpoint) {
                self.copilot_reply_prepared = reply.success;
                self.copilot_prepared_sequence = reply.data["state"]["transcript"]
                    .as_array()
                    .and_then(|entries| entries.iter().rev().find(|e| e["speaker"] == "COPILOT"))
                    .and_then(|e| e["sequence"].as_u64())
                    .and_then(|s| u32::try_from(s).ok())
                    .unwrap_or(0);
                let superseded = self
                    .engine
                    .state()
                    .transcript
                    .iter()
                    .any(|e| e.urgent && e.sequence > self.copilot_prepared_sequence);
                if superseded {
                    self.copilot_reply_prepared = false;
                    self.auto_reply_pending = false;
                    self.last_copilot_reply_sequence = 0;
                }
                if !reply.success {
                    self.auto_reply_pending = false;
                    self.notice = reply.error;
                }
            }
        }
        if let Some(reply) = self.engine.take_reply("/request/copilot-reply") {
            self.auto_reply_pending = false;
            if !reply.success {
                self.notice = reply.error;
            }
        }
        if let Some(reply) = self.engine.take_reply("/request/auto-reply") {
            self.auto_reply_pending = false;
            if reply.success {
                self.notice.clear();
            } else {
                self.notice = reply.error;
            }
        }
        if let Some(reply) = self.engine.take_reply("/request") {
            if reply.success {
                let silent = reply
                    .data
                    .get("result")
                    .and_then(|result| result.get("silent"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if silent {
                    self.notice = reply.data["result"]["notice"]
                        .as_str()
                        .filter(|notice| !notice.is_empty())
                        .unwrap_or("No answer — nobody on this frequency.")
                        .to_owned();
                } else {
                    self.notice.clear();
                }
            } else {
                self.notice = reply.error;
            }
        }
    }

    /// Queue new transcript entries for speech following the role toggles.
    pub fn update_speech(&mut self, state: &State) {
        use openatc_core::identify::interpret_text;
        use openatc_core::intents::speech_worth_sending;
        if self.last_com1_khz != 0 && self.last_com1_khz != state.telemetry.com1_khz {
            self.stop_chatter(openatc_core::state::Radio::Com1);
            self.speech.stop_speech();
            let tuned = state.telemetry.frequency(openatc_core::state::Radio::Com1);
            self.speech_queue.retain(|q| {
                q.radio != Some(openatc_core::state::Radio::Com1)
                    || q.frequency.is_none_or(|frequency| Some(frequency) == tuned)
            });
        }
        if self.last_com2_khz != state.telemetry.com2_khz {
            self.stop_chatter(openatc_core::state::Radio::Com2);
            self.speech_com2.stop_speech();
            let tuned = state.telemetry.frequency(openatc_core::state::Radio::Com2);
            self.speech_queue.retain(|q| {
                q.radio != Some(openatc_core::state::Radio::Com2)
                    || q.frequency.is_none_or(|frequency| Some(frequency) == tuned)
            });
        }
        self.last_com1_khz = state.telemetry.com1_khz;
        self.last_com2_khz = state.telemetry.com2_khz;
        if !state.version.is_empty() {
            self.engine_version.clone_from(&state.version);
        }
        self.speech.configure(
            &self.settings.input_device.clone(),
            &self.settings.output_device.clone(),
            self.settings.input_gain,
            self.settings.master_volume,
        );
        self.speech_com2.configure(
            "",
            &self.settings.output_device,
            self.settings.input_gain,
            self.settings.master_volume,
        );
        self.speech_com2.poll();
        self.speech.poll();
        self.reconcile_chatter(state);
        if self.ptt_armed && now_seconds() - self.record_start > 45.0 {
            self.ptt_armed = false;
        }
        if self.speech.recording() && !self.speech.monitoring() && self.ptt_armed {
            self.record_peak = self.record_peak.max(self.speech.input_level());
        }
        let transcription = self.speech.take_transcript();
        if !transcription.is_empty() && self.ptt_armed {
            self.ptt_armed = false;
            let role = self.ptt_role.clone();
            if speech_worth_sending(
                now_seconds() - self.record_start,
                f64::from(self.record_peak),
                &transcription,
            ) {
                let parsed = interpret_text(&transcription);
                let request = openatc_core::state::Request {
                    intent: parsed.intent,
                    text: transcription,
                    altitude_feet: parsed.altitude_feet,
                    waypoint: parsed.waypoint,
                    role: role.clone(),
                    ..Default::default()
                };
                self.notice.clear();
                self.send_on_radio(&request, self.ptt_radio);
                self.last_action = format!("PTT sent ({role})");
            } else {
                "Nothing heard — nothing sent.".clone_into(&mut self.notice);
                "PTT dropped: nothing heard".clone_into(&mut self.last_action);
            }
        } else if !transcription.is_empty() {
            transcription.clone_into(&mut self.message);
        }
        if state.transcript.is_empty() {
            self.last_speech_sequence = 0;
            self.last_handoff_tuned = 0;
        }
        for entry in &state.transcript {
            if entry.sequence > self.last_speech_sequence {
                if matches!(entry.speaker.as_str(), "ATIS" | "SYSTEM")
                    || entry.delivery == "preloaded-callout"
                {
                    self.last_speech_sequence = entry.sequence;
                    continue;
                }
                if entry.urgent && entry.speaker == "ATC" {
                    if entry.radio == Some(openatc_core::state::Radio::Com2) {
                        self.speech_com2.stop_speech();
                    } else {
                        self.speech.stop_speech();
                    }
                    self.speech_queue.retain(|queued| {
                        queued.radio.unwrap_or_default() != entry.radio.unwrap_or_default()
                    });
                    if self.copilot_prepared_sequence <= entry.sequence {
                        self.copilot_reply_prepared = false;
                        self.auto_reply_pending = false;
                        if entry.information_only {
                            self.last_copilot_reply_sequence = 0;
                        }
                    }
                }
                let controller = entry.speaker == "ATC";
                let traffic = entry.position == "Traffic";
                let pilot = !controller
                    && !traffic
                    && entry.speaker != "COPILOT"
                    && entry.speaker != "CABIN"
                    && entry.speaker != "ATTENDANT"
                    && entry.speaker != "GROUND";
                if !entry_frequency_tuned(entry, state) {
                    self.last_speech_sequence = entry.sequence;
                    continue;
                }
                if entry.chatter
                    && entry
                        .frequency
                        .is_some_and(|frequency| !state.chatter_frequencies.contains(&frequency))
                {
                    self.last_speech_sequence = entry.sequence;
                    continue;
                }
                if (traffic && (self.settings.hear_traffic_radio || self.settings.talk_to_traffic))
                    || (controller && self.settings.controller_speech)
                    || (entry.speaker == "COPILOT" && self.settings.copilot_speech)
                    || (pilot && self.settings.pilot_speech)
                    || matches!(entry.speaker.as_str(), "CABIN" | "ATTENDANT")
                    || entry.speaker == "GROUND"
                {
                    if !entry.background || !entry.chatter {
                        if let Some(radio) = entry.radio {
                            self.stop_airport_chatter(radio);
                        }
                    }
                    self.speech_queue.push_back(entry.clone());
                }
                self.last_speech_sequence = entry.sequence;
            }
        }
        while self.speech_queue.len() > 20 {
            self.speech_queue.pop_front();
        }
        let now = now_seconds();
        for radio in [
            openatc_core::state::Radio::Com1,
            openatc_core::state::Radio::Com2,
        ] {
            let busy = if radio == openatc_core::state::Radio::Com2 {
                self.speech_com2.busy()
            } else {
                self.speech.busy()
            };
            if busy || self.speech.recording() {
                continue;
            }
            let slot = usize::from(radio == openatc_core::state::Radio::Com2);
            let position = self
                .speech_queue
                .iter()
                .position(|entry| {
                    entry.radio.unwrap_or_default() == radio
                        && (!entry.background || !entry.chatter)
                })
                .or_else(|| {
                    self.speech_queue.iter().position(|entry| {
                        entry.radio.unwrap_or_default() == radio
                            && entry.chatter
                            && now >= self.chatter_gate_until[slot]
                    })
                });
            if let Some(index) = position
                && let Some(entry) = self.speech_queue.remove(index)
            {
                // Only airport chatter carries a live frequency. Ambient traffic
                // background calls have no frequency and must not be killed by the
                // station-death filter.
                if entry.chatter
                    && let Some(frequency) = entry.frequency
                {
                    self.chatter_playing = Some((radio, frequency, entry.gap_ms));
                }
                self.speak_entry(&entry);
            }
        }
    }

    fn reconcile_chatter(&mut self, state: &State) {
        let now = now_seconds();
        if let Some((radio, frequency, gap)) = self.chatter_playing {
            if !state.chatter_frequencies.contains(&frequency) {
                self.stop_chatter(radio);
            } else {
                let busy = if radio == openatc_core::state::Radio::Com2 {
                    self.speech_com2.busy()
                } else {
                    self.speech.busy()
                };
                if !busy {
                    let slot = usize::from(radio == openatc_core::state::Radio::Com2);
                    self.chatter_gate_until[slot] = now + f64::from(gap) / 1000.0;
                    self.chatter_playing = None;
                }
            }
        }
        self.speech_queue.retain(|entry| {
            !entry.chatter
                || entry
                    .frequency
                    .is_none_or(|frequency| state.chatter_frequencies.contains(&frequency))
        });
    }

    /// Stop synthetic airport chatter while leaving live background traffic queued.
    fn stop_airport_chatter(&mut self, radio: openatc_core::state::Radio) {
        self.speech_queue
            .retain(|entry| entry.radio != Some(radio) || !entry.chatter);
        if self
            .chatter_playing
            .is_some_and(|(playing, _, _)| playing == radio)
        {
            if radio == openatc_core::state::Radio::Com2 {
                self.speech_com2.stop_speech();
            } else {
                self.speech.stop_speech();
            }
            self.chatter_playing = None;
            self.chatter_gate_until[usize::from(radio == openatc_core::state::Radio::Com2)] =
                now_seconds();
        }
    }

    fn stop_chatter(&mut self, radio: openatc_core::state::Radio) {
        self.speech_queue
            .retain(|entry| entry.radio != Some(radio) || !entry.background);
        if self
            .chatter_playing
            .is_some_and(|(playing, _, _)| playing == radio)
        {
            if radio == openatc_core::state::Radio::Com2 {
                self.speech_com2.stop_speech();
            } else {
                self.speech.stop_speech();
            }
            self.chatter_playing = None;
            self.chatter_gate_until[usize::from(radio == openatc_core::state::Radio::Com2)] =
                now_seconds();
        }
    }

    fn stop_all_chatter(&mut self) {
        self.stop_chatter(openatc_core::state::Radio::Com1);
        self.stop_chatter(openatc_core::state::Radio::Com2);
    }

    /// Busy state for the controlling channel, independent of the other receiver.
    pub fn radio_busy(&self, radio: openatc_core::state::Radio) -> bool {
        self.radio_audio_busy(radio) || self.auto_reply_pending && self.auto_reply_radio == radio
    }

    fn radio_audio_busy(&self, radio: openatc_core::state::Radio) -> bool {
        let background = self
            .chatter_playing
            .is_some_and(|(playing, _, _)| playing == radio);
        let playback = if radio == openatc_core::state::Radio::Com2 {
            self.speech_com2.busy()
        } else {
            self.speech.busy()
        };
        playback && !background
            || self.speech.recording() && self.ptt_radio == radio
            || self
                .speech_queue
                .iter()
                .any(|entry| entry.radio.unwrap_or_default() == radio && !entry.background)
    }

    /// Speak one entry with the role voice and levels.
    pub fn speak_entry(&mut self, entry: &openatc_core::state::Transmission) {
        let role = if entry.speaker == "GROUND" {
            4
        } else if matches!(entry.speaker.as_str(), "CABIN" | "ATTENDANT") {
            3
        } else if entry.speaker == "COPILOT" {
            1
        } else if entry.speaker == "ATC" {
            0
        } else {
            2
        };
        let volume = if role == 4 {
            self.settings.ground_volume
        } else if role == 3 {
            self.settings.attendant_volume
        } else if role == 1 {
            self.settings.copilot_volume
        } else if role == 2 {
            self.settings.pilot_volume
        } else {
            self.settings.controller_volume
        };
        let endpoint = self.engine.endpoint();
        let output = if entry.radio == Some(openatc_core::state::Radio::Com2) {
            &mut self.speech_com2
        } else {
            &mut self.speech
        };
        let spoken = openatc_core::radiotelephony::spoken_flight_number(
            &entry.text,
            &entry.speaker,
            self.settings.callsign_numbers == openatc_settings::CallsignNumbers::Group,
        );
        output.speak(&openatc_audio::SpeakRequest {
            text: &spoken,
            engine_endpoint: &endpoint,
            role,
            received_radio: entry.background,
            volume,
            voice: &entry.voice,
            speed: entry.speed,
            urgent: entry.urgent,
            delivery: &entry.delivery,
        });
    }

    /// Process replies and refresh voice, settings and flight data.
    pub fn tick(&mut self) {
        self.poll_replies();
        if self.reset_pending || self.debug_jump_busy {
            return;
        }
        let now = now_seconds();
        if self.engine.connected()
            && (self.last_voice_check == 0.0 || now - self.last_voice_check > 15.0)
        {
            self.last_voice_check = now;
            self.engine.post("/voice-health", serde_json::Value::Null);
        }
        let state = self.engine.state();
        if self.engine.connected() && !self.settings_loaded {
            self.settings = self.engine.settings();
            if self.settings.ai_url == self.settings.stt_url
                && self.settings.ai_url == self.settings.tts_url
            {
                self.setup_server_url.clone_from(&self.settings.ai_url);
            }
            self.last_settings_json = serde_json::to_string(&self.settings).unwrap_or_default();
            self.transcript_fraction = self.settings.transcript_fraction.clamp(0.2, 0.8);
            self.pending_window_bounds = self.settings.window_bounds;
            self.settings_loaded = true;
            self.last_speech_sequence = state.transcript.last().map_or(0, |entry| entry.sequence);
        }
        if self.settings_loaded && self.settings.simulator_root.is_empty() {
            self.settings.simulator_root = self.engine.settings().simulator_root;
        }
        if self.settings_loaded
            && self.engine.connected()
            && self.settings.auto_generated_traffic
            && !self.settings.generated_traffic
            && state.ifr
            && !state.plan.departure.is_empty()
            && !state.plan.destination.is_empty()
        {
            let key = format!("{}/{}", state.plan.departure, state.plan.destination);
            if self.auto_generated_plan != key {
                self.auto_generated_plan = key;
                self.settings.generated_traffic = true;
                self.save_settings();
            }
        }
        if self.engine.connected() && !self.plan_loaded {
            self.draft = state.plan.clone();
            self.plan_loaded = true;
        }
        self.update_radio(&state, now);
        self.update_local_airport(&state);
        self.update_arrival(&state, now);
        self.update_speech(&state);
        if self.settings.copilot_replies
            && !self.auto_reply_pending
            && !self.radio_audio_busy(state.control_radio)
            && let Some(last) = state
                .transcript
                .iter()
                .rev()
                .find(|e| e.is_controller_instruction())
            && (last.sequence > self.last_copilot_reply_sequence
                || state.taxi_clearance.stage()
                    == openatc_core::state::SurfaceStage::CrossingComplete)
            && openatc_core::readback::auto_reply(&state).is_some()
        {
            self.last_copilot_reply_sequence = last.sequence;
            self.auto_reply_pending = true;
            self.auto_reply_radio = state.control_radio;
            self.engine
                .post("/request/copilot-prepare", serde_json::json!({}));
        }
        if self.copilot_reply_prepared
            && self.last_speech_sequence >= self.copilot_prepared_sequence
            && !self.radio_audio_busy(state.control_radio)
        {
            self.copilot_reply_prepared = false;
            self.engine.post(
                "/request/copilot-reply",
                serde_json::json!({"sequence": self.copilot_prepared_sequence}),
            );
        }
        let handoff_radio = state.handoff_radio();
        if self.settings.copilot_replies
            && (self.settings.copilot_tunes || self.settings.auto_tune_handoff)
            && !self.auto_reply_pending
            && !self.radio_audio_busy(handoff_radio)
            && state.frequency_sequence > self.last_checkin_sequence
            && state.telemetry.frequency(handoff_radio) == Some(state.recommended_frequency_khz)
            && state
                .transcript
                .iter()
                .rev()
                .find(|e| e.is_controller_instruction())
                .is_some_and(|e| {
                    e.sequence == state.frequency_sequence
                        && (e.pilot_reply.is_empty() || handoff_readback_spoken(&state, e.sequence))
                })
            && let Some(_station) = self
                .nearby_stations
                .iter()
                .find(|s| Some(s.khz) == state.telemetry.frequency(handoff_radio) && s.receivable)
        {
            self.last_checkin_sequence = state.frequency_sequence;
            self.auto_reply_pending = true;
            self.auto_reply_radio = handoff_radio;
            self.engine.post(
                "/request/copilot-checkin",
                serde_json::json!({"radio":handoff_radio}),
            );
        }
        if self.settings_loaded && self.engine.connected() {
            let settings_json = serde_json::to_string(&self.settings).unwrap_or_default();
            if settings_json != self.pending_settings_json {
                self.pending_settings_json.clone_from(&settings_json);
                self.settings_changed_at = now;
            }
            if settings_json != self.last_settings_json && now - self.settings_changed_at >= 0.75 {
                self.save_settings();
            }
        }
    }

    /// Refresh provider voices after saving the selected TTS endpoint.
    pub fn refresh_voices(&mut self) {
        if !self.voices_loading {
            self.save_settings();
            self.voices_loading = true;
            self.voices_origin.clone_from(&self.settings.tts_url);
            self.available_voices.clear();
            self.voices_notice = "Loading voices from the TTS service…".into();
            self.engine.post("/speech/voices", serde_json::json!({}));
        }
    }

    fn update_radio(&mut self, state: &State, now: f64) {
        if self.settings_loaded
            && self.engine.connected()
            && self.voices_origin != self.settings.tts_url
        {
            self.refresh_voices();
        }
        if let Some(reply) = self.engine.take_reply("/speech/voices") {
            self.voices_loading = false;
            if reply.success {
                self.available_voices =
                    serde_json::from_value(reply.data["voices"].clone()).unwrap_or_default();
                self.voices_notice = format!(
                    "{} voices returned by the TTS service",
                    self.available_voices.len()
                );
            } else {
                self.available_voices.clear();
                self.voices_notice = reply.error;
            }
        }
        if state.frequency_sequence != 0
            && state.frequency_sequence != self.last_handoff_tuned
            && state.recommended_frequency_khz != 0
            && (self.settings.copilot_tunes || self.settings.auto_tune_handoff)
            && self.requested_frequency_khz.is_none()
        {
            let radio = state
                .transcript
                .iter()
                .find(|entry| entry.sequence == state.frequency_sequence)
                .and_then(|entry| entry.radio)
                .unwrap_or(self.active_radio);
            if state.telemetry.frequency(radio) == Some(state.recommended_frequency_khz) {
                self.last_handoff_tuned = state.frequency_sequence;
            } else if state.frequency_sequence <= self.last_speech_sequence
                && !self.radio_audio_busy(radio)
                && !self
                    .speech_queue
                    .iter()
                    .any(|entry| entry.sequence == state.frequency_sequence)
            {
                // Tune only after the handoff itself has been heard.
                self.requested_frequency_khz = Some(state.recommended_frequency_khz);
                self.requested_frequency_radio = radio;
                self.active_radio = radio;
                self.last_handoff_tuned = state.frequency_sequence;
            }
        } else if state.frequency_sequence != 0
            && state.frequency_sequence != self.last_handoff_tuned
            && self.requested_frequency_khz.is_some()
        {
            // A manual tune wins over the pending handoff.
            self.last_handoff_tuned = state.frequency_sequence;
        }
        if let Some(reply) = self.engine.take_reply("/debug/map") {
            self.map_loading = false;
            if reply.success {
                self.map_airports =
                    serde_json::from_value(reply.data["airports"].clone()).unwrap_or_default();
                self.map_stations =
                    serde_json::from_value(reply.data["stations"].clone()).unwrap_or_default();
                self.map_notice = reply.data["notice"].as_str().unwrap_or("").into();
            } else {
                self.map_notice = reply.error;
            }
        }
        if self.settings.dev_mode
            && self.page == 7
            && !self.map_loading
            && self.engine.connected()
            && (self.map_checked == 0.0 || now - self.map_checked > 1.0)
        {
            self.map_loading = true;
            self.map_checked = now;
            let center = self
                .radio_map_center
                .unwrap_or([state.telemetry.latitude, state.telemetry.longitude]);
            self.engine.post("/debug/map", serde_json::json!({"latitude":center[0],"longitude":center[1],"rangeNm":self.radio_map_range}));
        }
        if let Some(reply) = self.engine.take_reply("/stations/nearby") {
            self.stations_loading = false;
            if reply.success {
                self.occupied_airport = reply.data["occupiedAirport"].as_str().unwrap_or("").into();
                self.nearby_stations =
                    serde_json::from_value(reply.data["stations"].clone()).unwrap_or_default();
                self.radio_notice = reply.data["notice"].as_str().unwrap_or("").to_owned();
            } else {
                self.radio_notice = reply.error;
            }
        }
        if self.engine.connected()
            && !self.stations_loading
            && (self.stations_checked == 0.0 || now - self.stations_checked > 5.0)
        {
            self.stations_loading = true;
            self.stations_checked = now;
            self.engine.post("/stations/nearby", serde_json::json!({}));
        }
        let receivable = openatc_core::stations::nearby(&self.nearby_stations, &state.telemetry);
        let primary = openatc_core::stations::tuned(&receivable, state.telemetry.com1_khz);
        let secondary = state
            .telemetry
            .frequency(openatc_core::state::Radio::Com2)
            .and_then(|frequency| openatc_core::stations::tuned(&receivable, frequency));
        let (radio, station) = if self.settings.show_com2
            && secondary.is_some_and(|station| station.service == "ATIS")
            && (self.active_radio == openatc_core::state::Radio::Com2
                || primary.is_none_or(|station| station.service != "ATIS"))
        {
            (openatc_core::state::Radio::Com2, secondary)
        } else {
            (openatc_core::state::Radio::Com1, primary)
        };
        let frequency = state.telemetry.frequency(radio).unwrap_or(0);
        let instruction_waiting = state.transcript.iter().any(|entry| {
            entry.radio.unwrap_or_default() == radio
                && entry.sequence > self.last_speech_sequence
                && entry.is_controller_instruction()
        });
        let active = self.engine.connected()
            && self.radio_power
            && self.requested_frequency_khz.is_none()
            && !instruction_waiting
            && station.is_some_and(|s| s.service == "ATIS");
        self.receives_atis = self.engine.connected()
            && self.radio_power
            && station.is_some_and(|s| s.service == "ATIS");
        if self.atis_frequency != 0
            && (!active
                || self.atis_frequency != frequency
                || self.atis_radio != radio
                || station.is_none_or(|s| s.airport != self.atis_airport))
        {
            if self.atis_radio == openatc_core::state::Radio::Com2 {
                self.speech_com2.stop_speech();
            } else {
                self.speech.stop_speech();
            }
            self.atis_frequency = 0;
        }
        if !(if radio == openatc_core::state::Radio::Com2 {
            self.speech_com2.busy()
        } else {
            self.speech.busy()
        }) {
            self.atis_frequency = 0;
        }
        if let Some(reply) = self.engine.take_reply("/atis") {
            self.atis_pending = false;
            if active
                && reply.success
                && reply.data["frequency"].as_i64() == Some(i64::from(frequency))
                && serde_json::from_value::<openatc_core::state::Radio>(reply.data["radio"].clone())
                    .unwrap_or_default()
                    == radio
                && station
                    .is_some_and(|s| reply.data["airport"].as_str() == Some(s.airport.as_str()))
            {
                if let Some(text) = reply.data["text"].as_str()
                    && !(if radio == openatc_core::state::Radio::Com2 {
                        self.speech_com2.busy()
                    } else {
                        self.speech.busy()
                    })
                    && !self.speech.recording()
                    && !self
                        .speech_queue
                        .iter()
                        .any(|entry| entry.radio.unwrap_or_default() == radio)
                {
                    self.atis_frequency = frequency;
                    self.atis_radio = radio;
                    self.atis_airport = reply.data["airport"].as_str().unwrap_or("").to_owned();
                    self.speak_entry(&Transmission {
                        radio: Some(radio),
                        frequency: None,
                        gap_ms: 0,
                        information_only: false,
                        pilot_reply: String::new(),
                        background: false,
                        chatter: false,
                        speaker: "ATC".to_owned(),
                        text: text.to_owned(),
                        delivery: if self.settings.atis_electronic {
                            "atis"
                        } else {
                            "atis-natural"
                        }
                        .to_owned(),
                        speed: 1.0,
                        voice: if self.settings.atis_voice.is_empty() {
                            self.settings.voice.clone()
                        } else {
                            self.settings.atis_voice.clone()
                        },
                        sequence: 0,
                        position: "ATIS".to_owned(),
                        urgent: false,
                    });
                    self.radio_notice =
                        format!("ATIS {}", reply.data["information"].as_str().unwrap_or(""));
                }
            } else if active {
                self.radio_notice = reply.error;
            }
        }
        if active
            && !self.atis_pending
            && !(if radio == openatc_core::state::Radio::Com2 {
                self.speech_com2.busy()
            } else {
                self.speech.busy()
            })
            && !self.speech.recording()
            && !self
                .speech_queue
                .iter()
                .any(|entry| entry.radio.unwrap_or_default() == radio)
            && now - self.atis_checked > 8.0
        {
            self.atis_checked = now;
            self.atis_pending = true;
            self.engine
                .post("/atis", serde_json::json!({"radio":radio}));
        }
    }

    fn update_arrival(&mut self, state: &State, now: f64) {
        let destination = &state.plan.destination;
        if self.arrival.airport.icao != *destination && !self.arrival.airport.icao.is_empty() {
            self.arrival = ArrivalAirport::default();
            self.arrival_terrain = TerrainProfile::default();
        }
        if self.page == 3
            && self.engine.connected()
            && !self.arrival_loading
            && destination.len() == 4
            && (self.arrival_requested != *destination
                || (!self.arrival_notice.is_empty() && now - self.arrival_retry_at > 15.0))
        {
            self.arrival_requested.clone_from(destination);
            self.arrival_retry_at = now;
            self.arrival_loading = true;
            self.engine
                .post("/airport/arrival", serde_json::json!({"icao": destination}));
        }
    }

    fn update_local_airport(&mut self, state: &State) {
        if !matches!(self.page, 1 | 2 | 6) {
            return;
        }
        let planned = if state.has_departed {
            &state.plan.destination
        } else {
            &state.plan.departure
        };
        let airport = if state.telemetry.on_ground && !planned.is_empty() {
            planned
        } else if !self.current_airport.is_empty() {
            &self.current_airport
        } else if state.has_departed {
            &state.plan.destination
        } else {
            &state.plan.departure
        };
        if self.engine.connected()
            && !self.settings.simulator_root.is_empty()
            && !airport.is_empty()
            && !self.local_loading
            && self.local_requested != *airport
        {
            self.local_requested.clone_from(airport);
            self.local_loading = true;
            self.engine
                .post("/airport/local", serde_json::json!({"icao":airport}));
        }
    }

    /// Replace the inspected airport and its static map geometry together.
    pub fn set_airport(&mut self, airport: Airport) {
        self.map_geometry = crate::maps::Geometry::new(&airport);
        self.airport = airport;
    }

    /// Switch page, clamped.
    pub fn set_page(&mut self, page: usize) {
        let page = page.min(PAGES.len() - 1);
        if self.page != page {
            self.show_voice_popup = false;
            self.confirm_reset = false;
            self.modal_intent.clear();
        }
        self.page = page;
        if page == 0 {
            self.active_radio = openatc_core::state::Radio::Com1;
        } else if page == 10 {
            self.active_radio = openatc_core::state::Radio::Com2;
        }
        self.expand();
    }

    /// Cockpit panel role from call lights (ground/cabin/ATC).
    pub fn set_panel_role(&mut self, role: &str, source: &str) {
        role.clone_into(&mut self.panel_role);
        source.clone_into(&mut self.panel_source);
    }

    /// Electrical power state for transmit gating.
    pub fn set_electrical_power(&mut self, radio: bool, bus: bool) {
        self.radio_power = radio;
        self.bus_power = bus;
    }

    /// Engine fault line shown beside the talk box.
    pub fn set_engine_fault(&mut self, fault: &str) {
        fault.clone_into(&mut self.engine_fault);
    }

    fn preserve_window_layout(&mut self, previous: &Self) {
        self.window_offset = previous.window_offset;
        self.window_size = previous.window_size;
        self.can_pop_out = previous.can_pop_out;
        self.popped_out = previous.popped_out;
        self.compact = previous.compact;
        self.manual_compact = previous.manual_compact;
        self.expanded_height = previous.expanded_height;
        self.transcript_fraction = previous.transcript_fraction;
        self.pending_window_bounds = previous.pending_window_bounds;
        self.pending_mode_height = previous.pending_mode_height;
        self.window_actions.height = previous.window_actions.height;
        self.window_actions.restore_bounds = previous.window_actions.restore_bounds;
    }

    /// Apply host window placement for this frame.
    pub fn set_window_frame(&mut self, frame: WindowFrame) {
        if let Some(height) = self.pending_mode_height
            && ((frame.size[1] - height).abs() <= 2.0
                || (frame.size[1] - self.window_size[1]).abs() > 2.0)
        {
            // The host can clamp a resize. Observe its size change before saving
            // that frame as the expanded layout again.
            self.pending_mode_height = None;
        }
        if self.settings_loaded {
            self.settings.transcript_fraction = self.transcript_fraction;
            if self.pending_window_bounds.is_none()
                && self.pending_mode_height.is_none()
                && !frame.popped_out
                && let Some([left, top, right, _]) = frame.native_bounds
            {
                if self.compact {
                    let height = self
                        .settings
                        .window_bounds
                        .map_or(self.expanded_height.round() as i32, |bounds| {
                            bounds[1] - bounds[3]
                        });
                    self.settings.window_bounds = Some([left, top, right, top - height]);
                } else {
                    self.settings.window_bounds = frame.native_bounds;
                    self.expanded_height = frame.size[1];
                }
            }
        }
        self.window_offset = frame.position;
        self.window_size = frame.size;
        self.can_pop_out = frame.can_pop_out;
        self.popped_out = frame.popped_out;
    }

    /// Expand to the full panel layout.
    pub fn expand(&mut self) {
        self.last_interaction = now_seconds();
        self.manual_compact = false;
        if self.compact {
            self.compact = false;
            let height = self.expanded_height.max(460.0 * self.settings.ui_scale);
            self.pending_mode_height = Some(height);
            self.window_actions.height = Some(height);
        }
    }

    fn collapse(&mut self, manual: bool) {
        if !matches!(self.page, 0 | 10) || self.compact || !self.modal_intent.is_empty() {
            return;
        }
        if self.pending_mode_height.is_none() {
            self.expanded_height = self.window_size[1];
        }
        self.compact = true;
        self.manual_compact = manual;
        let scale = self.settings.ui_scale;
        let compact_height = (self.expanded_height * self.transcript_fraction + 36.0 * scale)
            .max(200.0 * scale)
            .min(self.expanded_height);
        self.pending_mode_height = Some(compact_height);
        self.window_actions.height = Some(compact_height);
    }

    fn update_visibility(&mut self, ui: &imgui::Ui) {
        let now = now_seconds();
        let mouse = ui.io().mouse_pos;
        let hovered = mouse[0] >= self.window_offset[0]
            && mouse[1] >= self.window_offset[1]
            && mouse[0] < self.window_offset[0] + self.window_size[0]
            && mouse[1] < self.window_offset[1] + self.window_size[1];
        // An explicit collapse stays put until the pilot opens the controls again.
        if self.manual_compact
            && matches!(self.page, 0 | 10)
            && !self.speech.recording()
            && !self.ptt_armed
            && self.modal_intent.is_empty()
        {
            return;
        }
        let editing = !self.message.is_empty()
            || self.setup_overlay
            || !self.modal_intent.is_empty()
            || self.show_voice_popup
            || ui.is_any_item_active();
        if !matches!(self.page, 0 | 10)
            || self.speech.recording()
            || self.ptt_armed
            || editing
            || hovered
        {
            self.expand();
        } else if self.last_interaction == 0.0 {
            self.last_interaction = now;
        } else if !self.settings.pin_open
            && now - self.last_interaction >= f64::from(self.settings.fade_delay)
        {
            self.collapse(false);
        }
    }

    /// Start a clean flight without unloading the simulator plugin.
    pub fn new_flight(&mut self) {
        if self.reset_pending {
            return;
        }
        self.speech.stop_monitoring();
        self.speech.stop_speech();
        self.speech_com2.stop_speech();
        self.speech_queue.clear();
        self.ptt_armed = false;
        self.pending_clearance = None;
        self.requested_debug_jump = None;
        self.debug_jump_busy = false;
        self.auto_generated_plan.clear();
        self.requested_frequency_khz = None;
        self.flight_reset_generation = self.flight_reset_generation.wrapping_add(1);
        self.reset_pending = true;
        self.reset_error.clear();
        self.confirm_reset = false;
        self.engine.post("/session/reset", serde_json::Value::Null);
    }

    /// Draw the panel, returning one-frame window requests for the host.
    ///
    /// # Panics
    ///
    /// Panics if the engine state cannot be read.
    pub fn draw(&mut self, ui: &imgui::Ui) -> WindowActions {
        self.window_actions.restore_bounds = self.pending_window_bounds.take();
        let _theme = crate::theme::apply(ui, self.settings.light_theme);
        crate::setup::poll(ui, self);
        if self.setup_overlay && self.compact {
            self.expand();
        }
        self.update_visibility(ui);
        let mut state = self.engine.state();
        if self.page == 10 && !self.settings.show_com2 {
            self.set_page(0);
        }
        if self.page == 10 {
            if self
                .com2_view
                .as_ref()
                .is_none_or(|(source, _)| !std::sync::Arc::ptr_eq(source, &state))
            {
                let mut view = (*state).clone();
                // The page reads a snapshot; the aircraft's COM1 remains untouched.
                view.telemetry.com1_khz = view
                    .telemetry
                    .frequency(openatc_core::state::Radio::Com2)
                    .unwrap_or(0);
                self.com2_view = Some((state.clone(), std::sync::Arc::new(view)));
            }
            state = self.com2_view.as_ref().unwrap().1.clone();
        }
        let opacity = if self.compact {
            self.settings.faded_opacity.clamp(0.1, 0.85)
        } else {
            self.settings.window_opacity.clamp(0.2, 1.0)
        };
        self.background_opacity +=
            (opacity - self.background_opacity) * (ui.io().delta_time * 12.0).min(1.0);
        let mut background = ui.style_color(imgui::StyleColor::WindowBg);
        background[3] = self.background_opacity;
        let _background = ui.push_style_color(imgui::StyleColor::WindowBg, background);
        ui.window("OpenATC AI")
            .position(self.window_offset, imgui::Condition::Always)
            .size(self.window_size, imgui::Condition::Always)
            .flags(
                imgui::WindowFlags::NO_DECORATION
                    | imgui::WindowFlags::NO_MOVE
                    | imgui::WindowFlags::NO_SAVED_SETTINGS
                    | imgui::WindowFlags::NO_SCROLLBAR
                    | imgui::WindowFlags::NO_SCROLL_WITH_MOUSE,
            )
            .build(|| {
                {
                    let _disabled = ui.begin_disabled(self.confirm_reset || self.reset_pending);
                    self.draw_toolbar(ui);
                }
                let _page_background = ui.push_style_color(imgui::StyleColor::ChildBg, [0.0; 4]);
                ui.child_window("##page")
                    .size([0.0, (ui.content_region_avail()[1] - 24.0).max(1.0)])
                    .scroll_bar(self.setup_overlay || !matches!(self.page, 0 | 10))
                    .build(|| {
                        if self.confirm_reset || self.reset_pending {
                            self.draw_reset_confirmation(ui);
                        } else if self.setup_overlay {
                            crate::setup::overlay(ui, self);
                        } else {
                            self.draw_body(ui, &state);
                        }
                    });
                self.resize_grip(ui);
            });
        let mut actions = std::mem::take(&mut self.window_actions);
        // WantTextInput is updated on the following frame. Include the item
        // activated by this click before the host decides to release focus.
        actions.capture_keyboard =
            ui.is_any_item_active() || ui.io().want_capture_keyboard || ui.io().want_text_input;
        if actions.close {
            self.save_settings();
        }
        actions.ui_scale = self.settings.ui_scale;
        actions.minimum_size = [
            520.0 * self.settings.ui_scale,
            (if self.compact { 180.0 } else { 460.0 }) * self.settings.ui_scale,
        ];
        actions
    }

    fn draw_reset_confirmation(&mut self, ui: &imgui::Ui) {
        let scale = ui.current_font_size() / 18.0;
        let available = ui.content_region_avail();
        let size = [
            (480.0 * scale).min(available[0]),
            (310.0 * scale).min(available[1]),
        ];
        let cursor = ui.cursor_pos();
        ui.set_cursor_pos([
            cursor[0] + (available[0] - size[0]) * 0.5,
            cursor[1] + (available[1] - size[1]) * 0.5,
        ]);
        let _background =
            ui.push_style_color(imgui::StyleColor::ChildBg, widgets::panel_color(ui, 1.0));
        let _padding =
            ui.push_style_var(imgui::StyleVar::WindowPadding([20.0 * scale, 18.0 * scale]));
        let _rounding = ui.push_style_var(imgui::StyleVar::ChildRounding(10.0 * scale));
        ui.child_window("##new-flight-confirmation")
            .size(size)
            .border(true)
            .always_use_window_padding(true)
            .scroll_bar(false)
            .build(|| {
                ui.text_colored(crate::widgets::accent(ui), if self.reset_pending { "Starting a new flight" } else { "Start a new flight?" });
                ui.spacing();
                ui.text_wrapped("Clear the current flight plan, conversation, clearances and pending crew actions.");
                ui.spacing();
                ui.text_wrapped("Your settings and controller voices are kept. Aircraft controls stay as they are.");
                if !self.reset_error.is_empty() {
                    ui.spacing();
                    ui.text_colored(crate::widgets::warning(ui), "Could not reset the flight");
                    ui.text_wrapped(&self.reset_error);
                }
                ui.set_cursor_pos([20.0 * scale, size[1] - 58.0 * scale]);
                if self.reset_pending {
                    ui.text_disabled("Waiting for the engine...");
                } else {
                    let width = ((size[0] - 48.0 * scale) * 0.5).max(1.0);
                    if ui.button_with_size(if self.reset_error.is_empty() { "Start new flight" } else { "Retry" }, [width, 36.0 * scale]) {
                        self.new_flight();
                    }
                    #[cfg(test)]
                    {
                        let a = ui.item_rect_min(); let b = ui.item_rect_max();
                        self.reset_button_bounds[0] = [a[0], a[1], b[0], b[1]];
                    }
                    ui.same_line();
                    if ui.button_with_size("Cancel", [width, 36.0 * scale]) {
                        self.confirm_reset = false;
                        self.reset_error.clear();
                    }
                    #[cfg(test)]
                    {
                        let a = ui.item_rect_min(); let b = ui.item_rect_max();
                        self.reset_button_bounds[1] = [a[0], a[1], b[0], b[1]];
                    }
                }
            });
    }

    fn resize_grip(&mut self, ui: &imgui::Ui) {
        let size = 24.0;
        let content = ui.window_content_region_max();
        let corner = [
            self.window_offset[0] + content[0],
            self.window_offset[1] + content[1],
        ];
        ui.set_cursor_screen_pos([corner[0] - size, corner[1] - size]);
        ui.invisible_button("##resize-window", [size, size]);
        let hovered = ui.is_item_hovered();
        if hovered || ui.is_item_active() {
            ui.set_mouse_cursor(Some(imgui::MouseCursor::ResizeNWSE));
        }
        if ui.is_item_active() && ui.is_mouse_dragging(imgui::MouseButton::Left) {
            self.window_actions.resize_delta = ui.io().mouse_delta;
        }
        if hovered {
            ui.tooltip_text("Drag to resize");
        }
        let color = if hovered {
            [0.2, 0.8, 0.95, 1.0]
        } else {
            [0.4, 0.55, 0.65, 0.8]
        };
        let draw = ui.get_window_draw_list();
        for offset in [7.0, 12.0, 17.0] {
            draw.add_line(
                [corner[0] - offset, corner[1] - 4.0],
                [corner[0] - 4.0, corner[1] - offset],
                color,
            )
            .thickness(2.0)
            .build();
        }
    }

    fn draw_toolbar(&mut self, ui: &imgui::Ui) {
        let scale = ui.current_font_size() / 18.0;
        let page_count =
            (if self.settings.dev_mode { 10 } else { 9 }) + usize::from(self.settings.show_com2);
        let action_count = 3 + usize::from(self.can_pop_out) + usize::from(self.settings.dev_mode);
        let wrap = ui.content_region_avail()[0]
            < ((1 + page_count + action_count) as f32 * 39.0 + 140.0) * scale;
        let _padding = ui.push_style_var(imgui::StyleVar::WindowPadding([0.0, 0.0]));
        let _spacing = ui.push_style_var(imgui::StyleVar::ItemSpacing([7.0 * scale, 7.0 * scale]));
        let _background = ui.push_style_color(imgui::StyleColor::ChildBg, [0.0; 4]);
        ui.child_window("##toolbar")
            .size([0.0, if wrap { 73.0 } else { 34.0 } * scale])
            .scroll_bar(false)
            .build(|| {
                if widgets::icon_button(ui, "Close OpenATC AI", widgets::ToolbarIcon::Close, false)
                {
                    self.window_actions.close = true;
                }
                if !wrap {
                    self.toolbar_pages(ui, true);
                }
                self.toolbar_drag(ui, scale);
                self.toolbar_right(ui, scale);
                if wrap {
                    self.toolbar_pages(ui, false);
                }
            });
    }

    fn toolbar_pages(&mut self, ui: &imgui::Ui, inline: bool) {
        use widgets::ToolbarIcon;
        const PAGES: [(usize, &str, ToolbarIcon); 11] = [
            (0, "ATC and crew", ToolbarIcon::Messages),
            (10, "ATC COM2", ToolbarIcon::Messages),
            (6, "Radios", ToolbarIcon::Headset),
            (1, "Flight plan", ToolbarIcon::FlightPlan),
            (2, "Taxi", ToolbarIcon::Taxi),
            (3, "Arrival", ToolbarIcon::Arrival),
            (4, "Airports", ToolbarIcon::Airports),
            (5, "Settings", ToolbarIcon::Settings),
            (8, "Traffic", ToolbarIcon::Traffic),
            (9, "Missions", ToolbarIcon::Missions),
            (7, "Map (debug)", ToolbarIcon::Map),
        ];
        for (index, &(page, label, icon)) in PAGES.iter().enumerate() {
            if page == 7 && !self.settings.dev_mode {
                continue;
            }
            if page == 10 && !self.settings.show_com2 {
                continue;
            }
            if inline || index > 0 {
                ui.same_line();
            }
            if widgets::icon_button(ui, label, icon, self.page == page) {
                self.set_page(page);
            }
            if page == 10 {
                let corner = ui.item_rect_max();
                let size = ui.current_font_size();
                ui.get_window_draw_list().add_text(
                    [corner[0] - size * 0.55, corner[1] - size],
                    widgets::accent(ui),
                    "2",
                );
            }
            #[cfg(test)]
            {
                let (a, b) = (ui.item_rect_min(), ui.item_rect_max());
                self.toolbar_bounds[if page == 10 { 11 } else { page }] = [a[0], a[1], b[0], b[1]];
            }
        }
    }

    /// Drag area with the centered title.
    fn toolbar_drag(&mut self, ui: &imgui::Ui, scale: f32) {
        ui.same_line();
        let right_buttons = (if self.can_pop_out { 4.0 } else { 3.0 })
            + if self.settings.dev_mode { 1.0 } else { 0.0 };
        let drag_width =
            (ui.content_region_avail()[0] - right_buttons * 39.0 * scale - 7.0 * scale).max(1.0);
        let position = ui.cursor_screen_pos();
        ui.invisible_button("##drag-window", [drag_width, 32.0 * scale]);
        #[cfg(test)]
        {
            let (a, b) = (ui.item_rect_min(), ui.item_rect_max());
            self.toolbar_drag_bounds = [a[0], a[1], b[0], b[1]];
        }
        if ui.is_item_hovered() {
            ui.tooltip_text("Drag here to move OpenATC AI");
        }
        if ui.is_item_active() && ui.is_mouse_dragging(imgui::MouseButton::Left) {
            self.window_actions.drag_delta = ui.io().mouse_delta;
        }
        if drag_width > 135.0 * scale {
            let text = "OpenATC AI";
            let text_size = ui.calc_text_size(text);
            ui.get_window_draw_list().add_text(
                [
                    position[0] + (drag_width - text_size[0]) * 0.5,
                    position[1] + (32.0 * scale - text_size[1]) * 0.5,
                ],
                widgets::muted(ui),
                text,
            );
        }
    }

    /// Discard speech belonging to a position before a developer jump.
    pub fn cancel_jump_speech(&mut self) {
        self.speech.stop_speech();
        self.speech_com2.stop_speech();
        self.speech_queue.clear();
        self.auto_reply_pending = false;
        self.copilot_reply_prepared = false;
        self.last_copilot_reply_sequence = self.engine.state().next_sequence.saturating_sub(1);
        self.last_speech_sequence = self.engine.state().next_sequence.saturating_sub(1);
    }

    /// New flight, pin, compact and pop-out controls on the right.
    fn toolbar_right(&mut self, ui: &imgui::Ui, _scale: f32) {
        use widgets::{ToolbarIcon, icon_button};
        if self.settings.dev_mode {
            ui.same_line();
            if icon_button(ui, "Debug flight jump", ToolbarIcon::FastForward, false) {
                ui.open_popup("Debug flight jump");
            }
            ui.popup("Debug flight jump", || {
                ui.text("Move aircraft for testing");
                if ui.button("Next holding point") {
                    self.requested_debug_jump = Some("hold".into());
                    ui.close_current_popup();
                }
                if ui.button("Departure runway start") {
                    self.requested_debug_jump = Some("runway".into());
                    ui.close_current_popup();
                }
                ui.separator();
                ui.text("Airborne checkpoints (pause for review)");
                ui.text_wrapped("A powered aircraft and IFR clearance are required. These move the route monitor but do not grant approach or landing clearance.");
                ui.input_float(
                    "Air start speed kt (0 = current TAS)",
                    &mut self.debug_jump_speed,
                )
                .build();
                for (kind, label) in [
                    ("sid", "Departure SID"),
                    ("cruise", "Cruise"),
                    ("star", "STAR entry"),
                    ("approach", "20 NM before approach"),
                ] {
                    if ui.button(label) {
                        self.requested_debug_jump = Some(kind.into());
                        ui.close_current_popup();
                    }
                }
                ui.text_wrapped("Review thrust, speed, gear/flaps, AP and the active FMS leg before resuming. Scenery may reload. Fly the landing normally; use Next holding point for an approved taxi-in route.");
            });
        }
        ui.same_line();
        {
            let _disabled = ui.begin_disabled(self.reset_pending);
            if icon_button(ui, "New flight", ToolbarIcon::NewFile, false) {
                self.collapse(false);
                self.confirm_reset = true;
            }
        }
        ui.same_line();
        if icon_button(
            ui,
            "Keep expanded",
            ToolbarIcon::Pin,
            self.settings.pin_open,
        ) {
            self.settings.pin_open = !self.settings.pin_open;
            if self.settings.pin_open {
                self.expand();
            }
            self.save_settings();
        }
        ui.same_line();
        let disabled = ui.begin_disabled(!matches!(self.page, 0 | 10));
        if icon_button(
            ui,
            if self.compact {
                "Expand controls"
            } else {
                "Communications only"
            },
            if self.compact {
                ToolbarIcon::Expand
            } else {
                ToolbarIcon::Collapse
            },
            self.compact,
        ) {
            if self.compact {
                self.expand();
            } else {
                self.collapse(true);
            }
        }
        #[cfg(test)]
        {
            let (a, b) = (ui.item_rect_min(), ui.item_rect_max());
            self.collapse_bounds = [a[0], a[1], b[0], b[1]];
        }
        drop(disabled);
        if self.can_pop_out {
            ui.same_line();
            if icon_button(
                ui,
                if self.popped_out {
                    "Return to simulator"
                } else {
                    "Pop out window"
                },
                ToolbarIcon::PopOut,
                self.popped_out,
            ) {
                self.window_actions.toggle_pop_out = true;
            }
            #[cfg(test)]
            {
                let (a, b) = (ui.item_rect_min(), ui.item_rect_max());
                self.toolbar_bounds[10] = [a[0], a[1], b[0], b[1]];
            }
        }
    }

    fn draw_body(&mut self, ui: &imgui::Ui, state: &openatc_core::state::State) {
        if self.page == 7 && !self.settings.dev_mode {
            self.set_page(0);
        }
        if self.compact && matches!(self.page, 0 | 10) {
            crate::pages::atc_page(ui, self, state);
            return;
        }
        let descriptions = [
            "Talk with your crew and air traffic control.",
            "Plan your route, procedures and fuel.",
            "Plan and follow your taxi route on the airport surface.",
            "See your descent against the destination runway and terrain.",
            "Explore airport layouts, approaches and services.",
            "Configure your crew, audio and flight preferences.",
            "Nearby radio stations. Select a station to tune COM1.",
            "Controller boundaries used by flight handoffs. Debug view.",
            "Nearby aircraft, traffic settings and radio conversations.",
            "Choose a flight story, create your own and follow the flight journal.",
            "Talk with air traffic control on the second radio.",
        ];
        crate::widgets::page_heading(ui, PAGES[self.page], descriptions[self.page]);
        match self.page {
            1 => crate::pages::plan_page(ui, self, state),
            2 => crate::pages::taxi_page(ui, self, state),
            3 => crate::pages::arrival_page(ui, self, state),
            4 => crate::pages::airports_page(ui, self, state),
            5 => crate::pages::settings_page(ui, self, state),
            6 => crate::pages::radio_page(ui, self, state),
            7 => crate::sectors::page(ui, self, state),
            8 => crate::traffic::tab(ui, self, state),
            9 => self.missions.page(ui, &self.engine, state),
            _ => crate::pages::atc_page(ui, self, state),
        }
    }
}

/// Steady-clock seconds for interaction timing.
#[must_use]
pub fn now_seconds() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
}

pub(crate) fn format_typed_transmission(station: &str, callsign: &str, message: &str) -> String {
    let compact = |text: &str| {
        text.chars()
            .filter(char::is_ascii_alphanumeric)
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let has_callsign = !callsign.is_empty() && compact(message).contains(&compact(callsign));
    let has_station =
        !station.is_empty() && message.to_lowercase().contains(&station.to_lowercase());
    let id = match (has_station, has_callsign) {
        (true, true) => return message.to_owned(),
        (true, false) => "pilot_typed_callsign",
        (false, true) => "pilot_typed_station",
        _ => "pilot_typed_transmission",
    };
    openatc_core::dialogue::say(
        id,
        &[
            ("station", station.into()),
            ("callsign", callsign.into()),
            ("message", message.into()),
        ],
    )
}

#[cfg(test)]
mod typed_transmission_tests {
    #[test]
    fn addressing_is_added_once_to_typed_requests() {
        let text = super::format_typed_transmission("Los Angeles Ground", "C-GTLT", "Request taxi");
        assert!(
            text.contains("Los Angeles Ground")
                && text.contains("C-GTLT")
                && text.contains("Request taxi")
        );
        assert_eq!(
            super::format_typed_transmission("Los Angeles Ground", "C-GTLT", &text),
            text
        );
        let own = super::format_typed_transmission(
            "Los Angeles Ground",
            "C-GTLT",
            "C-GTLT, request taxi",
        );
        assert_eq!(own.matches("C-GTLT").count(), 1);
        let station = super::format_typed_transmission(
            "Los Angeles Ground",
            "C-GTLT",
            "Los Angeles Ground, request taxi",
        );
        assert_eq!(station.matches("Los Angeles Ground").count(), 1);
    }
}

#[cfg(test)]
mod layout_tests {
    fn frame(bounds: [i32; 4]) -> super::WindowFrame {
        super::WindowFrame {
            native_bounds: Some(bounds),
            position: [0.0, 0.0],
            size: [
                (bounds[2] - bounds[0]) as f32,
                (bounds[1] - bounds[3]) as f32,
            ],
            can_pop_out: true,
            popped_out: false,
        }
    }
    #[test]
    fn compact_moves_keep_the_full_height_and_pending_expansion_cannot_save_compact_bounds() {
        let mut interface = super::Interface::new("http://127.0.0.1:1");
        interface.settings_loaded = true;
        interface.set_window_frame(frame([150, 1000, 1110, 240]));
        interface.collapse(true);
        // Observe the host's requested compact height before the user drags it.
        let compact_height = interface.window_actions.height.unwrap().round() as i32;
        interface.set_window_frame(frame([150, 1000, 1110, 1000 - compact_height]));
        interface.set_window_frame(frame([210, 970, 1250, 970 - compact_height]));
        assert_eq!(
            interface.settings.window_bounds,
            Some([210, 970, 1250, 210]),
            "Moving/resizing compact mode must preserve its new anchor and expanded height"
        );
        interface.expand();
        interface.set_window_frame(frame([210, 970, 1250, 970 - compact_height]));
        assert_eq!(
            interface.settings.window_bounds,
            Some([210, 970, 1250, 210]),
            "A pre-expansion host frame cannot replace saved full geometry"
        );
        interface.set_window_frame(frame([210, 970, 1250, 210]));
        assert_eq!(
            interface.settings.window_bounds,
            Some([210, 970, 1250, 210])
        );
    }
    #[test]
    fn pending_expansion_does_not_replace_the_expanded_height_on_another_toggle() {
        let mut interface = super::Interface::new("http://127.0.0.1:1");
        interface.settings_loaded = true;
        interface.set_window_frame(frame([150, 1000, 1110, 240]));
        interface.collapse(true);
        interface.set_window_frame(frame([150, 1000, 1110, 560]));
        interface.expand();
        interface.collapse(true);
        interface.expand();
        assert_eq!(interface.window_actions.height, Some(760.0));
    }

    #[test]
    fn new_flight_keeps_splitter_and_compact_layout_state() {
        let mut old = super::Interface::new("http://127.0.0.1:1");
        old.settings_loaded = true;
        old.transcript_fraction = 0.35;
        old.set_window_frame(frame([150, 1000, 1110, 240]));
        old.collapse(true);
        let mut fresh = super::Interface::new("http://127.0.0.1:1");
        fresh.settings = old.settings.clone();
        fresh.settings_loaded = true;
        fresh.preserve_window_layout(&old);
        assert!(fresh.compact && fresh.manual_compact);
        assert_eq!(fresh.transcript_fraction, 0.35);
        assert_eq!(fresh.expanded_height, 760.0);
        assert_eq!(fresh.pending_mode_height, old.pending_mode_height);
        let restored: openatc_settings::Settings =
            serde_json::from_value(serde_json::to_value(&fresh.settings).unwrap()).unwrap();
        assert_eq!(restored.transcript_fraction, 0.35);
        assert_eq!(restored.window_bounds, Some([150, 1000, 1110, 240]));
    }

    #[test]
    fn compact_frames_do_not_overwrite_expanded_layout() {
        let mut interface = super::Interface::new("http://127.0.0.1:1");
        interface.settings_loaded = true;
        interface.transcript_fraction = 0.4;
        let frame = super::WindowFrame {
            native_bounds: Some([150, 1000, 1110, 240]),
            position: [0.0, 0.0],
            size: [960.0, 760.0],
            can_pop_out: true,
            popped_out: false,
        };
        interface.set_window_frame(frame);
        assert_eq!(
            interface.settings.window_bounds,
            Some([150, 1000, 1110, 240])
        );
        assert_eq!(interface.settings.transcript_fraction, 0.4);
        interface.compact = true;
        interface.set_window_frame(super::WindowFrame {
            native_bounds: Some([150, 1000, 1110, 800]),
            ..frame
        });
        assert_eq!(
            interface.settings.window_bounds,
            Some([150, 1000, 1110, 240])
        );
    }
}

#[cfg(test)]
mod planning_submission_tests {
    #[test]
    fn clearance_checks_the_draft_before_automatic_filing() {
        let mut interface = super::Interface::new("http://127.0.0.1:1");
        interface.draft.callsign = "C-GTLT".into();
        let request = openatc_core::state::Request {
            role: "atc".into(),
            intent: "clearance".into(),
            text: "C-GTLT, request IFR clearance".into(),
            ..Default::default()
        };
        interface.send(&request);
        assert!(interface.notice.starts_with("C-GTLT,"));
        assert!(interface.notice.contains("not filed a flight plan"));
        assert!(interface.pending_clearance.is_none());
        interface.draft.departure = "LFMN".into();
        interface.send(&request);
        assert!(interface.notice.contains("incomplete or invalid"));
        assert!(interface.pending_clearance.is_none());
        interface.draft.destination = "LIMC".into();
        interface.draft.runway = "04R".into();
        interface.send(&request);
        assert!(interface.pending_clearance.is_some());
        assert!(interface.notice.contains("Submitting"));
    }
}

#[cfg(test)]
mod radio_busy_tests {
    use super::*;
    #[test]
    fn traffic_on_an_untuned_station_is_not_audible() {
        let mut state = State::default();
        state.telemetry.com1_khz = 120_160;
        let tower: Transmission = serde_json::from_value(serde_json::json!({
            "radio": "com1",
            "frequency": 118700
        }))
        .expect("tower entry");
        assert!(!entry_frequency_tuned(&tower, &state));
        state.telemetry.com1_khz = 118_700;
        assert!(entry_frequency_tuned(&tower, &state));
        let own: Transmission =
            serde_json::from_value(serde_json::json!({"radio": "com1"})).expect("own entry");
        assert!(entry_frequency_tuned(&own, &state));
    }

    #[test]
    fn another_receiver_queue_does_not_block_the_controlling_radio() {
        let mut interface = Interface::new("http://127.0.0.1:1");
        interface
            .speech_queue
            .push_back(openatc_core::state::Transmission {
                radio: Some(openatc_core::state::Radio::Com2),
                frequency: None,
                gap_ms: 0,
                speaker: "ATC".into(),
                information_only: false,
                pilot_reply: String::new(),
                background: false,
                chatter: false,
                text: String::new(),
                sequence: 1,
                position: String::new(),
                voice: String::new(),
                delivery: String::new(),
                speed: 1.0,
                urgent: false,
            });
        assert!(!interface.radio_busy(openatc_core::state::Radio::Com1));
        assert!(interface.radio_busy(openatc_core::state::Radio::Com2));
        interface.speech_queue.clear();
        interface.auto_reply_pending = true;
        interface.auto_reply_radio = openatc_core::state::Radio::Com2;
        assert!(!interface.radio_busy(openatc_core::state::Radio::Com1));
        assert!(interface.radio_busy(openatc_core::state::Radio::Com2));
    }

    #[test]
    fn stopping_airport_chatter_keeps_live_traffic_queued() {
        let mut interface = Interface::new("http://127.0.0.1:1");
        let entry = |chatter: bool, sequence: u32| openatc_core::state::Transmission {
            radio: Some(openatc_core::state::Radio::Com2),
            frequency: Some(118_700),
            gap_ms: 700,
            speaker: "ATC".into(),
            information_only: false,
            pilot_reply: String::new(),
            background: true,
            chatter,
            text: String::new(),
            sequence,
            position: "Tower".into(),
            voice: String::new(),
            delivery: "standard".into(),
            speed: 1.0,
            urgent: false,
        };
        interface.speech_queue.push_back(entry(true, 1));
        interface.speech_queue.push_back(entry(false, 2));
        interface.chatter_playing = Some((openatc_core::state::Radio::Com2, 118_700, 700));
        interface.stop_airport_chatter(openatc_core::state::Radio::Com2);
        assert_eq!(interface.speech_queue.len(), 1);
        assert!(!interface.speech_queue[0].chatter);
        assert!(interface.chatter_playing.is_none());
    }

    #[test]
    fn background_chatter_does_not_block_the_receiver() {
        let mut interface = Interface::new("http://127.0.0.1:1");
        interface
            .speech_queue
            .push_back(openatc_core::state::Transmission {
                radio: Some(openatc_core::state::Radio::Com1),
                frequency: Some(121_900),
                gap_ms: 700,
                speaker: "EZY1234".into(),
                information_only: false,
                pilot_reply: String::new(),
                background: true,
                chatter: true,
                text: "ready to taxi".into(),
                sequence: 1,
                position: "Traffic".into(),
                voice: String::new(),
                delivery: "standard".into(),
                speed: 1.0,
                urgent: false,
            });
        assert!(!interface.radio_busy(openatc_core::state::Radio::Com1));
        interface.chatter_playing = Some((openatc_core::state::Radio::Com1, 121_900, 700));
        assert!(!interface.radio_busy(openatc_core::state::Radio::Com1));
        interface.stop_chatter(openatc_core::state::Radio::Com1);
        assert!(interface.speech_queue.is_empty());
        assert!(interface.chatter_playing.is_none());
    }
}

#[cfg(test)]
mod checkin_gate_tests {
    use super::*;

    #[test]
    fn a_spoken_handoff_readback_opens_the_checkin_gate() {
        let mut state = State::default();
        state.plan.callsign = "C-GTLT".into();
        state.transcript.push(
            serde_json::from_value(serde_json::json!({
                "speaker": "ATC",
                "sequence": 85,
                "position": "NICE Tower",
                "text": "C-GTLT, contact NICE APPROACH on 120.160.",
                "pilotReply": "Contact NICE APPROACH, 120.160, C-GTLT."
            }))
            .unwrap(),
        );
        assert!(!handoff_readback_spoken(&state, 85));
        state.transcript.push(
            serde_json::from_value(serde_json::json!({
                "speaker": "COPILOT",
                "sequence": 86,
                "text": "Contact NICE APPROACH, 120.160, C-GTLT."
            }))
            .unwrap(),
        );
        assert!(handoff_readback_spoken(&state, 85));
    }
}

#[cfg(test)]
mod station_gate_tests {
    use super::*;

    fn controller(frequency: i32, service: &str) -> openatc_core::stations::Station {
        openatc_core::stations::Station {
            khz: frequency,
            service: service.into(),
            receivable: true,
            ..Default::default()
        }
    }

    fn telemetry(frequency: i32) -> openatc_core::state::Telemetry {
        openatc_core::state::Telemetry {
            position_valid: true,
            com1_khz: frequency,
            ..Default::default()
        }
    }

    #[test]
    fn off_station_push_to_talk_never_starts_a_transcription() {
        let mut interface = Interface::new("http://127.0.0.1:1");
        interface.nearby_stations = vec![controller(121_700, "Ground")];
        interface.engine.telemetry(telemetry(118_700));
        interface.talk_push_to_talk(true, false);
        assert!(!interface.ptt_armed);
        assert!(interface.last_action.contains("not on station"));
        assert!(interface.notice.contains("Not on station"));
        interface.mic_push_to_talk(true);
        assert!(!interface.speech.recording());
        assert!(interface.notice.contains("Not on station"));
        assert!(!interface.speech.busy());
    }

    #[test]
    fn a_tuned_controller_permits_radio_push_to_talk() {
        let mut interface = Interface::new("http://127.0.0.1:1");
        interface.nearby_stations = vec![controller(121_700, "Ground")];
        interface.engine.telemetry(telemetry(121_700));
        interface.radio_push_to_talk(true, openatc_core::state::Radio::Com1);
        assert!(interface.ptt_armed);
        assert_eq!(interface.ptt_role, "atc");
        assert_eq!(interface.ptt_radio, openatc_core::state::Radio::Com1);
        interface.radio_push_to_talk(false, openatc_core::state::Radio::Com1);
    }
}
