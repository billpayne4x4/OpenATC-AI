//! Checks against the installed airport scenery used for simulator testing.
use openatc_core::{
    airport::{airport_point, load_airport_from_simulator, taxi_point_on_pavement},
    state::Telemetry,
    stations,
};

#[test]
#[ignore = "requires an installed X-Plane scenery directory"]
fn los_angeles_surface_ownership_and_stations() {
    let root = std::env::var("OPENATC_TEST_XPLANE_ROOT").expect("Set OPENATC_TEST_XPLANE_ROOT");
    let airports = stations::map_airports(&root).unwrap();
    let index = stations::load_index(&root).unwrap();
    for (latitude, longitude) in [
        (33.938_282, -118.383_087),
        (33.940_738, -118.383_234),
        (33.941_596, -118.382_634),
    ] {
        let telemetry = Telemetry {
            latitude,
            longitude,
            on_ground: true,
            position_valid: true,
            ..Default::default()
        };
        assert_eq!(
            stations::occupied_airport(&airports, &telemetry).map(|a| a.id.as_str()),
            Some("KLAX")
        );
        let nearby = stations::nearby(&index, &telemetry);
        for role in ["Clearance", "Ground", "Tower"] {
            assert!(
                nearby
                    .iter()
                    .any(|s| s.airport == "KLAX" && s.service == role && s.receivable),
                "Missing {role}"
            );
        }
    }
}

#[test]
#[ignore = "requires installed X-Plane scenery via OPENATC_TEST_SIMULATOR_ROOT"]
fn canberra_pavement_separates_the_taxiway_from_the_grass_where_a_departure_stopped() {
    let root =
        std::env::var("OPENATC_TEST_SIMULATOR_ROOT").expect("Set OPENATC_TEST_SIMULATOR_ROOT");
    let airport = load_airport_from_simulator(
        &root,
        "YSCB",
        &|path| std::path::Path::new(path).is_file(),
        &|path| std::fs::read_to_string(path).map_err(|error| error.to_string()),
    )
    .expect("installed Canberra scenery");
    assert!(
        !airport.pavement_holes.is_empty(),
        "YSCB publishes pavement holes"
    );
    // Live position where an ambient departure stopped during taxi to runway 35.
    let stopped = airport_point(&airport, -35.306_415_557_861_33, 149.192_672_729_492_2);
    assert!(
        !taxi_point_on_pavement(&airport, stopped),
        "the stopped position is grass, not a pavement gap"
    );
    let on_taxiway = openatc_core::state::Point {
        east: stopped.east - 20.0,
        north: stopped.north,
        height: 0.0,
    };
    assert!(
        taxi_point_on_pavement(&airport, on_taxiway),
        "the parallel taxiway twenty metres west must stay pavement"
    );
    let grass = openatc_core::state::Point {
        east: stopped.east + 40.0,
        north: stopped.north,
        height: 0.0,
    };
    assert!(
        !taxi_point_on_pavement(&airport, grass),
        "open grass between the taxiway and runway must stay off pavement"
    );
}
