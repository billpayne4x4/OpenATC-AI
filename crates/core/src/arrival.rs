//! Destination-anchored vertical profile geometry and installed arrival data.
use crate::airport::{Airport, airport_point};
use crate::state::{Point, Telemetry};
use serde::{Deserialize, Serialize};

/// Arrival airport data, separate from local radio services and airport search.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ArrivalAirport {
    /// Installed airport geometry.
    pub airport: Airport,
    /// Regional transition altitude reference, not the QNH-dependent transition level.
    pub transition_feet: i32,
    /// Published threshold elevations from CIFP.
    pub thresholds: Vec<Threshold>,
    /// Initial final-segment altitude for installed ILS procedures.
    pub intercepts: Vec<Intercept>,
}
/// Runway threshold metadata.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Threshold {
    /// Runway identifier.
    pub runway: String,
    /// Feet MSL.
    pub elevation_feet: f64,
}
/// An installed ILS final-segment entry altitude.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Intercept {
    /// Procedure identifier.
    pub procedure: String,
    /// Feet MSL.
    pub altitude_feet: f64,
}
impl ArrivalAirport {
    /// Read the narrowly used CIFP runway and initial-final-segment fields.
    pub fn read_cifp(&mut self, text: &str) {
        for line in text.lines() {
            let Some((kind, body)) = line.split_once(':') else {
                continue;
            };
            let fields: Vec<_> = body.split(';').next().unwrap_or("").split(',').collect();
            if kind == "RWY" && fields.len() >= 4 {
                if let Ok(elevation) = fields[3].trim().parse::<f64>() {
                    self.thresholds.push(Threshold {
                        runway: fields[0].trim().trim_start_matches("RW").into(),
                        elevation_feet: elevation,
                    });
                }
            } else if kind == "APPCH"
                && fields.len() >= 25
                && fields[1].trim() == "I"
                && fields[3].trim().is_empty()
                && fields[11].trim() == "IF"
            {
                let code = fields[22].trim();
                // J: altitude 1 is the glide-slope intercept altitude. Only
                // exact/GS-intercept altitudes are used; altitude limits are not targets.
                if (code == "J" || code.is_empty())
                    && let Ok(altitude) = fields[23].trim().parse::<f64>()
                {
                    self.intercepts.push(Intercept {
                        procedure: fields[2].trim().into(),
                        altitude_feet: altitude,
                    });
                }
            }
        }
    }
}
/// Selected runway and published glide-slope information.
#[derive(Clone, Debug)]
pub struct Target {
    /// Runway name.
    pub runway: String,
    /// Latitude of landing threshold.
    pub latitude: f64,
    /// Longitude of landing threshold.
    pub longitude: f64,
    /// Threshold feet MSL; sampled scenery may refine it.
    pub elevation_feet: f64,
    /// Published glide angle, absent when no GS is installed.
    pub glide_degrees: Option<f64>,
    /// Installed ILS final-segment entry altitude, if supplied.
    pub intercept_feet: Option<f64>,
    /// ILS procedure supplying the entry altitude.
    pub intercept_procedure: String,
}
/// Select the flight plan runway without silently using a different end.
#[must_use]
pub fn target(data: &ArrivalAirport, selected: &str, approach: &str) -> Option<Target> {
    let airport = &data.airport;
    let runway = airport
        .runways
        .iter()
        .find(|r| selected.is_empty() || r.first_name == selected || r.second_name == selected)?;
    let name = if selected.is_empty() {
        &runway.first_name
    } else {
        selected
    };
    let p = runway.landing_threshold(name)?;
    let gs = airport
        .navaids
        .iter()
        .find(|n| n.kind == "GS" && n.runway == name && (1.0..=10.0).contains(&n.glide_angle));
    let entry = data
        .intercepts
        .iter()
        .find(|n| {
            !approach.is_empty()
                && n.procedure == approach
                && n.procedure.strip_prefix('I').is_some_and(|s| {
                    s.strip_prefix(name).is_some_and(|suffix| {
                        suffix.is_empty() || matches!(suffix, "X" | "Y" | "Z")
                    })
                })
        })
        .or_else(|| {
            data.intercepts.iter().find(|n| {
                n.procedure.strip_prefix('I').is_some_and(|s| {
                    s.strip_prefix(name).is_some_and(|suffix| {
                        suffix.is_empty() || matches!(suffix, "Y" | "Z" | "X")
                    })
                })
            })
        });
    Some(Target {
        runway: name.into(),
        latitude: airport.reference_latitude + p.north / 111_320.0,
        longitude: airport.reference_longitude
            + p.east / (111_320.0 * airport.reference_latitude.to_radians().cos()),
        elevation_feet: data
            .thresholds
            .iter()
            .find(|t| t.runway == name)
            .map_or(airport.elevation_feet, |t| t.elevation_feet),
        glide_degrees: gs.map(|n| n.glide_angle),
        intercept_feet: gs.and_then(|_| entry.map(|e| e.altitude_feet)),
        intercept_procedure: entry.map_or_else(String::new, |e| e.procedure.clone()),
    })
}
/// Direct geographic distance in NM, using a spherical Earth.
#[must_use]
pub fn distance_nm(a: [f64; 2], b: [f64; 2]) -> f64 {
    let lat = (b[0] - a[0]).to_radians();
    let lon = (b[1] - a[1]).to_radians();
    let h = (lat / 2.0).sin().powi(2)
        + a[0].to_radians().cos() * b[0].to_radians().cos() * (lon / 2.0).sin().powi(2);
    3_440.065 * 2.0 * h.clamp(0.0, 1.0).sqrt().asin()
}
/// Initial true bearing to the destination.
#[must_use]
pub fn bearing(a: [f64; 2], b: [f64; 2]) -> f64 {
    let (lat1, lat2, lon) = (
        a[0].to_radians(),
        b[0].to_radians(),
        (b[1] - a[1]).to_radians(),
    );
    (lon.sin() * lat2.cos())
        .atan2(lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * lon.cos())
        .to_degrees()
        .rem_euclid(360.0)
}
/// Point at a given distance and true bearing, including beyond the runway.
#[must_use]
pub fn destination(a: [f64; 2], course: f64, nm: f64) -> [f64; 2] {
    let (lat, lon, angle, d) = (
        a[0].to_radians(),
        a[1].to_radians(),
        course.to_radians(),
        nm / 3_440.065,
    );
    let lat2 = (lat.sin() * d.cos() + lat.cos() * d.sin() * angle.cos()).asin();
    let lon2 = lon + (angle.sin() * d.sin() * lat.cos()).atan2(d.cos() - lat.sin() * lat2.sin());
    [
        lat2.to_degrees(),
        (lon2.to_degrees() + 180.0).rem_euclid(360.0) - 180.0,
    ]
}
/// Feet gained per NM along a geometric slope.
#[must_use]
pub fn gradient(degrees: f64) -> f64 {
    degrees.to_radians().tan() * 6_076.115_486
}
/// Smooth destination-anchored planning reference, with a short threshold flare.
#[must_use]
pub fn planned_feet(
    remaining_nm: f64,
    elevation: f64,
    ceiling: f64,
    glide: f64,
    intercept: Option<f64>,
) -> f64 {
    let remaining = remaining_nm.max(0.0);
    let final_altitude =
        elevation + gradient(glide) * remaining + 50.0 * (remaining / 0.15).min(1.0);
    let entry = intercept.filter(|entry| *entry > elevation + 50.0);
    let altitude = if let Some(entry) = entry {
        let intercept_nm = (entry - elevation - 50.0) / gradient(glide);
        if remaining > intercept_nm {
            entry + gradient(3.0) * (remaining - intercept_nm)
        } else {
            final_altitude
        }
    } else {
        final_altitude
    };
    altitude.min(ceiling.max(elevation))
}
/// Current vertical projection in feet per NM of runway closing distance.
/// Suppressed while stationary or flying away, rather than drawing a false landing.
#[must_use]
pub fn current_gradient(telemetry: &Telemetry, target: &Target) -> Option<f64> {
    if !telemetry.position_valid || telemetry.ground_speed_knots < 30.0 || telemetry.on_ground {
        return None;
    }
    let course = bearing(
        [telemetry.latitude, telemetry.longitude],
        [target.latitude, target.longitude],
    );
    let track = telemetry
        .ground_track_degrees
        .unwrap_or(telemetry.heading_degrees);
    let closing = telemetry.ground_speed_knots * (track - course).to_radians().cos();
    (closing >= 30.0 && telemetry.vertical_speed_fpm.is_finite())
        .then_some(telemetry.vertical_speed_fpm * 60.0 / closing)
}
/// One scenery station in the approach corridor; absent samples remain gaps.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TerrainStation {
    /// Distance before the runway (negative after the runway).
    pub remaining_nm: f64,
    /// Centre-line terrain MSL.
    pub ground_feet: Option<f64>,
    /// Highest sampled scenery across the corridor.
    pub corridor_feet: Option<f64>,
}
/// Cached scenery geometry for one arrival.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TerrainProfile {
    /// Arrival airport/runway cache key.
    pub key: String,
    /// Sample stations.
    pub stations: Vec<TerrainStation>,
    /// Scenery data source shown beside the graph.
    pub source: String,
    /// Refresh in progress.
    pub sampling: bool,
}
/// Upper terrain guide with 2,000 ft margin. Never crosses an unknown station.
#[must_use]
pub fn clearance_feet(stations: &[TerrainStation], at: usize) -> Option<f64> {
    let centre = stations.get(at)?;
    let mut highest = centre.corridor_feet?;
    for sample in stations
        .iter()
        .filter(|s| (s.remaining_nm - centre.remaining_nm).abs() <= 1.0)
    {
        highest = highest.max(sample.corridor_feet?);
    }
    Some(highest + 2000.0)
}
/// Horizontal clearance reference above the highest known corridor sample.
/// Unknown scenery remains unknown; this is not a surveyed minimum altitude.
#[must_use]
pub fn clearance_floor(stations: &[TerrainStation]) -> Option<f64> {
    stations
        .iter()
        .filter_map(|s| s.corridor_feet)
        .filter(|v| v.is_finite())
        .reduce(f64::max)
        .map(|highest| highest + 2000.0)
}

