//! Aircraft TOML profiles, radio datarefs and electrical power checks.

/// Comms datarefs: call lights, never buttons.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AircraftComms {
    /// Attendant-call lights → cabin.
    pub attendant_refs: Vec<String>,
    /// Ground-crew-call lights → ground.
    pub ground_refs: Vec<String>,
    /// Only `ignore` is implemented.
    pub emer_action: String,
}

/// Electrical sources; empty means unmapped.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AircraftElectrical {
    /// Battery volts gauge ref, empty when unmapped.
    pub bat_volts_ref: String,
    /// Volts at or above which the bus counts as powered.
    pub min_volts: f64,
    /// Battery switch refs.
    pub battery_refs: Vec<String>,
    /// Ground power refs.
    pub gpu_refs: Vec<String>,
    /// APU generator refs.
    pub apu_refs: Vec<String>,
    /// Radio management panel refs.
    pub rmp_refs: Vec<String>,
    /// Avionics switch refs.
    pub avionics_refs: Vec<String>,
}

/// One aircraft family profile.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AircraftProfile {
    /// Display name.
    pub name: String,
    /// Author substring that selects this profile.
    pub match_author: String,
    /// ICAO codes that select this profile.
    pub match_icao: Vec<String>,
    /// Source path, for diagnostics.
    pub source: String,
    /// Comms mapping.
    pub comms: AircraftComms,
    /// Electrical mapping.
    pub electrical: AircraftElectrical,
    /// Autopilot target mapping. Absent section means stock dials.
    pub autopilot: AircraftAutopilot,
    /// Writable COM1 active channel, integer kHz.
    pub com1_active_ref: String,
}

/// Autopilot target refs. Defaults are the stock X-Plane dials, which is
/// why standard aircraft need no mapping at all.
#[derive(Clone, Debug, PartialEq)]
pub struct AircraftAutopilot {
    /// AP master mode ref (off=0, FD=1, on=2).
    pub mode_ref: String,
    /// Target altitude feet MSL.
    pub altitude_ref: String,
    /// Target heading degrees magnetic.
    pub heading_ref: String,
    /// Target speed knots.
    pub speed_ref: String,
}

impl Default for AircraftAutopilot {
    fn default() -> Self {
        Self {
            mode_ref: "sim/cockpit/autopilot/autopilot_mode".to_owned(),
            altitude_ref: "sim/cockpit/autopilot/altitude".to_owned(),
            heading_ref: "sim/cockpit/autopilot/heading_mag".to_owned(),
            speed_ref: "sim/cockpit/autopilot/airspeed".to_owned(),
        }
    }
}

/// Live readings feeding power evaluation.
#[derive(Clone, Debug, Default)]
pub struct PowerInput {
    /// Battery volts gauge value.
    pub bat_volts: f64,
    /// Whether any volts ref resolved.
    pub has_volts: bool,
    /// Battery switch readings.
    pub battery: Vec<i32>,
    /// Ground power readings.
    pub gpu: Vec<i32>,
    /// APU generator readings.
    pub apu: Vec<i32>,
    /// RMP readings.
    pub rmp: Vec<i32>,
    /// Avionics switch readings.
    pub avionics: Vec<i32>,
}

/// Effective power: radio (ATC path) and bus (interphone paths).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PowerState {
    /// Radio path powered.
    pub radio: bool,
    /// Interphone bus powered.
    pub bus: bool,
}

fn string_list(
    value: Option<&toml::Value>,
    section: &str,
    key: &str,
) -> Result<Vec<String>, String> {
    let items = value
        .and_then(|value| value.as_array())
        .ok_or_else(|| format!("aircraft [{section}] misses {key}"))?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(std::string::ToString::to_string)
                .ok_or_else(|| format!("aircraft [{section}] {key} holds strings"))
        })
        .collect()
}

/// Read a required string key or fail naming section and key.
fn sub<'a>(table: &'a toml::Table, section: &str, key: &str) -> Result<&'a str, String> {
    table
        .get(key)
        .and_then(|value| value.as_str())
        .ok_or_else(|| format!("aircraft [{section}] misses {key}"))
}

