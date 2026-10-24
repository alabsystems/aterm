// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-workers-bench`'s unit tests: the Williams order's two balance properties, the summary parser on
//! real captures, each gate's pass and fail, C0 on a real bundle, C1–C3 holding and not, the fault
//! session's criteria and the record's stable lines.

use super::*;

/// A real `--workers 8` summary of the session's workload (`aos-search --grid --plans <the M4 record's
/// 17-line file> --workers 8`, captured 2026-10-23 on this tree's debug lab; its `wrote` line dropped).
const P8: &str = r"aos-search policy=38c83f009e0e build=aterm-lab_0.128.0+gd97369e77400-dirty toolchain=aterm-lab_0.128.0+gd97369e77400-dirty
MEASURED (never scored) evaluator_build_ms=6380.4
search proposer=grid grid v1 config=c4b618825d6c
  counts proposed=54 distinct=54 admitted_claimed=18 oversized=0 duplicates=0 over_budget=0 rounds=4
  refused_claimed HotHasWake=9 NoStepSlept=9 WakeNoLaterThanClear=18 unreported=0
  winner plan=010100000000000000000000 digest=0a4234ea1db9 round=0 index=6 useful=true
  selection provenance=declared
  train T=6394 Z=94 baseline T=8441 Z=2175
  held_out T=5119 Z=161 baseline T=6685 Z=1750 gain_ppm=234255
  s0 T=2500 Z=10 baseline T=2776 Z=296 gain_ppm=99423
  bundle file=grid.bundle bytes=5479 digest=da99610ba181 verified=true build=aterm-lab_0.128.0+gd97369e77400-dirty toolchain=aterm-lab_0.128.0+gd97369e77400-dirty
  MEASURED (never scored) wall_ms=15866.1 stepped_here=401400 stepped_claimed=571764 (declared)
  claimed jobs=54 rows=351 local_judged=168 claimed_only=183 check=dominance barrier_threads=10
  MEASURED (never scored) last_claim_ms=9024.7 check_ms=53:532.5,17:4634.8
  MEASURED (never scored) trip_ms class=admitted n=18 p50=3208.4 p95=3974.9 p99=3974.9 overhead_ms(declared) p50=0.1 p95=0.3 p99=0.3
  MEASURED (never scored) trip_ms class=refused n=36 p50=8.0 p95=60.1 p99=60.3 overhead_ms(declared) p50=0.1 p95=3.0 p99=3.8
  MEASURED (never scored) held_out_ms=1673.8 verify_ms=5519.2
search proposer=plans file v1 config=e5463d8e4c08
  counts proposed=17 distinct=16 admitted_claimed=7 oversized=1 duplicates=0 over_budget=0 rounds=4
  refused_claimed Budget=1 PlanRefusal=8 WakeNoLaterThanClear=1 unreported=0
  winner plan=010100000000000000000000 digest=0a4234ea1db9 round=0 index=7 useful=true
  selection provenance=declared
  train T=6394 Z=94 baseline T=8441 Z=2175
  held_out T=5119 Z=161 baseline T=6685 Z=1750 gain_ppm=234255
  s0 T=2500 Z=10 baseline T=2776 Z=296 gain_ppm=99423
  bundle file=plans.bundle bytes=5479 digest=608a5a93e7df verified=true build=aterm-lab_0.128.0+gd97369e77400-dirty toolchain=aterm-lab_0.128.0+gd97369e77400-dirty
  MEASURED (never scored) wall_ms=13947.9 stepped_here=167787 stepped_claimed=180730 (declared)
  claimed jobs=8 rows=120 local_judged=83 claimed_only=37 check=dominance barrier_threads=10
  MEASURED (never scored) last_claim_ms=6043.7 check_ms=18:719.2,6:5025.9
  MEASURED (never scored) trip_ms class=admitted n=7 p50=4442.7 p95=6043.6 p99=6043.6 overhead_ms(declared) p50=0.1 p95=9.9 p99=9.9
  MEASURED (never scored) trip_ms class=refused n=1 p50=19.7 p95=19.7 p99=19.7 overhead_ms(declared) p50=10.0 p95=10.0 p99=10.0
  MEASURED (never scored) held_out_ms=2159.0 verify_ms=2780.6
workers n=8 window_ms=60000 faults=none
workers counts joined=8 leases=62 retries=0 expired=0 lost=0 foreign=0 cancelled=0 duplicate=0 stale=0 late_progress=0 dropped=0
workers processes spawned=8 reaped=8 killed=0
workers MEASURED (never scored) pool_ms=6865.0 peak_rss_mb_max=137 peak_rss_mb_sum=1088 cpu_ms=113510.8 frames_in=504 bytes_in=92039 frames_out=62 bytes_out=4898 queue_max=8 core_ms=7.6
workers faults fired=none
aos-search result=pass searches=2 winners=2
";

/// A real A run (no flag) of the same workload, captured the same way.
const A: &str = r"aos-search policy=38c83f009e0e build=aterm-lab_0.128.0+gd97369e77400-dirty toolchain=aterm-lab_0.128.0+gd97369e77400-dirty
MEASURED (never scored) evaluator_build_ms=3328.9
search proposer=grid grid v1 config=c4b618825d6c
  counts proposed=54 distinct=54 admitted=18 oversized=0 duplicates=0 over_budget=0 rounds=4
  refused HotHasWake=9 NoStepSlept=9 WakeNoLaterThanClear=18 unreported=0
  winner plan=010100000000000000000000 digest=0a4234ea1db9 round=0 index=6 useful=true
  selection provenance=rederived
  train T=6394 Z=94 baseline T=8441 Z=2175
  held_out T=5119 Z=161 baseline T=6685 Z=1750 gain_ppm=234255
  s0 T=2500 Z=10 baseline T=2776 Z=296 gain_ppm=99423
  bundle file=grid.bundle bytes=5479 digest=ad367abe668e verified=true build=aterm-lab_0.128.0+gd97369e77400-dirty toolchain=aterm-lab_0.128.0+gd97369e77400-dirty
  MEASURED (never scored) wall_ms=31376.1 stepped=592060
  MEASURED (never scored) held_out_ms=1344.1 verify_ms=2586.0
search proposer=plans file v1 config=e5463d8e4c08
  counts proposed=17 distinct=16 admitted=7 oversized=1 duplicates=0 over_budget=0 rounds=4
  refused Budget=1 PlanRefusal=8 WakeNoLaterThanClear=1 unreported=0
  winner plan=010100000000000000000000 digest=0a4234ea1db9 round=0 index=7 useful=true
  selection provenance=declared
  train T=6394 Z=94 baseline T=8441 Z=2175
  held_out T=5119 Z=161 baseline T=6685 Z=1750 gain_ppm=234255
  s0 T=2500 Z=10 baseline T=2776 Z=296 gain_ppm=99423
  bundle file=plans.bundle bytes=5479 digest=ffd89d63a4f2 verified=true build=aterm-lab_0.128.0+gd97369e77400-dirty toolchain=aterm-lab_0.128.0+gd97369e77400-dirty
  MEASURED (never scored) wall_ms=11263.9 stepped=201026
  MEASURED (never scored) held_out_ms=1351.3 verify_ms=2612.1
