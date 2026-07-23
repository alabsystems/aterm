// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! FFI pointer-to-reference and pointer-to-slice helpers for null-safe conversion.
//!
//! | Helper          | Pattern                           | Replaces                              |
//! |-----------------|-----------------------------------|---------------------------------------|
//! | `ffi_ref`       | Null-check + const deref          | `unsafe { &*ptr }`                    |
//! | `ffi_ref_mut`   | Null-check + mutable deref        | `unsafe { &mut *ptr }`                |
//! | `ffi_slice`     | Null + bounds + const slice       | `unsafe { from_raw_parts(ptr, len) }` |
//! | `ffi_slice_mut` | Null + bounds + mutable slice     | `unsafe { from_raw_parts_mut(..) }`   |
//!
//! Each function documents its safety invariants once. Call sites replace
//! inline `unsafe` blocks with the corresponding helper, reducing the total
//! unsafe surface and centralising the SAFETY contract.
//!
//! Originally in `aterm-core-ffi/src/safety.rs`. Extracted to `aterm-types`
//! so all FFI crates can share the same helpers (#4628, #4633).

use crate::ffi_bounds::{MAX_FFI_ARRAY_ELEMENTS, MAX_FFI_INPUT_BYTES, is_valid_ffi_len};
use crate::verification::FfiTracker;

/// Convert a raw const pointer to a shared reference.
///
/// Returns `None` if `ptr` is null.
///
/// # Safety
///
/// When `ptr` is non-null, it must point to a valid, aligned `T` whose
/// referent lives for at least `'a`. The caller must not create a mutable
/// reference to the same `T` for the duration of `'a`.
#[inline]
pub unsafe fn ffi_ref<'a, T>(ptr: *const T) -> Option<&'a T> {
    if ptr.is_null() {
        None
    } else {
        // SAFETY: Caller guarantees the non-null pointer is valid for 'a.
        Some(unsafe { &*ptr })
    }
}

/// Convert a tracked raw const pointer to a shared reference.
///
/// Returns `None` if `ptr` is null or the selected tracker has no live
/// allocation record for the address.
///
/// # Safety
///
/// Same as [`ffi_ref`], plus the pointer must have been registered with
/// [`FfiTracker::is_allocated`] for the selected tracker.
#[inline]
pub unsafe fn ffi_ref_tracked<'a, T>(ptr: *const T, tracker: FfiTracker) -> Option<&'a T> {
    if ptr.is_null() || !tracker.is_allocated(ptr.cast()) {
        None
    } else {
        // SAFETY: Tracker membership and caller contract establish validity.
        unsafe { ffi_ref(ptr) }
    }
}

/// Convert a raw mutable pointer to a mutable reference.
///
/// Returns `None` if `ptr` is null.
///
/// # Safety
///
/// When `ptr` is non-null, it must point to a valid, aligned `T` whose
/// referent lives for at least `'a`. The caller must hold exclusive access
/// to the referent for the duration of `'a` (no other references, mutable
/// or shared, may exist).
#[inline]
pub unsafe fn ffi_ref_mut<'a, T>(ptr: *mut T) -> Option<&'a mut T> {
    if ptr.is_null() {
        None
    } else {
        // SAFETY: Caller guarantees the non-null pointer is valid for 'a
        // and that exclusive access is held.
        Some(unsafe { &mut *ptr })
    }
}

