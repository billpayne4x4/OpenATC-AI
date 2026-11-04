//! Shared UI controls, typography, icons and branding.

// Layout tests compare exact fixture coordinates.
#![allow(clippy::float_cmp)]
// Layout limits bound the numeric casts below.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

/// Accent cyan, muted grey, warning amber.
pub const ACCENT: [f32; 4] = [0.31, 0.79, 0.94, 1.0];
/// Muted grey for secondary text.
pub const MUTED: [f32; 4] = [0.61, 0.67, 0.74, 1.0];
/// Warning amber.
pub const AMBER: [f32; 4] = [1.0, 0.73, 0.36, 1.0];

/// Keep every tab visible by wrapping when the window gets narrow.
pub fn section_tabs(ui: &imgui::Ui, labels: &[&str], selected: &mut usize) {
    let width = ui.content_region_avail()[0];
    let spacing = ui.clone_style().item_spacing[0];
    let padding = ui.clone_style().frame_padding[0] * 2.0;
    let mut used = 0.0;
    for (index, label) in labels.iter().enumerate() {
        let tab_width = ui.calc_text_size(label)[0] + padding;
        if used > 0.0 && used + spacing + tab_width <= width {
            ui.same_line();
            used += spacing;
        } else {
            used = 0.0;
        }
        let _selected = (*selected == index).then(|| {
            ui.push_style_color(
                imgui::StyleColor::Button,
                ui.style_color(imgui::StyleColor::ButtonActive),
            )
        });
        if ui.button(label) {
            *selected = index;
        }
        used += tab_width;
    }
    ui.separator();
}

/// Whether the current palette uses dark text on a light background.
pub fn is_light(ui: &imgui::Ui) -> bool {
    ui.style_color(imgui::StyleColor::Text)[0] < 0.5
}

/// Accent used for links and section titles.
pub fn accent(ui: &imgui::Ui) -> [f32; 4] {
    ui.style_color(imgui::StyleColor::CheckMark)
}

/// Secondary text, with contrast matched to the selected theme.
pub fn muted(ui: &imgui::Ui) -> [f32; 4] {
    ui.style_color(imgui::StyleColor::TextDisabled)
}

/// Warning text, distinct from the blue accent in either palette.
pub fn warning(ui: &imgui::Ui) -> [f32; 4] {
    if is_light(ui) {
        [0.53, 0.28, 0.0, 1.0]
    } else {
        AMBER
    }
}

/// Adapt coloured labels without changing the colours of dark map canvases.
pub fn text_color(ui: &imgui::Ui, mut color: [f32; 4]) -> [f32; 4] {
    if is_light(ui) {
        for channel in &mut color[..3] {
            *channel *= 0.43;
        }
    }
    color
}

/// Inset panel fill with independent background opacity.
pub fn panel_color(ui: &imgui::Ui, alpha: f32) -> [f32; 4] {
    let mut color = if is_light(ui) {
        [0.86, 0.90, 0.93, 1.0]
    } else {
        [0.13, 0.17, 0.21, 1.0]
    };
    color[3] = alpha;
    color
}

/// Font raster oversampling for crisp text.
pub const FONT_RASTER_SCALE: f32 = 3.0;

/// Load the embedded interface font.
pub fn configure_fonts(context: &mut imgui::Context) {
    context.fonts().add_font(&[imgui::FontSource::TtfData {
        data: include_bytes!("../assets/fonts/DejaVuSans.ttf"),
        size_pixels: 18.0 * FONT_RASTER_SCALE,
        config: Some(imgui::FontConfig {
            oversample_h: 1,
            oversample_v: 1,
            glyph_ranges: imgui::FontGlyphRanges::from_slice(&[
                0x0020, 0x00ff, 0x2000, 0x206f, 0x2190, 0x21ff, 0x2500, 0x25ff, 0,
            ]),
            ..Default::default()
        }),
    }]);
    configure_scale(context, 1.0);
}

/// Apply the interface scale factor.
pub fn configure_scale(context: &mut imgui::Context, scale: f32) {
    static BASE_STYLE: std::sync::OnceLock<imgui::Style> = std::sync::OnceLock::new();
    let scale = scale.clamp(0.85, 1.5);
    let base_style = *BASE_STYLE.get_or_init(|| {
        let mut style = *context.style_mut();
        configure_style(&mut style);
        style
    });
    *context.style_mut() = base_style;
    context.style_mut().scale_all_sizes(scale);
    context.io_mut().font_global_scale = scale / FONT_RASTER_SCALE;
}

