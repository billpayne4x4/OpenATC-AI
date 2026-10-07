// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::ffi::c_int;
use std::{
    ffi::{c_void, CStr, CString},
    marker::PhantomData,
    path::Path,
};

use snafu::prelude::*;
use xplane_sys::{self, XPLMGetPluginInfo};

use crate::{ffi::StringBuffer, message::MessageId, NoSendSync};

/// An iterator over all loaded plugins
pub struct Plugins {
    /// The index of the next plugin to return
    ///
    /// If this is equal to count, no more plugins are available
    next: c_int,
    /// The total number of plugins available
    count: c_int,
    _phantom: NoSendSync,
}

#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
impl Iterator for Plugins {
    type Item = Plugin;
    fn next(&mut self) -> Option<Self::Item> {
        if self.next < self.count {
            let id = unsafe { xplane_sys::XPLMGetNthPlugin(self.next) };
            self.next += 1;
            // Skip past X-Plane
            if id == xplane_sys::XPLM_PLUGIN_XPLANE as xplane_sys::XPLMPluginID {
                self.next()
            } else {
                Some(unsafe { get_plugin(id) })
            }
        } else {
            None
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self.count - self.next) as usize;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for Plugins {}

/// Another plugin running in X-Plane (or this plugin)
pub struct Plugin {
    id: xplane_sys::XPLMPluginID,
    name: CString,
    file_path: CString,
    signature: CString,
    description: CString,
    _phantom: NoSendSync,
}

#[derive(Debug, Snafu)]
#[snafu(display("The requested plugin could not be enabled."))]
/// Returned when a plugin could not be enabled. No further information is available.
pub struct PluginEnableError;

impl Plugin {
    /// Returns the name of this plugin
    #[must_use]
    pub fn name_c(&self) -> &CStr {
        &self.name
    }
    /// Returns the signature of this plugin
    #[must_use]
    pub fn signature_c(&self) -> &CStr {
        &self.signature
    }
    /// Returns the description of this plugin
    #[must_use]
    pub fn description_c(&self) -> &CStr {
        &self.description
    }
    /// Returns the absolute path to this plugin
    #[must_use]
    pub fn path_c(&self) -> &CStr {
        &self.file_path
    }

    /// Returns true if this plugin is enabled
    #[must_use]
    pub fn is_enabled(&mut self) -> bool {
        unsafe { xplane_sys::XPLMIsPluginEnabled(self.id) == 1 }
    }

    /// Enables the plugin.
    /// # Errors
    /// A [`PluginEnableError`] is returned if the plugin could not be enabled.
    /// <div class="warning"> Disabling and enabling plugins when the simulator is running can sometimes be catastrophic.
    /// Be very careful when doing this.</div>
    pub fn enable(&mut self) -> Result<(), PluginEnableError> {
        unsafe {
            if xplane_sys::XPLMEnablePlugin(self.id) == 1 {
                Ok(())
            } else {
                Err(PluginEnableError)
            }
        }
    }

    /// Disables the plugin.
    /// <div class="warning"> Disabling and enabling plugins when the simulator is running can sometimes be catastrophic.
    /// Be very careful when doing this.</div>
    pub fn disable(&mut self) {
        unsafe {
            xplane_sys::XPLMDisablePlugin(self.id);
        }
    }

    /// Send a message to this function.
    /// # Safety
    /// You are sending a raw pointer to memory to code you probably don't control.
    /// Here be dragons.
    pub unsafe fn send_message(&mut self, message_id: MessageId, param: *mut c_void) {
        unsafe {
            xplane_sys::XPLMSendMessageToPlugin(self.id, message_id.into(), param);
        }
    }
}

/// Retrieve data about a given plugin based on its ID from X-Plane.
unsafe fn get_plugin(id: xplane_sys::XPLMPluginID) -> Plugin {
    let mut name = StringBuffer::new(257);
    let mut fp = StringBuffer::new(257);
    let mut sig = StringBuffer::new(257);
    let mut desc = StringBuffer::new(257);
    unsafe {
        XPLMGetPluginInfo(
            id,
            name.as_mut_ptr(),
            fp.as_mut_ptr(),
            sig.as_mut_ptr(),
            desc.as_mut_ptr(),
        );
    }
    Plugin {
        id,
        name: name.into(),
        file_path: fp.into(),
        signature: sig.into(),
        description: desc.into(),
        _phantom: PhantomData,
    }
}

/// Access struct for the X-Plane plugin API.
pub struct PluginApi {
    pub(crate) _phantom: NoSendSync,
}

impl PluginApi {
    /// Looks for a plugin with the provided signature and returns it if it exists
    #[must_use]
    pub fn from_signature(&mut self, signature: &str) -> Option<Plugin> {
        let signature = CString::new(signature).ok()?;
        let plugin_id = unsafe { xplane_sys::XPLMFindPluginBySignature(signature.as_ptr()) };
        if plugin_id == xplane_sys::XPLM_NO_PLUGIN_ID {
            None
        } else {
            Some(unsafe { get_plugin(plugin_id) })
        }
    }

    /// Looks for a plugin at the provided absolute path, and returns it if it exists.
    #[must_use]
    pub fn from_path(&mut self, path: &Path) -> Option<Plugin> {
        let path_c = CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
        let plugin_id = unsafe { xplane_sys::XPLMFindPluginByPath(path_c.as_ptr()) };
        if plugin_id == xplane_sys::XPLM_NO_PLUGIN_ID {
            None
        } else {
            Some(unsafe { get_plugin(plugin_id) })
        }
    }

    /// Returns the plugin that is currently running
    /// # Panics
    /// Panics if you've somehow managed to call this without being in a plugin (or X-Plane has no ID for your plugin, somehow). Congratulations.
    #[must_use]
    pub fn this_plugin(&mut self) -> Plugin {
        let plugin_id = unsafe { xplane_sys::XPLMGetMyID() };
        assert_ne!(
            plugin_id,
            xplane_sys::XPLM_NO_PLUGIN_ID,
            "XPLMGetMyId() returned no plugin ID. Please get in touch -- this should be impossible."
        );
        unsafe { get_plugin(plugin_id) }
    }

    /// Returns an iterator over all loaded plugins
    #[must_use]
    pub fn all_plugins(&mut self) -> Plugins {
        Plugins {
            next: 0,
            // Subtract 1 because X-Plane is considered a plugin
            count: unsafe { xplane_sys::XPLMCountPlugins() - 1 },
            _phantom: PhantomData,
        }
    }

    /// Reload all plugins.
    /// After the callback in which this function is called, is returned from,
    /// the plugin disable and then stop functions will be called.
    /// The plugin start process will then occur as if the sim was starting up.
    /// <div class="warning"> Many plugins will behave in strange manners, or even cause a crash if reloaded at runtime.
    /// Reloading plugins may cause issues, including but not limited to a simulator crash. </div>
    pub fn reload_all(&mut self) {
        unsafe {
            xplane_sys::XPLMReloadPlugins();
        }
    }

    /// Broadcast a message to all plugins.
    /// # Safety
    /// You are passing in a raw pointer to memory, that any other plugin may do something with.
    /// Here be dragons.
    pub unsafe fn broadcast_plugin_message(&mut self, message_id: MessageId, param: *mut c_void) {
        unsafe {
            xplane_sys::XPLMSendMessageToPlugin(
                xplane_sys::XPLM_NO_PLUGIN_ID,
                message_id.into(),
                param,
            );
        }
    }
}
