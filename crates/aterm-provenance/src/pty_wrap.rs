// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `pty_wrap_ref` — the single entry point that reinterprets a `&T` as a
//! `&Provenance<T, Pty>`.
//!
//! This is the parser-side PTY boundary: every byte slice coming out of the
//! parser's state machine flows through here before reaching an `ActionSink`
//! method, tagging it at the type level as `Pty`-origin data (Phase 1 of
//! #7877; see `designs/2026-04-19-provenance-framework.md` §Phase 1 and
//! Appendix A).
//!
//! # Why this is the only `unsafe` in `aterm-provenance`
//!
//! [`Provenance<T, O>`] is `#[repr(transparent)]` over `T`, which guarantees
//! identical in-memory layout and alignment. The cast is a pointer reinterpret
//! that changes only the static type, not the bytes at the address. Tagging
//! with `PhantomData<fn() -> O>` is a compile-time-only marker; it contributes
//! no runtime state to reinterpret.
//!
//! The crate-level `forbid(unsafe_code)` is relaxed narrowly for this module
//! via `#![allow(unsafe_code)]` at the file top; the rest of the crate
//! continues to forbid unsafe.
//!
//! # Kani coverage
//!
//! The harness `pty_wrap_ref_identity_slice` in `proofs/kani/provenance/`
//! asserts that for any `&[u8]` the wrapped reference has the same address,
//! length, and byte contents as the input reference (bit-identical).

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

    #[test]
    fn wrap_slice_preserves_address_and_contents() {
        let bytes: &[u8] = b"hello";
        let wrapped = pty_wrap_ref(bytes);
        // The inner slice must point to the same memory and have the same len.
        assert_eq!(wrapped.as_ref().as_ptr(), bytes.as_ptr());
        assert_eq!(wrapped.as_ref().len(), bytes.len());
        assert_eq!(wrapped.as_ref(), bytes);
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

// -- Kani layout-equivalence harness --------------------------------------

#[cfg(kani)]
mod kani_proofs {
    use super::pty_wrap_ref;

    /// `pty_wrap_ref` preserves the pointer address and slice length for any
    /// `&[u8]`. With `#[repr(transparent)]`, the wrapped reference is
    /// bit-identical to the input reference.
    #[kani::proof]
    fn pty_wrap_ref_identity_slice() {
        // Bounded symbolic slice — Kani requires a concrete allocation.
        let len: usize = kani::any();
        kani::assume(len <= 4);
        let data = [0u8; 4];
        let slice: &[u8] = &data[..len];
        let wrapped = pty_wrap_ref(slice);
        kani::assert(
            wrapped.as_ref().as_ptr() == slice.as_ptr(),
            "wrapped ptr equals input ptr",
        );
        kani::assert(
            wrapped.as_ref().len() == slice.len(),
            "wrapped len equals input len",
        );
    }
}
