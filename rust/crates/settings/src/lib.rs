//! Persisted engine settings, mirroring the C++ `Settings` schema field for field.
//!
//! Old files keep loading (every field has a default); unknown keys are ignored.

use serde::{Deserialize, Serialize};

fn http_origin() -> String {
    "http://127.0.0.1:11434".to_owned()
}
fn stt_origin() -> String {
    "http://127.0.0.1:8000".to_owned()
}
fn tts_origin() -> String {
    "http://127.0.0.1:8001".to_owned()
}
fn alloy() -> String {
    "alloy".to_owned()
}
fn am_adam() -> String {
    "am_adam".to_owned()
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
            require_frequency: false,
            require_callsign: false,
            strict_phraseology: true,
            teaching_corrections: true,
            practice_emergencies: false,
        }
    }
}

/// Full settings file. Field names and order mirror the C++ JSON keys.
/// New fields go at the end of their section so diffs stay readable.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Numeric `SimBrief` pilot ID for OFP import.
    pub simbrief_id: String,
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
    #[serde(default = "alloy")]
    /// Copilot readback voice.
    pub copilot_voice: String,
    /// Comma-separated controller voices; new airspaces draw from it.
    pub voice_pool: String,
    #[serde(default = "brisk")]
    /// Default delivery preset for assigned controllers.
    pub controller_delivery: Delivery,
    #[serde(default = "brisk")]
    /// Delivery preset for copilot readbacks.
    pub copilot_delivery: Delivery,
    #[serde(default = "am_adam")]
    /// Voice reading back your own transmissions when enabled.
    pub pilot_voice: String,
    /// Custom copilot persona for AI readbacks; empty uses the fixed template.
    pub copilot_personality: String,
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
    /// Copilot voices clearance readbacks.
    pub copilot_replies: bool,
    /// Copilot writes assigned COM1 frequencies.
    pub copilot_tunes: bool,
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
    /// Interface text and layout scale.
    pub ui_scale: f32,
}

fn whisper() -> String {
    "whisper-1".to_owned()
}
fn tts() -> String {
    "tts-1".to_owned()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // Services.
            simbrief_id: String::new(),
            simulator_root: String::new(),
            ai_url: http_origin(),
            ai_model: String::new(),
            stt_url: stt_origin(),
            stt_model: whisper(),
            tts_url: tts_origin(),
            tts_model: tts(),
            // Voices.
            voice: alloy(),
            copilot_voice: alloy(),
            voice_pool: String::new(),
            controller_delivery: Delivery::Brisk,
            copilot_delivery: Delivery::Brisk,
            pilot_voice: am_adam(),
            copilot_personality: String::new(),
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
            ai_enabled: false,
            copilot_replies: false,
            copilot_tunes: false,
            controller_speech: false,
            copilot_speech: false,
            pilot_speech: false,
            randomize_delivery: false,
            radio_effects: true,
            radio_bandpass: true,
            radio_fx_cabin: false,
            radio_fx_ground: false,
            // Discipline (Standard preset).
            strict_readbacks: true,
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

    /// Mirror of the C++ `validateSettings` rules.
    pub fn validate(&self) -> Result<(), ValidationError> {
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
        for (name, value) in [
            ("hiss", self.radio_hiss),
            ("crackle", self.radio_crackle),
            ("static", self.radio_static),
        ] {
            if !(0.0..=1.0).contains(&value) {
                return Err(ValidationError::OutOfRange(name, value));
            }
        }
        if self.copilot_personality.len() > 2000 {
            return Err(ValidationError::PersonalityTooLong);
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

/// Validation failures, mirroring the C++ error strings.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        Settings::default().validate().unwrap();
    }

    #[test]
    fn old_files_load_with_defaults() {
        let settings: Settings =
            serde_json::from_str(r#"{"voice":"alloy","ttsSpeed":1.0}"#).unwrap();
        assert_eq!(settings.voice, "alloy");
        assert_eq!(settings.controller_delivery, Delivery::Brisk);
        assert_eq!(settings.congestion, Congestion::Quiet);
        assert_eq!(settings.units, Units::Imperial);
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
