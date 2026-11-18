//! Standalone UI preview using an engine URL supplied on the command line.
//! The default engine address is http://127.0.0.1:8087.

use std::num::NonZeroU32;
#[cfg(debug_assertions)]
mod review;

use glow::HasContext as _;
use glutin::{
    config::ConfigTemplateBuilder,
    context::{ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext},
    display::{GetGlDisplay, GlDisplay},
    surface::{GlSurface, Surface, SurfaceAttributesBuilder, WindowSurface},
};
use imgui_winit_support::{
    WinitPlatform,
    winit::{
        dpi::{LogicalSize, PhysicalSize},
        event_loop::EventLoop,
        window::{Window, WindowAttributes},
    },
};
use winit::raw_window_handle::HasWindowHandle as _;

use openatc_ui::interface::{Interface, WindowFrame};

fn main() {
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://127.0.0.1:8087".to_owned());
    let (event_loop, window, surface, context) = create_window();
    let (mut winit_platform, mut imgui_context) = imgui_init(&window);
    let gl = glow_context(&context);
    let mut renderer =
        imgui_glow_renderer::AutoRenderer::new(gl, &mut imgui_context).expect("renderer");
    let mut interface = Interface::new(&endpoint);
    #[cfg(debug_assertions)]
    let mut capture = review::Capture::from_env();
    if let Some(page) = std::env::args()
        .nth(2)
        .and_then(|value| value.parse::<usize>().ok())
    {
        interface.set_page(page);
    }
    if std::env::args().nth(3).as_deref() == Some("3d") {
        interface.orbit_view = true;
    }
    // Explicit debug-only scenery fixture for reproducible visual review.
    #[cfg(debug_assertions)]
    if let Ok(path) = std::env::var("OPENATC_TERRAIN_PREVIEW")
        && let Ok(text) = std::fs::read_to_string(path)
        && let Ok(mut profile) =
            serde_json::from_str::<openatc_core::arrival::TerrainProfile>(&text)
    {
        profile.source = "PREVIEW terrain fixture".into();
        interface.arrival_terrain = profile;
    }
    let mut last_frame = std::time::Instant::now();
    let mut applied_ui_scale = 1.0f32;

    #[allow(deprecated)]
    event_loop
        .run(move |event, window_target| match event {
            winit::event::Event::NewEvents(_) => {
                let now = std::time::Instant::now();
                imgui_context
                    .io_mut()
                    .update_delta_time(now.duration_since(last_frame));
                last_frame = now;
            }
            winit::event::Event::AboutToWait => {
                winit_platform
                    .prepare_frame(imgui_context.io_mut(), &window)
                    .unwrap();
                window.request_redraw();
            }
            winit::event::Event::WindowEvent {
                event: winit::event::WindowEvent::RedrawRequested,
                ..
            } => {
                interface.tick();
                let physical = window.inner_size();
                let scale = winit_platform.hidpi_factor();
                imgui_context.io_mut().display_size = [
                    (f64::from(physical.width) / scale) as f32,
                    (f64::from(physical.height) / scale) as f32,
                ];
                #[cfg(debug_assertions)]
                if let Some(review) = &capture {
                    if let Some(section) = review.section {
                        interface.show_settings_section(section);
                    }
                    interface.settings.pin_open = true;
                    if let Some(scale) = std::env::var("OPENATC_PREVIEW_SCALE")
                        .ok()
                        .and_then(|v| v.parse::<f32>().ok())
                    {
                        interface.settings.ui_scale = scale.clamp(0.85, 1.5);
                    }
                    interface.settings.light_theme =
                        std::env::var("OPENATC_PREVIEW_LIGHT").ok().as_deref() == Some("1");
                    if let Some(opacity) = std::env::var("OPENATC_PREVIEW_OPACITY")
                        .ok()
                        .and_then(|v| v.parse::<f32>().ok())
                    {
                        interface.settings.window_opacity = opacity.clamp(0.2, 1.0);
                    }
                }
                let ui_scale = interface.settings.ui_scale;
                if (ui_scale - applied_ui_scale).abs() > 0.001 {
                    openatc_ui::widgets::configure_scale(&mut imgui_context, ui_scale);
                    applied_ui_scale = ui_scale;
                }
                let frame = WindowFrame {
                    native_bounds: None,
                    position: [0.0, 0.0],
                    size: imgui_context.io().display_size,
                    can_pop_out: cfg!(debug_assertions)
                        && std::env::var_os("OPENATC_PREVIEW_NATIVE_HEADER").is_some(),
                    popped_out: false,
                };
                unsafe { renderer.gl_context().clear(glow::COLOR_BUFFER_BIT) };
                interface.set_window_frame(frame);
                #[cfg(debug_assertions)]
                if let Some(review) = &mut capture {
                    review.drive(imgui_context.io_mut());
                }
                let ui = imgui_context.frame();
                let actions = interface.draw(ui);
                winit_platform.prepare_render(ui, &window);
                let draw_data = imgui_context.render();
                renderer.render(draw_data).expect("render");
                #[cfg(debug_assertions)]
                if let Some(review) = &mut capture {
                    let physical = window.inner_size();
                    if review.save(
                        renderer.gl_context(),
                        physical.width as i32,
                        physical.height as i32,
                    ) {
                        window_target.exit();
                    }
                }
                surface.swap_buffers(&context).expect("swap");
                if actions.close {
                    window_target.exit();
                }
                let scale = winit_platform.hidpi_factor();
                window.set_min_inner_size(Some(PhysicalSize::new(
                    f64::from(actions.minimum_size[0]) * scale,
                    f64::from(actions.minimum_size[1]) * scale,
                )));
                if actions.resize_delta != [0.0, 0.0] || actions.height.is_some() {
                    let current = window.inner_size().to_logical::<f64>(scale);
                    let width = (current.width + f64::from(actions.resize_delta[0]))
                        .max(f64::from(actions.minimum_size[0]));
                    let height = actions.height.map_or_else(
                        || {
                            (current.height + f64::from(actions.resize_delta[1]))
                                .max(f64::from(actions.minimum_size[1]))
                        },
                        f64::from,
                    );
                    // Some hosts resize immediately without a later resize event.
                    if let Some(size) =
                        window.request_inner_size(PhysicalSize::new(width * scale, height * scale))
                        && let (Some(width), Some(height)) =
                            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                    {
                        surface.resize(&context, width, height);
                    }
                }
            }
            winit::event::Event::WindowEvent {
                event: winit::event::WindowEvent::CloseRequested,
                ..
            } => {
                window_target.exit();
            }
            winit::event::Event::WindowEvent {
                event: winit::event::WindowEvent::Resized(new_size),
                ..
            } => {
                if new_size.width > 0 && new_size.height > 0 {
                    surface.resize(
                        &context,
                        NonZeroU32::new(new_size.width).unwrap(),
                        NonZeroU32::new(new_size.height).unwrap(),
                    );
                }
                winit_platform.handle_event(imgui_context.io_mut(), &window, &event);
            }
            event => {
                winit_platform.handle_event(imgui_context.io_mut(), &window, &event);
            }
        })
        .expect("EventLoop error");
}

