//! Embedded text-to-speech: text in, 24 kHz mono WAV bytes out.
//!
//! Pipeline (ported from the `kokoro-onnx` Python package, MIT, by
//! thewh1teagle — phoneme vocab vendored in `vocab.json` from its config):
//! text → phonemes (`misaki`, self-contained) → token ids → ONNX (`ort`,
//! prebuilt) with per-batch voice style → trim → pause → radio FX.
//!
//! Voice styles come from the same `voices-v1.0.bin` npz the gateway uses
//! (54 voices, `[510, 1, 256]` float32 each). The engine owns the session on
//! a single worker thread.

pub mod fx;

use std::collections::BTreeMap;
use std::path::Path;

/// Sample rate the kokoro model renders.
pub const SAMPLE_RATE: u32 = 24_000;
/// Longest phoneme run per inference, matching the Python pipeline.
pub const MAX_PHONEMES: usize = 510;
/// Longest accepted input, matching the engine-side limit.
pub const MAX_TEXT: usize = 4000;

/// `OpenAI` voice name to kokoro voice, mirroring the gateway map.
const VOICE_MAP: [(&str, &str); 6] = [
    ("alloy", "af_alloy"),
    ("echo", "am_echo"),
    ("fable", "bm_fable"),
    ("onyx", "am_onyx"),
    ("nova", "af_nova"),
    ("shimmer", "af_sky"),
];
/// Voice proven in local tests, mirroring the gateway default.
const DEFAULT_VOICE: &str = "af_bella";

/// Resolve a requested voice name to a kokoro voice id.
#[must_use]
pub fn resolve_voice(name: &str, available: &[String]) -> String {
    if name.is_empty() {
        return DEFAULT_VOICE.to_owned();
    }
    if let Some(mapped) = VOICE_MAP.iter().find(|(openai, _)| *openai == name) {
        return mapped.1.to_owned();
    }
    if available.iter().any(|voice| voice == name) {
        return name.to_owned();
    }
    DEFAULT_VOICE.to_owned()
}

/// Transcription engine bound to one ONNX file plus its voice table.
pub struct TtsEngine {
    session: ort::session::Session,
    use_input_ids: bool,
    speed_is_float: bool,
    voices: BTreeMap<String, Vec<f32>>,
    vocab: BTreeMap<String, i64>,
    g2p: misaki_rs::G2P,
}

impl TtsEngine {
    /// Load the model, voice table and phonemizer.
    ///
    /// # Errors
    ///
    /// Missing files, unreadable voices, or a session that cannot start.
    pub fn load(model_path: &Path, voices_path: &Path) -> Result<Self, String> {
        let session = ort::session::Session::builder()
            .map_err(|error| format!("cannot build session: {error}"))?
            .commit_from_file(model_path)
            .map_err(|error| format!("cannot load {}: {error}", model_path.display()))?;
        let names: Vec<String> = session
            .inputs()
            .iter()
            .map(|input| input.name().to_owned())
            .collect();
        let use_input_ids = names.iter().any(|name| name == "input_ids");
        let speed_is_float = session
            .inputs()
            .iter()
            .find(|input| input.name() == "speed")
            .is_none_or(|input| {
                !matches!(
                    input.dtype(),
                    ort::value::ValueType::Tensor {
                        ty: ort::value::TensorElementType::Int32,
                        ..
                    }
                )
            });
        let file = std::fs::File::open(voices_path)
            .map_err(|error| format!("cannot open {}: {error}", voices_path.display()))?;
        let mut archive = ndarray_npy::NpzReader::new(file)
            .map_err(|error| format!("unreadable voices: {error}"))?;
        let mut voices = BTreeMap::new();
        let names = archive
            .names()
            .map_err(|error| format!("unreadable voices: {error}"))?;
        for full in &names {
            let stem = full.strip_suffix(".npy").unwrap_or(full);
            if stem.is_empty() {
                continue;
            }
            let array: ndarray::Array3<f32> = archive
                .by_name(full)
                .map_err(|error| format!("bad voice {stem}: {error}"))?;
            if array.shape() != [510, 1, 256] {
                return Err(format!("bad voice {stem} shape: {:?}", array.shape()));
            }
            voices.insert(stem.to_owned(), array.into_raw_vec_and_offset().0);
        }
        if voices.is_empty() {
            return Err("no voices found".to_owned());
        }
        let vocab: BTreeMap<String, i64> = serde_json::from_str(include_str!("vocab.json"))
            .map_err(|error| format!("bad embedded vocab: {error}"))?;
        Ok(Self {
            session,
            use_input_ids,
            speed_is_float,
            voices,
            vocab,
            g2p: misaki_rs::G2P::new(misaki_rs::Language::EnglishUS),
        })
    }

