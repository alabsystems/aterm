// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn box_id(key: &str) -> MachineId {
    MachineId {
        key: key.to_string(),
        shape: "Test-Chip-8c".to_string(),
        identities: vec![format!("host:{key}")],
        claimed: true,
    }
}

fn judged(ledger: &str, key: &str, samples: &[TrendSample]) -> TrendJudgment {
    judge_trend(ledger, "", &box_id(key), samples)
}

fn rows_for(date: &str, sha: &str, key: &str, samples: &[TrendSample]) -> String {
    trend_rows(date, sha, &box_id(key), samples, &[])
}

#[test]
fn interrupted_trend_publication_preserves_history() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("trend.tsv");
    let aliases = dir.path().join("boxes.tsv");
    let me = box_id("m21");
    let samples = [sample("throughput", "median_mbps", 1000.0)];
    gate_trend_at(&path, &aliases, &me, &samples, true, &[], "test-tree");
    let old = std::fs::read(&path).unwrap();
    assert!(
        gate_trend_using(
            &path,
            &aliases,
            &me,
            &samples,
            true,
            &[],
            "test-tree",
            super::super::publication::interrupted
        )
        .failed()
    );
    assert_eq!(std::fs::read(&path).unwrap(), old);
}

#[test]
fn failed_trend_publication_is_a_failing_lane() {
    let world = aterm_tempfile::tempdir().unwrap();
    let aliases = world.path().join("boxes.tsv");
    let me = box_id("m21");
    let samples = [sample("throughput", "median_mbps", 1000.0)];
    let writable = world.path().join("trend.tsv");
    let seeded = gate_trend_at(&writable, &aliases, &me, &samples, true, &[], "test-tree");
    assert!(matches!(seeded.status, super::super::Status::NotJudged(_)));
    assert_eq!(
        trend_ledger_rows(&std::fs::read_to_string(&writable).unwrap()).len(),
        1
    );
    let unavailable = world.path().join("missing-parent").join("trend.tsv");
    let failed = gate_trend_at(
        &unavailable,
        &aliases,
        &me,
        &samples,
        true,
        &[],
        "test-tree",
    );
    assert!(!unavailable.exists());
    assert!(matches!(failed.status, super::super::Status::Fail(_)));
    assert_eq!(super::super::verdict(&[("trend", &failed, 0.0)]).1, 1);
}

#[test]
fn unavailable_trend_inputs_do_not_replace_existing_history() {
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("trend.tsv");
    let aliases = world.path().join("boxes.tsv");
    let me = box_id("m21");
    let samples = [sample("throughput", "median_mbps", 1000.0)];
    // The real gate can seed an absent ledger and then retain its valid history.
    gate_trend_at(&path, &aliases, &me, &samples, true, &[], "test-tree");
    let first = std::fs::read_to_string(&path).unwrap();
    assert_eq!(trend_ledger_rows(&first).len(), 1);
    gate_trend_at(&path, &aliases, &me, &samples, true, &[], "test-tree");
    let second = std::fs::read_to_string(&path).unwrap();
    assert!(second.starts_with(&first));
    assert_eq!(trend_ledger_rows(&second).len(), 2);
    let corrupt = [0xff, 0xfe];
    std::fs::write(&path, corrupt).unwrap();
    let run = gate_trend_at(&path, &aliases, &me, &samples, true, &[], "test-tree");
    assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    assert!(matches!(run.status, super::super::Status::Fail(_)));
    std::fs::write(&path, &second).unwrap();
    std::fs::write(&aliases, corrupt).unwrap();
    let run = gate_trend_at(&path, &aliases, &me, &samples, true, &[], "test-tree");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), second);
    assert!(matches!(run.status, super::super::Status::Fail(_)));
    #[cfg(unix)]
    {
        std::fs::remove_file(&aliases).unwrap();
        let target = world.path().join("history.tsv");
        std::fs::rename(&path, &target).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let run = gate_trend_at(&path, &aliases, &me, &samples, true, &[], "test-tree");
        let appended = std::fs::read_to_string(&target).unwrap();
        assert!(appended.starts_with(&second));
        assert_eq!(trend_ledger_rows(&appended).len(), 3);
        assert!(matches!(run.status, super::super::Status::Pass));
        std::fs::write(&target, corrupt).unwrap();
        let run = gate_trend_at(&path, &aliases, &me, &samples, true, &[], "test-tree");
        assert_eq!(std::fs::read(&target).unwrap(), corrupt);
        assert!(matches!(run.status, super::super::Status::Fail(_)));
    }
}

