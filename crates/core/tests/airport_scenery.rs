//! Regression checks against the simulator airport format and installed taxi graph.
use openatc_core::airport::{calculate_taxi_route, load_airport, load_airport_from_simulator};
use openatc_core::state::Telemetry;
#[test]
fn standard_runway_has_26_fields_and_pavement_holes_stay_separate() {
    let text = "1 562 0 0 YMLT Launceston\n100 45.00 1 1 0.25 0 3 0 14R -41.5370582 147.2041027 0 61 2 0 0 0 32L -41.5520733 147.2169328 0 75 2 3 0 1\n110 1 0.25 0 Apron\n111 -41.54 147.20\n112 -41.55 147.20 -41.55 147.21\n113 -41.54 147.21\n111 -41.541 147.201\n111 -41.542 147.201\n113 -41.541 147.202\n";
    let airport = load_airport(text, "YMLT").unwrap();
    assert_eq!(airport.runways.len(), 1);
    assert_eq!(airport.runways[0].first_name, "14R");
    assert_eq!(airport.runways[0].second_name, "32L");
    assert_eq!(airport.pavements.len(), 2);
}
#[test]
#[ignore = "requires installed X-Plane scenery"]
fn installed_airport_has_geometry_and_a_route_to_hold_short() {
    let root = std::env::var("OPENATC_SIM_ROOT").expect("OPENATC_SIM_ROOT");
    let airport =
        load_airport_from_simulator(&root, "YMLT", &|p| std::path::Path::new(p).exists(), &|p| {
            std::fs::read_to_string(p).map_err(|e| e.to_string())
        })
        .unwrap();
    assert!(!airport.pavements.is_empty());
    assert!(!airport.edges.is_empty());
    assert!(!airport.parking.is_empty());
    let mut routes = 0;
    for parking in &airport.parking {
        let telemetry = Telemetry {
            latitude: airport.reference_latitude + parking.point.north / 111_320.0,
            longitude: airport.reference_longitude
                + parking.point.east / (111_320.0 * airport.reference_latitude.to_radians().cos()),
            position_valid: true,
            on_ground: true,
            ..Telemetry::default()
        };
        if let Ok(route) = calculate_taxi_route(&airport, &telemetry, "32L", false, 'C')
            && route.points.len() > 1
        {
            routes += 1;
        }
    }
    assert!(routes > 0, "no parking stand has a usable runway route");
    println!(
        "{}: {} runways, {} contours, {} edges, {} stands, {} usable routes",
        airport.icao,
        airport.runways.len(),
        airport.pavements.len(),
        airport.edges.len(),
        airport.parking.len(),
        routes
    );
}

#[test]
fn modern_ils_region_column_is_not_the_runway() {
    let mut airport = openatc_core::airport::Airport {
        icao: "YMLT".to_owned(),
        ..Default::default()
    };
    openatc_core::airport::load_navaids(
        &mut airport,
        "4 -41.534222222 147.201666667 545 10950 18 113007.372 ILT YMLT YM 32L ILS-cat-I\n6 -41.548916667 147.216388889 545 10950 18 300327.372 ILT YMLT YM 32L GS",
    );
    assert_eq!(airport.navaids.len(), 2);
    for navaid in &airport.navaids {
        assert_eq!(navaid.runway, "32L");
        assert!((navaid.bearing - 327.372).abs() < 0.001);
    }
    assert!((airport.navaids[1].glide_angle - 3.0).abs() < 0.001);
}

#[test]
fn taxi_stops_before_a_protected_crossing_instead_of_authorizing_it() {
    let text = "1 0 0 0 TEST Crossing airport\n100 45 1 1 0.25 0 3 0 09 0 0 0 0 2 0 0 0 27 0 0.02 0 0 2 0 0 0\n100 45 1 1 0.25 0 3 0 18 0.004 -0.005 0 0 2 0 0 0 36 -0.004 -0.005 0 0 2 0 0 0\n1201 0.001 -0.012 both 1\n1201 0.001 -0.0065 both 2\n1201 0.001 -0.0035 both 3\n1201 0.001 -0.001 both 4\n1202 1 2 twoway taxiway_C Alpha\n1202 2 3 twoway taxiway_C Alpha\n1204 departure 18,36\n1202 3 4 twoway taxiway_C Alpha\n";
    let airport = load_airport(text, "TEST").unwrap();
    let telemetry = Telemetry {
        latitude: 0.001,
        longitude: -0.012,
        position_valid: true,
        on_ground: true,
        ..Default::default()
    };
    let route = calculate_taxi_route(&airport, &telemetry, "09", false, 'C').unwrap();
    assert!(matches!(route.hold_short_runway.as_str(), "18" | "36"));
    assert!(route.instructions.to_lowercase().contains("hold short"));
    assert!(!route.approved);
    assert!(route.points.len() >= 2);
    let endpoint = route.points.last().unwrap();
    let longitude = route.reference_longitude + endpoint.east / 111_320.0;
    assert!(longitude < -0.006, "route crosses the protected runway");
}

