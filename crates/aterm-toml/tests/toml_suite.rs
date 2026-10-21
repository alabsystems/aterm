// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-toml integration files as modules of one test binary, sharing one copy of
//! `tests/common`. None touches the process environment or a global.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a `mod` line
//! here nor a `[[test]]` of its own never compiles.

mod common;

mod cost;
mod depth;
mod edit;
mod fuzz;
mod roundtrip;
mod spec;
mod typed;
