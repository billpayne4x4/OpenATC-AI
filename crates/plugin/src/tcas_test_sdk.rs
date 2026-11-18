//! In-process SDK boundary checks; no simulator is loaded by these tests.
#![allow(non_snake_case, non_camel_case_types)]

use std::{
    cell::RefCell,
    ffi::{CStr, c_char, c_void},
};

pub type XPLMDataRef = *mut c_void;
type Callback = unsafe extern "C-unwind" fn(*mut c_void);
const MY_ID: i32 = 42;
const IDS: usize = 1;
const OVERRIDE: usize = 2;
const MODES: usize = 15;

struct State {
    owner: i32,
    active: i32,
    override_tcas: i32,
    callback: Option<Callback>,
    acquire_calls: usize,
    ids: [i32; 64],
    modes: [i32; 64],
    position_writes: usize,
    counts: Vec<i32>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            owner: -1,
            active: 4,
            override_tcas: 0,
            callback: None,
            acquire_calls: 0,
            ids: [0; 64],
            modes: [0; 64],
            position_writes: 0,
            counts: Vec::new(),
        }
    }
}
thread_local! { static SDK: RefCell<State> = RefCell::new(State::default()); }

pub unsafe fn XPLMFindDataRef(name: *const c_char) -> XPLMDataRef {
    let name = unsafe { CStr::from_ptr(name) }.to_str().unwrap();
    let key = match name.rsplit('/').next().unwrap() {
        "modeS_id" => IDS,
        "override_TCAS" => OVERRIDE,
        "x" => 3,
        "y" => 4,
        "z" => 5,
        "vx" => 6,
        "vy" => 7,
        "vz" => 8,
        "psi" => 9,
        "the" => 10,
        "phi" => 11,
        "gear_deploy" => 12,
        "flap_ratio" => 13,
        "weight_on_wheels" => 14,
        "ssr_mode" => MODES,
        "flight_id" => 16,
        "icao_type" => 17,
        _ => panic!("Unexpected dataref: {name}"),
    };
    key as XPLMDataRef
}
pub unsafe fn XPLMCanWriteDataRef(_: XPLMDataRef) -> i32 {
    1
}
pub unsafe fn XPLMGetMyID() -> i32 {
    MY_ID
}
pub unsafe fn XPLMCountAircraft(total: *mut i32, active: *mut i32, owner: *mut i32) {
    SDK.with_borrow(|s| unsafe {
        *total = 64;
        *active = s.active;
        *owner = s.owner;
    });
}
pub unsafe fn XPLMAcquirePlanes(
    models: *mut *mut c_char,
    callback: Option<Callback>,
    context: *mut c_void,
) -> i32 {
    assert!(models.is_null() && context.is_null());
    SDK.with_borrow_mut(|s| {
        s.acquire_calls += 1;
        if s.owner != -1 && s.owner != MY_ID {
            s.callback = callback;
            return 0;
        }
        s.owner = MY_ID;
        s.callback = None;
        1
    })
}
pub unsafe fn XPLMReleasePlanes() {
    SDK.with_borrow_mut(|s| {
        assert_eq!(s.owner, MY_ID);
        assert_eq!(s.override_tcas, 0, "release override before ownership");
        s.owner = -1;
    });
}
pub unsafe fn XPLMGetDatai(reference: XPLMDataRef) -> i32 {
    assert_eq!(reference as usize, OVERRIDE);
    SDK.with_borrow(|s| s.override_tcas)
}
pub unsafe fn XPLMSetDatai(reference: XPLMDataRef, value: i32) {
    assert_eq!(reference as usize, OVERRIDE);
    SDK.with_borrow_mut(|s| {
        assert_eq!(s.owner, MY_ID);
        s.override_tcas = value;
    });
}
pub unsafe fn XPLMSetActiveAircraftCount(count: i32) {
    SDK.with_borrow_mut(|s| {
        assert_eq!(s.owner, MY_ID);
        s.counts.push(count);
        s.active = count;
    });
}
pub unsafe fn XPLMGetDatavi(reference: XPLMDataRef, out: *mut i32, offset: i32, count: i32) -> i32 {
    assert_eq!(reference as usize, IDS);
    if out.is_null() {
        return 64;
    }
    assert_eq!((offset, count), (0, 1));
    SDK.with_borrow(|s| unsafe {
        *out = s.ids[0];
    });
    1
}
pub unsafe fn XPLMSetDatavi(reference: XPLMDataRef, data: *mut i32, offset: i32, count: i32) {
    assert_eq!(offset, 1);
    SDK.with_borrow_mut(|s| {
        assert_eq!(s.owner, MY_ID);
        let data = unsafe { std::slice::from_raw_parts(data, count as usize) };
        match reference as usize {
            IDS => s.ids[1..=data.len()].copy_from_slice(data),
            MODES => s.modes[1..=data.len()].copy_from_slice(data),
            14 => {}
            _ => panic!("Unexpected array"),
        }
    });
}
pub unsafe fn XPLMSetDatab(_: XPLMDataRef, data: *mut c_void, offset: i32, count: i32) {
    assert_eq!(offset, 8);
    let data = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), count as usize) };
    assert!(data.as_chunks::<8>().0.iter().all(|chunk| chunk[7] == 0));
}
pub unsafe fn XPLMSetDatavf(reference: XPLMDataRef, data: *mut f32, offset: i32, count: i32) {
    assert_eq!(offset, 1);
    SDK.with_borrow_mut(|s| {
        assert_eq!(s.owner, MY_ID);
        assert_eq!(s.override_tcas, 1);
        assert_eq!(count, s.active, "publish only filled target slots");
        assert!(
            s.ids[1..=count as usize]
                .iter()
                .all(|id| (1..=0xffffff).contains(id))
        );
        let data = unsafe { std::slice::from_raw_parts(data, count as usize) };
        assert!(data.iter().all(|value| value.is_finite()));
        if (3..=5).contains(&(reference as usize)) {
            s.position_writes += 1;
        }
    });
}
pub unsafe fn XPLMWorldToLocal(
    lat: f64,
    lon: f64,
    altitude: f64,
    x: *mut f64,
    y: *mut f64,
    z: *mut f64,
) {
    unsafe {
        *x = lon * 1000.0;
        *y = altitude;
        *z = -lat * 1000.0;
    }
}

