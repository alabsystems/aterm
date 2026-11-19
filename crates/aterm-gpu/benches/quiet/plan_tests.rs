// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_sysprobe::self_cpu::Queue;
use aterm_sysprobe::{GpuClient, GpuClients};
use aterm_time::Duration;

use crate::quiet_plan::*;
use crate::{QuietArm, QuietKeep, QuietTake};

/// A repetition holds the four blocks at their sizes (40, 4, 6, 2) and the recipe's shapes.
/// Block 3 holds every level at 500 ms and the other process at 100 ms too. The negative
/// control is block 3's (level, period) pairs: a plan that dropped one (or doubled one) would
/// leave the six distinct pairs short.
#[test]
fn quiet_plan_repetition_has_every_block() {
    let rep = repetition();
    let count = |b: u8| rep.iter().filter(|(block, _)| *block == b).count();
    assert_eq!((count(1), count(2), count(3), count(4)), (40, 4, 6, 2));
    assert_eq!(rep.len(), 52);
    for arm in CURVE_ARMS {
        for q in CURVE_QUIETS_MS {
            assert!(rep.contains(&(1, QuietTake::bare(arm, q))), "{arm:?} {q}");
        }
    }
    let keeps: std::collections::BTreeSet<(&str, Option<u128>)> = rep
        .iter()
        .filter(|(b, _)| *b == 3)
        .map(|(_, t)| (t.keep_name(), t.keep.map(|k| k.period().as_millis())))
        .collect();
    assert_eq!(
        keeps.len(),
        6,
        "negative control: each keep level and period once: {keeps:?}"
    );
    let at_500: Vec<&str> = keeps
        .iter()
        .filter(|(_, p)| *p == Some(500))
        .map(|(k, _)| *k)
        .collect();
    assert_eq!(
        at_500,
        [
            "other_process_dot",
            "other_queue_compute",
            "other_queue_dot",
            "same_queue_dot"
        ],
        "every level at the one period"
    );
    assert!(keeps.contains(&("other_process_dot", Some(100))));
    assert!(
        rep.iter()
            .filter(|(b, _)| *b >= 3)
            .all(|(_, t)| t.arm == QuietArm::P && t.quiet.as_millis() == 5_000)
    );
    let ballasts: Vec<u32> = rep
        .iter()
        .filter(|(b, _)| *b == 4)
        .map(|(_, t)| t.ballast_mib)
        .collect();
    assert_eq!(ballasts, [0, 512]);
    let quiet_s: f64 = rep.iter().map(|(_, t)| t.quiet.as_secs_f64()).sum();
    assert!(
        (quiet_s - 167.0).abs() < 1e-9,
        "one repetition's quiets: 5x23 + 2x6 + 6x5 + 2x5 s, got {quiet_s}"
    );
}

/// Each repetition of the plan is the repetition's multiset in its own order, and the order
/// is the xorshift stream from the seed given. The negative control is the unshuffled order: a
/// shuffle that left it alone (or one repetition's order reused for the next) fails here, and
/// another seed gives another order.
#[test]
fn quiet_plan_shuffles_each_repetition_with_the_xorshift() {
    let base = repetition();
    let planned = plan(3, SEED, |_| true);
    assert_eq!(planned.len(), 3 * base.len());
    let reps: Vec<Vec<(u8, QuietTake)>> = (0..3)
        .map(|r| {
            planned
                .iter()
                .filter(|p| p.rep == r)
                .map(|p| (p.block, p.take))
                .collect()
        })
        .collect();
    let sorted = |mut v: Vec<(u8, QuietTake)>| {
        v.sort_by_key(|(b, t)| {
            (
                *b,
                t.arm,
                t.quiet,
                t.keep_name(),
                t.keep.map(QuietKeep::period),
                t.ballast_mib,
            )
        });
        v
    };
    for r in &reps {
        assert_eq!(sorted(r.clone()), sorted(base.clone()), "same takes");
    }
    assert_ne!(reps[0], base, "negative control: the order moved");
    assert_ne!(reps[0], reps[1], "each repetition has its own order");
    let mut seed = SEED;
    let mut again = base.clone();
    shuffle(&mut again, &mut seed);
    assert_eq!(again, reps[0], "the plan is the xorshift's first shuffle");
    let other: Vec<(u8, QuietTake)> = plan(1, SEED ^ 1, |_| true)
        .into_iter()
        .map(|p| (p.block, p.take))
        .collect();
    assert_ne!(other, reps[0], "another seed, another order");
    let mut first = [1, 2, 3, 4, 5];
    let mut s = SEED;
    shuffle(&mut first, &mut s);
    assert_ne!(s, SEED, "the seed advances");
}

/// The admit filter drops takes before the shuffle (as `QUIET_COMMIT_ARMS` and a build without
/// `compute` do), and the control is the full plan, which keeps them.
#[test]
fn quiet_plan_admit_drops_takes_before_the_shuffle() {
    let no_compute = plan(2, SEED, |t| {
        t.arm != QuietArm::Compute && !matches!(t.keep, Some(QuietKeep::OtherQueueCompute(_)))
    });
    assert_eq!(no_compute.len(), 2 * (52 - 2 - 1));
    assert!(no_compute.iter().all(|p| p.take.arm != QuietArm::Compute));
    assert_eq!(plan(2, SEED, |_| true).len(), 104, "control");
    let only_p = plan(1, SEED, |t| t.arm == QuietArm::P);
    assert_eq!(only_p.len(), 8 + 6 + 2);
}

/// `alternate` gives even repetitions the quiet-end placement and odd ones the completed one;
/// a fixed mode gives every repetition its own. The negative control is an unknown word, which
/// names no mode.
#[test]
fn quiet_plan_placement_modes() {
    let alternate = PlacementMode::parse("alternate").expect("a mode");
    assert_eq!(
        (0..4).map(|r| alternate.of(r)).collect::<Vec<_>>(),
        [
            Placement::QuietEnd,
            Placement::Completed,
            Placement::QuietEnd,
            Placement::Completed
        ]
    );
    let completed = PlacementMode::parse("completed").expect("a mode");
    assert!((0..4).all(|r| completed.of(r) == Placement::Completed));
    assert_eq!(completed.name(), "completed");
    assert_eq!(
        PlacementMode::parse("quiet_end"),
        Some(PlacementMode::Every(Placement::QuietEnd))
    );
    assert_eq!(PlacementMode::parse("after"), None, "negative control");
}

fn sample(id: u64, role: ThreadRole, cpu: u64) -> ThreadSample {
    ThreadSample {
        id,
        role,
        name: None,
        queue: None,
        cpu_ns: Some(cpu),
        user_ns: Some(cpu / 4),
        system_ns: Some(cpu - cpu / 4),
        instructions: Some(cpu * 2),
        cycles: Some(cpu * 3),
        energy_nj: Some(cpu / 10),
    }
}

/// The window's end, ns: the last cut must come at or after it.
const END_NS: u64 = 41_000;

