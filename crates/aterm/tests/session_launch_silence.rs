// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies that `aterm --session` launches print nothing about package updates or reroute failures to stderr or the terminal.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::{Read as _, Write as _};
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

/// One-minute hang detector for session exit and pipe drains.
const DEADLINE: Duration = Duration::from_secs(60);

/// `[machine]` table keeping host settings untouched.
const MACHINE: &str = "[machine]\nuniversal_control = \"leave\"\n";

/// `[update]` table disabling the app updater.
const UPDATE: &str = "[update]\nenabled = false\nauto_apply = false\n";

/// Creates an isolated root with `packages` as the `[packages]` table.
fn fixture(label: &str, packages: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "aterm-session-launch-silence-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    launch_isolation::prepare(&root).expect("prepare private session state");
    std::fs::write(
        root.join("cfg/aterm/aterm.toml"),
        format!("agents_auto_prime = false\n{UPDATE}{packages}{MACHINE}"),
    )
    .expect("write the fixture config");
    root
}

/// Default store prefix under `root/home`.
fn prefix(root: &Path) -> PathBuf {
    atpkg::store::default_prefix(&root.join("home"))
}

/// Creates the default store prefix with `0700` permissions on each component.
fn make_prefix(root: &Path) -> PathBuf {
    let home = root.join("home");
    let prefix = prefix(root);
    let mut dir = home.clone();
    for part in prefix
        .strip_prefix(&home)
        .expect("the prefix is under HOME")
    {
        dir.push(part);
        std::fs::create_dir_all(&dir).expect("make the prefix");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .expect("0700 like atpkg's own");
    }
    prefix
}

/// Writes a fresh `session-pass.stamp` in the scratch prefix so launching with packages enabled spawns no network pass.
fn hold_the_pass(root: &Path) {
    let layout = atpkg::store::Layout {
        prefix: make_prefix(root),
    };
    let now = aterm_time::unix_secs().expect("clock");
    std::fs::write(layout.session_pass_stamp(), format!("{now}\n")).expect("claim the slot");
}

/// Launches `aterm --session` on a real PTY, sends `exit 0`, and returns `(stderr, screen)`.
fn launch(root: &Path, reroute: bool) -> (String, String) {
    // Serialize spawns until the child's slave fds are dropped so a sibling launch cannot inherit open pipes.
    static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let spawning = SPAWN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: both fds come from the successful open above and are owned here.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };

    let mut cmd = Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root);
    cmd.arg("--session");
    if !reroute {
        cmd.arg(launch_isolation::NO_REROUTE);
    }
    cmd.stdin(Stdio::from(slave.try_clone().expect("dup pty slave")))
        .stdout(Stdio::from(slave.try_clone().expect("dup pty slave")))
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn the aterm binary");
    drop(cmd);
    drop(slave);
    drop(spawning);

    let mut stderr = child.stderr.take().expect("piped stderr");
    let stderr_drain = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let mut pty = std::fs::File::from(master);
    let mut input = pty.try_clone().expect("dup pty master");
    let pty_drain = std::thread::spawn(move || {
        let (mut out, mut chunk) = (Vec::new(), [0u8; 4096]);
        loop {
            match pty.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
            }
        }
        out
    });
    input.write_all(b"exit 0\n").expect("type `exit 0`");

    let start = Instant::now();
    let status_code = loop {
        match child.try_wait().expect("wait on aterm") {
            Some(status) => break status,
            None if start.elapsed() >= DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`aterm --session` did not exit within {DEADLINE:?}");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    drop(input);
    let join = |drain: std::thread::JoinHandle<Vec<u8>>, what: &str| -> String {
        let grace = Instant::now() + DEADLINE;
        while !drain.is_finished() {
            assert!(
                Instant::now() < grace,
                "the {what} stayed open past the session's exit"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        String::from_utf8_lossy(&drain.join().expect("drain")).into_owned()
    };
    let stderr = join(stderr_drain, "stderr pipe");
    let screen = join(pty_drain, "terminal");
    assert!(
        status_code.success(),
        "the session exits with the shell's status: {status_code:?}; screen={screen:?}; \
         stderr={stderr:?}"
    );
    (stderr, screen)
}

/// Asserts that `stderr` is empty and `screen` contains no package-update or reroute diagnostics.
fn assert_silent(what: &str, stderr: &str, screen: &str) {
    assert_eq!(
        stderr, "",
        "{what}: the launch printed on stderr; screen={screen:?}"
    );
    for word in ["atpkg:", "no update check", "reroute", "aterm.toml"] {
        assert!(
            !screen.contains(word),
            "{what}: {word:?} reached the terminal: {screen:?}"
        );
    }
}

#[test]
fn an_interactive_session_launch_over_a_never_checked_store_writes_nothing_to_stderr() {
    let root = fixture("never-checked", "[packages]\nenabled = true\n");
    hold_the_pass(&root);
    let status = prefix(&root).join("status.toml");
    assert!(
        !status.exists(),
        "precondition: no pass has written a status record in this HOME"
    );
    let (stderr, screen) = launch(&root, false);
    assert_silent("a never-checked store", &stderr, &screen);
    let _ = std::fs::remove_dir_all(&root);
}

/// A malformed `[packages]` table is logged rather than printed into the new shell.
#[test]
fn a_malformed_packages_table_is_not_reported_into_the_new_shell() {
    let root = fixture("malformed", "[packages]\nenabled = \"yes\"\n");
    hold_the_pass(&root);
    let (stderr, screen) = launch(&root, false);
    assert_silent("a malformed [packages] table", &stderr, &screen);
    let _ = std::fs::remove_dir_all(&root);
}

/// An engaged reroute over an uncreatable `reroute/` directory and an unwritable prefix logs without printing to the shell.
#[test]
fn an_engaged_reroute_that_cannot_be_laid_and_a_misconfigured_table_say_nothing() {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let root_user = unsafe { libc::geteuid() } == 0;
    let foreign_prefix = if root_user {
        String::new()
    } else {
        "prefix = \"/usr/aterm-session-launch-silence\"\n".to_owned()
    };
    let root = fixture(
        "reroute",
        &format!("[packages]\nenabled = true\n{foreign_prefix}"),
    );
    let prefix = make_prefix(&root);
    hold_the_pass(&root);
    std::fs::write(prefix.join(atpkg::reroute::DIR_NAME), "not a directory\n")
        .expect("block reroute/");
    let (stderr, screen) = launch(&root, true);
    assert_silent("an unlaid reroute", &stderr, &screen);
    assert!(
        prefix.join(atpkg::reroute::DIR_NAME).is_file(),
        "precondition held: nothing could lay the reroute"
    );
    let _ = std::fs::remove_dir_all(&root);
    aterm_uds::host::could_not_run_as_root("a root-owned prefix this user cannot write");
}
