// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::ffi::{c_int, c_void};
use std::{
    cell::{Cell, RefCell},
    ffi::{CString, NulError},
    fmt,
    marker::PhantomData,
    ptr,
    rc::Rc,
};

use snafu::prelude::*;

use xplane_sys::XPLMMenuCheck;

use crate::{make_x, NoSendSync, XPAPI};

/// Struct to access X-Plane's menu API.
pub struct MenuApi {
    pub(crate) _phantom: NoSendSync,
}

impl MenuApi {
    /// Creates a new menu with the provided name
    /// # Errors
    /// Returns an error if the name contains a NUL byte
    pub fn new_menu<S: Into<String>>(&mut self, name: S) -> Result<Menu, NulError> {
        Menu::new(name)
    }
    /// Creates a new item
    /// # Errors
    /// Returns an error if the name contains a null byte
    pub fn new_action_item<S: Into<String>, H: ClickHandler>(
        &mut self,
        name: S,
        handler: H,
    ) -> Result<ActionItem, NulError> {
        ActionItem::new(name, handler)
    }
    /// Creates a new item
    /// # Errors
    /// Returns an error if the name contains a null byte
    pub fn new_check_item<S: Into<String>, H: CheckHandler>(
        &mut self,
        name: S,
        checked: bool,
        handler: H,
    ) -> Result<CheckItem, NulError> {
        CheckItem::new(name, checked, handler)
    }
}

/// Something that can be added to a menu
#[derive(Debug, Clone)]
pub enum Item {
    /// A submenu
    Submenu(Rc<Menu>),
    /// An action item
    Action(Rc<ActionItem>),
    /// A checkable item
    Check(Rc<CheckItem>),
    /// A separator
    Separator,
}

impl Item {
    /// Called when this item is added to a parent menu
    fn add_to_menu(&self, parent_id: xplane_sys::XPLMMenuID) -> Result<(), MenuError> {
        match *self {
            Item::Submenu(ref menu) => menu.add_to_menu(parent_id),
            // Pass the address of this Item as a reference for the callback
            Item::Action(ref action) => action.add_to_menu(parent_id, self),
            Item::Check(ref check) => check.add_to_menu(parent_id, self),
            Item::Separator => {
                Separator.add_to_menu(parent_id);
                Ok(())
            }
        }
    }
    /// Called when the position of this item in the parent menu changes. The new index
    /// is provided.
    fn update_index(&self, index_in_parent: c_int) -> Result<(), MenuError> {
        match *self {
            Item::Submenu(ref menu) => menu.update_index(index_in_parent),
            Item::Action(ref action) => action.update_index(index_in_parent),
            Item::Check(ref check) => check.update_index(index_in_parent),
            Item::Separator => {
                Separator.update_index(index_in_parent);
                Ok(())
            }
        }
    }
    /// Called when this item is removed from a parent menu
    fn remove_from_menu(
        &self,
        parent_id: xplane_sys::XPLMMenuID,
        index_in_parent: c_int,
    ) -> Result<(), MenuError> {
        match *self {
            Item::Submenu(ref menu) => menu.remove_from_menu(parent_id, index_in_parent),
            Item::Action(ref action) => action.remove_from_menu(parent_id, index_in_parent),
            Item::Check(ref check) => check.remove_from_menu(parent_id, index_in_parent),
            Item::Separator => {
                Separator.remove_from_menu(parent_id, index_in_parent);
                Ok(())
            }
        }
    }
    /// Called when the user clicks on this menu item
    fn handle_click(&self) {
        match *self {
            Item::Action(ref action) => action.handle_click(),
            Item::Check(ref check) => check.handle_click(),
            _ => {}
        }
    }
}

impl From<Rc<Menu>> for Item {
    fn from(m: Rc<Menu>) -> Self {
        Item::Submenu(m)
    }
}
impl From<Rc<ActionItem>> for Item {
    fn from(a: Rc<ActionItem>) -> Self {
        Item::Action(a)
    }
}
impl From<Rc<CheckItem>> for Item {
    fn from(c: Rc<CheckItem>) -> Self {
        Item::Check(c)
    }
}
impl From<Rc<Separator>> for Item {
    fn from(_: Rc<Separator>) -> Self {
        Item::Separator
    }
}

