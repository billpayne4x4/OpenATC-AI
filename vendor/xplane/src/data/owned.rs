// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::ffi::{c_int, c_void};
use std::{
    cmp,
    ffi::{CString, NulError},
    i32,
    marker::PhantomData,
    ptr,
};

use snafu::prelude::*;

use xplane_sys::{
    XPLMDataRef, XPLMFindDataRef, XPLMGetDatab_f, XPLMGetDatad_f, XPLMGetDataf_f, XPLMGetDatai_f,
    XPLMGetDatavf_f, XPLMGetDatavi_f, XPLMRegisterDataAccessor, XPLMSetDatab_f, XPLMSetDatad_f,
    XPLMSetDataf_f, XPLMSetDatai_f, XPLMSetDatavf_f, XPLMSetDatavi_f, XPLMUnregisterDataAccessor,
};

use super::{Access, ArrayRead, ArrayReadWrite, DataRead, DataReadWrite, DataType, ReadOnly};

/// A dataref owned by this plugin
///
/// The access parameter of this type determines whether X-Plane and other plugins can write
/// this dataref. Owned datarefs can always be written by this plugin.
pub struct OwnedData<T: DataType + ?Sized, A = ReadOnly> {
    /// The dataref handle
    id: XPLMDataRef,
    /// The current value
    ///
    /// This comes from a Box, so that it will have a constant memory location that is
    /// provided as a refcon to the callbacks.
    value: *mut T::Storage,
    /// Data access and type phantom data.
    _phantom: PhantomData<(A, T)>,
}

impl<T: DataType + ?Sized, A: Access> OwnedData<T, A> {
    pub(super) fn new_with_value<S: AsRef<str>>(name: S, value: &T) -> Result<Self, CreateError> {
        let name = name.as_ref();
        let name_c = CString::new(name)?;

        let existing = unsafe { XPLMFindDataRef(name_c.as_ptr()) };
        if !existing.is_null() {
            return Err(CreateError::Exists);
        }

        let value = Box::into_raw(Box::new(value.to_storage()));

        let id = unsafe {
            XPLMRegisterDataAccessor(
                name_c.as_ptr(),
                T::sim_type(),
                Self::writeable(),
                Self::int_read(),
                Self::int_write(),
                Self::float_read(),
                Self::float_write(),
                Self::double_read(),
                Self::double_write(),
                Self::int_array_read(),
                Self::int_array_write(),
                Self::float_array_read(),
                Self::float_array_write(),
                Self::byte_array_read(),
                Self::byte_array_write(),
                value.cast(),
                value.cast(),
            )
        };

        assert!(!id.is_null(), "Dataref ID of created dataref is null!");
        Ok(OwnedData {
            id,
            value,
            _phantom: PhantomData,
        })
    }

    /// Returns 1 if this dataref should be writeable by other plugins and X-Plane
    fn writeable() -> i32 {
        i32::from(A::writeable())
    }
    fn int_read() -> XPLMGetDatai_f {
        if T::sim_type().int() {
            Some(read_single::<i32>)
        } else {
            None
        }
    }
    fn int_write() -> XPLMSetDatai_f {
        if T::sim_type().int() && A::writeable() {
            Some(write_single::<i32>)
        } else {
            None
        }
    }
    fn float_read() -> XPLMGetDataf_f {
        if T::sim_type().float() {
            Some(read_single::<f32>)
        } else {
            None
        }
    }
    fn float_write() -> XPLMSetDataf_f {
        if T::sim_type().float() && A::writeable() {
            Some(write_single::<f32>)
        } else {
            None
        }
    }
    fn double_read() -> XPLMGetDatad_f {
        if T::sim_type().double() {
            Some(read_single::<f64>)
        } else {
            None
        }
    }
    fn double_write() -> XPLMSetDatad_f {
        if T::sim_type().double() && A::writeable() {
            Some(write_single::<f64>)
        } else {
            None
        }
    }
    fn int_array_read() -> XPLMGetDatavi_f {
        if T::sim_type().int_array() {
            Some(array_read::<i32>)
        } else {
            None
        }
    }
    fn int_array_write() -> XPLMSetDatavi_f {
        if T::sim_type().int_array() && A::writeable() {
            Some(array_write::<i32>)
        } else {
            None
        }
    }
    fn float_array_read() -> XPLMGetDatavf_f {
        if T::sim_type().float_array() {
            Some(array_read::<f32>)
        } else {
            None
        }
    }
    fn float_array_write() -> XPLMSetDatavf_f {
        if T::sim_type().float_array() && A::writeable() {
            Some(array_write::<f32>)
        } else {
            None
        }
    }
    fn byte_array_read() -> XPLMGetDatab_f {
        if T::sim_type().data() {
            Some(byte_array_read)
        } else {
            None
        }
    }
    fn byte_array_write() -> XPLMSetDatab_f {
        if T::sim_type().data() && A::writeable() {
            Some(byte_array_write)
        } else {
            None
        }
    }
    fn value_ref(&self) -> &T::Storage {
        unsafe { self.value.as_ref().unwrap() } // Unwrap: This is guaranteed to not be a null pointer.
    }
    fn value_mut(&mut self) -> &mut T::Storage {
        unsafe { self.value.as_mut().unwrap() } // Unwrap: This will not be a null pointer.
    }
}

