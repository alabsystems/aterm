// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Duration;

use super::*;

const GIB: u64 = 1 << 30;

fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

/// A binary whose last run took `wall` seconds at width `width` over `tests` tests, `cpu`
/// seconds of CPU.
fn seen(wall: f64, cpu: f64, width: u32, tests: u32) -> Seen {
    Seen {
        wall: secs(wall),
        width,
        tests,
        per_thread: cpu / wall / f64::from(width.min(tests).max(1)),
        peak: 0,
        failed: false,
    }
}

fn planned(index: usize, charge: u32, peak: u64, wall: f64) -> Planned {
    Planned {
        index,
        width: charge,
        charge,
        peak,
        predicted: Some(secs(wall)),
    }
}

/// A fitting short job uses the gap before a whole-share job without moving that job's start.
/// The control changes only its measured duration: a job longer than the gap stays behind it.
#[test]
fn measured_short_work_fills_a_gap_without_delaying_its_head() {
    let original = |short_wall| Plan {
        share: 4,
        target: secs(10.0),
        order: vec![
            planned(0, 3, 2 * GIB, 10.0),
            planned(1, 4, 10 * GIB, 9.0),
            planned(2, 1, 3 * GIB, short_wall),
        ],
    };
    let base = original(8.0);
    assert_eq!(schedule(&base).end, secs(27.0));
    let filled = fill_gaps(base.clone());
    assert_eq!(
        filled.order.iter().map(|b| b.index).collect::<Vec<_>>(),
        [0, 2, 1]
    );
    let learned = plan(
        &[
            Some(seen(10.0, 30.0, 1, 1)),
            Some(seen(9.0, 36.0, 1, 1)),
            Some(seen(8.0, 8.0, 1, 1)),
        ],
        4,
        None,
        0,
    );
    assert_eq!(
        learned.order.iter().map(|b| b.index).collect::<Vec<_>>(),
        [0, 2, 1]
    );
    assert_eq!(
        schedule(&filled),
        Schedule {
            end: secs(19.0),
            peak: 10 * GIB
        }
    );
    // Every job keeps its width, charge, memory and predicted cost.
    for b in &base.order {
        assert_eq!(at(&filled, b.index), *b);
    }
    let too_long = original(11.0);
    assert_eq!(fill_gaps(too_long.clone()), too_long);
    let mut unknown = original(8.0);
    unknown.order[2].predicted = None;
    assert_eq!(fill_gaps(unknown.clone()), unknown);
    // Runtime memory admission still refuses the fitting short job until room exists.
    let mut memory_plan = filled.clone();
    memory_plan.share = 5;
    let mut limited = Dispatcher::new(&memory_plan, Some(10 * GIB));
    assert_eq!(limited.next(0.0).map(|b| b.index), Some(0));
    assert_eq!(limited.next(0.0).map(|b| b.index), Some(2));
    assert_eq!(limited.next(0.0), None);
    limited.finish(0).unwrap();
    assert_eq!(limited.next(0.0), None, "the short job still holds 3 GiB");
    limited.finish(2).unwrap();
    assert_eq!(limited.next(0.0).map(|b| b.index), Some(1));
}

/// Binary `index` of plan `p`.
fn at(p: &Plan, index: usize) -> Planned {
    p.order
        .iter()
        .find(|b| b.index == index)
        .copied()
        .unwrap_or_else(|| panic!("binary {index} is not in {p:?}"))
}

