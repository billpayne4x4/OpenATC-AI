# OpenATC root Cargo workspace (edition 2024)

Crates:

- `crates/settings` — engine settings schema, preserves the existing camelCase JSON keys, plus validation. Old files load via defaults.
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
- `crates/core` — controller and simulator data logic, regression-tested against the shared fixtures in `tests/fixtures/`. The earlier C++ implementation has been removed.

Conventions: `cargo fmt --check` and `cargo clippy -- -D warnings` must pass;
workspace lints forbid `unsafe` except in crates that opt out explicitly
(FFI crates will). Models download to `~/.local/share/openatc-ai/models`
(`OPENATC_AI_MODELS` overrides); see `models.toml` for pins.

Build: `cargo build` / `cargo test` from the repository root use the default core, engine, UI and plugin members. Optional desktop and AI services use explicit `-p` targets; `--workspace` includes every member and its native build requirements.

Audio device selection uses miniaudio's native backend priorities: Linux can
use PulseAudio (including PipeWire's PulseAudio compatibility server), with
ALSA as a fallback; Windows and macOS retain their native backends. JACK
probing is disabled. PulseAudio playback-monitor sources are excluded from
the microphone list using native monitor metadata (a guarded local change
in the vendored miniaudio header). Device names and native IDs are cached together when
**Refresh audio devices** is pressed, and streams open those cached IDs.
Saved `name #index` selections are snapshot keys: after hardware changes or
migration from an ALSA-only build, refresh and reselect an unavailable device.
**System default** follows the operating system's default routing.

Audio checks: `cargo test -p openatc-audio -p openatc-ui`. On a machine with
an audio server and microphone, `cargo test -p openatc-audio
live_selected_devices_open -- --ignored --nocapture` also opens a selected
microphone briefly and plays silence through a selected output; it makes no
network requests. `cargo test -p openatc-core --test shipped_config` validates
the shipped intents, speech, regions and aircraft files with application loaders.

The simulator plugin starts its local companion engine with
`--plugin-parent-pid=<X-Plane PID>`. Plugin-owned engines accept
`/plugin/heartbeat` and `/plugin/shutdown` only for that owning PID. A joined
background worker sends heartbeats every two seconds using wall-clock time;
it makes no simulator API calls, so pause and scenery loading do not suspend
liveness. Heartbeats use a separate lock from dialogue processing. Plugin
disable stops the worker and requests shutdown with a bounded local HTTP call.
The engine also exits if the Linux host process disappears or heartbeats stop
for ten seconds, allowing sixty seconds for the first heartbeat at startup.
Manually started engines and the independent AI/STT/TTS server are unaffected.

Linux plugin installation after a release build:

```sh
cargo build --release -p openatc-plugin -p openatc-engine
python3 scripts/install-rust-plugin.py "/path/to/X-Plane 12"
```

Close X-Plane before installing. The installer backs up the existing OpenATC folder outside the simulator's plugin directory and installs binaries plus configuration and taxi arrow assets. Personal settings remain in the user configuration directory.

Taxi opens on Departure and loads the simulator's current airport when available, falling back to the flight plan departure airport. The explicit Departure and Arrival buttons select the flight plan airports. Maps use installed runway, pavement, taxi graph and parking data. Ground guidance is opt-in under Settings / Realism; map arrows default on. Only an approved route produces arrows, and the unverified connector from a parking stand is never rendered on simulator ground. Guidance stops before the network's clearance limit, disappears off the ground or when disabled, and skips terrain probe misses, water and steep/discontinuous surfaces. Night textures illuminate the arrow surfaces without lighting the surrounding scenery.

The desktop preview accepts an optional zero-based page index after its engine URL (Taxi is `2`) for visual checks.

Airports / Overview has a persistent ILS / glideslope overlay. It uses published glide-slope navdata and matching runway thresholds; airports without a glide slope do not get a fabricated beam. The 4 NM display is an illustration, not operational approach guidance. Append `3d` after the desktop page index to preview the orbit view. Channels loads all receivable nearby stations independently of airport searches.

The ToLiss A320neo profile matches A20N and supports scalar and array cockpit datarefs. The vendored xplane SDK wrapper corrects a reversed type-mask check; its complete MPL source and license ship with the plugin.

Arrival now loads the flight plan destination independently of the Airports browser and local Frequencies. Its direct-distance profile ends at the selected runway's landing threshold (including scenery displacement), using CIFP threshold elevation when available. Planned descent uses a geometric profile and the runway's published glide angle; a published ILS initial-final-segment altitude supplies the intercept reference when present. The current projection uses live vertical speed and true ground track, reports short/long runway-elevation crossings, and stops at a sampled scenery intersection. These are simulator planning references, not a fully constrained STAR/FMS flight path.

The Arrival terrain sampler runs incrementally on the X-Plane main thread, with a maximum of 25 probe calls per half-second tick and cached updates. It samples centre-line ground and a 2 NM corridor. The clearance contour and horizontal floor add 2,000 ft to known corridor scenery; unsampled/distant scenery remains a gap. A conservative proximity limit prevents the SDK's unloaded 0-MSL sphere from appearing as terrain. The transition line is explicitly a regional transition-altitude reference, not a computed transition level. Sliders and redundant metric cards have been removed.

Debug desktop builds accept `OPENATC_TERRAIN_PREVIEW=/path/to/profile.json` for reproducible Arrival visual checks; the graph labels the source PREVIEW and this hook is absent from release desktop builds.

## Editable speech library

The recursive [speech library and editing guide](../speech/README.md) contains 476 situations and 1,420 phrases. ATC files live in `common` or regional `ifr`, `vfr`, `shared` folders. Copilot, attendant and ground-service speech have one global copy in `speech/crew`. Regional `profile.toml` files match the longest airport ICAO prefix and choose ICAO/FAA wording. Unreviewed regions use the documented international baseline.

The loader rejects malformed entries, unknown fields/placeholders, duplicates and incomplete offers with file/entry diagnostics. Extended context slots carry headings, speeds, traffic, station names, weather and explicit units. Checked rendering refuses incomplete instructions. `/suggest` accepts `airport` and `flightRules` (`ifr`/`vfr`); otherwise it infers the plan airport by phase and defaults to IFR. It returns a proposal rather than changing the plan. Flight-plan IFR/VFR state and comprehensive operational speech dispatch remain future work; initial departure clearance/start/pushback dispatch is implemented.

```sh
cargo test -p openatc-core --test shipped_config --test speech_library
cargo run -p openatc-engine -- --check-speech ../speech
```

The checker is offline and starts no service. `python3 scripts/test-speech-engine.py target/release/open-atc-engine ../speech` exercises the real HTTP selection path with an isolated engine and a local fake model; it does not call the configured AI server. Speech changes load on engine restart. Set `OPENATC_SPEECH_DIR` or copy the entire library to the personal config speech directory to preserve edits across updates. The installer backs up the plugin and disables old flat TOMLs as `.legacy` copies before installing the recursive library. Authority references, regional review status and safe authoring rules are in the editing guide. Crew prompts no longer claim they transmitted a radio request or completed an unverified action.

## Radio and departure sprint — 7 October 2026

Channels (Frequencies) lists all receivable published airport stations around the aircraft: airport, station, service, channel, distance and tuned status. Select a row inside X-Plane to tune COM1. Airport searches remain on Airports. Enabled scenery priority and modern 8.33-kHz channel records are respected; the lightweight station index is cached per simulator root.

A powered radio tuned to a published receivable controller is required for every ATC response. Unassigned/out-of-range channels, ATIS and Unicom do not answer controller requests. Clearance delivery follows Delivery → Ground → Tower when separate services are absent. Ground handles start-up/pushback/taxi; Tower takes these duties when no Ground station is published and handles departure readiness. An incorrect controller directs you to an actual published service. Missing services are not invented. Optional `radio-stations.toml` entries explicitly grant combined capabilities to existing stations; existing user overrides survive installation. Restart the engine after editing these overrides.

Create a flight or import SimBrief, review Planning, tune the departure clearance controller, request clearance, read it back, tune Ground, request start-up, pushback or both, then taxi. Start and pushback are recorded as separate permissions; they do not operate the aircraft or tug. Spoken IFR readbacks check callsign, destination, route, altitude, squawk and runway, including NATO letters and spoken digits. Incorrect or unclear values receive a say-again response. Taxi guidance activates after taxi readback and stops at the hold point; runway crossings require separate authorization. Taxi uses the receiving airport's installed graph, not an airport-browser selection, and publishes the approved map/ground guidance path. No traffic separation or runway occupancy clearance is implied by this initial workflow.

Tuning a receivable ATIS starts a repeating automatic broadcast, with fixed pace and a mild electronic timbre. Retuning, radio power loss or loss of reception cancels pending synthesis/playback. ATIS and radio weather reports use X-Plane surface observations at that airport, never aircraft-local weather or online METAR substitutes. The plugin prioritises the tuned station and nearest airport, samples each at most every 15 seconds, and makes at most one SDK query per two seconds in the pre-flight callback. Observations expire after 45 seconds. Information starts at Alpha and advances on meaningful rounded observation changes. The planned runway is identified as planned, rather than presented as a known runway in use.

[XPLMGetWeatherAtLocation](https://developer.x-plane.com/sdk/XPLMGetWeatherAtLocation/) provides airport-specific or best-available regional simulator weather only within the surrounding region. SDK return zero is not itself failure; invalid/incomplete samples are rejected. A receivable distant ATIS gets its own airport sample when tuned; unavailable surface data remains an explicit notice. `/weather` returns fresh simulator samples; `/weather/online` is separate planning-only Internet data. ATIS is an initial English broadcast: full regional unit/number diction, operating-runway selection and NOTAM content remain future work.

The editable phrasebook now has 476 situations and 1,420 phrases. ATC entries declare eligible services. Common departure clearance/start/pushback responses use checked live slots. **Allow AI wording variety** is off by default; the model sees relevant examples but any changed operational token falls back to the original. Broader phrasebook examples do not automatically implement their procedures.

Build and install the Rust implementation:

```sh
cargo build --release -p openatc-plugin -p openatc-engine
python3 scripts/install-rust-plugin.py "/path/to/X-Plane 12"
```

The installer validates speech offline and retains a full backup outside the plugin directory. Personal settings and remote AI services are preserved. Restart X-Plane to load a new plugin build.

Pending speech and engine HTTP requests support cancellation. Rust workers are joined during unload before releasing audio resources; `cargo test -p openatc-http -p openatc-audio -p openatc-ui` covers stalled-provider/client shutdown.

Channels also reads published controllers from enabled scenery `atc.dat` files and the active custom/default ATC navdata. Tower controller definitions replace scenery Tower frequencies; Ground/Delivery remain from airport scenery. Approach and Center discovery uses published horizontal coverage plus a simulated 150 NM regional network reception margin. Assigned controller altitude bands do not prevent tuning from the ground; actual transmitter locations and terrain shielding remain unmodelled. Center rows show “Sector coverage” rather than a misleading distance to a polygon centre. No controller or frequency is invented; regional transmitter locations and terrain shielding are not modelled.

Channels has an editable airport/station search, initially filled with the nearest current airport ID; Show all clears it. Click list column headers to toggle ascending/descending order in Channels, airport frequencies, navaids, parking stands and procedure lists. Filtering/sorting affects display only, not controller reception or tuning.

Developer/debug mode shows Copy ATC conversation on the ATC page. It copies the transcript plus active flight, imported dispatch draft, COM1, aircraft position/ground state and speed. Text fields and conversation export use the native system clipboard (X11/XWayland on Linux). Ctrl+C/Ctrl+X/Ctrl+V exchange text with other applications. Export also tries wl-copy (Wayland), xclip or xsel if native access fails, then saves ~/openatc-conversation.txt. Provider credentials/settings are excluded. Import from SimBrief fills Planning. Requesting IFR clearance automatically validates and submits that displayed flight before sending the clearance request; there is no separate activation button. Empty airports/runways remain empty instead of sample YMLT/YMML defaults.

Live controller responses, readback prompts/corrections, taxi instructions, ATIS/weather wording, crew fallback replies, copilot callouts and background exchanges now come from `speech/runtime/responses.toml` (191 templates, 381 phrases). The regional/crew example library retains 476 situations and 1,420 phrases. Runtime alternatives preserve named fact placeholders; startup rejects missing responses or changed placeholder contracts. Phrase edits do not change Rust authorization logic. Taxi readback uses structured taxiway/hold-short facts, so editable wording is not parsed as route data. An isolated engine regression edits the TOML acknowledgment and verifies that exact edited text is returned; invalid placeholder alternatives are rejected.

Pending readback evidence now takes priority over generic altitude/taxi intent classification. Regression coverage uses the reported VLVT→VTBS typed readback and garbled transcript; an ordinary taxi request is not treated as a readback. The plugin provides a visible bottom-right resize grip that respects minimum size and pop-out pixel scaling.

ATC Auto Reply sits beside Replay ATC. It submits a complete pilot readback using the active IFR/taxi clearance, or acknowledges start-up/pushback approval once. It excludes exchanges addressed to background traffic. It is disabled when no reply is pending. Pilot reply wording is editable in `speech/runtime/responses.toml`; operational values and authorization remain in Rust. Click a chat message to open a read-only selectable text box; use Ctrl+C for a selection or Copy message for the whole line.

Taxi loading prefers published scenery ATC graphs. When absent, it derives an unnamed graph from solid/enhanced painted centerlines (apt.dat styles 1/7/51/57), samples Bezier curves and intersects connected lines. Pavement gaps, edge markings and hold barriers do not become ordinary taxi links. The UI identifies this fallback. Fallback stand connectors are limited to 35 metres of continuous pavement; hold-line buffers allow room ahead of the aircraft reference point.

A holding point away from the departure threshold ends the ordinary taxi route and requires separate Tower backtrack clearance. Request runway backtrack becomes available while stopped at that route endpoint. At airports without a taxiway graph or holding point, Tower can issue runway taxi/backtrack to the assigned departure end over verified pavement and the selected runway centerline. Ground must hand the request to Tower; no specific airport is hardcoded. Backtrack clearance requires readback, grants no takeoff permission, and the arrows stop when the aircraft reaches the end. Backtrack routes reject other-runway crossings, disconnected pavement and overlap with another runway. Ordinary taxi routes stop before an intervening runway; a separate crossing clearance and readback authorise that crossing. Simulator-reported runway occupancy and final-approach protection are implemented; traffic ownership and full separation remain future work.

Auto Reply resolves the current pending instruction in the engine, with separate IFR and taxi readback scopes. Taxi acknowledgments refer to taxi instructions. Ordinary holding-point routes do not say backtrack; runway backtrack wording is reserved for an explicitly authorized route along the runway. The Taxi page includes a saved simulator ground-arrow switch.

Taxi speech uses holding-point and hold-short instructions without referring to map paths or marked networks. Published taxiway names are retained where available; unnamed scenery routes use the assigned runway holding point without invented identifiers. All alternatives and pilot readbacks remain in TOML.

At the approved taxi endpoint, stopped within 25 m, the engine checks the actual tuned airport station. Ground hands off to the published Tower frequency; Tower issues departure clearance when simulator-reported traffic is clear, or holds for runway occupancy/final traffic. Missing traffic data keeps the aircraft holding. Pausing, radio power off, untuned channels and other airports cannot trigger departure. Holding for traffic is reconsidered when the runway clears. Routes needing backtrack or another runway crossing retain their hold restriction and require further explicit permission. Scenery-owned runway guard lights remain unchanged. Duplicate plugin holding-light markers have been removed; no per-holding-point native lighting control has been established. TCAS targets omit ownship and do not include traffic hidden from X-Plane’s traffic interface; this is not full traffic sequencing or wake separation.

Live sessions no longer generate fictional callsigns or canned background clearances; that TOML chatter is demo-only. Actual simulator traffic still gates runway entry and departure. Copilot readbacks are controlled by the readback setting, use complete structured Auto Reply facts, and carry the Copilot label without a controller-station name. Routine auto-response does not parrot taxi prompts. Taxi prompts do not request nonexistent taxiway names, and controller acknowledgments use short ATC wording. Duplicate plugin holding-light objects were removed. The installed simulator exposes global airport-light controls and a read-only wigwag brightness value, not an established per-holding-point native control; existing scenery lights are preserved.

Legacy cleanup: removed top-level `src/`, `include/`, CMake configuration and obsolete C++ test executables. Shared JSON/TOML fixtures remain in `tests/fixtures/`. `scripts/fedora-build.sh` and GitHub CI use Cargo; `scripts/package.py linux-x64` packages the built Rust plugin through the installer. CI currently targets Linux; Windows/macOS packaging remains unverified. Native third-party dependencies remain.

Workspace layout: Cargo.toml, Cargo.lock, models.toml, .cargo configuration and crates/ now live at the repository root. Build with cargo from the root; outputs are under target/. Build/CI/install/package paths and embedded resource/fixture paths were updated. packages/ and target/ are ignored; the patched vendor/xplane source is explicitly included. Local old-crate archives were moved outside the repository.

## Local files

`target/` and `packages/` are generated output. Assistant configuration, instruction files, histories and scratch plans are ignored, as are downloaded model weights and personal environment overrides. Application prompts, speech, `models.toml`, `.cargo/` and CI workflows remain project files. The patched `vendor/xplane` source is included. Ignore rules do not untrack existing files.

## Relocatable AI service data

After `cargo build --locked --release -p openatc-ai -p openatc-stt`, run `python3 scripts/install-ai-runtime.py /path/to/openatc-ai` to install both binaries and the required `bin/espeak-ng-data` directory. Copy the entire installation when moving it to another computer. Without that data, the phonemizer can refer to the original Cargo build path and fail on unfamiliar words or numbers. Restart the service after replacing its data.

## Starting another flight

Use the page-with-plus New flight icon in the plugin header. It clears the active plan, conversation, clearances, taxi route, checklist progress and pending crew actions after confirmation. Settings and controller voices remain available. This reset uses the running engine; it does not unload the plugin or restart the AI server. Automated reset and late-response checks are covered by the radio and crew integration scripts. Live simulator reset and re-enable remain acceptance checks.

## Native platform builds

`.github/workflows/platform-builds.yml` builds both products on native GitHub runners, manually or on `v*` tags. Windows uses MSVC, the target supported by the X-Plane SDK bindings. Macs are built separately for Intel and Apple Silicon. The builds use CPU inference defaults; CUDA builds, signing, installers and universal Mac bundles are not covered.

`vendor/xplane-sys` retains the upstream SDK bindings and licenses, with the macOS framework search directive corrected to `cargo:rustc-link-search=framework=...`. The Cargo patch keeps that fix reproducible rather than changing a developer's registry cache.