/// Other-thread CPU is the process's growth less the caller's and the sampler's, so a
/// workqueue thread that exited inside the window still counts. The negative control is the
/// live-thread sum, which misses it: the two differ by exactly the exited thread's 400 ns.
#[test]
fn quiet_plan_window_counts_threads_that_exited() {
    let first = WindowCut {
        at_ns: 1_000,
        process_ns: 10_000,
        complete: true,
        threads: vec![
            sample(1, ThreadRole::Caller, 5_000),
            sample(2, ThreadRole::Sampler, 1_000),
            sample(3, ThreadRole::Other, 2_000),
            sample(4, ThreadRole::Other, 2_000),
        ],
    };
    let mid = WindowCut {
        at_ns: 3_000,
        process_ns: 12_000,
        complete: true,
        threads: vec![
            sample(1, ThreadRole::Caller, 5_500),
            sample(3, ThreadRole::Other, 2_000),
            sample(5, ThreadRole::Other, 100),
        ],
    };
    // The process grew by every thread's growth: the caller's 1000, the sampler's 200,
    // thread 3's 3000, thread 5's 500, and the 400 thread 4 ran before it exited.
    let end = WindowCut {
        at_ns: END_NS,
        process_ns: 15_100,
        complete: true,
        threads: vec![
            sample(1, ThreadRole::Caller, 6_000),
            sample(2, ThreadRole::Sampler, 1_200),
            sample(3, ThreadRole::Other, 5_000),
            sample(5, ThreadRole::Other, 500),
        ],
    };
    let w = window_cpu(&first, &[mid, end], END_NS).expect("a window that reached its end");
    assert_eq!(w.caller_ns, Some(1_000));
    assert_eq!(
        (w.caller_user_ns, w.caller_system_ns),
        (
            Some(6_000 / 4 - 5_000 / 4),
            Some(1_000 - (6_000 / 4 - 5_000 / 4))
        ),
        "the caller's split is its own record's"
    );
    assert_eq!(w.other_ns, Some(5_100 - 1_000 - 200));
    let live_sum = 3_000 + 500;
    assert_eq!(
        w.other_ns.unwrap() - live_sum,
        400,
        "negative control: the live sum misses thread 4's exit"
    );
    assert_eq!(
        w.other_system_ns,
        (5_000 - 5_000 / 4) - (2_000 - 2_000 / 4) + (500 - 500 / 4)
    );
    assert_eq!(w.other_user_ns, (5_000 / 4 - 2_000 / 4) + 500 / 4);
    assert_eq!(w.other_instructions, 2 * 3_000 + 2 * 500);
    assert_eq!(
        w.burst,
        Some((3, 3_000, 40_000)),
        "thread 3 first moved at the end cut"
    );
    assert_eq!((w.unread, w.counts_unread), (0, 0));
    let rows: Vec<(u64, u64)> = w.threads.iter().map(|t| (t.id, t.cpu_ns)).collect();
    assert_eq!(
        rows,
        [(1, 1_000), (2, 200), (3, 3_000), (5, 500)],
        "every thread that grew has a row, the caller and sampler among them"
    );
    assert_eq!(w.row(5).map(|r| r.moved_ns), Some(2_000));
}

/// A window the sampler did not finish is no window: an empty series, or one whose last cut
/// came before the window's end (its last read failed), is `None`, never a window of zero
/// growth. The control is the same series with its end cut at the window's end, which reads.
#[test]
fn quiet_plan_window_not_read_is_none() {
    let first = WindowCut {
        at_ns: 0,
        process_ns: 0,
        complete: true,
        threads: vec![
            sample(1, ThreadRole::Caller, 0),
            sample(2, ThreadRole::Sampler, 0),
        ],
    };
    let cut = |at_ns| WindowCut {
        at_ns,
        process_ns: 300,
        complete: true,
        threads: vec![
            sample(1, ThreadRole::Caller, 100),
            sample(2, ThreadRole::Sampler, 100),
        ],
    };
    assert_eq!(window_cpu(&first, &[], END_NS), None, "no cut");
    assert_eq!(
        window_cpu(&first, &[cut(1_000), cut(END_NS - 1)], END_NS),
        None,
        "the last cut fell short of the end"
    );
    let read = window_cpu(&first, &[cut(1_000), cut(END_NS)], END_NS).expect("control");
    assert_eq!(read.other_ns, Some(100));
}

/// A thread first seen after an incomplete first cut may predate the window, so its counter
/// is not taken as growth; with a complete first cut it was born inside the window and counts
/// from zero (the control). A thread whose CPU grew with its counts missing is counted in
/// `counts_unread` and adds no counts.
#[test]
fn quiet_plan_window_does_not_invent_a_base() {
    let first = |complete| WindowCut {
        at_ns: 0,
        process_ns: 0,
        complete,
        threads: vec![
            sample(1, ThreadRole::Caller, 0),
            sample(2, ThreadRole::Sampler, 0),
        ],
    };
    let end = WindowCut {
        at_ns: END_NS,
        process_ns: 900,
        complete: true,
        threads: vec![
            sample(1, ThreadRole::Caller, 100),
            sample(2, ThreadRole::Sampler, 100),
            sample(9, ThreadRole::Other, 700),
        ],
    };
    let incomplete = window_cpu(&first(false), std::slice::from_ref(&end), END_NS).unwrap();
    assert_eq!(incomplete.unread, 1);
    assert_eq!(incomplete.burst, None);
    assert_eq!(
        incomplete.other_ns,
        Some(700),
        "the process total still counts it"
    );
    let complete = window_cpu(&first(true), std::slice::from_ref(&end), END_NS).unwrap();
    assert_eq!(complete.unread, 0);
    assert_eq!(complete.burst, Some((9, 700, END_NS)));
    let mut uncounted = end.clone();
    uncounted.threads[2].cycles = None;
    let partial = window_cpu(&first(true), &[uncounted], END_NS).unwrap();
    assert_eq!((partial.unread, partial.counts_unread), (0, 1));
    assert_eq!(partial.other_cycles, 0);
    assert_eq!(
        partial.other_instructions, 1_400,
        "the counts it has still add"
    );
    let missing = window_cpu(
        &WindowCut {
            threads: vec![sample(2, ThreadRole::Sampler, 0)],
            ..first(true)
        },
        &[end],
        END_NS,
    )
    .unwrap();
    assert_eq!(missing.other_ns, None, "no caller row, no subtraction");
}

/// Each thread's row names it and the queue it drained: the label read at the most cuts that
/// asked (the earliest on a tie), with the asked, labelled and unread counts. The negative
/// control is a thread no cut asked about: it has a row with no queue and no counts.
#[test]
fn quiet_plan_window_names_each_threads_queue() {
    let at = |id, cpu, queue: Option<Queue>| ThreadSample {
        name: Some(format!("t{id}")),
        queue,
        ..sample(id, ThreadRole::Other, cpu)
    };
    let label = |l: &str| Some(Queue::Label(l.to_owned()));
    let base = |cpu| WindowCut {
        at_ns: 0,
        process_ns: 0,
        complete: true,
        threads: vec![
            sample(1, ThreadRole::Caller, 0),
            sample(2, ThreadRole::Sampler, 0),
            at(3, cpu, None),
            at(4, 0, None),
        ],
    };
    let cut = |at_ns, q3: Option<Queue>, cpu3| WindowCut {
        at_ns,
        process_ns: 0,
        complete: true,
        threads: vec![
            sample(1, ThreadRole::Caller, 0),
            sample(2, ThreadRole::Sampler, 0),
            at(3, cpu3, q3),
            at(4, 900, None),
        ],
    };
    let series = [
        cut(1_000, label("com.apple.Metal.CompletionQueueDispatch"), 100),
        cut(2_000, label("com.apple.CoreAnimation"), 200),
        cut(3_000, Some(Queue::Unread), 300),
        cut(4_000, label("com.apple.CoreAnimation"), 400),
        cut(END_NS, Some(Queue::None), 500),
    ];
    let w = window_cpu(&base(0), &series, END_NS).unwrap();
    let three = w.row(3).expect("thread 3 grew");
    assert_eq!(three.name.as_deref(), Some("t3"));
    assert_eq!(three.queue.as_deref(), Some("com.apple.CoreAnimation"));
    assert_eq!(
        (three.queue_asked, three.queue_labelled, three.queue_unread),
        (5, 3, 1)
    );
    assert_eq!(three.moved_ns, 1_000);
    let four = w.row(4).expect("thread 4 grew");
    assert_eq!(
        (four.queue.as_deref(), four.queue_asked),
        (None, 0),
        "negative control: never asked, never named"
    );
    assert_eq!(w.burst.map(|b| b.0), Some(4), "thread 4 grew most");
    let tie = [
        cut(1_000, label("first"), 100),
        cut(END_NS, label("second"), 200),
    ];
    let w = window_cpu(&base(0), &tie, END_NS).unwrap();
    assert_eq!(w.row(3).and_then(|r| r.queue.as_deref()), Some("first"));
}

