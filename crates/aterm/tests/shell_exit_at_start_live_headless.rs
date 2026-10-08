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
//! * a shell that asks for the device attributes as it starts (fish and
//!   powerlevel10k query at startup), reads aterm's answer, then `exit 1`:
//!   `[fish ended at start: exit 1]` — aterm's reply is not anyone's input;
//! * NEGATIVE CONTROLS: `exit 0` at start, and `-e false`, still end the
//!   instance by themselves, as they did before.
//!
//! Every wait is for its causal event; the ceiling is for a broken build.

#![cfg(unix)]

use crate::{child_output, headless_boot, launch_isolation};

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The ceiling on any one wait: a debug binary booting on a loaded machine.
const CEILING: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(50);

/// A stub shell called `name` under `root/bin` that runs `body`, already run
/// once.
///
/// THE FIRST EXEC IS PAID HERE (2026-09-30), the repo's rule since bc2918c70
/// (`aterm-verify`'s `tests/common` `run_once`, `aterm-cli`'s `manual.rs`):
/// the first exec of a file this process wrote waits on macOS's assessment of
/// it, one file at a time machine-wide, and later execs do not — 111 ms
/// against 4.6 ms at load 11.7 here, many seconds with the assessor busy. The
/// app keeps the pane only when the shell's PTY closed within 2 s of its
/// spawn (`aterm-gui`'s `app_tabs.rs` `SHELL_START_WINDOW`), so a stub whose
/// FIRST exec is that spawn spends the window in the assessor's queue: a
/// whole-tree run of 2026-09-30 failed both kept-pane cases at 2.6 s and
/// 2.9 s ("the instance ended with its shell"), and main's own two-at-a-time
/// run of 2026-09-29 passed them at 2.05 s. An installed shell is not a fresh
/// file, so this is also the faithful case. The run's status is not read:
/// `exit 3` and `kill -KILL $$` end it as they will end the pane's.
fn stub_shell(root: &Path, name: &str, body: &str) -> PathBuf {
    let dir = root.join("bin");
    std::fs::create_dir_all(&dir).expect("the stub's directory");
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write the stub shell");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make the stub shell executable");
    let _ = Command::new(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn aterm ctl");
    // Read until the client exits (`support/child_output.rs`: a sibling's
    // spawn can hold its pipes open).
    child_output::wait_and_read(child, Instant::now() + CEILING)
        .unwrap_or_else(|| panic!("aterm ctl {args:?} did not exit in time"))
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

/// A shell that queried the terminal and got its answer before its rc
/// failed: the reply aterm wrote does not count as someone using the shell.
/// The stub reads the first byte of the reply before it exits, so the reply
/// was written before the exit is judged.
#[test]
fn a_shell_that_queried_the_terminal_then_failed_keeps_its_pane() {
    kept_with_line(
        "atsxq",
        "fish",
        "printf '\\033[c'\nstty -icanon -echo min 1 time 0\n\
         dd bs=1 count=1 >/dev/null 2>&1\nexit 1",
        "[fish ended at start: exit 1]",
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
