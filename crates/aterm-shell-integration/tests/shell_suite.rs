// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-shell-integration integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s.
//! Both files clear a variable through `aterm_log::env::scoped_unset`, whose
//! one lock serialises them, so they share a process safely.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.
//!
//! Run one file's tests with
//! `targo --unverified test -p aterm-shell-integration --test shell_suite -- <file>::`.

mod prepare_fish_xdg_default;
mod prepare_zsh_unset_zdotdir;
