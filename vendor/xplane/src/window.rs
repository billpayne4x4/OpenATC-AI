// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::ffi::{c_char, c_int, c_void};
use std::{marker::PhantomData, mem, ops::Deref, ptr};

use num_enum::{IntoPrimitive, TryFromPrimitive};
use snafu::prelude::*;

#[allow(clippy::wildcard_imports)]
use xplane_sys::*;

use crate::{make_x, NoSendSync};

use super::geometry::{Point, Rect};

/// Struct to access window APIs.
pub struct WindowApi {
    _phantom: NoSendSync,
}

impl WindowApi {
    /// Creates a new window with the provided geometry and returns a reference to it
    ///
    /// The window is originally not visible.
    #[must_use]
    pub fn create_window<R: Into<Rect<i32>>, D: WindowDelegate>(
        geometry: R,
        delegate: D,
    ) -> WindowRef {
        Window::create(geometry, delegate)
    }
}

/// Cursor states that windows can apply
#[derive(Debug, Clone, Default)]
pub enum Cursor {
    /// X-Plane draws the default cursor
    #[default]
    Default,
    /// X-Plane draws an arrow cursor (not any other cursor type)
    Arrow,
    /// X-Plane hides the cursor. The plugin should draw its own cursor.
    Hide,
}

#[allow(clippy::cast_possible_wrap)]
impl From<Cursor> for XPLMCursorStatus {
    fn from(value: Cursor) -> Self {
        match value {
            Cursor::Default => XPLMCursorStatus::Default,
            Cursor::Arrow => XPLMCursorStatus::Arrow,
            Cursor::Hide => XPLMCursorStatus::Hidden,
        }
    }
}

/// Trait for things that can define the behavior of a window
pub trait WindowDelegate: 'static {
    /// Draws this window.
    /// You will need to perform all OpenGL calls and related XPLM calls
    /// unsafely.
    fn draw(&mut self, window: &Window);
    /// Handles a keyboard event
    ///
    /// The default implementation does nothing
    fn keyboard_event(&mut self, _window: &Window, _event: KeyEvent) {}
    /// Handles a mouse event
    ///
    /// Return false to consume the event or true to propagate it.
    ///
    /// The default implementation does nothing and allows the event to propagate.
    fn mouse_event(&mut self, _window: &Window, _event: MouseEvent) -> bool {
        true
    }
    /// Handles a scroll event
    ///
    /// Return false to consume the event or true to propagate it.
    ///
    /// The default implementation does nothing and allows the event to propagate.
    fn scroll_event(&mut self, _window: &Window, _event: ScrollEvent) -> bool {
        true
    }
    /// Tells X-Plane what cursor to draw over a section of the window
    ///
    /// The default implementation allows X-Plane to draw the default cursor.
    fn cursor(&mut self, _window: &Window, _position: Point<i32>) -> Cursor {
        Cursor::Default
    }
}

/// A reference to a window
pub struct WindowRef {
    /// The window
    window: *mut Window,
}

impl Deref for WindowRef {
    type Target = Window;
    fn deref(&self) -> &Self::Target {
        unsafe { self.window.as_ref().unwrap() } // This will not be a null pointer.
    }
}

impl Drop for WindowRef {
    fn drop(&mut self) {
        unsafe {
            let _ = Box::from_raw(self.window);
        }
    }
}

/// A basic window that may appear on the screen
///
/// A window has a position and size, but no appearance. Plugins must draw in their draw callbacks
/// to make windows appear.
pub struct Window {
    /// The window ID
    id: xplane_sys::XPLMWindowID,
    /// The delegate
    delegate: *mut dyn WindowDelegate,
    _phantom: NoSendSync,
}