/// First intersection of the current straight descent with known scenery.
/// Unknown terrain segments are never bridged.
#[must_use]
pub fn terrain_contact(
    stations: &[TerrainStation],
    runway_distance: f64,
    altitude: f64,
    slope: f64,
    limit: f64,
) -> Option<(f64, f64)> {
    for pair in stations.windows(2) {
        let (Some(first_ground), Some(second_ground)) = (pair[0].ground_feet, pair[1].ground_feet)
        else {
            continue;
        };
        let first_distance = runway_distance - pair[0].remaining_nm;
        let second_distance = runway_distance - pair[1].remaining_nm;
        if second_distance <= first_distance || second_distance < 0.0 || first_distance > limit {
            continue;
        }
        let terrain_slope = (second_ground - first_ground) / (second_distance - first_distance);
        let ground = |at: f64| first_ground + terrain_slope * (at - first_distance);
        let start = first_distance.max(0.0);
        let end = second_distance.min(limit);
        let above_start = altitude + slope * start - ground(start);
        let above_end = altitude + slope * end - ground(end);
        let contact = if above_start <= 0.0 {
            Some(start)
        } else if above_end <= 0.0 {
            Some(start + (end - start) * above_start / (above_start - above_end))
        } else {
            None
        };
        if let Some(distance) = contact {
            return Some((distance, ground(distance)));
        }
    }
    None
}

