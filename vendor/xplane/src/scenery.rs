// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::{
    ffi::{c_char, c_void, CStr, CString, NulError},
    marker::PhantomData,
    path::{Path, PathBuf},
};

use snafu::prelude::*;
use xplane_sys::{
    XPLMCreateInstance, XPLMCreateProbe, XPLMDestroyProbe, XPLMLoadObject, XPLMLoadObjectAsync,
    XPLMLookupObjects, XPLMObjectRef, XPLMProbeInfo_t, XPLMProbeRef, XPLMProbeResult,
    XPLMProbeTerrainXYZ, XPLMProbeType, XPLMReloadScenery, XPLMUnloadObject,
};

#[cfg(feature = "XPLM300")]
use xplane_sys::{XPLMDegMagneticToDegTrue, XPLMDegTrueToDegMagnetic, XPLMGetMagneticVariation};

#[cfg(feature = "XPLM303")]
use crate::obj_instance::Instance;

use crate::NoSendSync;

/// A probe for terrain. Keep it around in whatever will be probing.
/// See [the X-Plane documentation](https://developer.x-plane.com/sdk/XPLMScenery/#Performance_Guidelines)
/// for more information on proper use.
pub struct TerrainProbe {
    handle: XPLMProbeRef,
    _typ: XPLMProbeType,
    _phantom: NoSendSync,
}

impl TerrainProbe {
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    /// Probe the terrain at the given coordinates.
    /// All coordinates are OpenGL local coordinates.
    ///
    /// # Errors
    ///
    /// If [`XPLMProbeTerrainXYZ`] says that an error has occured, an error will be returned.
    /// I have no way of telling you what went wrong.
    ///
    /// # Panics
    /// This function will panic if [`XPLMProbeTerrainXYZ`] returns an invalid result.
    /// This shouldn't be possible.
    pub fn probe_terrain(
        &mut self,
        x: f32,
        y: f32,
        z: f32,
    ) -> Result<Option<XPLMProbeInfo_t>, ProbeError> {
        let mut probe_info = XPLMProbeInfo_t {
            structSize: std::mem::size_of::<XPLMProbeInfo_t>() as i32,
            locationX: 0.0,
            locationY: 0.0,
            locationZ: 0.0,
            normalX: 0.0,
            normalY: 0.0,
            normalZ: 0.0,
            velocityX: 0.0,
            velocityY: 0.0,
            velocityZ: 0.0,
            is_wet: 0,
        };
        match unsafe { XPLMProbeTerrainXYZ(self.handle, x, y, z, &mut probe_info) } {
            XPLMProbeResult::Missed => Ok(None),
            XPLMProbeResult::HitTerrain => Ok(Some(probe_info)),
            XPLMProbeResult::Error => Err(ProbeError),
            _ => panic!("XPLMProbeTerrainXYZ has returned an invalid result!"),
        }
    }
}

impl Drop for TerrainProbe {
    fn drop(&mut self) {
        unsafe {
            XPLMDestroyProbe(self.handle);
        }
    }
}

#[derive(Debug, Snafu)]
#[snafu(display("The terrain probe did not succeed."))]
/// The terrain probe did not succeed. Either the probe struct size is bad (should not be possible),
/// the probe is invalid, or the type is mismatched for the specific query call.
pub struct ProbeError;

/// An X-Plane OBJ file handle.
pub struct XObject {
    handle: XPLMObjectRef,
    path: PathBuf,
    _phantom: NoSendSync,
}

impl XObject {
    #[must_use]
    /// Get the path to this object, relative to the X-System root.
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    /// Try to clone this object.
    /// If X-Plane decides for whatever reason it doesn't want to load another
    /// copy of this object, this will return [`None`].
    pub fn try_clone(&self) -> Option<Self> {
        let path_c =
            std::ffi::CString::new(self.path.as_os_str().to_string_lossy().into_owned()).ok()?;
        let handle = unsafe { XPLMLoadObject(path_c.as_ptr()) };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                path: self.path.clone(),
                _phantom: PhantomData,
            })
        }
    }

    #[cfg(feature = "XPLM303")]
    /// Make a new instance of this object.
    /// # Errors
    /// Returns an error if any of the dataref names contain a NUL byte.
    /// <div class="warning"> All datarefs used by this instance must be registered before the object is
    /// even _loaded._ Failure to have them registered beforehand is undefined behavior.</div>
    pub fn new_instance<const NUM_DATAREFS: usize, S: Into<Vec<u8>>>(
        self,
        datarefs: [S; NUM_DATAREFS],
    ) -> Result<Instance<NUM_DATAREFS>, NulError> {
        let datarefs = datarefs
            .into_iter()
            .map(|s| CString::new(s))
            .collect::<Result<Vec<_>, _>>()?;
        let mut dr_ptrs: Vec<_> = datarefs
            .iter()
            .map(|dr| dr.as_ptr())
            .chain([std::ptr::null()])
            .collect();
        let handle = unsafe { XPLMCreateInstance(self.handle, dr_ptrs.as_mut_ptr()) };
        Ok(Instance {
            handle,
            _phantom: PhantomData,
        })
    }
}

impl Drop for XObject {
    fn drop(&mut self) {
        unsafe {
            XPLMUnloadObject(self.handle);
        }
    }
}

struct ObjectLoadContext<C>
where
    C: FnOnce(Option<XObject>),
{
    obj_path: PathBuf,
    callback: C,
}

/// Access struct for X-Plane's scenery APIs.
pub struct SceneryApi {
    pub(crate) _phantom: NoSendSync,
}

