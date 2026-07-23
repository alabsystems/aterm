// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared semantic FFI bounds contract.
//!
//! Defines domain-specific validation constants for pointer+len FFI APIs.
//! All production FFI boundary crates MUST import from this module rather
//! than defining their own constants, preventing cross-crate drift (#3076).
//!
//! The raw [`super::MAX_FFI_BUFFER_SIZE`] (256 MiB) at the crate root remains
//! as the bulk-buffer fallback for file I/O and GPU uploads. Semantic
//! subsystems (terminal I/O, paths, arrays) use the tighter limits here.

use std::ffi::{CStr, c_char};

use crate::ffi_slice;

/// Hard cap for byte buffers accepted at the terminal FFI boundary (64 MiB).
///
/// Protects pointer+len APIs from unbounded lengths supplied by foreign callers.
/// Appropriate for PTY I/O, paste operations, and protocol parsing.
///
/// For file I/O operations that handle entire file contents, use
/// [`super::MAX_FFI_BUFFER_SIZE`] (256 MiB) instead.
pub const MAX_FFI_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// Hard cap for path byte buffers accepted at the FFI boundary (16 KiB).
pub const MAX_FFI_PATH_BYTES: usize = 16 * 1024;

/// Hard cap for C string parameter reads at the FFI boundary (1 MiB).
pub const MAX_FFI_PARAM_STRING_BYTES: usize = 1024 * 1024;

/// Hard cap for array lengths accepted at the FFI boundary.
pub const MAX_FFI_ARRAY_ELEMENTS: usize = 1_000_000;

/// Validate an FFI-provided length before converting pointer+len to slices.
///
/// Requirements:
/// - Must fit in `isize` for `from_raw_parts` APIs.
/// - Must not exceed a subsystem-defined maximum bound.
#[must_use]
pub fn is_valid_ffi_len(len: usize, max_len: usize) -> bool {
    len <= max_len && isize::try_from(len).is_ok()
}