/// THE ORDER: binaries with no history first in cargo's order, then the ones whose last run
/// failed, then the rest, each of those groups longest predicted first. NEGATIVE CONTROL: the
/// same binary passing last time sits by its length among the rest.
#[test]
fn unseen_binaries_start_first_then_the_failed_then_the_longest() {
    let short = seen(2.0, 2.0, 1, 1);
    let long = seen(30.0, 30.0, 1, 1);
    let failed = Seen {
        failed: true,
        ..seen(5.0, 5.0, 1, 1)
    };
    let order = |p: &Plan| -> Vec<usize> { p.order.iter().map(|b| b.index).collect::<Vec<_>>() };
    let plan_of = |third: Seen| {
        plan(
            &[Some(short), None, Some(long), Some(third), None],
            8,
            None,
            GIB,
        )
    };
    assert_eq!(order(&plan_of(failed)), [1, 4, 3, 2, 0]);
    assert_eq!(
        order(&plan_of(Seen {
            failed: false,
            ..failed
        })),
        [1, 4, 2, 3, 0]
    );
    let unseen = plan_of(failed);
    assert!(
        unseen.order[..2]
            .iter()
            .all(|b| (b.width, b.charge, b.peak, b.predicted)
                == (UNSEEN_WIDTH, UNSEEN_WIDTH, GIB, None)),
        "a binary with no history is planned as heavy as the heaviest kept: {unseen:?}"
    );
    assert_eq!(
        plan(&[None], 2, None, 0).order[0].width,
        2,
        "never wider than the share"
    );
    assert_eq!(
        plan(&[None], 8, Some(3), 0).order[0].width,
        3,
        "nor than the ceiling"
    );
}

/// The whole-run target never narrows isolated cases below the initial width. This removes
/// the old 80 s packing choice: the same work finishes in 70 s at four threads. The two-case
/// control is capped by its case count, not forced to four threads.
#[test]
fn the_widths_are_those_whose_schedule_ends_first_from_the_bound_up() {
    let wide = seen(40.0, 160.0, 4, 100);
    let narrow = seen(60.0, 60.0, 1, 100);
    let all = [Some(wide), Some(narrow), Some(narrow)];
    let known: Vec<(usize, Seen)> = all
        .iter()
        .enumerate()
        .filter_map(|(i, s)| Some((i, (*s)?)))
        .collect();
    assert_eq!(bound(&known, 4, 4), secs(70.0));
    let p = plan(&all, 4, None, 0);
    assert_eq!(
        (at(&p, 0).width, at(&p, 0).charge, at(&p, 0).predicted),
        (4, 4, Some(secs(40.0))),
        "{p:?}"
    );
    for i in 1..3 {
        assert_eq!((at(&p, i).width, at(&p, i).charge), (4, 4), "{p:?}");
    }
    assert_eq!(schedule(&p).end, secs(70.0), "{p:?}");
    let at_bound = Plan {
        share: 4,
        target: secs(70.0),
        order: at_target(&known, secs(70.0), 4, 4),
    };
    assert_eq!(at(&at_bound, 0).width, 4, "{at_bound:?}");
    assert_eq!(schedule(&at_bound).end, secs(70.0), "{at_bound:?}");
    let packed = Plan {
        share: 4,
        target: secs(15.0),
        order: at_target(&known, secs(15.0), 4, 4),
    };
    assert!(packed.order.iter().all(|b| b.width == 4), "{packed:?}");
    assert_eq!(schedule(&packed).end, secs(70.0), "{packed:?}");

    let pair = [Some(seen(1.0, 1.0, 1, 2)), Some(seen(2.0, 2.0, 1, 2))];
    let pair_known: Vec<(usize, Seen)> = pair
        .iter()
        .enumerate()
        .filter_map(|(i, s)| Some((i, (*s)?)))
        .collect();
    let pair_floor = bound(&pair_known, 2, 2);
    assert_eq!(pair_floor, secs(1.5));
    let below = Plan {
        share: 2,
        target: secs(0.5),
        order: at_target(&pair_known, secs(0.5), 2, 2),
    };
    assert!(below.order.iter().all(|b| b.width == 2), "{below:?}");
    assert_eq!(schedule(&below).end, pair_floor, "{below:?}");
    let regularized = plan(&pair, 2, None, 0);
    assert_eq!(schedule(&regularized).end, pair_floor, "{regularized:?}");
}

