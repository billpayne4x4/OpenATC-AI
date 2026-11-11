//! Editable wake-turbulence and runway-relationship policy.
//!
#![allow(missing_docs)]
//!
//! The data lives in `traffic/wake/` so a standard can be changed without rebuilding.
//! `runways.toml` holds the shared runway relationships and the other files each hold
//! one wake standard. The single `traffic/wake.toml` remains the bundled fallback for
//! first start and for installations that still use one file.
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::Path,
    sync::{OnceLock, RwLock},
};

#[derive(Clone, Debug, Deserialize)]
pub struct File {
    pub version: u32,
    pub runways: RunwayRules,
    pub regions: BTreeMap<String, Region>,
}

#[derive(Clone, Debug, Deserialize)]
struct RunwayFile {
    version: u32,
    runways: RunwayRules,
}

#[derive(Clone, Debug, Deserialize)]
struct StandardFile {
    name: String,
    #[serde(flatten)]
    region: Region,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct RunwayRules {
    pub parallel_independent_m: f64,
    pub parallel_wake_m: f64,
    pub crossing_degrees: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Region {
    pub label: String,
    #[serde(default)]
    pub prefixes: Vec<String>,
    /// `cwt` reads the aircraft CWT letter; `wake` reads the wake letter or simulator category.
    pub category: String,
    pub categories: Vec<String>,
    pub lua_w_small_behind_heavy: bool,
    /// Region override for wake-related parallel runways.
    #[serde(default)]
    pub parallel_wake_m: Option<f64>,
    /// Region override for independent parallel operations.
    #[serde(default)]
    pub parallel_independent_m: Option<f64>,
    #[serde(default = "default_lua_w_seconds")]
    pub max_lua_w_seconds: u16,
    /// Rows are leader categories, columns follower categories, values nautical miles.
    pub arrival: Vec<Vec<f64>>,
    /// Rows are leader categories, columns follower categories,
    /// each cell `[full_length, intersection, opposite_direction]` seconds.
    pub departure: Vec<Vec<[u16; 3]>>,
    /// Radar lateral and vertical minima for airports in this region.
    #[serde(default)]
    pub separation: Option<Separation>,
}

/// Radar minima for one region. Every value carries the source it was taken from, so a
/// correction can point at the document it came from.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Separation {
    /// Lateral minimum in en route airspace, nautical miles.
    pub enroute_lateral_nm: f64,
    /// Lateral minimum in terminal airspace, nautical miles.
    pub terminal_lateral_nm: f64,
    /// Vertical minimum below FL290, feet.
    pub vertical_below_fl290_feet: f64,
    /// Vertical minimum at FL290 and above, feet.
    pub vertical_fl290_and_above_feet: f64,
    #[serde(default)]
    pub source: String,
    /// `verified` when checked against the published text, `provisional` otherwise.
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "provisional".into()
}

fn default_lua_w_seconds() -> u16 {
    90
}

#[derive(Clone, Debug)]
pub struct Policy {
    pub file: File,
    pub standard: String,
}

impl Policy {
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: File = toml::from_str(text).map_err(|error| error.to_string())?;
        Self::from_file(file)
    }

    /// Validate a policy that has already been parsed from TOML.
    pub fn from_file(file: File) -> Result<Self, String> {
        if file.version != 1 {
            return Err(format!("Unsupported wake policy version {}", file.version));
        }
        if !file.runways.parallel_independent_m.is_finite()
            || !file.runways.parallel_wake_m.is_finite()
            || !(0.0..=180.0).contains(&file.runways.crossing_degrees)
        {
            return Err("Wake policy runway limits are invalid".into());
        }
        for (key, region) in &file.regions {
            let width = region.categories.len();
            if width == 0
                || region.arrival.len() != width
                || region.departure.len() != width
                || region.arrival.iter().any(|row| row.len() != width)
                || region.departure.iter().any(|row| row.len() != width)
            {
                return Err(format!("Wake policy region {key} has inconsistent tables"));
            }
        }
        if !file.regions.contains_key("icao") {
            return Err("Wake policy needs an `icao` fallback region".into());
        }
        for (key, region) in &file.regions {
            let Some(separation) = &region.separation else {
                continue;
            };
            let lateral = |value: f64| (3.0..=10.0).contains(&value);
            let vertical = |value: f64| (1000.0..=5000.0).contains(&value);
            if !lateral(separation.enroute_lateral_nm)
                || !lateral(separation.terminal_lateral_nm)
                || !vertical(separation.vertical_below_fl290_feet)
                || !vertical(separation.vertical_fl290_and_above_feet)
            {
                return Err(format!(
                    "Wake policy region {key} has separation outside 3-10 NM or 1000-5000 ft"
                ));
            }
        }
        Ok(Self {
            file,
            standard: "auto".into(),
        })
    }

