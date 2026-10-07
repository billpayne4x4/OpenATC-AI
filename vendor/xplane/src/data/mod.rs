// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com>
//
// SPDX-License-Identifier: MPL-2.0

use std::{
    ffi::{CString, NulError},
    string::FromUtf8Error,
};

use xplane_sys::XPLMDataTypeID;

use crate::{
    data::shared::{SharedData, SharedDataError, SharedDataHandler},
    ffi::StringBuffer,
    NoSendSync,
};

use self::{
    borrowed::{DataRef, FindError},
    owned::{CreateError, OwnedData},
};

/// Datarefs created by X-Plane or other plugins
pub mod borrowed;
/// Datarefs created by this plugin
pub mod owned;
/// Datarefs shared between plugins.
pub mod shared;

/// Marks a dataref as readable
pub enum ReadOnly {}

/// Marks a dataref as writeable
pub enum ReadWrite {}

/// Marker for data access types
pub trait Access {
    /// Returns true if this access allows the dataref to be written
    fn writeable() -> bool;
}

impl Access for ReadOnly {
    fn writeable() -> bool {
        false
    }
}

impl Access for ReadWrite {
    fn writeable() -> bool {
        true
    }
}

/// Trait for data accessors that can be read
pub trait DataRead<T> {
    /// Reads a value
    fn get(&self) -> T;
}

/// Trait for writable data accessors
pub trait DataReadWrite<T>: DataRead<T> {
    /// Writes a value
    fn set(&mut self, value: T);
}

/// Trait for readable array data accessors
pub trait ArrayRead<T: ArrayType + ?Sized> {
    /// Reads values
    ///
    /// Values are stored in the provided slice. If the dataref is larger than the
    /// provided slice, values beyond the bounds of the slice are ignored.
    ///
    /// If the dataref is smaller than the provided slice, the extra values in the
    /// slice will not be modified.
    ///
    /// The maximum number of values in an array dataref is `i32::MAX`.
    ///
    /// This function returns the number of values that were read.
    ///
    /// # Panics
    /// If the return value is outside of the range of [`usize`], this function
    /// will panic.
    /// This should not be possible.
    fn get(&self, dest: &mut [T::Element]) -> usize;

    /// Returns the length of the data array
    ///
    /// # Panics
    /// If the dataref array length is outside the range of [`usize`], this function
    /// will panic.
    /// This should not be possible.
    fn len(&self) -> usize;

    /// Returns whether the data array is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns all values in this accessor as a Vec
    fn as_vec(&self) -> Vec<T::Element>
    where
        T::Element: Default + Clone,
    {
        let mut values = vec![T::Element::default(); self.len()];
        self.get(&mut values);
        values
    }
}

/// Trait for array accessors that can be read and written
pub trait ArrayReadWrite<T: ArrayType + ?Sized>: ArrayRead<T> {
    /// Writes values
    ///
    /// Values are taken from the provided slice. If the dataref is larger than
    /// the provided slice, values beyond the bounds of the slice are not changed.
    ///
    /// If the dataref is smaller than the provided slice, the values beyond the
    /// dataref bounds will be ignored.
    fn set(&mut self, values: &[T::Element]);
}

/// Trait for data accessors that can be read as strings
pub trait StringRead {
    /// Reads the value of this dataref and appends it to the provided string
    ///
    /// If the provided string is not empty, the value of the dataref will be
    /// appended to it.
    ///
    /// # Errors
    /// Returns an error if the dataref is not valid UTF-8.
    fn get_to_string(&self, out: &mut String) -> Result<(), FromUtf8Error>;

    /// Reads the value of this dataref as a string and returns it.
    ///
    /// # Errors
    /// Returns an error if the dataref is not valid UTF-8.
    fn get_as_string(&self) -> Result<String, FromUtf8Error>;
}

/// Trait for data accessors that can be written as strings
pub trait StringReadWrite: StringRead {
    /// Sets the value of this dataref from a string.
    ///
    /// # Errors
    /// Returns an error if the string contains a NUL byte
    fn set_as_string(&mut self, value: &str) -> Result<(), NulError>;
}

impl<T> StringRead for T
where
    T: ArrayRead<[u8]>,
{
    fn get_to_string(&self, out: &mut String) -> Result<(), FromUtf8Error> {
        let mut buffer = StringBuffer::new(self.len());
        self.get(buffer.as_bytes_mut());
        let value_string = buffer.into_string()?;
        out.push_str(&value_string);
        Ok(())
    }
    fn get_as_string(&self) -> Result<String, FromUtf8Error> {
        let mut buffer = StringBuffer::new(self.len());
        self.get(buffer.as_bytes_mut());
        buffer.into_string()
    }
}

