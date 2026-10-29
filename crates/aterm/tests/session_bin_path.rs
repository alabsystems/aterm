// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE `--session` LANE HANDS ITS SHELL THE MANAGED `bin/`, AHEAD OF `/usr/bin` AND ONCE, FROM ITS
//! FIRST BYTE (`crates/aterm/src/main.rs`, through `atpkg::session_bin_dir` and
//! `atpkg::store::prepend_bin_to_path`): a script, an agent or a shell that never sources the hook
//! still finds a tool (`git`, `gh`) once its shim lands, ahead of Apple's `/usr/bin/git` CLT stub.

use std::io::Write as _;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::launch_isolation;

/// The PATH `aterm --session` hands `/bin/sh` over `root`, from `PATH=/usr/bin:/bin:/custom/after-system`.
fn session_path(root: &std::path::Path) -> String {
    #[expect(
        clippy::disallowed_types,
        reason = "launch_isolation::apply arms a std Command, and strips this process's descriptors from it itself"
    )]
    let mut cmd = std::process::Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["--session", launch_isolation::NO_REROUTE])
        .env("PATH", "/usr/bin:/bin:/custom/after-system")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn the aterm binary");
    drop(cmd);
    child
        .stdin
        .take()
        .expect("aterm stdin")
        .write_all(b"printf 'PATH_%s=<%s>\\n' $((6*7)) \"$PATH\"\nexit\n")
        .expect("type the line");
    let out = crate::child_output::wait_and_read(child, Instant::now() + Duration::from_secs(60))
        .expect("`aterm --session` did not exit within a minute");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{:?}: {stdout:?}", out.status);
    let (_, rest) = stdout
        .split_once("PATH_42=<")
        .unwrap_or_else(|| panic!("the shell printed no PATH: {stdout:?}"));
    let (path, _) = rest
        .split_once('>')
        .unwrap_or_else(|| panic!("an unterminated PATH: {stdout:?}"));
    path.to_string()
}

#[test]
fn a_session_hands_its_shell_the_managed_bin_last_and_once_where_the_hook_is() {
    let root = launch_isolation::scratch_root("aterm-session-bin-path");
    let home = root.path().join("home");
    let bin = atpkg::store::Layout {
        prefix: atpkg::store::default_prefix(&home),
    }
    .bin_dir();
    let bin = bin.to_str().expect("a UTF-8 scratch path");

    let unsurfaced = session_path(root.path());
    assert!(
        !unsurfaced.split(':').any(|entry| entry == bin),
        "no hook and the lanes off: nothing is surfaced: {unsurfaced:?}"
    );

    let shell_d = home.join(".aterm/shell.d");
    std::fs::create_dir_all(&shell_d).expect("make shell.d");
    let hook = atpkg::hooks::hook_for_shell("zsh").expect("the zsh hook's name");
    std::fs::write(shell_d.join(hook), "# the seed's hook\n").expect("write the hook");
    let surfaced = session_path(root.path());
    let entries: Vec<&str> = surfaced.split(':').collect();
    let bin_pos = entries
        .iter()
        .position(|entry| *entry == bin)
        .unwrap_or_else(|| panic!("bin/ present: {surfaced:?}"));
    let custom_pos = entries
        .iter()
        .position(|entry| *entry == "/custom/after-system")
        .unwrap_or_else(|| panic!("/custom/after-system present: {surfaced:?}"));
    assert!(
        bin_pos < custom_pos,
        "bin/ sits ahead of inherited non-system entries (append_bin_to_path would trail them): {surfaced:?}"
    );
    assert!(
        entries.contains(&"/usr/bin") && entries.contains(&"/bin"),
        "system entries remain on PATH: {surfaced:?}"
    );
    assert_eq!(
        entries.iter().filter(|entry| **entry == bin).count(),
        1,
        "bin/ once: {surfaced:?}"
    );
}