    /// Read the split policy folder: `runways.toml` plus one standard per TOML file.
    pub fn from_dir(path: &Path) -> Result<Self, String> {
        let runways_path = path.join("runways.toml");
        let text = std::fs::read_to_string(&runways_path)
            .map_err(|error| format!("{}: {error}", runways_path.display()))?;
        let runways: RunwayFile = toml::from_str(&text)
            .map_err(|error| format!("{}: {error}", runways_path.display()))?;
        let mut paths = std::fs::read_dir(path)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|candidate| candidate.extension() == Some(OsStr::new("toml")))
            .collect::<Vec<_>>();
        paths.sort();
        let mut regions = BTreeMap::new();
        for standard_path in paths {
            if standard_path == runways_path {
                continue;
            }
            let text = std::fs::read_to_string(&standard_path)
                .map_err(|error| format!("{}: {error}", standard_path.display()))?;
            let standard: StandardFile = toml::from_str(&text)
                .map_err(|error| format!("{}: {error}", standard_path.display()))?;
            let name = standard.name.trim().to_owned();
            if name.is_empty() {
                return Err(format!("{} needs a standard name", standard_path.display()));
            }
            if regions.insert(name.clone(), standard.region).is_some() {
                return Err(format!("Wake policy standard `{name}` is defined twice"));
            }
        }
        if regions.is_empty() {
            return Err("Wake policy folder has no standards".into());
        }
        Self::from_file(File {
            version: runways.version,
            runways: runways.runways,
            regions,
        })
    }

    pub fn with_standard(mut self, standard: &str) -> Self {
        self.standard = normalize_standard(standard).to_owned();
        self
    }

    #[must_use]
    pub fn runways(&self) -> RunwayRules {
        self.file.runways
    }

    /// Radar minima for an airport: its region's table, then the `icao` table. `None`
    /// means neither defines one, so the caller keeps its own defaults.
    #[must_use]
    pub fn separation(&self, airport: &str) -> Option<&Separation> {
        self.region(airport).separation.as_ref().or_else(|| {
            self.file
                .regions
                .get("icao")
                .and_then(|region| region.separation.as_ref())
        })
    }

    #[must_use]
    pub fn region(&self, airport: &str) -> &Region {
        let key = if self.standard == "auto" {
            self.file
                .regions
                .iter()
                .filter(|(_, region)| {
                    region
                        .prefixes
                        .iter()
                        .any(|prefix| airport.starts_with(prefix))
                })
                .max_by_key(|(_, region)| {
                    region
                        .prefixes
                        .iter()
                        .filter(|prefix| airport.starts_with(prefix.as_str()))
                        .map(String::len)
                        .max()
                        .unwrap_or(0)
                })
                .map_or("icao", |(key, _)| key.as_str())
        } else {
            match self.standard.as_str() {
                "faa_cwt" => "faa",
                "mats" => "mats",
                "recat_eu" => "recat_eu",
                _ => "icao",
            }
        };
        self.file
            .regions
            .get(key)
            .or_else(|| self.file.regions.get("icao"))
            .expect("validated icao region")
    }

    #[must_use]
    pub fn category(&self, region: &Region, aircraft: &str, sim_wake: Option<u8>) -> Option<char> {
        if region.category == "cwt" {
            return super::profiles::aircraft(aircraft)
                .and_then(|profile| profile.cwt)
                .or_else(|| {
                    sim_wake.map(|value| match value {
                        0 => 'I',
                        1 => 'F',
                        2 => 'B',
                        3 => 'A',
                        _ => 'I',
                    })
                });
        }
        sim_wake
            .and_then(sim_category)
            .or_else(|| super::profiles::aircraft(aircraft).map(|profile| profile.wake))
    }

    #[must_use]
    pub fn arrival(
        &self,
        airport: &str,
        leader: &str,
        leader_wake: Option<u8>,
        follower: &str,
        follower_wake: Option<u8>,
    ) -> Option<f64> {
        let region = self.region(airport);
        let leader = self.category(region, leader, leader_wake)?;
        let follower = self.category(region, follower, follower_wake)?;
        let row = region
            .categories
            .iter()
            .position(|category| category.len() == 1 && category.starts_with(leader))?;
        let column = region
            .categories
            .iter()
            .position(|category| category.len() == 1 && category.starts_with(follower))?;
        region.arrival.get(row)?.get(column).copied()
    }

    #[must_use]
    pub fn departure(
        &self,
        airport: &str,
        leader: &str,
        leader_wake: Option<u8>,
        follower: &str,
        follower_wake: Option<u8>,
        kind: usize,
    ) -> Option<u16> {
        let region = self.region(airport);
        let leader = self.category(region, leader, leader_wake)?;
        let follower = self.category(region, follower, follower_wake)?;
        let row = region
            .categories
            .iter()
            .position(|category| category.len() == 1 && category.starts_with(leader))?;
        let column = region
            .categories
            .iter()
            .position(|category| category.len() == 1 && category.starts_with(follower))?;
        region
            .departure
            .get(row)?
            .get(column)
            .map(|cell| cell[kind.clamp(0, 2)])
    }

    #[must_use]
    pub fn lua_w_allowed(
        &self,
        airport: &str,
        leader: &str,
        leader_wake: Option<u8>,
        follower: &str,
        follower_wake: Option<u8>,
    ) -> bool {
        let region = self.region(airport);
        if region.lua_w_small_behind_heavy {
            return true;
        }
        let leader = self.category(region, leader, leader_wake);
        let follower = self.category(region, follower, follower_wake);
        !(matches!(leader, Some('A' | 'B' | 'C' | 'H' | 'J'))
            && matches!(follower, Some('G' | 'H' | 'I' | 'L')))
    }

    #[must_use]
    pub fn max_lua_w_seconds(&self, airport: &str) -> u16 {
        self.region(airport).max_lua_w_seconds
    }

    #[must_use]
    pub fn parallel_wake_m(&self, airport: &str) -> f64 {
        self.region(airport)
            .parallel_wake_m
            .unwrap_or(self.file.runways.parallel_wake_m)
    }

    #[must_use]
    pub fn parallel_independent_m(&self, airport: &str) -> f64 {
        self.region(airport)
            .parallel_independent_m
            .unwrap_or(self.file.runways.parallel_independent_m)
    }
}

