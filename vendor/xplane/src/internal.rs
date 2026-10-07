// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::ffi::c_char;
use std::{ffi::CString, ptr};

use crate::XPAPI;

/// Copies up to 256 bytes (including null termination) to
/// the provided destination. If the provided source string is too long, it will be
/// truncated.
pub unsafe fn copy_to_c_buffer(mut src: String, dest: *mut c_char) {
    // Truncate to 255 bytes (256 including the null terminator)
    src.truncate(255);
    let src_c = CString::new(src).unwrap_or_else(|_| CString::new("<invalid>").unwrap());
    let src_c_length = src_c.to_bytes_with_nul().len();
    debug_assert!(src_c_length <= 256);
    unsafe {
        ptr::copy_nonoverlapping(src_c.as_ptr(), dest, src_c_length);
    }
}

/// Performs initialization required for the XPLM crate to work correctly
pub fn xplm_init(x: &mut XPAPI) {
    super::paths::path_init(x);
}
