// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Duration;

use aterm_effects::aos_momentum::corpus::scenario;
use aterm_effects::aos_momentum::lane::{self, After, LaneRun};
use aterm_effects::aos_momentum::m4::Evaluator;
use aterm_effects::aos_momentum::plan::{Split, Wake};
use aterm_effects::aos_momentum::{MOMENTUM_HOST_MAX_EVENTS, MomentumHost};

use super::*;

/// The build the tests' windows and replays name.
const TEST_BUILD: &str = "aterm-lab+g0123456789ab";

/// A key at `at_ms` ms, as an attached host records it.
fn key(cursor: u64, at_ms: i128) -> Step {
    Step {
        cursor,
        input: MomentumEvent::Key {
            at: at_ms * 1_000_000,
            tau_bits: 1.2f32.to_bits(),
            echo: aterm_effects::aos_momentum::KeyEcho::Seen,
        },
        obs: MomentumObservation::Keyed,
    }
}

fn tick(cursor: u64, at_ms: i128, fp: u64, cur: (u16, u16)) -> Step {
    tick_ns(cursor, at_ms * 1_000_000, fp, cur)
}

/// A tick at `at` ns drawing the frame `fp` (inert when 0) with the caret at `cur`.
fn tick_ns(cursor: u64, at: i128, fp: u64, cur: (u16, u16)) -> Step {
    let lane = aterm_effects::aos_momentum::lane::s0();
    let p = lane.start();
    Step {
        cursor,
        input: MomentumEvent::Tick {
            cur: Some(cur),
            at,
            geom: p.geom,
            cfg: p.cfg,
        },
        obs: MomentumObservation::Frame {
            fill: (fp != 0).then_some(0x00ff_00ff),
            hot: fp != 0,
            fp,
            out: Vec::new(),
            under: Vec::new(),
        },
    }
}

fn query(cursor: u64, at_ms: i128, deadline_ms: Option<i128>) -> Step {
    query_ns(
        cursor,
        at_ms * 1_000_000,
        16_666_667,
        deadline_ms.map(|d| d * 1_000_000),
    )
}

/// A query at `at` ns carrying the frame interval `frame_ns`, answered settling at `deadline` (ns), or not settling without one.
fn query_ns(cursor: u64, at: i128, frame_ns: u64, deadline: Option<i128>) -> Step {
    Step {
        cursor,
        input: MomentumEvent::Query {
            at,
            frame_interval: Duration::from_nanos(frame_ns),
        },
        obs: MomentumObservation::Schedule {
            is_active: false,
            settling: deadline.is_some(),
            cold_at: Some(9_000_000_000),
            deadline,
        },
    }
}

/// A hand-made log: an earlier burst, then a phase of two keys, repeated and changed frames, settle answers and a clearing tick, then a capture's tick after it.
fn log() -> Vec<Step> {
    vec![
        key(0, 0),
        tick(1, 10, 7, (1, 0)),
        tick(2, 20, 0, (1, 0)),
        key(3, 100),
        tick(4, 110, 5, (1, 1)),
        key(5, 160),
        tick(6, 170, 6, (1, 2)),
        tick(7, 180, 6, (1, 2)),
        query(8, 180, Some(230)),
        query(9, 200, Some(240)),
        tick(10, 243, 4, (1, 2)),
        query(11, 243, None),
        query(12, 250, Some(300)),
        tick(13, 290, 4, (1, 2)),
        query(14, 290, Some(320)),
        tick(15, 321, 0, (1, 2)),
        tick(16, 900, 0, (1, 2)),
    ]
}

