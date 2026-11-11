// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::waits::Stuck;

fn p(pid: u32, ppid: u32, cpu_ns: u64, written: u64) -> Proc {
    Proc {
        pid,
        ppid,
        cpu_ns,
        written,
        ..Proc::default()
    }
}

/// `secs` seconds of CPU time, nanoseconds.
fn secs(secs: f64) -> u64 {
    Duration::from_secs_f64(secs)
        .as_nanos()
        .try_into()
        .expect("fits")
}

/// A tree's work is its root's and every descendant's, and nothing of a process outside it;
/// each process is runnable for the more of its runnable and its CPU time, its CPU time where
/// its runnable time is unread; a root no sweep holds has none.
#[test]
fn a_trees_work_is_its_own_processes() {
    let procs = [
        p(1, 0, 1_000, 0),
        p(10, 1, 5, 7),
        Proc {
            runnable_ns: 30,
            ..p(11, 10, 3, 2)
        },
        Proc {
            runnable_ns: 2,
            ..p(12, 10, 4, 1)
        },
        p(20, 1, 100, 100),
    ];
    assert_eq!(
        work_of(10, &procs),
        Some(Work {
            cpu_ns: 12,
            runnable_ns: 39,
            written: 10,
            members: 3,
            ..Work::default()
        })
    );
    assert_eq!(work_of(99, &procs), None);
    assert_eq!(work_of(1, &procs).map(|w| w.members), Some(5));
}

/// A tree's work with what it is credited is the credited processes' trees too, each process
/// once: a credited process inside the tree, or credited twice, adds nothing more, and one
/// that is gone adds nothing; a root that is gone has none, whatever it is credited.
#[test]
fn a_credited_process_works_for_its_tree() {
    let procs = [
        p(1, 0, 1_000, 0),
        p(10, 1, 5, 7),
        p(11, 10, 3, 2),
        p(30, 1, 40, 9),
        p(31, 30, 2, 1),
    ];
    let vm = Some(Work {
        cpu_ns: 50,
        runnable_ns: 50,
        written: 19,
        members: 4,
        ..Work::default()
    });
    assert_eq!(work_with(10, &[30], &procs, &[]), vm);
    assert_eq!(work_with(10, &[30, 31, 11, 30, 10], &procs, &[]), vm);
    assert_eq!(work_with(10, &[99], &procs, &[]), work_of(10, &procs));
    assert_eq!(work_with(99, &[30], &procs, &[]), None);
}

/// A tree waits in its turn when a process of it is marked waiting: behind holders that work
/// while any of its waits says none is stuck, else stuck for the least span its waits say;
/// a mark of a process outside it says nothing of it.
#[test]
fn a_marked_wait_in_the_tree_is_the_trees() {
    let procs = [
        p(1, 0, 0, 0),
        p(10, 1, 0, 0),
        p(11, 10, 0, 0),
        p(30, 1, 0, 0),
    ];
    let wait = |pid, stuck: Option<u64>| {
        let stuck = stuck.map(|s| Stuck {
            holders: vec![7],
            span: Duration::from_secs(s),
        });
        Waiting::said(pid, 0, "", stuck)
    };
    let waits = |marks: &[Waiting]| work_with(10, &[], &procs, marks).map(|w| w.waits);
    assert_eq!(waits(&[]), Some(Waits::No));
    assert_eq!(waits(&[wait(30, None), wait(1, None)]), Some(Waits::No));
    assert_eq!(waits(&[wait(11, None)]), Some(Waits::Behind));
    assert_eq!(
        waits(&[wait(10, Some(960))]),
        Some(Waits::Stuck(Duration::from_secs(960)))
    );
    assert_eq!(
        waits(&[wait(10, Some(960)), wait(11, Some(900))]),
        Some(Waits::Stuck(Duration::from_secs(900)))
    );
    assert_eq!(
        waits(&[wait(10, Some(960)), wait(11, None)]),
        Some(Waits::Behind)
    );
}