aos-search result=pass searches=2 winners=2
";

/// A real `--workers 8 --lease-ms 10000 --fault kill@1.6.1:3` run of the same workload.
const KILL: &str = r"aos-search policy=38c83f009e0e build=aterm-lab_0.128.0+gd97369e77400-dirty toolchain=aterm-lab_0.128.0+gd97369e77400-dirty
MEASURED (never scored) evaluator_build_ms=5385.6
search proposer=grid grid v1 config=c4b618825d6c
  counts proposed=54 distinct=54 admitted_claimed=18 oversized=0 duplicates=0 over_budget=0 rounds=4
  refused_claimed HotHasWake=9 NoStepSlept=9 WakeNoLaterThanClear=18 unreported=0
  winner plan=010100000000000000000000 digest=0a4234ea1db9 round=0 index=6 useful=true
  selection provenance=declared
  train T=6394 Z=94 baseline T=8441 Z=2175
  held_out T=5119 Z=161 baseline T=6685 Z=1750 gain_ppm=234255
  s0 T=2500 Z=10 baseline T=2776 Z=296 gain_ppm=99423
  bundle file=grid.bundle bytes=5479 digest=f8baa514cf05 verified=true build=aterm-lab_0.128.0+gd97369e77400-dirty toolchain=aterm-lab_0.128.0+gd97369e77400-dirty
  MEASURED (never scored) wall_ms=13088.6 stepped_here=401400 stepped_claimed=571764 (declared)
  claimed jobs=54 rows=351 local_judged=168 claimed_only=183 check=dominance barrier_threads=10
  MEASURED (never scored) last_claim_ms=7466.6 check_ms=53:362.8,17:3855.4
  MEASURED (never scored) trip_ms class=admitted n=18 p50=2273.8 p95=2870.1 p99=2870.1 overhead_ms(declared) p50=0.1 p95=0.5 p99=0.5
  MEASURED (never scored) trip_ms class=refused n=36 p50=7.9 p95=41.5 p99=46.4 overhead_ms(declared) p50=0.1 p95=0.4 p99=0.9
  MEASURED (never scored) held_out_ms=1403.5 verify_ms=2686.5
search proposer=plans file v1 config=e5463d8e4c08
  counts proposed=17 distinct=16 admitted_claimed=7 oversized=1 duplicates=0 over_budget=0 rounds=4
  refused_claimed Budget=1 PlanRefusal=8 WakeNoLaterThanClear=1 unreported=0
  winner plan=010100000000000000000000 digest=0a4234ea1db9 round=0 index=7 useful=true
  selection provenance=declared
  train T=6394 Z=94 baseline T=8441 Z=2175
  held_out T=5119 Z=161 baseline T=6685 Z=1750 gain_ppm=234255
  s0 T=2500 Z=10 baseline T=2776 Z=296 gain_ppm=99423
  bundle file=plans.bundle bytes=5479 digest=3e2151257301 verified=true build=aterm-lab_0.128.0+gd97369e77400-dirty toolchain=aterm-lab_0.128.0+gd97369e77400-dirty
  MEASURED (never scored) wall_ms=6194.1 stepped_here=167787 stepped_claimed=180730 (declared)
  claimed jobs=8 rows=120 local_judged=83 claimed_only=37 check=dominance barrier_threads=10
  MEASURED (never scored) last_claim_ms=2567.2 check_ms=18:363.6,6:1822.6
  MEASURED (never scored) trip_ms class=admitted n=7 p50=1958.9 p95=2567.1 p99=2567.1 overhead_ms(declared) p50=0.1 p95=0.1 p99=0.1
  MEASURED (never scored) trip_ms class=refused n=1 p50=6.8 p95=6.8 p99=6.8 overhead_ms(declared) p50=0.1 p95=0.1 p99=0.1
  MEASURED (never scored) held_out_ms=1440.6 verify_ms=2693.8
workers n=8 window_ms=10000 faults=kill@1.6.1:3
workers counts joined=8 leases=63 retries=1 expired=0 lost=1 foreign=0 cancelled=0 duplicate=0 stale=0 late_progress=0 dropped=0
workers processes spawned=8 reaped=8 killed=1
workers MEASURED (never scored) pool_ms=5559.3 peak_rss_mb_max=137 peak_rss_mb_sum=1087 cpu_ms=101300.7 frames_in=507 bytes_in=92275 frames_out=63 bytes_out=4977 queue_max=8 core_ms=7.3
workers faults fired=kill@1.6.1:3=1
aos-search result=pass searches=2 winners=2
";

/// The fixtures' build with its `-dirty` stamp dropped: the clean build a registered session runs.
const BUILD: &str = "aterm-lab_0.128.0+gd97369e77400";

fn clean(text: &str) -> String {
    text.replace("-dirty", "")
}

/// The summary an honest run of `arm` prints, from the real captures: T arms are A's shape with
/// `threads n=`, P4 is P8's with four workers, P8A is P8's under `--check all`.
fn text_for(arm: Arm) -> String {
    let text = match arm {
        Arm::A => A.to_owned(),
        Arm::T4 | Arm::T8 => A.replacen(
            "evaluator_build_ms=3328.9\n",
            &format!(
                "evaluator_build_ms=3328.9\nthreads n={}\n",
                arm.threads().unwrap_or(0)
            ),
            1,
        ),
        Arm::P4 => four_workers(P8),
        Arm::P8 => P8.to_owned(),
        Arm::P8A => P8
            .replace("check=dominance", "check=all")
            .replace("admitted_claimed=", "admitted=")
            .replace("refused_claimed ", "refused "),
    };
    clean(&text)
}

/// `text` (a P8 summary) as four workers print it.
fn four_workers(text: &str) -> String {
    text.replace("workers n=8 ", "workers n=4 ")
        .replace("joined=8 ", "joined=4 ")
        .replace("spawned=8 reaped=8 ", "spawned=4 reaped=4 ")
}

/// Every run judged against the session's build, as `settle` judges them.
fn rejudge(runs: &mut [Run]) {
    let build = session_build(runs);
    for r in runs.iter_mut() {
        r.judged = judge(r, build.as_deref());
    }
}

/// The predicted walls (ms) and CPU (ms) of an honest round, per arm.
fn figures_of(arm: Arm) -> (f64, f64) {
    match arm {
        Arm::A => (48_000.0, 47_000.0),
        Arm::T4 => (25_000.0, 60_000.0),
        Arm::T8 => (20_000.0, 62_000.0),
        Arm::P4 => (30_000.0, 140_000.0),
        Arm::P8 => (26_000.0, 150_000.0),
        Arm::P8A => (27_000.0, 170_000.0),
    }
}