fn client(entry: u64, pid: u32, name: &str, gpu_ns: u64) -> GpuClient {
    GpuClient {
        entry,
        pid,
        name: name.to_owned(),
        gpu_ns,
    }
}

fn clients(rows: Vec<GpuClient>, complete: bool) -> GpuClients {
    GpuClients {
        entries: rows.iter().map(|r| r.entry).collect(),
        children: u32::try_from(rows.len()).unwrap(),
        rows,
        truncated: false,
        complete,
    }
}

/// Other processes' GPU time is each client's growth, this process's and its keeper's left
/// out: a client born after a complete first read grew by its whole counter, one that vanished
/// is counted gone. The negative control is the same reads with this process's pid not left
/// out, whose own 9 ms then counts as foreign.
#[test]
fn quiet_plan_foreign_gpu_time() {
    let start = clients(
        vec![
            client(10, 100, "self", 1_000_000),
            client(11, 200, "WindowServer", 5_000_000),
            client(12, 300, "keeper", 0),
            client(13, 400, "gone", 7),
        ],
        true,
    );
    let end = clients(
        vec![
            client(10, 100, "self", 10_000_000),
            client(11, 200, "WindowServer", 5_250_000),
            client(12, 300, "keeper", 4_000_000),
            client(14, 500, "newcomer", 60_000),
        ],
        true,
    );
    let f = foreign(&start, &end, &[100, 300]);
    assert_eq!(f.ns, 250_000 + 60_000);
    assert_eq!((f.pids, f.unread, f.gone), (2, 0, 1));
    assert_eq!(f.top, Some(("WindowServer".to_owned(), 250_000)));
    let without = foreign(&start, &end, &[300]);
    assert_eq!(
        without.ns - f.ns,
        9_000_000,
        "negative control: this process's own growth"
    );
    let partial = clients(start.rows.clone(), false);
    let f = foreign(&partial, &end, &[100, 300]);
    assert_eq!(
        (f.ns, f.unread),
        (250_000, 1),
        "a newcomer after an incomplete read may predate it"
    );
    let quiet = foreign(&start, &start, &[100, 300]);
    assert_eq!((quiet.ns, quiet.pids, quiet.top), (0, 0, None));
}

/// A two-step release is two steps, timed from the instant given and tagged with their phase;
/// a dip under the 4 MiB floor is none (the negative control: a detector without the floor
/// reports it), and a step before the instant is timed negative.
#[test]
fn quiet_plan_release_steps() {
    const MB: u64 = 1 << 20;
    let at = |ms: u64, mib: u64| VmPoint {
        at_ns: ms * 1_000_000,
        phys: mib * MB,
        gfx: Some(i64::try_from(mib * MB).unwrap() / 2),
    };
    let points = [
        at(1_000, 289),
        at(1_010, 288),
        at(1_250, 237),
        at(2_400, 189),
        at(2_410, 189),
    ];
    let steps = releases(Some(at(990, 289)), &points, 0, ReleasePhase::Quiet);
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].at_ms, 1_250.0);
    assert_eq!((steps[0].from_mib, steps[0].to_mib), (288.0, 237.0));
    assert_eq!(steps[1].gfx_to_mib, Some(94.5));
    assert!(steps.iter().all(|s| s.phase == ReleasePhase::Quiet));
    assert!(
        points
            .windows(2)
            .any(|w| w[0].phys > w[1].phys && w[0].phys - w[1].phys < RELEASE_STEP_MIN),
        "negative control: the series holds a sub-floor dip the detector ignored"
    );
    assert!(releases(None, &points[..2], 0, ReleasePhase::Quiet).is_empty());
    let early = releases(None, &points, 1_500_000_000, ReleasePhase::Teardown);
    assert_eq!(early[0].at_ms, -250.0);
    assert_eq!(early[0].phase, ReleasePhase::Teardown);
}

fn record(arm: QuietArm, quiet_ms: u64) -> TakeRecord {
    TakeRecord::new(0, 0, 1, QuietTake::bare(arm, quiet_ms))
}

/// The stretch between a test record's two reads of every GPU client, ns.
const SPAN_NS: u64 = 1_000_000_000;

/// A record whose window was read with `other_ns` and no foreign GPU time over a [`SPAN_NS`]
/// stretch.
fn read_record(arm: QuietArm, quiet_ms: u64, other_ns: u64) -> TakeRecord {
    TakeRecord {
        window: Some(WindowCpu {
            other_ns: Some(other_ns),
            ..WindowCpu::default()
        }),
        foreign: Some(Foreign::default()),
        foreign_span_ns: Some(SPAN_NS),
        ..record(arm, quiet_ms)
    }
}

/// `r` with other processes' GPU time set to `duty` of its [`SPAN_NS`] stretch.
fn at_duty(mut r: TakeRecord, duty: f64) -> TakeRecord {
    r.foreign = Some(Foreign {
        ns: (duty * SPAN_NS as f64).round() as u64,
        pids: usize::from(duty > 0.0),
        ..Foreign::default()
    });
    r
}

/// `r` with its graphics footprint rising by 178 MiB (a re-take) or by none.
fn at_state(mut r: TakeRecord, retake: bool) -> TakeRecord {
    r.gfx_fp = Some(50.0);
    r.gfx_fp_peak = Some(if retake { 228.0 } else { 50.0 });
    r
}

/// Read re-takes of bare [`QuietArm::P`] after 5 s, one per `(foreign duty, other-thread CPU
/// ns)` pair, in that order.
fn shape_takes(pairs: &[(f64, u64)]) -> Vec<TakeRecord> {
    pairs
        .iter()
        .enumerate()
        .map(|(i, &(duty, other_ns))| TakeRecord {
            index: i,
            ..at_state(
                at_duty(read_record(QuietArm::P, 5_000, other_ns), duty),
                true,
            )
        })
        .collect()
}

