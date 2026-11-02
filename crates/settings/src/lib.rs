//! Persisted settings, defaults and validation.

use serde::{Deserialize, Serialize};

fn http_origin() -> String {
    "http://127.0.0.1:8099".to_owned()
}
fn stt_origin() -> String {
    http_origin()
}
fn tts_origin() -> String {
    http_origin()
}
fn alloy() -> String {
    "alloy".to_owned()
}
fn am_adam() -> String {
    "am_adam".to_owned()
}
fn am_echo() -> String {
    "am_echo".to_owned()
}
fn brisk() -> Delivery {
    Delivery::Brisk
}
fn quiet() -> Congestion {
    Congestion::Quiet
}
fn imperial() -> Units {
    Units::Imperial
}

/// TTS delivery preset. Speeds and pauses are resolved by the speech service.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Delivery {
    /// Measured readback pace.
    Standard,
    /// Brisk ATC delivery, the default.
    #[default]
    Brisk,
    /// Clipped urgent delivery for emergencies and go-arounds.
    Urgent,
}

/// How flight numbers are spoken in callsigns.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CallsignNumbers {
    /// Australia/FAA group form: 160 is "one sixty", 1300 is "thirteen hundred".
    Group,
    /// Digit-by-digit: 160 is "one six zero". This is the default.
    #[default]
    Digits,
}

/// How often the generated traffic is drawn.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TrafficRate {
    /// Draw on every simulator frame.
    #[default]
    EveryFrame,
    /// Draw on every n-th simulator frame.
    EveryFrames(u8),
    /// Draw at a target rate in hertz.
    Hertz(u8),
}

/// Generated-traffic diagnostics written to the engine log.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrafficLog {
    /// Silent.
    Off,
    /// Publication, phase changes, gate counts and every refusal.
    #[default]
    Summary,
    /// Per-aircraft decisions, routes and clearance state.
    Detail,
}

impl TrafficRate {
    /// The draw interval in seconds, or zero when the rate is frame based.
    #[must_use]
    pub fn interval(self) -> f32 {
        match self {
            Self::EveryFrame | Self::EveryFrames(_) => 0.0,
            Self::Hertz(hertz) => 1.0 / f32::from(hertz.clamp(5, 60)),
        }
    }
    /// The number of frames between draws, or zero when the rate is time based.
    #[must_use]
    pub fn frames(self) -> u32 {
        match self {
            Self::EveryFrame => 1,
            Self::EveryFrames(frames) => u32::from(frames.clamp(1, 30)),
            Self::Hertz(_) => 0,
        }
    }
}

/// Frequency-congestion simulation level.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Congestion {
    /// Replies return immediately.
    Off,
    /// Occasional short delays.
    #[default]
    Quiet,
    /// Longer delays with standby calls and background chatter.
    Busy,
}

/// Display and speech units. Imperial is the aviation standard.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Units {
    /// Feet, knots, nautical miles.
    #[default]
    Imperial,
    /// Meters, km/h, kilometers.
    Metric,
    /// Local procedure per departure region.
    Region,
}

/// Discipline rules for a request. Defaults reproduce the classic controller.
/// Collected here so core logic takes one small parameter instead of six.
/// Field order and names mirror the flat settings file.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Realism {
    #[serde(default = "yes")]
    /// Readbacks must match altitude, route and sequence exactly.
    pub strict_readbacks: bool,
    /// Monitor accepted instructions and issue corrections for unresolved deviations.
    #[serde(default)]
    pub enforce_clearance_constraints: bool,
    #[serde(default)]
    /// Answer service requests only on the assigned COM1 frequency.
    pub require_frequency: bool,
    #[serde(default)]
    /// Free-text requests must include your callsign.
    pub require_callsign: bool,
    #[serde(default = "yes")]
    /// Answer nonstandard phrasing with a correction.
    pub strict_phraseology: bool,
    #[serde(default = "yes")]
    /// Corrections explain the rule briefly.
    pub teaching_corrections: bool,
    #[serde(default)]
    /// Enable mayday practice flows.
    pub practice_emergencies: bool,
}

fn yes() -> bool {
    true
}

impl Default for Realism {
    fn default() -> Self {
        Self {
            strict_readbacks: true,
            enforce_clearance_constraints: false,
            require_frequency: false,
            require_callsign: false,
            strict_phraseology: true,
            teaching_corrections: true,
            practice_emergencies: false,
        }
    }
}

