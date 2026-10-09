//! Approved taxi guidance. All SDK calls happen on the simulator main thread.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
use openatc_core::state::{Point, State};
use std::{
    ffi::CString,
    ptr,
    time::{Duration, Instant},
};
use xplane_sys as xp;

pub struct GroundGuidance {
    object: xp::XPLMObjectRef,
    probe: xp::XPLMProbeRef,
    instances: Vec<xp::XPLMInstanceRef>,
    last_update: Instant,
    load_attempted: bool,
    last_sequence: u32,
    holding_marker: bool,
}
impl GroundGuidance {
    pub fn new() -> Self {
        Self {
            object: ptr::null_mut(),
            probe: ptr::null_mut(),
            instances: Vec::new(),
            last_update: Instant::now()
                .checked_sub(Duration::from_secs(2))
                .unwrap_or_else(Instant::now),
            load_attempted: false,
            last_sequence: 0,
            holding_marker: false,
        }
    }
    pub fn new_holding_marker() -> Self {
        let mut marker = Self::new();
        marker.holding_marker = true;
        marker
    }
    pub fn clear(&mut self) {
        for instance in self.instances.drain(..) {
            unsafe {
                xp::XPLMDestroyInstance(instance);
            }
        }
    }
    pub fn update(&mut self, enabled: bool, state: &State) {
        let route = &state.taxi_clearance;
        if !enabled
            || !route.approved
            || (!self.holding_marker && route.guidance_complete)
            || (self.holding_marker
                && (!route.crossing_runway.is_empty()
                    || route.hold_short_runway.is_empty()
                    || !matches!(
                        state.phase,
                        openatc_core::state::PhaseCode::Taxi
                            | openatc_core::state::PhaseCode::TaxiIn
                    )))
            || !state.telemetry.on_ground
            || !state.telemetry.position_valid
            || route.points.len() < 2
        {
            self.clear();
            return;
        }
        if self.last_sequence == route.sequence
            && self.last_update.elapsed() < Duration::from_secs(1)
        {
            return;
        }
        self.last_sequence = route.sequence;
        self.last_update = Instant::now();
        self.clear(); // Never keep objects at an old local origin or on a replaced route.
        if self.object.is_null() && !self.load_attempted {
            self.load_attempted = true;
            if let Some(file) = crate::supervise::plugin_file_path() {
                let path = std::path::Path::new(&file)
                    .parent()
                    .and_then(std::path::Path::parent)
                    .map(|p| {
                        p.join(if self.holding_marker {
                            "assets/taxi-arrow/holding.obj"
                        } else {
                            "assets/taxi-arrow/arrow.obj"
                        })
                    });
                if let Some(path) =
                    path.and_then(|p| CString::new(p.to_string_lossy().as_bytes()).ok())
                {
                    unsafe {
                        self.object = xp::XPLMLoadObject(path.as_ptr());
                    }
                }
            }
            if self.object.is_null() {
                crate::debug_log(if self.holding_marker {
                    "OpenATC: failed to load holding marker asset\n"
                } else {
                    "OpenATC: failed to load taxi arrow asset\n"
                });
            }
        }
        if self.object.is_null() {
            return;
        }
        if self.probe.is_null() {
            unsafe {
                self.probe = xp::XPLMCreateProbe(xp::XPLMProbeType::Y);
            }
        }
        if self.probe.is_null() {
            return;
        }
        let ownship = Point {
            east: (state.telemetry.longitude - route.reference_longitude)
                * 111_320.0
                * route.reference_latitude.to_radians().cos(),
            north: (state.telemetry.latitude - route.reference_latitude) * 111_320.0,
            height: 0.0,
        };
        let positions = if self.holding_marker {
            holding_sample(state, &ownship).into_iter().collect()
        } else {
            samples(&route.points, &ownship)
        };
        for (point, heading) in positions {
            let Some(position) = self.position(state, &point, heading) else {
                continue;
            };
            let mut datarefs = [ptr::null()];
            unsafe {
                let instance = xp::XPLMCreateInstance(self.object, datarefs.as_mut_ptr());
                if !instance.is_null() {
                    xp::XPLMInstanceSetPosition(instance, &raw const position, ptr::null());
                    self.instances.push(instance);
                }
            }
        }
    }
    fn position(&self, state: &State, point: &Point, heading: f64) -> Option<xp::XPLMDrawInfo_t> {
        let route = &state.taxi_clearance;
        let latitude = route.reference_latitude + point.north / 111_320.0;
        let longitude = route.reference_longitude
            + point.east / (111_320.0 * route.reference_latitude.to_radians().cos());
        let (mut x, mut y, mut z) = (0.0, 0.0, 0.0);
        unsafe {
            xp::XPLMWorldToLocal(
                latitude,
                longitude,
                state.telemetry.altitude_feet * 0.3048,
                &raw mut x,
                &raw mut y,
                &raw mut z,
            );
        }
        let (x, y, z) = (x as f32, y as f32, z as f32);
        let hit = self.hit(x, y + 1000.0, z)?;
        terrain_pose_with_footprint(
            x,
            z,
            heading,
            &hit,
            if self.holding_marker {
                (15.0, 15.0)
            } else {
                (1.6, 3.0)
            },
            |px, pz| self.hit(px, y + 1000.0, pz).map(|h| h.locationY),
        )
    }
    fn hit(&self, x: f32, y: f32, z: f32) -> Option<xp::XPLMProbeInfo_t> {
        let mut info: xp::XPLMProbeInfo_t = unsafe { std::mem::zeroed() };
        info.structSize =
            i32::try_from(std::mem::size_of::<xp::XPLMProbeInfo_t>()).expect("SDK struct size");
        let result = unsafe { xp::XPLMProbeTerrainXYZ(self.probe, x, y, z, &raw mut info) };
        (result == xp::XPLMProbeResult::HitTerrain
            && info.is_wet == 0
            && info.locationY.is_finite())
        .then_some(info)
    }
}
impl Drop for GroundGuidance {
    fn drop(&mut self) {
        self.clear();
        unsafe {
            if !self.object.is_null() {
                xp::XPLMUnloadObject(self.object);
            }
            if !self.probe.is_null() {
                xp::XPLMDestroyProbe(self.probe);
            }
        }
    }
}