/// The `TAKE` line and the JSON object carry the same fields; the JSON parses (aterm-json is
/// the oracle) with `"kind":"take"` first, `null` for unread and for a non-finite value, nested
/// threads and releases arrays, and escaped strings; a line's text has no space or `=` left in
/// it. The negative control is the oracle itself: it refuses the bare `NaN` a naive writer
/// would print, so a parse that passes is evidence.
#[test]
fn quiet_plan_record_json_and_line_formats() {
    let mut r = TakeRecord::new(
        7,
        1,
        3,
        QuietTake {
            keep: Some(QuietKeep::OtherProcessDot(Duration::from_millis(100))),
            ..QuietTake::bare(QuietArm::P, 5_000)
        },
    );
    r.prev = Some((6, QuietArm::Clear));
    r.window = Some(WindowCpu {
        other_ns: Some(3_125_040),
        other_instructions: 12_345,
        burst: Some((42, 3_000_000, 1_500)),
        threads: vec![ThreadRow {
            id: 42,
            role: Some(ThreadRole::Other),
            name: Some("gpu worker".to_owned()),
            cpu_ns: 3_000_000,
            queue: Some("com.apple.metal=q".to_owned()),
            queue_asked: 3,
            queue_labelled: 2,
            ..ThreadRow::default()
        }],
        ..WindowCpu::default()
    });
    r.cuts = 41;
    r.gpu_us = Some(f64::NAN);
    r.keep_ok = Some(true);
    r.foreign = Some(Foreign {
        ns: 12_000,
        pids: 1,
        top: Some(("WindowServer".to_owned(), 12_000)),
        ..Foreign::default()
    });
    r.foreign_span_ns = Some(4_000_000);
    r.releases.push(ReleaseStep {
        phase: ReleasePhase::Quiet,
        at_ms: 1_030.0,
        from_mib: 289.1,
        to_mib: 236.6,
        gfx_from_mib: None,
        gfx_to_mib: Some(100.0),
    });
    let line = r.line();
    assert!(line.starts_with(
        "TAKE i=7 rep=1 block=3 arm=p quiet_ms=5000 keep=other_process_dot keep_period_ms=100 \
         ballast_mib=0 prev_i=6 prev_arm=clear iokit_at=quiet_end "
    ));
    assert!(line.contains(" other_us=3125.0 "), "{line}");
    assert!(line.contains(" gpu_us=- "), "{line}");
    assert!(line.contains(" window=true cuts=41 "), "{line}");
    assert!(
        line.contains(" burst_tid=42 burst_name=gpu_worker burst_queue=com.apple.metal_q "),
        "{line}"
    );
    assert!(
        line.contains(
            " foreign_gpu_us=12.0 foreign_span_us=4000.0 foreign_duty=0.003000 foreign_pids=1 "
        ),
        "{line}"
    );
    assert!(line.contains(" threads=1 "), "{line}");
    assert!(line.contains(" keep_ok=true releases=1"), "{line}");
    let json = r.json();
    assert!(json.starts_with("{\"kind\":\"take\",\"i\":7,"), "{json}");
    let v: aterm_json::Value = aterm_json::from_str(&json).expect("the take parses as JSON");
    assert_eq!(v.get("arm").and_then(aterm_json::Value::as_str), Some("p"));
    assert_eq!(
        v.get("prev_arm").and_then(aterm_json::Value::as_str),
        Some("clear")
    );
    assert!(
        v.get("gpu_us").is_some_and(aterm_json::Value::is_null),
        "NaN is null"
    );
    assert!(v.get("caller_us").is_some_and(aterm_json::Value::is_null));
    assert_eq!(
        v.get("other_us").and_then(aterm_json::Value::as_f64),
        Some(3_125.0)
    );
    assert_eq!(
        v.get("foreign_top").and_then(aterm_json::Value::as_str),
        Some("WindowServer")
    );
    assert_eq!(
        v.get("foreign_duty").and_then(aterm_json::Value::as_f64),
        Some(0.003)
    );
    let threads = v
        .get("threads")
        .and_then(aterm_json::Value::as_array)
        .expect("a threads array");
    assert_eq!(
        threads[0].get("queue").and_then(aterm_json::Value::as_str),
        Some("com.apple.metal=q"),
        "the JSON keeps the label as it was read"
    );
    assert_eq!(
        threads[0].get("name").and_then(aterm_json::Value::as_str),
        Some("gpu worker")
    );
    let steps = v
        .get("releases")
        .and_then(aterm_json::Value::as_array)
        .expect("an array");
    assert_eq!(steps.len(), 1);
    assert_eq!(
        steps[0].get("phase").and_then(aterm_json::Value::as_str),
        Some("quiet")
    );
    assert!(
        steps[0]
            .get("gfx_from_mib")
            .is_some_and(aterm_json::Value::is_null)
    );
    assert!(!json.contains('\n'), "one object, one line");
    assert!(
        aterm_json::from_str::<aterm_json::Value>("{\"x\":NaN}").is_err(),
        "negative control: the oracle refuses NaN"
    );
    assert_eq!(json_string("a\"b\\c\n\u{1}"), "\"a\\\"b\\\\c\\n\\u0001\"");
    assert_eq!(
        r.releases[0].line(8),
        "RELEASE i=8 phase=quiet at_ms=1030.0 from_mib=289.1 to_mib=236.6 gfx_from_mib=- \
         gfx_to_mib=100.0 step_mib=52.5"
    );
    let unread: aterm_json::Value = aterm_json::from_str(&record(QuietArm::Dot, 0).json()).unwrap();
    assert!(
        unread
            .get("releases")
            .and_then(aterm_json::Value::as_array)
            .is_some_and(Vec::is_empty)
    );
    assert!(
        unread
            .get("threads")
            .is_some_and(aterm_json::Value::is_null),
        "an unread window has no thread rows"
    );
    assert_eq!(
        unread.get("window").and_then(aterm_json::Value::as_bool),
        Some(false)
    );
    assert!(
        unread
            .get("foreign_duty")
            .is_some_and(aterm_json::Value::is_null),
        "no reads, no duty"
    );
}

/// Median and nearest-rank quantile on known values.
#[test]
fn quiet_plan_median_and_quantile() {
    assert_eq!(median(vec![]), None);
    assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
    assert_eq!(median(vec![4.0, 1.0, 2.0, 3.0]), Some(2.5));
    assert_eq!(median(vec![f64::NAN, 5.0]), Some(5.0));
    assert_eq!(
        quantile((1..=20).map(f64::from).collect(), 0.95),
        Some(19.0)
    );
    assert_eq!(quantile(vec![7.0], 0.95), Some(7.0));
}

/// Takes shaped to pass all four controls, with each `fp`/`other`/`gfx` figure and foreign
/// duty given, every one at the quiet's end: the four P takes after 5 s are the one stratum the
/// foreign split reads (the clear takes' graphics rise is unread, and the two back-to-back
/// takes have no quiet), their other-thread CPU 3.1, 3.0, 3.1 and 3.0 ms at duties 0, 0.01,
/// 0.02 and 0.03.
fn passing() -> Vec<TakeRecord> {
    let mut out = Vec::new();
    for (fp, q) in [(130.0, 1_000), (131.0, 1_000), (40.0, 1_500), (42.0, 1_500)] {
        let mut r = read_record(QuietArm::Clear, q, 0);
        r.fp_quiet = Some(fp);
        out.push(r);
    }
    for (duty, other) in [
        (0.0, 3_100_000),
        (0.01, 3_000_000),
        (0.02, 3_100_000),
        (0.03, 3_000_000),
    ] {
        let mut r = at_duty(read_record(QuietArm::P, 5_000, other), duty);
        r.gfx_fp = Some(110.0);
        r.gfx_fp_peak = Some(290.0);
        out.push(r);
    }
    for other in [200_000, 300_000] {
        out.push(read_record(QuietArm::P, 0, other));
    }
    out
}