/// Convert a tracked raw mutable pointer to a mutable reference.
///
/// Returns `None` if `ptr` is null or the selected tracker has no live
/// allocation record for the address.
///
/// # Safety
///
/// Same as [`ffi_ref_mut`], plus the pointer must have been registered with
/// [`FfiTracker::is_allocated`] for the selected tracker.
#[inline]
pub unsafe fn ffi_ref_mut_tracked<'a, T>(ptr: *mut T, tracker: FfiTracker) -> Option<&'a mut T> {
    if ptr.is_null() || !tracker.is_allocated(ptr.cast()) {
        None
    } else {
        // SAFETY: Tracker membership and caller contract establish validity.
        unsafe { ffi_ref_mut(ptr) }
    }
}

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
pub unsafe fn ffi_slice<'a, T>(ptr: *const T, len: usize, max_len: usize) -> Option<&'a [T]> {
    if ptr.is_null() || !is_valid_ffi_len(len, max_len) {
        return None;
    }
    // SAFETY: Caller guarantees ptr is valid for len Ts, and we verified
    // len fits in isize and does not exceed max_len.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// Convert a raw mutable pointer + length to a mutable slice.
///
/// Returns `None` if `ptr` is null or `len` exceeds `max_len`
/// (or does not fit in `isize`).
///
/// # Safety
///
/// When non-null and within bounds, `ptr` must point to `len` initialized,
/// properly aligned `T` values. The caller must hold exclusive access for `'a`.
#[inline]
pub unsafe fn ffi_slice_mut<'a, T>(ptr: *mut T, len: usize, max_len: usize) -> Option<&'a mut [T]> {
    if ptr.is_null() || !is_valid_ffi_len(len, max_len) {
        return None;
    }
    // SAFETY: Caller guarantees ptr is valid for len Ts with exclusive access,
    // and we verified len fits in isize and does not exceed max_len.
    Some(unsafe { std::slice::from_raw_parts_mut(ptr, len) })
}

// ============================================================================
// Convenience aliases for common FFI bounds
// ============================================================================

/// Convert a raw byte pointer + length to a shared byte slice.
///
/// Bounded by [`MAX_FFI_INPUT_BYTES`] (64 MiB). Covers ~80% of FFI byte-buffer
/// call sites (PTY I/O, paste, protocol parsing).
///
/// # Safety
///
/// Same as [`ffi_slice`] with `T = u8`.
#[inline]
pub unsafe fn ffi_byte_slice<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    // SAFETY: Caller upholds ffi_slice preconditions.
    unsafe { ffi_slice(ptr, len, MAX_FFI_INPUT_BYTES) }
}

/// Convert a raw mutable byte pointer + length to a mutable byte slice.
///
/// Bounded by [`MAX_FFI_INPUT_BYTES`] (64 MiB).
///
/// # Safety
///
/// Same as [`ffi_slice_mut`] with `T = u8`.
#[inline]
pub unsafe fn ffi_byte_slice_mut<'a>(ptr: *mut u8, len: usize) -> Option<&'a mut [u8]> {
    // SAFETY: Caller upholds ffi_slice_mut preconditions.
    unsafe { ffi_slice_mut(ptr, len, MAX_FFI_INPUT_BYTES) }
}

/// Convert a raw const pointer + length to a shared slice of typed elements.
///
/// Bounded by [`MAX_FFI_ARRAY_ELEMENTS`] (1M elements). Use for arrays of
/// structs, indices, or other non-byte typed data at FFI boundaries.
///
/// # Safety
///
/// Same as [`ffi_slice`].
#[inline]
pub unsafe fn ffi_array_slice<'a, T>(ptr: *const T, len: usize) -> Option<&'a [T]> {
    // SAFETY: Caller upholds ffi_slice preconditions.
    unsafe { ffi_slice(ptr, len, MAX_FFI_ARRAY_ELEMENTS) }
}

