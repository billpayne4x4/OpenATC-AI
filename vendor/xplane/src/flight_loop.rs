// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

//! # Flight loop callbacks
//!
//! X-Plane can call plugin code at timed intervals or when it runs its flight model.
//!
//! A [`FlightLoop`] object must persist for callbacks to occur. When the [`FlightLoop`] is dropped,
//! its callbacks will stop.
//!
//! # Examples
//!
//! Closure handler:
//!
//! ```no_run
//! use xplane::{XPAPI, flight_loop::{FlightLoop, FlightLoopPhase, LoopState, LoopResult}};
//!
//! struct MyLoopState;
//! fn a_callback(xpapi: &mut XPAPI) {
//!     let handler = |_xpapi: &mut XPAPI, loop_state: &mut LoopState<()>| -> LoopResult {
//!         println!("Flight loop callback running");
//!         LoopResult::NextLoop
//!     };
//!
//!     let mut flight_loop = xpapi.new_flight_loop(FlightLoopPhase::BeforeFlightModel, handler, ());
//!     flight_loop.schedule_immediate();
//! }
//! ```
//!
//! Struct handler:
//!
//! ```no_run
//! use xplane::{XPAPI, flight_loop::{FlightLoop, FlightLoopCallback, FlightLoopPhase, LoopState, LoopResult}};
//!
//! struct MyLoopHandler;
//!
//! impl FlightLoopCallback<()> for MyLoopHandler {
//!     fn flight_loop(&mut self, _xpapi: &mut XPAPI, state: &mut LoopState<()>) -> LoopResult {
//!         println!("Flight loop callback running");
//!         // You can keep state data in your own struct.
//!         LoopResult::NextLoop
//!     }
//! }
//! fn a_callback(xpapi: &mut XPAPI) {
//!     let mut flight_loop = xpapi.new_flight_loop(FlightLoopPhase::BeforeFlightModel, MyLoopHandler, ());
//!     flight_loop.schedule_immediate();
//! }
//! ```
//!

use std::{f32, fmt, marker::PhantomData, mem, time::Duration};

use std::ffi::{c_float, c_int, c_void};

pub use xplane_sys::XPLMFlightLoopPhaseType as FlightLoopPhase;

use crate::{make_x, NoSendSync, XPAPI};

/// Tracks a flight loop callback, which can be called by X-Plane periodically for calculations
///
#[derive(Debug)]
pub struct FlightLoop<T: 'static> {
    /// The loop data, allocated by a [`Box`]
    data: *mut LoopData<T>,
}

impl<T: 'static> FlightLoop<T> {
    pub(crate) fn new(
        phase: FlightLoopPhase,
        callback: impl FlightLoopCallback<T>,
        base_state: T,
    ) -> Self {
        let data = Box::new(LoopData::new(callback, base_state));
        let data_ptr: *mut LoopData<T> = Box::into_raw(data);
        // Create a flight loop
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let mut config = xplane_sys::XPLMCreateFlightLoop_t {
            structSize: mem::size_of::<xplane_sys::XPLMCreateFlightLoop_t>() as c_int,
            phase,
            callbackFunc: Some(flight_loop_callback::<T>),
            refcon: data_ptr.cast::<c_void>(),
        };
        unsafe {
            (*data_ptr).loop_id = Some(xplane_sys::XPLMCreateFlightLoop(&mut config));
        }
        FlightLoop { data: data_ptr }
    }

    /// Schedules the flight loop callback to be executed in the next flight loop
    ///
    /// After the flight loop callback is first called, it will continue to be called
    /// every flight loop unless it cancels itself or changes its schedule.
    pub fn schedule_immediate(&mut self) {
        unsafe {
            (*self.data).set_interval(LoopResult::Loops(1));
        }
    }

    /// Schedules the flight loop callback to be executed after a specified number of flight loops
    ///
    /// After the callback is first called, it will continue to be called with the provided loop
    /// interval.
    pub fn schedule_after_loops(&mut self, loops: u32) {
        unsafe {
            (*self.data).set_interval(LoopResult::Loops(loops));
        }
    }

    /// Schedules the flight loop callback to be executed after the specified delay
    ///
    /// After the callback is first called, it will continue to be called with that interval.
    #[allow(clippy::cast_precision_loss)]
    pub fn schedule_after(&mut self, time: Duration) {
        let seconds_f = time.as_secs_f32();
        unsafe {
            (*self.data).set_interval(LoopResult::Seconds(seconds_f));
        }
    }

    /// Deactivates the flight loop
    pub fn deactivate(&mut self) {
        unsafe {
            (*self.data).set_interval(LoopResult::Deactivate);
        }
    }
}

impl<T> Drop for FlightLoop<T> {
    fn drop(&mut self) {
        unsafe {
            let _ = Box::from_raw(self.data);
        }
    }
}