/// Every control holds on takes shaped to pass, and each fails alone when its own figure
/// crosses its bound (the negative controls), including at the boundary values themselves.
/// A keep or ballast take of the same arm and quiet never enters a bare control.
#[test]
fn quiet_plan_validity_controls() {
    let ok = controls(&passing(), FOREIGN_MAX_DUTY);
    assert_eq!(ok.len(), 4);
    assert!(ok.iter().all(|c| c.holds), "{ok:?}");
    assert_eq!(
        ok[0].line(),
        "VALID control=clear_release fp_quiet_1000_mib=130.5 fp_quiet_1500_mib=41.0 drop_mib=89.5 need=drop_mib>=64"
    );
    assert_eq!(
        ok[3].line(),
        "VALID control=foreign_split iokit_at=quiet_end verdict=holds strata=1 takes=4 \
         alone=0 no_quiet=2 state_unread=4 pairs=6 slope_us_per_duty=-1666.7 \
         duty_range=0.030000 effect_us=-50.0 need=abs(effect_us)<=250"
    );

    let mut small_drop = passing();
    for r in small_drop
        .iter_mut()
        .filter(|r| r.take.quiet.as_millis() == 1_500)
    {
        r.fp_quiet = Some(67.0);
    }
    let c = controls(&small_drop, FOREIGN_MAX_DUTY);
    assert!(
        !c[0].holds && c[1].holds && c[2].holds && c[3].holds,
        "{c:?}"
    );
    assert!(c[0].line().starts_with("VOID control=clear_release"));
    for r in small_drop
        .iter_mut()
        .filter(|r| r.take.quiet.as_millis() == 1_500)
    {
        r.fp_quiet = Some(130.5 - 64.0);
    }
    assert!(
        controls(&small_drop, FOREIGN_MAX_DUTY)[0].holds,
        "exactly 64 MiB holds"
    );

    let mut cheap = passing();
    for r in cheap
        .iter_mut()
        .filter(|r| r.take.quiet.as_millis() == 5_000)
    {
        r.window.as_mut().unwrap().other_ns = Some(2_499_000);
    }
    assert!(!controls(&cheap, FOREIGN_MAX_DUTY)[1].holds);
    let mut no_rise = passing();
    for r in no_rise
        .iter_mut()
        .filter(|r| r.take.quiet.as_millis() == 5_000)
    {
        r.gfx_fp_peak = Some(209.9);
    }
    assert!(
        !controls(&no_rise, FOREIGN_MAX_DUTY)[1].holds,
        "a rise under 100 MiB is void"
    );

    let mut warm = passing();
    for r in warm
        .iter_mut()
        .filter(|r| r.take == QuietTake::bare(QuietArm::P, 0))
    {
        r.window.as_mut().unwrap().other_ns = Some(501_000);
    }
    assert!(!controls(&warm, FOREIGN_MAX_DUTY)[2].holds);

    let mut kept = passing();
    let mut held = read_record(QuietArm::P, 5_000, 0);
    held.take.keep = Some(QuietKeep::SameQueueDot(Duration::from_millis(500)));
    kept.extend([held.clone(), held.clone(), held.clone(), held]);
    assert!(
        controls(&kept, FOREIGN_MAX_DUTY)[1].holds,
        "keep takes stay out of the bare control"
    );

    let none = controls(&[], FOREIGN_MAX_DUTY);
    assert!(none.iter().all(|c| !c.holds && c.value == "no_takes"));
}

/// The decisions leave out a take whose window was not read and, of the takes after a quiet,
/// one whose foreign duty is unknown (a client read missing or lost, or no stretch between the
/// reads to divide by) and one over the bound, and count each. A back-to-back take is admitted
/// on its window alone, whatever its foreign read says (the grid's first full run read 0.66
/// and 0.69 on two); the negative control is the same reads after a 500 ms quiet, each left
/// out. The bound is on the duty, not the time: 50 ms of other processes' GPU work over a 5 s
/// stretch is admitted under the default 0.10, and the same 50 ms over 100 ms is over (the
/// negative control: a bound on the time alone takes the two alike). The cheap back-to-back
/// control is another: three zero-cost takes with unread windows (the old fallback's zero)
/// would pass it, and left out they leave it `no_takes`.
#[test]
fn quiet_plan_admission_excludes_and_counts() {
    let mut void = record(QuietArm::P, 0);
    void.foreign = Some(Foreign::default());
    void.foreign_span_ns = Some(SPAN_NS);
    let mut unknown = read_record(QuietArm::P, 500, 100_000);
    unknown.foreign = None;
    let mut gone = read_record(QuietArm::P, 500, 100_000);
    gone.foreign = Some(Foreign {
        gone: 1,
        ..Foreign::default()
    });
    let mut unspanned = read_record(QuietArm::P, 500, 100_000);
    unspanned.foreign_span_ns = None;
    let mut no_stretch = read_record(QuietArm::P, 500, 100_000);
    no_stretch.foreign_span_ns = Some(0);
    let busy = at_duty(read_record(QuietArm::P, 500, 100_000), 0.100_001);
    let run_like = at_duty(read_record(QuietArm::P, 500, 100_000), 0.69);
    let at_bound = at_duty(read_record(QuietArm::P, 500, 100_000), 0.10);
    let fine = read_record(QuietArm::P, 500, 100_000);
    assert_eq!(admission(&void, FOREIGN_MAX_DUTY), Admission::WindowVoid);
    for r in [&unknown, &gone, &unspanned, &no_stretch] {
        assert_eq!(admission(r, FOREIGN_MAX_DUTY), Admission::ForeignUnread);
    }
    assert_eq!(no_stretch.foreign_duty(), None, "nothing to divide by");
    for r in [&busy, &run_like] {
        assert_eq!(admission(r, FOREIGN_MAX_DUTY), Admission::ForeignOver);
    }
    for r in [&unknown, &gone, &unspanned, &no_stretch, &busy, &run_like] {
        let back = TakeRecord {
            take: QuietTake::bare(QuietArm::P, 0),
            ..r.clone()
        };
        assert_eq!(
            admission(&back, FOREIGN_MAX_DUTY),
            Admission::Admitted,
            "back to back: {back:?}"
        );
    }
    assert_eq!(at_bound.foreign_duty(), Some(0.10));
    assert_eq!(
        admission(&at_bound, FOREIGN_MAX_DUTY),
        Admission::Admitted,
        "at the bound"
    );
    assert_eq!(
        admission(&fine, 0.0),
        Admission::Admitted,
        "no foreign time passes a zero bound"
    );

    let long = TakeRecord {
        foreign: Some(Foreign {
            ns: 50_000_000,
            pids: 1,
            ..Foreign::default()
        }),
        foreign_span_ns: Some(5_000_000_000),
        ..read_record(QuietArm::P, 5_000, 3_000_000)
    };
    let short = TakeRecord {
        foreign_span_ns: Some(100_000_000),
        ..long.clone()
    };
    assert_eq!(long.foreign_duty(), Some(0.01));
    assert_eq!(admission(&long, FOREIGN_MAX_DUTY), Admission::Admitted);
    assert_eq!(
        admission(&short, FOREIGN_MAX_DUTY),
        Admission::ForeignOver,
        "negative control: the same 50 ms over a fiftieth of the stretch"
    );

    let all = [
        void.clone(),
        unknown,
        gone,
        unspanned,
        no_stretch,
        busy,
        fine,
    ];
    assert_eq!(
        Excluded::of(&all, FOREIGN_MAX_DUTY),
        Excluded {
            admitted: 1,
            window_void: 1,
            foreign_unread: 4,
            foreign_over: 1,
        }
    );
    assert_eq!(
        Excluded::of(&all, FOREIGN_MAX_DUTY).line(FOREIGN_MAX_DUTY),
        "EXCLUDED admitted=1 window_void=1 foreign_unread=4 foreign_over=1 \
         foreign_max_duty=0.100000"
    );
    let voids = [void.clone(), void.clone(), void];
    let back = &controls(&voids, FOREIGN_MAX_DUTY)[2];
    assert_eq!(
        (back.holds, back.value.as_str()),
        (false, "no_takes"),
        "negative control: unread windows never pass as zero"
    );
}

/// The all-takes summary groups by shape and placement over every read window; the quiet one
/// reads only the admitted takes and counts the rest as left out. Two placements of one shape
/// are two lines.
#[test]
fn quiet_plan_arm_lines_group_by_shape() {
    let lines = arm_lines(&passing(), Scope::All);
    assert_eq!(lines.len(), 4, "{lines:#?}");
    assert!(lines[0].starts_with(
        "ARM set=all arm=p quiet_ms=0 keep=none keep_period_ms=0 ballast_mib=0 \
         iokit_at=quiet_end n=2 left_out=0 other_p50_us=250.0"
    ));
    assert!(
        lines[1].contains("quiet_ms=5000")
            && lines[1].contains(" n=4 ")
            && lines[1].contains("gfx_rise_p50_mib=180.0")
            && lines[1].ends_with(" foreign_duty_p50=0.015000"),
        "{}",
        lines[1]
    );
    assert!(lines[2].contains("arm=clear quiet_ms=1000"));
    let mut mixed = passing();
    mixed[4].iokit_at = Placement::Completed;
    let all = arm_lines(&mixed, Scope::All);
    assert_eq!(all.len(), 5, "{all:#?}");
    let quiet = arm_lines(&mixed, Scope::Quiet(0.015));
    let p5: Vec<&String> = quiet
        .iter()
        .filter(|l| l.contains("arm=p quiet_ms=5000") && l.contains("iokit_at=quiet_end"))
        .collect();
    assert_eq!(p5.len(), 1);
    assert!(
        p5[0].starts_with("ARM set=quiet foreign_max_duty=0.015000 ")
            && p5[0].contains(" n=1 left_out=2 "),
        "{p5:?}"
    );
}

