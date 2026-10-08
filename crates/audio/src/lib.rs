//! Microphone capture, metering, WAV encoding and speech playback.
//! Capture is 16 kHz mono; playback is 48 kHz stereo.
//! The miniaudio adapter owns native audio objects behind opaque pointers.

#![allow(unsafe_code)]

use std::ffi::{c_char, c_float, c_int, c_short, c_uchar, c_uint, c_ulonglong, c_void};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

/// Opaque miniaudio context owned by the C shim.
enum MaContext {}

/// Opaque stream handle owned by the C shim.
enum MaStream {}

/// Opaque decoder owned by the C shim.
enum MaDecoder {}

unsafe extern "C" {
    fn oatc_context_new() -> *mut MaContext;
    fn oatc_context_free(context: *mut MaContext);
    fn oatc_context_init(context: *mut MaContext) -> c_int;
    fn oatc_capture_list(context: *mut MaContext, names: *mut [c_char; 256], max: c_int) -> c_int;
    fn oatc_playback_list(context: *mut MaContext, names: *mut [c_char; 256], max: c_int) -> c_int;
    fn oatc_capture_open(
        context: *mut MaContext,
        index: c_int,
        callback: unsafe extern "C" fn(*mut c_void, *const c_short, c_uint),
        ud: *mut c_void,
    ) -> *mut MaStream;
    fn oatc_render_open(
        context: *mut MaContext,
        index: c_int,
        callback: unsafe extern "C" fn(*mut c_void, *mut c_float, c_uint),
        ud: *mut c_void,
    ) -> *mut MaStream;
    fn oatc_stream_start(handle: *mut MaStream) -> c_int;
    fn oatc_stream_stop(handle: *mut MaStream) -> c_int;
    fn oatc_stream_close(handle: *mut MaStream);
    fn oatc_decode_open(data: *const c_uchar, length: c_ulonglong) -> *mut MaDecoder;
    fn oatc_decode_read(
        decoder: *mut MaDecoder,
        out: *mut c_float,
        frames: c_ulonglong,
    ) -> c_ulonglong;
    fn oatc_decode_total(decoder: *mut MaDecoder) -> c_ulonglong;
    fn oatc_decode_close(decoder: *mut MaDecoder);
}

/// Maximum devices listed per direction.
pub const MAX_DEVICES: usize = 32;

/// Named audio device with the snapshot `name #index` key.
#[derive(Clone, Debug)]
pub struct AudioDevice {
    /// Snapshot key (`name #index`), empty for system default.
    pub key: String,
    /// Human device name.
    pub name: String,
}

/// Capture format: 16 kHz mono 16-bit, 30 seconds maximum.
pub const CAPTURE_HZ: u32 = 16_000;
/// Maximum capture samples (30 seconds).
pub const MAX_SAMPLES: usize = 480_000;
/// Minimum bytes for a transcription attempt.
pub const MIN_WAVE_BYTES: usize = 3200;

/// Shared capture state written by the audio callback.
#[derive(Default)]
struct Capture {
    samples: Vec<i16>,
    rms: f32,
    peak: f32,
    gain: f32,
    full: bool,
}

/// Shared playback state read by the audio callback.
struct Playback {
    frames: Vec<f32>,
    position: usize,
    volume: f32,
}

/// FFI device cap as C int. 32 is exact in any int width.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
fn max_list() -> c_int {
    MAX_DEVICES as c_int
}

/// Turn one C enumeration into keyed devices, dropping empties.
fn names_to_devices(names: &[[c_char; 256]], count: c_int) -> Vec<AudioDevice> {
    let mut devices = Vec::new();
    for (index, name) in names
        .iter()
        .enumerate()
        .take(usize::try_from(count.max(0)).unwrap_or(0))
    {
        let bytes: Vec<u8> = name
            .iter()
            .take_while(|value| **value != 0)
            .map(|value| (*value).cast_unsigned())
            .collect();
        let name = String::from_utf8_lossy(&bytes).into_owned();
        if name.is_empty() {
            continue;
        }
        devices.push(AudioDevice {
            key: format!("{name} #{index}"),
            name,
        });
    }
    devices
}