fn run_with(arm: Arm, round: usize, pos: usize, text: &str, wall_ms: f64, cpu_ms: f64) -> Run {
    let mut r = Run {
        id: run_id(Phase::Clean, round, pos),
        phase: Phase::Clean,
        round,
        pos,
        arm,
        fault: None,
        out: format!("runs/{}-{}", run_id(Phase::Clean, round, pos), arm.name()),
        load1: Ok(2.5),
        m: Measured {
            wall_ms,
            exit: Some(0),
            cpu_ms: Some(cpu_ms),
            maxrss: Some(300 * MIB),
            stdout: text.to_owned(),
            ..Measured::default()
        },
        summary: parse_summary(text),
        parity: Some(vec![Parity::Equal, Parity::Equal]),
        judged: None,
    };
    r.judged = judge(&r, Some(BUILD));
    r
}

fn honest(arm: Arm, round: usize, pos: usize) -> Run {
    let (wall, cpu) = figures_of(arm);
    run_with(arm, round, pos, &text_for(arm), wall, cpu)
}

/// A clean session of `rounds` honest rounds in the Williams order, each run passed through `edit`.
fn session(rounds: usize, edit: impl Fn(&mut Run)) -> Vec<Run> {
    let mut runs = Vec::new();
    for (n, order) in schedule(rounds).iter().enumerate() {
        for (p, arm) in order.iter().enumerate() {
            let mut r = honest(*arm, n + 1, p + 1);
            edit(&mut r);
            runs.push(r);
        }
    }
    rejudge(&mut runs);
    runs
}

const LAB: LabFacts = LabFacts {
    debug: Some(true),
    unchanged: Some(true),
};

fn evaluated(runs: &[Run], rounds: usize) -> CleanEval {
    evaluate_clean(runs, rounds, LAB, true)
}

fn void_reasons(ev: &CleanEval) -> Vec<String> {
    match &ev.outcome {
        Outcome::Void(v) => v.clone(),
        other => panic!("not void: {other:?}"),
    }
}

fn header(rounds: usize, faults: FaultPlan) -> Header {
    Header {
        rounds,
        faults,
        lab: "/lab/aterm-lab".to_owned(),
        lab_digest: "0123456789ab".to_owned(),
        runner_is_lab: Some(true),
        profile: "debug opt-level=0".to_owned(),
        lab_facts: LAB,
        plans: "/plans/model-plans.txt".to_owned(),
        parallelism: 10,
    }
}

// --- The order.

#[test]
fn the_williams_square_puts_each_arm_once_in_every_position() {
    let square = williams(6).expect("six is even");
    assert_eq!(square.len(), 6);
    for row in &square {
        let mut sorted = row.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            (0..6).collect::<Vec<_>>(),
            "each order is a permutation"
        );
    }
    for pos in 0..6 {
        let mut column: Vec<usize> = square.iter().map(|row| row[pos]).collect();
        column.sort_unstable();
        assert_eq!(column, (0..6).collect::<Vec<_>>(), "position {pos}");
    }
}

#[test]
fn the_williams_square_puts_each_arm_after_every_other_exactly_once() {
    for n in [2, 4, 6, 8] {
        let square = williams(n).expect("even");
        let mut after = vec![vec![0u32; n]; n];
        for row in &square {
            for pair in row.windows(2) {
                after[pair[0]][pair[1]] += 1;
            }
        }
        for (a, followers) in after.iter().enumerate() {
            for (b, count) in followers.iter().enumerate() {
                let want = u32::from(a != b);
                assert_eq!(*count, want, "n={n}: {b} after {a}");
            }
        }
    }
    assert_eq!(williams(5), None, "an odd n has no n-row square");
    assert_eq!(williams(0), None);
}

#[test]
fn twelve_rounds_are_the_six_orders_twice_over() {
    let s = schedule(12);
    assert_eq!(s.len(), 12);
    assert_eq!(
        s[0],
        [Arm::A, Arm::T4, Arm::P8A, Arm::T8, Arm::P8, Arm::P4],
        "row 0 is 0, 1, 5, 2, 4, 3"
    );
    for r in 0..6 {
        assert_eq!(s[r], s[r + 6], "round {} repeats round {}", r + 7, r + 1);
    }
    let distinct: std::collections::BTreeSet<_> = s.iter().collect();
    assert_eq!(distinct.len(), 6);
    assert_eq!(schedule(1), vec![s[0]]);
}

// --- Arguments.

