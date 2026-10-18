// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-grid integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-grid --test grid_suite -- <file>::`.

mod conformance_retention;
mod conformance_top_anchored_scroll;
mod grid_unsafe_access_properties;
mod history_renumber_epoch;
mod security_regression_5573;
