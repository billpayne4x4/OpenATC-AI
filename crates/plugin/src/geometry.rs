#[allow(clippy::cast_precision_loss)]
fn px(pixels: i32) -> f32 {
    pixels as f32
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameLayout {
    pub(crate) display_size: [f32; 2],
    pub(crate) framebuffer_scale: [f32; 2],
    pub(crate) window_position: [f32; 2],
    pub(crate) window_size: [f32; 2],
}

impl FrameLayout {
    pub(crate) fn from_matrices(
        bounds: [i32; 4],
        projection: &[f32; 16],
        modelview: &[f32; 16],
        viewport: [i32; 4],
    ) -> Option<Self> {
        let width = px(bounds[2] - bounds[0]);
        let height = px(bounds[1] - bounds[3]);
        if width <= 0.0 || height <= 0.0 || viewport[2] <= 0 || viewport[3] <= 0 {
            return None;
        }
        let project = |horizontal: f32, vertical: f32| {
            let eye: [f32; 4] = std::array::from_fn(|row| {
                modelview[row] * horizontal + modelview[4 + row] * vertical + modelview[12 + row]
            });
            let clip: [f32; 4] = std::array::from_fn(|row| {
                (0..4)
                    .map(|column| projection[column * 4 + row] * eye[column])
                    .sum()
            });
            if !clip[3].is_finite() || clip[3].abs() < f32::EPSILON {
                return None;
            }
            Some([
                px(viewport[0]) + (clip[0] / clip[3] + 1.0) * px(viewport[2]) * 0.5,
                px(viewport[1]) + (clip[1] / clip[3] + 1.0) * px(viewport[3]) * 0.5,
            ])
        };
        let top_left = project(px(bounds[0]), px(bounds[1]))?;
        let bottom_right = project(px(bounds[2]), px(bounds[3]))?;
        let scale = [
            (bottom_right[0] - top_left[0]) / width,
            (top_left[1] - bottom_right[1]) / height,
        ];
        if scale
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return None;
        }
        let framebuffer = [px(viewport[0] + viewport[2]), px(viewport[1] + viewport[3])];
        if framebuffer.iter().any(|value| *value <= 0.0) {
            return None;
        }
        Some(Self {
            display_size: [framebuffer[0] / scale[0], framebuffer[1] / scale[1]],
            framebuffer_scale: scale,
            window_position: [
                top_left[0] / scale[0],
                (framebuffer[1] - top_left[1]) / scale[1],
            ],
            window_size: [width, height],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::FrameLayout;

    const IDENTITY: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];

    fn orthographic(left: f32, right: f32, bottom: f32, top: f32) -> [f32; 16] {
        [
            2.0 / (right - left),
            0.0,
            0.0,
            0.0,
            0.0,
            2.0 / (top - bottom),
            0.0,
            0.0,
            0.0,
            0.0,
            -1.0,
            0.0,
            -(right + left) / (right - left),
            -(top + bottom) / (top - bottom),
            0.0,
            1.0,
        ]
    }

    fn assert_pair(actual: [f32; 2], expected: [f32; 2]) {
        for index in 0..2 {
            assert!(
                (actual[index] - expected[index]).abs() < 0.01,
                "{actual:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn floating_panel_uses_boxels_at_normal_and_double_density() {
        let projection = orthographic(0.0, 1920.0, 0.0, 1080.0);
        for density in [1, 2] {
            let layout = FrameLayout::from_matrices(
                [60, 940, 1020, 180],
                &projection,
                &IDENTITY,
                [0, 0, 1920 * density, 1080 * density],
            )
            .unwrap();
            assert_pair(layout.display_size, [1920.0, 1080.0]);
            assert_pair(layout.window_position, [60.0, 140.0]);
            assert_pair(layout.window_size, [960.0, 760.0]);
            assert_pair(layout.framebuffer_scale, [density as f32, density as f32]);
        }
    }

    #[test]
    fn negative_monitor_coordinates_map_to_the_current_framebuffer() {
        let projection = orthographic(-1920.0, 0.0, 0.0, 1080.0);
        let layout = FrameLayout::from_matrices(
            [-1800, 940, -840, 180],
            &projection,
            &IDENTITY,
            [0, 0, 1920, 1080],
        )
        .unwrap();
        assert_pair(layout.window_position, [120.0, 140.0]);
        assert_pair(layout.window_size, [960.0, 760.0]);
    }

    #[test]
    fn viewport_offsets_are_preserved_when_the_renderer_sets_its_viewport() {
        let projection = orthographic(0.0, 1920.0, 0.0, 1080.0);
        let layout = FrameLayout::from_matrices(
            [60, 940, 1020, 180],
            &projection,
            &IDENTITY,
            [128, 64, 1920, 1080],
        )
        .unwrap();
        assert_pair(layout.display_size, [2048.0, 1144.0]);
        assert_pair(layout.window_position, [188.0, 140.0]);
    }

    #[test]
    fn popped_out_panel_uses_its_own_framebuffer() {
        let projection = orthographic(60.0, 1020.0, 180.0, 940.0);
        let layout = FrameLayout::from_matrices(
            [60, 940, 1020, 180],
            &projection,
            &IDENTITY,
            [0, 0, 1920, 1520],
        )
        .unwrap();
        assert_pair(layout.display_size, [960.0, 760.0]);
        assert_pair(layout.window_position, [0.0, 0.0]);
        assert_pair(layout.framebuffer_scale, [2.0, 2.0]);
    }

    #[test]
    fn invalid_drawing_transforms_are_rejected() {
        assert!(
            FrameLayout::from_matrices([0, 0, 0, 0], &IDENTITY, &IDENTITY, [0, 0, 1920, 1080])
                .is_none()
        );
        assert!(
            FrameLayout::from_matrices(
                [60, 940, 1020, 180],
                &[0.0; 16],
                &IDENTITY,
                [0, 0, 1920, 1080]
            )
            .is_none()
        );
        assert!(
            FrameLayout::from_matrices([60, 940, 1020, 180], &IDENTITY, &IDENTITY, [0, 0, 0, 0])
                .is_none()
        );
    }
}