/// A split reads one placement, and its shape's re-takes and held takes are two strata: the
/// state is the graphics rise's, a re-take at [`RETAKE_RISE_MIB`] and held just under it. Pairs
/// are taken within a stratum, so the step from held to re-take gives no slope, though here
/// every re-take ran at a higher duty than every held take; the negative control is the same
/// takes with the held takes' rise at the bound: one stratum, whose step follows the duty and
/// is over. The completed takes of the same shape, whose cost here climbs with their duty, are
/// their own split, recorded and over, and the control judges the quiet-end split, which holds;
/// the negative control is the completed takes read at the quiet's end, which puts that climb
/// into the judged split. A take whose rise is unread, one alone in its stratum and a
/// back-to-back one (admitted at a duty of 0.69) are counted and in no pair.
#[test]
fn quiet_plan_foreign_split_reads_one_placement_within_block_state() {
    let state_of = |rise: f64| {
        BlockState::of(&TakeRecord {
            gfx_fp: Some(50.0),
            gfx_fp_peak: Some(50.0 + rise),
            ..record(QuietArm::P, 0)
        })
    };
    assert_eq!(state_of(RETAKE_RISE_MIB), Some(BlockState::Retake));
    assert_eq!(state_of(RETAKE_RISE_MIB - 0.1), Some(BlockState::Held));
    assert_eq!(BlockState::of(&record(QuietArm::P, 0)), None);

    let takes = |rise_held: f64, completed_at: Placement| -> Vec<TakeRecord> {
        [
            (0.010, 200_000, false, Placement::QuietEnd),
            (0.012, 210_000, false, Placement::QuietEnd),
            (0.014, 190_000, false, Placement::QuietEnd),
            (0.016, 200_000, false, Placement::QuietEnd),
            (0.030, 3_600_000, true, Placement::QuietEnd),
            (0.032, 3_590_000, true, Placement::QuietEnd),
            (0.034, 3_610_000, true, Placement::QuietEnd),
            (0.036, 3_600_000, true, Placement::QuietEnd),
            (0.040, 3_600_000, true, completed_at),
            (0.050, 4_600_000, true, completed_at),
            (0.060, 5_600_000, true, completed_at),
            (0.070, 6_600_000, true, completed_at),
        ]
        .iter()
        .enumerate()
        .map(|(i, &(duty, other_ns, retake, iokit_at))| {
            let mut r = at_state(
                at_duty(read_record(QuietArm::P, 1_000, other_ns), duty),
                retake,
            );
            if !retake {
                r.gfx_fp_peak = Some(50.0 + rise_held);
            }
            TakeRecord {
                index: i,
                iokit_at,
                ..r
            }
        })
        .collect()
    };
    let mut parted = takes(0.0, Placement::Completed);
    parted.push(at_duty(read_record(QuietArm::P, 1_000, 3_600_000), 0.02));
    parted.push(at_state(
        at_duty(read_record(QuietArm::Dot, 5_000, 3_600_000), 0.09),
        true,
    ));
    parted.push(at_state(
        at_duty(read_record(QuietArm::P, 0, 100_000), 0.69),
        false,
    ));
    let split = foreign_split(&parted, FOREIGN_MAX_DUTY, Placement::QuietEnd);
    assert_eq!(
        (split.placement, split.judged()),
        (Placement::QuietEnd, true)
    );
    assert_eq!(split.strata.len(), 2, "{split:?}");
    let (re, held) = (split.strata[0], split.strata[1]);
    assert_eq!(
        (re.stratum.state, held.stratum.state),
        (BlockState::Retake, BlockState::Held)
    );
    assert_eq!(
        (re.stratum.shape.iokit_at, held.stratum.shape.iokit_at),
        (Placement::QuietEnd, Placement::QuietEnd)
    );
    assert_eq!((re.n, re.pairs), (4, 6), "the quiet-end takes alone");
    assert_eq!((held.n, held.pairs), (4, 6));
    assert!((re.duty_min - 0.030).abs() < 1e-12 && (re.duty_max - 0.036).abs() < 1e-12);
    assert_eq!(re.other_p50_us, 3_600.0);
    assert_eq!((split.alone, split.state_unread, split.no_quiet), (1, 1, 1));
    assert_eq!((split.takes(), split.pairs), (8, 12));
    assert_eq!(split.slope_us_per_duty, Some(0.0), "{split:?}");
    assert!(
        (split.duty_range.unwrap() - 0.026).abs() < 1e-12,
        "the lone take's 0.09 and the back-to-back 0.69 are outside the range: {split:?}"
    );
    assert_eq!(split.verdict(), SplitVerdict::Holds);
    assert!(
        re.line().starts_with(
            "SPLIT state=retake arm=p quiet_ms=1000 keep=none keep_period_ms=0 ballast_mib=0 \
             iokit_at=quiet_end n=4 pairs=6 duty_min=0.030000 duty_max=0.036000 \
             other_p50_us=3600.0 "
        ),
        "{}",
        re.line()
    );
    assert!(
        split.line().starts_with(
            "POOLED judged=true iokit_at=quiet_end verdict=holds strata=2 takes=8 alone=1 \
             no_quiet=1 state_unread=1 pairs=12 slope_us_per_duty=0.0 duty_range=0.026000 \
             effect_us=0.0"
        ),
        "{}",
        split.line()
    );

    let completed = foreign_split(&parted, FOREIGN_MAX_DUTY, Placement::Completed);
    assert!(!completed.judged());
    assert_eq!((completed.strata.len(), completed.takes()), (1, 4));
    assert_eq!(completed.verdict(), SplitVerdict::Over, "{completed:?}");
    assert!(
        completed
            .line()
            .starts_with("POOLED judged=false iokit_at=completed verdict=over ")
    );
    let control = &controls(&parted, FOREIGN_MAX_DUTY)[3];
    assert!(control.holds, "{control:?}");
    assert!(
        control
            .value
            .starts_with("iokit_at=quiet_end verdict=holds strata=2 takes=8 ")
    );

    let read_at_end = takes(0.0, Placement::QuietEnd);
    assert_eq!(
        foreign_split(&read_at_end, FOREIGN_MAX_DUTY, Placement::QuietEnd).verdict(),
        SplitVerdict::Over,
        "negative control: the climb read at the quiet's end"
    );
    assert!(!controls(&read_at_end, FOREIGN_MAX_DUTY)[3].holds);

    let one = foreign_split(
        &takes(RETAKE_RISE_MIB, Placement::Completed),
        FOREIGN_MAX_DUTY,
        Placement::QuietEnd,
    );
    assert_eq!(one.strata.len(), 1);
    assert_eq!((one.strata[0].n, one.pairs), (8, 28));
    assert_eq!(
        one.verdict(),
        SplitVerdict::Over,
        "negative control: {one:?}"
    );
}

