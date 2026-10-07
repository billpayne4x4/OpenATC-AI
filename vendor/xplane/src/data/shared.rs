// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::{
    ffi::{c_void, CString, NulError},
    marker::PhantomData,
};

use snafu::prelude::*;
use xplane_sys::{XPLMFindDataRef, XPLMShareData, XPLMUnshareData};

use crate::{
    data::{borrowed::DataRef, DataType, ReadWrite},
    make_x, XPAPI,
};

#[derive(Debug, Snafu)]
/// Possible errors raised when trying to get a [`SharedData`].
pub enum SharedDataError {
    #[snafu(context(false))]
    /// The passed string contains a NUL byte.
    Nul {
        /// The source [`NulError`].
        source: NulError,
    },
    #[snafu(display(
        "XPLMShareData returned 0. The shared data already exists, but is of the wrong type."
    ))]
    /// [`XPLMShareData`] returned zero. This indicates that the shared data already exists,
    /// but that its type does not match.
    WrongType,
}

/// A shared dataref. Your [`SharedDataHandler::data_changed`] will be called whenever the value of the dataref changes.
/// You will need to use the function to read the value. Weird, I know.
pub struct SharedData<T: DataType + ?Sized + 'static> {
    ctx: *mut SharedDataContext<T>,
    _phantom: PhantomData<(*mut (), T)>,
}

impl<T: DataType + ?Sized + 'static> SharedData<T> {
    pub(super) fn new<S: Into<Vec<u8>>>(
        name: S,
        handler: impl SharedDataHandler<T>,
    ) -> Result<SharedData<T>, SharedDataError> {
        let name = CString::new(name)?;
        let handler: *mut dyn SharedDataHandler<T> = Box::into_raw(Box::new(handler));
        let ctx = Box::into_raw(Box::new(SharedDataContext {
            name,
            dref: None,
            handler,
            _phantom: PhantomData,
        }));
        let res = unsafe {
            XPLMShareData(
                (*ctx).name.as_ptr(),
                T::sim_type(),
                Some(handle_shared_data_change::<T>),
                ctx.cast(),
            )
        };
        if res == 1 {
            Ok(SharedData {
                ctx,
                _phantom: PhantomData,
            })
        } else {
            WrongTypeSnafu.fail()
        }
    }
}

impl<T: DataType + ?Sized + 'static> Drop for SharedData<T> {
    fn drop(&mut self) {
        unsafe {
            let _ = XPLMUnshareData(
                // Don't know what this int actually means.
                (*self.ctx).name.as_ptr(),
                T::sim_type(),
                Some(handle_shared_data_change::<T>),
                self.ctx.cast(),
            );
        }
        let _ = unsafe { Box::from_raw(self.ctx) };
    }
}

struct SharedDataContext<T: DataType + ?Sized + 'static> {
    name: CString,
    dref: Option<DataRef<T, ReadWrite>>,
    handler: *mut dyn SharedDataHandler<T>,
    _phantom: PhantomData<(*mut (), T)>,
}

impl<T: DataType + ?Sized> Drop for SharedDataContext<T> {
    fn drop(&mut self) {
        let _ = unsafe { Box::from_raw(self.handler) };
    }
}

/// A handler for when shared data is changed.
pub trait SharedDataHandler<T>: 'static
where
    T: DataType + ?Sized + 'static,
{
    /// Called when shared data is changed.
    fn data_changed(&mut self, x: &mut XPAPI, dref: &mut DataRef<T, ReadWrite>);
}

unsafe extern "C-unwind" fn handle_shared_data_change<T: DataType + ?Sized + 'static>(
    refcon: *mut c_void,
) {
    let ctx = unsafe {
        refcon.cast::<SharedDataContext<T>>().as_mut().unwrap() // UNWRAP: This should never be null.
    };
    let dref = if let Some(ref mut dref) = ctx.dref {
        dref
    } else {
        let dref = DataRef {
            id: unsafe {
                XPLMFindDataRef(ctx.name.as_ptr()) // X-Plane promises this dataref exists.
            },
            _phantom: PhantomData,
        };
        ctx.dref = Some(dref);
        ctx.dref.as_mut().unwrap() // UNWRAP: We just set this to Some.
    };
    let cb = unsafe { ctx.handler.as_mut().unwrap() }; // UNWRAP: This will not be a null pointer.
    let mut x = make_x();
    cb.data_changed(&mut x, dref);
}
