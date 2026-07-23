// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! FFI combinator traits and macros for reducing boilerplate in `extern "C"` functions.
//!
//! The ~200+ FFI entry points in aterm share a common preamble: panic catching,
//! null terminal guard, output pointer validation, lock acquisition, and error
//! translation. This module provides:
//!
//! - [`FfiErrorCode`] — trait that unifies error enum sentinel constructors
//! - `check_null_outputs!` — variadic macro for output pointer validation
//!
//! Higher-order combinator functions (`terminal_read_ffi`, `terminal_write_ffi`)
//! live in `aterm-core::ffi::combinator` since they depend on `AtermTerminal`.
//!
//! Part of #4660.

use crate::{AtermCheckpointError, AtermTerminalError};

/// Trait for FFI error types that support the terminal combinator pattern.
///
/// Every `AtermXxxError` enum with terminal-facing FFI functions has the same
/// sentinel variants: `Ok`, `ErrInternal`, `ErrNullTerminal`, `ErrNullOutput`.
/// This trait provides constructor methods so generic combinators can work
/// across all error types.
///
/// # Example
///
/// ```no_run
/// use aterm_ffi_types::FfiErrorCode;
///
/// fn example<E: FfiErrorCode>(ptr: *mut i32) -> E {
///     if ptr.is_null() {
///         return E::null_output();
///     }
///     E::ok()
/// }
/// ```
pub trait FfiErrorCode: Copy {
    /// Success value (e.g., `AtermTerminalError::Ok`).
    fn ok() -> Self;

    /// Internal error (panic, unexpected state).
    fn internal() -> Self;

    /// Null terminal pointer was passed.
    fn null_terminal() -> Self;

    /// Null output pointer was passed.
    fn null_output() -> Self;

    /// Null handle pointer was passed (generic — works for any opaque handle type).
    ///
    /// Default delegates to `null_terminal()` for backward compatibility with
    /// terminal-centric error types. Override for non-terminal handle types
    /// where the null-handle sentinel differs (e.g., `AtermAppError::ErrNullApp`,
    /// `AtermCheckpointError::ErrNullCheckpoint`).
    fn null_handle() -> Self {
        Self::null_terminal()
    }

    /// Double-free detected.
    ///
    /// Default returns `internal()`. Override for error types with a specific
    /// `ErrDoubleFree` or `DoubleFree` variant (e.g., `AtermTerminalError`,
    /// `AtermAppError`, `AtermGpuError`).
    fn double_free() -> Self {
        Self::internal()
    }

    /// Map a terminal lock/reentrant error to this error type.
    ///
    /// Default returns `internal()`. Override for `AtermTerminalError` to
    /// preserve the specific `ErrReentrant` variant.
    fn from_terminal_lock_error(_err: AtermTerminalError) -> Self {
        Self::internal()
    }
}

/// Validates that all provided output pointers are non-null.
///
/// Returns the specified error if any pointer is null. This replaces the
/// repetitive `if ptr.is_null() { return Err; }` chains found in ~200+
/// FFI functions.
///
/// # Example
///
/// ```no_run
/// use aterm_ffi_types::AtermTerminalError;
///
/// unsafe extern "C" fn example(out_x: *mut i32, out_y: *mut i32, out_z: *mut i32) -> AtermTerminalError {
///     aterm_ffi_types::check_null_outputs!(AtermTerminalError::ErrNullOutput, out_x, out_y, out_z);
///     AtermTerminalError::Ok
/// }
/// ```
#[macro_export]
macro_rules! check_null_outputs {
    ($err:expr_2021, $($ptr:expr_2021),+ $(,)?) => {
        $(if $ptr.is_null() { return $err; })+
    };
}

/// Validates terminal pointer and output pointers with correct null-check ordering.
///
/// This macro enforces the invariant that `ErrNullTerminal` takes precedence
/// over `ErrNullOutput` when both the terminal and output pointers are null.
/// It also provides defense-in-depth zeroing of output pointers before any
/// early return.
///
/// Sequence:
/// 1. Zero all non-null output pointers to `Default::default()` (defense-in-depth)
/// 2. If `term` is null, return `$err_term`
/// 3. If any output pointer is null, return `$err_output`
///
/// # Safety
///
/// Output pointers that are non-null must be valid for writes. The caller is
/// responsible for ensuring this (standard FFI contract).
///
/// # Example
///
/// ```no_run
/// use aterm_ffi_types::AtermTerminalError;
///
/// unsafe extern "C" fn example(
///     term: *const (),
///     out_x: *mut i32,
///     out_y: *mut i32,
/// ) -> AtermTerminalError {
///     aterm_ffi_types::check_null_term_and_outputs!(
///         AtermTerminalError::ErrNullTerminal,
///         AtermTerminalError::ErrNullOutput,
///         term,
///         out_x, out_y
///     );
///     AtermTerminalError::Ok
/// }
/// ```
///
/// Part of #4770.
#[macro_export]
macro_rules! check_null_term_and_outputs {
    ($err_term:expr_2021, $err_output:expr_2021, $term:expr_2021, $($out:expr_2021),+ $(,)?) => {
        // Defense-in-depth: zero outputs before any early return, if non-null.
        // SAFETY: Caller guarantees non-null output pointers are valid for writes
        // and properly aligned. The is_null() guard skips null pointers.
        $( if !$out.is_null() { unsafe { *$out = Default::default(); } } )+
        if $term.is_null() {
            return $err_term;
        }
        $crate::check_null_outputs!($err_output, $($out),+);
    };
}