#[test]
fn appending_preserves_an_unterminated_final_ledger_row() {
    let samples = [sample("throughput", "median_mbps", 1000.0)];
    let first = rows_for("2026-11-03", "before", "m21", &samples);
    let next = rows_for("2026-11-04", "after", "m21", &samples);
    let mut empty = String::new();
    append_trend_rows(&mut empty, &next);
    assert_eq!(empty, next);
    for initial in [first.clone(), first.trim_end_matches('\n').to_string()] {
        let mut ledger = initial;
        assert_eq!(trend_ledger_rows(&ledger).len(), 1);
        append_trend_rows(&mut ledger, &next);
        let (rows, dropped) = trend_ledger_rows_counted(&ledger);
        assert!(dropped.is_empty());
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.shape == "Test-Chip-8c"));
        let judgment = judged(&ledger, "m21", &samples);
        assert!(judgment.conflicts.is_empty());
        assert!(judgment.breaches.is_empty());
        assert!(judgment.skipped.is_empty());
    }
}

fn sample(lane: &'static str, metric: &str, value: f64) -> TrendSample {
    TrendSample {
        lane,
        metric: metric.to_string(),
        value,
        inverted: false,
        at: Vec::new(),
    }
}

fn gpu_point(ref_us: f64, blank_us: f64) -> Vec<PointFigure> {
    vec![
        PointFigure {
            name: "ref_gpu_p50_us",
            value: ref_us,
        },
        PointFigure {
            name: "ref_blank_gpu_p50_us",
            value: blank_us,
        },
    ]
}

fn gpu_sample(metric: &str, value: f64, ref_us: f64, blank_us: f64) -> TrendSample {
    TrendSample {
        at: gpu_point(ref_us, blank_us),
        ..sample("effects", metric, value)
    }
}

fn inverted_sample(lane: &'static str, metric: &str, value: f64) -> TrendSample {
    TrendSample {
        inverted: true,
        ..sample(lane, metric, value)
    }
}

#[test]
fn only_a_healthy_run_joins_the_ledger_and_a_re_record_excuses_only_its_lane() {
    let breach = |lane: &'static str| Breach {
        lane,
        metric: format!("{lane}/m"),
        value: 1.0,
        bound: 2.0,
        best: 3.0,
        inverted: false,
    };
    let judgment = |breaches: Vec<Breach>, conflicts: Vec<(String, String)>| TrendJudgment {
        breaches,
        skipped: Vec::new(),
        elsewhere: Vec::new(),
        conflicts,
    };
    let clean = judgment(Vec::new(), Vec::new());
    assert!(may_append(true, &clean, &[]));
    assert!(!may_append(false, &clean, &["restore"]));
    let regressed = judgment(vec![breach("throughput")], Vec::new());
    assert!(!may_append(true, &regressed, &["restore"]));
    assert!(!may_append(true, &regressed, &[]));
    assert!(may_append(true, &regressed, &["throughput"]));
    let merged = judgment(Vec::new(), vec![("m15".into(), "Apple M4".into())]);
    assert!(!may_append(true, &merged, &["throughput"]));
}