#[test]
#[ignore = "requires installed X-Plane scenery"]
fn installed_centerlines_supply_missing_taxi_graph() {
    let root = std::env::var("OPENATC_SIM_ROOT").expect("OPENATC_SIM_ROOT");
    let airport =
        load_airport_from_simulator(&root, "VLVT", &|p| std::path::Path::new(p).exists(), &|p| {
            std::fs::read_to_string(p).map_err(|e| e.to_string())
        })
        .unwrap();
    println!(
        "inferred={} nodes={} edges={}",
        airport.inferred_taxi,
        airport.nodes.len(),
        airport.edges.len()
    );
    let telemetry = Telemetry {
        latitude: 17.974_573,
        longitude: 102.570_646,
        position_valid: true,
        on_ground: true,
        ..Default::default()
    };
    let route = calculate_taxi_route(&airport, &telemetry, "31", false, 'C').unwrap();
    println!(
        "{}; points={}; first={:?}; last={:?}; target={:?}",
        route.instructions,
        route.points.len(),
        route.points.first(),
        route.points.last(),
        route.destination_point
    );
    assert!(airport.inferred_taxi);
    assert!(route.points.len() > 1);
    assert_eq!(route.hold_short_runway, "31");
    assert!(!route.approved);
    let opposite = calculate_taxi_route(&airport, &telemetry, "13", false, 'C').unwrap();
    assert!(opposite.backtrack_required);
    assert!(!opposite.runway_taxi);
    let end = opposite.points.last().unwrap();
    let stopped = Telemetry {
        latitude: airport.reference_latitude + end.north / 111_320.0,
        longitude: airport.reference_longitude
            + end.east / (111_320.0 * airport.reference_latitude.to_radians().cos()),
        ..telemetry
    };
    if let Ok(path) = std::env::var("OPENATC_ROUTE_REVIEW_PATH") {
        std::fs::write(
            path,
            serde_json::to_vec(
                &serde_json::json!({"airport":airport,"taxi":route,"holding":opposite}),
            )
            .unwrap(),
        )
        .unwrap();
    }
    let backtrack =
        openatc_core::airport::calculate_runway_taxi_route(&airport, &stopped, "13").unwrap();
    assert!(backtrack.runway_taxi);
    if let Ok(path) = std::env::var("OPENATC_ROUTE_REVIEW_PATH") {
        std::fs::write(path,serde_json::to_vec(&serde_json::json!({"airport":airport,"taxi":route,"holding":opposite,"backtrack":backtrack})).unwrap()).unwrap();
    }
}

#[test]
fn inferred_graph_respects_centerline_types_pavement_and_hold_barriers() {
    let text = "1 0 0 0 TEST Painted airport\n100 30 1 1 0.2 0 0 0 09 0 0.01 0 0 0 0 0 0 27 0 0.03 0 0 0 0 0 0\n110 1 0.2 0 Apron\n111 -0.001 -0.001\n111 -0.001 0.005\n111 0.001 0.005\n113 0.001 -0.001\n120 Center\n111 0 0 1\n115 0 0.004\n120 Hold\n111 -0.001 0.002 4\n115 0.001 0.002\n120 Edge-not-route\n111 0.0005 0 3\n115 0.0005 0.004\n120 Outside-pavement\n111 0.002 0 1\n115 0.002 0.004\n";
    let airport = load_airport(text, "TEST").unwrap();
    assert!(airport.inferred_taxi);
    assert_eq!(
        airport.edges.len(),
        2,
        "hold divides the centerline without bridging it; edge and off-pavement lines excluded"
    );
    assert!(airport.edges.iter().all(|e| e.name.is_empty()));
    assert_eq!(airport.taxi_hold_lines.len(), 1);
    let published = load_airport(
        &format!(
            "{text}1201 0 0 both 100\n1201 0 0.004 both 101\n1202 100 101 twoway taxiway_C Alpha\n"
        ),
        "TEST",
    )
    .unwrap();
    assert!(!published.inferred_taxi);
    assert_eq!(published.edges.len(), 1);
    assert_eq!(published.edges[0].name, "Alpha");
}

