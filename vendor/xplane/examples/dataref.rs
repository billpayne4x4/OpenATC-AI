// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::ffi::NulError;

use xplane::{
    data::{
        borrowed::{DataRef, FindError},
        ArrayRead, DataRead, ReadOnly, ReadWrite, StringRead,
    },
    debugln,
    message::MessageId,
    plugin::{Plugin, PluginInfo},
    xplane_plugin,
};

struct DataRefPlugin {
    has_joystick: DataRef<bool, ReadOnly>,
    earth_mu: DataRef<f32, ReadOnly>,
    date: DataRef<i32, ReadWrite>,
    sim_build_string: DataRef<[u8], ReadOnly>,
    latitude: DataRef<f64, ReadOnly>,
    joystick_axis_values: DataRef<[f32], ReadOnly>,
    battery_on: DataRef<[i32], ReadWrite>,
}

impl DataRefPlugin {
    fn test_datarefs(&mut self, xpapi: &mut xplane::XPAPI) -> Result<(), NulError> {
        debugln!(xpapi, "Has joystick: {}", self.has_joystick.get())?;
        debugln!(xpapi, "Earth mu: {}", self.earth_mu.get())?;
        debugln!(xpapi, "Date: {}", self.date.get())?;
        debugln!(
            xpapi,
            "Simulator build: {}",
            self.sim_build_string
                .get_as_string()
                .unwrap_or(String::from("Unknown"))
        )?;
        debugln!(xpapi, "Latitude: {}", self.latitude.get())?;
        debugln!(
            xpapi,
            "Joystick axis values: {:?}",
            self.joystick_axis_values.as_vec()
        )?;
        debugln!(xpapi, "Battery on: {:?}", self.battery_on.as_vec())?;
        Ok(())
    }
}

impl Plugin for DataRefPlugin {
    type Error = FindError;
    fn start(xpapi: &mut xplane::XPAPI) -> Result<Self, Self::Error> {
        let plugin = DataRefPlugin {
            has_joystick: xpapi.data.find("sim/joystick/has_joystick")?,
            earth_mu: xpapi.data.find("sim/physics/earth_mu")?,
            date: xpapi
                .data
                .find("sim/time/local_date_days")?
                .writeable()
                .expect("Could not make dataref writeable!"),
            sim_build_string: xpapi.data.find("sim/version/sim_build_string")?,
            latitude: xpapi.data.find("sim/flightmodel/position/latitude")?,
            joystick_axis_values: xpapi.data.find("sim/joystick/joystick_axis_values")?,
            battery_on: xpapi
                .data
                .find("sim/cockpit2/electrical/battery_on")?
                .writeable()
                .expect("Could not make dataref writeable!"),
        };
        Ok(plugin)
    }

    fn enable(&mut self, xpapi: &mut xplane::XPAPI) -> Result<(), Self::Error> {
        self.test_datarefs(xpapi).unwrap(); // There should be no NUL bytes in there.
        Ok(())
    }

    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: String::from("Dataref Test"),
            signature: String::from("com.jdemille.xplane.examples.dataref"),
            description: String::from("Tests the DataRef features of xplm"),
        }
    }
    fn receive_message(
        &mut self,
        _xpapi: &mut xplane::XPAPI,
        _from: i32,
        _message: MessageId,
        _param: *mut std::ffi::c_void,
    ) {
    }

    fn disable(&mut self, _xpapi: &mut xplane::XPAPI) {
        todo!()
    }
}

xplane_plugin!(DataRefPlugin);
