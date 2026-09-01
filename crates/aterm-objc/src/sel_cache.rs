// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cached selectors.
//!
//! `metal/ffi.rs` resolves every selector with a fresh `sel_registerName` on
//! every send. That is correct — the runtime interns, so the same pointer comes
//! back — but it is a call into `libobjc` that hashes a string, and the sites it
//! guards are per-frame (`super::blit` sends nine selectors per readback; the
//! W6a rig assembly sends dozens per frame).
//!
//! [`crate::sel!`] replaces the call with one relaxed atomic load after the first use.
//!
//! # Why `Relaxed` is enough
//!
//! The cached value is a pointer into the runtime's IMMORTAL selector table.
//! There is no other memory whose visibility depends on it: a thread that reads
//! the cached pointer needs nothing that was written before the store, because
//! the pointee was interned by `libobjc` (with its own synchronisation) before
//! the pointer ever existed. Two threads racing to fill the cache both call
//! `sel_registerName`, both get the SAME pointer back — interning is idempotent
//! — and both store it, so the race is benign by value, not merely by ordering.
//!
//! This is the one place this crate is deliberately weaker than the obvious
//! `OnceLock`: a `OnceLock` would add an acquire load plus an initialised-flag
//! branch to buy a guarantee (happens-before on unrelated memory) that has no
//! consumer here.

use std::ffi::CStr;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::runtime::{Sel, sel};

/// One selector's cache slot. Construct with [`SelCache::new`] in a `static`.
#[derive(Debug)]
pub struct SelCache(AtomicPtr<std::ffi::c_void>);

impl SelCache {
    /// An empty slot.
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicPtr::new(std::ptr::null_mut()))
    }

    /// The selector for `name`, resolving it on first use.
    ///
    /// `name` must be the SAME literal on every call for a given slot; the
    /// [`crate::sel!`] macro is what guarantees that, by pairing each slot with the
    /// literal at one call site.
    #[inline]
    #[must_use]
    pub fn get(&self, name: &'static CStr) -> Sel {
        let cached = self.0.load(Ordering::Relaxed);
        if !cached.is_null() {
            return cached.cast_const().cast();
        }
        self.fill(name)
    }

    /// The cold path, kept out of line so the hot path is a load and a branch.
    #[cold]
    #[inline(never)]
    fn fill(&self, name: &'static CStr) -> Sel {
        let resolved = sel(name);
        self.0.store(resolved.cast_mut().cast(), Ordering::Relaxed);
        resolved
    }
}

impl Default for SelCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve a selector, cached per call site.
///
/// Written with the selector's own Objective-C spelling, so a colon is a colon:
///
/// ```
/// # use aterm_objc::sel;
/// let a = sel!(length);
/// let b = sel!(initWithBytes:length:encoding:);
/// assert!(!a.is_null() && !b.is_null());
/// // Interning is idempotent: the cached pointer IS the uncached one.
/// assert_eq!(a, aterm_objc::sel_uncached(c"length"));
/// ```
///
/// The name is assembled at COMPILE time (`stringify!` per token, `concat!`,
/// then a `const` `CStr::from_bytes_with_nul`), so a name that is not a valid
/// C string cannot be built at all — the same "missing NUL is a compile error"
/// property `metal/ffi.rs` got from taking a `c"…"` literal.
#[macro_export]
macro_rules! sel {
    ($($tok:tt)+) => {{
        static __SEL: $crate::SelCache = $crate::SelCache::new();
        const __NAME: &::core::ffi::CStr = match ::core::ffi::CStr::from_bytes_with_nul(
            ::core::concat!($(::core::stringify!($tok)),+, "\0").as_bytes(),
        ) {
            ::core::result::Result::Ok(s) => s,
            ::core::result::Result::Err(_) => panic!("selector name contains an interior NUL"),
        };
        __SEL.get(__NAME)
    }};
}
