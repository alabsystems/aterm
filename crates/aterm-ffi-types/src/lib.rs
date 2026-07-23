// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! FFI boundary types, error enums, safety helpers, and pointer lifecycle
//! tracking for the aterm terminal engine.
//!
//! Extracted from `aterm-types` to separate C-ABI infrastructure from pure
//! terminal domain types (#3353). This crate owns:
//!
//! - Error enums (`AtermTerminalError`, `AtermConfigError`, etc.)
//! - FFI combinator traits and macros (`FfiErrorCode`, `check_null_outputs!`)
//! - Pointer safety helpers (`ffi_ref`, `ffi_slice`, bounds validation)
//! - Panic catching (`aterm_ffi_catch_panic!`)
//! - Pointer lifecycle tracking (`FfiTracker`, free-tracking)
//! - Free combinators (`box_handle_free_v1`, `box_handle_free_v2`, etc.)

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]
#![deny(clippy::all)]

pub mod callback_struct_manifest;
pub mod ffi_bounds;
mod ffi_callback;
pub mod ffi_combinator;
mod ffi_error;
pub mod ffi_error_contract;
pub mod ffi_error_types;
pub mod ffi_free_combinator;
pub mod ffi_graphics_types;
mod ffi_panic;
pub mod ffi_safety;
pub mod verification;

// F11-2 (#7941): re-export aterm_log so `aterm_ffi_catch_panic!` macro
// expansions resolve the logger without requiring every downstream caller
// to add aterm-log to their own Cargo.toml.
#[doc(hidden)]
pub use aterm_log;

// Re-export the panic-payload helper so `aterm_ffi_catch_unwind!` macro
// expansions in downstream crates can reach it via `$crate::`.
#[doc(hidden)]
pub use ffi_panic::panic_payload_msg;

// Re-export FFI safety helpers at crate root for ergonomic imports.
pub use ffi_safety::{
    ffi_array_slice, ffi_array_slice_mut, ffi_byte_slice, ffi_byte_slice_mut, ffi_ref, ffi_ref_mut,
    ffi_ref_mut_tracked, ffi_ref_tracked, ffi_slice, ffi_slice_mut,
};

// Re-export unified error contract types at crate root.
pub use ffi_combinator::FfiErrorCode;
pub use ffi_error_contract::{AtermErrorDomain, AtermErrorInfo, AtermErrorKind, AtermFfiErrorCode};

// Re-export free combinators and tracker at crate root.
pub use ffi_free_combinator::{
    box_handle_free_v1, box_handle_free_v1_with_teardown, box_handle_free_v2,
    box_handle_free_v2_nulling, box_handle_free_v2_with_null, box_handle_free_v2_with_teardown,
    cstring_handle_free_v1,
};
pub use verification::FfiTracker;

// Re-export graphics FFI types at crate root.
pub use ffi_graphics_types::{
    AtermGraphicsError, AtermKittyImageInfo, AtermKittyPlacement, AtermKittyPlacementLocation,
};

// Re-export domain FFI error types at crate root.
#[allow(deprecated)] // AtermApprovalError kept for ABI stability
pub use ffi_error_types::{
    AtermAppError, AtermApprovalError, AtermBidiError, AtermCheckpointError, AtermConfigError,
    AtermDetectionError, AtermImeError, AtermMemoryError, AtermPerceptionError, AtermResponseError,
    AtermSelectionError, AtermSixelError, AtermTerminalError,
};

// Re-export FfiCallback at crate root.
pub use ffi_callback::FfiCallback;

// Re-export SendContext at crate root (#5697 Phase 1: dedup).
mod send_context;
pub use send_context::SendContext;

/// Maximum length parameter accepted by FFI slice-creation functions (256 MiB).
///
/// Prevents out-of-bounds access from corrupted or malicious length values (CWE-120 / #2641).
pub const MAX_FFI_BUFFER_SIZE: usize = 256 * 1024 * 1024;