/// The stall counts from the first read that found the tree's work as it is: work since the
/// last read sets it to zero, and a new holder or an unread tree starts it again; `stuck`
/// keeps it once it reaches its bound.
#[test]
fn a_stall_counts_from_the_last_work_seen() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let w = |ran: f64| {
        Some(Work {
            runnable_ns: secs(ran),
            members: 1,
            ..Work::default()
        })
    };
    let mut s = Stall::default();
    assert_eq!(s.read(7, w(1.0), at(0)), Some(Duration::ZERO));
    assert_eq!(s.read(7, w(1.0), at(60)), Some(Duration::from_secs(60)));
    assert_eq!(
        s.read(7, w(31.0), at(120)),
        Some(Duration::ZERO),
        "it worked"
    );
    assert_eq!(s.read(7, w(31.0), at(300)), Some(Duration::from_secs(180)));
    assert_eq!(
        s.read(8, w(31.0), at(360)),
        Some(Duration::ZERO),
        "another holder"
    );
    assert_eq!(s.read(8, None, at(420)), None);
    assert_eq!(
        s.read(8, w(31.0), at(480)),
        Some(Duration::ZERO),
        "read again"
    );
    let after = Duration::from_secs(600);
    assert_eq!(s.stuck(8, w(31.0), at(1000), after), None);
    assert_eq!(
        s.stuck(8, w(31.0), at(1080), after),
        Some(Duration::from_secs(600))
    );
    assert_eq!(words(Duration::from_secs(16 * 60 + 59)), "16 min");
    assert_eq!(words(Duration::from_secs(59)), "59 s");
}

/// A holder busy with nothing but upkeep, as this Mac measured each part of it, at `ms`
/// milliseconds: its own process walking its meter's directories each minute (6.5 s of CPU
/// time, the most a walk of the whole shared build cache took) and rewriting its claim (1 KiB);
/// a Go child (`limactl shell`) whose runtime wakes each minute (50 µs) and collects its
/// garbage every two (4 ms); that child's ssh client, idle; and a credited VM whose idle
/// guest's timers take 3% of a core and whose host agent and journal write 70 KiB a minute.
/// `extra` adds work to one of them from `from` ms on: `(pid, cpu, runnable, written, from)`,
/// each a second's.
fn upkeep(ms: u64, extra: Option<(u32, u64, u64, u64, u64)>) -> Vec<Proc> {
    let minutes = ms / 60_000;
    let mut procs = vec![
        p(7, 1, minutes * secs(6.5), minutes * 1024),
        p(8, 7, minutes * secs(50e-6) + ms / 120_000 * secs(4e-3), 0),
        p(9, 8, secs(1e-3), 0),
        p(40, 1, ms * 30_000, minutes * 70 * 1024),
    ];
    if let Some((pid, cpu, runnable, written, from)) = extra {
        let at = ms.saturating_sub(from);
        for q in &mut procs {
            if q.pid == pid && ms >= from {
                let was = q.runnable_ns.max(q.cpu_ns);
                q.cpu_ns += cpu * at / 1000;
                q.runnable_ns = was + runnable.max(cpu) * at / 1000;
                q.written += written * at / 1000;
            }
        }
    }
    procs
}

