// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

fn seen(width: u32, wall_s: f64, cpu_s: f64) -> Seen {
    Seen {
        width,
        wall_s,
        cpu_s,
        floor_s: wall_s,
        tests: None,
        failed: false,
    }
}

fn history(rows: &[(&str, Seen)]) -> BTreeMap<String, Seen> {
    rows.iter()
        .map(|(spec, s)| ((*spec).to_string(), s.clone()))
        .collect()
}

/// THE BUDGET: the charges of the binaries running never sum past the share, every binary runs
/// once at its planned width, and the answers come back by index. NEGATIVE CONTROL: the same
/// binaries started by as many workers as the share with no budget (the pool `e77a54f4d9`
/// replaced held `jobs × threads` fixed) hold the first six charges, 14 cores, at once.
#[test]
fn running_charges_never_exceed_the_share() {
    let specs: Vec<String> = (0..12).map(|i| format!("b{i}")).collect();
    let refs: Vec<&str> = specs.iter().map(String::as_str).collect();
    let mut plan = plan(&refs, &BTreeMap::new(), 6, 6);
    plan.order = (0..12).collect();
    for i in 0..12 {
        plan.width[i] = [4, 4, 2, 2, 1, 1][i % 6];
        plan.charge[i] = plan.width[i];
    }
    let held = AtomicU32::new(0);
    let peak = AtomicU32::new(0);
    let started = AtomicU32::new(0);
    let answers = dispatch(&plan, 6, |i, width| {
        assert_eq!(width, plan.width[i]);
        started.fetch_add(1, Ordering::SeqCst);
        let now = held.fetch_add(plan.charge[i], Ordering::SeqCst) + plan.charge[i];
        peak.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(5));
        held.fetch_sub(plan.charge[i], Ordering::SeqCst);
        i
    });
    assert!(peak.load(Ordering::SeqCst) <= 6, "{peak:?}");
    assert_eq!(answers, (0..12).map(Some).collect::<Vec<_>>());
    assert_eq!(
        started.load(Ordering::SeqCst),
        12,
        "each binary started once"
    );

    let held = AtomicU32::new(0);
    let peak = AtomicU32::new(0);
    let all_started = std::sync::Barrier::new(6);
    let next = AtomicU32::new(0);
    std::thread::scope(|scope| {
        for _ in 0..6 {
            scope.spawn(|| {
                let mut first = true;
                while let Some(&i) = plan.order.get(next.fetch_add(1, Ordering::SeqCst) as usize) {
                    let now = held.fetch_add(plan.charge[i], Ordering::SeqCst) + plan.charge[i];
                    peak.fetch_max(now, Ordering::SeqCst);
                    if std::mem::take(&mut first) {
                        all_started.wait();
                    }
                    held.fetch_sub(plan.charge[i], Ordering::SeqCst);
                }
            });
        }
    });
    // The first six charges (14 cores) are all held at the barrier.
    assert!(peak.load(Ordering::SeqCst) >= 14, "the control overloads");
}

/// Binaries whose charges fit together run together: two charges of three in a share of six
/// each wait for the other to start (a one-minute hang detector), so a dispatcher that ran them
/// one at a time would hang here.
#[test]
fn charges_that_fit_run_together() {
    let mut plan = plan(&["a", "b"], &BTreeMap::new(), 6, 6);
    plan.charge = vec![3, 3];
    let started = (Mutex::new(0_u32), Condvar::new());
    let met = dispatch(&plan, 6, |_, _| {
        let mut n = started.0.lock().unwrap();
        *n += 1;
        started.1.notify_all();
        let (n, waited) = started
            .1
            .wait_timeout_while(n, aterm_spec::wait::MUST_HAPPEN, |n| *n < 2)
            .unwrap();
        *n == 2 && !waited.timed_out()
    });
    assert_eq!(met, [Some(true), Some(true)]);
}