/// Load and validate an aircraft profile.
/// Error messages may differ; only the verdict must agree.
pub fn load_aircraft_profile(path: &std::path::Path) -> Result<AircraftProfile, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("cannot open file: {error}"))?;
    let table: toml::Table = text
        .parse()
        .map_err(|error| format!("malformed file: {error}"))?;
    for key in table.keys() {
        if key != "aircraft" {
            return Err("only [aircraft] sections allowed".to_owned());
        }
    }
    let root = table
        .get("aircraft")
        .and_then(|value| value.as_table())
        .ok_or_else(|| "aircraft file misses [aircraft]".to_owned())?;
    let get = |key: &str| {
        root.get(key)
            .and_then(|value| value.as_str())
            .ok_or_else(|| format!("aircraft [aircraft] misses {key}"))
    };
    let comms_table = root
        .get("comms")
        .and_then(|value| value.as_table())
        .ok_or_else(|| "aircraft file misses [aircraft.comms]".to_owned())?;
    let electrical_table = root
        .get("electrical")
        .and_then(|value| value.as_table())
        .ok_or_else(|| "aircraft file misses [aircraft.electrical]".to_owned())?;
    let emer_action = sub(comms_table, "aircraft.comms", "emer_action")?.to_owned();
    if emer_action != "ignore" {
        return Err("aircraft emer_action must be ignore".to_owned());
    }
    let min_volts = electrical_table
        .get("min_volts")
        .and_then(toml::Value::as_float)
        .ok_or_else(|| "aircraft [aircraft.electrical] misses min_volts".to_owned())?;
    if min_volts < 0.0 {
        return Err("aircraft min_volts must not be negative".to_owned());
    }
    Ok(AircraftProfile {
        name: get("name")?.to_owned(),
        match_author: get("match_author")?.to_owned(),
        match_icao: string_list(root.get("match_icao"), "aircraft", "match_icao")?,
        source: path.to_string_lossy().into_owned(),
        comms: AircraftComms {
            attendant_refs: string_list(
                comms_table.get("attendant_refs"),
                "aircraft.comms",
                "attendant_refs",
            )?,
            ground_refs: string_list(
                comms_table.get("ground_refs"),
                "aircraft.comms",
                "ground_refs",
            )?,
            emer_action,
        },
        electrical: AircraftElectrical {
            bat_volts_ref: sub(electrical_table, "aircraft.electrical", "bat_volts_ref")?
                .to_owned(),
            min_volts,
            battery_refs: string_list(
                electrical_table.get("battery_refs"),
                "aircraft.electrical",
                "battery_refs",
            )?,
            gpu_refs: string_list(
                electrical_table.get("gpu_refs"),
                "aircraft.electrical",
                "gpu_refs",
            )?,
            apu_refs: string_list(
                electrical_table.get("apu_refs"),
                "aircraft.electrical",
                "apu_refs",
            )?,
            rmp_refs: string_list(
                electrical_table.get("rmp_refs"),
                "aircraft.electrical",
                "rmp_refs",
            )?,
            avionics_refs: string_list(
                electrical_table.get("avionics_refs"),
                "aircraft.electrical",
                "avionics_refs",
            )?,
        },
        autopilot: parse_autopilot(root),
        com1_active_ref: root
            .get("radio")
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get("com1_active_ref"))
            .and_then(toml::Value::as_str)
            .unwrap_or("sim/cockpit2/radios/actuators/com1_frequency_hz_833")
            .to_owned(),
    })
}

/// Autopilot mapping, optional section. Absent means stock dials, which is
/// why standard aircraft ship no mapping at all.
fn parse_autopilot(root: &toml::Table) -> AircraftAutopilot {
    let Some(table) = root.get("autopilot").and_then(|value| value.as_table()) else {
        return AircraftAutopilot::default();
    };
    let optional = |key: &str, fallback: &str| {
        table
            .get(key)
            .and_then(|value| value.as_str())
            .unwrap_or(fallback)
            .to_owned()
    };
    let standard = AircraftAutopilot::default();
    AircraftAutopilot {
        mode_ref: optional("mode_ref", &standard.mode_ref),
        altitude_ref: optional("altitude_ref", &standard.altitude_ref),
        heading_ref: optional("heading_ref", &standard.heading_ref),
        speed_ref: optional("speed_ref", &standard.speed_ref),
    }
}

