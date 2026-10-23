// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies that closing the initial session (tab 0) in a headless instance releases its PTY
//! master.

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

const HANG: Duration = Duration::from_secs(60);

/// One poll of the instance's PTY census and, right after it, the OS's count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reading {
    masters: usize,
    sessions: usize,
    ahead: usize,
    open: Option<usize>,
}

impl Reading {
    fn holds(&self, n: usize, witnessed: bool) -> bool {
        self.masters == n && self.sessions == n && self.open == witnessed.then_some(n + self.ahead)
    }
}

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
    let sessions = ctl_ok(&inst, &["sessions"]);
    let boot_sid = sessions
        .lines()
        .find(|l| l.split_whitespace().next() == Some("0"))
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or_else(|| panic!("no local 0 in sessions: {sessions}"))
        .to_string();
    let spawned = ctl_ok(&inst, &["spawn"]);
    assert!(spawned.starts_with("OK s-"), "spawn: {spawned}");

    let witnessed = open_masters(pid).is_some();

    settle("two tabs hold two PTY masters", &inst, pid, 2, witnessed);

    let closed = ctl_ok(&inst, &[&format!("@{boot_sid}"), "close"]);
    assert!(closed.starts_with("OK closed"), "close: {closed}");

    settle("after closing tab 0", &inst, pid, 1, witnessed);
}
