// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Generic wrapper for C FFI callback function pointer + context pairs.
//!
//! # Safety Contract
//!
//! The C caller guarantees:
//! 1. The context pointer remains valid for the lifetime of the registration
//! 2. The callback function is safe to call from any thread
//! 3. The context's thread safety is managed by the native layer (e.g.,
//!    dispatch to main thread, mutex protection, or inherent immutability)
//!
//! This contract is the same one asserted by the single-callback wrappers
//! already used across aterm. Consolidating it here means one place to audit
//! instead of 11 near-duplicates (#5697 Phase 0).

use std::ffi::c_void;

/// A C FFI callback function pointer paired with its opaque context.
pub struct FfiCallback<F: Copy> {
    callback: F,
    context: *mut c_void,
}

// SAFETY: Function pointers are code addresses. The thread-safety invariant for
// the opaque context remains delegated to the C caller per the module contract.
unsafe impl<F: Copy + Send> Send for FfiCallback<F> {}

// SAFETY: Same contract. Shared access is only sound when the callback value
// itself is Sync in addition to the context-side guarantees.
unsafe impl<F: Copy + Send + Sync> Sync for FfiCallback<F> {}

impl<F: Copy> FfiCallback<F> {
    /// Create a new callback wrapper from a function pointer and context.
    #[must_use]
    pub const fn new(callback: F, context: *mut c_void) -> Self {
        Self { callback, context }
    }

    /// Return the stored function pointer.
    #[must_use]
    pub const fn callback(&self) -> F {
        self.callback
    }

    /// Return the stored context pointer.
    #[must_use]
    pub const fn context(&self) -> *mut c_void {
        self.context
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestCb = extern "C" fn(*mut c_void, i32) -> i32;

    extern "C" fn dummy_cb(_ctx: *mut c_void, val: i32) -> i32 {
        val + 1
    }

    #[test]
    fn new_stores_callback_and_context() {
        let ctx = 0xDEAD_BEEF as *mut c_void;
        let wrapper: FfiCallback<TestCb> = FfiCallback::new(dummy_cb, ctx);
        assert_eq!(wrapper.context(), ctx);
        let result = (wrapper.callback())(std::ptr::null_mut(), 41);
        assert_eq!(result, 42);
    }

    #[test]
    fn null_context_is_valid() {
        let wrapper: FfiCallback<TestCb> = FfiCallback::new(dummy_cb, std::ptr::null_mut());
        assert!(wrapper.context().is_null());
    }

    // Compile-time assertions that FfiCallback<extern "C" fn(...)> is Send + Sync.
    const _: () = {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        fn check() {
            assert_send::<FfiCallback<TestCb>>();
            assert_sync::<FfiCallback<TestCb>>();
        }
        // Suppress unused warning — this is a compile-time assertion.
        _ = check;
    };
}