/// Above the initial width the target still optimizes the predicted schedule. Smaller
/// binaries keep four threads; a long CPU-bound binary takes six of the eight cores.
#[test]
fn of_plans_that_end_together_the_narrowest() {
    let busy = seen(80.0, 640.0, 8, 100);
    let quick = seen(10.0, 80.0, 8, 100);
    let all = [
        Some(busy),
        Some(quick),
        Some(quick),
        Some(quick),
        Some(quick),
    ];
    let p = plan(&all, 8, None, 0);
    assert_eq!(p.target, secs(120.0), "{p:?}");
    assert_eq!(schedule(&p).end, busy.wall_at(6) + secs(40.0), "{p:?}");
    assert_eq!((at(&p, 0).width, at(&p, 0).charge), (6, 6), "{p:?}");
    for i in 1..5 {
        assert_eq!(
            (at(&p, i).width, at(&p, i).charge, at(&p, i).predicted),
            (4, 4, Some(secs(20.0))),
            "{p:?}"
        );
    }
    let known: Vec<(usize, Seen)> = all
        .iter()
        .enumerate()
        .filter_map(|(i, s)| Some((i, (*s)?)))
        .collect();
    let at_bound = Plan {
        share: 8,
        target: secs(120.0),
        order: at_target(&known, secs(120.0), 8, 8),
    };
    assert_eq!(at(&at_bound, 0).width, 6, "{at_bound:?}");
    assert_eq!(
        schedule(&at_bound).end,
        busy.wall_at(6) + secs(40.0),
        "{at_bound:?}"
    );
}

/// THE WALL: the threads, not the CPU, scale a wall. A binary that waits on all eight threads
/// for 10 s and computes on almost none takes 80 s on one thread, as long as one that computed
/// on all eight; a binary of two tests is no faster on more than two threads, and none runs
/// wider than the ceiling. NEGATIVE CONTROL: the two binaries' CPU differ eightfold and their
/// walls on one thread do not.
#[test]
fn a_binarys_threads_scale_its_wall() {
    let waits = seen(10.0, 10.0, 8, 100);
    let computes = seen(10.0, 80.0, 8, 100);
    assert_eq!(waits.wall_at(1), secs(80.0));
    assert_eq!(waits.wall_at(1), computes.wall_at(1));
    assert_eq!(waits.wall_at(8), secs(10.0));
    let few = seen(9.0, 18.0, 8, 2);
    assert_eq!(few.wall_at(8), few.wall_at(2), "two tests use two threads");
    assert_eq!(
        plan(&[Some(few)], 8, None, 0).order[0].width,
        2,
        "no wider than its tests"
    );
    let busy = seen(80.0, 640.0, 8, 100);
    let narrowed = plan(&[Some(busy), Some(waits)], 8, Some(4), 0);
    assert_eq!(
        (at(&narrowed, 0).width, schedule(&narrowed).end),
        (4, secs(160.0)),
        "the busy binary at the ceiling's four threads: {narrowed:?}"
    );
}

/// THE CHARGE: a binary is charged its width, or the cores it used, whichever is more, up to the
/// share: tests that spawn processes use more than their threads. NEGATIVE CONTROL: the same
/// binary using one core per thread is charged its width.
#[test]
fn a_binary_that_used_more_than_its_threads_is_charged_what_it_used() {
    let spawns = Seen {
        per_thread: 2.5,
        ..seen(10.0, 100.0, 4, 50)
    };
    assert_eq!(spawns.charge_at(4, 16), 10);
    assert_eq!(spawns.charge_at(4, 8), 8, "never more than the share");
    assert_eq!(
        Seen {
            per_thread: 1.0,
            ..spawns
        }
        .charge_at(4, 16),
        4
    );
}

