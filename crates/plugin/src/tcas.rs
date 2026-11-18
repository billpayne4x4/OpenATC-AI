//! Publish generated targets only while `OpenATC` owns the SDK traffic slots.
use openatc_core::state::GeneratedTraffic;
use std::{
    ffi::CStr,
    sync::atomic::{AtomicBool, Ordering},
};
#[cfg(not(test))]
use xplane_sys as xp;
#[cfg(test)]
#[path = "tcas_test_sdk.rs"]
mod xp;

// XPLM can call this synchronously while another provider releases its planes.
// Defer acquisition to the plugin's next flight loop instead of re-entering plugin state.
static PLANES_AVAILABLE: AtomicBool = AtomicBool::new(false);
unsafe extern "C-unwind" fn planes_available(_: *mut std::ffi::c_void) {
    PLANES_AVAILABLE.store(true, Ordering::Relaxed);
}
const _: xplane_sys::XPLMPlanesAvailable_f = Some(planes_available);

pub(crate) fn mode_s_identity(raw: i32) -> Option<u32> {
    let value = raw as u32;
    let id = value & 0x00ff_ffff;
    (id != 0 && value & 0x7f00_0000 == 0).then_some(id)
}

struct References {
    override_tcas: usize,
    ids: usize,
    floats: [usize; 11],
    wheels: usize,
    modes: usize,
    flights: usize,
    types: usize,
    capacity: usize,
}
impl References {
    fn load() -> Option<Self> {
        let find = |name: &CStr| {
            let reference = unsafe { xp::XPLMFindDataRef(name.as_ptr()) };
            (!reference.is_null() && unsafe { xp::XPLMCanWriteDataRef(reference) } != 0)
                .then_some(reference as usize)
        };
        let ids = find(c"sim/cockpit2/tcas/targets/modeS_id")?;
        let capacity =
            unsafe { xp::XPLMGetDatavi(ids as xp::XPLMDataRef, std::ptr::null_mut(), 0, 0) };
        if capacity < 2 {
            return None;
        }
        Some(Self {
            override_tcas: find(c"sim/operation/override/override_TCAS")?,
            ids,
            floats: [
                find(c"sim/cockpit2/tcas/targets/position/x")?,
                find(c"sim/cockpit2/tcas/targets/position/y")?,
                find(c"sim/cockpit2/tcas/targets/position/z")?,
                find(c"sim/cockpit2/tcas/targets/position/vx")?,
                find(c"sim/cockpit2/tcas/targets/position/vy")?,
                find(c"sim/cockpit2/tcas/targets/position/vz")?,
                find(c"sim/cockpit2/tcas/targets/position/psi")?,
                find(c"sim/cockpit2/tcas/targets/position/the")?,
                find(c"sim/cockpit2/tcas/targets/position/phi")?,
                find(c"sim/cockpit2/tcas/targets/position/gear_deploy")?,
                find(c"sim/cockpit2/tcas/targets/position/flap_ratio")?,
            ],
            wheels: find(c"sim/cockpit2/tcas/targets/position/weight_on_wheels")?,
            modes: find(c"sim/cockpit2/tcas/targets/ssr_mode")?,
            flights: find(c"sim/cockpit2/tcas/targets/flight_id")?,
            types: find(c"sim/cockpit2/tcas/targets/icao_type")?,
            capacity: (capacity as usize - 1).min(63),
        })
    }
}
struct Buffer {
    ids: [i32; 63],
    floats: [[f32; 63]; 11],
    wheels: [i32; 63],
    modes: [i32; 63],
    flights: [u8; 504],
    types: [u8; 504],
}
impl Default for Buffer {
    fn default() -> Self {
        Self {
            ids: [0; 63],
            floats: [[0.0; 63]; 11],
            wheels: [0; 63],
            modes: [0; 63],
            flights: [0; 504],
            types: [0; 504],
        }
    }
}
#[derive(Default)]
pub struct Tcas {
    /// Slots actually filled with a target this cycle; slots beyond this must not be
    /// advertised to the simulator, or it receives targets with no identity.
    used: usize,
    owned: bool,
    attempted: bool,
    original_count: i32,
    references: Option<References>,
    buffer: Buffer,
    source: Vec<GeneratedTraffic>,
}
impl Tcas {
    pub fn active(&self) -> bool {
        self.owned
    }
    pub fn receive(&mut self, frames: &[GeneratedTraffic]) {
        if frames.is_empty() {
            self.release();
            self.attempted = false;
            self.source.clear();
            PLANES_AVAILABLE.store(false, Ordering::Relaxed);
            return;
        }
        // Do not take the simulator's traffic slots for targets the SDK cannot address.
        // X-Plane Mode S identities are 24-bit; a scene containing only local or advisory
        // aircraft outside that range used to acquire and release every frame.
        let addressable = frames.iter().any(|frame| {
            let target = &frame.target;
            (1..=0x00ff_ffff).contains(&target.id)
                && (-90.0..=90.0).contains(&target.latitude)
                && (-180.0..=180.0).contains(&target.longitude)
                && (-2000.0..=60000.0).contains(&target.altitude_feet)
                && (0.0..360.0).contains(&target.track_degrees)
        });
        if !addressable {
            self.release();
            self.attempted = false;
            self.source.clear();
            return;
        }
        if self.owned && !Self::still_owned() {
            self.forget();
            self.attempted = false;
        }
        if !self.owned {
            if self.attempted && !PLANES_AVAILABLE.swap(false, Ordering::Relaxed) {
                return;
            }
            self.attempted = true;
            let (mut total, mut active, mut owner) = (0, 0, 0);
            unsafe { xp::XPLMCountAircraft(&raw mut total, &raw mut active, &raw mut owner) };
            let Some(references) = References::load() else {
                return;
            };
            if owner == unsafe { xp::XPLMGetMyID() }
                && unsafe { xp::XPLMGetDatai(references.override_tcas as xp::XPLMDataRef) } != 0
            {
                return;
            }
            if unsafe {
                xp::XPLMAcquirePlanes(
                    std::ptr::null_mut(),
                    Some(planes_available),
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return;
            }
            self.original_count = active;
            self.owned = true;
            unsafe { xp::XPLMSetDatai(references.override_tcas as xp::XPLMDataRef, 1) };
            self.references = Some(references);
            self.source.clear();
        }
        if frames == self.source {
            return;
        }
        self.source = frames.iter().take(63).cloned().collect();
        self.buffer = Buffer::default();
        let mut identities = std::collections::BTreeSet::new();
        let capacity = self.references.as_ref().unwrap().capacity;
        let mut own_id = 0;
        unsafe {
            xp::XPLMGetDatavi(
                self.references.as_ref().unwrap().ids as xp::XPLMDataRef,
                &raw mut own_id,
                0,
                1,
            )
        };
        let mut slot = 0;
        for frame in &self.source {
            let target = &frame.target;
            if slot == capacity {
                break;
            }
            if !(1..=0xffffff).contains(&target.id)
                || Some(target.id) == mode_s_identity(own_id)
                || !identities.insert(target.id)
                || !(-90.0..=90.0).contains(&target.latitude)
                || !(-180.0..=180.0).contains(&target.longitude)
                || !(-2000.0..=60000.0).contains(&target.altitude_feet)
                || !(0.0..360.0).contains(&target.track_degrees)
            {
                continue;
            }
            let (mut x, mut y, mut z) = (0.0, 0.0, 0.0);
            unsafe {
                xp::XPLMWorldToLocal(
                    target.latitude,
                    target.longitude,
                    target.altitude_feet * 0.3048,
                    &raw mut x,
                    &raw mut y,
                    &raw mut z,
                );
            };
            let speed = target
                .ground_speed_knots
                .filter(|speed| (0.0..=600.0).contains(speed))
                .unwrap_or(0.0)
                * 0.514444;
            let angle = target.track_degrees.to_radians();
            let values = [
                x,
                y,
                z,
                speed * angle.sin(),
                frame.vertical_speed_fpm * 0.00508,
                -speed * angle.cos(),
                frame.body_heading_degrees.unwrap_or(target.track_degrees),
                frame.animation.pitch,
                frame.animation.bank,
                frame.animation.gear,
                frame.animation.flaps,
            ];
            if values.iter().any(|value| !value.is_finite()) {
                continue;
            }
            self.buffer.ids[slot] = target.id as i32;
            for (channel, value) in values.into_iter().enumerate() {
                self.buffer.floats[channel][slot] = value as f32;
            }
            self.buffer.wheels[slot] = i32::from(target.on_ground);
            self.buffer.modes[slot] = if !target.on_ground {
                3
            } else if frame.animation.engine <= 0.0 {
                0
            } else {
                5
            };
            for (data, text) in [
                (&mut self.buffer.flights, &target.callsign),
                (&mut self.buffer.types, &target.aircraft),
            ] {
                for (index, byte) in text
                    .bytes()
                    .filter(u8::is_ascii_alphanumeric)
                    .take(7)
                    .enumerate()
                {
                    data[slot * 8 + index] = byte;
                }
            }
            slot += 1;
        }
        self.used = slot;
        if self.used == 0 {
            self.release();
            self.attempted = false;
            return;
        }
        // TCAS override counts traffic targets; slot zero remains the user's aircraft.
        unsafe { xp::XPLMSetActiveAircraftCount(self.used as i32) };
        self.refresh();
    }
    /// Refresh cached targets each frame so the SDK does not expire them between samples.
    pub fn refresh(&mut self) {
        if !self.owned {
            return;
        }
        if !Self::still_owned() {
            self.forget();
            self.attempted = false;
            return;
        }
        let refs = self.references.as_ref().unwrap();
        let capacity = self.used.min(refs.capacity) as i32;
        if capacity == 0 {
            return;
        }
        unsafe {
            xp::XPLMSetDatavi(
                refs.ids as xp::XPLMDataRef,
                self.buffer.ids.as_mut_ptr(),
                1,
                capacity,
            );
            xp::XPLMSetDatavi(
                refs.wheels as xp::XPLMDataRef,
                self.buffer.wheels.as_mut_ptr(),
                1,
                capacity,
            );
            xp::XPLMSetDatavi(
                refs.modes as xp::XPLMDataRef,
                self.buffer.modes.as_mut_ptr(),
                1,
                capacity,
            );
            xp::XPLMSetDatab(
                refs.flights as xp::XPLMDataRef,
                self.buffer.flights.as_mut_ptr().cast(),
                8,
                capacity * 8,
            );
            xp::XPLMSetDatab(
                refs.types as xp::XPLMDataRef,
                self.buffer.types.as_mut_ptr().cast(),
                8,
                capacity * 8,
            );
            for (reference, data) in refs.floats.iter().zip(&mut self.buffer.floats) {
                xp::XPLMSetDatavf(
                    *reference as xp::XPLMDataRef,
                    data.as_mut_ptr(),
                    1,
                    capacity,
                );
            }
        }
    }
    pub fn yield_to_provider(&mut self) {
        self.release();
        self.attempted = false;
    }
    fn still_owned() -> bool {
        let (mut total, mut active, mut owner) = (0, 0, 0);
        unsafe { xp::XPLMCountAircraft(&raw mut total, &raw mut active, &raw mut owner) };
        owner == unsafe { xp::XPLMGetMyID() }
    }
    fn forget(&mut self) {
        self.owned = false;
        self.references = None;
        self.source.clear();
        self.used = 0;
    }
    fn release(&mut self) {
        if !self.owned {
            return;
        }
        let (mut total, mut active, mut owner) = (0, 0, 0);
        unsafe { xp::XPLMCountAircraft(&raw mut total, &raw mut active, &raw mut owner) };
        if owner == unsafe { xp::XPLMGetMyID() } {
            if let Some(refs) = &self.references {
                unsafe { xp::XPLMSetDatai(refs.override_tcas as xp::XPLMDataRef, 0) };
            }
            unsafe {
                xp::XPLMSetActiveAircraftCount(self.original_count.max(1));
                xp::XPLMReleasePlanes();
            }
        }
        self.forget();
    }
}
impl Drop for Tcas {
    fn drop(&mut self) {
        self.release();
    }
}
