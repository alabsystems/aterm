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
mod tests {
    use super::*;

    fn m(pid: u32, start: u64, cpu: u64, runnable: u64, io: u64) -> MemberUsage {
        MemberUsage {
            pid,
            start,
            cpu_ns: cpu,
            runnable_ns: runnable,
            io_bytes: io,
            ..MemberUsage::default()
        }
    }

    fn used(cpu: u64, runnable: u64, io: u64) -> Used {
        Used {
            cpu: Duration::from_nanos(cpu),
            runnable: Duration::from_nanos(runnable),
            io,
        }
    }

    /// A member new to a read counts whole, one both reads found by its growth, a pid taken anew
    /// (another start) whole, and one gone since takes its use with it. NEGATIVE CONTROL: keyed
    /// by pid alone, the reused pid would read its counters against the old member's and count 0
    /// CPU time, not 4.
    #[test]
    fn between_counts_new_members_whole_and_growth_of_the_rest() {
        let (first, seen) = between(&Members::new(), [m(7, 1, 10, 30, 5)]);
        assert_eq!(first, used(10, 30, 5), "a new member counts whole");

        let (next, seen) = between(&seen, [m(7, 1, 25, 70, 5), m(9, 3, 2, 2, 0)]);
        assert_eq!(
            next,
            used(15 + 2, 40 + 2, 0),
            "growth, and a member started since"
        );

        let (gone, seen) = between(&seen, [m(9, 3, 2, 2, 0)]);
        assert_eq!(
            gone,
            used(0, 0, 0),
            "pid 7 ended: what it used goes with it"
        );
        assert!(gone.idle(), "{gone:?}");

        let (reused, _) = between(&seen, [m(9, 4, 4, 6, 0)]);
        assert_eq!(reused, used(4, 6, 0), "pid 9 taken anew counts whole");
        let by_pid: BTreeMap<u32, MemberUsage> = seen.values().map(|u| (u.pid, *u)).collect();
        let wrong = m(9, 4, 4, 6, 0)
            .cpu_ns
            .saturating_sub(by_pid.get(&9).map_or(0, |u| u.cpu_ns));
        assert_eq!(wrong, 2, "the control keyed by pid alone undercounts");
    }

    /// A tree is idle exactly when it used no CPU time and moved no bytes; time runnable alone is
    /// not activity. NEGATIVE CONTROL: one nanosecond of CPU time, or one byte, is not idle.
    #[test]
    fn a_tree_is_idle_only_without_cpu_time_or_io() {
        assert!(used(0, 0, 0).idle());
        assert!(used(0, 500, 0).idle(), "waiting for a core is no activity");
        assert!(!used(1, 0, 0).idle());
        assert!(!used(0, 0, 1).idle());
    }

    /// A sweep's process reads as a member with its own CPU time and its disk reads and writes,
    /// and 0 for what the sweep did not read. NEGATIVE CONTROL: its reaped children's 20 ns are
    /// not its own, so a tree whose only use since the last read is a reap of a child read before
    /// is idle, not 20 ns busy twice over.
    #[test]
    fn a_sweep_reading_is_a_member() {
        let p = ProcessReading {
            pid: 42,
            start: Some(7),
            cpu_ns: Some(100),
            child_cpu_ns: Some(20),
            runnable_ns: Some(150),
            disk_read: Some(3),
            disk_written: Some(4),
            footprint: Some(1 << 20),
            ..ProcessReading::default()
        };
        assert_eq!(
            member(&p),
            MemberUsage {
                pid: 42,
                start: 7,
                cpu_ns: 100,
                runnable_ns: 150,
                io_bytes: 7,
                footprint: 1 << 20,
                peak_footprint: 0,
            }
        );
        let unread = ProcessReading {
            pid: 43,
            ..ProcessReading::default()
        };
        assert_eq!(
            member(&unread),
            MemberUsage {
                pid: 43,
                ..MemberUsage::default()
            }
        );
    }
}
