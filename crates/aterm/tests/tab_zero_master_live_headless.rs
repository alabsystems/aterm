// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance: A CLOSED TAB 0 RELEASES ITS PTY MASTER
//! (2026-09-29).
//!
//! The owner's window (v0.98.0) held a PTY master, fd 6 on `ttys000`, with no
//! process left on its terminal: the master of the session it had started
//! with, closed hours before (`exits`: `local=0 reason=ui-close`). Its later
//! tabs closed cleanly. The startup function kept named clones of session 0's
//! engine and byte sink as locals, and every local there lives until the event
//! loop returns; the sink owns the master and closes it only with its last
//! clone, so tab 0's master stayed open for the window's life (and its shell,
//! exiting with output unread, could not finish its exit).
//!
//! Driven through the real binary: boot, open a second tab, close the BOOT
//! session (local 0), and read both the instance's own PTY census (`metrics`:
//! `pty_masters=` vs `pty_sessions=`) and, where the platform can say it, the
//! PTY masters the process actually has open.
//!
//! The process also holds masters no session owns yet: the spawn kits its
//! prep thread opens ahead for the next spawns (`pty_kits=`, since
//! 2026-10-17), and a warm spare's (`spare_ptys=`, none here). The census
//! names them, so the OS witness is that the open `/dev/ptmx` descriptors
//! number `pty_masters + pty_kits + spare_ptys`, all read in one poll: the
//! prep thread fills on its own clock, so a poll that straddles a fill reads
//! apart and the next one reads again.
//!
//! NEGATIVE CONTROL: before the close both counts read two — the census and
//! the descriptor count see a live master — so a one afterwards is the close,
//! not a count that never counted. Measured without the fix: `pty_masters=2
//! pty_sessions=1`, and two `/dev/ptmx` descriptors, for as long as the
//! instance ran. A master tab 0 left open outside its sink reads one more
//! descriptor than the census accounts for, and fails the witness the same way.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, every automatic
//! lane off, `--no-reroute`, `SHELL=/bin/sh`, and a lifeline
//! (`support/launch_isolation.rs`). A headless instance opens no window.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::headless_boot;

use headless_boot::{Instance, ctl_ok, field};

/// How long something that MUST happen may take before it is read as a hang
/// (a hang detector, never a latency budget). A closing session's master
/// closes once its reader has wound down, normally well under a second.
const HANG: Duration = Duration::from_secs(60);

/// One poll of the instance's PTY census and, right after it, the OS's count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reading {
    /// `pty_masters=`: the masters the sessions' sinks hold open.
    masters: usize,
    /// `pty_sessions=`: the live sessions that own one.
    sessions: usize,
    /// `pty_kits=` + `spare_ptys=`: masters opened ahead, owned by no session.
    ahead: usize,
    /// The `/dev/ptmx` descriptors the process has open ([`open_masters`]).
    open: Option<usize>,
}

impl Reading {
    /// `n` sessions, each with its master, and — where the platform can say it
    /// (`witnessed`) — every open master accounted for by the census.
    fn holds(&self, n: usize, witnessed: bool) -> bool {
        self.masters == n && self.sessions == n && self.open == witnessed.then_some(n + self.ahead)
    }
}

/// Read the instance's `metrics` summary, then the OS's count for `pid`.
fn reading(inst: &Instance, pid: u32) -> Reading {
    let reply = ctl_ok(inst, &["metrics"]);
    let read = |key: &str| -> usize {
        field(&reply, key)
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("metrics names no {key}=: {reply}"))
    };
    Reading {
        masters: read("pty_masters"),
        sessions: read("pty_sessions"),
        ahead: read("pty_kits") + read("spare_ptys"),
        open: open_masters(pid),
    }
}

/// The PTY masters `pid` has open, counted from the OS (`/dev/ptmx`
/// descriptors), or `None` where this platform cannot say it here.
fn open_masters(pid: u32) -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        let dir = std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?;
        Some(
            dir.filter_map(Result::ok)
                .filter_map(|e| std::fs::read_link(e.path()).ok())
                .filter(|target| target == std::path::Path::new("/dev/ptmx"))
                .count(),
        )
    }
    #[cfg(target_os = "macos")]
    {
        // `lsof` is part of the base system; a runner without it only loses
        // this second witness, never the census.
        let out = Command::new("/usr/sbin/lsof")
            .args(["-n", "-P", "-w", "-p", &pid.to_string()])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| l.ends_with("/dev/ptmx"))
                .count(),
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// Poll [`reading`] until it [`Reading::holds`] for `n`, or panic with `what`
/// after [`HANG`].
fn settle(what: &str, inst: &Instance, pid: u32, n: usize, witnessed: bool) {
    let deadline = Instant::now() + HANG;
    loop {
        let got = reading(inst, pid);
        if got.holds(n, witnessed) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: still {got:?}, wanted {n} sessions holding {n} masters \
             (witnessed: {witnessed}) after {HANG:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn closing_the_boot_session_closes_its_pty_master() {
    let inst = headless_boot::boot("atz0", &["--lines", "24", "--columns", "80"]);
    let pid = inst.child.id();
    // The boot session is local 0.
    let sessions = ctl_ok(&inst, &["sessions"]);
    let boot_sid = sessions
        .lines()
        .find(|l| l.split_whitespace().next() == Some("0"))
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or_else(|| panic!("no local 0 in sessions: {sessions}"))
        .to_string();
    let spawned = ctl_ok(&inst, &["spawn"]);
    assert!(spawned.starts_with("OK s-"), "spawn: {spawned}");

    // Whether this platform can count the process's descriptors at all; where
    // it can, every reading below must.
    let witnessed = open_masters(pid).is_some();

    // NEGATIVE CONTROL: both masters are live and both witnesses see them.
    settle("two tabs hold two PTY masters", &inst, pid, 2, witnessed);

    let closed = ctl_ok(&inst, &[&format!("@{boot_sid}"), "close"]);
    assert!(closed.starts_with("OK closed"), "close: {closed}");

    // The session is gone at once; its master closes once its reader has
    // wound down — and must, however long the instance runs.
    settle("after closing tab 0", &inst, pid, 1, witnessed);
}