/// A charge as wide as the share runs alone, and a run of one-core binaries fills the share.
#[test]
fn a_full_share_charge_runs_alone_and_narrow_ones_fill_the_share() {
    let specs = ["wide", "n1", "n2", "n3", "n4"];
    let mut plan = plan(&specs, &BTreeMap::new(), 4, 4);
    plan.order = vec![0, 1, 2, 3, 4];
    plan.charge = vec![4, 1, 1, 1, 1];
    let held = AtomicU32::new(0);
    let wide_alone = std::sync::atomic::AtomicBool::new(true);
    let peak_narrow = AtomicU32::new(0);
    dispatch(&plan, 4, |i, _| {
        let now = held.fetch_add(plan.charge[i], Ordering::SeqCst) + plan.charge[i];
        if i == 0 && now != 4 {
            wide_alone.store(false, Ordering::SeqCst);
        }
        if i > 0 {
            peak_narrow.fetch_max(now, Ordering::SeqCst);
        }
        std::thread::sleep(Duration::from_millis(5));
        held.fetch_sub(plan.charge[i], Ordering::SeqCst);
    });
    assert!(wide_alone.load(Ordering::SeqCst));
    assert!(peak_narrow.load(Ordering::SeqCst) <= 4);
}

/// THE WIDTHS: the short binaries drop to one thread, each charged one core, and the plan is
/// the one whose schedule ends soonest: the pole takes the whole share and the shorts then run
/// together (14 s), not the pole at the least width within `T*` = 12.5 s with the shorts queued
/// one at a time in the core it leaves (15.4 s). NEGATIVE CONTROL: the rule `e77a54f4d9` left
/// gives every binary the whole share, so one runs at a time; and the `T*`-bound plan, the
/// first candidate, predicts the longer schedule it was passed over for.
#[test]
fn short_binaries_run_narrow_and_the_plan_is_the_shortest_schedule() {
    let mut rows = vec![("pole", seen(8, 10.0, 80.0))];
    let shorts: Vec<String> = (0..5).map(|i| format!("short{i}")).collect();
    for s in &shorts {
        let mut short = seen(8, 1.0, 4.0);
        short.tests = Some(4);
        rows.push((s.as_str(), short));
    }
    let specs: Vec<&str> = rows.iter().map(|(k, _)| *k).collect();
    let h = history(&rows);
    let p = plan(&specs, &h, 8, 8);
    let target = p.target_s.expect("history gives a target");
    assert!((target - 14.0).abs() < 1e-9, "{target}");
    assert_eq!(p.width[0], 8, "{p:?}");
    assert_eq!(&p.width[1..], [1; 5], "{p:?}");
    assert_eq!(&p.charge[1..], [1; 5], "{p:?}");
    assert_eq!(p.order[0], 0, "longest predicted first");
    assert_ne!(
        p.width[1..],
        [4; 5],
        "the control's whole-share widths are gone"
    );
    let seen: Vec<Option<&Seen>> = specs.iter().map(|s| h.get(*s)).collect();
    let bound = super::plan_within(&seen, 8, 8, Some(100.0 / 8.0));
    assert_eq!(bound.width[0], 7, "{bound:?}");
    let longer = bound.makespan();
    assert!((longer - (80.0 / 7.0 + 4.0)).abs() < 1e-9, "{longer}");
}

/// A binary seen only at width 1 is not pinned there: its tests are a queue, so four threads
/// drain it in a quarter of the time. NEGATIVE CONTROL: the shortest wall it has taken (100 s at
/// width 1) is no floor, and taken as one it would hold the binary at 100 s at every width.
#[test]
fn a_binary_seen_narrow_is_predicted_to_drain_its_tests_wider() {
    let mut narrow = seen(1, 100.0, 100.0);
    narrow.tests = Some(100);
    narrow.floor_s = 0.0;
    assert!(
        (narrow.predicted(4) - 25.0).abs() < 1e-9,
        "{}",
        narrow.predicted(4)
    );
    let p = plan(&["narrow"], &history(&[("narrow", narrow.clone())]), 4, 4);
    assert_eq!((p.width[0], p.charge[0]), (4, 4), "{p:?}");
    let pinned = Seen {
        floor_s: 100.0,
        ..narrow
    };
    assert_eq!(pinned.predicted(4), 100.0, "the control");
}

/// A thread that waited holds its core: a binary that used half a core per thread is predicted
/// to take twice as long per thread when narrowed, and the target counts the cores its threads
/// held, not only the CPU they burned. NEGATIVE CONTROL: the CPU alone (400 core-seconds over a
/// share of 4) would set a target of 100 s that the two cannot meet side by side.
#[test]
fn a_binary_whose_threads_wait_holds_its_width_of_the_share() {
    let waits = seen(4, 100.0, 200.0);
    assert!((waits.rate(1) - 0.5).abs() < 1e-9, "{}", waits.rate(1));
    assert!((waits.predicted(1) - 400.0).abs() < 1e-9);
    assert!((waits.held_s() - 400.0).abs() < 1e-9);
    let rows = [("a", waits.clone()), ("b", waits)];
    let p = plan(&["a", "b"], &history(&rows), 4, 4);
    let target = p.target_s.expect("a target");
    assert!((target - 200.0).abs() < 1e-9, "{target}");
    assert_ne!(target, 400.0 / 4.0, "the control's CPU-only target");
    assert_eq!((p.width, p.charge), (vec![2, 2], vec![2, 2]));
}