fn words(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn aos_workers_bench_parses_its_flags_and_defaults_to_the_registered_twelve_rounds() {
    let a = parse_args(&words(&["--plans", "p.txt", "--out", "/tmp/o"])).expect("parses");
    assert_eq!(
        a,
        Args {
            plans: "p.txt".into(),
            out: "/tmp/o".into(),
            lab: None,
            faults: false,
            rounds: 12,
        }
    );
    let a = parse_args(&words(&[
        "--plans", "p", "--out", "o", "--lab", "l", "--faults", "--rounds", "1",
    ]))
    .expect("parses");
    assert_eq!((a.lab.as_deref(), a.faults, a.rounds), (Some("l"), true, 1));
    for bad in [
        &["--out", "o"][..],
        &["--plans", "p"][..],
        &["--plans", "p", "--out", "o", "--rounds", "0"][..],
        &["--plans", "p", "--out", "o", "--rounds", "13"][..],
        &["--plans", "p", "--out", "o", "--grid"][..],
    ] {
        let f = parse_args(&words(bad)).expect_err("a usage error");
        assert_eq!(f.code, 2, "{bad:?}: {:?}", f.msg);
    }
}

#[test]
fn a_run_adds_its_arms_flags_then_its_fault() {
    assert!(run_flags(Arm::A, None).is_empty());
    assert_eq!(
        run_flags(Arm::P8A, None),
        words(&["--workers", "8", "--check", "all"])
    );
    assert_eq!(
        run_flags(Arm::P8, Some(FaultCase::Hang)),
        words(&[
            "--workers",
            "8",
            "--lease-ms",
            "10000",
            "--fault",
            "hang@1.6.1:3"
        ])
    );
}

// --- The summary.

#[test]
fn a_real_workers_summary_parses_field_by_field() {
    let s = parse_summary(P8).expect("parses");
    assert_eq!(s.policy, "38c83f009e0e");
    assert_eq!(s.build, "aterm-lab_0.128.0+gd97369e77400-dirty");
    assert_eq!(s.evaluator_build_ms, Some(6380.4));
    assert_eq!(s.threads, None);
    assert_eq!(
        (s.result.as_str(), s.searches_n, s.winners),
        ("pass", Some(2), Some(2))
    );
    let names: Vec<&str> = s.searches.iter().map(|x| x.proposer.as_str()).collect();
    assert_eq!(names, SEARCHES);
    let g = s.search("grid").expect("the grid");
    assert_eq!(g.config, "c4b618825d6c");
    assert_eq!(
        (g.proposed, g.distinct, g.admitted, g.on_claims),
        (Some(54), Some(54), Some(18), true)
    );
    assert_eq!(
        g.refused,
        vec![
            ("HotHasWake".to_owned(), 9),
            ("NoStepSlept".to_owned(), 9),
            ("WakeNoLaterThanClear".to_owned(), 18)
        ]
    );
    assert_eq!(g.unreported, Some(0));
    assert_eq!(
        g.winner,
        Some(Winner {
            plan: "010100000000000000000000".into(),
            digest: "0a4234ea1db9".into(),
            round: 0,
            index: 6,
            useful: true,
        })
    );
    assert_eq!(g.provenance.as_deref(), Some("declared"));
    assert_eq!(g.train, Some((6394, 94)));
    assert_eq!(g.held_out, Some((5119, 161, 234_255)));
    assert_eq!(g.s0, Some((2500, 10, 99_423)));
    assert_eq!(
        g.bundle,
        Some(("grid.bundle".to_owned(), "da99610ba181".to_owned()))
    );
    assert_eq!(g.wall_ms, Some(15866.1));
    assert_eq!(
        (g.stepped, g.stepped_here, g.stepped_claimed),
        (None, Some(401_400), Some(571_764))
    );
    assert_eq!(
        g.claimed,
        Some(Claimed {
            jobs: 54,
            rows: 351,
            local_judged: 168,
            claimed_only: 183,
            check: "dominance".into(),
            barrier_threads: 10,
        })
    );
    assert_eq!(g.last_claim_ms, Some(9024.7));
    assert_eq!(g.check_ms, vec![(53, 532.5), (17, 4634.8)]);
    assert_eq!(
        g.trip_admitted,
        Some(Trip {
            n: 18,
            trip: Some([3208.4, 3974.9, 3974.9]),
            over: Some([0.1, 0.3, 0.3]),
        })
    );
    assert_eq!(g.trip_refused.as_ref().map(|t| t.n), Some(36));
    assert_eq!((g.held_out_ms, g.verify_ms), (Some(1673.8), Some(5519.2)));
    let p = s.search("plans").expect("the plans file");
    assert_eq!(p.config, "e5463d8e4c08");
    assert_eq!(p.winner.as_ref().map(|w| w.index), Some(7));
    assert_eq!(p.refused.len(), 3);
    let pool = s.pool.as_ref().expect("the pool");
    assert_eq!((pool.n, pool.window_ms), (Some(8), Some(60_000)));
    assert!(pool.faults.is_empty() && pool.fired.is_empty());
    assert_eq!(pool.count("leases"), Some(62));
    assert_eq!(pool.count("joined"), Some(8));
    for k in CLEAN_COUNTERS {
        assert_eq!(pool.count(k), Some(0), "{k}");
    }
    assert_eq!(
        (pool.spawned, pool.reaped, pool.killed),
        (Some(8), Some(8), Some(0))
    );
    assert_eq!(pool.pool_ms, Some(6865.0));
    assert_eq!(
        (pool.peak_rss_mb_max, pool.peak_rss_mb_sum),
        (Some(137), Some(1088))
    );
    assert_eq!(pool.cpu_ms, Some(113_510.8));
    assert_eq!(
        (
            pool.frames_in,
            pool.bytes_in,
            pool.frames_out,
            pool.bytes_out,
            pool.queue_max
        ),
        (Some(504), Some(92_039), Some(62), Some(4898), Some(8))
    );
    assert_eq!(pool.core_ms, Some(7.6));
}

#[test]
fn a_real_serial_summary_and_a_real_fault_summary_parse() {
    let a = parse_summary(A).expect("parses");
    assert!(a.pool.is_none());
    let g = a.search("grid").expect("grid");
    assert!(!g.on_claims);
    assert_eq!(
        (g.admitted, g.stepped, g.wall_ms),
        (Some(18), Some(592_060), Some(31376.1))
    );
    assert_eq!(g.claimed, None);
    assert_eq!(g.provenance.as_deref(), Some("rederived"));
    let k = parse_summary(KILL).expect("parses");
    let pool = k.pool.as_ref().expect("the pool");
    assert_eq!(pool.faults, vec!["kill@1.6.1:3".to_owned()]);
    assert_eq!(pool.fired, vec![("kill@1.6.1:3".to_owned(), 1)]);
    assert_eq!(
        (
            pool.count("lost"),
            pool.count("retries"),
            pool.count("leases")
        ),
        (Some(1), Some(1), Some(63))
    );
    assert_eq!((pool.window_ms, pool.killed), (Some(10_000), Some(1)));
}

#[test]
fn a_summary_without_its_header_or_result_or_with_a_stray_line_does_not_parse() {
    assert!(parse_summary("").is_err());
    assert!(parse_summary(A.lines().take(5).collect::<Vec<_>>().join("\n").as_str()).is_err());
    assert!(parse_summary(&A.replace("aos-search policy=", "aos-search polic=")).is_err());
    assert!(parse_summary(&format!("  counts proposed=1\n{A}")).is_err());
    assert!(parse_summary(&A.replace("useful=true", "useful=maybe")).is_err());
}

#[test]
fn the_load_average_is_the_first_of_sysctls_three() {
    assert_eq!(parse_loadavg("{ 2.85 3.10 2.97 }\n"), Some(2.85));
    assert_eq!(parse_loadavg(""), None);
}

// --- Gate 0.

#[test]
fn gate0_holds_for_one_clean_commit_on_a_debug_lab_and_fails_otherwise() {
    assert!(gate0(Some(BUILD), LAB, true).is_empty());
    let dirty = gate0(Some("aterm-lab_0.128.0+gd97369e77400-dirty"), LAB, true);
    assert!(dirty.iter().any(|w| w.contains("-dirty")), "{dirty:?}");
    assert!(!gate0(Some("aterm-lab_0.128.0+gunknown"), LAB, true).is_empty());
    let unknown = LabFacts { debug: None, ..LAB };
    assert!(gate0(Some(BUILD), unknown, true)[0].contains("profile is unknown"));
    let changed = LabFacts {
        unchanged: Some(false),
        ..LAB
    };
    assert!(gate0(Some(BUILD), changed, true)[0].contains("changed"));
    let unread = LabFacts {
        unchanged: None,
        ..LAB
    };
    assert!(
        gate0(Some(BUILD), unread, false).is_empty(),
        "not yet re-read"
    );
    assert!(!gate0(Some(BUILD), unread, true).is_empty());
    let runs = session(1, |r| {
        r.m.stdout = r.m.stdout.replace(BUILD, &format!("{BUILD}-dirty"));
        r.summary = parse_summary(&r.m.stdout);
    });
    let ev = evaluate_clean(&runs, 1, LAB, true);
    assert!(void_reasons(&ev).iter().any(|w| w.starts_with("gate 0:")));
}

// --- Gate 1.

#[test]
fn gate1_passes_the_m4_records_figures_and_names_each_mismatch() {
    assert_eq!(
        gate1(&parse_summary(&clean(A)).expect("parses")),
        Vec::<String>::new()
    );
    for (from, to, what) in [
        ("index=6", "index=5", "round 0 index 6"),
        ("admitted=18", "admitted=17", "grid admitted 18"),
        ("NoStepSlept=9", "NoStepSlept=8", "grid refused"),
        ("T=5119 Z=161", "T=5120 Z=161", "grid held out"),
        ("gain_ppm=234255", "gain_ppm=234256", "grid held out"),
        ("config=e5463d8e4c08", "config=000000000000", "plans config"),
        ("admitted=7", "admitted=6", "plans admitted 7"),
        ("index=7", "index=8", "plans winner at index 7"),
    ] {
        let s = parse_summary(&A.replacen(from, to, 1)).expect("parses");
        let miss = gate1(&s);
        assert!(
            miss.iter().any(|m| m.contains(what)),
            "{from} -> {to}: {miss:?}"
        );
    }
    let claimed = parse_summary(P8).expect("parses");
    assert!(
        gate1(&claimed).iter().any(|m| m.contains("judged here")),
        "a count resting on claims is not the record's"
    );
}

#[test]
fn an_a_run_that_does_not_reproduce_the_record_voids_the_session() {
    let runs = session(1, |r| {
        if r.arm == Arm::A {
            r.m.stdout = r.m.stdout.replacen("index=6", "index=5", 1);
            r.summary = parse_summary(&r.m.stdout);
        }
    });
    let ev = evaluated(&runs, 1);
    let v = void_reasons(&ev);
    assert!(v.iter().any(|w| w.starts_with("gate 1: c01.1")), "{v:?}");
}

// --- Gate 2.

fn defect_of(r: &Run) -> String {
    match r.judged.as_ref().map(|j| &j.verdict) {
        Some(Verdict::Defect(d)) => d.clone(),
        other => panic!("not a defect: {other:?}"),
    }
}

#[test]
fn gate2_a_void_an_unreaped_worker_a_failing_exit_or_a_c0_mismatch_is_a_defect_that_stops() {
    let void = run_with(
        Arm::P8,
        1,
        5,
        &clean(&P8.replace(
            "result=pass searches=2 winners=2",
            "result=void refusal=Contradicted searches=2 winners=0",
        )),
        1.0,
        1.0,
    );
    assert!(defect_of(&void).contains("result=void refusal=Contradicted"));
    let unreaped = run_with(
        Arm::P8,
        1,
        5,
        &clean(&P8.replace("reaped=8", "reaped=7")),
        1.0,
        1.0,
    );
    assert!(defect_of(&unreaped).contains("reaped=7 spawned=8"));
    let mut failed = honest(Arm::T4, 1, 2);
    failed.m.exit = Some(1);
    failed.judged = judge(&failed, Some(BUILD));
    assert!(defect_of(&failed).contains("exit:1"));
    let mut wrong_arm = honest(Arm::P8, 1, 5);
    wrong_arm.arm = Arm::P4;
    wrong_arm.judged = judge(&wrong_arm, Some(BUILD));
    assert!(defect_of(&wrong_arm).contains("not the arm"));
    let mut mismatch = honest(Arm::P8, 1, 5);
    mismatch.parity = Some(vec![Parity::Equal, Parity::Differs(vec!["gain_ppm"])]);
    mismatch.judged = judge(&mismatch, Some(BUILD));
    assert_eq!(defect_of(&mismatch), "C0: plans differs(gain_ppm)");
    let mut unread = honest(Arm::A, 1, 1);
    unread.summary = Err("no `aos-search result=` line".into());
    unread.judged = judge(&unread, Some(BUILD));
    assert!(defect_of(&unread).contains("does not parse"));
    let runs = session(1, |r| {
        if r.arm == Arm::P8 {
            r.parity = Some(vec![
                Parity::Mode {
                    found: Mode::Search,
                    want: Mode::Distributed,
                },
                Parity::Equal,
            ]);
        }
    });
    let ev = evaluated(&runs, 1);
    match &ev.outcome {
        Outcome::Stopped(d) => assert!(d.contains("C0: grid mode=search want=distributed"), "{d}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(defect_in(&runs, Phase::Clean), ev.defect);
}

#[test]
fn gate2_a_run_waits_for_its_reference_and_a_run_never_compared_voids() {
    let mut r = honest(Arm::T8, 1, 4);
    r.parity = None;
    assert_eq!(judge(&r, Some(BUILD)), None, "C0 waits for A's bundles");
    let runs = session(1, |r| r.parity = None);
    let v = void_reasons(&evaluated(&runs, 1));
    assert!(
        v.iter()
            .any(|w| w.starts_with("gate 2: 6 runs were never compared")),
        "{v:?}"
    );
}

// --- Gate 3.

#[test]
fn gate3_a_clean_p_run_that_counts_a_lost_worker_leaves_its_round_out() {
    let p = parse_summary(P8).expect("parses").pool.expect("pool");
    assert!(gate3(&p).is_empty());
    let k = parse_summary(KILL).expect("parses").pool.expect("pool");
    assert_eq!(
        gate3(&k),
        vec![
            "lost=1".to_owned(),
            "retries=1".to_owned(),
            "faults armed kill@1.6.1:3".to_owned(),
            "faults fired kill@1.6.1:3:1".to_owned()
        ]
    );
    let runs = session(12, |r| {
        if r.round == 3 && r.arm == Arm::P4 {
            r.m.stdout = clean(&four_workers(P8).replace("stale=0", "stale=1"));
            r.summary = parse_summary(&r.m.stdout);
        }
    });
    let ev = evaluated(&runs, 12);
    assert_eq!(
        ev.gate3_out,
        vec!["c03.2 P4: stale=1".to_owned()],
        "round 3 is T8 P4 T4 P8 A P8A"
    );
    assert_eq!(ev.valid_rounds.len(), 11);
    assert!(!ev.valid_rounds.contains(&3));
    assert_eq!(ev.outcome, Outcome::Valid, "11 of 12 rounds valid");
}

// --- Gate 4.

#[test]
fn gate4_a_wall_times_spanning_more_than_1_3x_void_the_session() {
    let within = session(12, |r| {
        if r.arm == Arm::A && r.round == 5 {
            r.m.wall_ms = 48_000.0 * 1.3;
        }
    });
    assert_eq!(evaluated(&within, 12).outcome, Outcome::Valid);
    let over = session(12, |r| {
        if r.arm == Arm::A && r.round == 5 {
            r.m.wall_ms = 48_000.0 * 1.31;
        }
    });
    let v = void_reasons(&evaluated(&over, 12));
    assert!(
        v.iter()
            .any(|w| w.starts_with("gate 4: A's wall times span 1.310x")),
        "{v:?}"
    );
    assert_eq!(span_ratio(&[]), None);
}

// --- Gate 5.

#[test]
fn gate5_the_deadline_and_the_memory_budget_each_leave_a_run_out() {
    let mut late = honest(Arm::P8, 2, 5);
    late.m.deadline = true;
    late.judged = judge(&late, Some(BUILD));
    assert_eq!(
        late.judged.as_ref().map(|j| j.verdict.clone()),
        Some(Verdict::Invalid(vec![(
            Gate::G5,
            "killed at the 180 s deadline".to_owned()
        )]))
    );
    let slow = Measured {
        wall_ms: 180_001.0,
        maxrss: Some(MIB),
        ..Measured::default()
    };
    assert!(gate5(&slow, None)[0].contains("past the 180 s deadline"));
    let p = parse_summary(P8).expect("parses").pool.expect("pool");
    let fits = Measured {
        maxrss: Some(RSS_BUDGET - 1089 * MIB + 1),
        ..Measured::default()
    };
    assert!(
        gate5(&fits, Some(&p)).is_empty(),
        "1088 MiB summed, floored"
    );
    let over = Measured {
        maxrss: Some(RSS_BUDGET - 1089 * MIB + 2),
        ..Measured::default()
    };
    assert_eq!(gate5(&over, Some(&p)).len(), 1);
    assert_eq!(rss_bound(10, Some(0)), 10 + MIB - 1);
    assert_eq!(rss_bound(10, None), 10);
    let runs = session(12, |r| {
        if r.round == 7 && r.arm == Arm::T8 {
            r.m.maxrss = Some(RSS_BUDGET + 1);
        }
    });
    let ev = evaluated(&runs, 12);
    assert_eq!(ev.gate5_out.len(), 1);
    assert!(!ev.valid_rounds.contains(&7));
}

// --- Gate 6.

#[test]
fn gate6_ten_of_twelve_valid_rounds_hold_and_nine_do_not() {
    assert_eq!(required_valid(12), 10);
    assert_eq!(required_valid(1), 1);
    assert_eq!(required_valid(6), 5);
    let late = |rounds: &'static [usize]| {
        session(12, move |r| {
            if rounds.contains(&r.round) && r.arm == Arm::T4 {
                r.m.deadline = true;
            }
        })
    };
    let ten = evaluated(&late(&[2, 9]), 12);
    assert_eq!(
        (ten.valid_rounds.len(), &ten.outcome),
        (10, &Outcome::Valid)
    );
    let nine = evaluated(&late(&[2, 9, 11]), 12);
    let v = void_reasons(&nine);
    assert_eq!(v, vec!["gate 6: 9 of 12 rounds valid, under 10".to_owned()]);
}

// --- C0, on a real bundle.

fn bundle() -> UncheckedBundle {
    static B: std::sync::OnceLock<UncheckedBundle> = std::sync::OnceLock::new();
    B.get_or_init(|| {
        use aterm_effects::aos_momentum::m4::Evaluator;
        use aterm_effects::aos_momentum::search::FileProposer;
        let ev = Evaluator::v1("aterm-lab 0.0.0+g0123456789ab", Instant::now())
            .expect("the pinned policy");
        let mut proposer = FileProposer::from_text("010100000000000000000000\n");
        let selection = ev.select(&mut proposer).expect("a winner");
        let mut admitted = ev.held_out(selection).expect("the held-out read");
        assert!(admitted.record_timing("t", Duration::from_millis(5)));
        UncheckedBundle::decode(&admitted.into_bundle().encode()).expect("decodes")
    })
    .clone()
}

#[test]
fn c0_sets_aside_the_timings_the_toolchain_line_and_the_mode_and_nothing_else() {
    let a = bundle();
    assert_eq!(a.selection.mode, Mode::Search);
    assert_eq!(parity(&a, &a, Mode::Search), Parity::Equal);
    let mut b = a.clone();
    b.timings.clear();
    b.toolchain = "another line".into();
    b.selection.mode = Mode::Distributed;
    assert_eq!(parity(&b, &a, Mode::Distributed), Parity::Equal);
    assert_eq!(
        parity(&b, &a, Mode::Search),
        Parity::Mode {
            found: Mode::Distributed,
            want: Mode::Search
        }
    );
    let mut c = a.clone();
    c.selection.gain_ppm += 1;
    c.index += 1;
    assert_eq!(
        parity(&c, &a, Mode::Search),
        Parity::Differs(vec!["index", "gain_ppm"])
    );
    let mut d = a.clone();
    d.selection.stats.admitted += 1;
    assert_eq!(parity(&d, &a, Mode::Search), Parity::Differs(vec!["stats"]));
    assert_eq!(
        compare(
            &[Ok(a.clone()), Err("absent".into())],
            &[Ok(a.clone()), Ok(a)],
            Mode::Search
        ),
        vec![Parity::Equal, Parity::Unread("absent".into())]
    );
}

#[test]
fn settle_takes_the_first_reproducing_a_run_as_the_reference_and_judges_the_waiting_runs() {
    let b = bundle();
    let both = || vec![Ok(b.clone()), Ok(b.clone())];
    let mut runs = vec![
        honest(Arm::T4, 1, 1),
        honest(Arm::P8, 1, 2),
        honest(Arm::A, 1, 3),
    ];
    for r in &mut runs {
        r.parity = None;
        r.judged = None;
    }
    let bundles = vec![both(), both(), vec![Ok(b.clone()), Err("absent".into())]];
    settle(&mut runs, &bundles, Phase::Clean);
    assert!(runs.iter().all(|r| r.judged.is_none()), "no reference yet");
    let bundles = vec![both(), both(), both()];
    assert_eq!(reference(&runs, &bundles, Phase::Clean), Some(2));
    assert_eq!(reference(&runs, &bundles, Phase::Fault), None);
    settle(&mut runs, &bundles, Phase::Clean);
    assert_eq!(
        runs[0].judged.as_ref().map(|j| j.verdict.clone()),
        Some(Verdict::Valid)
    );
    assert_eq!(runs[2].judged.as_ref().map(|j| j.uar), Some(2));
    assert!(
        defect_of(&runs[1]).contains("mode=search want=distributed"),
        "a P8 bundle in mode search breaks the mode rule"
    );
}

// --- The criteria.

#[test]
fn c1_c2_c3_are_shown_when_they_hold_in_every_valid_round() {
    let runs = session(12, |_| {});
    let ev = evaluated(&runs, 12);
    assert_eq!(ev.outcome, Outcome::Valid);
    let c = criteria(&runs, &ev.valid_rounds);
    let keys: Vec<&str> = c.iter().map(|c| c.key).collect();
    assert_eq!(keys, ["C1", "C2", "C3"]);
    for c in &c {
        assert_eq!(
            (c.holds(), c.pairs.len(), c.shown()),
            (12, 12, true),
            "{}",
            c.key
        );
    }
    assert_eq!(c[2].figure, Figure::Cpu);
    assert_eq!(c[0].pairs[0], (1, 26_000.0, 48_000.0));
}

#[test]
fn a_criterion_failing_in_one_valid_round_is_not_shown() {
    let runs = session(12, |r| {
        if r.round == 4 && r.arm == Arm::P8 {
            r.m.wall_ms = 19_000.0;
            r.m.cpu_ms = Some(171_000.0);
        }
    });
    let ev = evaluated(&runs, 12);
    let c = criteria(&runs, &ev.valid_rounds);
    assert!(c[0].shown(), "C1: P8 still below A");
    assert_eq!(
        (c[1].holds(), c[1].shown()),
        (11, false),
        "C2: T8 above P8 in round 4"
    );
    assert_eq!(
        (c[2].holds(), c[2].shown()),
        (11, false),
        "C3: P8 CPU above P8A's in round 4"
    );
    assert!(
        !criteria(&runs, &[])[0].shown(),
        "no valid round shows nothing"
    );
}

#[test]
fn a_left_out_round_is_not_paired() {
    let runs = session(12, |r| {
        if r.round == 6 {
            if r.arm == Arm::A {
                r.m.deadline = true;
            }
            if r.arm == Arm::P8 {
                r.m.wall_ms = 90_000.0;
            }
        }
    });
    let ev = evaluated(&runs, 12);
    let c = criteria(&runs, &ev.valid_rounds);
    assert_eq!((c[0].pairs.len(), c[0].shown()), (11, true));
}

// --- The fault session.

fn fault_text(case: FaultCase) -> String {
    let moved = match case {
        FaultCase::Kill => KILL.to_owned(),
        FaultCase::Hang => KILL
            .replace("lost=1", "lost=0")
            .replace("expired=0", "expired=1"),
        FaultCase::Slow => KILL
            .replace("lost=1", "lost=0")
            .replace("stale=0", "stale=1")
            .replace("late_progress=0", "late_progress=1"),
        FaultCase::Dup => KILL
            .replace("lost=1", "lost=0")
            .replace("duplicate=0", "duplicate=1"),
    };
    clean(&moved.replace("kill@1.6.1:3", case.spec()))
}

fn fault_runs(edit: impl Fn(FaultCase, &mut Run)) -> Vec<Run> {
    let mut runs = session(12, |_| {});
    let mut a = honest(Arm::A, 0, 1);
    a.phase = Phase::Fault;
    a.id = run_id(Phase::Fault, 0, 1);
    a.judged = judge(&a, Some(BUILD));
    runs.push(a);
    for n in 1..=FAULT_ROUNDS {
        for (p, case) in FaultCase::ALL.iter().enumerate() {
            let mut r = run_with(Arm::P8, n, p + 1, &fault_text(*case), 40_000.0, 160_000.0);
            r.phase = Phase::Fault;
            r.id = run_id(Phase::Fault, n, p + 1);
            r.fault = Some(*case);
            edit(*case, &mut r);
            runs.push(r);
        }
    }
    rejudge(&mut runs);
    runs
}

#[test]
fn each_fault_must_fire_once_and_move_its_counter() {
    for case in FaultCase::ALL {
        let p = parse_summary(&fault_text(case))
            .expect("parses")
            .pool
            .expect("pool");
        assert!(fault_shown(case, &p).is_empty(), "{case:?}");
    }
    let kill = parse_summary(KILL).expect("parses").pool.expect("pool");
    let hang = fault_shown(FaultCase::Hang, &kill);
    assert!(
        hang.contains(&"hang@1.6.1:3 did not fire".to_owned()),
        "{hang:?}"
    );
    assert!(
        hang.contains(&"other faults fired: kill@1.6.1:3".to_owned()),
        "{hang:?}"
    );
    assert!(hang.iter().any(|w| w.starts_with("expired=0")), "{hang:?}");
    let twice = parse_summary(&KILL.replace("kill@1.6.1:3=1", "kill@1.6.1:3=2")).expect("parses");
    assert_eq!(
        fault_shown(FaultCase::Kill, twice.pool.as_ref().expect("pool")),
        vec!["kill@1.6.1:3 fired 2 times".to_owned()]
    );
}

#[test]
fn the_fault_session_shows_every_fault_and_discloses_its_109_builds() {
    let runs = fault_runs(|_, _| {});
    let clean_ev = evaluated(&runs, 12);
    assert_eq!(clean_ev.outcome, Outcome::Valid);
    let f = evaluate_faults(&runs, FAULT_ROUNDS, &clean_ev);
    assert_eq!(f.outcome, FaultOutcome::Shown);
    assert_eq!(f.clean_p8_wall, Some(26_000.0));
    for row in &f.rows {
        assert!(row.shown, "{:?}", row.case);
        assert_eq!(row.added_wall, vec![14_000.0; 3]);
        assert_eq!(row.added_cpu, vec![10_000.0; 3]);
        assert_eq!(row.extra_leases, vec![1; 3]);
    }
    let d = disclosed(&runs, Phase::Fault);
    assert_eq!(
        (d.builds, d.gate_reads, d.verify_reads, d.by_arm),
        (109, 26, 26, 0)
    );
    let c = disclosed(&runs, Phase::Clean);
    assert_eq!((c.builds, c.gate_reads), (12 * BUILDS_PER_ROUND, 144));
}

#[test]
fn a_fault_that_did_not_show_in_one_run_is_not_shown() {
    let runs = fault_runs(|case, r| {
        if case == FaultCase::Dup && r.round == 2 {
            r.m.stdout = r.m.stdout.replace("duplicate=1", "duplicate=0");
            r.summary = parse_summary(&r.m.stdout);
        }
    });
    let f = evaluate_faults(&runs, FAULT_ROUNDS, &evaluated(&runs, 12));
    assert_eq!(
        f.outcome,
        FaultOutcome::NotShown(vec!["dup@1.6.1".to_owned()])
    );
    let dup = f
        .rows
        .iter()
        .find(|r| r.case == FaultCase::Dup)
        .expect("dup");
    assert_eq!(dup.runs[1].0, "f02.4");
    assert!(
        dup.runs[1].1[0].starts_with("fault duplicate=0"),
        "{:?}",
        dup.runs[1]
    );
}

#[test]
fn a_fault_runs_c0_mismatch_stops_the_fault_session() {
    let runs = fault_runs(|case, r| {
        if case == FaultCase::Slow && r.round == 1 {
            r.parity = Some(vec![Parity::Differs(vec!["rows"]), Parity::Equal]);
        }
    });
    let f = evaluate_faults(&runs, FAULT_ROUNDS, &evaluated(&runs, 12));
    match &f.outcome {
        FaultOutcome::Stopped(d) => assert!(
            d.starts_with("f01.3 P8 slow@1.6.1:3: C0: grid differs(rows)"),
            "{d}"
        ),
        other => panic!("{other:?}"),
    }
}

// --- The record.

#[test]
fn a_smoke_records_stable_lines_say_smoke_and_make_no_claim() {
    let runs = session(1, |_| {});
    let (text, code) = render(&header(1, FaultPlan::NotAsked), &runs, State::Done);
    assert_eq!(code, 0, "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        "aos-workers-bench record v1: design §11.8, pre-registered; every figure MEASURED (never scored)"
    );
    assert_eq!(
        lines[1],
        "session kind=smoke rounds=1 registered_rounds=12 faults=no (SMOKE: NOT the registered \
         session, which runs 12 rounds; no claim is made from it)"
    );
    assert_eq!(
        lines[2],
        "lab path=/lab/aterm-lab digest=0123456789ab runner_is_lab=yes runner_profile=debug_opt-level=0 \
         debug=yes unchanged=yes"
    );
    for want in [
        "arms A=[none] T4=[--threads 4] T8=[--threads 8] P4=[--workers 4] P8=[--workers 8] \
         P8A=[--workers 8 --check all]",
        "order round=1 A T4 P8A T8 P8 P4",
        "verdict c01.1 A valid c0=grid:equal,plans:equal uar=2 gate1=pass",
        "round 1 complete=yes valid=yes load1(paired, not gated) A=2.50 T4=2.50 P8A=2.50 T8=2.50 \
         P8=2.50 P4=2.50",
        "  gate 0 one clean commit and one debug lab binary: pass build=aterm-lab_0.128.0+gd97369e77400",
        "  gate 4 A's wall times across the valid rounds span at most 1.3x: pass (span=1.000x \
         min_ms=48000.0 max_ms=48000.0 over 1 valid rounds)",
        "  gate 6 at least 1 of 1 rounds valid: pass (valid=1 [1] complete=1 of 1)",
        "session clean result=valid",
        "  C1 P8 wall_ms < A wall_ms: shown (holds in 1 of 1 valid rounds) | P8 median=26000.0 \
         range=26000.0..26000.0 n=1 | A median=48000.0 range=48000.0..48000.0 n=1",
        "    claim \"Process workers reach useful accepted results sooner than serial search on this \
         Mac\": not made (a smoke run is not the registered session)",
        "  arm=A wall_ms median=48000.0 range=48000.0..48000.0 n=1 | uar_per_s median=0.0417 \
         range=0.0417..0.0417 n=1 | uar_per_cpu_s median=0.0426 range=0.0426..0.0426 n=1 | cpu_ms \
         median=47000.0 range=47000.0..47000.0 n=1 | maxrss_mib median=300.0 range=300.0..300.0 n=1 \
         | uar median=2 range=2..2 n=1 | load1 median=2.50 range=2.50..2.50 n=1",
        "  stepped grid under A, T4 and T8: 3 of 3 runs equal the M4 record's 592060 (P runs print \
         stepped_here and stepped_claimed instead)",
        "aos-workers-bench result=valid kind=smoke exit=0",
    ] {
        assert!(lines.contains(&want), "missing `{want}` in:\n{text}");
    }
    let run = lines
        .iter()
        .find(|l| l.starts_with("run c01.5 "))
        .expect("P8's run line");
    for part in [
        "arm=P8 fault=none out=runs/c01.5-P8 MEASURED (never scored) load1=2.50 wall_ms=26000.0 \
         cpu_ms=150000.0 maxrss_bytes=314572800 status=exit:0",
        "grid config=c4b618825d6c proposed=54 distinct=54 admitted_claimed=18",
        "check_ms=53:532.5,17:4634.8",
        "pool n=8 window_ms=60000 faults=none joined=8 leases=62 retries=0",
        "peak_rss_mb_sum=1088",
    ] {
        assert!(run.contains(part), "missing `{part}` in {run}");
    }
}

#[test]
fn a_registered_valid_session_makes_its_claims_and_a_void_one_exits_1() {
    let runs = session(12, |_| {});
    let (text, code) = render(&header(12, FaultPlan::NotAsked), &runs, State::Done);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("session kind=registered rounds=12 registered_rounds=12 faults=no\n"));
    assert!(text.contains("    claim \"Threads beat process workers on this Mac\": made\n"));
    let dirty = session(12, |r| {
        r.m.stdout = r.m.stdout.replace(BUILD, &format!("{BUILD}-dirty"));
        r.summary = parse_summary(&r.m.stdout);
    });
    let (text, code) = render(&header(12, FaultPlan::NotAsked), &dirty, State::Done);
    assert_eq!(code, 1);
    assert!(text.contains("not made (the session is void)"), "{text}");
    assert!(
        text.lines()
            .next_back()
            .is_some_and(|l| l == "aos-workers-bench result=void kind=registered exit=1")
    );
    let (_, code) = render(
        &header(12, FaultPlan::NotAsked),
        &runs,
        State::Interrupted(2),
    );
    assert_eq!(code, 130);
    let (_, code) = render(&header(12, FaultPlan::Pending), &runs, State::Running);
    assert_eq!(code, 1);
}

#[test]
fn a_session_with_faults_exits_0_only_when_every_fault_shows() {
    let runs = fault_runs(|_, _| {});
    let (text, code) = render(&header(12, FaultPlan::Ran(None)), &runs, State::Done);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains(
        "  disclosed (fault session, counted from the 13 runs made): this session builds 109 evaluators"
    ), "{text}");
    assert!(text.contains("fault session result=shown"), "{text}");
    let missing = fault_runs(|case, r| {
        if case == FaultCase::Hang && r.round == 3 {
            r.m.deadline = true;
        }
    });
    let (text, code) = render(&header(12, FaultPlan::Ran(None)), &missing, State::Done);
    assert_eq!(code, 1);
    assert!(
        text.contains("fault session result=not-shown (hang@1.6.1:3)"),
        "{text}"
    );
    let (text, code) = render(
        &header(12, FaultPlan::Skipped("the clean session is void".into())),
        &session(12, |_| {}),
        State::Done,
    );
    assert_eq!(code, 1);
    assert!(text.contains("fault session: not run (the clean session is void)"));
}

#[test]
fn the_statistics_are_the_median_and_the_range() {
    assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
    assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), Some(2.5));
    assert_eq!(median(&[]), None);
    assert_eq!(span(&[3.0, 1.0, 2.0]), Some((1.0, 3.0)));
    assert_eq!(stat(&[1.0, 2.0], 1), "median=1.5 range=1.0..2.0 n=2");
    assert_eq!(stat(&[], 1), "none");
}
