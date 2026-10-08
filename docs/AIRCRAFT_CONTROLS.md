# Aircraft controls and crew checklists

Stock aircraft use built-in mappings for X-Plane cockpit datarefs and public cockpit commands. No aircraft TOML is required. Flap detents come from the loaded aircraft; fixed-gear aircraft do not offer gear retraction. A third-party profile overlays these standard mappings with its own controls. The A320neo profile is `aircraft/toliss_a320.toml`; an `openatc.toml` beside the aircraft's ACF takes precedence over the bundled profile.

Settings → Aircraft controls provides a search and live availability list. Available means that the simulator exposes the required writable dataref or command. It does not prove that a third-party aircraft implements a stock command correctly. Missing commands, read-only refs and failed state checks produce a failure reply instead of a completed-action claim.

## ToLiss communication selection

The profile's `cabin_commands`, `ground_commands` and `radio_commands` list commands to observe. ATTN/purser and CAB select cabin crew; MECH and INT select ground services; VHF selects radio ATC. Pressing the same call button again clears the selection. Selection persists after the momentary command ends. These listeners allow the original ToLiss command to run. An aircraft change clears the selection. The old purser names were commands, not readable call-light datarefs.

## Wording and mappings

Keep spoken wording in `speech`, not in the aircraft profile:

- `speech/runtime/responses.toml`: `crew_request_<control>` contains editable request anchors; `crew_control_<control>` supplies its spoken name; `crew_state_*` supplies the setting words; `crew_action_*` supplies confirmations and failures.
- `speech/crew/controls_and_checklists.toml`: pilot request/response examples and crew wording examples. Pilot entries illustrate the captain's part and are not spoken by the copilot.
- `aircraft/toliss_a320.toml`: simulator refs, commands, limits, crew roles, target values and checklist order. `label` is a UI label. Checklist `challenge` and `response` are speech keys, not spoken strings.

For example:

```toml
[aircraft.crew.controls.heading]
label = "Heading"
role = "copilot"
dataref = "sim/cockpit/autopilot/heading_mag"
min = 0
max = 359
on = 0

[[aircraft.crew.checklists.before_start]]
control = "seatbelts"
challenge = "checklist_seatbelts"
response = "crew_state_on"
value = 1
```

Commands use `on_command` and `off_command`, plus a readable `readback_ref`. `index` selects an array element. Without an index, array readback requires every element to agree. `write_scale` and `readback_scale` convert user units to simulator units and back. `states` maps words such as `armed`, `up` or `managed` to numeric targets. `integer` rejects fractional enum targets. `ground_only` requires stationary ground operation; `airborne_only` requires flight. A gear-up request is rejected on the ground.

`momentary = true` exposes a button press. The copilot reports that the command was dispatched and asks you to check the indication; this is not confirmation of an engaged system. The neo catalogue includes 463 additional button mappings. The built-in catalogue contains 1,326 stock cockpit commands. The running aircraft determines which resolve. Held controls are not reduced to an unattended button tap.

## Requests

Use Talk for the copilot. Transmit uses the selected radio/interphone recipient.

Examples: `set heading 270`, `set altitude flight level 240`, `speed 210`, `autopilot on`, `autothrust armed`, `gear down`, `flaps two`, `speed brakes armed`, `heading mode selected`, `cabin brightness 50 percent`, `arm slides and crosscheck`, `remove chocks`, `connect external power`. A heading of 360 becomes 000. Stock flap numbers are detents, not arbitrary percentages. Set a target and its managed/selected mode as separate requests.

The local parser handles explicit requests without an LLM. With AI intent classification enabled, free-form requests can return JSON containing an allowed control ID and numeric value. The engine validates role, limits and aircraft state; the model cannot supply a dataref or command path. It considers a bounded candidate catalogue and all matching pilot request examples. State questions do not operate controls. Negated requests leave them unchanged.

The plugin executes actions on the simulator thread. Target-setting replies wait for readback, with a three-second confirmation window. Failed actions stop a performing checklist. Acknowledgements are idempotent; a lost acknowledgement can be retried without pressing the button again. Aircraft replacement clears pending work. Reconnects and unavailable readings must not be treated as successful actions.

## Checklist directions

`Read the before start checklist` makes the copilot challenge one item and wait. Respond `on`, `off`, `up`, `armed`, `set` or `checked`, as appropriate. The observed control must match the configured expected value before advancing.

`Perform the before start checklist` makes the copilot set one item, verify it, announce the result and proceed. `Stop the checklist` ends it. Reading an individual mapped control's name reports its observed state.

The bundled lists are deliberately abbreviated editable examples, not a complete Airbus or operator checklist. Use your aircraft/operator procedure to add items and expected values. Do not copy performance-dependent flap or speed values into a fixed checklist. A speech example does not execute an aircraft action.

## Pushback limitation

ToLiss's installed Simulation Manual describes a straight distance in metres followed by a turn angle. Requests accept metres and degrees in either order; left/right describe the tail, with 90 degrees as the left/right default. The user's `%` shorthand is interpreted as degrees. Current parser limits are 1–200 metres and 0–180 degrees.

The installed neo exposes `toliss_airbus/iscsinterface/startPushBack`, but writable distance/angle controls have not been identified. Configure distance and turn in the ToLiss ISCS/IACP. A plain ground-crew request to start pushback triggers ToLiss using those existing settings; spoken distance or angle does not change those settings; the tug uses the values already entered in the aircraft. The confirmation reports command dispatch, not verified tug movement. ATC pushback permission remains separate from movement. OpenATC does not move the aircraft itself.

## Verification

Run `cargo test --locked -p openatc-core -p openatc-engine -p openatc-settings -p openatc-ui` and `python3 scripts/test-crew-engine.py target/release/open-atc-engine`. The latter runs the actual engine with simulated plugin observations and acknowledgements. Live ToLiss controls, panel routing and display indications still need a simulator test. Reload the aircraft/plugin and restart the engine after editing mappings or wording.