/// Settings serialized with the existing camelCase JSON keys.
/// New fields go at the end of their section so diffs stay readable.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// CPU worker limit for scenery and traffic jobs; zero chooses a conservative automatic limit.
    pub cpu_workers: u8,
    /// Offer optional en-route shortcuts; restriction checks still apply.
    pub atc_shortcut_offers: bool,
    /// Percent chance of offering an eligible shortcut.
    pub atc_shortcut_chance_percent: u8,
    /// Allow new discretionary arrival holds; existing clearances and safety checks remain active.
    pub player_traffic_holding: bool,
    /// Let `OpenATC` set the native simulator AI count; other providers keep ownership.
    pub manage_native_traffic: bool,
    /// Suppress `X-Plane` AI while `OpenATC` is connected; other plugins retain ownership.
    pub suppress_native_traffic: bool,
    /// Mute the simulator's own radio audio while `OpenATC` is connected. Turn this off when
    /// another plugin, such as a pushback helper, speaks through the radio channel.
    pub mute_native_radio: bool,
    /// Native AI aircraft excluding ownship, limited to nineteen SDK slots.
    pub native_traffic_count: u8,
    /// Simulated airport radio chatter when flying above a receivable station.
    #[serde(default = "yes")]
    pub airport_chatter: bool,
    /// Enable `OpenATC`'s lightweight generated model instances.
    pub generated_traffic: bool,
    /// How often the generated traffic is drawn. A higher rate moves the traffic more smoothly
    /// and costs more of the simulator's frame time.
    pub traffic_rate: TrafficRate,
    /// Generated-traffic diagnostics in the engine log.
    pub traffic_log: TrafficLog,
    /// Add a local pattern aircraft and departures when the network released little traffic.
    pub local_pattern_traffic: bool,
    /// Start the first released flight on approach instead of at its stand.
    pub start_first_flight_on_approach: bool,
    /// Active mainline-airline budget.
    pub airline_traffic: u8,
    /// Active regional/turboprop budget.
    pub regional_traffic: u8,
    /// Active freight budget.
    pub cargo_traffic: u8,
    /// Active private/GA budget.
    pub ga_traffic: u8,
    /// Minimum generated aircraft moving on the ground when traffic is full.
    pub traffic_taxi_min: u8,
    /// Maximum generated aircraft moving on the ground when traffic is full.
    pub traffic_taxi_max: u8,
    /// Keep an airport alive this far from an airliner flying a local circuit.
    pub traffic_circuit_jet_nm: f64,
    /// Keep an airport alive this far from a turboprop flying a local circuit.
    pub traffic_circuit_turboprop_nm: f64,
    /// Keep an airport alive this far from a light aircraft flying a local circuit.
    pub traffic_circuit_ga_nm: f64,
    /// Wake-separation table: auto, icao, recat_eu, faa_cwt or mats.
    pub wake_standard: String,
    /// Allow departure wake-separation waits.
    #[serde(default = "yes")]
    pub allow_wake_waits: bool,
    /// Speak simulated pilot reports based on observed local traffic movements.
    pub hear_traffic_radio: bool,
    /// Allow conversations with simulated pilots for identified traffic.
    pub talk_to_traffic: bool,
    /// Allow brief informal exchanges with identified simulated pilots.
    pub informal_pilot_chat: bool,
    /// Allow restrained humour in informal pilot replies.
    pub pilot_chat_humour: bool,
    /// Let nearby simulated pilots initiate a quiet-flight conversation.
    pub pilots_call_me: bool,
    /// Percent chance of a pilot call after checking weather, phase and radio availability.
    pub pilot_chat_chance_percent: u8,
    /// Numeric `SimBrief` pilot ID for OFP import.
    pub simbrief_id: String,
    /// Default pilot callsign used when a flight plan does not provide one.
    pub callsign: String,
    /// Spoken style for flight numbers in callsigns.
    pub callsign_numbers: CallsignNumbers,
    /// X-Plane installation folder override; empty means auto-detect.
    pub simulator_root: String,
    #[serde(default = "http_origin")]
    /// Bare origin serving OpenAI-compatible chat completions.
    pub ai_url: String,
    /// Classifier model name; empty disables AI features.
    pub ai_model: String,
    #[serde(default = "stt_origin")]
    /// Bare origin serving OpenAI-compatible transcriptions.
    pub stt_url: String,
    #[serde(default = "whisper")]
    /// Transcription model name sent with uploads.
    pub stt_model: String,
    #[serde(default = "tts_origin")]
    /// Bare origin serving OpenAI-compatible speech synthesis.
    pub tts_url: String,
    #[serde(default = "tts")]
    /// Speech model name sent with synthesis requests.
    pub tts_model: String,
    #[serde(default = "alloy")]
    /// Fallback controller voice when the pool is empty.
    pub voice: String,
    #[serde(default = "am_echo")]
    /// Copilot readback voice.
    pub copilot_voice: String,
    /// Comma-separated controller voices; new airspaces draw from it.
    pub voice_pool: String,
    /// Restrict voice selection and playback to English-language voices.
    pub english_voices_only: bool,
    #[serde(default = "brisk")]
    /// Default delivery preset for assigned controllers.
    pub controller_delivery: Delivery,
    #[serde(default = "brisk")]
    /// Delivery preset for copilot readbacks.
    pub copilot_delivery: Delivery,
    #[serde(default = "am_adam")]
    /// Voice reading back your own transmissions when enabled.
    pub pilot_voice: String,
    /// ATIS voice; empty uses the default controller voice.
    pub atis_voice: String,
    /// Steady ATIS reading speed, independent of random controller speeds.
    pub atis_speed: f32,
    /// Add the electronic ATIS timbre to synthesized speech.
    pub atis_electronic: bool,
    /// Custom copilot persona for AI readbacks; empty uses the fixed template.
    pub copilot_personality: String,
    /// Attendant-call dataref; empty means the panel never routes to cabin.
    pub attendant_ref: String,
    /// Ground-crew-call dataref; empty means the panel never routes to ground.
    pub ground_ref: String,
    #[serde(default = "sky")]
    /// Cabin crew voice.
    pub attendant_voice: String,
    #[serde(default = "george")]
    /// Ground crew voice.
    pub ground_voice: String,
    /// Cabin persona for AI crew chat; empty uses the fixed template.
    pub attendant_personality: String,
    /// Ground persona for AI crew chat; empty uses the fixed template.
    pub ground_personality: String,
    /// Cabin intercom volume.
    pub attendant_volume: f32,
    /// Ground interphone volume.
    pub ground_volume: f32,
    #[serde(default = "yes")]
    /// Show the copilot talk button beside Transmit.
    pub copilot_button: bool,
    #[serde(default)]
    /// Show the second radio panel when COM2 is available.
    pub show_com2: bool,
    /// Developer mode: verbose errors and error copying.
    pub dev_mode: bool,
    #[serde(default = "quiet")]
    /// Simulated frequency congestion level.
    pub congestion: Congestion,
    #[serde(default = "imperial")]
    /// Display and speech units; imperial is the standard.
    pub units: Units,
    /// Lower bound of the per-controller speed draw.
    pub controller_speed_min: f32,
    /// Upper bound of the per-controller speed draw.
    pub controller_speed_max: f32,
    /// Copilot speech speed multiplier.
    pub copilot_speed: f32,
    /// Own-transmission speech speed multiplier.
    pub pilot_speed: f32,
    /// Own-transmission playback volume.
    pub pilot_volume: f32,
    /// Baked-in radio hiss bed, 0 to 1.
    pub radio_hiss: f32,
    /// Baked-in crackle impulse density, 0 to 1.
    pub radio_crackle: f32,
    /// Baked-in static burst level, 0 to 1.
    pub radio_static: f32,
    /// Microphone key from the device list; empty is system default.
    pub input_device: String,
    /// Playback device key; empty is system default.
    pub output_device: String,
    /// Use AI intent classification for free-form requests.
    pub ai_enabled: bool,
    /// Allow validated AI paraphrases of controller templates.
    pub llm_phrase_variety: bool,
    /// Copilot voices clearance readbacks.
    pub copilot_replies: bool,
    /// Copilot writes assigned COM1 frequencies.
    pub copilot_tunes: bool,
    /// Copilot retunes on ATC handoffs specifically (subset of tuning above).
    pub auto_tune_handoff: bool,
    /// Copilot answers routine ATC calls with readbacks and parrots.
    pub copilot_auto_respond: bool,
    /// Copilot sets autopilot targets (altitude, heading, speed) when the
    /// autopilot is engaged, announcing each change; advises only otherwise.
    pub copilot_flies_ap: bool,
    /// Speak controller transmissions automatically.
    pub controller_speech: bool,
    /// Speak copilot readbacks automatically.
    pub copilot_speech: bool,
    /// Speak your own transmitted requests.
    pub pilot_speech: bool,
    /// Draw each controller delivery from all presets.
    pub randomize_delivery: bool,
    /// Bake radio effects into synthesized speech.
    pub radio_effects: bool,
    /// Limit synthesized speech to telephone bandwidth.
    pub radio_bandpass: bool,
    /// Radio effects on the cabin intercom (off: cabin is not radio).
    pub radio_fx_cabin: bool,
    /// Radio effects on the ground interphone (off: the jack is wired).
    pub radio_fx_ground: bool,
    #[serde(default = "yes")]
    /// Readbacks must match altitude, route and sequence exactly.
    pub strict_readbacks: bool,
    /// Warn about deviations from accepted clearances and restrictions.
    pub enforce_clearance_constraints: bool,
    /// Ask about inability or assistance after repeated corrections. The saved key is retained.
    #[serde(default = "yes")]
    pub terminate_after_warnings: bool,
    /// Compare the observed transponder code and mode with the clearance.
    pub monitor_transponder: bool,
    /// Warn about runway entry, takeoff or landing without permission.
    pub monitor_runway_permissions: bool,
    /// Offer help after a sustained deviation from the approved taxi route.
    pub monitor_taxi_route: bool,
    #[serde(default)]
    /// Answer service requests only on the assigned COM1 frequency.
    pub require_frequency: bool,
    #[serde(default)]
    /// Free-text requests must include your callsign.
    pub require_callsign: bool,
    #[serde(default = "yes")]
    /// Answer nonstandard phrasing with a correction.
    pub strict_phraseology: bool,
    #[serde(default = "yes")]
    /// Corrections explain the rule briefly.
    pub teaching_corrections: bool,
    #[serde(default)]
    /// Enable mayday practice flows.
    pub practice_emergencies: bool,
    /// Keep the ATC window open when idle.
    pub pin_open: bool,
    /// Draw taxiway geometry on the airport map.
    pub show_taxiways: bool,
    /// Draw parking positions on the airport map.
    pub show_parking: bool,
    /// Draw the approved taxi route.
    pub show_taxi_route: bool,
    /// Direction arrows on the plugin taxi map.
    pub show_taxi_arrows: bool,
    /// Illuminated taxi guidance objects in the simulator.
    pub show_ground_taxi_arrows: bool,
    /// Highlight the authorised holding point on the map and simulator ground.
    pub show_holding_point: bool,
    /// Debug: ring each generated aircraft's next hold, before a crossing or at the
    /// departure runway.
    pub show_traffic_holding_points: bool,
    /// Debug: colour traffic on the taxi map by movement state and draw blocker links.
    pub show_traffic_debug: bool,
    /// Display the simulator's follow-me vehicle on the approved taxi route.
    pub show_follow_me: bool,
    /// ILS approach and glideslope volumes on the airport map.
    pub show_ils_beams: bool,
    /// Draw your aircraft on maps.
    pub show_ownship: bool,
    /// Draw map labels.
    pub show_labels: bool,
    /// Draw the planned descent profile.
    pub show_planned_descent: bool,
    /// Draw the current-trajectory descent line.
    pub show_current_descent: bool,
    /// Master playback volume.
    pub master_volume: f32,
    /// Controller speech volume.
    pub controller_volume: f32,
    /// Copilot speech volume.
    pub copilot_volume: f32,
    /// Microphone input gain multiplier.
    pub input_gain: f32,
    /// Master speech speed multiplier.
    pub tts_speed: f32,
    /// Idle seconds before the ATC window fades.
    pub fade_delay: f32,
    /// Faded window background opacity.
    pub faded_opacity: f32,
    /// Use dark text and light controls instead of the default dark palette.
    pub light_theme: bool,
    /// Expanded window background opacity. Text and controls remain opaque.
    pub window_opacity: f32,
    /// Saved floating plugin bounds: left, top, right, bottom.
    pub window_bounds: Option<[i32; 4]>,
    /// ATC transcript share of the split panel.
    pub transcript_fraction: f32,
    /// Interface text and layout scale.
    pub ui_scale: f32,
}

