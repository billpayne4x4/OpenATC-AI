<picture>
  <source media="(prefers-color-scheme: dark)" srcset="rust/images/logo-dark.png">
  <img src="rust/images/logo-light.png" alt="OpenATC AI" width="600">
</picture>

# OpenATC AI

A local AI air-traffic controller for **X-Plane 12**: a staged, rule-driven controller with
free-text understanding, voice-in/voice-out, distinct controller voices, a copilot that reads
back clearances, and cabin/ground crew chat — all running on your own hardware.

> **Docs upkeep rule (maintainers):** every user-facing change updates this README *and*
> `docs/CHANGELOG.md` in the same commit. If you touch a setting, page, flow, or setup step,
> the docs change with it — no separate docs PRs, no drift.

## Contents

- [Features](#features)
- [Repository layout](#repository-layout)
- [Requirements](#requirements)
- [Setup](#setup)
  - [1. AI services (same machine or separate)](#1-ai-services-same-machine-or-separate)
  - [2. Engine (sim machine)](#2-engine-sim-machine)
  - [3. Plugin (sim machine)](#3-plugin-sim-machine)
  - [4. First run checklist](#4-first-run-checklist)
- [Using the plugin](#using-the-plugin)
  - [ATC page](#atc-page)
  - [Flight Plan, Taxi, Cruise, Arrival, Airports, Weather](#flight-plan-taxi-cruise-arrival-airports-weather)
  - [Settings reference](#settings-reference)
- [Configuration reference](#configuration-reference)
- [openatc-ai server (Rust, preview)](#openatc-ai-server-rust-preview)
- [Troubleshooting](#troubleshooting)
- [Architecture and docs](#architecture-and-docs)
- [Contributing](#contributing)
- [License](#license)

## Features

- **Staged local controller** (Parked → Clearance → Pushback → Taxi → Departure → Cruise →
  Arrival → Approach → Landed → TaxiIn → Finished) driven by live X-Plane telemetry. Read-only
  except assigned COM1 tuning and ground/cabin actions you explicitly enable.
- **Free-text requests** via on-device LLM intent classification, with a built-in command parser
  as fallback. The LLM never issues clearances — it maps words to intents; `core` decides.
- **Voice in, voice out**: mic recording → STT transcription into the message box (review before
  sending) → ATC replies spoken via TTS. Replay any ATC transmission.
- **Voice roster**: comma-separated kokoro voice pool with a Test/Remove popup; every airspace
  (`ICAO:Service`) draws a voice, skips recently used ones, and remembers it across restarts
  (`controllers.json`). Controller/copilot/pilot/cabin/ground each have their own voice, speed,
  delivery (Standard/Brisk/Urgent — never relaxed), and volumes.
- **CALLS panel**: ATC | CABIN | GND selector above the message box. Transmit to cabin or ground
  crew for conversational crew chat (needs AI on); ATC keeps the full controller.
- **Copilot**: reads back clearances (fixed template, or AI-drafted in a chosen personality),
  tunes assigned COM1, optional voice.
- **Realism tab**: Relaxed/Standard/Strict presets — verbatim readbacks, tuned-frequency and
  callsign requirements, phraseology corrections with student-mode explanations, frequency
  congestion (delays, standby, background chatter), mayday practice flows, emergency intent.
- **Radio sound**: bandpass, hiss, crackle, static bursts baked into every radio transmission.
  Cabin intercom and the wired ground interphone stay clean by default (per-role toggles).
- **SimBrief OFP import**, NOAA METAR map, airport/taxi-graph/frequency/navaid loading from
  installed scenery, demo session, persistent settings, session reset.
- **Desktop preview**: run the identical UI outside X-Plane for testing (Linux/Wayland).
- **Rust workspace** (`rust/`): settings schema, tri-OS platform layer, model manager, and the
  `openatc-ai` server (LLM slice live; STT/TTS slices in progress). See below.

## Repository layout

```
src/            C++ engine, plugin, UI, speech, core logic
include/openatc/ public headers (protocol, settings, core, UI)
tests/          core checks + integration tests
scripts/        build, packaging, engine test, speech gateway (Python), SDK fetch
aircraft/       (planned) per-aircraft TOML profiles + checklists
checklists/     (planned) shipped crew flows
rust/           Rust workspace: settings, platform, ai-core, openatc-ai server, models.toml
vendor/SDK/     X-Plane SDK 4.3 headers (downloaded, not committed)
docs/           ARCHITECTURE, ROADMAP, THIRD_PARTY, VALIDATION, CHANGELOG
```

## Requirements

| | Linux | macOS | Windows |
|---|---|---|---|
| Build | cmake, ninja, GCC/Clang, ALSA, OpenSSL dev | Xcode CLT, cmake, ninja, OpenSSL (Homebrew) | MSVC, cmake, ninja, OpenSSL (vcpkg) |
| Sim | X-Plane 12 (Steam or standalone) | X-Plane 12 | X-Plane 12 |
| AI services | Ollama **or** `openatc-ai` server (Rust) | Ollama Mac app **or** `openatc-ai` | Ollama Windows installer **or** `openatc-ai` |
| Voice services | Python 3.10+ `speech_gateway.py` (kokoro + faster-whisper) until the Rust TTS/STT slices land | same (venv) | same (venv) |
| Paste in plugin | `wl-copy`/`wl-paste` (`wl-clipboard` package) on Wayland | native | native |

Status notes, stated plainly: **Linux is the fully exercised platform.** Windows/macOS plugin
binaries are produced by CI but their in-sim validation is pending; macOS downloads run unsigned
(right-click → Open on first launch) and the mic needs Microphone permission (System Settings →
Privacy & Security). The X-Plane plugin targets SDK 4.3 with the compatibility renderer; the
SDK 4.4 native-renderer migration is on the roadmap.

## Setup

Three pieces: **AI services** (LLM + STT + TTS, anywhere reachable) → **engine** (sim machine,
loopback `:8087`) → **plugin** (sim machine, `Resources/plugins`). The plugin and engine must
share a machine (loopback HTTP); services may live on the same machine or a separate box on
your LAN — only the three base URLs in Settings change.

### 1. AI services (same machine or separate)

You need three OpenAI-compatible endpoints. Mix and match per service:

- **LLM** (`POST /v1/chat/completions`): Ollama with `qwen2.5:7b-instruct`
  (`ollama pull qwen2.5:7b-instruct`), **or** the Rust `openatc-ai` server
  (`rust/`: `cargo run -p openatc-ai -- --port 8099`; downloads Qwen GGUF itself on first
  start into `~/.local/share/openatc-ai/models`, `OPENATC_AI_MODELS` overrides).
  For LAN use, Ollama must bind off localhost once:
  `sudo mkdir -p /etc/systemd/system/ollama.service.d` +
  `Environment="OLLAMA_HOST=0.0.0.0:11434"` in `lan.conf`, then daemon-reload + restart.
- **STT + TTS**: the Python gateway until the Rust slices land —
  `python3 -m venv ~/.venvs/atc && source ~/.venvs/atc/bin/activate &&
  pip install -r scripts/speech-gateway-requirements.txt`,
  models (`kokoro-v1.0.onnx`, `voices-v1.0.bin`) beside `scripts/speech_gateway.py`,
  serve with `atc-speech-gateway.service` (user unit in `scripts/`, port `8099`).
  CUDA is used when the NVIDIA runtime is visible, CPU otherwise; the `/health` endpoint
  reports the actual providers so silent CPU fallback is visible.

Verify each from the sim machine with `curl` (`/health`, one `/v1/chat/completions` call,
one `/v1/audio/speech` call returning a valid WAV) before touching the UI.

### 2. Engine (sim machine)

```bash
bash scripts/fedora-build.sh plugin   # builds engine + plugin + tests + audit (Linux)
./build/wayland/open-atc-engine       # listens on http://127.0.0.1:8087, keep running
```

Start the engine **before** X-Plane. Settings persist to `~/.config/openatc/settings.json`
(`%APPDATA%/openatc` on Windows, `~/Library/Application Support/openatc` on macOS);
controller memory to `controllers.json` beside it. API keys (if your providers need them) come
from `OPENATC_AI_KEY` / `OPENATC_STT_KEY` / `OPENATC_TTS_KEY` on the engine only — never in
settings. `OPENATC_AI_URL` / `OPENATC_AI_MODEL` override the AI endpoint at startup.

macOS/Windows: build with CMake (`-DOPENATC_BUILD_PLUGIN=ON -DXPLANE_SDK=<sdk>`) or use the
CI zips; the engine binary runs the same way (`open-atc-engine` / `open-atc-engine.exe`).

### 3. Plugin (sim machine)

Copy the staged plugin into X-Plane **with the sim closed**:

- Linux (Steam): `~/.local/share/Steam/steamapps/common/X-Plane 12/Resources/plugins/OpenATC/`
- macOS: `~/Library/Application Support/Steam/steamapps/common/X-Plane 12/Resources/plugins/OpenATC/`
  (or `<X-Plane.app dir>/Resources/plugins/OpenATC/` for standalone installs)
- Windows: `<X-Plane 12>\Resources\plugins\OpenATC\`

The folder needs `64/lin.xpl` (Linux) / `64/win.xpl` (Windows) / `64/mac.xpl` (macOS). Back up
the old folder first; never keep two copies of the plugin folder inside `Resources/plugins`
(X-Plane loads both and warns about duplicates). Plugin binaries load at sim startup — a
running sim will not pick up a replaced `.xpl`.

### 4. First run checklist

1. Engine terminal shows `OpenATC AI … engine: http://127.0.0.1:8087`.
2. Launch X-Plane, load an apron. `Log.txt` shows `lin.xpl (org.openatc.development)` with no
   loader errors.
3. Plugins → OpenATC AI → Show/hide. Status reads `X-Plane connected` (if it says
   `Engine disconnected`, the engine isn't running).
4. Settings → AI: base URL + model, enable. Settings → STT/TTS: bases + models. Apply saves
   (edits also auto-save ~1 s after you stop typing, once connected).
5. Type `request altitude FL320` → Transmit → reply in transcript → **Replay ATC** speaks it.

## Using the plugin

### ATC page

- **CALLS selector** (`ATC | CABIN | GND`, above the message box): chooses who receives your
  transmission. ATC runs the full controller; CABIN/GROUND open conversational crew chat
  (needs AI on — with AI off you get a plain pointer, never silence).
- **Message box + Transmit** (or Enter): typed requests; STT results land here for review —
  nothing is ever sent automatically.
- **Request buttons**: contextual by flight stage (clearance, pushback, taxi, ready, altitude,
  descent, direct-to, approach, go-around, …). Unavailable actions stay hidden; with Realism
  Strict, out-of-stage or off-frequency requests are refused with the reason.
- **Mic on → speak → Mic off/Transcribe**: records (30 s max), posts audio for transcription,
  fills the box for review. Mic test lives in Settings → Audio and sends no audio anywhere.
- **Replay ATC**: repeats the newest controller transmission in its original voice.
- **Status line** (right of the buttons): `Generating speech…`, `playing`, or the exact failure
  reason — always read this first when audio misbehaves.
- Transcript rows show the speaker plus ATC position (`ATC · Tower`); crew lines show
  `CABIN`/`GROUND`; copilot readbacks show `COPILOT`.

### Flight Plan, Taxi, Cruise, Arrival, Airports, Weather

- **Flight Plan**: SimBrief Pilot ID → import OFP (route, weights, cruise, callsign), or hand-fill;
  validation errors name the bad field. Demo session available without the sim.
- **Taxi**: airport load (scenery priority), clearance → pushback → taxi approval gates the green
  route; stands, taxiways, parking layers toggleable.
- **Cruise / Arrival**: stage-appropriate requests (altitude, direct-to, descent, approach
  briefing); descent shows planned vs current profile.
- **Airports**: scenery-priority airport search with frequencies, navaids, procedures; loads the
  active airport for taxi graph + frequency selection.
- **Weather**: NOAA METAR map (route, ownship, stations, wind, clouds, labels). The engine
  `/weather` endpoint stays available for future ATC weather awareness.

### Settings reference

- **General**: X-Plane folder (auto-detected; engine learns it from the plugin), SimBrief ID,
  session save/restore, demo controls, Reset flight (clears flight state, keeps controller
  memory — airspace assignments are about places, not flights).
- **Copilot**: readbacks on/off, COM1 auto-tune (structured OpenATC assignments only; never
  barometer/autopilot; X-Plane's own ATC, Next ATC and VATSIM untouched).
- **AI**: enable, base URL (bare origin, no `/v1`), model. AI-off falls back to buttons and the
  built-in parser.
- **STT / TTS**: bases, models. Master speech speed (multiplies all voices).
- **Voices**: controller pool (comma-separated) + Test voices popup (Play per voice, Remove edits
  the pool), fallback voice, delivery (Standard/Brisk/Urgent, optional per-controller randomize),
  controller min/max speed (fixed draw per controller at assignment; urgent calls may exceed it
  until you Apply, which saves the new speed), copilot/pilot/cabin/ground voices, speeds,
  volumes, personalities (presets + custom text; empty = classic behavior).
- **Audio**: devices (Refresh list), master volume, input gain with live RMS meter, mic test,
  controller-voice test, radio FX (master toggle, 300–3400 Hz bandpass, hiss/crackle/static).
- **Realism**: Relaxed/Standard/Strict presets (Standard by default) + individual toggles —
  verbatim readbacks, tuned-frequency and callsign requirements, phraseology corrections with
  student-mode explanations, congestion (delays, standby, background chatter from other
  callsigns), mayday practice flows.
- **Session**: save/restore, reset. Settings apply on edit; Apply also flushes controller speeds.

Text fields support system clipboard copy/paste (Ctrl+C/V/X/A), including the 1024-char pool box
(Linux/Wayland uses `wl-copy`/`wl-paste`).

## Configuration reference

| Item | Linux | macOS | Windows |
|---|---|---|---|
| `settings.json` | `~/.config/openatc/` | `~/Library/Application Support/openatc/` | `%APPDATA%\openatc\` |
| `controllers.json` | same dir | same dir | same dir |
| Engine | `./build/wayland/open-atc-engine` | `./build/open-atc-engine` | `open-atc-engine.exe` |
| Engine port | `http://127.0.0.1:8087` (fixed loopback) | same | same |
| Service URLs | bare origins, e.g. `http://192.168.1.10:11434` (LAN) or `http://127.0.0.1:8099` (local) | same | same |
| Model names | `qwen2.5:7b-instruct`, `whisper-1`, `tts-1` (accepted by all servers; the Rust server maps the LLM name) | same | same |
| Provider keys | `OPENATC_AI_KEY`, `OPENATC_STT_KEY`, `OPENATC_TTS_KEY` (engine env only) | same | same (`setx` or session env) |
| AI models (Rust server) | `~/.local/share/openatc-ai/models` (`OPENATC_AI_MODELS` overrides) | `~/Library/Application Support/openatc-ai/models` | `%LOCALAPPDATA%\openatc-ai\models` |

Split-machine rule: plugin + engine share the sim machine; services go wherever has the GPU —
point the three base URLs at them. Keep TTS/STT bases on one gateway when using the Python
server (it serves both paths on one port).

## openatc-ai server (Rust, preview)

Single-box AI: one process embedding LLM inference (llama.cpp, Qwen 7B Q4_K_M today, CPU with
CUDA planned), whisper STT and kokoro TTS slices landing next — audio endpoints currently answer
`501` with a pointer to the Python gateway.

```bash
cd rust && cargo run -p openatc-ai -- --port 8099
```

First start downloads and verifies all pinned models (`models.toml` + `models.lock.toml`;
resume supported; corrupt files refetch). Contract: `/health` (per-model status + backend),
`/v1/chat/completions` (OpenAI shape, usage counts), `/v1/voices` (54 kokoro names).
Point the engine AI base at it to cut Ollama out — the engine needs no changes.

## Troubleshooting

| Symptom | Check |
|---|---|
| `Engine disconnected` / Apply says “connect first” / Transmit greyed | Engine not running — start it, plugin reconnects on its own |
| `AI endpoint unavailable` | AI base URL typo, model name wrong, or provider down; buttons still work |
| `Speech synthesis failed…` | TTS base/model wrong, voice unknown (falls back with a gateway-log warning), or key missing |
| `TTS response is not decodable audio` | Gateway WAV bug — report with the status text |
| `Transcription failed…` | STT base wrong, or audio upload empty (mic permission on macOS!) |
| `Cannot open default microphone` | No input device / permission; Refresh audio devices; test in Settings → Audio |
| Speech silent but test voice works | `Speak controller messages` (etc.) off; old transcript lines never retro-play — send a new request |
| Wrong-frequency / callsign refusals | Realism toggles doing their job — tune COM1 / include callsign, or relax the preset |
| Plugin missing / not loading | Wrong folder (see paths above), duplicate plugin folders, or sim was running during copy — check `Log.txt` for the `lin.xpl` line |
| Sim crash | Save `Log.txt` + engine log *before* relaunching; note what you clicked; the project keeps a known-good plugin backup one copy away |

## Architecture and docs

- `docs/ARCHITECTURE.md` — engine/plugin/UI boundaries, TLS isolation (plugin links no TLS),
  dev loop without the sim, hot-reload warning.
- `docs/ROADMAP.md` — what's next (SDK 4.4 renderer, win/mac validation, STT upgrade, crew
  flows, checklists, aircraft profiles).
- `docs/THIRD_PARTY.md` — pinned dependencies and versions.
- `docs/VALIDATION.md` — test gates (core checks, integration script, Linux dep audit).
- `docs/CHANGELOG.md` — per-release notes, updated with every user-facing change.

## Contributing

Aircraft profiles and checklists are data, not code (TOML, landing with the crew-flow work):
copy `generic/`, rename, fill in your datarefs/flows, test in verify mode first. Code changes
keep the module layout (`core`, `flight`, `speech`, `ui*`, `engine`, `plugin`), one statement
per line, comments that explain *why* (failure modes, constraints) — never restatements. Rust
code: edition 2024 idioms, `cargo fmt --check` + `cargo clippy -- -D warnings` clean.

## License

MIT — see LICENSE. Model weights (Qwen GGUF, whisper, kokoro) and the X-Plane SDK carry
their own licenses; the download scripts fetch them from their canonical sources.
