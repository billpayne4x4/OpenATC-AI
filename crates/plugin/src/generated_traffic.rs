//! Lightweight instances. All SDK entry points run on the simulator thread.
use openatc_core::state::GeneratedTraffic;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::{CString, c_void},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use xplane::data::DataType;
use xplane_sys as xp;

unsafe extern "C-unwind" fn unused_value(_cookie: *mut c_void) -> f32 {
    0.0
}

struct Pending {
    alive: Arc<AtomicBool>,
    result: Arc<Mutex<Option<usize>>>,
    path: String,
}
unsafe extern "C-unwind" fn loaded(object: xp::XPLMObjectRef, cookie: *mut c_void) {
    let pending = unsafe { Box::from_raw(cookie.cast::<Pending>()) };
    if object.is_null() {
        crate::debug_log(&format!(
            "OpenATC: failed to load CSL object {}\n",
            pending.path
        ));
    }
    if pending.alive.load(Ordering::Acquire) {
        *pending
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(object as usize);
    } else if !object.is_null() {
        unsafe { xp::XPLMUnloadObject(object) };
    }
}
struct Asset {
    alive: Arc<AtomicBool>,
    result: Arc<Mutex<Option<usize>>>,
}
impl Drop for Asset {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        if let Some(object) = *self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            && object != 0
        {
            unsafe { xp::XPLMUnloadObject(object as xp::XPLMObjectRef) };
        }
    }
}
struct Drawn {
    model: String,
    objects: Vec<std::path::PathBuf>,
    instances: Vec<usize>,
    terrain: Option<Terrain>,
}
struct Terrain {
    x: f64,
    z: f64,
    y: f64,
    normal: [f64; 3],
    coordinate: [f64; 2],
}
/// A missing/stale probe retains the SDK-converted local height, never an MSL value.
fn ground_pose(
    local: [f64; 3],
    ground: Option<&Terrain>,
    heading: f64,
    pitch: f64,
    roll: f64,
) -> (f64, f64, f64) {
    let [x, y, z] = local;
    let Some(ground) = ground.filter(|ground| (ground.x - x).hypot(ground.z - z) <= 25.0) else {
        return (y, pitch, roll);
    };
    let height = ground.y
        - (ground.normal[0] * (x - ground.x) + ground.normal[2] * (z - ground.z))
            / ground.normal[1];
    let radians = heading.to_radians();
    let slope = |x: f64, z: f64| -(ground.normal[0] * x + ground.normal[2] * z) / ground.normal[1];
    (
        height,
        pitch + slope(radians.sin(), -radians.cos()).atan().to_degrees(),
        -slope(radians.cos(), radians.sin()).atan().to_degrees(),
    )
}

/// Interpolate only between received poses; never extrapolate past a cleared stop.
fn blended(previous: &GeneratedTraffic, frame: &mut GeneratedTraffic, amount: f64) {
    let body = frame
        .body_heading_degrees
        .unwrap_or(frame.target.track_degrees);
    let mix = |from: f64, to: f64| from + (to - from) * amount;
    frame.target.latitude = mix(previous.target.latitude, frame.target.latitude);
    let longitude = ((frame.target.longitude - previous.target.longitude + 540.0) % 360.0) - 180.0;
    frame.target.longitude =
        ((previous.target.longitude + longitude * amount + 540.0) % 360.0) - 180.0;
    frame.target.altitude_feet = mix(previous.target.altitude_feet, frame.target.altitude_feet);
    let heading =
        ((frame.target.track_degrees - previous.target.track_degrees + 540.0) % 360.0) - 180.0;
    frame.target.track_degrees = (previous.target.track_degrees + heading * amount + 360.0) % 360.0;
    let old_body = previous
        .body_heading_degrees
        .unwrap_or(previous.target.track_degrees);
    frame.body_heading_degrees = Some(
        (old_body + openatc_core::navigation::turn(old_body, body) * amount).rem_euclid(360.0),
    );
    frame.animation.pitch = mix(previous.animation.pitch, frame.animation.pitch);
    frame.animation.bank = mix(previous.animation.bank, frame.animation.bank);
    frame.animation.gear = mix(previous.animation.gear, frame.animation.gear);
    frame.animation.flaps = mix(previous.animation.flaps, frame.animation.flaps);
}