fn whisper() -> String {
    "whisper-1".to_owned()
}
fn sky() -> String {
    "af_sky".to_owned()
}
fn george() -> String {
    "bm_george".to_owned()
}
fn tts() -> String {
    "tts-1".to_owned()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cpu_workers: 0,
            atc_shortcut_offers: false,
            atc_shortcut_chance_percent: 2,
            player_traffic_holding: true,
            manage_native_traffic: false,
            suppress_native_traffic: true,
            mute_native_radio: true,
            native_traffic_count: 0,
            airport_chatter: true,
            generated_traffic: false,
            traffic_rate: TrafficRate::EveryFrame,
            traffic_log: TrafficLog::Summary,
            local_pattern_traffic: false,
            start_first_flight_on_approach: false,
            airline_traffic: 8,
            regional_traffic: 4,
            cargo_traffic: 2,
            ga_traffic: 5,
            traffic_taxi_min: 4,
            traffic_taxi_max: 6,
            traffic_circuit_jet_nm: 30.0,
            traffic_circuit_turboprop_nm: 20.0,
            traffic_circuit_ga_nm: 10.0,
            wake_standard: "auto".into(),
            allow_wake_waits: true,
            hear_traffic_radio: true,
            talk_to_traffic: false,
            informal_pilot_chat: false,
            pilot_chat_humour: false,
            pilots_call_me: false,
            pilot_chat_chance_percent: 2,
            // Services.
            simbrief_id: String::new(),
            callsign: "VH-BIL".to_owned(),
            callsign_numbers: CallsignNumbers::Digits,
            simulator_root: String::new(),
            ai_url: http_origin(),
            ai_model: "qwen2.5-7b".to_owned(),
            stt_url: stt_origin(),
            stt_model: whisper(),
            tts_url: tts_origin(),
            tts_model: tts(),
            // Voices.
            voice: alloy(),
            copilot_voice: am_echo(),
            voice_pool: String::new(),
            english_voices_only: true,
            controller_delivery: Delivery::Brisk,
            copilot_delivery: Delivery::Brisk,
            pilot_voice: am_adam(),
            atis_voice: String::new(),
            atis_speed: 1.0,
            atis_electronic: true,
            copilot_personality: String::new(),
            attendant_ref: String::new(),
            ground_ref: String::new(),
            attendant_voice: sky(),
            ground_voice: george(),
            attendant_personality: String::new(),
            ground_personality: String::new(),
            attendant_volume: 0.8,
            ground_volume: 0.8,
            copilot_button: true,
            show_com2: false,
            dev_mode: false,
            // Speeds (multiplied by the master tts_speed, clamped 0.5..2).
            controller_speed_min: 0.9,
            controller_speed_max: 1.15,
            copilot_speed: 1.0,
            pilot_speed: 1.0,
            // Radio FX levels.
            radio_hiss: 0.12,
            radio_crackle: 0.08,
            radio_static: 0.08,
            // Devices.
            input_device: String::new(),
            output_device: String::new(),
            // Toggles.
            ai_enabled: true,
            llm_phrase_variety: false,
            copilot_replies: true,
            copilot_tunes: true,
            auto_tune_handoff: true,
            copilot_auto_respond: true,
            copilot_flies_ap: false,
            controller_speech: true,
            copilot_speech: true,
            pilot_speech: true,
            randomize_delivery: false,
            radio_effects: true,
            radio_bandpass: true,
            radio_fx_cabin: false,
            radio_fx_ground: false,
            // Discipline (Standard preset).
            strict_readbacks: true,
            enforce_clearance_constraints: false,
            terminate_after_warnings: true,
            monitor_transponder: false,
            monitor_runway_permissions: false,
            monitor_taxi_route: false,
            require_frequency: false,
            require_callsign: false,
            strict_phraseology: true,
            teaching_corrections: true,
            practice_emergencies: false,
            congestion: Congestion::Quiet,
            units: Units::Imperial,
            // Window and map display.
            pin_open: true,
            show_taxiways: true,
            show_parking: true,
            show_taxi_route: true,
            show_taxi_arrows: true,
            show_ground_taxi_arrows: false,
            show_holding_point: false,
            show_traffic_holding_points: false,
            show_traffic_debug: false,
            show_follow_me: false,
            show_ils_beams: false,
            show_ownship: true,
            show_labels: true,
            show_planned_descent: true,
            show_current_descent: true,
            // Audio levels.
            master_volume: 0.8,
            controller_volume: 1.0,
            copilot_volume: 0.8,
            pilot_volume: 0.8,
            input_gain: 1.0,
            tts_speed: 1.0,
            fade_delay: 8.0,
            faded_opacity: 0.4,
            light_theme: false,
            window_opacity: 0.9,
            window_bounds: None,
            transcript_fraction: 0.57,
            ui_scale: 1.0,
        }
    }
}

