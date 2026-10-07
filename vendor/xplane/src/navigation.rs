// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::{marker::PhantomData, ops::Range, ptr, rc::Rc};

use snafu::prelude::*;
use xplane_sys::{
    XPLMClearFMSEntry, XPLMCountFMSEntries, XPLMFindFirstNavAidOfType, XPLMFindLastNavAidOfType,
    XPLMGetDestinationFMSEntry, XPLMGetDisplayedFMSEntry, XPLMGetFMSEntryInfo, XPLMGetFirstNavAid,
    XPLMGetGPSDestination, XPLMGetNavAidInfo, XPLMGetNextNavAid, XPLMNavRef, XPLMNavType,
    XPLMSetFMSEntryInfo, XPLMSetFMSEntryLatLon, XPLM_NAV_NOT_FOUND,
};

use crate::{ffi::StringBuffer, NoSendSync};

#[derive(Debug, Clone)]
/// A navaid. Clone as you desire.
pub struct NavAid {
    handle: XPLMNavRef,
    typ: XPLMNavType,
    lat: f32,
    lon: f32,
    height: f32,
    frequency: i32,
    hdg: f32,
    id: Rc<str>,
    name: Rc<str>,
    _phantom: NoSendSync,
}

impl NavAid {
    fn from_handle(handle: XPLMNavRef) -> Self {
        let mut typ = XPLMNavType::Unknown;
        let (mut lat, mut lon, mut height, mut hdg) = (0.0, 0.0, 0.0, 0.0);
        let mut frequency = 0i32;
        let mut id_buf = StringBuffer::new(33);
        let mut name_buf = StringBuffer::new(257);
        unsafe {
            XPLMGetNavAidInfo(
                handle,
                &mut typ,
                &mut lat,
                &mut lon,
                &mut height,
                &mut frequency,
                &mut hdg,
                id_buf.as_mut_ptr(),
                name_buf.as_mut_ptr(),
                ptr::null_mut(),
            );
        }
        Self {
            handle,
            typ,
            lat,
            lon,
            height,
            frequency,
            hdg,
            id: id_buf.into_string().unwrap().into(), // UNWRAP: X-Plane *should* give us good UTF-8.
            name: name_buf.into_string().unwrap().into(), // UNWRAP: X-Plane *should* give us good UTF-8.
            _phantom: PhantomData,
        }
    }

    #[must_use]
    /// Get the [`XPLMNavType`] of this navaid.
    pub fn typ(&self) -> XPLMNavType {
        self.typ
    }

    #[must_use]
    /// Get the latitude of this navaid.
    pub fn lat(&self) -> f32 {
        self.lat
    }

    #[must_use]
    /// Get the longitude of this navaid.
    pub fn lon(&self) -> f32 {
        self.lon
    }

    #[must_use]
    /// Get the height of this navaid (presumed m).
    pub fn height(&self) -> f32 {
        self.height
    }

    #[must_use]
    /// Get the frequency of this navaid.
    /// NDB frequencies are exact, all others are multiplied (divided?) by 100.
    pub fn frequency(&self) -> i32 {
        self.frequency
    }

    #[must_use]
    /// Get the heading of this navaid.
    pub fn hdg(&self) -> f32 {
        self.hdg
    }

    #[must_use]
    /// Get the ID of this navaid.
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    /// Get the name of this navaid.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// An [`Iterator`] over the [`NavAid`]s in X-Plane's database.
pub struct NavAidIter {
    last_handle: XPLMNavRef,
    stop_at: XPLMNavRef,
    _phantom: NoSendSync,
}

impl Iterator for NavAidIter {
    type Item = NavAid;

    fn next(&mut self) -> Option<Self::Item> {
        if self.last_handle == XPLM_NAV_NOT_FOUND || self.last_handle == self.stop_at {
            None
        } else {
            let handle = self.last_handle;
            self.last_handle = unsafe { XPLMGetNextNavAid(handle) };
            Some(NavAid::from_handle(handle))
        }
    }
}

#[derive(Debug, Snafu)]
#[snafu(display("The nav type {:#b} has more than one bit flag set. This is not allowable for iteration.", typ.0))]
/// Returned from [`NavApi::iter_navaids`] if the given navaid type has more than one bit flag set.
pub struct BadNavType {
    typ: XPLMNavType,
}

/// An entry in the [`Fms`].
pub struct FmsEntry<'a> {
    navaid: Option<NavAid>,
    fms_wpt_id: String,
    altitude: i32,
    lat: f32,
    lon: f32,
    _phantom: PhantomData<*mut &'a mut ()>,
}

impl FmsEntry<'_> {
    #[must_use]
    ///
    pub fn navaid(&self) -> &Option<NavAid> {
        &self.navaid
    }

    #[must_use]
    ///
    pub fn fms_wpt_id(&self) -> &str {
        &self.fms_wpt_id
    }

    #[must_use]
    ///
    pub fn altitude(&self) -> i32 {
        self.altitude
    }

    #[must_use]
    ///
    pub fn lat(&self) -> f32 {
        self.lat
    }

    #[must_use]
    ///
    pub fn lon(&self) -> f32 {
        self.lon
    }
}

#[derive(Debug, Snafu)]
#[snafu(display("A bad index has been provided to the FMS: {idx}"))]
/// An out-of-bounds index has been used with an FMS function.
pub struct BadIndex {
    idx: i32,
}