/// Data stored as part of a [`FlightLoop`] and used as a refcon
struct LoopData<T> {
    /// The loop result, or [`None`] if the loop has not been scheduled
    loop_result: Option<LoopResult>,
    /// The loop ID
    loop_id: Option<xplane_sys::XPLMFlightLoopID>,
    /// The callback (stored here but not used)
    callback: *mut dyn FlightLoopCallback<T>,
    /// The flight loop's stored state
    loop_state: *mut T,
    _phantom: NoSendSync,
}

#[allow(clippy::missing_fields_in_debug)] // Clippy thinks _phantom is missing. There is no reason to include it, and a lack of inclusion does not make it non-exhaustive.
impl<T> fmt::Debug for LoopData<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("LoopData")
            .field("loop_result", &self.loop_result)
            .field("loop_id", &self.loop_id)
            .field("callback", &"[callback]")
            .field("loop_state", &"[callback state]")
            .finish()
    }
}

impl<T> LoopData<T> {
    /// Creates a new [`LoopData`] with a callback
    pub(crate) fn new(callback: impl FlightLoopCallback<T>, base_state: T) -> Self {
        LoopData {
            loop_result: None,
            loop_id: None,
            callback: Box::into_raw(Box::new(callback)),
            loop_state: Box::into_raw(Box::new(base_state)),
            _phantom: PhantomData,
        }
    }

    fn set_interval(&mut self, loop_result: LoopResult) {
        let loop_id = self.loop_id.expect("Loop ID not set");
        unsafe {
            xplane_sys::XPLMScheduleFlightLoop(loop_id, loop_result.into(), 1);
        }
        self.loop_result = Some(loop_result);
    }
}

impl<T> Drop for LoopData<T> {
    fn drop(&mut self) {
        if let Some(loop_id) = self.loop_id {
            unsafe { xplane_sys::XPLMDestroyFlightLoop(loop_id) }
        }
        let _ = unsafe { Box::from_raw(self.callback) };
        let _ = unsafe { Box::from_raw(self.loop_state) };
    }
}

/// Trait for objects that can receive flight loop callbacks
pub trait FlightLoopCallback<T>: 'static {
    /// Called periodically by X-Plane according to the provided scheduling
    ///
    /// In this callback, processing can be done. Drawing cannot be done.
    ///
    /// The provided [`LoopState`] can be used to get information and change the scheduling of
    /// callbacks. If the scheduling is not changed, this callback will continue to be called
    /// with the same schedule.
    fn flight_loop(&mut self, x: &mut XPAPI, state: &mut LoopState<T>) -> LoopResult;
}

/// Closures can be used as [`FlightLoopCallback`]s
impl<F, T> FlightLoopCallback<T> for F
where
    F: 'static + FnMut(&mut XPAPI, &mut LoopState<T>) -> LoopResult,
{
    fn flight_loop(&mut self, x: &mut XPAPI, state: &mut LoopState<T>) -> LoopResult {
        self(x, state)
    }
}

/// Information available during a flight loop callback
///
/// By default, a flight loop callback will continue to be called on its initial schedule.
/// The scheduling functions only need to be called if the callback scheduling should change.
#[derive(Debug)]
pub struct LoopState<T> {
    /// Time since last callback call
    since_call: Duration,
    /// Time since last flight loop
    since_loop: Duration,
    /// Callback counter
    counter: i32,
    state_data: *mut T,
}

impl<T> LoopState<T> {
    /// Returns the duration since the last time this callback was called
    #[must_use]
    pub fn since_last_call(&self) -> Duration {
        self.since_call
    }
    /// Returns the duration since the last flight loop
    ///
    /// If this callback is not called every flight loop, this may be different from the
    /// value returned from `time_since_last_call`.
    #[must_use]
    pub fn since_last_loop(&self) -> Duration {
        self.since_loop
    }
    /// Returns the value of a counter that increments every time the callback is called
    #[must_use]
    pub fn counter(&self) -> i32 {
        self.counter
    }
    /// Get an immutable reference to the state data.
    /// # Panics
    /// Panics if the pointer to the state data is null. This should not be possible.
    #[must_use]
    pub fn state(&mut self) -> &T {
        unsafe {
            self.state_data.as_ref().unwrap() // This will not be a null pointer.
        }
    }
    /// Get a mutable reference to the state data.
    /// # Panics
    /// Panics if the pointer to the state data is null. This should not be possible.
    #[must_use]
    pub fn state_mut(&mut self) -> &mut T {
        unsafe {
            self.state_data.as_mut().unwrap() // This will not be a null pointer.
        }
    }
}

/// Loop results, which determine when the callback will be called next
#[derive(Debug, Clone, Copy)]
pub enum LoopResult {
    /// Callback will be called after the provided number of seconds
    Seconds(f32),
    /// Callback will be called after the provided number of loops
    Loops(u32),
    /// Callback will be called after the next loop.
    /// Equivalent to Loops(1).
    NextLoop,
    /// Callback will not be called again until it is rescheduled
    Deactivate,
}

/// Converts a [`LoopResult`] into an [`f32`] suitable for returning from a flight loop callback
impl From<LoopResult> for f32 {
    #[allow(clippy::cast_precision_loss)]
    fn from(lr: LoopResult) -> Self {
        match lr {
            LoopResult::Deactivate => 0f32,
            LoopResult::Seconds(secs) => secs,
            LoopResult::NextLoop => -1.0f32,
            LoopResult::Loops(loops) => -1.0f32 * (loops as f32),
        }
    }
}