impl Settings {
    /// Discipline rules collected from the flat settings fields.
    #[must_use]
    pub fn realism(&self) -> Realism {
        Realism {
            strict_readbacks: self.strict_readbacks,
            enforce_clearance_constraints: self.enforce_clearance_constraints,
            require_frequency: self.require_frequency,
            require_callsign: self.require_callsign,
            strict_phraseology: self.strict_phraseology,
            teaching_corrections: self.teaching_corrections,
            practice_emergencies: self.practice_emergencies,
        }
    }

    /// Split a comma-separated voice pool, trimming entries and skipping empties.
    pub fn voice_pool(&self) -> Vec<String> {
        self.voice_pool
            .split(',')
            .map(str::trim)
            .filter(|voice| !voice.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Validate settings before saving or applying them.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !(0.2..=0.8).contains(&self.transcript_fraction) {
            return Err(ValidationError::OutOfRange(
                "transcript fraction",
                self.transcript_fraction,
            ));
        }
        if let Some([left, top, right, bottom]) = self.window_bounds {
            let width = i64::from(right) - i64::from(left);
            let height = i64::from(top) - i64::from(bottom);
            if ![left, top, right, bottom]
                .iter()
                .all(|value| (-100_000..=100_000).contains(value))
                || !(442..=4800).contains(&width)
                || !(153..=3200).contains(&height)
            {
                #[allow(clippy::cast_precision_loss)]
                let label = width as f32;
                return Err(ValidationError::OutOfRange("window bounds", label));
            }
        }

        let callsign = self.callsign.trim();
        if !callsign.is_empty()
            && (callsign.len() > 30
                || !callsign
                    .bytes()
                    .all(|value| value.is_ascii_alphanumeric() || value == b'-' || value == b' '))
        {
            return Err(ValidationError::BadCallsign);
        }
        if self.traffic_taxi_min > self.traffic_taxi_max || self.traffic_taxi_max > 16 {
            return Err(ValidationError::OutOfRange(
                "taxi traffic target",
                f32::from(self.traffic_taxi_max),
            ));
        }
        for (name, value) in [
            ("jet circuit radius", self.traffic_circuit_jet_nm),
            (
                "turboprop circuit radius",
                self.traffic_circuit_turboprop_nm,
            ),
            ("GA circuit radius", self.traffic_circuit_ga_nm),
        ] {
            if !value.is_finite() || !(1.0..=200.0).contains(&value) {
                return Err(ValidationError::OutOfRange(name, value as f32));
            }
        }
        if self.cpu_workers > 16 {
            return Err(ValidationError::OutOfRange(
                "CPU workers",
                f32::from(self.cpu_workers),
            ));
        }
        if self.native_traffic_count > 19 {
            return Err(ValidationError::OutOfRange(
                "native traffic count",
                f32::from(self.native_traffic_count),
            ));
        }
        let generated_count = [
            self.airline_traffic,
            self.regional_traffic,
            self.cargo_traffic,
            self.ga_traffic,
        ]
        .into_iter()
        .map(u16::from)
        .sum::<u16>();
        if generated_count > 63 {
            return Err(ValidationError::OutOfRange(
                "generated traffic count",
                f32::from(generated_count),
            ));
        }
        if self.atc_shortcut_chance_percent > 100 {
            return Err(ValidationError::OutOfRange(
                "ATC shortcut chance",
                f32::from(self.atc_shortcut_chance_percent),
            ));
        }
        if self.pilot_chat_chance_percent > 100 {
            return Err(ValidationError::OutOfRange(
                "pilot chat chance",
                f32::from(self.pilot_chat_chance_percent),
            ));
        }
        for url in [&self.ai_url, &self.stt_url, &self.tts_url] {
            let rest = url
                .strip_prefix("http://")
                .or_else(|| url.strip_prefix("https://"))
                .unwrap_or("");
            if rest.is_empty()
                || rest.contains(['/', ' ', '\t'])
                || !(url.starts_with("http://") || url.starts_with("https://"))
            {
                return Err(ValidationError::BadUrl(url.clone()));
            }
        }
        for (name, value) in [
            ("master volume", self.master_volume),
            ("controller volume", self.controller_volume),
            ("copilot volume", self.copilot_volume),
            ("pilot volume", self.pilot_volume),
            ("attendant volume", self.attendant_volume),
            ("ground volume", self.ground_volume),
        ] {
            if !(0.0..=1.0).contains(&value) {
                return Err(ValidationError::OutOfRange(name, value));
            }
        }
        for (name, value) in [
            ("controller minimum speed", self.controller_speed_min),
            ("controller maximum speed", self.controller_speed_max),
            ("copilot speed", self.copilot_speed),
            ("pilot speed", self.pilot_speed),
            ("ATIS speed", self.atis_speed),
        ] {
            if !(0.5..=2.0).contains(&value) {
                return Err(ValidationError::OutOfRange(name, value));
            }
        }
        if self.controller_speed_min > self.controller_speed_max {
            return Err(ValidationError::InvertedSpeedRange);
        }
        if !(0.25..=4.0).contains(&self.tts_speed)
            || !(0.0..=4.0).contains(&self.input_gain)
            || !(0.85..=1.5).contains(&self.ui_scale)
        {
            return Err(ValidationError::OutOfRange("audio/UI scale", 0.0));
        }
        if !(0.2..=1.0).contains(&self.window_opacity) {
            return Err(ValidationError::OutOfRange(
                "window opacity",
                self.window_opacity,
            ));
        }
        for (name, value) in [
            ("hiss", self.radio_hiss),
            ("crackle", self.radio_crackle),
            ("static", self.radio_static),
        ] {
            if !(0.0..=1.0).contains(&value) {
                return Err(ValidationError::OutOfRange(name, value));
            }
        }
        if self.copilot_personality.len() > 2000
            || self.attendant_personality.len() > 2000
            || self.ground_personality.len() > 2000
        {
            return Err(ValidationError::PersonalityTooLong);
        }
        for reference in [&self.attendant_ref, &self.ground_ref] {
            if reference.len() > 256
                || !reference
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '/')
            {
                return Err(ValidationError::BadRef(reference.clone()));
            }
        }
        for voice in self.voice_pool() {
            if !voice
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(ValidationError::BadVoice(voice));
            }
        }
        Ok(())
    }
}