/// The Flight Management System
pub struct Fms {
    pub(crate) _phantom: NoSendSync,
}

impl Fms {
    const IDX_RANGE: Range<i32> = 0..100;

    #[must_use]
    /// Get the number of entries currently in the FMS.
    pub fn num_entries(&mut self) -> i32 {
        unsafe { XPLMCountFMSEntries() }
    }

    #[must_use]
    /// Get the index of the entry that the pilot is currently viewing.
    pub fn get_displayed_idx(&mut self) -> i32 {
        unsafe { XPLMGetDisplayedFMSEntry() }
    }

    #[must_use]
    /// Get the index of the entry the FMS is flying to.
    pub fn get_dest_idx(&mut self) -> i32 {
        unsafe { XPLMGetDestinationFMSEntry() }
    }

    /// Get the FMS entry at the index. The index must be in the range `0..100`.
    /// # Errors
    /// An error will be returned if this function is passed an out-of-bounds index.
    /// # Panics
    /// This function will panic if X-Plane provides invalid UTF-8.
    /// If this happens, you have bigger issues.
    pub fn get(&mut self, idx: i32) -> Result<FmsEntry<'_>, BadIndex> {
        if !Self::IDX_RANGE.contains(&idx) {
            return BadIndexSnafu { idx }.fail();
        }
        let mut typ = XPLMNavType::Unknown;
        let mut id_buf = StringBuffer::new(33);
        let mut handle = XPLM_NAV_NOT_FOUND;
        let mut alt = i32::MIN;
        let mut lat = 0f32;
        let mut lon = 0f32;
        unsafe {
            XPLMGetFMSEntryInfo(
                idx,
                &mut typ,
                id_buf.as_mut_ptr(),
                &mut handle,
                &mut alt,
                &mut lat,
                &mut lon,
            );
        }
        let navaid = if handle == XPLM_NAV_NOT_FOUND {
            None
        } else {
            Some(NavAid::from_handle(handle))
        };
        Ok(FmsEntry {
            navaid,
            fms_wpt_id: id_buf.into_string().unwrap(), // UNWRAP: X-Plane should be giving valid UTF-8.
            altitude: alt,
            lat,
            lon,
            _phantom: PhantomData,
        })
    }

    /// Set the FMS entry at `idx` to correspond to `navaid`.
    /// # Errors
    /// Errors if an out-of-bounds index is passed.
    pub fn set_navaid(&mut self, idx: i32, navaid: &NavAid, alt: i32) -> Result<(), BadIndex> {
        if !Self::IDX_RANGE.contains(&idx) {
            return BadIndexSnafu { idx }.fail();
        }
        unsafe {
            XPLMSetFMSEntryInfo(idx, navaid.handle, alt);
        }
        Ok(())
    }

    /// Set the FMS entry at `idx` to correspond to `lat` and `lon`.
    /// # Errors
    /// Errors if an out-of-bounds index is passed.
    pub fn set_lat_lon(&mut self, idx: i32, lat: f32, lon: f32, alt: i32) -> Result<(), BadIndex> {
        if !Self::IDX_RANGE.contains(&idx) {
            return BadIndexSnafu { idx }.fail();
        }
        unsafe {
            XPLMSetFMSEntryLatLon(idx, lat, lon, alt);
        }
        Ok(())
    }

    /// Removes the FMS entry at `idx`.
    /// # Errors
    /// Errors if an out-of-bounds index is passed.
    pub fn remove_entry(&mut self, idx: i32) -> Result<(), BadIndex> {
        if !Self::IDX_RANGE.contains(&idx) {
            return BadIndexSnafu { idx }.fail();
        }

        unsafe {
            XPLMClearFMSEntry(idx);
        }

        Ok(())
    }
}

/// Access functions for X-Plane's navigation API.
pub struct NavApi {
    /// The Flight Management System
    pub fms: Fms,
    pub(crate) _phantom: NoSendSync,
}

impl NavApi {
    /// Iterate over all navaids of the given type, or all navaids of all types if
    /// `typ` is [`None`].
    /// # Errors
    /// Returns an error if `typ` has more than one bit flag set.
    pub fn iter_navaids(&mut self, typ: Option<XPLMNavType>) -> Result<NavAidIter, BadNavType> {
        if let Some(typ) = typ {
            if typ.0.count_ones() == 1 {
                let start = unsafe { XPLMFindFirstNavAidOfType(typ) };
                let end = unsafe { XPLMFindLastNavAidOfType(typ) };
                Ok(NavAidIter {
                    last_handle: start,
                    stop_at: end,
                    _phantom: PhantomData,
                })
            } else {
                BadNavTypeSnafu { typ }.fail()
            }
        } else {
            let first_handle = unsafe { XPLMGetFirstNavAid() };
            Ok(NavAidIter {
                last_handle: first_handle,
                stop_at: XPLM_NAV_NOT_FOUND,
                _phantom: PhantomData,
            })
        }
    }

    /// Get the current destination of the GPS, if any.
    pub fn get_gps_dest(&mut self) -> Option<NavAid> {
        let handle = unsafe { XPLMGetGPSDestination() };
        if handle == XPLM_NAV_NOT_FOUND {
            None
        } else {
            Some(NavAid::from_handle(handle))
        }
    }
}
