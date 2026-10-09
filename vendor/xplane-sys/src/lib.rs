// Copyright (c) 2023 xplm-sys developers
// SPDX-FileCopyrightText: 2024 Julia DeMille <me@jdemille.com
//
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Allow C-like conventions
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]

use std::fmt::Debug;

use core::ffi::c_uint;

use bitfield::{bitfield_bitrange, bitfield_debug, bitfield_fields};

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

bitfield_bitrange! {struct XPLMDataTypeID(c_uint)}
impl XPLMDataTypeID {
    bitfield_fields! {
        c_uint;
        pub int, _ : 0;
        pub float, _ : 1;
        pub double, _ : 2;
        pub float_array, _ : 3;
        pub int_array, _ : 4;
        pub data, _ : 4;
        pub unrecognized_bits, _ : 31, 5;
    }
}

impl Debug for XPLMDataTypeID {
    bitfield_debug! {
        struct XPLMDataTypeID;
        int, _ : 0;
        float, _ : 1;
        double, _ : 2;
        float_array, _ : 3;
        int_array, _ : 4;
        data, _ : 5;
        unrecognized_bits, _ : 31, 6;
    }
}

bitfield_bitrange! {struct XPLMKeyFlags(c_uint)}
impl XPLMKeyFlags {
    bitfield_fields! {
        c_uint;
        pub shift, _ : 0;
        pub option_alt, _ : 1;
        pub ctrl, _ : 2;
        pub down, _ : 3;
        pub up, _ : 4;
        pub unrecognized, _ : 31, 5;
    }
}

impl Debug for XPLMKeyFlags {
    bitfield_debug! {
        struct XPLMKeyFlags;
        shift, _ : 0;
        option_alt, _ : 1;
        ctrl, _ : 2;
        down, _ : 3;
        up, _ : 4;
        unrecognized, _ : 31, 5;
    }
}

bitfield_bitrange! {struct XPLMNavType(c_uint)}
impl XPLMNavType {
    bitfield_fields! {
        c_uint;
        pub airport, _ : 0;
        pub ndb, _ : 1;
        pub vor, _ : 2;
        pub ils, _ : 3;
        pub localizer, _ : 4;
        pub glideslope, _ : 5;
        pub outer_marker, _ : 6;
        pub middle_marker, _ : 7;
        pub inner_marker, _ : 8;
        pub fix, _ : 9;
        pub dme, _ : 10;
        pub lat_lon, _ : 11;
        pub unrecognized, _ : 31, 12;
    }
}

impl Debug for XPLMNavType {
    bitfield_debug! {
        struct XPLMNavType;
        airport, _ : 0;
        ndb, _ : 1;
        vor, _ : 2;
        ils, _ : 3;
        localizer, _ : 4;
        glideslope, _ : 5;
        outer_marker, _ : 6;
        middle_marker, _ : 7;
        inner_marker, _ : 8;
        fix, _ : 9;
        dme, _ : 10;
        lat_lon, _ : 11;
        unrecognized, _ : 31, 12;
    }
}
