# Local AI server

The AI server provides the LLM, recognition and synthesis behind one HTTP address. It can run on the simulator computer or another Linux, Windows or macOS machine. The X-Plane plugin and its companion controller engine stay with the simulator. Moving the AI server moves inference load; it does not move X-Plane rendering or the controller's simulator integration.

## Runtime files and startup

Use the complete AI-server package. It contains `openatc-ai`, the `openatc-stt` child executable, eSpeak phonemizer data, required packaged shared libraries and a launcher. Keep them together. Model weights are separate downloads, cached in the platform data directory.

```sh
# Loopback only, Kokoro by default.
openatc-ai

# Serve another computer on the LAN.
openatc-ai --host 0.0.0.0 --port 8099

# Smaller optional English TTS model.
openatc-ai --tts-provider kitten

# Choose where model files are stored.
openatc-ai --models-dir /path/to/models
```

In a packaged installation use `openatc-launcher` with the same arguments. Windows executable names end in `.exe`. The default port is 8099 and the default bind address is `127.0.0.1`. The included service does not authenticate callers; use a trusted network or provide your own access control and TLS termination.

Models are checked before the HTTP listener opens. The default manifest is compiled from `models.toml`; Kitten adds the pinned `models-kitten.toml` manifest and does not download Kokoro for a Kitten-only installation. Changing a manifest requires rebuilding. Files with an explicit hash are verified against it; legacy manifest entries without a hash record their first observed hash in `models.lock.toml` and verify it thereafter. Model weights are not included in Git or release archives.

The STT process is started automatically on a private loopback port. Do not point the plugin at that child port. Set all three service URLs in the plugin to `http://SERVER-IP:8099`, without `/v1`. Kokoro/Kitten run inside the server, while STT remains separate because the LLM and STT native libraries currently cannot coexist in one binary.

## Browser dashboard and setup

The packaged launcher opens a persistent dashboard hosted by the AI-server executable. The HTML, CSS, JavaScript and banner image are embedded; there is no separate web-server installation or remote web dependency. The default form schema is embedded too. An installed editable `server/settings-schema.toml` overrides the embedded form definitions; changing that schema requires reopening the manager. User settings and model weights remain external so upgrades can preserve them.

First run opens the guided wizard before downloading models. Afterwards use **Edit settings** for direct tabs or **Run setup wizard** to review the same settings step by step. Both are generated from the TOML field definitions. Qwen2.5 Instruct and Kokoro remain the defaults, with a fixed 4096-token context; automatic sizing is opt-in.

**Save and apply** validates the full configuration before stopping the running inference process. Invalid edits leave its saved settings and runtime intact. A successful save reloads the selected models automatically, with verification, download byte counts, component loading and readiness shown on the same page. The dashboard remains available throughout. A modal overlay blocks duplicate commands during load/unload, Save and apply, and deletion. It shows a spinner with actual verification/loading stages and download bytes when available. The overlay stays open until the requested operation completes, rather than closing as soon as the server accepts it. Errors expose a Dismiss action; connection failures never imply success. A valid setting can still fail to start because of an occupied port, insufficient memory or an incompatible model; the dashboard reports the error and keeps settings available for correction.

The **Models** panel combines model files, locations, download progress, loaded state and actions. **Delete** removes only that model’s known cached files, preserving their pinned checksums for future downloads; an enabled selected model must be unloaded first. Voice weights and their supporting data are grouped under one model row. **Edit** opens the appropriate direct settings tab. Each row has **Load / Unload** for its language, recognition or voice component. Loading another bundled model switches that component’s selection. Component changes automatically reload the inference runtime, so other enabled components briefly reload too; the dashboard stays available throughout. **Load models / Unload models** applies to the whole model set. Once all models are unloaded, inference health remains reachable and model-dependent endpoints return HTTP 503 with a clear explanation. A partial runtime reports its enabled components separately from HTTP liveness.

