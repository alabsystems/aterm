// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::per_user_default;
use std::path::PathBuf;

/// In a test binary the per-user default is the binary's scratch root's `run`
/// folder, so a test that names no folder never reaches the person's live
/// socket; one the environment names (`$XDG_RUNTIME_DIR`, when this run has
/// one of its own) still wins.
#[test]
fn a_test_binary_resolves_a_control_dir_of_its_own() {
    let root = crate::test_binary::scratch_root().expect("a test binary's root");
    let runtime = std::env::var_os("XDG_RUNTIME_DIR");
    let chosen = runtime
        .clone()
        .filter(|dir| !crate::test_binary::is_login_runtime_dir(dir));
    let Some(chosen) = chosen else {
        assert_eq!(super::control_socket_dir(), Some(root.join("run")));
        let person = super::control_socket_dir_inner(None);
        let persons = match runtime {
            Some(login) => Some(PathBuf::from(login).join("aterm")),
            None => per_user_default(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME")),
        };
        assert_eq!(person, persons);
        assert!(person.is_some_and(|dir| !dir.starts_with(root)));
        return;
    };
    assert_eq!(
        super::control_socket_dir(),
        Some(PathBuf::from(chosen).join("aterm"))
    );
}

/// With `$XDG_RUNTIME_DIR` unset the control folder is aterm's state folder
/// on macOS, and on every other Unix a `run` folder under aterm's XDG state
/// folder — `$XDG_STATE_HOME` when absolute, else `~/.local/state` — never a
/// `~/Library` in a Linux home.
#[test]
fn the_default_control_dir_follows_each_platforms_state_folder() {
    let os = |s: &str| Some(std::ffi::OsString::from(s));
    let home = os("/home/u");
    if cfg!(target_os = "macos") {
        assert_eq!(
            per_user_default(os("/x/state"), home),
            Some(PathBuf::from("/home/u/Library/Application Support/aterm"))
        );
        return;
    }
    assert_eq!(
        per_user_default(os("/x/state"), home.clone()),
        Some(PathBuf::from("/x/state/aterm/run"))
    );
    for state in [None, os(""), os("relative/state")] {
        assert_eq!(
            per_user_default(state.clone(), home.clone()),
            Some(PathBuf::from("/home/u/.local/state/aterm/run")),
            "XDG_STATE_HOME {state:?} is no state root"
        );
    }
    assert_eq!(per_user_default(None, None), None);
    assert_eq!(
        per_user_default(os("/x/state"), None),
        Some(PathBuf::from("/x/state/aterm/run")),
        "an absolute XDG_STATE_HOME needs no HOME"
    );
}