#[test]
fn a_re_record_starts_its_lanes_window() {
    let old: String = (0..4)
        .map(|_| {
            rows_for(
                "2026-10-19",
                "a",
                "boxA",
                &[
                    sample("effects", "m", 1000.0),
                    sample("search", "q", 1000.0),
                ],
            )
        })
        .collect();
    let new = [sample("effects", "m", 500.0), sample("search", "q", 500.0)];
    let me = box_id("boxA");
    let marked = trend_rows("2026-10-20", "b", &me, &new, &["effects"]);
    assert!(
        marked
            .lines()
            .next()
            .is_some_and(|l| l.ends_with("\trerecorded")),
        "{marked}"
    );
    let parsed = trend_ledger_rows(&marked);
    assert_eq!(
        parsed.iter().map(|r| r.rerecorded).collect::<Vec<_>>(),
        [true, false],
        "only the re-recorded lane's rows carry the mark"
    );
    let next = [sample("effects", "m", 450.0), sample("search", "q", 450.0)];
    let after = judged(&format!("{TREND_HEADER}{old}{marked}"), "boxA", &next);
    assert_eq!(
        after
            .breaches
            .iter()
            .map(|b| b.metric.as_str())
            .collect::<Vec<_>>(),
        ["search/q"],
        "450 >= 0.70 x 500 for the re-recorded lane; the other still holds 1000"
    );
    let unmarked = trend_rows("2026-10-20", "b", &me, &new, &[]);
    let control = judged(&format!("{old}{unmarked}"), "boxA", &next);
    assert_eq!(control.breaches.len(), 2, "CONTROL: 450 < 0.70 x 1000");
    assert!(!trend_ledger_rows(&old)[0].rerecorded);
}

#[test]
fn a_gpu_ratio_is_judged_only_against_rows_at_its_operating_point() {
    let seed =
        "2026-10-22\ta1d868260ffb\tboxA\teffects/episode_gpu_rel\t2.884\ttrust\tTest-Chip-8c\n";
    let fast = rows_for(
        "2026-10-23",
        "b",
        "boxA",
        &[gpu_sample("episode_gpu_rel", 2.842, 195.3, 80.4)],
    );
    let ledger = format!("{TREND_HEADER}{seed}{fast}");
    let now = [gpu_sample("episode_gpu_rel", 1.80, 535.7, 367.5)];
    let j = judged(&ledger, "boxA", &now);
    assert!(j.breaches.is_empty(), "{:?}", j.breaches);
    assert!(j.skipped.is_empty(), "history exists: {:?}", j.skipped);
    assert_eq!(
        j.elsewhere,
        [Elsewhere {
            metric: "effects/episode_gpu_rel".to_string(),
            at: gpu_point(535.7, 367.5),
            rows: 2,
        }]
    );
    let control = judged(
        &ledger,
        "boxA",
        &[sample("effects", "episode_gpu_rel", 1.80)],
    );
    assert_eq!(control.breaches.len(), 1, "CONTROL: 1.80 < 0.70 x 2.884");
    assert!(control.elsewhere.is_empty());
    let fresh = judged("", "boxA", &now);
    assert_eq!(
        fresh.skipped,
        ["effects/episode_gpu_rel"],
        "no history at all"
    );
    assert!(fresh.breaches.is_empty(), "a first run never blocks");
    assert!(fresh.elsewhere.is_empty());
}

#[test]
fn ratio_local_calibration_excludes_pooled_history_but_keeps_real_slowdowns() {
    let old = gpu_sample("episode_gpu_rel", 4.5, 270.0, 80.0);
    let ledger = rows_for("2026-11-08", "old", "boxA", &[old]);
    let mut local = gpu_sample("episode_gpu_rel", 2.0, 270.0, 80.0);
    local.at[0].name = "ratio_ref_gpu_p50_us";
    local.at[1].name = "ratio_ref_blank_gpu_p50_us";
    let judged_local = judged(&ledger, "boxA", std::slice::from_ref(&local));
    assert!(judged_local.breaches.is_empty());
    assert_eq!(judged_local.elsewhere.len(), 1);
    assert_eq!(
        judged(
            &ledger,
            "boxA",
            &[gpu_sample("episode_gpu_rel", 2.0, 270.0, 80.0)]
        )
        .breaches
        .len(),
        1,
        "the pooled calibration would report the original breach"
    );
    let calibrated = rows_for("2026-11-08", "local", "boxA", std::slice::from_ref(&local));
    local.value = 0.9;
    assert_eq!(
        judged(&calibrated, "boxA", &[local]).breaches.len(),
        1,
        "a slowdown at the same ratio-local operating point still fails"
    );
}