/// Settings validation errors.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ValidationError {
    /// URL is not a bare http(s) origin.
    #[error("Service URLs must be http(s) origins, without /v1 paths: {0}")]
    BadUrl(String),
    /// Numeric setting outside its valid band.
    #[error("Setting out of range: {0} ({1})")]
    OutOfRange(&'static str, f32),
    /// Controller speed minimum exceeds its maximum.
    #[error("Controller minimum speed must not exceed maximum speed.")]
    InvertedSpeedRange,
    /// Personality text exceeds the engine limit.
    #[error("Copilot personality too long.")]
    PersonalityTooLong,
    /// Voice pool entry is not a plain voice name.
    #[error("Voice pool entries must be voice names separated by commas: {0}")]
    BadVoice(String),
    /// Crew dataref is not a dataref name.
    #[error("Crew datarefs must be dataref names: {0}")]
    BadRef(String),
    /// Default callsign has unsupported characters or is too long.
    #[error(
        "Default callsign must be 30 characters or fewer using letters, digits, spaces or hyphens."
    )]
    BadCallsign,
}

#[cfg(test)]
mod tests {
    #[test]
    fn default_callsign_validation_rejects_bad_identities() {
        let mut settings = super::Settings::default();
        assert!(settings.validate().is_ok());
        settings.callsign = "VH/BIL".into();
        assert!(settings.validate().is_err());
        settings.callsign = "X".repeat(31);
        assert!(settings.validate().is_err());
    }

