# Architecture

The engine owns the flight plan, clearance sequence, telemetry-derived phase, approved taxi path, transcript and persisted settings. One mutex protects snapshots and mutations. Network/provider calls and airport-file reads execute outside that mutex. UI commands and telemetry polling use separate connections so a slow provider request does not block simulator telemetry.

Both UI frontends use `openatc_transport_plain`: nlohmann JSON, cpp-httplib headers and threading, without OpenSSL compile definitions or TLS libraries. The engine alone links the TLS transport. The plugin's link audit and compile-time checks enforce that boundary. Speech uploads/downloads go through `/speech/transcribe` and `/speech/speak` on the engine.

`core.cpp`, `flight.cpp` and `airport_data.cpp` contain dependency-free logic. `simbrief.cpp` maps provider JSON into the expanded model. `ui.cpp` handles the shell, request dialogs and speech queue; `ui_pages.cpp`, `ui_maps.cpp` and `ui_settings.cpp` implement the pages. Font bytes are generated during CMake builds and embedded in the UI library. `assets/logo.svg` is the standalone vector equivalent of the in-app line logo.

X-Plane API calls remain on the simulator thread. The flight loop supplies position, altitude, groundspeed, vertical speed, AGL and COM1. It may write only the assigned COM1 frequency when the copilot option is enabled and the dataref is writable. It also ticks speech while the window is hidden. The graphics path remains X-Plane's OpenGL compatibility path with its native floating-window decoration.

A calculated taxi route is not automatically approved. `applyRequest` validates the flight stage and loaded airport, records approval and stores the exact path used by the renderer. Graph traversal excludes runway edges, active zones and runway-area geometry and honors direction and width. Ramp connections are not validated against apron pavement. The map identifies these connectors separately. Taxi clearance is invalidated on departure, completion, airport replacement and session restore.

The weather map is an equirectangular view with dateline-aware longitudes. METAR positions and imported route coordinates supply its geographic data. Cloud-cover circles denote station observations, not the extent of cloud or rain. The arrival graph compares a constant-angle reference with a constant-current-V/S projection; it is not an aircraft VNAV solution.
