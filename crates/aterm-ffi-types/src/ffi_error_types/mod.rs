// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Domain-specific FFI error enums for the aterm terminal engine.
//!
//! These `#[repr(C)]` error enums are shared across multiple crates
//! (`aterm-core`, `aterm-core-ffi`, domain extraction crates).
//! Centralizing them in `aterm-types` breaks the dependency on the
//! `aterm-core` monolith and enables domain crates to own their FFI
//! surfaces end-to-end (Part of #2584).
//!
//! All enums follow `docs/FFI_GUIDELINES.md` error code ranges:
//! - 0: Success
//! - 1-9: Null pointer errors
//! - 10-19: Parameter/configuration errors
//! - 20-29: Resource errors
//! - 30+: Domain-specific errors
//! - 255: Unknown/future compatibility

mod core_errors;
mod extension_errors;

pub use core_errors::*;
pub use extension_errors::*;