/// Convert a raw mutable pointer + length to a mutable slice of typed elements.
///
/// Bounded by [`MAX_FFI_ARRAY_ELEMENTS`] (1M elements).
///
/// # Safety
///
/// Same as [`ffi_slice_mut`].
#[inline]
pub unsafe fn ffi_array_slice_mut<'a, T>(ptr: *mut T, len: usize) -> Option<&'a mut [T]> {
    // SAFETY: Caller upholds ffi_slice_mut preconditions.
    unsafe { ffi_slice_mut(ptr, len, MAX_FFI_ARRAY_ELEMENTS) }
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
    fn ffi_slice_mut_returns_none_on_null() {
        let result = unsafe { ffi_slice_mut::<u8>(std::ptr::null_mut(), 10, 100) };
        assert!(result.is_none());
    }

    #[test]
    fn ffi_slice_mut_returns_none_on_exceeding_max() {
        let mut data = [0u8; 4];
        let result = unsafe { ffi_slice_mut(data.as_mut_ptr(), 4, 3) };
        assert!(result.is_none());
    }

    #[test]
    fn ffi_slice_mut_returns_some_on_valid() {
        let mut data = [1u8, 2, 3, 4];
        let result = unsafe { ffi_slice_mut(data.as_mut_ptr(), 4, 100) };
        assert!(result.is_some());
        assert_eq!(result.unwrap(), &[1, 2, 3, 4]);
    }

    #[test]
    fn ffi_slice_zero_len_with_non_null_ptr() {
        let data = [0u8; 1];
        let result = unsafe { ffi_slice(data.as_ptr(), 0, 100) };
        assert_eq!(result, Some(&[][..]));
    }

    // Convenience alias tests

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

    #[test]
    fn ffi_byte_slice_mut_returns_some_on_valid() {
        let mut data = [10u8, 20, 30];
        let result = unsafe { ffi_byte_slice_mut(data.as_mut_ptr(), 3) };
        assert!(result.is_some());
        assert_eq!(result.unwrap(), &[10, 20, 30]);
    }

    #[test]
    fn ffi_array_slice_returns_some_on_valid() {
        let data = [1u32, 2, 3, 4, 5];
        let result = unsafe { ffi_array_slice(data.as_ptr(), 5) };
        assert_eq!(result, Some(&data[..]));
    }

    #[test]
    fn ffi_array_slice_returns_none_on_null() {
        let result = unsafe { ffi_array_slice::<u32>(std::ptr::null(), 1) };
        assert!(result.is_none());
    }

    #[test]
    fn ffi_array_slice_mut_returns_some_on_valid() {
        let mut data = [1u32, 2, 3];
        let result = unsafe { ffi_array_slice_mut(data.as_mut_ptr(), 3) };
        assert!(result.is_some());
        assert_eq!(result.unwrap(), &[1, 2, 3]);
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::*;

    // ffi_slice_none_on_null removed (#5887): covered by concrete test
    // ffi_slice_returns_none_on_null (line 164).

    #[kani::proof]
    fn ffi_slice_none_on_invalid_len() {
        let max_len: usize = kani::any();
        let len: usize = kani::any();
        kani::assume(!is_valid_ffi_len(len, max_len));
        let ptr = 1usize as *const u8; // non-null sentinel
        let result = unsafe { ffi_slice(ptr, len, max_len) };
        kani::assert(result.is_none(), "ffi_slice none on invalid len");
    }

    // ffi_slice_mut_none_on_null removed (#5887): covered by concrete test
    // ffi_slice_mut_returns_none_on_null (line 184).

    #[kani::proof]
    fn ffi_slice_mut_none_on_invalid_len() {
        let max_len: usize = kani::any();
        let len: usize = kani::any();
        kani::assume(!is_valid_ffi_len(len, max_len));
        let ptr = 1usize as *mut u8; // non-null sentinel
        let result = unsafe { ffi_slice_mut(ptr, len, max_len) };
        kani::assert(result.is_none(), "ffi_slice_mut none on invalid len");
    }

    // -- Convenience alias proofs --
    // Verify each alias passes the correct domain-specific bound constant.
    // A swap between MAX_FFI_INPUT_BYTES (64 MiB) and MAX_FFI_ARRAY_ELEMENTS
    // (1M) would silently accept or reject the wrong lengths. These proofs
    // catch that class of bug for all possible len values.

    /// ffi_byte_slice rejects any len exceeding MAX_FFI_INPUT_BYTES.
    #[kani::proof]
    fn ffi_byte_slice_rejects_overlength() {
        let len: usize = kani::any();
        kani::assume(len > MAX_FFI_INPUT_BYTES);
        let ptr = 1usize as *const u8; // non-null sentinel
        let result = unsafe { ffi_byte_slice(ptr, len) };
        kani::assert(result.is_none(), "ffi_byte_slice rejects overlength");
    }

    /// ffi_byte_slice_mut rejects any len exceeding MAX_FFI_INPUT_BYTES.
    #[kani::proof]
    fn ffi_byte_slice_mut_rejects_overlength() {
        let len: usize = kani::any();
        kani::assume(len > MAX_FFI_INPUT_BYTES);
        let ptr = 1usize as *mut u8; // non-null sentinel
        let result = unsafe { ffi_byte_slice_mut(ptr, len) };
        kani::assert(result.is_none(), "ffi_byte_slice_mut rejects overlength");
    }

    /// ffi_array_slice rejects any len exceeding MAX_FFI_ARRAY_ELEMENTS.
    #[kani::proof]
    fn ffi_array_slice_rejects_overlength() {
        let len: usize = kani::any();
        kani::assume(len > MAX_FFI_ARRAY_ELEMENTS);
        let ptr = 1usize as *const u32; // non-null sentinel
        let result = unsafe { ffi_array_slice(ptr, len) };
        kani::assert(result.is_none(), "ffi_array_slice rejects overlength");
    }

    /// ffi_array_slice_mut rejects any len exceeding MAX_FFI_ARRAY_ELEMENTS.
    #[kani::proof]
    fn ffi_array_slice_mut_rejects_overlength() {
        let len: usize = kani::any();
        kani::assume(len > MAX_FFI_ARRAY_ELEMENTS);
        let ptr = 1usize as *mut u32; // non-null sentinel
        let result = unsafe { ffi_array_slice_mut(ptr, len) };
        kani::assert(result.is_none(), "ffi_array_slice_mut rejects overlength");
    }
}