/// A menu, which contains zero or more items
///
// Invariant: No [`RefCell`] is borrowed outside functions of this struct
#[derive(Debug)]
pub struct Menu {
    /// The name of this menu
    ///
    /// If this menu is in the menu bar directly, this name appears in the menu bar.
    /// If this menu is a submenu, this name appears in the menu item that opens this menu.
    ///
    /// Invariant: this can be converted into a CString
    name: RefCell<String>,
    /// The items, separators, and submenus in this menu
    ///
    /// Each item is in a Box, to allow callbacks to reference it.
    #[allow(clippy::vec_box)]
    children: RefCell<Vec<*mut Item>>,
    /// The status of this menu
    state: Cell<MenuState>,
    _phantom: NoSendSync,
}

impl Menu {
    fn new<S: Into<String>>(name: S) -> Result<Self, NulError> {
        let name = name.into();
        check_c_string(&name)?;
        Ok(Menu {
            name: RefCell::new(name),
            children: RefCell::new(Vec::new()),
            state: Cell::new(MenuState::Free),
            _phantom: PhantomData,
        })
    }

    /// Returns the name of this menu
    pub fn name(&self) -> String {
        let borrow = self.name.borrow();
        borrow.clone()
    }
    /// Sets the name of this menu
    /// # Errors
    /// Returns an error if the name contains a NUL byte
    pub fn set_name<S: AsRef<str>>(&self, name: S) -> Result<(), NulError> {
        let name = name.as_ref();
        check_c_string(name)?;
        let mut borrow = self.name.borrow_mut();
        borrow.clear();
        borrow.push_str(name);
        Ok(())
    }
    /// Adds a child to this menu
    /// The child argument may be a [`Menu`], [`ActionItem`], [`CheckItem`], or [`Separator`],
    /// or an Rc containing one of these types.
    pub fn add_child<R, C>(&self, child: R)
    where
        R: Into<Rc<C>>,
        Rc<C>: Into<Item>,
    {
        let mut borrow = self.children.borrow_mut();
        borrow.push(Box::into_raw(Box::new(child.into().into())));
    }

    /// Adds this menu as a child of the plugins menu
    /// # Errors
    /// This function will error if this [`Menu`] is already in a menu.
    pub fn add_to_plugins_menu(&self) -> Result<(), MenuError> {
        let plugins_menu = unsafe { xplane_sys::XPLMFindPluginsMenu() };
        if let MenuState::Free = self.state.get() {
            self.add_to_menu(plugins_menu)?;
            Ok(())
        } else {
            Err(MenuError::AlreadyInMenu)
        }
    }
    /// Removes this menu from the plugins menu
    /// # Errors
    /// This function will error if this [`Menu`] is not in the plugins menu, or
    /// if it is not in any menu at all.
    pub fn remove_from_plugins_menu(&self) -> Result<(), MenuError> {
        let plugins_menu = unsafe { xplane_sys::XPLMFindPluginsMenu() };
        if let MenuState::InMenu {
            id: _id,
            parent,
            index_in_parent,
        } = self.state.get()
        {
            if parent == plugins_menu {
                self.remove_from_menu(plugins_menu, index_in_parent)?;
                Ok(())
            } else {
                Err(MenuError::NotInThatMenu)
            }
        } else {
            Err(MenuError::NotInMenu)
        }
    }
}

/// Status that a menu can have
#[derive(Debug, Copy, Clone)]
enum MenuState {
    /// Not attached to a menu or a menu bar
    Free,
    /// Attached as a submenu
    /// activator is the menu item that causes this menu to appear.
    /// parent is the menu ID of the parent menu.
    /// index_in_parent is the index of the activator in the parent menu
    InMenu {
        id: xplane_sys::XPLMMenuID,
        parent: xplane_sys::XPLMMenuID,
        index_in_parent: c_int,
    },
}