Resource readings identify the server host, rather than the browser’s computer. Overall CPU/RAM and NVIDIA GPU/VRAM counters are separate from inference-process figures. Recognition has its own process. When language and synthesis are both enabled, they share one process, so their table figures are marked **Shared**, include context/runtime overhead, and must not be added together. With only one of those components loaded, the process observation applies to that component and its runtime overhead. These are process observations, not exact tensor-allocation accounting. Unavailable per-process GPU counters remain unavailable; macOS unified memory is not represented as dedicated VRAM. `GPU requested` remains distinct from confirmed accelerator initialization.

```sh
# Dashboard with automatic browser opening and saved-model startup.
openatc-ai --manage-web

# Headless dashboard with a fixed management port.
openatc-ai --manage-web --no-browser --dashboard-port 8100

# Trusted LAN management using this server's actual interface address.
openatc-ai --manage-web --no-browser --dashboard-host 192.168.1.10 --dashboard-port 8100
```

The management listener defaults to loopback and has no login. An explicit LAN bind is for trusted networks. Mutations require the dashboard's exact browser Origin. Do not expose it directly to the public internet. The plugin continues to use the inference endpoint, normally port 8099; do not substitute the dashboard port in plugin settings.

The manager owns its inference child and speech sidecar. Reopening the same configuration reuses its existing dashboard without changing its model load state. Unloading or applying settings stops the owned process tree; an unrelated service on the requested inference port is reported rather than terminated. A manager pipe closure also terminates its managed child tree. Desktop process shutdown and Windows/macOS native behavior still need their platform checks.

### Desktop tray

`tray-enabled` in `server.toml` accepts `auto`, `on` or `off`. `auto` starts the tray only when a Linux desktop session and session bus are available. Headless services log `Tray disabled: no desktop session` and continue. The tray shows the manager status and release and performs Hara-kiri. Everything else stays in the web dashboard, which remains the full control surface.

```sh
openatc-ai --tray                 # force an attempt
openatc-ai --no-tray              # force it off
openatc-ai --tray-helper PATH     # use a specific helper
```

The Linux helper uses StatusNotifierItem/AppIndicator. GNOME needs its AppIndicator extension; KDE supports it directly. Windows and macOS currently use the web dashboard; their native tray backends are not part of this build.

The dashboard header shows the manager release and the running inference child version, so an old child is visible without reading the logs. **Hara-kiri** stops the managed inference and speech processes and then exits the manager; the dashboard closes. It asks for confirmation first. Restart the service with systemd or the packaged launcher. The web page remains the control surface on headless servers.

## Compute flags

GPU is preferred when the build has a suitable backend and the runtime can initialize it. CPU remains available for every component.

| Option | Meaning |
| --- | --- |
| `--force-cpu` | Use CPU for all three components. |
| `--force-llm-cpu` | Put the language model on CPU. |
| `--force-stt-cpu` | Put Whisper on CPU. |
| `--force-tts-cpu` | Put the selected Kokoro or Kitten model on CPU. |
| `--tts-provider kokoro` | Load Kokoro v1; this is the default. |
| `--tts-provider kitten` | Load Kitten Mini 0.8. |
| `--ctx-size 4096` | Set LLM context capacity; 4096 is the default. Larger contexts need more memory. |
| `--stt-backend auto` | Prefer the compiled recognition accelerator, with CPU fallback. |
| `--stt-backend cpu` | Legacy recognition-only CPU choice. |
| `--stt-backend cuda` | Require a CUDA-enabled Whisper build; not needed for automatic selection. |
| `--stt-bin PATH` | Override the automatically located STT helper executable. |

`--force-cpu` wins over every individual setting. `--force-stt-cpu` also overrides `--stt-backend`. No flag is needed to request the normal GPU-first behaviour.

For a constrained GPU, start by moving TTS to CPU:

```sh
openatc-ai --tts-provider kokoro --force-tts-cpu
openatc-ai --tts-provider kitten --force-tts-cpu
```

For a CPU-only server:

```sh
openatc-ai --force-cpu
```

A missing provider or failed GPU initialization falls back to CPU. Speech also performs a short warm-up before the service is ready: some cuDNN dependencies are loaded only when the model first runs. A failure during that warm-up selects CPU and records the error in health diagnostics. Later inference errors still return an error and are logged with the attempted text and voice; a health response cannot guarantee every future request will fit in memory.

## GPU requirements and source builds

