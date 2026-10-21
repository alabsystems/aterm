// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crate's integration test files as modules of one test binary. Both clear a variable
//! through `aterm_log::env::scoped_unset`, whose lock serialises them.
//!
//! `autotests = false`: a `tests/*.rs` file that is neither a `mod` here nor its own
//! `[[test]]` never compiles. Run one file's tests with
//! `targo --unverified test -p aterm-shell-integration --test shell_suite -- <file>::`.

mod prepare_fish_xdg_default;
mod prepare_zsh_unset_zdotdir;
