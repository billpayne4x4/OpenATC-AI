//! Airport map layers, 2D pan/zoom and the 3D orbit view.

use crate::interface::Interface;
use openatc_core::airport::{Airport, airport_point};
use openatc_core::state::{Point, State};

/// 8-bit color helper standing in for `IM_COL32`.
fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> [f32; 4] {
    [
        f32::from(red) / 255.0,
        f32::from(green) / 255.0,
        f32::from(blue) / 255.0,
        f32::from(alpha) / 255.0,
    ]
}

/// Aircraft triangle with its 19-pixel ring, rotated by heading.
fn aircraft_symbol(draw: &imgui::DrawListMut<'_>, center: [f32; 2], heading: f64, color: [f32; 4]) {
    // Headings are small whole degrees; pixels cannot tell the difference.
    #[allow(clippy::cast_possible_truncation)]
    let angle = (heading as f32) * std::f32::consts::PI / 180.0;
    let point = |east: f32, north: f32| {
        [
            center[0] + east * angle.cos() + north * angle.sin(),
            center[1] + east * angle.sin() - north * angle.cos(),
        ]
    };
    draw.add_triangle(point(0.0, 13.0), point(-8.0, -9.0), point(0.0, -5.0), color)
        .filled(true)
        .build();
    draw.add_triangle(point(0.0, 13.0), point(0.0, -5.0), point(8.0, -9.0), color)
        .filled(true)
        .build();
    draw.add_circle(center, 19.0, rgba(220, 241, 252, 70))
        .thickness(1.0)
        .build();
}

/// Airport canvas: grid, runways, taxiways, approved route, stands, buildings,
/// navaids, ownship, scale bar. `on_airports` selects the orbit behavior that
/// only the Airports page gets.
pub fn airport_canvas(
    ui: &imgui::Ui,
    interface: &mut Interface,
    state: &State,
    on_airports: bool,
    height: f32,
) {
    let cursor = ui.cursor_screen_pos();
    let avail = ui.content_region_avail();
    let size = [
        avail[0],
        if height > 0.0 {
            height
        } else {
            (avail[1] - 42.0).max(180.0)
        },
    ];
    ui.invisible_button_flags(
        "AirportCanvas",
        size,
        imgui::ButtonFlags::MOUSE_BUTTON_LEFT | imgui::ButtonFlags::MOUSE_BUTTON_RIGHT,
    );
    let input = ui.io();
    let hovered = ui.is_item_hovered();
    if hovered {
        interface.zoom = (interface.zoom * 1.12f32.powf(input.mouse_wheel)).clamp(0.2, 18.0);
        if on_airports && interface.orbit_view && ui.is_mouse_dragging(imgui::MouseButton::Left) {
            interface.yaw += input.mouse_delta[0] * 0.008;
            interface.pitch = (interface.pitch + input.mouse_delta[1] * 0.008).clamp(0.2, 1.5);
        } else if ui.is_mouse_dragging(imgui::MouseButton::Left)
            || ui.is_mouse_dragging(imgui::MouseButton::Right)
        {
            interface.pan[0] += input.mouse_delta[0];
            interface.pan[1] += input.mouse_delta[1];
        }
    }
    let draw = ui.get_window_draw_list();
    draw.with_clip_rect(cursor, [cursor[0] + size[0], cursor[1] + size[1]], || {
        draw.add_rect(
            cursor,
            [cursor[0] + size[0], cursor[1] + size[1]],
            rgba(18, 28, 36, 255),
        )
        .filled(true)
        .rounding(7.0)
        .build();
        draw_map(
            ui,
            &draw,
            interface,
            state,
            &Canvas {
                on_airports,
                cursor,
                size,
                hovered,
            },
        );
    });
    if interface.airport.inferred_taxi {
        ui.text_colored(
            [1.0, 0.75, 0.3, 1.0],
            "Taxi guidance uses scenery centerlines; published ATC routes are unavailable.",
        );
    }
    ui.text_wrapped(format!(
        "{} / {} parking positions / dashed amber: unverified ramp connector",
        interface.airport.icao,
        interface.airport.parking.len()
    ));
}

/// Canvas frame for one map draw: where it sits and how it behaves.
struct Canvas {
    /// True on the Airports page (orbit allowed).
    on_airports: bool,
    /// Top-left corner in screen space.
    cursor: [f32; 2],
    /// Canvas size.
    size: [f32; 2],
    /// Mouse is over the canvas.
    hovered: bool,
}

