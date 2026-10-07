// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

#![allow(non_upper_case_globals)] // Some weirdness from the proc-macro.
#![allow(clippy::cast_possible_wrap)]

use num_enum::{FromPrimitive, IntoPrimitive};
use xplane_sys::{
    XPLM_MSG_AIRPLANE_COUNT_CHANGED, XPLM_MSG_AIRPORT_LOADED, XPLM_MSG_LIVERY_LOADED,
    XPLM_MSG_PLANE_CRASHED, XPLM_MSG_PLANE_LOADED, XPLM_MSG_PLANE_UNLOADED,
    XPLM_MSG_SCENERY_LOADED, XPLM_MSG_WILL_WRITE_PREFS,
};

#[cfg(feature = "XPLM301")]
use xplane_sys::{XPLM_MSG_ENTERED_VR, XPLM_MSG_EXITING_VR};

#[cfg(feature = "XPLM303")]
use xplane_sys::XPLM_MSG_RELEASE_PLANES;

#[cfg(feature = "XPLM400")]
use xplane_sys::{
    XPLM_MSG_DATAREFS_ADDED, XPLM_MSG_FMOD_BANK_LOADED, XPLM_MSG_FMOD_BANK_UNLOADING,
};

#[repr(i32)]
#[derive(Debug, Eq, PartialEq, FromPrimitive, IntoPrimitive, Copy, Clone)]
/// Inter-plugin message.
pub enum MessageId {
    /// A plane has crashed. The message parameter is meaningless.
    PlaneCrashed = XPLM_MSG_PLANE_CRASHED,
    /// A plane has finished loading.
    ///
    /// The message parameter is the ID of the affected plane.
    /// `0` indicates the user's plane.
    /// Interpret the value of the message parameter as a [`c_int`](std::ffi::c_int), not as a pointer.
    PlaneLoaded = XPLM_MSG_PLANE_LOADED,
    /// An airport has been loaded.
    /// The message parameter is meaningless.
    AirportLoaded = XPLM_MSG_AIRPORT_LOADED,
    /// Scenery has been loaded.
    /// The message parameter is meaningless. Use datarefs to determine what files were loaded.
    SceneryLoaded = XPLM_MSG_SCENERY_LOADED,
    /// The user has adjusted the number of X-Plane aircraft models.
    /// Use [`XPLMCountPlanes`](xplane_sys::XPLMCountPlanes) to find out how many planes are now available.
    /// The message parameter is meaningless.
    AirplaneCountChanged = XPLM_MSG_AIRPLANE_COUNT_CHANGED,
    /// A plane has been unloaded.
    ///
    /// The message parameter is the ID of the affected plane.
    /// `0` indicates the user's plane.
    /// Interpret the value of the message parameter as a [`c_int`](std::ffi::c_int), not as a pointer.
    PlaneUnloaded = XPLM_MSG_PLANE_UNLOADED,
    /// X-Plane is going to write its preferences file.
    /// You should write your own preferences file, and if applicable, modify any datarefs that might
    /// influence X-Plane's preference file output.
    /// The message parameter is meaningless.
    WillWritePrefs = XPLM_MSG_WILL_WRITE_PREFS,
    /// A livery has been loaded for an airplane.
    /// The loaded livery can be checked via datarefs.
    ///
    /// The message parameter is the ID of the affected plane.
    /// `0` indicates the user's plane.
    /// Interpret the value of the message parameter as a [`c_int`](std::ffi::c_int), not as a pointer.
    LiveryLoaded = XPLM_MSG_LIVERY_LOADED,
    #[cfg(feature = "XPLM301")]
    /// Sent just before X-Plane enters virtual-reality mode.
    /// Any windows not positioned in VR mode will no longer be visible to the user.
    ///
    /// The message parameter is meaningless.
    EnteredVR = XPLM_MSG_ENTERED_VR,
    #[cfg(feature = "XPLM301")]
    /// Sent just before X-Plane leaves virtual-reality modes.
    /// You probably want to clean up any windows positioned in VR mode.
    ///
    /// The message parameter is meaningless.
    ExitingVR = XPLM_MSG_EXITING_VR,
    #[cfg(feature = "XPLM303")]
    /// Another plugin wants to take over AI planes.
    /// Use the sender ID to decide whether you wish to give up control. If you will not, ignore the message.
    /// See [X-Plane's docs](https://developer.x-plane.com/sdk/XPLMPlugin) for more info.
    ///
    /// The message parameter is meaningless.
    ReleasePlanes = XPLM_MSG_RELEASE_PLANES,
    #[cfg(feature = "XPLM400")]
    /// Sent after FMOD sound banks are loaded.
    /// The parameter is the [`XPLMBankID`](xplane_sys::XPLMBankID) that has been loaded.
    /// Untested, but the parameter is probably a [`c_int`](std::ffi::c_int), not a pointer.
    FmodBankLoaded = XPLM_MSG_FMOD_BANK_LOADED,
    #[cfg(feature = "XPLM400")]
    /// Sent before FMOD sound banks are unloaded.
    /// Any associated resources should be cleaned up.
    /// The parameter is the [`XPLMBankID`](xplane_sys::XPLMBankID) that is being unloaded.
    /// Untested, but the parameter is probably a [`c_int`](std::ffi::c_int), not a pointer.
    FmodBankUnloading = XPLM_MSG_FMOD_BANK_UNLOADING,
    #[cfg(feature = "XPLM400")]
    /// Sent per-frame (at-most) if/when datarefs are added.
    /// Includes the new dataref total count so your plugin can cache the count, and only query about the newly added ones.
    /// Untested, but the parameter is probably a [`c_int`](std::ffi::c_int), not a pointer.
    ///
    /// This message is only sent to plugins that enable the `XPLM_WANTS_DATAREF_NOTIFICATIONS` feature.
    DatarefsAdded = XPLM_MSG_DATAREFS_ADDED,
    #[num_enum(catch_all)]
    /// The message is unknown to this library. Its ID is included.
    UnknownMessage(i32),
}

impl MessageId {
    #[must_use]
    /// Check if this message ID is one reserved by X-Plane.
    pub fn is_xp_reserved(&self) -> bool {
        i32::from(*self) < 0x00FF_FFFF_i32
    }
}

pub struct Message {
    pub id: MessageId,
}