impl Window {
    fn create<R: Into<Rect<i32>>, D: WindowDelegate>(geometry: R, delegate: D) -> WindowRef {
        let geometry = geometry.into();

        let window_ptr = Box::into_raw(Box::new(Window {
            id: ptr::null_mut(),
            delegate: Box::into_raw(Box::new(delegate)), // This pointer should never end up null.
            _phantom: PhantomData,
        }));

        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let mut window_info = xplane_sys::XPLMCreateWindow_t {
            structSize: mem::size_of::<xplane_sys::XPLMCreateWindow_t>() as _,
            left: geometry.left(),
            top: geometry.top(),
            right: geometry.right(),
            bottom: geometry.bottom(),
            visible: 0,
            drawWindowFunc: Some(window_draw),
            handleMouseClickFunc: Some(window_mouse),
            handleKeyFunc: Some(window_key),
            handleCursorFunc: Some(window_cursor),
            handleMouseWheelFunc: Some(window_scroll),
            refcon: window_ptr.cast(),
            #[cfg(feature = "XPLM301")]
            decorateAsFloatingWindow: XPLMWindowDecoration::None,
            #[cfg(feature = "XPLM300")]
            layer: XPLMWindowLayer::FloatingWindows,
            #[cfg(feature = "XPLM300")]
            handleRightClickFunc: Some(window_mouse),
        };

        let window_id = unsafe { xplane_sys::XPLMCreateWindowEx(&mut window_info) };
        unsafe {
            (*window_ptr).id = window_id;
        }

        WindowRef { window: window_ptr } // This pointer should never end up null.
    }

    /// Returns the geometry of this window
    #[must_use]
    pub fn geometry(&self) -> Rect<i32> {
        unsafe {
            let mut left = 0;
            let mut top = 0;
            let mut right = 0;
            let mut bottom = 0;
            xplane_sys::XPLMGetWindowGeometry(
                self.id,
                &mut left,
                &mut top,
                &mut right,
                &mut bottom,
            );
            Rect::from_left_top_right_bottom(left, top, right, bottom)
        }
    }
    /// Sets the geometry of this window
    pub fn set_geometry<R: Into<Rect<i32>>>(&self, geometry: R) {
        let geometry = geometry.into();
        unsafe {
            xplane_sys::XPLMSetWindowGeometry(
                self.id,
                geometry.left(),
                geometry.top(),
                geometry.right(),
                geometry.bottom(),
            );
        }
    }

    /// Returns true if this window is visible
    #[must_use]
    pub fn visible(&self) -> bool {
        1 == unsafe { xplane_sys::XPLMGetWindowIsVisible(self.id) }
    }
    /// Sets the window as visible or invisible
    pub fn set_visible(&self, visible: bool) {
        unsafe {
            xplane_sys::XPLMSetWindowIsVisible(self.id, i32::from(visible));
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            xplane_sys::XPLMDestroyWindow(self.id);
            let _ = Box::from_raw(self.delegate);
        }
    }
}

/// Callback in which windows are drawn
unsafe extern "C-unwind" fn window_draw(_window: xplane_sys::XPLMWindowID, refcon: *mut c_void) {
    let window = unsafe { refcon.cast::<Window>().as_mut().unwrap() }; // This pointer should not be null.
    unsafe {
        window.delegate.as_mut().unwrap().draw(window);
    } // This will not be a null pointer.
}

/// Keyboard callback
unsafe extern "C-unwind" fn window_key(
    _window: xplane_sys::XPLMWindowID,
    key: c_char,
    flags: xplane_sys::XPLMKeyFlags,
    virtual_key: c_char,
    refcon: *mut c_void,
    losing_focus: c_int,
) {
    if losing_focus == 0 {
        match KeyEvent::from_xplm(key, flags, virtual_key) {
            Ok(event) => {
                let window = unsafe { refcon.cast::<Window>().as_mut().unwrap() }; // This pointer should not be null.
                unsafe {
                    window
                        .delegate
                        .as_mut()
                        .unwrap() // This will not be a null pointer.
                        .keyboard_event(window, event);
                }
            }
            Err(e) => {
                let mut x = make_x();
                super::debugln!(x, "Invalid key event received: {:?}", e).unwrap();
                // This should always be a valid string.
            }
        }
    }
}

/// Mouse callback
unsafe extern "C-unwind" fn window_mouse(
    _window: xplane_sys::XPLMWindowID,
    x: c_int,
    y: c_int,
    status: xplane_sys::XPLMMouseStatus,
    refcon: *mut c_void,
) -> c_int {
    if let Ok(action) = MouseAction::try_from(status) {
        let position = Point::from((x, y));
        let event = MouseEvent::new(position, action);
        let window = unsafe { refcon.cast::<Window>().as_mut().unwrap() }; // This pointer should not be null.
        let propagate = unsafe { window.delegate.as_mut().unwrap().mouse_event(window, event) }; // This will not be a null pointer.
        i32::from(!propagate)
    } else {
        // Propagate
        0
    }
}

