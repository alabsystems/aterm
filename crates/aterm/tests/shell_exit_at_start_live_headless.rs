// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LIVE: an interactive shell that fails at start keeps its pane (audit #7
//! finding 49). An rc file that runs `exit 1`, or a shell whose dylib moved and
//! aborts, used to close its only pane — and with it the window and aterm — with
//! nothing saying why. Now the pane stays, with one line naming the shell and how
//! it ended, and the instance keeps running.
//!
//! The real binary (`CARGO_BIN_EXE_aterm`) under a scratch world
//! (`support/launch_isolation.rs`), its shell a stub script given by `--shell`:
//!
//! * `exit 3` at start: the instance stays up, `text` shows
//!   `[zsh ended at start: exit 3]`, and `ls` still lists the session;
//! * a `SIGKILL` at start (no crash report): `[fish ended at start: signal 9]`;
//! * NEGATIVE CONTROLS: `exit 0` at start, and `-e false`, still end the
//!   instance by themselves, as they did before.
//!
//! Every wait is for its causal event; the ceiling is for a broken build.

#![cfg(unix)]

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

#[path = "support/headless_boot.rs"]
mod headless_boot;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The ceiling on any one wait: a debug binary booting on a loaded machine.
const CEILING: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(50);

/// A stub shell called `name` under `root/bin` that runs `body`.
fn stub_shell(root: &Path, name: &str, body: &str) -> PathBuf {
    let dir = root.join("bin");
    std::fs::create_dir_all(&dir).expect("the stub's directory");
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write the stub shell");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make the stub shell executable");
    path
}

/// One `aterm ctl` call against `inst`, under the same scratch world.
fn ctl(inst: &headless_boot::Instance, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, &inst.tmp);
    cmd.arg("ctl")
        .arg("--sock")
        .arg(&inst.sock)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run aterm ctl")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Boot with `--shell <stub running body>` and wait for the pane to show
/// `line`; then the instance must still be running, and `ls` must still list
/// its one session.
fn kept_with_line(prefix: &str, name: &str, body: &str, line: &str) {
    let Some(mut inst) = headless_boot::boot_with(
        prefix,
        |root, cmd| {
            cmd.arg("--shell").arg(stub_shell(root, name, body));
        },
        |_| true,
    ) else {
        return;
    };
    let deadline = Instant::now() + CEILING;
    let text = loop {
        let text = stdout(&ctl(&inst, &["text"]));
        if text.contains(line) {
            break text;
        }
        assert!(
            inst.child.try_wait().expect("poll the instance").is_none(),
            "the instance ended with its shell; log tail:\n{}",
            headless_boot::log_tail(&inst.log)
        );
        assert!(
            Instant::now() < deadline,
            "the pane never said {line:?}; it shows:\n{text}"
        );
        std::thread::sleep(POLL);
    };
    assert_eq!(
        text.matches("ended at start").count(),
        1,
        "one line: {text:?}"
    );
    assert!(
        inst.child.try_wait().expect("poll the instance").is_none(),
        "the instance is still running"
    );
    let ls = stdout(&ctl(&inst, &["ls"]));
    assert!(
        ls.lines().any(|l| l.contains("s-")),
        "ls still lists the kept session:\n{ls}"
    );
}

#[test]
fn a_shell_that_exits_3_at_start_keeps_its_pane_and_the_instance() {
    kept_with_line("atsx3", "zsh", "exit 3", "[zsh ended at start: exit 3]");
}

#[test]
fn a_shell_killed_at_start_keeps_its_pane_with_the_signal() {
    kept_with_line(
        "atsx9",
        "fish",
        "kill -KILL $$",
        "[fish ended at start: signal 9]",
    );
}

/// Launch a headless instance over a fresh scratch world with `extra` args and
/// require it to end by itself, cleanly. No scratch base is a SKIP.
fn ends_by_itself(prefix: &str, extra: impl FnOnce(&Path, &mut Command)) {
    let Some(root) = headless_boot::scratch_root(prefix) else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return;
    };
    let log = root.join("gui.log");
    let out = std::fs::File::create(&log).expect("the instance log");
    let err = out.try_clone().expect("the instance log, twice");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, &root);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .args(launch_isolation::control_sock(&root))
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    let _lifeline = launch_isolation::lifeline(&mut cmd, &root);
    extra(&root, &mut cmd);
    let mut child = cmd.spawn().expect("launch aterm --headless");
    let deadline = Instant::now() + CEILING;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll the instance") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let tail = headless_boot::log_tail(&log);
            let _ = std::fs::remove_dir_all(&root);
            panic!("the instance outlived its shell; log tail:\n{tail}");
        }
        std::thread::sleep(POLL);
    };
    let _ = std::fs::remove_dir_all(&root);
    assert!(status.success(), "a clean quit: {status}");
}

/// NEGATIVE CONTROL: a shell that exits 0 at start closes its pane, and the
/// last pane ends the instance, as before.
#[test]
fn a_shell_that_exits_0_at_start_ends_the_instance_as_before() {
    ends_by_itself("atsx0", |root, cmd| {
        cmd.arg("--shell").arg(stub_shell(root, "zsh", "exit 0"));
    });
}

/// NEGATIVE CONTROL: `-e false` keeps its documented close ("the window closes
/// when it exits"), failure or not.
#[test]
fn an_exec_command_that_fails_at_once_ends_the_instance_as_before() {
    ends_by_itself("atsxe", |_, cmd| {
        cmd.args(["-e", "false"]);
    });
}
