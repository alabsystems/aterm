// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS dynamic-loader boundary, checked against the SDK's dlfcn.h.

use core::ffi::{c_char, c_int, c_void};

/// Bind symbols lazily.
pub const RTLD_LAZY: c_int = 0x1;
/// Keep the opened library out of the process-wide default symbol scope.
pub const RTLD_LOCAL: c_int = 0x4;

unsafe extern "C" {
    /// Open a library by pathname.
    pub fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    /// Look up a symbol through a live library handle.
    pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    /// Read and reset this thread's most recent loader error.
    pub fn dlerror() -> *mut c_char;
}
