# Traffic preferences

## Wake and runway policy

`wake.toml` is the editable wake-turbulence and runway-relationship policy. It contains FAA CWT, ICAO Doc 4444, EUROCONTROL RECAT-EU and Australian MATS tables, with leader/follower categories, departure intervals, arrival spacing, line-up-and-wait limits and parallel-runway thresholds. The **Wake separation** setting selects `auto`, `icao`, `recat_eu`, `faa_cwt` or `mats`; `auto` uses FAA CWT at US airports, MATS at Australian airports and ICAO elsewhere. Aircraft categories come from the simulator's reported wake category first, then the aircraft profile's `cwt` or `wake` field. Edit the tables and restart the engine to apply them.


Airborne planning is still being integrated. The worker geometry builder now checks bank and turn-rate limits, crossing floors/ceilings and climb/descent performance across the whole proposed path. It begins descent early enough for later ceilings, and rejects paths whose sampled corridor is not protected. These checks do not yet supply complete generated departures, arrivals or controller clearances.

These versioned files describe plausible generated traffic. They are not a real-world timetable, a current fleet census or a source of dispatch performance. There are no per-flight schedule files to maintain. The renderer's model inventory comes from installed CSL metadata.

- `airlines.toml`: operating callsign, home area, served ICAO prefixes, category, aircraft weights and explicitly permitted partner liveries. Both endpoints must be served and at least one must be in the home area. A Singapore Airlines hub preference at Sydney does not create Singapore Airlines flights from Sydney to Melbourne.
- `hubs.toml`: relative operator preferences at an airport. Values do not have to sum to 100 and are not market shares. Preferences at both ends influence the selection.
- `aircraft.toml`: nominal simulation envelopes, dimensions, equipment and propeller animation requirements. Wake category, approach category and parking size are separate. Parking size comes from wingspan; a medium-wake A320 needs a size C stand. Envelopes are conservative approximations, not certified wet/hot/high or aircraft-weight performance calculations.

Profiles currently cover 77 operating/private fleet entries, 176 airport preferences and 52 aircraft types, including piston singles/twins, turboprops, regional jets, business jets, airliners and freighters. This is a starter set, not worldwide exhaustive coverage. Aircraft that do not have a suitable installed model are excluded. Operating airline, marketed paint and model identity remain separate, including declared SkyWest partner liveries. Other airline paint is never used as an undeclared fallback. The ambient airport layer uses the same hub, fleet and paint rules: it does not fall back to the first installed model, and general aviation uses the private fleet's registration patterns.