/// Pure metering math shared by the callback and the tests: RMS with decay
/// against the previous value, peak hold, and the capped sample push.
fn meter_samples(previous_rms: f32, samples: &[i16], gain: f32) -> (f32, f32, Vec<i16>) {
    let mut sum = 0.0;
    let mut peak = 0.0f32;
    let mut kept = Vec::with_capacity(samples.len().min(MAX_SAMPLES));
    for sample in samples {
        let level = (f32::from(*sample) / 32768.0 * gain).clamp(-1.0, 1.0);
        sum += level * level;
        peak = peak.max(level.abs());
        if kept.len() < MAX_SAMPLES {
            // Clamped above, so this is exact.
            #[allow(clippy::cast_possible_truncation)]
            let quantized = (level * 32767.0) as i16;
            kept.push(quantized);
        }
    }
    // Callback chunks are hundreds of frames; exactly representable.
    #[allow(clippy::cast_precision_loss)]
    let count = samples.len().max(1) as f32;
    ((sum / count).sqrt().max(previous_rms * 0.85), peak, kept)
}

/// One speech job for the audio engine.
pub struct SpeakRequest<'a> {
    /// Text to speak.
    pub text: &'a str,
    /// Engine base URL for synthesis.
    pub engine_endpoint: &'a str,
    /// Role index (0 ATC, 1 copilot, 2 pilot, 3 cabin, 4 ground).
    pub role: i32,
    /// Playback volume.
    pub volume: f32,
    /// Voice id.
    pub voice: &'a str,
    /// Speech speed.
    pub speed: f32,
    /// Urgent delivery.
    pub urgent: bool,
    /// Delivery preset.
    pub delivery: &'a str,
}

/// Audio recording, playback and status for the interface.
pub struct Speech {
    context: *mut MaContext,
    capture: Arc<Mutex<Capture>>,
    gain: f32,
    master_volume: f32,
    pending_volume: f32,
    input_key: String,
    output_key: String,
    recording: bool,
    monitor_only: bool,
    capture_handle: *mut MaStream,
    capture_ud: *mut c_void,
    playing: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    busy_job: Arc<AtomicBool>,
    jobs: Mutex<Vec<std::thread::JoinHandle<()>>>,
    transcript: Arc<Mutex<String>>,
    status: Arc<Mutex<String>>,
    cached_inputs: Vec<AudioDevice>,
    cached_outputs: Vec<AudioDevice>,
}

impl Default for Speech {
    fn default() -> Self {
        Self::new()
    }
}

impl Speech {
    /// Create the speech engine without touching audio backends. The
    /// miniaudio context starts on first use (refresh or record) in a
    /// timeout-guarded thread, so a stuck backend can never freeze plugin
    /// load: worst case the panel reports audio unavailable.
    #[must_use]
    pub fn new() -> Self {
        Self {
            context: std::ptr::null_mut(),
            capture: Arc::new(Mutex::new(Capture {
                gain: 1.0,
                ..Capture::default()
            })),
            gain: 1.0,
            master_volume: 0.8,
            pending_volume: 1.0,
            input_key: String::new(),
            output_key: String::new(),
            recording: false,
            monitor_only: false,
            capture_handle: std::ptr::null_mut(),
            capture_ud: std::ptr::null_mut(),
            playing: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            busy_job: Arc::new(AtomicBool::new(false)),
            jobs: Mutex::new(Vec::new()),
            transcript: Arc::new(Mutex::new(String::new())),
            status: Arc::new(Mutex::new("Microphone off".to_owned())),
            cached_inputs: Vec::new(),
            cached_outputs: Vec::new(),
        }
    }

