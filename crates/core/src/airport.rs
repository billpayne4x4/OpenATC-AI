//! Airport scenery and navdata loading, local coordinates and taxi routing.

use super::state::{Point, TaxiClearance, Telemetry};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

/// Runway with both ends in local meters.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Runway {
    /// First end name.
    pub first_name: String,
    /// Second end name.
    pub second_name: String,
    /// First threshold.
    pub first: Point,
    /// Second threshold.
    pub second: Point,
    /// Displacement from the first runway end, meters.
    pub first_displaced_meters: f64,
    /// Displacement from the second runway end, meters.
    pub second_displaced_meters: f64,
    /// Width in meters.
    pub width: f64,
}

impl Runway {
    /// Landing threshold, respecting scenery displaced thresholds.
    #[must_use]
    pub fn landing_threshold(&self, name: &str) -> Option<Point> {
        let (start, end, displacement) = if name == self.first_name {
            (self.first, self.second, self.first_displaced_meters)
        } else if name == self.second_name {
            (self.second, self.first, self.second_displaced_meters)
        } else {
            return None;
        };
        let length = (end.east - start.east).hypot(end.north - start.north);
        let fraction = if length > 0.0 {
            (displacement / length).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Some(Point {
            east: start.east + (end.east - start.east) * fraction,
            north: start.north + (end.north - start.north) * fraction,
            height: start.height + (end.height - start.height) * fraction,
        })
    }
}

/// Taxiway graph node.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct TaxiNode {
    /// Node id.
    pub id: i64,
    /// Position.
    pub point: Point,
}

/// Serialize the aircraft size letter as its numeric character code.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn serialize_size<S: serde::Serializer>(size: &char, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_u8(*size as u8)
}

/// Reads the numeric form back; a stray single letter is forgiven.
fn deserialize_size<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<char, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    if let Some(number) = value.as_u64() {
        return char::from_u32(u32::try_from(number).unwrap_or(u32::from(b'F')))
            .ok_or_else(|| serde::de::Error::custom("bad size code"));
    }
    if let Some(text) = value.as_str()
        && let Some(letter) = text.chars().next()
    {
        return Ok(letter);
    }
    Err(serde::de::Error::custom("bad size code"))
}

/// Taxiway graph edge.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TaxiEdge {
    /// From node.
    pub first: i64,
    /// To node.
    pub second: i64,
    /// Taxiway name.
    pub name: String,
    /// Edge is runway pavement.
    pub runway: bool,
    /// One-way edge.
    pub one_way: bool,
    /// Active runways annotation.
    pub active_runways: String,
    /// Aircraft size class, single letter. Serializes as its code number,
    /// preserving the existing JSON format.
    #[serde(
        serialize_with = "serialize_size",
        deserialize_with = "deserialize_size"
    )]
    pub size: char,
}

/// Parking stand.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Parking {
    /// Stand name.
    pub name: String,
    /// Stand type.
    #[serde(rename = "type")]
    pub kind: String,
    /// Equipment.
    pub equipment: String,
    /// Size class.
    pub size: String,
    /// Operations.
    pub operations: String,
    /// Position.
    pub point: Point,
    /// Heading degrees.
    pub heading: f64,
}

/// Radio frequency.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Frequency {
    /// Service name.
    pub service: String,
    /// Station name.
    pub name: String,
    /// Frequency in kHz.
    pub khz: i32,
}

/// Navaid record.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Navaid {
    /// Navaid type.
    #[serde(rename = "type")]
    pub kind: String,
    /// Identifier.
    pub identifier: String,
    /// Name.
    pub name: String,
    /// Airport ICAO.
    pub airport: String,
    /// Runway.
    pub runway: String,
    /// Degrees north.
    pub latitude: f64,
    /// Degrees east.
    pub longitude: f64,
    /// Frequency.
    pub frequency: f64,
    /// Bearing degrees.
    pub bearing: f64,
    /// Elevation feet.
    pub elevation_feet: f64,
    /// Range in nautical miles.
    pub range_nm: f64,
    /// Glide angle degrees.
    pub glide_angle: f64,
}

/// Instrument procedure header.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Procedure {
    /// Procedure type.
    #[serde(rename = "type")]
    pub kind: String,
    /// Procedure name.
    pub name: String,
    /// Transition.
    pub transition: String,
}

/// Airport scenery geometry and operational data.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Airport {
    /// ICAO code.
    pub icao: String,
    /// Name.
    pub name: String,
    /// Source path or description.
    pub source: String,
    /// Reference latitude.
    pub reference_latitude: f64,
    /// Reference longitude.
    pub reference_longitude: f64,
    /// Elevation feet.
    pub elevation_feet: f64,
    /// Runways.
    pub runways: Vec<Runway>,
    /// Installed pavement boundary contours in local meters.
    pub pavements: Vec<Vec<Point>>,
    /// Triangulated pavement faces, preserving cutouts.
    pub pavement_triangles: Vec<[Point; 3]>,
    /// Taxi graph nodes.
    pub nodes: Vec<TaxiNode>,
    /// Taxi graph edges.
    pub edges: Vec<TaxiEdge>,
    /// Graph inferred from painted centerlines rather than published ATC routes.
    pub inferred_taxi: bool,
    /// Painted hold barriers for centerline fallback routing.
    pub taxi_hold_lines: Vec<[Point; 2]>,
    /// Tessellated installed taxi centerlines, retained for authorized runway entry.
    pub taxi_centerlines: Vec<[Point; 2]>,
    /// Parking stands.
    pub parking: Vec<Parking>,
    /// Frequencies.
    pub frequencies: Vec<Frequency>,
    /// Navaids.
    pub navaids: Vec<Navaid>,
    /// Procedures.
    pub procedures: Vec<Procedure>,
}

/// Project geographic coordinates into local meters.
#[must_use]
pub fn airport_point(airport: &Airport, latitude: f64, longitude: f64) -> Point {
    let delta_longitude = (longitude - airport.reference_longitude) % 360.0;
    Point {
        east: delta_longitude
            * 111_320.0
            * (airport.reference_latitude * std::f64::consts::PI / 180.0).cos(),
        north: (latitude - airport.reference_latitude) * 111_320.0,
        height: 0.0,
    }
}

/// Synthetic airport for the desktop demo and test fixtures.
#[must_use]
pub fn demo_airport() -> Airport {
    Airport {
        icao: "DEMO".to_owned(),
        name: "Coastal International".to_owned(),
        source: "Synthetic geometry - not a real airport".to_owned(),
        runways: vec![demo_runway()],
        nodes: demo_nodes(),
        edges: demo_edges(),
        parking: demo_parking(),
        frequencies: demo_frequencies(),
        ..Default::default()
    }
}

/// Demo runway 09/27, 2.4 km long.
fn demo_runway() -> Runway {
    Runway {
        first_name: "09".to_owned(),
        second_name: "27".to_owned(),
        first: Point {
            east: -1200.0,
            north: 0.0,
            height: 0.0,
        },
        second: Point {
            east: 1200.0,
            north: 0.0,
            height: 0.0,
        },
        first_displaced_meters: 0.0,
        second_displaced_meters: 0.0,
        width: 45.0,
    }
}

/// Demo taxiway graph nodes.
fn demo_nodes() -> Vec<TaxiNode> {
    vec![
        TaxiNode {
            id: 1,
            point: Point {
                east: -1000.0,
                north: 200.0,
                height: 0.0,
            },
        },
        TaxiNode {
            id: 2,
            point: Point {
                east: 0.0,
                north: 200.0,
                height: 0.0,
            },
        },
        TaxiNode {
            id: 3,
            point: Point {
                east: 1000.0,
                north: 200.0,
                height: 0.0,
            },
        },
        TaxiNode {
            id: 4,
            point: Point {
                east: -1000.0,
                north: 0.0,
                height: 0.0,
            },
        },
        TaxiNode {
            id: 5,
            point: Point {
                east: 1000.0,
                north: 0.0,
                height: 0.0,
            },
        },
        TaxiNode {
            id: 6,
            point: Point {
                east: 0.0,
                north: 430.0,
                height: 0.0,
            },
        },
    ]
}

