// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! With XDG_DATA_DIRS unset, `prepare_fish` falls back to the XDG Base Directory default
//! (`/usr/local/share:/usr/share`) instead of only our injection dir, so fish keeps its
//! vendor conf.d paths.
//!
//! It mutates the process environment, so it lives outside the unit-test binary (whose tests
//! call `prepare_into` in parallel); `aterm_log::env::scoped_unset`'s lock serialises it with
//! the other env test in `shell_suite` and restores the variable even on a panic.

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