impl Menu {
    fn add_to_menu(&self, parent_id: xplane_sys::XPLMMenuID) -> Result<(), MenuError> {
        if let MenuState::Free = self.state.get() {
            let name_c = CString::new(self.name()).unwrap();
            // A submenu requires a menu item to open it
            let index = unsafe {
                xplane_sys::XPLMAppendMenuItem(parent_id, name_c.as_ptr(), ptr::null_mut(), 0)
            };

            let menu_id = unsafe {
                xplane_sys::XPLMCreateMenu(
                    name_c.as_ptr(),
                    parent_id,
                    index,
                    Some(menu_handler),
                    ptr::null_mut(),
                )
            };
            self.state.set(MenuState::InMenu {
                id: menu_id,
                parent: parent_id,
                index_in_parent: index,
            });
            // Add children
            let borrow = self.children.borrow();
            for child in borrow.iter() {
                // Memory safety warning: Child must be allocated by a Box to prevent it from
                // moving.
                let child = unsafe { child.as_ref().unwrap() }; // Unwrap: We know we won't have null pointers.
                child.add_to_menu(menu_id)?;
            }
            Ok(())
        } else {
            Err(MenuError::AlreadyInMenu)
        }
    }
    fn update_index(&self, index_in_parent: c_int) -> Result<(), MenuError> {
        let mut state = self.state.get();
        if let MenuState::InMenu {
            id: _,
            parent: _,
            index_in_parent: ref mut index,
        } = state
        {
            *index = index_in_parent;
        } else {
            return Err(MenuError::NotInMenu);
        }
        self.state.set(state);
        Ok(())
    }
    fn remove_from_menu(
        &self,
        parent_id: xplane_sys::XPLMMenuID,
        index_in_parent: c_int,
    ) -> Result<(), MenuError> {
        if let MenuState::InMenu {
            id,
            parent: state_parent,
            index_in_parent: state_idx,
        } = self.state.get()
        {
            if parent_id != state_parent || index_in_parent != state_idx {
                return Err(MenuError::NotInThatMenu);
            }
            // Remove children
            {
                let borrow = self.children.borrow();
                for child in borrow.iter() {
                    // Unwrap: We know we won't have null pointers.
                    let child = unsafe { child.as_ref().unwrap() };

                    // As each item is removed, the later items move up to index 0.
                    child.update_index(0)?;
                    child.remove_from_menu(id, 0)?;
                }
            }
            unsafe {
                xplane_sys::XPLMDestroyMenu(id);
            }
            // Destroy activator item
            unsafe {
                xplane_sys::XPLMRemoveMenuItem(state_parent, index_in_parent as c_int);
            }
            self.state.set(MenuState::Free);
            Ok(())
        } else {
            Err(MenuError::NotInMenu)
        }
    }
}

/// Removes this menu from X-Plane, to prevent the menu handler from running and accessing
/// a dangling pointer.
/// Also drops all child items.
impl Drop for Menu {
    fn drop(&mut self) {
        if let MenuState::InMenu {
            id: _id,
            parent,
            index_in_parent,
        } = self.state.get()
        {
            self.remove_from_menu(parent, index_in_parent).unwrap(); // The failure condition will not occur due to using the InMenu.
        }
        for child in self.children.borrow().iter() {
            let _ = unsafe { Box::from_raw(*child) };
        }
    }
}

/// A separator between menu items
#[derive(Debug)]
pub struct Separator;

#[allow(clippy::unused_self)]
impl Separator {
    fn add_to_menu(&self, parent_id: xplane_sys::XPLMMenuID) {
        // API note: XPLMAppendMenuItem returns the index of the appended item.
        // A menu separator also has an index and takes up a slot, but
        // XPLMAppendMenuSeparator does not return the index of the added separator.
        unsafe { xplane_sys::XPLMAppendMenuSeparator(parent_id) }
    }
    fn update_index(&self, _index_in_parent: c_int) {
        // Nothing
    }
    fn remove_from_menu(&self, parent_id: xplane_sys::XPLMMenuID, index_in_parent: c_int) {
        unsafe { xplane_sys::XPLMRemoveMenuItem(parent_id, index_in_parent as c_int) }
    }
}

/// An item that can be clicked on to perform an action
pub struct ActionItem {
    /// The text displayed for this item
    ///
    /// Invariant: this can be converted into a CString
    name: RefCell<String>,
    /// Information about the menu this item is part of
    in_menu: Cell<Option<InMenu>>,
    /// The item click handler
    handler: *mut dyn ClickHandler,
    _phantom: NoSendSync,
}