#[derive(Clone, Copy)]
/// Toolbar icon glyphs.
pub enum ToolbarIcon {
    /// Speech bubble: communications.
    Messages,
    /// Headset: radio frequencies.
    Headset,
    /// Folded map: controller coverage.
    Map,
    /// Clipboard: flight plan.
    FlightPlan,
    /// Connected waypoints: taxi guidance.
    Taxi,
    /// Aircraft: arrival planning.
    Arrival,
    /// Control tower: airport information.
    Airports,
    /// Three sliders: preferences.
    Settings,
    /// Radar sweep: nearby traffic.
    Traffic,
    /// Two figures: copilot, ground crew and flight attendants.
    Crew,
    /// Flag: flight missions.
    Missions,
    /// Blank page with a plus: new flight.
    NewFile,
    /// Two forward triangles: surface test jump.
    FastForward,
    /// Pin: keep expanded.
    Pin,
    /// Collapse chevron.
    Collapse,
    /// Expand chevron.
    Expand,
    /// Pop-out window.
    PopOut,
    /// Close button.
    Close,
}

/// Toolbar icon button with tooltip. Returns true when clicked.
pub fn icon_button(ui: &imgui::Ui, label: &str, icon: ToolbarIcon, selected: bool) -> bool {
    let scale = ui.current_font_size() / 18.0;
    let size = 32.0 * scale;
    let position = ui.cursor_screen_pos();
    let clicked = ui.invisible_button(label, [size, size]);
    let hovered = ui.is_item_hovered();
    let draw = ui.get_window_draw_list();
    let center = [position[0] + size * 0.5, position[1] + size * 0.5];
    let mut color = if selected {
        [1.0; 4]
    } else {
        ui.style_color(imgui::StyleColor::Text)
    };
    let mut background = if ui.is_item_active() {
        ui.style_color(imgui::StyleColor::ButtonActive)
    } else if selected {
        [0.0, 0.59, 0.75, 1.0]
    } else if hovered {
        ui.style_color(imgui::StyleColor::ButtonHovered)
    } else {
        ui.style_color(imgui::StyleColor::Button)
    };
    // Draw-list colours do not inherit ImGui's disabled alpha.
    let alpha = ui.clone_style().alpha;
    color[3] *= alpha;
    background[3] *= alpha;
    if !matches!(icon, ToolbarIcon::Close) {
        draw.add_rect(
            position,
            [position[0] + size, position[1] + size],
            background,
        )
        .filled(true)
        .rounding(3.0 * scale)
        .build();
    }
    draw_toolbar_glyph(&draw, icon, center, color, background, hovered, scale);
    if ui.is_item_hovered_with_flags(imgui::ItemHoveredFlags::ALLOW_WHEN_DISABLED) {
        ui.tooltip_text(label);
    }
    clicked
}