/// A phase counts `T`, `P` and `Z` as the lane counts them, and pairs each settle answer with the next tick.
#[test]
fn a_phase_is_counted_as_the_lane_counts_and_its_answers_are_paired_with_the_next_tick() {
    let p = phase(&log(), 1).expect("a phase after one key");
    assert_eq!((p.first, p.last, p.cleared), (3, 15, true));
    assert_eq!((p.from_ns, p.to_ns), (100_000_000, 321_000_000));
    assert_eq!(
        (
            p.counts.keys,
            p.counts.ticks,
            p.counts.presented,
            p.counts.queries
        ),
        (2, 6, 4, 5)
    );
    assert_eq!(p.counts.wasted(), 2);
    assert_eq!(
        (p.ticks_after_keys, p.settle_wakes, p.refused_ticks),
        (5, 2, 0)
    );
    let ms = |v: &[i128]| v.iter().map(|ns| ns / 1_000_000).collect::<Vec<_>>();
    assert_eq!(ms(&p.rounding), [63 - 50, 3, -10, 1]);
    assert_eq!(ms(&p.earliest), [13, -10, 1]);
    assert_eq!(
        p.early,
        EarlyTicks {
            after_key: 0,
            moved: 0,
            other: 1,
            locked: 0
        }
    );
    assert_eq!(ms(&p.after_early), [21]);
    assert!(p.floor_period.is_empty() && p.query_lag.is_empty());
    assert_eq!(
        p.run,
        Run {
            ticks: 1,
            from_ns: 170_000_000,
            to_ns: 170_000_000
        }
    );
    assert_eq!(p.run.mean_period(), None);
    assert_eq!(p.frame_intervals, [(16_666_667, 5)]);
    assert_eq!(p.frame_interval(), Some(16_666_667));
    assert_eq!(p.unanswered, 0);

    // From the first key, the earlier burst's clearing tick presents (hot to inert).
    let all = phase(&log(), 0).expect("a phase from the first key");
    assert_eq!(
        (all.first, all.counts.ticks, all.counts.presented),
        (0, 8, 6)
    );
    assert_eq!(phase(&log(), 3), None);
    // A glow that never clears runs to the log's last input, its last answer unanswered.
    let mut open = log();
    open.truncate(15);
    let p = phase(&open, 1).expect("a phase");
    assert_eq!((p.last, p.cleared, p.unanswered), (14, false, 1));
}

/// A floor wake's period holds the query's lag, which the slot rounding `t − d` cannot see.
#[test]
fn a_floor_wakes_period_holds_the_querys_lag_which_the_rounding_cannot_see() {
    const F: u64 = 33_333_332;
    let f = i128::from(F);
    let (lag, late) = (1_500_000, 1_100_000);
    let mut steps = vec![key(0, 0)];
    let mut t = 100_000;
    steps.push(tick_ns(1, t, 1, (1, 1)));
    for k in 0..10u64 {
        let q = t + lag;
        steps.push(query_ns(2 + 2 * k, q, F, Some(q + f)));
        t = q + f + late;
        steps.push(tick_ns(3 + 2 * k, t, 2 + k, (1, 1)));
    }
    let q = t + lag;
    steps.push(query_ns(22, q, F, Some(q + 100_000_000)));
    steps.push(tick_ns(23, q + 100_000_000 + late, 0, (1, 1)));
    let p = phase(&steps, 0).expect("a phase");
    assert!(p.cleared);
    assert_eq!((p.counts.ticks, p.counts.presented), (12, 12));
    assert_eq!(p.rounding, vec![late; 11]);
    assert_eq!(p.earliest, p.rounding);
    assert_eq!(p.settle_wakes, 11);
    assert_eq!(p.early.total(), 0);
    assert_eq!(p.floor_period, vec![lag + f + late; 10]);
    assert_eq!(p.query_lag, vec![lag; 10]);
    assert_eq!(p.run.ticks, 11);
    assert_eq!(p.run.mean_period(), Some(lag + f + late));
    assert_eq!(p.frame_intervals, [(u128::from(F), 11)]);
    let lines = measured_lines(&p, None);
    for want in [
        "rounding all n=11 before_d=0 min_ms=1.100 p50_ms=1.100 p90_ms=1.100 max_ms=1.100 \
         unanswered=0",
        "period floor n=10 min_ms=35.933 p50_ms=35.933 p90_ms=35.933 max_ms=35.933 \
         frame_ms=33.333",
        "query_lag floor n=10 min_ms=1.500 p50_ms=1.500 p90_ms=1.500 max_ms=1.500",
        "run ticks=11 span_ms=359.333 mean_period_ms=35.933 frame_ms=33.333",
        "frame_interval 33333332ns=11",
    ] {
        assert!(lines.iter().any(|l| l == want), "{want}\n{lines:#?}");
    }
    assert!(lines[0].starts_with("phase T=12 P=12 Z=0 "), "{}", lines[0]);
    assert!(
        lines[0].ends_with(" settle_wakes=11 early_ticks=0 cleared=true span_ms=462.033"),
        "{}",
        lines[0]
    );
    assert!(measured_lines(&p, Some(12))[0].ends_with(" presents=12"));
}

