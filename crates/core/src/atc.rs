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
                    let khz = if key == "FREQ" { number * 10 } else { number };
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
        .filter(|p| !p.contains("/Global Scenery/") && !p.contains("/Resources/default scenery/"))
        .map(|p| p.replace("/apt.dat", "/atc.dat"))
        .collect();
    let custom = format!("{root}/Custom Data/1200 atc data/Earth nav data/atc.dat");
    paths.push(if Path::new(&custom).is_file() {
        custom
    } else {
        format!("{root}/Resources/default scenery/1200 atc data/Earth nav data/atc.dat")
    });
    let mut seen = BTreeSet::new();
    for path in paths {
        if !Path::new(&path).is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
        for c in controllers(&text) {
            if c.frequencies.is_empty() || !seen.insert((c.id.clone(), c.service.clone())) {
                continue;
            }
            let base = stations.iter().find(|s| s.airport == c.airport).cloned();
            // Tower navdata overrides the apt.dat Tower list, as in X-Plane.
            if c.service == "Tower" && base.is_some() {
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
            station.name = format!("{} {}", c.name, c.service);
            station.service.clone_from(&c.service);
            station.services.clear();
            station.coverage = c.volumes;
            for khz in c.frequencies {
                if let Some(existing) = stations.iter_mut().find(|s| {
                    s.airport == station.airport && s.service == station.service && s.khz == khz
                }) {
                    existing.name.clone_from(&station.name);
                    existing.coverage.clone_from(&station.coverage);
                } else {
                    station.khz = khz;
                    stations.push(station.clone());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{state::Telemetry, stations::nearby};
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
