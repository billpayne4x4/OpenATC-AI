// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use crate::make_x;

use super::{
    super::{debugln, internal::copy_to_c_buffer},
    Plugin,
};

/// Information on a plugin
pub struct PluginData<P> {
    /// A pointer to the plugin, allocated in a Box
    pub plugin: *mut P,
}

/// Implements the `XPluginStart` callback
///
/// This reduces the amount of code in the `xplane_plugin` macro.
///
/// data is a reference to a `PluginData` object where the created plugin will be stored.
/// The other parameters are the same as for `XPluginStart`.
///
/// This function tries to create and allocate a plugin. On success, it stores a pointer to the
/// plugin in data.plugin and returns 1. If the plugin fails to start, it stores a null pointer
/// in data.plugin and returns 0.
pub unsafe fn xplugin_start<P>(
    data: &mut PluginData<P>,
    name: *mut c_char,
    signature: *mut c_char,
    description: *mut c_char,
) -> c_int
where
    P: Plugin,
{
    let mut x = make_x();
    super::super::internal::xplm_init(&mut x);
    match P::start(&mut x) {
        Ok(plugin) => {
            let info = plugin.info();
            unsafe {
                copy_to_c_buffer(info.name, name);
                copy_to_c_buffer(info.signature, signature);
                copy_to_c_buffer(info.description, description);
            }

            let plugin_box = Box::new(plugin);
            data.plugin = Box::into_raw(plugin_box);
            1
        }
        Err(e) => {
            debugln!(x, "Plugin failed to start: {}", e).unwrap(); // This string should be valid.
            data.plugin = ptr::null_mut();
            0
        }
    }
}

/// Implements the `XPluginStop` callback
pub unsafe fn xplugin_stop<P>(data: &mut PluginData<P>)
where
    P: Plugin,
{
    let plugin = unsafe { Box::from_raw(data.plugin) };
    data.plugin = ptr::null_mut();
    drop(plugin);
}

/// Implements the `XPluginEnable` callback
pub unsafe fn xplugin_enable<P>(data: &mut PluginData<P>) -> c_int
where
    P: Plugin,
{
    let mut x = make_x();
    match unsafe { (*data.plugin).enable(&mut x) } {
        Ok(()) => 1,
        Err(e) => {
            debugln!(x, "Plugin failed to enable: {}", e).unwrap(); // This string should be valid.
            0
        }
    }
}

/// Implements the `XPluginDisable` callback
pub unsafe fn xplugin_disable<P>(data: &mut PluginData<P>)
where
    P: Plugin,
{
    let mut x = make_x();
    unsafe {
        (*data.plugin).disable(&mut x);
    }
}

/// Implements the `XPluginReceiveMessage` callback
pub unsafe fn xplugin_receive_message<P>(
    data: &mut PluginData<P>,
    from: c_int,
    message: c_int,
    param: *mut c_void,
) where
    P: Plugin,
{
    let mut x = make_x();
    unsafe {
        (*data.plugin).receive_message(&mut x, from, message.into(), param);
    }
}