/// Demo taxiway graph edges.
fn demo_edges() -> Vec<TaxiEdge> {
    vec![
        TaxiEdge {
            first: 1,
            second: 2,
            name: "A".to_owned(),
            size: 'F',
            ..Default::default()
        },
        TaxiEdge {
            first: 2,
            second: 3,
            name: "A".to_owned(),
            size: 'F',
            ..Default::default()
        },
        TaxiEdge {
            first: 1,
            second: 4,
            name: "A1".to_owned(),
            size: 'F',
            ..Default::default()
        },
        TaxiEdge {
            first: 3,
            second: 5,
            name: "A2".to_owned(),
            size: 'F',
            ..Default::default()
        },
        TaxiEdge {
            first: 2,
            second: 6,
            name: "B".to_owned(),
            size: 'F',
            ..Default::default()
        },
    ]
}

/// Demo parking stands.
fn demo_parking() -> Vec<Parking> {
    vec![
        Parking {
            name: "Gate 1".to_owned(),
            kind: "gate".to_owned(),
            equipment: "jets".to_owned(),
            size: "C".to_owned(),
            operations: "airline".to_owned(),
            point: Point {
                east: 0.0,
                north: 430.0,
                height: 0.0,
            },
            heading: 180.0,
        },
        Parking {
            name: "Gate 2".to_owned(),
            kind: "gate".to_owned(),
            equipment: "jets".to_owned(),
            size: "C".to_owned(),
            operations: "airline".to_owned(),
            point: Point {
                east: 250.0,
                north: 430.0,
                height: 0.0,
            },
            heading: 180.0,
        },
        Parking {
            name: "GA apron".to_owned(),
            kind: "misc".to_owned(),
            equipment: "props".to_owned(),
            size: "A".to_owned(),
            operations: "general_aviation".to_owned(),
            point: Point {
                east: -400.0,
                north: 400.0,
                height: 0.0,
            },
            heading: 90.0,
        },
    ]
}

/// Demo radio frequencies.
fn demo_frequencies() -> Vec<Frequency> {
    vec![
        Frequency {
            service: "ATIS".to_owned(),
            name: "Demo ATIS".to_owned(),
            khz: 126_300,
        },
        Frequency {
            service: "Clearance".to_owned(),
            name: "Demo Clearance".to_owned(),
            khz: 121_700,
        },
        Frequency {
            service: "Ground".to_owned(),
            name: "Demo Ground".to_owned(),
            khz: 121_900,
        },
        Frequency {
            service: "Tower".to_owned(),
            name: "Demo Tower".to_owned(),
            khz: 118_100,
        },
        Frequency {
            service: "Departure".to_owned(),
            name: "Demo Departure".to_owned(),
            khz: 123_800,
        },
        Frequency {
            service: "Approach".to_owned(),
            name: "Demo Approach".to_owned(),
            khz: 125_500,
        },
    ]
}

/// Great-circle distance in nautical miles, local to this module to avoid a
/// dependency cycle with the ops module.
fn distance_nm(latitude_a: f64, longitude_a: f64, latitude_b: f64, longitude_b: f64) -> f64 {
    const RADIANS: f64 = std::f64::consts::PI / 180.0;
    let sine = ((latitude_b - latitude_a) * RADIANS / 2.0).sin().mul_add(
        ((latitude_b - latitude_a) * RADIANS / 2.0).sin(),
        ((longitude_b - longitude_a) * RADIANS / 2.0).sin()
            * ((longitude_b - longitude_a) * RADIANS / 2.0).sin()
            * (latitude_a * RADIANS).cos()
            * (latitude_b * RADIANS).cos(),
    );
    2.0 * 3440.06479 * sine.sqrt().asin()
}

/// True when a local point lies on (or just beside) a runway strip.
/// Whether a point lies within a physical runway corridor.
pub fn within_runway(airport: &Airport, point: &Point) -> bool {
    for runway in &airport.runways {
        let east = runway.second.east - runway.first.east;
        let north = runway.second.north - runway.first.north;
        let length = east.hypot(north);
        if length < 1.0 {
            continue;
        }
        let along = ((point.east - runway.first.east) * east
            + (point.north - runway.first.north) * north)
            / length;
        let across = ((point.east - runway.first.east) * north
            - (point.north - runway.first.north) * east)
            .abs()
            / length;
        if (-40.0..=length + 40.0).contains(&along) && across < runway.width / 2.0 + 35.0 {
            return true;
        }
    }
    false
}

/// Resolve the route target: a parking stand point or a runway end.
fn route_target(airport: &Airport, destination: &str, to_parking: bool) -> Result<Point, String> {
    if to_parking {
        return airport
            .parking
            .iter()
            .find(|stand| stand.name == destination)
            .map(|stand| stand.point)
            .ok_or_else(|| crate::dialogue::say("taxi_arrival_stand_missing", &[]));
    }
    let mut found = None;
    for runway in &airport.runways {
        if runway.first_name == destination {
            found = Some(runway.first);
        }
        if runway.second_name == destination {
            found = Some(runway.second);
        }
    }
    found.ok_or_else(|| crate::dialogue::say("taxi_departure_runway_missing", &[]))
}

/// Local planar distance in meters.
fn flat_distance(first: &Point, second: &Point) -> f64 {
    (first.east - second.east).hypot(first.north - second.north)
}

/// Build the taxi graph, skipping runway pavement, active-zone edges and
/// segments too small for the aircraft, and any edge crossing a runway.
fn taxi_graph(
    airport: &Airport,
    nodes: &BTreeMap<i64, Point>,
    aircraft_size: char,
) -> BTreeMap<i64, Vec<(i64, f64, String)>> {
    let mut graph: BTreeMap<i64, Vec<(i64, f64, String)>> = BTreeMap::new();
    for edge in &airport.edges {
        let (Some(first), Some(second)) = (nodes.get(&edge.first), nodes.get(&edge.second)) else {
            continue;
        };
        if edge.runway || !edge.active_runways.is_empty() || edge.size < aircraft_size {
            continue;
        }
        // Sample the segment at approximately 15-meter intervals.
        #[allow(clippy::cast_possible_truncation)]
        let steps = (flat_distance(first, second) / 15.0) as i64;
        let steps = steps.max(1);
        let conflict = (0..=steps).any(|step| {
            let fraction = f64::from(i32::try_from(step).unwrap_or(i32::MAX))
                / f64::from(i32::try_from(steps).unwrap_or(i32::MAX).max(1));
            within_runway(
                airport,
                &Point {
                    east: first.east + (second.east - first.east) * fraction,
                    north: first.north + (second.north - first.north) * fraction,
                    height: 0.0,
                },
            )
        });
        if conflict {
            continue;
        }
        let length = flat_distance(first, second);
        graph
            .entry(edge.first)
            .or_default()
            .push((edge.second, length, edge.name.clone()));
        if !edge.one_way {
            graph
                .entry(edge.second)
                .or_default()
                .push((edge.first, length, edge.name.clone()));
        }
    }
    graph
}

/// Dijkstra from `start`; settled nodes carry minimal cost because every edge
/// is non-negative. Ignore obsolete queue entries.
fn shortest_paths(
    graph: &BTreeMap<i64, Vec<(i64, f64, String)>>,
    start: i64,
) -> (BTreeMap<i64, f64>, BTreeMap<i64, (i64, String)>) {
    let mut costs: BTreeMap<i64, f64> = BTreeMap::new();
    let mut previous: BTreeMap<i64, (i64, String)> = BTreeMap::new();
    let mut settled = std::collections::BTreeSet::new();
    let mut pending: BinaryHeap<(Reverse<u64>, i64)> = BinaryHeap::new();
    costs.insert(start, 0.0);
    pending.push((Reverse(0), start));
    while let Some((_, node)) = pending.pop() {
        if !settled.insert(node) {
            continue;
        }
        let cost = costs.get(&node).copied().unwrap_or(f64::INFINITY);
        if let Some(edges) = graph.get(&node) {
            for (target, length, name) in edges {
                let next = cost + length;
                if next < costs.get(target).copied().unwrap_or(f64::INFINITY) {
                    costs.insert(*target, next);
                    previous.insert(*target, (node, name.clone()));
                    pending.push((Reverse(next.to_bits()), *target));
                }
            }
        }
    }
    (costs, previous)
}