/// A binary whose tests start busy children is charged the cores they used, not its width, and
/// one with a single test runs one thread.
#[test]
fn charges_follow_descendants_and_widths_follow_test_counts() {
    let mut spawner = seen(1, 10.0, 40.0);
    spawner.tests = Some(3);
    let mut single = seen(8, 2.0, 2.0);
    single.tests = Some(1);
    let rows = [("spawner", spawner), ("single", single)];
    let p = plan(&["spawner", "single"], &history(&rows), 8, 8);
    assert_eq!((p.width[0], p.charge[0]), (1, 4), "{p:?}");
    assert_eq!((p.width[1], p.charge[1]), (1, 1), "{p:?}");
    let mut capped = seen(1, 10.0, 400.0);
    capped.tests = Some(1);
    let p = plan(&["capped"], &history(&[("capped", capped)]), 8, 8);
    assert_eq!(p.charge[0], 8, "a charge never passes the share");
}

/// THE ORDER: binaries with no history first at [`NEW_WIDTH`] (within the cap), then the ones
/// that failed last time, then the rest longest predicted first; ties keep cargo's order.
#[test]
fn new_then_failed_then_longest_first() {
    let mut failed = seen(2, 1.0, 2.0);
    failed.failed = true;
    let rows = [
        ("short", seen(2, 1.0, 2.0)),
        ("failed", failed),
        ("long", seen(2, 30.0, 60.0)),
    ];
    let specs = ["short", "failed", "fresh", "long", "fresh2"];
    let p = plan(&specs, &history(&rows), 8, 8);
    assert_eq!(p.order, [2, 4, 1, 3, 0], "{p:?}");
    assert_eq!((p.width[2], p.charge[2]), (NEW_WIDTH, NEW_WIDTH));
    assert_eq!(p.class[1], Class::FailedLast);
    let narrow = plan(&["fresh"], &BTreeMap::new(), 8, 3);
    assert_eq!(narrow.width, [3], "the cap bounds a new binary's width");
    assert_eq!(narrow.target_s, None);
    let tiny = plan(&["fresh"], &BTreeMap::new(), 2, 8);
    assert_eq!(
        (tiny.width[0], tiny.charge[0]),
        (2, 2),
        "and the share does"
    );
}

/// A history row reads as its newest sample (its width, wall, CPU, test count and verdict), with
/// the shortest wall of the samples that ran every test at once as its floor; a sample whose
/// CPU was unread stands at its wall times its width. NEGATIVE CONTROL: the shortest wall of all
/// its samples (a queue drained at width 3) is not the floor.
#[test]
fn a_row_reads_as_its_newest_sample_and_its_all_at_once_floor() {
    use crate::test_history::{Row, Sample};
    let sample = |width: u32, wall_ms: u64, cpu_ms: Option<u64>, outcome: &str| Sample {
        at_ms: 1,
        outcome: outcome.into(),
        wall_ms,
        cpu_ms,
        starved_ms: None,
        peak_bytes: None,
        width,
        tests: Some(12),
    };
    let row = Row {
        test: "-p x --lib".into(),
        samples: vec![
            sample(3, 1000, Some(2000), "ok"),
            sample(12, 2500, Some(9000), "ok"),
            sample(12, 1500, Some(9000), "ok"),
            sample(3, 6000, Some(12000), "FAIL"),
        ],
    };
    let seen = Seen::of(&row).expect("a sample");
    assert_eq!(
        (seen.width, seen.wall_s, seen.cpu_s, seen.tests, seen.failed),
        (3, 6.0, 12.0, Some(12), true)
    );
    assert_eq!(seen.floor_s, 1.5, "the shortest all-at-once wall");
    assert_ne!(seen.floor_s, 1.0, "the control: a width-3 queue's wall");
    let unread = Row {
        test: "-p y --lib".into(),
        samples: vec![sample(3, 2000, None, "ok")],
    };
    let unread = Seen::of(&unread).expect("a sample");
    assert_eq!((unread.cpu_s, unread.floor_s), (6.0, 0.0));
    let empty = Row {
        test: "-p z --lib".into(),
        samples: Vec::new(),
    };
    assert_eq!(Seen::of(&empty), None);
}

