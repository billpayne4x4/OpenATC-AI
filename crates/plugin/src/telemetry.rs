//! Simulator readings and unit conversions. Numeric datarefs can use any SDK
//! width. Unavailable optional instruments remain unknown instead of rejecting
//! the aircraft's position and radio state.

use xplane::data::{ArrayRead, DataRead};

/// Readers resolved when the plugin is enabled.
#[derive(Default)]
pub struct Refs {
    /// Type code supplied by the currently loaded aircraft.
    pub aircraft_icao: Option<Box<dyn Fn() -> String>>,
    /// Simulator wake category for the ownship: 0 light, 1 medium, 2 heavy, 3 super.
    pub wake_category: Option<Box<dyn Fn() -> i32>>,
    /// Simulator wingspan for the ownship, metres (from TCAS data).
    pub wingspan_meters: Option<Box<dyn Fn() -> f64>>,
    /// Simulator wingspan for the ownship, metres (from acf_size_x).
    pub wingspan_meters_acf: Option<Box<dyn Fn() -> f64>>,
    /// Simulator length for the ownship, metres (from acf_size_z).
    pub length_meters_acf: Option<Box<dyn Fn() -> f64>>,
    /// Maximum takeoff weight for the ownship, kg (from acf_m_max).
    pub max_takeoff_weight_kg: Option<Box<dyn Fn() -> f64>>,
    pub traffic: Option<Box<dyn Fn() -> Vec<openatc_core::state::TrafficTarget>>>,
    /// Latitude degrees.
    pub latitude: Option<Box<dyn Fn() -> f64>>,
    /// Longitude degrees.
    pub longitude: Option<Box<dyn Fn() -> f64>>,
    /// Elevation meters MSL.
    pub altitude: Option<Box<dyn Fn() -> f64>>,
    /// Pilot's barometric altitude, feet.
    pub indicated_altitude: Option<Box<dyn Fn() -> f64>>,
    /// Standard-pressure altitude, feet.
    pub pressure_altitude: Option<Box<dyn Fn() -> f64>>,
    /// Ground speed meters per second.
    pub speed: Option<Box<dyn Fn() -> f64>>,
    /// Heading degrees true.
    pub heading: Option<Box<dyn Fn() -> f64>>,
    /// Actual ground track degrees true.
    pub track: Option<Box<dyn Fn() -> f64>>,
    /// Indicated airspeed knots.
    pub indicated_speed: Option<Box<dyn Fn() -> f64>>,
    /// Magnetic heading degrees.
    pub magnetic_heading: Option<Box<dyn Fn() -> f64>>,
    /// Simulator engine-running flags.
    pub engines: Option<Box<dyn Fn() -> bool>>,
    /// Weight on wheels flag.
    pub ground: Option<Box<dyn Fn() -> i32>>,
    /// Pause flag.
    pub pause: Option<Box<dyn Fn() -> i32>>,
    /// Simulator clock for traffic progress and wake intervals.
    pub simulation_seconds: Option<Box<dyn Fn() -> f64>>,
    /// Simulator UTC clock, independent of wall-clock time.
    pub zulu_seconds: Option<Box<dyn Fn() -> f64>>,
    /// Vertical speed feet per minute, if present.
    pub vertical_speed: Option<Box<dyn Fn() -> f64>>,
    /// Height above ground meters, if present.
    pub agl: Option<Box<dyn Fn() -> f64>>,
    /// COM1 frequency in kHz, if present.
    pub com1: Option<Box<dyn Fn() -> i32>>,
    /// COM2 frequency in kHz, if present.
    pub com2: Option<Box<dyn Fn() -> i32>>,
    pub transponder_code: Option<Box<dyn Fn() -> i32>>,
    pub transponder_mode: Option<Box<dyn Fn() -> i32>>,
}

