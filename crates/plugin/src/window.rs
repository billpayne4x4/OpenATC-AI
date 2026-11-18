//! Floating X-Plane window and keyboard/mouse input.
//! Uses xplane-sys for window features not exposed by the safe wrapper.

use std::ffi::{c_char, c_int, c_void};

/// Plugin window geometry at creation.
const DEFAULT_WIDTH: i32 = 960;
const DEFAULT_HEIGHT: i32 = 760;

unsafe extern "C-unwind" fn receive_monitor(
    _index: c_int,
    left: c_int,
    top: c_int,
    right: c_int,
    bottom: c_int,
    refcon: *mut c_void,
) {
    let monitors = unsafe { &mut *refcon.cast::<Vec<[i32; 4]>>() };
    if right > left && top > bottom {
        monitors.push([left, top, right, bottom]);
    }
}

fn display_bounds() -> Vec<[i32; 4]> {
    let mut monitors = Vec::<[i32; 4]>::new();
    unsafe {
        xplane_sys::XPLMGetAllMonitorBoundsGlobal(
            Some(receive_monitor),
            (&raw mut monitors).cast(),
        );
    }
    // Windowed X-Plane does not enumerate full-screen monitors.
    if monitors.is_empty() {
        let mut bounds = [0; 4];
        unsafe {
            xplane_sys::XPLMGetScreenBoundsGlobal(
                &raw mut bounds[0],
                &raw mut bounds[1],
                &raw mut bounds[2],
                &raw mut bounds[3],
            );
        }
        if bounds[2] > bounds[0] && bounds[1] > bounds[3] {
            monitors.push(bounds);
        }
    }
    monitors
}

/// Opaque window handle owned by this module.
pub struct Floater {
    window: xplane_sys::XPLMWindowID,
}

unsafe extern "C-unwind" fn draw_callback(window: xplane_sys::XPLMWindowID, refcon: *mut c_void) {
    let state = unsafe { &mut *refcon.cast::<WindowState>() };
    let actions = state.render.on_draw(window);
    state.queue_actions(actions);
}

unsafe extern "C-unwind" fn mouse_callback(
    window: xplane_sys::XPLMWindowID,
    x: c_int,
    y: c_int,
    status: xplane_sys::XPLMMouseStatus,
    refcon: *mut c_void,
) -> c_int {
    let state = unsafe { &mut *refcon.cast::<WindowState>() };
    state.render.on_mouse(window, x, y, status, false);
    1
}

unsafe extern "C-unwind" fn right_callback(
    window: xplane_sys::XPLMWindowID,
    x: c_int,
    y: c_int,
    status: xplane_sys::XPLMMouseStatus,
    refcon: *mut c_void,
) -> c_int {
    let state = unsafe { &mut *refcon.cast::<WindowState>() };
    state.render.on_mouse(window, x, y, status, true);
    1
}

unsafe extern "C-unwind" fn wheel_callback(
    window: xplane_sys::XPLMWindowID,
    x: c_int,
    y: c_int,
    wheel: c_int,
    clicks: c_int,
    refcon: *mut c_void,
) -> c_int {
    let state = unsafe { &mut *refcon.cast::<WindowState>() };
    state.render.on_wheel(window, x, y, wheel, clicks);
    1
}

unsafe extern "C-unwind" fn cursor_callback(
    window: xplane_sys::XPLMWindowID,
    x: c_int,
    y: c_int,
    refcon: *mut c_void,
) -> xplane_sys::XPLMCursorStatus {
    let state = unsafe { &mut *refcon.cast::<WindowState>() };
    state.render.on_cursor(window, x, y);
    xplane_sys::XPLMCursorStatus::Default
}

unsafe extern "C-unwind" fn key_callback(
    window: xplane_sys::XPLMWindowID,
    key: c_char,
    flags: xplane_sys::XPLMKeyFlags,
    virtual_key: c_char,
    refcon: *mut c_void,
    losing_focus: c_int,
) {
    let state = unsafe { &mut *refcon.cast::<WindowState>() };
    state
        .render
        .on_key(window, key, flags, virtual_key, losing_focus);
}

/// Per-window state behind the refcon pointer.
pub struct WindowState {
    /// imgui renderer and input backend.
    pub render: crate::render::RenderState,
    pub minimum_size: [i32; 2],
    pending: Option<openatc_ui::interface::WindowActions>,
    pub floating_bounds: Option<[i32; 4]>,
    drag_remainder: [f32; 2],
    monitors: Vec<[i32; 4]>,
    last_display_check: Option<std::time::Instant>,
}