impl From<Duration> for LoopResult {
    fn from(value: Duration) -> Self {
        LoopResult::Seconds(value.as_secs_f32())
    }
}

/// The flight loop callback that X-Plane calls
///
/// This expands to a separate callback for every type C.
unsafe extern "C-unwind" fn flight_loop_callback<T: 'static>(
    since_last_call: c_float,
    since_loop: c_float,
    counter: c_int,
    refcon: *mut c_void,
) -> c_float {
    // Get the loop data
    let loop_data = refcon.cast::<LoopData<T>>();
    // Create a state
    let mut state = LoopState {
        since_call: Duration::from_secs_f32(since_last_call),
        since_loop: Duration::from_secs_f32(since_loop),
        counter,
        state_data: unsafe { (*loop_data).loop_state },
    };
    let mut x = make_x();
    let res = unsafe { (*(*loop_data).callback).flight_loop(&mut x, &mut state) };

    unsafe {
        (*loop_data).loop_result = Some(res);
    }

    // Return the next loop time
    f32::from(res)
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, ptr::NonNull, rc::Rc};

    use super::*;
    #[test]
    #[allow(clippy::too_many_lines, clippy::float_cmp)]
    fn test_flight_loops() {
        struct TestLoopHandler {
            internal_state: bool,
        }
        impl FlightLoopCallback<TestLoopState> for TestLoopHandler {
            fn flight_loop(
                &mut self,
                _x: &mut XPAPI,
                state: &mut LoopState<TestLoopState>,
            ) -> LoopResult {
                let test_state = state.state_mut();
                test_state.test_thing += 1;
                self.internal_state = !self.internal_state;
                println!("Test thing: {}", test_state.test_thing);
                println!("Internal state: {}", self.internal_state);
                match test_state.test_thing {
                    1 => {
                        assert_eq!(state.since_last_call(), Duration::from_secs_f32(2.0));
                        assert_eq!(state.since_last_loop(), Duration::from_secs_f32(2.0));
                        LoopResult::NextLoop
                    }
                    2 => LoopResult::Loops(2),
                    3 => LoopResult::Seconds(1.5f32),
                    4 => LoopResult::Deactivate,
                    _ => unreachable!(),
                }
            }
        }
        struct TestLoopState {
            test_thing: i32,
        }
        let expected_ptr = NonNull::<c_void>::dangling().as_ptr();
        let refcon_cell = Rc::new(RefCell::new(NonNull::<c_void>::dangling().as_ptr()));
        let create_flight_loop_ctx = xplane_sys::XPLMCreateFlightLoop_context();
        let refcon_cell_1 = refcon_cell.clone();
        create_flight_loop_ctx
            .expect()
            .once()
            .return_once_st(move |s| {
                let s = unsafe { *s };
                *refcon_cell_1.borrow_mut() = s.refcon;
                expected_ptr
            });
        let schedule_flight_loop_ctx = xplane_sys::XPLMScheduleFlightLoop_context();
        schedule_flight_loop_ctx.expect().once().return_once_st(
            move |loop_ref, when, relative_to_now| {
                assert!(loop_ref == expected_ptr);
                assert_eq!(when, -1.0f32);
                assert_eq!(relative_to_now, 1);
            },
        );
        let destroy_flight_loop_ctx = xplane_sys::XPLMDestroyFlightLoop_context();
        destroy_flight_loop_ctx
            .expect()
            .once()
            .return_once_st(move |loop_ref| {
                assert!(loop_ref == expected_ptr);
            });
        let mut x = make_x();
        let mut fl = x.new_flight_loop(
            FlightLoopPhase::BeforeFlightModel,
            TestLoopHandler {
                internal_state: false,
            },
            TestLoopState { test_thing: 0 },
        );
        fl.schedule_immediate();
        create_flight_loop_ctx.checkpoint();
        schedule_flight_loop_ctx.checkpoint();
        let refcon = *refcon_cell.borrow();
        unsafe {
            let res = flight_loop_callback::<TestLoopState>(2.0f32, 2.0f32, 1, refcon);
            assert_eq!(res, -1.0f32);
            let res = flight_loop_callback::<TestLoopState>(2.0f32, 2.0f32, 2, refcon);
            assert_eq!(res, -2.0f32);
            let res = flight_loop_callback::<TestLoopState>(2.0f32, 2.0f32, 3, refcon);
            assert_eq!(res, 1.5f32);
            let res = flight_loop_callback::<TestLoopState>(2.0f32, 2.0f32, 4, refcon);
            assert_eq!(res, 0.0f32);
        }
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_duration() {
        let dur = Duration::from_secs_f32(2.5f32);
        let loop_res: LoopResult = dur.into();
        let LoopResult::Seconds(secs) = loop_res else {
            panic!("Conversion failure somehow!");
        };
        assert_eq!(secs, 2.5f32);
    }
}
