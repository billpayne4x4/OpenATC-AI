// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::ffi::{c_int, c_void};
use std::{fmt, marker::PhantomData, mem};

use snafu::prelude::*;

use xplane_sys::{
    XPLMAvionicsID, XPLMCustomizeAvionics_t, XPLMDeviceID, XPLMRegisterAvionicsCallbacksEx,
    XPLMUnregisterAvionicsCallbacks,
};

use crate::NoSendSync;

#[non_exhaustive]
#[allow(missing_docs)]
pub enum DeviceID {
    GNS430(TwoSideDevice),
    GNS530(TwoSideDevice),
    CDU739(TwoSideDevice),
    G1000Pfd(TwoSideDevice),
    G1000Mfd,
    CDU815(TwoSideDevice),
    PrimusPfd(TwoSideDevice),
    PrimusMfd(ThreeSideDevice),
    PrimusRmu(TwoSideDevice),
}

#[allow(missing_docs)]
pub enum ThreeSideDevice {
    Pilot,
    Copilot,
    Center,
}

#[allow(missing_docs)]
pub enum TwoSideDevice {
    Pilot,
    Copilot,
}

#[derive(Debug, Snafu)]
#[snafu(display("No match for XPLMDeviceID: {id:?}"))]
/// The device ID was unrecognized.
pub struct DeviceUnmatchedError {
    /// The device ID.
    pub id: XPLMDeviceID,
}

impl TryFrom<XPLMDeviceID> for DeviceID {
    type Error = DeviceUnmatchedError;
    fn try_from(value: XPLMDeviceID) -> Result<Self, Self::Error> {
        use xplane_sys::XPLMDeviceID as di;
        Ok(match value {
            di::GNS430_Pilot => DeviceID::GNS430(TwoSideDevice::Pilot),
            di::GNS430_Copilot => DeviceID::GNS430(TwoSideDevice::Copilot),
            di::GNS530_Pilot => DeviceID::GNS530(TwoSideDevice::Pilot),
            di::GNS530_Copilot => DeviceID::GNS530(TwoSideDevice::Copilot),
            di::CDU739_Pilot => DeviceID::CDU739(TwoSideDevice::Pilot),
            di::CDU739_Copilot => DeviceID::CDU739(TwoSideDevice::Copilot),
            di::G1000_PFD_Pilot => DeviceID::G1000Pfd(TwoSideDevice::Pilot),
            di::G1000_PFD_Copilot => DeviceID::G1000Pfd(TwoSideDevice::Copilot),
            di::G1000_MFD => DeviceID::G1000Mfd,
            di::CDU815_Pilot => DeviceID::CDU815(TwoSideDevice::Pilot),
            di::CDU815_Copilot => DeviceID::CDU815(TwoSideDevice::Copilot),
            di::Primus_PFD_Pilot => DeviceID::PrimusPfd(TwoSideDevice::Pilot),
            di::Primus_PFD_Copilot => DeviceID::PrimusPfd(TwoSideDevice::Copilot),
            di::Primus_MFD_Pilot => DeviceID::PrimusMfd(ThreeSideDevice::Pilot),
            di::Primus_MFD_Copilot => DeviceID::PrimusMfd(ThreeSideDevice::Copilot),
            di::Primus_MFD_Center => DeviceID::PrimusMfd(ThreeSideDevice::Center),
            di::Primus_RMU_Pilot => DeviceID::PrimusRmu(TwoSideDevice::Pilot),
            di::Primus_RMU_Copilot => DeviceID::PrimusRmu(TwoSideDevice::Copilot),
            _ => return Err(DeviceUnmatchedError { id: value }),
        })
    }
}

