// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::accessibility_trusted`] and [`super::System::request_accessibility`] on the
//! live system: macOS's Accessibility trust for the app it holds responsible for this process
//! (`AXIsProcessTrusted`, and `AXIsProcessTrustedWithOptions` with the prompt option, which
//! shows macOS's own consent dialog), read in this process. The frameworks are opened at the
//! first look (`dlopen`), so a run that never asks loads nothing more; neither call reaches
//! WindowServer.

use std::ffi::{CStr, c_char, c_int, c_void};

/// HIServices, through its umbrella framework.
const APPLICATION_SERVICES: &CStr =
    c"/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices";

/// CoreFoundation, for the request's options dictionary.
const CORE_FOUNDATION: &CStr =
    c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation";

/// `<dlfcn.h>`'s `RTLD_LAZY` on Darwin.
const RTLD_LAZY: c_int = 0x1;

// SAFETY: libSystem's `dlopen`, with `<dlfcn.h>`'s prototype; aterm's `libc` declares
// `dlsym` but not it.
unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
}

/// `path`'s handle, kept for the process's life (never closed), or `None`.
fn open(path: &CStr) -> Option<*mut c_void> {
    // SAFETY: a NUL-terminated path of a system framework in the shared cache.
    let handle = unsafe { dlopen(path.as_ptr(), RTLD_LAZY) };
    (!handle.is_null()).then_some(handle)
}

/// `name` in `handle`, or `None`.
fn symbol(handle: *mut c_void, name: &CStr) -> Option<*mut c_void> {
    // SAFETY: a live handle and a NUL-terminated name.
    let found = unsafe { libc::dlsym(handle, name.as_ptr()) };
    (!found.is_null()).then_some(found)
}

/// `AXIsProcessTrusted()`: whether the responsible app holds Accessibility, with no prompt.
pub(super) fn trusted() -> Option<bool> {
    let ax = open(APPLICATION_SERVICES)?;
    let found = symbol(ax, c"AXIsProcessTrusted")?;
    // SAFETY: HIServices' `Boolean AXIsProcessTrusted(void)` (`Boolean` is an unsigned char);
    // a function pointer and a data pointer share their width on every Mac.
    let is_trusted =
        unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> u8>(found) };
    // SAFETY: the function takes nothing and only reads this process's trust.
    Some(unsafe { is_trusted() } != 0)
}

/// `AXIsProcessTrustedWithOptions({kAXTrustedCheckOptionPrompt: true})`: macOS shows its
/// consent dialog for the responsible app when it holds no Accessibility yet, and answers the
/// trust as it stands (the dialog does not wait for the person).
pub(super) fn request() -> Option<bool> {
    let ax = open(APPLICATION_SERVICES)?;
    let cf = open(CORE_FOUNDATION)?;
    type Create = unsafe extern "C" fn(
        *const c_void,
        *const *const c_void,
        *const *const c_void,
        isize,
        *const c_void,
        *const c_void,
    ) -> *const c_void;
    let with_options = symbol(ax, c"AXIsProcessTrustedWithOptions")?;
    let prompt = symbol(ax, c"kAXTrustedCheckOptionPrompt")?.cast::<*const c_void>();
    let yes = symbol(cf, c"kCFBooleanTrue")?.cast::<*const c_void>();
    let key_callbacks = symbol(cf, c"kCFTypeDictionaryKeyCallBacks")?;
    let value_callbacks = symbol(cf, c"kCFTypeDictionaryValueCallBacks")?;
    let create = symbol(cf, c"CFDictionaryCreate")?;
    let release = symbol(cf, c"CFRelease")?;
    // SAFETY: each symbol is the framework's export of that name: `kAXTrustedCheckOptionPrompt`
    // and `kCFBooleanTrue` are constant object pointers read through their addresses, the two
    // callback tables are passed by address as `CFDictionaryCreate` takes them, and the
    // functions have the C prototypes they are transmuted to (function and data pointers share
    // their width). The dictionary is a +1 object released exactly once after its one use.
    unsafe {
        let with_options = std::mem::transmute::<
            *mut c_void,
            unsafe extern "C" fn(*const c_void) -> u8,
        >(with_options);
        let create = std::mem::transmute::<*mut c_void, Create>(create);
        let release =
            std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*const c_void)>(release);
        let keys = [*prompt];
        let values = [*yes];
        let options = create(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            key_callbacks,
            value_callbacks,
        );
        if options.is_null() {
            return None;
        }
        let trusted = with_options(options) != 0;
        release(options);
        Some(trusted)
    }
}

#[cfg(test)]
mod tests {
    /// The frameworks open and every function and constant the two calls take resolves; the
    /// calls themselves read this Mac's privacy grants, which a test leaves alone.
    #[test]
    fn every_symbol_the_calls_take_resolves() {
        let ax = super::open(super::APPLICATION_SERVICES).unwrap();
        let cf = super::open(super::CORE_FOUNDATION).unwrap();
        for name in [
            c"AXIsProcessTrusted",
            c"AXIsProcessTrustedWithOptions",
            c"kAXTrustedCheckOptionPrompt",
        ] {
            assert!(super::symbol(ax, name).is_some(), "{name:?}");
        }
        for name in [
            c"kCFBooleanTrue",
            c"kCFTypeDictionaryKeyCallBacks",
            c"kCFTypeDictionaryValueCallBacks",
            c"CFDictionaryCreate",
            c"CFRelease",
        ] {
            assert!(super::symbol(cf, name).is_some(), "{name:?}");
        }
        assert!(super::symbol(ax, c"AXNoSuchFunction").is_none());
    }
}
