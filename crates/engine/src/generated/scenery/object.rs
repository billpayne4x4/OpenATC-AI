//! Static OBJ8 geometry used by the worker's obstacle check.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
#[allow(unused_imports)]
use std::fmt::Write as _;
use std::{io::Read, path::Path};

/// A box in the local metre frame of one scenery object.
///
/// The coordinates are tile-local metres, so single precision keeps a fraction of a
/// millimetre and halves the memory a dense tile needs.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct Bounds {
    pub low: [f32; 3],
    pub high: [f32; 3],
}

/// Build a box from the parser's double precision values.
fn bound(low: [f64; 3], high: [f64; 3]) -> Bounds {
    Bounds {
        low: [
            super::narrow(true, low[0]),
            super::narrow(true, low[1]),
            super::narrow(true, low[2]),
        ],
        high: [
            super::narrow(false, high[0]),
            super::narrow(false, high[1]),
            super::narrow(false, high[2]),
        ],
    }
}

pub(super) fn load(path: &Path) -> Result<Vec<Bounds>, String> {
    let mut bytes = Vec::new();
    super::cache::open_file(path)
        .map_err(|e| e.to_string())?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("Scenery object exceeds its input budget".into());
    }
    // Scenery packages are authored on several platforms and some carry non-UTF-8 bytes in
    // comments or names. Replacement characters cannot move geometry, so the byte stream is
    // decoded lossily and any real geometry damage still fails the directive parse.
    parse(&String::from_utf8_lossy(&bytes))
}