impl From<DeviceID> for XPLMDeviceID {
    fn from(val: DeviceID) -> Self {
        use xplane_sys::XPLMDeviceID as di;
        match val {
            DeviceID::GNS430(side) => match side {
                TwoSideDevice::Pilot => di::GNS430_Pilot,
                TwoSideDevice::Copilot => di::GNS430_Copilot,
            },
            DeviceID::GNS530(side) => match side {
                TwoSideDevice::Pilot => di::GNS530_Pilot,
                TwoSideDevice::Copilot => di::GNS530_Copilot,
            },
            DeviceID::CDU739(side) => match side {
                TwoSideDevice::Pilot => di::CDU739_Pilot,
                TwoSideDevice::Copilot => di::CDU739_Copilot,
            },
            DeviceID::G1000Pfd(side) => match side {
                TwoSideDevice::Pilot => di::G1000_PFD_Pilot,
                TwoSideDevice::Copilot => di::G1000_PFD_Copilot,
            },
            DeviceID::G1000Mfd => XPLMDeviceID::G1000_MFD,
            DeviceID::CDU815(side) => match side {
                TwoSideDevice::Pilot => di::CDU815_Pilot,
                TwoSideDevice::Copilot => di::CDU815_Copilot,
            },
            DeviceID::PrimusPfd(side) => match side {
                TwoSideDevice::Pilot => di::Primus_PFD_Pilot,
                TwoSideDevice::Copilot => di::Primus_PFD_Copilot,
            },
            DeviceID::PrimusMfd(side) => match side {
                ThreeSideDevice::Pilot => di::Primus_MFD_Pilot,
                ThreeSideDevice::Copilot => di::Primus_MFD_Copilot,
                ThreeSideDevice::Center => di::Primus_MFD_Center,
            },
            DeviceID::PrimusRmu(side) => match side {
                TwoSideDevice::Pilot => di::Primus_RMU_Pilot,
                TwoSideDevice::Copilot => di::Primus_RMU_Copilot,
            },
        }
    }
}

/// Returned from avionics callbacks.
/// Instructs X-Plane what to do next.
pub enum AvionicsCallbackResult {
    /// Allow X-Plane to do its own drawing.
    AllowDraw,
    /// Suppress further drawing of this device.
    SuppressDraw,
}

impl From<AvionicsCallbackResult> for c_int {
    fn from(val: AvionicsCallbackResult) -> Self {
        match val {
            AvionicsCallbackResult::AllowDraw => 0,
            AvionicsCallbackResult::SuppressDraw => 1,
        }
    }
}

/// Handlers for avionics drawing.
/// Store some data in here if you like.
pub trait AvionicsDrawer: 'static {
    /// Draw the avionics before X-Plane.
    /// All OpenGL calls, and XPLM calls related to OpenGL must
    /// be performed unsafely. Remain aware of the restrictions
    /// on OpenGL use.
    fn draw_before_xp(
        &mut self,
        device_id: Result<DeviceID, DeviceUnmatchedError>,
    ) -> AvionicsCallbackResult;

    /// Draw the avionics after X-Plane.
    /// All OpenGL calls, and XPLM calls related to OpenGL must
    /// be performed unsafely. Remain aware of the restrictions
    /// on OpenGL use.
    fn draw_after_xp(&mut self, device_id: Result<DeviceID, DeviceUnmatchedError>);
}

#[derive(Debug, Snafu)]
#[snafu(display("X-Plane didn't give a handle for the customization. I have no way to know why."))]
/// X-Plane didn't give a handle for the customization.
pub struct AvionicsCustomizationError;

#[derive(Debug)]
/// An avionics customization.
pub struct AvionicsCustomization {
    data: *mut AvionicsCustomizationData,
    _phantom: NoSendSync,
}

impl AvionicsCustomization {
    fn try_new(
        device_id: DeviceID,
        drawer: impl AvionicsDrawer,
    ) -> Result<Self, AvionicsCustomizationError> {
        let drawer = Box::into_raw(Box::new(drawer));
        let data = Box::into_raw(Box::new(AvionicsCustomizationData {
            handle: None,
            drawer,
            _phantom: PhantomData,
        }));
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let mut customize_avionics_struct = XPLMCustomizeAvionics_t {
            structSize: mem::size_of::<XPLMCustomizeAvionics_t>() as i32,
            deviceId: device_id.into(),
            drawCallbackBefore: Some(avionics_draw_callback),
            drawCallbackAfter: Some(avionics_draw_callback),
            refcon: data.cast::<c_void>(),
        };
        let handle = unsafe { XPLMRegisterAvionicsCallbacksEx(&mut customize_avionics_struct) };
        if handle.is_null() {
            let _ = unsafe { Box::from_raw(data) };
            Err(AvionicsCustomizationError)
        } else {
            unsafe {
                (*data).handle = Some(handle);
            }
            Ok(Self {
                data,
                _phantom: PhantomData,
            })
        }
    }
}

impl Drop for AvionicsCustomization {
    fn drop(&mut self) {
        let _ = unsafe { Box::from_raw(self.data) };
    }
}

struct AvionicsCustomizationData {
    handle: Option<XPLMAvionicsID>,
    drawer: *mut dyn AvionicsDrawer,
    _phantom: NoSendSync,
}