/// Vector glyph for one toolbar icon.
fn draw_toolbar_glyph(
    draw: &imgui::DrawListMut<'_>,
    icon: ToolbarIcon,
    center: [f32; 2],
    color: [f32; 4],
    background: [f32; 4],
    hovered: bool,
    scale: f32,
) {
    let point = |horizontal: f32, vertical: f32| {
        [center[0] + horizontal * scale, center[1] + vertical * scale]
    };
    let line = |start: [f32; 2], end: [f32; 2]| {
        draw.add_line(point(start[0], start[1]), point(end[0], end[1]), color)
            .thickness(1.6 * scale)
            .build();
    };
    match icon {
        ToolbarIcon::Messages => {
            draw.add_rect(point(-9.0, -9.0), point(9.0, 5.0), color)
                .rounding(4.0 * scale)
                .thickness(1.6 * scale)
                .build();
            line([-5.0, 5.0], [-7.0, 10.0]);
            line([-7.0, 10.0], [1.0, 5.0]);
            line([-5.0, -4.0], [5.0, -4.0]);
            line([-5.0, 0.0], [2.0, 0.0]);
        }
        ToolbarIcon::Crew => {
            draw.add_circle(point(-3.0, -6.0), 3.5 * scale, color)
                .thickness(1.5 * scale)
                .build();
            draw.add_circle(point(5.0, -4.0), 2.8 * scale, color)
                .thickness(1.5 * scale)
                .build();
            draw.add_rect(point(-9.0, 0.0), point(3.0, 9.0), color)
                .rounding(4.0 * scale)
                .thickness(1.5 * scale)
                .build();
            draw.add_rect(point(1.0, 2.0), point(9.0, 9.0), color)
                .rounding(3.5 * scale)
                .thickness(1.5 * scale)
                .build();
        }
        ToolbarIcon::FlightPlan => {
            draw.add_rect(point(-7.0, -10.0), point(7.0, 10.0), color)
                .rounding(2.0 * scale)
                .thickness(1.6 * scale)
                .build();
            for y in [-5.0, 0.0, 5.0] {
                line([-3.0, y], [if y == 5.0 { 1.0 } else { 3.0 }, y]);
            }
        }
        ToolbarIcon::Taxi => {
            line([-7.0, 8.0], [-2.0, 8.0]);
            line([-2.0, 8.0], [-2.0, -8.0]);
            line([-2.0, -8.0], [7.0, -8.0]);
            draw.add_circle(point(-8.0, 8.0), 3.0 * scale, color)
                .thickness(1.6 * scale)
                .build();
            draw.add_circle(point(8.0, -8.0), 3.0 * scale, color)
                .filled(true)
                .build();
        }
        ToolbarIcon::Arrival => {
            line([0.0, -11.0], [0.0, 10.0]);
            line([-10.0, 2.0], [0.0, -4.0]);
            line([0.0, -4.0], [10.0, 2.0]);
            line([-5.0, 9.0], [0.0, 6.0]);
            line([0.0, 6.0], [5.0, 9.0]);
        }
        ToolbarIcon::Airports => {
            draw.add_rect(point(-8.0, -8.0), point(8.0, -2.0), color)
                .rounding(scale)
                .thickness(1.6 * scale)
                .build();
            line([0.0, -11.0], [0.0, -8.0]);
            line([-4.0, -2.0], [-4.0, 10.0]);
            line([4.0, -2.0], [4.0, 10.0]);
            line([-8.0, 10.0], [8.0, 10.0]);
        }
        ToolbarIcon::Settings => {
            for (x, y) in [(-4.0, -8.0), (4.0, 0.0), (-4.0, 8.0)] {
                line([-10.0, y], [10.0, y]);
                draw.add_circle(point(x, y), 3.0 * scale, color)
                    .filled(true)
                    .build();
            }
        }
        ToolbarIcon::Traffic => {
            draw.add_circle(center, 10.0 * scale, color)
                .thickness(1.6 * scale)
                .build();
            line([0.0, 0.0], [7.0, -7.0]);
            draw.add_circle(point(-4.0, -5.0), 1.8 * scale, color)
                .filled(true)
                .build();
            draw.add_circle(point(5.0, 4.0), 1.8 * scale, color)
                .filled(true)
                .build();
        }
        ToolbarIcon::Missions => {
            line([-7.0, 11.0], [-7.0, -10.0]);
            for (a, b) in [
                ([-7.0, -9.0], [1.0, -9.0]),
                ([1.0, -9.0], [1.0, -6.0]),
                ([1.0, -6.0], [9.0, -6.0]),
                ([9.0, -6.0], [9.0, 3.0]),
                ([9.0, 3.0], [1.0, 3.0]),
                ([1.0, 3.0], [1.0, 0.0]),
                ([1.0, 0.0], [-7.0, 0.0]),
            ] {
                line(a, b);
            }
        }
        ToolbarIcon::FastForward => {
            for x in [-9.0, 1.0] {
                draw.add_triangle(point(x, -8.0), point(x + 9.0, 0.0), point(x, 8.0), color)
                    .filled(true)
                    .build();
            }
        }
        ToolbarIcon::NewFile => {
            for (a, b) in [
                ([-8.0, -11.0], [3.0, -11.0]),
                ([3.0, -11.0], [8.0, -6.0]),
                ([8.0, -6.0], [8.0, 11.0]),
                ([8.0, 11.0], [-8.0, 11.0]),
                ([-8.0, 11.0], [-8.0, -11.0]),
                ([3.0, -11.0], [3.0, -6.0]),
                ([3.0, -6.0], [8.0, -6.0]),
                ([-4.0, 3.0], [4.0, 3.0]),
                ([0.0, -1.0], [0.0, 7.0]),
            ] {
                line(a, b);
            }
        }
        ToolbarIcon::Map => {
            for (a, b) in [
                ([-11., -7.], [-4., -10.]),
                ([-4., -10.], [4., -6.]),
                ([4., -6.], [11., -9.]),
                ([11., -9.], [11., 7.]),
                ([11., 7.], [4., 10.]),
                ([4., 10.], [-4., 6.]),
                ([-4., 6.], [-11., 9.]),
                ([-11., 9.], [-11., -7.]),
                ([-4., -10.], [-4., 6.]),
                ([4., -6.], [4., 10.]),
            ] {
                line(a, b);
            }
        }
        ToolbarIcon::Headset => {
            for i in 0..16 {
                let angle = std::f32::consts::PI + i as f32 * std::f32::consts::PI / 16.0;
                let next = angle + std::f32::consts::PI / 16.0;
                line(
                    [8.0 * angle.cos(), 8.0 * angle.sin() - 1.0],
                    [8.0 * next.cos(), 8.0 * next.sin() - 1.0],
                );
            }
            draw.add_rect(point(-10.0, -1.0), point(-6.0, 8.0), color)
                .thickness(1.6 * scale)
                .rounding(scale)
                .build();
            draw.add_rect(point(6.0, -1.0), point(10.0, 8.0), color)
                .thickness(1.6 * scale)
                .rounding(scale)
                .build();
            line([8.0, 8.0], [8.0, 11.0]);
            line([8.0, 11.0], [2.0, 11.0]);
        }
        ToolbarIcon::Pin => {
            line([-5.0, -9.0], [5.0, -9.0]);
            line([-4.0, -9.0], [-4.0, 0.0]);
            line([4.0, -9.0], [4.0, 0.0]);
            line([-4.0, 0.0], [-8.0, 4.0]);
            line([4.0, 0.0], [8.0, 4.0]);
            line([-8.0, 4.0], [8.0, 4.0]);
            line([0.0, 4.0], [0.0, 11.0]);
        }
        ToolbarIcon::Collapse | ToolbarIcon::Expand => {
            let direction = if matches!(icon, ToolbarIcon::Expand) {
                1.0
            } else {
                -1.0
            };
            line([-7.0, -3.0 * direction], [0.0, 4.0 * direction]);
            line([0.0, 4.0 * direction], [7.0, -3.0 * direction]);
        }
        ToolbarIcon::PopOut => {
            draw.add_rect(point(-9.0, -4.0), point(4.0, 9.0), color)
                .thickness(1.5 * scale)
                .build();
            draw.add_rect(point(-3.0, -10.0), point(10.0, 3.0), background)
                .filled(true)
                .build();
            draw.add_rect(point(-3.0, -10.0), point(10.0, 3.0), color)
                .thickness(1.5 * scale)
                .build();
        }
        ToolbarIcon::Close => {
            draw.add_circle(center, 7.0 * scale, [1.0, 0.34, 0.32, color[3]])
                .filled(true)
                .build();
            if hovered {
                line([-2.5, -2.5], [2.5, 2.5]);
                line([-2.5, 2.5], [2.5, -2.5]);
            }
        }
    }
}

