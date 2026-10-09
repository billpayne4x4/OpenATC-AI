Debug surface jumps and addressed controller calls: target tests verify taxi permission and ground-state gating, heading and safe hold position. Radio integration checks no handoff while stopped with takeoff clearance, addressed controller messages and no backwards Delivery redirect for repeated clearance requests. SDK repositioning in ToLiss has not been exercised live.

Airport-filter and circle-rendering revision: default workspace tests, the UI pavement-containment regression, release build and radio integration pass. The regression checks KLAX surface geometry overrides 57CA reference-point selection and does not override it away from that pavement. Circle geometry is now two triangles with one shared alpha texture. Simulator appearance still requires a live check.

Holding-marker approach-side revision: tests cover both approach directions and an oblique approach, requiring the disc edge to remain at least four metres before the line. Default tests, release build and radio integration pass. The marker is now 30 metres across and uses the shared computed position on map and ground. Live scenery alignment remains user acceptance.

Successful crossing-readback regression: accepted readback produces no ATC transmission, enables the crossing, and far-side completion issues onward taxi. Default tests, release build, speech validation and radio integration pass.

# Holding-point marker — 9 October 2026

Default workspace tests, settings/plugin tests and release radio integration pass. The marker-placement test checks that it uses the route limit and approach direction; the setting defaults off. Clippy checks pass with existing advisory warnings. Ground terrain placement retains slope/footprint checks. Your local setting is enabled. The marker has not yet been visually exercised in X-Plane; live appearance and aircraft alignment remain simulator acceptance checks.

# Vacated reports and final holding-point handoff — 9 October 2026

Core/engine/UI tests and release radio integration pass. The radio sequence verifies “VACTED” produces the onward taxi instruction without “readback correct”, then its readback activates monitoring and stopping at the departure hold produces the Tower frequency. Core tests cover holding lines offset from graph endpoints and the surface lifecycle with readback preceding monitoring. Installed KLAX scenery regression also passes. Live simulator map colours, holding-line detection and physical taxi remain user acceptance checks.

# Controller voices and crossing completion — 9 October 2026

Core, engine and UI tests pass. Radio integration verifies distinct English Delivery/Ground voices, reuse of the saved Ground voice after a full engine restart, crossing completion while moving and onward taxi requiring readback. The installed KLAX regression checks the reported position clear of 25R and routes onward to the 25L hold. Physical simulator playback and taxi behaviour remain user acceptance checks.

# New flight and runway crossings — 8 October 2026

The release engine and plugin build passed. Rust core, engine and UI tests passed, including a delayed HTTP snapshot check that prevents an old flight from returning after reset. Clippy completed with the existing vendor and pedantic warnings; it was not warning-free.

The isolated radio integration verifies proactive Ground crossing clearance, holds when traffic data is missing or a runway is occupied, crossing readback, no crossing completion on the runway, and onward taxi to the original departure runway. The installed KLAX regression verifies that a route toward 25L first holds at 25R, crosses only 25R and continues toward the 25L hold. The airport identifiers belong to regression tests, not production routing branches.

Reset checks verify an empty plan and conversation, cleared permissions and crew actions, preserved live telemetry and monotonic transmission IDs, rejection of stale crew acknowledgments, and rejection of a delayed LLM response after reset. Crew integration also passed. The installed plugin and engine were updated after X-Plane closed.

The New flight icon and re-enable lifecycle change still require live simulator acceptance. The crash log identified OpenATC during re-enable; inspection found that the previous enable path created a second active ImGui context while retaining the first. Enable now reuses that renderer and avoids duplicate command registration. Yellow scenery guard lights remain unchanged; individual native light control has not been established.

# Crew control validation — 8 October 2026

The real engine crew integration passes role-specific bounded controls, heading 360 normalization, flight-level units, cabin slides/cross-check, no-action queries/negations, ground gear-up rejection, both checklist modes, failed-item stopping, idempotent acknowledgements, button-dispatch wording and aircraft replacement. This uses simulated plugin observations and acknowledgements. It does not prove live ToLiss writability or actual cockpit operation. The user must check ATTN/MECH selection, controls, cabin lighting and slides in the simulator. ToLiss pushback distance/angle are not implemented.

