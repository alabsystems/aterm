// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// docs/DESIGN-init-system-2026-10-20.md §10 S2: the hermetic helper's child prints the
/// scratch HOME and XDG dirs.
/// NEGATIVE CONTROL: the same child without the helper prints this process's
/// own HOME. `/bin/sh -c printf` reads and writes nothing in either home.
#[test]
fn the_hermetic_childs_home_is_the_scratch_and_a_plain_childs_is_not() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let print = r#"printf '%s\n' "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME" "${ZDOTDIR-unset}" "${BASH_ENV-unset}""#;
    let mut cmd = shell_command("/bin/sh", scratch.path());
    let cleared: Vec<&std::ffi::OsStr> = cmd
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key)
        .collect();
    assert_eq!(cleared, ["BASH_ENV", "ZDOTDIR"].map(std::ffi::OsStr::new));
    let out = cmd.args(["-c", print]).output().expect("/bin/sh");
    assert!(out.status.success(), "{out:?}");
    let home = scratch.path().display().to_string();
    let want: Vec<String> = [
        home.clone(),
        format!("{home}/.config"),
        format!("{home}/.local/share"),
        format!("{home}/.local/state"),
        format!("{home}/.cache"),
        "unset".into(),
        "unset".into(),
    ]
    .into();
    let got: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(got, want);

    let plain = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", r#"printf '%s' "$HOME""#])
        .output()
        .expect("/bin/sh");
    let real = std::env::var("HOME").unwrap_or_default();
    let got = String::from_utf8_lossy(&plain.stdout);
    assert_eq!(
        got, real,
        "without the helper the child has this process's HOME"
    );
    assert_ne!(got, home, "and that is not the scratch");
}