/// Text field bound to a String. Returns true on change.
pub fn edit_text(ui: &imgui::Ui, label: &str, value: &mut String, capacity: usize) -> bool {
    value.reserve(capacity.saturating_sub(value.len()));
    ui.input_text(label, value).build()
}

/// Text field with flags (enter-returns-true, uppercase, ...).
pub fn edit_text_flags(
    ui: &imgui::Ui,
    label: &str,
    value: &mut String,
    capacity: usize,
    flags: imgui::InputTextFlags,
) -> bool {
    value.reserve(capacity.saturating_sub(value.len()));
    ui.input_text(label, value).flags(flags).build()
}

/// Multi-line paragraph field with a fixed height.
pub fn edit_paragraph(
    ui: &imgui::Ui,
    label: &str,
    value: &mut String,
    height: f32,
    capacity: usize,
) -> bool {
    value.reserve(capacity.saturating_sub(value.len()));
    ui.input_text_multiline(label, value, [ui.content_region_avail()[0], height])
        .build()
}

/// Secondary text that stays inside the content area.
pub fn muted_wrapped(ui: &imgui::Ui, text: impl AsRef<str>) {
    let _color = ui.push_style_color(imgui::StyleColor::Text, muted(ui));
    ui.text_wrapped(text);
}

/// Keep related controls together when there is room for the next item.
pub fn same_line_if_fits(ui: &imgui::Ui, width: f32) {
    let right = ui.cursor_screen_pos()[0] + ui.content_region_avail()[0];
    if ui.item_rect_max()[0] + ui.clone_style().item_spacing[0] + width <= right {
        ui.same_line();
    }
}