fn find_integer(data: &mut xplane::data::DataApi, name: &str) -> Option<Box<dyn Fn() -> i32>> {
    data.find::<i32, _>(name)
        .ok()
        .map(|found| Box::new(move || found.get()) as Box<dyn Fn() -> i32>)
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
        aircraft_icao: data
            .find::<[u8], _>("sim/aircraft/view/acf_ICAO")
            .ok()
            .map(|found| {
                Box::new(move || {
                    let mut code = [0; 40];
                    found.get(&mut code);
                    String::from_utf8_lossy(&code)
                        .trim_end_matches('\0')
                        .trim()
                        .to_owned()
                }) as Box<dyn Fn() -> String>
            }),
        wake_category: data
            .find::<[i32], _>("sim/cockpit2/tcas/targets/wake/wake_cat")
            .ok()
            .map(|found| {
                Box::new(move || {
                    let mut value = [0; 1];
                    found.get(&mut value);
                    value[0]
                }) as Box<dyn Fn() -> i32>
            }),
        wingspan_meters: data
            .find::<[f32], _>("sim/cockpit2/tcas/targets/wake/wing_span_m")
            .ok()
            .map(|found| {
                Box::new(move || {
                    let mut value = [0.0; 1];
                    found.get(&mut value);
                    f64::from(value[0])
                }) as Box<dyn Fn() -> f64>
            }),
        // ACF file datarefs for aircraft dimensions and weights
        wingspan_meters_acf: find_number(data, "sim/aircraft/view/acf_size_x"),
        length_meters_acf: find_number(data, "sim/aircraft/view/acf_size_z"),
        max_takeoff_weight_kg: find_number(data, "sim/aircraft/weight/acf_m_max"),
        transponder_code: find_integer(data, "sim/cockpit2/radios/actuators/transponder_code"),
        transponder_mode: find_integer(data, "sim/cockpit2/radios/actuators/transponder_mode"),
        indicated_speed: find_number(data, "sim/cockpit2/gauges/indicators/airspeed_kts_pilot"),
        magnetic_heading: find_number(
            data,
            "sim/cockpit2/gauges/indicators/heading_AHARS_deg_mag_pilot",
        ),
        engines: data
            .find::<[i32], _>("sim/flightmodel/engine/ENGN_running")
            .ok()
            .map(|r| {
                Box::new(move || {
                    let mut running = [0; 16];
                    r.get(&mut running);
                    running.iter().any(|v| *v != 0)
                }) as Box<dyn Fn() -> bool>
            }),
        traffic: traffic_reader(data),
        latitude: named_number(data, log, "sim/flightmodel/position/latitude"),
        longitude: named_number(data, log, "sim/flightmodel/position/longitude"),
        altitude: named_number(data, log, "sim/flightmodel/position/elevation"),
        indicated_altitude: find_number(data, "sim/cockpit2/gauges/indicators/altitude_ft_pilot"),
        pressure_altitude: find_number(data, "sim/flightmodel2/position/pressure_altitude"),
        speed: named_number(data, log, "sim/flightmodel/position/groundspeed"),
        heading: named_number(data, log, "sim/flightmodel/position/psi"),
        track: find_number(
            data,
            "sim/cockpit2/gauges/indicators/ground_track_true_pilot",
        ),
        ground: named_flag(data, log, "sim/flightmodel/failures/onground_any"),
        pause: named_flag(data, log, "sim/time/paused"),
        simulation_seconds: find_number(data, "sim/time/total_flight_time_sec"),
        zulu_seconds: find_number(data, "sim/time/zulu_time_sec"),
        vertical_speed: find_number(data, "sim/flightmodel/position/vh_ind_fpm"),
        agl: find_number(data, "sim/flightmodel/position/y_agl"),
        com1: find_integer(data, "sim/cockpit2/radios/actuators/com1_frequency_hz_833"),
        com2: find_integer(data, "sim/cockpit2/radios/actuators/com2_frequency_hz_833"),
    }
}

