// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::lock_screen`]'s live arm via `login.framework`'s `SACLockScreenImmediate`.

use std::ffi::{CStr, c_char, c_int, c_void};

/// login.framework's binary (in the shared cache; `dlopen` finds it there).
const LOGIN_FRAMEWORK: &CStr = c"/System/Library/PrivateFrameworks/login.framework/login";

/// The call: no argument, an `int` back, 0 when loginwindow took the lock.
const LOCK_SCREEN_IMMEDIATE: &CStr = c"SACLockScreenImmediate";

/// `<dlfcn.h>` on Darwin.
const RTLD_LAZY: c_int = 0x1;
const RTLD_LOCAL: c_int = 0x4;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlerror() -> *mut c_char;
}

/// Lock this process's login session now.
/// # Errors
/// One line: the framework or the symbol is missing.
pub(super) fn lock_screen_immediate() -> Result<(), String> {
    // SAFETY: A NUL-terminated path and a valid mode.
    let handle = unsafe { dlopen(LOGIN_FRAMEWORK.as_ptr(), RTLD_LAZY | RTLD_LOCAL) };
    if handle.is_null() {
        return Err(format!(
            "{}: {}",
            LOGIN_FRAMEWORK.to_string_lossy(),
            last_error()
        ));
    }
    // SAFETY: A live handle and a NUL-terminated name; null when the symbol is absent.
    let symbol = unsafe { libc::dlsym(handle, LOCK_SCREEN_IMMEDIATE.as_ptr()) };
    if symbol.is_null() {
        return Err(format!(
            "{} has no {}: {}",
            LOGIN_FRAMEWORK.to_string_lossy(),
            LOCK_SCREEN_IMMEDIATE.to_string_lossy(),
            last_error()
        ));
    }
    // SAFETY: The address of `SACLockScreenImmediate`.
    let lock =
        unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> c_int>(symbol) };
    // SAFETY: The C call above; it only sends loginwindow a request.
    let status = unsafe { lock() };
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "{} answered {status}",
            LOCK_SCREEN_IMMEDIATE.to_string_lossy()
        ))
    }
}

/// `dlerror()`'s message for the last failed call on this thread.
fn last_error() -> String {
    // SAFETY: `dlerror` returns a NUL-terminated message owned by the loader, or null.
    let message = unsafe { dlerror() };
    if message.is_null() {
        return "no reason given".to_owned();
    }
    // SAFETY: Non-null, NUL-terminated, and read before any other dl call on this thread.
    unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned()
}
