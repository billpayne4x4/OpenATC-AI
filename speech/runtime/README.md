# Live operational speech

`responses.toml` contains the operational templates and crew request vocabulary, including ATIS-letter and measurement templates. These are the words used by the deterministic controller, readback feedback, taxi instructions, ATIS and simulator-weather reports, background exchanges, crew fallback replies and automatic copilot callouts. Regional IFR/VFR examples remain in the regional folders; global crew examples remain in `crew`.

Edit `say` or add alternatives to an existing `say` array. Keep its section ID and declared `slots`. Every alternative must use every declared placeholder exactly as written. Engine startup rejects missing IDs, changed slot contracts, empty phrases and missing placeholders. Each response ID selects its own alternatives across successive calls; unrelated responses do not advance its selection. Restart X-Plane and its engine to reload plugin-generated weather/callouts as well as engine responses.

```toml
[taxi_via_hold_short_of_runway_at]
slots = ["via", "hold_short"]
say = [
  "Taxi via {via}, hold short of runway {hold_short}.",
  "Taxi via {via}. Hold short of runway {hold_short}.",
]
```

Placeholders receive validated flight, station, runway, route and weather facts. Keep authorization, hold-short restrictions, negation and units consistent with the original meaning. Phrasing does not grant new controller capabilities. Taxi readback checks the structured taxiway identifiers and hold-short runway, independently of the words chosen from this file. Unacknowledged taxi instructions do not activate guidance.

The regional phrasebook supplies examples to AI suggestions and optional phrase variety. The phrase-variety setting permits conservative rewording of a TOML response; changed operational words or numbers are rejected. Free crew conversation uses global crew TOML examples. Pilot transmissions and simulator station names are supplied by the pilot and simulator.

Validate before starting:

```sh
open-atc-engine --check-speech /path/to/speech
```

The engine reports the offending template if an edit is invalid. The compiled-in default text is generated from this same TOML source for core tests and installations with no configured library; it is not an English fallback maintained separately in Rust.

Manual Auto Reply uses `pilot_ifr_readback`, `pilot_taxi_readback`, `pilot_start_ack`, `pilot_pushback_ack` and `pilot_start_pushback_ack`. Keep every declared placeholder in every alternative. Runway taxi speech uses `runway_taxi_backtrack` and `taxi_hold_for_backtrack`; its readback must retain the runway, backtrack and stop/hold-position restriction. These phrases never authorize movement by themselves.

Auto Reply resolves the current pending instruction in the engine, with separate IFR and taxi readback scopes. Taxi acknowledgments refer to taxi instructions. Ordinary holding-point routes do not say backtrack; runway backtrack wording is reserved for an explicitly authorized route along the runway. The Taxi page includes a saved simulator ground-arrow switch.

Taxi speech uses holding-point and hold-short instructions without referring to map paths or marked networks. Published taxiway names are retained where available; unnamed scenery routes use the assigned runway holding point without invented identifiers. All alternatives and pilot readbacks remain in TOML.

At the approved taxi endpoint, stopped within 25 m, the engine checks the actual tuned airport station. Ground hands off to the published Tower frequency; Tower issues departure clearance when simulator-reported traffic is clear, or holds for runway occupancy/final traffic. Missing traffic data keeps the aircraft holding. Pausing, radio power off, untuned channels and other airports cannot trigger departure. Holding for traffic is reconsidered when the runway clears. Routes needing backtrack or another runway crossing retain their hold restriction and require further explicit permission. Scenery-owned runway guard lights remain unchanged. Duplicate plugin holding-light markers have been removed; no per-holding-point native lighting control has been established. TCAS targets omit ownship and do not include traffic hidden from X-Plane’s traffic interface; this is not full traffic sequencing or wake separation.

Live sessions no longer generate fictional callsigns or canned background clearances; that TOML chatter is demo-only. Actual simulator traffic still gates runway entry and departure. Copilot readbacks are controlled by the readback setting, use complete structured Auto Reply facts, and carry the Copilot label without a controller-station name. Routine auto-response does not parrot taxi prompts. Taxi prompts do not request nonexistent taxiway names, and controller acknowledgments use short ATC wording. Duplicate plugin holding-light objects were removed. The installed simulator exposes global airport-light controls and a read-only wigwag brightness value, not an established per-holding-point native control; existing scenery lights are preserved.

Crew control requests use `crew_request_*` entries. Checklist challenges, expected answers and control confirmations also live here; aircraft TOML files refer to their keys and contain only control mappings and checklist order. Keep request alternatives specific enough to distinguish similar controls.