/// Cursor callback
unsafe extern "C-unwind" fn window_cursor(
    _window: xplane_sys::XPLMWindowID,
    x: c_int,
    y: c_int,
    refcon: *mut c_void,
) -> xplane_sys::XPLMCursorStatus {
    let window = unsafe { refcon.cast::<Window>().as_mut().unwrap() }; // This pointer should not be null.
    let cursor = unsafe {
        window
            .delegate
            .as_mut()
            .unwrap() // This will not be a null pointer.
            .cursor(window, Point::from((x, y)))
    };
    cursor.into()
}

/// Scroll callback
unsafe extern "C-unwind" fn window_scroll(
    _window: xplane_sys::XPLMWindowID,
    x: c_int,
    y: c_int,
    wheel: c_int,
    clicks: c_int,
    refcon: *mut c_void,
) -> c_int {
    let position = Point::from((x, y));
    let (dx, dy) = if wheel == 1 {
        // Horizontal
        (clicks, 0)
    } else {
        // Vertical
        (0, clicks)
    };
    let event = ScrollEvent::new(position, dx, dy);

    let window = unsafe { refcon.cast::<Window>().as_mut().unwrap() }; // This pointer should not be null.
    let propagate = unsafe {
        window
            .delegate
            .as_mut()
            .unwrap() // This will not be a null pointer.
            .scroll_event(window, event)
    };
    i32::from(!propagate)
}

/// Key actions
#[derive(Debug, Clone)]
pub enum KeyAction {
    /// The key was pressed down
    Press,
    /// The key was released
    Release,
}

