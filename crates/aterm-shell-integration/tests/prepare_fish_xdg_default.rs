// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Separate test binary because `scoped_unset` mutates the process environment while unit tests
//! call `prepare_into` in parallel.

use aterm_shell_integration::{ShellType, prepare_into};

#[test]
fn prepare_fish_xdg_includes_default_fallback() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
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
