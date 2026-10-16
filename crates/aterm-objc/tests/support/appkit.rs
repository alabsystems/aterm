// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AppKit as the AUTHORITY `winit_seam.rs`'s declaration census consults.

use std::ffi::{CString, c_char, c_int, c_void};

unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
}

/// `RTLD_LAZY`.
const RTLD_LAZY: c_int = 0x1;

/// Load AppKit into this test process, once.
///
/// Returns whether it is present. On a machine without AppKit the census has no
/// authority to consult and says so loudly rather than passing vacuously.
pub fn load_appkit() -> bool {
    use std::sync::OnceLock;
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED.get_or_init(|| {
        let path = CString::new("/System/Library/Frameworks/AppKit.framework/AppKit")
            .expect("no interior NUL");
        // SAFETY: `dlopen` with a valid C path and `RTLD_LAZY`; the handle is
        // deliberately never closed — AppKit is process-lifetime once loaded,
        // and the classes and protocols it registers are immortal.
        let handle = unsafe { dlopen(path.as_ptr(), RTLD_LAZY) };
        !handle.is_null()
    })
}
