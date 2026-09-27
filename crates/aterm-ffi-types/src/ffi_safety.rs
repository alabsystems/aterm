// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! FFI pointer-to-slice helper for null-safe, bounded conversion.
//!
//! [`ffi_byte_slice`] replaces an inline `unsafe { from_raw_parts(ptr, len) }`
//! at the C ABI: it null-checks `ptr`, enforces the `from_raw_parts`
//! `isize::MAX` precondition, and caps `len` at [`MAX_FFI_INPUT_BYTES`], so
//! the SAFETY contract is stated once.

use crate::ffi_bounds::{MAX_FFI_INPUT_BYTES, is_valid_ffi_len};

/// Convert a raw const pointer + length to a shared slice.
///
/// Returns `None` if `ptr` is null or `len` exceeds `max_len`
/// (or does not fit in `isize`).
///
/// # Safety
///
/// When non-null and within bounds, `ptr` must point to `len` initialized,
/// properly aligned `T` values. The memory must not be mutated for lifetime `'a`.
#[inline]
unsafe fn ffi_slice<'a, T>(ptr: *const T, len: usize, max_len: usize) -> Option<&'a [T]> {
    if ptr.is_null() || !is_valid_ffi_len(len, max_len) {
        return None;
    }
    // SAFETY: Caller guarantees ptr is valid for len Ts, and we verified
    // len fits in isize and does not exceed max_len.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

// ============================================================================
// Convenience alias for the terminal FFI byte bound
// ============================================================================

/// Convert a raw byte pointer + length to a shared byte slice.
///
/// Bounded by [`MAX_FFI_INPUT_BYTES`] (64 MiB).
///
/// # Safety
///
/// When non-null and within bounds, `ptr` must point to `len` initialized
/// bytes that are not mutated for lifetime `'a`.
#[inline]
pub unsafe fn ffi_byte_slice<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    // SAFETY: Caller upholds ffi_slice preconditions.
    unsafe { ffi_slice(ptr, len, MAX_FFI_INPUT_BYTES) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_slice_returns_none_on_null() {
        let result = unsafe { ffi_slice::<u8>(std::ptr::null(), 10, 100) };
        assert!(result.is_none());
    }

    #[test]
    fn ffi_slice_returns_none_on_exceeding_max() {
        let data = [0u8; 4];
        let result = unsafe { ffi_slice(data.as_ptr(), 4, 3) };
        assert!(result.is_none());
    }

    #[test]
    fn ffi_slice_returns_some_on_valid() {
        let data = [1u8, 2, 3, 4];
        let result = unsafe { ffi_slice(data.as_ptr(), 4, 100) };
        assert_eq!(result, Some(&data[..]));
    }

    #[test]
    fn ffi_slice_zero_len_with_non_null_ptr() {
        let data = [0u8; 1];
        let result = unsafe { ffi_slice(data.as_ptr(), 0, 100) };
        assert_eq!(result, Some(&[][..]));
    }

    #[test]
    fn ffi_byte_slice_returns_some_on_valid() {
        let data = [10u8, 20, 30];
        let result = unsafe { ffi_byte_slice(data.as_ptr(), 3) };
        assert_eq!(result, Some(&data[..]));
    }

    #[test]
    fn ffi_byte_slice_returns_none_on_null() {
        let result = unsafe { ffi_byte_slice(std::ptr::null(), 1) };
        assert!(result.is_none());
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::*;

    #[kani::proof]
    fn ffi_slice_none_on_invalid_len() {
        let max_len: usize = kani::any();
        let len: usize = kani::any();
        kani::assume(!is_valid_ffi_len(len, max_len));
        let ptr = 1usize as *const u8; // non-null sentinel
        let result = unsafe { ffi_slice(ptr, len, max_len) };
        kani::assert(result.is_none(), "ffi_slice none on invalid len");
    }

    /// ffi_byte_slice rejects any len exceeding MAX_FFI_INPUT_BYTES — a wrong
    /// bound constant would silently accept or reject the wrong lengths.
    #[kani::proof]
    fn ffi_byte_slice_rejects_overlength() {
        let len: usize = kani::any();
        kani::assume(len > MAX_FFI_INPUT_BYTES);
        let ptr = 1usize as *const u8; // non-null sentinel
        let result = unsafe { ffi_byte_slice(ptr, len) };
        kani::assert(result.is_none(), "ffi_byte_slice rejects overlength");
    }
}
