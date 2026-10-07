//! Embedded speech-to-text: WAV bytes in, transcript out.
//!
//! One ggml model (tiny today) via `whisper-rs`. The engine owns the context on
//! a single worker thread; this crate only parses, decodes and infers.
//! GPU-first by design: `Auto` takes CUDA when this crate is built with the
//! `cuda` feature, otherwise CPU. `Cpu` forces CPU either way.

use std::path::Path;

/// Which compute backend transcribes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SttBackend {
    /// CUDA when built with the `cuda` feature, else CPU.
    #[default]
    Auto,
    /// Always CPU, even with CUDA available.
    Cpu,
    /// Always CUDA; fails clearly without the `cuda` feature.
    Cuda,
}

/// Transcription engine bound to one ggml model file.
pub struct SttEngine {
    context: whisper_rs::WhisperContext,
    backend: &'static str,
}

impl SttEngine {
    /// Load the model file, selecting compute per `backend`.
    ///
    /// # Errors
    ///
    /// Missing/unreadable model, unsupported backend, or CUDA without support.
    pub fn load(path: &Path, backend: SttBackend) -> Result<Self, String> {
        let use_cuda = match backend {
            SttBackend::Cpu => false,
            SttBackend::Cuda => {
                if !cfg!(feature = "cuda") {
                    return Err(
                        "CUDA requested but this build lacks it; rebuild with --features cuda (needs the CUDA toolkit)"
                            .to_owned(),
                    );
                }
                true
            }
            SttBackend::Auto => cfg!(feature = "cuda"),
        };
        let params = whisper_rs::WhisperContextParameters::default();
        let context = whisper_rs::WhisperContext::new_with_params(path, params)
            .map_err(|error| format!("cannot load {}: {error}", path.display()))?;
        Ok(Self {
            context,
            backend: if use_cuda { "cuda" } else { "cpu" },
        })
    }

    /// Backend actually selected by [`SttEngine::load`].
    #[must_use]
    pub fn backend(&self) -> &'static str {
        self.backend
    }

    /// Decode 16 kHz mono s16 WAV bytes to normalized samples.
    ///
    /// # Errors
    ///
    /// Unreadable audio or the wrong format.
    pub fn decode_wav(bytes: &[u8]) -> Result<Vec<f32>, String> {
        let reader =
            hound::WavReader::new(bytes).map_err(|error| format!("unreadable WAV: {error}"))?;
        let spec = reader.spec();
        if spec.sample_rate != 16_000 || spec.channels != 1 {
            return Err(format!(
                "need 16 kHz mono WAV, got {} Hz {}ch",
                spec.sample_rate, spec.channels
            ));
        }
        reader
            .into_samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("unreadable samples: {error}"))
            .map(|samples| {
                samples
                    .into_iter()
                    .map(|sample| f32::from(sample) / 32768.0)
                    .collect()
            })
    }

    /// Decode 16 kHz mono s16 WAV bytes and transcribe them to text.
    ///
    /// # Errors
    ///
    /// Unreadable audio, wrong format, or an inference failure.
    pub fn transcribe_wav(&self, bytes: &[u8]) -> Result<String, String> {
        let samples = Self::decode_wav(bytes)?;
        if samples.is_empty() {
            return Ok(String::new());
        }
        let mut state = self
            .context
            .create_state()
            .map_err(|error| format!("cannot start inference: {error}"))?;
        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_n_threads(4);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        state
            .full(params, &samples)
            .map_err(|error| format!("inference failed: {error}"))?;
        let mut text = String::new();
        for segment in state.as_iter() {
            let piece = segment
                .to_str_lossy()
                .map_err(|error| format!("cannot read segment: {error}"))?;
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(piece);
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_bytes(samples: &[i16]) -> Vec<u8> {
        wav_bytes_with(samples, 1, 16_000)
    }

    fn wav_bytes_with(samples: &[i16], channels: u16, rate: u32) -> Vec<u8> {
        let data: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + u32::try_from(data.len()).unwrap()).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        bytes.extend_from_slice(&(channels * 2).to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(&data);
        bytes
    }

    fn stereo_bytes() -> Vec<u8> {
        wav_bytes_with(&[0, 0], 2, 44_100)
    }

    #[test]
    fn decodes_valid_wav() {
        let samples = SttEngine::decode_wav(&wav_bytes(&[0, 1000, -1000, 32767])).unwrap();
        assert_eq!(samples.len(), 4);
        assert!((samples[3] - 32767.0 / 32768.0).abs() < 1e-6);
    }

    #[test]
    fn rejects_garbage() {
        assert!(SttEngine::decode_wav(b"not audio").is_err());
        assert!(SttEngine::decode_wav(&[]).is_err());
    }

    #[test]
    fn rejects_wrong_format() {
        let error = SttEngine::decode_wav(&stereo_bytes()).unwrap_err();
        assert!(error.contains("44"), "unexpected error: {error}");
    }

    #[test]
    fn cuda_requires_the_feature() {
        if cfg!(feature = "cuda") {
            return;
        }
        assert!(
            SttEngine::load(std::path::Path::new("/nonexistent.bin"), SttBackend::Cuda).is_err()
        );
    }
}