/// Tilt to the terrain normal and lift clear of the sampled arrow footprint.
#[cfg(test)]
fn terrain_pose(
    x: f32,
    z: f32,
    heading: f64,
    hit: &xp::XPLMProbeInfo_t,
    terrain: impl FnMut(f32, f32) -> Option<f32>,
) -> Option<xp::XPLMDrawInfo_t> {
    terrain_pose_with_footprint(x, z, heading, hit, (1.6, 3.0), terrain)
}

fn terrain_pose_with_footprint(
    x: f32,
    z: f32,
    heading: f64,
    hit: &xp::XPLMProbeInfo_t,
    footprint: (f32, f32),
    mut terrain: impl FnMut(f32, f32) -> Option<f32>,
) -> Option<xp::XPLMDrawInfo_t> {
    if hit.normalY < 0.85
        || ![hit.normalX, hit.normalY, hit.normalZ, hit.locationY]
            .iter()
            .all(|v| v.is_finite())
    {
        return None;
    }
    let angle = heading.to_radians() as f32;
    let forward = [angle.sin(), -angle.cos()];
    let right = [angle.cos(), angle.sin()];
    let slope = |dx: f32, dz: f32| -(hit.normalX * dx + hit.normalZ * dz) / hit.normalY;
    let mut lift = 0.12_f32;
    let mut valid = true;
    // Probe the footprint too: a normal alone cannot detect a crest/curb.
    for (side, ahead) in [
        (-footprint.0, 0.0),
        (footprint.0, 0.0),
        (-footprint.0, footprint.1),
        (footprint.0, footprint.1),
        (0.0, -footprint.1),
    ] {
        let dx = right[0] * side + forward[0] * ahead;
        let dz = right[1] * side + forward[1] * ahead;
        if let Some(height) = terrain(x + dx, z + dz) {
            lift = lift.max(height - hit.locationY - slope(dx, dz) + 0.12);
        } else {
            valid = false;
        }
    }
    if !valid || lift > 0.65 {
        return None;
    }
    Some(xp::XPLMDrawInfo_t {
        structSize: i32::try_from(std::mem::size_of::<xp::XPLMDrawInfo_t>())
            .expect("SDK struct size"),
        x,
        y: hit.locationY + lift,
        z,
        pitch: slope(forward[0], forward[1]).atan().to_degrees(),
        heading: heading as f32,
        roll: -slope(right[0], right[1]).atan().to_degrees(),
    })
}

/// Draw the same route limit used by the controller, oriented across the approach.
fn holding_sample(state: &State, ownship: &Point) -> Option<(Point, f64)> {
    let route = &state.taxi_clearance;
    let end = *route.points.last()?;
    let before = route
        .points
        .iter()
        .rev()
        .skip(1)
        .find(|p| (p.east - end.east).hypot(p.north - end.north) > 0.5)?;
    if (end.east - ownship.east).hypot(end.north - ownship.north) > 1200.0 {
        return None;
    }
    Some((
        route.holding_marker_point.unwrap_or_else(|| {
            openatc_core::airport::holding_marker_position(
                &openatc_core::airport::Airport::default(),
                &route.points,
            )
            .unwrap_or(end)
        }),
        (end.east - before.east)
            .atan2(end.north - before.north)
            .to_degrees()
            .rem_euclid(360.0),
    ))
}

