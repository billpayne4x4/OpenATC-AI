# Validation record

Authoring date: 2026-10-05. Source revision: 0.2.0.

## Executed

GCC 13.3 compiled the dependency-free controller and airport-data sources in C++17 mode with warnings treated as errors. All **94 checks passed**.

```bash
mkdir -p build/core-only
g++ -std=c++17 -O2 -Wall -Wextra -Wpedantic -Werror -Iinclude \
  src/core.cpp src/flight.cpp src/airport_data.cpp tests/core_tests.cpp \
  -o build/core-only/openatc-tests
./build/core-only/openatc-tests
```

Coverage includes phase-gated buttons and backend requests; clearance/readback sequencing; altitude and direct-to validation; ground/airborne transitions, pause and bounce hysteresis; go-around and landing; taxi graph paths, one-way restrictions, aircraft size and active-runway zones; parking completion; airport, parking, frequency, navaid and procedure parsing; localizer/glideslope encoding; descent geometry; dateline distance; flight-plan validation; METAR fields and transcript retention.

Python scripts passed syntax compilation. Shell scripts passed `bash -n`. Source delimiters and archive integrity were checked. These checks do not substitute for compiling the engine and UI.

## Not executed

- Full CMake engine, desktop or X-Plane plugin builds. CMake and development dependencies were unavailable in the authoring environment, and dependency/SDK downloads returned HTTP 403.
- The JSON/SimBrief parser executable and engine HTTP integration tests supplied in `tests/integration_tests.cpp` and `scripts/test_engine.py`.
- Actual `ldd`/`nm` auditing of a newly built plugin, installation into X-Plane or simulator startup.
- Rendered UI inspection, audio-device capture/playback, STT/TTS round trips, real SimBrief OFP downloads or weather API calls.
- Windows/macOS compilation or CI execution.

No working binary or simulator screenshot is claimed. The Fedora build script and CI configuration are the next integration gates; integration fixes may still be required after dependencies become available.

## Steam Runtime / TLS correction

Muse reported that the previous plugin linked `libssl.so.3` and `libcrypto.so.3`, which X-Plane could not resolve inside Steam Runtime. The supplied `src.zip` contains the same seven source files as the supplied full-project archive; it does not contain Muse's reported CMake or dependency-audit changes. This revision implements those build changes in the complete project.

`openatc_transport_plain` includes httplib headers without linking its TLS-enabled CMake target. `openatc_ui_plugin` and the plugin use that plain transport. A compile-time guard rejects accidental `CPPHTTPLIB_OPENSSL_SUPPORT` in the UI. The engine retains HTTPS and proxies SimBrief, weather, AI, STT and TTS. This also permits HTTPS speech providers without linking TLS into the simulator plugin. The plugin uses an `$ORIGIN` RPATH.

The Linux audit rejects missing dependencies, X11/XCB/GLX and legacy GL dependencies, and additionally rejects SSL/crypto/curl dependencies in `.xpl` files. It checks all five required XPlugin exports. The script was syntax-checked here, but only a build on the target host can establish its actual linked dependency list.

## Runtime checks after building

1. Run the Fedora plugin build and `python3 scripts/test_engine.py build/wayland` as shown in the README.
 2. Install the staged plugin with X-Plane closed, then launch X-Plane (the plugin starts its bundled engine itself; `~/.config/openatc/engine.log` shows its output). Check `Log.txt` for OpenATC startup and loader errors.
3. At a loaded airport, confirm that parked controls exclude altitude/direct-to, aircraft position matches the surface map, and taxi is unavailable before clearance readback.
4. Import the latest SimBrief OFP and compare its route/procedures, fuel and payload with the SimBrief output before accepting it.
5. Check font clarity, centered menu labels, resizing, request dialogs and layer filters on the target display.
6. Select microphone/output devices; use the local input monitor before enabling transcription, controller speech or copilot speech. Check separate volume controls and COM1 tuning on an explicit airport frequency assignment.
7. Fly a departure, descent, go-around and landing. Confirm stage transitions and that taxi-in finishes only when stopped at the assigned stand.

Weather awareness is controller-spoken METAR hazard advisories, not precipitation radar. Procedure lists do not implement full procedure-leg navigation. Taxi approval covers the loaded graph; dashed ramp connectors are unverified and runway-crossing clearances are not implemented. These limits are also stated in the UI and README.

## 0.2.1 branding follow-up

The vector logo asset was rendered and inspected, branding references were checked, and sidebar wordmark dimensions were checked over the supported UI scale range. The complete plugin build and X-Plane rendering were not executed in this workspace. The 94 core checks above describe revision 0.2.0; this branding follow-up does not change controller behavior or font rendering.
