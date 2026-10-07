// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::ffi::{CString, NulError};
use std::mem::{self, MaybeUninit};

use xplane_sys::{
    XPLMFixedString150_t, XPLMGetMETARForAirport, XPLMGetWeatherAtLocation, XPLMWeatherInfo_t,
};

use crate::NoSendSync;

/// Struct to access weather APIs.
pub struct WeatherApi {
    pub(crate) _phantom: NoSendSync,
}

impl WeatherApi {
    /// Get the METAR for the given aerodrome.
    /// # Errors
    /// Returns an error if the aerodrome ID contains a null byte, or if the METAR returned
    /// by X-Plane is not valid UTF-8. X-Plane should be giving UTF-8, per the developers.
    /// # Panics
    /// Panics if X-Plane provides invalid UTF-8. This should be impossible.
    #[allow(clippy::cast_sign_loss)]
    pub fn get_aerodrome_metar<S: Into<Vec<u8>>>(ad: S) -> Result<String, NulError> {
        let ad = CString::new(ad)?;
        let mut get_metar_out = XPLMFixedString150_t { buffer: [0i8; 150] };
        unsafe {
            XPLMGetMETARForAirport(ad.as_ptr(), &mut get_metar_out);
        };
        let buffer =
            Vec::from(unsafe { mem::transmute::<[i8; 150], [u8; 150]>(get_metar_out.buffer) });
        Ok(String::from_utf8(buffer).unwrap())
    }

    #[must_use]
    /// Get the weather at the given location.
    /// The location must be near the user.
    /// Weather may not be available at the location, in which case [`None`] will be returned.
    pub fn get_weather_at_location(lat: f64, lon: f64, alt_m: f64) -> Option<XPLMWeatherInfo_t> {
        let mut weather: MaybeUninit<XPLMWeatherInfo_t> = MaybeUninit::zeroed();
        if unsafe { XPLMGetWeatherAtLocation(lat, lon, alt_m, weather.as_mut_ptr()) } == 1 {
            unsafe { Some(weather.assume_init()) }
        } else {
            None
        }
    }
}