    #[test]
    fn default_callsign_is_configured_and_round_trips() {
        let defaults = super::Settings::default();
        assert_eq!(defaults.callsign, "VH-BIL");
        let text = serde_json::to_string(&defaults).unwrap();
        let parsed: super::Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.callsign, "VH-BIL");
        let custom = super::Settings {
            callsign: "QFA123".into(),
            ..defaults
        };
        let text = serde_json::to_string(&custom).unwrap();
        let parsed: super::Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.callsign, "QFA123");
    }

    use super::*;

    #[test]
    fn defaults_are_valid() {
        let settings = Settings::default();
        settings.validate().unwrap();
        let missing: Settings = serde_json::from_str("{}").unwrap();
        for value in [settings, missing] {
            assert_eq!(value.ai_url, "http://127.0.0.1:8099");
            assert_eq!(value.stt_url, value.ai_url);
            assert_eq!(value.tts_url, value.ai_url);
            assert_eq!(value.ai_model, "qwen2.5-7b");
            assert!(
                value.ai_enabled
                    && value.controller_speech
                    && value.copilot_speech
                    && value.pilot_speech
            );
            assert!(
                value.copilot_replies
                    && value.copilot_tunes
                    && value.auto_tune_handoff
                    && value.copilot_auto_respond
            );
            assert!(value.hear_traffic_radio);
            assert!(
                !value.copilot_flies_ap
                    && !value.generated_traffic
                    && !value.pilots_call_me
                    && !value.informal_pilot_chat
            );
            assert!(
                value.pilot_volume > 0.0
                    && value.copilot_volume > 0.0
                    && value.attendant_volume > 0.0
                    && value.ground_volume > 0.0
            );
        }
    }

    #[test]
    fn saved_service_addresses_and_speech_choices_survive_new_defaults() {
        let saved = serde_json::json!({
            "aiUrl":"http://remote-model:11434", "sttUrl":"http://remote-stt:8000",
            "ttsUrl":"http://remote-voice:8001", "aiModel":"custom-model",
            "aiEnabled":false, "controllerSpeech":false, "copilotSpeech":false,
            "pilotSpeech":false, "copilotReplies":false, "copilotTunes":false,
            "hearTrafficRadio":false
        });
        let settings: Settings = serde_json::from_value(saved.clone()).unwrap();
        settings.validate().unwrap();
        let persisted = serde_json::to_value(settings).unwrap();
        for (key, value) in saved.as_object().unwrap() {
            assert_eq!(&persisted[key], value, "Saved {key} changed");
        }
    }

    #[test]
    fn retired_appearance_keys_do_not_reset_service_or_voice_settings() {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "frostedBackground": true, "glassBlur": 0.8, "glassFrost": 0.7,
            "ttsUrl": "http://192.168.1.10:8099", "sttUrl": "http://192.168.1.10:8099",
            "aiUrl": "http://192.168.1.10:11434", "pilotVoice": "am_adam",
            "lightTheme": true, "windowOpacity": 0.65
        }))
        .unwrap();
        settings.validate().unwrap();
        let saved = serde_json::to_value(&settings).unwrap();
        assert_eq!(settings.tts_url, "http://192.168.1.10:8099");
        assert_eq!(settings.ai_url, "http://192.168.1.10:11434");
        assert_eq!(settings.pilot_voice, "am_adam");
        assert!(settings.light_theme);
        for key in ["frostedBackground", "glassBlur", "glassFrost"] {
            assert!(saved.get(key).is_none());
        }
    }

    #[test]
    fn old_files_load_with_defaults() {
        let settings: Settings =
            serde_json::from_str(r#"{"voice":"alloy","ttsSpeed":1.0}"#).unwrap();
        assert_eq!(settings.voice, "alloy");
        assert_eq!(settings.controller_delivery, Delivery::Brisk);
        assert_eq!(settings.congestion, Congestion::Quiet);
        assert_eq!(settings.units, Units::Imperial);
        assert!(settings.attendant_ref.is_empty());
        assert!(settings.ground_ref.is_empty());
        assert!(settings.copilot_button);
        assert!(!settings.show_com2);
        assert!(settings.atis_voice.is_empty());
        assert!((settings.atis_speed - 1.0).abs() < f32::EPSILON);
        assert!(settings.atis_electronic);
        let com2: Settings = serde_json::from_str(r#"{"showCom2":true}"#).unwrap();
        assert!(com2.show_com2);
        assert_eq!(serde_json::to_value(&com2).unwrap()["showCom2"], true);
        assert!(settings.show_taxi_arrows);
        assert!(!settings.show_ground_taxi_arrows);
        assert!(!settings.show_holding_point);
        assert!(!settings.dev_mode);
        assert!(!settings.light_theme);
        let edited: Settings =
            serde_json::from_str(r#"{"devMode":true,"lightTheme":true,"windowOpacity":0.55}"#)
                .unwrap();
        assert!(edited.dev_mode && edited.light_theme);
        assert_eq!(
            serde_json::to_value(&edited).unwrap()["windowOpacity"],
            serde_json::json!(0.55_f32)
        );
        assert!(
            Settings {
                window_opacity: f32::NAN,
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
        assert!(settings.realism().strict_readbacks);
        settings.validate().unwrap();
    }

    #[test]
    fn units_accept_all_modes() {
        let settings: Settings = serde_json::from_str(r#"{"units":"metric"}"#).unwrap();
        assert_eq!(settings.units, Units::Metric);
        let settings: Settings = serde_json::from_str(r#"{"units":"region"}"#).unwrap();
        assert_eq!(settings.units, Units::Region);
    }

    #[test]
    fn rejects_bad_urls() {
        let settings = Settings {
            ai_url: "http://127.0.0.1:11434/v1".to_owned(),
            ..Settings::default()
        };
        assert!(matches!(
            settings.validate(),
            Err(ValidationError::BadUrl(_))
        ));
    }

    #[test]
    fn rejects_inverted_speed_range() {
        let settings = Settings {
            controller_speed_min: 1.5,
            controller_speed_max: 1.0,
            ..Settings::default()
        };
        assert_eq!(
            settings.validate(),
            Err(ValidationError::InvertedSpeedRange)
        );
    }

    #[test]
    fn rejects_bad_pool_entry() {
        let settings = Settings {
            voice_pool: "af_bella, not a voice!".to_owned(),
            ..Settings::default()
        };
        assert!(matches!(
            settings.validate(),
            Err(ValidationError::BadVoice(_))
        ));
    }

    #[test]
    fn pool_parsing_trims_and_skips_empties() {
        let settings = Settings {
            voice_pool: "af_bella, am_adam ,, ".to_owned(),
            ..Settings::default()
        };
        assert_eq!(settings.voice_pool(), ["af_bella", "am_adam"]);
    }
}

#[cfg(test)]
mod layout_tests {
    #[test]
    fn layout_survives_settings_round_trip_and_old_settings_keep_defaults() {
        let mut settings = super::Settings {
            window_bounds: Some([150, 1000, 1110, 240]),
            transcript_fraction: 0.4,
            player_traffic_holding: false,
            ..super::Settings::default()
        };
        settings.validate().unwrap();
        let restored: super::Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.window_bounds, settings.window_bounds);
        assert!(!restored.player_traffic_holding);
        assert!((restored.transcript_fraction - 0.4).abs() < f32::EPSILON);
        let old: super::Settings = serde_json::from_str("{}").unwrap();
        assert!(old.player_traffic_holding);
        assert_eq!(old.window_bounds, None);
        assert!((old.transcript_fraction - 0.57).abs() < f32::EPSILON);
        // Native minimum dimensions scale down with the supported 85% UI scale.
        settings.ui_scale = 0.85;
        settings.window_bounds = Some([30, 1000, 472, 847]);
        settings.validate().unwrap();
        settings.window_bounds = Some([30, 1000, 471, 847]);
        assert!(settings.validate().is_err());
        settings.transcript_fraction = f32::NAN;
        assert!(settings.validate().is_err());
    }
    #[test]
    fn missing_draw_rate_uses_frames_and_saved_hertz_is_preserved() {
        use super::{Settings, TrafficRate};
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.traffic_rate, TrafficRate::EveryFrame);
        let settings: Settings = serde_json::from_str(r#"{"trafficRate":{"hertz":20}}"#).unwrap();
        assert_eq!(settings.traffic_rate, TrafficRate::Hertz(20));
        let saved = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.traffic_rate, TrafficRate::Hertz(20));
    }
}
