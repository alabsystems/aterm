// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for [`FfiErrorCode`] trait, macro helpers, and impl correctness.
//!
//! Split from `ffi_combinator.rs` to stay under 500-line limit.

use super::*;

#[test]
fn terminal_error_sentinels() {
    assert_eq!(AtermTerminalError::ok(), AtermTerminalError::Ok);
    assert_eq!(
        AtermTerminalError::internal(),
        AtermTerminalError::ErrInternal
    );
    assert_eq!(
        AtermTerminalError::null_terminal(),
        AtermTerminalError::ErrNullTerminal
    );
    assert_eq!(
        AtermTerminalError::null_output(),
        AtermTerminalError::ErrNullOutput
    );
}

#[test]
fn terminal_error_preserves_lock_error() {
    let reentrant = AtermTerminalError::ErrReentrant;
    assert_eq!(
        AtermTerminalError::from_terminal_lock_error(reentrant),
        AtermTerminalError::ErrReentrant,
    );
}

#[test]
fn detection_error_maps_lock_to_internal() {
    use crate::AtermDetectionError;
    let reentrant = AtermTerminalError::ErrReentrant;
    assert_eq!(
        AtermDetectionError::from_terminal_lock_error(reentrant),
        AtermDetectionError::ErrInternal,
    );
}

#[test]
fn check_null_outputs_passes_on_non_null() {
    fn helper(a: *mut u8, b: *mut u8) -> AtermTerminalError {
        check_null_outputs!(AtermTerminalError::ErrNullOutput, a, b);
        AtermTerminalError::Ok
    }
    let mut x = 0u8;
    let mut y = 0u8;
    assert_eq!(helper(&raw mut x, &raw mut y), AtermTerminalError::Ok);
}

#[test]
fn check_null_outputs_returns_error_on_null() {
    fn helper(a: *mut u8, b: *mut u8) -> AtermTerminalError {
        check_null_outputs!(AtermTerminalError::ErrNullOutput, a, b);
        AtermTerminalError::Ok
    }
    let mut x = 0u8;
    assert_eq!(
        helper(&raw mut x, std::ptr::null_mut()),
        AtermTerminalError::ErrNullOutput,
    );
    assert_eq!(
        helper(std::ptr::null_mut(), &raw mut x),
        AtermTerminalError::ErrNullOutput,
    );
}

#[test]
fn generic_function_using_trait() {
    fn null_guard<E: FfiErrorCode>(term: *const u8) -> E {
        if term.is_null() {
            return E::null_terminal();
        }
        E::ok()
    }
    assert_eq!(
        null_guard::<AtermTerminalError>(std::ptr::null()),
        AtermTerminalError::ErrNullTerminal
    );
    let v = 42u8;
    assert_eq!(
        null_guard::<AtermTerminalError>(&raw const v),
        AtermTerminalError::Ok
    );
}

#[test]
fn null_handle_defaults_to_null_terminal() {
    assert_eq!(
        AtermTerminalError::null_handle(),
        AtermTerminalError::ErrNullTerminal,
    );
}

#[test]
fn checkpoint_null_handle_overrides_default() {
    assert_eq!(
        AtermCheckpointError::null_handle(),
        AtermCheckpointError::ErrNullCheckpoint,
    );
    // null_terminal() is different from null_handle() for checkpoint
    assert_eq!(
        AtermCheckpointError::null_terminal(),
        AtermCheckpointError::ErrNullTerminal,
    );
}

#[test]
fn selection_null_handle_overrides_default() {
    use crate::AtermSelectionError;

    assert_eq!(
        AtermSelectionError::null_handle(),
        AtermSelectionError::ErrNullSelection,
    );
    assert_eq!(
        AtermSelectionError::null_terminal(),
        AtermSelectionError::ErrNullTerminal,
    );
}

#[test]
fn app_error_sentinels() {
    use crate::AtermAppError;
    assert_eq!(AtermAppError::ok(), AtermAppError::Ok);
    assert_eq!(AtermAppError::internal(), AtermAppError::ErrInternal);
    assert_eq!(AtermAppError::null_handle(), AtermAppError::ErrNullApp);
    assert_eq!(AtermAppError::null_output(), AtermAppError::ErrNullOutput);
}

#[test]
fn memory_error_sentinels() {
    use crate::AtermMemoryError;
    assert_eq!(AtermMemoryError::ok(), AtermMemoryError::Ok);
    assert_eq!(AtermMemoryError::internal(), AtermMemoryError::ErrInternal);
    assert_eq!(
        AtermMemoryError::null_handle(),
        AtermMemoryError::ErrNullMemory
    );
    assert_eq!(
        AtermMemoryError::null_output(),
        AtermMemoryError::ErrNullOutput
    );
}