fn sim_category(value: u8) -> Option<char> {
    Some(match value {
        0 => 'L',
        1 => 'M',
        2 => 'H',
        3 => 'J',
        _ => return None,
    })
}

fn normalize_standard(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "faa" | "faa_cwt" | "cwt" => "faa_cwt",
        "mats" | "australia" => "mats",
        "recat" | "recat_eu" | "eurocontrol" => "recat_eu",
        "icao" | "intl" => "icao",
        _ => "auto",
    }
}

static POLICY: OnceLock<RwLock<Policy>> = OnceLock::new();

fn global() -> &'static RwLock<Policy> {
    POLICY.get_or_init(|| {
        let policy = Policy::parse(include_str!("../../../../traffic/wake.toml"))
            .expect("bundled wake policy must parse");
        RwLock::new(policy)
    })
}

pub fn load(text: &str) -> Result<(), String> {
    install(Policy::parse(text)?)
}

/// Load a split policy folder and keep the selected standard.
pub fn load_dir(path: &Path) -> Result<(), String> {
    install(Policy::from_dir(path)?)
}

fn install(policy: Policy) -> Result<(), String> {
    let standard = global()
        .read()
        .map_or("auto".into(), |current| current.standard.clone());
    *global()
        .write()
        .map_err(|_| "Wake policy lock poisoned".to_owned())? = policy.with_standard(&standard);
    Ok(())
}

pub fn set_standard(value: &str) {
    if let Ok(mut policy) = global().write() {
        policy.standard = normalize_standard(value).into();
    }
}

/// Borrow the active policy without cloning its tables.
pub fn read<R>(apply: impl FnOnce(&Policy) -> R) -> R {
    let guard = global()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    apply(&guard)
}

