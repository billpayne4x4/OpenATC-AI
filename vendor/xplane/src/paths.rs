// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::path::PathBuf;

use crate::{feature::Feature, ffi::StringBuffer, NoSendSync, XPAPI};
use xplane_sys::{XPLMGetNthAircraftModel, XPLMGetPrefsPath, XPLMGetSystemPath};

/// Struct to access X-Plane's path API.
pub struct PathApi {
    pub(crate) _phantom: NoSendSync,
}

impl PathApi {
    /// Get the folder that X-Plane lives in.
    /// # Panics
    /// Panics if X-Plane provides invalid UTF-8. This should be impossible.
    pub fn xplane_folder(&mut self) -> PathBuf {
        const BUFF_LEN: usize = 1024;
        let mut buffer = StringBuffer::new(BUFF_LEN);

        unsafe {
            XPLMGetSystemPath(buffer.as_mut_ptr());
        }

        let value_string = buffer.into_string().unwrap();

        value_string.into()
    }

    /// Get the X-Plane plugin folder.
    pub fn plugins_folder(&mut self) -> PathBuf {
        self.xplane_folder().join("Resources").join("plugins")
    }

    /// Get the path to the loaded aircraft.
    /// # Panics
    /// Panics if X-Plane provides invalid UTF-8. This should be impossible.
    pub fn acf_path(&mut self, acf_id: i32) -> PathBuf {
        // https://developer.x-plane.com/sdk/XPLMGetNthAircraftModel/

        let mut filename = StringBuffer::new(257);
        let mut path = StringBuffer::new(513);

        unsafe {
            XPLMGetNthAircraftModel(acf_id, filename.as_mut_ptr(), path.as_mut_ptr());
        }

        let path = path.into_string().unwrap();
        PathBuf::from(path)
    }

    /// Get the path to the preferences folder.
    /// # Panics
    /// Panics if X-Plane provides invalid UTF-8, or if there is not a parent
    /// to the file in the preferences folder it gives. Both cases should be impossible.
    pub fn prefs_folder(&mut self) -> PathBuf {
        let mut folder = StringBuffer::new(513);
        unsafe {
            XPLMGetPrefsPath(folder.as_mut_ptr());
        }
        let folder = folder.into_string().unwrap(); // Unwrap: this should always be valid UTF-8.
        PathBuf::from(folder).parent().unwrap().to_owned() // Unwrap: This should always return Some.
    }
}

/// Enables native paths
pub(crate) fn path_init(x: &mut XPAPI) {
    // Feature specified to exist in SDK 2.1
    let native_path_feature = x
        .features
        .find(Feature::USE_NATIVE_PATHS)
        .unwrap() // Unwrap: We know that there are no NUL bytes here.
        .expect("No native paths feature"); // This should always exist within a simulator new enough to support this library.
    native_path_feature.set_enabled(true);
}