/// A locked floor wake, an early tick one frame after the tick before with its floor answer pending, keeps the frame period and comes the query's lag before that answer, counted early but not `other` and measured by no `after_early`; a key or a caret move since the tick before takes precedence over it; a trace of floor wakes armed at the query's floor reads none.
#[test]
fn a_locked_floor_wake_keeps_the_frame_period() {
    const F: u64 = 33_333_332;
    let f = i128::from(F);
    let (lag, late) = (1_500_000, 1_160_000);
    let trace = |locked: bool, moved_at: Option<u64>, key_at: Option<u64>| {
        let mut steps = vec![key(0, 0)];
        let mut t = 100_000;
        steps.push(tick_ns(1, t, 1, (1, 1)));
        for k in 0..10u64 {
            let q = t + lag;
            steps.push(query_ns(2 + 3 * k, q, F, Some(q + f)));
            if key_at == Some(k) {
                steps.push(key(3 + 3 * k, q / 1_000_000));
            }
            t = if locked { t + f } else { q + f + late };
            let cur = if moved_at.is_some_and(|m| k >= m) {
                (1, 2)
            } else {
                (1, 1)
            };
            steps.push(tick_ns(4 + 3 * k, t, 2 + k, cur));
        }
        let q = t + lag;
        steps.push(query_ns(32, q, F, Some(q + 100_000_000)));
        steps.push(tick_ns(33, q + 100_000_000 + late, 0, (1, 1)));
        phase(&steps, 0).expect("a phase")
    };
    let p = trace(true, None, None);
    assert!(p.cleared);
    assert_eq!((p.counts.ticks, p.counts.presented), (12, 12));
    assert_eq!(
        p.early,
        EarlyTicks {
            after_key: 0,
            moved: 0,
            other: 0,
            locked: 10
        }
    );
    assert_eq!(p.early.total(), 10);
    assert_eq!(p.period_locked, vec![f; 10]);
    assert_eq!(p.ahead_locked, vec![lag; 10]);
    assert!(p.after_early.is_empty(), "a locked tick takes no deadline");
    assert!(p.floor_period.is_empty() && p.query_lag.is_empty());
    assert_eq!(p.settle_wakes, 1);
    assert_eq!(p.run.ticks, 11);
    assert_eq!(p.run.mean_period(), Some(f));
    let lines = measured_lines(&p, None);
    for want in [
        "early_ticks after_key=0 moved=0 other=0",
        "period floor n=0 frame_ms=33.333",
        "period locked n=10 min_ms=33.333 p50_ms=33.333 p90_ms=33.333 max_ms=33.333 \
         frame_ms=33.333",
        "ahead locked n=10 min_ms=1.500 p50_ms=1.500 p90_ms=1.500 max_ms=1.500",
        "run ticks=11 span_ms=333.333 mean_period_ms=33.333 frame_ms=33.333",
    ] {
        assert!(lines.iter().any(|l| l == want), "{want}\n{lines:#?}");
    }
    assert!(
        lines[0].contains(" settle_wakes=1 early_ticks=10 cleared=true "),
        "{}",
        lines[0]
    );

    let moved = trace(true, Some(4), None);
    assert_eq!((moved.early.moved, moved.early.locked), (1, 9));
    assert_eq!(
        moved.after_early.len(),
        1,
        "a moved tick takes its deadline"
    );

    let keyed = trace(true, None, Some(4));
    assert_eq!((keyed.early.after_key, keyed.early.locked), (1, 9));
    assert_eq!(
        keyed.after_early.len(),
        1,
        "a keyed tick takes its deadline"
    );
    assert_eq!(keyed.period_locked.len(), 9);

    let unlocked = trace(false, None, None);
    assert_eq!(unlocked.early.total(), 0);
    assert_eq!(unlocked.floor_period, vec![lag + f + late; 10]);
    let lines = measured_lines(&unlocked, None);
    for want in ["period locked n=0 frame_ms=33.333", "ahead locked n=0"] {
        assert!(lines.iter().any(|l| l == want), "{want}\n{lines:#?}");
    }
}