/// A short playout buffer absorbs the engine/HTTP cadence without advancing aircraft beyond
/// the newest authorised sample. Arrival instants share the renderer's monotonic clock; source
/// simulator time and session generation detect resets and duplicate HTTP replies.
struct TimelineSample {
    arrived: std::time::Instant,
    source: Option<f64>,
    frames: Vec<GeneratedTraffic>,
}

/// A short playout buffer absorbs the engine/HTTP cadence without advancing aircraft beyond
/// the newest authorised sample. Interpolation uses the source simulator clock when it is
/// available, so an irregular HTTP delivery cannot make an aircraft appear to speed up and
/// slow down. Arrival instants are the fallback for older servers.
#[derive(Default)]
struct Timeline {
    samples: VecDeque<TimelineSample>,
    stamp: Option<(u64, f64)>,
    paused: bool,
    rendered: Vec<GeneratedTraffic>,
    /// Monotonic source-time playhead; a late HTTP sample must not jump the traffic back.
    playhead: Option<f64>,
    /// Playout delay derived from the observed source cadence.
    delay: Option<f64>,
}
impl Timeline {
    /// Smallest buffer; fast servers do not need the extra lag.
    const MIN_DELAY_SECONDS: f64 = 0.75;
    /// Largest buffer; protects against a slow scene-preparation interval.
    const MAX_DELAY_SECONDS: f64 = 2.5;

    fn delay(&self) -> f64 {
        self.delay.unwrap_or(Self::MIN_DELAY_SECONDS)
    }

    fn receive(
        &mut self,
        frames: &[GeneratedTraffic],
        stamp: Option<(u64, f64)>,
        paused: bool,
        now: std::time::Instant,
    ) {
        let same = self
            .samples
            .back()
            .is_some_and(|sample| sample.frames == frames);
        if same && stamp == self.stamp && paused == self.paused {
            return;
        }
        let resumed = self.paused && !paused;
        let reset = match (self.stamp, stamp) {
            (Some((epoch, time)), Some((next_epoch, next_time))) => {
                epoch != next_epoch || !(0.0..=30.0).contains(&(next_time - time))
            }
            _ => false,
        };
        if reset || frames.is_empty() {
            self.samples.clear();
            self.rendered.clear();
            self.playhead = None;
        } else if self.paused && !paused {
            self.playhead = None;
            self.samples.clear();
            if !self.rendered.is_empty() {
                self.samples.push_back(TimelineSample {
                    arrived: now
                        .checked_sub(std::time::Duration::from_secs_f64(self.delay()))
                        .unwrap_or(now),
                    source: stamp.map(|(_, time)| time),
                    frames: self.rendered.clone(),
                });
            }
        }
        if let (Some((_, previous)), Some((_, next))) = (self.stamp, stamp)
            && next > previous
            && next - previous <= 30.0
        {
            self.delay = Some(
                ((next - previous) * 1.5).clamp(Self::MIN_DELAY_SECONDS, Self::MAX_DELAY_SECONDS),
            );
        }
        self.paused = paused;
        // Equal source times can carry a clearance/visibility change; replace that sample
        // without shifting its clock. Unchanged positions at a later time are real stops.
        if !resumed && stamp.is_some() && stamp == self.stamp && !self.samples.is_empty() {
            self.samples.back_mut().unwrap().frames = frames.iter().take(63).cloned().collect();
        } else {
            self.samples.push_back(TimelineSample {
                arrived: now,
                source: stamp.map(|(_, time)| time),
                frames: frames.iter().take(63).cloned().collect(),
            });
        }
        self.stamp = stamp;
        while self.samples.len() > 8 {
            self.samples.pop_front();
        }
    }

