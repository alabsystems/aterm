// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-types integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s. None of them touches the
//! process environment, installs a hook or an allocator, or keeps a global, so
//! they share a process safely.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-types --test types_suite -- <file>::`.

mod color_space;
mod kitty_keyboard_tests;
