// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The narrow native boundary for macOS's Natural scrolling setting.

#[cfg(target_os = "macos")]
mod native {
    use std::ffi::{CStr, c_char, c_int, c_long, c_void};
    use std::sync::OnceLock;

    const FRAMEWORK: &CStr = c"/System/Library/PrivateFrameworks/PreferencePanesSupport.framework/PreferencePanesSupport";
    const RTLD_LAZY_LOCAL: c_int = 0x1 | 0x4;

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
            // SAFETY: signatures match the integer argument/result ABI of Apple's TrackpadExtension
            // calls described above.
            let get = unsafe {
                std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> c_long>(getter)
            };
            // SAFETY: the setter's single integer argument and ignored result match Apple's
            // TrackpadExtension call; the framework stays loaded.
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
        // SAFETY: a resolved one-integer setter in the retained Apple framework; this is an
        // explicit local init in the resolved console user's session.
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
