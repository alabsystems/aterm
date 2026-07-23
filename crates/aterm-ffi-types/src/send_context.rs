// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Wrapper to make a raw C context pointer `Send`-safe for callback closures.
//!
//! # Safety Contract
//!
//! The C caller owns the context pointer and must keep it thread-safe for the
//! lifetime of the registered callback. This is the same contract asserted by
//! the single-callback wrappers across aterm — consolidating it here means one
//! place to audit instead of multiple near-duplicates (#5697 Phase 1).

use std::ffi::c_void;

/// A raw `*mut c_void` wrapper that is `Send`-safe for FFI callback closures.
///
/// The inner field is public to allow tuple-struct construction (`SendContext(ptr)`)
/// at call sites, matching the established pattern in both `aterm-core` and
/// `aterm-core-ffi`.
pub struct SendContext(pub *mut c_void);

// SAFETY: The C caller owns the context pointer and must keep it thread-safe
// for the lifetime of the registered callback. This is the same contract
// previously asserted independently in `aterm-core::ffi::app_callbacks::SendContext`
// and `aterm-core-ffi::callbacks::send_context::SendContext`.
unsafe impl Send for SendContext {}

impl SendContext {
    /// Return the stored context pointer.
    #[must_use]
    pub fn as_ptr(&self) -> *mut c_void {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_context_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<SendContext>();
    }

    #[test]
    fn as_ptr_returns_stored_pointer() {
        let ptr = 0xDEAD_BEEF as *mut c_void;
        let ctx = SendContext(ptr);
        assert_eq!(ctx.as_ptr(), ptr);
    }

    #[test]
    fn null_context_is_valid() {
        let ctx = SendContext(std::ptr::null_mut());
        assert!(ctx.as_ptr().is_null());
    }
}
