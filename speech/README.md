# Editing what OpenATC says

This is the open, editable English speech library. It contains **579 situations and 2,202 alternative phrases**, expanded from 195 situations and 399 phrases. Change a phrase, add a situation or contribute a reviewed regional example without recompiling the library. Restart the engine to reload edits. Live operational wording, crew request anchors and control names are in [runtime/responses.toml](runtime/responses.toml). See [the editing guide](runtime/README.md).

Browse [the situation index](INDEX.md) for every ID, file, phase and tag.

## Folder layout

```text
speech/
  runtime/               live operational responses and edit instructions
  crew/
    copilot.toml
    flight_attendant.toml
    ground_services.toml
    controls_and_checklists.toml
  common/
    profile.toml
    ifr/                 instrument-flight examples
    vfr/                 visual-flight examples
    shared/              ATC examples applicable to either
  us/
    profile.toml         ICAO prefixes and FAA variant
    ifr/
    vfr/
    shared/
  canada/, united_kingdom/, europe/, australia/, newzealand/
  asia/, japan_korea/, china/, russia_central_asia/, middle_east/
  africa/, south_america/, central_america/, caribbean/, pacific/
```

Every regional folder has `ifr`, `vfr` and `shared` subfolders. Empty regional folders deliberately use the common baseline; their README states the review limit. Cabin, ground-service and copilot speech have **one global copy** in `crew`: they are not duplicated by region or flight rules. Their flight-phase tags still determine when examples apply. Aircraft mappings, checklist order and expected values belong in aircraft profiles. Spoken challenges, requests and responses belong in speech TOMLs.

The common ATC files describe clearance, departure, enroute, arrival, tower, ground, communications, weather, readbacks, advisories, emergencies and offers. `atc_ground` is the **ground controller**, distinct from `crew/ground_services.toml`, the ramp team.

## How selection works

The Rust loader searches recursively. The folder determines regional and flight-rule scope; do not repeat `region` or `flight_rules` inside entries. A regional `profile.toml` looks like:

```toml
name = "us"
prefixes = ["K", "PA", "PF", "PH", "PO"]
variant = "faa"
notes = "FAA examples; use current airport and state publications."
```

The longest matching airport prefix wins: `EG` selects the UK before general Europe `E`; `NZ` selects New Zealand before Pacific `N`; `PH` selects the US before Pacific `P`. Caribbean `T` is not assumed to be FAA. Duplicate prefixes are errors. Regional matching is an editable phrasebook routing scheme, not a complete FIR, sovereignty or sector boundary database.

Selection matches role, flight phase, phraseology variant, and any requested situation tag. It admits only the selected region and `common`, and only the selected flight rules and `shared`. Regional entries are ranked first. New regional entries need unique IDs; copying a common ID does not override it. `variant = "common"` means the wording is usable with either ICAO or FAA selection, not that every state has the same operational rules.

The `/suggest` API accepts `airport` and `flightRules` (`"ifr"` or `"vfr"`). If omitted, it uses the plan departure, or destination during arrival/approach/landing/taxi-in, and defaults to IFR. The existing flight plan has no IFR/VFR field yet. Crew prompts use only global shared examples. Unknown flight rules in the core selector admit shared examples only. Normal controller requests still run the staged Rust controller: **adding a speech situation does not implement a clearance, runway crossing, separation service, CPDLC, LAHSO or aircraft action**. `/suggest` returns proposed text/effect and does not apply it to the flight plan.

## Editing an entry

```toml
[my_ground.hold_short]
role = "atc"
service = ["Ground"]
phase = ["taxi"]
variant = "common"
situation = ["hold_short_runway"]
slots = ["callsign", "rwy"]
say = [
  "{callsign}, hold short of runway {rwy}.",
  "{callsign}, runway {rwy}, hold short.",
]
```

Put this in `common/shared/my_ground.toml`, or a selected region's `shared` folder. `[group.name]` becomes the unique ID `group.name`; the filename is just a subject label. Use descriptive lowercase names with underscores. The loader rejects unknown fields, duplicate IDs across files, invalid roles/phases/variants, unknown or undeclared placeholders, malformed braces, duplicate phrases, empty phrases and incomplete offers. Its error identifies the file and entry.

| Field | Meaning |
|---|---|
| `role` | `atc`, `copilot`, `attendant` or `ground` (ramp crew) |
| `phase` | One or more of `parked`, `clearance`, `pushback`, `taxi`, `departure`, `cruise`, `arrival`, `approach`, `landed`, `taxi_in`, `finished` |
| `variant` | `icao`, `faa`, or `common` |
| `situation` | Retrieval tags describing one event and its verified conditions |
| `slots` | Every placeholder used in say/accept/decline, exactly once |
| `say` | One or more equivalent phrasings of that same event |
| `service` | Eligible ATC roles: `Clearance` (Delivery), `Ground`, `Tower`, `Approach`, `Departure`; omit for crew. Unknown names are errors. Legacy entries can omit it. |
| `accept`, `decline`, `effect` | All three required for an offer; omit for ordinary speech |

