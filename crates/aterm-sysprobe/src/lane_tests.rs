// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use super::{Lane, Read, mark};

/// A gate a stand-in reader waits on: shut, it stands for a kernel call that never returns.
struct Gate {
    open: Mutex<bool>,
    opened: Condvar,
}

impl Gate {
    const fn shut() -> Self {
        Self {
            open: Mutex::new(false),
            opened: Condvar::new(),
        }
    }

    fn pass(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.opened.wait(open).unwrap();
        }
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}

/// A bound that ends a wait on a reader that never returns: what it times is a read that cannot
/// answer, so its length is no latency budget.
const SHORT: Duration = Duration::from_millis(20);

/// A reader held in the kernel is named stalled, with the call it marked and since when, and
/// however many asks follow it holds one call on one worker; an open gate answers.
#[test]
fn a_reader_that_never_returns_is_named_stalled_and_holds_one_call() {
    static GATE: Gate = Gate::shut();
    static ENTERED: AtomicUsize = AtomicUsize::new(0);
    fn held(pid: u32) -> u32 {
        // Marked before it counts itself entered: an ask after the count reads the mark.
        mark("proc_pid_rusage", Some(pid));
        ENTERED.fetch_add(1, Ordering::SeqCst);
        GATE.pass();
        pid
    }
    let lane = Lane::new("procs", held);
    let before = super::unix_ms_now();
    let Read::Stalled(first) = lane.ask_within(4321, SHORT) else {
        panic!("a held read is stalled");
    };
    until_within(MUST_HAPPEN, "the reader entered its call", || {
        (ENTERED.load(Ordering::SeqCst) == 1).then_some(())
    });
    for _ in 0..50 {
        match lane.ask_within(1, MUST_HAPPEN) {
            Read::Stalled(stall) => {
                assert_eq!(stall.lane, "procs");
                assert_eq!(stall.call, "proc_pid_rusage");
                assert_eq!(stall.pid, Some(4321));
                assert_eq!(stall.since_unix_ms, first.since_unix_ms);
                assert!(stall.since_unix_ms >= before);
            }
            other => panic!("an overdue call stalls every later ask at once: {other:?}"),
        }
    }
    assert_eq!(
        ENTERED.load(Ordering::SeqCst),
        1,
        "one call, however many asks"
    );
    assert_eq!(lane.workers(), 1, "one worker, however many asks");
    assert_eq!(lane.stall().map(|s| s.call), Some("proc_pid_rusage"));
    // The negative control: the same reader with its gate open answers.
    GATE.open();
    let open = Lane::new("procs", held);
    match open.ask_within(7, MUST_HAPPEN) {
        Read::Answered { out, ended, .. } => {
            assert_eq!(out, 7);
            assert_eq!(ended, None);
        }
        other => panic!("an open gate answers: {other:?}"),
    }
    assert_eq!(open.stall(), None);
}

/// A call that returns after its bound ends the stall at the next ask, which reads afresh (the
/// late answer stands for an instant long past) and names the episode it ends, once.
#[test]
fn a_late_answer_ends_the_stall_at_the_next_ask() {
    static GATE: Gate = Gate::shut();
    static ENTERED: AtomicUsize = AtomicUsize::new(0);
    fn held(n: u32) -> u32 {
        ENTERED.fetch_add(1, Ordering::SeqCst);
        if n == 1 {
            GATE.pass();
        }
        n * 10
    }
    let lane = Lane::new("kproc", held);
    assert!(matches!(lane.ask_within(1, SHORT), Read::Stalled(_)));
    until_within(MUST_HAPPEN, "the reader entered its call", || {
        (ENTERED.load(Ordering::SeqCst) == 1).then_some(())
    });
    GATE.open();
    until_within(MUST_HAPPEN, "the late call returned", || {
        lane.stall().is_none().then_some(())
    });
    match lane.ask_within(4, MUST_HAPPEN) {
        Read::Answered { out, ended, .. } => {
            assert_eq!(out, 40, "a fresh read, not the late one");
            assert_eq!(
                ended.map(|s| s.call),
                Some("kproc"),
                "unmarked: the lane's name"
            );
        }
        other => panic!("a fresh read: {other:?}"),
    }
    match lane.ask_within(5, MUST_HAPPEN) {
        Read::Answered { out, ended, .. } => {
            assert_eq!(out, 50);
            assert_eq!(ended, None, "an episode ends once");
        }
        other => panic!("the next ask reads afresh: {other:?}"),
    }
}

/// A read that panics answers nothing, and the next ask reads on the same worker.
#[test]
fn a_read_that_panics_lets_the_next_one_read() {
    fn panics(n: u32) -> u32 {
        assert!(n != 0, "a read that panics");
        n
    }
    let lane = Lane::new("kproc", panics);
    assert!(matches!(lane.ask_within(0, MUST_HAPPEN), Read::Off(_)));
    assert!(matches!(
        lane.ask_within(3, MUST_HAPPEN),
        Read::Answered { out: 3, .. }
    ));
    assert_eq!(lane.workers(), 1);
}

