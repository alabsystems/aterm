// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn json(text: &str) -> Value {
    aterm_json::from_str(text).expect("test JSON parses")
}

fn report(median: f64) -> PerfReport {
    PerfReport {
        median_mbps: median,
        min_mbps: median * 0.9,
        max_mbps: median * 1.1,
        workload_bytes: 32 << 20,
        n: 7,
        warmup: 2,
    }
}

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("atv-perf-floors-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch dir")
}

#[test]
fn compare_boundary_is_inclusive_pass() {
    let (v, floor) = compare(1000.0, 450.0, 0.45);
    assert_eq!(v, Verdict::Pass);
    assert!((floor - 450.0).abs() < 1e-9, "floor {floor}");
    assert_eq!(compare(1000.0, 449.999, 0.45).0, Verdict::Fail);
}

#[test]
fn compare_nonpositive_baseline_is_no_baseline() {
    assert_eq!(compare(0.0, 1000.0, 0.45).0, Verdict::NoBaseline);
    assert_eq!(compare(-5.0, 1000.0, 0.45).0, Verdict::NoBaseline);
    assert_eq!(compare(f64::NAN, 1000.0, 0.45).0, Verdict::NoBaseline);
}

#[test]
fn the_throughput_baseline_round_trips_and_reads_the_harness_line() {
    let r = report(1234.5);
    let back = parse_report(&json(&baseline_json(&r, PASS_RATIO, "a \"quoted\" note")))
        .expect("parse the baseline we wrote");
    assert!((back.median_mbps - r.median_mbps).abs() < 1e-3);
    assert_eq!(back.workload_bytes, r.workload_bytes);
    let line = "{\"median_mbps\":3142.000,\"min_mbps\":3000.500,\"max_mbps\":3300.250,\
                \"workload_bytes\":33554432,\"n\":7,\"warmup\":2}";
    let r = parse_report(&json(line)).expect("harness line");
    assert_eq!((r.workload_bytes, r.n, r.warmup), (33_554_432, 7, 2));
    assert!(parse_report(&json("{\"median_mbps\":100.0}")).is_none());
}

#[test]
fn a_missing_baseline_judges_nothing_and_an_unusable_one_fails() {
    let dir = scratch();
    let path = dir.path().join("perf-baseline.json");
    let run = compare_against_baseline(&path, &report(1234.5));
    assert_eq!(run.status, Status::NotJudged("no baseline".into()));
    std::fs::write(&path, "{\"median_mbps\": 1000.0}").expect("write");
    let run = compare_against_baseline(&path, &report(1234.5));
    assert!(run.failed(), "{run:?}");
    std::fs::write(&path, "not json").expect("write");
    assert!(compare_against_baseline(&path, &report(1234.5)).failed());
}

const KEYED_LANES: [&FloorLane; 16] = [
    &PATHOLOGICAL_LANE,
    &SCROLL_LANE,
    &SEARCH_LANE,
    &RESTORE_LANE,
    &RESIZE_LANE,
    &ZSTD_LANE,
    &CRASH_RESTORE_LANE,
    &EFFECTS.full,
    &EFFECTS.gpu,
    &EFFECTS.cpu,
    &TRAIL_DARK_COMPOSER.full,
    &TRAIL_DARK_COMPOSER.gpu,
    &TRAIL_DARK_COMPOSER.cpu,
    &TRAIL_LIGHT_COMPOSER.full,
    &TRAIL_LIGHT_COMPOSER.gpu,
    &TRAIL_LIGHT_COMPOSER.cpu,
];

fn keyed_line(lane: &FloorLane, scale: f64) -> Value {
    let mut s = String::from("{");
    for key in lane.keys {
        s.push_str(&format!("\"{key}\":{:.3},", 100.0 * scale));
    }
    s.push_str("\"n\":5,\"warmup\":1}");
    json(&s)
}

#[test]
fn keyed_reports_round_trip_through_their_baselines() {
    for lane in KEYED_LANES
        .iter()
        .copied()
        .chain([&super::super::wasm::WASM_LANE])
    {
        let parsed = parse_keyed(lane, &keyed_line(lane, 1.0)).expect("full line parses");
        assert_eq!(parsed.len(), lane.keys.len());
        let back = parse_keyed(lane, &json(&keyed_baseline_json(&parsed, PASS_RATIO, "c")))
            .expect("baseline round-trips");
        assert_eq!(back, parsed);
    }
}