# Taxi fallback and Auto Reply — 8 October 2026

Core/engine/UI tests cover complete pilot readbacks, background traffic exclusion, centerline versus edge marking types, pavement gaps and holding-line barriers, assigned runway backtracking and rejection of other-runway crossings. The release engine integration verifies Ground-to-Tower handoff for runway taxi, mandatory backtrack readback, endpoint gating before departure and rejection of reused start/pushback acknowledgments. Installed VLVT scenery is separately exercised for runway 31 taxi and a holding point plus runway 13 backtrack. ATC was rendered at 960 and 520 pixels with Auto Reply beside Replay ATC. Live simulator message selection/system clipboard, physical taxi tracking and ground arrows remain user acceptance checks.

Run the installed-scene regression with `OPENATC_SIM_ROOT="/path/to/X-Plane 12" cargo test -p openatc-core --test airport_scenery installed_centerlines -- --ignored --nocapture`. Optionally set `OPENATC_ROUTE_REVIEW_PATH` to export the airport and calculated paths for geometry review. Production routing has no VLVT-specific branch; only this regression names the reported airport.

# Debug export and Planning activation — 8 October 2026

Removed sample departure/destination/runway defaults from live plans. YMLT/YMML remain in explicit historical test fixtures only. A clearance request submits the Planning draft, waits for successful engine acceptance, then transmits; plan validation failures stop the request. Conversation history is preserved through plan submission. Developer mode offers Copy ATC conversation, including active and draft flights and radio/ground telemetry; export formatting has a regression test. Clipboard helper failures produce a text-file fallback. Core/engine/UI tests and release radio workflow checks passed; live clipboard copying and simulator import-to-clearance are user acceptance checks.

# Clearance feedback and list controls — 8 October 2026

Core/engine/UI tests and strict Clippy passed. Real-engine checks confirmed a mismatched flight departure returns the same specific message in the transcript and result, and VLVT Tower still accepts a valid IFR clearance. The full radio workflow regression and dependency audit passed. Channels was rendered at 960 and 520 px with the nearest-airport filter applied; the narrow layout keeps Show all visible. Native simulator header clicks and COM1 tuning remain user acceptance checks. Sort support was added to every data-list table (stations, frequencies, navaids, stands and both procedure lists); form layout tables are unchanged.

# Consolidated controller duties — 8 October 2026

The real release engine, using installed VLVT scenery/navdata and isolated settings, accepted an IFR clearance on Tower 118.100 for a VLVT–VTUD plan. Regression tests cover Tower-only clearance/ground duties, Ground clearance where Delivery is absent, separate Delivery priority, and ATIS exclusion. Core/engine tests and strict Clippy passed, as did the existing full radio workflow integration test. Simulator UI acceptance still requires reloading the plugin.

# Channels controller-data correction — 7 October 2026

Loaded the installed scenery and active custom ATC data at VTUD and VLVT. VTUD now includes Ground 121.900, Tower 119.450 and 122.500, and Approach 126.200 (plus its shared 119.450 channel). At VLVT the loaded data provides Tower 118.100 and Vientiane Center 124.100/128.300. The initial list incorrectly filtered Approach by its assigned altitude and omitted adjacent regional Center coverage. A follow-up real-engine check at VLVT now verifies the screenshot’s Tower 118.100, Approach 119.700 and Hanoi Center 123.300 using installed data. Other published regional channels can also appear; the simulator airport pop-out is not a complete nearby receiver list. Tests cover Tower override, Ground preservation, FREQ/CHAN units, regional polygon reception, altitude limits, and longitude wrapping without opposite-hemisphere false matches. Core/engine/UI tests, strict Clippy, release radio integration and binary dependency audit passed. Live simulator Channels and COM1 selection still require a restart and user acceptance.

# Engine startup correction — 7 October 2026

The installed engine log reported missing `libssl.so.3` under Steam. Rebuilt the Rust engine using reqwest Rust TLS with bundled roots, and strengthened the dependency audit to reject OpenSSL in either runtime binary. Four engine unit tests and the release radio integration workflow passed. The release engine’s offline speech validation and the installed engine’s complete isolated radio HTTP workflow also passed through the installed Steam runtime launcher. Its remaining dynamic dependencies are libc, libm and libgcc_s. A live simulator restart remains the final plugin-launch acceptance check.

