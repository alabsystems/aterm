// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless tests for panes whose interactive shell exits at start.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use crate::{child_output, headless_boot, launch_isolation};

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const CEILING: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(50);

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

fn ctl(inst: &headless_boot::Instance, args: &[&str]) -> Output {
    headless_boot::until_served(|| {
        let mut cmd = Command::new(launch_isolation::aterm_executable());
        launch_isolation::apply(&mut cmd, &inst.tmp);
        cmd.arg("ctl")
            .arg("--sock")
            .arg(&inst.sock)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = cmd.spawn().expect("spawn aterm ctl");
        child_output::wait_and_read(child, Instant::now() + CEILING)
            .unwrap_or_else(|| panic!("aterm ctl {args:?} did not exit in time"))
    })
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn kept_with_line(prefix: &str, name: &str, body: &str, line: &str) {
    let mut inst = headless_boot::boot_with(
        prefix,
        |root, cmd| {
            cmd.arg("--shell").arg(stub_shell(root, name, body));
        },
        |_| true,
    );
    let began = Instant::now();
    let deadline = began + CEILING;
    let text = loop {
        let text = stdout(&ctl(&inst, &["text"]));
        if text.contains(line) {
            break text;
        }
        let ended = inst.child.try_wait().expect("poll the instance");
        assert!(
            ended.is_none(),
            "the instance ended with its shell ({ended:?}, {:?} after boot); log tail:\n{}",
            began.elapsed(),
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

fn ends_by_itself(prefix: &str, extra: impl FnOnce(&Path, &mut Command)) {
    let root = headless_boot::scratch_root(prefix);
    let log = root.join("gui.log");
    let out = std::fs::File::create(&log).expect("the instance log");
    let err = out.try_clone().expect("the instance log, twice");
    let mut cmd = Command::new(launch_isolation::aterm_executable());
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

#[test]
fn a_shell_that_exits_0_at_start_ends_the_instance_as_before() {
    ends_by_itself("atsx0", |root, cmd| {
        cmd.arg("--shell").arg(stub_shell(root, "zsh", "exit 0"));
    });
}

#[test]
fn an_exec_command_that_fails_at_once_ends_the_instance_as_before() {
    ends_by_itself("atsxe", |_, cmd| {
        cmd.args(["-e", "false"]);
    });
}