    /// Ensure a live backend context, guarded by a timeout. Returns false
    /// with a status line when backends will not start.
    fn ensure_context(&mut self) -> bool {
        if !self.context.is_null() {
            return true;
        }
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let context = unsafe {
                let context = oatc_context_new();
                if !context.is_null() && oatc_context_init(context) != 0 {
                    oatc_context_free(context);
                    std::ptr::null_mut()
                } else {
                    context
                }
            };
            if done_tx.send(context as usize).is_err() && !context.is_null() {
                unsafe { oatc_context_free(context) };
            }
        });
        match done_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(bits) if bits != 0 => {
                self.context = bits as *mut MaContext;
                true
            }
            _ => {
                self.set_status("Audio unavailable");
                false
            }
        }
    }

    /// Whether the audio engine is live.
    #[must_use]
    pub fn live(&self) -> bool {
        !self.context.is_null()
    }

    /// Apply device and level settings.
    pub fn configure(&mut self, input: &str, output: &str, gain: f32, volume: f32) {
        self.gain = gain.clamp(0.0, 4.0);
        if let Ok(mut capture) = self.capture.lock() {
            capture.gain = self.gain;
        }
        self.master_volume = volume.clamp(0.0, 1.0);
        if input != self.input_key && !self.recording {
            input.clone_into(&mut self.input_key);
        }
        if !self.playing.load(Ordering::SeqCst) {
            output.clone_into(&mut self.output_key);
        }
    }

    /// Whether a background transcription or synthesis job runs, or audio plays.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.busy_job.load(Ordering::SeqCst) || self.playing.load(Ordering::SeqCst)
    }

    /// Whether a capture is running for transcription.
    #[must_use]
    pub fn recording(&self) -> bool {
        self.recording && !self.monitor_only
    }

    /// Whether the mic test monitor runs.
    #[must_use]
    pub fn monitoring(&self) -> bool {
        self.recording && self.monitor_only
    }

    /// RMS input level in dBFS.
    #[must_use]
    pub fn input_db(&self) -> f32 {
        let rms = self.capture.lock().map_or(0.0, |capture| capture.rms);
        20.0 * rms.max(0.000_001).log10()
    }

    /// Meter level 0..1 over a -60 dBFS floor.
    #[must_use]
    pub fn input_level(&self) -> f32 {
        ((self.input_db() + 60.0) / 60.0).clamp(0.0, 1.0)
    }

    /// Whether the input clips.
    #[must_use]
    pub fn clipping(&self) -> bool {
        self.capture
            .lock()
            .is_ok_and(|capture| capture.peak >= 0.99)
    }

    /// Human status line for the record button.
    #[must_use]
    pub fn status(&self) -> String {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default()
    }

    fn set_status(&self, text: &str) {
        if let Ok(mut status) = self.status.lock() {
            text.clone_into(&mut status);
        }
    }

    /// Split a `name #index` key back into its parts.
    fn split_key(key: &str) -> Option<(&str, c_int)> {
        let (name, index) = key.rsplit_once(" #")?;
        Some((name, index.parse().ok()?))
    }

    /// Reject stale settings instead of opening an unrelated device by index.
    fn selected_index(key: &str, devices: &[AudioDevice]) -> Result<c_int, &'static str> {
        if key.is_empty() {
            return Ok(-1);
        }
        devices
            .iter()
            .find(|device| device.key == key)
            .and_then(|device| Self::split_key(&device.key))
            .map(|(_, index)| index)
            .ok_or("Selected audio device is unavailable. Refresh devices and select it again.")
    }

    /// List input devices with the stable keys. Cached: refreshed only by
    /// `refresh_devices`, never per frame.
    #[must_use]
    pub fn input_devices(&self) -> Vec<AudioDevice> {
        let mut devices = vec![AudioDevice {
            key: String::new(),
            name: "System default".to_owned(),
        }];
        devices.extend(self.cached_inputs.iter().cloned());
        devices
    }

    /// List output devices with the stable keys. Cached like the inputs.
    #[must_use]
    pub fn output_devices(&self) -> Vec<AudioDevice> {
        let mut devices = vec![AudioDevice {
            key: String::new(),
            name: "System default".to_owned(),
        }];
        devices.extend(self.cached_outputs.iter().cloned());
        devices
    }

    /// Read one enumeration of capture names into Rust strings.
    fn enumerate_inputs(&self) -> Result<Vec<AudioDevice>, &'static str> {
        if self.context.is_null() {
            return Err("Audio unavailable");
        }
        let mut names = vec![[0 as c_char; 256]; MAX_DEVICES];
        let count = unsafe { oatc_capture_list(self.context, names.as_mut_ptr(), max_list()) };
        if count < 0 {
            Err("Cannot enumerate audio devices")
        } else {
            Ok(names_to_devices(&names, count))
        }
    }

    /// Read one enumeration of playback names into Rust strings.
    fn enumerate_outputs(&self) -> Result<Vec<AudioDevice>, &'static str> {
        if self.context.is_null() {
            return Err("Audio unavailable");
        }
        let mut names = vec![[0 as c_char; 256]; MAX_DEVICES];
        let count = unsafe { oatc_playback_list(self.context, names.as_mut_ptr(), max_list()) };
        if count < 0 {
            Err("Cannot enumerate audio devices")
        } else {
            Ok(names_to_devices(&names, count))
        }
    }

    /// Refresh the cached device lists. This is the ONLY enumeration point;
    /// the pickers read the cache so normal frames never touch backends.
    pub fn refresh_devices(&mut self) {
        if self.busy() || self.recording {
            return;
        }
        if !self.ensure_context() {
            return;
        }
        let inputs = self.enumerate_inputs();
        let outputs = self.enumerate_outputs();
        if let (Ok(inputs), Ok(outputs)) = (inputs, outputs) {
            self.cached_inputs = inputs;
            self.cached_outputs = outputs;
            self.set_status("Audio devices refreshed");
        } else {
            self.cached_inputs.clear();
            self.cached_outputs.clear();
            self.set_status("Cannot enumerate audio devices");
        }
    }

    /// Begin a capture (or a monitor-only mic test).
    pub fn start_recording(&mut self, monitor_only: bool) {
        if self.busy() || self.recording {
            return;
        }
        if !self.ensure_context() {
            return;
        }
        if let Ok(mut locked) = self.capture.lock() {
            locked.samples.clear();
            locked.samples.reserve(MAX_SAMPLES);
            locked.rms = 0.0;
            locked.peak = 0.0;
            locked.gain = self.gain;
            locked.full = false;
        }
        let index = match Self::selected_index(&self.input_key, &self.cached_inputs) {
            Ok(index) => index,
            Err(error) => {
                self.set_status(error);
                return;
            }
        };
        let ud = Arc::into_raw(self.capture.clone()) as *mut c_void;
        let handle = unsafe { oatc_capture_open(self.context, index, capture_callback, ud) };
        if handle.is_null() {
            unsafe { drop(Arc::from_raw(ud as *const Mutex<Capture>)) };
            self.set_status("Cannot open selected microphone");
            return;
        }
        if unsafe { oatc_stream_start(handle.cast()) } != 0 {
            unsafe {
                oatc_stream_close(handle.cast());
                drop(Arc::from_raw(ud as *const Mutex<Capture>));
            }
            self.set_status("Cannot start microphone");
            return;
        }
        self.capture_handle = handle.cast();
        self.capture_ud = ud;
        self.monitor_only = monitor_only;
        self.recording = true;
        self.set_status(if monitor_only {
            "Microphone test (not sent)"
        } else {
            "Recording (maximum 30 seconds)"
        });
    }

    fn stop_capture(&mut self) {
        if !self.capture_handle.is_null() {
            unsafe {
                oatc_stream_stop(self.capture_handle);
                oatc_stream_close(self.capture_handle);
            }
            self.capture_handle = std::ptr::null_mut();
        }
        if !self.capture_ud.is_null() {
            unsafe { drop(Arc::from_raw(self.capture_ud as *const Mutex<Capture>)) };
            self.capture_ud = std::ptr::null_mut();
        }
        self.recording = false;
        self.monitor_only = false;
    }

    /// Stop the mic test monitor.
    pub fn stop_monitoring(&mut self) {
        self.stop_capture();
        self.set_status("Microphone off");
    }

    /// Encode the current capture for transcription.
    fn capture_wave(&self) -> Result<Vec<u8>, String> {
        let locked = self
            .capture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if locked.samples.len() * 2 < MIN_WAVE_BYTES {
            return Err("Recording too short".to_owned());
        }
        Ok(build_wave(&locked.samples))
    }

    /// Send the capture for transcription on a worker thread.
    pub fn transcribe(&mut self, engine_endpoint: &str) {
        if !self.recording || self.monitor_only || self.busy() {
            return;
        }
        self.stop_capture();
        let wave = match self.capture_wave() {
            Ok(wave) => wave,
            Err(error) => {
                self.set_status(&error);
                return;
            }
        };
        self.set_status("Transcribing...");
        let endpoint = engine_endpoint.to_owned();
        self.spawn_transcribe(endpoint, wave);
    }

    /// Synthesize and play one entry on a worker thread.
    pub fn speak(&mut self, request: &SpeakRequest<'_>) {
        if self.busy() || self.recording {
            return;
        }
        if !self.ensure_context() {
            return;
        }
        if let Err(error) = Self::selected_index(&self.output_key, &self.cached_outputs) {
            self.set_status(error);
            return;
        }
        self.pending_volume = request.volume.clamp(0.0, 1.0);
        self.set_status("Generating speech...");
        let speaker = match request.role {
            4 => "ground",
            3 => "cabin",
            2 => "pilot",
            1 => "copilot",
            _ => "atc",
        };
        let mut body = serde_json::json!({
            "text": request.text,
            "speaker": speaker,
            "delivery": request.delivery,
            "urgent": request.urgent,
        });
        if !request.voice.is_empty() {
            body["voice"] = request.voice.into();
        }
        if request.speed > 0.0 {
            body["speed"] = request.speed.into();
        }
        self.spawn_speak(request.engine_endpoint.to_owned(), body);
    }

    /// Pump background jobs: the full-recording notice lands here.
    pub fn poll(&mut self) {
        if self.recording {
            let full = self.capture.lock().is_ok_and(|capture| capture.full);
            if full {
                self.set_status("30 seconds recorded. Select Transcribe to send.");
            }
        }
    }

    /// Take a finished transcript, if any.
    #[must_use]
    pub fn take_transcript(&mut self) -> String {
        if let Ok(mut transcript) = self.transcript.lock() {
            std::mem::take(&mut transcript)
        } else {
            String::new()
        }
    }

    fn spawn_transcribe(&self, endpoint: String, wave: Vec<u8>) {
        self.cancelled.store(false, Ordering::SeqCst);
        let cancelled = self.cancelled.clone();
        self.busy_job.store(true, Ordering::SeqCst);
        let transcript = self.transcript.clone();
        let status = self.status.clone();
        let busy = self.busy_job.clone();
        let job = std::thread::spawn(move || {
            match transcribe_wave(&endpoint, &wave, &cancelled) {
                Ok(text) => {
                    if let Ok(mut slot) = transcript.lock() {
                        text.clone_into(&mut slot);
                    }
                    if let Ok(mut slot) = status.lock() {
                        "Transcript ready - review and transmit".clone_into(&mut slot);
                    }
                }
                Err(error) => {
                    if let Ok(mut slot) = status.lock() {
                        error.clone_into(&mut slot);
                    }
                }
            }
            busy.store(false, Ordering::SeqCst);
        });
        self.track_job(job);
    }

    fn track_job(&self, job: std::thread::JoinHandle<()>) {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        jobs.retain(|job| !job.is_finished());
        jobs.push(job);
    }

    /// Cancel pending speech and interrupt playback within one audio polling interval.
    pub fn stop_speech(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn spawn_speak(&self, endpoint: String, body: serde_json::Value) {
        self.cancelled.store(false, Ordering::SeqCst);
        let cancelled = self.cancelled.clone();
        self.busy_job.store(true, Ordering::SeqCst);
        let context = self.context as usize;
        let output_key = self.output_key.clone();
        let playing = self.playing.clone();
        let status = self.status.clone();
        let busy = self.busy_job.clone();
        let gain = self.pending_volume;
        let job = std::thread::spawn(move || {
            playing.store(true, Ordering::SeqCst);
            let result = synthesize_and_play(
                context as *mut MaContext,
                &endpoint,
                &body,
                &output_key,
                gain,
                &cancelled,
            );
            if let Ok(mut slot) = status.lock() {
                if result.is_ok() {
                    "Speech complete".clone_into(&mut slot);
                } else if let Err(error) = &result {
                    error.clone_into(&mut slot);
                }
            }
            playing.store(false, Ordering::SeqCst);
            busy.store(false, Ordering::SeqCst);
        });
        self.track_job(job);
    }
}