/// A split reads only the takes [`admission`] admits: here a take over the bound at a duty of
/// 0.9 and two whose foreign reads were not whole (a client gone at a duty of zero, a client
/// unread at 0.95), each in the stratum of four admitted takes and each 27 ms dear, stay out of
/// its takes, its pairs and its duty range, which runs from 0.02 to 0.05 alone. A filter that
/// let every take through puts each one's duty in the range and its cost in the slope.
#[test]
fn quiet_plan_foreign_split_reads_only_admitted_takes() {
    let mut takes = shape_takes(&[
        (0.02, 3_000_000),
        (0.03, 3_000_000),
        (0.04, 3_000_000),
        (0.05, 3_000_000),
        (0.9, 30_000_000),
        (0.0, 30_000_000),
        (0.95, 30_000_000),
    ]);
    takes[5].foreign.as_mut().unwrap().gone = 1;
    takes[6].foreign.as_mut().unwrap().unread = 1;
    assert_eq!(
        admission(&takes[4], FOREIGN_MAX_DUTY),
        Admission::ForeignOver
    );
    for r in &takes[5..] {
        assert_eq!(admission(r, FOREIGN_MAX_DUTY), Admission::ForeignUnread);
        assert!(r.foreign_duty().is_some(), "its duty reads: {r:?}");
    }
    let split = foreign_split(&takes, FOREIGN_MAX_DUTY, Placement::QuietEnd);
    assert_eq!(split.strata.len(), 1, "{split:?}");
    let s = split.strata[0];
    assert_eq!((s.n, s.pairs, split.alone), (4, 6, 0));
    assert!(
        (s.duty_min - 0.02).abs() < 1e-12 && (s.duty_max - 0.05).abs() < 1e-12,
        "{split:?}"
    );
    assert!((split.duty_range.unwrap() - 0.03).abs() < 1e-12);
    assert_eq!(split.slope_us_per_duty, Some(0.0));
    assert_eq!(split.verdict(), SplitVerdict::Holds);
    assert_eq!(
        Excluded::of(&takes, FOREIGN_MAX_DUTY),
        Excluded {
            admitted: 4,
            window_void: 0,
            foreign_unread: 2,
            foreign_over: 1,
        }
    );
}

/// The effect is the pooled slope times the duty range, held to 250 µs either way at its own
/// boundary and over just past it. The slope is a median of pair slopes: one take 27 ms dear at
/// the highest duty leaves it at zero, where a least-squares line through the same takes (the
/// negative control) puts 21.6 ms across the range.
#[test]
fn quiet_plan_foreign_split_effect_bound() {
    let two = |low_ns: u64, high_ns: u64| {
        foreign_split(
            &shape_takes(&[(0.0, low_ns), (0.0625, high_ns)]),
            FOREIGN_MAX_DUTY,
            Placement::QuietEnd,
        )
    };
    let at = two(1_000_000, 1_250_000);
    assert_eq!(
        (at.slope_us_per_duty, at.duty_range, at.effect_us()),
        (Some(4_000.0), Some(0.0625), Some(SPLIT_MAX_EFFECT_US))
    );
    assert_eq!(at.verdict(), SplitVerdict::Holds);
    assert_eq!(two(1_000_000, 1_250_001).verdict(), SplitVerdict::Over);
    assert_eq!(two(1_000_000, 750_000).verdict(), SplitVerdict::Holds);
    assert_eq!(
        two(1_000_000, 749_999).verdict(),
        SplitVerdict::Over,
        "cheaper by more than the bound is over too"
    );

    let hiccup = shape_takes(&[
        (0.0, 3_000_000),
        (0.01, 3_000_000),
        (0.02, 3_000_000),
        (0.03, 3_000_000),
        (0.04, 30_000_000),
    ]);
    let split = foreign_split(&hiccup, FOREIGN_MAX_DUTY, Placement::QuietEnd);
    assert_eq!((split.pairs, split.slope_us_per_duty), (10, Some(0.0)));
    assert_eq!(split.verdict(), SplitVerdict::Holds);
    let points: Vec<(f64, f64)> = hiccup
        .iter()
        .map(|r| (r.foreign_duty().unwrap(), r.other_us().unwrap()))
        .collect();
    let n = points.len() as f64;
    let (mx, my) = (
        points.iter().map(|p| p.0).sum::<f64>() / n,
        points.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let least_squares = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>()
        / points.iter().map(|p| (p.0 - mx).powi(2)).sum::<f64>();
    assert!(
        (least_squares * split.duty_range.unwrap() - 21_600.0).abs() < 1e-3,
        "negative control: {least_squares}"
    );
}

/// A machine where nothing else ran gives every take a duty of zero: no pair of different
/// duty, so the split is flat and the control holds, with no slope to give; the negative
/// control is one take moved to another duty, which gives pairs and a slope. A run with no
/// stratum of two takes is `no_takes`, and void: none at all, takes alone in their strata, or
/// takes whose graphics rise is unread.
#[test]
fn quiet_plan_foreign_split_flat_and_no_takes() {
    let still = shape_takes(&[
        (0.0, 3_000_000),
        (0.0, 3_900_000),
        (0.0, 2_800_000),
        (0.0, 4_100_000),
    ]);
    let split = foreign_split(&still, FOREIGN_MAX_DUTY, Placement::QuietEnd);
    assert_eq!((split.pairs, split.slope_us_per_duty), (0, None));
    assert_eq!(split.verdict(), SplitVerdict::Flat);
    let c = &controls(&still, FOREIGN_MAX_DUTY)[3];
    assert!(c.holds, "{c:?}");
    assert_eq!(
        c.value,
        "iokit_at=quiet_end verdict=flat strata=1 takes=4 alone=0 no_quiet=0 state_unread=0 \
         pairs=0 slope_us_per_duty=- duty_range=0.000000 effect_us=-"
    );
    let mut moved = still;
    moved[3] = at_duty(moved[3].clone(), 0.01);
    let split = foreign_split(&moved, FOREIGN_MAX_DUTY, Placement::QuietEnd);
    assert_eq!(split.pairs, 3, "negative control");
    assert!(split.slope_us_per_duty.is_some());

    let none = controls(&[], FOREIGN_MAX_DUTY);
    assert_eq!((none[3].holds, none[3].value.as_str()), (false, "no_takes"));
    let alone = vec![
        at_state(
            at_duty(read_record(QuietArm::P, 5_000, 3_000_000), 0.01),
            true,
        ),
        at_state(
            at_duty(read_record(QuietArm::Dot, 5_000, 3_000_000), 0.02),
            true,
        ),
        at_state(
            at_duty(read_record(QuietArm::P, 5_000, 200_000), 0.03),
            false,
        ),
    ];
    let split = foreign_split(&alone, FOREIGN_MAX_DUTY, Placement::QuietEnd);
    assert_eq!((split.alone, split.verdict()), (3, SplitVerdict::NoTakes));
    assert!(!controls(&alone, FOREIGN_MAX_DUTY)[3].holds);
    let unread: Vec<TakeRecord> = shape_takes(&[(0.0, 3_000_000), (0.01, 3_000_000)])
        .into_iter()
        .map(|mut r| {
            r.gfx_fp = None;
            r
        })
        .collect();
    let split = foreign_split(&unread, FOREIGN_MAX_DUTY, Placement::QuietEnd);
    assert_eq!(
        (split.state_unread, split.verdict()),
        (2, SplitVerdict::NoTakes)
    );
}

/// A uniform draw in [0, 1): the top 53 bits of the next state of the plan's xorshift
/// ([`shuffle`]'s step).
fn unit(seed: &mut u64) -> f64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    (*seed >> 11) as f64 / (1_u64 << 53) as f64
}

