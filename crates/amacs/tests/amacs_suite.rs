// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The amacs integration files as modules of ONE test binary.
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! The engine's own tests live in its source files (`#[cfg(test)]` modules),
//! so parallel slices never collide on this file. Run one file's tests with
//! `targo --unverified test -p amacs --test amacs_suite -- <file>::`.
