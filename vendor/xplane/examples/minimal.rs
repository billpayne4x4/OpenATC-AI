// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use xplane::{
    debugln,
    message::MessageId,
    plugin::{Plugin, PluginInfo},
    xplane_plugin, XPAPI,
};

struct MinimalPlugin;

impl Plugin for MinimalPlugin {
    type Error = std::convert::Infallible;

    fn start(xpapi: &mut XPAPI) -> Result<Self, Self::Error> {
        // The following message should be visible in the developer console and the Log.txt file
        debugln!(xpapi, "Hello, World! From the Minimal Rust Plugin").unwrap(); // No NUL bytes.
        Ok(MinimalPlugin)
    }

    fn enable(&mut self, _xpapi: &mut XPAPI) -> Result<(), Self::Error> {
        Ok(())
    }

    fn disable(&mut self, _xpapi: &mut XPAPI) {}

    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: String::from("Minimal Rust Plugin"),
            signature: String::from("com.jdemille.xplane.examples.minimal"),
            description: String::from("A plugin written in Rust"),
        }
    }
    fn receive_message(
        &mut self,
        _xpapi: &mut XPAPI,
        _from: i32,
        _message: MessageId,
        _param: *mut std::ffi::c_void,
    ) {
    }
}

xplane_plugin!(MinimalPlugin);
