// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CPU time as the kernel accounts it, for counters: the calling thread's, the whole process's,
//! a meter that adds what a thread has spent to a running total, and a count of the threads that
//! ended with what they spent.
//!
//! The thread read is [`aterm_sysprobe::dstate::thread_ns`], a read of
//! `CLOCK_THREAD_CPUTIME_ID` production code already made: this module names it for the crates
//! that depend on this one and adds no read of that clock of its own.

use std::sync::atomic::{AtomicU64, Ordering};

/// The calling thread's CPU time so far, in nanoseconds; 0 where the clock refuses, and on a
/// platform with no such clock here (any but macOS and Linux).
#[must_use]
pub fn thread_ns() -> u64 {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        aterm_sysprobe::dstate::thread_ns()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

/// This process's CPU time so far, in nanoseconds (`CLOCK_PROCESS_CPUTIME_ID`: every thread's,
/// those that ended included); 0 where the clock refuses, and off Unix.
#[must_use]
pub fn process_ns() -> u64 {
    #[cfg(unix)]
    {
        let mut stamp = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `stamp` is a writable timespec that outlives the call; the clock id is a
        // constant the OS defines.
        if unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &raw mut stamp) } != 0 {
            return 0;
        }
        u64::try_from(stamp.tv_sec)
            .unwrap_or(0)
            .saturating_mul(1_000_000_000)
            .saturating_add(u64::try_from(stamp.tv_nsec).unwrap_or(0))
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// What one thread has spent, added to a total a piece at a time: each [`Self::add_to`] adds the
/// CPU time the thread spent since the last one (since the thread began, for the first), so the
/// pieces of one meter sum to the thread's CPU time at its last add and none is added twice.
///
/// A meter belongs to the thread that made it and starts at that thread's birth: make it on a
/// thread started for the work it counts, where the thread's whole time is that work's.
#[derive(Debug, Default)]
pub struct ThreadCpu {
    /// The thread's CPU time at the last add.
    added_ns: u64,
}

impl ThreadCpu {
    /// A meter that has added nothing yet: its first add is the thread's whole time so far.
    #[must_use]
    pub const fn born() -> Self {
        Self { added_ns: 0 }
    }

    /// Add what the calling thread spent since the last add to `total`: one clock read and one
    /// relaxed add.
    pub fn add_to(&mut self, total: &AtomicU64) {
        self.add_at(thread_ns(), total);
    }

    /// [`Self::add_to`] at the reading `now_ns`. A reading behind the last one (a refused clock
    /// reads 0) adds nothing and leaves the meter where it was.
    fn add_at(&mut self, now_ns: u64, total: &AtomicU64) {
        let Some(spent) = now_ns.checked_sub(self.added_ns) else {
            return;
        };
        self.added_ns = now_ns;
        total.fetch_add(spent, Ordering::Relaxed);
    }
}

/// The threads started for one kind of work that have ended, and the CPU time they spent: totals
/// that only rise, so `cpu_ns / ended` is the kind's CPU per thread.
#[derive(Debug, Default)]
pub struct ThreadsCpu {
    /// Threads that ended.
    pub ended: AtomicU64,
    /// Their CPU time, in nanoseconds.
    pub cpu_ns: AtomicU64,
}

impl ThreadsCpu {
    /// No thread yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ended: AtomicU64::new(0),
            cpu_ns: AtomicU64::new(0),
        }
    }

    /// Run `work` and count the calling thread as ended, with all its CPU time so far: one
    /// clock read and two relaxed adds. Call it on a thread started for `work` alone, as the
    /// last thing that thread does; a `work` that panics is not counted.
    pub fn ending<T>(&self, work: impl FnOnce() -> T) -> T {
        let done = work();
        self.cpu_ns.fetch_add(thread_ns(), Ordering::Relaxed);
        self.ended.fetch_add(1, Ordering::Relaxed);
        done
    }
}

#[cfg(test)]
#[path = "cpu_tests.rs"]
mod tests;
