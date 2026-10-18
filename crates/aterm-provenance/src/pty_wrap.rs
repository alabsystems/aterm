// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `pty_wrap_ref` — the single entry point that reinterprets a `&T` as a
//! `&Provenance<T, Pty>`.
//!
//! This is the parser-side PTY boundary: every byte slice coming out of the
//! parser's state machine flows through here before reaching an `ActionSink`
//! method, tagging it at the type level as `Pty`-origin data (Phase 1 of
//! #7877).
//!
//! # Why this is the only `unsafe` in `aterm-provenance`
//!
//! [`Provenance<T, O>`] is `#[repr(transparent)]` over `T`, which guarantees
//! identical in-memory layout and alignment. The cast is a pointer reinterpret
//! that changes only the static type, not the bytes at the address. Tagging
//! with `PhantomData<fn() -> O>` is a compile-time-only marker; it contributes
//! no runtime state to reinterpret.
//!
//! The crate-level `deny(unsafe_code)` is relaxed narrowly for this module
//! via `#![allow(unsafe_code)]` at the file top; the rest of the crate
//! denies unsafe.
//!
//! # Coverage
//!
//! The tests below assert that `Provenance` adds no bytes to what it wraps,
//! and that the wrapped reference has the same address, length and bytes as
//! the input reference (bit-identical), sized and unsized.

#![allow(
    unsafe_code,
    reason = "Narrow reinterpret over `#[repr(transparent)]` layout"
)]

use crate::origin::Pty;
use crate::provenance::Provenance;

/// Reinterpret a `&T` as a `&Provenance<T, Pty>` at zero runtime cost.
///
/// The parser uses this at its PTY-byte boundary so every downstream
/// `ActionSink` method sees slice arguments pre-tagged as `Pty`-origin.
///
/// # Safety (author-side)
///
/// [`Provenance<T, O>`] is `#[repr(transparent)]` over `T`: its in-memory
/// layout is identical to `T`'s. `PhantomData<fn() -> O>` is zero-sized and
/// contributes no runtime state. The reinterpret preserves the referent's
/// address, size, and alignment; the only change is the static type of the
/// reference.
///
/// # Audit
///
/// `grep -rn 'pty_wrap_ref' crates/` enumerates every PTY-boundary wrap.
/// There should be very few call sites — ideally only inside the parser's
/// `dispatch.rs`.
#[must_use]
#[inline]
pub const fn pty_wrap_ref<T: ?Sized>(value: &T) -> &Provenance<T, Pty> {
    // SAFETY: `Provenance<T, Pty>` is `#[repr(transparent)]` over `T`
    // (see `provenance.rs`). The pointer values are identical; we only
    // refine the static type of the reference, not the underlying bytes.
    // `PhantomData<fn() -> Pty>` is zero-sized and adds no runtime state.
    unsafe { &*(core::ptr::from_ref::<T>(value) as *const Provenance<T, Pty>) }
}

#[cfg(test)]
mod tests {
    use super::pty_wrap_ref;
    use crate::{Provenance, Pty};

    /// The reinterpret is sound only while `Provenance` is layout-identical
    /// to what it wraps.
    #[test]
    fn provenance_adds_no_bytes() {
        assert_eq!(core::mem::size_of::<Provenance<u8, Pty>>(), 1);
        assert_eq!(core::mem::size_of::<Provenance<u32, Pty>>(), 4);
        assert_eq!(
            core::mem::size_of::<Provenance<&[u8], Pty>>(),
            core::mem::size_of::<&[u8]>()
        );
    }

    #[test]
    fn wrap_scalar_reference_preserves_identity() {
        let value: u8 = 0x7F;
        let wrapped = pty_wrap_ref(&value);
        assert_eq!(
            core::ptr::from_ref(wrapped.as_ref()),
            core::ptr::from_ref(&value)
        );
        assert_eq!(*wrapped.as_ref(), 0x7F);
    }

    #[test]
    fn wrap_str_preserves_bytes() {
        let s: &str = "héllo";
        let wrapped = pty_wrap_ref(s);
        assert_eq!(wrapped.as_ref().as_ptr(), s.as_ptr());
        assert_eq!(wrapped.as_ref().len(), s.len());
        assert_eq!(wrapped.as_ref(), s);
    }
}
