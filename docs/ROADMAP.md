# Remaining work

The 0.2 interface, online OFP import, local navdata reader, stage controller, taxi graph, simulator airport-weather/ATIS reporting, audio settings and HTTP/TLS separation are implemented in source. Check `VALIDATION.md` before assuming runtime coverage.

Current Rust work includes native audio selection, plugin-owned engine lifecycle, improved maps/Arrival profiles, local-frequency tuning, ToLiss A20N datarefs and the recursive editable speech library. See the READMEs and current validation section.

Still needed for a complete ATC system:

- Simulator runtime validation across X-Plane UI scales, multi-monitor and pop-out configurations; native SDK 4.4 graphics integration, non-ASCII keyboard entry.
- Traffic ownership, full separation and sequencing beyond simulator-reported runway occupancy/final-approach protection; full published-procedure execution and navigation/terrain validation.
- Enroute airspace/sector data and automatic handoffs. Existing copilot tuning uses explicit structured assignments from the loaded airport.
- Aircraft-size/obstacle validation beyond the current painted-centerline fallback, taxi-route amendments and richer holding-point metadata. Separate runway crossing clearances, crossing readbacks and onward taxi are implemented and checked against installed KLAX scenery; broader simulator acceptance remains. Conservative centerline fallback and separate Tower-authorized runway backtracking are implemented.
- Navigraph developer registration, entitled-user authorization and licensed chart rendering. No working login or chart support is claimed yet.
- Precipitation radar tiles, map terrain/coastline base layers, forecast fields and simulator-weather grids. Current radio weather comes from airport-location simulator surface samples, not radar.
- Aircraft-specific performance, mass/fuel loading, route generation and restrictions. Current dispatch data is imported from SimBrief or entered by the user.
- Aircraft-specific radio overrides where a third-party aircraft does not honor X-Plane's standard COM1 dataref; hardware push-to-talk binding and broader speech-cancellation controls.

Editable speech follow-through:

- Add explicit IFR/VFR state to the flight plan and operational controller flow; `/suggest` already accepts it, but its default remains IFR.
- Extend checked live-slot speech dispatch beyond the implemented departure clearance/start/pushback workflow; expand spoken readback recognition beyond the current NATO/English matching, VFR clearance flow and service-specific airborne requests.
- Add country/FIR/airport procedure packs with authority, section and effective-date review, including metric flight levels, national RNAV terminology and local language support. The existing broad regional folders are a baseline, not completed worldwide procedure coverage.
- Model eligibility for conditional runway operations, visual/SVFR services, CPDLC, intersection performance and LAHSO before wiring those example situations into automatic clearance generation.
- Add speech hot reload and a user-facing editor/validation report. Current changes load on engine restart; the offline checker and personal-library override are available now.

The current sprint implements nearby station discovery, unconditional reception gating, service-aware departure permissions, simulated ATIS and optional conservative AI wording. Full sector routing, terrain radio shielding, remote radio sites, worldwide procedural certification and end-to-end simulator acceptance remain separate work.

## Crew follow-up

- Validate the new ToLiss panel routing and each custom control in the running neo. Button-only mappings expose a press, not a verified engaged state.
- Identify a supported ToLiss pushback distance/angle interface before connecting automatic start.
- Add reviewed, complete aircraft/operator checklists; current examples are abbreviated.
- Add held-control semantics where the SDK requires more than a tap.

New flight clears active flight state without unloading the plugin. Live header reset and disable/re-enable checks remain part of simulator acceptance; automated checks cover reset, pending actions and delayed responses. Native per-holding-point light control is not implemented.
