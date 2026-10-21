// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The narrow native boundary for macOS's Natural scrolling setting.
//!
//! PRIVATE API, scoped to explicit `aterm init`: macOS 27.0.1's TrackpadExtension
//! imports `swipeScrollDirection` and `setSwipeScrollDirection` from
//! PreferencePanesSupport. Its arm64e caller compares the getter's x0 with 1 and
//! passes a zero/one integer in x0 to the setter, ignoring its return. The same
//! extension observes `SwipeScrollDirectionDidChangeNotification` through the
//! distributed notification center. These exports also appear in the installed
//! SDK's PreferencePanesSupport.tbd. Dynamic loading makes an absent future API an
//! explicit refusal for this setting, without affecting other aterm commands.
//!
//! Cache disassembly confirms the setter calls CGSSetSwipeScrollDirection, saves
//! and synchronizes the preference, then posts the distributed notification. The
//! getter synchronizes and reads CFPreferences, so its result must not be treated
//! as an independent query of WindowServer's active scroll direction.

#[cfg(target_os = "macos")]
mod native {
    use std::ffi::{CStr, c_char, c_int, c_long, c_void};
    use std::sync::OnceLock;

    const FRAMEWORK: &CStr = c"/System/Library/PrivateFrameworks/PreferencePanesSupport.framework/PreferencePanesSupport";
    const RTLD_LAZY_LOCAL: c_int = 0x1 | 0x4;

    // As in screen_lock.rs: these are libSystem's dlfcn.h declarations, absent
    // from aterm-libc's intentionally narrow generated macOS cells.
    unsafe extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    }

    struct Api {
        get: unsafe extern "C" fn() -> c_long,
        set: unsafe extern "C" fn(c_long),
    }

    fn api() -> Result<&'static Api, String> {
        static API: OnceLock<Result<Api, String>> = OnceLock::new();
        API.get_or_init(|| {
            // SAFETY: static NUL-terminated framework path and Darwin dlfcn flags.
            // The framework remains loaded while the cached function pointers exist.
            let handle = unsafe { dlopen(FRAMEWORK.as_ptr(), RTLD_LAZY_LOCAL) };
            if handle.is_null() {
                return Err("macOS's native scroll-direction framework is unavailable".into());
            }
            // SAFETY: live framework handle and static NUL-terminated export names.
            let getter = unsafe { libc::dlsym(handle, c"swipeScrollDirection".as_ptr()) };
            // SAFETY: the same retained handle and a static NUL-terminated name.
            let setter = unsafe { libc::dlsym(handle, c"setSwipeScrollDirection".as_ptr()) };
            if getter.is_null() || setter.is_null() {
                return Err(
                    "macOS's native scroll-direction getter or setter is unavailable".into(),
                );
            }
            // SAFETY: signatures match the integer argument/result ABI of Apple's
            // TrackpadExtension calls described above. Only values zero and one
            // cross this boundary; no pointer or aggregate crosses it.
            let get = unsafe {
                std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> c_long>(getter)
            };
            // SAFETY: the setter's single integer argument and ignored result match
            // Apple's TrackpadExtension call; the framework stays loaded.
            let set =
                unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn(c_long)>(setter) };
            Ok(Api { get, set })
        })
        .as_ref()
        .map_err(Clone::clone)
    }

    pub(super) fn get() -> Result<bool, String> {
        let api = api()?;
        // SAFETY: a resolved no-argument getter in the retained Apple framework.
        match unsafe { (api.get)() } {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(format!(
                "native scroll-direction getter returned unknown value {other}"
            )),
        }
    }

    pub(super) fn set(natural: bool) -> Result<(), String> {
        let api = api()?;
        // SAFETY: a resolved one-integer setter in the retained Apple framework;
        // this is an explicit local init in the resolved console user's session.
        unsafe { (api.set)(c_long::from(natural)) };
        if get()? != natural {
            return Err("native scroll direction did not match after applying the setting".into());
        }
        Ok(())
    }
}

pub(super) fn get() -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    return native::get();
    #[cfg(not(target_os = "macos"))]
    Err("native scroll direction requires macOS".into())
}

pub(super) fn set(natural: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return native::set(natural);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = natural;
        Err("native scroll direction requires macOS".into())
    }
}
