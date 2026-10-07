//! Aircraft profile matching, cockpit call controls and radio power readings.

use openatc_core::ops::read_acf_identity;
use openatc_core::profiles::{
    AircraftProfile, PowerInput, aircraft_matches, evaluate_power, load_aircraft_profile,
    profile_has_power_sources,
};
use xplane::data::{ArrayRead, DataRead, ReadOnly, borrowed::DataRef};

/// Scalar or array readings, optionally selecting a TOML array index.
pub type Readings = Box<dyn Fn() -> Vec<f64>>;

fn find_readings(data: &mut xplane::data::DataApi, name: &str) -> Option<Readings> {
    let (root, index) = name
        .strip_suffix(']')
        .and_then(|v| v.rsplit_once('['))
        .and_then(|(root, index)| index.parse::<usize>().ok().map(|i| (root, Some(i))))
        .unwrap_or((name, None));
    if index.is_none() {
        if let Ok(value) = data.find::<f64, _>(root) {
            return Some(Box::new(move || vec![value.get()]));
        }
        if let Ok(value) = data.find::<f32, _>(root) {
            return Some(Box::new(move || vec![f64::from(value.get())]));
        }
        if let Ok(value) = data.find::<i32, _>(root) {
            return Some(Box::new(move || vec![f64::from(value.get())]));
        }
    }
    if let Ok(value) = data.find::<[f32], _>(root) {
        return Some(Box::new(move || {
            select_values(value.as_vec().into_iter().map(f64::from).collect(), index)
        }));
    }
    if let Ok(value) = data.find::<[i32], _>(root) {
        return Some(Box::new(move || {
            select_values(value.as_vec().into_iter().map(f64::from).collect(), index)
        }));
    }
    None
}
fn select_values(values: Vec<f64>, index: Option<usize>) -> Vec<f64> {
    match index {
        Some(index) => values.get(index).copied().into_iter().collect(),
        None => values,
    }
}

/// Resolved datarefs for one profile aspect; missing refs read as absent.
pub struct Panel {
    /// Active profile, if the aircraft matched one.
    pub profile: Option<AircraftProfile>,
    /// Attendant call-light refs.
    pub attendant: Vec<Readings>,
    /// Ground call-light refs.
    pub ground: Vec<Readings>,
    /// Battery volts gauge.
    pub bat_volts: Option<Readings>,
    /// Battery switch refs.
    pub battery: Vec<Readings>,
    /// Ground power refs.
    pub gpu: Vec<Readings>,
    /// APU generator refs.
    pub apu: Vec<Readings>,
    /// Radio management panel refs.
    pub rmp: Vec<Readings>,
    /// Avionics switch refs.
    pub avionics: Vec<Readings>,
    /// Autopilot master mode (off=0, FD=1, on=2).
    pub ap_mode: Option<DataRef<i32, ReadOnly>>,
    /// Target altitude dial, writable when the sim allows.
    pub ap_altitude: Option<DataRef<f32, xplane::data::ReadWrite>>,
    /// Resolved profile path for diagnostics.
    pub profile_path: String,
    /// Last aircraft path seen.
    pub aircraft_path: String,
}

