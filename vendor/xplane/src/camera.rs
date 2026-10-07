// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::{
    ffi::{c_int, c_void},
    marker::PhantomData,
    mem::MaybeUninit,
};

use xplane_sys::{
    XPLMCameraControlDuration, XPLMCameraPosition_t, XPLMControlCamera, XPLMDontControlCamera,
    XPLMIsCameraBeingControlled, XPLMReadCameraPosition,
};

use crate::{make_x, NoSendSync, XPAPI};

#[derive(Debug)]
/// A camera position.
/// You'll want to use `XPLMGraphics` APIs to convert to local coordinates if you have world coordinates.
/// One unit in X, Y, and Z is one meter, and the origin is an arbitrary point nearby at sea level.
pub struct Position {
    /// Local OpenGL X coordinate.
    pub x: f32,
    /// Local OpenGL Y coordinate.
    pub y: f32,
    /// Local OpenGL Z coordinate.
    pub z: f32,
    /// Rotation from flat north, in degrees.
    /// Positive means nose up.
    pub pitch: f32,
    /// Rotation from flat north, in degrees.
    /// Positive means yaw right.
    pub yaw: f32,
    /// Rotation from flat north, in degrees.
    /// Positive means roll right.
    pub roll: f32,
    /// Zoom factor. `1.0` is normal, `2.0` is magnifying by 2x, and so on.
    pub zoom: f32,
}

#[derive(Debug)]
/// Returned from [`CameraController::control_camera`].
pub enum CameraControlResult {
    /// Let X-Plane control the camera on this draw loop.
    /// May or may not cause X-Plane to stop using this controller.
    /// Assume it does, for now.
    Surrender,
    /// Keep control of the camera, and reposition it.
    Reposition(Position),
}

/// An object to control the camera.
pub trait CameraController: 'static {
    /// The callback in which you control the camera.
    fn control_camera(&mut self, x: &mut XPAPI, is_losing_control: bool) -> CameraControlResult;
}

/// A registered controller for the camera.
/// Will release control when it is dropped.
/// Your callback will not be called with `is_losing_control` being true.
pub struct RegisteredController {
    ctx: *mut RegisteredControllerCtx,
    _phantom: NoSendSync,
}

impl RegisteredController {
    fn new(controller: impl CameraController, duration: XPLMCameraControlDuration) -> Self {
        let controller = Box::into_raw(Box::new(controller));
        let ctx = RegisteredControllerCtx {
            controller,
            is_active: true,
            _phantom: PhantomData,
        };
        let ctx = Box::into_raw(Box::new(ctx));
        unsafe {
            XPLMControlCamera(duration, Some(camera_controller), ctx.cast());
        }
        Self {
            ctx,
            _phantom: PhantomData,
        }
    }

    #[must_use]
    /// Check if this camera controller is active.
    pub fn is_active(&mut self) -> bool {
        unsafe { (*self.ctx).is_active }
    }
}

impl Drop for RegisteredController {
    fn drop(&mut self) {
        let is_active = unsafe { (*self.ctx).is_active };
        if is_active {
            unsafe {
                XPLMDontControlCamera();
            }
        }
        let _ = unsafe { Box::from_raw(self.ctx) };
    }
}

struct RegisteredControllerCtx {
    controller: *mut dyn CameraController,
    is_active: bool,
    _phantom: NoSendSync,
}

impl Drop for RegisteredControllerCtx {
    fn drop(&mut self) {
        let _ = unsafe { Box::from_raw(self.controller) };
    }
}

/// Access struct for X-Plane's camera API.
pub struct CameraApi {
    pub(crate) _phantom: NoSendSync,
}

impl CameraApi {
    /// Control the camera.
    /// The struct returned will stop controlling the camera when it is dropped.
    /// # Beware
    /// <div id="warning"> If all you want to do is control the head position in the cockpit,
    /// use datarefs instead. </div>
    /// # Warning
    /// <div id="warning"> If drawing a view external to the plane, use datarefs to first set
    /// the camera mode to an external view. </div>
    pub fn control_camera(
        &mut self,
        controller: impl CameraController,
        duration: XPLMCameraControlDuration,
    ) -> RegisteredController {
        RegisteredController::new(controller, duration)
    }
    /// Check whether the camera is being controlled.
    /// If it is being controlled, [`Some`] with the duration of the control
    /// will be returned.
    pub fn is_controlled(&mut self) -> Option<XPLMCameraControlDuration> {
        let mut duration = XPLMCameraControlDuration(1000);
        if unsafe { XPLMIsCameraBeingControlled(&mut duration) } == 0 {
            None
        } else {
            (duration.0 != 1000).then_some(duration)
        }
    }
    /// Get the position of the camera.
    pub fn get_pos(&mut self) -> Position {
        let mut pos: MaybeUninit<XPLMCameraPosition_t> = MaybeUninit::zeroed();
        unsafe {
            XPLMReadCameraPosition(pos.as_mut_ptr());
        }
        let pos = unsafe { pos.assume_init() };
        Position {
            x: pos.x,
            y: pos.y,
            z: pos.z,
            pitch: pos.pitch,
            yaw: pos.heading,
            roll: pos.roll,
            zoom: pos.zoom,
        }
    }
}

unsafe extern "C-unwind" fn camera_controller(
    out_pos: *mut XPLMCameraPosition_t,
    is_losing_control: c_int,
    refcon: *mut c_void,
) -> c_int {
    let reg_con = unsafe {
        refcon.cast::<RegisteredControllerCtx>().as_mut().unwrap() // This will not be a null pointer.
    };
    let losing_control = is_losing_control != 0;
    let mut x = make_x();
    let res = match unsafe {
        reg_con
            .controller
            .as_mut()
            .unwrap() // UNWRAP: This will not be null.
            .control_camera(&mut x, losing_control)
    } {
        CameraControlResult::Surrender => {
            reg_con.is_active = true;
            0
        }
        CameraControlResult::Reposition(Position {
            x,
            y,
            z,
            pitch,
            yaw: hdg,
            roll,
            zoom,
        }) => unsafe {
            (*out_pos).x = x;
            (*out_pos).y = y;
            (*out_pos).z = z;
            (*out_pos).pitch = pitch;
            (*out_pos).heading = hdg;
            (*out_pos).roll = roll;
            (*out_pos).zoom = zoom;
            1
        },
    };
    if losing_control {
        reg_con.is_active = false;
    }
    res
}
