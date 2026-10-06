# openatc `rust/` workspace (edition 2024)

Crates:

- `crates/settings` — engine settings schema, mirrors the C++ JSON keys
  exactly (camelCase), plus validation. Old files load via defaults.
- `crates/platform` — all OS-dependent code (`linux`/`macos`/`windows`
  modules): data directories, service installation strings, GPU detection.
- `crates/ai-core` — model manifest (`models.toml`), verified paths,
  resumable downloads with hash lockfile.

Conventions: `cargo fmt --check` and `cargo clippy -- -D warnings` must pass;
workspace lints forbid `unsafe` except in crates that opt out explicitly
(FFI crates will). Models download to `~/.local/share/openatc-ai/models`
(`OPENATC_AI_MODELS` overrides); see `models.toml` for pins.

Build: `cargo build` / `cargo test` from this directory.
