# Unreleased

## Platform warning cleanup — 9 October 2026

ZIP permission options now use a Unix-only shadowed value, removing the Windows unused-mut warning. The vendored SDK binding crate suppresses unnecessary-transmute warnings only on Windows; generated ABI conversions remain unchanged.

## Intel Mac build time — 9 October 2026

Disabled ONNX Runtime unit-test compilation for the Intel Mac AI-server build. The shared runtime and its model support remain enabled.

## Aircraft crew controls and checklist exchanges — 8 October 2026

Added standard X-Plane controls with third-party TOML overrides, a searchable live-availability page, and 463 additional ToLiss cockpit button mappings. Fixed purser/MECH routing by observing actual commands instead of reading nonexistent datarefs. Added bounded control requests, optional validated JSON interpretation, verified setting replies, button-dispatch replies, cabin lighting/slides, chocks/external power and both checklist directions. All spoken requests/challenges/responses are in speech TOML, with 168 new examples. ToLiss pushback distance/angle remain unavailable through identified public controls; no tug is started with stale parameters.


## Named position reports and TTS diagnostics — 8 October 2026

Position requests use an offline worldwide settlement lookup with approximate distance/direction and an airport fallback, replacing spoken GPS coordinates. Added GeoNames attribution and dataset coverage notes. TTS failures record attempted text, voice/model and transport error or provider status; synthesis-worker failures also record text.

## Strict and natural ATC wording — 8 October 2026

Clarified the existing wording setting: off uses TOML controller phrases as written; on permits validated natural wording. Added a real-engine regression proving that strict mode does not call the wording model. Crew conversations remain separate from this ATC wording switch.

## LLM phrase examples — 8 October 2026

Removed the four/six-situation cutoff from crew, suggestion and optional ATC wording prompts. All matching phrase alternatives now reach the model, retaining role, phase, regional and flight-rule selection. Prompts explicitly request fresh wording while preserving standard phraseology, live facts and controller permissions. Deterministic fallback and operational-token validation remain in place.

## Repository housekeeping — 8 October 2026

Expanded ignore rules for local assistant settings, instruction files, histories, generated plans and model downloads. Product prompts, speech, model manifests, Cargo configuration and CI workflows remain project files. Reworded first-party source comments to describe current behavior and removed obsolete porting notes without changing executable code. Consolidated the README around setup, departure operation, editable files and current limitations. No Git operations were performed.

## Taxi fallback, runway backtracking and pilot auto replies — 8 October 2026

ATC Auto Reply sits beside Replay ATC. It submits a complete pilot readback using the active IFR/taxi clearance, or acknowledges start-up/pushback approval once. It excludes exchanges addressed to background traffic. It is disabled when no reply is pending. Pilot reply wording is editable in `speech/runtime/responses.toml`; operational values and authorization remain in Rust. Click a chat message to open a read-only selectable text box; use Ctrl+C for a selection or Copy message for the whole line.

Taxi loading prefers published scenery ATC graphs. When absent, it derives an unnamed graph from solid/enhanced painted centerlines (apt.dat styles 1/7/51/57), samples Bezier curves and intersects connected lines. Pavement gaps, edge markings and hold barriers do not become ordinary taxi links. The UI identifies this fallback. Fallback stand connectors are limited to 35 metres of continuous pavement; hold-line buffers allow room ahead of the aircraft reference point.

A holding point away from the departure threshold ends the ordinary taxi route and requires separate Tower backtrack clearance. Request runway backtrack becomes available while stopped at that route endpoint. At airports without a taxiway graph or holding point, Tower can issue runway taxi/backtrack to the assigned departure end over verified pavement and the selected runway centerline. Ground must hand the request to Tower; no specific airport is hardcoded. Backtrack clearance requires readback, grants no takeoff permission, and the arrows stop when the aircraft reaches the end. Other runway crossings, disconnected pavement and overlap with another runway are rejected. Simulator-reported runway occupancy and final-approach protection are implemented; traffic ownership and full separation remain future work.


## Debug conversation export — 8 October 2026

Removed sample YMLT/YMML airport and runway defaults from live flight plans; the airport browser no longer starts at YMLT. Removed Use flight for ATC; clearance automatically validates and submits the Planning flight before requesting service. Invalid plan submissions prevent the request. Conversation history survives plan submission until explicit reset. Added a debug-only Copy ATC conversation button with transcript and active/draft flight-radio diagnostics. Copy remains available during speech. Linux clipboard helpers have a text-file fallback; provider settings are excluded.


## Clearance feedback and sortable lists — 8 October 2026

Clearance validation now uses identical chat/result messages and identifies departure mismatch, aircraft airborne/moving status or an existing clearance rather than blaming the parked phase. Added a current-airport station search and clickable ascending/descending headers to station, airport-frequency, navaid, stand and procedure tables.


## Consolidated airport controller duties — 8 October 2026

Fixed Tower IFR clearance refusal at airports without separate Delivery/Ground frequencies. Infer consolidated clearance and ground duties from the published airport services; retain separate controller restrictions and explicit TOML overrides. AI phrase examples now include combined capabilities.


## Missing Channels frequencies correction — 7 October 2026