impl ActionItem {
    fn new<S: Into<String>, H: ClickHandler>(name: S, handler: H) -> Result<Self, NulError> {
        let name = name.into();
        check_c_string(&name)?;
        Ok(ActionItem {
            name: RefCell::new(name),
            in_menu: Cell::new(None),
            handler: Box::into_raw(Box::new(handler)),
            _phantom: PhantomData,
        })
    }

    /// Returns the name of this item
    pub fn name(&self) -> String {
        let borrow = self.name.borrow();
        borrow.clone()
    }
    /// Sets the name of this item
    /// # Errors
    /// Returns an error if the name contains a null byte
    pub fn set_name(&self, name: &str) -> Result<(), NulError> {
        let name_c = CString::new(name)?;
        let mut borrow = self.name.borrow_mut();
        borrow.clear();
        borrow.push_str(name);
        if let Some(in_menu) = self.in_menu.get() {
            unsafe {
                xplane_sys::XPLMSetMenuItemName(
                    in_menu.parent,
                    in_menu.index as c_int,
                    name_c.as_ptr(),
                    0,
                );
            }
        }
        Ok(())
    }
    fn add_to_menu(
        &self,
        parent_id: xplane_sys::XPLMMenuID,
        enclosing_item: *const Item,
    ) -> Result<(), MenuError> {
        let name_c = CString::new(self.name()).unwrap();
        if self.in_menu.get().is_some() {
            return Err(MenuError::AlreadyInMenu);
        }
        let index = unsafe {
            let index = xplane_sys::XPLMAppendMenuItem(
                parent_id,
                name_c.as_ptr(),
                enclosing_item as *mut _,
                0,
            );
            // Ensure item is not checkable
            xplane_sys::XPLMCheckMenuItem(parent_id, index, XPLMMenuCheck::NoCheck);
            index
        };
        self.in_menu.set(Some(InMenu::new(parent_id, index)));
        Ok(())
    }
    fn update_index(&self, index_in_parent: c_int) -> Result<(), MenuError> {
        let mut in_menu = self.in_menu.get();
        if let Some(ref mut in_menu) = in_menu {
            in_menu.index = index_in_parent;
        } else {
            return Err(MenuError::NotInMenu);
        }
        self.in_menu.set(in_menu);
        Ok(())
    }

    fn remove_from_menu(
        &self,
        parent_id: xplane_sys::XPLMMenuID,
        index_in_parent: c_int,
    ) -> Result<(), MenuError> {
        let Some(in_menu) = self.in_menu.get() else {
            return Err(MenuError::NotInMenu);
        };
        if parent_id != in_menu.parent || index_in_parent != in_menu.index {
            return Err(MenuError::NotInThatMenu);
        }
        unsafe {
            xplane_sys::XPLMRemoveMenuItem(parent_id, index_in_parent as c_int);
        }
        Ok(())
    }

    fn handle_click(&self) {
        let handler = unsafe { self.handler.as_mut().unwrap() }; // Unwrap: this will not be a null pointer.
        let mut x = make_x();
        handler.item_clicked(&mut x, self);
    }
}

/// Removes this menu from X-Plane, to prevent the menu handler from running and accessing
/// a dangling pointer.
/// Also drops the handler.
impl Drop for ActionItem {
    fn drop(&mut self) {
        if let Some(in_menu) = self.in_menu.get() {
            self.remove_from_menu(in_menu.parent, in_menu.index)
                .unwrap(); // The failure condition will not occur due to using the InMenu.
        }
        let _ = unsafe { Box::from_raw(self.handler) };
    }
}

#[allow(clippy::missing_fields_in_debug)] // PhantomData.
impl fmt::Debug for ActionItem {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ActionItem")
            .field("name", &self.name)
            .field("in_menu", &self.in_menu)
            .field("handler", &"[handler]")
            .finish()
    }
}

/// Trait for things that can respond when the user clicks on a menu item
pub trait ClickHandler: 'static {
    /// Called when the user clicks on a menu item. The clicked item is passed.
    fn item_clicked(&mut self, x: &mut XPAPI, item: &ActionItem);
}

impl<F> ClickHandler for F
where
    F: FnMut(&mut XPAPI, &ActionItem) + 'static,
{
    fn item_clicked(&mut self, x: &mut XPAPI, item: &ActionItem) {
        self(x, item);
    }
}

