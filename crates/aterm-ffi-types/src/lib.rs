// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! FFI boundary helpers for the aterm C ABI (`aterm-ffi`).
//!
//! This crate owns:
//!
//! - Panic catching (`aterm_ffi_catch_unwind!`): a Rust panic must never unwind
//!   across the C ABI, so every exported function body runs inside it.
//! - Bounded pointer+len conversion ([`ffi_byte_slice`]), capped at 64 MiB.

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]
#![deny(clippy::all)]

mod ffi_bounds;
mod ffi_panic;
mod ffi_safety;

// F11-2 (#7941): re-export aterm_log so `aterm_ffi_catch_unwind!` macro
// expansions resolve the logger without requiring every downstream caller
// to add aterm-log to their own Cargo.toml.
#[doc(hidden)]
pub use aterm_log;

// Re-export the panic-payload helper so `aterm_ffi_catch_unwind!` macro
// expansions in downstream crates can reach it via `$crate::`.
#[doc(hidden)]
pub use ffi_panic::panic_payload_msg;

pub use ffi_safety::ffi_byte_slice;
