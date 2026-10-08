<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/branding/logo-dark.png">
  <img src="assets/branding/logo-light.png" alt="OpenATC AI" width="600">
</picture>

# OpenATC AI

OpenATC AI is a free, MIT-licensed air traffic control plugin for **X-Plane 12**. It combines controller rules, editable TOML speech, voice input/output and cockpit crew conversations. The companion engine runs locally; optional AI and speech services can run on the same computer or another machine.

Linux is the current validated build target. Windows and macOS release packaging still needs validation. This is an active project: phrasebook coverage extends beyond the procedures implemented by the controller.

## Build and install

Install a current stable Rust toolchain, Python 3, a C/C++ compiler, Clang/libclang and pkg-config. Native dependencies are built through Cargo. From the repository root:

```sh
cargo build --locked --release -p openatc-plugin -p openatc-engine
python3 scripts/install-rust-plugin.py "/path/to/X-Plane 12"
```

Close X-Plane before installing. The installer validates speech and backs up the existing OpenATC folder outside the simulator's plugin directory. It installs `OpenATC/64/lin.xpl`, the companion engine, configuration and assets. Personal settings and existing station-role overrides are preserved. Restart X-Plane after installing.

Open OpenATC from X-Plane's plugin menu. In Settings, refresh audio devices, select your microphone and output, and test input. Linux uses PulseAudio, including PipeWire's PulseAudio compatibility server, with ALSA as a fallback. Microphone lists exclude playback monitors. System default follows the operating system's routing.

Configure AI, speech recognition and voice services in Settings. Service URLs are origins without a `/v1` suffix. Settings and engine output are stored at `~/.config/openatc/settings.json` and `~/.config/openatc/engine.log`. `OPENATC_CONFIG_DIR` selects another configuration directory. Optional provider credentials belong in `OPENATC_AI_KEY`, `OPENATC_STT_KEY` and `OPENATC_TTS_KEY`, never in speech files. `OPENATC_AI_URL` and `OPENATC_AI_MODEL` override startup settings. Separately chosen paid providers are optional.

The plugin starts its bundled engine when needed and shuts down the engine it owns when disabled or closed. A wall-clock heartbeat continues while the simulator is paused. Manually started engines and independently hosted AI/STT/TTS servers keep running.

## Fly a departure

1. Create a flight in Planning or import a SimBrief OFP. Check departure, destination, runway, route and callsign. Clearance requests use the displayed Planning flight automatically.
2. Power the aircraft radio. In Channels, select a published station to tune COM1. Delivery handles clearance; Ground or Tower assumes this duty when the airport has no separate service.
3. Request IFR clearance and read back the destination, route, altitude, squawk and runway with your callsign. Unclear or incorrect readbacks leave the clearance pending. **ATC Auto Reply**, beside Replay ATC, submits the complete pending readback or acknowledges start-up/pushback once.
4. Request start-up, pushback or both, then taxi from the appropriate controller. These permissions do not operate the aircraft or tug. Read back the taxi instruction and hold-short limit to activate guidance.
5. Stop at each holding point. An intervening runway needs an explicit crossing clearance and readback; Ground or Tower checks traffic and can issue it proactively. Once clear, read back the onward taxi instruction. At the departure runway, Ground directs you to the published Tower frequency. When already on Tower, the controller checks simulator-reported runway/final traffic and either clears departure or instructs you to hold. Missing traffic data keeps you holding. Runway crossings and backtracking need separate permission.

ATC requires a powered radio tuned to a published station within simulated reception range. Unassigned channels receive no reply. Controller duties come from installed airport/controller data, with combined duties inferred from available services and optional station overrides. No airport-specific frequency or duty is hardcoded.

Live sessions do not generate fictional aircraft calls or canned traffic clearances. Demo chatter is confined to demo sessions. Runway checks use X-Plane's traffic interface; aircraft absent from that interface cannot be detected. Full sequencing and wake separation are not implemented.

## Plugin pages