/// The release's observed poles retain concurrent cases even when the whole-run bound is
/// longer than either binary's serial wall. Reverting the initial-width floor makes both
/// serial; charging waiting threads as cores makes the suite's charge four instead of one.
#[test]
fn unrelated_work_cannot_serialize_the_release_poles() {
    let mut all = vec![
        Some(seen(805.0, 122.0, 1, 250)),
        Some(seen(392.0, 392.0, 1, 1072)),
    ];
    all.extend(std::iter::repeat_n(Some(seen(1262.0, 1262.0, 1, 1)), 8));
    let p = plan(&all, 8, None, 0);
    assert_eq!((at(&p, 0).width, at(&p, 0).charge), (4, 1), "{p:?}");
    assert_eq!((at(&p, 1).width, at(&p, 1).charge), (4, 4), "{p:?}");
    for share in 1..=3 {
        let p = plan(&all[..2], share, None, 0);
        assert!(
            p.order
                .iter()
                .all(|b| b.width <= share && b.charge <= share)
        );
    }
    let p = plan(&all[..2], 8, Some(2), 0);
    assert!(p.order.iter().all(|b| b.width == 2));
}

/// Idle cases need threads but not one core each. The CPU-bound control needs four cores
/// at the same width, and a measured zero still needs one dispatcher token.
#[test]
fn waiting_threads_are_charged_their_measured_cpu() {
    let waits = seen(100.0, 10.0, 1, 100);
    assert_eq!(waits.charge_at(4, 8), 1);
    assert_eq!(seen(100.0, 100.0, 1, 100).charge_at(4, 8), 4);
    assert_eq!(seen(100.0, 0.0, 1, 100).charge_at(4, 8), 1);
    assert_eq!(waits.wall_at(4), secs(25.0));
}

/// THE WIDEST: a binary whose tests each use more than their thread (processes, a pool of their
/// own) runs no wider than the width at which what it uses fits the share, however long that
/// takes; one that uses more than the share on one thread runs one wide, charged the share.
/// NEGATIVE CONTROL: the same binary using one core per thread runs at the whole share.
#[test]
fn a_binary_that_uses_more_than_its_threads_runs_no_wider_than_the_share_holds() {
    // 11.1 cores at six threads: 1.85 per thread, so 5.55 at three and 7.4 at four.
    let pools = seen(100.0, 1110.0, 6, 50);
    let planned = at(&plan(&[Some(pools)], 6, None, 0), 0);
    assert_eq!((planned.width, planned.charge), (3, 6), "{planned:?}");
    assert_eq!(planned.predicted, Some(secs(200.0)));
    let threads = seen(100.0, 600.0, 6, 50);
    let planned = at(&plan(&[Some(threads)], 6, None, 0), 0);
    assert_eq!((planned.width, planned.charge), (6, 6), "{planned:?}");
    let lanes = Seen {
        per_thread: 14.0,
        ..pools
    };
    let planned = at(&plan(&[Some(lanes)], 6, None, 0), 0);
    assert_eq!((planned.width, planned.charge), (1, 6), "{planned:?}");
}

