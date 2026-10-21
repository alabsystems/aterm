// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! With ZDOTDIR unset, `prepare_zsh` emits `ATERM_UNSET_ZDOTDIR=1`, so the wrapper `.zshenv`
//! unsets ZDOTDIR instead of restoring a value that never existed.
//!
//! It mutates the process environment, so it lives outside the unit-test binary (whose tests
//! call `prepare_into` in parallel); `aterm_log::env::scoped_unset`'s lock serialises it with
//! the other env test in `shell_suite` and restores the variable even on a panic.

use aterm_shell_integration::{ShellType, prepare_into};

#[test]
fn prepare_zsh_sets_unset_zdotdir_when_empty() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    let result = aterm_log::env::scoped_unset("ZDOTDIR", || prepare_into(ShellType::Zsh, &base))
        .unwrap()
        .unwrap();

    let has_unset_marker = result
        .env_add
        .iter()
        .any(|(k, v)| k == "ATERM_UNSET_ZDOTDIR" && v == "1");
    assert!(
        has_unset_marker,
        "prepare_zsh must set ATERM_UNSET_ZDOTDIR=1 when ZDOTDIR is unset"
    );
}
