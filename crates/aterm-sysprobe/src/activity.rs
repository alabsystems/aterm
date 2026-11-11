// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! IDLE: what a process tree did between two reads of its members, and whether that was nothing.
//!
//! A member is one process incarnation, known by its pid and its start ([`MemberUsage::start`]):
//! a pid the kernel hands out anew reads as another member, whose counters count whole, never as
//! the old one's run backwards. Between two reads, a member both found counts by the growth of
//! its counters; one only the later read found (it started since) counts whole; one gone since
//! takes what it used after the earlier read with it. A tree whose members used no CPU time and
//! moved no bytes between two reads was idle between them ([`Used::idle`]).
//!
//! How long a tree may stay idle is each caller's: verify's stage meter calls a child's tree hung
//! after 10 s of it, and aterm-lab waits for a starting child while it is not. Two other
//! quantities are not this one: whether a lock or ledger holder makes progress
//! (`aterm_toolchain::stall`: runnable a quarter of the time or a MiB written a minute, which a
//! tree's upkeep never reaches, so a holder that only keeps its books is stuck though not idle),
//! and semantic progress, what `sys jobs` says of a command, read in aterm-vitals.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::{MemberUsage, ProcessReading};

/// One read's members, by pid and start: what the next [`between`] reads against.
pub type Members = BTreeMap<(u32, u64), MemberUsage>;

/// What a tree's members used between two reads of them ([`between`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Used {
    /// CPU time, every thread's.
    pub cpu: Duration,
    /// Time runnable, on a core or waiting for one, every thread's.
    pub runnable: Duration,
    /// Bytes read and written ([`MemberUsage::io_bytes`]).
    pub io: u64,
}

impl Used {
    /// Whether the tree was idle: no CPU time and no bytes moved. Time runnable alone is no
    /// activity: a thread that waited for a core and never got one did nothing.
    #[must_use]
    pub fn idle(&self) -> bool {
        self.cpu.is_zero() && self.io == 0
    }
}

/// What `now`'s members used since `before`'s read of them ([`Used`]), and `now`'s members keyed
/// for the next read: each member both found, by pid and start, by the growth of its counters;
/// one only `now` found, by all of them (it started since). A member gone since takes what it
/// used after `before` with it.
#[must_use]
pub fn between(before: &Members, now: impl IntoIterator<Item = MemberUsage>) -> (Used, Members) {
    let mut used = Used::default();
    let mut seen = Members::new();
    for m in now {
        let was = before.get(&(m.pid, m.start));
        let grew = |pick: fn(&MemberUsage) -> u64| pick(&m).saturating_sub(was.map_or(0, pick));
        used.cpu += Duration::from_nanos(grew(|u| u.cpu_ns));
        used.runnable += Duration::from_nanos(grew(|u| u.runnable_ns));
        used.io = used.io.saturating_add(grew(|u| u.io_bytes));
        seen.insert((m.pid, m.start), m);
    }
    (used, seen)
}

/// Process `p` of a sweep ([`crate::processes`]) as a member ([`MemberUsage`]): its pid and start
/// (0 where unread), its own CPU time (not its reaped children's), its runnable time, the bytes it
/// read from and wrote to disk, and its footprints; 0 for each figure the sweep did not read. A
/// group's read ([`crate::group_usage`]) reads each member through this too.
#[must_use]
pub fn member(p: &ProcessReading) -> MemberUsage {
    MemberUsage {
        pid: p.pid,
        start: p.start.unwrap_or(0),
        cpu_ns: p.cpu_ns.unwrap_or(0),
        runnable_ns: p.runnable_ns.unwrap_or(0),
        io_bytes: p
            .disk_read
            .unwrap_or(0)
            .saturating_add(p.disk_written.unwrap_or(0)),
        footprint: p.footprint.unwrap_or(0),
        peak_footprint: p.peak_footprint.unwrap_or(0),
    }
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod tests;