One situation must mean one thing. Do not put “continue approach” and “cleared to land” in the same `say` list. Keep ready/complete, boarding/arrival, clearance/offer and pilot/controller speech separate. Mandatory standard callouts may have a single canonical phrase; more variation is not always better. Comments beginning with `#` are suitable for editing notes and local source references.

Offers ask whether the pilot can accept a proposal. Their alternatives must never silently issue it. Supported effects are `amend_route`, `amend_altitude`, `amend_arrival`, `amend_destination`, `amend_speed`, and `none`. `accept` is a pilot response example, not authority to execute the change. The controller must validate and issue the resulting clearance.

## Placeholders and units

| Placeholders | Values |
|---|---|
| `{callsign}`, `{dest}`, `{rwy}`, `{sq}`, `{qnh}` | Callsign, destination, runway, code, pressure value |
| `{sid}`, `{star}`, `{approach}`, `{wpt}`, `{via}` | Published procedure, arrival, approach, fix, route/taxiway |
| `{freq}`, `{station}`, `{atis}`, `{stand}`, `{holding_point}` | Frequency, station name, ATIS, parking, clearance limit |
| `{alt}` | Legacy feet value; renderer adds `feet` |
| `{mins}`, `{nm}` | Bare numeric values; renderer adds `minutes` / `miles` |
| `{heading}`, `{turn}`, `{direction}`, `{sequence}`, `{readability}` | Heading, left/right, direction, sequence, radio readability |
| `{level}`, `{block}`, `{speed}`, `{mach}`, `{rate}` | Complete spoken values with units where applicable |
| `{wind}`, `{visibility}`, `{weather}`, `{surface}`, `{traffic}`, `{location}` | Verified reports and descriptions |
| `{time}`, `{fluid}`, `{fuel_quantity}`, `{landing_distance}`, `{status}` | Time with zone, treatment details, quantities with units, distance with units, confirmed status |

Do not write `{mins} minutes` or `{nm} miles`: units would be repeated. New string values are supplied through the Rust `Slots.extra` map. Illustrative prompt values are explicitly fictional. They are not live simulator facts. `render_checked` rejects missing context; the canned renderer returns no incomplete phrase. Legacy `{alt|3000}` syntax is supported but operational altitude defaults should be avoided. Canada selection also excludes baseline QNH wording in favour of its altimeter examples. Foot-based ATC examples are excluded from China/Russia pack selection; use explicit unit-bearing `{level}` with verified published values when contributing metric material. This exclusion is not a complete national unit/procedure implementation.

A speech file cannot create safe headings, runway availability, traffic separation, terrain clearance, weather, aircraft systems, completed checks or emergency-services response. These must be known before selecting the corresponding situation. Narrow tags such as `conditional_lineup_traffic_identified_and_in_sight` describe eligibility; the tag alone does not verify it. Aircraft emergency speech refers to the aircraft procedure instead of guessing flap settings or memory actions.

## Validate and install

From the repository:

```sh
cd rust
cargo test -p openatc-core --test shipped_config --test speech_library
cargo run -p openatc-engine -- --check-speech ../speech
# HTTP selection check against a local fake model, with isolated settings:
cargo xtask test-speech target/release/open-atc-engine speech
```

On an installed Linux plugin:

```sh
"/path/to/X-Plane 12/Resources/plugins/OpenATC/bin/open-atc-engine" \
  --check-speech "/path/to/X-Plane 12/Resources/plugins/OpenATC/speech"
```

This command does not start a server, change settings, call AI, speak or connect to X-Plane. Successful validation checks syntax, metadata and rendering against illustrative values; a human procedure review is still needed.