/// A grid shaped like a decision run of eight repetitions at the default placement: every arm
/// at every block-1 quiet, eight takes a shape, every one read at the quiet's end. As in the
/// first full run, a take re-took after 1.5 s or more, and three of eight did at 0.5 and 1 s.
/// Its other-thread CPU is 3.6 ms ± 1 ms on a re-take and 150 ± 150 µs held, drawn apart from
/// its foreign duty (0.005 to 0.04), plus a planted cost of `planted_us` over the grid's duty
/// range per unit of duty: `planted_us` between its least and its most foreign-busy take.
fn grid(seed: u64, planted_us: f64) -> Vec<TakeRecord> {
    let mut seed = seed;
    let mut draws = Vec::new();
    for arm in QuietArm::ALL {
        for q in CURVE_QUIETS_MS {
            for i in 0..8 {
                let retake = q >= 1_500 || (q >= 500 && i < 3);
                let u = unit(&mut seed);
                let duty =
                    ((0.005 + 0.035 * unit(&mut seed)) * SPAN_NS as f64).round() / SPAN_NS as f64;
                let other_us = if retake {
                    3_600.0 + (u - 0.5) * 2_000.0
                } else {
                    150.0 + (u - 0.5) * 300.0
                };
                draws.push((arm, q, retake, duty, other_us));
            }
        }
    }
    let duties = || draws.iter().map(|d| d.3);
    let range = duties().fold(f64::NEG_INFINITY, f64::max) - duties().fold(f64::INFINITY, f64::min);
    draws
        .iter()
        .enumerate()
        .map(|(index, &(arm, q, retake, duty, other_us))| {
            let other_ns = ((other_us + planted_us / range * duty) * 1e3).round() as u64;
            TakeRecord {
                index,
                ..at_state(at_duty(read_record(arm, q, other_ns), duty), retake)
            }
        })
        .collect()
}

/// `records` with their foreign reads shuffled among the takes of each stratum (arm, quiet and
/// state, which is all a [`grid`] take's stratum holds) by the plan's [`shuffle`] from `seed`.
fn shuffled_duty(records: &[TakeRecord], seed: u64) -> Vec<TakeRecord> {
    let mut strata: std::collections::BTreeMap<(QuietArm, u64, bool), Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, r) in records.iter().enumerate() {
        let retake = BlockState::of(r) == Some(BlockState::Retake);
        strata
            .entry((r.take.arm, r.take.quiet.as_millis() as u64, retake))
            .or_default()
            .push(i);
    }
    let mut out = records.to_vec();
    let mut seed = seed;
    for at in strata.values() {
        let mut reads: Vec<Option<Foreign>> =
            at.iter().map(|&i| records[i].foreign.clone()).collect();
        shuffle(&mut reads, &mut seed);
        for (&i, read) in at.iter().zip(reads) {
            out[i].foreign = read;
        }
    }
    out
}

/// On grids shaped like a decision run (seeds 1 to 100), with cost drawn apart from the duty,
/// the split holds in at least 95 of 100 (the first's back-to-back takes in no stratum); with
/// 600 µs planted across each grid's duty range every one is over (the negative control). The
/// planted takes of one grid with their duties shuffled within each stratum, which keeps every
/// stratum's costs and duties and breaks only their pairing, hold in at least 190 of 200
/// shuffles (at most 10 over): with nothing left to find, the control holds by chance.
#[test]
fn quiet_plan_foreign_split_holds_on_shuffled_duty() {
    let split =
        |records: &[TakeRecord]| foreign_split(records, FOREIGN_MAX_DUTY, Placement::QuietEnd);
    let one = split(&grid(1, 0.0));
    assert_eq!(one.strata.len(), 7 * 7 + 7 * 2, "{one:?}");
    assert_eq!(one.no_quiet, 7 * 8);
    let seeds = 1..=100_u64;
    let null_over = seeds
        .clone()
        .filter(|&seed| split(&grid(seed, 0.0)).verdict() != SplitVerdict::Holds)
        .count();
    assert!(null_over <= 5, "{null_over} of 100 null grids over");
    let planted_held: Vec<u64> = seeds
        .filter(|&seed| split(&grid(seed, 600.0)).verdict() != SplitVerdict::Over)
        .collect();
    assert!(
        planted_held.is_empty(),
        "negative control: planted grids not over: {planted_held:?}"
    );
    let planted = grid(1, 600.0);
    let shuffles = 200;
    let over = (1..=shuffles)
        .filter(|&seed| split(&shuffled_duty(&planted, seed)).verdict() != SplitVerdict::Holds)
        .count();
    assert!(
        over * 20 <= shuffles as usize,
        "{over} of {shuffles} shuffles over"
    );
}

/// The controls object parses, says whether every control held, carries the exclusion counts
/// and each placement's foreign split with its strata, the judged one marked, so a reader of
/// the JSON alone can tell a void run and where the judged slope came from.
#[test]
fn quiet_plan_controls_json_parses() {
    let c = controls(&passing(), FOREIGN_MAX_DUTY);
    let splits = Placement::ALL.map(|at| foreign_split(&passing(), FOREIGN_MAX_DUTY, at));
    let excluded = Excluded::of(&passing(), FOREIGN_MAX_DUTY);
    let v: aterm_json::Value =
        aterm_json::from_str(&controls_json(&c, &splits, &excluded, FOREIGN_MAX_DUTY))
            .expect("the controls parse");
    assert_eq!(
        v.get("kind").and_then(aterm_json::Value::as_str),
        Some("controls")
    );
    assert_eq!(
        v.get("valid").and_then(aterm_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        v.get("foreign_max_duty")
            .and_then(aterm_json::Value::as_f64),
        Some(0.1)
    );
    assert_eq!(
        v.get("admitted").and_then(aterm_json::Value::as_u64),
        Some(10)
    );
    let list = v
        .get("controls")
        .and_then(aterm_json::Value::as_array)
        .expect("a list");
    assert_eq!(list.len(), 4);
    let got = v
        .get("splits")
        .and_then(aterm_json::Value::as_array)
        .expect("the splits");
    assert_eq!(got.len(), 2);
    fn text<'a>(o: &'a aterm_json::Value, k: &str) -> Option<&'a str> {
        o.get(k).and_then(aterm_json::Value::as_str)
    }
    let judged = |o: &aterm_json::Value| o.get("judged").and_then(aterm_json::Value::as_bool);
    assert_eq!(
        (
            text(&got[0], "iokit_at"),
            judged(&got[0]),
            text(&got[0], "verdict")
        ),
        (Some("quiet_end"), Some(true), Some("holds"))
    );
    assert_eq!(
        (
            text(&got[1], "iokit_at"),
            judged(&got[1]),
            text(&got[1], "verdict")
        ),
        (Some("completed"), Some(false), Some("no_takes"))
    );
    assert_eq!(
        got[0].get("effect_us").and_then(aterm_json::Value::as_f64),
        Some(-50.0)
    );
    let rows = got[0]
        .get("strata")
        .and_then(aterm_json::Value::as_array)
        .expect("the strata");
    assert_eq!(rows.len(), 1);
    assert_eq!(text(&rows[0], "state"), Some("retake"));
    assert_eq!(text(&rows[0], "iokit_at"), Some("quiet_end"));
    assert_eq!(
        rows[0].get("quiet_ms").and_then(aterm_json::Value::as_u64),
        Some(5_000)
    );
    assert_eq!(
        rows[0].get("pairs").and_then(aterm_json::Value::as_u64),
        Some(6)
    );
    assert_eq!(
        rows[0]
            .get("other_p50_us")
            .and_then(aterm_json::Value::as_f64),
        Some(3_050.0)
    );
    assert!(
        got[1]
            .get("strata")
            .and_then(aterm_json::Value::as_array)
            .is_some_and(Vec::is_empty)
    );
    let void = controls(&[], FOREIGN_MAX_DUTY);
    let v: aterm_json::Value = aterm_json::from_str(&controls_json(
        &void,
        &[],
        &Excluded::default(),
        FOREIGN_MAX_DUTY,
    ))
    .unwrap();
    assert_eq!(
        v.get("valid").and_then(aterm_json::Value::as_bool),
        Some(false),
        "negative control: a void run says so"
    );
    assert!(
        v.get("splits")
            .and_then(aterm_json::Value::as_array)
            .is_some_and(Vec::is_empty)
    );
}
