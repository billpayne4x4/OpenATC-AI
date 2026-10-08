//! UI state, asynchronous replies and frame updates.

use crate::client::EngineClient;
use crate::widgets;
use openatc_audio::Speech;
use openatc_core::airport::Airport;
use openatc_core::arrival::{ArrivalAirport, TerrainProfile};
use openatc_core::identify::interpret_text;
use openatc_core::intents::{can_transmit, resolve_crew_role};
use openatc_core::state::{FlightPlan, State, Transmission};
use openatc_settings::Settings;

/// Local notice shown under the transcript (power refusals and the like).
pub struct LocalNotice {
    /// Speaker tag, usually SYSTEM.
    pub speaker: String,
    /// Notice text.
    pub text: String,
}

/// Page indices in sidebar order.
pub const PAGES: [&str; 7] = [
    "ATC",
    "Flight Plan",
    "Taxi",
    "Arrival",
    "Airports",
    "Settings",
    "Channels",
];

#[derive(Clone, Copy, Debug)]
/// In-sim window placement and pop-out state.
pub struct WindowFrame {
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
    /// Close button pressed.
    pub close: bool,
    /// Pop-out toggle pressed.
    pub toggle_pop_out: bool,
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
    /// Engine connection plus snapshot cache.
    pub engine: EngineClient,
    /// Receivable published stations around the aircraft.
    pub nearby_stations: Vec<openatc_core::stations::Station>,
    /// Station/weather reception status.
    pub radio_notice: String,
    /// User-editable station search, initially the nearest airport.
    pub station_filter: String,
    /// Search configured aircraft crew controls.
    pub crew_control_filter: String,
    /// Preserve an intentionally cleared station search.
    pub station_filter_initialized: bool,
    /// Station index/discovery request is pending.
    pub stations_loading: bool,
    stations_checked: f64,
    atis_checked: f64,
    atis_frequency: i32,
    atis_airport: String,
    atis_pending: bool,
    /// Audio capture and playback.
    pub speech: Speech,
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
    /// Last voice check time.
    pub last_voice_check: f64,
    /// Last action line for dev mode.
    pub last_action: String,
    /// Prevent duplicate automatic replies while the command is in flight.
    pub auto_reply_pending: bool,
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
    /// Last transcript sequence queued for speech.
    pub last_speech_sequence: u32,
    /// Pending speech entries.
    pub speech_queue: std::collections::VecDeque<Transmission>,
    /// Panel window offset in screen points. Stays zero on desktop; the
    /// simulator host sets it so content lands inside the floating window.
    pub window_offset: [f32; 2],
    /// Last measured panel size.
    pub window_size: [f32; 2],
    /// Communications-only compact layout.
    pub compact: bool,
    /// Transcript share of the ATC page.
    pub transcript_fraction: f32,
    /// Frequency picked on the radio page, if any.
    pub requested_frequency_khz: Option<i32>,
    /// True when the plugin (not desktop) hosts the panel.
    pub simulator_host: bool,
    /// Background opacity when faded.
    pub background_opacity: f32,
    /// Reset confirmation armed.
    pub confirm_reset: bool,
    /// Session reset awaiting an engine response.
    pub reset_pending: bool,
    /// Notifies the simulator host to clear flight-local callbacks and guidance.
    pub flight_reset_generation: u64,
    can_pop_out: bool,
    popped_out: bool,
    expanded_height: f32,
    compact_until_mouse_leaves: bool,
    window_actions: WindowActions,
    pending_settings_json: String,
}

