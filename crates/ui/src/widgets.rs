//! Shared UI controls, typography, icons and branding.

/// Accent cyan, muted grey, warning amber.
pub const ACCENT: [f32; 4] = [0.31, 0.79, 0.94, 1.0];
/// Muted grey for secondary text.
pub const MUTED: [f32; 4] = [0.61, 0.67, 0.74, 1.0];
/// Warning amber.
pub const AMBER: [f32; 4] = [1.0, 0.73, 0.36, 1.0];

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
    /// Clipboard: flight plan.
    FlightPlan,
    /// Blank page with a plus: new flight.
    NewFile,
    /// Two forward triangles: surface test jump.
    FastForward,
    /// Hamburger: page menu.
    Menu,
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
    let color = if selected {
        [1.0; 4]
    } else {
        [0.84, 0.88, 0.90, 1.0]
    };
    let background = if selected {
        [0.0, 0.59, 0.75, 1.0]
    } else if hovered {
        [0.43, 0.49, 0.53, 0.96]
    } else {
        [0.37, 0.41, 0.44, 0.70]
    };
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
    if hovered {
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
        ToolbarIcon::Messages => draw_symbol(draw, 0, center, color, 11.0 * scale),
        ToolbarIcon::FlightPlan => draw_symbol(draw, 1, center, color, 10.0 * scale),
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
        ToolbarIcon::Headset => {
            draw.add_circle(point(0.0, -1.0), 8.0 * scale, color)
                .thickness(1.6 * scale)
                .build();
            draw.add_rect(point(-10.0, -1.0), point(-6.0, 8.0), background)
                .filled(true)
                .build();
            draw.add_rect(point(6.0, -1.0), point(10.0, 8.0), background)
                .filled(true)
                .build();
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
        ToolbarIcon::Menu => {
            for vertical in [-6.0, 0.0, 6.0] {
                line([-8.0, vertical], [8.0, vertical]);
            }
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
            draw.add_circle(center, 7.0 * scale, [1.0, 0.34, 0.32, 1.0])
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

/// Consistent page title and short purpose line.
pub fn page_heading(ui: &imgui::Ui, title: &str, description: &str) {
    ui.text_colored(ACCENT, title.to_uppercase());
    ui.text_wrapped(description);
    ui.spacing();
    ui.separator();
    ui.spacing();
}

/// Section heading with rules above and below.
pub fn section_title(ui: &imgui::Ui, title: &str) {
    ui.spacing();
    ui.text_colored(ACCENT, title);
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
            let color = ui.push_style_color(imgui::StyleColor::Text, MUTED);
            ui.text_wrapped(title);
            color.pop();
            ui.text(value);
        });
    token.pop();
}

/// Vector glyphs for map legends (0..=5), drawn in immediate mode.
pub fn draw_symbol(
    draw: &imgui::DrawListMut<'_>,
    symbol: i32,
    center: [f32; 2],
    color: [f32; 4],
    radius: f32,
) {
    let point = |x: f32, y: f32| [center[0] + x * radius, center[1] + y * radius];
    let line = |draw: &imgui::DrawListMut<'_>, x1: f32, y1: f32, x2: f32, y2: f32| {
        draw.add_line(point(x1, y1), point(x2, y2), color)
            .thickness(1.7)
            .build();
    };
    match symbol {
        0 => {
            draw.add_rect(point(-0.7, -0.65), point(0.7, 0.5), color)
                .rounding(3.0)
                .thickness(1.7)
                .build();
            line(draw, -0.4, 0.5, -0.65, 0.85);
            line(draw, -0.65, 0.85, 0.05, 0.5);
            line(draw, -0.4, -0.2, 0.4, -0.2);
            line(draw, -0.4, 0.1, 0.1, 0.1);
        }
        1 => {
            draw.add_rect(point(-0.65, -0.85), point(0.65, 0.85), color)
                .rounding(2.0)
                .thickness(1.7)
                .build();
            line(draw, -0.3, -0.4, 0.3, -0.4);
            line(draw, -0.3, 0.0, 0.3, 0.0);
            line(draw, -0.3, 0.4, 0.1, 0.4);
        }
        2 => {
            line(draw, -0.8, 0.65, -0.2, 0.65);
            line(draw, -0.2, 0.65, -0.2, -0.6);
            line(draw, -0.2, -0.6, 0.7, -0.6);
            draw.add_circle(point(-0.8, 0.65), 3.0, color)
                .thickness(1.5)
                .build();
            draw.add_circle(point(0.7, -0.6), 3.0, color)
                .filled(true)
                .build();
        }
        3 => {
            line(draw, 0.0, -0.9, 0.0, 0.85);
            line(draw, -0.85, 0.2, 0.0, -0.3);
            line(draw, 0.0, -0.3, 0.85, 0.2);
            line(draw, -0.4, 0.8, 0.0, 0.5);
            line(draw, 0.0, 0.5, 0.4, 0.8);
        }
        4 => {
            line(draw, -0.7, 0.8, 0.7, 0.8);
            line(draw, -0.35, 0.8, -0.35, -0.2);
            line(draw, 0.35, 0.8, 0.35, -0.2);
            draw.add_rect(point(-0.65, -0.7), point(0.65, -0.15), color)
                .rounding(1.0)
                .thickness(1.7)
                .build();
            line(draw, 0.0, -0.7, 0.0, -1.0);
        }
        _ => {
            for (index, vertical) in [-0.65, 0.0, 0.65].iter().enumerate() {
                let vertical = *vertical;
                line(draw, -0.85, vertical, 0.85, vertical);
                let dot = if index == 1 { 0.35 } else { -0.25 };
                draw.add_circle(point(dot, vertical), 3.5, color)
                    .filled(true)
                    .build();
            }
        }
    }
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
