// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one Darwin loader lookup declaration, shared by wait sampling and sleeping-thread reads.

use std::ffi::{c_char, c_void};

/// Darwin's `Dl_info` ABI. Every pointer is borrowed from the loader; dereferencing one requires
/// that its image remains loaded. This type owns none of the memory it names.
#[repr(C)]
pub(crate) struct DlInfo {
    pub(crate) fname: *const c_char,
    pub(crate) fbase: *mut c_void,
    pub(crate) sname: *const c_char,
    pub(crate) saddr: *mut c_void,
}

unsafe extern "C" {
    fn dladdr(address: *const c_void, info: *mut DlInfo) -> i32;
}

/// Look up an address in the loader's tables without dereferencing it. A found record's raw
/// pointers retain the loader's lifetime; callers must keep the image loaded before reading
/// its name, symbol or mapped header.
pub(crate) fn lookup(address: *const c_void) -> Option<DlInfo> {
    let mut info = DlInfo {
        fname: std::ptr::null(),
        fbase: std::ptr::null_mut(),
        sname: std::ptr::null(),
        saddr: std::ptr::null_mut(),
    };
    // SAFETY: dladdr searches the loader's tables for the numeric address without reading
    // memory at it, and `info` is a live, correctly laid-out output record.
    (unsafe { dladdr(address, &raw mut info) } != 0).then_some(info)
}
