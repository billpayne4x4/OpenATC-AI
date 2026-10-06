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

# 0.2.1

The visible product name is OpenATC AI in the sidebar, compact view, plugin menus, window titles, voice test and engine banner. The sidebar has a cyan vector control tower with radio arcs, a white OpenATC wordmark and cyan AI suffix. The subtitle is removed and sidebar width accounts for the complete wordmark at the selected UI scale.

Plugin installation paths, command identifiers, settings paths and the HTTP protocol retain their existing identities. Font rasterization and DPI behavior are unchanged in this branding revision.

# 0.2.0

Compared the supplied full 0.1.0 archive and Muse's seven-file `src.zip`: all seven source files matched exactly. Muse's separately reported OpenSSL isolation was absent from those source-only changes; this revision implements it in CMake and the Linux dependency audit, with all external HTTPS delegated to the engine.

The eight-page interface now has scalable embedded fonts, centered icon navigation, modal ATC parameters, contextual requests, an expanded dispatch sheet with online OFP import, airport stands/navaids/procedures, approved taxi-path layers, measured-versus-planned descent, georeferenced weather observations, and persistent copilot/AI/STT/TTS/audio-device settings.

Live telemetry now drives a limited local simulator controller instead of causing every operational request to be refused. Its limitations are listed in README and ROADMAP. The implementation does not claim traffic separation or a complete approach controller.
