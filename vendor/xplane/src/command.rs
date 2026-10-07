// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::ffi::{c_int, c_void};
use std::{
    ffi::{CString, NulError},
    marker::PhantomData,
};

use snafu::prelude::*;

use xplane_sys::{
    XPLMCommandBegin, XPLMCommandEnd, XPLMCommandOnce, XPLMCommandPhase, XPLMCommandRef,
    XPLMCreateCommand, XPLMFindCommand, XPLMRegisterCommandHandler, XPLMUnregisterCommandHandler,
};

use crate::{make_x, NoSendSync, XPAPI};

/// Struct to access X-Plane's command API.
pub struct CommandApi {
    pub(crate) _phantom: NoSendSync,
}

impl CommandApi {
    /// Make a new command.
    /// # Errors
    /// Returns an error if a matching command already exists.
    pub fn try_new(
        &mut self,
        name: &str,
        description: &str,
    ) -> Result<Command, CommandCreateError> {
        Command::try_new(name, description)
    }
    /// Finds a command
    ///
    /// The command should have already been created by X-Plane or another plugin.
    /// # Errors
    /// Errors if command could not be found.
    pub fn try_find(&mut self, name: &str) -> Result<Command, CommandFindError> {
        Command::try_find(name)
    }
}

/// A command created by X-Plane or another plugin, that can be triggered
#[derive(Debug)]
pub struct Command {
    /// The command reference
    id: XPLMCommandRef,
    _phantom: NoSendSync,
}

impl Command {
    fn try_new(name: &str, description: &str) -> Result<Self, CommandCreateError> {
        let name_c = CString::new(name)?;
        let description_c = CString::new(description)?;

        let existing = unsafe { XPLMFindCommand(name_c.as_ptr()) };
        if existing.is_null() {
            let command_id = unsafe { XPLMCreateCommand(name_c.as_ptr(), description_c.as_ptr()) };
            Ok(Command {
                id: command_id,
                _phantom: PhantomData,
            })
        } else {
            Err(CommandCreateError::Exists {
                existing_command: Command {
                    id: existing,
                    _phantom: PhantomData,
                },
            })
        }
    }
    fn try_find(name: &str) -> Result<Self, CommandFindError> {
        let name_c = CString::new(name)?;
        let command_ref = unsafe { XPLMFindCommand(name_c.as_ptr()) };
        if command_ref.is_null() {
            Err(CommandFindError::NotFound)
        } else {
            Ok(Command {
                id: command_ref,
                _phantom: PhantomData,
            })
        }
    }

    /// Triggers a command once
    ///
    /// This is equivalent to pressing a button down and immediately releasing it.
    pub fn trigger(&mut self) {
        unsafe {
            XPLMCommandOnce(self.id);
        }
    }

    /// Starts holding down this command
    ///
    /// The command will be released when the returned hold object is dropped.
    pub fn hold_down(&'_ mut self) -> CommandHold<'_> {
        unsafe {
            XPLMCommandBegin(self.id);
        }
        CommandHold {
            command: self,
            _phantom: PhantomData,
        }
    }

    /// Releases this command
    fn release(&mut self) {
        unsafe {
            XPLMCommandEnd(self.id);
        }
    }
    /// Register a [`CommandHandler`] for this command.
    ///
    /// If `before` is `true`, then this handler will be run before X-Plane executes the command.
    pub fn handle(
        &mut self,
        handler: impl CommandHandler,
        before: bool,
    ) -> RegisteredCommandHandler {
        RegisteredCommandHandler::new(self, handler, before)
    }
}

/// An RAII lock that keeps a command held down.
///
/// The command will be released when this object is dropped.
#[derive(Debug)]
pub struct CommandHold<'a> {
    /// The command being held
    command: &'a mut Command,
    _phantom: NoSendSync,
}

impl<'a> Drop for CommandHold<'a> {
    fn drop(&mut self) {
        self.command.release();
    }
}

/// Errors that can occur when finding a command
#[derive(Snafu, Debug)]
#[snafu(module)]
pub enum CommandFindError {
    /// The provided command name contained a NUL byte
    #[snafu(display("Null byte in command name."))]
    #[snafu(context(false))]
    Nul {
        /// The source of this error.
        source: NulError,
    },