/// Number of readable columns for dispatch fields at the current font size.
pub fn form_columns(ui: &imgui::Ui) -> usize {
    (ui.content_region_avail()[0] / (250.0 * ui.current_font_size() / 18.0))
        .floor()
        .clamp(1.0, 3.0) as usize
}

/// Consistent page title and short purpose line.
pub fn page_heading(ui: &imgui::Ui, title: &str, description: &str) {
    ui.text_colored(accent(ui), title.to_uppercase());
    ui.text_wrapped(description);
    ui.spacing();
    ui.separator();
    ui.spacing();
}

/// Section heading with rules above and below.
pub fn section_title(ui: &imgui::Ui, title: &str) {
    ui.spacing();
    ui.text_colored(accent(ui), title);
    ui.separator();
    ui.spacing();
}

/// Fixed-height metric card with a title.
pub fn metric(ui: &imgui::Ui, title: &str, value: &str, width: f32, ui_scale: f32) {
    // Owned non-empty ID: this imgui build dereferences empty string IDs.
    let id = if title.is_empty() {
        "(metric)".to_owned()
    } else {
        title.to_owned()
    };
    #[allow(clippy::unnecessary_to_owned)]
    let token = ui.push_id(id);
    ui.child_window(title)
        .size([width, 46.0 + 44.0 * ui_scale])
        .border(true)
        .scroll_bar(false)
        .build(|| {
            let color = ui.push_style_color(imgui::StyleColor::Text, muted(ui));
            ui.text_wrapped(title);
            color.pop();
            ui.text(value);
        });
    token.pop();
}

/// Apply the dark `OpenATC` style: spacing, rounding and the color table.
pub fn configure_style(style: &mut imgui::Style) {
    use imgui::StyleColor;
    style.window_padding = [14.0, 12.0];
    style.frame_padding = [10.0, 6.0];
    style.item_spacing = [10.0, 8.0];
    style.cell_padding = [10.0, 8.0];
    style.window_rounding = 5.0;
    style.child_rounding = 4.0;
    style.frame_rounding = 5.0;
    style.frame_border_size = 1.0;
    style.grab_rounding = 5.0;
    style.popup_rounding = 8.0;
    style.scrollbar_size = 11.0;
    style.window_border_size = 0.0;
    style.child_border_size = 1.0;
    let paint = |style: &mut imgui::Style, slot: StyleColor, color: [f32; 4]| {
        style.colors[slot as usize] = color;
    };
    paint(style, StyleColor::WindowBg, [0.205, 0.25, 0.29, 0.96]);
    paint(style, StyleColor::ChildBg, [0.13, 0.17, 0.215, 0.80]);
    paint(style, StyleColor::Border, [0.22, 0.31, 0.39, 0.55]);
    paint(style, StyleColor::FrameBg, [0.09, 0.11, 0.14, 1.0]);
    paint(style, StyleColor::Button, [0.16, 0.22, 0.285, 0.98]);
    paint(style, StyleColor::ButtonHovered, [0.20, 0.38, 0.46, 1.0]);
    paint(style, StyleColor::ButtonActive, [0.15, 0.45, 0.56, 1.0]);
    paint(style, StyleColor::Header, [0.17, 0.33, 0.41, 1.0]);
    paint(style, StyleColor::HeaderHovered, [0.22, 0.39, 0.47, 1.0]);
    paint(style, StyleColor::Tab, [0.12, 0.18, 0.24, 1.0]);
    paint(style, StyleColor::TabHovered, [0.16, 0.34, 0.43, 1.0]);
    paint(style, StyleColor::TabActive, [0.13, 0.30, 0.39, 1.0]);
    paint(style, StyleColor::Separator, [0.23, 0.34, 0.42, 0.55]);
    paint(style, StyleColor::ScrollbarBg, [0.04, 0.07, 0.10, 0.6]);
    paint(style, StyleColor::ScrollbarGrab, [0.23, 0.34, 0.43, 0.9]);
    paint(style, StyleColor::CheckMark, ACCENT);
    paint(style, StyleColor::SliderGrab, ACCENT);
    paint(style, StyleColor::Text, [0.93, 0.95, 0.97, 1.0]);
    paint(style, StyleColor::TextDisabled, MUTED);
    paint(style, StyleColor::TableRowBg, [0.10, 0.12, 0.13, 0.28]);
    paint(style, StyleColor::TableRowBgAlt, [0.24, 0.34, 0.43, 0.18]);
}