/// Read a bounded C string from an FFI parameter pointer.
///
/// Scans at most [`MAX_FFI_PARAM_STRING_BYTES`] bytes for a NUL terminator
/// before constructing the [`CStr`], preventing unbounded memory reads on
/// unterminated foreign input.
///
/// Returns `None` if:
/// - `ptr` is null
/// - No NUL byte is found within [`MAX_FFI_PARAM_STRING_BYTES`]
///
/// # Safety
/// - `ptr` must point to readable memory for at least
///   `min(strlen + 1, MAX_FFI_PARAM_STRING_BYTES)` bytes
/// - The pointed-to memory must remain valid and unmodified for lifetime `'a`
pub unsafe fn bounded_cstr_from_ptr_param<'a>(ptr: *const c_char) -> Option<&'a CStr> {
    if ptr.is_null() {
        return None;
    }

    let mut len = 0usize;
    while len < MAX_FFI_PARAM_STRING_BYTES {
        // SAFETY: Caller guarantees ptr is readable for min(strlen+1, MAX) bytes.
        if unsafe { *ptr.add(len) } == 0 {
            // SAFETY: ptr is non-null and len+1 is bounded by MAX_FFI_PARAM_STRING_BYTES.
            let bytes =
                unsafe { ffi_slice(ptr.cast::<u8>(), len + 1, MAX_FFI_PARAM_STRING_BYTES) }?;
            // SAFETY: bytes[len] == 0 (NUL) and earlier bytes were non-zero.
            return Some(unsafe { CStr::from_bytes_with_nul_unchecked(bytes) });
        }
        len += 1;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_ffi_len_rejects_values_over_limit() {
        assert!(is_valid_ffi_len(1024, 1024));
        assert!(!is_valid_ffi_len(1025, 1024));
    }

    #[test]
    fn is_valid_ffi_len_rejects_isize_overflow() {
        assert!(!is_valid_ffi_len((isize::MAX as usize) + 1, usize::MAX));
    }

    #[test]
    fn is_valid_ffi_len_zero_is_valid() {
        assert!(is_valid_ffi_len(0, MAX_FFI_INPUT_BYTES));
        assert!(is_valid_ffi_len(0, MAX_FFI_PATH_BYTES));
        assert!(is_valid_ffi_len(0, MAX_FFI_ARRAY_ELEMENTS));
    }

    #[test]
    fn bounded_cstr_from_ptr_param_null_returns_none() {
        let result = unsafe { bounded_cstr_from_ptr_param(std::ptr::null()) };
        assert!(result.is_none(), "null pointer must return None");
    }

    #[test]
    fn bounded_cstr_from_ptr_param_valid_string() {
        let s = std::ffi::CString::new("hello").unwrap();
        let result = unsafe { bounded_cstr_from_ptr_param(s.as_ptr()) };
        assert_eq!(result.unwrap().to_str().unwrap(), "hello");
    }

    #[test]
    fn bounded_cstr_from_ptr_param_empty_string() {
        let s = std::ffi::CString::new("").unwrap();
        let result = unsafe { bounded_cstr_from_ptr_param(s.as_ptr()) };
        assert_eq!(result.unwrap().to_str().unwrap(), "");
    }

    #[test]
    fn bounded_cstr_from_ptr_param_invalid_utf8_returns_cstr() {
        let bytes: &[u8] = &[0xFF, 0xFE, 0x00];
        let result = unsafe { bounded_cstr_from_ptr_param(bytes.as_ptr().cast::<c_char>()) };
        let cstr = result.expect("non-UTF-8 bytes with NUL should produce a CStr");
        assert_eq!(cstr.to_bytes(), &[0xFF, 0xFE]);
    }

    #[test]
    fn bounded_cstr_from_ptr_param_unicode() {
        let s = std::ffi::CString::new("hello 🌍").unwrap();
        let result = unsafe { bounded_cstr_from_ptr_param(s.as_ptr()) };
        assert_eq!(result.unwrap().to_str().unwrap(), "hello 🌍");
    }

    #[test]
    fn bounded_cstr_from_ptr_param_overlength_returns_none() {
        let buf = vec![b'A'; MAX_FFI_PARAM_STRING_BYTES + 1];
        let result = unsafe { bounded_cstr_from_ptr_param(buf.as_ptr().cast()) };
        assert!(
            result.is_none(),
            "overlength string without NUL must return None"
        );
    }

    #[test]
    fn is_valid_ffi_len_boundary_max_ffi_input_bytes() {
        assert!(
            is_valid_ffi_len(MAX_FFI_INPUT_BYTES, MAX_FFI_INPUT_BYTES),
            "exact boundary must be accepted"
        );
        assert!(
            is_valid_ffi_len(MAX_FFI_INPUT_BYTES - 1, MAX_FFI_INPUT_BYTES),
            "one below boundary must be accepted"
        );
        assert!(
            !is_valid_ffi_len(MAX_FFI_INPUT_BYTES + 1, MAX_FFI_INPUT_BYTES),
            "one above boundary must be rejected"
        );
    }

    #[test]
    fn is_valid_ffi_len_boundary_max_ffi_path_bytes() {
        assert!(
            is_valid_ffi_len(MAX_FFI_PATH_BYTES, MAX_FFI_PATH_BYTES),
            "exact boundary must be accepted"
        );
        assert!(
            is_valid_ffi_len(MAX_FFI_PATH_BYTES - 1, MAX_FFI_PATH_BYTES),
            "one below boundary must be accepted"
        );
        assert!(
            !is_valid_ffi_len(MAX_FFI_PATH_BYTES + 1, MAX_FFI_PATH_BYTES),
            "one above boundary must be rejected"
        );
    }

    #[test]
    fn is_valid_ffi_len_boundary_max_ffi_array_elements() {
        assert!(
            is_valid_ffi_len(MAX_FFI_ARRAY_ELEMENTS, MAX_FFI_ARRAY_ELEMENTS),
            "exact boundary must be accepted"
        );
        assert!(
            is_valid_ffi_len(MAX_FFI_ARRAY_ELEMENTS - 1, MAX_FFI_ARRAY_ELEMENTS),
            "one below boundary must be accepted"
        );
        assert!(
            !is_valid_ffi_len(MAX_FFI_ARRAY_ELEMENTS + 1, MAX_FFI_ARRAY_ELEMENTS),
            "one above boundary must be rejected"
        );
    }

    #[test]
    fn is_valid_ffi_len_boundary_max_ffi_buffer_size() {
        use crate::MAX_FFI_BUFFER_SIZE;
        assert!(
            is_valid_ffi_len(MAX_FFI_BUFFER_SIZE, MAX_FFI_BUFFER_SIZE),
            "exact boundary must be accepted"
        );
        assert!(
            is_valid_ffi_len(MAX_FFI_BUFFER_SIZE - 1, MAX_FFI_BUFFER_SIZE),
            "one below boundary must be accepted"
        );
        assert!(
            !is_valid_ffi_len(MAX_FFI_BUFFER_SIZE + 1, MAX_FFI_BUFFER_SIZE),
            "one above boundary must be rejected"
        );
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::*;

    /// Prove that `is_valid_ffi_len` returning true implies both:
    /// 1. `len` fits in `isize` (required by `std::slice::from_raw_parts`)
    /// 2. `len` does not exceed the domain-specific maximum
    #[kani::proof]
    fn is_valid_ffi_len_implies_from_raw_parts_precondition() {
        let len: usize = kani::any();
        let max_len: usize = kani::any();

        if is_valid_ffi_len(len, max_len) {
            kani::assert(
                isize::try_from(len).is_ok(),
                "valid FFI len must fit in isize",
            );
            kani::assert(len <= max_len, "valid FFI len must not exceed domain max");
        }
    }

    /// Prove that any len exceeding isize::MAX is always rejected,
    /// regardless of the max_len bound.
    #[kani::proof]
    fn is_valid_ffi_len_always_rejects_isize_overflow() {
        let len: usize = kani::any();
        let max_len: usize = kani::any();

        kani::assume(isize::try_from(len).is_err());
        kani::assert(
            !is_valid_ffi_len(len, max_len),
            "isize-overflowing len must always be rejected",
        );
    }

    /// Zero length is always valid for any max_len.
    #[kani::proof]
    fn is_valid_ffi_len_accepts_zero() {
        let max_len: usize = kani::any();
        kani::assert(
            is_valid_ffi_len(0, max_len),
            "zero length must always be valid",
        );
    }

    // =========================================================================
    // Model proofs for bounded_cstr_from_ptr_param scanning logic
    // =========================================================================
    //
    // bounded_cstr_from_ptr_param uses 3 unsafe operations:
    //   1. *ptr.add(len) — raw pointer read during scan
    //   2. ffi_slice(ptr, len+1, MAX) — creates a bounded slice
    //   3. CStr::from_bytes_with_nul_unchecked(bytes) — requires last byte NUL
    //
    // The function hardcodes MAX_FFI_PARAM_STRING_BYTES (1 MiB) which makes
    // direct Kani verification infeasible (1M loop unwinds). These model proofs
    // verify the scanning algorithm at a tractable scale (4 bytes) to confirm
    // the safety invariants that the unsafe calls depend on.

    /// Model proof: bounded C string scan finds the FIRST NUL byte and
    /// satisfies the ffi_slice and CStr::from_bytes_with_nul_unchecked
    /// preconditions.
    ///
    /// For all possible 4-byte buffers, when a NUL is found at position `len`:
    /// 1. `len + 1 <= MAX` (ffi_slice won't reject for overlength)
    /// 2. `buf[len] == 0` (CStr last-byte-is-NUL precondition)
    /// 3. No NUL exists before position `len` (first-NUL guarantee, so
    ///    CStr has no interior NUL)
    #[kani::proof]
    #[kani::unwind(5)]
    fn bounded_cstr_scan_finds_first_nul() {
        const KANI_MAX_SCAN: usize = 4;
        let buf: [u8; KANI_MAX_SCAN] = kani::any();

        // Model the scanning loop from bounded_cstr_from_ptr_param
        let mut len = 0usize;
        let mut found_nul = false;
        while len < KANI_MAX_SCAN {
            if buf[len] == 0 {
                found_nul = true;
                break;
            }
            len += 1;
        }

        if found_nul {
            // Invariant 1: ffi_slice(ptr, len+1, MAX) won't reject
            kani::assert(
                len + 1 <= KANI_MAX_SCAN,
                "len+1 must fit within max scan bound",
            );

            // Invariant 2: byte at scan position is NUL
            kani::assert(buf[len] == 0, "byte at scan position must be NUL");

            // Invariant 3: no interior NUL before the found position
            let mut i = 0usize;
            while i < len {
                kani::assert(buf[i] != 0, "no interior NUL before first NUL position");
                i += 1;
            }
        }
    }

    /// Model proof: scan returns None when buffer has no NUL within max.
    ///
    /// When all bytes are non-zero, the scan must exhaust the buffer
    /// without finding a NUL, modeling the None return path.
    #[kani::proof]
    #[kani::unwind(5)]
    fn bounded_cstr_scan_rejects_no_nul() {
        const KANI_MAX_SCAN: usize = 4;
        let buf: [u8; KANI_MAX_SCAN] = kani::any();

        // Assume no NUL in the buffer
        let mut all_nonzero = true;
        let mut j = 0usize;
        while j < KANI_MAX_SCAN {
            if buf[j] == 0 {
                all_nonzero = false;
            }
            j += 1;
        }
        kani::assume(all_nonzero);

        // Model the scanning loop
        let mut len = 0usize;
        let mut found_nul = false;
        while len < KANI_MAX_SCAN {
            if buf[len] == 0 {
                found_nul = true;
                break;
            }
            len += 1;
        }

        kani::assert(!found_nul, "scan must not find NUL when none exists");
        kani::assert(
            len == KANI_MAX_SCAN,
            "scan must exhaust the buffer when no NUL present",
        );
    }

    /// Model proof: the ffi_slice call in bounded_cstr_from_ptr_param
    /// always receives a valid length argument.
    ///
    /// When NUL is found at position `len`, the function calls
    /// `ffi_slice(ptr, len+1, MAX_FFI_PARAM_STRING_BYTES)`. This proof
    /// verifies that `is_valid_ffi_len(len+1, MAX)` holds for all
    /// possible NUL positions within a bounded scan.
    #[kani::proof]
    #[kani::unwind(5)]
    fn bounded_cstr_ffi_slice_arg_valid() {
        const KANI_MAX_SCAN: usize = 4;
        let buf: [u8; KANI_MAX_SCAN] = kani::any();

        let mut len = 0usize;
        while len < KANI_MAX_SCAN {
            if buf[len] == 0 {
                // At this point bounded_cstr_from_ptr_param calls:
                //   ffi_slice(ptr, len+1, MAX_FFI_PARAM_STRING_BYTES)
                // Verify the length argument is valid.
                let slice_len = len + 1;
                kani::assert(
                    is_valid_ffi_len(slice_len, KANI_MAX_SCAN),
                    "ffi_slice length argument must be valid when NUL found",
                );
                // Also verify isize fits (production MAX is 1 MiB, always fits)
                kani::assert(
                    isize::try_from(slice_len).is_ok(),
                    "slice_len must fit in isize",
                );
                return;
            }
            len += 1;
        }
        // No NUL found — function returns None, no ffi_slice call made.
    }
}
