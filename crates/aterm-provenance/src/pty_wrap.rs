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

#[cfg(test)]
mod tests {
    use super::pty_wrap_ref;
    use crate::{Provenance, Pty};

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