#[allow(clippy::too_many_lines)]
fn draw_map(
    ui: &imgui::Ui,
    draw: &imgui::DrawListMut<'_>,
    interface: &Interface,
    state: &State,
    canvas: &Canvas,
) {
    let airport = &interface.airport;
    let (mut minimum_east, mut maximum_east) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut minimum_north, mut maximum_north) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut include = |point: &Point| {
        minimum_east = minimum_east.min(point.east);
        maximum_east = maximum_east.max(point.east);
        minimum_north = minimum_north.min(point.north);
        maximum_north = maximum_north.max(point.north);
    };
    for runway in &airport.runways {
        include(&runway.first);
        include(&runway.second);
    }
    for contour in &airport.pavements {
        for point in contour {
            include(point);
        }
    }
    for parking in &airport.parking {
        include(&parking.point);
    }
    for node in &airport.nodes {
        include(&node.point);
    }
    let beams = if canvas.on_airports && interface.settings.show_ils_beams {
        approach_beams(airport)
    } else {
        Vec::new()
    };
    for beam in &beams {
        for point in &beam.corners {
            include(point);
        }
    }
    if minimum_east > maximum_east {
        minimum_east = -500.0;
        minimum_north = -500.0;
        maximum_east = 500.0;
        maximum_north = 500.0;
    }
    let center_east = f64::midpoint(minimum_east, maximum_east);
    let center_north = f64::midpoint(minimum_north, maximum_north);
    // Screen pixels: f32 holds whole pixels exactly well past 4K widths.
    #[allow(clippy::cast_possible_truncation)]
    let scale = ((f64::from(canvas.size[0]) - 100.0) / 1000.0f64.max(maximum_east - minimum_east))
        .min((f64::from(canvas.size[1]) - 90.0) / 1000.0f64.max(maximum_north - minimum_north))
        as f32
        * interface.zoom;
    let angle = if canvas.on_airports && interface.orbit_view {
        f64::from(interface.yaw)
    } else {
        0.0
    };
    let tilt = if canvas.on_airports && interface.orbit_view {
        f64::from(interface.pitch)
    } else {
        std::f64::consts::FRAC_PI_2
    };
    // Same story: the projection lands on whole pixels.
    #[allow(clippy::cast_possible_truncation)]
    // Same story: the projection lands on whole pixels.
    #[allow(clippy::cast_possible_truncation)]
    let project = |point: &Point| {
        let east = point.east - center_east;
        let north = point.north - center_north;
        [
            canvas.cursor[0]
                + canvas.size[0] / 2.0
                + interface.pan[0]
                + (east * angle.cos() - north * angle.sin()) as f32 * scale,
            canvas.cursor[1] + canvas.size[1] / 2.0 + interface.pan[1]
                - ((east * angle.sin() + north * angle.cos()) * tilt.sin()
                    + point.height * tilt.cos()) as f32
                    * scale,
        ]
    };
    let project_raw = |east: f64, north: f64| {
        project(&Point {
            east,
            north,
            height: 0.0,
        })
    };
    for grid in -12..=12 {
        let offset = f64::from(grid) * 500.0;
        draw.add_line(
            project_raw(center_east + offset, center_north - 6000.0),
            project_raw(center_east + offset, center_north + 6000.0),
            rgba(32, 47, 58, 255),
        )
        .build();
        draw.add_line(
            project_raw(center_east - 6000.0, center_north + offset),
            project_raw(center_east + 6000.0, center_north + offset),
            rgba(32, 47, 58, 255),
        )
        .build();
    }
    if interface.settings.show_taxiways {
        for triangle in &airport.pavement_triangles {
            draw.add_triangle(
                project(&triangle[0]),
                project(&triangle[1]),
                project(&triangle[2]),
                rgba(38, 53, 65, 255),
            )
            .filled(true)
            .build();
        }
        for contour in &airport.pavements {
            let mut points: Vec<_> = contour.iter().map(&project).collect();
            if let Some(first) = points.first().copied() {
                points.push(first);
            }
            draw.add_polyline(points, rgba(65, 83, 95, 255))
                .thickness(1.5)
                .build();
        }
    }
    if interface.show_runways {
        for runway in &airport.runways {
            let east = runway.second.east - runway.first.east;
            let north = runway.second.north - runway.first.north;
            let length = east.hypot(north);
            if length < 1.0 {
                continue;
            }
            let normal_east = -north / length * runway.width / 2.0;
            let normal_north = east / length * runway.width / 2.0;
            let corners = vec![
                project(&Point {
                    east: runway.first.east + normal_east,
                    north: runway.first.north + normal_north,
                    height: 0.0,
                }),
                project(&Point {
                    east: runway.second.east + normal_east,
                    north: runway.second.north + normal_north,
                    height: 0.0,
                }),
                project(&Point {
                    east: runway.second.east - normal_east,
                    north: runway.second.north - normal_north,
                    height: 0.0,
                }),
                project(&Point {
                    east: runway.first.east - normal_east,
                    north: runway.first.north - normal_north,
                    height: 0.0,
                }),
            ];
            draw.add_polyline(corners.clone(), rgba(71, 84, 99, 255))
                .filled(true)
                .build();
            let mut outline = corners;
            outline.push(outline[0]);
            draw.add_polyline(outline, rgba(167, 187, 200, 220))
                .thickness(1.5)
                .build();
            draw.add_line(
                project(&runway.first),
                project(&runway.second),
                rgba(218, 226, 234, 230),
            )
            .thickness(1.0)
            .build();
            if interface.settings.show_labels {
                draw.add_text(
                    project(&runway.first),
                    rgba(255, 210, 133, 255),
                    runway.first_name.clone(),
                );
                draw.add_text(
                    project(&runway.second),
                    rgba(255, 210, 133, 255),
                    runway.second_name.clone(),
                );
            }
        }
    }
    let mut nodes = std::collections::BTreeMap::new();
    for node in &airport.nodes {
        nodes.insert(node.id, node.point);
    }
    let mut taxi_labels = std::collections::BTreeSet::new();
    if interface.settings.show_taxiways {
        for edge in &airport.edges {
            let (Some(first), Some(second)) = (nodes.get(&edge.first), nodes.get(&edge.second))
            else {
                continue;
            };
            let (first, second) = (project(first), project(second));
            draw.add_line(
                first,
                second,
                if edge.runway {
                    rgba(176, 99, 94, 210)
                } else {
                    rgba(75, 112, 126, 255)
                },
            )
            .thickness(2.0f32.max(18.0 * scale))
            .build();
            if interface.settings.show_labels
                && interface.zoom > 0.65
                && !edge.name.is_empty()
                && taxi_labels.insert(edge.name.clone())
            {
                draw.add_text(
                    [
                        f32::midpoint(first[0], second[0]),
                        f32::midpoint(first[1], second[1]),
                    ],
                    rgba(153, 188, 202, 255),
                    edge.name.clone(),
                );
            }
        }
    }
    if interface.settings.show_taxi_route
        && (state.taxi_clearance.approved || state.taxi_clearance.pending_readback)
        && state.taxi_clearance.airport == airport.icao
    {
        draw_route(draw, interface, state, airport, &project);
    }
    if interface.settings.show_parking {
        draw_parking(ui, draw, interface, canvas.hovered, &project);
    }
    if canvas.on_airports
        && interface.orbit_view
        && interface.show_buildings
        && airport.icao == "DEMO"
    {
        draw_buildings(draw, &project, angle);
    }
    if interface.show_navaids && canvas.on_airports {
        draw_navaids(draw, interface, airport, &project);
    }
    for beam in &beams {
        draw_approach_beam(
            draw,
            beam,
            &project,
            canvas.on_airports && interface.orbit_view,
            [
                canvas.cursor[0] + 8.0,
                canvas.cursor[0] + canvas.size[0] - 8.0,
            ],
            ui.calc_text_size(&beam.label)[0],
        );
    }
    draw_ownship(ui, draw, interface, state, airport, canvas.cursor, &project);
    draw.add_text(
        [canvas.cursor[0] + 16.0, canvas.cursor[1] + 14.0],
        rgba(163, 188, 207, 255),
        if canvas.on_airports && interface.orbit_view {
            "3D / drag to orbit / right-drag to pan"
        } else {
            "NORTH UP / wheel to zoom / drag to pan"
        },
    );
    let bar = (canvas.size[0] * 0.25).min(500.0 * scale);
    draw.add_line(
        [
            canvas.cursor[0] + 20.0,
            canvas.cursor[1] + canvas.size[1] - 24.0,
        ],
        [
            canvas.cursor[0] + 20.0 + bar,
            canvas.cursor[1] + canvas.size[1] - 24.0,
        ],
        rgba(198, 214, 227, 255),
    )
    .thickness(2.0)
    .build();
    draw.add_text(
        [
            canvas.cursor[0] + 22.0,
            canvas.cursor[1] + canvas.size[1] - 48.0,
        ],
        rgba(198, 214, 227, 255),
        format!("{:.0} m", bar / scale),
    );
}

