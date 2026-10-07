// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::{
    ffi::{CStr, CString, NulError},
    fmt,
    marker::PhantomData,
};

use std::ffi::{c_char, c_void};

use xplane_sys;

use crate::NoSendSync;

/// A feature provided by the SDK that this plugin is running in
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Feature {
    /// The name of this feature
    /// Invariant: this can be successfully converted into a CString
    name: String,
    _phantom: NoSendSync,
}

/// Access struct for the Feature API.
pub struct FeatureApi {
    pub(crate) _phantom: NoSendSync, // Make this !Send + !Sync
}

impl Feature {
    /// Paradoxically, when this is enabled, X-Plane will use Unix-style paths.
    /// On Windows, the drive letter will be retained, but backslashes will be converted to slashes.
    ///
    /// # Note
    /// This feature should be enabled automatically by this library.
    pub const USE_NATIVE_PATHS: &'static str = "XPLM_USE_NATIVE_PATHS";
    /// When this is enabled, the X-Plane widgets library will use new, modern, X-Plane backed `XPLMDisplay`
    /// windows to anchor all widget trees. Without it, widgets will always use legacy windows.
    ///
    /// You probably want this enabled. Make sure your widget code can handle the UI coordinate system
    /// not being the same as the OpenGL window coordinate system.
    pub const USE_NATIVE_WIDGET_WINDOWS: &'static str = "XPLM_USE_NATIVE_WIDGET_WINDOWS";
    /// When enabled, X-Plane will send a message any time new datarefs are added.
    ///
    /// XPLM will combine consecutive dataref registrations to minimize the number of messages sent.
    pub const WANTS_DATAREF_NOTIFICATIONS: &'static str = "XPLM_WANTS_DATAREF_NOTIFICATIONS";
    /// Returns the name of this feature
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns true if this feature is currently enabled
    /// # Panics
    /// This function can in theory panic if the `Feature` name is not a valid `CString`. This should not be possible.
    #[must_use]
    pub fn enabled(&self) -> bool {
        let name_c = CString::new(&*self.name).unwrap();
        let enabled = unsafe { xplane_sys::XPLMIsFeatureEnabled(name_c.as_ptr()) };
        enabled == 1
    }

    /// Enables or disables this feature
    /// # Panics
    /// This function can in theory panic if the `Feature` name is not a valid `CString`. This should not be possible.
    pub fn set_enabled(&self, enable: bool) {
        // Because this name was either copied from C with XPLMEnumerateFeatures or
        // checked with XPLMHasFeature, it must be valid as a C string.
        let name_c = CString::new(&*self.name).unwrap();
        unsafe { xplane_sys::XPLMEnableFeature(name_c.as_ptr(), i32::from(enable)) }
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl FeatureApi {
    /// Looks for a feature with the provided name and returns it if it exists
    /// # Errors
    /// This function will return an error if `name` contains a NUL byte.
    /// # Panics
    /// This function can panic if `name`, which came in as a String-like, then was turned into a `CString`,
    /// cannot be turned back into a `String`. This should not be possible.
    pub fn find<S: Into<String>>(&mut self, name: S) -> Result<Option<Feature>, NulError> {
        let name = CString::new(name.into())?;
        let has_feature = unsafe { xplane_sys::XPLMHasFeature(name.as_ptr()) };
        if has_feature == 1 {
            // Convert name back into a String
            // Because the string was not modified, conversion should always work.
            Ok(Some(Feature {
                name: name.into_string().unwrap(),
                _phantom: PhantomData,
            }))
        } else {
            Ok(None)
        }
    }

    /// Returns all features supported by the X-Plane plugin SDK
    pub fn all(&mut self) -> Vec<Feature> {
        let mut features = Vec::new();
        let features_ptr: *mut _ = &mut features;
        unsafe {
            xplane_sys::XPLMEnumerateFeatures(
                Some(feature_callback),
                features_ptr.cast::<c_void>(),
            );
        }
        features
    }
}

/// Interprets refcon as a pointer to a Vec<Feature>.
/// Allocates a new Feature and adds it to the vector
unsafe extern "C-unwind" fn feature_callback(feature: *const c_char, refcon: *mut c_void) {
    let features = refcon.cast::<Vec<Feature>>();

    let name = unsafe { CStr::from_ptr(feature) };
    if let Ok(name) = name.to_str() {
        let new_feature = Feature {
            name: name.to_owned(),
            _phantom: PhantomData,
        };
        unsafe {
            (*features).push(new_feature);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::make_x;

    use super::*;
    #[test]
    #[allow(clippy::too_many_lines)]
    fn test_features() {
        // Part miri food, part unit test.
        let enumerate_features_ctx = xplane_sys::XPLMEnumerateFeatures_context();
        enumerate_features_ctx
            .expect()
            .once()
            .return_once_st(|cb, refcon| {
                let cb = cb.unwrap(); // This should be Some.
                let paths_feature = CString::new("XPLM_USE_NATIVE_PATHS").unwrap(); // We know that this is a valid C-string.
                let other_feature = CString::new("XPLM_SOME_OTHER_FEATURE").unwrap(); // We know that this is a valid C-string.
                unsafe {
                    cb(paths_feature.as_ptr(), refcon);
                    cb(other_feature.as_ptr(), refcon);
                }
            });
        let has_feature_ctx = xplane_sys::XPLMHasFeature_context();
        has_feature_ctx.expect().once().return_once_st(|feat| {
            let feat = unsafe { CStr::from_ptr(feat) };
            let feat = feat.to_str().unwrap(); // This should be valid UTF-8.
            assert_eq!(feat, "XPLM_ANOTHER_FEATURE");
            1
        });
        let feature_enabled_ctx = xplane_sys::XPLMIsFeatureEnabled_context();
        feature_enabled_ctx.expect().once().return_once_st(|feat| {
            let feat = unsafe { CStr::from_ptr(feat) };
            let feat = feat.to_str().unwrap(); // This should be valid UTF-8.
            assert_eq!(feat, "XPLM_ANOTHER_FEATURE");
            0
        });
        let enable_feature_ctx = xplane_sys::XPLMEnableFeature_context();
        enable_feature_ctx
            .expect()
            .once()
            .return_once_st(|feat, enable| {
                let feat = unsafe { CStr::from_ptr(feat) };
                let feat = feat.to_str().unwrap(); // This should be valid UTF-8.
                assert_eq!(feat, "XPLM_ANOTHER_FEATURE");
                assert_eq!(enable, 1);
            });
        let mut x = make_x();
        let feats: Vec<String> = x
            .features
            .all()
            .iter()
            .map(|feat| feat.name.clone())
            .collect();
        assert_eq!(
            feats,
            vec!["XPLM_USE_NATIVE_PATHS", "XPLM_SOME_OTHER_FEATURE"]
        );
        let feat = x.features.find("XPLM_ANOTHER_FEATURE").unwrap().unwrap();
        assert!(!feat.enabled());
        feat.set_enabled(true);
    }
}
