// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: when XDG_DATA_DIRS is unset, `prepare_fish` must fall
//! back to the XDG Base Directory spec default (`/usr/local/share:/usr/share`)
//! rather than emit only our injection dir — otherwise fish would lose its
//! own vendor conf.d lookup paths.
//!
//! This test MUTATES the process environment (`remove_var("XDG_DATA_DIRS")`)
//! and `prepare_fish` READS it. Inside the crate's multi-test unit binary
//! that was a race against sibling tests calling `prepare_into` on parallel
//! test threads, so it lives here as a SINGLE-test-per-binary integration
//! test — the same convention as aterm-containment's `init_from_env_*` and
//! aterm-render's `no_procedural_glyphs_env`.

use aterm_shell_integration::{ShellType, prepare_into};

#[test]
fn prepare_fish_xdg_includes_default_fallback() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    // XDG_DATA_DIRS is cleared for exactly the length of the `prepare_into` call
    // and restored on the way out — on a panic as well as a return — by the
    // workspace's one lock-scoped env helper, so nothing leaks into a test added
    // to this binary later or into a same-process embedder of this harness.
    let result =
        aterm_log::env::scoped_unset("XDG_DATA_DIRS", || prepare_into(ShellType::Fish, &base))
            .unwrap()
            .unwrap();
    let xdg = result
        .env_add
        .iter()
        .find(|(k, _)| k == "XDG_DATA_DIRS")
        .expect("fish injection must set XDG_DATA_DIRS");

    assert!(
        xdg.1.contains("/usr/local/share") && xdg.1.contains("/usr/share"),
        "XDG_DATA_DIRS must include XDG spec defaults when unset; got: {}",
        xdg.1
    );
}