/// Approved taxi route with the dashed amber connector from ownship.
fn draw_route(
    draw: &imgui::DrawListMut<'_>,
    interface: &Interface,
    state: &State,
    airport: &Airport,
    project: &dyn Fn(&Point) -> [f32; 2],
) {
    let points = &state.taxi_clearance.points;
    if state.telemetry.position_valid && !points.is_empty() {
        let current = project(&airport_point(
            airport,
            state.telemetry.latitude,
            state.telemetry.longitude,
        ));
        let first = project(&points[0]);
        let length = (first[0] - current[0]).hypot(first[1] - current[1]);
        if length > 1.0 && f64::from(length) < 350.0 * f64::from(interface.zoom.max(0.2)) {
            let mut offset = 0.0;
            while offset < length {
                let end = length.min(offset + 6.0);
                draw.add_line(
                    [
                        current[0] + (first[0] - current[0]) * offset / length,
                        current[1] + (first[1] - current[1]) * offset / length,
                    ],
                    [
                        current[0] + (first[0] - current[0]) * end / length,
                        current[1] + (first[1] - current[1]) * end / length,
                    ],
                    rgba(255, 192, 87, 255),
                )
                .thickness(2.0)
                .build();
                offset += 12.0;
            }
        }
    }
    for index in 1..points.len() {
        draw.add_line(
            project(&points[index - 1]),
            project(&points[index]),
            rgba(20, 54, 49, 255),
        )
        .thickness(9.0)
        .build();
        draw.add_line(
            project(&points[index - 1]),
            project(&points[index]),
            if state.taxi_clearance.pending_readback {
                rgba(255, 192, 87, 255)
            } else {
                rgba(64, 223, 168, 255)
            },
        )
        .thickness(4.0)
        .build();
    }
    if interface.settings.show_taxi_arrows {
        for pair in points.windows(2) {
            let a = project(&pair[0]);
            let b = project(&pair[1]);
            let length = (b[0] - a[0]).hypot(b[1] - a[1]);
            if length < 18.0 {
                continue;
            }
            let direction = [(b[0] - a[0]) / length, (b[1] - a[1]) / length];
            let mut distance = 14.0;
            while distance < length - 8.0 {
                let tip = [
                    a[0] + direction[0] * distance,
                    a[1] + direction[1] * distance,
                ];
                for side in [-1.0, 1.0] {
                    draw.add_line(
                        [
                            tip[0] - direction[0] * 7.0 - direction[1] * side * 5.0,
                            tip[1] - direction[1] * 7.0 + direction[0] * side * 5.0,
                        ],
                        tip,
                        rgba(220, 255, 245, 255),
                    )
                    .thickness(2.5)
                    .build();
                }
                distance += 42.0;
            }
        }
    }
    if let Some(endpoint) = points.last().filter(|_| {
        interface.settings.show_holding_point
            && (state.taxi_clearance.pending_readback
                || matches!(
                    state.phase,
                    openatc_core::state::PhaseCode::Taxi | openatc_core::state::PhaseCode::TaxiIn
                ))
            && !state.taxi_clearance.hold_short_runway.is_empty()
            && state.taxi_clearance.crossing_runway.is_empty()
    }) {
        let endpoint = state
            .taxi_clearance
            .holding_marker_point
            .as_ref()
            .unwrap_or(endpoint);
        let edge = project(&Point {
            east: endpoint.east + openatc_core::airport::HOLDING_MARKER_RADIUS_METRES,
            ..*endpoint
        });
        let endpoint = project(endpoint);
        draw.add_circle(
            endpoint,
            (edge[0] - endpoint[0]).hypot(edge[1] - endpoint[1]),
            rgba(255, 192, 87, 45),
        )
        .filled(true)
        .build();
        draw.add_circle(endpoint, 9.0, rgba(255, 192, 87, 255))
            .thickness(3.0)
            .build();
        let label = [endpoint[0] - 22.0, endpoint[1] - 39.0];
        draw.add_rect(
            [label[0] - 4.0, label[1] - 3.0],
            [label[0] + 48.0, label[1] + 20.0],
            rgba(18, 28, 36, 240),
        )
        .filled(true)
        .rounding(4.0)
        .build();
        draw.add_text(label, rgba(255, 192, 87, 255), "HOLD");
    }
}

