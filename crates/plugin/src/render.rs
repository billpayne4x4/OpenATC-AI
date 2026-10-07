//! OpenGL rendering and simulator mouse/keyboard input for ImGui.

use crate::geometry::FrameLayout;
use openatc_ui::interface::{WindowActions, WindowFrame};
use std::ffi::{CString, c_char, c_int, c_void};
use std::time::Instant;

/// Panel draw callback. Everything here runs on the simulator main thread,
// so no Send bound is needed. The offset positions content inside the
// floating window; see below.
pub type DrawCallback = Box<dyn FnMut(&imgui::Ui, WindowFrame) -> WindowActions>;

/// Screen pixels as floats. Display coordinates stay in the low thousands,
// exactly representable in f32.
#[allow(clippy::cast_precision_loss)]
fn px(pixels: i32) -> f32 {
    pixels as f32
}

struct TransformRefs {
    projection: xplane_sys::XPLMDataRef,
    modelview: xplane_sys::XPLMDataRef,
    viewport: xplane_sys::XPLMDataRef,
}

impl TransformRefs {
    fn new() -> Self {
        unsafe {
            Self {
                projection: xplane_sys::XPLMFindDataRef(
                    c"sim/graphics/view/projection_matrix".as_ptr(),
                ),
                modelview: xplane_sys::XPLMFindDataRef(
                    c"sim/graphics/view/modelview_matrix".as_ptr(),
                ),
                viewport: xplane_sys::XPLMFindDataRef(c"sim/graphics/view/viewport".as_ptr()),
            }
        }
    }

    fn layout(&self, bounds: [i32; 4]) -> Option<FrameLayout> {
        if self.projection.is_null() || self.modelview.is_null() || self.viewport.is_null() {
            return None;
        }
        let mut projection = [0.0; 16];
        let mut modelview = [0.0; 16];
        let mut viewport = [0; 4];
        unsafe {
            if xplane_sys::XPLMGetDatavf(self.projection, projection.as_mut_ptr(), 0, 16) != 16
                || xplane_sys::XPLMGetDatavf(self.modelview, modelview.as_mut_ptr(), 0, 16) != 16
                || xplane_sys::XPLMGetDatavi(self.viewport, viewport.as_mut_ptr(), 0, 4) != 4
            {
                return None;
            }
        }
        FrameLayout::from_matrices(bounds, &projection, &modelview, viewport)
    }
}

/// Renderer state behind the window refcon.
pub struct RenderState {
    /// imgui context for the panel.
    pub imgui: imgui::Context,
    /// GL renderer, built on first draw with the sim context current.
    pub renderer: Option<imgui_glow_renderer::AutoRenderer>,
    /// GL bindings resolved from the host.
    pub gl: Option<glow::Context>,
    /// Whether the GL side initialized.
    pub ready: bool,
    /// Last frame time for delta clamping.
    pub previous: Instant,
    /// Panel draw callback installed by the plugin.
    pub draw_fn: Option<DrawCallback>,
    transform_refs: TransformRefs,
    window_offset: [f32; 2],
    ui_scale: f32,
    keyboard_focus: bool,
    transform_error_logged: bool,
}

impl RenderState {
    /// Fresh backend with its own imgui context and style.
    #[must_use]
    pub fn new() -> Self {
        let mut imgui = imgui::Context::create();
        imgui.set_clipboard_backend(openatc_ui::clipboard::Backend::default());
        imgui.set_ini_filename(None);
        openatc_ui::widgets::configure_fonts(&mut imgui);
        imgui.io_mut().backend_flags |= imgui::BackendFlags::RENDERER_HAS_VTX_OFFSET;
        Self {
            imgui,
            renderer: None,
            gl: None,
            ready: false,
            previous: Instant::now(),
            draw_fn: None,
            transform_refs: TransformRefs::new(),
            window_offset: [0.0, 0.0],
            ui_scale: 1.0,
            keyboard_focus: false,
            transform_error_logged: false,
        }
    }

    fn log(text: &str) {
        if let Ok(line) = CString::new(text) {
            unsafe { xplane_sys::XPLMDebugString(line.as_ptr()) };
        }
    }

    /// Window geometry in global screen coordinates.
    fn geometry(window: xplane_sys::XPLMWindowID) -> (i32, i32, i32, i32) {
        let (mut left, mut top, mut right, mut bottom) = (0, 0, 0, 0);
        unsafe {
            xplane_sys::XPLMGetWindowGeometry(
                window,
                &raw mut left,
                &raw mut top,
                &raw mut right,
                &raw mut bottom,
            );
        }
        (left, top, right, bottom)
    }