/// UPKEEP IS NO PROGRESS, WORK IS (CW-WAIT-UNBOUNDED): a holder whose processes only keep
/// their books ([`upkeep`]: its meter, a Go child that ticks, an idle ssh client, an idle
/// credited VM), read every 2 s as a ledger waiter reads or every minute as a lock's waiter
/// does, is stuck at the bound; once any one of them runs a core, or is runnable all the time
/// on a loaded Mac that gives it a twelfth of one (a load of 166 on 14 cores), or writes
/// 10 MiB a minute, or a process starts among them, it works within the minute, though it was
/// idle for most of the bound before; that twelfth of a core with no runnable time read (off
/// macOS) does not. NEGATIVE CONTROLS: the rule before this one, any change of the CPU time of
/// a process the holder runs progress, reads the Go child's ticks alone as work for ever; and
/// so does the rule before that, any change at all.
#[test]
fn upkeep_is_no_progress_and_work_is() {
    let t0 = Instant::now();
    let credited = [40];
    let read = |procs: &[Proc]| work_with(7, &credited, procs, &[]);
    for every in [2_000u64, 60_000] {
        let mut s = Stall::default();
        let (mut child_moved, mut any_moved) = (None::<(u64, u64)>, None::<(u64, u64)>);
        let mut stuck_at = None;
        let mut ms = 0;
        while ms <= 20 * 60_000 {
            let procs = upkeep(ms, None);
            let now = t0 + Duration::from_millis(ms);
            if s.stuck(7, read(&procs), now, STALLED_AFTER).is_some() && stuck_at.is_none() {
                stuck_at = Some(ms);
            }
            // The rules before: any change of the CPU time of what it runs (the Go child and
            // its ssh client alone, a lock's waiter reading no credited VM); any change of
            // anyone's.
            let theirs: u64 = procs
                .iter()
                .filter(|q| [8, 9].contains(&q.pid))
                .map(|q| q.cpu_ns)
                .sum();
            let all: u64 = procs.iter().map(|q| q.cpu_ns).sum();
            for (moved, now) in [(&mut child_moved, theirs), (&mut any_moved, all)] {
                *moved = match *moved {
                    Some((since, was)) if was == now => Some((since, was)),
                    _ => Some((ms, now)),
                };
            }
            ms += every;
        }
        assert_eq!(
            stuck_at,
            Some(15 * 60_000),
            "read every {every} ms: stuck at the bound"
        );
        for (rule, moved) in [("the child's", child_moved), ("any", any_moved)] {
            let (since, _) = moved.expect("read");
            assert!(
                20 * 60_000 - since < 15 * 60_000,
                "read every {every} ms, {rule} change reads it working"
            );
        }
    }
    let core = 1_000_000_000;
    let mib = 1 << 20;
    let start = 12 * 60_000;
    for (what, extra) in [
        ("the holder's own process ran a core", (7, core, 0, 0)),
        ("its child ran a core", (8, core, 0, 0)),
        ("its credited VM ran a core", (40, core, 0, 0)),
        (
            "its child ran a twelfth of a core, runnable all the time",
            (8, core / 12, core, 0),
        ),
        (
            "its credited VM wrote 10 MiB a minute",
            (40, 0, 0, 10 * mib / 60),
        ),
        (
            "its child ran a twelfth of a core, runnable unread",
            (8, core / 12, 0, 0),
        ),
    ] {
        let (pid, cpu, runnable, written) = extra;
        let works = !what.ends_with("unread");
        let mut s = Stall::default();
        let mut idle = Duration::ZERO;
        for m in 0..=12u64 {
            let procs = upkeep(m * 60_000, None);
            idle = s
                .read(7, read(&procs), t0 + Duration::from_secs(m * 60))
                .expect("read");
        }
        assert_eq!(idle, Duration::from_secs(12 * 60), "{what}: idle before");
        let at = |secs: u64| {
            let procs = upkeep(
                start + secs * 1000,
                Some((pid, cpu, runnable, written, start)),
            );
            (
                read(&procs),
                t0 + Duration::from_millis(start + secs * 1000),
            )
        };
        let (work, now) = at(2);
        assert_ne!(
            s.read(7, work, now),
            Some(Duration::ZERO),
            "{what}: 2 s of it"
        );
        let (work, now) = at(30);
        assert_eq!(
            s.read(7, work, now) == Some(Duration::ZERO),
            works,
            "{what}: half a minute"
        );
    }
    let mut s = Stall::default();
    for m in 0..=12u64 {
        s.read(
            7,
            read(&upkeep(m * 60_000, None)),
            t0 + Duration::from_secs(m * 60),
        );
    }
    let mut more = upkeep(start + 2_000, None);
    more.push(p(10, 8, 0, 0));
    assert_eq!(
        s.read(7, read(&more), t0 + Duration::from_millis(start + 2_000)),
        Some(Duration::ZERO),
        "a process started"
    );
    assert!(busy(15_000_000_000, BUSY_OVER) && !busy(14_999_999_999, BUSY_OVER));
    assert!(writing(mib, BUSY_OVER) && !writing(mib - 1, BUSY_OVER));
    assert!(!busy(0, Duration::ZERO) && !writing(0, Duration::ZERO));
}