/// An item with a checkbox that can be checked or unchecked
pub struct CheckItem {
    /// The text displayed for this item
    ///
    /// Invariant: this can be converted into a CString
    name: RefCell<String>,
    /// If this item is checked
    checked: Cell<bool>,
    /// Information about the menu this item is part of
    in_menu: Cell<Option<InMenu>>,
    /// The check handler
    handler: *mut dyn CheckHandler,
    _phantom: NoSendSync,
}

impl CheckItem {
    fn new<S: Into<String>, H: CheckHandler>(
        name: S,
        checked: bool,
        handler: H,
    ) -> Result<Self, NulError> {
        let name = name.into();
        check_c_string(&name)?;
        Ok(CheckItem {
            name: RefCell::new(name),
            checked: Cell::new(checked),
            in_menu: Cell::new(None),
            handler: Box::into_raw(Box::new(handler)),
            _phantom: PhantomData,
        })
    }
    /// Returns true if this item is checked
    pub fn checked(&self) -> bool {
        if let Some(in_menu) = self.in_menu.get() {
            // Update from X-Plane
            unsafe {
                let mut check_state = XPLMMenuCheck::NoCheck;
                xplane_sys::XPLMCheckMenuItemState(
                    in_menu.parent,
                    in_menu.index as c_int,
                    &mut check_state,
                );
                if check_state == XPLMMenuCheck::NoCheck {
                    self.checked.set(false);
                } else if check_state == XPLMMenuCheck::Checked {
                    self.checked.set(true);
                } else {
                    // Unexpected state, correct
                    xplane_sys::XPLMCheckMenuItem(
                        in_menu.parent,
                        in_menu.index as c_int,
                        XPLMMenuCheck::NoCheck,
                    );
                    self.checked.set(false);
                }
            }
        }
        self.checked.get()
    }
    /// Sets this item as checked or unchecked
    pub fn set_checked(&self, checked: bool) {
        self.checked.set(checked);
        if let Some(in_menu) = self.in_menu.get() {
            unsafe {
                xplane_sys::XPLMCheckMenuItem(
                    in_menu.parent,
                    in_menu.index as c_int,
                    check_state(checked),
                );
            }
        }
    }
    /// Returns the name of this item
    pub fn name(&self) -> String {
        let borrow = self.name.borrow();
        borrow.clone()
    }
    /// Sets the name of this item
    /// # Errors
    /// Returns an error if the name contains a null byte
    pub fn set_name(&self, name: &str) -> Result<(), NulError> {
        let name_c = CString::new(name)?;
        let mut borrow = self.name.borrow_mut();
        borrow.clear();
        borrow.push_str(name);
        if let Some(in_menu) = self.in_menu.get() {
            unsafe {
                xplane_sys::XPLMSetMenuItemName(
                    in_menu.parent,
                    in_menu.index as c_int,
                    name_c.as_ptr(),
                    0,
                );
            }
        }
        Ok(())
    }

    fn add_to_menu(
        &self,
        parent_id: xplane_sys::XPLMMenuID,
        enclosing_item: *const Item,
    ) -> Result<(), MenuError> {
        if self.in_menu.get().is_some() {
            return Err(MenuError::AlreadyInMenu);
        }
        let name_c = CString::new(self.name()).unwrap();
        let index = unsafe {
            let index = xplane_sys::XPLMAppendMenuItem(
                parent_id,
                name_c.as_ptr(),
                enclosing_item as *mut _,
                0,
            );
            // Configure check
            let check_state = check_state(self.checked.get());
            xplane_sys::XPLMCheckMenuItem(parent_id, index, check_state);
            index
        };
        self.in_menu.set(Some(InMenu::new(parent_id, index)));
        Ok(())
    }

    fn update_index(&self, index_in_parent: c_int) -> Result<(), MenuError> {
        let mut in_menu = self.in_menu.get();
        if let Some(ref mut in_menu) = in_menu {
            in_menu.index = index_in_parent;
        } else {
            return Err(MenuError::NotInMenu);
        }
        self.in_menu.set(in_menu);
        Ok(())
    }