/// A redraw within half a frame of the tick before neither counts in the run nor ends it, unless the phase carried no frame interval.
#[test]
fn a_redraw_within_half_a_frame_neither_counts_in_the_run_nor_ends_it() {
    const F: u64 = 33_333_332;
    let f = i128::from(F);
    let t0 = 500_000;
    let mut steps = vec![key(0, 0), tick_ns(1, t0, 1, (1, 1))];
    let mut t = t0;
    for k in 1..=4u64 {
        t = t0 + i128::from(k) * f;
        steps.push(tick_ns(1 + k, t, 1 + k, (1, 1)));
    }
    steps.push(tick_ns(6, t + 2_500_000, 5, (1, 1)));
    let q = t + 4_000_000;
    steps.push(query_ns(7, q, F, Some(q + f)));
    t = q + f + 1_200_000;
    let resumed = t;
    steps.push(tick_ns(8, t, 6, (1, 1)));
    for (cursor, fp) in [(9, 7), (10, 8)] {
        t += f;
        steps.push(tick_ns(cursor, t, fp, (1, 1)));
    }
    let last = t;
    steps.push(tick_ns(11, t + f, 8, (1, 1)));
    let q = t + f + 1_000_000;
    steps.push(query_ns(12, q, F, Some(q + 200_000_000)));
    steps.push(tick_ns(13, q + 201_000_000, 0, (1, 1)));
    let p = phase(&steps, 0).expect("a phase");
    assert_eq!((p.counts.ticks, p.counts.presented), (11, 9));
    assert_eq!(
        p.run,
        Run {
            ticks: 8,
            from_ns: t0,
            to_ns: last
        }
    );
    assert_eq!(p.run.mean_period(), Some((last - t0) / 7));
    assert_eq!(p.floor_period, [resumed - (t0 + 4 * f + 2_500_000)]);

    let mut no_query = steps.clone();
    no_query.retain(|s| !matches!(s.input, MomentumEvent::Query { .. }));
    let p = phase(&no_query, 0).expect("a phase");
    assert_eq!(p.frame_interval(), None);
    assert_eq!(p.run.ticks, 5);
}

/// Early ticks are counted by cause, and the tick after each is measured against the deadline it took.
#[test]
fn early_ticks_are_counted_by_cause_and_the_tick_after_each_is_measured() {
    let steps = vec![
        key(0, 0),
        tick(1, 1, 5, (0, 0)),
        query(2, 2, Some(40)),
        key(3, 20),
        tick(4, 20, 6, (0, 1)),
        query(5, 21, Some(60)),
        tick(6, 30, 7, (1, 0)),
        query(7, 31, Some(70)),
        tick(8, 35, 7, (1, 0)),
        tick(9, 71, 0, (1, 0)),
    ];
    let p = phase(&steps, 0).expect("a phase");
    assert_eq!(
        p.early,
        EarlyTicks {
            after_key: 1,
            moved: 1,
            other: 1,
            locked: 0
        }
    );
    assert_eq!(p.early.total(), 3);
    assert_eq!(p.settle_wakes, 0);
    let ms = |v: &[i128]| v.iter().map(|ns| ns / 1_000_000).collect::<Vec<_>>();
    assert_eq!(ms(&p.earliest), [-20, -30, -35]);
    assert_eq!(ms(&p.after_early), [-10, -25, 1]);
    assert_eq!(
        measured_lines(&p, None)[1],
        "early_ticks after_key=1 moved=1 other=1"
    );
}

/// The frame on glass before a phase is the last tick before it, so a first tick that repeats it is not presented.
#[test]
fn the_frame_on_glass_before_a_phase_is_the_last_tick_before_it() {
    let steps = vec![
        key(0, 0),
        tick(1, 10, 9, (0, 0)),
        key(2, 20),
        tick(3, 30, 9, (0, 0)),
        tick(4, 40, 0, (0, 0)),
    ];
    let p = phase(&steps, 1).expect("a phase");
    assert_eq!((p.counts.ticks, p.counts.presented), (2, 1));
}

