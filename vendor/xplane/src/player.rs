// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::{ffi::NulError, marker::PhantomData, path::Path};

use snafu::prelude::*;

use crate::{paths::PathApi, NoSendSync};

#[derive(Snafu, Debug)]
/// Error that can result from trying to set the player's aircraft.
pub enum SetAircraftError {
    /// An absolute path was passed, but it was not a child of the X-Plane system root.
    #[snafu(display("The passed path was absolute, but not a child of the X-Plane system root!"))]
    InvalidAbsolutePath,
    /// The passed path was not an acf file.
    #[snafu(display("The passed path was not an acf file!"))]
    NotAcf,
    /// An I/O error occurred.
    #[snafu(display("An I/O error occurred!"))]
    #[snafu(context(false))]
    IoError {
        /// The source I/O error.
        source: std::io::Error,
    },
    /// A path contained a NUL byte. This really shouldn't happen.
    #[snafu(display("A path contained a NUL byte."))]
    #[snafu(context(false))]
    Nul {
        /// The source error.
        source: NulError,
    },
}

/// Struct to access API functions to manipulate the player's aircraft.
pub struct PlayerApi {
    pub(crate) _phantom: NoSendSync,
}

impl PlayerApi {
    /// Reload the aircraft currently in use.
    /// NOTE: This will place the player on a runway at the nearest airport.
    /// # Errors
    /// This function can theoretically error if there is a NUL byte in the
    /// path to the loaded aircraft. This should not be possible.
    pub fn reload_aircraft(&mut self) -> Result<(), SetAircraftError> {
        // debugln!("xplane_sys: player.rs: reload_aircraft()");

        //get the acf in use..
        let mut path_api = PathApi {
            _phantom: PhantomData,
        };
        let current_acf_path = path_api.acf_path(0);
        self.set_aircraft(current_acf_path)?;
        Ok(())
    }

    /// Set the aircraft being used.
    ///
    /// `file` can be passed either relative or absolute.
    /// # Errors
    /// If `file` is absolute, but is not relative to the X-Plane system root, this function will return an error.
    /// Additionally, if `file` does not exist, an error will be returned.
    /// An error can also be returned if there is a NUL byte in the acf path.
    pub fn set_aircraft<P: AsRef<Path>>(&mut self, file: P) -> Result<(), SetAircraftError> {
        let file = file.as_ref();
        let file = file.canonicalize()?;
        let mut path_api = PathApi {
            _phantom: PhantomData,
        };
        if !file.is_relative() && !file.starts_with(path_api.xplane_folder()) {
            return Err(SetAircraftError::InvalidAbsolutePath);
        }
        if file.extension().ok_or(SetAircraftError::NotAcf)? != "acf" {
            return Err(SetAircraftError::NotAcf);
        }
        // debugln!("xplane_sys: player.rs: reload_aircraft()");
        let filename_c = std::ffi::CString::new(file.as_os_str().to_string_lossy().into_owned())?;
        unsafe {
            xplane_sys::XPLMSetUsersAircraft(filename_c.as_ptr());
        }
        Ok(())
    }

    /// Change location using airport ID code eg: KBOS
    /// # Errors
    /// Will return an error if `airport_code` contains a NUL byte.
    pub fn place_at_airport(&mut self, airport_code: &str) -> Result<(), NulError> {
        let airport_code_c = std::ffi::CString::new(airport_code)?;
        unsafe {
            xplane_sys::XPLMPlaceUserAtAirport(airport_code_c.as_ptr());
        }
        Ok(())
    }
}

// Suggested wrapper for placing player aircraft at arbitrary locations...

// #[allow(dead_code)]
// struct PlayerLocation {
//     latitude_degrees: f64,
//     longitude_degrees: f64,
//     elevation_meters_msl: f32,
//     heading_degrees_true: f32,
//     speed_meters_per_second: f32,
// }

// #[allow(dead_code)]
// impl PlayerLocation {
//     // get the current location as a set of sensible defaults.

//     // pub fn new() -> Self{
//     //     PlayerLocation{
//     //         latitude_degrees: 0.0,
//     //         longitude_degrees: 0.0,
//     //         elevation_meters_msl: 0.0,
//     //         heading_degrees_true: 0.0,
//     //         speed_meters_per_second: 0.0,
//     //     }
//     // }

//     // move incrementally with direction/vector wrappers...

//     // setters
//     // getters

//     // not sure on this fn name..
//     pub fn place_aircraft_at(&self) {
//         debugln!("xplm::player::place_aircraft_at() - UNTESTED");
//         unsafe {
//             xplane_sys::XPLMPlaceUserAtLocation(
//                 self.latitude_degrees,
//                 self.longitude_degrees,
//                 self.elevation_meters_msl,
//                 self.heading_degrees_true,
//                 self.speed_meters_per_second,
//             );
//         }
//     }
// }