impl Interface {
    /// Connect to the engine at the endpoint.
    #[must_use]
    pub fn new(endpoint: &str) -> Self {
        Self {
            engine: EngineClient::new(endpoint),
            nearby_stations: Vec::new(),
            radio_notice: String::new(),
            station_filter: String::new(),
            crew_control_filter: String::new(),
            station_filter_initialized: false,
            stations_loading: false,
            stations_checked: 0.0,
            atis_checked: 0.0,
            atis_frequency: 0,
            atis_airport: String::new(),
            atis_pending: false,
            speech: Speech::default(),
            page: 0,
            settings_loaded: false,
            plan_loaded: false,
            settings: Settings::default(),
            draft: FlightPlan::default(),
            airport: Airport::default(),
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
            last_voice_check: 0.0,
            last_action: String::new(),
            auto_reply_pending: false,
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
            last_speech_sequence: 0,
            speech_queue: std::collections::VecDeque::new(),
            window_offset: [0.0, 0.0],
            window_size: [960.0, 760.0],
            compact: false,
            transcript_fraction: 0.57,
            requested_frequency_khz: None,
            simulator_host: false,
            background_opacity: 0.96,
            confirm_reset: false,
            reset_pending: false,
            flight_reset_generation: 0,
            can_pop_out: false,
            popped_out: false,
            expanded_height: 760.0,
            compact_until_mouse_leaves: false,
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
        if self.reset_pending {
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
            if let Err(error) = openatc_core::ops::validate_flight_plan(&self.draft) {
                self.notice = format!("Planning flight is incomplete: {error}");
                self.plan_notice.clone_from(&self.notice);
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
        let body = serde_json::to_value(request).unwrap_or(serde_json::Value::Null);
        self.engine.post("/request", body);
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

    /// Push-to-talk for the message box mic: record on press, transcribe on release.
    pub fn mic_push_to_talk(&mut self, down: bool) {
        if self.reset_pending {
            return;
        }
        if down {
            self.expand();
        }
        self.ptt_armed = false;
        if down {
            if self.speech.busy() || self.speech.recording() || self.speech.monitoring() {
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
            self.expand();
            if self.speech.busy() || self.speech.recording() || self.speech.monitoring() {
                "Mic busy — wait for the current job".clone_into(&mut self.last_action);
                return;
            }
            self.ptt_armed = true;
            self.ptt_role = if copilot {
                "copilot".to_owned()
            } else {
                "auto".to_owned()
            };
            self.record_start = now_seconds();
            self.record_peak = 0.0;
            self.speech.start_recording(false);
            self.last_action = if copilot {
                "PTT recording (copilot)".to_owned()
            } else {
                "PTT recording".to_owned()
            };
        } else if self.speech.recording() {
            let endpoint = self.engine.endpoint();
            self.speech.transcribe(&endpoint);
        } else {
            self.ptt_armed = false;
        }
    }

    /// Current engine refusal line for error reports.
    #[must_use]
    pub fn last_notice(&self) -> String {
        self.notice.clone()
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
                "Flight plan accepted by OpenATC AI.".to_owned()
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
                            self.airport = airport.clone();
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
                        self.airport = airport;
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
                fresh.last_settings_json = self.last_settings_json.clone();
                fresh.simulator_host = self.simulator_host;
                fresh.window_offset = self.window_offset;
                fresh.window_size = self.window_size;
                fresh.can_pop_out = self.can_pop_out;
                fresh.popped_out = self.popped_out;
                fresh.current_airport = self.current_airport.clone();
                fresh.flight_reset_generation = self.flight_reset_generation;
                fresh.clipboard = self.clipboard.take();
                fresh.plan_notice = "New flight ready.".into();
                *self = fresh;
            } else {
                self.reset_pending = false;
                reply.error.clone_into(&mut self.notice);
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
                    "No answer — nobody on this frequency.".clone_into(&mut self.notice);
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
        self.speech.configure(
            &self.settings.input_device.clone(),
            &self.settings.output_device.clone(),
            self.settings.input_gain,
            self.settings.master_volume,
        );
        self.speech.poll();
        if self.ptt_armed && now_seconds() - self.record_start > 45.0 {
            self.ptt_armed = false;
        }
        if self.speech.recording() && !self.speech.monitoring() && self.ptt_armed {
            self.record_peak = self.record_peak.max(self.speech.input_level());
        }
        let transcription = self.speech.take_transcript();
        if !transcription.is_empty() && self.ptt_armed {
            self.ptt_armed = false;
            let role = if self.ptt_role == "copilot" {
                "copilot".to_owned()
            } else {
                self.crew_role()
            };
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
                self.send(&request);
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
        }
        for entry in &state.transcript {
            if entry.sequence > self.last_speech_sequence {
                let controller = entry.speaker == "ATC";
                let pilot = !controller
                    && entry.speaker != "COPILOT"
                    && entry.speaker != "CABIN"
                    && entry.speaker != "ATTENDANT"
                    && entry.speaker != "GROUND";
                if (controller && self.settings.controller_speech)
                    || (entry.speaker == "COPILOT" && self.settings.copilot_speech)
                    || (pilot && self.settings.pilot_speech)
                    || matches!(entry.speaker.as_str(), "CABIN" | "ATTENDANT")
                    || entry.speaker == "GROUND"
                {
                    self.speech_queue.push_back(entry.clone());
                }
                self.last_speech_sequence = entry.sequence;
            }
        }
        while self.speech_queue.len() > 20 {
            self.speech_queue.pop_front();
        }
        let ready =
            !self.speech_queue.is_empty() && !self.speech.busy() && !self.speech.recording();
        if ready && let Some(entry) = self.speech_queue.pop_front() {
            self.speak_entry(&entry);
        }
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
        self.speech.speak(&openatc_audio::SpeakRequest {
            text: &entry.text,
            engine_endpoint: &endpoint,
            role,
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
        if self.reset_pending {
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
            self.last_settings_json = serde_json::to_string(&self.settings).unwrap_or_default();
            self.settings_loaded = true;
            self.last_speech_sequence = state.transcript.last().map_or(0, |entry| entry.sequence);
        }
        if self.settings_loaded && self.settings.simulator_root.is_empty() {
            self.settings.simulator_root = self.engine.settings().simulator_root;
        }
        if self.engine.connected() && !self.plan_loaded {
            self.draft = state.plan.clone();
            self.plan_loaded = true;
        }
        self.update_radio(&state, now);
        self.update_local_airport(&state);
        self.update_arrival(&state, now);
        self.update_speech(&state);
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

    fn update_radio(&mut self, state: &State, now: f64) {
        if let Some(reply) = self.engine.take_reply("/stations/nearby") {
            self.stations_loading = false;
            if reply.success {
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
        let station = openatc_core::stations::tuned(&receivable, state.telemetry.com1_khz);
        let active = self.engine.connected()
            && self.radio_power
            && self.requested_frequency_khz.is_none()
            && station.is_some_and(|s| s.service == "ATIS");
        if self.atis_frequency != 0
            && (!active
                || self.atis_frequency != state.telemetry.com1_khz
                || station.is_none_or(|s| s.airport != self.atis_airport))
        {
            self.speech.stop_speech();
            self.atis_frequency = 0;
        }
        if !self.speech.busy() {
            self.atis_frequency = 0;
        }
        if let Some(reply) = self.engine.take_reply("/atis") {
            self.atis_pending = false;
            if active
                && reply.success
                && reply.data["frequency"].as_i64() == Some(i64::from(state.telemetry.com1_khz))
                && station
                    .is_some_and(|s| reply.data["airport"].as_str() == Some(s.airport.as_str()))
            {
                if let Some(text) = reply.data["text"].as_str()
                    && !self.speech.busy()
                    && !self.speech.recording()
                    && self.speech_queue.is_empty()
                {
                    self.atis_frequency = state.telemetry.com1_khz;
                    self.atis_airport = reply.data["airport"].as_str().unwrap_or("").to_owned();
                    self.speak_entry(&Transmission {
                        pilot_reply: String::new(),
                        background: false,
                        speaker: "ATC".to_owned(),
                        text: text.to_owned(),
                        delivery: "atis".to_owned(),
                        speed: 1.0,
                        voice: self.settings.voice.clone(),
                        sequence: 0,
                        position: "ATIS".to_owned(),
                        urgent: false,
                    });
                    self.radio_notice = format!(
                        "ATIS {} • simulator weather",
                        reply.data["information"].as_str().unwrap_or("")
                    );
                }
            } else if active {
                self.radio_notice = reply.error;
            }
        }
        if active
            && !self.atis_pending
            && !self.speech.busy()
            && !self.speech.recording()
            && self.speech_queue.is_empty()
            && now - self.atis_checked > 8.0
        {
            self.atis_checked = now;
            self.atis_pending = true;
            self.engine.post("/atis", serde_json::json!({}));
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
        if self.page != 2 {
            return;
        }
        let airport = if !self.current_airport.is_empty() {
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

    /// Switch page, clamped.
    pub fn set_page(&mut self, page: usize) {
        let page = page.min(PAGES.len() - 1);
        if self.page != page {
            self.show_voice_popup = false;
            self.confirm_reset = false;
            self.modal_intent.clear();
        }
        self.page = page;
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

    /// Apply host window placement for this frame.
    pub fn set_window_frame(&mut self, frame: WindowFrame) {
        self.window_offset = frame.position;
        self.window_size = frame.size;
        self.can_pop_out = frame.can_pop_out;
        self.popped_out = frame.popped_out;
    }

    /// Expand to the full panel layout.
    pub fn expand(&mut self) {
        self.last_interaction = now_seconds();
        self.compact_until_mouse_leaves = false;
        if self.compact {
            self.compact = false;
            self.window_actions.height = Some(self.expanded_height);
        }
    }

    fn collapse(&mut self, manual: bool) {
        if self.page != 0 || self.compact || !self.modal_intent.is_empty() {
            return;
        }
        self.expanded_height = self.window_size[1];
        self.compact = true;
        self.compact_until_mouse_leaves = manual;
        let scale = self.settings.ui_scale;
        let compact_height = (self.window_size[1] * self.transcript_fraction + 36.0 * scale)
            .max(200.0 * scale)
            .min(self.window_size[1]);
        self.window_actions.height = Some(compact_height);
    }

    fn update_visibility(&mut self, ui: &imgui::Ui) {
        let now = now_seconds();
        let mouse = ui.io().mouse_pos;
        let hovered = mouse[0] >= self.window_offset[0]
            && mouse[1] >= self.window_offset[1]
            && mouse[0] < self.window_offset[0] + self.window_size[0]
            && mouse[1] < self.window_offset[1] + self.window_size[1];
        if !hovered {
            self.compact_until_mouse_leaves = false;
        }
        let editing = !self.message.is_empty()
            || !self.modal_intent.is_empty()
            || self.show_voice_popup
            || ui.is_any_item_active();
        if self.page != 0
            || self.speech.recording()
            || self.ptt_armed
            || editing
            || (hovered && !self.compact_until_mouse_leaves)
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
        self.speech_queue.clear();
        self.ptt_armed = false;
        self.pending_clearance = None;
        self.requested_frequency_khz = None;
        self.flight_reset_generation = self.flight_reset_generation.wrapping_add(1);
        self.reset_pending = true;
        self.confirm_reset = false;
        self.engine.post("/session/reset", serde_json::Value::Null);
    }

    /// Draw the panel, returning one-frame window requests for the host.
    pub fn draw(&mut self, ui: &imgui::Ui) -> WindowActions {
        self.update_visibility(ui);
        let state = self.engine.state();
        let opacity = if self.compact {
            self.settings.faded_opacity.clamp(0.1, 0.85)
        } else {
            0.96
        };
        self.background_opacity +=
            (opacity - self.background_opacity) * (ui.io().delta_time * 12.0).min(1.0);
        let _background = ui.push_style_color(
            imgui::StyleColor::WindowBg,
            [0.205, 0.25, 0.29, self.background_opacity],
        );
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
                self.draw_toolbar(ui);
                let _page_background = ui.push_style_color(imgui::StyleColor::ChildBg, [0.0; 4]);
                ui.child_window("##page")
                    .size([0.0, (ui.content_region_avail()[1] - 24.0).max(1.0)])
                    .scroll_bar(self.page != 0)
                    .build(|| {
                        if self.reset_pending { ui.text("Starting a new flight..."); }
                        else { self.draw_body(ui, &state); }
                    });
                if self.confirm_reset { ui.open_popup("New flight?"); }
                ui.modal_popup("New flight?", || {
                    ui.text_wrapped("Clear the flight plan, conversation, clearances and pending crew actions? Settings and controller voices are kept. Aircraft controls stay as they are.");
                    if ui.button("New flight") {
                        self.new_flight();
                        ui.close_current_popup();
                    }
                    ui.same_line();
                    if ui.button("Cancel") {
                        self.confirm_reset = false;
                        ui.close_current_popup();
                    }
                });
                self.resize_grip(ui);
            });
        let mut actions = std::mem::take(&mut self.window_actions);
        actions.ui_scale = self.settings.ui_scale;
        actions.minimum_size = [
            520.0 * self.settings.ui_scale,
            (if self.compact { 180.0 } else { 460.0 }) * self.settings.ui_scale,
        ];
        actions
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
        let _padding = ui.push_style_var(imgui::StyleVar::WindowPadding([0.0, 0.0]));
        let _spacing = ui.push_style_var(imgui::StyleVar::ItemSpacing([7.0 * scale, 7.0 * scale]));
        let _background = ui.push_style_color(imgui::StyleColor::ChildBg, [0.0; 4]);
        ui.child_window("##toolbar")
            .size([0.0, 34.0 * scale])
            .scroll_bar(false)
            .build(|| {
                self.toolbar_pages(ui, scale);
                self.toolbar_drag(ui, scale);
                self.toolbar_right(ui, scale);
            });
    }

    /// Page switch icons on the left of the toolbar.
    fn toolbar_pages(&mut self, ui: &imgui::Ui, _scale: f32) {
        use widgets::{ToolbarIcon, icon_button};
        if icon_button(ui, "Close OpenATC AI", ToolbarIcon::Close, false) {
            self.window_actions.close = true;
        }
        for (page, label, icon) in [
            (0, "ATC communications", ToolbarIcon::Messages),
            (6, "Radio frequencies", ToolbarIcon::Headset),
            (1, "Flight plan / SimBrief", ToolbarIcon::FlightPlan),
        ] {
            ui.same_line();
            if icon_button(ui, label, icon, self.page == page) {
                self.set_page(page);
            }
        }
        for (page, label, symbol) in [
            (2, "Taxi", 2),
            (3, "Arrival", 3),
            (4, "Airports", 4),
            (5, "Settings", 5),
        ] {
            ui.same_line();
            if page_icon_button(ui, label, symbol, self.page == page) {
                self.set_page(page);
            }
        }
    }

    /// Drag area with the centered title.
    fn toolbar_drag(&mut self, ui: &imgui::Ui, scale: f32) {
        ui.same_line();
        let right_buttons = if self.can_pop_out { 4.0 } else { 3.0 };
        let drag_width = (ui.content_region_avail()[0] - right_buttons * 39.0 * scale).max(1.0);
        let position = ui.cursor_screen_pos();
        ui.invisible_button("##drag-window", [drag_width, 32.0 * scale]);
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
                widgets::MUTED,
                text,
            );
        }
    }

    /// New flight, pin, compact and pop-out controls on the right.
    fn toolbar_right(&mut self, ui: &imgui::Ui, _scale: f32) {
        use widgets::{ToolbarIcon, icon_button};
        ui.same_line();
        {
            let _disabled = ui.begin_disabled(self.reset_pending);
            if icon_button(ui, "New flight", ToolbarIcon::NewFile, false) {
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
        let _disabled = ui.begin_disabled(self.page != 0);
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
        }
    }

    fn draw_body(&mut self, ui: &imgui::Ui, state: &openatc_core::state::State) {
        let descriptions = [
            "Talk with your crew and air traffic control.",
            "Plan your route, procedures and fuel.",
            "Follow your approved route on the airport surface.",
            "See your descent against the destination runway and terrain.",
            "Explore airport layouts, approaches and services.",
            "Configure your crew, audio and flight preferences.",
            "Nearby radio stations. Select a station to tune COM1.",
        ];
        crate::widgets::page_heading(ui, PAGES[self.page], descriptions[self.page]);
        match self.page {
            1 => crate::pages::plan_page(ui, self, state),
            2 => crate::pages::taxi_page(ui, self, state),
            3 => crate::pages::arrival_page(ui, self, state),
            4 => crate::pages::airports_page(ui, self, state),
            5 => crate::pages::settings_page(ui, self, state),
            6 => crate::pages::radio_page(ui, self, state),
            _ => crate::pages::atc_page(ui, self, state),
        }
    }
}

fn page_icon_button(ui: &imgui::Ui, label: &str, symbol: i32, selected: bool) -> bool {
    let scale = ui.current_font_size() / 18.0;
    let size = 32.0 * scale;
    let position = ui.cursor_screen_pos();
    let clicked = ui.invisible_button(label, [size, size]);
    let hovered = ui.is_item_hovered();
    let draw = ui.get_window_draw_list();
    let center = [position[0] + size * 0.5, position[1] + size * 0.5];
    let color = if selected {
        [1.0; 4]
    } else {
        [0.84, 0.88, 0.90, 1.0]
    };
    let background = if selected {
        [0.0, 0.59, 0.75, 1.0]
    } else if hovered {
        [0.43, 0.49, 0.53, 0.96]
    } else {
        [0.37, 0.41, 0.44, 0.70]
    };
    draw.add_rect(
        position,
        [position[0] + size, position[1] + size],
        background,
    )
    .filled(true)
    .rounding(3.0 * scale)
    .build();
    widgets::draw_symbol(&draw, symbol, center, color, 10.0 * scale);
    if hovered {
        ui.tooltip_text(label);
    }
    clicked
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