/// The spread is nearest rank over the sorted values and counts those before `d`, and `ms` prints three decimals.
#[test]
fn the_rounding_spread_is_nearest_rank_and_counts_the_early_ticks() {
    assert_eq!(Spread::of(&[]), None);
    let s = Spread::of(&[5, -2, 9, 1, 3, 7, 0, 8, 4, 6]).expect("ten values");
    assert_eq!(
        s,
        Spread {
            n: 10,
            before: 1,
            min: -2,
            p50: 4,
            p90: 8,
            max: 9
        }
    );
    let one = Spread::of(&[1_234_567]).expect("one value");
    assert_eq!((one.p50, one.p90), (1_234_567, 1_234_567));
    assert_eq!(
        one.words(),
        "n=1 before_d=0 min_ms=1.234 p50_ms=1.234 p90_ms=1.234 max_ms=1.234"
    );
    assert_eq!(ms(-2_500_000), "-2.500");
    assert_eq!(ms(999), "0.000");
    assert_eq!(ms(16_666_667), "16.666");
}

/// The ticks around a move are the last before it, the first after it, and the first that carries the new cell.
#[test]
fn the_ticks_around_a_move_name_the_cells_they_carry() {
    let steps = log();
    let m = move_ticks(&steps, 175_000_000, (1, 2));
    assert_eq!(m.before, Some((170_000_000, Some((1, 2)))));
    assert_eq!(m.next, Some((180_000_000, Some((1, 2)))));
    assert_eq!(m.at_cell, Some(180_000_000));
    let m = move_ticks(&steps, 100_000_000, (1, 2));
    assert_eq!(m.next, Some((110_000_000, Some((1, 1)))));
    assert_eq!(m.at_cell, Some(170_000_000));
    assert_eq!(move_ticks(&steps, 1_000_000_000, (1, 2)).next, None);
}

/// The first tick from a main-loop read on proves that the first present after the read showed a moved caret when it carries the moved cell; a first tick on the old cell, or none, proves nothing.
#[test]
fn a_first_tick_on_the_moved_cell_proves_the_present_after_the_read_showed_it() {
    let steps = log();
    assert!(first_present_carries(&steps, 115_000_000, (1, 2)));
    assert!(
        !first_present_carries(&steps, 110_000_000, (1, 2)),
        "a tick at the read's instant counts as after it"
    );
    assert!(
        !first_present_carries(&steps, 100_000_000, (1, 2)),
        "the tick at 110 ms, still on (1, 1), came after the read"
    );
    assert!(
        !first_present_carries(&steps, 1_000_000_000, (1, 2)),
        "no tick after it"
    );
}

/// The lane's slot rounding, in order: `t − w` for the tick after each tick that armed a settle wake at `w`.
fn lane_rounding(run: &LaneRun) -> Vec<i128> {
    let ticks = run.ticks();
    ticks
        .iter()
        .zip(ticks.iter().skip(1))
        .filter_map(|(t, next)| match t.after {
            After::Wake { wake: Some(w), .. } => Some(i128::from(next.at) - i128::from(w)),
            _ => None,
        })
        .collect()
}

/// `ev` with its time moved `by` ns later: a lane's call (ns from the lane's origin) as the log records it (ns from the attach epoch).
fn later(ev: MomentumEvent, by: i128) -> MomentumEvent {
    match ev {
        MomentumEvent::Key { at, tau_bits, echo } => MomentumEvent::Key {
            at: at + by,
            tau_bits,
            echo,
        },
        MomentumEvent::Tick { cur, at, geom, cfg } => MomentumEvent::Tick {
            cur,
            at: at + by,
            geom,
            cfg,
        },
        MomentumEvent::Query { at, frame_interval } => MomentumEvent::Query {
            at: at + by,
            frame_interval,
        },
        MomentumEvent::Reset => MomentumEvent::Reset,
    }
}