/// Walk the predecessor chain back to `start`, returning path points plus the
/// readable `via` list with consecutive duplicates removed.
fn taxi_path(
    nodes: &BTreeMap<i64, Point>,
    previous: &BTreeMap<i64, (i64, String)>,
    start: i64,
    target: i64,
) -> (Vec<Point>, String) {
    let mut points = Vec::new();
    let mut names = Vec::new();
    let mut node = target;
    loop {
        points.push(nodes[&node]);
        if node == start {
            break;
        }
        let (previous_node, name) = previous[&node].clone();
        names.push(name);
        node = previous_node;
    }
    points.reverse();
    names.reverse();
    let mut via = String::new();
    let mut last = String::new();
    for name in names {
        if !name.is_empty() && name != last {
            if !via.is_empty() {
                via += ", ";
            }
            via += &name;
            last = name;
        }
    }
    (points, via)
}

/// Route to the destination over connected taxi edges sized for the aircraft.
/// Keep runway crossings protected and return structured taxiway/hold-short facts.
pub fn calculate_taxi_route(
    airport: &Airport,
    telemetry: &Telemetry,
    destination: &str,
    to_parking: bool,
    aircraft_size: char,
) -> Result<TaxiClearance, String> {
    if !telemetry.on_ground || !telemetry.position_valid {
        return Err(crate::dialogue::say(
            "taxi_routing_needs_a_valid_ground_position",
            &[],
        ));
    }
    if airport.nodes.is_empty() {
        return Err(crate::dialogue::say(
            "no_taxi_network_is_available_for_this",
            &[],
        ));
    }
    let start_point = airport_point(airport, telemetry.latitude, telemetry.longitude);
    let target_point = route_target(airport, destination, to_parking)?;
    let nodes: BTreeMap<i64, Point> = airport
        .nodes
        .iter()
        .map(|node| (node.id, node.point))
        .collect();
    let graph = taxi_graph(airport, &nodes, aircraft_size);
    if within_runway(airport, &start_point) {
        return Err(crate::dialogue::say(
            "vacate_the_runway_before_requesting_a_ground",
            &[],
        ));
    }
    let (mut start, mut nearest) = (-1, f64::INFINITY);
    for (id, point) in &nodes {
        if within_runway(airport, point) || !graph.contains_key(id) {
            continue;
        }
        let separation = flat_distance(&start_point, point);
        if separation < nearest {
            nearest = separation;
            start = *id;
        }
    }
    if start < 0
        || nearest > if airport.inferred_taxi { 35.0 } else { 350.0 }
        || (airport.inferred_taxi
            && (holds_or_pavement_conflict(airport, start_point, nodes[&start])))
    {
        return Err(crate::dialogue::say(
            if airport.inferred_taxi {
                "taxi_inferred_start_missing"
            } else {
                "no_safe_taxi_network_start_within_m"
            },
            &[],
        ));
    }
    let (costs, previous) = shortest_paths(&graph, start);
    let (mut target, mut target_distance) = (-1, f64::INFINITY);
    for node in costs.keys() {
        let separation = flat_distance(&nodes[node], &target_point);
        if separation < target_distance {
            target_distance = separation;
            target = *node;
        }
    }
    let mut hold_short = if to_parking {
        String::new()
    } else {
        destination.to_owned()
    };
    if target < 0 || target_distance > if to_parking { 350.0 } else { 500.0 } {
        // Route only through safe graph edges, ending before the first published
        // protected runway edge. A crossing is never part of this clearance.
        let mut stop = None;
        let mut best = f64::INFINITY;
        for edge in &airport.edges {
            if edge.size < aircraft_size {
                continue;
            }
            let protected = format!(
                "{} {}",
                edge.active_runways,
                if edge.runway { &edge.name } else { "" }
            );
            let runway = protected
                .split(|c: char| !c.is_ascii_alphanumeric())
                .find(|name| {
                    airport
                        .runways
                        .iter()
                        .any(|r| r.first_name == *name || r.second_name == *name)
                });
            let Some(runway) = runway else { continue };
            for (before, after) in [(edge.first, edge.second), (edge.second, edge.first)] {
                if edge.one_way && before != edge.first {
                    continue;
                }
                let (Some(cost), Some(a), Some(b)) =
                    (costs.get(&before), nodes.get(&before), nodes.get(&after))
                else {
                    continue;
                };
                if within_runway(airport, a)
                    || flat_distance(b, &target_point) >= flat_distance(a, &target_point)
                {
                    continue;
                }
                let score = cost + flat_distance(b, &target_point);
                if score < best {
                    best = score;
                    stop = Some((before, runway.to_owned()));
                }
            }
        }
        if stop.is_none() && airport.inferred_taxi && !to_parking {
            for (node, cost) in &costs {
                let point = nodes[node];
                if airport
                    .taxi_hold_lines
                    .iter()
                    .any(|h| segment_distance(point, h[0], h[1]) <= 26.0)
                    && airport
                        .runways
                        .iter()
                        .filter(|r| r.first_name == destination || r.second_name == destination)
                        .any(|r| segment_distance(point, r.first, r.second) < 150.0)
                    && *cost < best
                {
                    best = *cost;
                    stop = Some((*node, destination.to_owned()));
                }
            }
        }
        let Some((node, runway)) = stop else {
            return Err(crate::dialogue::say(
                "no_connected_safe_taxi_route_or_identifiable",
                &[],
            ));
        };
        target = node;
        hold_short = runway;
    }
    // A reachable node can be close to the departure threshold while still
    // stopping before a different parallel runway.
    if !to_parking
        && let Some(end) = nodes.get(&target)
        && let Some(runway) = airport.runways.iter().min_by(|a, b| {
            segment_distance(*end, a.first, a.second)
                .total_cmp(&segment_distance(*end, b.first, b.second))
        })
        && segment_distance(*end, runway.first, runway.second) <= 150.0
        && airport
            .runways
            .iter()
            .find(|r| r.first_name == destination || r.second_name == destination)
            .is_some_and(|r| {
                segment_distance(*end, r.first, r.second)
                    > segment_distance(*end, runway.first, runway.second) + 20.0
            })
    {
        let destination_number = destination.get(..2).unwrap_or(destination);
        hold_short = if runway.first_name.starts_with(destination_number) {
            runway.first_name.clone()
        } else if runway.second_name.starts_with(destination_number) {
            runway.second_name.clone()
        } else {
            runway.first_name.clone()
        };
    }
    let (points, via) = taxi_path(&nodes, &previous, start, target);
    let holding_marker_point = holding_marker_position(airport, &points);
    let needs_backtrack = target_distance > 500.0 && !to_parking && hold_short == destination;
    let instructions = if via.is_empty() && !hold_short.is_empty() {
        crate::dialogue::say("taxi_to_holding_point", &[("runway", hold_short.clone())])
    } else if via.is_empty() && to_parking {
        crate::dialogue::say(
            "taxi_toward_parking_hold",
            &[("destination", destination.to_owned())],
        )
    } else if needs_backtrack {
        crate::dialogue::say(
            "taxi_hold_for_backtrack",
            &[("via", via.clone()), ("runway", destination.to_owned())],
        )
    } else if !hold_short.is_empty() {
        crate::dialogue::say(
            "taxi_via_hold_short_of_runway_at",
            &[("via", (via).clone()), ("hold_short", (hold_short).clone())],
        )
    } else if to_parking {
        crate::dialogue::say(
            "taxi_via_toward_stop_at_the_end",
            &[
                ("via", (via).clone()),
                ("destination", (destination).to_string()),
            ],
        )
    } else {
        crate::dialogue::say(
            "taxi_via_hold_short_of_runway_at",
            &[("via", (via).clone()), ("hold_short", (hold_short).clone())],
        )
    };
    Ok(TaxiClearance {
        airport: airport.icao.clone(),
        destination: destination.to_owned(),
        instructions,
        via,
        points,
        approved: false,
        pending_readback: false,
        backtrack_required: needs_backtrack,
        runway_taxi: false,
        crossing_runway: String::new(),
        entry_runway: String::new(),
        holding_channel: 0,
        holding_marker_point,
        waiting_for_traffic: false,
        guidance_complete: false,
        hold_short_runway: hold_short,
        sequence: 0,
        reference_latitude: airport.reference_latitude,
        reference_longitude: airport.reference_longitude,
        destination_point: target_point,
        to_parking,
    })
}