    fn sample(&mut self, now: std::time::Instant) -> &[GeneratedTraffic] {
        let Some(latest) = self.samples.back() else {
            return &self.rendered;
        };
        if self.paused && !self.rendered.is_empty() {
            self.rendered.retain(|frame| {
                latest
                    .frames
                    .iter()
                    .any(|new| new.target.id == frame.target.id && new.model == frame.model)
            });
            return &self.rendered;
        }
        let (left_index, right_index, amount) = if let (Some(latest_source), Some(first_source)) = (
            latest.source,
            self.samples.front().and_then(|sample| sample.source),
        ) {
            // Advance the source-time playhead with wall time since the newest sample.
            // This keeps motion continuous when HTTP samples arrive early or late, and it
            // still reaches the newest pose after the playout delay.
            let target = latest_source - self.delay()
                + now.saturating_duration_since(latest.arrived).as_secs_f64();
            let target = self
                .playhead
                .map_or(target, |previous| target.max(previous));
            self.playhead = Some(target);
            if target <= first_source {
                (0, 0, 0.0)
            } else if target >= latest_source {
                (self.samples.len() - 1, self.samples.len() - 1, 1.0)
            } else {
                let mut left = 0;
                let mut right = 1;
                for index in 1..self.samples.len() {
                    let Some(source) = self.samples[index].source else {
                        break;
                    };
                    if source >= target {
                        right = index;
                        break;
                    }
                    left = index;
                }
                let start = self.samples[left].source.unwrap_or(first_source);
                let end = self.samples[right].source.unwrap_or(latest_source);
                let span = end - start;
                (
                    left,
                    right,
                    if span > 0.0 {
                        ((target - start) / span).clamp(0.0, 1.0)
                    } else {
                        1.0
                    },
                )
            }
        } else {
            let at = now
                .checked_sub(std::time::Duration::from_secs_f64(self.delay()))
                .unwrap_or(now);
            while self.samples.len() > 2 && self.samples[1].arrived <= at {
                self.samples.pop_front();
            }
            let start = self.samples[0].arrived;
            let end = self.samples.get(1).map_or(start, |sample| sample.arrived);
            let span = end.saturating_duration_since(start).as_secs_f64();
            let amount = if span > 0.0 {
                (at.saturating_duration_since(start).as_secs_f64() / span).min(1.0)
            } else {
                1.0
            };
            (0, self.samples.len().min(2) - 1, amount)
        };
        let left = &self.samples[left_index];
        let right = &self.samples[right_index];
        let latest_frames = &self.samples.back().unwrap().frames;
        self.rendered = latest_frames
            .iter()
            .map(|latest| {
                let Some(to) = right.frames.iter().find(|frame| {
                    frame.target.id == latest.target.id && frame.model == latest.model
                }) else {
                    return latest.clone();
                };
                let Some(from) = left
                    .frames
                    .iter()
                    .find(|frame| frame.target.id == to.target.id && frame.model == to.model)
                else {
                    return to.clone();
                };
                let mut frame = to.clone();
                // A scene relocation is not a trajectory to fly across the airport.
                let displacement = openatc_core::ops::distance_nm(
                    from.target.latitude,
                    from.target.longitude,
                    to.target.latitude,
                    to.target.longitude,
                );
                let max_distance = if from.target.on_ground && to.target.on_ground {
                    0.03
                } else {
                    1.0
                };
                if displacement <= max_distance {
                    blended(from, &mut frame, amount);
                    if amount < 1.0 {
                        frame.target.on_ground = from.target.on_ground;
                    }
                    let span = left
                        .source
                        .zip(right.source)
                        .map_or(self.delay(), |(start, end)| (end - start).max(0.0));
                    frame.animation.propeller_angle = (from.animation.propeller_angle
                        + from.animation.propeller_rpm * 6.0 * span * amount)
                        .rem_euclid(360.0);
                }
                frame
            })
            .collect();
        &self.rendered
    }
}

/// The gate counts simulator callbacks, including when TCAS requires every frame.
#[derive(Default)]
struct DrawClock {
    skipped: u32,
    last: Option<std::time::Instant>,
}
impl DrawClock {
    /// Never let a slow user setting starve the animation below this cadence. The engine
    /// snapshots remain at their own rate; this only controls how often the renderer samples
    /// the interpolated timeline.
    const MIN_RATE_HZ: f32 = 24.0;

    fn ready(&mut self, rate: openatc_settings::TrafficRate, now: std::time::Instant) -> bool {
        self.skipped += 1;
        let floor_due = self
            .last
            .is_some_and(|last| now.duration_since(last).as_secs_f32() >= 1.0 / Self::MIN_RATE_HZ);
        let due = self.last.is_none()
            || floor_due
            || if rate.frames() > 0 {
                self.skipped >= rate.frames()
            } else {
                self.last
                    .is_some_and(|last| now.duration_since(last).as_secs_f32() >= rate.interval())
            };
        if due {
            self.last = Some(now);
            self.skipped = 0;
        }
        due
    }
}

/// Accept only an absolute .obj path that stays inside its package.
fn valid_model_path(path: &std::path::Path) -> bool {
    path.is_absolute()
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("obj"))
        && !path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
}

