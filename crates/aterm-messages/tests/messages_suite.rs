// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-messages integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s. None of them touches
//! the process environment or a global.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.

mod conformance_digest;
mod conformance_digest_ends;
mod motion_review;
