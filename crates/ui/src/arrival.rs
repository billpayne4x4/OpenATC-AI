//! Arrival profile rendering; geometry and descent calculations live in core.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
use crate::{interface::Interface, widgets};
use openatc_core::{
    arrival::{self, Target},
    resolve_units,
    state::State,
};
const GROUND: [f32; 4] = [0.65, 0.75, 0.61, 1.0];
const SAFE: [f32; 4] = [0.9, 0.48, 0.47, 1.0];
const CURRENT: [f32; 4] = [1.0, 0.76, 0.38, 1.0];
const ILS: [f32; 4] = [0.73, 0.64, 0.98, 1.0];
const TRANSITION: [f32; 4] = [0.59, 0.68, 0.81, 1.0];

pub fn draw(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    let units = resolve_units(
        &format!("{:?}", interface.settings.units).to_lowercase(),
        &state.plan.destination,
    );
    let dest = &state.plan.destination;
    if dest.is_empty() {
        ui.text_wrapped(
            "Set your destination and arrival runway in Flight Plan to show the arrival profile.",
        );
        return;
    }
    ui.text_colored(
        widgets::ACCENT,
        format!(
            "{}  /  RWY {}  /  {}  {}",
            dest, state.plan.arrival_runway, state.plan.star, state.plan.approach
        ),
    );
    if interface.arrival.airport.icao != *dest {
        ui.text_wrapped(if interface.arrival_loading {
            "Loading destination runway and approach data…"
        } else if interface.arrival_notice.is_empty() {
            "Waiting for destination airport data…"
        } else {
            &interface.arrival_notice
        });
        return;
    }
    let Some(mut target) = arrival::target(
        &interface.arrival,
        &state.plan.arrival_runway,
        &state.plan.approach,
    ) else {
        ui.text_wrapped("The arrival runway is not in this airport's installed scenery. Choose its runway in Flight Plan.");
        return;
    };
    if !state.telemetry.position_valid {
        ui.text_wrapped("Waiting for a valid aircraft position. The profile will run from your aircraft to the arrival runway.");
        return;
    }
    let key = format!("{dest}/{}", target.runway);
    let terrain = if interface.arrival_terrain.key == key {
        Some(&interface.arrival_terrain)
    } else {
        None
    };
    if let Some(elevation) = terrain
        .and_then(|p| p.stations.iter().find(|s| s.remaining_nm.abs() < 0.001))
        .and_then(|s| s.ground_feet)
    {
        target.elevation_feet = elevation;
    }
    ui.checkbox(
        "Planned descent",
        &mut interface.settings.show_planned_descent,
    );
    ui.same_line();
    ui.checkbox(
        "Current descent",
        &mut interface.settings.show_current_descent,
    );
    let d = arrival::distance_nm(
        [state.telemetry.latitude, state.telemetry.longitude],
        [target.latitude, target.longitude],
    );
    let current = arrival::current_gradient(&state.telemetry, &target);
    if interface.settings.show_current_descent {
        let status = if let Some(slope) = current {
            let error = state.telemetry.altitude_feet + slope * d - target.elevation_feet;
            if slope < -1.0 {
                let landing = (target.elevation_feet - state.telemetry.altitude_feet) / slope;
                if landing < 0.0 {
                    "Already below runway elevation".into()
                } else if (landing - d).abs() < 0.25 {
                    "Current descent reaches runway elevation at the airport".into()
                } else {
                    format!(
                        "Current descent reaches runway elevation {} {} the airport",
                        openatc_core::distance_text((landing - d).abs(), units, false),
                        if landing > d { "beyond" } else { "before" }
                    )
                }
            } else {
                format!(
                    "{} — projected {} above the runway",
                    if slope > 1.0 { "Climbing" } else { "Level" },
                    openatc_core::altitude_text(error.max(0.0), units, false)
                )
            }
        } else if state.telemetry.on_ground {
            "Current descent appears once airborne".into()
        } else {
            "Current descent unavailable: low speed or track not closing on the airport".into()
        };
        ui.text_wrapped(status);
    }
    if interface.settings.show_current_descent
        && let (Some(slope), Some(profile)) = (current, terrain)
        && let Some((distance, _)) = arrival::terrain_contact(
            &profile.stations,
            d,
            state.telemetry.altitude_feet,
            slope,
            d,
        )
        && distance < d
    {
        ui.text_wrapped(format!(
            "Current descent meets sampled terrain {} before the runway.",
            openatc_core::distance_text(d - distance, units, false)
        ));
    }
    canvas(ui, interface, state, &target, d, current, terrain);
    for (label, color) in [
        ("Planned", widgets::ACCENT),
        ("Current", CURRENT),
        ("Ground", GROUND),
        ("Terrain clearance", SAFE),
        ("Transition altitude", TRANSITION),
        ("ILS", ILS),
    ] {
        if ui.cursor_pos()[0] > 20.0
            && ui.content_region_avail()[0] < ui.calc_text_size(label)[0] + 30.0
        {
            ui.new_line();
        }
        ui.text_colored(color, label);
        ui.same_line();
    }
    ui.new_line();
    ui.text_wrapped("Distance follows the direct path to the arrival runway, not STAR turns. Planned descent ends on the runway; Current uses live vertical speed and ground track to show a short landing or overshoot.");
    ui.text_wrapped("The horizontal terrain floor clears the highest known scenery in the 2 NM corridor by 2,000 ft. Gaps are unknown terrain; this is not an obstacle survey or an approach minimum.");
    let complete = terrain.map_or(0, |p| {
        p.stations
            .iter()
            .filter(|s| s.ground_feet.is_some())
            .count()
    });
    if complete == 0 {
        ui.text_wrapped(if interface.simulator_host {
            "Terrain appears as nearby scenery is sampled."
        } else {
            "Live terrain is available inside X-Plane."
        });
    } else if let Some(p) = terrain {
        ui.text_wrapped(format!(
            "{}{} — {} terrain stations",
            p.source,
            if p.sampling { " (updating)" } else { "" },
            complete
        ));
    }
    ui.text_wrapped(format!(
        "Transition altitude: {} (regional reference; confirm the airport chart).",
        openatc_core::altitude_text(f64::from(interface.arrival.transition_feet), units, false)
    ));
    if target.glide_degrees.is_none() {
        ui.text_wrapped("No published glide slope for this runway. Planned uses a 3° geometric reference; no ILS intercept is drawn.");
    } else if target.intercept_feet.is_none() {
        ui.text_wrapped("The installed glide slope is shown. No published intercept altitude was found; no intercept is assumed.");
    }
}
fn canvas(
    ui: &imgui::Ui,
    interface: &Interface,
    state: &State,
    target: &Target,
    d: f64,
    current: Option<f64>,
    terrain: Option<&arrival::TerrainProfile>,
) {
    let units = resolve_units(
        &format!("{:?}", interface.settings.units).to_lowercase(),
        &state.plan.destination,
    );
    let metric = units == openatc_core::UnitSystem::Metric;
    let cursor = ui.cursor_screen_pos();
    let size = [
        ui.content_region_avail()[0].max(200.0),
        (ui.content_region_avail()[1] - 195.0).clamp(300.0, 520.0),
    ];
    ui.invisible_button("ArrivalProfile", size);
    let draw = ui.get_window_draw_list();
    draw.add_rect(
        cursor,
        [cursor[0] + size[0], cursor[1] + size[1]],
        [0.06, 0.09, 0.13, 1.0],
    )
    .filled(true)
    .rounding(6.0)
    .build();
    let extra = (d * 0.15).clamp(3.0, 15.0);
    let total = (d + extra).max(5.0);
    let slope = target.glide_degrees.unwrap_or(3.0);
    let ceiling = f64::from(state.plan.cruise_feet)
        .max(state.telemetry.altitude_feet)
        .max(target.elevation_feet + 50.0);
    let plan = |remaining| {
        arrival::planned_feet(
            remaining,
            target.elevation_feet,
            ceiling,
            slope,
            target.intercept_feet,
        )
    };
    let terrain_floor = terrain.and_then(|p| arrival::clearance_floor(&p.stations));
    let max_terrain = terrain_floor.unwrap_or(target.elevation_feet);
    let high = state
        .telemetry
        .altitude_feet
        .max(plan(d))
        .max(max_terrain)
        .max(f64::from(interface.arrival.transition_feet))
        .max(target.intercept_feet.unwrap_or(0.0))
        .max(target.elevation_feet + 1500.0)
        * 1.13;
    let low = terrain
        .map_or(0.0, |p| {
            p.stations
                .iter()
                .filter_map(|s| s.ground_feet)
                .fold(0.0, f64::min)
        })
        .min(target.elevation_feet)
        - 150.0;
    let left = cursor[0] + 61.0;
    let right = cursor[0] + size[0] - 17.0;
    let top = cursor[1] + 26.0;
    let bottom = cursor[1] + size[1] - 43.0;
    let project = |forward: f64, alt: f64| {
        [
            left + (forward / total) as f32 * (right - left),
            bottom - ((alt - low) / (high - low)) as f32 * (bottom - top),
        ]
    };
    let label = |at: [f32; 2], text: &str, color: [f32; 4]| {
        let w = ui.calc_text_size(text)[0];
        draw.add_text(
            [
                at[0].clamp(left, (right - w).max(left)),
                at[1].clamp(top, bottom - 16.0),
            ],
            color,
            text,
        );
    };
    for i in 0..=4 {
        let altitude = low + (high - low) * f64::from(i) / 4.0;
        let y = project(0.0, altitude)[1];
        draw.add_line([left, y], [right, y], [0.19, 0.25, 0.31, 0.6])
            .build();
        draw.add_text(
            [cursor[0] + 5.0, y - 8.0],
            widgets::MUTED,
            format!("{:.0}", if metric { altitude * 0.3048 } else { altitude }),
        );
        let forward = total * f64::from(i) / 4.0;
        let x = project(forward, low)[0];
        let remaining = d - forward;
        let tick = if remaining >= 0.0 {
            format!("{:.0}", if metric { remaining * 1.852 } else { remaining })
        } else {
            format!(
                "+{:.0}",
                if metric {
                    -remaining * 1.852
                } else {
                    -remaining
                }
            )
        };
        draw.add_text([x - 8.0, bottom + 9.0], widgets::MUTED, tick);
    }
    draw.add_text(
        [left, bottom + 26.0],
        widgets::MUTED,
        if metric {
            "km to runway  /  + beyond"
        } else {
            "NM to runway  /  + beyond"
        },
    );
    draw.add_text(
        [cursor[0] + 5.0, cursor[1] + 5.0],
        widgets::MUTED,
        if metric { "m MSL" } else { "ft MSL" },
    );
    let runway = project(d, target.elevation_feet);
    dashed(&draw, [runway[0], top], [runway[0], bottom], widgets::MUTED);
    label(
        [runway[0] + 5.0, top],
        &format!("{} / {}", state.plan.destination, target.runway),
        GROUND,
    );
    draw.with_clip_rect([left, top], [right, bottom], || {
        if let Some(profile) = terrain {
            for samples in profile.stations.windows(2) {
                if let (Some(a), Some(b)) = (samples[0].ground_feet, samples[1].ground_feet) {
                    let p = project(d - samples[0].remaining_nm, a);
                    let q = project(d - samples[1].remaining_nm, b);
                    draw.add_polyline(
                        vec![p, q, [q[0], bottom], [p[0], bottom]],
                        [0.27, 0.35, 0.28, 0.35],
                    )
                    .filled(true)
                    .build();
                    draw.add_line(p, q, GROUND).thickness(2.0).build();
                }
            }
            for i in 1..profile.stations.len() {
                if let (Some(a), Some(b)) = (
                    arrival::clearance_feet(&profile.stations, i - 1),
                    arrival::clearance_feet(&profile.stations, i),
                ) {
                    dashed(
                        &draw,
                        project(d - profile.stations[i - 1].remaining_nm, a),
                        project(d - profile.stations[i].remaining_nm, b),
                        SAFE,
                    );
                }
            }
        }
        if let Some(floor) = terrain_floor {
            draw.add_line(project(0.0, floor), project(total, floor), SAFE)
                .thickness(2.0)
                .build();
        }
        let ta = f64::from(interface.arrival.transition_feet);
        if ta > 0.0 {
            dashed(&draw, project(0.0, ta), project(d, ta), TRANSITION);
        }
        if interface.settings.show_planned_descent {
            let points: Vec<_> = (0..=120)
                .map(|i| {
                    let x = d * f64::from(i) / 120.0;
                    project(x, plan(d - x))
                })
                .collect();
            draw.add_polyline(points, widgets::ACCENT)
                .thickness(3.0)
                .build();
        }
        if let (Some(angle), Some(entry)) = (target.glide_degrees, target.intercept_feet) {
            let intercept = (entry - target.elevation_feet - 50.0) / arrival::gradient(angle);
            if intercept > 0.0 {
                let x = (d - intercept).max(0.0);
                dashed(&draw, project(x, entry), project(d, entry), ILS);
                draw.add_line(
                    project(
                        x,
                        target.elevation_feet + 50.0 + arrival::gradient(angle) * (d - x),
                    ),
                    project(d, target.elevation_feet + 50.0),
                    ILS,
                )
                .thickness(2.0)
                .build();
                if d >= intercept {
                    draw.add_circle(project(d - intercept, entry), 4.0, ILS)
                        .filled(true)
                        .build();
                }
            }
        } else if let Some(angle) = target.glide_degrees {
            draw.add_line(
                project(
                    (d - 10.0).max(0.0),
                    target.elevation_feet + 50.0 + arrival::gradient(angle) * d.min(10.0),
                ),
                project(d, target.elevation_feet + 50.0),
                ILS,
            )
            .thickness(2.0)
            .build();
        }
        if interface.settings.show_current_descent
            && let Some(slope) = current
        {
            let contact = terrain.and_then(|profile| {
                arrival::terrain_contact(
                    &profile.stations,
                    d,
                    state.telemetry.altitude_feet,
                    slope,
                    total,
                )
            });
            let (end_distance, end_altitude) =
                contact.unwrap_or((total, state.telemetry.altitude_feet + slope * total));
            draw.add_line(
                project(0.0, state.telemetry.altitude_feet),
                project(end_distance, end_altitude),
                CURRENT,
            )
            .thickness(3.0)
            .build();
            if contact.is_some() {
                draw.add_circle(project(end_distance, end_altitude), 5.0, CURRENT)
                    .filled(true)
                    .build();
            }
        }
        draw.add_line(
            [runway[0] - 7.0, runway[1]],
            [runway[0] + 7.0, runway[1]],
            GROUND,
        )
        .thickness(4.0)
        .build();
        draw.add_circle(
            project(0.0, state.telemetry.altitude_feet),
            5.0,
            [0.97, 0.98, 1.0, 1.0],
        )
        .filled(true)
        .build();
    });
    label([left + 7.0, top], "AIRCRAFT", [0.97, 0.98, 1.0, 1.0]);
    if let Some(floor) = terrain_floor {
        label(
            [left + 8.0, project(0.0, floor)[1] - 18.0],
            &format!(
                "Sampled terrain floor {}",
                openatc_core::altitude_text(floor, units, false)
            ),
            SAFE,
        );
    }
    let ta = f64::from(interface.arrival.transition_feet);
    if ta > 0.0 {
        label(
            [left + 8.0, project(0.0, ta)[1] - 18.0],
            &format!(
                "Transition altitude {}",
                openatc_core::altitude_text(ta, units, false)
            ),
            TRANSITION,
        );
    }
    if let Some(entry) = target.intercept_feet {
        label(
            [left + 8.0, project(0.0, entry)[1] - 18.0],
            &format!(
                "{} ILS intercept {}",
                target.intercept_procedure,
                openatc_core::altitude_text(entry, units, false)
            ),
            ILS,
        );
    }
    label(
        [runway[0] + 7.0, runway[1] - 18.0],
        &format!(
            "Runway {}",
            openatc_core::altitude_text(target.elevation_feet, units, false)
        ),
        GROUND,
    );
}
fn dashed(draw: &imgui::DrawListMut<'_>, a: [f32; 2], b: [f32; 2], color: [f32; 4]) {
    let length = (b[0] - a[0]).hypot(b[1] - a[1]);
    if length < 0.01 {
        return;
    }
    let direction = [(b[0] - a[0]) / length, (b[1] - a[1]) / length];
    let mut at = 0.0;
    while at < length {
        let end = (at + 7.0).min(length);
        draw.add_line(
            [a[0] + direction[0] * at, a[1] + direction[1] * at],
            [a[0] + direction[0] * end, a[1] + direction[1] * end],
            color,
        )
        .thickness(1.5)
        .build();
        at += 12.0;
    }
}
