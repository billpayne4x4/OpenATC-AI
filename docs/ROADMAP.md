# Remaining work

The 0.2 interface, online OFP import, local navdata reader, stage controller, taxi graph, weather observation map, audio settings and HTTP/TLS separation are implemented in source. Check `VALIDATION.md` before assuming runtime coverage.

Still needed for a complete ATC system:

- Simulator runtime validation across X-Plane UI scales, multi-monitor and pop-out configurations; native SDK 4.4 graphics integration, clipboard and non-ASCII keyboard entry.
- Traffic ownership, separation, runway occupancy and sequencing; full published-procedure execution and navigation/terrain validation.
- Enroute airspace/sector data and automatic handoffs. Existing copilot tuning uses explicit structured assignments from the loaded airport.
- Validated ramp/apron connectors, clearance for specific runway crossings, taxi-route amendments and hold-short geometry from richer scenery data.
- Navigraph developer registration, entitled-user authorization and licensed chart rendering. No working login or chart support is claimed yet.
- Precipitation radar tiles, map terrain/coastline base layers, forecast fields and simulator-weather grids. Current weather is a map of timestamped METAR observations.
- Aircraft-specific performance, mass/fuel loading, route generation and restrictions. Current dispatch data is imported from SimBrief or entered by the user.
- Aircraft-specific radio overrides where a third-party aircraft does not honor X-Plane's standard COM1 dataref; hardware push-to-talk binding and speech cancellation.
