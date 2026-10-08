// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AppKit as the AUTHORITY the send and declaration censuses consult
//! (`winit_seam.rs`, `winit_sent_prototypes.rs`, `gui_sent_prototypes.rs`),
//! and the encoding a `send_*` helper's NAME implies.

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

/// The Objective-C type encoding a `send_*` helper's name implies, as
/// `<return><self><_cmd><args…>`, spelled token by token with the census's own
/// `token` vocabulary — each census keeps its own, so neither accepts a shape
/// only the other can spell.
///
/// This is the half of the check that makes it a check: the expected string
/// comes from the HELPER the code calls, so a row that names the wrong helper
/// is caught even when the SDK string was transcribed perfectly.
#[allow(dead_code)]
pub fn shape_of(helper: &str, token: fn(&str) -> &'static str) -> String {
    // Return type, then argument types, keyed on the helper's own naming rule:
    // `send_<ret>[_<arg>…]`, where a missing `<ret>` reads as `id`.
    let rest = helper.strip_prefix("send").expect("a send helper");
    let mut parts = rest.split('_').filter(|p| !p.is_empty());
    let ret = parts.next().expect("a return token");
    let mut out = String::from(token(ret));
    out.push_str("@:");
    for a in parts {
        out.push_str(token(a));
    }
    out
}
