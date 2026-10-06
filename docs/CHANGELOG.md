# Unreleased

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
deleted from the services host (~3.9 GB reclaimed, `scripts/` kept as rollback reference).
Full Rust round-trip proven: TTS then STT with zero Python in the path.

# 0.2.1

The visible product name is OpenATC AI in the sidebar, compact view, plugin menus, window titles, voice test and engine banner. The sidebar has a cyan vector control tower with radio arcs, a white OpenATC wordmark and cyan AI suffix. The subtitle is removed and sidebar width accounts for the complete wordmark at the selected UI scale.

Plugin installation paths, command identifiers, settings paths and the HTTP protocol retain their existing identities. Font rasterization and DPI behavior are unchanged in this branding revision.

# 0.2.0

Compared the supplied full 0.1.0 archive and Muse's seven-file `src.zip`: all seven source files matched exactly. Muse's separately reported OpenSSL isolation was absent from those source-only changes; this revision implements it in CMake and the Linux dependency audit, with all external HTTPS delegated to the engine.

The eight-page interface now has scalable embedded fonts, centered icon navigation, modal ATC parameters, contextual requests, an expanded dispatch sheet with online OFP import, airport stands/navaids/procedures, approved taxi-path layers, measured-versus-planned descent, georeferenced weather observations, and persistent copilot/AI/STT/TTS/audio-device settings.

Live telemetry now drives a limited local simulator controller instead of causing every operational request to be refused. Its limitations are listed in README and ROADMAP. The implementation does not claim traffic separation or a complete approach controller.