#[test]
fn publication_uses_identified_slots_and_recovers_after_another_provider() {
    use super::{GeneratedTraffic, Ordering, PLANES_AVAILABLE, Tcas, mode_s_identity};
    use openatc_core::{
        state::TrafficTarget,
        traffic::{animation::Animation, csl::Model},
    };
    PLANES_AVAILABLE.store(false, Ordering::Relaxed);
    SDK.with_borrow_mut(|s| {
        *s = State::default();
        s.ids[0] = 0x8000_0123_u32 as i32;
    });
    let frame = |id, ground, engine| GeneratedTraffic {
        target: TrafficTarget {
            id,
            callsign: "LONGCALLSIGN".into(),
            aircraft: "A320".into(),
            latitude: 43.0,
            longitude: 7.0,
            altitude_feet: 1000.0,
            track_degrees: 90.0,
            ground_speed_knots: Some(150.0),
            on_ground: ground,
            ..Default::default()
        },
        model: std::sync::Arc::new(Model {
            id: "test".into(),
            objects: Vec::new(),
            vertical_offset: None,
            matches: Vec::new(),
        }),
        animation: Animation {
            engine,
            ..Default::default()
        },
        vertical_speed_fpm: 500.0,
        body_heading_degrees: None,
    };
    let frames = vec![
        frame(0, false, 1.0),
        frame(0x0100_0000, false, 1.0),
        frame(0x123, false, 1.0),
        frame(0xa00001, true, 1.0),
        frame(0xa00001, true, 1.0),
        frame(0xffffff, false, 0.0),
        frame(0xa00002, true, 0.0),
    ];
    let mut tcas = Tcas::default();
    tcas.receive(&frames);
    SDK.with_borrow(|s| {
        assert_eq!(s.counts, [3]);
        assert_eq!(&s.ids[1..4], &[0xa00001, 0xffffff, 0xa00002]);
        assert_eq!(&s.modes[1..4], &[5, 3, 0]);
        assert_eq!(s.ids[0], 0x8000_0123_u32 as i32);
    });
    for _ in 0..12 {
        tcas.refresh();
    }
    SDK.with_borrow(|s| assert_eq!(s.position_writes, 39));
    tcas.receive(&frames[5..6]);
    SDK.with_borrow(|s| {
        assert_eq!(s.active, 1);
        assert_eq!(s.ids[1], 0xffffff);
    });
    tcas.yield_to_provider();
    SDK.with_borrow_mut(|s| {
        assert_eq!(s.active, 4);
        s.owner = 77;
        s.override_tcas = 1;
    });
    tcas.receive(&frames);
    let calls = SDK.with_borrow(|s| s.acquire_calls);
    tcas.receive(&frames);
    SDK.with_borrow(|s| assert_eq!(s.acquire_calls, calls));
    let callback = SDK.with_borrow_mut(|s| {
        s.owner = -1;
        s.override_tcas = 0;
        s.active = 2;
        s.ids[1..].fill(0);
        s.callback.take().expect("retry must be registered")
    });
    unsafe {
        callback(std::ptr::null_mut());
    }
    tcas.receive(&frames);
    assert!(tcas.active());
    SDK.with_borrow(|s| {
        assert_eq!(s.active, 3);
        assert_eq!(s.ids[1], 0xa00001);
    });
    // Unexpected loss of ownership must not write into the new provider's arrays.
    SDK.with_borrow_mut(|s| s.owner = 77);
    tcas.refresh();
    assert!(!tcas.active());
    tcas.receive(&[]);
    for (raw, expected) in [
        (0_u32, None),
        (0x8000_0000, None),
        (0x8000_0123, Some(0x123)),
        (0x7f00_0123, None),
        (0x00ff_ffff, Some(0xffffff)),
    ] {
        assert_eq!(mode_s_identity(raw as i32), expected);
    }
}

