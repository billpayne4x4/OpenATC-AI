//! Opt-in render captures for layout checks in debug builds.
use glow::HasContext;
use std::{io::Write, path::PathBuf};

pub struct Capture {
    path: PathBuf,
    frames: usize,
    started: std::time::Instant,
    delay: std::time::Duration,
    clicks: Vec<(f32, [f32; 2])>,
    click_index: usize,
    click_stage: u8,
    pub section: Option<usize>,
}
impl Capture {
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os("OPENATC_PREVIEW_CAPTURE")?;
        Some(Self {
            path: PathBuf::from(path),
            frames: 0,
            started: std::time::Instant::now(),
            clicks: std::env::var("OPENATC_PREVIEW_CLICKS")
                .unwrap_or_default()
                .split(';')
                .filter_map(|click| {
                    let values = click
                        .split(':')
                        .map(str::parse::<f32>)
                        .collect::<Result<Vec<_>, _>>()
                        .ok()?;
                    (values.len() == 3 && values.iter().all(|v| v.is_finite()))
                        .then(|| (values[0], [values[1], values[2]]))
                })
                .collect(),
            click_index: 0,
            click_stage: 0,
            delay: std::time::Duration::from_secs_f32(
                std::env::var("OPENATC_PREVIEW_DELAY")
                    .ok()
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(2.0)
                    .clamp(0.0, 30.0),
            ),
            section: std::env::var("OPENATC_PREVIEW_SECTION")
                .ok()
                .and_then(|s| s.parse().ok()),
        })
    }

    pub fn drive(&mut self, io: &mut imgui::Io) {
        let Some(&(time, position)) = self.clicks.get(self.click_index) else {
            return;
        };
        if self.started.elapsed().as_secs_f32() < time {
            return;
        }
        match self.click_stage {
            0 => io.add_mouse_pos_event(position),
            1 => io.add_mouse_button_event(imgui::MouseButton::Left, true),
            _ => io.add_mouse_button_event(imgui::MouseButton::Left, false),
        }
        self.click_stage += 1;
        if self.click_stage == 3 {
            self.click_stage = 0;
            self.click_index += 1;
        }
    }

    pub fn save(&mut self, gl: &glow::Context, width: i32, height: i32) -> bool {
        self.frames += 1;
        if self.frames < 45 || self.started.elapsed() < self.delay {
            return false;
        }
        let mut pixels = vec![0; (width * height * 4) as usize];
        unsafe {
            gl.read_pixels(
                0,
                0,
                width,
                height,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(&mut pixels),
            );
        }
        let mut file = std::fs::File::create(&self.path).expect("capture file");
        write!(file, "P6\n{width} {height}\n255\n").unwrap();
        for row in pixels.chunks_exact((width * 4) as usize).rev() {
            for pixel in row.as_chunks::<4>().0 {
                file.write_all(&pixel[..3]).unwrap();
            }
        }
        println!("UI capture: {}", self.path.display());
        true
    }
}