impl Panel {
    /// Empty panel with no profile.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: None,
            attendant: Vec::new(),
            ground: Vec::new(),
            bat_volts: None,
            battery: Vec::new(),
            gpu: Vec::new(),
            apu: Vec::new(),
            rmp: Vec::new(),
            avionics: Vec::new(),
            ap_mode: None,
            ap_altitude: None,
            profile_path: String::new(),
            aircraft_path: String::new(),
        }
    }

    /// Re-resolve everything when the aircraft changes. Returns log lines.
    pub fn refresh(
        &mut self,
        data: &mut xplane::data::DataApi,
        aircraft_path: &str,
        aircraft_dir: &std::path::Path,
        attendant_override: &str,
        ground_override: &str,
    ) -> Vec<String> {
        let mut log = Vec::new();
        if aircraft_path == self.aircraft_path {
            return log;
        }
        *self = Self::new();
        aircraft_path.clone_into(&mut self.aircraft_path);
        if aircraft_path.is_empty() {
            return log;
        }
        // Stock dials first; a matched profile overrides below.
        self.resolve_autopilot(
            data,
            &openatc_core::profiles::AircraftAutopilot::default(),
            &mut log,
        );
        let identity = read_acf_identity(std::path::Path::new(aircraft_path));
        let Ok((author, icao)) = identity else {
            log.push("OpenATC AI: aircraft identity unreadable, profile matching off".to_owned());
            return log;
        };
        if let Some(path) = find_profile(aircraft_path, aircraft_dir, &author, &icao) {
            match load_aircraft_profile(std::path::Path::new(&path)) {
                Ok(profile) => {
                    self.profile_path = path;
                    self.profile = Some(profile);
                }
                Err(_) => return log,
            }
        } else {
            self.resolve_autopilot(
                data,
                &openatc_core::profiles::AircraftAutopilot::default(),
                &mut log,
            );
            return log;
        }
        let profile = self.profile.clone().unwrap_or_default();
        self.resolve_autopilot(data, &profile.autopilot, &mut log);
        log.push(format!(
            "OpenATC AI: aircraft profile {} active",
            profile.name
        ));
        let attendant_names = if attendant_override.is_empty() {
            profile.comms.attendant_refs.clone()
        } else {
            vec![attendant_override.to_owned()]
        };
        let ground_names = if ground_override.is_empty() {
            profile.comms.ground_refs.clone()
        } else {
            vec![ground_override.to_owned()]
        };
        for name in &attendant_names {
            match find_readings(data, name) {
                Some(found) => {
                    self.attendant.push(found);
                    log.push(format!("OpenATC AI: attendant ref {name} found"));
                }
                None => log.push(format!("OpenATC AI: attendant ref {name} MISSING")),
            }
        }
        for name in &ground_names {
            match find_readings(data, name) {
                Some(found) => {
                    self.ground.push(found);
                    log.push(format!("OpenATC AI: ground ref {name} found"));
                }
                None => log.push(format!("OpenATC AI: ground ref {name} MISSING")),
            }
        }
        let electrical = &profile.electrical;
        if !electrical.bat_volts_ref.is_empty() {
            self.bat_volts = find_readings(data, &electrical.bat_volts_ref);
        }
        for (names, target) in [
            (&electrical.battery_refs, &mut self.battery),
            (&electrical.gpu_refs, &mut self.gpu),
            (&electrical.apu_refs, &mut self.apu),
            (&electrical.rmp_refs, &mut self.rmp),
            (&electrical.avionics_refs, &mut self.avionics),
        ] {
            for name in names {
                if let Some(found) = find_readings(data, name) {
                    target.push(found);
                    log.push(format!("OpenATC AI: power ref {name} found"));
                } else {
                    log.push(format!("OpenATC AI: power ref {name} MISSING"));
                }
            }
        }
        log
    }

    /// Resolve autopilot target refs from a mapping, logging what sticks.
    /// Missing or read-only dials simply stay unresolved; the copilot then
    /// advises instead of writing.
    fn resolve_autopilot(
        &mut self,
        data: &mut xplane::data::DataApi,
        mapping: &openatc_core::profiles::AircraftAutopilot,
        log: &mut Vec<String>,
    ) {
        self.ap_mode = data.find::<i32, _>(&mapping.mode_ref).ok();
        self.ap_altitude = data
            .find::<f32, _>(&mapping.altitude_ref)
            .ok()
            .and_then(|found| found.writeable().ok());
        if self.ap_mode.is_none() {
            log.push("OpenATC AI: autopilot mode ref MISSING".to_owned());
        }
        if self.ap_altitude.is_none() {
            log.push("OpenATC AI: altitude dial unwritable or MISSING".to_owned());
        }
    }

    /// Call-light role: ground wins, then attendant, else ATC.
    #[must_use]
    pub fn role(&self) -> String {
        if self
            .ground
            .iter()
            .any(|value| value().iter().any(|v| *v > 0.5))
        {
            return "ground".to_owned();
        }
        if self
            .attendant
            .iter()
            .any(|value| value().iter().any(|v| *v > 0.5))
        {
            return "cabin".to_owned();
        }
        String::new()
    }

    /// Radio and bus power from the resolved refs.
    #[must_use]
    pub fn power(&self) -> (bool, bool) {
        let Some(profile) = &self.profile else {
            return (true, true);
        };
        if !profile_has_power_sources(profile) {
            return (true, true);
        }
        let mut input = PowerInput::default();
        if let Some(volts) = &self.bat_volts {
            input.bat_volts = volts()
                .into_iter()
                .filter(|v| v.is_finite())
                .reduce(f64::max)
                .unwrap_or(0.0);
            input.has_volts = true;
        }
        let read = |refs: &[Readings]| {
            refs.iter()
                .flat_map(|read| read())
                .map(|value| i32::from(value > 0.5))
                .collect::<Vec<_>>()
        };
        // PowerInput carries volts plus per-source levels; map the int
        // readings across.
        input.battery = read(&self.battery);
        input.gpu = read(&self.gpu);
        input.apu = read(&self.apu);
        input.rmp = read(&self.rmp);
        input.avionics = read(&self.avionics);
        let state = evaluate_power(&input, profile.electrical.min_volts);
        (state.radio, state.bus)
    }
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

/// Find the profile beside the .acf first, then the bundled aircraft dir.
fn find_profile(
    acf_path: &str,
    aircraft_dir: &std::path::Path,
    author: &str,
    icao: &str,
) -> Option<String> {
    let beside = std::path::Path::new(acf_path)
        .parent()
        .map(|dir| dir.join("openatc.toml"));
    if let Some(path) = beside
        && path.is_file()
        && load_aircraft_profile(&path)
            .is_ok_and(|profile| aircraft_matches(&profile, author, icao))
    {
        return Some(path.to_string_lossy().into_owned());
    }
    let entries = std::fs::read_dir(aircraft_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        if load_aircraft_profile(&path)
            .is_ok_and(|profile| aircraft_matches(&profile, author, icao))
        {
            return Some(path.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(test)]
mod reading_tests {
    use super::*;
    #[test]
    fn indexed_array_readings_do_not_use_the_other_radio() {
        assert_eq!(select_values(vec![0.0, 1.0], Some(0)), vec![0.0]);
        assert_eq!(select_values(vec![0.0, 1.0], Some(1)), vec![1.0]);
        assert!(select_values(vec![0.0, 1.0], Some(3)).is_empty());
    }
}