impl Drop for Speech {
    fn drop(&mut self) {
        self.stop_capture();
        self.stop_speech();
        let jobs = self
            .jobs
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for job in jobs.drain(..) {
            let _ = job.join();
        }
        unsafe { oatc_context_free(self.context) };
    }
}

// Audio callbacks fire on miniaudio's threads through mutex-guarded state,
// and device creation uses the shared audio context.
// Everything else in here is behind mutexes or atomics.
unsafe impl Send for Speech {}

/// Capture callback: meter and stash 16-bit mono frames.
unsafe extern "C" fn capture_callback(ud: *mut c_void, samples: *const c_short, count: c_uint) {
    if ud.is_null() || samples.is_null() {
        return;
    }
    // Gain is baked into the pushed samples; the callback owns no state.
    let capture = unsafe { &*(ud as *const Mutex<Capture>) };
    let count = usize::try_from(count).unwrap_or(0);
    let frames = unsafe { std::slice::from_raw_parts(samples, count) };
    if let Ok(mut locked) = capture.lock() {
        let (rms, peak, kept) = meter_samples(locked.rms, frames, locked.gain);
        locked.rms = rms;
        locked.peak = peak.max(locked.peak);
        let room = MAX_SAMPLES - locked.samples.len().min(MAX_SAMPLES);
        locked.samples.extend(kept.into_iter().take(room));
        if locked.samples.len() >= MAX_SAMPLES {
            locked.full = true;
        }
    }
}

