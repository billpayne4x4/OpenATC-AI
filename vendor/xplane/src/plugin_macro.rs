// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

/// Creates an X-Plane plugin
///
/// Provide the name of your plugin struct. The callbacks that X-Plane uses will be created.
///
/// Creating a plugin involves three steps:
///
/// 1. Create a struct for your plugin
/// 2. Implement Plugin for your plugin struct
/// 3. Place `xplane_plugin!(YourPluginStruct)` in a file, not in any function
///
#[macro_export]
macro_rules! xplane_plugin {
    ($plugin_type: ty) => {
        struct _UncheckedSyncUnsafeCell<T>(::std::cell::UnsafeCell<T>);

        /// Safety: dereferencing the pointer from `UnsafeCell::get` must involve external synchronization.
        unsafe impl Sync
            for _UncheckedSyncUnsafeCell<$crate::plugin::internal::PluginData<$plugin_type>>
        {
        }

        // The plugin
        static PLUGIN: _UncheckedSyncUnsafeCell<
            $crate::plugin::internal::PluginData<$plugin_type>,
        > = _UncheckedSyncUnsafeCell(::std::cell::UnsafeCell::new(
            $crate::plugin::internal::PluginData {
                plugin: ::std::ptr::null_mut(),
            },
        ));

        #[allow(non_snake_case)]
        #[no_mangle]
        /// Shim around `xplane::plugin::internal::xplugin_start`.
        pub unsafe extern "C-unwind" fn XPluginStart(
            name: *mut ::std::ffi::c_char,
            signature: *mut ::std::ffi::c_char,
            description: *mut ::std::ffi::c_char,
        ) -> ::std::ffi::c_int {
            unsafe {
                $crate::plugin::internal::xplugin_start(
                    PLUGIN
                        .0
                        .get()
                        .as_mut()
                        .expect("The contents of PLUGIN should never be NULL."),
                    name,
                    signature,
                    description,
                )
            }
        }

        #[allow(non_snake_case)]
        #[no_mangle]
        /// Shim around `xplane::plugin::internal::xplugin_stop`.
        pub unsafe extern "C-unwind" fn XPluginStop() {
            unsafe {
                $crate::plugin::internal::xplugin_stop(
                    PLUGIN
                        .0
                        .get()
                        .as_mut()
                        .expect("The contents of PLUGIN should never be NULL."),
                )
            }
        }

        #[allow(non_snake_case)]
        #[no_mangle]
        /// Shim around `xplane::plugin::internal::xplugin_enable`.
        pub unsafe extern "C-unwind" fn XPluginEnable() -> ::std::ffi::c_int {
            unsafe {
                $crate::plugin::internal::xplugin_enable(
                    PLUGIN
                        .0
                        .get()
                        .as_mut()
                        .expect("The contents of PLUGIN should never be NULL."),
                )
            }
        }

        #[allow(non_snake_case)]
        #[no_mangle]
        /// Shim around `xplane::plugin::internal::xplugin_disable`.
        pub unsafe extern "C-unwind" fn XPluginDisable() {
            unsafe {
                $crate::plugin::internal::xplugin_disable(
                    PLUGIN
                        .0
                        .get()
                        .as_mut()
                        .expect("The contents of PLUGIN should never be NULL."),
                )
            }
        }

        #[allow(non_snake_case)]
        #[no_mangle]
        /// Shim around `xplane::plugin::internal::xplugin_receive_message`.
        pub unsafe extern "C-unwind" fn XPluginReceiveMessage(
            from: ::std::ffi::c_int,
            message: ::std::ffi::c_int,
            param: *mut ::std::ffi::c_void,
        ) {
            unsafe {
                $crate::plugin::internal::xplugin_receive_message(
                    PLUGIN
                        .0
                        .get()
                        .as_mut()
                        .expect("The contents of PLUGIN should never be NULL."),
                    from,
                    message,
                    param,
                )
            }
        }
    };
}