/// Parking stands with hover tooltips.
fn draw_parking(
    ui: &imgui::Ui,
    draw: &imgui::DrawListMut<'_>,
    interface: &Interface,
    hovered: bool,
    project: &dyn Fn(&Point) -> [f32; 2],
) {
    let mouse = ui.io().mouse_pos;
    let mut label_boxes: Vec<[f32; 4]> = Vec::new();
    for parking in &interface.airport.parking {
        let position = project(&parking.point);
        let color = if parking.kind == "gate" {
            rgba(115, 175, 235, 255)
        } else {
            rgba(170, 145, 213, 255)
        };
        draw.add_rect(
            [position[0] - 5.0, position[1] - 5.0],
            [position[0] + 5.0, position[1] + 5.0],
            color,
        )
        .filled(true)
        .rounding(2.0)
        .build();
        let near_mouse = hovered && (mouse[0] - position[0]).hypot(mouse[1] - position[1]) < 13.0;
        let label = parking
            .name
            .rsplit_once(" - ")
            .map_or(parking.name.as_str(), |(_, v)| v);
        let size = ui.calc_text_size(label);
        let bounds = [
            position[0] + 8.0,
            position[1] - 8.0,
            position[0] + 12.0 + size[0],
            position[1] + size[1] + 2.0,
        ];
        let overlaps = label_boxes
            .iter()
            .any(|b| bounds[0] < b[2] && bounds[2] > b[0] && bounds[1] < b[3] && bounds[3] > b[1]);
        if interface.settings.show_labels && (!overlaps || near_mouse) {
            draw.add_rect(
                [bounds[0] - 3.0, bounds[1] - 2.0],
                [bounds[2], bounds[3] - 3.0],
                rgba(18, 28, 36, 225),
            )
            .filled(true)
            .rounding(3.0)
            .build();
            draw.add_text([bounds[0], bounds[1]], rgba(214, 226, 237, 255), label);
            label_boxes.push(bounds);
        }
        if hovered && (mouse[0] - position[0]).hypot(mouse[1] - position[1]) < 13.0 {
            let _tooltip = ui.begin_tooltip();
            ui.text(format!("{} / {}", parking.name, parking.kind));
            ui.text(format!("Size {} / {}", parking.size, parking.equipment));
        }
    }
}