/// Asserts the trace `file` of `run` (started `lane_ns` after the attach epoch) replays to its head and counts as the lane counted it.
fn assert_counts_as_the_lane(
    run: &LaneRun,
    lane_ns: i128,
    file: &[u8],
    plan: Option<SchedulePlan>,
    why: &str,
) {
    let replayed = replay_trace(file, TEST_BUILD, plan).unwrap_or_else(|e| panic!("{why}: {e}"));
    assert_eq!(
        replayed.steps.iter().map(|s| s.input).collect::<Vec<_>>(),
        run.calls()
            .iter()
            .map(|&c| later(c, lane_ns))
            .collect::<Vec<_>>(),
        "{why}: the log is the lane's calls"
    );
    let p = phase(&replayed.steps, 0).expect("the run typed");
    let c = run.counts();
    assert!(p.cleared, "{why}: the run cleared");
    assert_eq!(
        (
            p.counts.keys,
            p.counts.ticks,
            p.counts.presented,
            p.counts.wasted()
        ),
        (c.keys, c.ticks, c.presented, c.wasted()),
        "{why}: T, P and Z"
    );
    assert_eq!(p.counts.queries, c.queries, "{why}: queries");
    assert_eq!(p.counts.quads, c.quads, "{why}: quads");
    assert!(
        !p.rounding.is_empty(),
        "{why}: the lane woke a settling glow"
    );
    assert_eq!(
        p.rounding,
        lane_rounding(run),
        "{why}: each answer, the tick it woke"
    );
    assert_eq!(p.earliest, p.rounding, "{why}: the lane asks once per tick");
    assert!(
        p.rounding.iter().all(|&r| r <= 0),
        "{why}: a lane wakes at d, or an input beats it"
    );
    assert_eq!(p.early.locked, 0, "{why}: the lane cannot lock");
    assert!(
        p.period_locked.is_empty() && p.ahead_locked.is_empty(),
        "{why}: no locked tick is measured"
    );
}

/// A shipped trace replays to its head and counts as the lane counted, and a trace of another build or a cut file is refused.
#[test]
fn a_shipped_trace_replays_to_its_head_and_counts_as_the_lane_counted() {
    for seed in [6, 1] {
        let sc = scenario(seed).expect("a corpus scenario");
        let origin = Instant::now();
        let mut host = MomentumHost::default();
        host.aos_attach(origin, MomentumCode::Shipped, TEST_BUILD, 1);
        let run = lane::run(&mut host, &sc, origin);
        assert_eq!(run.fault(), None, "seed {seed}");
        assert!(
            run.calls().len() < MOMENTUM_HOST_MAX_EVENTS,
            "fixture: one log epoch"
        );
        let file = host.aos_trace().expect("attached").encode();
        assert_counts_as_the_lane(&run, 0, &file, None, &format!("seed {seed}"));
        let other = replay_trace(&file, "another build", None).expect_err("another build");
        assert!(
            other.contains("is none of build `another build`'s prebuilt codes"),
            "{other}"
        );
        let cut = replay_trace(&file[..file.len() - 1], TEST_BUILD, None).expect_err("cut");
        assert!(cut.starts_with("the trace file: "), "{cut}");
    }
}

/// A published plan's trace replays only when the run names that plan, and counts as the lane counted.
#[test]
fn a_published_plans_trace_replays_with_its_plan_and_counts_as_the_lane_counted() {
    let origin = Instant::now();
    let ev = Evaluator::v1(TEST_BUILD, origin).expect("the pinned policy");
    let winner =
        SchedulePlan::new(Split::SettleAtStep { gap_us: 0 }, Wake::NextStep).expect("in range");
    let admitted = ev
        .admit_one(&winner.encode())
        .expect("the winner is admitted");
    let mut host = MomentumHost::default();
    host.aos_attach(origin, MomentumCode::Shipped, TEST_BUILD, 1);
    host.aos_pause(origin, Duration::from_nanos(16_666_667))
        .expect("a running glow pauses");
    host.aos_publish(admitted).expect("the winner publishes");
    let sc = scenario(1).expect("a corpus scenario");
    let run = lane::run(&mut host, &sc, origin + Duration::from_secs(1));
    assert_eq!(run.fault(), None);
    let file = host.aos_trace().expect("attached").encode();
    assert_counts_as_the_lane(
        &run,
        1_000_000_000,
        &file,
        Some(winner),
        "the published plan",
    );
    let unnamed = replay_trace(&file, TEST_BUILD, None).expect_err("no plan named");
    assert!(unnamed.contains("is none of build"), "{unnamed}");
    let cadence = SchedulePlan::new(Split::Shipped, Wake::Cadence).expect("in range");
    let other = replay_trace(&file, TEST_BUILD, Some(cadence)).expect_err("another plan");
    assert!(other.contains("nor the plan the run published"), "{other}");
}

