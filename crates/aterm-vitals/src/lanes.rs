// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's reader lanes a reading found stalled: a read of other processes the kernel holds
//! (the process sweep, the process table) that the host's probe stopped waiting for, so the
//! figures it feeds are unknown at that reading while every other figure is read as usual.

/// The host's process sweep: every process's usage, state and names, and the pid counts.
pub const PROCS: &str = "procs";
/// The kernel's whole process table (`KERN_PROC_ALL`): births past a dozen, and which parents
/// the kernel reaps for.
pub const KPROC_TABLE: &str = "kproc_table";

/// A lane stalled at a reading: its reader is in the kernel past the reading's bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneStall {
    /// The lane ([`PROCS`], [`KPROC_TABLE`]).
    pub lane: &'static str,
    /// The kernel call its reader is in (`proc_pid_rusage`).
    pub call: &'static str,
    /// The process that call reads, where it reads one.
    pub pid: Option<u32>,
    /// That process's name as the last answered sweep read it, never a live read.
    pub name: Option<String>,
    /// When the stalled call was asked, unix ms.
    pub since_unix_ms: i64,
    /// How long it had been in the kernel at the reading, ms.
    pub for_ms: u64,
}

impl LaneStall {
    /// The figures this lane feeds, unknown while it is stalled: the words `sys` prints them by.
    #[must_use]
    pub fn figures(&self) -> &'static [&'static str] {
        figures(self.lane)
    }
}

/// The figures lane `lane` feeds ([`LaneStall::figures`]).
#[must_use]
pub fn figures(lane: &str) -> &'static [&'static str] {
    match lane {
        PROCS => &[
            "aterm", "tab", "procs", "groups", "wait", "writes", "split", "top", "why", "jobs",
            "left",
        ],
        KPROC_TABLE => &["ages", "reaping"],
        _ => &[],
    }
}

/// Whether one of `stalled` feeds `figure`.
#[must_use]
pub fn stalls(stalled: &[LaneStall], figure: &str) -> Option<&'static str> {
    stalled
        .iter()
        .find(|s| s.figures().contains(&figure))
        .map(|s| s.lane)
}
