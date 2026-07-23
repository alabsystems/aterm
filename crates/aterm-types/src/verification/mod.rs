// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared verification helpers used across extracted crates.
//!
//! - `stubs`: Kani-friendly replacements for `HashMap`, `HashSet`, `Instant`, `VecDeque`
//!
//! FFI pointer lifecycle tracking (`ffi_free_tracker`, `terminal_handle_tracker`)
//! lives in `aterm-ffi-types` (#3353).

/// Verification-friendly container stubs for Kani proofs.
pub mod stubs;
