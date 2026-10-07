//! Simulator telemetry bundle: the datarefs the flight loop reads, with the
//! unit conversions the engine expects. Numeric refs resolve flexibly (f64,
//! f32, i32) because sim versions disagree on widths; a missing ref reads
//! zero and gets logged, never failing enable.

use xplane::data::{ArrayRead, DataRead, ReadOnly, borrowed::DataRef};

/// Live datarefs; missing ones read as zero.
pub struct Refs {
    pub traffic: Option<Box<dyn Fn() -> Vec<openatc_core::state::TrafficTarget>>>,
    /// Latitude degrees.
    pub latitude: Option<Box<dyn Fn() -> f64>>,
    /// Longitude degrees.
    pub longitude: Option<Box<dyn Fn() -> f64>>,
    /// Elevation meters MSL.
    pub altitude: Option<Box<dyn Fn() -> f64>>,
    /// Ground speed meters per second.
    pub speed: Option<Box<dyn Fn() -> f64>>,
    /// Heading degrees true.
    pub heading: Option<Box<dyn Fn() -> f64>>,
    /// Actual ground track degrees true.
    pub track: Option<Box<dyn Fn() -> f64>>,
    /// Weight on wheels flag.
    pub ground: Option<Box<dyn Fn() -> i32>>,
    /// Pause flag.
    pub pause: Option<Box<dyn Fn() -> i32>>,
    /// Vertical speed feet per minute, if present.
    pub vertical_speed: Option<Box<dyn Fn() -> f64>>,
    /// Height above ground meters, if present.
    pub agl: Option<Box<dyn Fn() -> f64>>,
    /// COM1 frequency Hz, if present.
    pub com1: Option<DataRef<i32, ReadOnly>>,
}

/// Find a numeric ref in whatever width the sim provides it.
fn find_number(data: &mut xplane::data::DataApi, name: &str) -> Option<Box<dyn Fn() -> f64>> {
    if let Ok(found) = data.find::<f64, _>(name) {
        return Some(Box::new(move || found.get()));
    }
    if let Ok(found) = data.find::<f32, _>(name) {
        return Some(Box::new(move || f64::from(found.get())));
    }
    if let Ok(found) = data.find::<i32, _>(name) {
        return Some(Box::new(move || f64::from(found.get())));
    }
    None
}

/// Look a numeric ref up, logging the miss.
fn named_number(
    data: &mut xplane::data::DataApi,
    log: &mut Vec<String>,
    name: &str,
) -> Option<Box<dyn Fn() -> f64>> {
    if let Some(found) = find_number(data, name) {
        return Some(found);
    }
    log.push(format!("OpenATC AI: dataref {name} MISSING, reading zero"));
    None
}

/// Look a flag ref up, logging the miss.
fn named_flag(
    data: &mut xplane::data::DataApi,
    log: &mut Vec<String>,
    name: &str,
) -> Option<Box<dyn Fn() -> i32>> {
    if let Ok(found) = data.find::<i32, _>(name) {
        let found = Box::new(found);
        let reader: Box<dyn Fn() -> i32> = Box::new(move || found.get());
        return Some(reader);
    }
    log.push(format!("OpenATC AI: dataref {name} MISSING, reading zero"));
    None
}

/// Look every ref up, logging the misses. Never fails: a missing ref reads
/// zero downstream so enable always succeeds.
pub fn find_refs(data: &mut xplane::data::DataApi, log: &mut Vec<String>) -> Refs {
    Refs {
        traffic: traffic_reader(data),
        latitude: named_number(data, log, "sim/flightmodel/position/latitude"),
        longitude: named_number(data, log, "sim/flightmodel/position/longitude"),
        altitude: named_number(data, log, "sim/flightmodel/position/elevation"),
        speed: named_number(data, log, "sim/flightmodel/position/groundspeed"),
        heading: named_number(data, log, "sim/flightmodel/position/psi"),
        track: find_number(
            data,
            "sim/cockpit2/gauges/indicators/ground_track_true_pilot",
        ),
        ground: named_flag(data, log, "sim/flightmodel/failures/onground_any"),
        pause: named_flag(data, log, "sim/time/paused"),
        vertical_speed: find_number(data, "sim/flightmodel/position/vh_ind_fpm"),
        agl: find_number(data, "sim/flightmodel/position/y_agl"),
        com1: data
            .find("sim/cockpit2/radios/actuators/com1_frequency_hz_833")
            .ok(),
    }
}