impl<T: DataType + ?Sized, A> Drop for OwnedData<T, A> {
    fn drop(&mut self) {
        unsafe { XPLMUnregisterDataAccessor(self.id) }
    }
}

// DataRead and DataReadWrite
macro_rules! impl_read_write {
    ([$native_type:ty]) => {
        impl<A: Access> ArrayRead<[$native_type]> for OwnedData<[$native_type], A> {
            fn get(&self, dest: &mut [$native_type]) -> usize {
                let copy_length = cmp::min(dest.len(), self.value_ref().len());
                let dest_sub = &mut dest[..copy_length];
                let value_sub = &self.value_ref()[..copy_length];
                dest_sub.copy_from_slice(value_sub);
                copy_length
            }
            fn len(&self) -> usize {
                self.value_ref().len()
            }
        }
        impl<A: Access> ArrayReadWrite<[$native_type]> for OwnedData<[$native_type], A> {
            fn set(&mut self, values: &[$native_type]) {
                let copy_length = cmp::min(values.len(), self.value_ref().len());
                let src_sub = &values[..copy_length];
                let values_sub = &mut self.value_mut()[..copy_length];
                values_sub.copy_from_slice(src_sub);
            }
        }
    };
    ($native_type:ty) => {
        impl<A: Access> DataRead<$native_type> for OwnedData<$native_type, A> {
            fn get(&self) -> $native_type {
                unsafe { *self.value }
            }
        }
        impl<A: Access> DataReadWrite<$native_type> for OwnedData<$native_type, A> {
            fn set(&mut self, value: $native_type) {
                unsafe {
                    *self.value = value;
                }
            }
        }
    };
}

impl_read_write!(u8);
impl_read_write!(i8);
impl_read_write!(u16);
impl_read_write!(i16);
impl_read_write!(i32);
impl_read_write!(u32);
impl_read_write!(f32);
impl_read_write!(f64);
impl_read_write!(bool);
impl_read_write!([i32]);
impl_read_write!([u32]);
impl_read_write!([f32]);
impl_read_write!([u8]);
impl_read_write!([i8]);

/// Errors that can occur when creating a `DataRef`
#[derive(Snafu, Debug)]
pub enum CreateError {
    /// The provided DataRef name contained a null byte
    #[snafu(display("Null byte in dataref name"))]
    #[snafu(context(false))]
    Null {
        /// The source error.
        source: NulError,
    },

    /// The DataRef exists already
    #[snafu(display("DataRef already exists"))]
    Exists,
}

// Read/write callbacks
// The refcon is a pointer to the data

/// Default read callback for single sized item that is `Copy`.
unsafe extern "C-unwind" fn read_single<T: DataType + Copy>(refcon: *mut c_void) -> T {
    let data_ptr = refcon.cast::<T>();
    unsafe { *data_ptr }
}

/// Default write callback for single sized item that is `Copy`.
unsafe extern "C-unwind" fn write_single<T: DataType + Copy>(refcon: *mut c_void, value: T) {
    let data_ptr = refcon.cast::<T>();
    unsafe {
        *data_ptr = value;
    }
}

/// Byte array read callback
unsafe extern "C-unwind" fn byte_array_read(
    refcon: *mut c_void,
    values: *mut c_void,
    offset: c_int,
    max: c_int,
) -> c_int {
    unsafe { array_read::<u8>(refcon, values.cast::<u8>(), offset, max) }
}

/// Byte array write callback
unsafe extern "C-unwind" fn byte_array_write(
    refcon: *mut c_void,
    values: *mut c_void,
    offset: c_int,
    max: c_int,
) {
    unsafe {
        array_write::<u8>(refcon, values.cast::<u8>(), offset, max);
    }
}

/// If values is null, returns the length of this dataref.
/// Otherwise, reads up to max elements from this dataref starting at offset offset and copies them
/// into values.
#[inline]
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]
unsafe extern "C-unwind" fn array_read<T: Copy>(
    refcon: *mut c_void,
    values: *mut T,
    offset: c_int,
    max: c_int,
) -> c_int {
    let offset = offset as usize;
    let max = max as usize;
    let dataref_content = refcon as *const Vec<T>;
    let dataref_length = unsafe { (*dataref_content).len() };
    if values.is_null() {
        dataref_length as c_int
    } else {
        // Check that offset is within dataref content
        if offset >= dataref_length {
            return 0;
        }
        let dataref_offset = unsafe { (*dataref_content).as_ptr().add(offset) };
        let copy_length = cmp::min(max, dataref_length - offset);
        unsafe {
            ptr::copy_nonoverlapping(dataref_offset, values, copy_length);
        }
        copy_length as c_int
    }
}

/// Reads up to max items from values and writes them to this dataref, starting at offset offset
#[inline]
#[allow(clippy::cast_sign_loss)]
unsafe extern "C-unwind" fn array_write<T: Copy>(
    refcon: *mut c_void,
    values: *mut T,
    offset: c_int,
    max: c_int,
) {
    let offset = offset as usize;
    let max = max as usize;
    let dataref_content = refcon.cast::<Vec<T>>();
    let dataref_length = unsafe { (*dataref_content).len() };

    if offset >= dataref_length {
        return;
    }
    let dataref_offset = unsafe { (*dataref_content).as_mut_ptr().add(offset) };
    let copy_length = cmp::min(max, dataref_length - offset);
    unsafe {
        ptr::copy_nonoverlapping(values, dataref_offset, copy_length);
    }
}
