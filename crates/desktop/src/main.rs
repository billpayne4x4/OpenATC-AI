//! Standalone UI preview using an engine URL supplied on the command line.
//! The default engine address is http://127.0.0.1:8087.

use std::num::NonZeroU32;

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
        dpi::LogicalSize,
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
    if let Ok(path) = std::env::var("OPENATC_TERRAIN_PREVIEW") {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(mut profile) =
                serde_json::from_str::<openatc_core::arrival::TerrainProfile>(&text)
            {
                profile.source = "PREVIEW terrain fixture".into();
                interface.arrival_terrain = profile;
            }
        }
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
                let ui_scale = interface.settings.ui_scale;
                if (ui_scale - applied_ui_scale).abs() > 0.001 {
                    openatc_ui::widgets::configure_scale(&mut imgui_context, ui_scale);
                    applied_ui_scale = ui_scale;
                }
                interface.set_window_frame(WindowFrame {
                    position: [0.0, 0.0],
                    size: imgui_context.io().display_size,
                    can_pop_out: false,
                    popped_out: false,
                });
                unsafe { renderer.gl_context().clear(glow::COLOR_BUFFER_BIT) };
                let ui = imgui_context.frame();
                let actions = interface.draw(ui);
                winit_platform.prepare_render(ui, &window);
                let draw_data = imgui_context.render();
                renderer.render(draw_data).expect("render");
                surface.swap_buffers(&context).expect("swap");
                if actions.close {
                    window_target.exit();
                }
                if actions.resize_delta != [0.0, 0.0] {
                    let current_size = window.inner_size().to_logical::<f64>(window.scale_factor());
                    let _ = window.request_inner_size(LogicalSize::new(
                        (current_size.width + f64::from(actions.resize_delta[0]))
                            .max(f64::from(actions.minimum_size[0])),
                        (current_size.height + f64::from(actions.resize_delta[1]))
                            .max(f64::from(actions.minimum_size[1])),
                    ));
                }
                if let Some(height) = actions.height {
                    let current_size = window.inner_size().to_logical::<f64>(window.scale_factor());
                    let _ = window.request_inner_size(LogicalSize::new(
                        current_size.width,
                        f64::from(height),
                    ));
                }
                window.set_min_inner_size(Some(LogicalSize::new(
                    actions.minimum_size[0],
                    actions.minimum_size[1],
                )));
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
    let window_attributes = WindowAttributes::default()
        .with_title(
            std::env::args()
                .nth(2)
                .and_then(|v| v.parse::<usize>().ok())
                .and_then(|p| openatc_ui::interface::PAGES.get(p))
                .map_or_else(|| "OpenATC AI".to_owned(), |p| format!("OpenATC AI — {p}")),
        )
        .with_inner_size(LogicalSize::new(960, 760));
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
