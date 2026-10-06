# openatc `rust/` workspace (edition 2024)

Crates:

- `crates/settings` — engine settings schema, mirrors the C++ JSON keys
  exactly (camelCase), plus validation. Old files load via defaults.
- `crates/platform` — all OS-dependent code (`linux`/`macos`/`windows`
  modules): data directories, service installation strings, GPU detection.
- `crates/ai-core` — model manifest (`models.toml`), verified paths,
  resumable downloads with hash lockfile.
- `crates/stt` — embedded speech-to-text (`whisper-rs`, ggml-tiny): WAV bytes
  in, transcript out, plus the `openatc-stt` sidecar binary. Own process
  because `whisper.cpp` and `llama.cpp` each vendor `ggml` and cannot link
  into one binary. GPU-first by design (`--stt-backend auto|cpu|cuda`;
  `cuda` needs a toolkit build, CPU is the fallback and the force option).
- `crates/tts` — embedded text-to-speech (kokoro ONNX via `ort`, phonemes via
  source-built espeak-ng, voice table from `voices-v1.0.bin`): text in,
  24 kHz WAV bytes out, plus the radio FX chain (`fx.rs`: bandpass, hiss,
  crackle, static — seeded, deterministic, perceptually matched to the old
  gateway rather than byte-identical). Zero system dependencies.
- `crates/core` — port of the C++ core logic (units first): differential-tested
  against C++ on the shared fixtures in `tests/fixtures/` — both sides must
  agree byte-for-byte.

Conventions: `cargo fmt --check` and `cargo clippy -- -D warnings` must pass;
workspace lints forbid `unsafe` except in crates that opt out explicitly
(FFI crates will). Models download to `~/.local/share/openatc-ai/models`
(`OPENATC_AI_MODELS` overrides); see `models.toml` for pins.

Build: `cargo build` / `cargo test` from this directory.