/// Render callback: copy decoded frames with volume, silence at the end.
unsafe extern "C" fn render_callback(ud: *mut c_void, out: *mut c_float, frames: c_uint) {
    if ud.is_null() || out.is_null() {
        return;
    }
    let playback = unsafe { &*(ud as *const Mutex<Playback>) };
    let frames = usize::try_from(frames).unwrap_or(0);
    let dest = unsafe { std::slice::from_raw_parts_mut(out, frames * 2) };
    if let Ok(mut locked) = playback.lock() {
        for sample in dest {
            *sample = if locked.position < locked.frames.len() {
                let value = locked.frames[locked.position] * locked.volume;
                locked.position += 1;
                value
            } else {
                0.0
            };
        }
    }
}

/// Encode mono 16 kHz samples as a PCM WAV file.
#[must_use]
pub fn build_wave(samples: &[i16]) -> Vec<u8> {
    let mut wave = Vec::with_capacity(44 + samples.len() * 2);
    wave.extend_from_slice(b"RIFF");
    append_u32(
        &mut wave,
        36 + u32::try_from(samples.len()).unwrap_or(0) * 2,
    );
    wave.extend_from_slice(b"WAVEfmt ");
    append_u32(&mut wave, 16);
    append_u16(&mut wave, 1);
    append_u16(&mut wave, 1);
    append_u32(&mut wave, CAPTURE_HZ);
    append_u32(&mut wave, CAPTURE_HZ * 2);
    append_u16(&mut wave, 2);
    append_u16(&mut wave, 16);
    wave.extend_from_slice(b"data");
    append_u32(&mut wave, u32::try_from(samples.len()).unwrap_or(0) * 2);
    for sample in samples {
        append_u16(&mut wave, u16::from_ne_bytes(sample.to_ne_bytes()));
    }
    wave
}