    /// Voice ids available in the loaded table.
    #[must_use]
    pub fn voice_names(&self) -> Vec<String> {
        self.voices.keys().cloned().collect()
    }

    /// Render text to mono samples at [`SAMPLE_RATE`].
    ///
    /// Length casts below are safe: audio buffers never approach 2^52 samples.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    /// # Errors
    ///
    /// Empty or oversize input, unknown voice, phonemizer or inference failure.
    pub fn synthesize(
        &mut self,
        text: &str,
        voice: &str,
        speed: f32,
        sentence_pause: f32,
        clause_pause: f32,
    ) -> Result<Vec<f32>, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("missing input text".to_owned());
        }
        if text.len() > MAX_TEXT {
            return Err("speech text too long".to_owned());
        }
        let speed = speed.clamp(0.5, 2.0);
        let names: Vec<String> = self.voices.keys().cloned().collect();
        let voice = resolve_voice(voice, &names);
        let style = self
            .voices
            .get(&voice)
            .ok_or_else(|| format!("voice {voice} missing from table"))?
            .clone();
        let (phonemes, _) = self
            .g2p
            .g2p(text)
            .map_err(|error| format!("cannot phonemize: {error:?}"))?;
        let use_input_ids = self.use_input_ids;
        let speed_is_float = self.speed_is_float;
        let session = &mut self.session;
        let phoneme_batches = split_batches(&phonemes);
        let total = phoneme_batches.len();
        let mut out: Vec<f32> = Vec::new();
        for (index, batch) in phoneme_batches.iter().enumerate() {
            let ids = tokenize(&self.vocab, batch);
            if ids.is_empty() {
                continue;
            }
            let part = infer(session, use_input_ids, speed_is_float, &ids, &style, speed)?;
            out.extend_from_slice(&trim_silence(&part));
            if index + 1 < total {
                let pause = pause_after(batch, sentence_pause, clause_pause);
                let zeros = (pause.clamp(0.0, 1.0) * SAMPLE_RATE as f32) as usize;
                out.extend(std::iter::repeat_n(0.0, zeros));
            }
        }
        Ok(out)
    }
}

/// Seconds of silence after a batch, mirroring the gateway pause logic.
fn pause_after(batch: &str, sentence: f32, clause: f32) -> f32 {
    match batch.trim_end().chars().last() {
        Some(mark) if mark == '.' || mark == '!' || mark == '?' => sentence,
        Some(mark) if mark == ',' || mark == ';' || mark == ':' => clause,
        _ => 0.0,
    }
}

/// Map phonemes to token ids, dropping anything outside the vocab.
fn tokenize(vocab: &BTreeMap<String, i64>, phonemes: &str) -> Vec<i64> {
    phonemes
        .chars()
        .filter_map(|phoneme| vocab.get(&phoneme.to_string()).copied())
        .collect()
}

/// Run one batch through the model.
///
/// The speed cast below is safe: speed is clamped to 0.5..=2.0 first.
#[allow(clippy::cast_possible_truncation)]
fn infer(
    session: &mut ort::session::Session,
    use_input_ids: bool,
    speed_is_float: bool,
    ids: &[i64],
    style: &[f32],
    speed: f32,
) -> Result<Vec<f32>, String> {
    let row = ids.len().min(509);
    let (head, _) = style.as_chunks::<256>();
    let style_row: Vec<f32> = head
        .get(row)
        .ok_or_else(|| "voice table too short".to_owned())?
        .to_vec();
    let mut padded = vec![0i64];
    padded.extend_from_slice(ids);
    padded.push(0);
    let tokens = ndarray::Array2::from_shape_vec((1, padded.len()), padded)
        .map_err(|error| format!("cannot shape tokens: {error}"))?;
    let style_array = ndarray::Array2::from_shape_vec((1, 256), style_row)
        .map_err(|error| format!("cannot shape style: {error}"))?;
    let token_name = if use_input_ids { "input_ids" } else { "tokens" };
    let speed_value: ort::value::Value = if speed_is_float {
        let speed_array = ndarray::Array1::from_vec(vec![speed])
            .into_dimensionality::<ndarray::IxDyn>()
            .map_err(|error| format!("cannot shape speed: {error}"))?;
        ort::value::Tensor::from_array(speed_array)
            .map_err(|error| format!("bad speed: {error}"))?
            .into()
    } else {
        let speed_array = ndarray::Array1::from_vec(vec![speed as i32])
            .into_dimensionality::<ndarray::IxDyn>()
            .map_err(|error| format!("cannot shape speed: {error}"))?;
        ort::value::Tensor::from_array(speed_array)
            .map_err(|error| format!("bad speed: {error}"))?
            .into()
    };
    let outputs = session
        .run(ort::inputs![
            token_name => ort::value::Tensor::from_array(tokens).map_err(|error| format!("bad tokens: {error}"))?,
            "style" => ort::value::Tensor::from_array(style_array).map_err(|error| format!("bad style: {error}"))?,
            "speed" => speed_value
        ])
        .map_err(|error| format!("inference failed: {error}"))?;
    let samples = outputs[0]
        .try_extract_array::<f32>()
        .map(|view| view.iter().copied().collect())
        .map_err(|error| format!("bad audio output: {error}"))?;
    Ok(samples)
}

