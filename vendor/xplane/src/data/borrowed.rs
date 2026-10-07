// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::ffi::c_void;
use std::{
    ffi::{CString, NulError},
    fmt::Debug,
    marker::PhantomData,
    ptr,
};

use snafu::prelude::*;

use xplane_sys::{
    XPLMCanWriteDataRef, XPLMDataRef, XPLMFindDataRef, XPLMGetDataRefTypes, XPLMGetDatab,
    XPLMGetDatad, XPLMGetDataf, XPLMGetDatai, XPLMGetDatavf, XPLMGetDatavi, XPLMSetDatab,
    XPLMSetDatad, XPLMSetDataf, XPLMSetDatai, XPLMSetDatavf, XPLMSetDatavi,
};

use super::{ArrayRead, ArrayReadWrite, DataRead, DataReadWrite, DataType, ReadOnly, ReadWrite};

/// A dataref created by X-Plane or another plugin
///
/// T is the data type stored in the dataref.
///
/// A is the access level (`ReadOnly` or `ReadWrite`)
pub struct DataRef<T: ?Sized, A = ReadOnly> {
    /// The dataref handle
    pub(super) id: XPLMDataRef,
    /// Type and data access phantom data
    pub(super) _phantom: PhantomData<(*mut (), A, T)>,
}

impl<T: DataType + ?Sized> DataRef<T, ReadOnly> {
    pub(super) fn find<S: AsRef<str>>(name: S) -> Result<Self, FindError> {
        let name = name.as_ref();
        let name_c = CString::new(name)?;
        let expected_type = T::sim_type();

        let dataref = unsafe { XPLMFindDataRef(name_c.as_ptr()) };
        if dataref.is_null() {
            return Err(FindError::NotFound);
        }

        let actual_type = unsafe { XPLMGetDataRefTypes(dataref) };
        if !type_matches(actual_type, expected_type) {
            Err(FindError::WrongType)
        } else {
            Ok(DataRef {
                id: dataref,
                _phantom: PhantomData,
            })
        }
    }

    /// Makes this dataref writable
    /// # Errors
    /// Returns Err(self) if the dataref cannot be written.
    pub fn writeable(self) -> Result<DataRef<T, ReadWrite>, Self> {
        let writable = unsafe { XPLMCanWriteDataRef(self.id) == 1 };
        if writable {
            Ok(DataRef {
                id: self.id,
                _phantom: PhantomData,
            })
        } else {
            Err(self)
        }
    }
}

#[allow(clippy::missing_fields_in_debug)]
impl<T: ?Sized, A> Debug for DataRef<T, A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataRef")
            .field("id", &"[dataref handle]")
            .finish()
    }
}