/// `aos-phase` takes one trace, the window's build and the plan a publish run published; anything else is a usage error.
#[test]
fn aos_phase_takes_one_trace_the_windows_build_and_its_plan() {
    let words = |a: &[&str]| a.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    let usage = |a: &[&str]| {
        let f = parse_phase_args(&words(a)).expect_err("a usage error");
        assert_eq!(f.code, 2, "{:?}", f.msg);
        f.msg.unwrap_or_default()
    };
    assert_eq!(
        parse_phase_args(&words(&["t.trace", "--build", "0.1.0+gabc"])).expect("parses"),
        PhaseArgs {
            trace: "t.trace".into(),
            build: "0.1.0+gabc".into(),
            plan: None
        }
    );
    let winner =
        SchedulePlan::new(Split::SettleAtStep { gap_us: 0 }, Wake::NextStep).expect("in range");
    assert_eq!(
        parse_phase_args(&words(&[
            "--plan",
            "010100000000000000000000",
            "--build",
            "b",
            "t.trace"
        ]))
        .expect("parses")
        .plan,
        Some(winner)
    );
    assert!(usage(&["t.trace"]).contains("--build BUILD is required"));
    assert!(usage(&["--build", "b"]).contains("name one TRACE"));
    assert!(usage(&["a", "b", "--build", "b"]).contains("unexpected 'a b'"));
    assert!(usage(&["t", "--keep", "--build", "b"]).contains("unexpected"));
    assert!(usage(&["t", "--build"]).contains("--build needs a value"));
    assert!(usage(&["t", "--build", "b", "--plan", "zz"]).contains("24 hex digits"));
    assert!(
        usage(&["t", "--build", "b", "--plan", "020100000000000000000000"])
            .starts_with("aos-phase: --plan 020100000000000000000000: ")
    );
}

/// `aos-phase` measures a kept trace as the drive does, a trace of another build exits 1, and a missing file is usage.
#[test]
fn a_kept_trace_is_measured_as_the_drive_measures_it() {
    let sc = scenario(5).expect("a corpus scenario of 24 keys");
    let origin = Instant::now();
    let mut host = MomentumHost::default();
    host.aos_attach(origin, MomentumCode::Shipped, TEST_BUILD, 1);
    let run = lane::run(&mut host, &sc, origin);
    assert_eq!(run.fault(), None);
    let file = host.aos_trace().expect("attached").encode();
    let replayed = replay_trace(&file, TEST_BUILD, None).expect("replays");
    let keys = keys_in(&replayed.steps);
    assert!(keys >= phase_keys(), "fixture: {keys} keys");
    let p = phase(&replayed.steps, keys - phase_keys()).expect("a phase");
    let a = PhaseArgs {
        trace: "t.trace".into(),
        build: TEST_BUILD.into(),
        plan: None,
    };
    let mut want = format!(
        "aos-phase file=t.trace bytes={} build={} plan=none\nMEASURED {}\n",
        file.len(),
        word(TEST_BUILD),
        replay_line(&replayed, &p)
    );
    for line in measured_lines(&p, None) {
        want.push_str(&format!("MEASURED {line}\n"));
    }
    want.push_str(&format!("aos-phase result=pass cleared={}\n", p.cleared));
    assert_eq!(phase_report(&file, "t.trace", &a).expect("measured"), want);
    assert!(!want.contains("presents="));
    assert_eq!(p.counts.keys, 12);

    let other = PhaseArgs {
        build: "another build".into(),
        ..a.clone()
    };
    let why = phase_report(&file, "t.trace", &other).expect_err("another build");
    assert!(why.contains("is none of build `another build`"), "{why}");

    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let path = dir.path().join("kept.fresh.trace");
    std::fs::write(&path, &file).expect("written");
    let path = path.display().to_string();
    let args = |build: &str| {
        [path.as_str(), "--build", build]
            .iter()
            .map(|w| (*w).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(cmd_aos_phase(&args(TEST_BUILD)).expect("measured"), 0);
    let f = cmd_aos_phase(&args("another build")).expect_err("no replay");
    assert_eq!(f.code, 1, "{:?}", f.msg);
    let missing = dir.path().join("none.trace").display().to_string();
    let f = cmd_aos_phase(&[missing, "--build".into(), TEST_BUILD.into()]).expect_err("unreadable");
    assert_eq!(f.code, 2, "{:?}", f.msg);
}
