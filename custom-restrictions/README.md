# Custom restrictions

X-Plane already contains the published prohibited, restricted and danger areas
in `Resources/default data/airspaces/airspace.txt`. This folder is only for an
area that is missing from that file, or for a local or temporary restriction
you have an authoritative boundary for.

Do not draw a circle around a landmark and call it restricted. Use the current
AIP, ERSA, DAH or the controlling authority's published polygon. A guessed
boundary is worse than no boundary because the engine will trust it.

## Adding an area

Copy `example.toml`, give it a useful name, and uncomment the area. The engine
scans every `.toml` file in this folder, so one file per area or region is fine.

```toml
[[airspace]]
name = "Example restricted area"
class = "R"
lower_ft = 0
upper_ft = 4500
points = [
  [-35.0000, 149.0000],
  [-35.0000, 149.0100],
  [-35.0100, 149.0100],
  [-35.0100, 149.0000],
]
```

Fields:

- `name` is what ATC says.
- `class` is `P` for prohibited, `R` for restricted or `D` for danger.
- `lower_ft` and `upper_ft` are feet MSL.
- `points` are decimal degrees, latitude then longitude, in order around the
  boundary. Use at least three points. The polygon may be clockwise or
  counter-clockwise.

A malformed file is logged and skipped. It does not stop the engine. If two
files define the same name, the first one loaded wins.

## Where to put it

The installed folder is:

```
X-Plane 12/Resources/plugins/OpenATC/custom-restrictions/
```

A personal copy can also go in the engine config directory:

```
~/.config/openatc-ai/custom-restrictions/
```

Both folders are read. Restart the engine after editing.

The examples and local copies are not a substitute for checking the current
AIP. Temporary restrictions change; remove the file when it no longer applies.