Aircraft entries also carry `cwt`, the FAA consolidated wake category A–I. It is separate from ICAO weight class, approach category and gate size: for example, an A320 is CWT F, while a Piper PA-28 is CWT I. The bundled values were checked against [FAA Order JO 7360.1K](https://www.faa.gov/documentLibrary/media/Order/FAA_Order_JO_7360.1K_Aircraft_Type_Designators.pdf). Older custom files may omit this field; missing categories remain unknown. Operational observations currently use the bundled type lookup, including the explicitly configured P28A alias, and simulator-reported wingspan where available. Editing a generation profile does not change the published wake rule for an observed aircraft.

The engine loads bundled defaults, then files alongside the installation in `traffic`, then `OPENATC_TRAFFIC_DIR` if configured, then personal files in the engine configuration directory's `traffic` folder. Each overriding file replaces the corresponding complete file; copy it before editing. Personal files are outside the plugin upgrade directory. Use **Traffic → Airline, hub and aircraft preferences → Load / validate profile files** to reload. Validation checks versions, unknown fields, finite ranges, fleet types, and hub references together. A failed reload reports the error and preserves the previous valid set. The API is `POST /traffic/profiles` with `{"refresh":true}`.

The selector requires known runway/parking capacity at both ends and reserves 20% of nominal range. It selects only model-compatible aircraft and uses stable seeded choices. These checks do not by themselves validate a SID, STAR, approach, terrain clearance or runway separation. Those remain route/movement checks. Profile selection is implemented; the full generated-traffic lifecycle and native renderer remain under development. Native X-Plane AI currently uses X-Plane models and routes.

## Source notes

`aircraft.toml` can declare `model_aliases` for equivalent advertised CSL types. The PA-28 fixed-gear envelope explicitly matches Bluebell's `P28A`; retractable/turbo variants are not silently treated as fixed-gear aircraft. Aliases must be unique and cannot override another aircraft envelope. Published traffic reports retain the model's actual type.

Private fleets use editable `registrations` patterns in `airlines.toml`. Keys are origin ICAO prefixes, with the longest matching prefix selected; `?` generates a letter and `#` a digit. The empty-prefix entry is the simulated fallback. These are fictional identifiers, not registrations sourced from real aircraft. Generated identities remain stable for a scene, and duplicate callsigns are excluded before reserving stands.

Optional `registration_alphabets` restricts generated letters per region. The bundled US pattern starts with a nonzero digit and excludes I/O, following the [FAA N-number format](https://www.faa.gov/licenses_certificates/aircraft_certification/aircraft_registry/forming_nnumber/index.cfm?print=go); it does not claim the invented registration is available or belongs to the represented aircraft.

Experimental surface traffic reserves a verified taxi corridor up to its next runway crossing before issuing taxi instructions. Busy corridors produce hold-position/readback exchanges; each crossing remains a separate grant. Actors with no usable route or protected stand are excluded rather than parked as unplanned obstructions. The worker searches a bounded set of alternate stands, and piston aircraft require propeller-compatible parking metadata. Full generated airborne flights and native acceptance remain outstanding.

`handling.toml` also supplies each equipment family's rotation pitch and climb/approach pitch bias. Pitch follows the actual flight-path angle plus the configured nominal angle of attack; rotation is rate-limited around the configured rotation speed. Ground rendering adds the pavement slope to that attitude rather than flattening the rotation. These are nominal visual envelopes. `flare_height_feet` and `touchdown_sink_fpm` control the generated threshold-to-touchdown roundout; old handling overrides inherit these two fields from bundled category defaults. Production preparation joins that nominal roundout to rollout, runway exit and free-stand taxi geometry. This does not complete the airborne planner, landing permission or native model acceptance. Flare speed loss and per-aircraft weight/performance remain unfinished.

Terminal planning reads the user's installed navdata on workers and retains ordered procedure legs, repeated fix identifiers and timed holding data. Fixes/navaids are resolved by region and terminal owner, with source-priority and cycle checks; the plugin does not distribute the user's licensed data. A nominal published hold requires explicit course/turn/level/speed information and verified path protection. Complete holding entry, wind correction, release and generated airborne operations remain under development.

Generated movement also reads `handling.toml`, `operations.toml` and `animation-channels.toml` with the same installed/environment/personal priority. These load during worker preparation; they are not read on each simulator frame. Handling separates piston, turboprop, regional jet, jet and heavy-jet rates, flaps, steering geometry and propeller speeds. Operations bounds preparation, surface subdivision, conflict cells and radio pacing. `terminal_intercept_limit_nm` bounds the published course-intercept search during aircraft-specific terminal decoding; it is a planning search limit, not an ATC clearance. `runway_slope_limit_percent` rejects selected runway-end elevation gradients outside the supported nominal envelope. The selected physical runway surface is used for departure and landing instead of the airport maximum elevation. Taxi graph nodes lack surveyed heights; lineup and exit connections use continuous nominal datums until native terrain placement. Older override files inherit new operation fields from the bundled defaults. `visible_aircraft_limit` (up to the SDK limit of 63), `display_enter_nm`, `display_leave_nm` and `display_replacement_margin_nm` bound nearby visual/TCAS publication with range and slot hysteresis. Distant flight state and engine separation observations are retained. This selection does not generate new flights or finish continuous traffic replenishment. Animation channels describe supported OBJ8 datarefs. Invalid overrides prevent a replacement scene and leave the current valid scene available with diagnostics.

`vertical.toml` supplies the player's nominal descent angle, lead distance and level increments, loaded when submitting a plan. Upcoming published ceilings drive crossing clearances. When those are absent, a known ILS entry level can initiate an earlier staged descent while retaining remaining minimum restrictions and known terrain floors. Unknown terrain or unsupported remaining leg geometry prevents that fallback; missing pressure information prevents an altitude clearance below transition. These conservative planning margins do not reproduce an aircraft's computed FMS top of descent.

The additional French/European preferences use [Air France's aircraft families](https://wwws.airfrance.fr/en/information/prepare/nos-avions), [HOP's Embraer operations](https://corporate.airfrance.com/en/news/paris-21-march-2024-air-france-upgrade-its-embraer-190-brand-new-travel-cabins), [easyJet's fleet](https://www.easyjet.com/en/traveller/best-of-europe/what-planes-does-easyjet-use) and [Air Corsica's operator information](https://www.aircorsica.com/la-compagnie/). Weights remain simulation choices. The historic Bluebell AT72 has its own envelope and is not relabelled as an AT76.

Weights and movement envelopes are OpenATC simulation choices. Fleet families and geographic preferences were checked against public operator pages on 13 October 2026. No paid schedules, proprietary navigation procedures or Bluebell objects/textures are redistributed in these files. User-installed navigation data and downloaded CSL packages retain their own licences.

Representative primary references: [Qantas fleet](https://www.qantas.com/en-us/onboard/fleet), [Singapore Airlines fleet](https://www.singaporeair.com/en_UK/us/flying-withus/our-story/our-fleet/), [SkyWest facts and partner hubs](https://www.skywest.com/about-skywest-airlines/facts/), [Virgin Australia fleet](https://www.virginaustralia.com/uk/en/travel-info/flying-with-us/our-fleet/), [Jetstar fleet](https://www.jetstar.com/sg/en/about-us/our-fleet), [Air New Zealand fleet](https://www.airnewzealand.com/fleet), [United facts](https://united.mediaroom.com/corporate-fact-sheet), [American fleet](https://www.aa.com/web/i18n/travel-info/experience/planes/planes.html), [British Airways facts](https://mediacentre.britishairways.com/factsheets), [Lufthansa fleet](https://investor-relations.lufthansagroup.com/en/corporate-facts/fleet.html), [FedEx structure](https://www.fedex.com/en-us/about/company-structure.html), [UPS air operations](https://about.ups.com/content/dam/upsstories/assets/fact-sheets/airlines-fact-sheets/AirOperationsFacts-03312025.pdf), [Cathay fleet](https://www.cathaypacific.com/cx/en_CA/flying-with-us/aircraft-and-fleet.html), [Qatar fleet](https://www.qatarairways.com/en-qa/fleet.html), [ANA fleet](https://cad.www.ana.co.jp/group/en/company/ana/scale/), [Ethiopian fleet](https://corporate.ethiopianairlines.com/AboutEthiopian/OurFleets).

Historical model packs can lack newer aircraft and paint schemes. Selection may therefore contain fewer types than the profile lists. Do not add an unrelated model under a different type merely to fill a gap.

## Published route graph

`routes.toml` records the city pairs an operator actually flies. The pair is unordered, because
a scheduled service normally runs both ways, and the weight is a relative frequency band inside
the operator's own network, not a schedule and not a seat count:

```toml
[[route]]
airline = "AFR"
origin = "LFPG"
destination = "LFMN"
weight = 100
```

An operator with published routes is only selected for a pair it publishes, so the traffic at an
airport is the traffic that airport really has. The loader refuses the whole file when a route
names an unknown operator, an airport that no hub describes, equipment that no aircraft profile
describes, or a pair that is listed twice.

The airports are keyed by their published world identifier, and each hub carries the airport's
position. The engine matches that position against the installed airport metadata, because an
installed navdata cycle may not use the published identifier for every airport. The bundled
hubs previously carried several wrong identifiers, for example Adelaide, Bastia, Seville and
Bordeaux, which is why the hub preferences did not match the simulator.

The world reference for the airports and their positions is the public-domain OurAirports table.
The operator and aircraft data remains editable in `airlines.toml` and `aircraft.toml`.
