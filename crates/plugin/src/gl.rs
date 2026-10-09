//! OpenGL symbol lookup for the simulator's current rendering context.
use std::ffi::c_void;

pub(super) fn library_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "opengl32.dll"
    } else if cfg!(target_os = "macos") {
        "/System/Library/Frameworks/OpenGL.framework/OpenGL"
    } else {
        "libOpenGL.so.0"
    }
}

/// The library must stay loaded, and the simulator's GL context must be current.
pub(super) unsafe fn symbol(library: &libloading::Library, name: &str) -> *const c_void {
    #[cfg(windows)]
    {
        type GetProc = unsafe extern "system" fn(*const std::ffi::c_char) -> *const c_void;
        if let Ok(get_proc) = unsafe { library.get::<GetProc>(b"wglGetProcAddress\0") } {
            if let Ok(name) = std::ffi::CString::new(name) {
                let address = unsafe { get_proc(name.as_ptr()) };
                if !matches!(address as usize, 0 | 1 | 2 | 3 | usize::MAX) {
                    return address;
                }
            }
        }
    }
    unsafe { library.get::<*const c_void>(name.as_bytes()) }
        .map_or(std::ptr::null(), |symbol| *symbol)
}
