// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Screen capture contract types shared across FFI crates.
//!
//! The mutable process-global callback registry lives in
//! `aterm-runtime-callbacks::screen_capture`. This module provides only the
//! error enum and callback type alias so that FFI signatures can reference
//! them without pulling in runtime state.

use std::ffi::c_void;

/// Error returned when a screen capture fails.
#[non_exhaustive]
#[derive(Debug, aterm_error::Error)]
pub enum CaptureError {
    /// No callback has been registered (expected during startup).
    #[error("no screen capture callback registered")]
    NoCallback,
    /// The native callback returned a non-zero error code.
    #[error("screen capture callback returned error code {0}")]
    CallbackFailed(i32),
    /// The native callback returned a null data pointer or zero length.
    #[error("screen capture callback returned null or empty data")]
    NullData,
    /// The buffer exceeds `MAX_FFI_BUFFER_SIZE`.
    #[error("screen capture buffer exceeds MAX_FFI_BUFFER_SIZE ({size} bytes)")]
    OversizedBuffer {
        /// Reported buffer size in bytes.
        size: usize,
    },
}

/// Callback type for screen capture (C ABI).
///
/// Called from Rust, implemented in the native UI (Swift Metal renderer).
/// Renders the terminal offscreen and writes PNG bytes to the output pointers.
/// The buffer at `*out_data` is only valid until the callback returns.
///
/// Returns 0 on success, non-zero on failure.
pub type ScreenCaptureCallback = unsafe extern "C" fn(
    context: *mut c_void,
    out_data: *mut *const u8,
    out_len: *mut usize,
) -> i32;