Channels now includes active custom/default and enabled-scenery ATC controller data, including Approach and Center. Published tower overrides and FREQ/CHAN units follow X-Plane’s ATC data format. Regional discovery uses horizontal polygons and a simulated 150 NM network reception margin, including longitude wrapping. Controller responsibility altitude bands are retained as metadata and do not hide tunable frequencies from aircraft on the ground.


## Rust engine startup correction — 7 October 2026

Replaced the engine’s native OpenSSL HTTPS dependency with Rust TLS and bundled public certificate roots. Steam-launched engines no longer require `libssl.so.3` or `libcrypto.so.3`. The Linux dependency audit now rejects those dependencies for the engine as well as the plugin.

## Radio and departure sprint — 7 October 2026

Rust cancellation now covers pending speech and engine HTTP calls. Plugin unload joins synthesis/transcription and client workers before releasing their resources; stalled-provider regression tests exercise the shutdown path.

Channels (Frequencies) lists all receivable published airport stations around the aircraft: airport, station, service, channel, distance and tuned status. Select a row inside X-Plane to tune COM1. Airport searches remain on Airports. Enabled scenery priority and modern 8.33-kHz channel records are respected; the lightweight station index is cached per simulator root.

A powered radio tuned to a published receivable controller is required for every ATC response. Unassigned/out-of-range channels, ATIS and Unicom do not answer controller requests. Delivery handles IFR clearance, Ground handles start-up/pushback/taxi, Tower handles departure readiness. An incorrect controller directs you to an actual published service. Missing services are not invented. Optional `radio-stations.toml` entries explicitly grant combined capabilities to existing stations; existing user overrides survive installation. Restart the engine after editing these overrides.

Create a flight or import SimBrief, review and use the plan, tune departure Delivery, request clearance, read it back, tune Ground, request start-up, pushback or both, then taxi. Start and pushback are recorded as separate permissions; they do not operate the aircraft or tug. Readback currently checks the recorded altitude, route and clearance sequence; complete spoken squawk/runway readback grading remains future work. Taxi uses the receiving airport's installed graph, not an airport-browser selection, and publishes the approved map/ground guidance path. No traffic separation or runway occupancy clearance is implied by this initial workflow.

Tuning a receivable ATIS starts a repeating automatic broadcast, with fixed pace and a mild electronic timbre. Retuning, radio power loss or loss of reception cancels pending synthesis/playback. ATIS and radio weather reports use X-Plane surface observations at that airport, never aircraft-local weather or online METAR substitutes. The plugin prioritises the tuned station and nearest airport, samples each at most every 15 seconds, and makes at most one SDK query per two seconds in the pre-flight callback. Observations expire after 45 seconds. Information starts at Alpha and advances on meaningful rounded observation changes. The planned runway is identified as planned, rather than presented as a known runway in use.