- **ATC:** text and voice conversations, pending clearances and readbacks. Talk addresses the copilot; Transmit addresses the tuned controller. Click a message for selectable text. Text fields support system copy/paste. Debug mode adds conversation export with flight/radio diagnostics and a text-file fallback.
- **Planning:** manual flight setup and SimBrief import. Review the imported flight before requesting clearance.
- **Channels:** receivable station names, services, frequencies, distance or sector coverage, and tuned status. The filter starts with the current airport ID; Show all clears it. Selecting a station tunes COM1. Click table headers to switch ascending/descending sorting; airport frequency, navaid, stand and procedure lists also support sorting.
- **Taxi:** departure/current airport by default, an explicit arrival selector, airport surface layout and approved route. Published taxi graphs take priority; painted centerlines provide a fallback where safe geometry is available. Speech uses published taxiway names when known and does not invent names for unnamed paths. Backtrack wording is used only for an authorized route along the runway.
- **Arrival:** planned and current descent profiles ending at the destination landing threshold, ground and safe corridor contours, a horizontal safe-altitude floor, transition reference and available ILS intercept reference. Missing terrain remains a gap. This is a geometric preview rather than a complete STAR/FMS path.
- **Airports:** search airports, procedures and services; view 2D/3D geometry and optional ILS/glideslope beams from published navdata.
- **Settings:** audio devices, gain, role volumes, voice effects/pacing, service endpoints, realism and optional AI wording variety.

Settings / Realism controls map arrows and illuminated simulator-ground taxi arrows separately. Only approved routes produce guidance; unsafe surfaces and missing terrain samples are skipped. Arrows stop at the clearance limit. Existing scenery runway guard lights are preserved; individual native holding-point lighting control has not been established.

The ToLiss A320neo profile matches A20N and maps its relevant power/radio controls. Other aircraft use standard controls or an aircraft-specific TOML profile. The plugin window has a bottom-right resize grip.

## Weather and station data

Stations are loaded from enabled airport scenery and active custom/default ATC data. Scenery priority and 8.33-kHz channels are supported. Published Tower controller definitions replace scenery Tower frequencies; Ground and Delivery remain sourced from airport scenery. Approach and Center reception uses horizontal coverage plus a simulated 150 NM margin. Transmitter locations and terrain shielding are not modelled.

Tuning a receivable ATIS starts a repeating broadcast with an electronic voice effect. Retuning, radio power loss or loss of reception stops it. Weather comes from X-Plane at the station's airport; unavailable distant observations are reported as unavailable. Online METAR data is a separate planning feature. ATIS currently uses English, identifies the planned runway as planned, and does not provide complete regional diction or NOTAM coverage.

## Edit speech and configuration

| Path | Purpose |
|---|---|
| `aircraft/*.toml` | Aircraft identities and cockpit control/dataref mappings. |
| `regions.toml` | Regional units and altitude/procedure references. |
| `radio-stations.toml` | Combined duties for existing published stations. |
| `intents.toml` | Requests, aliases and examples. |
| `speech/runtime/responses.toml` | Live controller, pilot readback, ATIS and crew response templates. |
| `speech/common`, `speech/<region>` | ATC examples divided into IFR, VFR and shared situations. |
| `speech/crew` | Global copilot, attendant and ground-service examples. |
| `prompts/*.txt` | Classifier and crew instructions. |

Read [speech editing](speech/README.md), [runtime templates](speech/runtime/README.md) and [the situation index](speech/INDEX.md). Runtime alternatives must preserve their named placeholders; startup validates these contracts. Wording edits cannot grant runway permission, change operational values or implement a procedure. Regional folders describe their review boundaries; they do not imply every local procedure has been verified.

**Allow AI wording variety** is off by default: ATC uses the validated TOML phrases as written. Turning it on permits natural wording based on the phrase examples. When enabled, the model receives all matching phrase alternatives for the selected role, region and flight rules, without a first-four/first-six cutoff. Examples guide fresh wording rather than verbatim copying; standard ATC terms may repeat. Changes to operational tokens cause a fallback to the original response. Controller authorization remains in Rust.

Validate edits without launching X-Plane or AI services:

```sh
target/release/open-atc-engine --check-speech speech
cargo test --locked -p openatc-core --test shipped_config --test speech_library
python3 scripts/test-radio-engine.py target/release/open-atc-engine speech
```

Restart the companion engine after changing speech or station duties. `OPENATC_SPEECH_DIR` or a complete speech copy in your personal configuration directory provides a custom phrasebook independent of installation updates.

## Development

The Cargo workspace lives at the repository root; first-party implementation is under `crates/`. The legacy top-level C++ source and CMake build were removed. Native audio adapters and third-party SDK, UI and model libraries still contain C/C++ and retain their licenses. Shared fixtures live in `tests/fixtures/`.

```sh
cargo fmt --all -- --check
cargo test --locked -p openatc-core -p openatc-engine -p openatc-ui -p openatc-plugin
python3 scripts/check_linux_dependencies.py target/release/libopenatc_plugin.so target/release/open-atc-engine
python3 scripts/package.py linux-x64
```

