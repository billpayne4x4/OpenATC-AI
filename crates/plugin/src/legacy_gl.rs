//! Compatibility-profile drawing for the OpenGL 2.1 context supplied by macOS.
//! The fixed-pipeline approach follows Dear `ImGui`'s OpenGL 2 backend; client
//! pointers are rebased per command to support maps with more than 64K vertices.
use imgui::{DrawCmd, DrawData, DrawIdx, DrawVert, TextureId, internal::RawWrapper};
use std::{
    ffi::c_void,
    mem::{offset_of, size_of},
};

#[allow(unsafe_op_in_unsafe_fn, clippy::all, warnings)]
mod bindings {
    include!(concat!(env!("OUT_DIR"), "/gl21.rs"));
}
use bindings as gl;

pub struct Renderer {
    gl: gl::Gl,
    font: u32,
}

impl Renderer {
    /// The loader and every drawing call must use the current host GL context.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub unsafe fn new(
        mut loader: impl FnMut(&str) -> *const c_void,
        imgui: &mut imgui::Context,
    ) -> Result<Self, &'static str> {
        let gl = gl::Gl::load_with(&mut loader);
        if !gl.PushAttrib.is_loaded() || !gl.VertexPointer.is_loaded() || !gl.UseProgram.is_loaded()
        {
            return Err("OpenGL 2.1 compatibility functions unavailable");
        }
        let mut font = 0;
        unsafe {
            gl.PushAttrib(gl::TEXTURE_BIT);
            gl.PushClientAttrib(gl::CLIENT_PIXEL_STORE_BIT);
            let mut unpack_buffer = 0;
            gl.GetIntegerv(gl::PIXEL_UNPACK_BUFFER_BINDING, &mut unpack_buffer);
            gl.BindBuffer(gl::PIXEL_UNPACK_BUFFER, 0);
            gl.ActiveTexture(gl::TEXTURE0);
            gl.GenTextures(1, &mut font);
            gl.BindTexture(gl::TEXTURE_2D, font);
            gl.TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl.TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl.PixelStorei(gl::UNPACK_ALIGNMENT, 1);
            gl.PixelStorei(gl::UNPACK_ROW_LENGTH, 0);
            gl.PixelStorei(gl::UNPACK_SKIP_PIXELS, 0);
            gl.PixelStorei(gl::UNPACK_SKIP_ROWS, 0);
            let atlas = imgui.fonts().build_rgba32_texture();
            gl.TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                atlas.width as i32,
                atlas.height as i32,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                atlas.data.as_ptr().cast(),
            );
            gl.BindBuffer(gl::PIXEL_UNPACK_BUFFER, unpack_buffer as u32);
            gl.PopClientAttrib();
            gl.PopAttrib();
        }
        if font == 0 {
            return Err("Could not allocate the OpenGL font texture");
        }
        imgui.fonts().tex_id = TextureId::new(font as usize);
        imgui.io_mut().backend_flags |= imgui::BackendFlags::RENDERER_HAS_VTX_OFFSET;
        Ok(Self { gl, font })
    }

    unsafe fn setup(&self, data: &DrawData, width: i32, height: i32) {
        let gl = &self.gl;
        unsafe {
            gl.UseProgram(0);
            gl.BindBuffer(gl::ARRAY_BUFFER, 0);
            gl.BindBuffer(gl::ELEMENT_ARRAY_BUFFER, 0);
            gl.ActiveTexture(gl::TEXTURE0);
            gl.ClientActiveTexture(gl::TEXTURE0);
            gl.Enable(gl::BLEND);
            gl.BlendEquation(gl::FUNC_ADD);
            gl.BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
            for flag in [
                gl::CULL_FACE,
                gl::DEPTH_TEST,
                gl::STENCIL_TEST,
                gl::LIGHTING,
                gl::COLOR_MATERIAL,
                gl::TEXTURE_CUBE_MAP,
            ] {
                gl.Disable(flag);
            }
            gl.Enable(gl::SCISSOR_TEST);
            gl.Enable(gl::TEXTURE_2D);
            for array in [gl::VERTEX_ARRAY, gl::TEXTURE_COORD_ARRAY, gl::COLOR_ARRAY] {
                gl.EnableClientState(array);
            }
            gl.DisableClientState(gl::NORMAL_ARRAY);
            gl.PolygonMode(gl::FRONT_AND_BACK, gl::FILL);
            gl.ShadeModel(gl::SMOOTH);
            gl.TexEnvi(gl::TEXTURE_ENV, gl::TEXTURE_ENV_MODE, gl::MODULATE as i32);
            gl.Viewport(0, 0, width, height);
            gl.MatrixMode(gl::PROJECTION);
            gl.LoadIdentity();
            gl.Ortho(
                data.display_pos[0] as f64,
                (data.display_pos[0] + data.display_size[0]) as f64,
                (data.display_pos[1] + data.display_size[1]) as f64,
                data.display_pos[1] as f64,
                -1.0,
                1.0,
            );
            gl.MatrixMode(gl::MODELVIEW);
            gl.LoadIdentity();
            gl.MatrixMode(gl::TEXTURE);
            gl.LoadIdentity();
        }
    }

    pub unsafe fn render(&self, data: &DrawData) {
        let width = (data.display_size[0] * data.framebuffer_scale[0]) as i32;
        let height = (data.display_size[1] * data.framebuffer_scale[1]) as i32;
        if width <= 0 || height <= 0 {
            return;
        }
        let gl = &self.gl;
        unsafe {
            let (mut program, mut array, mut element) = (0, 0, 0);
            gl.GetIntegerv(gl::CURRENT_PROGRAM, &mut program);
            gl.GetIntegerv(gl::ARRAY_BUFFER_BINDING, &mut array);
            gl.GetIntegerv(gl::ELEMENT_ARRAY_BUFFER_BINDING, &mut element);
            gl.PushAttrib(gl::ALL_ATTRIB_BITS);
            gl.PushClientAttrib(gl::CLIENT_ALL_ATTRIB_BITS);
            gl.ActiveTexture(gl::TEXTURE0);
            for matrix in [gl::PROJECTION, gl::MODELVIEW, gl::TEXTURE] {
                gl.MatrixMode(matrix);
                gl.PushMatrix();
            }
            self.setup(data, width, height);
            for list in data.draw_lists() {
                for command in list.commands() {
                    match command {
                        DrawCmd::Elements { count, cmd_params } => {
                            let rect = cmd_params.clip_rect;
                            let x = ((rect[0] - data.display_pos[0]) * data.framebuffer_scale[0])
                                .max(0.0);
                            let y = ((rect[1] - data.display_pos[1]) * data.framebuffer_scale[1])
                                .max(0.0);
                            let right = ((rect[2] - data.display_pos[0])
                                * data.framebuffer_scale[0])
                                .min(width as f32);
                            let bottom = ((rect[3] - data.display_pos[1])
                                * data.framebuffer_scale[1])
                                .min(height as f32);
                            if right <= x || bottom <= y {
                                continue;
                            }
                            let vertices = list.vtx_buffer();
                            let indices = list.idx_buffer();
                            if cmd_params.vtx_offset >= vertices.len()
                                || cmd_params
                                    .idx_offset
                                    .checked_add(count)
                                    .is_none_or(|end| end > indices.len())
                            {
                                continue;
                            }
                            let base = vertices.as_ptr().add(cmd_params.vtx_offset).cast::<u8>();
                            gl.VertexPointer(
                                2,
                                gl::FLOAT,
                                size_of::<DrawVert>() as i32,
                                base.add(offset_of!(DrawVert, pos)).cast(),
                            );
                            gl.TexCoordPointer(
                                2,
                                gl::FLOAT,
                                size_of::<DrawVert>() as i32,
                                base.add(offset_of!(DrawVert, uv)).cast(),
                            );
                            gl.ColorPointer(
                                4,
                                gl::UNSIGNED_BYTE,
                                size_of::<DrawVert>() as i32,
                                base.add(offset_of!(DrawVert, col)).cast(),
                            );
                            gl.Scissor(
                                x as i32,
                                height - bottom as i32,
                                (right - x) as i32,
                                (bottom - y) as i32,
                            );
                            gl.BindTexture(gl::TEXTURE_2D, cmd_params.texture_id.id() as u32);
                            gl.DrawElements(
                                gl::TRIANGLES,
                                count as i32,
                                if size_of::<DrawIdx>() == 2 {
                                    gl::UNSIGNED_SHORT
                                } else {
                                    gl::UNSIGNED_INT
                                },
                                indices.as_ptr().add(cmd_params.idx_offset).cast(),
                            );
                        }
                        DrawCmd::ResetRenderState => self.setup(data, width, height),
                        DrawCmd::RawCallback { callback, raw_cmd } => callback(list.raw(), raw_cmd),
                    }
                }
            }
            gl.ActiveTexture(gl::TEXTURE0);
            for matrix in [gl::TEXTURE, gl::MODELVIEW, gl::PROJECTION] {
                gl.MatrixMode(matrix);
                gl.PopMatrix();
            }
            gl.PopClientAttrib();
            gl.PopAttrib();
            gl.BindBuffer(gl::ELEMENT_ARRAY_BUFFER, element as u32);
            gl.BindBuffer(gl::ARRAY_BUFFER, array as u32);
            gl.UseProgram(program as u32);
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            self.gl.DeleteTextures(1, &self.font);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Check legacy rendering and GL state restoration with a Mesa EGL context.
    #[test]
    #[ignore = "requires Mesa EGL; run with LIBGL_ALWAYS_SOFTWARE=1 MESA_GL_VERSION_OVERRIDE=2.1"]
    #[cfg(target_os = "linux")]
    fn legacy_context_renders_large_draw_lists_and_restores_host_state() {
        unsafe {
            let egl = libloading::Library::new("libEGL.so.1").unwrap();
            macro_rules! egl_fn {
                ($name:literal, $signature:ty) => {
                    *egl.get::<$signature>(concat!($name, "\0").as_bytes())
                        .unwrap()
                };
            }
            let display = egl_fn!(
                "eglGetPlatformDisplay",
                unsafe extern "C" fn(u32, *mut c_void, *const isize) -> *mut c_void
            )(0x31DD, std::ptr::null_mut(), std::ptr::null());
            assert!(!display.is_null());
            let initialize = egl_fn!(
                "eglInitialize",
                unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32
            );
            assert_ne!(
                initialize(display, std::ptr::null_mut(), std::ptr::null_mut()),
                0
            );
            assert_ne!(
                egl_fn!("eglBindAPI", unsafe extern "C" fn(u32) -> u32)(0x30A2),
                0
            );
            let mut config = std::ptr::null_mut();
            let mut count = 0;
            let attrs = [
                0x3033, 1, 0x3040, 8, 0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 8, 0x3038,
            ];
            assert_ne!(
                egl_fn!(
                    "eglChooseConfig",
                    unsafe extern "C" fn(
                        *mut c_void,
                        *const i32,
                        *mut *mut c_void,
                        i32,
                        *mut i32,
                    ) -> u32
                )(display, attrs.as_ptr(), &mut config, 1, &mut count),
                0
            );
            assert_eq!(count, 1);
            let surface_attrs = [0x3057, 128, 0x3056, 128, 0x3038];
            let surface = egl_fn!(
                "eglCreatePbufferSurface",
                unsafe extern "C" fn(*mut c_void, *mut c_void, *const i32) -> *mut c_void
            )(display, config, surface_attrs.as_ptr());
            let context_attrs = [0x3098, 2, 0x3038];
            let context = egl_fn!(
                "eglCreateContext",
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                    *const i32,
                ) -> *mut c_void
            )(
                display,
                config,
                std::ptr::null_mut(),
                context_attrs.as_ptr(),
            );
            assert!(!surface.is_null() && !context.is_null());
            let make_current = egl_fn!(
                "eglMakeCurrent",
                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32
            );
            assert_ne!(make_current(display, surface, surface, context), 0);
            let get_proc = egl_fn!(
                "eglGetProcAddress",
                unsafe extern "C" fn(*const std::ffi::c_char) -> *const c_void
            );
            let mut imgui = imgui::Context::create();
            imgui.set_ini_filename(None);
            imgui.io_mut().display_size = [128.0, 128.0];
            imgui.io_mut().delta_time = 1.0 / 60.0;
            let renderer = Renderer::new(
                |name| get_proc(std::ffi::CString::new(name).unwrap().as_ptr()),
                &mut imgui,
            )
            .unwrap();
            let gl = &renderer.gl;
            let version =
                std::ffi::CStr::from_ptr(gl.GetString(gl::VERSION).cast()).to_string_lossy();
            assert!(version.starts_with("2.1"), "{version}");
            let ui = imgui.frame();
            let draw = ui.get_background_draw_list();
            for _ in 0..20_000 {
                draw.add_rect([2.0, 2.0], [4.0, 4.0], [0.0, 1.0, 0.0, 1.0])
                    .filled(true)
                    .build();
            }
            draw.add_rect([40.0, 40.0], [80.0, 80.0], [1.0, 0.0, 0.0, 1.0])
                .filled(true)
                .build();
            drop(draw);
            let data = imgui.render();
            assert!(data.total_vtx_count > 65_536);
            assert!(data.draw_lists().flat_map(|list| list.commands()).any(|command| matches!(command, DrawCmd::Elements { cmd_params, .. } if cmd_params.vtx_offset > 0)));
            gl.Viewport(3, 4, 100, 101);
            gl.MatrixMode(gl::PROJECTION);
            gl.LoadIdentity();
            gl.Translatef(0.25, 0.5, 0.0);
            gl.ActiveTexture(gl::TEXTURE1);
            gl.ClientActiveTexture(gl::TEXTURE1);
            gl.PixelStorei(gl::UNPACK_ROW_LENGTH, 7);
            gl.Enable(gl::DEPTH_TEST);
            let mut buffers = [0; 2];
            gl.GenBuffers(2, buffers.as_mut_ptr());
            gl.BindBuffer(gl::ARRAY_BUFFER, buffers[0]);
            gl.BindBuffer(gl::ELEMENT_ARRAY_BUFFER, buffers[1]);
            renderer.render(data);
            let query = |parameter| {
                let mut result = 0;
                gl.GetIntegerv(parameter, &mut result);
                result
            };
            assert_eq!(query(gl::ACTIVE_TEXTURE), gl::TEXTURE1 as i32);
            assert_eq!(query(gl::CLIENT_ACTIVE_TEXTURE), gl::TEXTURE1 as i32);
            assert_eq!(query(gl::UNPACK_ROW_LENGTH), 7);
            assert_eq!(query(gl::MATRIX_MODE), gl::PROJECTION as i32);
            assert_eq!(query(gl::ARRAY_BUFFER_BINDING), buffers[0] as i32);
            assert_eq!(query(gl::ELEMENT_ARRAY_BUFFER_BINDING), buffers[1] as i32);
            assert_ne!(gl.IsEnabled(gl::DEPTH_TEST), 0);
            let mut viewport = [0; 4];
            gl.GetIntegerv(gl::VIEWPORT, viewport.as_mut_ptr());
            assert_eq!(viewport, [3, 4, 100, 101]);
            let mut projection = [0.0; 16];
            gl.GetFloatv(gl::PROJECTION_MATRIX, projection.as_mut_ptr());
            assert_eq!([projection[12], projection[13]], [0.25, 0.5]);
            let mut pixel = [0_u8; 4];
            gl.ReadPixels(
                60,
                68,
                1,
                1,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                pixel.as_mut_ptr().cast(),
            );
            assert!(
                pixel[0] > 240 && pixel[1] < 10 && pixel[2] < 10,
                "{pixel:?}"
            );
            assert_eq!(gl.GetError(), gl::NO_ERROR);
            gl.DeleteBuffers(2, buffers.as_ptr());
            drop(renderer);
            make_current(
                display,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            egl_fn!(
                "eglDestroyContext",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32
            )(display, context);
            egl_fn!(
                "eglDestroySurface",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32
            )(display, surface);
            egl_fn!("eglTerminate", unsafe extern "C" fn(*mut c_void) -> u32)(display);
        }
    }
}
