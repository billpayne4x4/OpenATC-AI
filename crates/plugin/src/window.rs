//! Floating X-Plane window and keyboard/mouse input.
//! Uses xplane-sys for window features not exposed by the safe wrapper.

use std::ffi::{c_char, c_int, c_void};

/// Plugin window geometry at creation.
const DEFAULT_WIDTH: i32 = 960;
const DEFAULT_HEIGHT: i32 = 760;

/// Opaque window handle owned by this module.
pub struct Floater {
    window: xplane_sys::XPLMWindowID,
}

unsafe extern "C-unwind" fn draw_callback(window: xplane_sys::XPLMWindowID, refcon: *mut c_void) {
    let state = unsafe { &mut *refcon.cast::<WindowState>() };
    let actions = state.render.on_draw(window);
    state.apply_actions(window, actions);
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
    pub floating_bounds: Option<[i32; 4]>,
    drag_remainder: [f32; 2],
}

impl WindowState {
    pub fn new() -> Self {
        Self {
            render: crate::render::RenderState::new(),
            minimum_size: [520, 180],
            floating_bounds: None,
            drag_remainder: [0.0, 0.0],
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
            let minimum_size = [
                actions.minimum_size[0].round() as i32,
                actions.minimum_size[1].round() as i32,
            ];
            if minimum_size[0] > 0 && minimum_size != self.minimum_size {
                self.minimum_size = minimum_size;
                xplane_sys::XPLMSetWindowResizingLimits(
                    window,
                    minimum_size[0],
                    minimum_size[1],
                    4800,
                    3200,
                );
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
            let mut pixel_scale = [1.0, 1.0];
            if popped_out {
                let boxel_width = (right - left).max(1) as f32;
                let boxel_height = (top - bottom).max(1) as f32;
                xplane_sys::XPLMGetWindowGeometryOS(
                    window,
                    &raw mut left,
                    &raw mut top,
                    &raw mut right,
                    &raw mut bottom,
                );
                pixel_scale = [
                    (right - left) as f32 / boxel_width,
                    (top - bottom) as f32 / boxel_height,
                ];
            }
            let current_bounds = [left, top, right, bottom];
            if actions.drag_delta != [0.0, 0.0] {
                self.drag_remainder[0] += actions.drag_delta[0] * pixel_scale[0];
                self.drag_remainder[1] += actions.drag_delta[1] * pixel_scale[1];
                let horizontal = self.drag_remainder[0].round() as i32;
                let vertical = self.drag_remainder[1].round() as i32;
                self.drag_remainder[0] -= horizontal as f32;
                self.drag_remainder[1] -= vertical as f32;
                left += horizontal;
                right += horizontal;
                top -= vertical;
                bottom -= vertical;
            } else {
                self.drag_remainder = [0.0, 0.0];
            }
            right += (actions.resize_delta[0] * pixel_scale[0]).round() as i32;
            bottom -= (actions.resize_delta[1] * pixel_scale[1]).round() as i32;
            let minimum_width = (self.minimum_size[0] as f32 * pixel_scale[0]).round() as i32;
            let minimum_height = (self.minimum_size[1] as f32 * pixel_scale[1]).round() as i32;
            right = right.max(left + minimum_width);
            if let Some(height) = actions.height {
                bottom = top - ((height * pixel_scale[1]).round() as i32).max(minimum_height);
            } else {
                bottom = bottom.min(top - minimum_height);
            }
            if current_bounds != [left, top, right, bottom] {
                if popped_out {
                    xplane_sys::XPLMSetWindowGeometryOS(window, left, top, right, bottom);
                } else {
                    xplane_sys::XPLMSetWindowGeometry(window, left, top, right, bottom);
                }
            }
        }
    }
}

impl Floater {
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
            xplane_sys::XPLMSetWindowResizingLimits(window, 520, 180, 4800, 3200);
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
