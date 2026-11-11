<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/branding/logo-dark.png">
  <img src="assets/branding/logo-light.png" alt="OpenATC AI" width="600">
</picture>

> [!IMPORTANT]
> **Early alpha — testers welcome.** Release testing is still limited. Windows testers have confirmed installation, remote-server connection, button radio checks and initial microphone/controller-voice tests inside X-Plane; a complete Windows flight is not yet verified, and callsign and situation/replay errors have been reported. A macOS tester has reported window failures and tested proposed source fixes; the integrated fixes still need a fresh Mac build and simulator check. Linux release packages still need installation and simulator testing. Building from source on Linux is the best-tested option so far and has completed a full IFR flight through landing and parking. Automated builds and packaging checks do not replace testing in the simulator. Please expect bugs and share your findings with the community.

# OpenATC AI

Free, open-source ATC and cockpit crew conversations for **X-Plane 12**. OpenATC combines a Rust controller engine, editable TOML phrasebooks, voice input/output, simulator telemetry and aircraft control profiles. You can speak, type or use the available request buttons.

The **AI server can run on another computer**. Keep its language model, speech recognition and speech synthesis away from the simulator's CPU and GPU, or run everything locally if you prefer. The included models run locally without subscriptions, API token payments or a cloud account. Internet access is needed for the first model download and optional online services such as SimBrief.

OpenATC's application code is MIT-licensed. Models, libraries, fonts and simulator data have their own terms; see [third-party notices](docs/THIRD_PARTY.md). This is an early-development flight-simulator project. A phrase in the library does not mean that every associated procedure, aircraft or regional rule has been implemented or validated.

Release uploads include installer/archive files and verified SHA-256 sidecars; flight replay reports are kept out of the release assets. Regular releases target Linux x64, Windows x64 and Apple Silicon macOS. Intel Mac builds are retained for explicitly requested manual builds and are disabled in the normal release matrix.

## Contents