/// A read's rates are judged over when its sweep read it ([`Work::at`]), its stall over the
/// reads' own clock: reads a tenth of a second apart standing for a minute each, a core run
/// between them is work and a meter's tenth of a core is not.
#[test]
fn a_reads_rates_are_judged_over_when_it_was_read() {
    let (t0, real) = (Instant::now(), Instant::now());
    for (share, works) in [(1.0, true), (0.1, false)] {
        let mut s = Stall::default();
        let mut stuck_at = None;
        for m in 0..=20u32 {
            let at = real + Duration::from_millis(100) * m;
            let work = Work {
                runnable_ns: secs(share * 0.1 * f64::from(m)),
                members: 1,
                at: Some(at),
                ..Work::default()
            };
            let now = t0 + Duration::from_secs(60) * m;
            if s.stuck(7, Some(work), now, STALLED_AFTER).is_some() && stuck_at.is_none() {
                stuck_at = Some(m);
            }
        }
        assert_eq!(stuck_at, (!works).then_some(15), "{share} of a core");
    }
}

/// A tree that waits in its turn behind holders that work works, however idle it is itself;
/// one whose every wait is stuck has made no progress for as long as its waits say, or for
/// as long as its own reads find, whichever is longer; and work of its own sets it to zero
/// whatever its waits say.
#[test]
fn a_tree_that_waits_is_as_stuck_as_what_it_waits_behind() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let w = |ran: f64, waits| {
        Some(Work {
            runnable_ns: secs(ran),
            members: 1,
            waits,
            ..Work::default()
        })
    };
    let behind = |s: u64| Waits::Stuck(Duration::from_secs(s));
    let mut s = Stall::default();
    assert_eq!(
        s.read(7, w(1.0, Waits::Behind), at(0)),
        Some(Duration::ZERO)
    );
    for m in 1..=30 {
        assert_eq!(
            s.read(7, w(1.0, Waits::Behind), at(m * 60)),
            Some(Duration::ZERO)
        );
    }
    assert_eq!(
        s.read(7, w(1.0, behind(900)), at(1860)),
        Some(Duration::from_secs(900)),
        "its wait is stuck"
    );
    assert_eq!(
        s.read(7, w(1.0, behind(960)), at(1920)),
        Some(Duration::from_secs(960))
    );
    assert_eq!(
        s.read(7, w(1.0, behind(60)), at(2900)),
        Some(Duration::from_secs(2900 - 1800)),
        "its own reads find it idle longer"
    );
    assert_eq!(
        s.read(7, w(61.0, behind(990)), at(2960)),
        Some(Duration::ZERO),
        "it worked"
    );
    let mut first = Stall::default();
    assert_eq!(
        first.read(7, w(1.0, behind(960)), at(0)),
        Some(Duration::from_secs(960)),
        "a first read behind a stuck wait"
    );
}

/// This process's tree reads on a machine with a sweep, holding at least this process and
/// stamped with when it was read; a pid no process has reads as none.
#[test]
fn this_processs_tree_reads() {
    if aterm_sysprobe::processes().is_none() {
        return;
    }
    let before = Instant::now();
    let me = tree_work(std::process::id(), &[]).expect("this process is swept");
    assert!(me.members >= 1, "{me:?}");
    assert!(me.at.is_some_and(|at| at >= before), "{me:?}");
    assert_eq!(tree_work(u32::MAX, &[]), None);
}