/// Split a data line into whitespace tokens.
fn tokens(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

/// Rest of a raw line after its first `used` whitespace tokens, leading
/// separator intact.
fn rest_after_tokens(line: &str, used: usize) -> &str {
    let mut rest = line;
    for _ in 0..used {
        let start = rest
            .find(|value: char| !value.is_whitespace())
            .unwrap_or(rest.len());
        rest = &rest[start..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        rest = &rest[end..];
    }
    rest.trim_end_matches(['\r', '\n'])
}

/// Name field: rest of line after the parsed tokens, leading blanks stripped
/// Strip spaces and tabs only.
fn name_after_tokens(line: &str, used: usize) -> String {
    rest_after_tokens(line, used)
        .trim_start_matches([' ', '\t'])
        .to_owned()
}

/// Read one airport from an apt.dat file, supporting these records:
/// headers pick the airport, then runways, taxi nodes/edges, stands and
/// frequencies. Anything we do not understand is skipped, like over there.
pub fn load_airport(text: &str, icao: &str) -> Result<Airport, String> {
    let mut builder = AirportBuilder::default();
    let mut selected = false;
    let mut pavement = false;
    let mut contour = Vec::new();
    let mut controls = Vec::new();
    let mut pavement_starts = Vec::new();
    let mut linear = false;
    let mut line_nodes = Vec::new();
    let mut line_controls = Vec::new();
    let mut line_types = Vec::new();
    let mut centerlines = Vec::new();
    let mut hold_lines = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end_matches(['\r', '\n']);
        let fields = tokens(line);
        if fields.is_empty() {
            continue;
        }
        let Ok(record) = fields[0].parse::<i32>() else {
            continue;
        };
        if record == 1 || record == 16 || record == 17 {
            if selected {
                break;
            }
            selected = builder.select_header(&fields, line, icao);
            continue;
        }
        if !selected {
            continue;
        }
        if record == 120 {
            linear = true;
            line_nodes.clear();
            line_controls.clear();
            line_types.clear();
        } else if linear && (111..=116).contains(&record) {
            if let Some((lat, lon)) = fields
                .get(1)
                .and_then(|v| v.parse().ok())
                .zip(fields.get(2).and_then(|v| v.parse().ok()))
            {
                line_nodes.push(builder.project(lat, lon));
                let bezier = matches!(record, 112 | 114 | 116);
                line_controls.push(if bezier {
                    fields
                        .get(3)
                        .and_then(|v| v.parse().ok())
                        .zip(fields.get(4).and_then(|v| v.parse().ok()))
                        .map(|(lat, lon)| builder.project(lat, lon))
                } else {
                    None
                });
                line_types.push(
                    fields
                        .get(if bezier { 5 } else { 3 })
                        .and_then(|v| v.parse::<i32>().ok())
                        .unwrap_or(0),
                );
            }
            if matches!(record, 113..=116) {
                if matches!(record, 113 | 114) && !line_nodes.is_empty() {
                    line_nodes.push(line_nodes[0]);
                    line_controls.push(line_controls[0]);
                    line_types.push(0);
                }
                for i in 0..line_nodes.len().saturating_sub(1) {
                    let kind = line_types[i];
                    let output = if matches!(kind, 1 | 7 | 51 | 57) {
                        &mut centerlines
                    } else if matches!(kind, 4 | 5 | 6 | 54 | 55 | 56) {
                        &mut hold_lines
                    } else {
                        continue;
                    };
                    let curve = pavement_curve(&line_nodes[i..=i + 1], &line_controls[i..=i + 1]);
                    let mut points = curve[..curve.len() / 2].to_vec();
                    points.push(line_nodes[i + 1]);
                    output.extend(points.windows(2).map(|p| (p[0], p[1])));
                }
                line_nodes.clear();
                line_controls.clear();
                line_types.clear();
            }
        } else {
            linear = false;
        }
        if !(110..=114).contains(&record) {
            pavement = false;
        }
        if record == 110 {
            pavement_starts.push(builder.airport.pavements.len());
            pavement = true;
            contour.clear();
            controls.clear();
        } else if pavement && (111..=114).contains(&record) {
            if let (Some(lat), Some(lon)) = (
                fields.get(1).and_then(|v| v.parse().ok()),
                fields.get(2).and_then(|v| v.parse().ok()),
            ) {
                contour.push(builder.project(lat, lon));
                let control = if record == 112 || record == 114 {
                    fields
                        .get(3)
                        .and_then(|v| v.parse().ok())
                        .zip(fields.get(4).and_then(|v| v.parse().ok()))
                        .map(|(lat, lon)| builder.project(lat, lon))
                } else {
                    None
                };
                controls.push(control);
            }
            if record == 113 || record == 114 {
                if contour.len() >= 3 {
                    builder
                        .airport
                        .pavements
                        .push(pavement_curve(&contour, &controls));
                }
                contour.clear();
                controls.clear();
            }
        } else if record == 100 {
            builder.add_runway(&fields);
        } else if record == 1201 {
            builder.add_node(&fields);
        } else if record == 1202 {
            builder.add_edge(&fields, line);
        } else if record == 1204 {
            builder.extend_edge(line);
        } else if record == 1300 || record == 15 {
            builder.add_parking(record, &fields, line);
        } else if record == 1301 {
            builder.size_parking(&fields);
        } else if (50..=56).contains(&record) || (1050..=1056).contains(&record) {
            builder.add_frequency(record, &fields, line);
        }
    }
    pavement_starts.push(builder.airport.pavements.len());
    for limits in pavement_starts.windows(2) {
        builder.airport.pavement_triangles.extend(pavement_faces(
            &builder.airport.pavements[limits[0]..limits[1]],
        ));
    }
    builder.airport.taxi_centerlines = centerlines
        .iter()
        .map(|&(first, second)| [first, second])
        .collect();
    builder.airport.taxi_hold_lines = hold_lines
        .iter()
        .map(|&(first, second)| [first, second])
        .collect();
    if builder.airport.nodes.is_empty() && builder.airport.edges.is_empty() {
        infer_centerline_graph(&mut builder.airport, &centerlines, &hold_lines);
    }
    builder.finish()
}

/// Triangulate each pavement with its interior islands left open.
fn pavement_faces(contours: &[Vec<Point>]) -> Vec<[Point; 3]> {
    let mut vertices = Vec::new();
    let mut holes = Vec::new();
    for (index, contour) in contours.iter().enumerate() {
        if index > 0 {
            holes.push(vertices.len());
        }
        vertices.extend_from_slice(contour);
    }
    let coordinates: Vec<_> = vertices.iter().flat_map(|p| [p.east, p.north]).collect();
    earcutr::earcut(&coordinates, &holes, 2)
        .unwrap_or_default()
        .as_chunks::<3>()
        .0
        .iter()
        .map(|v| [vertices[v[0]], vertices[v[1]], vertices[v[2]]])
        .collect()
}

/// Tessellate closed pavement curves; incoming handles mirror the outgoing handle.
fn pavement_curve(nodes: &[Point], controls: &[Option<Point>]) -> Vec<Point> {
    let mut result = Vec::new();
    for index in 0..nodes.len() {
        let next = (index + 1) % nodes.len();
        let first = nodes[index];
        let second = nodes[next];
        let outgoing = controls[index].unwrap_or(first);
        let incoming = controls[next].map_or(second, |v| Point {
            east: 2.0 * second.east - v.east,
            north: 2.0 * second.north - v.north,
            height: 0.0,
        });
        let steps = if controls[index].is_some() || controls[next].is_some() {
            12
        } else {
            1
        };
        for step in 0..steps {
            let fraction = f64::from(step) / f64::from(steps);
            let inverse = 1.0 - fraction;
            result.push(Point {
                east: inverse * inverse * inverse * first.east
                    + 3.0 * inverse * inverse * fraction * outgoing.east
                    + 3.0 * inverse * fraction * fraction * incoming.east
                    + fraction * fraction * fraction * second.east,
                north: inverse * inverse * inverse * first.north
                    + 3.0 * inverse * inverse * fraction * outgoing.north
                    + 3.0 * inverse * fraction * fraction * incoming.north
                    + fraction * fraction * fraction * second.north,
                height: 0.0,
            });
        }
    }
    result
}

/// Builds an airport record by record. The projection reference latches onto
/// the first projected coordinate.
#[derive(Default)]
struct AirportBuilder {
    airport: Airport,
    reference_set: bool,
    modern_frequencies: bool,
}

impl AirportBuilder {
    fn project(&mut self, latitude: f64, longitude: f64) -> Point {
        if !self.reference_set {
            self.airport.reference_latitude = latitude;
            self.airport.reference_longitude = longitude;
            self.reference_set = true;
        }
        let reference_latitude = self.airport.reference_latitude;
        let reference_longitude = self.airport.reference_longitude;
        Point {
            east: (longitude - reference_longitude)
                * 111_320.0
                * (reference_latitude * std::f64::consts::PI / 180.0).cos(),
            north: (latitude - reference_latitude) * 111_320.0,
            height: 0.0,
        }
    }

    // Header row names the airport. Tells us whether this section is ours.
    fn select_header(&mut self, fields: &[&str], line: &str, icao: &str) -> bool {
        if fields.len() < 5 {
            return false;
        }
        let (Ok(elevation), Some(identifier)) = (fields[1].parse::<f64>(), fields.get(4).copied())
        else {
            return false;
        };
        if identifier != icao {
            return false;
        }
        identifier.clone_into(&mut self.airport.icao);
        self.airport.elevation_feet = elevation;
        rest_after_tokens(line, 5).clone_into(&mut self.airport.name);
        true
    }

    // Record 100. Field positions are fixed by the apt.dat spec.
    fn add_runway(&mut self, fields: &[&str]) {
        if fields.len() < 26 {
            return;
        }
        let parsed: Option<(f64, String, f64, f64, String, f64, f64)> = (|| {
            Some((
                fields[1].parse().ok()?,
                fields[8].to_owned(),
                fields[9].parse().ok()?,
                fields[10].parse().ok()?,
                fields[17].to_owned(),
                fields[18].parse().ok()?,
                fields[19].parse().ok()?,
            ))
        })();
        if let Some(runway) = parsed {
            let first = self.project(runway.2, runway.3);
            let second = self.project(runway.5, runway.6);
            self.airport.runways.push(Runway {
                first_name: runway.1,
                second_name: runway.4,
                first,
                second,
                first_displaced_meters: fields[11].parse().unwrap_or(0.0),
                second_displaced_meters: fields[20].parse().unwrap_or(0.0),
                width: runway.0,
            });
        }
    }

    // Record 1201, one taxi node.
    fn add_node(&mut self, fields: &[&str]) {
        if fields.len() < 5 {
            return;
        }
        if let (Ok(latitude), Ok(longitude), Ok(id)) = (
            fields[1].parse::<f64>(),
            fields[2].parse::<f64>(),
            fields[4].parse::<i64>(),
        ) {
            let point = self.project(latitude, longitude);
            self.airport.nodes.push(TaxiNode { id, point });
        }
    }

    // Record 1202, one taxi edge. Size letter hides at the end of the usage.
    fn add_edge(&mut self, fields: &[&str], line: &str) {
        if fields.len() < 5 {
            return;
        }
        if let (Ok(first), Ok(second)) = (fields[1].parse::<i64>(), fields[2].parse::<i64>()) {
            let direction = fields[3];
            let usage = fields[4];
            let size = if usage.starts_with("taxiway_") && usage.len() > 8 {
                usage.chars().last().unwrap_or('F')
            } else {
                'F'
            };
            self.airport.edges.push(TaxiEdge {
                first,
                second,
                name: name_after_tokens(line, 5),
                runway: usage == "runway",
                one_way: direction == "oneway",
                active_runways: String::new(),
                size,
            });
        }
    }

    // Record 1204 appends to whatever edge came last.
    fn extend_edge(&mut self, line: &str) {
        if let Some(edge) = self.airport.edges.last_mut() {
            edge.active_runways += rest_after_tokens(line, 2);
        }
    }

    // Records 1300 and 15, stands. Old rows have no type or equipment.
    fn add_parking(&mut self, record: i32, fields: &[&str], line: &str) {
        if fields.len() < 4 {
            return;
        }
        if let (Ok(latitude), Ok(longitude), Ok(heading)) = (
            fields[1].parse::<f64>(),
            fields[2].parse::<f64>(),
            fields[3].parse::<f64>(),
        ) {
            let (kind, equipment, name) = if record == 1300 && fields.len() >= 6 {
                (
                    fields[4].to_owned(),
                    fields[5].to_owned(),
                    name_after_tokens(line, 6),
                )
            } else {
                (
                    "misc".to_owned(),
                    "unknown".to_owned(),
                    name_after_tokens(line, 4),
                )
            };
            let point = self.project(latitude, longitude);
            self.airport.parking.push(Parking {
                name,
                kind,
                equipment,
                size: String::new(),
                operations: String::new(),
                point,
                heading,
            });
        }
    }

    // Record 1301 fills in the size line for the stand above it.
    fn size_parking(&mut self, fields: &[&str]) {
        if let Some(stand) = self.airport.parking.last_mut()
            && fields.len() >= 3
        {
            fields[1].clone_into(&mut stand.size);
            fields[2].clone_into(&mut stand.operations);
        }
    }

    // Records 50-56 (and the modern 1050s, which win when present).
    fn add_frequency(&mut self, record: i32, fields: &[&str], line: &str) {
        if record >= 1050 && !self.modern_frequencies {
            self.airport.frequencies.clear();
            self.modern_frequencies = true;
        }
        if record < 1050 && self.modern_frequencies {
            return;
        }
        if fields.len() < 2 {
            return;
        }
        if let Ok(frequency) = fields[1].parse::<i32>() {
            const SERVICES: [&str; 7] = [
                "ATIS",
                "Unicom",
                "Clearance",
                "Ground",
                "Tower",
                "Approach",
                "Departure",
            ];
            let index = usize::try_from(record % 1000 - 50).unwrap_or(0);
            self.airport.frequencies.push(Frequency {
                service: SERVICES[index].to_owned(),
                name: name_after_tokens(line, 2),
                khz: if record >= 1000 {
                    frequency
                } else {
                    frequency * 10
                },
            });
        }
    }

    // Checks we actually found the airport, then sorts and dedups frequencies.
    fn finish(mut self) -> Result<Airport, String> {
        if self.airport.icao.is_empty() {
            return Err(crate::dialogue::say(
                "airport_identifier_not_found_in_apt_dat",
                &[],
            ));
        }
        if self.airport.runways.is_empty() {
            return Err(crate::dialogue::say(
                "airport_has_no_supported_land_runways",
                &[],
            ));
        }
        self.airport.frequencies.sort_by(|first, second| {
            (first.service.clone(), first.khz).cmp(&(second.service.clone(), second.khz))
        });
        self.airport
            .frequencies
            .dedup_by(|second, first| first.service == second.service && first.khz == second.khz);
        Ok(self.airport)
    }
}

/// Reads navaids out of `earth_nav.dat`. Only keeps the kinds we care about,
/// and only the ones near our airport (or nailed to it, for ILS bits).
pub fn load_navaids(airport: &mut Airport, text: &str) {
    for raw in text.lines() {
        let fields = tokens(raw);
        if fields.len() < 11 {
            continue;
        }
        let parsed: Option<(i32, f64, f64, f64, f64, f64)> = (|| {
            Some((
                fields[0].parse().ok()?,
                fields[1].parse().ok()?,
                fields[2].parse().ok()?,
                fields[4].parse().ok()?,
                fields[5].parse().ok()?,
                fields[6].parse().ok()?,
            ))
        })();
        let Some((kind, latitude, longitude, frequency, range, bearing)) = parsed else {
            continue;
        };
        if ![2, 3, 4, 5, 6, 7, 8, 9, 12, 13].contains(&kind) {
            continue;
        }
        let identifier = fields[7].to_owned();
        let airport_code = fields[8].to_owned();
        let airport_specific = (4..=9).contains(&kind);
        if airport_specific && airport_code != airport.icao {
            continue;
        }
        // Far-away enroute stuff is someone else's problem.
        if !airport_specific
            && distance_nm(
                airport.reference_latitude,
                airport.reference_longitude,
                latitude,
                longitude,
            ) > 80.0
        {
            continue;
        }
        let mut runway = String::new();
        if airport_specific {
            if fields.len() < 12 {
                continue;
            }
            let runway_field =
                if fields[9].len() == 2 && fields[9].chars().all(|c| c.is_ascii_alphabetic()) {
                    10
                } else {
                    9
                };
            fields[runway_field].clone_into(&mut runway);
        }
        let name = name_after_tokens(
            raw,
            if airport_specific
                && fields[9].len() == 2
                && fields[9].chars().all(|c| c.is_ascii_alphabetic())
            {
                11
            } else {
                10
            },
        );
        let kind_name = match kind {
            2 => "NDB",
            3 => "VOR",
            4 => "ILS LOC",
            5 => "LOC",
            6 => "GS",
            7 => "OM",
            8 => "MM",
            9 => "IM",
            _ => "DME",
        };
        airport.navaids.push(Navaid {
            kind: kind_name.to_owned(),
            identifier,
            name,
            airport: airport_code,
            runway,
            latitude,
            longitude,
            frequency: if kind == 2 {
                frequency
            } else {
                frequency / 100.0
            },
            bearing: if kind == 4 || kind == 5 {
                bearing % 360.0
            } else if kind == 6 {
                bearing % 1000.0
            } else {
                bearing
            },
            elevation_feet: fields[3].parse().unwrap_or(0.0),
            range_nm: range,
            glide_angle: if kind == 6 {
                (bearing / 1000.0).floor() / 100.0
            } else {
                0.0
            },
        });
    }
}

/// SID/STAR/approach headers out of CIFP text. Only the envelope, deduped.
pub fn load_procedures(airport: &mut Airport, text: &str) {
    use std::collections::BTreeSet;
    let mut seen = BTreeSet::new();
    for raw in text.lines() {
        let Some(separator) = raw.find(':') else {
            continue;
        };
        let kind = &raw[..separator];
        if kind != "SID" && kind != "STAR" && kind != "APPCH" {
            continue;
        }
        let mut fields = raw[separator + 1..].split(',');
        let (Some(_sequence), Some(_route), Some(name), Some(transition)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let name = name.trim_matches([' ', '\t']).to_owned();
        let transition = transition.trim_matches([' ', '\t']).to_owned();
        if seen.insert(format!("{kind}/{name}/{transition}")) {
            airport.procedures.push(Procedure {
                kind: kind.to_owned(),
                name,
                transition,
            });
        }
    }
}

/// Hunts down the airport through the scenery packs, then Global, then the
/// defaults, and bolts on navaids and procedures. The `exists`/`read` hooks
/// keep the filesystem out of here so tests can fake a whole simulator.
pub fn load_airport_from_simulator(
    root: &str,
    icao: &str,
    exists: &dyn Fn(&str) -> bool,
    read: &dyn Fn(&str) -> Result<String, String>,
) -> Result<Airport, String> {
    if root.is_empty() {
        return Err(crate::dialogue::say(
            "set_your_x_plane_folder_in_settings",
            &[],
        ));
    }
    let mut paths = Vec::new();
    let packs = format!("{root}/Custom Scenery/scenery_packs.ini");
    if exists(&packs)
        && let Ok(text) = read(&packs)
    {
        for line in text.lines() {
            if let Some(relative) = line.strip_prefix("SCENERY_PACK ") {
                let relative = relative.trim();
                let base = if relative == "*GLOBAL_AIRPORTS*" {
                    format!("{root}/Global Scenery/Global Airports")
                } else if std::path::Path::new(relative).is_relative() {
                    format!("{root}/{relative}")
                } else {
                    relative.to_owned()
                };
                paths.push(format!("{base}/Earth nav data/apt.dat"));
            }
        }
    }
    paths.push(format!(
        "{root}/Global Scenery/Global Airports/Earth nav data/apt.dat"
    ));
    paths.push(format!(
        "{root}/Resources/default scenery/default apt dat/Earth nav data/apt.dat"
    ));
    let mut airport = None;
    for path in &paths {
        if !exists(path) {
            continue;
        }
        // A file that simply names a different airport is fine, keep looking.
        // Anything else going wrong aborts the whole search.
        let text = read(path)?;
        // Scenery selection is structural, never based on editable spoken error text.
        let contains_airport = text.lines().any(|line| {
            let mut fields = line.split_whitespace();
            matches!(fields.next(), Some("1" | "16" | "17")) && fields.nth(3) == Some(icao)
        });
        if !contains_airport {
            continue;
        }
        airport = Some(load_airport(&text, icao)?);
        break;
    }
    let mut airport = airport
        .ok_or_else(|| crate::dialogue::say("airport_not_found_in_enabled_scenery_or", &[]))?;
    let custom = format!("{root}/Custom Data/earth_nav.dat");
    let navdata = if exists(&custom) {
        custom
    } else {
        format!("{root}/Resources/default data/earth_nav.dat")
    };
    if exists(&navdata)
        && let Ok(text) = read(&navdata)
    {
        load_navaids(&mut airport, &text);
    }
    let custom_cifp = format!("{root}/Custom Data/CIFP/{icao}.dat");
    let cifp = if exists(&custom_cifp) {
        custom_cifp
    } else {
        format!("{root}/Resources/default data/CIFP/{icao}.dat")
    };
    if exists(&cifp)
        && let Ok(text) = read(&cifp)
    {
        load_procedures(&mut airport, &text);
    }
    Ok(airport)
}

fn cross(a: Point, b: Point) -> f64 {
    a.east * b.north - a.north * b.east
}
fn difference(a: Point, b: Point) -> Point {
    Point {
        east: a.east - b.east,
        north: a.north - b.north,
        height: 0.0,
    }
}
fn along(a: Point, b: Point, t: f64) -> Point {
    Point {
        east: a.east + (b.east - a.east) * t,
        north: a.north + (b.north - a.north) * t,
        height: 0.0,
    }
}
fn intersection(first: Point, second: Point, third: Point, fourth: Point) -> Option<(f64, f64)> {
    let first_delta = difference(second, first);
    let second_delta = difference(fourth, third);
    let determinant = cross(first_delta, second_delta);
    if determinant.abs() < 1e-8 {
        return None;
    }
    let offset = difference(third, first);
    let first_fraction = cross(offset, second_delta) / determinant;
    let second_fraction = cross(offset, first_delta) / determinant;
    ((0.0..=1.0).contains(&first_fraction) && (0.0..=1.0).contains(&second_fraction))
        .then_some((first_fraction, second_fraction))
}

/// Check actual scenery pavement/runway geometry rather than airport reference-point proximity.
#[must_use]
pub fn contains_aircraft(airport: &Airport, telemetry: &Telemetry) -> bool {
    telemetry.position_valid
        && telemetry.on_ground
        && on_pavement(
            airport,
            airport_point(airport, telemetry.latitude, telemetry.longitude),
        )
}

fn on_pavement(airport: &Airport, p: Point) -> bool {
    if airport.runways.iter().any(|r| {
        let delta = difference(r.second, r.first);
        let offset = difference(p, r.first);
        let length_squared = delta.east * delta.east + delta.north * delta.north;
        let dot = offset.east * delta.east + offset.north * delta.north;
        (0.0..=length_squared).contains(&dot)
            && segment_distance(p, r.first, r.second) <= r.width / 2.0
    }) {
        return true;
    }

    airport.pavement_triangles.iter().any(|t| {
        let signs = [
            cross(difference(t[1], t[0]), difference(p, t[0])),
            cross(difference(t[2], t[1]), difference(p, t[1])),
            cross(difference(t[0], t[2]), difference(p, t[2])),
        ];
        signs.iter().all(|s| *s >= -0.01) || signs.iter().all(|s| *s <= 0.01)
    })
}
/// Use only installed painted centerlines. Never join unrelated lines across an apron,
/// invent taxiway names, cross hold markings, or override a published routing network.
fn infer_centerline_graph(
    airport: &mut Airport,
    lines: &[(Point, Point)],
    holds: &[(Point, Point)],
) {
    let mut splits: Vec<Vec<f64>> = vec![vec![0.0, 1.0]; lines.len()];
    for (i, &(a, b)) in lines.iter().enumerate() {
        for (j, &(c, d)) in lines.iter().enumerate().skip(i + 1) {
            if let Some((t, u)) = intersection(a, b, c, d) {
                splits[i].push(t);
                splits[j].push(u);
            }
        }
        for &(c, d) in holds {
            if let Some((t, _)) = intersection(a, b, c, d) {
                // Stop the aircraft reference point before the marking, allowing room for an A320-size nose.
                let margin = 25.0 / flat_distance(&a, &b).max(1.0);
                splits[i].extend([(t - margin).max(0.0), (t + margin).min(1.0)]);
            }
        }
    }
    for (i, &(a, b)) in lines.iter().enumerate() {
        splits[i].sort_by(f64::total_cmp);
        splits[i].dedup_by(|a, b| (*a - *b).abs() < 1e-8);
        for ts in splits[i].windows(2) {
            let first = along(a, b, ts[0]);
            let second = along(a, b, ts[1]);
            if flat_distance(&first, &second) < 0.01
                || holds
                    .iter()
                    .any(|&(c, d)| intersection(first, second, c, d).is_some())
            {
                continue;
            }
            let steps = (flat_distance(&first, &second) / 5.0).ceil().max(1.0);
            let mut step = 0.0;
            let mut valid = true;
            while step <= steps {
                let point = along(first, second, step / steps);
                if !on_pavement(airport, point)
                    || holds.iter().any(|&(hold_first, hold_second)| {
                        segment_distance(point, hold_first, hold_second) < 24.99
                    })
                {
                    valid = false;
                    break;
                }
                step += 1.0;
            }
            if !valid {
                continue;
            }
            let mut ids = Vec::new();
            for point in [first, second] {
                let id = if let Some(node) = airport
                    .nodes
                    .iter()
                    .find(|n| flat_distance(&n.point, &point) < 0.25)
                {
                    node.id
                } else {
                    let id = i64::try_from(airport.nodes.len()).unwrap_or(i64::MAX) + 1;
                    airport.nodes.push(TaxiNode { id, point });
                    id
                };
                ids.push(id);
            }
            airport.edges.push(TaxiEdge {
                first: ids[0],
                second: ids[1],
                size: 'F',
                ..Default::default()
            });
        }
    }
    airport.taxi_hold_lines = holds.iter().map(|&(a, b)| [a, b]).collect();
    airport.inferred_taxi = !airport.edges.is_empty();
}

fn holds_or_pavement_conflict(airport: &Airport, a: Point, b: Point) -> bool {
    airport
        .taxi_hold_lines
        .iter()
        .any(|h| intersection(a, b, h[0], h[1]).is_some())
        || (0..=20).any(|step| !on_pavement(airport, along(a, b, f64::from(step) / 20.0)))
}

fn runway_projection(point: Point, first: Point, second: Point) -> Point {
    let delta = difference(second, first);
    let length_squared = delta.east * delta.east + delta.north * delta.north;
    let offset = difference(point, first);
    let fraction = if length_squared > 0.0 {
        ((offset.east * delta.east + offset.north * delta.north) / length_squared).clamp(0.0, 1.0)
    } else {
        0.0
    };
    along(first, second, fraction)
}
fn segment_distance(point: Point, first: Point, second: Point) -> f64 {
    flat_distance(&point, &runway_projection(point, first, second))
}
/// A separately authorized runway taxi route. Painted taxiway geometry is preferred;
/// an apron connector is accepted only where installed pavement reaches the runway.
/// No other runway, gap in pavement, or remote painted hold may be crossed.
pub fn calculate_runway_taxi_route(
    airport: &Airport,
    telemetry: &Telemetry,
    destination: &str,
) -> Result<TaxiClearance, String> {
    let failure = || crate::dialogue::say("runway_taxi_unavailable", &[]);
    if !telemetry.on_ground || !telemetry.position_valid {
        return Err(failure());
    }
    let runway = airport
        .runways
        .iter()
        .find(|r| r.first_name == destination || r.second_name == destination)
        .ok_or_else(failure)?;
    let start_point = airport_point(airport, telemetry.latitude, telemetry.longitude);
    // Departure taxi uses the physical runway end, not its displaced landing threshold.
    let threshold = if runway.first_name == destination {
        runway.first
    } else {
        runway.second
    };
    let opposite = if runway.first_name == destination {
        runway.second
    } else {
        runway.first
    };
    let stop = along(
        threshold,
        opposite,
        (30.0 / flat_distance(&threshold, &opposite).max(30.0)).min(1.0),
    );
    let mut surface = airport.clone();
    if airport.inferred_taxi {
        surface.nodes.clear();
        surface.edges.clear();
        let lines: Vec<_> = airport
            .taxi_centerlines
            .iter()
            .map(|p| (p[0], p[1]))
            .collect();
        let holds: Vec<_> = airport
            .taxi_hold_lines
            .iter()
            .filter(|h| {
                h.iter()
                    .any(|p| segment_distance(*p, runway.first, runway.second) > 150.0)
            })
            .map(|h| (h[0], h[1]))
            .collect();
        infer_centerline_graph(&mut surface, &lines, &holds);
    }
    surface
        .runways
        .retain(|r| r.first_name != destination && r.second_name != destination);
    for edge in &mut surface.edges {
        if edge.runway
            && (edge.name == runway.first_name
                || edge.name == runway.second_name
                || edge.name == format!("{}/{}", runway.first_name, runway.second_name))
        {
            edge.runway = false;
        }
        if edge
            .active_runways
            .split([',', ' '])
            .all(|r| r.is_empty() || r == runway.first_name || r == runway.second_name)
        {
            edge.active_runways.clear();
        }
    }
    let nodes: BTreeMap<_, _> = airport.nodes.iter().map(|n| (n.id, n.point)).collect();
    let graph = taxi_graph(&surface, &nodes, 'C');
    let mut points = Vec::new();
    if let Some(start) = nodes
        .keys()
        .filter(|id| graph.contains_key(id))
        .min_by(|a, b| {
            flat_distance(&nodes[a], &start_point)
                .total_cmp(&flat_distance(&nodes[b], &start_point))
        })
    {
        if flat_distance(&nodes[start], &start_point) > 35.0 {
            return Err(failure());
        }
        let (costs, previous) = shortest_paths(&graph, *start);
        let target = costs
            .keys()
            .filter(|node| {
                let point = nodes[node];
                let entry = runway_projection(point, runway.first, runway.second);
                segment_distance(point, runway.first, runway.second) <= 150.0
                    && clear_runway_connector(airport, &surface, runway, point, entry)
            })
            .min_by(|a, b| {
                let score =
                    |id: &i64| costs[id] + segment_distance(nodes[id], runway.first, runway.second);
                score(a).total_cmp(&score(b))
            })
            .ok_or_else(failure)?;
        points = taxi_path(&nodes, &previous, *start, *target).0;
        if !clear_runway_connector(airport, &surface, runway, start_point, points[0]) {
            return Err(failure());
        }
    } else {
        points.push(start_point);
    }
    let end = *points.last().ok_or_else(failure)?;
    let entry = runway_projection(end, runway.first, runway.second);
    if !clear_runway_connector(airport, &surface, runway, end, entry) {
        return Err(failure());
    }
    // This step stays on the assigned runway centerline and rejects overlapping runways.
    if (0..=100).any(|step| within_runway(&surface, &along(entry, stop, f64::from(step) / 100.0))) {
        return Err(failure());
    }
    points.extend([entry, stop]);
    let entry_runway =
        if flat_distance(&entry, &runway.first) <= flat_distance(&entry, &runway.second) {
            runway.first_name.clone()
        } else {
            runway.second_name.clone()
        };
    let instructions = crate::dialogue::say(
        "taxi_via_runway_backtrack",
        &[
            ("entry_runway", entry_runway.clone()),
            ("runway", destination.to_owned()),
        ],
    );
    Ok(TaxiClearance {
        airport: airport.icao.clone(),
        destination: destination.to_owned(),
        instructions,
        via: String::new(),
        points,
        runway_taxi: true,
        entry_runway,
        reference_latitude: airport.reference_latitude,
        reference_longitude: airport.reference_longitude,
        destination_point: stop,
        ..Default::default()
    })
}
fn clear_runway_connector(
    airport: &Airport,
    other: &Airport,
    runway: &Runway,
    first: Point,
    second: Point,
) -> bool {
    let steps = (flat_distance(&first, &second) / 3.0).ceil().max(1.0);
    let mut step = 0.0;
    while step <= steps {
        let point = along(first, second, step / steps);
        if within_runway(other, &point)
            || (!on_pavement(airport, point)
                && segment_distance(point, runway.first, runway.second) > runway.width / 2.0)
        {
            return false;
        }
        step += 1.0;
    }
    !airport.taxi_hold_lines.iter().any(|h| {
        intersection(first, second, h[0], h[1]).is_some()
            && segment_distance(h[0], runway.first, runway.second) > 150.0
    })
}

/// Radius around the route limit used for stopped-aircraft recognition.
pub const HOLDING_MARKER_RADIUS_METRES: f64 = 15.0;

/// Find a holding-line intersection along the final approach, then keep the disc behind it.
#[must_use]
pub fn holding_marker_position(airport: &Airport, points: &[Point]) -> Option<Point> {
    let end = *points.last()?;
    let before = points
        .iter()
        .rev()
        .skip(1)
        .find(|p| flat_distance(p, &end) > 1.0)?;
    let length = flat_distance(before, &end);
    let de = (end.east - before.east) / length;
    let dn = (end.north - before.north) / length;
    let mut stop = 0.0;
    let mut nearest = f64::INFINITY;
    let mut setback = HOLDING_MARKER_RADIUS_METRES + 4.0;
    for line in &airport.taxi_hold_lines {
        let le = line[1].east - line[0].east;
        let ln = line[1].north - line[0].north;
        let cross = de * ln - dn * le;
        if cross.abs() < 0.001 {
            continue;
        }
        let ae = line[0].east - end.east;
        let an = line[0].north - end.north;
        let t = (ae * ln - an * le) / cross;
        let u = (ae * dn - an * de) / cross;
        if (0.0..=1.0).contains(&u) && t.abs() < 80.0 && t.abs() < nearest {
            stop = t;
            nearest = t.abs();
            setback = (HOLDING_MARKER_RADIUS_METRES + 4.0) * le.hypot(ln) / cross.abs();
        }
    }
    let offset = stop - setback;
    Some(Point {
        east: end.east + de * offset,
        north: end.north + dn * offset,
        ..end
    })
}

/// Radius around the route limit used for stopped-aircraft recognition.
pub const HOLD_POINT_RADIUS_METRES: f64 = 45.0;

/// Recognize the route endpoint or its nearby painted holding line on approach.
#[must_use]
pub fn taxi_hold_reached(airport: &Airport, route: &TaxiClearance, telemetry: &Telemetry) -> bool {
    if !telemetry.position_valid
        || !telemetry.on_ground
        || telemetry.paused
        || !route.crossing_runway.is_empty()
    {
        return false;
    }
    let Some(end) = route.points.last() else {
        return false;
    };
    let here = airport_point(airport, telemetry.latitude, telemetry.longitude);
    if flat_distance(&here, end) <= HOLD_POINT_RADIUS_METRES {
        return true;
    }
    !route.to_parking
        && !route.runway_taxi
        && !route.hold_short_runway.is_empty()
        && flat_distance(&here, end) <= 120.0
        && airport.taxi_hold_lines.iter().any(|line| {
            segment_distance(*end, line[0], line[1]) <= 100.0
                && segment_distance(here, line[0], line[1]) <= 25.0
        })
}

/// A crossing finishes once the aircraft is on the opposite side with tail clearance.
#[must_use]
pub fn crossing_vacated(airport: &Airport, route: &TaxiClearance, telemetry: &Telemetry) -> bool {
    if !telemetry.position_valid || !telemetry.on_ground || telemetry.paused {
        return false;
    }
    let Some(runway) = airport
        .runways
        .iter()
        .find(|r| r.first_name == route.crossing_runway || r.second_name == route.crossing_runway)
    else {
        return false;
    };
    let Some(start) = route.points.first() else {
        return false;
    };
    let dx = runway.second.east - runway.first.east;
    let dn = runway.second.north - runway.first.north;
    let length = dx.hypot(dn);
    if length < 100.0 {
        return false;
    }
    let across = |p: Point| {
        ((p.east - runway.first.east) * dn - (p.north - runway.first.north) * dx) / length
    };
    let here = airport_point(airport, telemetry.latitude, telemetry.longitude);
    across(*start) * across(here) < 0.0 && across(here).abs() >= runway.width / 2.0 + 40.0
}

/// Authorize one transverse published-network crossing, ending clear of that runway.
/// Longitudinal runway taxi, unknown links and other runways are excluded.
pub fn calculate_crossing_route(
    airport: &Airport,
    telemetry: &Telemetry,
    runway: &str,
) -> Result<TaxiClearance, String> {
    let fail = || crate::dialogue::say("crossing_route_unavailable", &[]);
    let r = airport
        .runways
        .iter()
        .find(|r| r.first_name == runway || r.second_name == runway)
        .ok_or_else(fail)?;
    let start_point = airport_point(airport, telemetry.latitude, telemetry.longitude);
    let dx = r.second.east - r.first.east;
    let dn = r.second.north - r.first.north;
    let length = dx.hypot(dn);
    if length < 100.0 {
        return Err(fail());
    }
    let across =
        |p: Point| ((p.east - r.first.east) * dn - (p.north - r.first.north) * dx) / length;
    let side = across(start_point);
    if side.abs() < r.width / 2.0 + 10.0 {
        return Err(fail());
    }
    let nodes: BTreeMap<_, _> = airport.nodes.iter().map(|n| (n.id, n.point)).collect();
    let start = nodes
        .iter()
        .filter(|(_, p)| flat_distance(&start_point, p) < 50.0 && across(**p) * side > 0.0)
        .min_by(|a, b| {
            flat_distance(&start_point, a.1).total_cmp(&flat_distance(&start_point, b.1))
        })
        .map(|(id, _)| *id)
        .ok_or_else(fail)?;
    let mut graph: BTreeMap<i64, Vec<(i64, f64, String)>> = BTreeMap::new();
    for e in &airport.edges {
        let (Some(a), Some(b)) = (nodes.get(&e.first), nodes.get(&e.second)) else {
            continue;
        };
        if e.size < 'C' || flat_distance(a, b) > 300.0 {
            continue;
        }
        let edge_length = flat_distance(a, b);
        let transverse = ((b.east - a.east) * dx + (b.north - a.north) * dn).abs() / length;
        if e.runway && transverse > edge_length * 0.7 {
            continue;
        }
        if airport
            .runways
            .iter()
            .filter(|other| other.first_name != r.first_name || other.second_name != r.second_name)
            .any(|other| {
                let steps = (edge_length / 10.0).ceil().max(1.0);
                let mut step = 0.0;
                while step <= steps {
                    let point = Point {
                        east: a.east + (b.east - a.east) * step / steps,
                        north: a.north + (b.north - a.north) * step / steps,
                        height: 0.0,
                    };
                    if segment_distance(point, other.first, other.second) < other.width / 2.0 + 15.0
                    {
                        return true;
                    }
                    step += 1.0;
                }
                false
            })
        {
            continue;
        }
        let crossing_names: Vec<_> = e
            .active_runways
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|s| !s.is_empty())
            .collect();
        if crossing_names.iter().any(|n| {
            n.trim_start_matches('0') != r.first_name.trim_start_matches('0')
                && n.trim_start_matches('0') != r.second_name.trim_start_matches('0')
        }) {
            continue;
        }
        graph
            .entry(e.first)
            .or_default()
            .push((e.second, edge_length, e.name.clone()));
        if !e.one_way {
            graph
                .entry(e.second)
                .or_default()
                .push((e.first, edge_length, e.name.clone()));
        }
    }
    let (costs, previous) = shortest_paths(&graph, start);
    let target = costs
        .iter()
        .filter(|(id, cost)| {
            **cost < 500.0
                && across(nodes[id]) * side < 0.0
                && across(nodes[id]).abs() > r.width / 2.0 + 50.0
        })
        .min_by(|a, b| a.1.total_cmp(b.1))
        .map(|(id, _)| *id)
        .ok_or_else(fail)?;
    let (points, _via) = taxi_path(&nodes, &previous, start, target);
    if !points
        .windows(2)
        .any(|pair| across(pair[0]) * across(pair[1]) <= 0.0)
    {
        return Err(fail());
    }
    Ok(TaxiClearance {
        airport: airport.icao.clone(),
        destination: runway.to_owned(),
        crossing_runway: runway.to_owned(),
        instructions: crate::dialogue::say("crossing_clearance", &[("runway", runway.to_owned())]),
        via: String::new(),
        points,
        reference_latitude: airport.reference_latitude,
        reference_longitude: airport.reference_longitude,
        ..Default::default()
    })
}

#[cfg(test)]
mod pavement_tests {
    use super::*;
    #[test]
    fn triangulation_preserves_a_pavement_island() {
        let point = |east, north| Point {
            east,
            north,
            height: 0.0,
        };
        let contours = vec![
            vec![
                point(0.0, 0.0),
                point(10.0, 0.0),
                point(10.0, 10.0),
                point(0.0, 10.0),
            ],
            vec![
                point(2.0, 2.0),
                point(2.0, 8.0),
                point(8.0, 8.0),
                point(8.0, 2.0),
            ],
        ];
        let area: f64 = pavement_faces(&contours)
            .iter()
            .map(|v| {
                ((v[1].east - v[0].east) * (v[2].north - v[0].north)
                    - (v[2].east - v[0].east) * (v[1].north - v[0].north))
                    .abs()
                    * 0.5
            })
            .sum();
        assert!((area - 64.0).abs() < 1e-8);
    }
}