/// Keys that may be pressed
#[derive(Debug, Clone, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[allow(missing_docs)]
#[repr(u32)]
pub enum Key {
    Back = XPLM_VK_BACK,
    Tab = XPLM_VK_TAB,
    Clear = XPLM_VK_CLEAR,
    Return = XPLM_VK_RETURN,
    Escape = XPLM_VK_ESCAPE,
    Space = XPLM_VK_SPACE,
    Prior = XPLM_VK_PRIOR,
    Next = XPLM_VK_NEXT,
    End = XPLM_VK_END,
    Home = XPLM_VK_HOME,
    Left = XPLM_VK_LEFT,
    Up = XPLM_VK_UP,
    Right = XPLM_VK_RIGHT,
    Down = XPLM_VK_DOWN,
    Select = XPLM_VK_SELECT,
    Print = XPLM_VK_PRINT,
    Execute = XPLM_VK_EXECUTE,
    Snapshot = XPLM_VK_SNAPSHOT,
    Insert = XPLM_VK_INSERT,
    Delete = XPLM_VK_DELETE,
    Help = XPLM_VK_HELP,
    /// The 0 key in the number row.
    Key0 = XPLM_VK_0,
    /// The 1 key in the number row
    Key1 = XPLM_VK_1,
    /// The 2 key in the number row
    Key2 = XPLM_VK_2,
    /// The 3 key in the number row
    Key3 = XPLM_VK_3,
    /// The 4 key in the number row
    Key4 = XPLM_VK_4,
    /// The 5 key in the number row
    Key5 = XPLM_VK_5,
    /// The 6 key in the number row
    Key6 = XPLM_VK_6,
    /// The 7 key in the number row
    Key7 = XPLM_VK_7,
    /// The 8 key in the number row
    Key8 = XPLM_VK_8,
    /// The 9 key in the number row
    Key9 = XPLM_VK_9,
    A = XPLM_VK_A,
    B = XPLM_VK_B,
    C = XPLM_VK_C,
    D = XPLM_VK_D,
    E = XPLM_VK_E,
    F = XPLM_VK_F,
    G = XPLM_VK_G,
    H = XPLM_VK_H,
    I = XPLM_VK_I,
    J = XPLM_VK_J,
    K = XPLM_VK_K,
    L = XPLM_VK_L,
    M = XPLM_VK_M,
    N = XPLM_VK_N,
    O = XPLM_VK_O,
    P = XPLM_VK_P,
    Q = XPLM_VK_Q,
    R = XPLM_VK_R,
    S = XPLM_VK_S,
    T = XPLM_VK_T,
    U = XPLM_VK_U,
    V = XPLM_VK_V,
    W = XPLM_VK_W,
    X = XPLM_VK_X,
    Y = XPLM_VK_Y,
    Z = XPLM_VK_Z,
    /// The 0 key on the numerical keypad
    Numpad0 = XPLM_VK_NUMPAD0,
    /// The 1 key on the numerical keypad
    Numpad1 = XPLM_VK_NUMPAD1,
    /// The 2 key on the numerical keypad
    Numpad2 = XPLM_VK_NUMPAD2,
    /// The 3 key on the numerical keypad
    Numpad3 = XPLM_VK_NUMPAD3,
    /// The 4 key on the numerical keypad
    Numpad4 = XPLM_VK_NUMPAD4,
    /// The 5 key on the numerical keypad
    Numpad5 = XPLM_VK_NUMPAD5,
    /// The 6 key on the numerical keypad
    Numpad6 = XPLM_VK_NUMPAD6,
    /// The 7 key on the numerical keypad
    Numpad7 = XPLM_VK_NUMPAD7,
    /// The 8 key on the numerical keypad
    Numpad8 = XPLM_VK_NUMPAD8,
    /// The 9 key on the numerical keypad
    Numpad9 = XPLM_VK_NUMPAD9,
    Multiply = XPLM_VK_MULTIPLY,
    Add = XPLM_VK_ADD,
    Separator = XPLM_VK_SEPARATOR,
    Subtract = XPLM_VK_SUBTRACT,
    Decimal = XPLM_VK_DECIMAL,
    Divide = XPLM_VK_DIVIDE,
    F1 = XPLM_VK_F1,
    F2 = XPLM_VK_F2,
    F3 = XPLM_VK_F3,
    F4 = XPLM_VK_F4,
    F5 = XPLM_VK_F5,
    F6 = XPLM_VK_F6,
    F7 = XPLM_VK_F7,
    F8 = XPLM_VK_F8,
    F9 = XPLM_VK_F9,
    F10 = XPLM_VK_F10,
    F11 = XPLM_VK_F11,
    F12 = XPLM_VK_F12,
    F13 = XPLM_VK_F13,
    F14 = XPLM_VK_F14,
    F15 = XPLM_VK_F15,
    F16 = XPLM_VK_F16,
    F17 = XPLM_VK_F17,
    F18 = XPLM_VK_F18,
    F19 = XPLM_VK_F19,
    F20 = XPLM_VK_F20,
    F21 = XPLM_VK_F21,
    F22 = XPLM_VK_F22,
    F23 = XPLM_VK_F23,
    F24 = XPLM_VK_F24,
    Equal = XPLM_VK_EQUAL,
    Minus = XPLM_VK_MINUS,
    ClosingBrace = XPLM_VK_RBRACE,
    OpeningBrace = XPLM_VK_LBRACE,
    Quote = XPLM_VK_QUOTE,
    Semicolon = XPLM_VK_SEMICOLON,
    Backslash = XPLM_VK_BACKSLASH,
    Comma = XPLM_VK_COMMA,
    Slash = XPLM_VK_SLASH,
    Period = XPLM_VK_PERIOD,
    Backquote = XPLM_VK_BACKQUOTE,
    /// Enter, also known as return in Mac OS
    Enter = XPLM_VK_ENTER,
    NumpadEnter = XPLM_VK_NUMPAD_ENT,
    NumpadEqual = XPLM_VK_NUMPAD_EQ,
}

/// An event associated with a key press
#[derive(Debug)]
pub struct KeyEvent {
    /// A character representing the key
    basic_char: Option<char>,
    /// The key
    key: Key,
    /// The action
    action: KeyAction,
    /// If the control key was pressed
    control_pressed: bool,
    /// If the option/alt key was pressed
    alt_pressed: bool,
    /// If the shift key was pressed
    shift_pressed: bool,
}