    /// Feed an absolute mouse position into imgui coordinates.
    fn mouse_position(&mut self, window: xplane_sys::XPLMWindowID, x: i32, y: i32) {
        let (left, top, _, _) = Self::geometry(window);
        self.imgui.io_mut().add_mouse_pos_event([
            self.window_offset[0] + px(x - left),
            self.window_offset[1] + px(top - y),
        ]);
    }

    pub fn on_draw(&mut self, window: xplane_sys::XPLMWindowID) -> WindowActions {
        let (left, top, right, bottom) = Self::geometry(window);
        let Some(layout) = self.transform_refs.layout([left, top, right, bottom]) else {
            if !self.transform_error_logged {
                Self::log("OpenATC AI: invalid window drawing transform");
                self.transform_error_logged = true;
            }
            return WindowActions::default();
        };
        if !self.ready {
            self.ready = self.init_gl();
            if !self.ready {
                return WindowActions::default();
            }
        }
        self.window_offset = layout.window_position;
        {
            let io = self.imgui.io_mut();
            io.display_size = layout.display_size;
            io.display_framebuffer_scale = layout.framebuffer_scale;
            let now = Instant::now();
            io.delta_time = now
                .duration_since(self.previous)
                .as_secs_f32()
                .clamp(0.001, 0.1);
            self.previous = now;
        }
        let mut global_x = 0;
        let mut global_y = 0;
        unsafe { xplane_sys::XPLMGetMouseLocationGlobal(&raw mut global_x, &raw mut global_y) };
        self.mouse_position(window, global_x, global_y);
        let frame = WindowFrame {
            position: layout.window_position,
            size: layout.window_size,
            can_pop_out: unsafe { xplane_sys::XPLMWindowIsInVR(window) == 0 },
            popped_out: unsafe { xplane_sys::XPLMWindowIsPoppedOut(window) != 0 },
        };
        let ui = self.imgui.frame();
        let actions = self
            .draw_fn
            .as_mut()
            .map_or_else(WindowActions::default, |draw| draw(ui, frame));
        if let Some(renderer) = self.renderer.as_mut() {
            unsafe { xplane_sys::XPLMSetGraphicsState(0, 1, 0, 0, 1, 0, 0) };
            if renderer.render(self.imgui.render()).is_err() {
                Self::log("OpenATC AI: frame render failed");
            }
        }
        if self.keyboard_focus
            && !self.imgui.io().want_text_input
            && !self.imgui.io().mouse_down.iter().any(|down| *down)
        {
            unsafe { xplane_sys::XPLMTakeKeyboardFocus(std::ptr::null_mut()) };
            self.keyboard_focus = false;
        }
        if actions.ui_scale > 0.0 && (actions.ui_scale - self.ui_scale).abs() > 0.001 {
            self.ui_scale = actions.ui_scale;
            openatc_ui::widgets::configure_scale(&mut self.imgui, self.ui_scale);
        }
        actions
    }

    /// Resolve libGL and build the glow renderer with the sim context current.
    fn init_gl(&mut self) -> bool {
        if self.gl.is_none() {
            let library = unsafe { libloading::Library::new("libOpenGL.so.0").ok() };
            let Some(library) = library else {
                Self::log("OpenATC AI: libOpenGL.so.0 unavailable");
                return false;
            };
            // The library handle must outlive the context; leak it like the
            // Keep the library loaded while its function pointers remain in use.
            let library: &'static libloading::Library = Box::leak(Box::new(library));
            let gl = unsafe {
                glow::Context::from_loader_function(|name| {
                    library
                        .get::<*const c_void>(name.as_bytes())
                        .map_or(std::ptr::null(), |symbol| *symbol)
                })
            };
            self.gl = Some(gl);
        }
        if self.renderer.is_none() {
            let Some(gl) = self.gl.take() else {
                return false;
            };
            if let Ok(renderer) = imgui_glow_renderer::AutoRenderer::new(gl, &mut self.imgui) {
                self.renderer = Some(renderer);
            } else {
                Self::log("OpenATC AI: GL renderer unavailable");
                return false;
            }
        }
        true
    }

    /// Mouse click or right-click handler.
    pub fn on_mouse(
        &mut self,
        window: xplane_sys::XPLMWindowID,
        x: i32,
        y: i32,
        status: xplane_sys::XPLMMouseStatus,
        right: bool,
    ) {
        self.mouse_position(window, x, y);
        let button = if right {
            imgui::MouseButton::Right
        } else {
            imgui::MouseButton::Left
        };
        // XPLMMouseStatus values: 1 down, 2 drag, 3 up.
        let down = status == xplane_sys::XPLMMouseStatus::Down
            || status == xplane_sys::XPLMMouseStatus::Drag;
        self.imgui.io_mut().add_mouse_button_event(button, down);
        if !right && status == xplane_sys::XPLMMouseStatus::Down {
            unsafe { xplane_sys::XPLMTakeKeyboardFocus(window) };
            self.keyboard_focus = true;
        }
    }

