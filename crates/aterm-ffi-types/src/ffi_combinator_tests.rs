// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for the [`FfiErrorCode`] trait and its impls.
//!
//! Split from `ffi_combinator.rs` to stay under 500-line limit.

use super::*;

/// One sentinel row: `(what, constructor, expected variant)`.
type Sentinel<E> = (&'static str, fn() -> E, E);

/// Assert each sentinel constructor of one error type returns its variant.
fn assert_sentinels<E: FfiErrorCode + PartialEq + std::fmt::Debug>(rows: &[Sentinel<E>]) {
    for (what, sentinel, want) in rows {
        assert_eq!(sentinel(), *want, "{what}");
    }
}

/// Every error type's sentinel constructors return its own variants, and
/// `null_handle()` falls back to `null_terminal()` unless the type overrides it
/// with a handle-specific variant.
#[test]
fn sentinel_constructors() {
    use crate::{
        AtermAppError, AtermGraphicsError, AtermMemoryError, AtermPerceptionError,
        AtermSelectionError,
    };
    type T = AtermTerminalError;
    assert_sentinels::<T>(&[
        ("terminal ok", T::ok, T::Ok),
        ("terminal internal", T::internal, T::ErrInternal),
        (
            "terminal null_terminal",
            T::null_terminal,
            T::ErrNullTerminal,
        ),
        ("terminal null_output", T::null_output, T::ErrNullOutput),
        // null_handle defaults to null_terminal.
        ("terminal null_handle", T::null_handle, T::ErrNullTerminal),
    ]);
    type C = AtermCheckpointError;
    assert_sentinels::<C>(&[
        (
            "checkpoint null_handle",
            C::null_handle,
            C::ErrNullCheckpoint,
        ),
        // null_terminal() is different from null_handle() for checkpoint
        (
            "checkpoint null_terminal",
            C::null_terminal,
            C::ErrNullTerminal,
        ),
        ("checkpoint null_output", C::null_output, C::ErrNullOutput),
    ]);
    type S = AtermSelectionError;
    assert_sentinels::<S>(&[
        ("selection null_handle", S::null_handle, S::ErrNullSelection),
        (
            "selection null_terminal",
            S::null_terminal,
            S::ErrNullTerminal,
        ),
    ]);
    type A = AtermAppError;
    assert_sentinels::<A>(&[
        ("app ok", A::ok, A::Ok),
        ("app internal", A::internal, A::ErrInternal),
        ("app null_handle", A::null_handle, A::ErrNullApp),
        ("app null_output", A::null_output, A::ErrNullOutput),
    ]);
    type M = AtermMemoryError;
    assert_sentinels::<M>(&[
        ("memory ok", M::ok, M::Ok),
        ("memory internal", M::internal, M::ErrInternal),
        ("memory null_handle", M::null_handle, M::ErrNullMemory),
        ("memory null_output", M::null_output, M::ErrNullOutput),
    ]);
    assert_sentinels::<AtermPerceptionError>(&[(
        "perception null_output",
        AtermPerceptionError::null_output,
        AtermPerceptionError::ErrNullOutput,
    )]);
    assert_sentinels::<AtermGraphicsError>(&[(
        "graphics null_output",
        AtermGraphicsError::null_output,
        AtermGraphicsError::ErrNullOutput,
    )]);
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
