// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The amacs-files integration files as modules of ONE test binary.
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p amacs-files --test amacs_files_suite -- <file>::`.
