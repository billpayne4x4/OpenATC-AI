//! X-Plane controller data: enabled scenery overrides custom/default navdata.
use crate::stations::{Coverage, Station, scenery_paths};
use std::{collections::BTreeSet, path::Path};

#[derive(Default)]
struct Controller {
    name: String,
    id: String,
    airport: String,
    service: String,
    frequencies: Vec<i32>,
    volumes: Vec<Coverage>,
}
fn controllers(text: &str) -> Vec<Controller> {
    let mut result = Vec::new();
    let mut controller = Controller::default();
    let mut volume = Coverage::default();
    for line in text.lines() {
        let line = line.trim_start();
        let mut fields = line.split_whitespace();
        let Some(key) = fields.next() else { continue };
        let value = line[key.len()..].trim();
        match key {
            "CONTROLLER" => controller = Controller::default(),
            "NAME" => value.clone_into(&mut controller.name),
            "FACILITY_ID" => value.clone_into(&mut controller.id),
            "ICAO" => value.clone_into(&mut controller.airport),
            "ROLE" => {
                match value {
                    "del" => "Clearance",
                    "gnd" => "Ground",
                    "twr" => "Tower",
                    "tracon" => "Approach",
                    "ctr" => "Center",
                    _ => "",
                }
                .clone_into(&mut controller.service);
            }
            "FREQ" | "CHAN" => {
                if let Ok(number) = value.parse::<i32>() {
                    let khz = if key == "FREQ" {
                        crate::stations::legacy_com_khz(number)
                    } else {
                        number
                    };
                    if (118_000..=136_990).contains(&khz) {
                        controller.frequencies.push(khz);
                    }
                }
            }
            "AIRSPACE_POLYGON_BEGIN" => {
                let values: Vec<f64> = fields.filter_map(|s| s.parse().ok()).collect();
                volume = Coverage::default();
                if values.len() == 2 {
                    volume.floor = values[0];
                    volume.ceiling = values[1];
                }
            }
            "POINT" => {
                let values: Vec<f64> = fields.filter_map(|s| s.parse().ok()).collect();
                if values.len() == 2 && values.iter().all(|v| v.is_finite()) {
                    volume.points.push([values[0], values[1]]);
                }
            }
            "AIRSPACE_POLYGON_END" => {
                if volume.points.len() >= 3 {
                    controller.volumes.push(std::mem::take(&mut volume));
                }
            }
            "CONTROLLER_END" if !controller.id.is_empty() && !controller.service.is_empty() => {
                result.push(std::mem::take(&mut controller));
            }
            _ => {}
        }
    }
    result
}
/// Overlay published controller frequencies without inventing airport services.
pub fn extend_index(root: &str, stations: &mut Vec<Station>) -> Result<(), String> {
    let mut paths: Vec<String> = scenery_paths(root)
        .into_iter()
        .filter(|p| !p.contains("Global Scenery") && !p.contains("default scenery"))
        .map(|p| {
            Path::new(&p)
                .with_file_name("atc.dat")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let root = crate::stations::simulator_root(root);
    let custom = root
        .join("Custom Data")
        .join("1200 atc data")
        .join("Earth nav data")
        .join("atc.dat");
    let fallback = root
        .join("Resources")
        .join("default scenery")
        .join("1200 atc data")
        .join("Earth nav data")
        .join("atc.dat");
    paths.push(
        if custom.is_file() { custom } else { fallback }
            .to_string_lossy()
            .into_owned(),
    );
    let mut seen = BTreeSet::new();
    for path in paths {
        if !Path::new(&path).is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
        let mut file_facilities = BTreeSet::new();
        let mut replaced_towers = BTreeSet::new();
        for c in controllers(&text) {
            let facility = (c.id.clone(), c.service.clone());
            if c.frequencies.is_empty() || seen.contains(&facility) {
                continue;
            }
            // One navdata file can give a facility separate lower and upper sectors.
            // Scenery priority replaces the facility only between files, not between its sectors.
            file_facilities.insert(facility);
            let base = stations.iter().find(|s| s.airport == c.airport).cloned();
            // Tower navdata overrides the apt.dat Tower list, as in X-Plane.
            if c.service == "Tower" && base.is_some() && replaced_towers.insert(c.airport.clone()) {
                stations.retain(|s| s.airport != c.airport || s.service != "Tower");
            }
            let mut station = base.unwrap_or_default();
            if station.airport.is_empty() {
                station.airport.clone_from(&c.id);
                station.airport_name.clone_from(&c.name);
                let points: Vec<_> = c.volumes.iter().flat_map(|v| &v.points).collect();
                if points.is_empty() {
                    continue;
                }
                station.latitude = points.iter().map(|p| p[0]).sum::<f64>()
                    / f64::from(u32::try_from(points.len()).unwrap_or(u32::MAX));
                station.longitude = points.iter().map(|p| p[1]).sum::<f64>()
                    / f64::from(u32::try_from(points.len()).unwrap_or(u32::MAX));
            }
            station.name = if c
                .name
                .trim()
                .to_ascii_lowercase()
                .ends_with(&c.service.to_ascii_lowercase())
            {
                c.name.trim().to_owned()
            } else {
                format!("{} {}", c.name.trim(), c.service)
            };
            station.service.clone_from(&c.service);
            station.services.clear();
            station.coverage = c.volumes;
            for khz in c.frequencies {
                if let Some(existing) = stations.iter_mut().find(|s| {
                    s.airport == station.airport
                        && s.service == station.service
                        && s.khz == khz
                        && s.name == station.name
                }) {
                    existing.name.clone_from(&station.name);
                    for volume in &station.coverage {
                        if !existing.coverage.contains(volume) {
                            existing.coverage.push(volume.clone());
                        }
                    }
                } else {
                    station.khz = khz;
                    stations.push(station.clone());
                }
            }
        }
        seen.extend(file_facilities);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{state::Telemetry, stations::nearby};
    #[test]
    fn sibling_sectors_survive_without_reviving_lower_priority_facilities() {
        let root = std::env::temp_dir().join(format!("openatc-sectors-{}", std::process::id()));
        let custom = root.join("Custom Scenery/Local/Earth nav data/atc.dat");
        let master = root.join("Custom Data/1200 atc data/Earth nav data/atc.dat");
        std::fs::create_dir_all(custom.parent().unwrap()).unwrap();
        std::fs::create_dir_all(master.parent().unwrap()).unwrap();
        std::fs::write(
            root.join("Custom Scenery/scenery_packs.ini"),
            "SCENERY_PACK Custom Scenery/Local/\n",
        )
        .unwrap();
        let sector = |name: &str, floor: i32, ceiling: i32| {
            format!(
                "CONTROLLER\nNAME {name}\nFACILITY_ID TEST\nROLE ctr\nCHAN 118030\nAIRSPACE_POLYGON_BEGIN {floor} {ceiling}\nPOINT -1 -1\nPOINT -1 1\nPOINT 1 1\nPOINT 1 -1\nAIRSPACE_POLYGON_END\nCONTROLLER_END\n"
            )
        };
        std::fs::write(
            &custom,
            format!(
                "{}{}",
                sector("Lower", 0, 19500),
                sector("Upper", 19500, 60000)
            ),
        )
        .unwrap();
        std::fs::write(&master, sector("Obsolete", 0, 60000)).unwrap();
        let mut stations = Vec::new();
        extend_index(root.to_str().unwrap(), &mut stations).unwrap();
        assert_eq!(stations.len(), 2);
        for (height, expected) in [(10000.0, "Lower Center"), (35000.0, "Upper Center")] {
            let observed = Telemetry {
                latitude: 0.0,
                longitude: 0.0,
                altitude_feet: height,
                position_valid: true,
                ..Default::default()
            };
            let covered = stations
                .iter()
                .filter(|station| {
                    station
                        .coverage
                        .iter()
                        .any(|volume| volume.contains(&observed))
                })
                .collect::<Vec<_>>();
            assert_eq!(covered.len(), 1);
            assert_eq!(covered[0].name, expected);
            assert_eq!(covered[0].khz, 118_030);
        }
        assert!(
            stations
                .iter()
                .all(|station| !station.name.contains("Obsolete"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_frequencies_restore_the_omitted_digit_without_changing_channels() {
        let parsed = controllers(
            "CONTROLLER\nNAME LOS ANGELES\nFACILITY_ID KZLA\nROLE ctr\nFREQ 11802\nFREQ 12527\nFREQ 12572\nFREQ 13697\nCHAN 118020\nCHAN 118030\nCONTROLLER_END\n",
        );
        assert_eq!(
            parsed[0].frequencies,
            [118_025, 125_275, 125_725, 136_975, 118_020, 118_030]
        );
        for record in [54, 1054] {
            let input = format!(
                "1 100 0 0 TEST Test airport\n100 45 1 0 0 1 1 0 25L 33.93 -118.38 0 0 1 0 0 0 07R 33.93 -118.42 0 0 1 0 0 0\n{record} {} Test Tower\n99\n",
                if record == 54 { 11802 } else { 118_020 }
            );
            let airport = crate::airport::load_airport(&input, "TEST").unwrap();
            assert_eq!(
                airport.frequencies[0].khz,
                if record == 54 { 118_025 } else { 118_020 }
            );
        }
    }
    #[test]
    fn custom_controller_overrides_tower_and_preserves_ground() {
        let root = std::env::temp_dir().join(format!("openatc-atc-{}", std::process::id()));
        let file = root.join("Custom Data/1200 atc data/Earth nav data/atc.dat");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "CONTROLLER\nNAME UDON\nFACILITY_ID VTUD\nICAO VTUD\nROLE twr\nFREQ 11945\nCHAN 122500\nCONTROLLER_END\nCONTROLLER\nNAME REGION\nFACILITY_ID REG\nROLE ctr\nFREQ 12330\nAIRSPACE_POLYGON_BEGIN 0 60000\nPOINT 16 101\nPOINT 19 101\nPOINT 19 104\nPOINT 16 104\nAIRSPACE_POLYGON_END\nCONTROLLER_END\n").unwrap();
        let mut stations = vec![
            Station {
                airport: "VTUD".into(),
                service: "Tower".into(),
                khz: 118_100,
                latitude: 17.38,
                longitude: 102.78,
                ..Station::default()
            },
            Station {
                airport: "VTUD".into(),
                service: "Ground".into(),
                khz: 121_900,
                latitude: 17.38,
                longitude: 102.78,
                ..Station::default()
            },
        ];
        extend_index(root.to_str().unwrap(), &mut stations).unwrap();
        assert!(!stations.iter().any(|s| s.khz == 118_100));
        assert!(stations.iter().any(|s| s.khz == 119_450));
        assert!(stations.iter().any(|s| s.khz == 122_500));
        assert!(stations.iter().any(|s| s.khz == 121_900));
        let t = Telemetry {
            latitude: 17.99,
            longitude: 102.56,
            altitude_feet: 600.,
            position_valid: true,
            ..Telemetry::default()
        };
        assert!(nearby(&stations, &t).iter().any(|s| s.service == "Center"));
        let outside = Telemetry {
            longitude: 120.,
            ..t
        };
        assert!(
            !nearby(&stations, &outside)
                .iter()
                .any(|s| s.service == "Center")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
