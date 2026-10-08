// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-toml integration files as modules of ONE test binary, sharing ONE
//! copy of `tests/common` (declared here; each file reaches it as
//! `crate::common`). Each file stays at its own path and keeps its own
//! `#[test]`s; none touches the process environment or a global.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.

mod common;

mod cost;
mod depth;
mod edit;
mod fuzz;
mod roundtrip;
mod spec;
mod typed;
