# OpenATC developer tools

Run these commands from the repository root. `cargo xtask` uses the locked workspace dependencies. It does not install Python or run Git commands.

| Command | Purpose |
| --- | --- |
| `cargo xtask build` | Build and test the plugin/engine, run the radio integration suite and audit Linux dependencies |
| `cargo xtask build preview` | Also build the desktop preview |
| `cargo xtask install-plugin "/path/to/X-Plane 12"` | Install release binaries and resources, with a recoverable backup |
| `cargo xtask install-ai /path/to/openatc-ai` | Install AI/STT binaries, shared libraries and eSpeak data under `bin/` |
| `cargo xtask package linux-x64` | Create the plugin ZIP and SHA-256 file under `packages/` |
| `cargo xtask collect-artifacts plugin` | Collect native plugin and engine outputs for CI |
| `cargo xtask collect-artifacts ai-server` | Collect AI/STT outputs, shared libraries and eSpeak data for CI |
| `cargo xtask audit-linux <binary>...` | Check Linux link dependencies and plugin exports |
| `cargo xtask test-radio <engine> speech` | Exercise radio roles, readbacks, taxi/crossings, weather, voices and flight resets |
| `cargo xtask test-crew <engine> speech` | Exercise crew controls, confirmations, compound requests and checklists |
| `cargo xtask test-speech <engine> speech` | Exercise regional speech examples and prompt selection |

Build the requested release binaries first. Close X-Plane before replacing an installed plugin. The installer checks the speech library before modifying the destination, preserves the installed `radio-stations.toml`, and disables old flat speech files as `.legacy` copies. Backups are stored under the operating system's application data directory in `openatc-ai/backups/`.

Integration suites launch the real engine with temporary scenery and settings, and a local fake model. They do not contact your configured AI provider or change your simulator installation. Child engines are stopped on success or failure. `xtask/fixtures/radio-apt.dat` is synthetic test scenery, not production airport data.

The plugin ZIP includes executable permissions, editable resources, maintained dependency sources and third-party license notices. AI artifacts include the eSpeak data required when moving the installation to another computer. Model weights remain separate downloads.

The old Python speech gateway, C++ font generator, standalone SDK downloader and outdated CMake-era engine test have been removed. Rust embeds the existing fonts and uses the vendored SDK bindings. The maintained integration checks are in Rust.

OpenATC's tooling and runtime do not require Python. The Intel Mac CI job still uses ONNX Runtime's upstream build script, which requires Python while building that third-party library.