/// Read one telemetry snapshot with unit conversions.
#[must_use]
pub fn gather(refs: &Refs) -> openatc_core::state::Telemetry {
    let number = |value: &Option<Box<dyn Fn() -> f64>>| value.as_ref().map_or(0.0, |read| read());
    let flag = |value: &Option<Box<dyn Fn() -> i32>>| value.as_ref().map_or(0, |read| read());
    openatc_core::state::Telemetry {
        traffic: refs.traffic.as_ref().map(|read| read()),
        latitude: number(&refs.latitude),
        longitude: number(&refs.longitude),
        altitude_feet: number(&refs.altitude) * 3.280_839_895,
        ground_speed_knots: number(&refs.speed) * 1.943_844_492,
        heading_degrees: number(&refs.heading),
        ground_track_degrees: refs.track.as_ref().map(|read| read()),
        on_ground: flag(&refs.ground) != 0,
        paused: flag(&refs.pause) != 0,
        vertical_speed_fpm: refs.vertical_speed.as_ref().map_or(0.0, |read| read()),
        height_agl_feet: refs.agl.as_ref().map_or(0.0, |read| read()) * 3.280_839_895,
        radio_power: true,
        com1_khz: refs.com1.as_ref().map_or(0, DataRead::get),
        position_valid: refs.latitude.is_some()
            && refs.longitude.is_some()
            && number(&refs.latitude).is_finite()
            && number(&refs.longitude).is_finite(),
    }
}

fn traffic_reader(
    data: &mut xplane::data::DataApi,
) -> Option<Box<dyn Fn() -> Vec<openatc_core::state::TrafficTarget>>> {
    let ids = data
        .find::<[i32], _>("sim/cockpit2/tcas/targets/modeS_id")
        .ok()?;
    let lat = data
        .find::<[f32], _>("sim/cockpit2/tcas/targets/position/lat")
        .ok()?;
    let lon = data
        .find::<[f32], _>("sim/cockpit2/tcas/targets/position/lon")
        .ok()?;
    let ele = data
        .find::<[f32], _>("sim/cockpit2/tcas/targets/position/ele")
        .ok()?;
    let track = data
        .find::<[f32], _>("sim/cockpit2/tcas/targets/position/hpath")
        .ok()?;
    let wheels = data
        .find::<[i32], _>("sim/cockpit2/tcas/targets/position/weight_on_wheels")
        .ok()?;
    Some(Box::new(move || {
        let (ids, lat, lon, ele, track, wheels) = (
            ids.as_vec(),
            lat.as_vec(),
            lon.as_vec(),
            ele.as_vec(),
            track.as_vec(),
            wheels.as_vec(),
        );
        let len = [
            ids.len(),
            lat.len(),
            lon.len(),
            ele.len(),
            track.len(),
            wheels.len(),
        ]
        .into_iter()
        .min()
        .unwrap_or(0);
        (1..len)
            .filter(|i| ids[*i] != 0)
            .map(|i| openatc_core::state::TrafficTarget {
                latitude: f64::from(lat[i]),
                longitude: f64::from(lon[i]),
                altitude_feet: f64::from(ele[i]) * 3.280_839_895,
                track_degrees: f64::from(track[i]),
                on_ground: wheels[i] != 0,
            })
            .collect()
    }))
}