# Validation record

## Radio/departure sprint validation — 7 October 2026

The final Rust core, engine, audio, HTTP and UI run passed **74 tests**, with **two environment-dependent checks ignored** (live devices and installed scenery). Strict Clippy passed those five packages with the existing `too_many_lines` allowance. Release engine/plugin builds passed. Plugin Clippy completes with existing pedantic warnings in the older arrival/render/window code; the vendored SDK retains its documentation/import warnings.

The real release engine HTTP integration test uses isolated settings/scenery and a local fake model: station discovery and named services; silence on unknown channels, radio power loss and range loss; Delivery/Ground role restrictions; validated plan/runway, clearance and readback; separate start/pushback permissions, combined request and a generated taxi route; airport-specific simulator weather, ATIS information-letter changes, destination weather isolation, unavailable weather and rejected aircraft-altitude samples; and fallback when an LLM changes the clearance's operational values. No remote AI service is used by this check. Separate stalled-provider/client tests prove cancellation without waiting for a 30-second request timeout and worker joins before audio context destruction or client unload.

The real `/suggest` test passed three regional/rules selections and rejected three mismatches. Offline validation loaded 476 situations and 1,420 phrases. The installed Heathrow-area scenery produced 24 receivable stations; release indexing took approximately 2 seconds. Desktop Channels was rendered and visually inspected at 960 and 520 px, with complete narrow station cards. These previews do not prove simulator COM1 tuning or SDK weather values.

Simulator acceptance still needs the user to check: actual ToLiss power/COM1; microphone/STT and voice timbre; changing simulator weather while listening to local and destination ATIS; repeat broadcasts and stop-on-retune/power/range changes; SimBrief import or a manual plan followed by Delivery clearance/readback, Ground start/pushback/taxi; approved map and terrain-following night arrows; and pause/quit engine lifecycle. No claim is made that a live simulator flight or remote server was exercised in this sprint.

Reproduce the focused checks:

```sh
cargo test -p openatc-http -p openatc-core -p openatc-engine -p openatc-audio -p openatc-ui
cargo clippy --manifest-path Cargo.toml -p openatc-http -p openatc-core -p openatc-engine -p openatc-audio -p openatc-ui --all-targets -- -D warnings -A clippy::too_many_lines
python3 scripts/test-radio-engine.py target/release/open-atc-engine speech
python3 scripts/test-speech-engine.py target/release/open-atc-engine speech
target/release/open-atc-engine --check-speech speech
```

The older validation records below are historical checkpoints.


## Current Rust work — 2026-10-07

The records below the current section describe earlier C++ authoring and are historical, not the current deployment status. The Rust release engine/plugin have been built and installed on the local X-Plane target. Native selected-device capture/output, engine lifecycle ownership, ToLiss dataref type/tuning checks, configuration loading, terrain/descent geometry and desktop rendering have focused checks. Desktop map/Arrival images are previews; the Arrival terrain fixture is labelled PREVIEW and is not evidence of real simulator terrain.

The core/engine run passed 52 tests, with one installed-scenery test explicitly ignored. Focused speech/configuration tests and Clippy were rerun after the final regional filtering changes. Release engine/plugin builds passed; the vendored SDK retains existing warning messages. A local fake model exercised the real `/suggest` HTTP path: three regional/rules selections passed and three invalid/mismatched selections were rejected before model dispatch. This proves retrieval and endpoint integration, not a real model’s procedure accuracy.

The editable library has dedicated tests for every phrase rendering, historical IDs, regional longest-prefix boundaries, FAA/ICAO isolation, IFR/VFR separation, global crew selection, taxi-in phase normalization, missing context, malformed user edits and duplicate-ID diagnostics. `shipped_config` loads the real distributed files with application loaders. The offline engine `--check-speech` command also loads and renders the full library without calling AI or starting a service.

Source review uses FAA, EUROCONTROL, UK CAA, NAV CANADA and Airservices Australia references. The speech guide records the scope and retrieval limitations. Regional folder coverage does not mean every national procedure has been reviewed. Baseline-only packs say so.

