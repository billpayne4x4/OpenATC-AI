// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::ffi::c_char;
use std::ffi::CStr;

use xplane_sys::XPLMSetErrorCallback;

use crate::{make_x, XPAPI};

/// The current handler
static mut HANDLER: Option<fn(&str)> = None;

/// Sets the error handler
///
/// Once an error handler is set, it cannot be removed.
/// <div class="warning">Do not ship production code with a call to this function!
/// This will enable extra error checking, at a significant performance penalty.
/// Only set an error handler when debugging.</div>
pub fn set_error_handler(_x: &mut XPAPI, handler: fn(&str)) {
    unsafe {
        HANDLER = Some(handler);
        XPLMSetErrorCallback(Some(error_handler));
    }
}

/// C error handler callback
unsafe extern "C-unwind" fn error_handler(message: *const c_char) {
    let message_cs = unsafe { CStr::from_ptr(message) };
    if let Ok(message_str) = message_cs.to_str() {
        if let Some(handler) = unsafe { HANDLER } {
            handler(message_str);
        }
    } else {
        let mut x = make_x();
        super::debugln!(x, "[xplm] Error handler called with an invalid message").unwrap();
    }
}
