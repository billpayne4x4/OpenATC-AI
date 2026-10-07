// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//!
//! This plugin creates a submenu under the Plugins menu. The submenu has one checkable item
//! and one action item.
//!

use xplane::{
    debugln,
    menu::{ActionItem, CheckHandler, CheckItem, ClickHandler, Menu},
    message::MessageId,
    plugin::{Plugin, PluginInfo},
    xplane_plugin, XPAPI,
};

struct MenuPlugin {
    _plugins_submenu: Menu,
}

impl Plugin for MenuPlugin {
    type Error = std::convert::Infallible;

    fn start(x: &mut XPAPI) -> Result<Self, Self::Error> {
        let plugins_submenu = x.menu.new_menu("Menu Test Plugin").unwrap();
        plugins_submenu.add_child(
            x.menu
                .new_check_item("Checkable 1", false, CheckHandler1)
                .unwrap(),
        );
        plugins_submenu.add_child(x.menu.new_action_item("Action 1", ActionHandler1).unwrap());
        plugins_submenu.add_to_plugins_menu().unwrap();

        // The menu needs to be part of the plugin struct, or it will immediately get dropped and
        // will not appear
        Ok(MenuPlugin {
            _plugins_submenu: plugins_submenu,
        })
    }

    fn enable(&mut self, _x: &mut XPAPI) -> Result<(), Self::Error> {
        Ok(())
    }

    fn disable(&mut self, _x: &mut XPAPI) {}

    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: String::from("Rust Menu Plugin"),
            signature: String::from("com.jdemille.xplane.examples.menu"),
            description: String::from("A plugin written in Rust that creates menus and menu items"),
        }
    }
    fn receive_message(
        &mut self,
        _x: &mut XPAPI,
        _from: i32,
        _message: MessageId,
        _param: *mut std::ffi::c_void,
    ) {
    }
}

xplane_plugin!(MenuPlugin);

struct CheckHandler1;

impl CheckHandler for CheckHandler1 {
    fn item_checked(&mut self, x: &mut XPAPI, _item: &CheckItem, checked: bool) {
        debugln!(x, "Checkable 1 checked = {}", checked).unwrap(); // No NUL bytes.
    }
}

struct ActionHandler1;

impl ClickHandler for ActionHandler1 {
    fn item_clicked(&mut self, x: &mut XPAPI, _item: &ActionItem) {
        debugln!(x, "Action 1 selected").unwrap(); // No NUL bytes.
    }
}