A driver, model download and accelerated binary are separate requirements. A plain CPU build cannot gain a GPU backend just because a GPU is present.

### Linux and Windows

The native AI release workflow enables Vulkan for Qwen/Whisper and CUDA for ONNX TTS. This supports Vulkan LLM/STT on suitable NVIDIA, AMD or Intel drivers; CUDA TTS requires NVIDIA. Other machines can use CPU TTS alongside Vulkan LLM/STT.

For the same source configuration, install the Vulkan SDK and make `VULKAN_SDK` and its tools available, then run:

```sh
cargo build --locked --release -p openatc-ai -p openatc-stt \
  --features openatc-ai/vulkan,openatc-stt/vulkan,openatc-ai/tts-cuda
cargo xtask install-ai /path/to/openatc-ai
```

Use one line in PowerShell, or its backtick continuation syntax. Windows builds require Visual Studio C++ build tools, Clang/libclang and CMake. The workflow installs a pinned Vulkan SDK rather than assuming it is on the runner. For accelerated Windows builds, use a Visual Studio developer shell with Ninja available on `PATH` and set `$env:CMAKE_GENERATOR = "Ninja"` before Cargo. The Visual Studio project generator can exceed Windows path limits in the nested Vulkan shader-generator build. When switching an existing build from the Visual Studio generator, remove the affected generated CMake caches first; do not reuse them with Ninja.

The current `ort` prebuilt CUDA provider uses **CUDA 13**. Tested Linux runtime libraries include CUDA runtime, cuBLAS/cuBLASLt, cuRAND and **cuDNN 9**. An NVIDIA driver alone does not provide those user-space libraries. `libonnxruntime_providers_cuda` and its shared-provider companion ship with the GPU build; NVIDIA runtime installation remains a host requirement. The CUDA build feature does not bundle the entire CUDA toolkit into OpenATC.

On Linux, ensure the matching shared libraries are discoverable by the loader, using the system linker configuration or `LD_LIBRARY_PATH`. On Windows, the matching DLL directories must be on the process `PATH`. ONNX may look for the unversioned `libcudnn.so` loader name on Linux; some runtime-only archives contain only `libcudnn.so.9`, so preserve the installation's expected symlinks. Do not mix major versions or copy an unrelated CUDA DLL set into the application directory.