Still awaiting simulator acceptance: powered ToLiss radio behaviour, selected-microphone transcription, terrain-following/night arrows, pause/shutdown behaviour during a real sim session, and flight descent/clearance handling. Windows/macOS builds, full traffic separation, comprehensive published-procedure execution and worldwide national procedure validation are not claimed.

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

IFR clearance messages explicitly request readback. Spoken readback matching checks NATO callsign/destination, route, altitude, squawk and runway; unclear values receive say-again rather than a phase error. Taxi instructions remain pending until readback is accepted. Ground guidance ends when stopped within 25 metres of the route endpoint. Protected runway edges are excluded; a reachable identified crossing hold point may end a partial route, without crossing permission. The UI registers a native system clipboard backend for external text-field copy/paste, with persistent ownership. Live simulator voice and clipboard interaction remain user acceptance checks.

Live controller responses, readback prompts/corrections, taxi instructions, ATIS/weather wording, crew fallback replies, copilot callouts and background exchanges now come from `speech/runtime/responses.toml` (191 templates, 381 phrases). The regional/crew example library retains 476 situations and 1,420 phrases. Runtime alternatives preserve named fact placeholders; startup rejects missing responses or changed placeholder contracts. Phrase edits do not change Rust authorization logic. Taxi readback uses structured taxiway/hold-short facts, so editable wording is not parsed as route data. An isolated engine regression edits the TOML acknowledgment and verifies that exact edited text is returned; invalid placeholder alternatives are rejected.

Pending readback evidence now takes priority over generic altitude/taxi intent classification. Regression coverage uses the reported VLVT→VTBS typed readback and garbled transcript; an ordinary taxi request is not treated as a readback. The plugin provides a visible bottom-right resize grip that respects minimum size and pop-out pixel scaling.

The exact reported typed readback passes the parser-priority regression and a numeric typed readback passes the isolated release-engine workflow. Synthetic UI mouse-down/drag events verify that the visible resize grip emits resize actions. The preview rendering was inspected; X11 desktop automation did not deliver its mouse press, so native simulator dragging remains a user acceptance check.

2026-10-08 taxi follow-up: core/engine/UI test suites and all 12 plugin unit tests passed. Isolated engine integration exercised the server-side Auto Reply endpoint for ordinary taxi and runway backtrack, verified taxi approval and no altitude acknowledgment, and rejected repeated automatic replies. Ground-arrow sampling and terrain-placement unit tests passed. Ground guidance was enabled in the workstation settings; visible rendering in X-Plane remains a simulator acceptance check.

Holding-point follow-up: core tests cover an empty valid scan, missing traffic data, runway occupancy, inbound final and outbound non-conflict. Isolated engine tests cover stopped endpoint detection, Ground handoff with Tower frequency, pause protection, Tower holding for final traffic without repeated messages, and automatic clearance/removal of taxi approval when traffic clears. Plugin tests cover ground-arrow sampling and terrain placement; duplicate holding-light markers have since been removed. Native holding-light control has not been established.

Legacy-source cleanup: the Rust plugin/engine release build and core/engine/UI/plugin test suites pass after removal of top-level src/include and the old CMake targets. The isolated radio engine integration also passes. The Fedora helper passes shell syntax validation. The Cargo-based Linux package was built locally and checked for binaries, speech, source/license notices and SHA-256 integrity; the updated GitHub workflow has not yet run on GitHub.

Root-workspace migration: the Linux plugin/engine release build, core/engine/UI/plugin tests, settings/platform/AI-core/audio tests, desktop compilation check, formatting, scoped Clippy, isolated radio integration and dependency audit pass from the repository root. The Linux archive was rebuilt with the new paths and verified for binary/docs/branding/lockfile presence and checksum. Python and Fedora script syntax checks pass. packages/ and target/ are listed in .gitignore, and vendor/xplane is explicitly allowed. No Git commands were used for the migration or readiness checks; index/staging and commit operations remain with the maintainer. GitHub CI and new live simulator acceptance were not run during this filesystem-only migration.

## Named positions and TTS recovery — 8 October 2026