#[allow(clippy::missing_fields_in_debug)]
impl fmt::Debug for AvionicsCustomizationData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AvionicsCustomizationData")
            .field("handle", &self.handle)
            .field("cb_before", &"[before-draw callback]")
            .field("cb_after", &"[after-draw callback]")
            .finish()
    }
}

impl AvionicsCustomizationData {}

impl Drop for AvionicsCustomizationData {
    fn drop(&mut self) {
        unsafe {
            if let Some(handle) = self.handle {
                XPLMUnregisterAvionicsCallbacks(handle);
            }
            let _ = Box::from_raw(self.drawer);
        }
    }
}

unsafe extern "C-unwind" fn avionics_draw_callback(
    device_id: XPLMDeviceID,
    is_before: c_int,
    refcon: *mut c_void,
) -> c_int {
    let cb_data = refcon.cast::<AvionicsCustomizationData>();
    let drawer = unsafe { cb_data.as_mut().unwrap().drawer.as_mut().unwrap() };
    let device_id = DeviceID::try_from(device_id);
    if is_before == 1 {
        drawer.draw_before_xp(device_id).into()
    } else {
        drawer.draw_after_xp(device_id);
        0
    }
}

/// Access struct for X-Plane's avionics API.
pub struct AvionicsApi {
    pub(crate) _phantom: NoSendSync,
}

impl AvionicsApi {
    /// Try to make a new [`AvionicsCustomization`].
    /// # Errors
    /// Returns an error if X-Plane doesn't give a handle upon creation.
    /// There is no way for this crate to know *why* that happened, only that it did.
    pub fn try_new_customization(
        &mut self,
        device_id: DeviceID,
        drawer: impl AvionicsDrawer,
    ) -> Result<AvionicsCustomization, AvionicsCustomizationError> {
        AvionicsCustomization::try_new(device_id, drawer)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        ptr::{self, NonNull},
        rc::Rc,
    };

    use super::*;
    use crate::make_x;
    use mockall::*;
    #[test]
    fn test_avionics_customization() {
        struct Drawer {
            state: u8,
        }
        impl AvionicsDrawer for Drawer {
            fn draw_before_xp(
                &mut self,
                device_id: Result<DeviceID, DeviceUnmatchedError>,
            ) -> AvionicsCallbackResult {
                assert!(matches!(
                    device_id,
                    Ok(DeviceID::PrimusMfd(ThreeSideDevice::Center))
                ));
                self.state = 10;
                AvionicsCallbackResult::SuppressDraw
            }

            fn draw_after_xp(&mut self, device_id: Result<DeviceID, DeviceUnmatchedError>) {
                assert!(matches!(
                    device_id,
                    Ok(DeviceID::PrimusMfd(ThreeSideDevice::Center))
                ));
                self.state = 5;
            }
        }
        let mut x = make_x();
        let mut seq = Sequence::new();
        let refcon_cell = Rc::new(RefCell::new(ptr::null_mut::<c_void>()));
        let refcon_cell_cloned = refcon_cell.clone();
        let customize_avionics_ctx = xplane_sys::XPLMRegisterAvionicsCallbacksEx_context();
        customize_avionics_ctx
            .expect()
            .withf(|s| {
                let s = unsafe { **s };
                s.deviceId == XPLMDeviceID::Primus_MFD_Center // All I can really check here.
            })
            .once()
            .in_sequence(&mut seq)
            .return_once_st(move |s| {
                unsafe {
                    let s = *s;
                    *refcon_cell_cloned.borrow_mut() = s.refcon;
                }
                NonNull::dangling().as_ptr()
            }); // Pointer meaningless.
        let unregister_customize_avionics_ctx =
            xplane_sys::XPLMUnregisterAvionicsCallbacks_context();
        unregister_customize_avionics_ctx
            .expect()
            .once()
            .in_sequence(&mut seq)
            .return_once_st(|_| ());
        let customization = x
            .avionics
            .try_new_customization(
                DeviceID::PrimusMfd(ThreeSideDevice::Center),
                Drawer { state: 0 },
            )
            .expect("Could not customize avionics!");
        customize_avionics_ctx.checkpoint();
        unsafe {
            assert_eq!(
                avionics_draw_callback(XPLMDeviceID::Primus_MFD_Center, 1, *refcon_cell.borrow()),
                1
            );
            assert_eq!(
                avionics_draw_callback(XPLMDeviceID::Primus_MFD_Center, 0, *refcon_cell.borrow()),
                0
            );
        }
        drop(customization);
        unregister_customize_avionics_ctx.checkpoint();
    }
}