/// Read one telemetry snapshot with unit conversions.
#[must_use]
/// Read the takeoff instruments without collecting traffic or radio state.
pub fn gather_callouts(refs: &Refs) -> openatc_core::state::Telemetry {
    openatc_core::state::Telemetry {
        on_ground: refs.ground.as_ref().is_none_or(|read| read() != 0),
        ground_speed_knots: refs
            .speed
            .as_ref()
            .map_or(0.0, |read| read() * 1.943_844_492),
        indicated_speed_knots: optional_number(&refs.indicated_speed, true),
        paused: refs.pause.as_ref().is_some_and(|read| read() != 0),
        vertical_speed_fpm: refs.vertical_speed.as_ref().map_or(0.0, |read| read()),
        height_agl_feet: refs.agl.as_ref().map_or(0.0, |read| read() * 3.280_839_895),
        simulation_seconds: optional_number(&refs.simulation_seconds, true),
        zulu_seconds: optional_number(&refs.zulu_seconds, true).filter(|v| *v < 86400.0),
        ..Default::default()
    }
}

pub fn gather(refs: &Refs) -> openatc_core::state::Telemetry {
    let number = |value: &Option<Box<dyn Fn() -> f64>>| value.as_ref().map_or(0.0, |read| read());
    let flag = |value: &Option<Box<dyn Fn() -> i32>>| value.as_ref().map_or(0, |read| read());
    let latitude = number(&refs.latitude);
    let longitude = number(&refs.longitude);
    openatc_core::state::Telemetry {
        aircraft_icao: refs
            .aircraft_icao
            .as_ref()
            .map_or_else(String::new, |read| read()),
        wingspan_meters: refs
            .wingspan_meters
            .as_ref()
            .map(|read| read())
            .filter(|span| span.is_finite() && (1.0..=150.0).contains(span)),
        wake_category: refs
            .wake_category
            .as_ref()
            .map(|read| read())
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| *value <= 3),
        traffic: refs.traffic.as_ref().map(|read| read()),
        radio_busy: false,
        radio_sequence_seen: None,
        latitude,
        longitude,
        altitude_feet: number(&refs.altitude) * 3.280_839_895,
        indicated_altitude_feet: optional_number(&refs.indicated_altitude, false),
        pressure_altitude_feet: optional_number(&refs.pressure_altitude, false),
        ground_speed_knots: number(&refs.speed) * 1.943_844_492,
        heading_degrees: number(&refs.heading),
        ground_track_degrees: optional_number(&refs.track, false),
        indicated_speed_knots: optional_number(&refs.indicated_speed, true),
        magnetic_heading_degrees: optional_number(&refs.magnetic_heading, false),
        on_ground: flag(&refs.ground) != 0,
        engines_running: refs.engines.as_ref().is_some_and(|read| read()),
        paused: flag(&refs.pause) != 0,
        simulation_seconds: optional_number(&refs.simulation_seconds, true),
        zulu_seconds: optional_number(&refs.zulu_seconds, true).filter(|v| *v < 86400.0),
        vertical_speed_fpm: refs.vertical_speed.as_ref().map_or(0.0, |read| read()),
        height_agl_feet: refs.agl.as_ref().map_or(0.0, |read| read()) * 3.280_839_895,
        radio_power: true,
        com1_khz: flag(&refs.com1),
        com2_khz: refs.com2.as_ref().map(|read| read()),
        transponder_code: refs.transponder_code.as_ref().map(|read| read()),
        transponder_mode: refs.transponder_mode.as_ref().map(|read| read()),
        position_valid: refs.latitude.is_some()
            && refs.longitude.is_some()
            && latitude.is_finite()
            && longitude.is_finite()
            && latitude.abs() <= 90.0
            && longitude.abs() <= 180.0,
    }
}