#[test]
fn a_true_slowdown_at_the_same_operating_point_still_fails() {
    let mut ledger = String::new();
    for (value, ref_us, blank_us) in [
        (1.80, 535.8, 367.5),
        (1.79, 536.4, 367.3),
        (2.842, 195.3, 80.4),
        (1.81, 430.0, 300.0),
        (2.9, 535.8, 250.0),
    ] {
        ledger.push_str(&rows_for(
            "2026-10-23",
            "c",
            "boxA",
            &[gpu_sample("episode_gpu_rel", value, ref_us, blank_us)],
        ));
    }
    let slow = judged(
        &ledger,
        "boxA",
        &[gpu_sample("episode_gpu_rel", 0.9, 535.9, 367.4)],
    );
    assert_eq!(slow.breaches.len(), 1, "0.9 < 0.70 x 1.81");
    assert!(
        (slow.breaches[0].best - 1.81).abs() < 1e-9,
        "430/300 us is within 1.25x of 535.9/367.4 us; 195.3/80.4 and 535.8/250 are not: {:?}",
        slow.breaches
    );
    assert!(slow.elsewhere.is_empty());
    let level = judged(
        &ledger,
        "boxA",
        &[gpu_sample("episode_gpu_rel", 1.75, 535.9, 367.4)],
    );
    assert!(level.breaches.is_empty(), "CONTROL: {:?}", level.breaches);

    let here = gpu_point(195.3, 80.4);
    let row = |r: f64, b: f64| [("ref_gpu_p50_us", r), ("ref_blank_gpu_p50_us", b)];
    assert!(same_point(&here, &row(195.3 * OPERATING_POINT_BAND, 80.4)));
    assert!(!same_point(
        &here,
        &row(195.3 * OPERATING_POINT_BAND * 1.01, 80.4)
    ));
    assert!(
        !same_point(&here, &row(175.9, 117.6)),
        "the reference within the band, the blank repaint not"
    );
    assert!(
        !same_point(&here, &[("ref_gpu_p50_us", 195.3)]),
        "a row that lacks a figure cannot show it"
    );
    assert!(
        same_point(
            &here,
            &[
                ("ref_gpu_p50_us", 195.3),
                ("ref_blank_gpu_p50_us", 80.4),
                ("other_us", 1.0)
            ]
        ),
        "a figure the sample does not name constrains nothing"
    );
    assert!(same_point(&[], &[]), "a sample with no point takes any row");
}

#[test]
fn a_rows_operating_point_round_trips_beside_its_re_record_mark() {
    let me = box_id("boxA");
    let at = [gpu_sample("episode_gpu_rel", 1.8, 535.667, 367.5)];
    let plain = trend_rows("2026-10-23", "d", &me, &at, &[]);
    let marked = trend_rows("2026-10-23", "d", &me, &at, &["effects"]);
    let figures = "ref_gpu_p50_us=535.667\tref_blank_gpu_p50_us=367.500\n";
    assert!(
        plain.ends_with(&format!("\tTest-Chip-8c\t{figures}")),
        "{plain:?}"
    );
    assert!(
        marked.ends_with(&format!("\tTest-Chip-8c\trerecorded\t{figures}")),
        "{marked:?}"
    );
    for (rows, rerecorded) in [(&plain, false), (&marked, true)] {
        let parsed = trend_ledger_rows(rows);
        assert_eq!(
            parsed[0].at,
            [("ref_gpu_p50_us", 535.667), ("ref_blank_gpu_p50_us", 367.5)]
        );
        assert_eq!(parsed[0].rerecorded, rerecorded);
        assert_eq!(
            rows.split('\t')
                .nth(7)
                .is_some_and(|c| c.trim() == RERECORDED),
            rerecorded
        );
    }
    let cpu = rows_for("2026-10-23", "d", "boxA", &[sample("effects", "m", 9.0)]);
    assert!(trend_ledger_rows(&cpu)[0].at.is_empty());
    let bad = "2026-10-23\td\tboxA\teffects/m\t9.0\ttrust\tTest-Chip-8c\tref_gpu_p50_us=nan\n";
    assert!(trend_ledger_rows(bad)[0].at.is_empty());
}