fn create_window() -> (
    EventLoop<()>,
    Window,
    Surface<WindowSurface>,
    PossiblyCurrentContext,
) {
    let event_loop = EventLoop::new().unwrap();
    let preview_size = (960, 760);
    #[cfg(debug_assertions)]
    let mut preview_size = preview_size;
    #[cfg(debug_assertions)]
    if let Ok(size) = std::env::var("OPENATC_PREVIEW_SIZE")
        && let Some((w, h)) = size
            .split_once('x')
            .and_then(|(w, h)| w.parse::<u32>().ok().zip(h.parse::<u32>().ok()))
    {
        preview_size = (w.clamp(520, 1920), h.clamp(460, 1080));
    }
    let window_attributes = WindowAttributes::default()
        .with_title(
            std::env::args()
                .nth(2)
                .and_then(|v| v.parse::<usize>().ok())
                .and_then(|p| openatc_ui::interface::PAGES.get(p))
                .map_or_else(|| "OpenATC AI".to_owned(), |p| format!("OpenATC AI — {p}")),
        )
        .with_inner_size(LogicalSize::new(preview_size.0, preview_size.1));
    let (window, cfg) = glutin_winit::DisplayBuilder::new()
        .with_window_attributes(Some(window_attributes))
        .build(&event_loop, ConfigTemplateBuilder::new(), |mut configs| {
            configs.next().unwrap()
        })
        .expect("window");
    let window = window.unwrap();
    let context_attribs =
        ContextAttributesBuilder::new().build(Some(window.window_handle().unwrap().as_raw()));
    let context = unsafe {
        cfg.display()
            .create_context(&cfg, &context_attribs)
            .expect("context")
    };
    let actual_size = window.inner_size();
    let surface_attribs = SurfaceAttributesBuilder::<WindowSurface>::new()
        .with_srgb(Some(true))
        .build(
            window.window_handle().unwrap().as_raw(),
            NonZeroU32::new(actual_size.width.max(1)).unwrap(),
            NonZeroU32::new(actual_size.height.max(1)).unwrap(),
        );
    let surface = unsafe {
        cfg.display()
            .create_window_surface(&cfg, &surface_attribs)
            .expect("surface")
    };
    let context = context.make_current(&surface).expect("make current");
    (event_loop, window, surface, context)
}

fn glow_context(context: &PossiblyCurrentContext) -> glow::Context {
    unsafe {
        glow::Context::from_loader_function_cstr(|s| context.display().get_proc_address(s).cast())
    }
}

fn imgui_init(window: &Window) -> (WinitPlatform, imgui::Context) {
    let mut imgui_context = imgui::Context::create();
    imgui_context.set_clipboard_backend(openatc_ui::clipboard::Backend::default());
    imgui_context.set_ini_filename(None);
    let mut winit_platform = WinitPlatform::new(&mut imgui_context);
    winit_platform.attach_window(
        imgui_context.io_mut(),
        window,
        imgui_winit_support::HiDpiMode::Rounded,
    );
    openatc_ui::widgets::configure_fonts(&mut imgui_context);
    (winit_platform, imgui_context)
}