impl WindowState {
    pub fn new() -> Self {
        Self {
            render: crate::render::RenderState::new(),
            minimum_size: [520, 180],
            pending: None,
            floating_bounds: None,
            drag_remainder: [0.0, 0.0],
            monitors: Vec::new(),
            last_display_check: None,
        }
    }

    fn queue_actions(&mut self, actions: openatc_ui::interface::WindowActions) {
        self.pending = Some(merge_actions(self.pending.take(), actions));
    }

    fn recover_bounds(&mut self, window: xplane_sys::XPLMWindowID) {
        if self.render.imgui.io().mouse_down.iter().any(|down| *down)
            || self
                .last_display_check
                .is_some_and(|at| at.elapsed().as_secs_f32() < 1.0)
        {
            return;
        }
        self.last_display_check = Some(std::time::Instant::now());
        self.monitors = display_bounds();
        unsafe {
            if xplane_sys::XPLMWindowIsPoppedOut(window) != 0
                || xplane_sys::XPLMWindowIsInVR(window) != 0
            {
                return;
            }
            let mut bounds = [0; 4];
            xplane_sys::XPLMGetWindowGeometry(
                window,
                &raw mut bounds[0],
                &raw mut bounds[1],
                &raw mut bounds[2],
                &raw mut bounds[3],
            );
            if let Some(fitted) = crate::geometry::fit_window(bounds, &self.monitors)
                && fitted != bounds
            {
                let min_width = self.minimum_size[0].min(fitted[2] - fitted[0]);
                let min_height = compact_limit(self.minimum_size[1], self.render.ui_scale())
                    .min(fitted[1] - fitted[3]);
                xplane_sys::XPLMSetWindowResizingLimits(window, min_width, min_height, 4800, 3200);
                xplane_sys::XPLMSetWindowGeometry(
                    window, fitted[0], fitted[1], fitted[2], fitted[3],
                );
                crate::debug_log(
                    "OpenATC window: recovered floating bounds onto a visible display\n",
                );
            }
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn apply_actions(
        &mut self,
        window: xplane_sys::XPLMWindowID,
        actions: openatc_ui::interface::WindowActions,
    ) {
        unsafe {
            if actions.close {
                xplane_sys::XPLMTakeKeyboardFocus(std::ptr::null_mut());
                xplane_sys::XPLMSetWindowIsVisible(window, 0);
                return;
            }
            let (mut left, mut top, mut right, mut bottom) = (0, 0, 0, 0);
            xplane_sys::XPLMGetWindowGeometry(
                window,
                &raw mut left,
                &raw mut top,
                &raw mut right,
                &raw mut bottom,
            );
            let popped_out = xplane_sys::XPLMWindowIsPoppedOut(window) != 0;
            let mut os_bounds = [left, top, right, bottom];
            if popped_out {
                xplane_sys::XPLMGetWindowGeometryOS(
                    window,
                    &raw mut os_bounds[0],
                    &raw mut os_bounds[1],
                    &raw mut os_bounds[2],
                    &raw mut os_bounds[3],
                );
            }
            let mut minimum_size = [
                actions.minimum_size[0].round() as i32,
                actions.minimum_size[1].round() as i32,
            ];
            if !popped_out
                && let Some(monitor) = crate::geometry::monitor_for(
                    actions.restore_bounds.unwrap_or([left, top, right, bottom]),
                    &self.monitors,
                )
            {
                minimum_size[0] = minimum_size[0].min(monitor[2] - monitor[0]);
                minimum_size[1] = minimum_size[1].min(monitor[1] - monitor[3]);
            }
            let limits_changed = minimum_size[0] > 0 && minimum_size != self.minimum_size;
            if limits_changed {
                self.minimum_size = minimum_size;
                xplane_sys::XPLMSetWindowResizingLimits(
                    window,
                    minimum_size[0],
                    compact_limit(minimum_size[1], actions.ui_scale),
                    4800,
                    3200,
                );
            }
            // Apply the current scale's limits before restoring saved bounds.
            // Otherwise the SDK can clamp a smaller saved window to startup limits.
            if let Some([left, top, right, bottom]) = actions.restore_bounds
                && !popped_out
                && [left, top, right, bottom]
                    .iter()
                    .all(|value| (-100000..=100000).contains(value))
                && right - left >= 442
                && top - bottom >= 153
                && right - left <= 4800
                && top - bottom <= 3200
            {
                let [left, top, right, bottom] =
                    crate::geometry::fit_window([left, top, right, bottom], &self.monitors)
                        .unwrap_or([left, top, right, bottom]);
                xplane_sys::XPLMSetWindowGeometry(window, left, top, right, bottom);
                xplane_sys::XPLMGetWindowGeometry(
                    window,
                    &raw mut os_bounds[0],
                    &raw mut os_bounds[1],
                    &raw mut os_bounds[2],
                    &raw mut os_bounds[3],
                );
            }
            if actions.restore_bounds.is_some() && !popped_out {
                [left, top, right, bottom] = os_bounds;
            }
            if actions.toggle_pop_out {
                if popped_out {
                    xplane_sys::XPLMSetWindowPositioningMode(
                        window,
                        xplane_sys::XPLMWindowPositioningMode::PositionFree,
                        -1,
                    );
                    if let Some(bounds) = self.floating_bounds.take() {
                        xplane_sys::XPLMSetWindowGeometry(
                            window, bounds[0], bounds[1], bounds[2], bounds[3],
                        );
                    }
                } else {
                    self.floating_bounds = Some([left, top, right, bottom]);
                    xplane_sys::XPLMSetWindowPositioningMode(
                        window,
                        xplane_sys::XPLMWindowPositioningMode::PopOut,
                        -1,
                    );
                }
                return;
            }
            if xplane_sys::XPLMWindowIsInVR(window) != 0 {
                if let Some(height) = actions.height {
                    xplane_sys::XPLMSetWindowGeometryVR(
                        window,
                        (right - left).max(self.minimum_size[0]),
                        (height.round() as i32).max(self.minimum_size[1]),
                    );
                }
                return;
            }
            // macOS supplies the native frame for popped-out window movement.
            let (drag_delta, resize_delta) = if popped_out && cfg!(target_os = "macos") {
                ([0.0; 2], [0.0; 2])
            } else {
                (actions.drag_delta, actions.resize_delta)
            };
            let mut pixel_scale = [1.0, 1.0];
            if popped_out {
                let boxel_width = (right - left).max(1) as f32;
                let boxel_height = (top - bottom).max(1) as f32;
                [left, top, right, bottom] = os_bounds;
                pixel_scale = [
                    (right - left) as f32 / boxel_width,
                    (top - bottom) as f32 / boxel_height,
                ];
            }
            let current_bounds = [left, top, right, bottom];
            if drag_delta == [0.0, 0.0] {
                self.drag_remainder = [0.0, 0.0];
            } else {
                self.drag_remainder[0] += drag_delta[0] * pixel_scale[0];
                self.drag_remainder[1] += drag_delta[1] * pixel_scale[1];
                let horizontal = self.drag_remainder[0].round() as i32;
                let vertical = self.drag_remainder[1].round() as i32;
                self.drag_remainder[0] -= horizontal as f32;
                self.drag_remainder[1] -= vertical as f32;
                left += horizontal;
                right += horizontal;
                top -= vertical;
                bottom -= vertical;
            }
            right += (resize_delta[0] * pixel_scale[0]).round() as i32;
            bottom -= (resize_delta[1] * pixel_scale[1]).round() as i32;
            let minimum_width = (self.minimum_size[0] as f32 * pixel_scale[0]).round() as i32;
            let minimum_height = (self.minimum_size[1] as f32 * pixel_scale[1]).round() as i32;
            right = right.max(left + minimum_width);
            if let Some(height) = actions.height {
                bottom = top - ((height * pixel_scale[1]).round() as i32).max(minimum_height);
            } else {
                bottom = bottom.min(top - minimum_height);
            }
            // SDK resizing limits may reposition a window. Use the geometry
            // captured beforehand so changing modes preserves the screen anchor.
            if limits_changed || current_bounds != [left, top, right, bottom] {
                if popped_out {
                    xplane_sys::XPLMSetWindowGeometryOS(window, left, top, right, bottom);
                } else {
                    xplane_sys::XPLMSetWindowGeometry(window, left, top, right, bottom);
                }
            }
        }
    }
}

fn merge_actions(
    previous: Option<openatc_ui::interface::WindowActions>,
    mut actions: openatc_ui::interface::WindowActions,
) -> openatc_ui::interface::WindowActions {
    if let Some(previous) = previous {
        actions.restore_bounds = actions.restore_bounds.or(previous.restore_bounds);
        actions.height = actions.height.or(previous.height);
        actions.close |= previous.close;
        actions.toggle_pop_out ^= previous.toggle_pop_out;
        for axis in 0..2 {
            actions.drag_delta[axis] += previous.drag_delta[axis];
            actions.resize_delta[axis] += previous.resize_delta[axis];
        }
    }
    actions
}

// Keep SDK limits valid for either mode; expanded limits are enforced above.
fn compact_limit(minimum: i32, scale: f32) -> i32 {
    minimum.min((180.0 * scale).round() as i32).max(1)
}

impl Floater {
    /// Apply queued window changes from the main flight loop.
    pub fn apply_pending(&self, state: &mut WindowState) {
        state.recover_bounds(self.window);
        if let Some(actions) = state.pending.take() {
            state.apply_actions(self.window, actions);
        }
    }

    /// Open the hidden floating window with the state box as refcon.
    pub fn open(state: Box<WindowState>) -> Option<(Self, Box<WindowState>)> {
        let mut state = state;
        let raw = Box::into_raw(state);
        unsafe {
            let mut params: xplane_sys::XPLMCreateWindow_t = std::mem::zeroed();
            // Struct sizes are hundreds of bytes; the cast is exact.
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            {
                params.structSize = std::mem::size_of::<xplane_sys::XPLMCreateWindow_t>() as c_int;
            }
            let (mut left, mut top, mut right, mut bottom) = (0, 0, 0, 0);
            xplane_sys::XPLMGetScreenBoundsGlobal(
                &raw mut left,
                &raw mut top,
                &raw mut right,
                &raw mut bottom,
            );
            let width = DEFAULT_WIDTH.min((right - left - 80).max(520));
            let height = DEFAULT_HEIGHT.min((top - bottom - 100).max(300));
            params.left = left + 40;
            params.top = top - 50;
            params.right = params.left + width;
            params.bottom = params.top - height;
            if let Some(bounds) = crate::geometry::fit_window(
                [params.left, params.top, params.right, params.bottom],
                &display_bounds(),
            ) {
                [params.left, params.top, params.right, params.bottom] = bounds;
            }
            params.visible = 0;
            params.drawWindowFunc = Some(draw_callback);
            params.handleMouseClickFunc = Some(mouse_callback);
            params.handleKeyFunc = Some(key_callback);
            params.handleCursorFunc = Some(cursor_callback);
            params.handleMouseWheelFunc = Some(wheel_callback);
            params.refcon = raw.cast::<c_void>();
            params.decorateAsFloatingWindow =
                xplane_sys::XPLMWindowDecoration::SelfDecoratedResizable;
            params.layer = xplane_sys::XPLMWindowLayer::FloatingWindows;
            params.handleRightClickFunc = Some(right_callback);
            let window = xplane_sys::XPLMCreateWindowEx(&raw mut params);
            if window.is_null() {
                drop(Box::from_raw(raw));
                return None;
            }
            state = Box::from_raw(raw);
            xplane_sys::XPLMSetWindowTitle(window, c"OpenATC AI".as_ptr());
            xplane_sys::XPLMSetWindowResizingLimits(
                window,
                520.min(params.right - params.left),
                180.min(params.top - params.bottom),
                4800,
                3200,
            );
            Some((Self { window }, state))
        }
    }

    /// Show or hide the window.
    pub fn set_visible(&self, visible: bool) {
        unsafe { xplane_sys::XPLMSetWindowIsVisible(self.window, c_int::from(visible)) };
    }

    /// Whether the window is visible.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        unsafe { xplane_sys::XPLMGetWindowIsVisible(self.window) != 0 }
    }
}

impl Drop for Floater {
    fn drop(&mut self) {
        unsafe { xplane_sys::XPLMDestroyWindow(self.window) };
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn deferred_frames_preserve_one_shot_requests_and_accumulate_movement() {
        use openatc_ui::interface::WindowActions;
        let first = WindowActions {
            toggle_pop_out: true,
            height: Some(250.0),
            restore_bounds: Some([20, 900, 980, 140]),
            drag_delta: [2.5, 1.5],
            ..WindowActions::default()
        };
        let next = WindowActions {
            drag_delta: [3.5, -1.0],
            minimum_size: [520.0, 180.0],
            ui_scale: 1.0,
            ..WindowActions::default()
        };
        let merged = super::merge_actions(Some(first), next);
        assert!(merged.toggle_pop_out);
        assert_eq!(merged.height, Some(250.0));
        assert_eq!(merged.restore_bounds, first.restore_bounds);
        assert_eq!(merged.drag_delta, [6.0, 0.5]);
        assert_eq!(merged.minimum_size, [520.0, 180.0]);
        let last = super::merge_actions(
            Some(merged),
            WindowActions {
                toggle_pop_out: true,
                close: true,
                height: Some(760.0),
                ..next
            },
        );
        assert!(!last.toggle_pop_out);
        assert!(last.close);
        assert_eq!(last.height, Some(760.0));
    }
    #[test]
    fn native_minimum_stays_compact_across_expand_collapse() {
        for scale in [0.75, 0.91, 1.0, 1.5, 2.0] {
            let compact = (180.0_f32 * scale).round() as i32;
            for minimum in [460.0, 180.0, 460.0, 180.0] {
                assert_eq!(
                    super::compact_limit((minimum * scale).round() as i32, scale),
                    compact
                );
            }
        }
        assert_eq!(super::compact_limit(100, 1.0), 100);
    }
}