/// Encode mono f32 samples as 16-bit PCM WAV bytes at [`SAMPLE_RATE`].
///
/// The sample cast below is safe: values are clamped to [-1, 1] first.
#[allow(clippy::cast_possible_truncation)]
#[must_use]
pub fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    let data_bytes = u32::try_from(samples.len()).unwrap_or(u32::MAX) * 2;
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        let pcm = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
        bytes.extend_from_slice(&pcm.to_le_bytes());
    }
    bytes
}

/// Split phonemes at punctuation into runs of at most [`MAX_PHONEMES`].
/// Overlong runs without punctuation are hard-split (the Python pipeline
/// errors on those instead).
fn split_batches(phonemes: &str) -> Vec<String> {
    fn flush(current: &mut String, batches: &mut Vec<String>) {
        let trimmed = current.trim();
        if !trimmed.is_empty() {
            batches.push(trimmed.to_owned());
        }
        current.clear();
    }
    let mut batches = Vec::new();
    let mut current = String::new();
    for part in phonemes.split_inclusive(&[',', '.', '!', '?', ';'][..]) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if current.len() + part.len() + 1 >= MAX_PHONEMES {
            flush(&mut current, &mut batches);
        }
        if part.len() >= MAX_PHONEMES {
            for chunk in part.chars().collect::<Vec<_>>().chunks(MAX_PHONEMES - 1) {
                batches.push(chunk.iter().collect());
            }
            continue;
        }
        if ".,!?;".contains(part) {
            current.push_str(part);
        } else {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(part);
        }
    }
    flush(&mut current, &mut batches);
    batches
}

/// Drop leading/trailing near-silence (librosa-trim equivalent framing).
///
/// Length casts below are safe: audio buffers never approach 2^52 samples.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn trim_silence(samples: &[f32]) -> Vec<f32> {
    const FRAME: usize = 2048;
    const HOP: usize = 512;
    const FLOOR: f32 = 0.001; // -60 dBFS, matching librosa trim top_db=60
    if samples.is_empty() {
        return Vec::new();
    }
    let loud = |start: usize| {
        let end = (start + FRAME).min(samples.len());
        let energy: f32 = samples[start..end].iter().map(|s| s * s).sum();
        (energy / (end - start).max(1) as f32).sqrt() > FLOOR
    };
    let mut first = 0;
    while first < samples.len() && !loud(first) {
        first += HOP;
    }
    let mut last = samples.len();
    while last > first && !loud(last.saturating_sub(FRAME)) {
        last = last.saturating_sub(HOP);
    }
    samples[first.min(last)..last].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_map_mirrors_gateway() {
        let available = vec!["af_bella".to_owned(), "af_alloy".to_owned()];
        assert_eq!(resolve_voice("", &available), "af_bella");
        assert_eq!(resolve_voice("alloy", &available), "af_alloy");
        assert_eq!(resolve_voice("af_alloy", &available), "af_alloy");
        assert_eq!(resolve_voice("nobody", &available), "af_bella");
    }

    #[test]
    fn batches_split_at_punctuation() {
        let batches = split_batches("hello world. this is a test, indeed! yes?");
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0], "hello world. this is a test, indeed! yes?");
        let long = format!("{} {}", "mid-length sentence. ".repeat(30), "tail.");
        let batches = split_batches(&long);
        assert!(batches.len() > 1);
        for batch in &batches {
            assert!(batch.len() < MAX_PHONEMES);
        }
    }

    #[test]
    fn batches_cap_length() {
        let long = "word ".repeat(300);
        let batches = split_batches(&long);
        assert!(!batches.is_empty());
        for batch in &batches {
            assert!(batch.len() < MAX_PHONEMES);
        }
    }

    #[test]
    fn trim_keeps_speech_drops_silence() {
        let mut samples = vec![0.0; 3000];
        samples.extend(vec![0.5; 3000]);
        samples.extend(vec![0.0; 3000]);
        let trimmed = trim_silence(&samples);
        assert!(trimmed.len() < samples.len());
        assert!(trimmed.len() > 2000);
        assert!(trim_silence(&[]).is_empty());
    }

    #[test]
    fn wav_header_scans() {
        let bytes = wav_bytes(&[0.0, 0.5, -0.5]);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(bytes.len(), 44 + 3 * 2);
    }
}