/// Creates a `DataType` implementation, `DataRef::get` and `DataRef::set` for a type
macro_rules! dataref_type {
    // Array case
    (
        $(#[$meta:meta])*
        native [$native_type:ty];
        sim $sim_type:ident as [$sim_native_type:ty];
        $(#[$read_meta:meta])*
        read $read_fn:ident;
        $(#[$write_meta:meta])*
        write $write_fn:ident;
    ) => {
        impl<A> ArrayRead<[$native_type]> for DataRef<[$native_type], A> {
            fn get(&self, dest: &mut [$native_type]) -> usize {
                let size = array_size(dest.len());
                let copy_count = unsafe {
                    $read_fn(
                        self.id,
                        dest.as_mut_ptr().cast::<$sim_native_type>(),
                        0,
                        size,
                    )
                };
                copy_count.try_into().unwrap()
            }
            fn len(&self) -> usize {
                let size = unsafe { $read_fn(self.id, ptr::null_mut(), 0, 0) };
                size.try_into().unwrap()
            }
        }

        impl ArrayReadWrite<[$native_type]> for DataRef<[$native_type], ReadWrite> {
            fn set(&mut self, values: &[$native_type]) {
                let size = array_size(values.len());
                unsafe {
                    // Cast to *mut because the API requires it
                    $write_fn(
                        self.id,
                        values.as_ptr().cast::<$sim_native_type>().cast_mut(),
                        0,
                        size,
                    );
                }
            }
        }
    };
    // Basic case
    (
        $(#[$meta:meta])*
        native $native_type:ty;
        sim $sim_type:ident as $sim_native_type:ty;
        read $read_fn:ident;
        write $write_fn:ident;
    ) => {
        impl<A> DataRead<$native_type> for DataRef<$native_type, A> {
            fn get(&self) -> $native_type {
                unsafe { $read_fn(self.id) as $native_type }
            }
        }
        impl DataReadWrite<$native_type> for DataRef<$native_type, ReadWrite> {
            fn set(&mut self, value: $native_type) {
                unsafe { $write_fn(self.id, value as $sim_native_type) }
            }
        }
    };
}

dataref_type! {
    native u8;
    sim xplmType_Int as i32;
    read XPLMGetDatai;
    write XPLMSetDatai;
}

dataref_type! {
    native i8;
    sim xplmType_Int as i32;
    read XPLMGetDatai;
    write XPLMSetDatai;
}

dataref_type! {
    native u16;
    sim xplmType_Int as i32;
    read XPLMGetDatai;
    write XPLMSetDatai;
}

dataref_type! {
    native i16;
    sim xplmType_Int as i32;
    read XPLMGetDatai;
    write XPLMSetDatai;

}

dataref_type! {
    native u32;
    sim xplmType_Int as i32;
    read XPLMGetDatai;
    write XPLMSetDatai;
}

dataref_type! {
    native i32;
    sim xplmType_Int as i32;
    read XPLMGetDatai;
    write XPLMSetDatai;
}

dataref_type! {
    native f32;
    sim xplmType_Float as f32;
    read XPLMGetDataf;
    write XPLMSetDataf;
}

dataref_type! {
    native f64;
    sim xplmType_Double as f64;
    read XPLMGetDatad;
    write XPLMSetDatad;
}

dataref_type! {
    native [i32];
    sim xplmType_IntArray as [i32];
    read XPLMGetDatavi;
    write XPLMSetDatavi;
}

dataref_type! {
    native [u32];
    sim xplmType_IntArray as [i32];
    read XPLMGetDatavi;
    write XPLMSetDatavi;
}

dataref_type! {
    native [f32];
    sim xplmType_FloatArray as [f32];
    read XPLMGetDatavf;
    write XPLMSetDatavf;
}

dataref_type! {
    native [u8];
    sim xplmType_Data as [c_void];
    read XPLMGetDatab;
    write XPLMSetDatab;
}

dataref_type! {
    native [i8];
    sim xplmType_Data as [c_void];
    read XPLMGetDatab;
    write XPLMSetDatab;
}

impl<A> DataRead<bool> for DataRef<bool, A> {
    fn get(&self) -> bool {
        let int_value = unsafe { XPLMGetDatai(self.id) };
        int_value != 0
    }
}

impl DataReadWrite<bool> for DataRef<bool, ReadWrite> {
    fn set(&mut self, value: bool) {
        let int_value = i32::from(value);
        unsafe {
            XPLMSetDatai(self.id, int_value);
        }
    }
}

/// Converts a usize into an i32. Returns `i32::MAX` if the provided size is too large for an i32
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
fn array_size(size: usize) -> i32 {
    if size > (i32::MAX as usize) {
        i32::MAX
    } else {
        size as i32
    }
}

/// Errors that can occur when finding `DataRef`s
#[derive(Snafu, Debug)]
pub enum FindError {
    /// The provided DataRef name contained a null byte
    #[snafu(display("Null byte in DataRef name"))]
    #[snafu(context(false))]
    Null {
        /// The source error.
        source: NulError,
    },

    /// The DataRef could not be found
    #[snafu(display("DataRef not found"))]
    NotFound,

    /// The DataRef is not writable
    #[snafu(display("DataRef not writable"))]
    NotWritable,

    /// The DataRef does not have the correct type
    #[snafu(display("Incorrect DataRef type"))]
    WrongType,
}

#[cfg(test)]
mod tests {
    /// Checks that the as operator truncates values
    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn test_as_truncate() {
        let x = 0x1122_3344_u32;
        let x8 = x as u8;
        assert_eq!(x8, 0x44u8);
    }
}

// Keep the SDK bitmask check explicit: supported accessors must be accepted.
fn type_matches(actual: xplane_sys::XPLMDataTypeID, expected: xplane_sys::XPLMDataTypeID) -> bool {
    actual & expected == expected
}
#[cfg(test)]
mod type_tests {
    use super::*;
    #[test]
    fn supported_scalars_and_arrays_are_accepted() {
        use xplane_sys::XPLMDataTypeID as T;
        for kind in [T::Int, T::Float, T::Double, T::IntArray, T::FloatArray] {
            assert!(type_matches(kind, kind));
        }
        assert!(type_matches(T::Float | T::Double, T::Double));
        assert!(!type_matches(T::Int, T::Float));
        assert!(!type_matches(T::FloatArray, T::Float));
    }
}

#[cfg(test)]
mod sdk_regression {
    use super::*;
    #[test]
    fn integer_radio_resolves_and_tunes_through_sdk_accessors() {
        let find = xplane_sys::XPLMFindDataRef_context();
        find.expect()
            .times(2)
            .returning_st(|_| std::ptr::dangling_mut::<std::ffi::c_void>());
        let types = xplane_sys::XPLMGetDataRefTypes_context();
        types
            .expect()
            .times(2)
            .return_const(xplane_sys::XPLMDataTypeID::Int);
        let writable = xplane_sys::XPLMCanWriteDataRef_context();
        writable.expect().once().return_const(1);
        let write = xplane_sys::XPLMSetDatai_context();
        write
            .expect()
            .withf(|_, channel| *channel == 121900)
            .once()
            .return_const(());
        let read = xplane_sys::XPLMGetDatai_context();
        read.expect().once().return_const(121900);
        let mut radio = DataRef::<i32>::find("sim/cockpit2/radios/actuators/com1_frequency_hz_833")
            .unwrap()
            .writeable()
            .unwrap();
        radio.set(121900);
        assert_eq!(radio.get(), 121900);
        assert!(matches!(
            DataRef::<f32>::find("sim/cockpit2/radios/actuators/com1_frequency_hz_833"),
            Err(FindError::WrongType)
        ));
    }
}