/// A binary whose callback panics returns its charge: the binary queued behind it, which needs
/// the whole share, still runs, and the panic is the dispatcher's once it has. NEGATIVE CONTROL:
/// were the charge kept, the second binary would wait for it forever, which the one-minute hang
/// detector here reports.
#[test]
fn a_panicking_binary_returns_its_charge() {
    let mut plan = plan(&["boom", "after"], &BTreeMap::new(), 2, 2);
    plan.order = vec![0, 1];
    plan.charge = vec![2, 2];
    let (said, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ran_after = std::sync::atomic::AtomicBool::new(false);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dispatch(&plan, 2, |i, _| {
                assert!(i != 0, "the first binary's callback panics");
                ran_after.store(true, Ordering::SeqCst);
            })
        }));
        said.send((outcome.is_err(), ran_after.load(Ordering::SeqCst)))
            .ok();
    });
    let (panicked, ran_after) = heard
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the queued binary was never started: the panicking one kept its charge");
    assert!(panicked, "the panic is the dispatcher's");
    assert!(ran_after, "the queued binary still ran");
}

/// THE BACKSTOP: while the binaries running use the whole share, as their meter reads it, no
/// other starts beside them though its charge fits; once nothing runs, the next starts whatever
/// the rate says. NEGATIVE CONTROL: with the rate low, the same two binaries meet each other
/// (a one-minute hang detector), so the rate alone held them apart.
#[test]
fn a_hot_live_rate_holds_back_a_start_its_charge_allows() {
    let mut plan = plan(&["a", "b", "c"], &BTreeMap::new(), 6, 6);
    plan.order = vec![0, 1, 2];
    plan.charge = vec![1, 1, 1];
    let running = AtomicU32::new(0);
    let peak = AtomicU32::new(0);
    let hot = || Some(6.5);
    dispatch_with(&plan, 3, &|_| {}, &hot, |_, _| {
        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(5));
        running.fetch_sub(1, Ordering::SeqCst);
    });
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "one at a time while the share is hot"
    );

    let mut two = plan.clone();
    two.order = vec![0, 1];
    two.width.truncate(2);
    two.charge.truncate(2);
    let cool = || Some(1.0);
    let started = (Mutex::new(0_u32), Condvar::new());
    let met = dispatch_with(&two, 2, &|_| {}, &cool, |_, _| {
        let mut n = started.0.lock().unwrap();
        *n += 1;
        started.1.notify_all();
        let (n, waited) = started
            .1
            .wait_timeout_while(n, aterm_spec::wait::MUST_HAPPEN, |n| *n < 2)
            .unwrap();
        *n == 2 && !waited.timed_out()
    });
    assert_eq!(met, [Some(true), Some(true)]);
}

/// THE METER'S RATE: a group's rate is the CPU its members gained between two reads over the time
/// between them, a member first seen bringing all it has used; an ended group's rate leaves the
/// sum. NEGATIVE CONTROL: before any read the rate is unknown, not zero.
#[test]
fn the_meter_reads_the_cores_its_live_groups_use() {
    let member = |pid, cpu_s: u64| aterm_sysprobe::MemberUsage {
        pid,
        start: 1,
        cpu_ns: cpu_s * 1_000_000_000,
        runnable_ns: 0,
        io_bytes: 0,
        footprint: 1,
        peak_footprint: 1,
    };
    let meter = crate::memory::Meter::new();
    assert_eq!(meter.live_rate(), None);
    let t0 = std::time::Instant::now();
    meter.book_at(7, &[member(70, 1)], t0);
    meter.book_at(8, &[member(80, 0)], t0);
    assert_eq!(meter.live_rate(), Some(0.0), "one read is no rate yet");
    let t1 = t0 + Duration::from_secs(2);
    meter.book_at(7, &[member(70, 3), member(71, 4)], t1);
    meter.book_at(8, &[member(80, 2)], t1);
    assert_eq!(
        meter.live_rate(),
        Some(3.0 + 1.0),
        "6 s over 2 s, and 2 s over 2 s"
    );
    meter.ended(7);
    assert_eq!(meter.live_rate(), Some(1.0));
}
