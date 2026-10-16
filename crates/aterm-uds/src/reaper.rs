// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The process-wide reaper of children nobody else waits for. A child that exits unwaited
//! stays a zombie for the life of this process (in a window that runs for days, one per
//! click), so a dropped [`crate::spawnfd::Child`] or [`crate::spawnfd::LaneChild`] still
//! running is handed here by its `Drop`, and so is a PTY shell past its reap's deadline
//! (aterm-pty's `reap_shell`).
//!
//! One thread, started on the first hand-off and ended once nothing is left, looks after
//! [`FIRST_LOOK`] and doubles its pause up to [`SLOWEST_LOOK`] while nothing settles. Nothing
//! is signalled.

use std::sync::Mutex;
use std::time::Duration;

/// Whether a handed child is settled: reaped now, or no longer this process's
/// to reap. Never blocks, never signals.
type Settled = Box<dyn FnMut() -> bool + Send>;

/// The children still waited for, and whether the thread is running. A `const`
/// initializer: nothing lazy.
struct Ledger {
    waiting: Vec<Settled>,
    running: bool,
}

static LEDGER: Mutex<Ledger> = Mutex::new(Ledger {
    waiting: Vec::new(),
    running: false,
});

/// How soon the reaper first looks.
const FIRST_LOOK: Duration = Duration::from_millis(100);
/// The reaper's slowest look.
const SLOWEST_LOOK: Duration = Duration::from_secs(2);

unsafe extern "C" {
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}
const WNOHANG: i32 = 1;

/// Wait, off this thread, for a child whose settling `settled` answers (called
/// on the reaper's thread until it says `true`). A thread that cannot start
/// leaves the child queued; the next hand-off tries again.
pub fn hand_off(settled: impl FnMut() -> bool + Send + 'static) {
    let mut ledger = LEDGER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ledger.waiting.push(Box::new(settled));
    if !ledger.running {
        // Redundant to the compiler (`mod reaper` is unix-only); the wasm census scans
        // lexically and reads the marker here.
        #[cfg(not(target_arch = "wasm32"))]
        let started = std::thread::Builder::new()
            .name("aterm-reaper".into())
            .spawn(run)
            .is_ok();
        ledger.running = started;
    }
}

/// Reap `pid`, this process's own unreaped child, once it exits. For a handle
/// that owned the pid and is going away: nothing else may wait for it after.
pub(crate) fn hand_off_pid(pid: i32) {
    hand_off(move || reaped_or_gone(pid));
}

/// One `waitpid(pid, WNOHANG)`: `true` once the child is collected, or when it
/// is not this process's child to collect (`ECHILD`); `false` while it runs or
/// when the call was interrupted.
fn reaped_or_gone(pid: i32) -> bool {
    let mut status = 0;
    // SAFETY: `status` is a live local `waitpid` writes; the pid names a child
    // whose handle gave it up to the reaper, so nothing else waits for it.
    let got = unsafe { waitpid(pid, &mut status, WNOHANG) };
    got == pid
        || (got == -1 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted)
}

/// The reaper thread: asks each queued child, backing off while nothing settles, and ends once
/// none is left.
fn run() {
    let mut pause = FIRST_LOOK;
    loop {
        std::thread::sleep(pause);
        let mut ledger = LEDGER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = ledger.waiting.len();
        ledger.waiting.retain_mut(|settled| !settled());
        if ledger.waiting.is_empty() {
            ledger.running = false;
            return;
        }
        pause = if ledger.waiting.len() < before {
            FIRST_LOOK
        } else {
            (pause * 2).min(SLOWEST_LOOK)
        };
    }
}