#[test]
fn no_taxiway_runway_backtrack_uses_pavement_then_assigned_runway_and_holds() {
    use openatc_core::airport::calculate_runway_taxi_route;
    let text = "1 0 0 0 TEST Runway-only airport\n100 30 1 1 0.2 0 0 0 09 0 0 0 0 0 0 0 0 27 0 0.02 0 0 0 0 0 0\n110 1 0.2 0 Apron\n111 -0.001 0.007\n111 -0.001 0.009\n111 0.001 0.009\n113 0.001 0.007\n";
    let mut airport = load_airport(text, "TEST").unwrap();
    airport.runways[0].first_displaced_meters = 200.0;
    let telemetry = Telemetry {
        latitude: 0.0003,
        longitude: 0.008,
        on_ground: true,
        position_valid: true,
        ..Default::default()
    };
    let route = calculate_runway_taxi_route(&airport, &telemetry, "09").unwrap();
    assert!(route.runway_taxi);
    assert!(!route.approved);
    assert!(route.hold_short_runway.is_empty());
    assert!(
        route.instructions.contains("backtrack") && route.instructions.contains("hold position")
    );
    let end = route.points.last().unwrap();
    let threshold = airport.runways[0].first;
    assert!(((end.east - threshold.east).hypot(end.north - threshold.north) - 30.0).abs() < 0.001);
    // A disconnected apron and another runway must not be crossed by a guessed connector.
    assert!(
        calculate_runway_taxi_route(
            &airport,
            &Telemetry {
                latitude: 0.002,
                ..telemetry.clone()
            },
            "09"
        )
        .is_err()
    );
    let mut crossing = airport.runways[0].clone();
    crossing.first_name = "18".into();
    crossing.second_name = "36".into();
    crossing.first.east = 500.0;
    crossing.first.north = -200.0;
    crossing.second.east = 500.0;
    crossing.second.north = 200.0;
    airport.runways.push(crossing);
    assert!(calculate_runway_taxi_route(&airport, &telemetry, "09").is_err());
}

#[test]
fn mid_runway_hold_requires_separate_backtrack_to_the_departure_end() {
    let text = "1 0 0 0 TEST Mid-runway entry\n100 30 1 1 0.2 0 0 0 09 0 0 0 0 0 0 0 0 27 0 0.02 0 0 0 0 0 0\n110 1 0.2 0 Apron\n111 -0.001 0.009\n111 -0.001 0.011\n111 0.002 0.011\n113 0.002 0.009\n120 Centerline\n111 0.001 0.01 1\n115 0 0.01\n120 Hold\n111 0.0006 0.0095 4\n115 0.0006 0.0105\n";
    let airport = load_airport(text, "TEST").unwrap();
    let telemetry = Telemetry {
        latitude: 0.001,
        longitude: 0.01,
        on_ground: true,
        position_valid: true,
        ..Default::default()
    };
    let route = calculate_taxi_route(&airport, &telemetry, "09", false, 'C').unwrap();
    assert!(route.backtrack_required);
    assert!(!route.runway_taxi);
    assert_eq!(route.hold_short_runway, "09");
    assert!(route.instructions.to_lowercase().contains("hold short"));
    assert!(!route.instructions.contains("backtrack"));
    assert!(!route.instructions.contains("marked"));
    assert!(!route.instructions.contains("network"));
    assert!(route.via.is_empty());
    assert!(route.instructions.contains("holding point"));
    let end = route.points.last().unwrap();
    let stopped = Telemetry {
        latitude: airport.reference_latitude + end.north / 111_320.0,
        longitude: airport.reference_longitude
            + end.east / (111_320.0 * airport.reference_latitude.to_radians().cos()),
        ..telemetry
    };
    let runway_route =
        openatc_core::airport::calculate_runway_taxi_route(&airport, &stopped, "09").unwrap();
    assert!(runway_route.runway_taxi);
    assert!(!runway_route.approved);
}