fn append_u16(wave: &mut Vec<u8>, value: u16) {
    wave.extend_from_slice(&value.to_le_bytes());
}

fn append_u32(wave: &mut Vec<u8>, value: u32) {
    wave.extend_from_slice(&value.to_le_bytes());
}

/// POST a WAV capture for transcription, returning the text.
fn transcribe_wave(endpoint: &str, wave: &[u8], cancelled: &AtomicBool) -> Result<String, String> {
    const FAILED: &str = "Transcription failed. Check engine STT settings.";
    let (status, reply) = openatc_http::post_bytes_cancelled(
        endpoint,
        "/speech/transcribe",
        "audio/wav",
        wave,
        std::time::Duration::from_secs(35),
        cancelled,
    )
    .map_err(|_| FAILED.to_owned())?;
    if status != 200 {
        return Err(FAILED.to_owned());
    }
    reply
        .get("text")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| FAILED.to_owned())
}

/// Request synthesis, decode the WAV answer to f32 stereo 48 kHz, and play it.
fn synthesize_and_play(
    context: *mut MaContext,
    endpoint: &str,
    payload: &serde_json::Value,
    output_key: &str,
    volume: f32,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    const FAILED: &str = "Speech synthesis failed. Check engine TTS settings.";
    let bytes = openatc_http::post_wav_cancelled(
        endpoint,
        "/speech/speak",
        payload,
        std::time::Duration::from_secs(35),
        cancelled,
    )
    .map_err(|_| FAILED.to_owned())?;
    if cancelled.load(Ordering::SeqCst) {
        return Ok(());
    }
    let mut frames = decode_wav(&bytes)?;
    if payload["delivery"].as_str() == Some("atis") {
        // Mild ring modulation adds the fixed electronic timbre of automated ATIS.
        let mut phase = 0.0_f32;
        for stereo in frames.chunks_mut(2) {
            let carrier = phase.sin();
            phase = (phase + std::f32::consts::TAU * 65.0 / 48000.0) % std::f32::consts::TAU;
            for sample in stereo {
                *sample *= 0.75 + 0.25 * carrier;
            }
        }
    }
    play_frames(context, output_key, &frames, volume, cancelled)
}