// =============================================================================
// FfiErrorCode implementations for terminal-centric error types
// =============================================================================

/// Generates `impl FfiErrorCode` for domain error types that follow the
/// standard `Ok / ErrInternal / ErrNullTerminal / ErrNullOutput` pattern.
///
/// Five arms handle the known variation points:
///
/// - **default**: maps to `Ok`, `ErrInternal`, `ErrNullTerminal`, `ErrNullOutput`
/// - **null_handle**: same as default plus a `null_handle()` override
/// - **null_terminal + null_handle**: overrides both `null_terminal()` and `null_handle()`
/// - **null_terminal + null_handle + double_free**: overrides all three
/// - **preserve_terminal_lock_error + double_free**: identity pass-through with double-free
macro_rules! impl_ffi_error_code {
    // Default mapping: Ok, ErrInternal, ErrNullTerminal, ErrNullOutput.
    ($ty:ty) => {
        impl FfiErrorCode for $ty {
            fn ok() -> Self {
                Self::Ok
            }
            fn internal() -> Self {
                Self::ErrInternal
            }
            fn null_terminal() -> Self {
                Self::ErrNullTerminal
            }
            fn null_output() -> Self {
                Self::ErrNullOutput
            }
        }
    };
    // Default + null_handle() override.
    ($ty:ty, null_handle = $variant:ident) => {
        impl FfiErrorCode for $ty {
            fn ok() -> Self {
                Self::Ok
            }
            fn internal() -> Self {
                Self::ErrInternal
            }
            fn null_terminal() -> Self {
                Self::ErrNullTerminal
            }
            fn null_output() -> Self {
                Self::ErrNullOutput
            }
            fn null_handle() -> Self {
                Self::$variant
            }
        }
    };
    // Override both null_terminal() and null_handle().
    ($ty:ty, null_terminal = $nt:ident, null_handle = $nh:ident) => {
        impl FfiErrorCode for $ty {
            fn ok() -> Self {
                Self::Ok
            }
            fn internal() -> Self {
                Self::ErrInternal
            }
            fn null_terminal() -> Self {
                Self::$nt
            }
            fn null_output() -> Self {
                Self::ErrNullOutput
            }
            fn null_handle() -> Self {
                Self::$nh
            }
        }
    };
    // Override null_terminal + null_handle + double_free.
    ($ty:ty, null_terminal = $nt:ident, null_handle = $nh:ident, double_free = $df:ident) => {
        impl FfiErrorCode for $ty {
            fn ok() -> Self {
                Self::Ok
            }
            fn internal() -> Self {
                Self::ErrInternal
            }
            fn null_terminal() -> Self {
                Self::$nt
            }
            fn null_output() -> Self {
                Self::ErrNullOutput
            }
            fn null_handle() -> Self {
                Self::$nh
            }
            fn double_free() -> Self {
                Self::$df
            }
        }
    };
    // Preserve terminal lock error (identity pass-through) + double_free.
    ($ty:ty, preserve_terminal_lock_error, double_free = $df:ident) => {
        impl FfiErrorCode for $ty {
            fn ok() -> Self {
                Self::Ok
            }
            fn internal() -> Self {
                Self::ErrInternal
            }
            fn null_terminal() -> Self {
                Self::ErrNullTerminal
            }
            fn null_output() -> Self {
                Self::ErrNullOutput
            }
            fn from_terminal_lock_error(err: AtermTerminalError) -> Self {
                err
            }
            fn double_free() -> Self {
                Self::$df
            }
        }
    };
}

// Preserve lock error (identity pass-through for AtermTerminalError itself).
impl_ffi_error_code!(
    AtermTerminalError,
    preserve_terminal_lock_error,
    double_free = ErrDoubleFree
);

// Default mapping: Ok, ErrInternal, ErrNullTerminal, ErrNullOutput.
impl_ffi_error_code!(crate::AtermDetectionError);
impl_ffi_error_code!(crate::AtermBidiError);
impl_ffi_error_code!(crate::AtermImeError);
impl_ffi_error_code!(crate::AtermResponseError);
impl_ffi_error_code!(crate::AtermSixelError);
impl_ffi_error_code!(crate::AtermPerceptionError, null_handle = ErrNullPerception);
impl_ffi_error_code!(crate::AtermGraphicsError);

// Default + null_handle() override for handle-centric error types.
impl_ffi_error_code!(crate::AtermSelectionError, null_handle = ErrNullSelection);
impl_ffi_error_code!(AtermCheckpointError, null_handle = ErrNullCheckpoint);

// Override both null_terminal() and null_handle() for app/memory handles
// where the null-handle sentinel differs from ErrNullTerminal.
impl_ffi_error_code!(
    crate::AtermAppError,
    null_terminal = ErrNullApp,
    null_handle = ErrNullApp,
    double_free = ErrDoubleFree
);
impl_ffi_error_code!(
    crate::AtermMemoryError,
    null_terminal = ErrNullMemory,
    null_handle = ErrNullMemory
);

#[cfg(test)]
#[path = "ffi_combinator_tests.rs"]
mod tests;
