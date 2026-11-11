// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Reinterprets `&T` references as `&Provenance<T, Pty>` at the parser's PTY boundary.

#![allow(
    unsafe_code,
    reason = "Narrow reinterpret over `#[repr(transparent)]` layout"
)]

use crate::origin::Pty;
use crate::provenance::Provenance;

/// Reinterprets a `&T` as a `&Provenance<T, Pty>` at zero runtime cost.
#[must_use]
#[inline]
pub const fn pty_wrap_ref<T: ?Sized>(value: &T) -> &Provenance<T, Pty> {
    // SAFETY: `Provenance<T, Pty>` is `#[repr(transparent)]` over `T` (see `provenance.rs`).
    unsafe { &*(core::ptr::from_ref::<T>(value) as *const Provenance<T, Pty>) }
}

const _: () = {
    assert!(core::mem::size_of::<Provenance<(), Pty>>() == core::mem::size_of::<()>());
    assert!(core::mem::align_of::<Provenance<(), Pty>>() == core::mem::align_of::<()>());
    assert!(core::mem::size_of::<Provenance<u8, Pty>>() == core::mem::size_of::<u8>());
    assert!(core::mem::align_of::<Provenance<u8, Pty>>() == core::mem::align_of::<u8>());
    assert!(core::mem::size_of::<Provenance<u32, Pty>>() == core::mem::size_of::<u32>());
    assert!(core::mem::align_of::<Provenance<u32, Pty>>() == core::mem::align_of::<u32>());
    assert!(core::mem::size_of::<Provenance<[u16; 3], Pty>>() == core::mem::size_of::<[u16; 3]>());
    assert!(
        core::mem::align_of::<Provenance<[u16; 3], Pty>>() == core::mem::align_of::<[u16; 3]>()
    );
    assert!(core::mem::size_of::<Provenance<&[u8], Pty>>() == core::mem::size_of::<&[u8]>());
    assert!(core::mem::align_of::<Provenance<&[u8], Pty>>() == core::mem::align_of::<&[u8]>());
    assert!(
        core::mem::size_of::<&Provenance<[u8], Pty>>() == core::mem::size_of::<&[u8]>(),
        "fat reference layout must match"
    );
    assert!(core::mem::align_of::<&Provenance<[u8], Pty>>() == core::mem::align_of::<&[u8]>());
    assert!(core::mem::size_of::<&Provenance<str, Pty>>() == core::mem::size_of::<&str>());
    assert!(core::mem::align_of::<&Provenance<str, Pty>>() == core::mem::align_of::<&str>());
};

#[cfg(test)]
#[path = "pty_wrap_tests.rs"]
mod tests;