impl SceneryApi {
    /// Create a new terrain probe.
    pub fn new_terrain_probe(&mut self, typ: XPLMProbeType) -> TerrainProbe {
        let handle = unsafe { XPLMCreateProbe(typ) };
        TerrainProbe {
            handle,
            _typ: typ,
            _phantom: PhantomData,
        }
    }

    #[cfg(feature = "XPLM300")]
    /// Returns X-Plane’s simulated magnetic variation (declination) at the passed latitude and longitude.
    pub fn get_magnetic_variation(&mut self, lat: f64, lon: f64) -> f32 {
        unsafe { XPLMGetMagneticVariation(lat, lon) }
    }

    #[cfg(feature = "XPLM300")]
    /// Converts a heading in degrees relative to true north at the user's current location
    /// into a heading relative to magnetic north.
    pub fn deg_true_to_mag(&mut self, deg: f32) -> f32 {
        unsafe { XPLMDegTrueToDegMagnetic(deg) }
    }

    #[cfg(feature = "XPLM300")]
    /// Converts a heading in degrees relative to magnetic north at the user's current location
    /// into a heading relative to true north.
    pub fn deg_mag_to_true(&mut self, deg: f32) -> f32 {
        unsafe { XPLMDegMagneticToDegTrue(deg) }
    }

    /// Lookup a virtual path in X-Plane's library system, and return all matching objects.
    /// `lat` and `lon` indicate the location the object will be used -- location-specific objects
    /// will be returned. All paths will be relative to the X-System folder.
    /// # Errors
    /// An error will be returned if the virtual path contains a NUL byte.
    /// # Panics
    /// This function will panic if X-Plane provides invalid UTF-8. This should not be possible.
    pub fn lookup_objects<P: AsRef<Path>>(
        &mut self,
        path: P,
        lat: f32,
        lon: f32,
    ) -> Result<Vec<PathBuf>, NulError> {
        let mut objects = Vec::new();
        let objects_ptr: *mut _ = &mut objects;
        let objects_ptr: *mut c_void = objects_ptr.cast();
        let path_c =
            std::ffi::CString::new(path.as_ref().as_os_str().to_string_lossy().into_owned())?;

        unsafe {
            XPLMLookupObjects(
                path_c.as_ptr(),
                lat,
                lon,
                Some(library_enumerator),
                objects_ptr,
            );
        }

        Ok(objects)
    }

    /// Load an object synchronously.
    /// The path provided should be relative to the X-System root.
    /// If the object could not be loaded (reason unknown), [`Ok(None)`] will be returned.
    /// # Errors
    /// An error will be returned if the path contains a NUL byte.
    pub fn load_object(&mut self, path: PathBuf) -> Result<Option<XObject>, NulError> {
        let path_c = std::ffi::CString::new(path.as_os_str().to_string_lossy().into_owned())?;
        let handle = unsafe { XPLMLoadObject(path_c.as_ptr()) };
        if handle.is_null() {
            Ok(None)
        } else {
            Ok(Some(XObject {
                handle,
                path,
                _phantom: PhantomData,
            }))
        }
    }

    /// Load an object asynchronously.
    /// The path provided should be relative to the X-System root.
    /// If the object could not be loaded (reason unknown), [`Ok(None)`] will be returned.
    ///
    /// I can't think of a better way to handle the callback, so I recommend making
    /// an `Rc<Cell<Option<XObject>>>`, or something of the like, to get the [`XObject`]
    /// out of the callback.
    /// # Errors
    /// An error will be returned if the path contains a NUL byte.
    pub fn load_object_async<C>(&mut self, path: PathBuf, callback: C) -> Result<(), NulError>
    where
        C: FnOnce(Option<XObject>),
    {
        let path_c = std::ffi::CString::new(path.as_os_str().to_string_lossy().into_owned())?;

        let ctx = Box::into_raw(Box::new(ObjectLoadContext {
            obj_path: path,
            callback,
        }));

        unsafe {
            XPLMLoadObjectAsync(
                path_c.as_ptr(),
                Some(object_loaded_callback::<C>),
                ctx.cast::<c_void>(),
            );
        }
        Ok(())
    }

    /// Reload the current set of scenery.
    ///
    /// This will only cause X-Plane to re-read already loaded scenery, not load new scenery.
    /// Equivalent to pressing "reload scenery" in the developer menu.
    pub fn reload_scenery(&mut self) {
        unsafe {
            XPLMReloadScenery();
        }
    }
}

unsafe extern "C-unwind" fn library_enumerator(file_path: *const c_char, refcon: *mut c_void) {
    let out = unsafe {
        refcon.cast::<Vec<PathBuf>>().as_mut().unwrap() // UNWRAP: This pointer will never be null.
    };
    let file_path = unsafe { CStr::from_ptr(file_path) };
    let file_path = file_path.to_owned();
    let file_path = file_path.into_string().unwrap(); // UNWRAP: X-Plane promises to give good UTF-8.
    let file_path = PathBuf::from(file_path);
    out.push(file_path);
}

unsafe extern "C-unwind" fn object_loaded_callback<C>(obj: XPLMObjectRef, refcon: *mut c_void)
where
    C: FnOnce(Option<XObject>),
{
    let ctx = unsafe { Box::from_raw(refcon.cast::<ObjectLoadContext<C>>()) };
    let obj = if obj.is_null() {
        None
    } else {
        Some(XObject {
            handle: obj,
            path: ctx.obj_path,
            _phantom: PhantomData,
        })
    };
    (ctx.callback)(obj);
}