/// Even spacing across corners; discard consumed route, distant sections and the stop limit.
fn samples(points: &[Point], ownship: &Point) -> Vec<(Point, f64)> {
    let mut nearest = (f64::INFINITY, 0.0);
    let mut total = 0.0;
    for pair in points.windows(2) {
        let (dx, dn) = (pair[1].east - pair[0].east, pair[1].north - pair[0].north);
        let length = dx.hypot(dn);
        if length < 0.01 {
            continue;
        }
        let t = (((ownship.east - pair[0].east) * dx + (ownship.north - pair[0].north) * dn)
            / (length * length))
            .clamp(0.0, 1.0);
        let distance =
            (pair[0].east + dx * t - ownship.east).hypot(pair[0].north + dn * t - ownship.north);
        if distance < nearest.0 {
            nearest = (distance, total + t * length);
        }
        total += length;
    }
    if nearest.0 > 100.0 {
        return Vec::new();
    } // No invented ramp connector.
    let mut output = Vec::new();
    let mut along = nearest.1 + 10.0;
    let mut offset = 0.0;
    for pair in points.windows(2) {
        let (dx, dn) = (pair[1].east - pair[0].east, pair[1].north - pair[0].north);
        let length = dx.hypot(dn);
        if length < 0.01 {
            continue;
        }
        while along < offset + length && along < total - 6.0 && output.len() < 64 {
            let t = (along - offset) / length;
            let point = Point {
                east: pair[0].east + dx * t,
                north: pair[0].north + dn * t,
                height: 0.0,
            };
            if (point.east - ownship.east).hypot(point.north - ownship.north) < 1200.0 {
                output.push((point, dx.atan2(dn).to_degrees().rem_euclid(360.0)));
            }
            along += 25.0;
        }
        offset += length;
    }
    output
}
/// Resolve the closest airport from the simulator navigation database.
pub fn current_airport(latitude: f64, longitude: f64) -> String {
    let (mut lat, mut lon) = (latitude as f32, longitude as f32);
    unsafe {
        let nav = xp::XPLMFindNavAid(
            ptr::null(),
            ptr::null(),
            &raw mut lat,
            &raw mut lon,
            ptr::null_mut(),
            xp::XPLMNavType::Airport,
        );
        if nav < 0 {
            return String::new();
        }
        let mut id = [0 as std::ffi::c_char; 64];
        xp::XPLMGetNavAidInfo(
            nav,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            id.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
        );
        std::ffi::CStr::from_ptr(id.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(e: f64, n: f64) -> Point {
        Point {
            east: e,
            north: n,
            height: 0.0,
        }
    }
    fn terrain(normal: [f32; 3]) -> xp::XPLMProbeInfo_t {
        xp::XPLMProbeInfo_t {
            structSize: 44,
            locationX: 0.0,
            locationY: 20.0,
            locationZ: 0.0,
            normalX: normal[0],
            normalY: normal[1],
            normalZ: normal[2],
            velocityX: 0.0,
            velocityY: 0.0,
            velocityZ: 0.0,
            is_wet: 0,
        }
    }
    #[test]
    fn holding_marker_uses_the_route_limit_and_approach_heading() {
        let mut state = State::default();
        state.taxi_clearance.points = vec![p(0.0, 0.0), p(0.0, 100.0)];
        let (point, heading) = holding_sample(&state, &p(0.0, 80.0)).unwrap();
        assert_eq!(point, p(0.0, 81.0));
        assert_eq!(heading, 0.0);
        assert!(holding_sample(&state, &p(2000.0, 0.0)).is_none());
    }
    #[test]
    fn terrain_placement_tilts_and_clears_small_crests() {
        let hit = terrain([0.0, 1.0, 0.1]);
        let position =
            terrain_pose(0.0, 0.0, 0.0, &hit, |_, z| Some(20.0 - z * 0.1 + 0.2)).unwrap();
        assert!((position.y - 20.32).abs() < 0.001);
        assert!((position.pitch - 0.1_f32.atan().to_degrees()).abs() < 0.001);
    }
    #[test]
    fn terrain_misses_and_steps_are_skipped() {
        let hit = terrain([0.0, 1.0, 0.0]);
        assert!(terrain_pose(0.0, 0.0, 0.0, &hit, |_, _| None).is_none());
        assert!(terrain_pose(0.0, 0.0, 0.0, &hit, |_, _| Some(21.0)).is_none());
    }
    #[test]
    fn arrows_follow_turn_and_stop_before_limit() {
        let a = samples(&[p(0.0, 0.0), p(0.0, 50.0), p(100.0, 50.0)], &p(0.0, 20.0));
        assert_eq!(a[0].0, p(0.0, 30.0));
        assert_eq!(a[0].1, 0.0);
        assert_eq!(a[1].1, 90.0);
        assert!(a.last().unwrap().0.east < 94.0);
    }
    #[test]
    fn remote_airport_and_off_route_have_no_arrows() {
        assert!(samples(&[p(0.0, 0.0), p(0.0, 100.0)], &p(200.0, 0.0)).is_empty());
    }
}
