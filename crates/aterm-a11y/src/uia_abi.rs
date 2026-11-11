// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! COM ABI invariants independent of Windows calls: nullable out-parameters and
//! saturating reference counts.

use std::sync::atomic::{AtomicU32, Ordering};

/// Saturation point for COM reference counts; pinned sticky here so overflowing
/// `u32::MAX` never wraps a live object's count to zero.
pub(crate) const SATURATED_REFS: u32 = i32::MAX as u32;

/// Saturating atomic COM reference count.
pub(crate) struct RefCount(AtomicU32);

impl RefCount {
    /// One reference: the creator's.
    pub(crate) const fn one() -> Self {
        Self(AtomicU32::new(1))
    }

    /// Retains one reference, saturating sticky at [`SATURATED_REFS`].
    pub(crate) fn retain(&self) -> u32 {
        let mut cur = self.0.load(Ordering::Relaxed);
        loop {
            if cur >= SATURATED_REFS {
                return SATURATED_REFS;
            }
            let next = cur + 1;
            match self
                .0
                .compare_exchange_weak(cur, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return next,
                Err(actual) => cur = actual,
            }
        }
    }

    /// Releases one reference, returning `(remaining, should_drop)`.
    ///
    /// A saturated count stays pinned at [`SATURATED_REFS`] and never drops; a
    /// zero count stays at zero and returns `(0, false)`.
    pub(crate) fn release(&self) -> (u32, bool) {
        let mut cur = self.0.load(Ordering::Relaxed);
        loop {
            if cur >= SATURATED_REFS {
                return (SATURATED_REFS, false);
            }
            if cur == 0 {
                return (0, false);
            }
            let next = cur - 1;
            match self
                .0
                .compare_exchange_weak(cur, next, Ordering::Release, Ordering::Relaxed)
            {
                Ok(_) => return (next, next == 0),
                Err(actual) => cur = actual,
            }
        }
    }

    #[cfg(test)]
    const fn from_raw(refs: u32) -> Self {
        Self(AtomicU32::new(refs))
    }
}

/// Clear a nullable interface result before any delegated thunk runs.
///
/// # Safety
/// A non-null `out` must point to a writable COM output slot. This does not
/// release its previous value: COM out parameters do not transfer input ownership.
pub(crate) unsafe fn initialize_nullable_out<T>(out: *mut *mut T) -> bool {
    if out.is_null() {
        return false;
    }
    unsafe { out.write(std::ptr::null_mut()) };
    true
}

#[cfg(test)]
#[path = "uia_abi_tests.rs"]
mod tests;
