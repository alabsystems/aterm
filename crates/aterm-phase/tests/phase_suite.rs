// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-phase integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-phase --test phase_suite -- <file>::`.

#[path = "common/fixture_files.rs"]
mod fixture_files;

mod busy_zone_sweep;
mod live_zone_sweep;
