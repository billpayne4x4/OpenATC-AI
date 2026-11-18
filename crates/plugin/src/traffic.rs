//! Optional native AI count control. X-Plane continues to fly the aircraft.
use xplane_sys as xp;

/// Decide whether `OpenATC` should own X-Plane's native AI and how many to request.
///
/// Generated traffic always forces native AI to zero. Otherwise a configured native count is
/// used only when the user manages native traffic explicitly.
pub(crate) fn native_plan(
    suppress_native: bool,
    generated_traffic: bool,
    manage_native: bool,
    configured_count: u8,
) -> (bool, u8) {
    let suppress = suppress_native || generated_traffic;
    (
        manage_native || suppress,
        if suppress { 0 } else { configured_count },
    )
}

#[derive(Default)]
pub struct NativeTraffic {
    owned: bool,
    attempted: bool,
    original: i32,
    applied: Option<i32>,
    status: String,
}
impl NativeTraffic {
    pub fn update(&mut self, enabled: bool, count: u8) -> &str {
        if !enabled {
            self.release(true);
            self.attempted = false;
            self.status.clear();
            return &self.status;
        }
        if self.applied == Some(i32::from(count)) {
            return &self.status;
        }
        if !self.owned && self.attempted {
            return &self.status;
        }
        let (mut total, mut active, mut controller) = (0, 0, 0);
        unsafe {
            xp::XPLMCountAircraft(&raw mut total, &raw mut active, &raw mut controller);
        }
        if !self.owned {
            self.attempted = true;
            if controller != xp::XPLM_NO_PLUGIN_ID && controller != unsafe { xp::XPLMGetMyID() } {
                self.status =
                    "Another traffic provider owns the aircraft. Its traffic remains in control."
                        .into();
                return &self.status;
            }
            self.original = active;
            // No callback: releasing another provider must never cause us to take over later.
            self.owned = unsafe {
                xp::XPLMAcquirePlanes(std::ptr::null_mut(), None, std::ptr::null_mut()) != 0
            };
            if !self.owned {
                self.status =
                    "Another traffic provider owns the aircraft. Its traffic remains in control."
                        .into();
                return &self.status;
            }
        }
        let requested = (i32::from(count.min(19)) + 1).min(total).max(1);
        unsafe {
            xp::XPLMSetActiveAircraftCount(requested);
            xp::XPLMCountAircraft(&raw mut total, &raw mut active, &raw mut controller);
        }
        self.applied = Some(i32::from(count));
        self.status = if count == 0 {
            "X-Plane AI traffic suppressed. OpenATC generated traffic is separate.".into()
        } else {
            format!(
                "X-Plane AI: {} aircraft active. X-Plane supplies their flight models and routes.",
                (active - 1).max(0)
            )
        };
        &self.status
    }
    pub fn yield_to_provider(&mut self) {
        if self.owned {
            self.release(false);
            self.attempted = true;
            self.status="Traffic control was handed to another plugin. Turn native traffic off and on to request it again.".into();
        }
    }
    fn release(&mut self, restore: bool) {
        if !self.owned {
            return;
        }
        unsafe {
            if restore {
                xp::XPLMSetActiveAircraftCount(self.original.max(1));
            }
            xp::XPLMReleasePlanes();
        }
        self.owned = false;
        self.applied = None;
    }
}
impl Drop for NativeTraffic {
    fn drop(&mut self) {
        self.release(true);
    }
}

#[cfg(test)]
mod tests {
    use super::native_plan;

    #[test]
    fn generated_traffic_forces_native_ai_off() {
        assert_eq!(native_plan(false, true, false, 19), (true, 0));
        assert_eq!(native_plan(false, true, true, 19), (true, 0));
    }

    #[test]
    fn suppression_forces_native_ai_off() {
        assert_eq!(native_plan(true, false, false, 19), (true, 0));
        assert_eq!(native_plan(true, true, true, 19), (true, 0));
    }

    #[test]
    fn explicit_native_management_keeps_the_configured_count() {
        assert_eq!(native_plan(false, false, true, 6), (true, 6));
    }

    #[test]
    fn native_ai_stays_off_when_unmanaged_and_not_suppressed() {
        assert_eq!(native_plan(false, false, false, 19), (false, 19));
    }
}
