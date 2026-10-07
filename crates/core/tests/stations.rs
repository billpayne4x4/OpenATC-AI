//! Shared station reception and explicit combined-service regression checks.
use openatc_core::{state::Telemetry, stations::*};
fn station(airport: &str, service: &str, khz: i32, longitude: f64) -> Station {
    Station {
        airport: airport.into(),
        service: service.into(),
        khz,
        longitude,
        ..Station::default()
    }
}
#[test]
fn horizon_frequency_reuse_and_invalid_position() {
    let index = vec![
        station("TEST", "Ground", 121_900, 0.),
        station("NEAR", "Ground", 121_900, 0.1),
        station("FAR", "ATIS", 118_000, 1.),
    ];
    let mut t = Telemetry {
        position_valid: true,
        ..Telemetry::default()
    };
    let low = nearby(&index, &t);
    assert_eq!(low.len(), 2);
    assert_eq!(tuned(&low, 121_900).unwrap().airport, "TEST");
    assert!(tuned(&low, 123_450).is_none());
    t.altitude_feet = 10000.;
    assert!(nearby(&index, &t).iter().any(|s| s.airport == "FAR"));
    t.position_valid = false;
    assert!(nearby(&index, &t).is_empty());
}
#[test]
fn combined_service_requires_an_existing_controller_station() {
    let mut index = vec![
        station("TEST", "Ground", 121_900, 0.),
        station("TEST", "ATIS", 118_000, 0.),
    ];
    assert!(!station_serves(&index[0], "clearance"));
    apply_combined(
        &mut index,
        "[[combined]]\nairport='TEST'\nkhz=121900\nservices=['Ground','Clearance']",
    )
    .unwrap();
    assert!(station_serves(&index[0], "clearance"));
    assert!(station_serves(&index[0], "taxi"));
    assert!(!station_serves(&index[0], "ready"));
    for text in [
        "[[combined]]\nairport='TEST'\nkhz=118000\nservices=['Ground']",
        "[[combined]]\nairport='TEST'\nkhz=123450\nservices=['Ground']",
    ] {
        assert!(apply_combined(&mut index, text).is_err());
    }
}

#[test]
fn consolidated_roles_follow_delivery_ground_tower_priority() {
    let mut index = vec![
        station("SOLO", "Tower", 118_100, 0.),
        station("GROUND", "Tower", 118_200, 0.),
        station("GROUND", "Ground", 121_900, 0.),
        station("FULL", "Tower", 118_300, 0.),
        station("FULL", "Ground", 121_800, 0.),
        station("FULL", "Clearance", 121_700, 0.),
        station("SOLO", "ATIS", 118_000, 0.),
    ];
    assign_airport_capabilities(&mut index);
    assert!(station_serves(&index[0], "clearance"));
    assert!(station_serves(&index[0], "taxi"));
    assert!(station_serves(&index[0], "ready"));
    assert!(!station_serves(&index[1], "clearance"));
    assert!(station_serves(&index[2], "clearance"));
    assert!(!station_serves(&index[4], "clearance"));
    assert!(station_serves(&index[5], "clearance"));
    assert!(!station_serves(&index[6], "clearance"));
}