/// Verify null_output() returns ErrNullOutput for all error types.
#[test]
fn null_output_returns_correct_variant() {
    use crate::{AtermGraphicsError, AtermPerceptionError};
    assert_eq!(
        AtermPerceptionError::null_output(),
        AtermPerceptionError::ErrNullOutput
    );
    assert_eq!(
        AtermGraphicsError::null_output(),
        AtermGraphicsError::ErrNullOutput
    );
    assert_eq!(
        AtermCheckpointError::null_output(),
        AtermCheckpointError::ErrNullOutput
    );
}

#[test]
fn generic_handle_null_guard() {
    fn handle_guard<E: FfiErrorCode>(handle: *const u8) -> E {
        if handle.is_null() {
            return E::null_handle();
        }
        E::ok()
    }
    use crate::AtermAppError;
    assert_eq!(
        handle_guard::<AtermAppError>(std::ptr::null()),
        AtermAppError::ErrNullApp,
    );
    assert_eq!(
        handle_guard::<AtermTerminalError>(std::ptr::null()),
        AtermTerminalError::ErrNullTerminal,
    );
}

// =========================================================================
// check_null_term_and_outputs! tests (Part of #4770)
// =========================================================================

#[test]
fn check_null_term_and_outputs_both_valid() {
    fn helper(term: *const u8, out: *mut i32) -> AtermTerminalError {
        check_null_term_and_outputs!(
            AtermTerminalError::ErrNullTerminal,
            AtermTerminalError::ErrNullOutput,
            term,
            out
        );
        AtermTerminalError::Ok
    }
    let v = 1u8;
    let mut out = 0i32;
    assert_eq!(helper(&raw const v, &raw mut out), AtermTerminalError::Ok);
}

#[test]
fn check_null_term_and_outputs_null_term_returns_null_terminal() {
    fn helper(term: *const u8, out: *mut i32) -> AtermTerminalError {
        check_null_term_and_outputs!(
            AtermTerminalError::ErrNullTerminal,
            AtermTerminalError::ErrNullOutput,
            term,
            out
        );
        AtermTerminalError::Ok
    }
    let mut out = 42i32;
    assert_eq!(
        helper(std::ptr::null(), &raw mut out),
        AtermTerminalError::ErrNullTerminal
    );
    // Defense-in-depth: output was zeroed even though term was null.
    assert_eq!(out, 0);
}

#[test]
fn check_null_term_and_outputs_null_output_returns_null_output() {
    fn helper(term: *const u8, out: *mut i32) -> AtermTerminalError {
        check_null_term_and_outputs!(
            AtermTerminalError::ErrNullTerminal,
            AtermTerminalError::ErrNullOutput,
            term,
            out
        );
        AtermTerminalError::Ok
    }
    let v = 1u8;
    assert_eq!(
        helper(&raw const v, std::ptr::null_mut()),
        AtermTerminalError::ErrNullOutput
    );
}

#[test]
fn check_null_term_and_outputs_both_null_returns_null_terminal() {
    fn helper(term: *const u8, out: *mut i32) -> AtermTerminalError {
        check_null_term_and_outputs!(
            AtermTerminalError::ErrNullTerminal,
            AtermTerminalError::ErrNullOutput,
            term,
            out
        );
        AtermTerminalError::Ok
    }
    // When both are null, ErrNullTerminal takes precedence.
    assert_eq!(
        helper(std::ptr::null(), std::ptr::null_mut()),
        AtermTerminalError::ErrNullTerminal
    );
}

#[test]
fn check_null_term_and_outputs_multiple_outputs() {
    fn helper(term: *const u8, out_a: *mut i32, out_b: *mut bool) -> AtermTerminalError {
        check_null_term_and_outputs!(
            AtermTerminalError::ErrNullTerminal,
            AtermTerminalError::ErrNullOutput,
            term,
            out_a,
            out_b
        );
        AtermTerminalError::Ok
    }
    let v = 1u8;
    let mut a = 99i32;
    let mut b = true;
    // Both valid → Ok, and outputs were zeroed by defense-in-depth.
    assert_eq!(
        helper(&raw const v, &raw mut a, &raw mut b),
        AtermTerminalError::Ok
    );
    assert_eq!(a, 0);
    assert!(!b);
}

#[test]
fn check_null_term_and_outputs_second_output_null() {
    fn helper(term: *const u8, out_a: *mut i32, out_b: *mut i32) -> AtermTerminalError {
        check_null_term_and_outputs!(
            AtermTerminalError::ErrNullTerminal,
            AtermTerminalError::ErrNullOutput,
            term,
            out_a,
            out_b
        );
        AtermTerminalError::Ok
    }
    let v = 1u8;
    let mut a = 0i32;
    assert_eq!(
        helper(&raw const v, &raw mut a, std::ptr::null_mut()),
        AtermTerminalError::ErrNullOutput
    );
}
