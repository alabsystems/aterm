// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! NO EXEC WHILE A THREAD OF THIS PROCESS READS THE PROCESS TABLE ([`reading`], [`close`]).
//!
//! macOS answers a `kern.proc` read (`KERN_PROC_ALL`, `KERN_PROC_PID`) only once each process it
//! reads is through an exec in progress (`proc_transwait`), and no signal ends that wait. An
//! `execve` in turn leaves its own process in exec until every other thread of that process has
//! ended (`task_complete_halt`), and no signal ends that wait either. So a process that execs
//! while one of its threads is in such a read stays in exec for as long as the read waits, and
//! two that do it at once, each one's read waiting on the other process, never finish. Neither
//! can be killed, every later read of the table on the machine waits on them, and a shutdown
//! waits on them until the kernel's watchdog resets the Mac.
//!
//! Read from a panic's stackshot (m72, 2026-11-12, macOS 27.2): two build fronts called `execve`
//! 0.5 ms apart, each with its own table read still in the kernel (on [`crate::kproc`]'s reader
//! thread, `kproc-table` in that build, its lane's worker `lane-kproc_table` now); five more did
//! within 40 minutes, each one's read waiting on a front already stuck;
//! 17 hours later 264 threads (115 of them `ps`) were waiting on those seven, and the restart
//! ended in the kernel's `watchdog timeout: no checkins from watchdogd in 185 seconds`, launchd's
//! `reboot` waiting on one of the seven.
//!
//! The gate makes the two exclusive within one process. Each such read is made inside
//! [`reading`]. A process about to replace itself calls [`close`], which lets no new read start
//! and waits a bounded time for the ones in the kernel; it execs only while it holds the
//! [`Closed`] that gives. Where a read is still in the kernel it does not exec: it runs the
//! program as a child, which halts no thread of this process, and ends as that child ends.

use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long an exec waits for this process's reads to leave the kernel ([`close`]). A read takes
/// about 100 µs for the whole table and 7 µs for one process (m22, 2026-10-25); one still in the
/// kernel past this is waiting on another process's exec, for nobody knows how long.
pub const SETTLES_WITHIN: Duration = Duration::from_secs(1);

/// One process's gate between its process-table reads and its exec.
pub struct ExecGate {
    state: Mutex<State>,
    /// A read ended.
    read_ended: Condvar,
}

struct State {
    /// The reads in the kernel now.
    reading: usize,
    /// The execs that closed the gate and have not failed: while any, no read starts.
    closers: usize,
}

/// One read in the kernel, from [`ExecGate::reading`] until this is dropped.
#[must_use = "the read counts as in the kernel only while this lives"]
pub struct Reading(&'static ExecGate);

/// The gate, closed with no read in the kernel ([`ExecGate::close`]): exec while this lives.
/// Dropping it (the exec failed) lets reads start again.
#[must_use = "dropping this opens the gate again"]
pub struct Closed(&'static ExecGate);

/// The reads still in the kernel when [`ExecGate::close`]'s wait ended: how many.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StillReading(pub usize);

impl std::fmt::Display for StillReading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} process-table read(s) of this process still in the kernel",
            self.0
        )
    }
}

impl ExecGate {
    /// A gate with no read in the kernel, open.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(State {
                reading: 0,
                closers: 0,
            }),
            read_ended: Condvar::new(),
        }
    }

    /// Count a read as in the kernel from now until the answer is dropped; `None` while the gate
    /// is closed for an exec, when the read must not be made.
    #[must_use = "the read counts as in the kernel only while the answer lives"]
    pub fn reading(&'static self) -> Option<Reading> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closers > 0 {
            return None;
        }
        state.reading += 1;
        Some(Reading(self))
    }

    /// The reads in the kernel now.
    #[must_use]
    pub fn in_kernel(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reading
    }

    /// Close the gate to new reads and wait up to `within` for the ones in the kernel.
    /// # Errors
    /// Reads were still in the kernel when the wait ended: how many. The gate is open again.
    pub fn close(&'static self, within: Duration) -> Result<Closed, StillReading> {
        let since = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.closers += 1;
        while state.reading > 0 {
            let left = within.saturating_sub(since.elapsed());
            if left.is_zero() {
                state.closers -= 1;
                return Err(StillReading(state.reading));
            }
            state = self
                .read_ended
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        Ok(Closed(self))
    }
}

impl Default for ExecGate {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reading -= 1;
        self.0.read_ended.notify_all();
    }
}

impl Drop for Closed {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closers -= 1;
    }
}

/// This process's gate.
static PROCESS: ExecGate = ExecGate::new();

/// This process's gate ([`reading`], [`close`]), for a reader that keeps a reference to one.
#[must_use]
pub fn process() -> &'static ExecGate {
    &PROCESS
}

/// Count a process-table read of this process as in the kernel until the answer is dropped
/// ([`ExecGate::reading`]); `None` while an exec of this process has the gate closed.
#[must_use = "the read counts as in the kernel only while the answer lives"]
pub fn reading() -> Option<Reading> {
    PROCESS.reading()
}

/// Close this process's gate for an exec ([`ExecGate::close`]).
/// # Errors
/// Reads of this process were still in the kernel after `within`: do not exec.
pub fn close(within: Duration) -> Result<Closed, StillReading> {
    PROCESS.close(within)
}

#[cfg(test)]
#[path = "exec_gate_tests.rs"]
mod tests;