/// A caller behind another's call within its bound waits for it, then reads its own: contention
/// is not a stall.
#[test]
fn a_contending_caller_waits_then_reads_its_own() {
    static GATE: Gate = Gate::shut();
    static ENTERED: AtomicUsize = AtomicUsize::new(0);
    fn held(n: u32) -> u32 {
        ENTERED.fetch_add(1, Ordering::SeqCst);
        if n == 1 {
            GATE.pass();
        }
        n
    }
    static LANE: Lane<u32, u32> = Lane::new("kproc", held);
    let first = std::thread::spawn(|| LANE.ask_within(1, MUST_HAPPEN).answer());
    until_within(MUST_HAPPEN, "the first call entered", || {
        (ENTERED.load(Ordering::SeqCst) == 1).then_some(())
    });
    let second = std::thread::spawn(|| LANE.ask_within(2, MUST_HAPPEN).answer());
    until_within(
        MUST_HAPPEN,
        "the second caller waits behind the first",
        || (LANE.parked() >= 1).then_some(()),
    );
    GATE.open();
    assert_eq!(first.join().unwrap(), Some(1));
    assert_eq!(second.join().unwrap(), Some(2));
    assert_eq!(ENTERED.load(Ordering::SeqCst), 2);
}

/// A caller already waiting behind another's call stalls once that call goes overdue, not at its
/// own bound: its bound here is past the hang detector, so only the overdue call's notice ends
/// its wait in time.
#[test]
fn a_waiting_caller_stalls_when_the_call_ahead_goes_overdue() {
    static GATE: Gate = Gate::shut();
    static ENTERED: AtomicUsize = AtomicUsize::new(0);
    fn held(pid: u32) -> u32 {
        mark("proc_pid_rusage", Some(pid));
        ENTERED.fetch_add(1, Ordering::SeqCst);
        GATE.pass();
        pid
    }
    static LANE: Lane<u32, u32> = Lane::new("procs", held);
    /// The first asker's bound: the length of the stall it stands for, long enough that the
    /// second asker is waiting before it passes. Were it not, the second asker would find the
    /// call overdue and stall at once, which the test also takes.
    const AHEAD: Duration = Duration::from_secs(2);
    let first = std::thread::spawn(|| LANE.ask_within(4321, AHEAD));
    until_within(MUST_HAPPEN, "the first call entered", || {
        (ENTERED.load(Ordering::SeqCst) == 1).then_some(())
    });
    let second = std::thread::spawn(|| LANE.ask_within(7, MUST_HAPPEN * 10));
    until_within(MUST_HAPPEN, "the waiting caller stalled", || {
        second.is_finished().then_some(())
    });
    match second.join().unwrap() {
        Read::Stalled(stall) => {
            assert_eq!(stall.call, "proc_pid_rusage");
            assert_eq!(stall.pid, Some(4321));
        }
        other => panic!("the call ahead is overdue: {other:?}"),
    }
    assert!(matches!(first.join().unwrap(), Read::Stalled(_)));
    assert_eq!(
        ENTERED.load(Ordering::SeqCst),
        1,
        "the waiter asked nothing"
    );
    GATE.open();
}

/// A lane's reader that asks another lane is held in that lane's call while it waits: the outer
/// lane's stall names the inner read and its process, not the outer reader's last mark; once the
/// inner ask returns, the outer reader's own mark names it again.
#[test]
fn a_reader_waiting_on_another_lane_is_named_by_that_lanes_call() {
    static GATE: Gate = Gate::shut();
    static INNER_ENTERED: AtomicUsize = AtomicUsize::new(0);
    fn table(n: u32) -> u32 {
        mark("sysctl KERN_PROC_ALL", None);
        INNER_ENTERED.fetch_add(1, Ordering::SeqCst);
        if n == 1 {
            GATE.pass();
        }
        n
    }
    static INNER: Lane<u32, u32> = Lane::new("kproc_table", table);
    fn sweep(n: u32) -> u32 {
        mark("getsid", Some(99));
        INNER.ask_within(n, MUST_HAPPEN).answer().unwrap_or(0)
    }
    let outer = Lane::new("procs", sweep);
    let Read::Stalled(stall) = outer.ask_within(1, SHORT) else {
        panic!("a held inner read holds the outer one");
    };
    until_within(MUST_HAPPEN, "the inner call entered", || {
        (INNER_ENTERED.load(Ordering::SeqCst) == 1).then_some(())
    });
    let stall = outer.stall().unwrap_or(stall);
    assert_eq!(stall.lane, "procs");
    assert_eq!(
        stall.call, "sysctl KERN_PROC_ALL",
        "the inner read, not getsid"
    );
    assert_eq!(stall.pid, None, "not the outer reader's last pid");
    // The negative control: after its inner ask returned, the outer reader held in its own call
    // is named by its own mark.
    GATE.open();
    until_within(MUST_HAPPEN, "the late call returned", || {
        outer.stall().is_none().then_some(())
    });
    let held = Lane::new("procs", |n: u32| {
        let read = INNER.ask_within(3, MUST_HAPPEN).answer().unwrap_or(0);
        mark("proc_pid_rusage", Some(100));
        HELD.pass();
        read + n
    });
    static HELD: Gate = Gate::shut();
    let Read::Stalled(own) = held.ask_within(0, SHORT) else {
        panic!("a held outer read stalls");
    };
    let own = until_within(MUST_HAPPEN, "the outer reader marked its own call", || {
        held.stall()
            .filter(|s| s.call == "proc_pid_rusage")
            .or(Some(own).filter(|s| s.call == "proc_pid_rusage"))
    });
    assert_eq!(own.pid, Some(100));
    HELD.open();
}