- [Install and connect](#install-and-connect)
- [Joystick and keyboard bindings](#joystick-and-keyboard-bindings)
- [What is included](#what-is-included)
- [AI server, voices and GPU selection](#ai-server-voices-and-gpu-selection)
- [Fly an IFR flight](#fly-an-ifr-flight)
- [Copilot, cabin and ground crew](#copilot-cabin-and-ground-crew)
- [Missions and flight stories](#missions-and-flight-stories)
- [Emergencies and assistance](#emergencies-and-assistance)
- [Traffic and performance](#traffic-and-performance)
- [Maps, stations and simulator guidance](#maps-stations-and-simulator-guidance)
- [Settings and realism](#settings-and-realism)
- [Customise speech and aircraft](#customise-speech-and-aircraft)
- [Builds and packages](#builds-and-packages)
- [Development and validation](#development-and-validation)
- [Troubleshooting](#troubleshooting)

## Install and connect

> [!WARNING]
> **Windows + NVIDIA GPU:** on the computer running the AI server, install the [NVIDIA CUDA Toolkit](https://developer.nvidia.com/cuda-downloads) and [NVIDIA cuDNN](https://developer.nvidia.com/cudnn-downloads) before using CUDA-accelerated speech. The current bundled speech provider requires **CUDA 13 and cuDNN 9 built for CUDA 13**. Select Windows x86_64 and a compatible driver/package combination; use the download archives if needed. The display driver alone is not enough. Make sure the installed CUDA and cuDNN DLL directories are on `PATH`, then reopen the server. CPU speech does not require these downloads. See [NVIDIA compatibility](https://docs.nvidia.com/deeplearning/cudnn/backend/latest/reference/support-matrix.html) and the [GPU setup guide](docs/AI_SERVER.md#gpu-requirements-and-source-builds).

### Quick setup

| Step | What to do |
| --- | --- |
| 1. Install | Download the plugin and AI-server packages from [Releases](https://github.com/billpayne4x4/OpenATC-AI/releases). Close X-Plane, install the plugin into the **X-Plane root folder**, and keep the complete AI-server bundle together. |
| 2. Start the server | Open **OpenATC AI Server**. Complete the first-run wizard, allow the selected models to download, and wait until they are loaded. |
| 3. Connect the plugin | Open **Plugins → OpenATC → Settings → General**. For a local server use the defaults below; for a separate computer apply its LAN address to all three services. |
| 4. Check sound | Select microphone/speakers, test the microphone and controller voice, then try **Radio check**. Model downloads must finish before speech will work. |
| 5. Bind push-to-talk | In X-Plane **Settings → Joystick** or **Keyboard**, search for `openatc` and assign the commands in the [bindings table](#joystick-and-keyboard-bindings). |
| 6. Plan the flight | Import SimBrief or complete Planning, tune the published controller, and request clearance. See [Fly an IFR flight](#fly-an-ifr-flight). |

**Default plugin connections — AI server on the same computer:**

| Setting | URL | Port |
| --- | --- | --- |
| LLM | `http://127.0.0.1:8099` | 8099 |
| Speech recognition (STT) | `http://127.0.0.1:8099` | 8099 |
| Text to speech (TTS) | `http://127.0.0.1:8099` | 8099 |

- Enter the origin **without `/v1`**. Local-server API keys may stay empty.
- The default LLM model is `qwen2.5-7b`; STT uses `whisper-1` and TTS uses `tts-1` with Kokoro on the bundled server.
- A remote server needs its real LAN address, for example `http://192.168.1.10:8099`, and a LAN listener/firewall rule. `127.0.0.1` always means the computer making the request.
- The browser setup dashboard uses a separate management port shown when it starts. Use **8099**, not that dashboard port, in plugin AI settings.
- **Existing installations retain saved choices.** An upgrade does not replace old URLs, models or explicitly disabled speech. Review General, AI, STT / TTS and Voices settings once if upgrading from an earlier alpha.

| Fresh-install setting | Default |
| --- | --- |
| AI request understanding | On; bundled Qwen2.5 model selected |
| Controller, copilot and own-pilot speech | All on |
| Cabin and ground voices | Available with nonzero role volumes |
| Copilot radio readbacks, routine responses and handoff tuning | On |
| Heard reports from observed traffic | On; requires identified observed traffic |
| Autopilot target control | Off; enable explicitly if wanted |
| Informal pilot chat, humour and unsolicited pilot calls | Off; opt in when wanted |
| Experimental generated traffic and developer mode | Off |

### The three running components

1. **X-Plane plugin:** displays the interface, captures audio, plays speech, reads simulator state and applies supported aircraft controls on the simulator thread.
2. **Companion engine:** runs beside the simulator and owns the flight plan, controller state, readbacks, requests, monitoring and conversation. The plugin starts it automatically.
3. **Optional AI server:** runs the LLM, STT and TTS locally or on another machine. It exposes one HTTP service for all three.

The AI server still launches a bundled `openatc-stt` helper. The language and recognition libraries currently contain incompatible copies of a native dependency, so recognition remains in a child process. Kokoro and Kitten run inside the AI-server process. Install the supplied bundle rather than copying only the main executable.

### Install the plugin

For release downloads, follow the complete [installation and removal guide](docs/INSTALL.md). Windows offers Setup.exe and ZIP; macOS offers native app/DMG and ZIP; Linux plugin downloads contain terminal install.sh/uninstall.sh scripts in the ZIP. The AI server additionally offers Linux AppImage, DEB, RPM and Flatpak formats. Native Linux AI-server packages require glibc 2.39 or newer (Ubuntu 24.04 or equivalent); the plugin retains its Ubuntu 22.04 build baseline. Flatpak uses its packaged runtime.

Close X-Plane before replacing plugin files. A packaged plugin installer asks for the **X-Plane root folder**, not the aircraft folder. A complete plugin payload can also be copied into `X-Plane 12/Resources/plugins/OpenATC`.

For a source build:

```sh
cargo build --locked --release -p openatc-plugin -p openatc-engine
cargo xtask install-plugin "/path/to/X-Plane 12"
```

External Cargo dependencies use exact version requirements, and the committed `Cargo.lock` fixes the transitive dependency graph. Keep `--locked` on build and test commands. Dependency upgrades are deliberate changes: update the relevant manifest pins and lockfile together, then validate the affected components before installing.


Fresh Steam installations can be detected before their first launch; choose the X-Plane root folder, and the installer adds `Resources/plugins/OpenATC`. Standalone installations require a version log from one prior launch.

The installer checks speech files, makes a recoverable backup outside the plugin directory, and installs the plugin, engine and editable resources. It preserves personal settings and existing station-role overrides. Do not rename the engine executable into an `.xpl`: the simulator module and companion engine are different binaries.

The simulator module is `64/lin.xpl` on Linux, `64/win.xpl` on Windows and `64/mac.xpl` on macOS. Keep the rest of the OpenATC folder with it. Restart X-Plane and open **Plugins → OpenATC**.

### Saved server configuration

The redesigned dashboard is installed and has been reviewed on Fedora, including real model loading, downloads and settings application. The updated Windows server and installer build successfully; interactive Windows dashboard/install checks and macOS verification remain pending; see [Validation](docs/VALIDATION.md).

- The server supports Linux, Windows and macOS.
- Launching the packaged server opens its browser dashboard: the first launch offers a setup wizard, while later launches reuse your saved choices. **Edit settings** provides direct model, compute, speech and connection tabs; **Run setup wizard** remains available as a guided alternative. **Save and apply** validates and saves `server.toml`, then reloads the inference runtime automatically while the dashboard remains available.
- Load/unload, applying settings and deletion show a modal progress overlay; verification/download/loading stages remain visible until completion, and failures can be dismissed to correct the settings.
- Per-model **Load / Unload** controls each component; the top buttons control the whole model set.
- Changes reload inference automatically while keeping the dashboard open.
- The single **Models** panel includes download progress, cache locations, CPU/GPU status, RAM/VRAM observations and cached-model deletion.
- Supporting voice files are grouped with their model.
- Files cannot be deleted while their model is enabled in the running runtime.
- When language and synthesis are loaded together they share one process, so their RAM/VRAM observations are explicitly marked shared rather than attributed to each model separately.

- For source/headless use, `openatc-ai --manage-web` opens the dashboard; `--configure-web` opens it without starting the inference runtime automatically. `--no-browser` suppresses automatic browser opening. `--dashboard-port` selects a fixed management port, and `--dashboard-host` can bind to the server’s actual LAN address for trusted remote access; the default dashboard is loopback-only.
- This management port is separate from the plugin’s inference port, normally 8099.
- Both dashboards and inference are unauthenticated, so keep access on a trusted network. `openatc-ai --configure` still offers terminal setup.
- Settings use the platform state directory; `--config /path/to/server.toml` selects another file, and explicit inference command-line options override saved settings.
- Bare unattended inference services retain their existing startup behavior.

- The form is generated from [server/settings-schema.toml](server/settings-schema.toml).
- Labels, help, choices and defaults are editable; `--settings-schema` supplies an override.
- Settings include independent CPU choices for language, recognition and synthesis, model selection, context, GPU reserve and CPU thread counts.
- Qwen2.5 Instruct and Kokoro remain defaults.
- Automatic GPU context sizing is opt-in and warms speech before measuring headroom; fixed context remains the default for sharing a GPU with X-Plane.

- For another GGUF, fill the custom model URL, name, relative path, exact byte size and SHA-256 fields in the setup form, or use `llm-manifest = "/path/to/custom-model.toml"` in saved settings.
- Its one `[[model]]` entry specifies `name`, a contained `llm/*.gguf` path, an HTTPS `url`, byte `size` and SHA-256 `sha256`.
- Downloads use the existing verified cache.
- Plugin LLM requests explicitly cap replies at 512 tokens, matching the output allowance used by crew context checks.
- Custom models use their GGUF chat template; `chat-template` and `assistant-prefix` are advanced overrides.
- Model/backend compatibility still needs verification: adding a manifest is not proof that every GGUF architecture or quantisation works on every device.

- Role conversations use concise editable prompts, a bounded per-character social summary.
- Ordinary assistance, jokes, preferences and harmless fictional stories remain open-ended conversation.
- Mapped aircraft controls retain validation and execution acknowledgements.
- Social notes are not aircraft state and are cleared for a new flight.
- Long-flight social recall and joke variety remain under evaluation; summaries can lose or misstate personal details.
- The server advertises its allocated context and exposes `/v1/tokenize` for exact prompt budgeting.
- Authenticated requests use the same server key.
- When necessary, older exchanges and social notes are trimmed before the control catalogue; essential instructions and the current request are retained.
- A focused control-target pass checks the current request independently of conversational wording; validated targets still require aircraft acknowledgements before completion is reported.
- Optional social notes and older captain messages are removed before reducing the control catalogue when context is tight.
- Strict JSON-schema generation keeps replies, action arrays and memory fields typed.
- Context that cannot fit the remaining instructions is rejected rather than silently truncated.

### Start the AI server

From an extracted AI-server package:

```sh
# This computer only.
./openatc-launcher

# Accessible from other computers on your LAN.
./openatc-launcher --host 0.0.0.0 --port 8099
```

On Windows use `openatc-launcher.exe`; in PowerShell prefix it with `.\`. The launcher passes its options to the AI server. First startup downloads and verifies the selected models. Subsequent starts reuse the model cache. Keep `openatc-stt`, the supplied shared libraries and `espeak-ng-data` with the server.

In **Settings → General**, enter your **AI server address** and apply it to all three services. This preserves model and voice choices. Separate providers can still be configured under **AI** and **STT / TTS**. Example addresses:

```text
http://127.0.0.1:8099        same computer as X-Plane
http://SERVER-LAN-IP:8099   separate AI computer
```

Use an origin without `/v1`. If the server is remote, `127.0.0.1` refers to the simulator computer and will not reach it. Allow TCP 8099 on the server's local firewall. The bundled service has no authentication or TLS termination; bind it only where your intended clients can reach it. Local inference does not require provider API keys.

Check readiness from the simulator computer:

```sh
curl http://SERVER-LAN-IP:8099/health
curl http://SERVER-LAN-IP:8099/v1/voices
```

A running process is not enough: wait for a healthy response after model loading. The health response includes the selected TTS provider and each component's compute status. See [the AI server guide](docs/AI_SERVER.md) for build commands, GPU requirements, API examples and measurements.

### Select microphone, speakers and voices

In **Settings → Audio & microphone**, refresh devices and select the microphone and output. Test the input before flying. Linux supports PulseAudio/PipeWire, with ALSA fallback; recording lists exclude playback-monitor devices. System default follows the operating system's current routing. After changing hardware, refresh the list and reselect missing devices.

In **Settings → Voices & personalities**, refresh the provider's voice list. Test voices with the editable preview sentence before adding them to the controller pool. Role volume and voice effects are separate from the operating-system output volume.

**Speak my transmitted requests** controls synthesized playback of your own pilot transmissions and is on by default for new settings. Controller and copilot speech are also on. Cabin and ground replies use their own voice and volume settings. Existing saved switches are preserved; seeing text in the log does not confirm that playback is enabled in an older configuration.

### Data locations

| Data | Default or override |
| --- | --- |
| Engine settings and logs, Linux | `~/.config/openatc/settings.json`, `~/.config/openatc/engine.log` |
| Engine settings and logs, Windows | `%LOCALAPPDATA%/openatc` |
| Engine directory override | `OPENATC_CONFIG_DIR` |
| AI models, Linux | `~/.local/share/openatc-ai/models` |
| AI models on any OS | Platform application-data directory; override with `OPENATC_AI_MODELS` or `--models-dir` |
| Model integrity record | `models.lock.toml` in the model directory |
| Persistent controller identities | `controllers.json` in the engine configuration directory |
| Speech override | `OPENATC_SPEECH_DIR`, or a complete personal speech library |

Settings, downloaded models and controller identities are separate from packaged binaries. Back them up before moving machines. Optional third-party credentials belong in `OPENATC_AI_KEY`, `OPENATC_STT_KEY` and `OPENATC_TTS_KEY`, not in speech TOMLs. `OPENATC_AI_URL` and `OPENATC_AI_MODEL` override the engine's initial AI settings.

## Joystick and keyboard bindings

Open X-Plane **Settings → Joystick** to assign a controller button, or **Settings → Keyboard** to assign a key. Search for `openatc`. These are all seven commands registered by the plugin:

| Command | Action | How to use |
| --- | --- | --- |
| `openatc/toggle_window` | Show/hide OpenATC | Press once. Opening the window expands it. |
| `openatc/transmit_com1` | Speak on COM1 | Hold while speaking; release to transmit using COM1. |
| `openatc/transmit_com2` | Speak on COM2 | Hold while speaking; release to transmit using COM2. |
| `openatc/talk` | Speak to the selected radio/interphone recipient | Hold to record; release to send. Uses the current audio-panel routing, including mapped crew recipients. |
| `openatc/talk_copilot` | Speak directly to the copilot | Hold to record; release to send to the copilot. |
| `openatc/mic_push_to_talk` | Dictate into the message box | Hold to record; release to transcribe. Review the text and press Transmit when ready. |
| `openatc/copy_error` | Write the current error to X-Plane `Log.txt` | Press once; this writes a log entry rather than copying to the clipboard. |

- Assign a dedicated COM1 or COM2 command when you want an unambiguous radio button. Direct COM transmission is separate from cabin/ground interphone routing.
- The microphone must be selected and the STT server ready. Push-to-talk requires a tuned, receivable controller: off-station presses do not start recording or transcription, and typed ATC transmissions are refused before any engine request.
- Avoid assigning the same button to an X-Plane command as well as OpenATC. Finish editing a text field before using keyboard bindings.
- Radio tuning, the audio panel and other aircraft controls use the aircraft’s normal X-Plane bindings; OpenATC does not register additional joystick commands for every on-screen request.

## What is included

OpenATC brings the controller and crew into the same flight. Request your IFR clearance, taxi through runway crossings, check in with the next controller, then work through approach, landing and taxi to parking. Speak naturally, type a request, or use buttons that follow the current phase and controller's duties.

- **A helpful copilot:** spoken readbacks, handoff tuning and check-ins, aircraft controls and read-or-perform checklists. Replies wait for controller speech and are tied to the instruction being answered.
- **Cabin conversations:** ask for coffee, check on passengers, prepare the cabin for turbulence or request mapped door/slide actions. Editable personalities and wording provide variety, with restrained humour when the situation permits it.
- **Ground services:** ask to remove chocks and disconnect external power in one message. Aircraft controls are confirmed before the crew reports success.
- **Traffic you can hear and talk to:** choose native AI compatibility or suppress it, listen to background exchanges, and ask observed traffic about weather, landing conditions or crosswind. ATC can point out traffic, hold for taxi conflicts and protect occupied runways.
- **Stories you can create:** select an A320-family mission or edit your own triggers, dialogue, choices and outcomes with the mission form and advanced TOML editor.
- **An open local setup:** run the AI server on a second machine, choose voices, edit phrasebooks and aircraft profiles, and keep model inference local without paid API tokens.

| Area | Current behaviour |
| --- | --- |
| IFR departure | Planning/SimBrief import, clearance, readback checks, start-up/pushback permissions, taxi, runway crossings, Tower handoff and takeoff permission. |
| Airborne controller | Check-ins, sector handoffs, altitude requests, direct-to requests, heading/speed assignments, lateral deviations and selected procedure/runway amendments. Eligibility depends on phase, controller role and available navigation data. |
| Arrival | Separate approach and landing permissions, Tower check-in, touchdown/runway-exit monitoring, Ground handoff and taxi to a selected stand. Generated arrival planning can try later verified runway exits when an earlier exit cannot reach free parking. |
| Copilot | Optional spoken readbacks, frequency changes and check-ins, mapped aircraft controls, and two checklist modes. |
| Ground and cabin crew | Compound control/service requests, one confirmation after simulator acknowledgements, request history, refreshment requests and a simulated passenger cabin. |
| Missions | Eight A320-family flight stories, mission selection, a form/TOML editor, phase and probability triggers, choices and a flight journal. |
| Traffic | Simulator traffic inspection, optional native AI count control, taxi conflict holds, runway protection, limited approach spacing and observed-weather pilot conversations. |
| Speech | Local Whisper recognition, Kokoro synthesis by default, optional Kitten Mini 0.8, editable phrasebooks and optional LLM wording variety. |
| Simulator data | Published airport frequencies, combined controller duties, taxi networks, runways, stands, installed navigation data and controller coverage. |
| Interface | Resizable in-simulator window, Light/Dark themes, transparency; Planning, ATC, Radios, Taxi, Arrival, Airports, Traffic, Missions and Settings pages; developer Map and testing tools. |

These are implemented paths, not a claim of complete worldwide ATC coverage. Automated checks exercise important transitions and failure cases. Full aircraft-specific flight testing, separation, procedure interpretation and native platform acceptance remain ongoing; see [Validation](docs/VALIDATION.md) and [Remaining work](docs/ROADMAP.md).

Further work includes random mixed traffic, lightweight Bluebell rendering, remaining arrival sequencing and holding. The [roadmap](docs/ROADMAP.md) distinguishes these plans from implemented features.

## AI server, voices and GPU selection

### Included model choices

| Component | Default | Purpose |
| --- | --- | --- |
| Language model | Qwen2.5 7B Instruct, Q4_K_M | Interpret requests and produce optional natural wording. |
| Speech recognition | Whisper Tiny | Turn microphone audio into English text. |
| Speech synthesis | Kokoro v1, 82M parameters | 54 voice presets, including 28 English presets. |
| Optional synthesis | Kitten Mini 0.8, 80M parameters | Eight English presets in a smaller model download. |

Kokoro stays the default. Kitten Mini is selected explicitly; it is not the larger KittenTTS2 model. Only one TTS model is loaded by a server instance. Switching providers reloads the inference runtime through Save and apply; refresh the voices in the plugin afterwards. The model files are downloaded separately from the application.

```sh
openatc-ai --tts-provider kokoro
openatc-ai --tts-provider kitten
```

Kitten exposes Bella, Bruno, Hugo, Jasper, Kiki, Leo, Luna and Rosie through `/v1/voices`, using IDs such as `kitten-bella`. Kokoro's roster comes from its loaded voice table. The engine selects from the actual provider roster rather than assuming a fixed set of six names.

**English-language voices only** is enabled by default. It filters language, not accent. Disable it to audition international presets with English text. A model's number of supported languages is not its number of different voices. Kokoro's current OpenATC text preparation is English; non-English presets speaking English require listening tests and do not turn the controller into a multilingual service.

Controller voices are saved by controller identity and reused on later visits. New controllers use unused voices before reusing an exhausted pool. With eight Kitten presets, more than eight controllers necessarily share voices. A saved voice absent from the newly selected provider is replaced with an available one. The selected provider therefore affects both variety and pronunciation.

Traffic pilots are assigned a voice by callsign and keep it for the flight. A new callsign takes the least recently used pilot voice, so a used voice moves to the back of the queue rather than being picked again immediately. Ambient, generated and airport-chatter calls share the same assignment. Controller and pilot pools avoid each other where the provider has enough voices, and fall back to sharing when it does not.

### GPU preferred, CPU available

An accelerated build tries its GPU backend first and falls back to CPU if initialization or the TTS warm-up fails. There are four independent command-line overrides:

| Flag | Effect |
| --- | --- |
| `--force-cpu` | Force LLM, recognition and synthesis to CPU. |
| `--force-llm-cpu` | Force only the language model to CPU. |
| `--force-stt-cpu` | Force only Whisper to CPU. |
| `--force-tts-cpu` | Force only Kokoro or Kitten to CPU. |

For example, leave the larger LLM on the GPU while running speech synthesis on CPU:

```sh
openatc-ai --tts-provider kokoro --force-tts-cpu
openatc-ai --tts-provider kitten --force-tts-cpu
```

Or run without GPU inference:

```sh
openatc-ai --force-cpu
```

The global flag takes precedence over individual choices. CPU inference uses system RAM; it does not require dedicated VRAM. It can be slower, particularly for the LLM. Running the server on another computer is usually preferable to taking resources from a busy simulator.

GPU support depends on the compiled features and installed drivers. Linux/Windows native AI release jobs enable Vulkan for the LLM and Whisper, and CUDA for ONNX speech. macOS jobs enable Metal/Apple support and CoreML speech. CUDA TTS needs the matching NVIDIA CUDA/cuDNN runtime libraries; an NVIDIA driver alone is insufficient. AMD/Intel Vulkan can accelerate LLM/STT while TTS falls back to CPU. Plain source builds without these optional features remain usable on CPU.

The server warms up GPU speech before reporting ready because some CUDA libraries load only when inference first runs. `/health` reports a fallback reason if it cannot use the accelerator. Native startup logs identify the actual GPU: a `gpu-requested` status alone is not a VRAM measurement. Hardware requirements, measured usage and source-build flags are in [AI server setup and measurements](docs/AI_SERVER.md).

### Measured VRAM and speech speed

Measured **13 October 2026** on Fedora 44, an **RTX 2080 Ti (11,264 MiB / 11 GiB)**, NVIDIA driver **615.71.09**, Core i9-9900K and 32 GB RAM. Every run loaded Qwen2.5 7B Q4_K_M with a **4,096-token context**, Whisper Tiny and the selected TTS model. The GPU runs used Vulkan for Qwen/Whisper and CUDA for speech, with CUDA 13 and cuDNN 9.27. These are exact observations from this test, not fixed requirements for every installation.

| Configuration | AI/STT process VRAM after startup | Whole-device sampled peak | Speech generation, three requests |
| --- | ---: | ---: | ---: |
| Kokoro, all three on GPU | 5,520 MiB | **6,059 MiB** (5.92 GiB) | **0.15–0.20 s** |
| Kitten Mini, all three on GPU | 5,042 MiB | **5,499 MiB** (5.37 GiB) | **4.51–4.73 s** |
| Kokoro on CPU; LLM/STT on GPU | 4,794 MiB | 5,025 MiB | 1.90–1.95 s |
| Kitten on CPU; LLM/STT on GPU | 4,794 MiB | 4,926 MiB | 5.22–5.40 s |
| Kokoro, LLM and STT all forced to CPU | 8 MiB driver context; no inference tensors on GPU | 20 MiB | 1.78–1.97 s |

The sentence was “Charlie Golf Tango Lima Tango, turn left heading two seven zero. Climb and maintain five thousand feet.” Kokoro's `af_bella` produced 7.21 seconds of audio; Kitten's `kitten-bella` produced about 11.08 seconds. Both were requested at speed 1.0. Different delivery lengths mean this is not a listening-quality ranking. Kokoro was substantially faster here, so it remains the default; Kitten is available for its alternative voices and smaller download.

Startup figures sum the two GPU processes after model loading and the GPU speech warm-up. Peaks were sampled every 100 ms through three speech requests, a JSON completion and recognition of the same sample, with all models resident. Requests ran sequentially. The idle machine baseline was 1 MiB; NVIDIA also reported 429 MiB reserved separately. Sampling can miss shorter peaks, and concurrent inference or longer prompts/utterances can require more memory.

After these requests, the GPU processes retained 6,040 MiB for Kokoro and 5,274 MiB for Kitten, versus 4,802 MiB with TTS on CPU. The extra retained allocation was therefore approximately **1,238 MiB for Kokoro TTS** and **472 MiB for Kitten TTS** in this particular run. Those deltas include allocator/workspace effects and are not standalone model-weight requirements.

All tested configurations fit this 11 GiB card on a separate AI machine. Leave additional headroom if X-Plane or other applications share the GPU. The global CPU flag keeps inference on CPU even though a GPU-enabled binary can create a small driver context during backend discovery. A CPU-only build does not require an NVIDIA card. See [measurement reproduction](docs/AI_SERVER.md#measurements-and-reproduction) for the commands and limits.

### KittenTTS 2 evaluation

KittenTTS 2 has **47 voices (38 English and nine non-English)**. It is a different model from the eight-voice Mini 0.8 option above. It was tested separately on the same RTX 2080 Ti; it is not integrated into the OpenATC server.

The compact `emb4` language-model file is 492,114,467 bytes (469.3 MiB). That is not the complete runtime: the upstream GPU implementation also downloads a 1,064,875,036-byte audio decoder, voice assets and, when selected, a 38,817,934-byte student decoder. The language-model weights are expanded for inference.

With the model already loaded, the heading/climb sentence above took **5.62–5.69 seconds** to generate, compared with **0.17–0.29 seconds** in a fresh Kokoro check. The other tested Kitten instructions took **4.26–9.89 seconds**, depending on voice and text. Kitten added about **4.97–5.14 GiB** to GPU use. The whole device peaked at **10.40–10.58 GiB** while the existing LLM, Whisper and Kokoro also remained loaded. These totals include both TTS models and are not the memory requirement for replacing Kokoro with Kitten.

Both the default and smaller student decoders produced valid audio. Converting the language model to FP16 failed during generation, so that trial supplies no usable faster result. Kokoro remains the recommended built-in model for prompt radio replies. See [the evaluation details](docs/AI_SERVER.md#kittentts-2-evaluation) and [recorded measurements](docs/benchmarks/kitten-tts-2-2026-10-13.json).

A separate Rust native prototype also generated intelligible speech without Python. Its standard Q4 model is 1,447,458,656 bytes, plus a 312,339,253-byte student decoder and voice data. On the same i9-9900K, with four CPU threads, a takeoff instruction took **16.03–17.46 seconds** for 5.08–5.24 seconds of audio. The Vulkan GPU trials returned non-finite logits, so that path is not ready to ship. This prototype is not the selectable Mini provider and has not changed the default server or release dependencies. See the [native experiment](docs/AI_SERVER.md#native-kitten-experiment).

### How AI is allowed to act

The model does not issue simulator commands hidden in its prose. ATC interpretation returns bounded JSON, for example:

```json
{"intent":"altitude","altitudeFeet":7000,"waypoint":""}
```

The engine rejects unknown intents, fields and unsupported values. It keeps the original pilot text for readback validation. Model output cannot claim that a clearance was acknowledged, mark itself as controller-initiated, supply a clearance sequence, or write arbitrary datarefs.

Rust then checks the controller's duties, radio state, flight phase, pending exchange, route and constraints. A recognised request may be refused or clarified. Crew interpretation uses a separate bounded list of aircraft-profile control IDs; actual simulator confirmation is still required. Structured JSON improves interpretation without giving the language model control of flight-state transitions.

The bundled chat API supports grammar-constrained `response_format: {"type":"json_object"}` and supported `json_schema` formats. That constrains output structure, not the truth or operational correctness of its contents; validation remains in the engine. Unsupported formats or schema constructs return an error.

## Fly an IFR flight

### 1. Plan and request clearance

Create a flight in **Planning** or import a SimBrief OFP. Check the departure, destination, callsign, runway, route and cruise level. When no plan provides a callsign, Settings → General → **Default callsign** supplies it. A clearance request uses this flight automatically. Aircraft identification and flight callsign can differ, so verify the callsign shown in the plan.

Power the radio and tune a published station in **Channels**. Clearance duties are inferred from installed services: Delivery first, otherwise Ground, otherwise Tower. At an airport with only Tower, that controller can handle clearance and ground duties. Airports and frequencies are not hardcoded for test routes.

Older scenery frequencies restore the omitted final digit: `FREQ 11802` is **118.025 MHz**, so handoffs, copilot tuning, controller replies and request buttons agree with the aircraft radio. Modern 8.33 kHz `CHAN` records retain their exact channel numbers. See [X-Plane's frequency-format explanation](https://developer.x-plane.com/article/8-33-khz-radios-for-users-and-authors/).

Routine pressure calls use **QNH 1022**, without a unit suffix. Settings below 1000 hPa retain “hectopascals” to avoid ambiguity.

The clearance needs a correct readback. **ATC Auto Reply** submits the current pending reply; optional copilot replies can do it for you. A request button is a request, not permission to move. Unclear or incorrect readbacks leave the instruction pending. Routine clearances do not need to announce a list of fields to read back, but those fields remain checked.

A correct routine airborne or taxi readback receives a spoken “Readback correct.” and advances the clearance or taxi state. Incorrect or incomplete readbacks still receive a correction. Delivery can acknowledge the initial IFR clearance and give the next Ground contact instruction; that is a further operational instruction, not a required confirmation after every readback.

### 2. Start-up and pushback

After the IFR readback, Delivery directs you to Ground where one is published. Start-up and pushback are separate permissions; the quick requests reflect existing approvals and observed engine state. A start-up request should not remain available after the engines are running.

ATC approval does not operate the tug or engines. Address **Ground crew** for external power, chocks or the aircraft's pushback control. With ToLiss, set pushback distance and angle in its own panel first; OpenATC can start the configured sequence, but currently cannot apply a spoken distance or angle.

### 3. Taxi and cross intervening runways

Request taxi from Ground or the combined controller. OpenATC builds a route from installed scenery and names published taxiways where available. Read back the route and hold-short limit. The map displays pending routes in amber and approved routes in green; optional simulator arrows appear only for approved portions.

Approved taxiway arrows remain visible from stands within 1,200 metres of the route, even when the stand is outside the published taxi network. A dashed amber ramp connector on the map remains unverified and receives no simulator arrows. Guidance stays bounded to 64 nearby arrow instances.

Automatic crossing requests reuse the active airport data and cached controller voices. A refused crossing keeps its holding-point notification, so the controller says it once for that hold instead of repeating it on every telemetry frame. A request that produces no answer releases the notification, and a new pilot request, route change or traffic hold can ask again.

A Ground taxi request made from physical runway pavement identifies the occupied runway and directs the pilot to Tower for exit instructions, rather than suggesting an unrelated departure-runway backtrack. Ordinary gate taxi routing remains subject to the same runway protection.

Each intervening runway is a separate clearance limit. The controller can issue a crossing instruction when you approach the hold and traffic information permits it. A crossing clearance names the runway being crossed, which may differ from the departure runway. Read back the crossing before entering. Unknown traffic information does not count as a clear runway. The crossing check uses the time needed to taxi across, plus the clearance/readback and radio buffer, so a distant final that cannot reach the threshold in time does not block the runway. A landing aircraft that has already passed the crossing point and is moving away does not block the crossing behind it. An aircraft before or over the crossing, stopped past it, or still on final does block it. Generated traffic crosses at the taxi-turn speed (12 kt with the jet envelope) and switches its landing lights on for the crossing.

After the crossing, the copilot can report vacated at the observed completion point. The next taxi instruction replaces the route and requires its own readback. In manual mode physical completion can advance the crossing workflow; a vacated report is not treated as an altitude readback.

Published taxi networks are preferred. Where they are missing, painted scenery centerlines can provide a conservative fallback. Backtracking is used only when the authorized route runs along the runway. The engine does not invent taxiway names for unnamed scenery paths or treat a drawn path as runway-entry permission.

### 4. Holding point and takeoff

At the final departure hold, Ground sends you to the published Tower frequency. If already on Tower, stay with that controller. Reaching the hold does not invent a ready-for-departure call: report ready yourself or use an available supported copilot exchange.

Holding-point detection considers approach geometry and the aircraft's reference position; it does not require the aircraft to be perfectly stopped to recognise arrival. Tower checks runway, final-approach traffic and the runway sequence and either clears takeoff or explains the hold. The player and ambient traffic use the same takeoff decision: a clear runway with no wake interval and no conflict gets a direct takeoff clearance, a rolling departure or live wake interval gets line up and wait, and landing or short-final traffic gets a hold. A ready report is not consumed by a hold: the hold stays in force until the runway clears, and Tower then issues line up and wait or takeoff clearance without another ready call. If the runway is clear and no aircraft is lined up, Tower issues takeoff clearance directly. If a departure is already rolling, the next aircraft receives line up and wait immediately; if one is already lined up, the next holds short. A line-up clearance reserves the slot before the aircraft reaches the hold line, and a stopped aircraft on the runway keeps the next aircraft at the hold until it rolls. The aircraft physically at the hold is the queue leader, so a lined-up aircraft behind the player never keeps the player waiting. Wake spacing also produces line up and wait, followed automatically by takeoff clearance when the interval expires. Only one aircraft may hold in position on a runway, and a landing clearance on the same runway prevents a new line-up. Final-approach protection uses the time needed for the movement, not a fixed five-mile radius.

Generated departures keep the order in which they reached the holding point. An aircraft already lining up or rolling keeps the runway; a later arrival on a taxiway that joins nearer the threshold does not jump ahead. Ground starts the Tower handoff while the aircraft is still taxiing, so the aircraft can request ready immediately at the hold.

Generated radio keeps one durable station per published frequency, but its worker thread is short-lived: it exits when its trigger and transmission queues drain and restarts when a new aircraft, phase trigger or transmission arrives. Same-frequency speech stays ordered across the restart; different frequencies never wait on each other's queue.

Tower lines a departure up as soon as the runway surface is clear, even when traffic is still on final. The take-off clearance waits for same-runway arrival traffic to pass; an arrival on a parallel runway, including one on short final, does not withhold it. Once the roll has started, a late conflict does not cancel the take-off; the arrival or go-around is sequenced around the departing aircraft. A lined-up aircraft is not repeatedly told to hold, and a distant arrival does not block take-off if the departure will be airborne before it reaches the threshold.

A takeoff instruction may include known wind, pressure and departure guidance. The cleared altitude and departure constraints remain authoritative. Conversational radio lines and information-only wake cautions do not hold the takeoff clearance; the runway, wake and required-readback state govern it. The departure handoff is tied to the airborne workflow; accepting a takeoff readback alone should not send the aircraft to another controller while still parked at the hold.

### 5. Departure, enroute requests and constraints

- Altitude compliance allows a 300-foot band and a 30-second sustained-deviation delay; an aircraft climbing or descending toward its assigned level is not treated as non-compliant.
- The plugin supplies geometric, indicated and standard-pressure altitude separately.
- The engine compares flight-level clearances with standard-pressure altitude using the configured regional transition altitude, and lower assignments with the pilot's barometric altitude.
- Older clients fall back to geometric altitude when instrument readings are unavailable.
- Pilot check-ins use the indicated altitude.
- Debug conversation exports include all three readings for diagnosis.

Check in after tuning the assigned frequency. The airborne check-in includes current altitude and the assigned climb altitude when applicable. A completed handoff readback clears the handoff instruction so the receiving controller can issue the onward climb or route clearance. Handoffs use current and predicted sector coverage; receiving a frequency is not the same as owning the aircraft's sector. Published services that share an airport frequency are merged into one station, so a Tower and a co-located monitor cannot bounce the flight between them on the same frequency. When copilot tuning is enabled, a handoff moves the handoff radio to the recommended frequency after the instruction has been heard, and the readback is still accepted after that move. A frequency handoff does not withhold the next instruction while its readback is pending; the handoff completes when the radio moves and the readback is spoken. Other readbacks hold new instructions until heard, and the controller prompts again after 45 seconds. Audio is scoped to the tuned station: leaving a station stops its queued transmissions. Each ATC page keeps its own radio history, so COM2 traffic stays on the COM2 page, and retuning never hides earlier calls. The debug Map displays the controller geometry used by these decisions.

Supported request families include an altitude change, direct to a known fix, a lateral deviation, resuming the route, a speed request, a different approach, and departure/arrival runway amendments. Availability depends on phase and controller role. Requests that cannot be interpreted or supported should be clarified rather than silently applied.

Installed CIFP data supplies selected waypoint altitude restrictions. At a glide-path fix (`H`), the second published altitude is the glide-path altitude there and the first is the ceiling, so an aircraft on a 3 degree path crosses it between those two levels. A glide-slope intercept fix (`J`) keeps its first altitude as the minimum. SimBrief planned levels and step climbs are planning inputs, not permission to climb. The controller must issue an amended clearance, including continued climb/descent where appropriate. Descent clearances follow the planned vertical path where one is published, clamped to the waypoint's minimum and maximum window, so an at-or-below maximum does not keep the aircraft high until the next restriction. Strict constraints preserve published exact, minimum, maximum and window restrictions. Requested climbs also undergo a conservative descent-feasibility check toward the destination.

SimBrief's calculated TOC/TOD markers are excluded from the flown waypoint sequence; they are planning annotations, not mandatory aircraft fixes. Fly-by sequencing can observe the aircraft established on the next known straight leg after a turn rather than requiring a telemetry sample directly over the fix. Published fly-over requirements remain enforced. This prevents a passed planning marker or reversing turn from leaving descent lookahead anchored to route already behind the aircraft.

This is not a complete FMS procedure interpreter: curved/unnamed/vector legs, aircraft performance, winds and local minimum vectoring altitudes remain limitations. MORA corridor checks are conservative terrain references, not a substitute for every local radar vectoring rule. Review the [airborne architecture](docs/ARCHITECTURE.md#airborne-instructions-and-published-restrictions) before relying on a particular route type.

### 6. Approach, touchdown and taxi to parking

- SimBrief may supply the destination runway and STAR without an approach identifier.
- With `select_runway_ils = true` in `traffic/vertical.toml`, plan loading selects a published ILS for that runway when installed procedure and glide-slope data exist, preferring the higher published entry altitude between variants.
- The controller announces the expected approach during arrival.
- An explicitly selected approach takes precedence; the plugin does not read the approach selection directly from every aircraft's MCDU.
- Match the announced approach in the aircraft, or request a different published approach.
- Missing approach data prompts a request for the desired approach instead of silently completing the STAR with no approach permission.

Selected approach entry/final fixes are expanded from installed CIFP alongside the SID and STAR, with missed-approach legs kept out of the landing route. Off-course monitoring follows a fly-by turn while the aircraft is inside its turn radius and turning the cleared way; a turn away from the cleared turn is still corrected. Equal published glide-slope altitude fields retain their exact intercept restriction. Descent planning continues when telemetry enters the Approach phase until an approach clearance is actually issued; phase alone is not clearance. Once cleared and established on final within the approach-path tolerance, level monitoring permits the final descent instead of reinstating the earlier interception altitude. Off-final deviations remain monitored.

Approach variants remain distinct from runway designators in expectations, clearances and readbacks. For example, navdata `I35RY` is spoken as **ILS Yankee runway 35R**, not runway “35RY”; the right-runway designator remains intact. Approach names and variant pronunciations are editable in `speech/runtime/responses.toml`.

- Approach permission and landing permission are separate.
- Follow the selected arrival/approach and controller instructions.
- When the published ILS interception level is known, the controller amends a higher assigned level before issuing approach permission; approach clearance must not bypass the entry-level descent.
- Tower communications transfer uses established final position after approach clearance and does not wait for the aircraft to descend into Tower’s surface-control altitude band.
- Radio reception and destination identity still apply.
- When transferred to Tower, check in; a continue-approach instruction is not a landing clearance.
- Tower's landing decision also depends on available runway traffic information.

- Where no Approach sector is available, Center retains the uncleared arrival rather than handing descent responsibility prematurely to a surface-only Tower.
- Published entry restrictions use the shared descent planner for selected ILS, RNAV, localizer and VOR procedures; automatic selection currently covers ILS procedures.
- This does not remove the unsupported vector/arc and procedure-geometry limitations described above.
- The installed-data regression imports the actual Nice–Milan SimBrief document with its missing approach field, sequences the route, and checks the 4,000-foot amendment, approach clearance, Tower transfer and separate landing permission.
- Its accelerated telemetry and acknowledged readbacks do not replace native aircraft/audio acceptance.

Touchdown is detected from simulator telemetry. Tower names the first usable exit and its side, then includes the Ground contact and frequency when a separate Ground station is published. Without a separate Ground station, Tower keeps the arrival and the vacate instruction asks only for the runway-vacated report. The handoff follows the observed runway exit, not the first on-ground sample, and the combined instruction is not repeated. After Ground check-in, select a destination stand and request taxi; the clearance names the stand and known taxiways. Each intervening runway retains its own hold-short, crossing and readback requirements.

Select a destination stand using the parking list and its filter, then request taxi. Arrival taxi routes have the same runway-crossing and readback limits as departure routes. The flight reaches its completed state only when the approved parking route ends at the selected stand and the aircraft has stopped; stopping at an intermediate runway hold is not the end of the flight. Four complete HTTP-engine replay arrangements verify arrival through runway exit, Ground transfer, taxi-in crossings and parking. These tests do not establish native simulator acceptance.

Stand selection uses the loaded aircraft's wingspan override or ICAO type and the scenery's published A–F width class. An A320/A20N needs at least class C; its departure parking position is not used to guess its size. Too-small, unclassified and observed occupied stands are excluded. If the assigned stand becomes unavailable during taxi-in, Ground holds the aircraft and can issue a new route once stopped. That route needs a readback and retains every runway crossing limit. Static scenery aircraft are not included in the TCAS feed, so their occupancy cannot be guaranteed. OpenATC does not remove another traffic provider's aircraft.

Clearance squawks are allocated per flight from a simulated discrete-code pool and retained through handoffs. New flights avoid recent allocations. This is not a reproduction of each country's operational code allocation.

### Radio turn-taking

**Show COM2 communications** in Settings → Copilot adds a second ATC icon with a small **2** beside the first. Both pages use the same flight and station roster: a station keeps its controller voice when reached on either radio. COM2 has its own tuned channel and supports the compact communications view. The `openatc/transmit_com1` and `openatc/transmit_com2` X-Plane bindings target their named radios from any page; the existing `openatc/talk` binding retains the selected crew/radio workflow. Dedicated radio bindings bypass ATTN/MECH interphone selection. Push-to-talk captures the recipient and radio when recording starts.

The source's complete HTTP flight replay includes a COM2 flight from clearance through taxi to gate, checks that COM1 remains unchanged, and retains COM2 through readbacks and handoffs. Native aircraft audio-panel reception and third-party COM2 bindings still require simulator acceptance; these replays do not establish native audio or FPS.

Routine automatic ATC transmissions wait while the radio is occupied. Copilot readbacks play before their effects are committed; automatic tuning waits for the exchange, and check-in waits for telemetry to confirm the new frequency. An urgent short-final go-around preempts normal playback. These are separate steps so a fast model response cannot skip a pilot transmission.

**ATC Auto Reply** handles the pending response once. It does not generate permission for unrelated movement or answer background traffic. Quick buttons are filtered by tuned service, flight phase and applicable state; completed start-up/pushback permissions and duplicate readback actions are not intended to clutter the list. Every quick request, including crew requests, comes from `intents.toml`.

- The debug Map offers **Flight path**, showing the engine’s expanded route and installed SID/STAR fixes.
- Hover waypoints for altitude/flight-level and speed limits.
- A filed connection into the STAR’s initial fix stays continuous; genuinely unresolved vectors or curves retain dashed connectors.
- These are schematic rather than direct clearances.
- Controller volumes containing the aircraft at its current altitude receive a dark translucent tint derived from their border colour, independently of the tuned frequency.
- The tuned controller keeps its stronger border.
- Taxi arrows and holding-point markers appear when instructions are issued; readback still controls operational permission.

## Copilot, cabin and ground crew

The **Copilot** button always addresses the copilot directly, including when ATTN or MECH is selected. **Transmit** addresses the selected recipient. Default cabin and copilot prompts support warm conversation, follow-up questions and brief humour in quiet phases while preserving verified control outcomes.

- Takeoff callouts prepare the copilot's TTS audio during taxi.
- The sequence is **Airspeed alive → 80 knots → V1 → Rotate → Positive rate**.
- The first four calls use observed indicated airspeed; V1 and Rotate retain the aircraft’s configured V1 and VR values. “Positive rate” requires an observed takeoff roll followed by the aircraft being off the ground, above 5 feet AGL and climbing at least 100 ft/min for 0.3 seconds.
- It cannot trigger from airspeed or while weight remains on the wheels.
- V2 remains a performance/diagnostic value and is never spoken.
- Rotate takes priority over unfinished V1; equal or simultaneously crossed V1/VR use a preloaded combined clip.
- Instrument reads run at up to 50 Hz independently of the slower controller logic.
- Callouts have independent cockpit audio and produce one matching transcript entry.
- Wording, datarefs and confirmation thresholds live in `aircraft/callouts.toml`; older policy files inherit the new roll/climb defaults, and a legacy `v2_text` entry cannot enable V2 speech.
- Missing or invalid V1/VR bugs are skipped; the early roll calls and Positive rate remain available.
- ToLiss uses its performance V1 and VR datarefs alongside stock fallbacks; legacy display-pointer offsets are excluded.
- Native ToLiss/default-aircraft timing and audio still need confirmation.

- Crew conversation uses the LLM for open-ended food requests, jokes, family conversations and follow-up questions, without a service menu or spelling dictionary.
- Current aircraft-control proposals and role dialogue use separate bounded requests.
- Independent control interpretations are compared, and disagreements require quoted evidence from the current message.
- Rust validates role permissions, ranges, aircraft bindings and fresh observations.
- Mixed requests hold the conversational reply until the controls are acknowledged; successful outcomes use editable TOML confirmations without another generation step.
- Failed actions retain truthful failure wording.
- Continuity uses bounded exact captain quotes and recent role-specific chat history rather than generated personal summaries.
- Interpretation, conversation and optional review/retry calls have separate token checks with 512 output tokens reserved.
- Editable role instructions and examples guide tone and interpretation.
- Native sampling, action readbacks and acknowledgement delivery continue during radio speech and slow conversation requests.
- Small-model wording, mixed-request coverage, joke variety and copilot latency remain imperfect; native aircraft-control acceptance remains distinct from synthetic tests.
- Current guidance keeps crew replies warm while ending without unsolicited social questions or extra-service offers.
- A shared semantic check permits essential operational clarification and rhetorical questions within a requested joke; it applies to the attendant, ground services and copilot.
- Readable phase, ground/airborne/postlanding state, known flight duration and confirmed control outcomes remain authoritative.
- The newest conversation changes are installed in the Linux test build, and full conversation-quality acceptance remains open.

Role prompts in `prompts/prompts.toml` guide natural, distinct characters: friendly ramp and cabin crew, a capable conversational copilot, and concise professional ATC. Editable `crew_social_*` TOML rules handle greetings, thanks and farewells without turning them into preparation checks. With wording variety enabled, the LLM can phrase these social replies; TOML remains the fallback. Social examples are kept separate from operational examples, and a greeting combined with a control request does not discard that request.

**Copilot** addresses the copilot. **Transmit** follows the active radio/interphone role. The ToLiss A320neo profile observes ATTN/CAB and MECH/INT selections for cabin and ground routing. Check the displayed conversation role before transmitting.

The ToLiss profile marks latching ATTN/MECH and CAB/INT commands in `toggle_commands`: pressing a selected button again restores ATC. Momentary commands in other profiles can retain their recipient. Selecting a VHF radio also restores ATC. Start-up/pushback permission alone does not prevent ground services from disconnecting external power or removing chocks: those actions require a valid, stationary ground position and simulator confirmation. They are refused while moving.

ToLiss cabin-light on/off requests use the cabin brightness dataref: on is 100%, off is 0%, and an explicit percentage stays at that value. The ToLiss control's editable `instructions` in `aircraft/toliss_a320.toml` teach the interpreter to use brightness rather than a light switch or another light. Other aircraft retain their own mappings.

Crew requests can include up to eight explicit control actions, such as “remove chocks and disconnect external power.” The engine validates all targets against the loaded aircraft profile before sending a batch. Each action waits for simulator confirmation; a failure cancels the remaining actions. Unsupported or ambiguous clauses are clarified rather than partly executed and announced as complete.

Stock aircraft use standard X-Plane control mappings. Third-party TOMLs override controls where the aircraft uses its own datarefs or commands. **Settings → Aircraft controls** shows observed availability. A listed command does not prove that every aircraft exposes a reliable confirmation state: button-only mappings confirm a press, not a resulting switch position.

The copilot can change mapped cockpit settings, including heading, autopilot controls, gear and other supported systems. The available profile determines coverage. There is no unrestricted model-to-dataref access. Third-party aircraft can bypass standard simulator controls, so aircraft-specific live testing matters.

Checklists support two modes: the copilot reads a challenge and waits for your response, or you read an item and the copilot performs supported actions and responds. Checklist definitions belong with the aircraft; spoken pilot/copilot wording belongs in the speech library. The included checklists are examples and need operator/aircraft review before expansion.

Cabin conversation receives the live flight phase and ground state. With CABIN or ground services selected, both Talk and Transmit address that crew member. Copilot requests use the Crew page, where Transmit addresses the selected tab. Greetings and refreshment requests do not prepare the cabin for landing. Arming and cross-check requests still depend on the aircraft's mapped indications; an unconfirmed slide indication must not be announced as armed. See [Aircraft controls and checklists](docs/AIRCRAFT_CONTROLS.md) for editing examples and limitations.

Landing preparation and passenger-watch requests are cabin services rather than switch operations. They can be combined with light commands, and service-only requests do not wait for an unrelated control acknowledgement. The LLM interprets unfamiliar paraphrases using the same service/action schema; TOML examples guide it without requiring an exact phrase match. Cabin conversation uses a bounded set of style examples instead of sending the entire role phrasebook.

Role conversation is handled by the language model using editable role prompts, recent conversation and live facts. It interprets spelling mistakes, reordered requests, refreshments and humour without a service menu or replacement dictionary. Replies use a structured envelope that separates spoken dialogue from proposed aircraft controls. Controls still require profile validation and confirmed execution; ordinary conversation uses one model exchange. `cabin/response-policy.toml` keeps operational-authority checks separate from language understanding.

Requests such as “coffee, arm the doors and cross-check” keep the service request alongside the control actions. You can also combine a passenger update, “turn off cabin lights” and departure preparation in one request. The crew gives one combined result after the controls are acknowledged, including any unconfirmed items. Departure preparation acknowledges checks in progress rather than claiming they are complete. Recent requests, service counts and confirmed outcomes are retained separately: asking for something does not make it true. Repeat-coffee humour is limited to quiet, nonurgent cabin conversation.

The **simulated cabin** uses the imported passenger count when present, flight phase, seated/lavatory activity and turbulence preparation. It can discuss passengers resting or watching movies and follow up when the simulated cabin is ready. Unknown passenger counts remain unknown. This passenger activity is fictional; actual door/slide/seatbelt indications still come from the aircraft. Urgent mission incidents suppress cheerful all-clear reports and jokes.

Edit `cabin/behaviour.toml` to change service-cycle timing, lavatory activity, turbulence preparation time and the brightness threshold for resting reports. A personal copy in the engine configuration folder overrides the shipped file after an engine restart. Invalid policies are rejected with the file path and reason. Passenger activity follows simulator time when available, pauses with the flight and does not catch up after a large clock jump. Starting a new flight clears passenger activity while retaining your policy. Speech remains in the response TOML, including separate single-passenger and multiple-passenger lavatory reports.

## Missions and flight stories

Open **Missions** using the flag icon in the title bar. Choose a compatible flight story before taxi; loading a story leaves your IFR plan intact. The eight examples cover a medical priority landing, childbirth, rough air, parts transport, a nervous passenger, an island reunion, cabin smoke and an assisted arrival. Suggested routes are starting points for your own current flight plan, not prevalidated operational dispatches.

The mission editor has ordinary fields and an advanced TOML view. Events support flight phase, unpaused delay, probability, prior-choice conditions, passenger seat, alternative speech and captain responses. The journal records what occurred and which choices were made. An urgent mission permits an explicit emergency declaration even with ordinary emergency practice disabled; a cabin report does not declare one automatically. Full automatic diversion planning is still pending.

Files under `missions/` ship with the plugin. Personal copies are saved under the engine configuration folder's `missions/` directory and override matching IDs. `OPENATC_MISSIONS_DIR` adds an optional catalogue directory. Audio, video and image references with transcripts can be authored now, but media playback is not implemented and the example missions contain no media assets. See [Mission format and editor](missions/README.md).

## Emergencies and assistance

Declare an emergency or urgency explicitly, including when requesting a diversion, direct routing or descent. The request retains its emergency status instead of being reduced to an ordinary route change. Routine training penalties and discretionary traffic speed requests pause during a declared emergency.

Mission incidents can introduce a medical problem, childbirth or cabin smoke, with crew dialogue and captain choices. A cabin report does not declare an emergency for you or change your flight plan automatically. Urgent incidents suppress casual cabin humour and misleading all-clear reports. Full automatic diversion planning and mission-driven runway closures remain in development.

ATC can ask whether you need assistance after repeated deviations, and Ground can offer revised taxi routing from your stopped position. Existing IFR clearances remain active; accepting revised instructions still requires the appropriate readback.

## Traffic and performance

### TOML setup cache and Re-setup

The companion engine caches parsed plugin TOML in
`<data directory>/setup-cache/toml-v1.sqlite3`. Editable TOML remains the source of
truth: a content hash changes on every edit, and each consumer still deserializes
and validates its configuration. The cache covers distributed traffic, speech,
prompts, aircraft, cabin, mission, region, radio-station and intent TOML, plus the
existing configuration/environment override locations. The separate AI server's
model/server TOML and user settings JSON are outside this cache. The simulator
plugin performs no SQLite work. Engine calls outside startup/CPU workers continue
to parse directly.

In **Settings → Appearance → Developer tools**, enable debugging tools and press
**Re-setup**. The engine drops and recreates the TOML and scenery cache tables,
then imports the current TOML files again. Progress and named errors appear there.
Old workers cannot restore rows from before the reset. Scenery is rebuilt on
demand. Editable files, settings, plans, logs and active flights are preserved.
Restart the plugin after completion to reload every active policy; this button
does not change an existing flight's clearances or restart its traffic.

A setup panel opens automatically for the first configuration import and whenever
**Re-setup** is pressed. It shows the current file, completed/total file count,
errors and the result. **Continue in background** keeps the plugin available during
the scan; **Show setup progress** reopens the panel. Traffic preparation and scenery
cache progress appear below it. Their completion time depends on the installed
scenery, so they show stages and tile counts rather than an estimated percentage.
Use Re-setup after updating TOML files to rebuild their parsed cache from the edited
sources. The panel reports when a plugin restart is needed to apply every policy.

The TOML cache retains up to 128 MiB of parsed payloads/8,192 entries, with a
256 MiB SQLite page limit and 32 MiB source-file limit. A damaged or unavailable
cache falls back to source parsing. Setup scans are bounded and report oversized
or invalid files. Configuration errors retain their normal rejection behavior.

A release benchmark of operational responses, traffic profiles/routes and prompts
measured about **10.0 ms per direct parse batch versus 3.9 ms from SQLite** (30
iterations); initial cache population took 20.3 ms. This improves repeated parsing,
not the much larger scenery scan or complete traffic lifecycle. Native plugin UI
acceptance remains to be checked in X-Plane launched by the user through Steam.

### Traffic setup and smoother motion

**Settings → Traffic → Traffic setup** shows the current preparation stage, scenery
region, cache use and any cache error. Scenery is indexed on demand as traffic is
planned. The first visit still needs to parse the installed scenery; subsequent
visits can reuse the compiled data. A ready cache does not guarantee that every
flight passes the route, weather, terrain, obstacle and clearance checks.

The companion engine stores its rebuildable SQLite cache in
`scenery-index/scenery-v5.sqlite3` under the platform data directory: normally
`~/.local/share/openatc-ai` on Linux, `%LOCALAPPDATA%/openatc-ai` on Windows, or
`~/Library/Application Support/openatc-ai` on macOS. `OPENATC_AI_DATA_DIR` overrides
the data directory; `OPENATC_SCENERY_CACHE_DIR` selects an isolated cache directory.
The cache records source size/modification times and missing resources, scenery
pack order and parser version. Changes invalidate affected entries. Cache writes
are transactional and checksummed; a cache failure falls back to scenery parsing.
Entries are limited to 256 MiB, stored payloads to 1.5 GiB and the SQLite database
to 2 GiB, plus temporary transaction space. Planning retains at most 24 tile stacks
and 512 MiB per planner; this is not a limit on the entire engine's peak memory.
Unused tile stacks can leave RAM and be reloaded from SQLite. This avoids repeating
the scenery scan when the in-memory cache fills; the first visit still needs parsing.
Use **Scenery cache details → Clear scenery index** to rebuild it on demand.
Settings, flight plans and active flights are retained.

Version 5 joins the published autogen contour pieces into boundaries and holes,
retains object overhang and uses local terrain bounds for autogen tiles whose
ground-reference reach is known. Interior ridges and building/tree heights remain
protected. Shared or unresolved ground references retain the conservative area bound.
Route refusals include both endpoints, heights and the aircraft envelope for diagnosis.

**Traffic update rate** offers Every frame, Every N frames and Hertz. The default
is now Every frame; saved explicit choices are retained. A short 750 ms sample
buffer smooths movement between engine updates, including when TCAS is active.
Interpolation follows the simulator clock with a monotonic playhead, so an early
or late snapshot cannot make an aircraft surge or move backwards. If updates stop,
aircraft hold at the last received position without extrapolating past a clearance
limit. The delay affects displayed generated traffic and TCAS; controller decisions
continue to use current engine positions. Visual smoothness, takeoff/landing and
frame-time acceptance still require an X-Plane check.

Generated aircraft use X-Plane's instance renderer and a separate TCAS publisher.
TCAS receives only populated slots, with unique nonzero 24-bit identities before
their positions. Ownship stays in slot zero. Targets refresh every frame; airborne
traffic reports altitude even with its engine stopped. OpenATC yields TCAS ownership
to another traffic provider and waits for availability before retrying. These are
the requirements of the [TCAS override API](https://developer.x-plane.com/article/overriding-tcas-and-providing-traffic-information/)
and [X-Plane 12.4.1 TCAS II guidance](https://developer.x-plane.com/article/tcas-resolution-advisories-and-plugin-traffic/).

Generated CSL models animate through every movement phase. Gear, flaps, spoilers,
steering, wheel rotation, pitch and bank follow the current stage; beacon and
navigation lights run with the engines, and landing lights follow the Airservices
runway-safety guidance: on for runway crossings, approach, landing and takeoff, off
for line-up and ordinary taxiing. That crossing practice is national guidance rather
than an ICAO-wide rule. See
[CSL animation and runway lights](#csl-animation-and-runway-lights) for the phase
table, configuration keys and the speed used by generated runway crossings.

Generated Ground, Tower and Delivery positions work independently. A player
transmission blocks a controller only on the frequency in use, not every generated
position.

Generated traffic starts with a mixture of nearby approaches, airborne routes and
surface activity when enough protected routes are available. The fast apron first
places aircraft at real stands. They stay parked until a complete departure is ready,
so an aircraft without an onward route cannot block the runway queue. Startup taxi
traffic comes from fully prepared flights. Connecting a departure preserves the
aircraft's identity, position and existing taxi route; the front of a compatible
taxi queue receives its onward route first. Once a flight is ready, gate preparation
takes 5–20 simulated seconds before the normal clearance and readback. Planning,
occupied taxiways, runway traffic and wake can extend the wait.

Additional flights join the live movement world without resetting its clock or
clearances. Replacements respect the category sliders and the 63-aircraft cap.
After startup, airborne replacements enter in the outer half of the display area
(20–40 NM with the supplied policy). Distant traffic must remain beyond the outer
boundary for 60 simulated seconds, be moving away, be inaudible and have no route
returning to the player's area before its slot is recycled. Active runway movements,
holds, recovery routes and radio exchanges are retained. Turnarounds are planned in
the background while other aircraft continue moving.

Installed-data tests and native simulator acceptance are recorded separately in
[validation](docs/VALIDATION.md). Mixed cargo/GA coverage and native visibility,
audio, smoothness and frame times remain acceptance requirements.

In `traffic/airlines.toml`, `route_policy = "listed"` keeps an operator's listed
route catalogue exclusive when present. FedEx and UPS use `"hub_spokes"` to also
model connections through their configured hubs; a partial European route sample
therefore does not suppress their US hub traffic. These spokes are simulated
network choices, not published schedules. All model, airport, route and protection
checks still apply.

Fleet selection checks the runway chosen for the current flow before weighting
eligible aircraft; an unused longer parallel runway does not qualify a heavier
aircraft. GA can use its separately prepared departure candidates. Full landing,
braking, taxi, stand and scenery checks still determine whether a flight is released.

Discovery divides its survey budget between configured operator links and nearby
fields, considering requested traffic categories separately. A worker shares one
scenery planner across airport directions to avoid repeating static checks.

Scene publication keeps render/radio identities and movement ownership aligned:
a rejected or hidden aircraft no longer retains a live stand reservation. Moving apron aircraft retain their routes, reservations and radio progress during
publication. Conservative planning exclusions still need review.

The active acceptance queue includes complete generated VFR/CTAF flights, wake
warning/wait/release, mixed-speed arrivals and eventual landing. Source now includes
predictive arrival speed control, a player traffic-holding setting and separate
reason-based percentage chances for traffic questions and ATC shortcut offers.
These source paths still require integrated production and native verification;
they do not establish that the complete traffic system works in X-Plane.


**Suppress X-Plane AI traffic** is enabled by default in Traffic settings. While connected, OpenATC requests control of native AI and reduces its active count to ownship; another traffic plugin retains priority. Generated traffic uses the supported TCAS override instead. Disable suppression to use the optional native AI amount controls. The plugin also mutes X-Plane's native COM radio audio while the companion engine is connected, preserving each panel's previous volume for disconnect or plugin disable; OpenATC speech uses its own audio output.

X-Plane's **Draw Parked Aircraft** population and aircraft embedded in scenery are separate from flying AI. Turn off Draw Parked Aircraft in X-Plane when replacing that population with a traffic plugin. OpenATC does not remove scenery objects or modify airport files. Generated parked aircraft and their later departures must share stand reservations and verified movement paths; that complete lifecycle is still under development. An amber map connector is an aid across an unmapped apron, not verified pavement or permission to cross a runway.

**Informal pilot conversations** are optional under Settings → Realism. Choose an identified aircraft on Traffic and type a message; weather/landing/crosswind buttons remain available separately. **Allow light humour** enables restrained friendly replies. **Allow other pilots to call me** permits an operational weather question when fresh nearby surface observations show strong wind or low visibility. It requires a known matching radio frequency and a quiet, noncritical phase. Surface weather is a reason to ask for a report, not evidence of weather aloft. These are simulated conversations, not multiplayer transmissions. Clearances, pending readbacks, emergencies, takeoff/landing, runway crossings and other critical situations take priority. Replies do not issue permissions or operate aircraft controls.

Traffic calls and ATC shortcuts have separate **0–100% chance** settings. The plugin
finds a useful eligible reason before rolling: 0% suppresses optional offers;
100% selects eligible opportunities but never creates a reason or overrides radio
priority. A pilot/airport/weather-concern episode and a shortcut route-fix pair
are each attempted once, including unsuccessful rolls, with cooldown between
opportunities. A weather concern that ends and later returns can be considered
again. Direct questions and necessary safety instructions are never chance-based.

Trigger words, response priority, humour flags and fact conditions are editable in the `traffic_chat_*` entries of `speech/runtime/responses.toml`. `traffic/behaviour.toml` controls quiet phases, timing, range, history and pilot personality within validated limits. Personal overrides belong in the engine configuration's `traffic` folder and load on a new flight or engine restart. General free-text answers use the configured language model; without it, editable matching rules provide brief fallback replies.

Open **Traffic** from the visible page tabs to see simulator-reported aircraft. **Settings → Traffic** holds model downloads, aircraft amount and the separate **Hear simulated traffic pilot reports** and **Talk to simulated traffic pilots** switches. Select an identified aircraft to ask about weather, its landing or crosswind. Replies use recent simulator weather and observed landing history; missing information gets an honest unavailable response. These are simulated pilot conversations, not messages sent to multiplayer users.

**Airport radio chatter** plays short background Ground and Tower exchanges when you are airborne above a receivable station and tuned to it. Calls use the installed airport's real runway designators, taxiway names and operator codes, and never reuse a live traffic aircraft's callsign; no 3D aircraft, TCAS targets or calculated taxi routes are created. COM1 and COM2 can carry separate sessions, and both radios are audible independently. Live traffic calls take priority over synthetic chatter on the same frequency. The rate follows the traffic mix and congestion setting: quiet traffic gives longer gaps, busy traffic shorter ones. Chatter yields to your own transmissions and stops when you retune, leave reception, descend below 1,000 ft above the station, or switch the setting off. The setting is `airportChatter` and defaults on.

**Hear simulated traffic pilot reports** adds en-route position reports from the traffic drawn on your map. While you are airborne and tuned to an Approach or Center frequency, the nearest flight within 60 NM that reports on that frequency calls its position: altitude, and distance and direction from the station. The controller acknowledges. The callsign and position come from that flight, so the call matches the aircraft you can see. Reports are paced by the congestion setting, and each flight reports again only after five minutes. Nothing is spoken on a frequency you are not receiving.

When the player gives way, ATC says so on the ground frequency the player is receiving: a moving player gives way to crossing traffic on its right, and ATC says continue once the conflict clears. Crossing traffic on the player's right gets the same give-way instruction the other way round.

**Add local pattern traffic** adds a circuit arrival and two departures at your airport when the network released fewer than two flights. The pattern flies on the Tower frequency with a clearance and a readback. It defaults on, so the airport is not empty while the procedure data cannot release a complete city pair.

**Settings → Traffic → Traffic mix** controls the relative airline, regional, freight and general-aviation shares. Set **Regional** to zero to remove regional turboprops and regional jets, or **General aviation** to zero to remove light aircraft and business jets. The ambient layer rolls the general-aviation share per aircraft, so the mix varies rather than switching models at a fixed interval.

Ambient traffic is airport-scoped and needs no flight plan: it taxis, departs, arrives and recycles around the loaded airport while the player is on the ground, including during taxi. Departures are ordered by distance to the holding point, so a curved taxiway cannot put the player ahead of an aircraft already at the hold. Ambient traffic remains around the active airport and does not follow the player en route. En-route and STAR traffic comes from **generated traffic**, which prepares complete flights around the loaded plan where the installed procedures allow them. Where a missed approach ends in a climb-out or a vector instead of a published hold, such as Sydney's, the recovery flies the published climb to its level and then holds at the end of that path. That hold is an engine fallback, not published procedure: right turns, one-minute legs and the published level. It is enabled automatically for a loaded plan unless either automatic enable or generated traffic is switched off. The Traffic page shows its on/off state, provides the switch, and lists each nearby aircraft's ground speed, COM1 and COM2 frequencies.

Generated and ambient aircraft share one published traffic set. Each layer updates only its own entries, so a paused or incomplete layer cannot hide the other, and the map, table, radio and 3D models all read the same list. When the active airport changes, the previous airport's generated flights are retired rather than kept alongside the arrival scene. Simulator echoes are matched with punctuation removed, so `F-OMBT` and `FOMBT` cannot appear as two aircraft.

**Set X-Plane AI traffic amount** adjusts the native active-aircraft count, up to 19 and limited by available simulator aircraft. It is off by default, leaves flight models enabled, yields to other traffic plugins and restores the previous count when disabled. Native AI flies its own simulator routes and consumes flight-model CPU. Start with a small count and compare X-Plane's performance readings. The SDK exposes aircraft ownership/count and AI control, but no documented interface for handing arbitrary per-aircraft IFR plans to native ATC. See [XPLMPlanes](https://developer.x-plane.com/sdk/XPLMPlanes/).

- OpenATC accepts up to 63 simulator targets and checks up to 126 combined simulator/generated observations; pilot history updates at most once per second. Wake, landing, crossing, takeoff and traffic-advisory checks use that combined view, so a light GA or regional turboprop ahead of the player gets the correct wake category and spacing.
- It can hold taxiing aircraft for an observed conflict and resume when clear, protect individual runway crossings, and issue a short-final go-around for runway conflicts.
- Crossing completion and arrival Ground handoff include aircraft dimensions and the relevant holding line, so a centre point outside the runway does not immediately mean the tail is clear.
- Unknown aircraft use conservative dimensions.
- Automatic speed spacing uses editable indicated-speed envelopes for piston GA, turboprops, regional jets and airliners. The shared predictor considers reaction/deceleration, final-speed transition, wake/surveillance planning distance, following traffic and threshold timing.
- It seeks an achievable complete speed solution in the configured 8–20 NM window below 10,000 feet, preserves published limits and releases by five miles with the shipped policy. Australia receives no new speed instruction once final approach is cleared. Unknown regional/type data refuses an automatic solution.
- Generated pilots read back a proposed reduction before movement applies it. It preserves route authority, uses the movement model's acceleration/braking and leaves final/recovery speeds unchanged. Fresh traffic and accepted route identity are checked again after readback.
- It does not accelerate an aircraft towards a nearby leader.
- Pilots can answer **Unable**; it does not repeatedly offer the same rejected speed.
- When speed cannot solve the predicted encounter, the plugin does not force an airliner down to a light aircraft's speed. General protected vector/resequencing and complete generated/player parity remain unfinished; late runway/wake conflicts retain their existing go-around checks.

The `[spacing]`, `[spacing.prediction]` and each aircraft's `spacing_speed` in `traffic/aircraft.toml` configure these thresholds, planning margins and nominal speed ranges. These predictions do not replace actual runway/wake checks. Omit `spacing_speed` to disable automatic offers for that aircraft. Profile reloads publish a complete validated snapshot; controller checks retain the previous valid policy while a reload runs. Invalid initial overrides disable these offers rather than silently enabling fallback speeds. An approach clearance explicitly releases a prior assigned speed when it is not restated; published restrictions remain applicable.

**Allow ATC to assign traffic holding**, under Realism → Arrival sequencing, controls
new discretionary player delay holds. Turning it off retains an accepted hold,
published missed approach and safety separation. The implemented arrival path
requires an acknowledged IFR clearance, a named upcoming fix suitable for direct
entry, level flight below 10,000 feet, fresh weather, terrain margins and published
controller coverage for the entire pattern. Missing protection refuses the hold.
The assessment uses the same combined traffic picture as the map, so OpenATC's ambient and generated aircraft can cause a hold instead of only the simulator's own traffic. It specifies the fix/course/turns/level/one-minute legs/maximum speed and a UTC expected-further-clearance time, then validates readback, including strict mode. Unknown traffic
climb profiles receive horizontal corridor protection at every level. Release
requires stable clear spacing and a fresh onward clearance/readback; the player
leaves at the inbound fix crossing. Pause/time jumps cannot silently release it,
and the expected time is never permission to leave. This is a restricted direct-entry
implementation; general published/parallel/teardrop entry, vectors and full native
flight acceptance remain open.

- **Wake protection** uses editable tables in `traffic/wake/`: `runways.toml` holds the shared runway relationships and each standard has its own file (`icao.toml`, `faa.toml`, `recat_eu.toml`, `mats.toml`). The **Wake separation** setting selects a table or `auto`, which uses FAA at US airports, MATS at Australian airports and ICAO elsewhere. Aircraft category comes from the simulator when reported, then from the aircraft profile. The single `traffic/wake.toml` remains the bundled fallback when the folder is absent.
- Observed departures can produce a **Hold for wake turbulence** instruction, with different intervals for full-length, intersection and opposite-direction departures and qualifying close parallel runways. A non-zero interval is spoken to the affected aircraft; a zero interval produces no warning, so a light GA leader does not cause a blanket wake caution.
- Timing uses simulator flight time, stops while paused and does not expire instantly after a clock jump.
- Starting the interval at observed liftoff is conservative; no pilot waiver is assumed.
- Australian heavy-behind-heavy departures use a live distance check; a timer or a missing leader cannot satisfy it. Missing verified medium-aircraft MTOW conservatively applies the Medium→Light protection to all supported Medium types.
- Approach checks consider the aircraft pair, closing speed, parallel runway geometry and displaced landing thresholds.
- Insufficient spacing at the preceding aircraft's threshold passage remains a conflict after it touches down or disappears from the feed, and can require a go-around.
- These checks follow the [FAA departure rules](https://www.faa.gov/air_traffic/publications/atpubs/atc_html/chap3_section_9.html) and [approach wake table](https://www.faa.gov/air_traffic/publications/atpubs/atc_html/chap5_section_5.html).
- Australian references are [MATS 77.3](https://www.airservices.gov.au/mats/docs/nos-saf-2000.pdf), effective 3 September–25 November 2026, §§9.5, 10.6 and 11.1. The arrival predictor uses a conservative 5 NM Australian surveillance planning floor; it does not infer eligibility for a local 3 NM service.
- The represented US terminal service models the FAA FUSION 3 NM standard; Center-controlled generated arrivals and player-hold release use 5 NM. Airport distance alone does not establish eligibility for reduced radar separation. These are explicit simulator service assumptions, with wake separation applied in addition.
- Intersecting flight paths, preceding low/missed approaches and complete worldwide separation are still unfinished.

- **Download and install Bluebell** downloads the publisher's 12 DDS archives and installs them for you. **Settings → Traffic** shows the current archive, downloaded bytes and extraction progress, with Cancel and Retry controls.
- Completed archives are reused after an interrupted attempt; an existing installation is replaced only after all archives have extracted successfully.
- Allow about 6 GB of free disk space for downloads and installation, or more when replacing an existing pack.
- Models live in `Resources/plugins/OpenATC/Resources/CSL/Bluebell` under your selected X-Plane folder.
- The companion engine performs the network and disk work off the simulator thread.

- Downloads come directly from the [Bluebell publisher's public distribution](https://github.com/oktal3700/bluebell).
- Textures retain their original licences and are not bundled in OpenATC releases.
- A publisher quota or network failure appears in Settings → Traffic and can be retried. **Scan installed model catalogue** indexes available aircraft types, operators and liveries on a background worker.
- Matching prefers the requested type/operator, then an unmarked model of the same type; it does not silently substitute a competing airline.
- Incomplete models and unsupported texture overrides are reported in the catalogue diagnostics.
- Native AI uses X-Plane aircraft, not CSL objects; lightweight CSL rendering and randomly generated flights remain unfinished.
- Installing Bluebell alone does not create traffic.

- **Traffic advisories** use observed aircraft position, heading, speed and altitude to predict encounters up to two minutes ahead.
- Calls include clock position, range, direction, type when known and indicated altitude.
- Several encounters are tracked independently, so an earlier call cannot hide a more urgent aircraft.
- Urgent alerts can interrupt ordinary speech while preserving the outstanding clearance; routine calls wait their turn.
- The ATC page offers **Traffic in sight** and **Negative contact** for the most recently called traffic on the tuned frequency.
- Follow-up calls distinguish traffic that is no longer a factor from traffic that is no longer observed.
- Reporting traffic in sight does not grant visual separation or acknowledge another clearance.
- These calls remain available with background traffic chatter off.
- Trigger thresholds are conservative simulator heuristics, not a complete regional separation system or a replacement for TCAS.

Copilot playback and acceptance are separate steps. Repeated preparation requests reuse the pending transmission, and the UI identifies the exact transmission when reporting playback complete. A delayed completion cannot acknowledge a replacement reply or one prepared on another frequency. Live audible timing still needs simulator acceptance.

- **Background CPU workers** controls the engine's bounded scenery, navigation and model-indexing pool on Linux, Windows and macOS.
- Zero selects automatic: half the available logical CPU threads, at least one and at most four.
- Set one to limit background concurrency, or choose a higher limit up to sixteen, capped by available parallelism.
- This does not change traffic density.
- The pool starts on demand, its waiting queue holds at most twice the worker count, and an overloaded request returns a retryable error instead of blocking the simulator.
- Changes apply when the companion engine restarts. `/health` reports the active `cpuWorkers` value.
- SDK calls remain on the simulator thread.

### Traffic lifecycle

- **Traffic lifecycle** in Traffic settings controls how many aircraft are moving on the ground and how long an airport stays active after you leave.
- **Taxiing aircraft** sets a minimum and maximum, 4 and 6 by default. When traffic is full the engine rolls one target in that range and keeps it for the airport and settings. Seeding tries valid taxiway nodes until the target is met; a node with no surface route is skipped rather than reducing the density. Pushback and taxi-out count; the runway queue does not. Cargo weight participates in the ambient mix alongside airline, regional and GA traffic.
- **Airliner, turboprop and light-aircraft circuit radii** default to 30, 20 and 10 NM. Inside the radius, ground, approach and departing traffic stays alive. Beyond it, those slots are recycled into airborne traffic around you.
- Landed arrivals taxi to their stand, pause briefly, then despawn and recycle the slot to another arrival on final. Generated traffic does not leave parked rows; static scenery provides those.
- Gate departures are transient. They request IFR clearance from Delivery, then start and push back on Ground, then request taxi. They are not kept parked.
- Ground hands a taxiing departure to Tower before it reaches the hold, so the hold itself is not the start of a handoff, ready call and clearance chain.
- A taxi follower resumes when the aircraft ahead releases the shared taxiway cell. It still needs its own approved taxi scope and cannot bypass a readback or hold.
- Ambient taxiing aircraft hold a length-aware gap that includes the distance needed to brake, and keep a wider gap from your aircraft than from each other.
- A stopped aircraft whose current clearance is a runway crossing has that current crossing clearance reissued when the runway is clear. This prevents a lost crossing grant from holding the runway indefinitely.
- Generated poses without a frame, flight plan or station assignment are removed on the next movement tick and release their resources.
- The runway is fixed to the ATIS flow. Reciprocal runway ends are not selected for generated traffic just to improve the mix.
- **Wake separation** in Traffic settings selects `auto`, `icao`, `recat_eu`, `faa_cwt` or `mats`. The tables and runway-relationship thresholds are in `traffic/wake/`, with `traffic/wake.toml` used only when the folder is absent; aircraft category comes from the simulator when reported, then from the aircraft profile. **Settings → Realism → Allow wake separation waits** turns departure wake intervals off for the player and ambient traffic; runway occupancy, landing, crossing and go-around protection still apply. A wake interval produces **line up and wait**, not hold short; a landing on one parallel runway does not block a departure on the other.
- A takeoff clearance names the flight's own SID and initial level. It does not read latitude, longitude or a full hold procedure; a hold is issued separately only when needed.
- Hold short is reserved for actual runway occupancy, a crossing or a blocked holding point. Line up and wait is used when the runway is available but the wake interval is still running.
- The experimental generated-traffic simulation also uses this pool, with at most one simulation job outstanding and complete snapshots published to the plugin.
- The preparation worker now decodes bounded aircraft-specific SID/STAR/approach inputs from the actual nominal rotation point and fresh endpoint weather, preserving restrictions, IAS and selected-runway threshold heights.
- Missing or incompatible inputs are reported without granting flight.
- The preparation worker assembles the complete protected route, recovery and landing before publishing a flight. Runway-compatible terminal candidates, published recovery legs and enroute MORA floors are retained within `terminal_candidate_limit` in `traffic/operations.toml`.
- Departures use their published SID. Arrival STAR joining is not available for every airport and procedure combination; when no compatible STAR/approach join is resolved, the arrival is not claimed as STAR traffic.
- Generated speech is queued on one OS thread per frequency, so transmissions on one frequency stay ordered while different frequencies can transmit independently. Clearance selection still processes one exchange at a time; replacing that global selector remains unfinished.
- Generated traffic is installed and exercised at installed airports; native simulator visibility, audio and performance acceptance remain separate checks.

The controller consumer can now queue a complete onward flight from a recovery hold after its pilot readback. The aircraft finishes its current circuit and leaves at the holding fix; the new plan takes effect there, with separate landing and taxi permissions. Incomplete recovery sets and incompatible joins are refused. A simulation-clock change cancels a queued exit while preserving the hold. The integrated consumer regression reaches parking after rejoin; this does not establish production route generation or native simulator acceptance.

Generated flight clearances now accept published Departure controllers for climb and explicit combined station duties for their supported phases. A queued recovery-hold exit is tied to its original flight: removing the active flight, removing its aircraft frame or replacing its plan cancels the pending exit and leaves the current holding circuit intact. Focused controller tests follow both dedicated Departure and combined Tower/Departure cases through parking, and check stale-exit cancellation through another holding circuit.

- Generated departures consult the observed wake monitor before a takeoff grant and before the roll.
- A heavier native, generated or player departure can delay a smaller follower; the active regional table and runway relationship determine the interval.
- Player liftoffs are observed alongside traffic liftoffs, including at the generated scene's airports.
- A conflict during readback cancels the grant.
- A later conflict withdraws an accepted but unstarted roll and its initial climb authority, queues the cancellation until the radio is free, and requires a fresh clearance after the interval expires.
- Hold and cancellation wording is editable in `speech/runtime/responses.toml`.
- The bundled tables are starting policies and remain editable; native simulator acceptance is separate.

- **Airline, hub and aircraft preferences** are editable in `traffic/airlines.toml`, `traffic/hubs.toml` and `traffic/aircraft.toml`.
- The starter set has 29 operating/private fleet profiles, 50 airport preferences and 47 aircraft envelopes spanning piston aircraft through freighters.
- Weights are approximate simulation preferences, not market shares or real schedules.
- The selector checks both endpoints, home-region affinity, runway/parking capacity, range and compatible installed CSL models.
- Traffic can reload and validate personal overrides, retaining the last valid set after an invalid edit.
- The generated moving fleet uses these profiles; native simulator acceptance remains separate.
- See [profile editing and sources](traffic/README.md).

- **Advisory traffic at untowered fields** flies a right-hand visual circuit at any field that publishes a CTAF or UNICOM frequency and no Tower service.
- The aircraft announces its own position on the published frequency: downwind, base, final and landing. No call contains a clearance, and none of them can grant one.
- Calls use the standard advisory form: field, traffic, callsign, leg and runway, field - for example, "KCTF traffic, N12345, entering downwind runway 09, KCTF".
- The procedure is the authority for that traffic, so it never enters the controlled-clearance path and never occupies a taxiway or a runway outside its published pattern.
- The pattern repeats: the aircraft climbs from the end of the landing roll, rejoins the downwind and flies the circuit again.
- A straight-in arrival announces five miles final, final and runway vacated, lands on the same runway and leaves the world after its landing roll. It never repeats.
- A departure announces the departing runway and leaving the pattern, holds the runway during the climb-out and leaves the world after turning out. It never repeats.
- A cross-country VFR flight is planned between two untowered fields 10-40 nm apart; departure calls use the origin frequency, approach and landing calls use the destination frequency. An occupied destination runway produces a go-around on the destination pattern.
- A runway held by the player or another aircraft at the final call produces a go-around instead of a landing.
- These calls are heard only while that frequency is tuned, and are recorded as background information rather than as an instruction.
- The wording is editable in the `ctaf_*` entries of `speech/runtime/responses.toml`.
- General-aviation piston aircraft can use tie-down and hangar stands whose published equipment lists turboprops but omits `props`; gates and airline ramps keep their equipment restrictions.
- Fields are selected from the published station index near the player, and at most two advisory aircraft are published.
- This is source and installed-data behaviour; it has not been seen in the simulator.

## Maps, stations and simulator guidance

| Page | What it shows |
| --- | --- |
| ATC | Conversation, current clearance, text entry, role controls and eligible quick requests. Click a message for selectable/copyable text. |
| ATC COM2 | The same view on the second radio, with its own message history. |
| Crew | Copilot, ground and flight-attendant quick requests for the current phase, with the interphone conversation for the selected crew member. |
| Planning | Editable/imported flight plan, route, procedures and flight identity. |
| Channels | Automatically shows the airport's stations on the ground and nearby receivable stations in flight, with frequencies, duties, distance/coverage and tuned state. Click to tune COM1; there is no airport filter to maintain. |
| Taxi | Airport surface geometry, stands, pending/approved taxi route, aircraft and clearance limit. |
| Arrival | Geometric planned/current descent profiles, installed runway/approach references and sampled terrain where available. |
| Airports | Airport search, runways, services, navaids, procedures, stands and 2D/3D geometry. |
| Traffic | Native AI amount, reported aircraft, optional traffic speech and pilot conversation controls. |
| Missions | Compatible stories, form/TOML editor, captain choices and flight journal. |
| Map, developer mode | Aircraft-following controller boundaries, altitude bands, airports and current assignment for handoff diagnosis. |

The Crew page reads the same `intents.toml` catalogue as the ATC request grid. A
quick request names its `crew` tab, the phrase sent when pressed, the aircraft
`control` it needs, and a `phase` list. Omit `phase` or use `["all"]` for every
phase. Phase keys are `parked_out`, `pushback`, `taxi_out`, `takeoff`, `climb`,
`cruise`, `descend`, `approach`, `landing`, `taxi_in` and `parked_in`; takeoff and
climb split at 1,500 ft AGL. `aircraft = ["A20N"]` limits a button to that type and
an empty list applies it everywhere. `when = { control = "chocks", is = "on" }`
shows a button only when the switch is on, off, available or unavailable.

`actions = [{ control = "chocks", value = "off" }]` sets one or more controls in
order, with `on`, `off` or a numeric target. The engine validates the whole list
against the loaded aircraft profile and the crew role before queueing anything, and
speaks one summary. A button with no `actions` sends its phrase as before. Missing
controls hide the button rather than half-running the flow. Crew buttons never enter
the radio parser, so a spoken "thanks" still reaches the controller rather than a
copilot button. The drink and snack request uses the published cabin service phrases.

The bundled ToLiss A320 profile adds stateful controls for the ADIRs, GND CTL, smoke
sign, cockpit door, weather radar, NAV/LOGO lights, the six fuel buttons and the
transponder mode. Two copilot buttons use them: **Prepare A320** and **Prepare
Start**. The A320 fuel panel is read and written through `AirbusFBW/FuelOHPArray`;
MODE SEL and X FEED are left alone.

Channels identifies a ground airport using enabled scenery boundaries, pavement and runway footprints before falling back to reference-point proximity. This avoids choosing a nearby heliport or small airfield simply because its reference point is closer. It is scenery containment, not a legal airport-property survey. The station list updates as airport and radio data arrive; there is no airport filter to maintain.

Stations come from enabled scenery and installed ATC/navdata in priority order, including 8.33-kHz channels. Published controller definitions can replace airport Tower entries; Ground and Delivery remain airport services. Approach/Center coverage and receivability are separate concepts. Terrain shielding, actual transmitter sites and full remote-radio coverage are not modelled.

Click an underlined frequency in the conversation to tune COM1 through the same aircraft-control path as Channels. The radio must be powered. Table headers support ascending/descending sorting, and name/identifier filters help narrow airport and stand searches.

Taxi map arrows and simulator-ground arrows are separate settings. Only acknowledged routes produce ground arrows, and unverified stand connectors are not drawn on simulator ground. Guidance stops at the current clearance limit. Terrain probe misses, water and unsafe discontinuities are skipped.

The optional **Show holding point on map and simulator ground** setting is off by default. It draws a translucent amber circle on the map and simulator ground, set back on the approach side of available hold-line geometry. It disappears when the relevant crossing or takeoff permission permits entry. It is plugin guidance, not a replacement for painted markings or native airport lights. Existing scenery runway guard lights are preserved; individual native stop-bar control has not been established.

- The Arrival profile is a geometric aid, not a complete STAR/VNAV solver.
- Missing terrain stays unknown.
- The debug Map and Arrival profile have different purposes; dense airport labels can be hidden.
- Use the Map to compare the aircraft position and altitude with loaded sector coverage when diagnosing a missing handoff.
- Taxi and debug Map each have an independent **Show traffic** checkbox, enabled initially.
- Reported traffic appears green on the ground and orange airborne, with callsign labels and hover details for name, type, altitude, speed and known COM1/COM2 channels.
- Native TCAS does not supply tuned radio channels, so those fields show Unavailable unless a traffic provider reports them.
- Hiding markers does not disable traffic or radio.
- These layers display reported telemetry; they do not create traffic.

Radio check, check-in, frequency change, position and emergency requests are available while parked on a receivable controller frequency, even before filing a flight plan. The request list also includes **Request altimeter** and **Request weather**; local observations are sampled at either radio's tuned airport as well as the planned airports. Weather replies require a fresh simulator observation. Say again, stand by, unable and readback appear when there is an exchange to answer. Departure clearance, start-up, pushback and taxi retain their controller, readiness and clearance requirements. ATIS remains a listening service.

- ATIS uses simulator weather sampled at the station's airport and repeats until retuning or radio/reception changes.
- While OpenATC handles the tuned ATIS, it suppresses X-Plane's built-in broadcast to prevent overlapping voices.
- It restores the previous native ATIS setting when you tune away, disconnect the engine or disable the plugin.
- With COM2 enabled it can monitor ATIS on the second radio while you browse other pages; controller instructions take priority over ATIS playback.
- Settings → Voices provides a dedicated ATIS voice, steady reading speed and optional electronic effect.
- That effect approximates an older automated broadcast, rather than removing the synthesizer's pitch variation.
- Online METAR lookup is a separate planning feature.
- Distant unavailable observations are not replaced by invented weather.
- Current ATIS does not provide complete worldwide regional diction or NOTAM coverage.

## Settings and realism

ATIS broadcasts also appear in the conversation log with an **ATIS** badge. Spoken communications stay in character; internal software notices are not voiced. Runway selection uses the airport's installed runway geometry and observed wind rather than accepting a tailwind runway from the flight plan. A planned runway only breaks ties between equally suitable directions. Runway changes advance the information letter. Scenery loading runs on a bounded worker and runway geometry is cached. ATIS remains informational, not a takeoff or landing clearance.

New messages from every speaker follow the conversation only while you are at the bottom. Scroll up to read history without incoming pilot, controller, crew or ATIS entries moving your position; return to the bottom to resume following.

Page navigation lives in the title bar: ATC, Radios, Flight plan, Taxi, Arrival, Airports, Settings, Traffic and Missions. The Map icon appears in developer mode. Hover an icon for its name; cyan marks the current page. At narrow widths the navigation icons move together onto a second header row, keeping the title and window controls accessible. The right side holds the debug flight jump (developer mode), New flight, Keep expanded, compact ATC view and pop-out controls.

Clicking **Communications only** keeps the ATC window compact until you expand it, choose another page or start push-to-talk. Hover still expands a window that compacted automatically. The transcript keeps following the latest message across size changes; if you scroll up to read earlier traffic, it preserves that reading position instead.

Settings tabs wrap in narrow windows so every section remains visible. **General** shows the engine connection, current AI/speech addresses, SimBrief ID and scenery folder. It can optionally apply one server address to all three services. **Copilot** holds readbacks, automatic tuning/check-in and the copilot talk button. Advanced service addresses, intercom overrides and CPU limits stay with their relevant sections. Changes save automatically; Save now also submits them immediately.

Dispatch fields adapt to window width and text scale. You can edit a flight plan while the engine is disconnected; SimBrief import becomes available when connected. The Airports page offers departure/destination shortcuts before an airport has been loaded. The debug map keeps controller and airport labels apart and groups overlapping controller frequencies in its hover details.

### Appearance

**Settings → Appearance** contains Light/Dark theme, text scale, background transparency and window behaviour. Dark is the default. **Background transparency** ranges from 0% (solid) to 80%; text and controls retain their contrast. Collapsed-window opacity is separate. The background uses ordinary ImGui blending without scene capture or post-processing.

### Flight realism

Choose **Relaxed**, **Standard** or **Strict**, then review individual settings rather than assuming a preset is appropriate for every aircraft or test. Relevant controls include:

- **Readback and constraint checks:** keep clearances pending until acknowledged, and apply selected published restriction checks.
- **Deviation monitoring:** track assigned altitude/heading/speed where implemented. Repeated unresolved corrections can prompt ATC to ask whether you can comply or need assistance. IFR service and existing permissions are not automatically cancelled after a warning count. The old `terminateAfterWarnings` settings key is retained for existing files and now controls that assistance prompt.
- **Transponder monitoring:** compare the simulator's selected code with the acknowledged IFR clearance, with a 30-second adjustment period and at least a minute between reminders. Airborne checks also look for altitude-reporting mode. Missing or invalid observations and emergency codes are excluded from routine code corrections.
- **Runway permission warnings:** identify runway entry, takeoff and landing without the corresponding permission. Crossing permission applies only to the named physical runway. The monitor does not issue a late takeoff-abort instruction during a high-speed roll; an airborne departure without clearance receives an intentions query. Landing reminders wait until rollout slows.
- **Taxi assistance:** monitoring starts after the taxi readback and confirmed movement: at least five metres of displacement and two telemetry samples at one knot or more. Sitting at the stand, pushback before taxi readback and small speed fluctuations do not trigger an off-route warning. On an unmapped apron, the initial connector from the observed starting position to the first route point uses the same 60-metre corridor tolerance; moving toward the network does not trigger assistance. After joining the mapped route, that exemption ends. Moving away from the connector or spending 15 seconds more than 60 metres from the approved route prompts the issuing controller to offer assistance when the radio is free. A new taxi clearance starts fresh movement monitoring. Runway-permission checks remain independent.
- Answer the current taxi-assistance question with **yes**, **affirmative**, **no** or **negative**. This works without AI classification. When stopped, an accepted offer recalculates a route to the same runway or stand, updates the map and requires a new readback, retaining intervening runway holds. Missing safe geometry results in a hold-and-confirm-position response. These are revised route instructions, not continuous step-by-step progressive taxi. A short answer cannot acknowledge an unrelated clearance or a question from another frequency. Invalid AI classification JSON is rejected and ATC asks for clarification rather than executing model-supplied actions.
- **Copilot replies and tuning:** independently control automatic radio assistance. Keep both enabled if you want the full acknowledge/tune/check-in sequence.
- **AI wording variety:** off uses validated TOML alternatives; on allows natural model wording that must preserve operational facts.

Initial IFR clearances retain the full filed route internally but speak abbreviated routing: the assigned departure/transition, then as filed. The copilot repeats that abbreviation, altitude, squawk and runway, using the destination airport name when available. Full-route readbacks remain accepted; amendments still require the changed routing. Clearance and readback wording lives in `speech/runtime/responses.toml`.
- **English-language voices only:** limits controller selection by language while allowing English accents.
- **Pilot playback:** hear synthesized own transmissions if enabled.
- **Guidance visibility:** map arrows, ground arrows and holding-point marker are independent.
- **Developer mode:** exposes the sector Map, detailed errors and controlled surface test jumps. It defaults off for new settings files; existing explicit choices are preserved. Enable it under Appearance → Developer tools.

Optional live-session traffic speech is tied to identified simulator aircraft and observed events. Scripted demo chatter stays in demo sessions. Runway protection can only see traffic provided through the simulator interface; aircraft missing from that interface remain invisible to this check. Complete traffic management, wake sequencing and separation are still in development.

**Allow ATC to offer en-route shortcuts** is an optional Realism setting, off by default, with a minimum interval of 2–60 minutes. Offers require suitable downstream fixes, known terrain clearance, no skipped published restrictions and enough distance left for descent. Use **Accept direct…** or **Keep planned route** in the ATC page. Acceptance requests a new clearance and readback; copilot mode does not accept optional changes for you. Simulated runway closures and broader ATC scenario amendments are still planned. Safety instructions and required handoffs do not depend on this switch.

The three additional monitoring switches default off for existing configurations. Selecting Standard or Strict enables them; Relaxed disables them. They can also be changed individually. Required crossing, takeoff and landing permissions remain in force with reminders off.

Tower keeps checking the runway after issuing landing clearance. A newly observed conflict can trigger a go-around even with a readback outstanding. A missed approach does not immediately become another approach clearance merely because the aircraft is near the destination; request an approach again. Routine exit instructions wait until the aircraft has slowed below 40 knots on rollout. Published missed-approach leg guidance is not yet complete.

**Follow-me car** is an optional aid under Realism → Taxi guidance and on the Taxi page. It uses X-Plane's installed follow-me vehicle, appears only for an acknowledged departure or arrival taxi route, and waits before each clearance limit. It cannot authorize a runway crossing. Pending readback or pause stops its movement; reaching parking, taking off or ending the flight removes it. If the route is too far from the aircraft or the scenery vehicle is unavailable, it is not drawn. It is off by default. Live appearance and placement still need simulator testing.

- Simulator work is bounded: telemetry is sampled twice a second and coalesced, so an outstanding request cannot build up a queue of old positions.
- HTTP and speech run on workers; route planning and compliance run in the engine.
- The panel shares immutable snapshots instead of copying the entire route and conversation each frame, and response parsing stays outside the UI lock.
- Static airport bounds, taxi node lookups and ILS beams are cached when the airport loads.
- Ground guidance displays at most 64 nearby arrows and reuses object instances between updates.
- The follow-me car uses one scenery instance, no aircraft flight model or AI traffic slot, and updates at up to 20 Hz with one terrain probe per movement update.
- It caches its route and stops at the acknowledged limit.
- The arrival terrain sampler is capped at 25 probes per half-second update.
- These are workload limits, not an FPS guarantee; measure performance with your scenery, aircraft and traffic settings using X-Plane's Plugin Admin performance page.

The header's **New flight** icon opens a confirmation inside the plugin window. Starting a new flight clears the plan, conversation, permissions, guidance and pending crew actions. Settings, controller voices and actual aircraft controls remain. The aircraft stays at its current physical position; choose a parking stand in X-Plane’s flight setup when restarting a gate-to-runway test. Reset errors expose retry controls. Use this to begin another test instead of unloading a live plugin.

The developer fast-forward icon offers explicit test checkpoints rather than cycling the phase number. Surface options remain: an acknowledged route is required for the next holding point (including an approved taxi-in route), and takeoff clearance is required for the departure runway start. Surface repositioning writes the physics quaternion as well as the displayed heading, and rejects a flight-path override owned by another system.

- Airborne options select a known straight leg in the expanded SID, cruise, STAR, or 20 route miles before the first approach fix.
- A powered aircraft and acknowledged IFR clearance are required.
- Specify an air-start speed in knots; zero preserves the aircraft's current TAS and therefore cannot launch a stationary aircraft.
- The plugin uses X-Plane's `XPLMPlaceUserAtLocation` initialization API, which performs required scenery loads and supplies position, heading and speed with running engines.
- The flight is paused for setup review.
- This is a test setup, not an aircraft performance planner: review thrust, airspeed, gear/flaps, AP modes and the active FMS leg, particularly with ToLiss/custom avionics, before resuming.
- It does not program the third-party MCDU or manufacture approach/landing clearances.
- Fly the final approach and landing normally, then use approved surface checkpoints for taxi-in.

- The engine prepares a generation-bound checkpoint, suspends telemetry-driven automation while native initialization completes, and updates phase/route progress only after valid paused telemetry confirms the geographic position and geometric height.
- Cockpit altimeter indications can remain stale while a third-party aircraft is paused, so they do not gate native completion.
- Failed/cancelled or expired jumps do not advance the ATC state.
- Old queued speech/readbacks/actions are cancelled.
- Checkpoint levels replace the developer fixture's assigned level without an operational radio transmission; approach and landing permissions start unset, so the normal descent/approach/Tower sequence must run.
- Select the appropriate controller and check in after a distant jump.
- The Nice–Milan regression checks both the complete imported fly-by route and an arrival starting at the pre-approach checkpoint through the entry-level descent, ILS clearance, Tower transfer and landing permission.
- Native repositioning, third-party configuration and Windows/macOS behaviour still require simulator acceptance.

## Customise speech and aircraft

| File or directory | Purpose |
| --- | --- |
| `aircraft/*.toml` | Aircraft matching, supported commands/datarefs and checklist definitions. |
| `regions.toml` | Regional unit and procedure references. |
| `radio-stations.toml` | Optional combined-duty overrides for existing published stations. |
| `intents.toml` | Request names, aliases and examples. |
| `traffic/behaviour.toml` | Pilot conversation conditions, timing, probability, range, history and personality. |
| `cabin/behaviour.toml` | Simulated passenger activity, preparation timing and observed-control freshness. |
| `speech/runtime/responses.toml` | Live controller, pilot, ATIS, readback and crew response templates. |
| `speech/common` and `speech/<region>` | IFR, VFR and shared phrase examples. |
| `speech/crew` | Copilot, cabin and ground-service examples. |
| `missions/*.toml` | Editable flight stories, phase/probability triggers, dialogue, choices and optional future media references. |
| `prompts/prompts.toml` | Editable ATC, classifier, crew-action and conversation role instructions. Legacy personal `.txt` overrides remain supported when that directory has no TOML catalogue. |
| `models.toml`, `models-kitten.toml` | Built-in model download manifests, embedded when the server is compiled. |

All spoken phrase alternatives belong in the speech library. Aircraft profiles describe controls and checklist data; they should not contain a separate collection of pilot/copilot response scripts. See [Speech editing](speech/README.md), [Runtime templates](speech/runtime/README.md) and [Situation index](speech/INDEX.md).

Alternatives must retain required placeholders and operational facts. With wording variety enabled, matching phrases become examples for the LLM, together with its role and context. Repetition can still occur for standard phraseology. The engine remembers phrase selections and protects route, altitude, runway, frequency and other cleared values; a rewrite changing protected facts is discarded.

Displayed identifiers remain compact. Speech expands controller abbreviations such as GND to Ground, uses known airport names, spells registration callsigns phonetically, and applies aviation number pronunciation. IFR/VFR/ILS remain spoken letters. Missing airport-name data can still require an identifier fallback.

Adding TOML text does not implement a new operational permission or simulator action. Add the associated request handling, validation and monitoring before connecting a new scenario to live operations. Regional examples are a starting point and require local procedure review.

`intents.toml` now supplies the request-button labels/order, matching aliases and slot-recognition patterns. The engine and plugin load installed catalogues; a personal engine `intents.toml` can override them. Older catalogues retain bundled missing definitions and button order. Invalid grammar is rejected before activation. Mission phase/probability/choice logic and conversation equality/list/numeric-range conditions are declarative TOML; clearance authority and simulator safety remain validated by Rust.

Validate edited speech before flying:

```sh
target/release/open-atc-engine --check-speech speech
cargo test --locked -p openatc-core --test shipped_config --test speech_library
cargo xtask test-speech target/release/open-atc-engine speech
```

Restart the companion engine after changing speech, prompts or duties. Model manifests are compiled into the AI server; editing an installed copy documents the model set but does not change the executable's download plan. Rebuild to change the built-in manifests.

## Builds and packages

The **Native platform builds** workflow runs on version tags beginning with `v`, or manually through GitHub Actions. Pushes and pull requests do not automatically start the expensive platform matrix. A manual run accepts a version such as `0.0.1-alpha.1` and lets you select one platform and one product.

| Platform | Plugin product | AI-server product |
| --- | --- | --- |
| Linux x64 | ZIP/payload installer plus DEB, RPM, AppImage and Flatpak bundles | Separate DEB, RPM, AppImage, Flatpak and archive |
| Windows x64 | Complete plugin payload and installer | AI runtime and installer |
| macOS Intel / Apple Silicon | Platform plugin payload and DMG | Platform AI runtime and DMG |

The Linux plugin packages install an **installer application**: use it to select your X-Plane folder. They do not assume that your simulator lives in a system package directory. Flatpak also needs access to the chosen simulator folder. Choose one installation format; installing every format is unnecessary.

Manual runs publish downloadable **Artifacts** on the workflow run's page. Version-tag runs collect package outputs and attach them to a GitHub **Release**. These are release downloads, not GitHub Packages container images. Pre-release suffixes mark the release as a pre-release. The raw native artifacts are useful for diagnosis; normal users should use the complete packaged runtime.

Windows builds use MSVC and the Visual Studio linker. macOS builds run natively, including an ONNX Runtime source build for Intel Macs. That dependency is cached and its unit-test compilation is disabled, but a clean build can still take a long time. Linux cross-checks cannot replace loading the Windows or macOS plugin in its native simulator.

The workflows describe intended outputs; see the validation record for what has actually been run. New GPU-enabled Windows/macOS packaging needs native CI acceptance after this change. No hosted build is started by editing the workflow locally.

## Development and validation

### Source layout

| Directory | Responsibility |
| --- | --- |
| `crates/core` | Flight state, controller permissions, geometry, navigation and data readers. |
| `crates/engine` | Request orchestration, radio/crew workflows, surface and airborne monitoring, provider calls. |
| `crates/plugin` | X-Plane SDK integration, telemetry, aircraft controls and rendering host. |
| `crates/ui` | Shared interface and client state. |
| `crates/desktop` | Standalone UI preview. |
| `crates/server` | Local AI HTTP service, LLM inference, TTS worker and STT process ownership. |
| `crates/tts`, `crates/stt` | Speech synthesis/recognition and audio formats. |
| `crates/ai-core` | Model manifests, downloads and integrity records. |
| `crates/audio`, `crates/http`, `crates/platform`, `crates/settings` | Shared audio, transport, operating-system support and settings. |
| `xtask` | Rust build, install, package and integration-test commands. |
| `tests/fixtures`, `xtask/fixtures` | Synthetic test data; not production airport overrides. |

First-party implementation is Rust. Native dependencies still include C/C++ libraries such as X-Plane bindings, ImGui, audio and inference runtimes. Python is not needed to run OpenATC. Some upstream native builds, especially Intel Mac ONNX Runtime, use Python during compilation.

Install current stable Rust, a native compiler, Clang/libclang, CMake and pkg-config as appropriate for your operating system. Optional accelerated builds need their SDKs; see [AI server building](docs/AI_SERVER.md). The old top-level C++ implementation and Python scripts have been removed.

```sh
cargo fmt --all -- --check
cargo test --locked -p openatc-core -p openatc-engine -p openatc-ui -p openatc-plugin -p xtask
cargo test --locked -p openatc-ai -p openatc-stt -p openatc-tts
cargo build --locked --release -p openatc-plugin -p openatc-engine
cargo xtask test-radio target/release/open-atc-engine speech
cargo xtask test-crew target/release/open-atc-engine speech
cargo xtask test-speech target/release/open-atc-engine speech
cargo xtask test-flight target/release/open-atc-engine speech
cargo xtask audit-linux target/release/libopenatc_plugin.so target/release/open-atc-engine
```

Default workspace builds cover the plugin stack. Select optional AI/desktop targets explicitly; `--workspace` includes their native dependencies. On Windows, use the `.exe` engine path in integration commands. The Linux dependency audit is platform-specific.

Integration suites start a real companion engine against temporary scenery/settings and a fake provider. They verify workflow contracts without requiring models or mutating your simulator. Model/audio tests and installed-scenery tests provide separate evidence. None is a substitute for flying the complete route with live audio and the chosen aircraft.

Generated packages, model downloads, build directories, logs and personal environment files are ignored. Runtime configuration, phrasebooks, model manifests, Cargo files and CI workflows belong in the project. Ignore rules cannot remove an already-tracked file; review your own commit contents.

See [Architecture](docs/ARCHITECTURE.md), [Rust workspace](docs/RUST_WORKSPACE.md), [Validation](docs/VALIDATION.md), [Changelog](docs/CHANGELOG.md) and [Developer commands](xtask/README.md). Contributions are especially useful for native Windows/macOS testing, different airport layouts, aircraft profiles, pronunciation and reviewed regional procedures.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| No stations or the wrong airport | Radios shows the occupied airport's services on the ground and nearby stations in the air. Check the X-Plane folder in Settings → General, radio power and the position/loading message. Export the position if an airport footprint is wrong. |
| Simulator telemetry rejected | Copy the field and value from the error or developer log. The engine can remain connected while simulator data is rejected. A valid new sample clears the error; rejected data does not replace the last accepted flight state. |
| Cannot find speech or voice settings | Open Settings → STT / TTS, Voices or Audio. Tabs wrap in narrow windows. General shows the currently saved provider addresses; editing becomes available after saved settings load. |
| Controller rejects a request | Confirm phase, tuned service, airport duties and any pending readback. A recognisable sentence can still be unavailable in the current state. |
| Taxi stops at a runway | Read back the current route, then wait for or request the applicable crossing. A destination runway does not authorize crossing another runway. |
| No crossing/takeoff/landing clearance | Check runway/final traffic and whether the simulator supplied traffic data. Unknown traffic is not treated as an empty runway. |
| Copilot does not tune/check in | Enable the relevant copilot settings, let queued speech finish, and verify COM1 actually changes in telemetry. |
| Own text is silent | Enable Speak my transmitted requests and check pilot volume/output. |
| TTS failed | Inspect `engine.log` and AI-server output for the exact text, voice and error. Refresh voices after changing providers. Keep eSpeak data and shared libraries with the runtime. |
| GPU not used | Check `/health`, startup logs, build features and drivers. CUDA speech additionally needs compatible CUDA/cuDNN libraries. Remove force-CPU flags only if GPU use is intended. |
| LAN connection fails | Use the server's LAN address, bind with `--host 0.0.0.0`, and check the firewall. URLs should not end in `/v1`. |
| Aircraft action was not confirmed | Inspect the mapped control's availability and live indication. A custom aircraft may require a corrected TOML mapping. |
| Copy/paste fails | Check the native clipboard and display session. Debug conversation export also provides a file fallback. |
| Need to restart the flight | Use New flight in the header; inspect any reset error rather than repeatedly unloading the plugin. |

## Detailed implementation and validation notes

For current limitations and platform acceptance, see [Validation](docs/VALIDATION.md) and the [Roadmap](docs/ROADMAP.md). The notes below cover platform behavior and implementation details. Dated test results are kept in the validation guide.

<details>
<summary>Platform behavior and implementation details</summary>

Before IFR clearance, the ATC message controls and radio checks use the callsign reviewed in Planning, including a SimBrief import. A clearance request with an empty Planning draft receives a callsign-addressed “you have not filed a flight plan” response. An incomplete or invalid draft receives a refusal with the missing fields or validation error; a corrected valid plan is submitted automatically on the next clearance request. Filing the flight still happens when requesting clearance; an issued clearance prevents draft edits from changing the active radio identity. Drag the OpenATC AI title area to move the floating window. Initial and restored floating windows are fitted to a visible monitor, with recovery after display-layout changes.

macOS uses a Rust OpenGL 2.1 compatibility renderer for X-Plane’s plugin context, including large map draw lists. Native window changes are applied between frames. Compact and expanded modes share the compact native height limit while the interface enforces its expanded minimum, preserving the window’s top anchor.

Windows packages include the Microsoft C++ release runtime beside the launcher, companion engine and speech programs, including the installer discovery helper. Users do not need Visual Studio. Optional NVIDIA speech acceleration still requires compatible CUDA/cuDNN libraries; CPU fallback remains available.

`test-flight` runs four complete IFR flights through the real HTTP engine using simulated aircraft telemetry and synthetic scenery: separate Delivery/Ground/Tower, Ground plus Tower, combined Tower duties, and separate controllers using COM2 while COM1 remains unchanged. It checks sector handoffs, copilot exchanges, proactive descent before a crossing restriction becomes infeasible, arrival and departure runway crossings, traffic holds, landing, runway exit and taxi to the assigned gate. Reports and transcripts are written to `packages/flight-validation/report.json`. This suite also runs in platform build jobs. It does not prove live audio, third-party aircraft controls or simulator rendering.

When reporting a flight issue, include the conversation export, aircraft/profile, installed scenery/navdata, phase, COM frequency, position and the action you expected. For audio issues also include the provider, voice, relevant server error and whether CPU or GPU was selected. Remove credentials and unrelated personal information before sharing logs.

COM1 and COM2 use independent speech playback so a broadcast on one radio does not block the other. Each ATC log shows its own radio traffic; crew interphone messages remain in the primary conversation. ATIS repeats audibly while each station and radio retain one current bulletin in the log. Weather or runway updates replace that bulletin in place rather than adding another row.

The captain and copilot are heard directly in the cockpit, so their speech and takeoff callouts bypass radio hiss, crackle, static and bandpass filtering. Received controllers, ATIS and other aircraft retain the configured radio effects. Cabin and ground interphone effects follow their separate settings.

Click an underlined squawk in the clearance summary or conversation to select that code. ToLiss AUTO/ON switches to ON while the transponder/TCAS mode selector stays unchanged. Bindings are editable in `aircraft/transponder.toml`. Nice requires an active transponder before pushback or taxi; selecting a code alone does not complete that procedure.

Default aircraft use the standard X-Plane squawk dataref. Aircraft-specific digit bindings apply only when the complete custom control group is available; setting a code preserves the default aircraft’s transponder mode. Custom controls and the standard fallback are editable in `aircraft/transponder.toml`.

The floating plugin window remembers its position, expanded size and ATC transcript divider in settings. Compact mode preserves the expanded layout, and restored windows stay within the current screen bounds.

MECH/INT represents the external ramp headset. It remains available during stand work and pushback, then disconnects after observed taxi movement or becoming airborne. A stop at a holding point does not reconnect it. It becomes available again after assigned-stand parking is complete. A taxi clearance while still stationary at the gate does not itself disconnect the line. Movement thresholds are editable in `cabin/behaviour.toml`; disconnected requests produce an interphone notice rather than a fictional ramp-agent reply. Flight facts supplied to crew interpretation include the departure, destination and planned enroute minutes, when known. Ramp and cabin roles have separate responsibilities; food service belongs with the attendant, and unknown flight duration should not be invented.

Crew requests can combine controls with a friendly remark. Editable role prompts and examples teach the model to interpret the complete request, including misspellings, reordered clauses and multiple requests. There is no food, joke or social-topic menu. The model proposes mapped controls; the engine checks role permissions, ranges and fresh observations, then composes one reply from confirmed outcomes. Crew observations continue during radio speech and travel independently of slow conversational requests, and the captain’s request remains visible even when controls cannot yet be confirmed. Unknown controls still require clarification.

Generated-traffic flight installation retains its controller plan atomically. Production preparation now solves nominal joint SID, en-route, STAR and approach geometry and levels before retaining a candidate with its landing-to-parking route. Solved drafts keep their movement samples private until a separate complete-path protection check passes. This does not install an airborne flight: protected terminal corridors, feasible speed transitions, recovery paths at every clearance boundary and controller-plan assembly remain unfinished. Generated traffic stays disabled by default.

The production traffic worker now prepares bounded en-route crossing inputs between each selected SID and STAR. Grid MORA floors apply at both ends of each connection, so a higher cell constrains the preceding crossing without forcing the whole SID to the highest terrain level farther along the route. The STAR entry retains its published altitude, speed and course constraints. `traffic/operations.toml` controls the maximum nominal crossing spacing with `enroute_segment_nm` (20 NM by default, 5–50 NM supported); older overrides inherit that default. Terrain lookup covers grid cells between samples and across the date line, and rejects unknown cells. These are nominal planning inputs using endpoint airport weather, not a clearance, a surveyed terminal corridor or an airway route. Complete trajectory protection, recovery construction, aircraft speed transitions and production full-flight installation remain unfinished; generated traffic stays disabled by default.

Production candidate preparation also retains a nominal missed approach from the explicit missed-approach point to a holding fix. Where the procedure publishes one, the hold is flown as published. Where it ends in a climb-out or vector, the path is flown to its level and then held at the end of it, marked as an engine fallback rather than published procedure. Fix-to-altitude legs start at their resolved fix; missing fixes, incompatible holding levels, stale weather and unsupported entries reject the candidate. Both the missed path and the complete hold require protection before they can become a movement route. This does not yet cover early go-arounds, balked landings or recovery from every airborne clearance limit.

Generated airborne instructions can carry an explicit range of movement limits under one accepted readback. The range retains the same controller and instruction, requires a fallback at every boundary, and excludes holds and runway/surface permissions. It continues across short legs without redundant waypoint calls. A takeoff instruction can explicitly include a bounded initial SID climb; that permission leaves the roll subject to its separate runway grant. Simulator clock jumps withdraw outstanding airborne grants, including when movement runs on a worker. This controller integration remains part of the unfinished generated-flight factory.

Recovery transitions retain the original clearance limit and discard pending exchanges, grants and handoffs from the abandoned route. A generated aircraft following a checked missed-approach route reports going around once on its tuned frequency; that report grants no movement and does not retune the radio. Recovery rejoining and full-flight generation remain unfinished.

Traffic movement carries actual elapsed time across speed changes and recovery boundaries. Aircraft brake before a slower airborne segment begins, and a recovery that would require braking too late is refused while retaining the previous valid recovery. The same check covers speed changes inside a fallback and replacement airborne plans; leaving a hold cannot instantly select a slower speed, and airborne dynamics cannot change underneath an active clearance. Nominal planning preserves exact shared crossing coordinates so joined routes do not acquire projection-rounding gaps. These source checks do not establish native performance or complete traffic integration.

Resolved terminal crossings retain IAS and weather through vertical scheduling. A bounded solver recalculates ground speeds and turn geometry when downstream restrictions change crossing levels, rejecting unconverged or unprotected paths. This nominal crossing solution does not yet supply the complete production flight factory, continuous performance model or native traffic acceptance.

The experimental complete-flight consumer sends a runway-vacated pilot report on the runway controller frequency before Ground transfer and taxi-in permission. Whole-aircraft dimensions and applicable holding lines must be clear; the landing exit planner uses the same guard. Focused movement/controller/transcript and installed-airport data fixtures pass, but the production full-flight factory and native generated traffic acceptance remain unfinished. Full-flight taxi and crossing movements require their own acknowledged permission; adjacent taxi edges cannot inherit approval. A taxi clearance may explicitly cover several contiguous edges with one readback; the entire corridor is reserved while that readback is pending. Its stated limit cannot include a crossing, another instruction or controller change. Crossing conflicts are rechecked before entry and can cancel an outstanding clearance. Native and player runway occupants also block the runway reservation used for landing and takeoff. These changes are installed for testing; native acceptance remains open.


Nominal generated departure rolls require fresh airport weather and convert indicated rotation speed into TAS and runway ground speed at the selected runway's rotation height. Published elevations at both runway ends define a nominal slope, including displaced thresholds. Lineup connects continuously from the taxi hold, and rollout follows that same selected runway datum. Missing, conflicting or stale inputs reject the preparation. A prepared scene stays valid while live weather drifts within small limits: 50 ft of sample altitude, 1.5 hPa of pressure, 3 °C of temperature, 0.5° of magnetic variation, and 15° or 4 kt of wind. Position stays exact. A larger change discards the scene and prepares it again.

The production worker also prepares a continuous nominal arrival from the published threshold crossing through flare, rollout, runway exit and taxi to a compatible free stand. Candidate selection now checks that complete nominal arrival before accepting a procedure, trying the next bounded candidate if its landing or parking connection fails. Editable aircraft-family profiles control roundout height and touchdown sink rate. Threshold crossing remains airborne; it is not treated as touchdown. These profiles are nominal visual envelopes, not certified per-aircraft performance. Taxi-node heights and actual pavement slope still need native terrain placement. Background pilot readbacks use a `TFC · callsign` log badge rather than appearing as the player. Generated radio exchanges use the operator’s editable spoken callsign and the same flight number retained in their short TCAS identity. Nearby visual/TCAS publication now follows the player with configurable range and slot hysteresis; distant flights retain their movement state and engine separation observations. This display turnover does not generate replacement flights. The full protected airborne route, recovery paths, controller permissions, live gate ownership and continuous nearby-flight replenishment remain unfinished. These changes are installed for testing; source checks do not establish native visibility, audio or FPS acceptance.


All voice actors share the controller engine’s LLM transport and reply-decoding pipeline. Editable TOML role instructions, shared framing in `prompts/interaction.toml`, and example phrases teach tone and behaviour; live facts and conversation history guide fresh replies. Aircraft permissions and confirmed action outcomes remain enforced by Rust. Crew conversation now has its own compact prompt and ordered user/assistant history, independent of the larger aircraft-control catalogue. Mixed requests are separated into current control proposals and exact conversational excerpts, so a sandwich, family remark or joke does not need its own programmed service. Current control interpretations are checked independently; disagreements require quoted current-request evidence. Range, role, freshness and simulator-acknowledgement checks still determine whether a proposed operation can proceed.

Crew continuity retains bounded original captain quotes rather than generated personal biographies: at most 128 quotes of 240 characters per role, with up to 12 relevant quotes and six recent exchanges selected for a reply. Aircraft changes and new flights clear crew continuity; late replies from an earlier flight or aircraft are discarded. This is bounded continuity, not unlimited memory. Token counting and generation use the same ordered messages and reserve 512 output tokens. Older exchanges and optional evidence are removed under pressure; a request that cannot fit is refused, and conversation failures show an unavailable notice rather than pretending an action succeeded. Repeated jokes get at most one retry, preserving the factual part of the answer.

Conversation and verified-result narration receive readable phase names, the current ground/airborne situation and total planned flight minutes when known. Before-departure and postlanding ground states are distinguished explicitly; planned duration is not treated as time until takeoff. Prompts instruct crew to avoid unsupported long-haul descriptions, unsolicited social questions and aircraft-control claims in social replies. Real-model samples cover short-flight predeparture, cruise and taxi-in wording, mixed controls/banter and personal recall. Some generated replies still repeat control claims or add unsuitable filler, so conversation quality remains under review. These pipeline changes are installed for Linux testing; native voice and aircraft acceptance remains separate.

Current source repairs also preserve window geometry and splitter state across compact/full transitions and new-flight reset, pronounce taxiway lists phonetically without editing the log, and log ATIS again on retuning or changed information while suppressing unchanged loop duplicates. STAR speed reminders use published waypoint limits ahead of crossing, with bounded editable notice margins; crossing warnings identify altitude, speed or both. These flight repairs are in the previously installed Linux baseline; native acceptance of each UI/audio change remains separate.

Automatic climb and descent readbacks now retain the direction of the current instruction and its clearance sequence, instead of selecting a generic maintaining-altitude reply. Callout debug logging records threshold and playback timing for diagnosing delayed audio. Focused source tests and Linux builds pass; these flight repairs are included in the previously installed baseline.

On Tower check-in, current source gives an immediately available landing clearance as the response, rather than a separate acknowledgement followed by the clearance. A blocked runway or wake conflict still withholds landing permission, and contact away from final still receives an acknowledgement. Manual and copilot check-ins on both radios have focused source coverage and are included in the previously installed flight-repair baseline.

A contact acknowledgement received while an instruction still needs readback now preserves that instruction as actionable and cannot trigger an immediate landing clearance. This repair has focused flight-flow coverage and is included in the previously installed baseline.

A user has now verified the native Nice–Milan ToLiss flight through landing and parking, including the 4,000-foot descent, ILS approach clearance, Tower handoff and landing clearance. The landing-to-parking lifecycle is accepted for that installed flight; broader routes, aircraft and exceptional conditions remain separate coverage. Subsequent window, speed-reminder, crew-question, radio-log and readback repairs are in the previously installed Linux baseline, with their own native acceptance still pending. The conversation-pipeline changes described above are now installed for Linux testing, with conversation quality and native acceptance still open.

Installer artwork now uses the navy/cyan OpenATC branding without personal forum credits. Windows wizard assets, Mac application icons and offline welcome pages are included in release packaging. The manual platform workflow has a `preflight_only` option for small native packaging checks before expensive full builds. See [release packaging](docs/RELEASE_PACKAGING.md) for discovery, staged upgrades, ownership-based removal, optional signing/notarisation setup and the exact native acceptance boundary. The shared Rust installer discovers standalone and Steam installations (including Linux Flatpak/Snap Steam and external libraries), validates the simulator, preserves custom files and retains recoverable backups outside the plugins directory. Configuration is preserved rather than automatically merged; review changed default prompts/policies when upgrading. Windows and Mac native package/installation acceptance still requires the manual Actions and platform tests described there.


The latest player-flight source batch also combines a due published speed restriction with descent in one controller transmission. Its sequence-bound readback checks the level, crossing fix, speed and exact/maximum limit, including rephrased pilot replies; old speed facts cannot carry into a newer clearance. Tower check-in during the cleared approach no longer claims a climb back to the former intercept altitude. Floating-window restoration applies the current UI-scale limits before restoring saved geometry. Focused regressions and four real HTTP-engine flights through parking pass, including COM2 and a published STAR speed restriction. The updated source is built and packaged for Linux testing; native window/audio and Windows/macOS acceptance remain separate. Custom operational-speech TOML overrides must include the new required response entries; compare them with the shipped defaults when upgrading.

Windows 11 x64 VM validation now covers the native plugin/engine release build, five installer discovery/transaction tests, both Inno Setup installer fixture compilations and four HTTP-engine flights through parking. A real plugin Setup.exe and ZIP also build successfully. These checks use MSVC and Inno Setup 6.7.3; interactive installer appearance, native fresh-install/upgrade/removal, simulator loading and Windows GPU/audio acceptance remain separate. The hosted Windows workflow still needs a run containing the final linker, compiler-discovery and wizard-image-list corrections.

The Windows AI-server release variant also builds in the native VM with Vulkan LLM/STT and CUDA-capable TTS enabled. The packaged server and recognition sidecar pass their help/startup-linkage checks, and separate AI-server Setup.exe and ZIP packages are generated. Windows native inference dependencies now use Ninja to avoid the Visual Studio generator's nested shader-build path limit. Package construction does not establish model inference or GPU compatibility on the VM.

The native release checks also exercise local HTTP request/response transport without an AI server. The test fixture consumes complete POST bodies before closing sockets, including bodies larger than one read buffer. Intel Mac ONNX builds supply the CMake compatibility setting required by older bundled dependencies; the complete platform build remains a separate check.

Windows engine integration fixtures explicitly restore blocking mode on accepted mock-model sockets, while keeping listener polling nonblocking. Radio, crew, speech and all four flight arrangements pass against the native release engine with this correction; a hosted rerun must include the updated fixture.

Linux AI release builds use Ubuntu’s Vulkan development library, shader compiler and SPIR-V headers, avoiding a full SDK download. Shader compilation and loader linking are checked before the full build. These build dependencies are separate from the GPU driver required for acceleration.

Windows AI packages obtain the x64 Vulkan loader from a pinned, checksum-verified LunarG runtime archive during CI. Both server executables receive startup linkage checks before packaging; runtime binaries are not stored in the source repository.

Flatpak packaging uses a pre-rendered PNG tower icon and checks a small export/bundle fixture before packaging the real server, avoiding dependence on an SVG decoder on the build host.
### Traffic implementation

Complete-flight planning joins departure taxi and takeoff, published SID/en-route/
STAR/approach legs, flare, rollout, runway exit and taxi to a compatible stand.
Published crossing restrictions remain in force. Runway selection considers the
requested aircraft categories and requires a workable landing exit and parking
connection. Manual-termination arrivals need an explicit protected controller
amendment to the published approach entry.

Every released route must pass installed terrain and obstacle checks. The worker
reads DSF mesh, object, facade, forest and network data, preserving scenery priority,
polygon holes and neighbouring-tile coverage. Missing or unsupported scenery stays
unresolved. Vegetation uses conservative area bounds; the version 2 cache retains
clipped terrain maxima so unrelated triangle vertices cannot inflate those bounds.
The renderer applies terrain placement separately from the worker's nominal path.

A separate persistent ambient layer supplies the opening airport with ground traffic.
It fills the airport while the player is still parked at the gate, then stops creating
new aircraft once ground movement begins; stopping at a hold does not restart it.
Aircraft start at gates or along the initial taxi route, build a graph route to the
active departure runway, and give each aircraft a stand-to-runway-to-takeoff profile.
Aircraft hold short of the runway, contact Tower when the player is tuned to the
frequency, receive line-up and takeoff clearances, and only then enter the runway.
The departure queue is first-in, first-out: the lead aircraft gets takeoff clearance
when the runway is clear. Once it is rolling clear of the hold, the next aircraft may
line up and wait behind it, while a third waits at the hold. A lined-up aircraft
remains the queue leader until its own takeoff roll starts. The next takeoff is released
once the lead is airborne and 1,800 m from the threshold, or at the runway end on a
shorter runway; the full runway length is not required. The lined-up aircraft then takes
off and the next may line up. The player is sequenced the same way, with line up and
wait followed by takeoff clearance as the runway and wake interval clear. Taxi, rotation, climb,
approach and braking speeds come from each aircraft's handling and profile; light GA
does not use jet speeds. Taxi targets follow a braking curve into each corner and
accelerate out at the aircraft's handling rates. Ground paths are rounded with the
aircraft's turn radius and wheelbase before taxiing, and the runway entry follows the
installed lead-in for both line-up and direct takeoff. Ambient headings also blend
through a corner, so the model turns along the path instead of pivoting at a node. A
shared motion controller limits every speed change, so seeds, corner exit and phase
changes do not snap the model. The line-up from the hold keeps taxi speed until the
final turn onto the runway.
Ambient aircraft detect the player's position and yield
rather than taxiing through. Followers on the same taxi route keep a speed-controlled
gap to the aircraft ahead instead of stopping and starting in a queue. Surface
conflict warnings are limited to traffic crossing ahead: traffic behind, traffic
ahead on the same route, traffic at the hold and runway-queue traffic are ignored.
At a junction, the aircraft with the other on its right gives way, as the ICAO Annex 2
converging rule has it. Traffic going the same way on a parallel taxiway, or the opposite
way, does not create a give-way. A stopped player is ignored for right-of-way, so parked
aircraft beside the taxiway do not stop traffic; a player who stops on the route is still
followed by the aircraft behind. When an ambient or generated aircraft gives way, ATC says so
on the frequency the player is receiving and says continue once it clears. A moving player also
gives way to a generated aircraft on its right. Generated flights hold when they cannot reserve
a taxi route past the player, and the same move-up call follows. If
the player stays stopped on the route with traffic queued behind for 20 seconds, ATC asks the
player to continue taxi, at most once every 90 seconds. Nothing is spoken on a frequency the player
is not receiving. Departures are seeded along the
surface route from the spawn node toward the hold, spread across the usable taxiway
and spaced from the hold and from each other.
A seed is rejected if it is inside the runway strip or within the runway-edge margin,
and aircraft are never queued on a runway-flagged graph edge. If the safe taxiway is
shorter than the requested spacing, fewer aircraft are seeded rather than stacked at
the hold. Runway protection is per physical runway, so a landing
on 04L does not hold a departure from 04R. The destination airport is seeded while
the player is still about 30 NM away. The hold point is derived from
the painted hold-line geometry and set back on the approach side, and the aircraft
stops there rather than at the nearest graph node. The takeoff run starts at the
runway end nearest that hold, so the aircraft enters at the near threshold and rolls
away from it. Takeoff accelerates smoothly at the handling acceleration rate to the
aircraft's rotation speed, using a firmer rate only when the runway is shorter than
the nominal roll. The rotation point follows the class takeoff roll, so a heavy
aircraft uses more runway than a regional or light GA type. Vertical speed ramps to
the type's published climb rate, and the path uses the gradient implied by that rate
and climb speed. Climb speed is a nominal 55% of cruise speed, with approach and
rotation speeds as the lower bound, so a jet does not climb at approach speed. A
jet therefore climbs at a much higher vertical speed than a light aircraft while both use a comparable
flight-path angle, instead of every type following a fixed three-degree line.
A faster departure is not released immediately behind a much slower one. It waits
at the hold or lines up and waits until the performance gap is safe, then climbs at
its normal speed. The anti-overtake speed cap only applies to similar-performance
pairs and cannot reduce an airborne aircraft below its safe climb speed.
Ambient departures and arrivals use separate same-flow
runways when the airport has a usable parallel; runway occupancy is tracked per
physical runway, so traffic on 04R does not block 04L. Ambient airliners are chosen
from the airport's operator preferences in `traffic/hubs.toml`, with matching
callsigns and installed paints. Regional airlines can appear with turboprops or
regional jets, and a random share of general aviation uses registrations and light
aircraft, turboprops or business jets. Ambient arrivals are sequenced one at a time,
decelerate through final, and brake at the rate needed to reach taxi speed at the
first usable exit. They do not use maximum braking and then crawl the rest of the
runway. The exit must be beyond the aircraft's computed stopping distance; if no exit
qualifies, the arrival is not seeded rather than turning around at the threshold. CSL models animate through taxi-out, line-up, takeoff, climb, approach,
rollout and taxi-in; the phase states and runway-lighting policy are detailed in
[CSL animation and runway lights](#csl-animation-and-runway-lights) below. Startup,
cruise and shutdown phases belong to the complete-flight generated layer. Startup
never creates `TaxiIn` traffic; `TaxiIn` exists only after a landing. Surface
movement follows the taxiway graph, keeps a gap behind the leader, gives way to
traffic on the right, and allows only one aircraft on the runway at a time. The
older complete-flight generated layer remains available but is disabled in the
opening scene.

Visible airborne traffic is limited to three aircraft in the player's same-direction
flow, with at most two trailing. `same_direction_limit`, `trailing_limit`,
`same_direction_cone_degrees` and `same_direction_range_nm` in
`traffic/operations.toml` control the cap. The filed destination is prepared while
the aircraft is still high and 90 NM away, before the STAR entry, so arrivals are
already established when the player reaches the procedure. Airborne flights may start
up to 70 NM ahead of the player and are drawn once inside the display radius. A flight
that is already low keeps the 30 NM trigger. The early and terminal attempts are
tracked separately, so a scene that adds nothing before the STAR does not block the
terminal refresh.

**Settings → Traffic → Traffic debug** shows the exact planned hold point for each
ambient departure, the published airport hold lines, the current blocker and the
phase reason on the taxi map. The debug markers draw over the normal taxi traffic;
a red line runs to the blocker, and the hold point is a ring rather than a line to
the destination. `/debug/ambient` reports the graph route, hold distance,
chosen runway and taxiway sampling for the ambient layer; `/debug/generated` reports
the legacy generated layer.

Movement requires the appropriate controller clearance and readback. Taxi limits,
runway crossings, takeoff, airborne scopes, approach and landing keep separate
authority. Traffic is rechecked before a proposed airborne grant, after readback
and before an accepted but unstarted departure moves. A later conflict can revoke
permission and require a fresh exchange. Unpublished aircraft release their live
stand and surface ownership.

Airborne clearance limits have protected fallback holds; final limits use checked
missed-approach variants. Generated onward routing requires a separate traffic
check and readback, then leaves at the holding fix. The planner retains published
levels, including an explicit restriction on an immediately repeated holding fix.
Unknown levels, unsupported joins or unprotected paths are refused. These are
nominal simulation envelopes, not certified aircraft holding performance.

The static apron can publish before full-flight planning and retain its identities
when the full scene arrives. A parked aircraft waits for a complete departure before
joining the taxi/radio loop.
A separate local Tower layer can add a repeating GA circuit and prepared departures
when the network releases little traffic; it uses published frequencies, spoken
clearances, readbacks and the same scenery checks. Its local spare departure pool is finite; the network population worker supplies
additional complete flights as slots become available.

Untowered traffic requires a published CTAF/UNICOM frequency and no Tower service.
It announces its position and intentions without issuing controller clearances.
An occupied runway at final causes a go-around. Source tests cover circuits,
straight-in arrivals, departures and cross-country calls; full mixed-flight and
native acceptance remain open.

### CSL animation and runway lights

Every rendered CSL aircraft uses one animation state machine. The persistent ambient
layer and the complete-flight generated layer both supply the movement stage, speed,
turn rate, vertical speed and engine state, so models look consistent in each phase:

| Stage | Visual state |
| --- | --- |
| Parked, turnaround | Engines off, beacon and navigation lights off, gear down, flaps and spoilers retract |
| Pushback, taxi-out, taxi-in | Engines running, beacon and navigation lights on, gear down, flaps up, landing lights off; steering and wheels follow ground speed |
| Line-up | Takeoff flaps and strobes; landing lights stay off |
| Takeoff, initial climb | Landing lights on, rotation pitch, gear and flaps retract after rotation |
| Cruise | Gear and flaps up, landing lights off, beacon, navigation and strobes on |
| Approach | Landing gear down, landing flaps, landing lights and strobes on |
| Rollout | Spoilers and touchdown signal, gear down, landing lights on until runway exit |

Startup and shutdown live in the complete-flight layer: parked and turnaround
aircraft have their engines off, and the engine, beacon and navigation animation
spools up when pushback or taxi-out begins.

Animation rates come from `traffic/handling.toml`; the model dataref names come from
`traffic/animation-channels.toml`. The bundled catalogue covers the Bluebell/XPMP and
cjs/world_traffic channels. `/debug/ambient` reports the current stage, runway entry
and runway exit distance for each ambient aircraft.

Landing lights follow the Airservices Australia runway-safety guidance:

- crossing a runway: all exterior lights, including landing lights;
- line up and wait: all exterior lights except landing lights;
- take-off: landing lights on when take-off clearance is received;
- approach and landing: landing lights on;
- ordinary taxiing: landing lights off, beacon and navigation lights on.

The crossing practice is national guidance, not an ICAO rule, and it is not universal.
The FAA AIM recommends all exterior lights when taxiing on or across a runway at the
pilot's discretion; European and ICAO rules require night lighting but leave the
crossing procedure to the operator. There is also no universal numeric crossing speed.
Generated traffic crosses at `taxi_turn_knots` from the handling catalogue (12 kt with
the jet envelope); ambient aircraft use the same per-type taxi speeds.

See [validation](docs/VALIDATION.md) for current test results and earlier engineering
checks, and the [roadmap](docs/ROADMAP.md) for the remaining traffic scope.

</details>