/// Demo terminal buildings, painter-sorted back to front.
fn draw_buildings(draw: &imgui::DrawListMut<'_>, project: &dyn Fn(&Point) -> [f32; 2], angle: f64) {
    struct Face {
        corners: [[f64; 3]; 4],
        color: [f32; 4],
        depth: f64,
    }
    let boxes =
        |east: f64, north: f64, width: f64, depth: f64, height: f64, faces: &mut Vec<Face>| {
            let points = [
                [east, north, 0.0],
                [east + width, north, 0.0],
                [east + width, north + depth, 0.0],
                [east, north + depth, 0.0],
                [east, north, height],
                [east + width, north, height],
                [east + width, north + depth, height],
                [east, north + depth, height],
            ];
            let indices = [
                [0, 1, 5, 4],
                [1, 2, 6, 5],
                [2, 3, 7, 6],
                [3, 0, 4, 7],
                [4, 5, 6, 7],
            ];
            for (side, index) in indices.iter().enumerate() {
                let mut face = Face {
                    corners: [[0.0; 3]; 4],
                    color: if side == 4 {
                        rgba(118, 152, 165, 255)
                    } else {
                        let side = u8::try_from(side).unwrap_or(0);
                        rgba(49 + side * 9, 75 + side * 8, 90 + side * 8, 255)
                    },
                    depth: 0.0,
                };
                for (corner, at) in index.iter().enumerate() {
                    face.corners[corner] = points[*at];
                    face.depth += points[*at][0] * angle.sin() + points[*at][1] * angle.cos();
                }
                faces.push(face);
            }
        };
    let mut faces = Vec::new();
    boxes(-450.0, 480.0, 850.0, 120.0, 65.0, &mut faces);
    boxes(-400.0, 330.0, 70.0, 150.0, 35.0, &mut faces);
    boxes(260.0, 330.0, 70.0, 150.0, 35.0, &mut faces);
    boxes(550.0, 450.0, 60.0, 65.0, 150.0, &mut faces);
    faces.sort_by(|first, second| {
        second
            .depth
            .partial_cmp(&first.depth)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for face in &faces {
        let corners: Vec<[f32; 2]> = face
            .corners
            .iter()
            .map(|corner| {
                project(&Point {
                    east: corner[0],
                    north: corner[1],
                    height: corner[2],
                })
            })
            .collect();
        draw.add_polyline(corners, face.color).filled(true).build();
    }
}

/// Navaids near the airport with labels.
fn draw_navaids(
    draw: &imgui::DrawListMut<'_>,
    interface: &Interface,
    airport: &Airport,
    project: &dyn Fn(&Point) -> [f32; 2],
) {
    let mut labels: Vec<[f32; 2]> = Vec::new();
    for navaid in &airport.navaids {
        let position = project(&airport_point(airport, navaid.latitude, navaid.longitude));
        draw.add_circle(position, 5.0, rgba(202, 160, 241, 255))
            .thickness(2.0)
            .build();
        if interface.settings.show_labels
            && !labels
                .iter()
                .any(|p| (p[0] - position[0]).abs() < 70.0 && (p[1] - position[1]).abs() < 22.0)
        {
            labels.push(position);
            draw.add_text(
                [position[0] + 7.0, position[1]],
                rgba(202, 160, 241, 255),
                navaid.identifier.clone(),
            );
        }
    }
}

/// Published runway course and glide angle, in an airport-relative frame.
struct ApproachBeam {
    corners: [Point; 4],
    start: Point,
    end: Point,
    label: String,
}
fn approach_beams(airport: &Airport) -> Vec<ApproachBeam> {
    airport
        .navaids
        .iter()
        .filter(|n| n.kind == "GS" && (1.0..=10.0).contains(&n.glide_angle))
        .filter_map(|gs| {
            let runway = airport
                .runways
                .iter()
                .find(|r| r.first_name == gs.runway || r.second_name == gs.runway)?;
            let start = runway.landing_threshold(&gs.runway)?;
            let bearing = gs.bearing.to_radians();
            let direction = [-bearing.sin(), -bearing.cos()];
            let cross = [-direction[1], direction[0]];
            let length = 7408.0;
            let glide = gs.glide_angle.to_radians().tan();
            let point = |distance: f64, side: f64| Point {
                east: start.east + direction[0] * distance + cross[0] * side,
                north: start.north + direction[1] * distance + cross[1] * side,
                height: 15.0 + distance * glide,
            };
            let width = length * 2.5_f64.to_radians().tan();
            Some(ApproachBeam {
                corners: [
                    point(0.0, -20.0),
                    point(length, -width),
                    point(length, width),
                    point(0.0, 20.0),
                ],
                start: point(0.0, 0.0),
                end: point(length, 0.0),
                label: format!(
                    "ILS {}  /  {:.2}°  /  {:.2} MHz",
                    gs.runway, gs.glide_angle, gs.frequency
                ),
            })
        })
        .collect()
}
fn draw_approach_beam(
    draw: &imgui::DrawListMut<'_>,
    beam: &ApproachBeam,
    project: &dyn Fn(&Point) -> [f32; 2],
    three_d: bool,
    horizontal_bounds: [f32; 2],
    label_width: f32,
) {
    let corners: Vec<_> = beam.corners.iter().map(project).collect();
    draw.add_polyline(corners.clone(), rgba(42, 198, 229, 40))
        .filled(true)
        .build();
    let mut outline = corners;
    outline.push(outline[0]);
    draw.add_polyline(outline, rgba(70, 217, 242, 175))
        .thickness(1.5)
        .build();
    draw.add_line(
        project(&beam.start),
        project(&beam.end),
        rgba(180, 247, 255, 230),
    )
    .thickness(2.0)
    .build();
    for step in 1..=4 {
        let fraction = f64::from(step) / 4.0;
        let point = |side: usize| Point {
            east: beam.corners[side].east
                + (beam.corners[side + 1].east - beam.corners[side].east) * fraction,
            north: beam.corners[side].north
                + (beam.corners[side + 1].north - beam.corners[side].north) * fraction,
            height: beam.start.height + (beam.end.height - beam.start.height) * fraction,
        };
        // Both sides run from the runway threshold towards the approach.
        let left = point(0);
        let right = Point {
            east: beam.corners[3].east + (beam.corners[2].east - beam.corners[3].east) * fraction,
            north: beam.corners[3].north
                + (beam.corners[2].north - beam.corners[3].north) * fraction,
            height: left.height,
        };
        draw.add_line(project(&left), project(&right), rgba(95, 213, 236, 90))
            .build();
    }
    if three_d {
        let mut ground = beam.end;
        ground.height = 0.0;
        draw.add_line(
            project(&ground),
            project(&beam.end),
            rgba(70, 217, 242, 150),
        )
        .thickness(1.0)
        .build();
        let lower = |mut p: Point| {
            p.height = (p.height - 15.0) * 0.88 + 15.0;
            p
        };
        let upper = |mut p: Point| {
            p.height = (p.height - 15.0) * 1.12 + 15.0;
            p
        };
        for side in [0, 3] {
            let end = if side == 0 {
                beam.corners[1]
            } else {
                beam.corners[2]
            };
            draw.add_polyline(
                vec![
                    project(&beam.corners[side]),
                    project(&lower(end)),
                    project(&upper(end)),
                ],
                rgba(42, 198, 229, 35),
            )
            .filled(true)
            .build();
        }
    }
    let end = project(&beam.end);
    draw.add_text(
        [
            (end[0] + 10.0).clamp(
                horizontal_bounds[0],
                (horizontal_bounds[1] - label_width).max(horizontal_bounds[0]),
            ),
            end[1] - 22.0,
        ],
        rgba(151, 236, 249, 255),
        &beam.label,
    );
}

/// Ownship triangle or the outside-area/unavailable notes.
fn draw_ownship(
    ui: &imgui::Ui,
    draw: &imgui::DrawListMut<'_>,
    interface: &Interface,
    state: &State,
    airport: &Airport,
    cursor: [f32; 2],
    project: &dyn Fn(&Point) -> [f32; 2],
) {
    if !interface.settings.show_ownship {
        return;
    }
    if !state.telemetry.position_valid {
        draw.add_text(
            [cursor[0] + 16.0, cursor[1] + 44.0],
            rgba(160, 177, 193, 255),
            "Aircraft position unavailable",
        );
        return;
    }
    let point = airport_point(airport, state.telemetry.latitude, state.telemetry.longitude);
    if point.east.hypot(point.north) < 100_000.0 {
        aircraft_symbol(
            draw,
            project(&point),
            state.telemetry.heading_degrees,
            rgba(246, 251, 255, 255),
        );
    } else {
        draw.add_text(
            [cursor[0] + 16.0, cursor[1] + 44.0],
            rgba(245, 190, 100, 255),
            "Aircraft is outside this airport area",
        );
    }
    let _ = ui;
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod beam_tests {
    use super::*;
    use openatc_core::airport::{Navaid, Runway};
    #[test]
    fn glide_beam_rises_outward_from_the_published_threshold() {
        let airport = Airport {
            runways: vec![Runway {
                first_name: "32L".into(),
                first: Point {
                    east: 100.0,
                    north: 200.0,
                    height: 0.0,
                },
                ..Runway::default()
            }],
            navaids: vec![Navaid {
                kind: "GS".into(),
                runway: "32L".into(),
                bearing: 327.372,
                glide_angle: 3.0,
                frequency: 109.5,
                ..Navaid::default()
            }],
            ..Airport::default()
        };
        let beam = approach_beams(&airport).remove(0);
        assert_eq!(beam.start.east, 100.0);
        assert_eq!(beam.start.north, 200.0);
        assert!(beam.end.east > beam.start.east);
        assert!(beam.end.north < beam.start.north);
        assert!((beam.end.height - 403.23).abs() < 1.0);
        assert!(approach_beams(&Airport::default()).is_empty());
    }
}