/// Whether the profile selects an aircraft: author substring (case-insensitive)
/// plus exact ICAO (case-insensitive).
#[must_use]
pub fn aircraft_matches(profile: &AircraftProfile, author: &str, icao: &str) -> bool {
    if !author
        .to_ascii_lowercase()
        .contains(&profile.match_author.to_ascii_lowercase())
    {
        return false;
    }
    let upper = icao.to_ascii_uppercase();
    profile
        .match_icao
        .iter()
        .any(|code| code.to_ascii_uppercase() == upper)
}

/// Whether the profile maps any power source at all.
#[must_use]
pub fn profile_has_power_sources(profile: &AircraftProfile) -> bool {
    let electrical = &profile.electrical;
    !electrical.bat_volts_ref.is_empty()
        || !electrical.battery_refs.is_empty()
        || !electrical.gpu_refs.is_empty()
        || !electrical.apu_refs.is_empty()
        || !electrical.rmp_refs.is_empty()
        || !electrical.avionics_refs.is_empty()
}

/// Determine radio and bus power from the profile readings.
#[must_use]
pub fn evaluate_power(input: &PowerInput, min_volts: f64) -> PowerState {
    let any_on = |values: &[i32]| values.iter().any(|value| *value != 0);
    PowerState {
        bus: (input.has_volts && input.bat_volts >= min_volts)
            || any_on(&input.battery)
            || any_on(&input.gpu)
            || any_on(&input.apu),
        radio: any_on(&input.rmp) || any_on(&input.avionics),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
    }

    fn input(case: &serde_json::Value) -> PowerInput {
        let readings = |key: &str| {
            case[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| i32::try_from(value.as_i64().unwrap()).unwrap())
                .collect()
        };
        PowerInput {
            bat_volts: case["volts"].as_f64().unwrap(),
            has_volts: case["has_volts"].as_bool().unwrap(),
            battery: readings("battery"),
            gpu: readings("gpu"),
            apu: readings("apu"),
            rmp: readings("rmp"),
            avionics: readings("avionics"),
        }
    }

    #[test]
    fn matches_shared_fixtures() {
        let dir = fixtures();
        let cases: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("aircraft.json")).unwrap())
                .unwrap();
        let profile = load_aircraft_profile(&dir.join(cases["toml"].as_str().unwrap())).unwrap();
        assert_eq!(profile.name, "Test Bird");
        assert_eq!(profile.comms.attendant_refs, vec!["test/cabin".to_owned()]);
        assert!(profile_has_power_sources(&profile));
        let mut count = 0;
        for case in cases["match"].as_array().unwrap() {
            assert_eq!(
                aircraft_matches(
                    &profile,
                    case["author"].as_str().unwrap(),
                    case["icao"].as_str().unwrap()
                ),
                case["matches"].as_bool().unwrap(),
                "match {}",
                case["icao"].as_str().unwrap()
            );
            count += 1;
        }
        assert_eq!(count, 4, "matches run");
        for case in cases["power"].as_array().unwrap() {
            let power = evaluate_power(&input(case), profile.electrical.min_volts);
            assert_eq!(power.radio, case["radio"].as_bool().unwrap(), "radio");
            assert_eq!(power.bus, case["bus"].as_bool().unwrap(), "bus");
            count += 1;
        }
        assert_eq!(count, 9, "power cases run");
        for (index, text) in cases["invalid"].as_array().unwrap().iter().enumerate() {
            let path = std::env::temp_dir().join(format!("openatc_aircraft_bad_{index}.toml"));
            std::fs::write(&path, text.as_str().unwrap()).unwrap();
            assert!(
                load_aircraft_profile(&path).is_err(),
                "invalid fixture {index} must reject"
            );
            let _ = std::fs::remove_file(&path);
            count += 1;
        }
        assert_eq!(count, 12, "all fixtures run");
    }
}
