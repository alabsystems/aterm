// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure CoreFoundation string comparison; no filesystem, application or WindowServer probe.

use core::ffi::c_void;

// SAFETY: stable CoreFoundation C declarations from CFString.h/CFBase.h; the wrapper owns
// each non-null Create result and passes only live immutable CFStrings to Compare/Release.
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        allocator: *const c_void,
        bytes: *const u8,
        count: isize,
        encoding: u32,
        external: u8,
    ) -> *const c_void;
    fn CFStringCompare(left: *const c_void, right: *const c_void, options: usize) -> isize;
    fn CFRelease(value: *const c_void);
}

struct Text(*const c_void);

impl Text {
    fn new(text: &str) -> Option<Self> {
        let count = isize::try_from(text.len()).ok()?;
        // SAFETY: `text` is live UTF-8 for the call, count is its exact byte length, NULL
        // chooses the default allocator and UTF8/false creates an owned copy, not a borrowed
        // no-copy string. A null result is not wrapped.
        let value = unsafe {
            CFStringCreateWithBytes(core::ptr::null(), text.as_ptr(), count, 0x0800_0100, 0)
        };
        if value.is_null() {
            None
        } else {
            Some(Self(value))
        }
    }
}

impl Drop for Text {
    fn drop(&mut self) {
        // SAFETY: this is the one release of this wrapper's non-null owned Create result.
        unsafe { CFRelease(self.0) };
    }
}

pub(super) fn equivalent(left: &str, right: &str) -> Option<bool> {
    let (left, right) = (Text::new(left)?, Text::new(right)?);
    // SAFETY: both wrappers hold live immutable CFStrings. CaseInsensitive(1) and
    // Nonliteral(16) request canonical case equivalence, without locale/diacritic folding;
    // the comparison retains neither pointer, and both +1 references drop afterwards.
    Some(unsafe { CFStringCompare(left.0, right.0, 1 | 16) } == 0)
}
