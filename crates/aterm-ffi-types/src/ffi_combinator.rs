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
