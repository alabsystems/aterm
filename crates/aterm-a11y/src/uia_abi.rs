// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The nullable COM out-parameter rule, independent of Windows calls.

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
mod tests {
    use super::*;

    #[test]
    fn nullable_output_is_initialized_before_either_success_or_refusal() {
        let mut sentinel = 7u8;
        let mut out = &mut sentinel as *mut u8;
        assert!(unsafe { initialize_nullable_out(&mut out) });
        assert!(
            out.is_null(),
            "an absent success must expose null, not caller bytes"
        );
        out = &mut sentinel;
        assert_eq!(
            unsafe { *out },
            7,
            "clearing the slot does not release its old value"
        );
        assert!(!unsafe { initialize_nullable_out::<u8>(std::ptr::null_mut()) });
    }
}