    /// Mouse wheel handler.
    pub fn on_wheel(
        &mut self,
        window: xplane_sys::XPLMWindowID,
        x: i32,
        y: i32,
        wheel: i32,
        clicks: i32,
    ) {
        self.mouse_position(window, x, y);
        // Click counts are tiny whole numbers.
        #[allow(clippy::cast_precision_loss)]
        let clicks = clicks as f32;
        self.imgui.io_mut().add_mouse_wheel_event([
            if wheel != 0 { clicks } else { 0.0 },
            if wheel != 0 { 0.0 } else { clicks },
        ]);
    }

    /// Cursor query handler.
    pub fn on_cursor(&mut self, window: xplane_sys::XPLMWindowID, x: i32, y: i32) {
        self.mouse_position(window, x, y);
    }

    /// Convert simulator virtual keys into UI input events.
    pub fn on_key(
        &mut self,
        _window: xplane_sys::XPLMWindowID,
        character: c_char,
        flags: xplane_sys::XPLMKeyFlags,
        virtual_key: c_char,
        losing_focus: c_int,
    ) {
        let io = self.imgui.io_mut();
        io.app_focus_lost = losing_focus != 0;
        if losing_focus != 0 {
            self.keyboard_focus = false;
            return;
        }
        // Decode the XPLM modifier flags.
        let down = (flags & xplane_sys::XPLMKeyFlags::Up).0 == 0;
        io.add_key_event(
            imgui::Key::ModShift,
            (flags & xplane_sys::XPLMKeyFlags::Shift).0 != 0,
        );
        io.add_key_event(
            imgui::Key::ModCtrl,
            (flags & xplane_sys::XPLMKeyFlags::Control).0 != 0,
        );
        io.add_key_event(
            imgui::Key::ModAlt,
            (flags & xplane_sys::XPLMKeyFlags::OptionAlt).0 != 0,
        );
        let code = u8::try_from(virtual_key).unwrap_or(0);
        let mapped = match code {
            8 => Some(imgui::Key::Backspace),
            9 => Some(imgui::Key::Tab),
            13 => Some(imgui::Key::Enter),
            27 => Some(imgui::Key::Escape),
            37 => Some(imgui::Key::LeftArrow),
            38 => Some(imgui::Key::UpArrow),
            39 => Some(imgui::Key::RightArrow),
            40 => Some(imgui::Key::DownArrow),
            46 => Some(imgui::Key::Delete),
            36 => Some(imgui::Key::Home),
            35 => Some(imgui::Key::End),
            b'A'..=b'Z' => Some(match code {
                b'A' => imgui::Key::A,
                b'B' => imgui::Key::B,
                b'C' => imgui::Key::C,
                b'D' => imgui::Key::D,
                b'E' => imgui::Key::E,
                b'F' => imgui::Key::F,
                b'G' => imgui::Key::G,
                b'H' => imgui::Key::H,
                b'I' => imgui::Key::I,
                b'J' => imgui::Key::J,
                b'K' => imgui::Key::K,
                b'L' => imgui::Key::L,
                b'M' => imgui::Key::M,
                b'N' => imgui::Key::N,
                b'O' => imgui::Key::O,
                b'P' => imgui::Key::P,
                b'Q' => imgui::Key::Q,
                b'R' => imgui::Key::R,
                b'S' => imgui::Key::S,
                b'T' => imgui::Key::T,
                b'U' => imgui::Key::U,
                b'V' => imgui::Key::V,
                b'W' => imgui::Key::W,
                b'X' => imgui::Key::X,
                b'Y' => imgui::Key::Y,
                _ => imgui::Key::Z,
            }),
            _ => None,
        };
        if let Some(key) = mapped {
            io.add_key_event(key, down);
        }
        let character = character as u8;
        if down
            && character >= 32
            && character != 127
            && (flags & xplane_sys::XPLMKeyFlags::Control).0 == 0
            && (flags & xplane_sys::XPLMKeyFlags::OptionAlt).0 == 0
        {
            io.add_input_character(char::from(character));
        }
        if down && mapped == Some(imgui::Key::Escape) {
            unsafe { xplane_sys::XPLMTakeKeyboardFocus(std::ptr::null_mut()) };
        }
    }
}

impl Default for RenderState {
    fn default() -> Self {
        Self::new()
    }
}
