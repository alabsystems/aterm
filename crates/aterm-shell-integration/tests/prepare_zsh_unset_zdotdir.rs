// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: `prepare_zsh` must emit `ATERM_UNSET_ZDOTDIR=1` when
//! ZDOTDIR is unset, so the wrapper `.zshenv` knows to `unset ZDOTDIR`
//! instead of restoring a value that never existed.
//!
//! This test MUTATES the process environment (`remove_var("ZDOTDIR")`) and
//! `prepare_zsh` READS it. Inside the crate's multi-test unit binary that
//! was a race against sibling tests calling `prepare_into` on parallel test
//! threads, so it lives here as a SINGLE-test-per-binary integration test —
//! the same convention as aterm-containment's `init_from_env_*` and
//! aterm-render's `no_procedural_glyphs_env`.

use aterm_shell_integration::{ShellType, prepare_into};

/// Restores ZDOTDIR to its pre-test value on drop (including on panic), so
/// state cannot leak into a test added to this binary later or into a
/// same-process embedder of this harness.
struct RestoreZdotdir(Option<std::ffi::OsString>);

impl Drop for RestoreZdotdir {
    fn drop(&mut self) {
        // SAFETY: this binary contains exactly ONE #[test]; only this test
        // thread reads or writes the environment.
        unsafe {
            match self.0.take() {
                Some(prev) => std::env::set_var("ZDOTDIR", prev),
                None => std::env::remove_var("ZDOTDIR"),
            }
        }
    }
}

#[test]
fn prepare_zsh_sets_unset_zdotdir_when_empty() {
    let _restore = RestoreZdotdir(std::env::var_os("ZDOTDIR"));

    // Clear ZDOTDIR to simulate unset.
    // SAFETY: this binary contains exactly ONE #[test]; only this test
    // thread reads or writes the environment.
    unsafe { std::env::remove_var("ZDOTDIR") };

    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    let result = prepare_into(ShellType::Zsh, &base).unwrap().unwrap();

    let has_unset_marker = result
        .env_add
        .iter()
        .any(|(k, v)| k == "ATERM_UNSET_ZDOTDIR" && v == "1");
    assert!(
        has_unset_marker,
        "prepare_zsh must set ATERM_UNSET_ZDOTDIR=1 when ZDOTDIR is unset"
    );
}