[XPLMGetWeatherAtLocation](https://developer.x-plane.com/sdk/XPLMGetWeatherAtLocation/) provides airport-specific or best-available regional simulator weather only within the surrounding region. SDK return zero is not itself failure; invalid/incomplete samples are rejected. A receivable distant ATIS gets its own airport sample when tuned; unavailable surface data remains an explicit notice. `/weather` returns fresh simulator samples; `/weather/online` is separate planning-only Internet data. ATIS is an initial English broadcast: full regional unit/number diction, operating-runway selection and NOTAM content remain future work.

The editable phrasebook now has 476 situations and 1,420 phrases. ATC entries declare eligible services. Common departure clearance/start/pushback responses use checked live slots. **Allow AI wording variety** is off by default; the model sees relevant examples but any changed operational token falls back to the original. Broader phrasebook examples do not automatically implement their procedures.

Build and install the Rust implementation:

```sh
cargo build --release -p openatc-plugin -p openatc-engine
cargo xtask install-plugin "/path/to/X-Plane 12"
```

The installer validates speech offline and retains a full backup outside the plugin directory. Personal settings and remote AI services are preserved. Restart X-Plane to load a new plugin build.


## 2026-10-07 — Rust runtime, UI and editable speech

The current Linux plugin uses the Rust engine/plugin workspace. Audio now lists native device names, separates microphone sources from playback monitors, opens cached selected IDs and uses correct playback sample-rate handling and gain. Local engine supervision uses plugin ownership, explicit shutdown and wall-clock heartbeats that continue during pause; it does not stop the independent AI server.

UI changes include padded chat and role/station labels; improved airport/taxi maps; current/departure taxi defaults; approved-route map and terrain-following illuminated ground arrows with Realism switches; 2D/3D ILS overlays from published navdata; local-frequency selection tuning; ToLiss A320neo A20N power/radio mapping and corrected vendored SDK dataref type matching. Arrival now compares planned/current descent to the arrival threshold with ground, corridor clearance, horizontal safe-low-altitude, transition-altitude and published ILS intercept references; redundant sliders/cards were removed. Scenery gaps remain explicit.

Speech expanded from 195 situations / 399 phrases to **473 / 1,414**. Subject filenames were clarified, historical IDs retained, and alternatives split so offers, clearances, readiness, completion, pilot/controller and crew roles do not contradict one another. Dynamic context replaces invented fixed traffic, headings, speeds, weather and times. Global crew files are separate from common/regional ATC IFR/VFR/shared folders. Regional profiles use longest ICAO-prefix matching; selected US/Canada/UK/Australia examples have references and other packs document baseline-only status.

Rust loading is recursive and validates fields, phases, placeholders, duplicate IDs and offers. Checked rendering refuses missing context. `/suggest` supports airport and IFR/VFR selection and returns proposed text/effects only. An offline `--check-speech` command validates edits. The installer preserves old flat speech TOMLs as disabled legacy copies after a full backup. Crew prompts no longer claim unverified radio transmissions or aircraft actions. The new speech editing guide and both first-party READMEs document this work, override precedence, restart requirements and remaining limitations.

This is not completed worldwide operational ATC: traffic separation, published procedure execution, comprehensive speech dispatch and flight-plan IFR/VFR state still require engine work. X-Plane flight/radio/night-arrow acceptance is distinct from successful builds, tests and desktop previews.

Audio backend starts lazily with a timeout: a stuck backend reports
unavailable instead of freezing plugin load.

Dropdown crash fixed: this imgui build dereferences empty-string IDs, and
the mic picker pushed the empty "System default" key. All ID pushes are now
provably non-empty (indices and guarded titles), with a headless regression
test rendering the real device list.

Audio device enumeration hardened: single-call listing per side, cached
lists refreshed only on demand instead of every frame; native PulseAudio/PipeWire preferred with ALSA fallback (supersedes the earlier ALSA-only selection).
cached lists refreshed only on demand instead of every frame.

Engine accepts raw WAV as well as multipart on /speech/transcribe — the
plugin posts raw bytes, which the old extractor rejected, so every in-sim
transcription failed. Proven with a raw upload end to end.

Engine bundles its TLS (libssl/libcrypto/libz beside the binary, $ORIGIN
rpath): supervision spawns no longer die on libssl.so.3 under the Steam
runtime. Proven by launching the deployed binary in a bare environment.

Plugin enable can no longer fail on dataref types: telemetry refs resolve
flexibly (f64/f32/i32) with per-ref MISSING lines in the log, and enable logs
its own success line. Panel renders inside its own window (was empty
in-sim); engine binary resolved from the plugin path, not the process.
Panel content offset into the floating window rect (was rendering at the
screen corner).

Engine cutover: release Rust engine + plugin deployed to the sim plugin dir
(verified md5, zero TLS linkage); Standard preset now enforces tuned-COM1
frequency (C++ matcher updated to match — deleted with the rest in 4c).

Copilot autonomy: auto-tune-handoff and auto-respond switches, copilot
readback/parrot split, altitude-preselect management with AP-off advisories,
per-aircraft autopilot ref mapping (ToLiss profile included), Copilot tab UI.
Frequency discipline: tuned-COM1 gate on by default (Standard preset),
coverage model (expected airport, nearby airport, region center, else
silence — nobody answers), proactive handoffs on facility change with
retune verify and one complaint, all proven live.

Earlier speech library (`speech/*.toml`, superseded by the recursive library above): ATC offers with accept/decline
pairs and plan effects (shortcut, smoother altitude, below-minimums diversion and twelve
more), per-phase controller speech, advisories plus compliance complaints (altitude bust,
off route, speed, wrong runway, missing readback), emergency scripts by scenario, copilot
PM callouts and checklists, cabin PAs, ground-service coordination and frequency chatter.
`intents.toml` rewritten: 167 examples per intent in request/say/readback kinds with ICAO
and FAA variants, new `{dest} {rwy} {sq} {qnh} {sid} {freq} {via} {stand} {wind} {atis}`
`{mins} {nm}` placeholders, multi-sentence fallbacks. `prompts/*.txt` rewritten with role
rules and few-shot examples; new `prompts/atc_reply.txt` controller voice (offer/complaint/
emergency flows with plan-effect JSON) wired for the Rust engine. Rust `speech` module:
tagged pool loader, role/situation/phase/variant selector, few-shot prompt assembler with
phraseology system rules, AI output effect parser, AI-off canned renderer. `regions.toml`
extended to 12 regions with sourced transition altitudes (NZ 13000, Japan 14000, UAE 13000,
Europe default 5000, Moscow standard 10000, Canada 18000). Rust `compliance` module:
offer/clearance expectations with grace ticks and loose tolerances (300 ft, 20 kt) mapping
deviations to `complaint.*` speech ids for the Slice-2 engine loop. Port-first
Slice 1: `applyRequest` dialogue kernel, controller roster, taxi router, apt.dat
scenery loader, navaids/CIFP loaders in `openatc-core` (`apply.rs`, `airport.rs`),
proven by shared `tests/fixtures/apply.json` — 45 scenarios with byte-exact
replies plus roster checks, executed by both C++ (601 checks) and Rust (30 tests).
SimBrief OFP normalizer ported (`simbrief.rs`): lenient coercion, strict numbers,
lbs handling, shared `tests/fixtures/simbrief.json` — 9 documents, both sides green
(C++ 658, Rust 31). Slice 2 `atc-engine`: full Rust engine (all 18 endpoints, AI/
STT/TTS/METAR/SimBrief clients, weather watch, congestion, controller roster),
side-by-side parity against the C++ engine — 24 protocol steps plus byte-identical
TTS audio, all matching. Slice 3 `atc-ui` underway: engine client, interface
state, widget helpers, full ATC page with a new Frequencies strip (tuned and
assigned markers) plus the altitude/direct modal, desktop runner on the same
imgui/winit/glow stack, headless render tests green. All UI pages ported (dispatch, taxi map, arrival
profiler with canvas, airport browser with orbit view, full settings), map
canvas with layers and ownship, desktop runner verified. Slice 4: `atc-audio`
(miniaudio capture/playback/relay, same status strings), `openatc-http` plain-
HTTP client (plugin closure carries zero TLS linkage), `atc-plugin` cdylib
(window, flight loop, datarefs, commands, menu, supervision, panel/power) —
release 5.8 MB, all 5 XPlugin exports, deployed to the sim where it loads
with a clean log. AI generation wired end to end: the engine loads `speech/`
at startup, appends retrieved few-shot examples to crew/copilot/readback
prompts, and serves POST /suggest (role + situations + facts → fresh
transmission + effect JSON) — proven live against qwen2.5:7b with the
shortcut and smoother-altitude situations. Frequency discipline: tuned-COM1
gate on by default (Standard preset), coverage model (expected airport, nearby
airport, region center, else silence — nobody answers), proactive handoffs on
facility change with retune verify and one complaint, all proven live.

Voice roster: comma-separated kokoro pool with a Test/Remove popup; per-airspace
controller memory (`controllers.json`, persists across restarts) with no-repeat allocation;
delivery presets (Standard/Brisk/Urgent), per-controller min/max speed, copilot/pilot/cabin/ground
voices, speeds and volumes; pilot self-hear toggle; urgent delivery wins with Apply saving the
new speed. CALLS panel (ATC/CABIN/GND) with conversational crew chat behind AI classification.
Realism tab with Relaxed/Standard/Strict presets (verbatim readbacks, tuned-frequency and
callsign requirements, phraseology corrections, congestion delays/standby/chatter, mayday
practice, emergency intent). Gateway-baked radio FX (bandpass, hiss, crackle, static).
Fixed a mixer/UI-thread race in speech teardown that segfaulted the sim under rapid speech;
teardown now drains in-flight callbacks first. Settings serialization is hand-written (the
nlohmann member macro caps below our field count). In-plugin clipboard on Linux/Wayland.
Rust workspace: settings schema, tri-OS platform layer, verified model manager, and the
`openatc-ai` server with embedded LLM inference (STT/TTS slices next). README rewritten as the
full manual (per-OS setup, same/split machine, complete usage guide). Cabin and ground crew
default to FX-free audio (intercom/interphone are not radio) with per-role toggles.

Cruise and Weather pages removed (Arrival keeps descent planning; the engine `/weather`
endpoint stays). The controller now watches live METARs for departure/destination/alternate
and warns once per new hazard (thunderstorm, hail, freezing rain, ash, destination IFR/LIFR,
windshear on approach) in ICAO-neutral phraseology with regional pressure. Units switch
(Imperial/Metric/Region) for display and speech with `regions.toml` local procedure
(QNH/altimeter, clearance shape, transition altitude). AI prompts moved to `prompts/*.txt`;
cabin/ground address you as the captain and never deflect to the flight deck.
Crew comms moved to the cockpit panel: the CALLS selector is gone, replaced by a TALKING TO
readout — Transmit routes to cabin/ground while their call-light dataref is lit (mapped in
Settings, ground wins, else ATC). Optional Copilot button addresses the copilot (chat, or
relayed radio calls stamped COPILOT). `openatc/talk` and `openatc/talk_copilot` sim commands
transmit with the plugin window closed. `openatc/mic_push_to_talk` records while held and
transcribes into the box on release (review before transmitting). The plugin supervises the engine: if it cannot reach
one it launches the bundled `OpenATC/bin/open-atc-engine` (output to `engine.log` beside the
settings) with backoff, so a closed or crashed engine comes back on its own.
The bundled engine carries its own OpenSSL (`$ORIGIN` RPATH) after Steam-runtime
`libssl.so.3` failures, and repeated failed starts surface
`Engine won't start - see engine.log` in the plugin status instead of silent grey.
ToLiss A320neo first-class profile (`aircraft/toliss_a320.toml`, names extracted from the
installed plugin): CALLS ATTN zones route to cabin, MECH to ground, no config needed
(Settings override, `openatc.toml` beside the `.acf` overrides the bundle). Per-path power
gating — ATC needs the radio on, cabin/ground need bus power — with status-line refusals;
copilot chat is never gated. Transmit button renamed Talk for the copilot side: Transmit
sends everything, Talk addresses the copilot.
`openatc-ai.service` user unit (auto-restart, journal logs); engine verifies STT/TTS
reachability with `Voice: ready/down` status, and reloads `settings.json` from disk
within 30 s so file and UI edits converge. Talk sits left of Transmit; Transmit
greys out without power (Talk never does); refused transmissions log one `SYSTEM`
line per outage in the ATC log, never spoken.
Talk is to the copilot only: the ATC relay is removed, so the copilot never transmits
to ATC from it — ATC tasks get a brief in-character redirect instead.
Crash hardening: all X-Plane entry points (flight loop, window draw, PTT commands) log
instead of terminating on unexpected exceptions; PTT refuses overlaps with a Mic busy
status; copilot preset wording migrations preserve saved selections; panel role
transitions and call-ref resolution report to Log.txt. Attendant presets added (Warm
Professional, Cheerful Chatty, Humorous, Jokey, Calm Reassuring, Custom on edit).
Talk commands become true voice-PTT (hold to record, release to auto-transmit) with
nothing-heard guards (short/quiet/blank recordings are dropped, never sent); the
on-screen buttons and `mic_push_to_talk` keep the review flow.
Realism presets become a dropdown (Relaxed/Standard/Real/Custom-auto); Strict renamed
Real; unclear requests get helpful guesses at Relaxed instead of dead ends; the
classifier gets a per-level leniency line; cockpit chat is exempt from discipline by
rule (exact copilot readbacks pass full Strict). Copilot personalities become a
dropdown with Custom-on-edit. In-plugin clipboard removed (crash reports) pending a
planned replacement.
Copilot speech no longer needs auto-readbacks on: Talk conversations and relayed
calls speak whenever copilot speech is enabled (`copilotReplies` keeps meaning
automatic readbacks only).
Copilot presets grow Humorous + Super Silly (silliness stays cockpit-only, ops verbatim);
attendant presets added (Warm Professional, Cheerful Chatty, Humorous, Jokey, Calm
Reassuring); cabin base prompt is sociable by default; copilot voice defaults to am_echo.
AI null-valued JSON no longer throws (explicit nulls behave like missing keys).
Developer mode (default on) with verbose errors, auto-logging, and `openatc/copy_error`.
Rust STT slice live: `openatc-stt` sidecar (whisper tiny, CPU; `--stt-backend`
selects auto/cpu/cuda, CUDA needs a toolkit build) serving gateway-compatible
`/v1/audio/transcriptions` — A/B parity with faster-whisper on identical audio, engine
STT base repointed at it. Sidecar forced by the vendored-ggml link collision between
whisper.cpp and llama.cpp. TTS stays on the Python gateway (one process) until its slice.
Rust TTS slice live: kokoro via prebuilt ORT + source-built espeak-ng (zero system
deps), same `voices-v1.0.bin` table and voice map, pause semantics and seeded FX chain
ported from the gateway (perceptual parity, proven by closed-loop STT round-trip).
TTS base repointed at it; Python gateway service stopped + disabled, venv + test dir
deleted from the services host (~3.9 GB reclaimed, legacy scripts were retained at that time; they have since been removed).
Full Rust round-trip proven: TTS then STT with zero Python in the path.

# 0.2.1

The visible product name is OpenATC AI in the sidebar, compact view, plugin menus, window titles, voice test and engine banner. The sidebar has a cyan vector control tower with radio arcs, a white OpenATC wordmark and cyan AI suffix. The subtitle is removed and sidebar width accounts for the complete wordmark at the selected UI scale.

Plugin installation paths, command identifiers, settings paths and the HTTP protocol retain their existing identities. Font rasterization and DPI behavior are unchanged in this branding revision.

# 0.2.0

Compared the supplied full 0.1.0 archive and Muse's seven-file `src.zip`: all seven source files matched exactly. Muse's separately reported OpenSSL isolation was absent from those source-only changes; this revision implements it in CMake and the Linux dependency audit, with all external HTTPS delegated to the engine.

The eight-page interface now has scalable embedded fonts, centered icon navigation, modal ATC parameters, contextual requests, an expanded dispatch sheet with online OFP import, airport stands/navaids/procedures, approved taxi-path layers, measured-versus-planned descent, georeferenced weather observations, and persistent copilot/AI/STT/TTS/audio-device settings.

Live telemetry now drives a limited local simulator controller instead of causing every operational request to be refused. Its limitations are listed in README and ROADMAP. The implementation does not claim traffic separation or a complete approach controller.

### Rust taxi map and ground guidance

- Fixed 26-field simulator runway records and Global Airports scenery priority.
- Taxi defaults to the current simulator airport with explicit departure/arrival labels and selectors.
- Added curved, filled pavement geometry with holes, uncluttered stand labels, approved route chevrons and stop markers.
- Added separate Realism switches for plugin map arrows and illuminated, terrain-probed simulator ground arrows.
- Browsing another airport preserves the existing approved clearance, scoped to its airport.
- Added a backed-up Rust installer that includes the arrow model, day/night textures and license notice.
IFR clearance messages explicitly request readback. Spoken readback matching checks NATO callsign/destination, route, altitude, squawk and runway; unclear values receive say-again rather than a phase error. Taxi instructions remain pending until readback is accepted. Ground guidance ends when stopped within 25 metres of the route endpoint. Protected runway edges are excluded; a reachable identified crossing hold point may end a partial route, without crossing permission. The UI registers a native system clipboard backend for external text-field copy/paste, with persistent ownership. Live simulator voice and clipboard interaction remain user acceptance checks.

Live controller responses, readback prompts/corrections, taxi instructions, ATIS/weather wording, crew fallback replies, copilot callouts and background exchanges now come from `speech/runtime/responses.toml` (191 templates, 381 phrases). The regional/crew example library retains 476 situations and 1,420 phrases. Runtime alternatives preserve named fact placeholders; startup rejects missing responses or changed placeholder contracts. Phrase edits do not change Rust authorization logic. Taxi readback uses structured taxiway/hold-short facts, so editable wording is not parsed as route data. An isolated engine regression edits the TOML acknowledgment and verifies that exact edited text is returned; invalid placeholder alternatives are rejected.

Pending readback evidence now takes priority over generic altitude/taxi intent classification. Regression coverage uses the reported VLVT→VTBS typed readback and garbled transcript; an ordinary taxi request is not treated as a readback. The plugin provides a visible bottom-right resize grip that respects minimum size and pop-out pixel scaling.

Taxi speech uses holding-point and hold-short instructions without referring to map paths or marked networks. Published taxiway names are retained where available; unnamed scenery routes use the assigned runway holding point without invented identifiers. All alternatives and pilot readbacks remain in TOML.

At the approved taxi endpoint, stopped within 25 m, the engine checks the actual tuned airport station. Ground hands off to the published Tower frequency; Tower issues departure clearance when simulator-reported traffic is clear, or holds for runway occupancy/final traffic. Missing traffic data keeps the aircraft holding. Pausing, radio power off, untuned channels and other airports cannot trigger departure. Holding for traffic is reconsidered when the runway clears. Routes needing backtrack or another runway crossing retain their hold restriction and require further explicit permission. Scenery-owned runway guard lights remain unchanged. Duplicate plugin holding-light markers have been removed; no per-holding-point native lighting control has been established. TCAS targets omit ownship and do not include traffic hidden from X-Plane’s traffic interface; this is not full traffic sequencing or wake separation.

Live sessions no longer generate fictional callsigns or canned background clearances; that TOML chatter is demo-only. Actual simulator traffic still gates runway entry and departure. Copilot readbacks are controlled by the readback setting, use complete structured Auto Reply facts, and carry the Copilot label without a controller-station name. Routine auto-response does not parrot taxi prompts. Taxi prompts do not request nonexistent taxiway names, and controller acknowledgments use short ATC wording. Duplicate plugin holding-light objects were removed. The installed simulator exposes global airport-light controls and a read-only wigwag brightness value, not an established per-holding-point native control; existing scenery lights are preserved.

Legacy cleanup: removed top-level `src/`, `include/`, CMake configuration and obsolete C++ test executables. Shared JSON/TOML fixtures remain in `tests/fixtures/`. `cargo xtask build` and GitHub CI use Cargo; `cargo xtask package linux-x64` packages the built Rust plugin through the installer. CI currently targets Linux; Windows/macOS packaging remains unverified. Native third-party dependencies remain.

Workspace layout: Cargo.toml, Cargo.lock, models.toml, .cargo configuration and crates/ now live at the repository root. Build with cargo from the root; outputs are under target/. Build/CI/install/package paths and embedded resource/fixture paths were updated. packages/ and target/ are ignored; the patched vendor/xplane source is explicitly included. Local old-crate archives were moved outside the repository.

### Cabin phase and ToLiss pushback follow-up

- Cabin greetings use speech TOML and no longer select a landing-preparation response. Other cabin conversations receive live phase, ground state, speed and flight endpoints. Removed the landing example embedded in the cabin prompt.
- Ground crew can trigger ToLiss pushback with settings already entered in ToLiss. Requests containing distance or turn parameters remain unsupported, and questions, stop requests and negated requests do not start the tug.
- Verified the live simulator exposes writable cabin brightness, slide arm/disarm commands and readable slide state. Physical control behavior still needs simulator acceptance testing.
- Crew integration and radio regression checks pass.

### Ground crew wording and attendant identity

- Pushback acknowledgments use brief ground-crew wording without aircraft vendor or configuration details. Explicit pushback requests use the aircraft ground-panel settings, including when spoken requests contain distance or angle. OpenATC does not change those parameters.
- Recognize both CABIN and ATTENDANT transcript identities for badges, attendant voice, volume and speech playback settings.
- Treat explicit connection requests as actions rather than status queries. Unmapped ground-service connections cannot receive fabricated completion claims through free conversation.

### Friendly cabin phrasebook

- Added 47 situations and 282 phrases covering all 11 flight phases, with greetings, thanks, clarification, refreshments, cabin comfort, departure and arrival updates, and farewells.
- Separate interphone and passenger-announcement examples; casual conversation cannot be treated as an announcement request.
- Expanded the live wellbeing greeting to 12 alternatives. Updated the cabin prompt, editing documentation and phrasebook index.

### Clearance wording variation

- Rotate runtime phrases per response ID instead of using a shared counter. Rotate clearance/start/pushback templates independently of transcript sequence.
- Persist ATC phrase choices and previous station/task wording in `phrase-history.json`, retaining them across flight resets and engine restarts.
- Explain controller duties, live phase and current task in optional wording prompts, with prior wording and phase/rules/service-matched examples. Reject repeated model candidates and preserve operational facts.
- Expanded the IFR route-clearance template to ten alternatives. Strict mode stays on TOML wording; only the variety setting enables model rewording.

### Panel appearance and chat frequency links

- Lightened the panel and child backgrounds, with translucent tint, a soft top highlight and a subtle border. This is a glass-style finish without simulator-image blur.
- Underline decimal COM frequencies in ATC messages and tune COM1 when clicked. Respect radio power, wrap text and retain message selection/copy. Pilot and crew messages remain plain text.
- Headless UI checks exercise link clicks, powered-off behavior and ordinary text; parser checks exclude altitudes, runway numbers and navigation frequencies. Simulator appearance still needs a visual check.

### Simulator test follow-up

- Recognize chalks/shocks as chocks aliases, reject compound control requests instead of executing only one control, and prevent unconfirmed ground-service completion claims. Control acknowledgments now require 750 ms of stable readback within the confirmation timeout.
- Holding-point detection uses a 45 m stopped-aircraft tolerance to allow for the aircraft reference point behind its nose. Ground hands off to Tower, and traffic protection still governs runway permission.
- Remember the actual last clearance template as well as its next index. Skip repeated templates and log selected alternatives and rendering failures for diagnosis.
- Removed the extra square panel gradient/border and increased the tinted surface opacity. True simulator-image blur remains unavailable in the current renderer.

### Clearance readback and Ground handoff wording

- Removed the routine spoken list of required readback items from initial clearances. Validation still checks those items.
- Correct IFR readback on Delivery now includes the published Ground frequency for start-up/pushback when ready; no station or permission is invented where a separate Ground station is absent. All handoff alternatives are in speech TOML.

### Intermediate runway crossings

Taxi hold-short instructions now identify the nearby physical runway instead of always naming the departure runway. Ground and Tower can proactively issue a traffic-checked crossing clearance at an intermediate hold. Crossing readback enables its route; reaching the far-side endpoint produces a new taxi instruction toward the original destination, requiring readback. Crossing completion uses a tighter endpoint tolerance than an ordinary holding-point arrival. Comma-separated runway annotations and padded reciprocal runway numbers are accepted. Crossing edges are sampled against other runways to prevent an unintended second crossing. New crossing, hold, readback, vacated and onward-taxi alternatives remain editable in TOML. Yellow guard lights remain scenery-owned and continue flashing; they are not stop bars.

Removed the ATC-page taxi readback banner and its duplicate instruction display. Taxi clearance validation and Auto Reply remain available.

### New flight and plugin re-enable

Added a page-with-plus New flight icon in the window header with reset confirmation. Reset clears the plan, conversation, permissions, guidance, checklist progress, pending crew actions and local recording/playback state. Operator settings, phrase history, controller voices and live aircraft state are preserved. Previous queued commands, stale snapshots and delayed model responses cannot restore the old flight after reset. The simulator host clears its pending crew callbacks and guidance caches. Reset does not restart the remote AI server or change aircraft controls.

Re-enabling now reuses the existing window and ImGui renderer instead of creating a second active context. Command handlers and the Plugins menu are registered once. Disable releases flight-local crew bindings and datarefs, and enable rebuilds them. Live X-Plane re-enable and header reset remain simulator acceptance checks.

### Controller voices and crossing completion — 9 October 2026

Controller assignments persist across engine restarts. English-only pools discard other languages and duplicate aliases. New assignments prefer voices not already assigned; existing duplicate assignments are repaired when unused voices are available. Distinct saved assignments remain unchanged. Once the pool is exhausted, recent assignments are avoided where possible. Empty pools now provide six English voices instead of one.

An approved runway crossing completes once the aircraft is fully clear on the far side, including while moving. The next taxi clearance continues to the original departure runway and requires readback. This replaces the crossing endpoint stop requirement; ordinary holding points still require a stop.

### Vacated reports, taxi map and holding-point handoff

Recognise runway-vacated reports, including “VACTED”, and reply with the onward taxi instruction rather than a generic phase error or readback acknowledgement. New taxi geometry appears immediately on the map in amber while awaiting readback; approved guidance remains green. Simulator arrows still require approval. Holding-point recognition includes nearby scenery holding lines when the final graph node is offset. Surface telemetry monitoring now lives in its own engine module, with a shared derived clearance lifecycle. Automatic handoff replies retain the assigned controller voice.

### Optional holding-point graphic

Added an opt-in holding-point highlight and detection area on the map, with a terrain-following illuminated amber H marker in the simulator. The setting defaults off and is independent of taxi arrows. Holding detection and map radius share the same constant. Markers clear with route replacement, new flight, disable and departure permission.

The simulator holding marker now includes a translucent 24-metre amber disc with a clearer rim and enlarged H/bar. Terrain footprint probes cover the enlarged graphic. This changes visual guidance only; detection distances remain unchanged.

Successful runway-crossing readbacks now activate the crossing silently. Removed the redundant crossing-readback acknowledgement template. Incorrect readbacks remain pending and receive correction; verified crossing completion produces the next taxi instruction.

The Channels automatic filter uses the active surface-clearance airport, then the simulator airport, before falling back to the nearest station. It refreshes when airport data arrives; manually entered filters and Show all are preserved. This avoids a nearby airport transmitter selecting the wrong filter.

Holding-point graphics use the final taxi approach and a nearby scenery hold-line intersection to choose the approach side. The translucent disc is 30 metres across; its edge is placed four metres before the line, accounting for oblique approaches. Where no line is available, it is set back from the route endpoint. This is visual guidance derived from available scenery, not aircraft nose-position measurement.

Removed the H and bar from the simulator holding-point graphic, leaving the translucent amber circle and rim.

Fixed holding-circle flicker by removing coplanar overlap between the translucent disc and rim. Their geometry now shares a boundary.

Channels now checks loaded airport pavement/runway containment before using simulator reference-point proximity. Channels also loads the planned airport geometry for verification. The holding circle uses a single textured quad with a 256-pixel alpha mask instead of segmented fill/rim geometry.

Added debug-only header surface jumps to holding point and cleared departure runway. Address outgoing controller speech with the callsign and prevent acknowledged departure clearances from redirecting to Delivery. Restrict surface automatic readiness to taxi phases.

Copilot radio replies now originate in the simulator after its speech queue drains, rather than being applied inside the engine request handler. The same endpoint handles onward taxi instructions and takeoff acknowledgements. Automatic tuning and cockpit actions wait for playback and pending copilot replies. Final holding-point telemetry does not invent a pilot ready report; holding recognition no longer requires zero taxi speed. Channels also loads local airport geometry (page 6) to identify the occupied airport. Regression checks cover deferred readback, takeoff acknowledgement, moving holding detection and installed KLAX positions; live ToLiss audio timing still needs simulator testing.

Radio turn-taking uses a two-step copilot exchange: record and play the readback, then apply it after playback finishes. Telemetry marks the radio busy during playback, recording or pending crew replies, delaying automatic surface and airborne calls. A previously reported ready aircraft can receive takeoff clearance after blocking traffic clears. Crossing eligibility follows holding-point detection without a conflicting stopped-aircraft gate. Unknown runway status produces an explained standby instruction and retries when confirmation becomes available. The debug header uses a vector fast-forward icon with the same dimensions as other controls. Unit and engine regressions cover prepared readbacks, occupied-radio suppression, moving hold recognition and traffic-clearance retry; embedded simulator layout and live audio need user verification.

ATC quick requests follow the live flight phase and the published duties of the receivable tuned controller, including combined duties. ATIS and untuned frequencies do not expose controller requests. In flight, **Check in with controller** sends the tuned station name, callsign and current altitude using editable speech TOML. Crossing and landing requests are included when appropriate.

Typed ATC requests add missing controller and callsign addressing. Spoken transmissions retain the words recognized by speech-to-text. AI intent recovery tolerates clear request-word typos and recognition errors; operational values and readback checks remain authoritative. Crew requests can contain up to eight explicit actions, such as “remove chocks and external power.” Clauses inherit the action verb where appropriate. All targets are validated against the aircraft profile before the batch becomes visible to the simulator; each action needs confirmation, and failure cancels the remaining actions. Duplicate controls and unsupported clauses are rejected. The LLM can return a bounded actions array using mapped control IDs, never raw simulator refs. Settings → Voices → Pilot (own transmissions) → Speak my transmitted requests enables synthesized playback of the pilot requests. It is off by default. Engine regression tests use mocked model responses and simulator acknowledgements; live control effects remain a simulator check.

### Platform build validation

- Run builds only on version tags or manually.
- Add native Linux, Windows, Intel Mac and Apple Silicon build checks for the plugin/engine and AI/STT server.
- Correct the vendored X-Plane SDK's macOS framework search directive and remove an unused Windows import.
- Document the difference between Linux cross-target checks, native builds and simulator testing.

### Rust developer tooling

Replaced the Python installers, ZIP packager, Linux dependency checker, artifact collector and radio/crew/speech integration suites with `cargo xtask`. Retained backups, local station edits, legacy speech migration, license notices and ZIP executable permissions. Native AI artifacts now include eSpeak data and shared libraries. Removed the obsolete Python speech gateway, C++ font generator, standalone SDK downloader and old engine harness; removed `scripts/`. Moved the portable Linux service example to `packaging/systemd/`. ONNX Runtime's upstream Intel Mac source build remains a build-time Python dependency.

### Windows plugin build and startup

- Exclude Windows CRT functions from the generated X-Plane SDK bindings.
- Use a detached Windows process for companion-engine startup and resolve its `.exe` filename.
- Load native OpenGL libraries on Windows/macOS, with Windows extension lookup through `wglGetProcAddress`.
- Store Windows companion settings and logs in the local application-data directory.

## Controller check-in and handoff corrections

- Recognise “checking in”, “check-in” and “with you”, including calls with controller names and callsigns.
- Offer ground check-ins and include the active climb altitude in airborne pilot check-ins. Controller acknowledgements no longer present telemetry as a pilot-reported altitude.
- Give airborne handoffs a copilot acknowledgement, confirm tuning through telemetry, then prepare and play the copilot check-in before ATC processes it.
- Remove development-controller wording and routine taxi readback prompts from speech. Keep hold-short readbacks in the instruction form.
- Avoid duplicate expected/initial altitude wording and use a conservative SID instruction without claiming an unmodelled climb-via clearance.
- Recognise cabin arming/cross-check variants and handle a comma-separated coffee request independently. Allow slide indications time to settle and include actual readback values in failure logs.