To keep personal edits separate from upgrades, copy the **whole** speech folder to `~/.config/openatc/speech` (or the engine's configured directory). Alternatively set `OPENATC_SPEECH_DIR`. Resolution order is environment override, personal config directory, bundled plugin library. It chooses one complete library, not an overlay. `/health` reports the selected source. Restart the engine after edits; hot reload is not implemented.

The installer backs up the whole installed plugin outside X-Plane before updating. Old flat `.toml` speech files are preserved as `.toml.legacy` to prevent duplicate IDs. Historical dotted IDs remain stable. Names were clarified: `chatter.toml` → `atc_communications.toml`, `emergency.toml` → `atc_emergencies.toml`, `cabin.toml` → `flight_attendant.toml`, `ground_service.toml` → `ground_services.toml`. Some historical keys are retained for compatibility: `arrival.hold_short` now explicitly means an updated expected approach time, `arrival.speed_180_marker` uses contextual speed, `tower.takeoff_immediate` is a readiness question, and `emergency.bomb_equalize` acknowledges a reported threat without prescribing cabin-pressure actions.

## Procedure sources and worldwide coverage

Reference review: **7 October 2026**. This library is simulator wording, not an endorsed operational phrasebook. English examples use an ICAO-oriented baseline with selected national additions. It is not a complete review of every country's AIP, local language, military procedure or airport. Current AIP GEN 1.7 differences, GEN 3.4 communications, ENR and airport AD procedures, NOTAMs and aircraft manuals take precedence.

References used for the reviewed wording and regional distinctions:

- [FAA taxi and ground movements, JO 7110.65](https://www.faa.gov/air_traffic/publications/atpubs/atc_html/chap3_section_7.html): explicit runway crossing, hold-short readback and FAA restrictions on conditional runway movement.
- [FAA departure procedures](https://www.faa.gov/air_traffic/publications/atpubs/atc_html/chap3_section_9.html) and [landing procedures](https://www.faa.gov/air_traffic/publications/atpubs/atc_html/chap3_section_10.html).
- [EUROCONTROL phraseology database](https://contentzone.eurocontrol.int/phraseology/Default.aspx): international categories and standard wording; conditional examples require local eligibility and identified traffic in sight.
- [UK CAA CAP 413](https://www.caa.co.uk/data-and-publications/publications/documents/content/cap-413/): UK radiotelephony and service terminology.
- [NAV CANADA RNAV phraseology](https://www.navcanada.ca/en/rnav-phraseology.pdf) and [operational guides](https://www.navcanada.ca/en/aeronautical-information/operational-guides.aspx): national terminology differs from US climb/descend-via practice.
- [Airservices Australia communications and readbacks](https://www.airservicesaustralia.com/industry-info/pilot-tools/pilot-and-airside-safety/working-with-atc/): taxi route/holding point, frequency and other clearance readbacks; consult current AIP GEN 3.4.

Worldwide source discovery also checked [Singapore CAAS AIS](https://www.caas.gov.sg/industry/airspace-management-and-aerial-activities/aeronautical-information-services/), [South Africa SACAA AIS](https://www.caa.co.za/industry-information/aeronautical-information/), [Brazil DECEA phraseology publication](https://publicacoes.decea.mil.br/version/471), and [New Zealand AIP](https://www.aip.net.nz/). These establish where local review belongs; they do **not** constitute a completed review of local phraseology. Brazil's manual fetch was blocked and the New Zealand AIP required a terms page, so no unverified national wording was added from them. Baseline-only folders say so rather than inventing differences.

For a regional contribution, record the authority, section, effective date and a short rationale in comments. Author original examples; do not copy a whole manual or redistribute restricted charts. Test the region, flight rules, phase, units and exact operational meaning. Airport-specific routing belongs in current navigation/scenery data and controller logic, not a fixed phrase with an invented heading or safe altitude.

## Runtime departure wording and radio reception

The engine uses `delivery.ifr_route`, `ground.startup_only`, `ground.pushback_basic` and `ground.start_and_pushback` after its deterministic controller decision. Live callsign, destination, route/SID, initial altitude, squawk and runway fill checked slots. Taxi route generation still supplies its own checked instructions. Other situations are examples for `/suggest`, not implemented procedure triggers.

Settings / General has **Allow AI wording variety** (off by default). Eligible operational responses pass selected service/region examples to the model. Only filler changes that retain every operational token in its original order are accepted; altered values, routes, approvals or restrictions fall back to the checked original. The wording gate is intentionally conservative.

Radio ATC requires a published scenery station in simulated reception range and powered COM1. Delivery, Ground and Tower permissions are distinct; combined services are explicit in the installed `radio-stations.toml`. No tuned station means silence, including emergency/radio-check requests. ATIS is a repeating simulator-weather broadcast, separate from conversational phrases. Its letter advances when the rounded observation changes. Missing airport weather remains unavailable. Cockpit and crew communication retain their separate power/role rules.

The Auto Reply pilot readbacks and runway backtrack/hold instructions also live in runtime TOML. Ordinary taxi, runway taxi/backtrack and takeoff are separate permissions; editing a phrase does not bypass them. See [runtime/README.md](runtime/README.md) for the pilot reply template IDs.

## How the LLM uses examples

All alternatives from matching situations are supplied as wording examples, without a fixed first-four/first-six cutoff. Role, phase, region and flight-rule filters still apply; unrelated regional procedures are not mixed together. The prompts ask for fresh wording from live facts rather than verbatim copying. Standard ATC terms and required readbacks may repeat. Operational wording variety remains optional and conservative: changed instructions or values fall back to the validated TOML response. AI-off responses also use TOML directly. Large custom libraries require a model context window large enough for the matching examples.

Crew request anchors and confirmations are documented in [Aircraft controls](../docs/AIRCRAFT_CONTROLS.md). The `pilot` example role describes captain requests and responses; it is not a selectable AI crew speaker.

ATC response templates rotate independently, with their next choice and previous station/task transmission saved in the engine configuration folder as `phrase-history.json`. Flight resets do not clear this history. With AI wording variety off, replies use the TOML alternatives exactly. With it on, the model receives the controller role, live phase, current task, matching examples and previous wording; operational changes or repeated candidates fall back to the selected TOML reply.

Crossing examples cover controller-initiated clearance, pilot requests, traffic holds, readbacks, runway-vacated reports and onward taxi. Runtime crossing permission and continuation use checked scenery geometry and simulator traffic. Yellow runway guard lights remain flashing; they are not clearance-controlled stop bars.