#[test]
fn trend_inverted_metric_uses_min_best_and_a_ceiling() {
    let mut ledger = String::new();
    for v in [20.0, 18.0, 25.0] {
        ledger.push_str(&rows_for(
            "2026-07-22",
            "abc",
            "boxA",
            &[inverted_sample("resize", "resize_tiered_sync_worst_ms", v)],
        ));
    }
    let breaches = judged(
        &ledger,
        "boxA",
        &[inverted_sample(
            "resize",
            "resize_tiered_sync_worst_ms",
            30.0,
        )],
    )
    .breaches;
    assert_eq!(breaches.len(), 1);
    assert!((breaches[0].bound - 18.0 / TREND_RATIO).abs() < 1e-9);
    assert!(breaches[0].inverted, "breach is flagged inverted");
    for ok_v in [24.0, 5.0] {
        let ok = judged(
            &ledger,
            "boxA",
            &[inverted_sample(
                "resize",
                "resize_tiered_sync_worst_ms",
                ok_v,
            )],
        );
        assert!(ok.breaches.is_empty(), "{ok_v} ms must pass");
    }
    let fresh = judged(
        &ledger,
        "boxB",
        &[inverted_sample(
            "resize",
            "resize_tiered_sync_worst_ms",
            30.0,
        )],
    );
    assert!(fresh.breaches.is_empty());
    assert_eq!(fresh.skipped, vec!["resize/resize_tiered_sync_worst_ms"]);
}

#[test]
fn trend_same_box_regression_trips_and_other_boxes_do_not() {
    let ledger = rows_for(
        "2026-07-22",
        "abc",
        "boxA",
        &[sample("throughput", "median_mbps", 1000.0)],
    );
    assert_eq!(
        ledger.trim_end().split('\t').nth(5),
        Some("trust"),
        "row: {ledger:?}"
    );
    let breaches = judged(
        &ledger,
        "boxA",
        &[sample("throughput", "median_mbps", 400.0)],
    )
    .breaches;
    assert_eq!(breaches.len(), 1);
    assert!(
        (breaches[0].bound - 700.0).abs() < 1e-9,
        "floor is best x 0.70"
    );
    let cross = judged(
        &ledger,
        "boxB",
        &[sample("throughput", "median_mbps", 400.0)],
    );
    assert!(cross.breaches.is_empty());
    let ok = judged(
        &ledger,
        "boxA",
        &[sample("throughput", "median_mbps", 850.0)],
    );
    assert!(ok.breaches.is_empty());
}

#[test]
fn trend_floor_is_best_of_window_so_drift_cannot_ratchet_it_down() {
    let mut ledger = String::new();
    for v in [1000.0, 900.0, 800.0] {
        ledger.push_str(&rows_for(
            "2026-07-22",
            "abc",
            "boxA",
            &[sample("scroll", "scrub_median_rps", v)],
        ));
    }
    let breaches = judged(
        &ledger,
        "boxA",
        &[sample("scroll", "scrub_median_rps", 650.0)],
    )
    .breaches;
    assert_eq!(breaches.len(), 1);
    let mut long = String::new();
    long.push_str(&rows_for(
        "2026-07-22",
        "abc",
        "boxA",
        &[sample("scroll", "scrub_median_rps", 1000.0)],
    ));
    for _ in 0..TREND_WINDOW {
        long.push_str(&rows_for(
            "2026-07-22",
            "abc",
            "boxA",
            &[sample("scroll", "scrub_median_rps", 800.0)],
        ));
    }
    let windowed = judged(
        &long,
        "boxA",
        &[sample("scroll", "scrub_median_rps", 650.0)],
    );
    assert!(
        windowed.breaches.is_empty(),
        "650 >= 0.70 x 800 (recent best), old 1000 aged out"
    );
}