// Unavailable instrument readings must not discard a valid position and radio.
fn optional_number(reader: &Option<Box<dyn Fn() -> f64>>, nonnegative: bool) -> Option<f64> {
    reader
        .as_ref()
        .map(|read| read())
        .filter(|value| value.is_finite() && (!nonnegative || *value >= 0.0))
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
    let flight_ids = data
        .find::<[u8], _>("sim/cockpit2/tcas/targets/flight_id")
        .ok();
    let aircraft_types = data
        .find::<[u8], _>("sim/cockpit2/tcas/targets/icao_type")
        .ok();
    let wake_categories = data
        .find::<[i32], _>("sim/cockpit2/tcas/targets/wake/wake_cat")
        .ok();
    let wing_spans = data
        .find::<[f32], _>("sim/cockpit2/tcas/targets/wake/wing_span_m")
        .ok();
    let vx = data
        .find::<[f32], _>("sim/cockpit2/tcas/targets/position/vx")
        .ok();
    let vz = data
        .find::<[f32], _>("sim/cockpit2/tcas/targets/position/vz")
        .ok();
    Some(Box::new(move || {
        // TCAS exposes 64 slots, including ownship; read them without six temporary vectors.
        let (mut id_values, mut lat_values, mut lon_values) = ([0; 64], [0.0; 64], [0.0; 64]);
        let (mut ele_values, mut track_values, mut wheel_values) = ([0.0; 64], [0.0; 64], [0; 64]);
        let (mut flights, mut types) = ([0; 512], [0; 512]);
        if let Some(ids) = &flight_ids {
            ids.get(&mut flights);
        }
        if let Some(ids) = &aircraft_types {
            ids.get(&mut types);
        }
        let (mut east, mut north) = ([0.0; 64], [0.0; 64]);
        let (mut wake_values, mut wing_values) = ([0; 64], [0.0; 64]);
        if let Some(values) = &wake_categories {
            values.get(&mut wake_values);
        }
        if let Some(values) = &wing_spans {
            values.get(&mut wing_values);
        }
        let speed_available = vx
            .as_ref()
            .zip(vz.as_ref())
            .map(|(x, z)| x.get(&mut east).min(z.get(&mut north)))
            .unwrap_or(0);
        let code = |bytes: &[u8]| {
            String::from_utf8_lossy(bytes.split(|b| *b == 0).next().unwrap_or_default())
                .trim()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .take(8)
                .collect()
        };
        let len = [
            ids.get(&mut id_values),
            lat.get(&mut lat_values),
            lon.get(&mut lon_values),
            ele.get(&mut ele_values),
            track.get(&mut track_values),
            wheels.get(&mut wheel_values),
        ]
        .into_iter()
        .min()
        .unwrap_or(0);
        let mut seen = std::collections::BTreeSet::new();
        if let Some(own) = crate::tcas::mode_s_identity(id_values[0]) {
            seen.insert(own);
        }
        (1..len)
            .filter_map(|i| {
                let id = crate::tcas::mode_s_identity(id_values[i])?;
                seen.insert(id).then_some((i, id))
            })
            .map(|(i, id)| openatc_core::state::TrafficTarget {
                id,
                callsign: code(&flights[i * 8..i * 8 + 8]),
                aircraft: code(&types[i * 8..i * 8 + 8]),
                ground_speed_knots: (i < speed_available)
                    .then(|| f64::from(east[i].hypot(north[i])) * 1.943_844_492),
                wingspan_meters: (wing_values[i].is_finite()
                    && (1.0..=150.0).contains(&wing_values[i]))
                .then_some(f64::from(wing_values[i])),
                wake_category: u8::try_from(wake_values[i])
                    .ok()
                    .filter(|value| *value <= 3),
                latitude: f64::from(lat_values[i]),
                longitude: f64::from(lon_values[i]),
                altitude_feet: f64::from(ele_values[i]) * 3.280_839_895,
                track_degrees: f64::from(track_values[i]),
                on_ground: wheel_values[i] != 0,
                ..Default::default()
            })
            .collect()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callout_sampling_leaves_unrelated_readers_untouched() {
        let refs = Refs {
            ground: Some(Box::new(|| 0)),
            agl: Some(Box::new(|| 3.0)),
            vertical_speed: Some(Box::new(|| 500.0)),
            indicated_speed: Some(Box::new(|| 150.0)),
            zulu_seconds: Some(Box::new(|| 86399.5)),
            traffic: Some(Box::new(|| panic!("traffic must not be read by callouts"))),
            ..Default::default()
        };
        let t = gather_callouts(&refs);
        assert!(!t.on_ground);
        assert!(t.height_agl_feet > 9.8 && t.height_agl_feet < 9.9);
        assert_eq!(t.vertical_speed_fpm, 500.0);
        assert_eq!(t.indicated_speed_knots, Some(150.0));
        assert_eq!(t.zulu_seconds, Some(86399.5));
        let missing = gather_callouts(&Refs::default());
        assert!(missing.on_ground);
        assert_eq!(missing.vertical_speed_fpm, 0.0);
        assert!(missing.indicated_speed_knots.is_none());
    }

    #[test]
    fn barometric_altitudes_stay_separate_from_geometric_position() {
        let refs = Refs {
            altitude: Some(Box::new(|| 18311.7 / 3.280_839_895)),
            indicated_altitude: Some(Box::new(|| 18000.0)),
            pressure_altitude: Some(Box::new(|| 18000.0)),
            ..Default::default()
        };
        let telemetry = gather(&refs);
        assert!((telemetry.altitude_feet - 18311.7).abs() < 0.01);
        assert_eq!(telemetry.reported_altitude_feet(), 18000.0);
        assert_eq!(telemetry.clearance_altitude_feet(18000, 10000), 18000.0);
        let invalid = Refs {
            indicated_altitude: Some(Box::new(|| f64::NAN)),
            pressure_altitude: Some(Box::new(|| f64::INFINITY)),
            ..Default::default()
        };
        let telemetry = gather(&invalid);
        assert_eq!(telemetry.indicated_altitude_feet, None);
        assert_eq!(telemetry.pressure_altitude_feet, None);
    }

    #[test]
    fn unavailable_instruments_do_not_hide_the_airport_position() {
        let mut refs = Refs {
            latitude: Some(Box::new(|| 33.938318)),
            longitude: Some(Box::new(|| -118.383084)),
            ground: Some(Box::new(|| 1)),
            com1: Some(Box::new(|| 121705)),
            com2: Some(Box::new(|| 118700)),
            indicated_speed: Some(Box::new(|| -0.1)),
            simulation_seconds: Some(Box::new(|| -1.0)),
            zulu_seconds: Some(Box::new(|| 86400.0)),
            track: Some(Box::new(|| f64::NAN)),
            magnetic_heading: Some(Box::new(|| f64::INFINITY)),
            agl: Some(Box::new(|| -0.12)),
            ..Default::default()
        };
        let sample = gather(&refs);
        assert!(sample.position_valid);
        assert_eq!(sample.com1_khz, 121705);
        assert_eq!(sample.com2_khz, Some(118700));
        assert_eq!(sample.indicated_speed_knots, None);
        assert_eq!(sample.simulation_seconds, None);
        assert_eq!(sample.zulu_seconds, None);
        assert_eq!(sample.ground_track_degrees, None);
        assert_eq!(sample.magnetic_heading_degrees, None);
        let mut state = openatc_core::state::State::default();
        openatc_core::ops::update_flight_phase(&mut state, &sample).unwrap();
        assert_eq!(state.telemetry.latitude, 33.938318);
        assert!(state.telemetry.height_agl_feet < 0.0);
        refs.indicated_speed = Some(Box::new(|| 140.0));
        assert_eq!(gather(&refs).indicated_speed_knots, Some(140.0));
        refs.latitude = Some(Box::new(|| 95.0));
        let invalid = gather(&refs);
        assert!(!invalid.position_valid);
        let error = openatc_core::ops::update_flight_phase(&mut state, &invalid).unwrap_err();
        assert!(error.contains("latitude = 95"));
        assert_eq!(state.telemetry.latitude, sample.latitude);
        refs.com2 = None;
        assert_eq!(gather(&refs).com2_khz, None);
    }
}