    /// The Command could not be found
    #[snafu(display("Command not found."))]
    NotFound,
}

/// Enum returned from all functions of a [`CommandHandler`].
pub enum CommandHandlerResult {
    /// If handling before X-Plane, prevent X-Plane from running its own handler on this command.
    DisallowXPlaneProcessing,
    /// If handling before X-Plane, allow X-Plane to run its own handler on this command.
    AllowXPlaneProcessing,
    /// Return this if handling a command after X-Plane.
    Irrelevant,
}

impl From<CommandHandlerResult> for c_int {
    fn from(value: CommandHandlerResult) -> Self {
        match value {
            CommandHandlerResult::DisallowXPlaneProcessing => 0,
            CommandHandlerResult::AllowXPlaneProcessing | CommandHandlerResult::Irrelevant => 1,
        }
    }
}

/// Trait for things that can handle [`Commands`](Command).
/// Store your state data within the struct implementing this.
pub trait CommandHandler: 'static {
    /// Called when the command begins (corresponds to a button being pressed down)
    fn command_begin(&mut self, x: &mut XPAPI) -> CommandHandlerResult;
    /// Called frequently while the command button is held down
    fn command_continue(&mut self, x: &mut XPAPI) -> CommandHandlerResult;
    /// Called when the command ends (corresponds to a button being released)
    fn command_end(&mut self, x: &mut XPAPI) -> CommandHandlerResult;
}

/// A command created by this plugin that can be triggered by other components
pub struct RegisteredCommandHandler {
    /// The heap-allocated data
    data: *mut CommandHandlerData,
}

impl RegisteredCommandHandler {
    fn new(command: &Command, handler: impl CommandHandler, before: bool) -> Self {
        let data = Box::into_raw(Box::new(CommandHandlerData::new(command, handler, before)));
        unsafe {
            XPLMRegisterCommandHandler(
                (*data).command_ref,
                Some(command_handler),
                c_int::from(before),
                data.cast::<c_void>(),
            );
        }
        RegisteredCommandHandler { data }
    }
}

impl Drop for RegisteredCommandHandler {
    fn drop(&mut self) {
        unsafe {
            XPLMUnregisterCommandHandler(
                (*self.data).command_ref,
                Some(command_handler),
                (*self.data).before.into(),
                self.data.cast::<c_void>(),
            );
            let _ = Box::from_raw(self.data);
        }
    }
}

/// Data for an owned command, used as a refcon
struct CommandHandlerData {
    /// The command reference
    command_ref: XPLMCommandRef,
    /// The handler
    handler: *mut dyn CommandHandler,
    /// Whether this handler runs before others.
    before: bool,
}

impl CommandHandlerData {
    fn new(command: &Command, handler: impl CommandHandler, before: bool) -> Self {
        CommandHandlerData {
            command_ref: command.id,
            handler: Box::into_raw(Box::new(handler)),
            before,
        }
    }
}

impl Drop for CommandHandlerData {
    fn drop(&mut self) {
        let _ = unsafe { Box::from_raw(self.handler) };
    }
}

/// Command handler callback
unsafe extern "C-unwind" fn command_handler(
    _: XPLMCommandRef,
    phase: XPLMCommandPhase,
    refcon: *mut c_void,
) -> c_int {
    let data = refcon.cast::<CommandHandlerData>();
    let handler = unsafe { (*data).handler };
    let mut x = make_x();
    if phase == XPLMCommandPhase::Begin {
        unsafe { (*handler).command_begin(&mut x).into() }
    } else if phase == XPLMCommandPhase::Continue {
        unsafe { (*handler).command_continue(&mut x).into() }
    } else if phase == XPLMCommandPhase::End {
        unsafe { (*handler).command_end(&mut x).into() }
    } else {
        1 // If we've somehow achieved this, just let someone else deal with it.
    }
}