#[test]
fn a_row_measured_on_uncommitted_code_says_so() {
    assert_eq!(tree_label("abc", Some("")), "abc");
    let golden = " M crates/aterm-verify/perf/golden/perf-baseline-effects.json\n\
                  M  crates/aterm-verify/perf/golden/perf-trend.tsv\n";
    assert_eq!(tree_label("abc", Some(golden)), "abc");
    let code = format!("{golden} M crates/aterm-gpu/src/renderer.rs\n");
    assert_eq!(tree_label("abc", Some(&code)), "abc+dirty");
    let renamed = "R  crates/aterm-verify/perf/golden/a.json -> crates/aterm-gpu/a.json\n";
    assert_eq!(tree_label("abc", Some(renamed)), "abc+dirty");
    assert_eq!(
        tree_label("abc", None),
        "abc+dirty",
        "an unread status vouches nothing"
    );
}

#[test]
fn a_tree_that_changed_while_the_lanes_ran_is_dirty() {
    assert_eq!(measured_tree("abc", "abc"), "abc");
    assert_eq!(
        measured_tree("abc", "abc+dirty"),
        "abc+dirty",
        "an edit mid-run"
    );
    assert_eq!(
        measured_tree("abc+dirty", "abc"),
        "abc+dirty",
        "an edit built, then undone before the rows"
    );
    assert_eq!(measured_tree("abc", "def"), "abc+dirty", "a commit mid-run");
    assert_eq!(measured_tree("unknown", "def"), "unknown");
}

#[test]
fn trend_rows_record_the_wasm_lanes_toolchain_and_engine() {
    let rows = rows_for(
        "2026-07-22",
        "abc",
        "boxA",
        &[sample("wasm", "wasm_cpu_ingest_mixed_mbps", 343.0)],
    );
    assert_eq!(
        rows.trim_end().split('\t').nth(5),
        Some("stable+chrome-headless-shell"),
        "row: {rows:?}"
    );
    assert_eq!(
        trend_ledger_rows(&rows)[0].toolchain,
        "stable+chrome-headless-shell"
    );
}

#[test]
fn another_engines_rows_start_no_window_for_this_one() {
    let row = |toolchain: &str, value: f64| {
        format!("2026-10-01\tabc\tboxA\twasm/w\t{value:.3}\t{toolchain}\tTest-Chip-8c\n")
    };
    let node: String = (0..4).map(|_| row("stable", 1000.0)).collect();
    let now = [sample("wasm", "w", 100.0)];
    let fresh = judged(&node, "boxA", &now);
    assert!(fresh.breaches.is_empty(), "{:?}", fresh.breaches);
    assert_eq!(fresh.skipped, ["wasm/w"], "no history in this engine");
    let browser = row("stable+chrome-headless-shell", 120.0);
    let ledger = format!("{node}{browser}");
    assert!(
        judged(&ledger, "boxA", &now).breaches.is_empty(),
        "100 >= 0.70 x 120"
    );
    let low = [sample("wasm", "w", 50.0)];
    assert_eq!(
        judged(&ledger, "boxA", &low).breaches.len(),
        1,
        "50 < 0.70 x 120"
    );
    let same: String = (0..4)
        .map(|_| row("stable+chrome-headless-shell", 1000.0))
        .collect();
    assert_eq!(
        judged(&same, "boxA", &now).breaches.len(),
        1,
        "CONTROL: 100 < 0.70 x 1000 in the same engine"
    );
    let old = "2026-07-01\tabc\tboxA\tsearch/q\t1000.000\n";
    let trust = format!("{old}2026-07-02\tabc\tboxA\tsearch/q\t900.000\ttrust\n");
    let held = judged(&trust, "boxA", &[sample("search", "q", 650.0)]);
    assert_eq!(
        held.breaches.len(),
        1,
        "650 < 0.70 x 1000: the old row still counts"
    );
    assert!(held.skipped.is_empty());
}

const ALIASES: &str = "# box\tidentity\tnote\n\
     m21\tfp:deadbeefdeadbeef\tthe box\n\
     m21\thost:m21.local\tafter the rename\n\
     m21\thost:m15.local\tbefore the rename — SAME machine\n\
     m7\tfp:0123456789abcdef\ta different box\n";

