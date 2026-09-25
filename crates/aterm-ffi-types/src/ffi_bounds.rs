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

    /// Every cap is inclusive: its exact value and one below are accepted,
    /// one above is rejected.
    #[test]
    fn is_valid_ffi_len_boundary_at_every_cap() {
        for (cap, max) in [
            ("MAX_FFI_INPUT_BYTES", MAX_FFI_INPUT_BYTES),
            ("MAX_FFI_PATH_BYTES", MAX_FFI_PATH_BYTES),
            ("MAX_FFI_ARRAY_ELEMENTS", MAX_FFI_ARRAY_ELEMENTS),
            ("MAX_FFI_BUFFER_SIZE", crate::MAX_FFI_BUFFER_SIZE),
        ] {
            assert!(
                is_valid_ffi_len(max, max),
                "{cap}: exact boundary must be accepted"
            );
            assert!(
                is_valid_ffi_len(max - 1, max),
                "{cap}: one below boundary must be accepted"
            );
            assert!(
                !is_valid_ffi_len(max + 1, max),
                "{cap}: one above boundary must be rejected"
            );
        }
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
}