#[test]
fn a_partial_report_is_refused_not_judged() {
    let mut full = keyed_line(&SEARCH_LANE, 1.0);
    if let Some(o) = full.as_object_mut() {
        o.remove("replog_query_qps");
    }
    assert!(parse_keyed(&SEARCH_LANE, &full).is_none());
    let run = judge_keyed(&SEARCH_LANE, &full, false);
    assert!(run.failed(), "{run:?}");
}

#[test]
fn only_the_collapsed_metric_fails() {
    let dir = scratch();
    let path = dir.path().join("baseline.json");
    let base: Vec<(&'static str, f64)> = SEARCH_LANE.keys.iter().map(|k| (*k, 100.0)).collect();
    std::fs::write(&path, keyed_baseline_json(&base, PASS_RATIO, "c")).expect("write");
    let healthy: Vec<(&'static str, f64)> = SEARCH_LANE.keys.iter().map(|k| (*k, 90.0)).collect();
    assert_eq!(
        compare_keyed_against_baseline(&SEARCH_LANE, &path, &healthy).status,
        Status::Pass
    );
    let mut collapsed = healthy.clone();
    collapsed[3].1 = 10.0;
    match compare_keyed_against_baseline(&SEARCH_LANE, &path, &collapsed).status {
        Status::Fail(f) => {
            assert_eq!(f.len(), 1, "{f:?}");
            assert!(f[0].starts_with("replog_build_klps"), "{f:?}");
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_non_positive_baseline_metric_is_named_unjudged() {
    let dir = scratch();
    let path = dir.path().join("baseline.json");
    let mut base: Vec<(&'static str, f64)> = SEARCH_LANE.keys.iter().map(|k| (*k, 100.0)).collect();
    base[0].1 = 0.0;
    std::fs::write(&path, keyed_baseline_json(&base, PASS_RATIO, "c")).expect("write");
    let healthy: Vec<(&'static str, f64)> = SEARCH_LANE.keys.iter().map(|k| (*k, 90.0)).collect();
    let run = compare_keyed_against_baseline(&SEARCH_LANE, &path, &healthy);
    assert_eq!(run.status, Status::Pass);
    assert_eq!(
        run.unjudged,
        vec![format!(
            "search/{} (non-positive baseline)",
            SEARCH_LANE.keys[0]
        )]
    );
    std::fs::write(
        &path,
        keyed_baseline_json(&[("restore_median_hz", 0.0)], PASS_RATIO, "c"),
    )
    .expect("write");
    let run = compare_keyed_against_baseline(&RESTORE_LANE, &path, &[("restore_median_hz", 5.0)]);
    assert!(matches!(run.status, Status::NotJudged(_)), "{run:?}");
}

/// A baseline missing a key fails, naming it: read as absent it would
/// switch the whole lane off.
#[test]
fn an_incomplete_baseline_fails_and_names_the_missing_key() {
    let dir = scratch();
    let path = dir.path().join("baseline.json");
    let values: Vec<(&'static str, f64)> = SEARCH_LANE.keys.iter().map(|k| (*k, 100.0)).collect();
    std::fs::write(&path, keyed_baseline_json(&values[1..], PASS_RATIO, "c")).expect("write");
    match compare_keyed_against_baseline(&SEARCH_LANE, &path, &values).status {
        Status::Fail(f) => {
            assert!(f[0].contains(SEARCH_LANE.keys[0]), "{f:?}");
            assert!(f[0].contains("--record search"), "{f:?}");
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    let none = compare_keyed_against_baseline(&SEARCH_LANE, &dir.path().join("x"), &values);
    assert_eq!(none.status, Status::NotJudged("no baseline".into()));
}

#[test]
fn interrupted_baseline_publication_preserves_history() {
    let dir = scratch();
    let path = dir.path().join("baseline.json");
    let old = "{\"median_mbps\":42}";
    assert_eq!(
        write_baseline(&path, old).status,
        Status::Recorded(path.clone())
    );
    assert!(write_baseline_using(&path, "new", super::super::publication::interrupted).failed());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
}

#[test]
fn recorded_positive_floors_survive_reload() {
    let dir = scratch();
    for value in [42.0, 0.0004] {
        let path = dir.path().join("keyed.json");
        let values = [("restore_median_hz", value)];
        assert_eq!(
            record_keyed(&RESTORE_LANE, &path, &values).status,
            Status::Recorded(path.clone())
        );
        let stored = json(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(number(&stored, "restore_median_hz"), Some(value));
        assert_eq!(
            compare_keyed_against_baseline(&RESTORE_LANE, &path, &values).status,
            Status::Pass
        );
    }
}

#[test]
fn recorded_throughput_values_survive_reload() {
    let dir = scratch();
    for value in [42.0, 0.0004] {
        let path = dir.path().join("throughput.json");
        let report = PerfReport {
            median_mbps: value,
            min_mbps: value,
            max_mbps: value,
            workload_bytes: 32,
            n: 3,
            warmup: 1,
        };
        assert_eq!(
            record_throughput(&path, &report).status,
            Status::Recorded(path.clone())
        );
        let stored = json(&std::fs::read_to_string(&path).unwrap());
        let restored = parse_report(&stored).unwrap();
        assert_eq!(restored.median_mbps, value);
        assert_eq!(restored.min_mbps, value);
        assert_eq!(restored.max_mbps, value);
        assert_eq!(
            compare_against_baseline(&path, &report).status,
            Status::Pass
        );
    }
}

#[test]
fn recording_refuses_a_floor_that_judges_nothing() {
    let dir = scratch();
    let path = dir.path().join("baseline.json");
    let run = record_keyed(&RESTORE_LANE, &path, &[("restore_median_hz", 0.0)]);
    assert!(run.failed());
    assert!(!path.exists(), "a refused record writes nothing");
    let run = record_keyed(&RESTORE_LANE, &path, &[("restore_median_hz", 42.0)]);
    assert_eq!(run.status, Status::Recorded(path.clone()));
    let text = std::fs::read_to_string(&path).expect("written");
    assert_eq!(number(&json(&text), "restore_median_hz"), Some(42.0));
}

#[test]
fn a_re_record_keeps_the_committed_note_and_grows_it_once_a_day() {
    let prev = json("{\"_comment\": \"Recorded on m21 by hand; see \\\"why\\\".\"}");
    let (c, carried) = carried_comment(Some(&prev), "default", "2026-10-15");
    assert!(carried);
    assert_eq!(
        c,
        "Recorded on m21 by hand; see \"why\". Re-recorded 2026-10-15."
    );
    let again = json(&keyed_baseline_json(&[("k", 1.0)], PASS_RATIO, &c));
    let (twice, _) = carried_comment(Some(&again), "default", "2026-10-15");
    assert_eq!(twice, c, "a same-day re-record does not stutter");
    assert_eq!(
        carried_comment(None, "default", "d"),
        ("default".to_string(), false)
    );
}

#[test]
fn resize_fences_hold_absolutely() {
    let fences = |ring: f64, sync: f64, step: f64| {
        resize_fences(&json(&format!(
            "{{\"resize_ring_worst_ms\":{ring},\"resize_tiered_sync_worst_ms\":{sync},\
             \"deep_mixed_step_worst_p50_ms\":{step},\"deep_wrapall_step_worst_p50_ms\":2.5}}"
        )))
    };
    assert!(fences(73.3, 15.0, 0.9).is_empty());
    assert_eq!(fences(42000.0, 15.0, 0.9).len(), 1);
    assert_eq!(fences(73.3, 150.0, 0.9).len(), 1);
    let step = fences(73.3, 15.0, 420.0);
    assert_eq!(step.len(), 1);
    assert!(step[0].contains("pump step"), "{step:?}");
    assert_eq!(
        resize_fences(&json("{\"resize_ring_worst_ms\":73.3}")).len(),
        3,
        "missing is a breach"
    );
}

fn assert_failed_fence_preserves_baseline(
    lane: &FloorLane,
    judge: fn(&Value, bool, &Path) -> LaneRun,
    breach: bool,
) {
    let dir = scratch();
    let path = dir.path().join(lane.cli);
    let values: Vec<_> = lane.keys.iter().map(|key| (*key, 10.0)).collect();
    let report = keyed_baseline_json(&values, PASS_RATIO, "fixture");
    let mut good = json(&report);
    let fields = good.as_object_mut().unwrap();
    fields.insert("tail_tap_ratio".into(), Value::from(TAIL_TAP_RATIO_FENCE));
    for (key, cap, _) in RESIZE_FENCES {
        fields.insert(key.into(), Value::from(cap));
    }
    assert_eq!(
        judge(&good, true, &path).status,
        Status::Recorded(path.clone())
    );
    let original = std::fs::read(&path).unwrap();
    let mut missing = json(&report);
    for key in lane.keys {
        missing
            .as_object_mut()
            .unwrap()
            .insert((*key).into(), Value::from(20.0));
    }
    let mut breached = missing.clone();
    breached
        .as_object_mut()
        .unwrap()
        .insert("tail_tap_ratio".into(), Value::from(0.5));
    for (key, cap, _) in RESIZE_FENCES {
        breached
            .as_object_mut()
            .unwrap()
            .insert(key.into(), Value::from(cap * 2.0));
    }
    let run = judge(if breach { &breached } else { &missing }, true, &path);
    assert!(run.failed());
    assert_eq!(std::fs::read(&path).unwrap(), original, "{}", lane.cli);
    let mut changed = good;
    for key in lane.keys {
        changed
            .as_object_mut()
            .unwrap()
            .insert((*key).into(), Value::from(20.0));
    }
    assert_eq!(
        judge(&changed, true, &path).status,
        Status::Recorded(path.clone())
    );
    assert_ne!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn missing_crash_fence_preserves_recorded_baseline() {
    assert_failed_fence_preserves_baseline(&CRASH_RESTORE_LANE, judge_crash_restore_at, false);
}

#[test]
fn breached_crash_fence_preserves_recorded_baseline() {
    assert_failed_fence_preserves_baseline(&CRASH_RESTORE_LANE, judge_crash_restore_at, true);
}

#[test]
fn missing_resize_fence_preserves_recorded_baseline() {
    assert_failed_fence_preserves_baseline(&RESIZE_LANE, judge_resize_at, false);
}

#[test]
fn breached_resize_fence_preserves_recorded_baseline() {
    assert_failed_fence_preserves_baseline(&RESIZE_LANE, judge_resize_at, true);
}

#[test]
fn the_tail_tap_fence_holds_absolutely() {
    assert_eq!(tail_tap_fence(&json("{\"tail_tap_ratio\":0.994}")), Ok(()));
    assert_eq!(
        tail_tap_fence(&json("{\"tail_tap_ratio\":0.97}")),
        Ok(()),
        "inclusive"
    );
    assert!(tail_tap_fence(&json("{\"tail_tap_ratio\":0.93}")).is_err());
    assert!(
        tail_tap_fence(&json("{\"tail_tap_mbps\":500.0}")).is_err(),
        "missing is a breach"
    );
    let mut s = String::from("{\"tail_tap_ratio\":0.5,");
    for key in CRASH_RESTORE_LANE.keys {
        s.push_str(&format!("\"{key}\":10.0,"));
    }
    s.push_str("\"n\":9}");
    let run = judge_crash_restore(&json(&s), false);
    assert!(run.failed(), "{run:?}");
}

/// The trend takes each synchronous latency's median over the passes, inverted; the
/// run's one worst and a pump step's worst are the fences' alone. NEGATIVE CONTROL: a
/// report with only the worsts gives the trend nothing to judge them by.
#[test]
fn resize_trends_its_passes_median_worsts_inverted_and_fences_the_tails() {
    let report = |extra: &str| {
        let mut s = String::from("{");
        for key in RESIZE_LANE.keys {
            s.push_str(&format!("\"{key}\":10.0,"));
        }
        s.push_str(
            "\"resize_ring_worst_ms\":70.0,\"resize_tiered_sync_worst_ms\":15.0,\
             \"deep_mixed_step_worst_p50_ms\":0.9,\"deep_wrapall_step_worst_p50_ms\":2.5,",
        );
        s.push_str(extra);
        s.push_str("\"n\":5}");
        json(&s)
    };
    let inverted = |run: &LaneRun| -> Vec<String> {
        run.samples
            .iter()
            .filter(|t| t.inverted)
            .map(|t| t.metric.clone())
            .collect()
    };
    let medians = report(
        "\"resize_ring_worst_p50_ms\":66.0,\"resize_tiered_sync_worst_p50_ms\":1.1,\
         \"deep_mixed_sync_worst_p50_ms\":0.9,",
    );
    let run = judge_resize(&medians, false);
    assert!(!run.failed(), "{run:?}");
    assert_eq!(
        inverted(&run),
        vec![
            "resize_ring_worst_p50_ms",
            "resize_tiered_sync_worst_p50_ms",
            "deep_mixed_sync_worst_p50_ms"
        ]
    );
    assert!(inverted(&judge_resize(&report(""), false)).is_empty());
}

/// A synthetic value comfortably above the committed effects floors, so these
/// lane-selection checks do not depend on the measured costs of a frame.
const HEALTHY: f64 = 1e12;

/// Every lane the effects harness measures.
const EFFECTS_LANES: [&EffectsLane; 3] = [&EFFECTS, &TRAIL_DARK_COMPOSER, &TRAIL_LIGHT_COMPOSER];

/// An effects report on `ground`: every key of `keys` at `value`, the clock and counter the
/// harness named, and a few of the ungated keys it reports beside them.
fn report_on(
    ground: Option<&str>,
    clock: Option<&str>,
    counter: Option<&str>,
    keys: &[&str],
    value: f64,
) -> Value {
    let mut s = String::from("{");
    if let Some(ground) = ground {
        s.push_str(&format!("\"ground\":\"{ground}\","));
    }
    if let Some(clock) = clock {
        s.push_str(&format!("\"gpu_clock\":\"{clock}\","));
    }
    if let Some(counter) = counter {
        s.push_str(&format!("\"cpu_counter\":\"{counter}\","));
    }
    for key in keys {
        s.push_str(&format!("\"{key}\":{value:.3},"));
    }
    s.push_str("\"typing_encode_p50_us\":412.0,\"typing_pass_cost_pct\":6.1,\"runs\":5}");
    json(&s)
}

/// [`report_on`] the ground `lane` measures.
fn effects_report(
    lane: &EffectsLane,
    clock: Option<&str>,
    counter: Option<&str>,
    keys: &[&str],
    value: f64,
) -> Value {
    report_on(Some(lane.ground), clock, counter, keys, value)
}

/// The lane choice's verdict, as the first key it judges or the run's status.
fn chosen(
    lane: &EffectsLane,
    json: &Value,
    record: bool,
) -> Result<(&'static str, Option<String>), Status> {
    effects_lane(lane, json, record)
        .map(|(judged, missing)| (judged.keys[0], missing))
        .map_err(|run| run.status)
}

#[test]
fn an_effects_host_without_a_gpu_skips_and_names_why() {
    for lane in EFFECTS_LANES {
        let run = judge_effects(lane, &json("{\"skip\":\"no Metal device\"}"), false);
        match run.status {
            Status::Skip { why, .. } => assert!(why.contains("no Metal device"), "{why}"),
            other => panic!("a host without a GPU skips, got {other:?}"),
        }
    }
}

/// A host judges what it can measure and names the rest unjudged: no GPU clock, the CPU
/// work keys; no instruction counter, the GPU keys; neither, a skip.
#[test]
fn an_effects_host_is_judged_on_what_it_measures_and_records_only_with_both() {
    for lane in EFFECTS_LANES {
        let cli = lane.full.cli;
        let full = effects_report(
            lane,
            Some("metal"),
            Some("instructions"),
            lane.full.keys,
            HEALTHY,
        );
        assert_eq!(chosen(lane, &full, false), Ok((lane.full.keys[0], None)));
        assert_eq!(chosen(lane, &full, true), Ok((lane.full.keys[0], None)));
        assert!(!judge_effects(lane, &full, false).failed(), "{cli}");

        let clockless = effects_report(
            lane,
            Some("none"),
            Some("instructions"),
            lane.cpu.keys,
            HEALTHY,
        );
        let run = judge_effects(lane, &clockless, false);
        assert!(!run.failed(), "{run:?}");
        assert!(
            run.unjudged
                .iter()
                .any(|u| u.starts_with(cli) && u.contains("no GPU clock")),
            "{run:?}"
        );
        assert!(
            run.samples.iter().all(|t| t.metric.ends_with("_per_ginsn")),
            "{run:?}"
        );

        let uncounted = effects_report(lane, Some("metal"), Some("none"), lane.gpu.keys, HEALTHY);
        let run = judge_effects(lane, &uncounted, false);
        assert!(!run.failed(), "{run:?}");
        assert!(
            run.unjudged
                .iter()
                .any(|u| u.starts_with(cli) && u.contains("no instruction counter")),
            "{run:?}"
        );
        assert!(
            run.samples.iter().all(|t| t.metric.ends_with("_gpu_rel")),
            "{run:?}"
        );

        for partial in [&clockless, &uncounted] {
            match chosen(lane, partial, true) {
                Err(Status::Fail(f)) => {
                    assert!(f[0].starts_with("REFUSING to record"), "{f:?}");
                }
                other => panic!("a partial host must not record, got {other:?}"),
            }
        }
        let neither = effects_report(lane, Some("none"), Some("none"), &[], HEALTHY);
        assert!(matches!(
            chosen(lane, &neither, false),
            Err(Status::Skip { .. })
        ));
    }
}

/// A lane judges only a report drawn on its own ground: the light composer lane fed the
/// dark scene (a `--light` that never reached the harness) would hold the light trail to
/// the dark trail's floors. NEGATIVE CONTROL: the same report on the lane's own ground is
/// judged.
#[test]
fn a_report_on_another_ground_is_refused_not_judged() {
    for lane in EFFECTS_LANES {
        let keys = lane.full.keys;
        let other = if lane.ground == "dark" {
            "light"
        } else {
            "dark"
        };
        for ground in [Some(other), None] {
            let report = report_on(ground, Some("metal"), Some("instructions"), keys, HEALTHY);
            for record in [false, true] {
                match chosen(lane, &report, record) {
                    Err(Status::Fail(f)) => assert!(
                        f[0].contains(lane.ground) && f[0].contains(lane.full.cli),
                        "{f:?}"
                    ),
                    other => panic!("{ground:?} under {}: {other:?}", lane.full.cli),
                }
            }
        }
        let own = report_on(
            Some(lane.ground),
            Some("metal"),
            Some("instructions"),
            keys,
            HEALTHY,
        );
        assert!(
            !judge_effects(lane, &own, false).failed(),
            "{}",
            lane.full.cli
        );
    }
}

/// Each GPU trend sample names the reference and blank repaints' GPU p50s it was measured
/// at, and the CPU work samples name none; a report that lacks either figure sends no GPU
/// sample to the trend and says so. NEGATIVE CONTROL: the same report's CPU samples are
/// unchanged.
#[test]
fn effects_gpu_trend_samples_carry_the_reference_repaints_they_were_measured_at() {
    for lane in EFFECTS_LANES {
        let mut full = effects_report(
            lane,
            Some("metal"),
            Some("instructions"),
            lane.full.keys,
            HEALTHY,
        );
        let unplaced = judge_effects(lane, &full, false);
        let mut half = full.clone();
        if let Some(o) = full.as_object_mut() {
            // Deliberately unrelated pooled summaries cannot place a scene ratio.
            o.insert("ref_gpu_p50_us".to_string(), Value::from(535.667));
            o.insert("ref_blank_gpu_p50_us".to_string(), Value::from(367.5));
            for (index, key) in lane.gpu.keys.iter().enumerate() {
                o.insert(
                    format!("{key}_ref_gpu_p50_us"),
                    Value::from(100.0 + index as f64),
                );
                o.insert(
                    format!("{key}_ref_blank_gpu_p50_us"),
                    Value::from(50.0 + index as f64),
                );
            }
        }
        if let Some(o) = half.as_object_mut() {
            o.insert("ref_gpu_p50_us".to_string(), Value::from(535.667));
            for key in lane.gpu.keys {
                o.insert(format!("{key}_ref_gpu_p50_us"), Value::from(100.0));
            }
        }
        let run = judge_effects(lane, &full, false);
        assert!(!run.failed(), "{run:?}");
        assert_eq!(run.samples.len(), lane.full.keys.len());
        for s in &run.samples {
            let gpu = lane.gpu.keys.contains(&s.metric.as_str());
            let want = if gpu {
                let index = lane
                    .gpu
                    .keys
                    .iter()
                    .position(|key| *key == s.metric)
                    .unwrap();
                vec![
                    PointFigure {
                        name: "ratio_ref_gpu_p50_us",
                        value: 100.0 + index as f64,
                    },
                    PointFigure {
                        name: "ratio_ref_blank_gpu_p50_us",
                        value: 50.0 + index as f64,
                    },
                ]
            } else {
                Vec::new()
            };
            assert_eq!(s.at, want, "{}", s.metric);
        }
        for partial in [unplaced, judge_effects(lane, &half, false)] {
            assert!(!partial.failed(), "{partial:?}");
            assert!(
                partial
                    .samples
                    .iter()
                    .all(|s| s.metric.ends_with("_per_ginsn") && s.at.is_empty()),
                "{partial:?}"
            );
            assert_eq!(partial.samples.len(), lane.cpu.keys.len());
            assert!(
                partial
                    .unjudged
                    .iter()
                    .any(|u| u.starts_with(lane.full.cli) && u.contains("ref_blank_gpu_p50_us")),
                "{partial:?}"
            );
        }
    }
}

/// A full report is judged on every gated key: one missing, or a report that does not say
/// which clock or counter it read, is refused rather than judged on what is left.
#[test]
fn an_effects_report_names_its_clock_and_counter_and_carries_every_key() {
    for lane in EFFECTS_LANES {
        let keys = lane.full.keys;
        let full = effects_report(lane, Some("metal"), Some("instructions"), keys, HEALTHY);
        let gpu_key = lane.gpu.keys[lane.gpu.keys.len() - 1];
        let cpu_key = lane.cpu.keys[lane.cpu.keys.len() - 1];
        for key in [keys[0], gpu_key, cpu_key] {
            let mut partial = full.clone();
            if let Some(o) = partial.as_object_mut() {
                o.remove(key);
            }
            assert!(judge_effects(lane, &partial, false).failed(), "{key}");
        }
        let no_clock = effects_report(lane, None, Some("instructions"), keys, HEALTHY);
        assert!(judge_effects(lane, &no_clock, false).failed());
        let no_counter = effects_report(lane, Some("metal"), None, keys, HEALTHY);
        assert!(judge_effects(lane, &no_counter, false).failed());
        for (clock, counter) in [
            ("", "instructions"),
            ("metla", "instructions"),
            ("metal", ""),
            ("metal", "cycles"),
        ] {
            let unknown = effects_report(lane, Some(clock), Some(counter), keys, HEALTHY);
            assert!(judge_effects(lane, &unknown, false).failed());
            assert!(matches!(chosen(lane, &unknown, true), Err(Status::Fail(_))));
        }
    }
}

#[test]
fn throughput_rejects_invalid_counts_and_refuses_to_record_non_positive_median() {
    let bad_n = aterm_json::from_str::<Value>(
        r#"{"median_mbps":100.0,"min_mbps":90.0,"max_mbps":110.0,"workload_bytes":1024,"n":0,"warmup":1}"#,
    )
    .unwrap();
    assert!(parse_report(&bad_n).is_none());

    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("perf-baseline.json");
    let zero_report = PerfReport {
        median_mbps: 0.0,
        min_mbps: 0.0,
        max_mbps: 0.0,
        workload_bytes: 1024,
        n: 5,
        warmup: 1,
    };
    let run = record_throughput(&path, &zero_report);
    assert!(run.failed());
    assert!(!path.exists());
}
