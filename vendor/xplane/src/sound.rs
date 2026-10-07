// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use libfmod::{ChannelGroup, Studio};
use xplane_sys::{XPLMAudioBus, XPLMGetFMODChannelGroup, XPLMGetFMODStudio};

use crate::NoSendSync;

/// Access struct for X-Plane's sound APIs.
pub struct SoundApi {
    pub(crate) _phantom: NoSendSync,
}

impl SoundApi {
    #[must_use]
    /// Get the Fmod Studio system used by X-Plane.
    pub fn fmod_studio(&mut self) -> Studio {
        let sys = unsafe { XPLMGetFMODStudio() };
        Studio::from(sys.cast())
    }

    #[must_use]
    /// Get the Fmod channel group corresponding to `bus`.
    /// If `bus` is not valid, [`None`] will be returned.
    pub fn fmod_channel_group(&mut self, bus: XPLMAudioBus) -> Option<ChannelGroup> {
        if audio_bus_valid(bus) {
            let cg = unsafe { XPLMGetFMODChannelGroup(bus) };
            Some(ChannelGroup::from(cg.cast()))
        } else {
            None
        }
    }
}

#[inline]
fn audio_bus_valid(bus: XPLMAudioBus) -> bool {
    matches!(
        bus,
        XPLMAudioBus::RadioCom1
            | XPLMAudioBus::RadioCom2
            | XPLMAudioBus::RadioPilot
            | XPLMAudioBus::RadioCopilot
            | XPLMAudioBus::ExteriorAircraft
            | XPLMAudioBus::ExteriorEnvironment
            | XPLMAudioBus::ExteriorUnprocessed
            | XPLMAudioBus::Interior
            | XPLMAudioBus::UI
            | XPLMAudioBus::Ground
            | XPLMAudioBus::Master
    )
}
