// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use xplane_sys::{XPLMDestroyInstance, XPLMDrawInfo_t, XPLMInstanceRef, XPLMInstanceSetPosition};

use crate::NoSendSync;

#[derive(Debug, Clone, Copy)]
/// A position to draw an [`Instance`] in.
pub struct Position {
    /// The X position, in OpenGL local coordinates.
    pub x: f32,
    /// The Y position, in OpenGL local coordinates.
    pub y: f32,
    /// The Z position, in OpenGL local coordinates.
    pub z: f32,
    /// The pitch, in degrees.
    pub pitch: f32,
    /// The heading, in degrees.
    pub hdg: f32,
    /// The roll, in degrees.
    pub roll: f32,
}

/// An instance of an object, rendered in the world.
pub struct Instance<const NUM_DATAREFS: usize> {
    pub(crate) handle: XPLMInstanceRef,
    pub(crate) _phantom: NoSendSync,
}

impl<const NUM_DATAREFS: usize> Instance<NUM_DATAREFS> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    /// Set the position of this instance.
    /// <div class="warning"> Do not call this function from within a draw callback!
    /// That will cause UB. </div>
    pub fn set_position(&mut self, pos: Position, datarefs: &[f32; NUM_DATAREFS]) {
        let draw_info = XPLMDrawInfo_t {
            structSize: std::mem::size_of::<XPLMDrawInfo_t>() as i32,
            x: pos.x,
            y: pos.y,
            z: pos.z,
            pitch: pos.pitch,
            heading: pos.hdg,
            roll: pos.roll,
        };
        let datarefs: *const [f32] = datarefs;
        let datarefs: *const f32 = (datarefs).cast::<f32>();
        unsafe {
            XPLMInstanceSetPosition(self.handle, &draw_info, datarefs);
        }
    }
}

impl<const NUM_DATAREFS: usize> Drop for Instance<NUM_DATAREFS> {
    fn drop(&mut self) {
        unsafe {
            XPLMDestroyInstance(self.handle);
        }
    }
}
