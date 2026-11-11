// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