/// Decode any WAV answer to f32 stereo 48 kHz through the C decoder.
fn decode_wav(bytes: &[u8]) -> Result<Vec<f32>, String> {
    unsafe {
        let decoder = oatc_decode_open(bytes.as_ptr(), u64::try_from(bytes.len()).unwrap_or(0));
        if decoder.is_null() {
            return Err("TTS response is not decodable audio".to_owned());
        }
        let total = usize::try_from(oatc_decode_total(decoder)).unwrap_or(usize::MAX);
        let mut frames = vec![0.0f32; total.min(1 << 28) * 2];
        let mut read = 0;
        while read < frames.len() / 2 {
            let want = u64::try_from(frames.len() / 2 - read).unwrap_or(u64::MAX);
            let got = usize::try_from(oatc_decode_read(
                decoder,
                frames.as_mut_ptr().wrapping_add(read * 2),
                want,
            ))
            .unwrap_or(0);
            if got == 0 {
                break;
            }
            read += got;
        }
        frames.truncate(read * 2);
        oatc_decode_close(decoder);
        Ok(frames)
    }
}

/// Play stereo frames on the selected output, blocking until done.
fn play_frames(
    context: *mut MaContext,
    output_key: &str,
    frames: &[f32],
    volume: f32,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let index = if output_key.is_empty() {
        -1
    } else {
        Speech::split_key(output_key)
            .filter(|(_, index)| *index >= 0)
            .map(|(_, index)| index)
            .ok_or("Invalid output device selection")?
    };
    let playback = Arc::new(Mutex::new(Playback {
        frames: frames.to_vec(),
        position: 0,
        volume: volume.clamp(0.0, 1.0),
    }));
    let ud = Arc::into_raw(playback.clone()) as *mut c_void;
    let handle = unsafe { oatc_render_open(context, index, render_callback, ud) };
    if handle.is_null() {
        unsafe { drop(Arc::from_raw(ud as *const Mutex<Playback>)) };
        return Err("Cannot open selected output device".to_owned());
    }
    if unsafe { oatc_stream_start(handle.cast()) } != 0 {
        unsafe {
            oatc_stream_close(handle.cast());
            drop(Arc::from_raw(ud as *const Mutex<Playback>));
        }
        return Err("Cannot play speech".to_owned());
    }
    while !cancelled.load(Ordering::SeqCst)
        && playback
            .lock()
            .is_ok_and(|locked| locked.position < locked.frames.len())
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    unsafe {
        oatc_stream_stop(handle.cast());
        oatc_stream_close(handle.cast());
        drop(Arc::from_raw(ud as *const Mutex<Playback>));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_layout_matches_encoder() {
        let samples = vec![0i16, 32767, -32768, 1000];
        let wave = build_wave(&samples);
        assert_eq!(wave.len(), 44 + 8);
        assert_eq!(&wave[0..4], b"RIFF");
        assert_eq!(&wave[8..16], b"WAVEfmt ");
        assert_eq!(&wave[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wave[40..44].try_into().unwrap()), 8);
        assert_eq!(i16::from_le_bytes(wave[44..46].try_into().unwrap()), 0);
        assert_eq!(i16::from_le_bytes(wave[46..48].try_into().unwrap()), 32767);
        assert_eq!(u32::from_le_bytes(wave[4..8].try_into().unwrap()), 36 + 8);
    }

    #[test]
    fn metering_math_holds() {
        let (rms, peak, kept) = meter_samples(0.0, &[16384, -16384], 1.0);
        assert!((rms - 0.5).abs() < 0.01);
        assert!((peak - 0.5).abs() < 0.01);
        assert_eq!(kept, vec![16383, -16383]);
    }

    #[test]
    fn decode_round_trips_capture() {
        let samples = vec![1000i16; 48];
        let wave = build_wave(&samples);
        let frames = decode_wav(&wave).unwrap();
        // 16 kHz mono upsampled 3x to 48 kHz stereo.
        assert_eq!(frames.len(), samples.len() * 3 * 2);
        // Steady state past the resampler lead-in, both channels equal.
        let middle = frames.len() / 2;
        assert!((frames[middle] - 1000.0 / 32768.0).abs() < 0.002);
        assert!((frames[middle + 1] - 1000.0 / 32768.0).abs() < 0.002);
    }

    #[test]
    fn device_enumeration_never_panics() {
        // Headless machines may report zero devices; context creation and
        // enumeration itself must not fail.
        let speech = Speech::new();
        let _ = speech.input_devices();
        let _ = speech.output_devices();
    }

    #[test]
    fn refresh_enumerates_backends() {
        // Real backend probing, like the sim does on Refresh. Must not crash.
        let mut speech = Speech::new();
        speech.refresh_devices();
        eprintln!(
            "inputs={} outputs={} status={}",
            speech.input_devices().len(),
            speech.output_devices().len(),
            speech.status()
        );
    }

    #[test]
    fn playback_preserves_stereo_channels_and_duration() {
        let playback = Arc::new(Mutex::new(Playback {
            frames: vec![0.2, -0.4, 0.6, -0.8],
            position: 0,
            volume: 0.5,
        }));
        let mut output = [99.0_f32; 6];
        unsafe {
            render_callback(
                Arc::as_ptr(&playback) as *mut c_void,
                output.as_mut_ptr(),
                3,
            );
        };
        for (actual, expected) in output.iter().zip([0.1, -0.2, 0.3, -0.4, 0.0, 0.0]) {
            assert!((*actual - expected).abs() < f32::EPSILON);
        }
        assert_eq!(playback.lock().unwrap().position, 4);
    }

    #[test]
    fn configured_gain_reaches_capture_and_wave() {
        let mut speech = Speech::new();
        let samples = [8192_i16, -8192];
        let ud = Arc::as_ptr(&speech.capture) as *mut c_void;
        unsafe { capture_callback(ud, samples.as_ptr(), 2) };
        {
            let capture = speech.capture.lock().unwrap();
            assert!(capture.rms > 0.24);
            assert!(capture.samples.iter().all(|sample| *sample != 0));
        }
        speech.configure("", "", 2.0, 1.0);
        let samples = vec![8192_i16; 1600];
        unsafe { capture_callback(ud, samples.as_ptr(), 1600) };
        assert!(speech.input_level() > 0.8);
        let capture = speech.capture.lock().unwrap();
        assert!((capture.rms - 0.5).abs() < 0.01);
        assert_eq!(capture.samples[2], 16383);
        drop(capture);
        let wave = speech.capture_wave().unwrap();
        assert_eq!(i16::from_le_bytes(wave[48..50].try_into().unwrap()), 16383);
    }

    #[test]
    fn unloading_cancels_and_joins_workers_before_freeing_context() {
        let speech = Speech::new();
        let cancelled = speech.cancelled.clone();
        let exited = Arc::new(AtomicBool::new(false));
        let finished = exited.clone();
        speech.track_job(std::thread::spawn(move || {
            while !cancelled.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            finished.store(true, Ordering::SeqCst);
        }));
        drop(speech);
        assert!(exited.load(Ordering::SeqCst));
    }

    #[test]
    fn stale_selection_is_rejected() {
        let devices = vec![AudioDevice {
            key: "Headset #0".into(),
            name: "Headset".into(),
        }];
        assert_eq!(Speech::selected_index("", &devices), Ok(-1));
        assert_eq!(Speech::selected_index("Headset #0", &devices), Ok(0));
        assert!(Speech::selected_index("Old microphone #0", &devices).is_err());
        assert!(Speech::selected_index("Headset #1", &devices).is_err());
        assert!(Speech::selected_index("broken", &devices).is_err());
    }

    #[test]
    #[ignore = "requires a running audio server and microphone"]
    fn live_selected_devices_open() {
        let mut speech = Speech::new();
        speech.refresh_devices();
        for device in speech.input_devices() {
            eprintln!("INPUT: {}", device.name);
        }
        for device in speech.output_devices() {
            eprintln!("OUTPUT: {}", device.name);
        }
        let input = speech
            .cached_inputs
            .iter()
            .find(|d| !d.name.contains("Monitor"))
            .unwrap()
            .key
            .clone();
        let output = speech.cached_outputs.first().unwrap().key.clone();
        speech.configure(&input, &output, 1.0, 0.0);
        speech.start_recording(true);
        assert!(speech.monitoring(), "{}", speech.status());
        assert!(
            speech
                .cached_inputs
                .iter()
                .all(|device| !device.name.starts_with("Monitor of "))
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while speech.capture.lock().unwrap().samples.is_empty()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            !speech.capture.lock().unwrap().samples.is_empty(),
            "capture callback must deliver samples"
        );
        speech.stop_monitoring();
        play_frames(
            speech.context,
            &output,
            &vec![0.0; 9600],
            0.0,
            &AtomicBool::new(false),
        )
        .unwrap();
    }

    #[test]
    fn refresh_caches_devices() {
        let mut speech = Speech::new();
        speech.refresh_devices();
        // System default is always first, cache never panics.
        assert_eq!(
            speech
                .input_devices()
                .first()
                .map(|device| device.key.as_str()),
            Some("")
        );
        assert_eq!(
            speech
                .output_devices()
                .first()
                .map(|device| device.key.as_str()),
            Some("")
        );
    }
}