impl<T> StringReadWrite for T
where
    T: ArrayReadWrite<[u8]>,
{
    fn set_as_string(&mut self, value: &str) -> Result<(), NulError> {
        let name_c = CString::new(value)?;
        self.set(name_c.as_bytes_with_nul());
        Ok(())
    }
}

/// Marker for types that can be used with datarefs
pub trait DataType {
    /// The type that should be used to store data of this type
    /// For basic types, this is usually Self. For [T] types, this is Vec<T>.
    #[doc(hidden)]
    type Storage: Sized;
    /// Returns the X-Plane data type corresponding with this type
    #[doc(hidden)]
    fn sim_type() -> XPLMDataTypeID;
    /// Creates an instance of a storage type from an instance of self
    #[doc(hidden)]
    fn to_storage(&self) -> Self::Storage;
}

/// Marker for types that are arrays
pub trait ArrayType: DataType {
    /// The type of the array element
    type Element;
}

macro_rules! impl_type {
    ([$native_type:ty] as $sim_type:path) => {
        impl DataType for [$native_type] {
            type Storage = Vec<$native_type>;
            fn sim_type() -> XPLMDataTypeID {
                $sim_type
            }
            fn to_storage(&self) -> Self::Storage {
                self.to_vec()
            }
        }
        impl ArrayType for [$native_type] {
            type Element = $native_type;
        }
    };
    ($native_type:ty as $sim_type:path) => {
        impl DataType for $native_type {
            type Storage = Self;
            fn sim_type() -> XPLMDataTypeID {
                $sim_type
            }
            fn to_storage(&self) -> Self::Storage {
                self.clone()
            }
        }
    };
}

impl_type!(bool as XPLMDataTypeID::Int);
impl_type!(u8 as XPLMDataTypeID::Int);
impl_type!(i8 as XPLMDataTypeID::Int);
impl_type!(u16 as XPLMDataTypeID::Int);
impl_type!(i16 as XPLMDataTypeID::Int);
impl_type!(u32 as XPLMDataTypeID::Int);
impl_type!(i32 as XPLMDataTypeID::Int);
impl_type!(f32 as XPLMDataTypeID::Float);
impl_type!(f64 as XPLMDataTypeID::Double);
impl_type!([i32] as XPLMDataTypeID::IntArray);
impl_type!([u32] as XPLMDataTypeID::IntArray);
impl_type!([f32] as XPLMDataTypeID::FloatArray);
impl_type!([u8] as XPLMDataTypeID::Data);
impl_type!([i8] as XPLMDataTypeID::Data);

/// Access struct for X-Plane's data APIs.
pub struct DataApi {
    pub(crate) _phantom: NoSendSync,
}

impl DataApi {
    /// Finds a readable dataref by its name.
    /// # Errors
    /// Returns an error if the dataref does not exist or has the wrong type
    pub fn find<T: DataType + ?Sized, S: AsRef<str>>(
        &mut self,
        name: S,
    ) -> Result<DataRef<T, ReadOnly>, FindError> {
        DataRef::find(name)
    }

    /// Creates a new dataref with the provided name containing the default value of `T`.
    /// # Errors
    /// Errors if there is a NUL character in the dataref name, or if a dataref with that name already exists.
    pub fn new_owned<T: DataType + Default + ?Sized, A: Access, S: AsRef<str>>(
        &mut self,
        name: S,
    ) -> Result<OwnedData<T, A>, CreateError> {
        OwnedData::new_with_value(name, &T::default())
    }

    /// Creates a new dataref with the provided name and value.
    /// # Errors
    /// Errors if there is a NUL character in the dataref name, or if a dataref with that name already exists.
    /// # Panics
    /// Panics if the dataref ID returned from X-Plane is null. This should not occur.
    pub fn new_owned_with_value<T: DataType + ?Sized, A: Access, S: AsRef<str>>(
        &mut self,
        name: S,
        value: &T,
    ) -> Result<OwnedData<T, A>, CreateError> {
        OwnedData::new_with_value(name, value)
    }

    /// Creates a new [`SharedData<T>`].
    /// The function in your handler will be called every time the dataref's value changes.
    /// # Errors
    /// Returns an error if the dataref name contains a NUL byte, or if the type does not
    /// match the existing dataref of that name.
    pub fn new_shared<S: Into<Vec<u8>>, T: DataType + ?Sized + 'static>(
        &mut self,
        name: S,
        handler: impl SharedDataHandler<T>,
    ) -> Result<SharedData<T>, SharedDataError> {
        SharedData::new(name, handler)
    }
}