The package is written under `packages/`. Both `packages/` and `target/` are ignored, along with local assistant configuration, instruction files, histories, generated plans, model downloads, logs and personal environment overrides. `.env.example` and `.env.sample` remain eligible for inclusion. Product AI code, `models.toml`, `prompts/`, `speech/`, Cargo configuration and CI workflows remain project files. The patched `vendor/xplane` source is explicitly included. Ignore rules do not remove files that were previously tracked; maintainers handle that when preparing their commit.

Keep comments focused on behavior, constraints and the reason for a decision. User-facing changes should update this README and [the changelog](docs/CHANGELOG.md) together.

See [workspace/runtime notes](docs/RUST_WORKSPACE.md), [architecture](docs/ARCHITECTURE.md), [validation and simulator checklist](docs/VALIDATION.md), [remaining work](docs/ROADMAP.md) and [third-party notices](docs/THIRD_PARTY.md). Successful builds and automated checks do not replace testing the plugin, audio and weather inside X-Plane.

Position requests use an offline GeoNames settlement lookup to report a nearby town or city with approximate distance and direction. Names are factual lookup results, not generated by the LLM. Small settlements and administrative boundaries are not fully covered; the receiving airport is the fallback. Failed TTS requests log the attempted text, voice/model and error or HTTP status in `engine.log`; synthesis failures also log text in the AI server log.

Realism presets are **Relaxed**, **Standard** and **Strict**. Standard and Strict enable **Monitor assigned altitude**: after an acknowledged clearance, a continuing airborne altitude deviation receives three corrective calls, followed by cancellation on the fourth. Correcting the deviation resets the count. This is an optional simulator policy, not a real-world ATC rule. Disabling it leaves readback checks and radio reception rules unchanged. An amended altitude or route still requires controller approval. Heading, speed and published SID/STAR leg constraints are not yet monitored by this setting.

Crew controls now use stock X-Plane mappings by default, with third-party overrides in the aircraft TOML. The A320neo profile observes ATTN/CAB and MECH/INT commands for cabin and ground routing. Settings → Aircraft controls lists live availability. Explicit control requests, cabin brightness/slide arming and chocks/external-power requests wait for simulator confirmation; button-only mappings report a press. Editable checklists support captain-response and copilot-performing modes. All spoken wording is in speech TOMLs. See [Aircraft controls and checklist editing](docs/AIRCRAFT_CONTROLS.md) for examples and current ToLiss pushback limitations.

Cabin conversation receives the live flight phase and aircraft ground state. Greetings do not trigger cabin preparations. ToLiss pushback can be started through ground crew using settings already entered in its ground panel; distance and angle cannot yet be changed through OpenATC.

Ground-crew pushback requests use the aircraft’s existing ground-panel settings even when the spoken request includes a distance or angle; those spoken parameters are not applied. Attendant replies use the Attendant badge and voice. Unmapped ground-service connection requests report an unconfirmed service instead of claiming completion.

The cabin phrasebook includes friendly interphone conversation throughout the flight, plus separate examples for requested passenger announcements. All wording remains editable in speech TOML; these examples do not trigger announcements or invent cabin events.

ATC phrasing rotates independently of other conversation messages. The engine remembers phrase choices and previous station/task wording across flight resets and restarts. With AI wording variety enabled, role-aware prompts use the phrasebook as examples; otherwise the controller uses TOML wording directly. Protected clearance facts remain unchanged.

ATC frequencies in chat are underlined links: click a decimal COM frequency to tune COM1 using the same aircraft-control path as Channels. The radio must be powered. The panel uses a lighter, more opaque tint with one consistent rounded edge; the current renderer does not blur the simulator image.

Ground-control confirmations wait for stable simulator readback. Requests naming two controls are clarified rather than partially executed. Hold-point detection allows for the aircraft reference point behind the nose, while runway entry still requires a controller instruction.

IFR clearances omit readback coaching. After a correct readback, Delivery assigns the published Ground frequency for start-up/pushback when ready, where a separate Ground station exists. Readback validation and separate movement permissions remain in force.

Start a fresh flight with the **New flight** icon (a page with a plus) in the window header. Confirm to clear the plan, conversation, clearances, taxi guidance and pending crew actions. Settings, controller voices and the aircraft's actual controls are kept. This works without disabling the plugin or restarting the AI server. Re-enable now reuses the window renderer; live reset and re-enable acceptance are tracked in [Validation](docs/VALIDATION.md).