#[must_use]
pub fn current() -> Policy {
    global().read().map_or_else(
        |_| Policy::parse(include_str!("../../../../traffic/wake.toml")).unwrap(),
        |policy| policy.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU32, Ordering},
    };

    fn folder() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../traffic/wake")
    }

    struct TempFolder(PathBuf);

    impl TempFolder {
        fn new(tag: &str) -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "openatc-wake-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, name: &str, text: &str) {
            std::fs::write(self.0.join(name), text).unwrap();
        }
    }

    impl Drop for TempFolder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const RUNWAYS: &str = "version = 1\n[runways]\nparallel_independent_m = 760.0\nparallel_wake_m = 760.0\ncrossing_degrees = 12.0\n";

    fn standard(name: &str) -> String {
        format!(
            "name = \"{name}\"\nlabel = \"{name}\"\ncategory = \"wake\"\ncategories = [\"L\"]\nlua_w_small_behind_heavy = false\narrival = [[0]]\ndeparture = [[[0, 0, 0]]]\n"
        )
    }

    #[test]
    fn the_split_folder_loads_every_standard() {
        let policy = Policy::from_dir(&folder()).expect("split policy");
        for key in ["faa", "icao", "recat_eu", "mats"] {
            assert!(policy.file.regions.contains_key(key), "{key}");
        }
        assert_eq!(policy.runways().crossing_degrees, 12.0);
    }

    #[test]
    fn the_split_folder_matches_the_bundled_single_file() {
        let split = Policy::from_dir(&folder()).expect("split policy");
        let bundled =
            Policy::parse(include_str!("../../../../traffic/wake.toml")).expect("bundled policy");
        assert_eq!(split.file.regions.len(), bundled.file.regions.len());
        assert_eq!(
            split.file.runways.parallel_independent_m,
            bundled.file.runways.parallel_independent_m
        );
        assert_eq!(
            split.file.runways.parallel_wake_m,
            bundled.file.runways.parallel_wake_m
        );
        assert_eq!(
            split.file.runways.crossing_degrees,
            bundled.file.runways.crossing_degrees
        );
        for (key, region) in &bundled.file.regions {
            let split = split
                .file
                .regions
                .get(key)
                .unwrap_or_else(|| panic!("missing standard {key}"));
            assert_eq!(split.label, region.label, "{key}");
            assert_eq!(split.prefixes, region.prefixes, "{key}");
            assert_eq!(split.category, region.category, "{key}");
            assert_eq!(split.categories, region.categories, "{key}");
            assert_eq!(split.arrival, region.arrival, "{key}");
            assert_eq!(split.departure, region.departure, "{key}");
            assert_eq!(split.max_lua_w_seconds, region.max_lua_w_seconds, "{key}");
            assert_eq!(split.parallel_wake_m, region.parallel_wake_m, "{key}");
            assert_eq!(
                split.parallel_independent_m, region.parallel_independent_m,
                "{key}"
            );
        }
    }

    #[test]
    fn duplicate_standards_are_rejected() {
        let folder = TempFolder::new("duplicate");
        folder.write("runways.toml", RUNWAYS);
        folder.write("first.toml", &standard("icao"));
        folder.write("second.toml", &standard("icao"));
        let error = Policy::from_dir(&folder.0).expect_err("duplicate standard");
        assert!(error.contains("defined twice"), "{error}");
    }

    #[test]
    fn separation_comes_from_the_region_then_the_icao_table() {
        let policy =
            Policy::parse(include_str!("../../../../traffic/wake.toml")).expect("bundled policy");
        let australia = policy.separation("YSSY").expect("Australian radar minima");
        assert_eq!(australia.terminal_lateral_nm, 3.0);
        assert_eq!(australia.vertical_below_fl290_feet, 1000.0);
        assert_eq!(australia.status, "verified");
        assert!(
            policy.separation("KLAX").is_none(),
            "a region without a table, and no icao table, keeps the operations defaults"
        );
    }

    #[test]
    fn separation_outside_the_supported_range_is_rejected() {
        let text = include_str!("../../../../traffic/wake.toml")
            .replace("enroute_lateral_nm = 5.0", "enroute_lateral_nm = 1.0");
        assert!(Policy::parse(&text).is_err());
    }

    #[test]
    fn a_missing_icao_fallback_is_rejected() {
        let folder = TempFolder::new("fallback");
        folder.write("runways.toml", RUNWAYS);
        folder.write("faa.toml", &standard("faa"));
        let error = Policy::from_dir(&folder.0).expect_err("missing icao");
        assert!(error.contains("icao"), "{error}");
    }

    #[test]
    fn a_missing_runway_file_is_rejected() {
        let folder = TempFolder::new("runways");
        let error = Policy::from_dir(&folder.0).expect_err("missing runways");
        assert!(error.contains("runways.toml"), "{error}");
    }

    #[test]
    fn an_inconsistent_matrix_is_rejected() {
        let text = "version = 1\n[runways]\nparallel_independent_m = 760.0\nparallel_wake_m = 760.0\ncrossing_degrees = 12.0\n[regions.icao]\nlabel = \"ICAO\"\ncategory = \"wake\"\ncategories = [\"J\", \"H\"]\nlua_w_small_behind_heavy = true\narrival = [[0, 4], [0, 0]]\ndeparture = [[[0, 0, 0], [120, 180, 180]]]\n";
        let error = Policy::parse(text).expect_err("short departure matrix");
        assert!(error.contains("inconsistent tables"), "{error}");
    }
}
