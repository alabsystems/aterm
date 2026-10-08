// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-regex integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.

mod bounds;
mod differential;
mod fuzz;