#[test]
fn unaddressable_frames_do_not_take_and_release_the_traffic_slots() {
    use super::{GeneratedTraffic, Ordering, PLANES_AVAILABLE, Tcas};
    use openatc_core::{
        state::TrafficTarget,
        traffic::{animation::Animation, csl::Model},
    };
    PLANES_AVAILABLE.store(false, Ordering::Relaxed);
    SDK.with_borrow_mut(|state| *state = State::default());
    let frame = |id| GeneratedTraffic {
        target: TrafficTarget {
            id,
            callsign: "LOCAL1".into(),
            aircraft: "C172".into(),
            latitude: 43.0,
            longitude: 7.0,
            altitude_feet: 1000.0,
            track_degrees: 90.0,
            ..Default::default()
        },
        model: std::sync::Arc::new(Model {
            id: "test".into(),
            objects: Vec::new(),
            vertical_offset: None,
            matches: Vec::new(),
        }),
        animation: Animation::default(),
        vertical_speed_fpm: 0.0,
        body_heading_degrees: None,
    };
    let mut tcas = Tcas::default();
    tcas.receive(&[frame(0x0e00_0001), frame(0x0e00_0002)]);
    assert!(!tcas.active());
    SDK.with_borrow(|state| {
        assert_eq!(state.acquire_calls, 0, "no slots may be taken");
        assert_eq!(state.override_tcas, 0);
        assert_eq!(state.active, 4);
    });
}
