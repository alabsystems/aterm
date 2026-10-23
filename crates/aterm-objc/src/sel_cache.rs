// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cached selectors.
//!
//! `sel_registerName` hashes a string on every call, and many sends are on
//! per-frame paths. [`crate::sel!`] replaces it with one relaxed atomic load
//! after first use.
//!
//! `Relaxed` suffices: the cached value points into the runtime's immortal
//! selector table, interned by libobjc (with its own synchronisation) before
//! the pointer existed, and no other memory depends on it. Racing fillers both
//! get the same pointer (interning is idempotent), so the race is benign by
//! value. A `OnceLock` would add an acquire load and a flag branch for a
//! happens-before nothing consumes.

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

    /// The selector for `name`, resolving it on first use. `name` must be the
    /// same literal on every call for a given slot, which [`crate::sel!`]
    /// guarantees by pairing each slot with one call site's literal.
    #[inline]
    #[must_use]
    pub fn get(&self, name: &'static CStr) -> Sel {
        let cached = self.0.load(Ordering::Relaxed);
        if !cached.is_null() {
            return Sel::from_ptr(cached.cast_const());
        }
        self.fill(name)
    }

    /// The cold path, kept out of line so the hot path is a load and a branch.
    #[cold]
    #[inline(never)]
    fn fill(&self, name: &'static CStr) -> Sel {
        let resolved = sel(name);
        self.0
            .store(resolved.as_ptr().cast_mut(), Ordering::Relaxed);
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
/// # use aterm_objc::sel;
/// let a = sel!(length);
/// let b = sel!(initWithBytes:length:encoding:);
/// assert!(!a.is_null() && !b.is_null());
/// // Interning is idempotent: the cached pointer IS the uncached one.
///
/// The name is assembled at compile time (`stringify!` per token, `concat!`,
/// then a `const` `CStr::from_bytes_with_nul`), so an invalid C string cannot
/// be built.
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