/// Errors that can occur when creating a Command
#[derive(Snafu, Debug)]
#[snafu(module)]
pub enum CommandCreateError {
    #[snafu(display("Null byte in Command name or description."))]
    #[snafu(context(false))]
    /// The provided Command name contained a NUL byte
    Nul {
        /// The source of this error.
        source: NulError,
    },

    #[snafu(display("Command exists already."))]
    /// The [`Command`] exists already
    Exists {
        /// The already existing command.
        existing_command: Command,
    },
}

#[cfg(test)]
mod tests {

    use std::{
        cell::RefCell,
        ffi::CStr,
        ptr::{self, NonNull},
        rc::Rc,
    };

    use super::*;

    #[test]
    #[allow(clippy::too_many_lines)] // This function has to set up several mocks.
    fn test_command_create_and_handling() {
        struct TestCommandHandler {
            internal_data: i32,
        }
        impl CommandHandler for TestCommandHandler {
            fn command_begin(&mut self, _x: &mut XPAPI) -> CommandHandlerResult {
                println!("Command begin! internal: {}", self.internal_data);
                self.internal_data = 32;
                CommandHandlerResult::AllowXPlaneProcessing
            }
            fn command_continue(&mut self, _x: &mut XPAPI) -> CommandHandlerResult {
                println!("Command continue! internal: {}", self.internal_data);
                self.internal_data = 64;
                CommandHandlerResult::DisallowXPlaneProcessing
            }
            fn command_end(&mut self, _x: &mut XPAPI) -> CommandHandlerResult {
                println!("Command end! internal: {}", self.internal_data);
                self.internal_data = 16;
                CommandHandlerResult::AllowXPlaneProcessing
            }
        }
        let refcon_cell = Rc::new(RefCell::new(NonNull::<c_void>::dangling().as_ptr()));
        let find_command_context = xplane_sys::XPLMFindCommand_context();
        find_command_context
            .expect()
            .withf(|cmd_c| {
                let cmd_c = unsafe { CStr::from_ptr(*cmd_c) };
                cmd_c == CString::new("xplane_rs/test/command").unwrap().as_c_str()
                // This contains no NUL bytes, and so should construct a C-string.
            })
            .once()
            .return_once_st(|_| std::ptr::null_mut());
        let create_command_context = xplane_sys::XPLMCreateCommand_context();
        let expected_ptr = NonNull::<c_void>::dangling().as_ptr();
        create_command_context
            .expect()
            .withf(|cmd_c, desc_c| {
                let cmd_c = unsafe { CStr::from_ptr(*cmd_c) };
                let desc_c = unsafe { CStr::from_ptr(*desc_c) };
                (cmd_c == CString::new("xplane_rs/test/command").unwrap().as_c_str())
                    && (desc_c
                        == CString::new("A test command for rust-xplane unit tests.")
                            .unwrap()
                            .as_c_str())
            })
            .once()
            .return_once_st(move |_, _| expected_ptr);
        let register_handler_ctx = xplane_sys::XPLMRegisterCommandHandler_context();
        let refcon_cell_1 = refcon_cell.clone();
        register_handler_ctx.expect().once().return_once_st(
            move |cmd_ref, handler, before, refcon| {
                assert_eq!(cmd_ref, expected_ptr);
                assert!(handler == Some(command_handler));
                assert_eq!(before, 1);
                *refcon_cell_1.borrow_mut() = refcon;
            },
        );
        let unregister_handler_ctx = xplane_sys::XPLMUnregisterCommandHandler_context();
        let refcon_cell_1 = refcon_cell.clone();
        unregister_handler_ctx.expect().once().return_once_st(
            move |cmd_ref, handler, before, refcon| {
                assert_eq!(cmd_ref, expected_ptr);
                assert!(handler == Some(command_handler));
                assert_eq!(before, 1);
                assert_eq!(refcon, *refcon_cell_1.borrow());
            },
        );
        let command_once_ctx = xplane_sys::XPLMCommandOnce_context();
        let refcon_cell_1 = refcon_cell.clone();
        command_once_ctx
            .expect()
            .once()
            .return_once_st(move |cmd_ref| {
                assert!(cmd_ref == expected_ptr);
                let res = unsafe {
                    command_handler(cmd_ref, XPLMCommandPhase::Begin, *refcon_cell_1.borrow())
                };
                assert_eq!(res, 1);
                let res = unsafe {
                    command_handler(cmd_ref, XPLMCommandPhase::End, *refcon_cell_1.borrow())
                };
                assert_eq!(res, 1);
            });
        let command_begin_ctx = xplane_sys::XPLMCommandBegin_context();
        let refcon_cell_1 = refcon_cell.clone();
        command_begin_ctx
            .expect()
            .once()
            .return_once_st(move |cmd_ref| {
                assert!(cmd_ref == expected_ptr);
                let res = unsafe {
                    command_handler(cmd_ref, XPLMCommandPhase::Begin, *refcon_cell_1.borrow())
                };
                assert_eq!(res, 1);
                let res = unsafe {
                    command_handler(cmd_ref, XPLMCommandPhase::Continue, *refcon_cell_1.borrow())
                };
                assert_eq!(res, 0);
            });
        let command_end_ctx = xplane_sys::XPLMCommandEnd_context();
        let refcon_cell_1 = refcon_cell.clone();
        command_end_ctx
            .expect()
            .once()
            .return_once_st(move |cmd_ref| {
                assert!(cmd_ref == expected_ptr);
                let res = unsafe {
                    command_handler(cmd_ref, XPLMCommandPhase::End, *refcon_cell_1.borrow())
                };
                assert_eq!(res, 1);
            });
        let mut x = make_x();
        let mut cmd = x
            .command
            .try_new(
                "xplane_rs/test/command",
                "A test command for rust-xplane unit tests.",
            )
            .unwrap(); // This should succeed.
        let _reg_handler = cmd.handle(TestCommandHandler { internal_data: 0 }, true);
        cmd.trigger();
        {
            let _ = cmd.hold_down();
        }
    }

