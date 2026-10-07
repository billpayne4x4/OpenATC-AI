//! Incremental, cached terrain sampling on the simulator main thread.
#![allow(clippy::cast_possible_truncation)]
use openatc_core::arrival::{
    Target, TerrainProfile, TerrainStation, bearing, destination, distance_nm,
};
use openatc_core::state::Telemetry;
use std::{
    ptr,
    time::{Duration, Instant},
};
use xplane_sys as xp;

pub struct TerrainSampler {
    probe: xp::XPLMProbeRef,
    jobs: Vec<(f64, [f64; 2])>,
    at: usize,
    key: String,
    origin: [f64; 2],
    started: Instant,
    working: TerrainProfile,
}
impl TerrainSampler {
    pub fn new() -> Self {
        Self {
            probe: ptr::null_mut(),
            jobs: Vec::new(),
            at: 0,
            key: String::new(),
            origin: [0.0; 2],
            started: Instant::now(),
            working: TerrainProfile::default(),
        }
    }
    pub fn update(
        &mut self,
        active: bool,
        airport: &str,
        target: &Target,
        live: &Telemetry,
        output: &mut TerrainProfile,
    ) {
        if !active || !live.position_valid {
            return;
        }
        let own = [live.latitude, live.longitude];
        let key = format!("{airport}/{}", target.runway);
        let target_pos = [target.latitude, target.longitude];
        let changed = self.key != key;
        let complete = self.at >= self.jobs.len();
        if changed
            || (complete
                && (self.started.elapsed() > Duration::from_secs(20)
                    || distance_nm(self.origin, own) > 0.5))
        {
            if changed {
                *output = TerrainProfile::default();
            }
            self.key = key.clone();
            self.origin = own;
            self.started = Instant::now();
            self.at = 0;
            self.jobs = stations(own, target_pos);
            self.working = TerrainProfile {
                key: key.clone(),
                stations: Vec::new(),
                sampling: true,
                source: "Loaded X-Plane scenery".into(),
            };
            output.sampling = true;
        }
        if self.probe.is_null() {
            self.probe = unsafe { xp::XPLMCreateProbe(xp::XPLMProbeType::Y) };
        }
        if self.probe.is_null() {
            return;
        }
        let course = bearing(own, target_pos);
        // At most 25 probe calls per half-second flight tick; no SDK calls in a worker.
        for _ in 0..5 {
            let Some((remaining, centre)) = self.jobs.get(self.at).copied() else {
                break;
            };
            let mut heights = Vec::new();
            let mut ground = None;
            for offset in [0.0, -0.5, 0.5, -1.0, 1.0] {
                let p = destination(centre, course + 90.0, offset);
                let hit = self.height(p, own);
                if offset == 0.0 {
                    ground = hit;
                }
                heights.push(hit);
            }
            let corridor = heights
                .iter()
                .copied()
                .collect::<Option<Vec<_>>>()
                .and_then(|v| v.into_iter().reduce(f64::max));
            self.working.stations.push(TerrainStation {
                remaining_nm: remaining,
                ground_feet: ground,
                corridor_feet: corridor,
            });
            self.at += 1;
        }
        self.working.sampling = self.at < self.jobs.len();
        // Publish partial data immediately. Unknown/unprobed stations are gaps.
        *output = self.working.clone();
    }
    fn height(&self, p: [f64; 2], own: [f64; 2]) -> Option<f64> {
        // Outside loaded scenery the SDK can report a false 0-MSL sphere.
        // A conservative local radius avoids presenting distant ocean-height hits
        // as terrain. The UI explicitly leaves the remainder unknown.
        if distance_nm(p, own) * 1852.0 > 45_000.0 * own[0].to_radians().cos().abs().max(0.15) {
            return None;
        }
        unsafe {
            let (mut x, mut y, mut z) = (0.0, 0.0, 0.0);
            xp::XPLMWorldToLocal(p[0], p[1], 30_000.0, &raw mut x, &raw mut y, &raw mut z);
            let mut info: xp::XPLMProbeInfo_t = std::mem::zeroed();
            info.structSize = i32::try_from(std::mem::size_of::<xp::XPLMProbeInfo_t>()).ok()?;
            let result =
                xp::XPLMProbeTerrainXYZ(self.probe, x as f32, y as f32, z as f32, &raw mut info);
            if result != xp::XPLMProbeResult::HitTerrain || !info.locationY.is_finite() {
                return None;
            }
            let (mut lat, mut lon, mut altitude) = (0.0, 0.0, 0.0);
            xp::XPLMLocalToWorld(
                f64::from(info.locationX),
                f64::from(info.locationY),
                f64::from(info.locationZ),
                &raw mut lat,
                &raw mut lon,
                &raw mut altitude,
            );
            let feet = altitude / 0.3048;
            (feet.is_finite() && (-1500.0..=35_000.0).contains(&feet)).then_some(feet)
        }
    }
}
impl Drop for TerrainSampler {
    fn drop(&mut self) {
        if !self.probe.is_null() {
            unsafe { xp::XPLMDestroyProbe(self.probe) };
        }
    }
}
fn stations(own: [f64; 2], target: [f64; 2]) -> Vec<(f64, [f64; 2])> {
    let distance = distance_nm(own, target);
    let extra = (distance * 0.15).clamp(3.0, 15.0);
    let length = distance + extra;
    let count = (length.ceil() as usize).clamp(16, 160);
    let outward = bearing(target, own);
    let mut remaining: Vec<_> = (0..=count)
        .map(|i| distance - length * (i as f64) / (count as f64))
        .collect();
    remaining.push(0.0);
    remaining.sort_by(|a, b| b.total_cmp(a));
    remaining.dedup_by(|a, b| (*a - *b).abs() < 0.001);
    remaining
        .into_iter()
        .map(|r| (r, destination(target, outward, r)))
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sampling_includes_exact_runway_and_beyond() {
        let target = [-41.5, 147.2];
        let s = stations([-41.1, 147.0], target);
        let at = s.iter().find(|(r, _)| *r == 0.0).unwrap();
        assert!(distance_nm(at.1, target) < 0.00001);
        assert!(s.last().unwrap().0 < 0.0);
        assert!(s.windows(2).all(|w| w[0].0 > w[1].0));
        assert!(s.len() <= 162);
    }
}