#[derive(Default)]
pub struct Renderer {
    assets: BTreeMap<std::path::PathBuf, Asset>,
    aircraft: BTreeMap<u32, Drawn>,
    channels: Vec<(CString, openatc_core::traffic::animation::Channel)>,
    owned_refs: Vec<usize>,
    timeline: Timeline,
    probe: usize,
    mappings: BTreeMap<String, openatc_core::traffic::animation::Channel>,
    probe_cursor: u32,
    draw_clock: DrawClock,
}
impl Renderer {
    /// Keep the last two worker snapshots; rendering never scans model folders.
    pub fn receive(
        &mut self,
        frames: &[GeneratedTraffic],
        mappings: &BTreeMap<String, openatc_core::traffic::animation::Channel>,
        paused: bool,
        stamp: Option<(u64, f64)>,
    ) {
        // Channel mappings change more often than the model set. Rebuilding on each
        // mapping change reloaded every model and made traffic flicker.
        if self.mappings != *mappings {
            crate::debug_log(&format!(
                "OpenATC traffic: animation channels changed ({} -> {})",
                self.mappings.len(),
                mappings.len()
            ));
            self.mappings.clone_from(mappings);
            self.channels.clear();
            let ids: Vec<u32> = self.aircraft.keys().copied().collect();
            for id in ids {
                self.retire(id);
            }
        }
        self.timeline
            .receive(frames, stamp, paused, std::time::Instant::now());
    }
    /// Interpolate snapshots without inventing progress beyond a clearance limit.
    pub fn step(&mut self, rate: openatc_settings::TrafficRate) {
        let now = std::time::Instant::now();
        if !self.draw_clock.ready(rate, now) {
            return;
        }
        let frames = self.timeline.sample(now).to_vec();
        if !frames.is_empty() {
            self.update(&frames);
        }
    }
    /// Use a faster lightweight render loop only while generated traffic exists.
    pub fn active(&self) -> bool {
        self.timeline
            .samples
            .back()
            .is_some_and(|sample| !sample.frames.is_empty())
            || !self.aircraft.is_empty()
    }
    pub fn frames(&self) -> &[GeneratedTraffic] {
        &self.timeline.rendered
    }
    /// Begin at most one object load per update; existing instances remain usable.
    pub fn update(&mut self, frames: &[GeneratedTraffic]) {
        if !frames.is_empty() && self.channels.is_empty() {
            {
                for (name, channel) in &self.mappings {
                    let Ok(name) = CString::new(name.as_str()) else {
                        continue;
                    };
                    if unsafe { xp::XPLMFindDataRef(name.as_ptr()) }.is_null() {
                        let reference = unsafe {
                            xp::XPLMRegisterDataAccessor(
                                name.as_ptr(),
                                f32::sim_type(),
                                0,
                                None,
                                None,
                                Some(unused_value),
                                None,
                                None,
                                None,
                                None,
                                None,
                                None,
                                None,
                                None,
                                None,
                                std::ptr::null_mut(),
                                std::ptr::null_mut(),
                            )
                        };
                        if reference.is_null() {
                            continue;
                        }
                        self.owned_refs.push(reference as usize);
                    }
                    self.channels.push((name, *channel));
                }
            }
        }
        let wanted: BTreeSet<_> = frames
            .iter()
            .take(63)
            .map(|frame| frame.target.id)
            .collect();
        let retired: Vec<_> = self
            .aircraft
            .keys()
            .filter(|id| !wanted.contains(id))
            .copied()
            .collect();
        for id in retired {
            self.retire(id);
        }
        let changed: Vec<_> = frames
            .iter()
            .take(63)
            .filter(|frame| {
                self.aircraft.get(&frame.target.id).is_some_and(|drawn| {
                    drawn.model != frame.model.id || drawn.objects != frame.model.objects
                })
            })
            .map(|frame| frame.target.id)
            .collect();
        for id in changed {
            self.retire(id);
        }
        let wanted_objects: BTreeSet<_> = frames
            .iter()
            .take(63)
            .flat_map(|frame| frame.model.objects.iter())
            .collect();
        self.assets.retain(|path, _| wanted_objects.contains(path));
        let mut requested = false;
        let mut candidates: Vec<_> = frames
            .iter()
            .filter(|frame| {
                frame.target.on_ground
                    && self
                        .aircraft
                        .get(&frame.target.id)
                        .and_then(|drawn| drawn.terrain.as_ref())
                        .is_none_or(|ground| {
                            openatc_core::arrival::distance_nm(
                                ground.coordinate,
                                [frame.target.latitude, frame.target.longitude],
                            ) * 1852.0
                                > 5.0
                        })
            })
            .map(|frame| frame.target.id)
            .collect();
        candidates.sort_unstable();
        if let Some(start) = candidates.iter().position(|id| *id > self.probe_cursor) {
            candidates.rotate_left(start);
        }
        let probe_ids: BTreeSet<_> = candidates
            .into_iter()
            .take(4)
            .inspect(|id| self.probe_cursor = *id)
            .collect();
        for frame in frames.iter().take(63) {
            if frame.target.id == 0
                || frame.model.objects.is_empty()
                || frame.model.objects.len() > 8
                || frame
                    .model
                    .objects
                    .iter()
                    .any(|path| !valid_model_path(path))
                || !(-90.0..=90.0).contains(&frame.target.latitude)
                || !(-180.0..=180.0).contains(&frame.target.longitude)
                || !(-2000.0..=60000.0).contains(&frame.target.altitude_feet)
                || !(0.0..360.0).contains(&frame.target.track_degrees)
                || !frame.animation.pitch.is_finite()
                || !frame.animation.bank.is_finite()
            {
                self.retire(frame.target.id);
                continue;
            }
            for path in &frame.model.objects {
                if self.assets.contains_key(path) || requested || self.assets.len() >= 128 {
                    continue;
                }
                let Ok(name) = CString::new(path.to_string_lossy().as_bytes()) else {
                    continue;
                };
                let result = Arc::new(Mutex::new(None));
                let alive = Arc::new(AtomicBool::new(true));
                let cookie = Box::into_raw(Box::new(Pending {
                    alive: alive.clone(),
                    result: result.clone(),
                    path: path.to_string_lossy().into_owned(),
                }));
                self.assets.insert(path.clone(), Asset { alive, result });
                unsafe { xp::XPLMLoadObjectAsync(name.as_ptr(), Some(loaded), cookie.cast()) };
                requested = true;
            }
            let objects: Option<Vec<usize>> = frame
                .model
                .objects
                .iter()
                .map(|path| {
                    self.assets
                        .get(path)
                        .and_then(|asset| {
                            *asset
                                .result
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                        })
                        .filter(|object| *object != 0)
                })
                .collect();
            let Some(objects) = objects else {
                continue;
            };
            if self
                .aircraft
                .get(&frame.target.id)
                .is_some_and(|drawn| drawn.model != frame.model.id)
            {
                self.retire(frame.target.id);
            }
            self.aircraft
                .entry(frame.target.id)
                .or_insert_with(|| Drawn {
                    model: frame.model.id.clone(),
                    objects: frame.model.objects.clone(),
                    instances: Vec::new(),
                    terrain: None,
                });
            let (mut x, mut y, mut z) = (0.0, 0.0, 0.0);
            unsafe {
                xp::XPLMWorldToLocal(
                    frame.target.latitude,
                    frame.target.longitude,
                    frame.target.altitude_feet * 0.3048,
                    &raw mut x,
                    &raw mut y,
                    &raw mut z,
                );
            }
            let drawn = self.aircraft.get_mut(&frame.target.id).unwrap();
            let body_heading = frame
                .body_heading_degrees
                .filter(|heading| (0.0..360.0).contains(heading))
                .unwrap_or(frame.target.track_degrees);
            let (mut pitch, mut roll) = (frame.animation.pitch, frame.animation.bank);
            if frame.target.on_ground {
                let distance = drawn
                    .terrain
                    .as_ref()
                    .map_or(f64::INFINITY, |ground| (ground.x - x).hypot(ground.z - z));
                if distance > 5.0 && probe_ids.contains(&frame.target.id) {
                    if self.probe == 0 {
                        self.probe = unsafe { xp::XPLMCreateProbe(xp::XPLMProbeType::Y) } as usize;
                    }
                    if self.probe != 0 {
                        let mut hit: xp::XPLMProbeInfo_t = unsafe { std::mem::zeroed() };
                        hit.structSize = std::mem::size_of_val(&hit) as i32;
                        let result = unsafe {
                            xp::XPLMProbeTerrainXYZ(
                                self.probe as xp::XPLMProbeRef,
                                x as f32,
                                (y + 1000.0) as f32,
                                z as f32,
                                &raw mut hit,
                            )
                        };
                        if result == xp::XPLMProbeResult::HitTerrain
                            && hit.is_wet == 0
                            && hit.locationY.is_finite()
                            && hit.normalY >= 0.8
                        {
                            drawn.terrain = Some(Terrain {
                                x,
                                z,
                                y: f64::from(hit.locationY),
                                normal: [
                                    f64::from(hit.normalX),
                                    f64::from(hit.normalY),
                                    f64::from(hit.normalZ),
                                ],
                                coordinate: [frame.target.latitude, frame.target.longitude],
                            });
                        } else {
                            drawn.terrain = None;
                        }
                    }
                }
                if drawn
                    .terrain
                    .as_ref()
                    .is_some_and(|ground| (ground.x - x).hypot(ground.z - z) > 25.0)
                {
                    drawn.terrain = None;
                }
                (y, pitch, roll) =
                    ground_pose([x, y, z], drawn.terrain.as_ref(), body_heading, pitch, roll);
            }
            if drawn.instances.is_empty() {
                for object in objects {
                    let mut refs: Vec<_> = self
                        .channels
                        .iter()
                        .map(|(name, _)| name.as_ptr())
                        .collect();
                    refs.push(std::ptr::null());
                    let instance = unsafe {
                        xp::XPLMCreateInstance(object as xp::XPLMObjectRef, refs.as_mut_ptr())
                    };
                    if instance.is_null() {
                        break;
                    }
                    drawn.instances.push(instance as usize);
                }
                if drawn.instances.len() != frame.model.objects.len() {
                    self.retire(frame.target.id);
                    continue;
                }
            }
            y += frame
                .model
                .vertical_offset
                .filter(|offset| offset.is_finite())
                .unwrap_or(0.0);
            let position = xp::XPLMDrawInfo_t {
                structSize: std::mem::size_of::<xp::XPLMDrawInfo_t>() as i32,
                x: x as f32,
                y: y as f32,
                z: z as f32,
                pitch: pitch as f32,
                heading: body_heading as f32,
                roll: roll as f32,
            };
            let values: Vec<_> = self
                .channels
                .iter()
                .map(|(_, channel)| frame.animation.channel(*channel))
                .collect();
            for instance in &self.aircraft[&frame.target.id].instances {
                unsafe {
                    xp::XPLMInstanceSetPosition(
                        *instance as xp::XPLMInstanceRef,
                        &raw const position,
                        values.as_ptr(),
                    );
                };
            }
        }
    }
    fn retire(&mut self, id: u32) {
        if let Some(drawn) = self.aircraft.remove(&id) {
            for instance in drawn.instances {
                unsafe { xp::XPLMDestroyInstance(instance as xp::XPLMInstanceRef) };
            }
        }
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        let ids: Vec<_> = self.aircraft.keys().copied().collect();
        for id in ids {
            self.retire(id);
        }
        self.assets.clear();
        if self.probe != 0 {
            unsafe { xp::XPLMDestroyProbe(self.probe as xp::XPLMProbeRef) };
        }
        for reference in self.owned_refs.drain(..) {
            unsafe { xp::XPLMUnregisterDataAccessor(reference as xp::XPLMDataRef) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openatc_core::state::TrafficTarget;
    use openatc_core::traffic::animation::Animation;
    use openatc_core::traffic::csl::Model;

    fn frame(latitude: f64, track: f64) -> GeneratedTraffic {
        GeneratedTraffic {
            vertical_speed_fpm: 0.0,
            body_heading_degrees: Some(track),
            target: TrafficTarget {
                id: 1,
                callsign: "TEST".into(),
                latitude,
                longitude: 0.0,
                altitude_feet: 1000.0,
                track_degrees: track,
                ..Default::default()
            },
            model: Arc::new(Model {
                id: "test".into(),
                objects: Vec::new(),
                vertical_offset: None,
                matches: Vec::new(),
            }),
            animation: Animation::default(),
        }
    }

    #[test]
    fn renderer_accepts_absolute_object_paths_and_rejects_escapes() {
        let absolute = std::env::temp_dir().join("model.obj");
        assert!(valid_model_path(&absolute));
        assert!(!valid_model_path(std::path::Path::new("model.obj")));
        assert!(!valid_model_path(&std::env::temp_dir().join("model.dds")));
        assert!(!valid_model_path(
            &std::env::temp_dir().join("..").join("model.obj")
        ));
    }

    #[test]
    fn the_drawn_position_follows_the_clock_between_two_snapshots() {
        let previous = frame(10.0, 0.0);
        let current = frame(11.0, 90.0);
        let mut drawn = current.clone();
        blended(&previous, &mut drawn, 0.0);
        assert!((drawn.target.latitude - 10.0).abs() < 1e-9);
        assert!((drawn.target.track_degrees - 0.0).abs() < 1e-9);
        let mut drawn = current.clone();
        blended(&previous, &mut drawn, 0.5);
        assert!((drawn.target.latitude - 10.5).abs() < 1e-9);
        assert!((drawn.target.track_degrees - 45.0).abs() < 1e-9);
        let mut drawn = current.clone();
        blended(&previous, &mut drawn, 1.0);
        assert!((drawn.target.latitude - 11.0).abs() < 1e-9);
        assert!((drawn.target.track_degrees - 90.0).abs() < 1e-9);
    }
    #[test]
    fn buffer_moves_each_frame_and_never_overshoots_a_stop() {
        let start = std::time::Instant::now();
        let mut timeline = Timeline::default();
        for tick in 0..8 {
            let t = f64::from(tick) * 0.5;
            let position = t.min(2.0) * 0.001;
            let now = start + std::time::Duration::from_secs_f64(t);
            timeline.receive(&[frame(position, 0.0)], Some((1, t)), false, now);
            for sub in 0..10 {
                let wall = t + f64::from(sub) * 0.05;
                let drawn = timeline.sample(start + std::time::Duration::from_secs_f64(wall));
                let expected = (wall - Timeline::MIN_DELAY_SECONDS).clamp(0.0, 2.0) * 0.001;
                assert!(
                    (drawn[0].target.latitude - expected).abs() < 1e-9,
                    "wall={wall}"
                );
            }
        }
        assert!(
            (timeline.sample(start + std::time::Duration::from_secs(20))[0]
                .target
                .latitude
                - 0.002)
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn one_hertz_samples_keep_moving_without_a_hold() {
        let start = std::time::Instant::now();
        let mut timeline = Timeline::default();
        let mut previous = None;
        let mut hold: f64 = 0.0;
        let mut longest_hold: f64 = 0.0;
        for step in 0..80 {
            let wall = f64::from(step) * 0.1;
            let now = start + std::time::Duration::from_secs_f64(wall);
            if step % 10 == 0 {
                timeline.receive(&[frame(wall * 0.001, 0.0)], Some((1, wall)), false, now);
            }
            if wall < 2.0 {
                continue;
            }
            let latitude = timeline.sample(now)[0].target.latitude;
            if previous == Some(latitude) {
                hold += 0.1;
                longest_hold = longest_hold.max(hold);
            } else {
                hold = 0.0;
            }
            previous = Some(latitude);
        }
        assert!(
            longest_hold < 0.2,
            "one-hertz source samples stalled the timeline for {longest_hold:.1} s"
        );
    }

    #[test]
    fn jitter_pause_reset_removal_and_model_changes_do_not_extrapolate() {
        let start = std::time::Instant::now();
        let mut timeline = Timeline::default();
        let at = |seconds| start + std::time::Duration::from_secs_f64(seconds);
        for (wall, sim) in [(0.0, 0.0), (0.6, 0.5), (1.05, 1.0)] {
            timeline.receive(&[frame(sim * 0.001, 0.0)], Some((1, sim)), false, at(wall));
        }
        let before = timeline.sample(at(1.1))[0].target.latitude;
        timeline.receive(&[frame(0.001, 0.0)], Some((1, 1.0)), true, at(1.2));
        assert_eq!(timeline.sample(at(10.0))[0].target.latitude, before);
        timeline.receive(&[frame(0.001, 0.0)], Some((1, 1.0)), false, at(10.0));
        assert!((timeline.sample(at(10.0))[0].target.latitude - before).abs() < 1e-9);
        timeline.receive(&[frame(0.0, 0.0)], Some((2, 0.0)), false, at(11.0));
        assert_eq!(timeline.sample(at(11.0))[0].target.latitude, 0.0);
        let mut replacement = frame(0.002, 0.0);
        Arc::make_mut(&mut replacement.model).id = "replacement".into();
        timeline.receive(&[replacement], Some((2, 0.5)), false, at(11.5));
        assert_eq!(timeline.sample(at(11.6))[0].target.latitude, 0.002);
        timeline.receive(&[], Some((2, 1.0)), true, at(12.0));
        assert!(timeline.sample(at(12.0)).is_empty());
    }

    #[test]
    fn late_samples_do_not_reverse_the_playhead() {
        let start = std::time::Instant::now();
        let mut timeline = Timeline::default();
        timeline.receive(&[frame(0.0, 0.0)], Some((1, 0.0)), false, start);
        timeline.receive(
            &[frame(0.0005, 0.0)],
            Some((1, 0.5)),
            false,
            start + std::time::Duration::from_millis(200),
        );
        let before = timeline.sample(start + std::time::Duration::from_millis(990))[0]
            .target
            .latitude;
        timeline.receive(
            &[frame(0.001, 0.0)],
            Some((1, 1.0)),
            false,
            start + std::time::Duration::from_secs(1),
        );
        let after = timeline.sample(start + std::time::Duration::from_millis(1010))[0]
            .target
            .latitude;
        assert!(after >= before - 1e-12, "{before} then {after}");
    }

    #[test]
    fn body_heading_without_override_is_interpolated_once_across_north() {
        let mut previous = frame(0.0, 350.0);
        let mut current = frame(0.0, 10.0);
        previous.body_heading_degrees = None;
        current.body_heading_degrees = None;
        blended(&previous, &mut current, 0.5);
        assert_eq!(current.body_heading_degrees, Some(0.0));
        assert_eq!(current.target.track_degrees, 0.0);
    }

    #[test]
    fn frame_rate_gate_counts_callbacks_even_with_tcas_running() {
        let mut clock = DrawClock::default();
        let start = std::time::Instant::now();
        let draws: Vec<_> = (0..10)
            .filter(|frame| {
                clock.ready(
                    openatc_settings::TrafficRate::EveryFrames(3),
                    start + std::time::Duration::from_millis(frame * 16),
                )
            })
            .collect();
        assert_eq!(draws, [0, 3, 6, 9]);
    }

    #[test]
    fn probe_failure_keeps_the_converted_local_altitude() {
        let local = [100.0, -23.0, 200.0];
        assert_eq!(ground_pose(local, None, 90.0, 1.0, 2.0), (-23.0, 1.0, 2.0));
        let ground = Terrain {
            x: 100.0,
            y: -20.0,
            z: 200.0,
            normal: [0.0, 1.0, 0.0],
            coordinate: [45.0, 5.0],
        };
        assert_eq!(
            ground_pose(local, Some(&ground), 90.0, 1.0, 2.0),
            (-20.0, 1.0, 0.0)
        );
        assert_eq!(
            ground_pose([200.0, -23.0, 200.0], Some(&ground), 90.0, 1.0, 2.0),
            (-23.0, 1.0, 2.0)
        );
    }
    #[test]
    fn hertz_gate_never_falls_below_the_minimum_frame_rate() {
        let mut clock = DrawClock::default();
        let start = std::time::Instant::now();
        let draws: Vec<_> = (0..10)
            .filter(|tick| {
                clock.ready(
                    openatc_settings::TrafficRate::Hertz(10),
                    start + std::time::Duration::from_millis(tick * 50),
                )
            })
            .collect();
        // 50 ms callbacks are already 20 Hz, below the 24 Hz floor; the floor draws every
        // callback rather than dropping to the requested 10 Hz.
        assert_eq!(draws, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }
    #[test]
    fn ground_relocation_is_not_drawn_as_a_taxi_route() {
        let start = std::time::Instant::now();
        let mut timeline = Timeline::default();
        let mut old = frame(0.0, 0.0);
        old.target.on_ground = true;
        let mut new = frame(0.005, 0.0);
        new.target.on_ground = true;
        timeline.receive(&[old], Some((1, 0.0)), false, start);
        timeline.receive(
            &[new],
            Some((1, 0.5)),
            false,
            start + std::time::Duration::from_millis(500),
        );
        assert_eq!(
            timeline.sample(start + std::time::Duration::from_secs(2))[0]
                .target
                .latitude,
            0.005
        );
    }
}