    fn remove_from_menu(
        &self,
        parent_id: xplane_sys::XPLMMenuID,
        index_in_parent: c_int,
    ) -> Result<(), MenuError> {
        let Some(in_menu) = self.in_menu.get() else {
            return Err(MenuError::NotInMenu);
        };
        if parent_id != in_menu.parent || index_in_parent != in_menu.index {
            return Err(MenuError::NotInThatMenu);
        }
        unsafe {
            xplane_sys::XPLMRemoveMenuItem(parent_id, index_in_parent as c_int);
        }
        Ok(())
    }

    fn handle_click(&self) {
        // Invert check
        let checked = !self.checked();
        self.set_checked(checked);
        let handler = unsafe { self.handler.as_mut().unwrap() }; // Unwrap: This will not be a null pointer.
        let mut x = make_x();
        handler.item_checked(&mut x, self, checked);
    }
}
/// Removes this menu from X-Plane, to prevent the menu handler from running and accessing
/// a dangling pointer
impl Drop for CheckItem {
    fn drop(&mut self) {
        if let Some(in_menu) = self.in_menu.get() {
            self.remove_from_menu(in_menu.parent, in_menu.index)
                .unwrap(); // Unwrap: Using the data from in_menu, so this will not fail.
        }
        let _ = unsafe { Box::from_raw(self.handler) };
    }
}

#[allow(clippy::missing_fields_in_debug)] // PhantomData.
impl fmt::Debug for CheckItem {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("CheckItem")
            .field("name", &self.name)
            .field("checked", &self.checked)
            .field("in_menu", &self.in_menu)
            .field("handler", &"[handler]")
            .finish()
    }
}

/// Trait for things that can respond to check state changes
pub trait CheckHandler: 'static {
    /// Called when the user checks or unchecks an item
    fn item_checked(&mut self, x: &mut XPAPI, item: &CheckItem, checked: bool);
}

impl<F> CheckHandler for F
where
    F: FnMut(&mut XPAPI, &CheckItem, bool) + 'static,
{
    fn item_checked(&mut self, x: &mut XPAPI, item: &CheckItem, checked: bool) {
        self(x, item, checked);
    }
}

/// Maps true->checked and false->unchecked
fn check_state(checked: bool) -> XPLMMenuCheck {
    if checked {
        XPLMMenuCheck::Checked
    } else {
        XPLMMenuCheck::Unchecked
    }
}

/// Information stored by a menu item when it has been added to a menu
#[derive(Debug, Copy, Clone)]
struct InMenu {
    /// The menu ID of the parent menu
    pub parent: xplane_sys::XPLMMenuID,
    /// The index of this item in the parent menu
    pub index: c_int,
}

impl InMenu {
    pub fn new(parent: xplane_sys::XPLMMenuID, index: c_int) -> Self {
        InMenu { parent, index }
    }
}

/// Confirms that the provided string can be converted into a `CString`.
/// Returns an error if it cannot.
fn check_c_string(text: &str) -> Result<(), NulError> {
    CString::new(text).map(|_| ())
}

/// The menu handler callback used for all menu items
///
/// `item_ref` is a pointer to the relevant Item, allocated in an Rc
unsafe extern "C-unwind" fn menu_handler(_menu_ref: *mut c_void, item_ref: *mut c_void) {
    let item = item_ref as *const Item;
    unsafe {
        (*item).handle_click();
    }
}

#[derive(Snafu, Debug)]
#[allow(clippy::enum_variant_names)] // These variant names make sense.
/// Errors that may come from interacting with the menu API.
pub enum MenuError {
    #[snafu(display("This item is already in a menu, and so cannot be added to one."))]
    /// The item is already in a menu, and so cannot be added to one.
    AlreadyInMenu,
    #[snafu(display("This item is not in a menu. The requested action cannot be done."))]
    /// The item is not in a menu. Whatever you're trying to do requires it to be in one.
    NotInMenu,
    #[snafu(display("This item is not in the requested menu at the stated index, and so cannot be removed from it."))]
    /// The item is not in the requested menu at the stated index, and so cannot be removed from it.
    NotInThatMenu,
}

#[cfg(test)]
mod tests {
    #[test]
    #[allow(clippy::too_many_lines)]
    fn test_menus() {
        // TODO: Make a test case for menus.
    }
}