    #[test]
    fn test_command_exists() {
        let expected_ptr = NonNull::<c_void>::dangling().as_ptr();
        let find_command_context = xplane_sys::XPLMFindCommand_context();
        find_command_context
            .expect()
            .withf(|cmd_c| {
                let cmd_c = unsafe { CStr::from_ptr(*cmd_c) };
                cmd_c == CString::new("xplane_rs/test/command").unwrap().as_c_str()
                // This contains no NUL bytes, and so should construct a C-string.
            })
            .once()
            .return_once_st(move |_| expected_ptr);
        let mut x = make_x();
        let cmd_result = x
            .command
            .try_new("xplane_rs/test/command", "Test command.")
            .unwrap_err();
        let CommandCreateError::Exists { existing_command } = cmd_result else {
            unreachable!("This code should be unreachable!")
        };
        assert_eq!(existing_command.id, expected_ptr);
    }

    #[test]
    fn test_command_found() {
        let expected_ptr = NonNull::<c_void>::dangling().as_ptr();
        let find_command_context = xplane_sys::XPLMFindCommand_context();
        find_command_context
            .expect()
            .withf(|cmd_c| {
                let cmd_c = unsafe { CStr::from_ptr(*cmd_c) };
                cmd_c == CString::new("xplane_rs/test/command").unwrap().as_c_str()
                // This contains no NUL bytes, and so should construct a C-string.
            })
            .once()
            .return_once_st(move |_| expected_ptr);
        let mut x = make_x();
        let cmd = x.command.try_find("xplane_rs/test/command").unwrap();
        assert_eq!(cmd.id, expected_ptr);
    }

    #[test]
    fn test_command_not_found() {
        let find_command_context = xplane_sys::XPLMFindCommand_context();
        find_command_context
            .expect()
            .withf(|cmd_c| {
                let cmd_c = unsafe { CStr::from_ptr(*cmd_c) };
                cmd_c == CString::new("xplane_rs/test/command").unwrap().as_c_str()
                // This contains no NUL bytes, and so should construct a C-string.
            })
            .once()
            .return_once_st(|_| ptr::null_mut());
        let mut x = make_x();
        let cmd_result = x.command.try_find("xplane_rs/test/command");
        assert!(matches!(cmd_result, Err(CommandFindError::NotFound)));
    }
}
