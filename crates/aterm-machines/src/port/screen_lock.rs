// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::lock_screen`]'s live arm: loginwindow's Lock Screen, asked for through
//! login.framework (design §6).
//!
//! PRIVATE API, a sanctioned exception: `SACLockScreenImmediate`, exported by
//! `/System/Library/PrivateFrameworks/login.framework`, is the call macOS's own
//! ScreensharingAgent makes for this exact job. When a Screen Sharing connection that found
//! the session locked closes, the guest log shows it ask loginwindow `SACScreenLockEnabled`
//! and then call `SACLockScreenImmediate` (VM, 27.0). No public interface locks a session:
//! `pmset displaysleepnow` only sleeps the display, which locked nothing with the screen lock
//! at immediate and raised a shield a wake dismissed with it off, and `CGSession -suspend` is
//! gone from 27 (§11). This call locked with the screen lock at immediate and with it off,
//! and the lock held through a wake both times (loginwindow: `promptForPassword = YES`).
//!
//! It reaches loginwindow only from inside the login session it locks: run from root's own
//! bootstrap it answered 22 and nothing locked, run under `launchctl asuser` it answered 0
//! and the session locked (VM, 27.0). The framework is opened at run time, never linked, so
//! no other aterm process loads it, and a macOS without the symbol fails this one call with
//! a line rather than failing aterm's launch.

use std::ffi::{CStr, c_char, c_int, c_void};

/// login.framework's binary (in the shared cache; `dlopen` finds it there).
const LOGIN_FRAMEWORK: &CStr = c"/System/Library/PrivateFrameworks/login.framework/login";

/// The call: no argument, an `int` back, 0 when loginwindow took the lock.
const LOCK_SCREEN_IMMEDIATE: &CStr = c"SACLockScreenImmediate";

/// `<dlfcn.h>` on Darwin: bind on first use, keep the framework's symbols out of the global
/// namespace.
const RTLD_LAZY: c_int = 0x1;
const RTLD_LOCAL: c_int = 0x4;

// aterm-libc's macOS cells declare `dlsym` but not `dlopen` or `dlerror`: a cell carries
// only the names the tree used when it was generated, and regenerating one needs every
// cell's toolchain. These are libSystem's, with `<dlfcn.h>`'s signatures.
unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlerror() -> *mut c_char;
}

/// Lock this process's login session now.
///
/// # Errors
/// One line: the framework or the symbol is missing, or loginwindow answered nonzero (22
/// from outside a login session).
pub(super) fn lock_screen_immediate() -> Result<(), String> {
    // SAFETY: a NUL-terminated path and a valid mode. The handle is never closed: the
    // framework stays loaded for the rest of this short-lived process.
    let handle = unsafe { dlopen(LOGIN_FRAMEWORK.as_ptr(), RTLD_LAZY | RTLD_LOCAL) };
    if handle.is_null() {
        return Err(format!(
            "{}: {}",
            LOGIN_FRAMEWORK.to_string_lossy(),
            last_error()
        ));
    }
    // SAFETY: a live handle and a NUL-terminated name; null when the symbol is absent.
    let symbol = unsafe { libc::dlsym(handle, LOCK_SCREEN_IMMEDIATE.as_ptr()) };
    if symbol.is_null() {
        return Err(format!(
            "{} has no {}: {}",
            LOGIN_FRAMEWORK.to_string_lossy(),
            LOCK_SCREEN_IMMEDIATE.to_string_lossy(),
            last_error()
        ));
    }
    // SAFETY: the address of `SACLockScreenImmediate`, which takes no argument and returns
    // an `int` (ScreensharingAgent's use; on 27 it answered 0 and 22, measured).
    let lock =
        unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> c_int>(symbol) };
    // SAFETY: the C call above; it only sends loginwindow a request.
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
    // SAFETY: non-null, NUL-terminated, and read before any other dl call on this thread.
    unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned()
}