impl KeyEvent {
    /// Creates a key event from XPLM key information
    #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
    fn from_xplm(
        key: c_char,
        flags: xplane_sys::XPLMKeyFlags,
        virtual_key: c_char,
    ) -> Result<Self, KeyEventError> {
        let basic_char = match key as u8 {
            // Accept printable characters, including spaces and tabs
            b'\t' | b' '..=b'~' => Some(key as u8 as char),
            _ => None,
        };
        let action = if flags.down() {
            KeyAction::Press
        } else if flags.up() {
            KeyAction::Release
        } else {
            return Err(KeyEventError::InvalidFlags { flags });
        };
        let Ok(key) = Key::try_from_primitive(virtual_key as u32) else {
            return Err(KeyEventError::InvalidKey { key: virtual_key });
        };

        Ok(KeyEvent {
            basic_char,
            key,
            action,
            control_pressed: flags.ctrl(),
            alt_pressed: flags.option_alt(),
            shift_pressed: flags.shift(),
        })
    }
    /// Returns the character corresponding to the key associated with this event, if one exists
    ///
    /// Some key combinations, including combinations with non-Shift modifiers, may not have
    /// corresponding characters.
    #[must_use]
    pub fn char(&self) -> Option<char> {
        self.basic_char
    }
    /// Returns the key associated with this event
    #[must_use]
    pub fn key(&self) -> Key {
        self.key.clone()
    }
    /// Returns true if the control key was held down when the action occurred
    #[must_use]
    pub fn control_pressed(&self) -> bool {
        self.control_pressed
    }
    /// Returns true if the option/alt key was held down when the action occurred
    #[must_use]
    pub fn option_pressed(&self) -> bool {
        self.alt_pressed
    }
    /// Returns true if a shift key was held down when the action occurred
    #[must_use]
    pub fn shift_pressed(&self) -> bool {
        self.shift_pressed
    }
    /// Returns the key action that occurred
    #[must_use]
    pub fn action(&self) -> KeyAction {
        self.action.clone()
    }
}

/// Key event creation error
#[derive(Snafu, Debug)]
enum KeyEventError {
    #[snafu(display("Unexpected key flags {flags:?}"))]
    InvalidFlags { flags: xplane_sys::XPLMKeyFlags },

    #[snafu(display("Invalid or unsupported key with code: 0x{key:x}"))]
    InvalidKey { key: c_char },
}

/// Actions that the mouse/cursor can perform
#[derive(Debug, Clone)]
pub enum MouseAction {
    /// The user pressed the mouse button down
    Down,
    /// The user moved the mouse with the mouse button down
    Drag,
    /// The user released the mouse button
    Up,
}

impl TryFrom<XPLMMouseStatus> for MouseAction {
    type Error = XPLMMouseStatus;
    fn try_from(value: XPLMMouseStatus) -> Result<Self, Self::Error> {
        match value {
            XPLMMouseStatus::Down => Ok(MouseAction::Down),
            XPLMMouseStatus::Drag => Ok(MouseAction::Drag),
            XPLMMouseStatus::Up => Ok(MouseAction::Up),
            _ => Err(value),
        }
    }
}

/// A mouse event
#[derive(Debug)]
pub struct MouseEvent {
    /// The position of the mouse, in global window coordinates
    position: Point<i32>,
    /// The action of the mouse
    action: MouseAction,
}

impl MouseEvent {
    /// Creates a new event
    fn new(position: Point<i32>, action: MouseAction) -> Self {
        MouseEvent { position, action }
    }
    /// Returns the position of the mouse, in global coordinates relative to the X-Plane
    /// main window
    #[must_use]
    pub fn position(&self) -> Point<i32> {
        self.position
    }
    /// Returns the action that the user performed with the mouse
    #[must_use]
    pub fn action(&self) -> MouseAction {
        self.action.clone()
    }
}

/// A scroll event
#[derive(Debug, Clone)]
pub struct ScrollEvent {
    /// The position of the mouse, in global window coordinates
    position: Point<i32>,
    /// The amount of scroll in the X direction
    scroll_x: i32,
    /// The amount of scroll in the Y direction
    scroll_y: i32,
}

impl ScrollEvent {
    /// Creates a new event
    fn new(position: Point<i32>, scroll_x: i32, scroll_y: i32) -> Self {
        ScrollEvent {
            position,
            scroll_x,
            scroll_y,
        }
    }
    /// Returns the position of the mouse, in global coordinates relative to the X-Plane
    /// main window
    #[must_use]
    pub fn position(&self) -> Point<i32> {
        self.position
    }
    /// Returns the amount of scroll in the X direction
    #[must_use]
    pub fn scroll_x(&self) -> i32 {
        self.scroll_x
    }
    /// Returns the amount of scroll in the Y direction
    #[must_use]
    pub fn scroll_y(&self) -> i32 {
        self.scroll_y
    }
}