/// Convert a target into the local frame used by airport maps.
#[must_use]
pub fn target_point(airport: &Airport, target: &Target) -> Point {
    airport_point(airport, target.latitude, target.longitude)
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    #[test]
    fn geometry_reaches_airport_and_handles_longitudes() {
        let a = [-41.0, 147.0];
        let b = [-43.0, 148.0];
        let p = destination(a, bearing(a, b), distance_nm(a, b));
        assert!(distance_nm(p, b) < 0.00001);
        assert!(distance_nm([0.0, 179.9], [0.0, -179.9]) < 13.0);
    }
    #[test]
    fn planned_ends_on_ground_and_passes_ils_entry() {
        let entry = (3000.0 - 548.0 - 50.0) / gradient(3.0);
        assert_eq!(planned_feet(0.0, 548.0, 32000.0, 3.0, Some(3000.0)), 548.0);
        assert!((planned_feet(entry, 548.0, 32000.0, 3.0, Some(3000.0)) - 3000.0).abs() < 0.01);
        assert_eq!(planned_feet(200.0, 548.0, 32000.0, 3.0, None), 32000.0);
    }
    #[test]
    fn current_reveals_short_long_level_and_away_cases() {
        let t = Target {
            runway: "09".into(),
            latitude: 0.0,
            longitude: 1.0,
            elevation_feet: 100.0,
            glide_degrees: Some(3.0),
            intercept_feet: None,
            intercept_procedure: String::new(),
        };
        let mut live = Telemetry {
            position_valid: true,
            on_ground: false,
            ground_speed_knots: 120.0,
            heading_degrees: 90.0,
            altitude_feet: 3100.0,
            vertical_speed_fpm: -600.0,
            ..Telemetry::default()
        };
        let slope = current_gradient(&live, &t).unwrap();
        let touchdown = (t.elevation_feet - live.altitude_feet) / slope;
        assert_eq!(touchdown, 10.0);
        assert!(live.altitude_feet + slope * 8.0 > t.elevation_feet); // overshoot
        assert!(live.altitude_feet + slope * 12.0 < t.elevation_feet); // short
        live.vertical_speed_fpm = 0.0;
        assert_eq!(current_gradient(&live, &t), Some(0.0));
        live.ground_track_degrees = Some(270.0);
        assert!(current_gradient(&live, &t).is_none());
        live.ground_track_degrees = None;
        live.on_ground = true;
        assert!(current_gradient(&live, &t).is_none());
    }
    #[test]
    fn terrain_guide_covers_mountain_but_not_missing_data() {
        let s = vec![
            TerrainStation {
                remaining_nm: 1.0,
                ground_feet: Some(100.0),
                corridor_feet: Some(100.0),
            },
            TerrainStation {
                remaining_nm: 2.0,
                ground_feet: Some(3000.0),
                corridor_feet: Some(4000.0),
            },
        ];
        assert_eq!(clearance_feet(&s, 0), Some(6000.0));
        assert_eq!(clearance_floor(&s), Some(6000.0));
        assert_eq!(clearance_floor(&[]), None);
        let mut missing = s;
        missing[1].corridor_feet = None;
        assert_eq!(clearance_feet(&missing, 0), None);
    }
    #[test]
    fn current_profile_hits_a_mountain_exactly_and_leaves_unknown_gaps() {
        let samples = vec![
            TerrainStation {
                remaining_nm: 20.0,
                ground_feet: Some(0.0),
                corridor_feet: Some(0.0),
            },
            TerrainStation {
                remaining_nm: 10.0,
                ground_feet: Some(4000.0),
                corridor_feet: Some(4000.0),
            },
        ];
        let (at, height) = terrain_contact(&samples, 20.0, 5000.0, -300.0, 25.0).unwrap();
        assert!((at - 50.0 / 7.0).abs() < 0.0001);
        assert!((height - (5000.0 - 300.0 * at)).abs() < 0.0001);
        let mut unknown = samples;
        unknown[1].ground_feet = None;
        assert!(terrain_contact(&unknown, 20.0, 5000.0, -300.0, 25.0).is_none());
    }
    #[test]
    fn displaced_threshold_and_selected_runway_are_respected() {
        let mut data = ArrivalAirport::default();
        data.airport.reference_latitude = 0.0;
        data.airport.reference_longitude = 0.0;
        data.airport.runways.push(crate::airport::Runway {
            first_name: "09".into(),
            second_name: "27".into(),
            first: Point::default(),
            second: Point {
                east: 2000.0,
                ..Point::default()
            },
            first_displaced_meters: 300.0,
            ..Default::default()
        });
        let runway = target(&data, "09", "").unwrap();
        assert!((target_point(&data.airport, &runway).east - 300.0).abs() < 0.001);
        assert!(target(&data, "32L", "").is_none());
    }
    #[test]
    fn cifp_entry_is_not_a_missed_approach_altitude() {
        let mut data = ArrivalAirport::default();
        data.read_cifp("RWY:RW32L, , ,00548, ,ILT,1, ;\nAPPCH:010,I,I32LY, ,CI32L,YM,P,C,E  I, , ,IF, ,ILT,YM,P,I, ,1334,0088, , ,J,03000,03000;\nAPPCH:040,I,I32LY, , , , , , EM , , ,CA, , , , , , , ,3130, ,+,03200, ;");
        assert_eq!(data.thresholds[0].elevation_feet, 548.0);
        assert_eq!(data.intercepts.len(), 1);
        assert_eq!(data.intercepts[0].altitude_feet, 3000.0);
    }
}