/// THE DISPATCHER: the head starts only while its charge fits in what the running binaries leave
/// of the share and its peak in what they leave of the memory, and, beside a running binary, only
/// while the live rate and its charge stay within the share and one core; an ending binary gives
/// both back once; a halt starts nothing more. NEGATIVE CONTROLS: a second finish of one binary is
/// refused, and the third binary, which fits, waits behind the head, which does not.
#[test]
fn the_dispatcher_starts_the_head_only_while_its_charge_fits() {
    let p = Plan {
        share: 8,
        target: secs(10.0),
        order: vec![
            planned(0, 4, 2 * GIB, 10.0),
            planned(1, 4, 2 * GIB, 10.0),
            planned(2, 6, GIB, 5.0),
            planned(3, 1, GIB, 1.0),
        ],
    };
    let mut d = Dispatcher::new(&p, Some(5 * GIB));
    assert_eq!(d.next(0.0).map(|b| b.index), Some(0));
    assert_eq!(d.next(0.0).map(|b| b.index), Some(1));
    assert_eq!((d.held(), d.held_bytes()), (8, 4 * GIB));
    assert_eq!(d.next(0.0), None, "6 cores do not fit in none");
    assert_eq!(d.finish(0), Ok(()));
    assert_eq!(d.finish(0), Err("that binary is not running"));
    assert_eq!(d.held(), 4);
    assert_eq!(
        d.next(0.0),
        None,
        "6 cores do not fit in 4, and 3 waits behind"
    );
    assert_eq!(d.finish(1), Ok(()));
    assert_eq!(d.next(0.0).map(|b| b.index), Some(2));
    assert_eq!(
        d.next(8.5),
        None,
        "8.5 live cores and 1 more pass 8 and one"
    );
    assert_eq!(d.next(8.0).map(|b| b.index), Some(3));
    assert!(!d.waiting());
    assert_eq!(d.phase(3), Some(Phase::Running));
    let mut halted = Dispatcher::new(&p, None);
    halted.halt();
    assert_eq!(halted.next(0.0), None);
    assert!(!halted.waiting());
    let mut alone = Dispatcher::new(&p, Some(GIB));
    assert_eq!(alone.memory(), Some(2 * GIB), "raised to the largest peak");
    assert_eq!(
        alone.next(100.0).map(|b| b.index),
        Some(0),
        "nothing running: the head starts whatever the live rate"
    );
    assert_eq!(
        alone.next(0.0),
        None,
        "its peak leaves no room for the next"
    );
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

/// THE CLAIM: the most memory the schedule holds at once, not every binary's at once. The 10 GiB
/// binary takes the whole share and runs alone; the two 3 GiB ones run together after it. A
/// binary with no history holds what the plan gave it, the heaviest kept. NEGATIVE CONTROL: on a
/// share that fits them all at once, their sum.
#[test]
fn the_memory_claim_is_the_schedules_own_concurrent_peak() {
    let p = |share: u32| Plan {
        share,
        target: secs(10.0),
        order: vec![
            planned(0, 4, 10 * GIB, 10.0),
            planned(1, 2, 3 * GIB, 5.0),
            planned(2, 2, 3 * GIB, 5.0),
        ],
    };
    assert_eq!(
        schedule(&p(4)),
        Schedule {
            end: secs(15.0),
            peak: 10 * GIB
        }
    );
    assert_eq!(schedule(&p(8)).peak, 16 * GIB);
    assert_eq!(
        schedule(&plan(&[None, None], 4, None, GIB)).peak,
        GIB,
        "two unseen binaries four wide take turns on four cores"
    );
    assert_eq!(schedule(&plan(&[None, None], 8, None, GIB)).peak, 2 * GIB);
    assert_eq!(schedule(&plan(&[None, None], 8, None, 0)).peak, 0);
}
/// Recorded durations reach the actual dispatcher's first admission. With no records it
/// retains the declared order. The control changes only the planted binary's duration.
#[test]
fn a_planted_long_binary_starts_first_from_recorded_history() {
    use crate::test_history::{History, Sample};
    let history = |long_ms| {
        let mut history = History::default();
        for (name, wall_ms) in [("short", 10), ("planted", long_ms), ("other", 20)] {
            history.push(
                name,
                Sample {
                    partition: None,
                    at_ms: 1,
                    outcome: "ok".into(),
                    wall_ms,
                    cpu_ms: Some(wall_ms),
                    starved_ms: Some(0),
                    peak_bytes: Some(1),
                    width: 1,
                    tests: Some(1),
                },
            );
        }
        History::decode(&history.encode())
    };
    let first = |history: &History| {
        let seen = ["short", "planted", "other"].map(|name| history.seen(name));
        Dispatcher::new(&plan(&seen, 1, None, 1), None)
            .next(0.0)
            .unwrap()
            .index
    };
    assert_eq!(first(&history(1000)), 1);
    assert_eq!(first(&history(1)), 2);
    assert_eq!(first(&History::default()), 0);
}