#[test]
fn a_rename_no_longer_forks_a_box_from_its_own_history() {
    assert_eq!(resolve_ledger_box(ALIASES, "m15.local").key(), "m21");
    assert_eq!(resolve_ledger_box(ALIASES, "m21.local").key(), "m21");
    assert_eq!(resolve_ledger_box(ALIASES, "m21").key(), "m21");
    assert!(resolve_ledger_box(ALIASES, "m21").is_claimed());
    let mut ledger = String::new();
    ledger.push_str("2026-07-22\tabc\tm15.local\tthroughput/median_mbps\t1000.000\n");
    let me = MachineId {
        key: "m21".to_string(),
        shape: "Apple-M5-Max-18c".to_string(),
        identities: vec!["fp:deadbeefdeadbeef".to_string()],
        claimed: true,
    };
    let j = judge_trend(
        &ledger,
        ALIASES,
        &me,
        &[sample("throughput", "median_mbps", 400.0)],
    );
    assert_eq!(
        j.breaches.len(),
        1,
        "the pre-rename rows must judge the post-rename run"
    );
    assert!(
        j.skipped.is_empty(),
        "history was found, so nothing skipped"
    );
}

#[test]
fn an_unclaimed_identity_stands_alone_and_is_never_merged() {
    let r = resolve_ledger_box(ALIASES, "some-other-mac.local");
    assert_eq!(r.key(), "some-other-mac.local");
    assert!(!r.is_claimed());
    let ledger = "2026-07-22\tabc\tsome-other-mac.local\tthroughput/median_mbps\t1000.000\n";
    let me = MachineId {
        key: "m21".to_string(),
        shape: "Apple-M5-Max-18c".to_string(),
        identities: vec!["fp:deadbeefdeadbeef".to_string()],
        claimed: true,
    };
    let j = judge_trend(
        ledger,
        ALIASES,
        &me,
        &[sample("throughput", "median_mbps", 400.0)],
    );
    assert!(j.breaches.is_empty());
    assert_eq!(j.skipped, vec!["throughput/median_mbps"]);
}

#[test]
fn naming_a_box_adopts_the_rows_it_wrote_before_it_had_a_name() {
    assert_eq!(
        resolve_ledger_box(ALIASES, "box-deadbeefdeadbeef").key(),
        "m21"
    );
    assert!(resolve_ledger_box(ALIASES, "box-deadbeefdeadbeef").is_claimed());
    assert!(!resolve_ledger_box(ALIASES, "box-1111111111111111").is_claimed());
}

#[test]
fn a_chip_conflict_drops_the_rows_instead_of_comparing_across_machines() {
    let ledger =
        "2026-07-22\tabc\tm21\tthroughput/median_mbps\t9000.000\ttrust\tIntel-Core-i9-8c\n";
    let me = MachineId {
        key: "m21".to_string(),
        shape: "Apple-M5-Max-18c".to_string(),
        identities: vec!["fp:deadbeefdeadbeef".to_string()],
        claimed: true,
    };
    let j = judge_trend(
        ledger,
        ALIASES,
        &me,
        &[sample("throughput", "median_mbps", 400.0)],
    );
    assert_eq!(
        j.conflicts,
        vec![("m21".to_string(), "Intel-Core-i9".to_string())]
    );
    assert!(j.breaches.is_empty());
    assert_eq!(j.skipped, vec!["throughput/median_mbps"]);
    let same = "2026-07-22\tabc\tm21\tthroughput/median_mbps\t1000.000\ttrust\tApple-M5-Max-10c\n";
    let ok = judge_trend(
        same,
        ALIASES,
        &me,
        &[sample("throughput", "median_mbps", 400.0)],
    );
    assert!(ok.conflicts.is_empty());
    assert_eq!(ok.breaches.len(), 1, "same chip ⇒ still same-box history");
}