See [ONNX Runtime CUDA requirements](https://onnxruntime.ai/docs/execution-providers/CUDA-ExecutionProvider.html) and [LunarG's SDK instructions](https://vulkan.lunarg.com/doc/view/1.4.341.1/windows/getting_started.html). These dependencies have their own licenses and driver requirements.

Whisper chooses a discrete GPU before integrated graphics when both are visible, rather than assuming device zero is the intended card. GGML's `GGML_VK_VISIBLE_DEVICES` can restrict Vulkan devices for diagnosis, but device indices can change between driver versions. Check startup logs before using a saved index.

### macOS

The native macOS workflow enables Metal recognition and CoreML synthesis; the LLM's macOS backend comes from its native dependency. The Intel build compiles ONNX Runtime with CoreML support because the selected `ort` distribution does not provide an Intel Mac binary.

```sh
cargo build --locked --release -p openatc-ai -p openatc-stt \
  --features openatc-ai/tts-coreml,openatc-stt/metal
```

Intel Macs additionally need an appropriate ONNX Runtime library and the `ORT_LIB_PATH`/dynamic-link setup used by the workflow. On Apple Silicon, graphics memory shares system memory: the dedicated NVIDIA VRAM figures below do not describe Mac memory requirements. The new accelerated macOS build path still needs native CI and runtime validation.

### Linux binary compatibility

Native Linux server packages use an Ubuntu 24.04 build baseline and require glibc 2.39 or newer. The pinned ONNX Runtime 1.28 archive references C23 libc and newer C++ runtime symbols unavailable on Ubuntu 22.04. The plugin's separate Ubuntu 22.04 build baseline is unchanged. AppImage does not remove the host glibc requirement; Flatpak uses its own runtime. Building for older distributions requires compatible native inference libraries built for that distribution.

The release binaries keep a portable x86-64 CPU baseline. whisper.cpp otherwise compiles with `-march=native`, which ties the sidecar to the machine that built it; the workspace config sets `GGML_NATIVE=OFF` and ggml selects AVX2 and other kernels at runtime. AVX-512 is not required.

### CPU-compatible source build

```sh
cargo build --locked --release -p openatc-ai -p openatc-stt
cargo xtask install-ai /path/to/openatc-ai
/path/to/openatc-ai/bin/openatc-ai --force-cpu
```

The explicit runtime flag ensures CPU use even if the native dependency or a previously selected build feature includes acceleration. CPU-only inference needs sufficient system RAM for models, context and working buffers; the model file sizes alone are not a full RAM budget.

## Models and voices

| TTS model | Model file | Voice data | Presets | Text preparation in OpenATC |
| --- | --- | --- | --- | --- |
| Kokoro v1 | About 326 MB ONNX | About 28 MB | 54 total; 28 English | English Misaki phonemes |
| Kitten Mini 0.8 | 78,268,016 bytes ONNX | 3,278,902 bytes plus 470-byte config | Eight English | English eSpeak phonemes and Kitten's token/style format |

Both models produce mono 24 kHz audio, returned as 16-bit WAV after the configured radio effects. File sizes are storage sizes, not VRAM usage. Kitten Mini uses the upstream aliases and speed priors, with sentence chunking for longer transmissions. It is distinct from KittenTTS2.

Kitten's identifiers are `kitten-bella`, `kitten-bruno`, `kitten-hugo`, `kitten-jasper`, `kitten-kiki`, `kitten-leo`, `kitten-luna` and `kitten-rosie`. `/v1/voices` returns their names and English language metadata. Kokoro's table supplies its own IDs, such as `af_bella` and `bm_george`.

The controller engine refreshes the provider roster and replaces unavailable saved IDs when necessary. New controllers use unassigned voices before reuse; a provider with eight presets cannot give hundreds of controllers globally unique voices. Controller identity persistence is independent of a flight reset. Use the plugin's voice tester to choose voices you find intelligible, especially for callsigns, taxiway letters and similar-sounding numbers.

English phonemization is currently deliberate. Choosing a foreign-language preset is an accent/intelligibility experiment with English text, not full language support. Quality depends on the sentence and voice. The automated audio tests verify valid output and recognition of test samples; listening in the simulator is still required.

## HTTP API

| Endpoint | Purpose |
| --- | --- |
| `GET /health` | Readiness, models, selected TTS provider, compute choices and fallback reasons. |
| `GET /v1/voices` | The selected model's real voice roster. |
| `POST /v1/chat/completions` | Non-streaming chat completion; optionally constrained to a JSON object. |
| `POST /v1/audio/transcriptions` | Multipart `file` containing 16 kHz mono PCM WAV. |
| `POST /v1/audio/speech` | JSON speech request; returns PCM WAV. |

The bundled server serves its configured local models. The `model` field is API compatibility, not an instruction to download an arbitrary model on demand. It does not implement every option offered by external OpenAI-compatible services.

```sh
curl http://127.0.0.1:8099/v1/audio/speech \
  -H 'Content-Type: application/json' \
  -d '{"model":"tts-1","voice":"af_bella","input":"Charlie Golf Tango Lima Tango, hold short of runway two five right."}' \
  --output radio-check.wav
```

With a Kitten server, replace the voice with `kitten-bella`. Unknown Kitten IDs return an error so stale settings can be corrected; refresh the roster after changing models.

```sh
curl http://127.0.0.1:8099/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"openatc","messages":[{"role":"system","content":"Return a JSON object with intent altitude and altitudeFeet from the pilot request."},{"role":"user","content":"Request seven thousand feet."}],"response_format":{"type":"json_object"},"max_tokens":100}'
```

JSON grammar prevents free-form prose around the result. The companion engine still validates fields, values, authority and flight-state eligibility. A generated object is not an ATC clearance.

## Measurements and reproduction

The README contains the measured comparison for the test machine. Treat it as a dated measurement, not an exact requirement for every GPU. Model version, utterance length, voice, context, allocator caches, simultaneous jobs, drivers and other applications affect memory. Retained memory after inference can exceed the freshly loaded model's footprint.

For NVIDIA measurements, record a baseline before starting the test instance, then sample the whole device and processes while loading and generating:

```sh
nvidia-smi --query-gpu=name,driver_version,memory.total,memory.used,memory.free --format=csv
nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv
nvidia-smi --query-gpu=timestamp,memory.used --format=csv,noheader,nounits -lms 100
```

Use the same LLM, context size, Whisper model and sentences for Kokoro and Kitten. Measure a fresh instance with `--force-tts-cpu` to establish the LLM/STT portion, then repeat without the override. Subtracting those runs gives an approximate extra TTS allocation, not the isolated model weight size. Polling can miss peaks shorter than its interval.

The Rust audition example writes the same sentence in each loaded preset and prints generation/audio durations:

```sh
cargo run --release -p openatc-tts --example audition -- \
  kitten /path/to/models/tts/kitten /tmp/kitten-audio --cpu
cargo run --release -p openatc-tts --example audition -- \
  kokoro /path/to/models/tts /tmp/kokoro-audio --cpu
```

Omit `--cpu` and build the example with `--features cuda` or `coreml` for an accelerator test. Default Cargo builds without a provider feature use CPU. Compare generation time with audio duration: less generation time than audio time means synthesis is faster than real-time for that sample, not zero delay before speech starts.

## KittenTTS 2 evaluation

The 13 October 2026 evaluation used the upstream `kittenml` 0.9.4 package, PyTorch 2.8.0 with CUDA 12.8, Transformers 4.57.6 and model revision `f2eee74f4467855ad58fe16f1fb7b7df473263fd`. A separate Rust benchmark called the package through PyO3. This was an isolated experiment, not an additional OpenATC runtime dependency or a released provider.

The loaded model exposed 47 voices, including 38 English voices. Tests used Bruno, Bella, Marcus, Martha and French, with `weights="emb4"`, CUDA, the stable preset, seed 1234, a 700-token limit and already-expanded ATC text (`normalize=false`). Kitten's speech language model used bfloat16; this GPU has compute capability 7.5 and lacks native bfloat16 hardware support. PyTorch can still execute this configuration. CPU thread count was four.

| Measurement | Student decoder (`student_w4`) | Default decoder |
| --- | ---: | ---: |
| Model load, measured run | 25.85 s | 19.56 s |
| First radio-check generation | 13.02 s | 3.15 s |
| Repeated heading/climb generation, three calls | 5.62–5.66 s | 5.67–5.69 s |
| Heading/climb audio duration | 7.32 s | 7.30 s |
| Crossing instruction, Bella | 9.42 s | 9.55 s |
| Handoff, Marcus | 5.48 s | 5.44 s |
| IFR clearance, Martha | 9.89 s | 9.62 s |
| English landing instruction, French voice | 4.26 s | 4.26 s |
| Whole-device GPU peak | 10,652 MiB | 10,832 MiB |
| Increase above the 5,564 MiB baseline | 5,088 MiB | 5,268 MiB |

Model-load and first-use figures came from successive runs and can include different cache/initialization work; they are not a controlled cold-start comparison. Generation timing synchronizes the GPU before and after the complete synthesis call and excludes WAV-file writing. Audio was mono 24 kHz, finite and nonempty in all 16 requests. The default-decoder heading sample, resampled to 16 kHz, was transcribed by Whisper Tiny with the intended callsign, heading 270 and 5,000-foot altitude. That single check does not establish pronunciation quality across the roster.

The pre-existing OpenATC service retained Qwen2.5 7B Q4_K_M, Whisper Tiny and Kokoro throughout. Device peaks were sampled every 100 ms. No deliberate concurrent inference ran during these samples. The totals therefore include **two TTS models**, and the increment is a measured difference from that baseline, not an isolated tensor allocation. Other utterances or concurrent work may need more memory. NVIDIA reported another 429 MiB reserved separately, leaving very little headroom in the default-decoder run.

The small download uses compressed storage rather than compact GPU arithmetic. The selected LM was 492,114,467 bytes, the optional student decoder 38,817,934 bytes, and the upstream runtime additionally fetched `s3gen_meanflow.safetensors` (1,064,875,036 bytes) from `ResembleAI/chatterbox-turbo`. It keeps the original decoder components even when selecting the student flow. This is why a roughly half-gigabyte checkpoint is not a half-gigabyte installation or VRAM requirement.

A direct FP16 conversion initially exceeded available GPU memory while holding old and new tensors. Performing the conversion on CPU avoided that temporary allocation, but synthesis then failed on invalid probability values. No FP16 performance or compatibility is claimed. The optional vLLM backend and CPU performance of version 2 were not tested.

After Kitten exited, three Kokoro calls generated the same heading/climb text in 0.29, 0.21 and 0.17 seconds according to the server log. End-to-end HTTP times were 0.47, 0.27 and 0.47 seconds, including transfer overhead. Kokoro produced 7.21 seconds of audio with `af_bella`. These different speakers do not constitute a listening-quality ranking, but the measured delay supports keeping Kokoro for interactive ATC.

The raw measurements, voices, phrases and scope are in [the benchmark record](benchmarks/kitten-tts-2-2026-10-13.json). Upstream references: [model card](https://huggingface.co/KittenML/kitten-tts-2), [GPU weight loading](https://github.com/KittenML/KittenTTS/blob/main/kittenml/kittentts2/weights.py) and [audio decoder loading](https://github.com/KittenML/KittenTTS/blob/main/kittenml/kittentts2/vocoder.py). The model weights have a [separate community licence](https://huggingface.co/KittenML/kitten-tts-2/blob/main/LICENSE.md); the package's Apache licence does not cover those weights.

### Native Kitten experiment

A second isolated prototype uses Rust, stock `llama-cpp-sys-2` 0.1.158, `tch` 0.22 and CPU LibTorch 2.9.0. It reads the same pinned model revision's native speaker projections and student decoder. No Python interpreter or helper process runs during synthesis. This is experimental code, not a released provider; the production server still defaults to Kokoro.

The upstream TQ2_1 tensor type requires Kitten's custom native runtime. The prototype instead repacks each ternary scale into four standard Q4_0 blocks, preserving the scale and -1/0/+1 values. The resulting speech model is 1,447,458,656 bytes. Its TorchScript student decoder is 312,339,253 bytes and prepared voice data is 16,739,255 bytes. The LibTorch runtime also occupies disk and RAM; those model sizes are not the complete installation or memory budget.

With both attention and operation offloading disabled, three four-thread CPU runs generated “Charlie Golf Tango Lima Tango, runway two five left, cleared for takeoff.” in **17.18, 17.46 and 16.03 seconds**, excluding model/context loading and file writing. Audio lasted 5.08, 5.08 and 5.24 seconds. The last sample, resampled to 16 kHz, was transcribed correctly by the existing Whisper service. This only checks one voice and one instruction, not all 47 voices or complete pronunciation quality.

The Vulkan attempts on the RTX 2080 Ti returned non-finite logits. Disabling flash attention, using float32 key/value caches and disabling Vulkan half-precision/cooperative-matrix paths did not resolve it. No successful native GPU timing or VRAM requirement is claimed. The earlier CUDA/PyTorch timings above belong to a different runtime. The supported Kokoro service was not replaced during these tests.

See the [native result record](benchmarks/kitten-native-2026-10-13.json) and [upstream native format documentation](https://github.com/KittenML/kitten-tts-2-cpp/blob/main/tools/kitten-tts/README.md). Native Kitten integration, full-roster validation and platform packaging remain unfinished.

## Running as a service

A systemd user-service example is supplied at `packaging/systemd/openatc-ai.service`. Its default bind is loopback. Adjust the executable path, host and flags for your installation, and provide GPU-library environment paths if needed. Restart the service after changing startup options; the plugin's wording-variety checkbox does not change the server's compute device.

Capture stdout/stderr with the service manager. On Linux a user service can be inspected with `systemctl --user status openatc-ai` and `journalctl --user -u openatc-ai`. The companion's `engine.log` is a separate log on the simulator machine. Failed synthesis records the attempted phrase and voice; include the specific error and `/health` result when reporting a problem.
