// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::ExitWatch;

use crate::spawnfd::{Command, Stdio};

/// Exit 7 reads `0x0700`, SIGKILL `0x9`; a second read answers the same, and the watch
/// does not reap.
#[test]
fn the_watch_reads_exit_and_signal_statuses() {
    let mut exits = Command::new("/bin/sh")
        .args(["-c", "read _; exit 7"])
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn");
    let watch = ExitWatch::watch(exits.id()).expect("watch a live child");
    assert_eq!(watch.exit_status(), None, "alive: nothing recorded yet");
    drop(exits.stdin.take());
    let status = exits.wait().expect("wait");
    assert_eq!(status.code(), Some(7));
    let got = wait_for(&watch);
    assert_eq!(got, 0x0700, "exit(7)");
    assert_eq!(watch.exit_status(), Some(0x0700), "answers the same twice");

    let mut killed = Command::new("/bin/sleep")
        .arg("600")
        .spawn()
        .expect("spawn");
    let watch = ExitWatch::watch(killed.id()).expect("watch a live child");
    killed.kill().expect("SIGKILL");
    let _ = killed.wait();
    assert_eq!(wait_for(&watch), 0x9, "SIGKILL");
}

/// A timed read waits for an exit the kernel has not queued yet, and answers nothing for a
/// process that lives on past its bound.
#[test]
fn a_timed_read_waits_for_the_exit_and_no_longer() {
    let mut lives = Command::new("/bin/sleep")
        .arg("600")
        .spawn()
        .expect("spawn");
    let watch = ExitWatch::watch(lives.id()).expect("watch a live child");
    assert_eq!(
        watch.exit_status_within(std::time::Duration::from_millis(20)),
        None,
        "alive: nothing to read"
    );
    let _ = lives.kill();
    let _ = lives.wait();

    let mut exits = Command::new("/bin/sh")
        .args(["-c", "read _; exit 3"])
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn");
    let watch = ExitWatch::watch(exits.id()).expect("watch a live child");
    drop(exits.stdin.take());
    // A hang detector, not a latency budget (AGENTS.md).
    assert_eq!(
        watch.exit_status_within(std::time::Duration::from_mins(1)),
        Some(0x0300),
        "exit(3), read as soon as it was queued"
    );
    let _ = exits.wait();
}

/// A pid that names nothing cannot be watched.
#[test]
fn a_gone_pid_cannot_be_watched() {
    let mut child = Command::new("/usr/bin/true").spawn().expect("spawn");
    let pid = child.id();
    let _ = child.wait();
    assert!(ExitWatch::watch(pid).is_none(), "reaped: ESRCH");
    assert!(ExitWatch::watch(0).is_none());
    assert!(ExitWatch::watch(1).is_none());
}

/// Block on the watch's descriptor, never on a clock: the event is queued inside
/// `proc_exit`, which `wait` has already observed.
fn wait_for(watch: &ExitWatch) -> i32 {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, n: u32, timeout: i32) -> i32;
    }
    let mut pfd = PollFd {
        fd: watch.as_raw_fd(),
        events: 1,
        revents: 0,
    };
    // SAFETY: one live pollfd.
    let rc = unsafe { poll(&mut pfd, 1, 60_000) };
    assert_eq!(rc, 1, "the exit event is queued");
    watch
        .exit_status()
        .expect("a status once the queue is readable")
}