#[test]
fn chip_of_strips_only_a_trailing_core_count() {
    assert_eq!(chip_of("Apple-M5-Max-18c"), "Apple-M5-Max");
    assert_eq!(chip_of("Apple-M5-Max-1c"), "Apple-M5-Max");
    assert_eq!(chip_of("Apple-M5-Max"), "Apple-M5-Max");
    assert_eq!(chip_of("weird-c"), "weird-c");
    assert_eq!(chip_of("weird-xc"), "weird-xc");
    assert_eq!(chip_of(""), "");
    assert_eq!(chip_of("c"), "c");
}

#[test]
fn default_box_key_prefers_the_fingerprint() {
    assert_eq!(
        default_box_key(&["fp:abc".to_string(), "host:x.local".to_string()]),
        "box-abc"
    );
    assert_eq!(
        default_box_key(&["host:m21.local".to_string()]),
        "host-m21-local"
    );
    assert_eq!(default_box_key(&[]), "unknown-box");
}

#[test]
fn a_seven_column_row_round_trips_with_its_shape() {
    let me = MachineId {
        key: "m21".to_string(),
        shape: "Apple-M5-Max-18c".to_string(),
        identities: vec!["fp:deadbeefdeadbeef".to_string()],
        claimed: true,
    };
    let rows = trend_rows(
        "2026-08-23",
        "abc123",
        &me,
        &[sample("throughput", "median_mbps", 500.0)],
        &[],
    );
    let parsed = trend_ledger_rows(&rows);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].token, "m21");
    assert_eq!(parsed[0].shape, "Apple-M5-Max-18c");
    assert_eq!(parsed[0].metric, "throughput/median_mbps");
    let five = "2026-07-20\tabc\tboxA\tthroughput/median_mbps\t1000.000\n";
    assert_eq!(trend_ledger_rows(five)[0].shape, "");
}

#[test]
fn an_orphan_on_a_claimed_box_chip_is_named_as_a_likely_rename() {
    let aliases = "m21\thost:m15\tthe M5 Max\n";
    let ledger = "2026-07-01\ta\tm15\tthroughput/median_mbps\t1000.0\ttrust\tApple M5 Max-18\n\
                  2026-07-02\ta\tm15-renamed\tthroughput/median_mbps\t990.0\ttrust\tApple M5 Max-18\n\
                  2026-07-02\ta\tm15-renamed\trestore/restore_median_hz\t9.0\ttrust\tApple M5 Max-18\n\
                  2026-07-03\ta\tnewbox\tthroughput/median_mbps\t700.0\ttrust\tApple M4-10\n\
                  2026-07-04\ta\told\tthroughput/median_mbps\t600.0\n";
    let rows = trend_ledger_rows(ledger);
    assert_eq!(
        orphans(&rows, aliases),
        vec![
            ("m15-renamed", Some("m21".to_string())),
            ("newbox", None),
            ("old", None),
        ]
    );
}

#[test]
fn rfc3339_renders_epoch_seconds_exactly() {
    assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
    assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
}

#[test]
fn utc_date_is_iso_shaped() {
    let d = utc_date();
    let bytes = d.as_bytes();
    assert_eq!(bytes.len(), 10, "{d}");
    assert_eq!(bytes[4], b'-', "{d}");
    assert_eq!(bytes[7], b'-', "{d}");
    assert!(
        bytes[..4].iter().all(u8::is_ascii_digit)
            && bytes[5..7].iter().all(u8::is_ascii_digit)
            && bytes[8..10].iter().all(u8::is_ascii_digit),
        "{d}"
    );
    assert_eq!(civil_date(0), "1970-01-01");
    assert_eq!(civil_date(47_482), "2100-01-01");
}

#[test]
fn non_finite_ledger_values_are_discarded_and_counted() {
    let ledger = "2026-07-22\tabc\tboxA\tthroughput/median_mbps\tinf\ttrust\tTest-Chip-8c\n\
                  2026-07-22\tabc\tboxA\tthroughput/median_mbps\tNaN\ttrust\tTest-Chip-8c\n\
                  2026-07-22\tabc\tboxA\tthroughput/median_mbps\t500.0\ttrust\tTest-Chip-8c\n";
    let (rows, dropped) = trend_ledger_rows_counted(ledger);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value, 500.0);
    assert_eq!(dropped.len(), 2);
}
