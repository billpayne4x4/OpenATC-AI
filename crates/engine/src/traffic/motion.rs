//! Speed and vertical-speed ramps shared by the ground, takeoff and landing phases.

/// One aircraft's motion state. Targets are approached at the aircraft's own
/// acceleration and braking limits, so a new phase never snaps the model.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MotionController {
    speed: f64,
    vertical_fpm: f64,
}

impl MotionController {
    pub(crate) fn new(speed: f64, vertical_fpm: f64) -> Self {
        Self {
            speed: speed.max(0.0),
            vertical_fpm,
        }
    }

    /// Move the speed toward `target` without exceeding the acceleration or braking rate.
    pub(crate) fn update(&mut self, dt: f64, target: f64, acceleration: f64, braking: f64) -> f64 {
        if dt <= 0.0 || !dt.is_finite() || !target.is_finite() {
            return self.speed;
        }
        let target = target.max(0.0);
        let limit = if target >= self.speed {
            acceleration
        } else {
            braking
        }
        .max(0.02);
        let step = limit * dt;
        self.speed += (target - self.speed).clamp(-step, step);
        if (target - self.speed).abs() < 0.01 {
            self.speed = target;
        }
        self.speed
    }

    /// Move the vertical speed toward a target at a fixed rate in feet per minute per second.
    pub(crate) fn update_vertical(&mut self, dt: f64, target: f64, rate: f64) -> f64 {
        if dt <= 0.0 || !dt.is_finite() || !target.is_finite() {
            return self.vertical_fpm;
        }
        let step = rate.max(1.0) * dt;
        self.vertical_fpm += (target - self.vertical_fpm).clamp(-step, step);
        if (target - self.vertical_fpm).abs() < 1.0 {
            self.vertical_fpm = target;
        }
        self.vertical_fpm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_changes_are_limited_by_the_aircraft_rate() {
        let mut motion = MotionController::new(0.0, 0.0);
        let acceleration = 1.5;
        let dt = 0.1;
        let mut previous = 0.0;
        for _ in 0..80 {
            let speed = motion.update(dt, 11.3, acceleration, 2.0);
            assert!(speed >= previous);
            assert!(speed - previous <= acceleration * dt + 1e-9);
            previous = speed;
        }
        assert!((motion.speed - 11.3).abs() < 0.01);

        previous = motion.speed;
        for _ in 0..80 {
            let speed = motion.update(dt, 0.0, acceleration, 2.0);
            assert!(speed <= previous);
            assert!(previous - speed <= 2.0 * dt + 1e-9);
            previous = speed;
        }
        assert!(motion.speed < 0.01);
    }

    #[test]
    fn vertical_speed_ramps_before_the_climb_rate_is_reached() {
        let mut motion = MotionController::new(0.0, 0.0);
        let dt = 0.1;
        let first = motion.update_vertical(dt, 2_200.0, 1_200.0);
        assert!(first > 0.0 && first < 2_200.0);
        for _ in 0..40 {
            motion.update_vertical(dt, 2_200.0, 1_200.0);
        }
        assert!((motion.vertical_fpm - 2_200.0).abs() < 1.0);
    }
}
