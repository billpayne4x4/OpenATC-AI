# China ATC speech

Baseline only. Published metric clearances require explicit unit-bearing level data.

Airport prefixes: Z. Phraseology baseline: `icao`. This pack contains 0 examples; missing regional situations use matching common entries.

IFR, VFR and shared folders contain ATC only. Global crew files are in `../crew`. Prefix routing is for phrase selection, not a complete FIR boundary model.

See [editing and sources](../README.md) and [the situation index](../INDEX.md).

ATC entries declare `service` (Delivery is `Clearance`, plus `Ground`, `Tower`, `Approach` or `Departure`). Keep a situation within its controller role. The initial departure workflow uses checked common clearance/start/pushback entries; other examples remain a style library until their controller operations are implemented. See [runtime use and editing](../README.md).