Offline GeoNames tests cover the reported VLVT location, airborne/ground wording near Hobart, invalid coordinates and an ocean position with no nearby settlement. Core/engine suites passed after replacing the coordinate-response fixture. Restored eSpeak data alongside the existing fedora-server binary and restarted its AI service; live provider requests synthesized both the exact reported coordinate phrase and a named-place phrase into valid 24 kHz WAV audio. This does not establish every prior failure cause, but confirms coordinates themselves can synthesize. Failed-text diagnostics were added to engine and server source; simulator acceptance remains pending.

Phrase variation checks cover independent runtime-template rotation, saved-history reload, different IFR clearance wording across flight resets, strict mode avoiding wording-model calls, and role/task/previous-wording context in variety prompts. Unsafe model clearances still fall back to checked TOML text.

Simulator follow-up checks produce ten distinct IFR clearance transmissions for identical plans across resets, map the chalks alias to the chocks action, reject compound partial actions, and exercise Ground-to-Tower/traffic-hold/clear-runway flow with ownship 35 m from the taxi endpoint. Native ToLiss control behavior and the revised panel edge still require simulator acceptance. The earlier missed KLAX trigger could not be replayed exactly because no live taxi-state snapshot remained after simulator exit.

Holding-marker size revision: plugin tests and release build pass. The translucent disc is 24 metres across, with terrain checks expanded accordingly. Live cockpit visibility remains to be checked; installed after X-Plane closed; cockpit appearance remains unverified.

Copilot radio replies now originate in the simulator after its speech queue drains, rather than being applied inside the engine request handler. The same endpoint handles onward taxi instructions and takeoff acknowledgements. Automatic tuning and cockpit actions wait for playback and pending copilot replies. Final holding-point telemetry does not invent a pilot ready report; holding recognition no longer requires zero taxi speed. Channels also loads local airport geometry (page 6) to identify the occupied airport. Regression checks cover deferred readback, takeoff acknowledgement, moving holding detection and installed KLAX positions; live ToLiss audio timing still needs simulator testing.

Radio turn-taking uses a two-step copilot exchange: record and play the readback, then apply it after playback finishes. Telemetry marks the radio busy during playback, recording or pending crew replies, delaying automatic surface and airborne calls. A previously reported ready aircraft can receive takeoff clearance after blocking traffic clears. Crossing eligibility follows holding-point detection without a conflicting stopped-aircraft gate. Unknown runway status produces an explained standby instruction and retries when confirmation becomes available. The debug header uses a vector fast-forward icon with the same dimensions as other controls. Unit and engine regressions cover prepared readbacks, occupied-radio suppression, moving hold recognition and traffic-clearance retry; embedded simulator layout and live audio need user verification.

Typed ATC requests add missing controller and callsign addressing. Spoken transmissions retain the words recognized by speech-to-text. AI intent recovery tolerates clear request-word typos and recognition errors; operational values and readback checks remain authoritative. Crew requests can contain up to eight explicit actions, such as “remove chocks and external power.” Clauses inherit the action verb where appropriate. All targets are validated against the aircraft profile before the batch becomes visible to the simulator; each action needs confirmation, and failure cancels the remaining actions. Duplicate controls and unsupported clauses are rejected. The LLM can return a bounded actions array using mapped control IDs, never raw simulator refs. Settings → Voices → Pilot (own transmissions) → Speak my transmitted requests enables synthesized playback of the pilot requests. It is off by default. Engine regression tests use mocked model responses and simulator acknowledgements; live control effects remain a simulator check.

## Platform build validation

On 2026-10-09, Linux cross-target checks passed for `openatc-core`, `openatc-settings`, `openatc-platform` and `openatc-http` on `x86_64-pc-windows-msvc`, `x86_64-apple-darwin` and `aarch64-apple-darwin`. An unused Windows import was removed. These checks compile the Rust code; they do not run Windows or macOS tests.

Full plugin/engine and AI cross-target attempts encounter missing native tools on this Linux machine. Windows requires MSVC tools such as `lib.exe`; Apple targets require a compatible compiler and macOS SDK. The native GitHub matrix is the next validation gate and has not been executed from this checkout. Run it manually after uploading the workflow, then download the artifacts and test inside X-Plane. Native build success does not validate microphone devices, rendering, aircraft controls, signing or model execution.
