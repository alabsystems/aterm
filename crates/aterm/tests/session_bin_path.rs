// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE `--session` LANE HANDS ITS SHELL THE MANAGED `bin/`, LAST AND ONCE, FROM ITS FIRST
//! BYTE (`crates/aterm/src/main.rs`, through `atpkg::session_bin_dir` and
//! `atpkg::store::append_bin_to_path`): a script, an agent or a shell that never sources the
//! hook still finds a tool once its shim lands, and a managed tool never shadows a system one.
//! The real binary over a private root ([`crate::launch_isolation`]: `[packages]` off, the
//! reroute off), whose `/bin/sh` sources no atpkg hook (a login `sh`'s `/etc/profile` moves only
//! the system's own directories to the front), so `bin/` on its PATH is the lane's. The gate is
//! the hook a first-launch seed writes under `~/.aterm/shell.d`: with it `bin/` is there;
//! without it (and the lanes off) nothing is surfaced. Negative control: a lane that does not
//! append it leaves the hooked machine's shell without it.

use std::io::Write as _;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::launch_isolation;

/// The PATH `aterm --session` hands `/bin/sh` over `root`, from `PATH=/usr/bin:/bin`.
fn session_path(root: &std::path::Path) -> String {
    #[expect(
        clippy::disallowed_types,
        reason = "launch_isolation::apply arms a std Command, and strips this process's descriptors from it itself"
    )]
    let mut cmd = std::process::Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["--session", launch_isolation::NO_REROUTE])
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn the aterm binary");
    drop(cmd);
    // `$((6*7))` is 42 only once the shell ran the line; the terminal's echo of the typed
    // line keeps the literal, so the marker below is the shell's own output.
    child
        .stdin
        .take()
        .expect("aterm stdin")
        .write_all(b"printf 'PATH_%s=<%s>\\n' $((6*7)) \"$PATH\"\nexit\n")
        .expect("type the line");
    // A hang detector, not a latency budget.
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
    assert_eq!(entries.last(), Some(&bin), "bin/ last: {surfaced:?}");
    assert_eq!(
        entries.iter().filter(|entry| **entry == bin).count(),
        1,
        "bin/ once: {surfaced:?}"
    );
    assert!(
        entries.contains(&"/usr/bin") && entries.contains(&"/bin"),
        "the inherited PATH stays ahead of it: {surfaced:?}"
    );
}
