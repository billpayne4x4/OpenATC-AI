//! Debug view of the same published radio volumes used by controller selection.
use crate::interface::Interface;
use openatc_core::state::State;

pub fn page(ui: &imgui::Ui, interface: &mut Interface, state: &State) {
    if ui.button("Follow aircraft") {
        interface.radio_map_center = None;
    }
    ui.same_line();
    ui.text(format!(
        "{:.0} ft MSL / COM1 {:.3}",
        state.telemetry.altitude_feet,
        f64::from(state.telemetry.com1_khz) / 1000.
    ));
    ui.text_wrapped("Published sector outlines at your current altitude. Wheel to zoom; drag to pan. No airport or navigation markers. Reception range is separate from sector ownership.");
    let start = ui.cursor_screen_pos();
    let available = ui.content_region_avail();
    let size = [available[0].max(100.), available[1].max(180.)];
    ui.invisible_button("##radio-sector-map", size);
    let hovered = ui.is_item_hovered();
    if hovered {
        interface.radio_map_range =
            (interface.radio_map_range * 1.2_f32.powf(-ui.io().mouse_wheel)).clamp(2., 1500.);
    }
    let t = &state.telemetry;
    let mut center = interface
        .radio_map_center
        .unwrap_or([t.latitude, t.longitude]);
    let scale = f64::from(size[0].min(size[1])) / (2. * f64::from(interface.radio_map_range));
    let cosine = center[0].to_radians().cos().abs().max(0.05);
    if hovered && ui.is_item_active() && ui.is_mouse_dragging(imgui::MouseButton::Left) {
        center[0] += f64::from(ui.io().mouse_delta[1]) / (scale * 60.);
        center[1] -= f64::from(ui.io().mouse_delta[0]) / (scale * 60. * cosine);
        interface.radio_map_center = Some(center);
    }
    let middle = [start[0] + size[0] * 0.5, start[1] + size[1] * 0.5];
    let project = |p: [f64; 2]| {
        let lon = (p[1] - center[1] + 180.).rem_euclid(360.) - 180.;
        [
            middle[0] + (lon * cosine * 60. * scale) as f32,
            middle[1] - ((p[0] - center[0]) * 60. * scale) as f32,
        ]
    };
    let draw = ui.get_window_draw_list();
    draw.add_rect(
        start,
        [start[0] + size[0], start[1] + size[1]],
        [0.07, 0.10, 0.13, 1.],
    )
    .filled(true)
    .build();
    let mut outlines = 0;
    draw.with_clip_rect(start, [start[0] + size[0], start[1] + size[1]], || {
        for station in &interface.nearby_stations {
            for volume in &station.coverage {
                if volume.points.len() < 3
                    || t.altitude_feet < volume.floor
                    || t.altitude_feet > volume.ceiling
                {
                    continue;
                }
                outlines += 1;
                let active = station.khz == t.com1_khz && volume.contains(t);
                let color = if active {
                    [0.25, 0.90, 0.72, 0.95]
                } else {
                    [0.35, 0.65, 0.85, 0.65]
                };
                let mut previous = project(volume.points[volume.points.len() - 1]);
                for point in &volume.points {
                    let next = project(*point);
                    draw.add_line(previous, next, color)
                        .thickness(if active { 3. } else { 1.5 })
                        .build();
                    previous = next;
                }
            }
        }
        let aircraft = project([t.latitude, t.longitude]);
        let angle = t.heading_degrees.to_radians();
        let point = |x: f64, y: f64| {
            [
                aircraft[0] + (x * angle.cos() - y * angle.sin()) as f32,
                aircraft[1] + (x * angle.sin() + y * angle.cos()) as f32,
            ]
        };
        draw.add_triangle(
            point(0., -12.),
            point(-7., 8.),
            point(7., 8.),
            [0.95, 0.97, 1., 1.],
        )
        .filled(true)
        .build();
        draw.add_text(
            [start[0] + 12., start[1] + 12.],
            [0.8, 0.9, 1., 1.],
            format!("NORTH UP / {:.0} NM", interface.radio_map_range),
        );
        if outlines == 0 {
            draw.add_text(
                [start[0] + 12., start[1] + 36.],
                [1., 0.75, 0.35, 1.],
                "No published sector polygons at this altitude. Boundaries are not invented.",
            );
        }
    });
}