#[allow(clippy::items_after_statements)]
fn parse(text: &str) -> Result<Vec<Bounds>, String> {
    let mut lines = text
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty());
    if !matches!(lines.next(), Some("A" | "I"))
        || lines.next() != Some("800")
        || lines.next() != Some("OBJ")
    {
        return Err("Scenery obstacle requires OBJ8 geometry".into());
    }
    let mut vertices = Vec::<[f64; 3]>::new();
    let mut line_vertices = Vec::<[f64; 3]>::new();
    let mut indices = Vec::<usize>::new();
    let mut bounds = Vec::new();
    let mut counts = None;
    // Terrain alignment rotates an object onto the local ground slope. The published geometry
    // stays the same, so the horizontal footprint is widened by the object's own height, which
    // contains a tilt through any angle up to vertical.
    let mut tilted = false;
    // Animation can move a part of the object. Rotations are contained by the object's static
    // extent; translation keys are added on top of that.
    let mut animated = false;
    let mut translation = 0.0_f64;
    for line in lines {
        let mut fields = line.split_whitespace();
        let command = fields.next().unwrap();
        if command.starts_with("ANIM_") {
            animated = true;
            let values = fields
                .clone()
                .filter_map(|value| value.parse::<f64>().ok())
                .collect::<Vec<_>>();
            // ANIM_trans publishes start and end offsets; ANIM_trans_key publishes one position
            // per key. Both are in object metres.
            let positions = if command == "ANIM_trans" {
                values
                    .get(..6)
                    .map(|values| vec![&values[..3], &values[3..6]])
            } else if command == "ANIM_trans_key" {
                values.get(1..4).map(|values| vec![&values[..3]])
            } else {
                None
            };
            for position in positions.into_iter().flatten() {
                if position.iter().all(|value| value.is_finite()) {
                    translation = translation.max(
                        position
                            .iter()
                            .map(|value| value * value)
                            .sum::<f64>()
                            .sqrt(),
                    );
                }
            }
            continue;
        }
        if matches!(command, "GLOBAL_tilt" | "SLOPE_LIMIT" | "TILTED") {
            tilted = true;
            continue;
        }
        // Shading, material and export-name directives describe how a surface is drawn, not
        // where it is. They cannot move geometry, so an obstacle check can safely ignore them
        // rather than refusing a building that merely mentions a specular finish.
        if matches!(
            command,
            "NORMAL_METALNESS"
                | "SPECULAR"
                | "WEATHER_TRANSPARENT"
                | "SUPER_ROUGHNESS"
                | "SNOW_ALBEDO_LUMA"
                | "DITHER_ALPHA"
                | "NORMAL_TRANSLUCENCY"
                | "smoke_black"
                | "smoke_white"
                | "EXPORT"
                | "EXPORT_EXCLUDE"
                | "EXPORT_EXTEND"
                | "SHADOW"
                | "SHADOW_BLUR"
                | "TWO_SIDED"
                | "NO_BLEND"
                | "BLEND_GLASS"
                | "GLOBAL_specular"
                | "GLOBAL_no_blend"
                | "GLOBAL_cockpit_lit"
                | "GLOBAL_shadow_blend"
                | "GLOBAL_slope_align"
                | "GLOBAL_specular_ratio"
                | "LIGHT_FLASH"
                | "LIGHT_PARAM"
                | "LIGHT_SPILL_CUSTOM"
                | "LIGHT_CUSTOM"
                | "OBJ_8"
        ) {
            continue;
        }
        let mut number = || {
            fields
                .next()
                .ok_or("Truncated OBJ8 field")?
                .parse::<usize>()
                .map_err(|_| "Invalid OBJ8 integer")
        };
        match command {
            "POINT_COUNTS" => {
                if counts.is_some() {
                    return Err("Duplicated OBJ8 point counts".into());
                }
                counts = Some([number()?, number()?, number()?, number()?]);
                if counts.unwrap().iter().any(|count| *count > 1_000_000) {
                    return Err("OBJ8 geometry exceeds its count budget".into());
                }
            }
            "VT" | "VLINE" => {
                let values = line
                    .split_whitespace()
                    .skip(1)
                    .map(str::parse::<f64>)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| "Invalid OBJ8 coordinate")?;
                let expected = if command == "VT" { 8 } else { 6 };
                if values.len() != expected
                    || values.iter().any(|value| !value.is_finite())
                    || values[..3].iter().any(|value| value.abs() > 100_000.0)
                {
                    return Err("OBJ8 vertex exceeds its shape or coordinate budget".into());
                }
                let target = if command == "VT" {
                    &mut vertices
                } else {
                    &mut line_vertices
                };
                if target.len() >= 1_000_000 {
                    return Err("OBJ8 vertex budget exceeded".into());
                }
                target.push(values[..3].try_into().unwrap());
            }
            "IDX" | "IDX10" => {
                for _ in 0..if command == "IDX" { 1 } else { 10 } {
                    if indices.len() >= 1_000_000 {
                        return Err("OBJ8 index budget exceeded".into());
                    }
                    indices.push(number()?);
                }
                if fields.next().is_some() {
                    return Err("Unexpected OBJ8 index fields".into());
                }
            }
            "TRIS" | "LINES" => {
                let start = number()?;
                let count = number()?;
                let width = if command == "TRIS" { 3 } else { 2 };
                if count % width != 0 {
                    return Err("Incomplete OBJ8 primitive".into());
                }
                let selected = indices
                    .get(start..start.checked_add(count).ok_or("OBJ8 range overflow")?)
                    .ok_or("OBJ8 primitive index range is missing")?;
                let table = if command == "TRIS" {
                    &vertices
                } else {
                    &line_vertices
                };
                for primitive in selected.chunks_exact(width) {
                    if bounds.len() >= 333_334 {
                        return Err("OBJ8 primitive budget exceeded".into());
                    }
                    let mut low = [f64::INFINITY; 3];
                    let mut high = [f64::NEG_INFINITY; 3];
                    for index in primitive {
                        let point = table
                            .get(*index)
                            .ok_or("OBJ8 primitive vertex is missing")?;
                        for axis in 0..3 {
                            low[axis] = low[axis].min(point[axis]);
                            high[axis] = high[axis].max(point[axis]);
                        }
                    }
                    bounds.push(bound(low, high));
                }
            }
            _ if command.starts_with("ATTR_")
                || command.starts_with("GLOBAL_")
                || command.starts_with("TEXTURE")
                || command.starts_with("LIGHT_") => {}
            "VLIGHT" | "LIGHTS" | "COCKPIT_REGION" | "DECAL_LIB" | "LOAD_CENTER"
            | "LAYER_GROUP" | "IF" | "ELSE" | "ENDIF" => {}
            _ => return Err(format!("Unresolved OBJ8 directive {command}")),
        }
    }
    let Some(counts) = counts else {
        // A definition that carries nothing but its header describes no geometry and therefore
        // no obstacle. One that declares vertices or primitives without counts is truncated.
        return if vertices.is_empty()
            && line_vertices.is_empty()
            && indices.is_empty()
            && bounds.is_empty()
        {
            Ok(Vec::new())
        } else {
            Err("OBJ8 point counts are missing".into())
        };
    };
    if vertices.len() != counts[0] || line_vertices.len() != counts[1] || indices.len() != counts[3]
    {
        return Err("OBJ8 tables do not match declared counts".into());
    }
    if bounds.is_empty() {
        return Ok(Vec::new());
    }
    if animated {
        // A rotation about a pivot inside the object cannot take a vertex further than three
        // times the object's static reach from its origin: the vertex, the pivot and the pivot's
        // rotated image are each bounded by that reach. Published translations add to the reach.
        let reach = vertices
            .iter()
            .chain(&line_vertices)
            .map(|point| point.iter().map(|value| value * value).sum::<f64>().sqrt())
            .fold(0.0_f64, f64::max);
        let radius = 3.0 * reach + translation;
        if !radius.is_finite() || radius > 100_000.0 {
            return Err("OBJ8 animation exceeds its shape budget".into());
        }
        return Ok(vec![bound([-radius; 3], [radius; 3])]);
    }
    if tilted {
        for bound in &mut bounds {
            let height = f64::from(bound.high[1]) - f64::from(bound.low[1]);
            let height = height as f32;
            bound.low[0] -= height;
            bound.high[0] += height;
            bound.low[2] -= height;
            bound.high[2] += height;
        }
    }
    // A detailed model can carry hundreds of thousands of primitives. Once a model is that
    // detailed its individual triangles add nothing to an obstruction check, so it is replaced
    // by the single box that contains all of them. That keeps the retained geometry bounded
    // while still reporting an obstruction everywhere the model is drawn.
    const DETAIL_LIMIT: usize = 4096;
    if bounds.len() > DETAIL_LIMIT {
        let mut low = [f64::INFINITY; 3];
        let mut high = [f64::NEG_INFINITY; 3];
        for bound in &bounds {
            for axis in 0..3 {
                low[axis] = low[axis].min(f64::from(bound.low[axis]));
                high[axis] = high[axis].max(f64::from(bound.high[axis]));
            }
        }
        bounds = vec![bound(low, high)];
    }
    Ok(bounds)
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    const OBJECT: &str = "I\n800\nOBJ\nPOINT_COUNTS 4 0 0 3\nVT 0 0 0 0 1 0 0 0\nVT 2 12 0 0 1 0 0 0\nVT 0 0 3 0 1 0 0 0\nVT 900 900 900 0 1 0 0 0\nIDX 0\nIDX 1\nIDX 2\nTRIS 0 3\n";

    #[test]
    fn static_obstacles_use_drawn_primitives_and_bound_motion_conservatively() {
        let geometry = parse(OBJECT).unwrap();
        assert_eq!(geometry.len(), 1);
        assert_eq!(geometry[0].low, [0.0; 3]);
        assert_eq!(geometry[0].high, [2.0, 12.0, 3.0]);
        assert!(parse(&format!("{OBJECT}UNRECOGNISED_GEOMETRY\n")).is_err());
        // Terrain alignment widens the footprint by the object height so a tilted part stays
        // inside the checked volume.
        let tilted = parse(&format!("{OBJECT}TILTED\n")).unwrap();
        assert_eq!(tilted[0].low, [-12.0, 0.0, -12.0]);
        assert_eq!(tilted[0].high, [14.0, 12.0, 15.0]);
        // An animated part is contained by a box around the object's static reach. The fourth
        // vertex is far away, so the envelope has to include it.
        let animated = parse(&format!("{OBJECT}ANIM_rotate 0 1 0 0 90 0 1 sim/test\n")).unwrap();
        assert_eq!(animated.len(), 1);
        let reach = 900.0 * 3.0_f64.sqrt();
        assert!(f64::from(animated[0].low[0]) <= -3.0 * reach);
        assert!(f64::from(animated[0].high[0]) >= 3.0 * reach);
        // A published translation adds to that envelope rather than being discarded.
        let moving = parse(&format!("{OBJECT}ANIM_trans 0 0 0 5000 0 0 0 0 none\n")).unwrap();
        assert!(moving[0].high[0] >= 5000.0);
        for invalid in [
            OBJECT.replace("IDX 2", "IDX 9"),
            OBJECT.replace("TRIS 0 3", "TRIS 0 6"),
            OBJECT.replace("VT 2 12", "VT 2 NaN"),
            OBJECT.replace("4 0 0 3", "3 0 0 3"),
        ] {
            assert!(parse(&invalid).is_err());
        }
    }

    #[test]
    fn an_empty_object_definition_has_no_geometry() {
        assert!(parse("A\n800\nOBJ\n").unwrap().is_empty());
        assert!(parse("A\n800\nOBJ\nVT 0 0 0 0 1 0 0 0\n").is_err());
    }

    #[test]
    fn a_highly_detailed_model_is_reduced_to_one_containing_box() {
        let triangles = 4097usize;
        let mut text = format!("I\n800\nOBJ\nPOINT_COUNTS 2 0 0 {}\n", triangles * 3);
        text.push_str("VT 0 0 0 0 1 0 0 0\nVT 4 9 1 0 1 0 0 0\n");
        for index in 0..triangles * 3 {
            writeln!(text, "IDX {}", index % 2).unwrap();
        }
        writeln!(text, "TRIS 0 {}", triangles * 3).unwrap();
        let geometry = parse(&text).unwrap();
        assert_eq!(geometry.len(), 1);
        assert_eq!(geometry[0].low, [0.0, 0.0, 0.0]);
        assert_eq!(geometry[0].high, [4.0, 9.0, 1.0]);
    }
}
