// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn settle_evidence_keeps_the_matched_plan_and_detects_a_planted_value_shift() {
    for (lane, short, detail) in [
        (
            "measure-settle",
            "an_idle_recording_pane_adds_at_most_one_panes_budget_rss",
            "COST idle_rss off_median_bytes=100 on_median_bytes=101 extra_bytes=1 off_app_median_bytes=50 on_app_median_bytes=51",
        ),
        (
            "count-waits",
            "twenty_busy_recording_panes_add_at_most_the_pool",
            "COST busy_rss off_bytes=100 on_bytes=101 extra_bytes=1 pool_used=50",
        ),
        (
            "count-waits-only",
            "twenty_busy_recording_panes_add_at_most_the_pool",
            "COST busy_rss off_bytes=100 on_bytes=101 extra_bytes=1 pool_used=50",
        ),
    ] {
        let settle_records = |dir: &Path| {
            if lane.starts_with("count-waits") {
                wait_records(dir)
            } else {
                super::settle_records(dir)
            }
        };
        let tmp = aterm_tempfile::Builder::new()
            .prefix("settle-evidence-")
            .tempdir()
            .unwrap();
        let dir = tmp.path();
        let inputs = json!({"source_commit":"frozen", "astream_commit":"astream", "bundle_sha256":"bundle", "image":"image", "lane":lane, "guest":{"cpus":8}, "driver":"App"});
        std::fs::write(dir.join("inputs.json"), inputs.to_string()).unwrap();
        let row_name = format!("rewind::{short}");
        let started = json!({"event":"measure-row-started","matrix":"rewind","row":short,"at_unix_ms":1000,"seconds":0.0});
        let ended = json!({"event":"measure-row-finished","matrix":"rewind","row":short,"at_unix_ms":2000,"seconds":1.0});
        let other_start = json!({"event":"measure-row-started","matrix":"zz","row":"stable","at_unix_ms":2000,"seconds":0.0});
        let other_end = json!({"event":"measure-row-finished","matrix":"zz","row":"stable","at_unix_ms":3000,"seconds":1.0});
        let stable = format!(
            "measure kind: {row_name} Timing\nverify-event: {started}\nverify-event: {ended}\nmeasure row: {row_name} PASS\n{detail}\nverify-event: {other_start}\nverify-event: {other_end}\nmeasure row: zz::stable PASS\ntext\n"
        );
        let mut rows = Vec::new();
        for (i, mode) in "ABBAABBAAB".chars().enumerate() {
            let file = format!("take-{}.verifier.log", i + 1);
            std::fs::write(dir.join(&file), &stable).unwrap();
            if lane == "count-waits-only" {
                std::fs::write(
                    dir.join(format!("take-{}.stderr", i + 1)),
                    "measure phase: row=busy_rss record=false phase=ready seconds=0.5\n",
                )
                .unwrap();
            }
            rows.push(json!({"take":i+1, "settle_mode":if mode=='A' {"baseline"} else {"candidate"}, "exit":0, "success":true, "usage":{"real":1.0,"user":0.1,"sys":0.1}, "artifacts":{"verifier_log":file}}));
        }
        for warm in ["warmup", "warmup-candidate"] {
            std::fs::write(
                dir.join(format!("{warm}.json")),
                r#"{"exit":0,"success":true}"#,
            )
            .unwrap();
            std::fs::write(dir.join(format!("{warm}.verifier.log")), &stable).unwrap();
        }
        std::fs::write(
            dir.join("negative-control.json"),
            r#"{"expected_failure":true,"exit":101}"#,
        )
        .unwrap();
        let control = format!("measure row: {row_name} FAIL\nplanted RSS regression\n");
        std::fs::write(dir.join("negative-control.stdout"), &control).unwrap();
        let mut summary = inputs.clone();
        let object = summary.as_object_mut().unwrap();
        object.insert("complete".into(), json!(true));
        object.insert("rows".into(), json!(rows));
        std::fs::write(dir.join("rows.json"), summary.to_string()).unwrap();
        let report = settle_records(dir).unwrap();
        if lane == "count-waits-only" {
            assert_eq!(report["wall_takes"][0]["wall"]["setup_phases"]["row=busy_rss record=false"]["ready"].as_f64(), Some(0.5));
            std::fs::write(
                dir.join("take-1.stderr"),
                "measure phase: row=busy_rss record=false phase=ready seconds=NaN\n",
            )
            .unwrap();
            assert!(
                settle_records(dir).is_err(),
                "invalid phase evidence in stderr cannot be ignored"
            );
            std::fs::write(
                dir.join("take-1.stderr"),
                "measure phase: row=busy_rss record=false phase=ready seconds=0.5\n",
            )
            .unwrap();
        }
        assert_eq!(
            report["whole_check"]["baseline"]["wall_s"]["median"].as_f64(),
            Some(1.0)
        );
        assert_eq!(
            report["rows"][0]["metrics"]["extra_bytes"]["p_two_sided"].as_f64(),
            Some(1.)
        );
        assert!(
            report["rows"][0]["kind"].is_null(),
            "the offline reader must not reclassify a Timing row"
        );
        let missing_boundaries = stable
            .lines()
            .filter(|line| !line.contains("\"matrix\":\"zz\""))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.join("take-2.verifier.log"), missing_boundaries).unwrap();
        assert!(
            settle_records(dir).is_err(),
            "a row with both wall boundaries missing cannot silently disappear"
        );
        std::fs::write(dir.join("take-2.verifier.log"), &stable).unwrap();
        for i in [2, 3, 6, 7, 10] {
            std::fs::write(
                dir.join(format!("take-{i}.verifier.log")),
                stable.replace("extra_bytes=1", "extra_bytes=16000000"),
            )
            .unwrap();
        }
        let shifted = settle_records(dir).unwrap();
        assert_eq!(
            shifted["rows"][0]["metrics"]["extra_bytes"]["shift"].as_bool(),
            Some(true)
        );
        assert_eq!(
            shifted["rows"][0]["metrics"]["extra_bytes"]["extreme_assignments"].as_u64(),
            Some(2)
        );
        std::fs::write(dir.join("negative-control.stdout"), &stable).unwrap();
        assert!(
            settle_records(dir).is_err(),
            "a passing planted regression cannot qualify"
        );
        std::fs::write(dir.join("negative-control.stdout"), &control).unwrap();
        std::fs::write(dir.join("warmup.json"), r#"{"exit":1,"success":false}"#).unwrap();
        assert!(
            settle_records(dir).is_err(),
            "a failed warm-up cannot be discarded"
        );
        std::fs::write(dir.join("warmup.json"), r#"{"exit":0,"success":true}"#).unwrap();
        std::fs::remove_file(dir.join("take-2.verifier.log")).unwrap();
        assert!(
            settle_records(dir).is_err(),
            "a missing take cannot qualify"
        );
    }
}

#[test]
fn a_planted_concurrency_shift_stays_timing_even_when_every_verdict_passes() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::Duration;
    // No sleeps or latency assumptions: the partner stays until the row reads its value.
    // Its solo value is zero, its concurrent value one. Waits are minute hang detectors.
    let planted = |together| {
        let active = AtomicBool::new(false);
        if together {
            let (ready, wait_ready) = mpsc::channel();
            let (release, wait_release) = mpsc::channel();
            std::thread::scope(|scope| {
                let active = &active;
                scope.spawn(move || {
                    active.store(true, Ordering::SeqCst);
                    ready.send(()).unwrap();
                    wait_release.recv_timeout(Duration::from_secs(60)).unwrap();
                });
                wait_ready.recv_timeout(Duration::from_secs(60)).unwrap();
                let value = u64::from(active.load(Ordering::SeqCst));
                release.send(()).unwrap();
                value
            })
        } else {
            u64::from(active.load(Ordering::SeqCst))
        }
    };
    let mut takes = Vec::new();
    for mode in ["sequential", "together"] {
        for _ in 0..5 {
            let n = planted(mode == "together");
            let log = format!(
                "measure row: fixture::planted PASS\nREWIND idle_ms=10000 rewind_wakes={n} timer_wakes=0 frames=0\nmeasure row: fixture::stable PASS\nREWIND idle_ms=10000 rewind_wakes=0 timer_wakes=0 frames=0\n"
            );
            takes.push((mode.into(), parse(&log).unwrap()));
        }
    }
    let report = compare(&takes).unwrap();
    let rows = report["rows"].as_array().unwrap();
    assert_eq!(rows[0]["kind"].as_str(), Some("Timing"));
    assert_eq!(
        rows[0]["metrics"]["rewind_wakes"]["extreme_assignments"].as_u64(),
        Some(2)
    );
    assert_eq!(rows[1]["kind"].as_str(), Some("Count"));
    assert_eq!(
        rows[1]["metrics"]["rewind_wakes"]["p_two_sided"].as_f64(),
        Some(1.)
    );
    // Missing takes cannot qualify. A refused take cannot qualify even with equal values.
    assert!(compare(&takes[..9]).is_err());
    takes[0].1.get_mut("fixture::stable").unwrap().verdict = "COULD NOT RUN".into();
    assert_eq!(
        compare(&takes).unwrap()["rows"][1]["kind"].as_str(),
        Some("Timing")
    );
    takes[0].1.remove("fixture::stable");
    assert!(compare(&takes).is_err());
}

#[test]
fn malformed_or_missing_values_never_become_zero() {
    assert!(parse("empty").is_err());
    assert!(
        parse("measure row: x PASS\nREWIND idle_ms=1000 rewind_wakes=None frames=0 timer_wakes=0")
            .is_err()
    );
    assert!(parse("measure row: x PASS\nREWIND idle_ms=1000 frames=0 timer_wakes=0").is_err());
    assert!(parse("measure row: x PASS\ntext\nmeasure row: x PASS\ntext").is_err());
}

#[test]
fn solo_timing_values_never_requalify_the_row_for_concurrent_execution() {
    let log = "measure kind: x Timing\nmeasure row: x PASS\nREWIND idle_ms=10000 rewind_wakes=0 frames=0 timer_wakes=0\n";
    let takes: Vec<_> = (0..10)
        .map(|i| {
            (
                if i < 5 { "sequential" } else { "together" }.into(),
                parse(log).unwrap(),
            )
        })
        .collect();
    let report = compare(&takes).unwrap();
    assert_eq!(report["rows"][0]["kind"].as_str(), Some("Timing"));
    assert_eq!(
        report["rows"][0]["metrics"]["frames"]["p_two_sided"].as_f64(),
        Some(1.)
    );
    let mut takes = takes;
    takes[0].1 = parse("measure kind: x Count { windowed: false }\nmeasure row: x COULD NOT RUN\nSPIN COULD NOT RUN: no subject\n").unwrap();
    assert_eq!(
        compare(&takes).unwrap()["rows"][0]["kind"].as_str(),
        Some("Timing")
    );
    assert!(parse("measure kind: absent Timing\nmeasure row: x PASS\ntext").is_err());
    assert!(parse("measure kind: x unknown\nmeasure row: x PASS\ntext").is_err());
    assert!(parse("measure kind: spin::a Timing\nmeasure row: spin::a PASS\ntext\nmeasure row: spin::b PASS\ntext").is_err());
    assert!(parse("measure kind: spin::a Timing\nmeasure row: spin::a PASS\ntext\nmeasure row: paint::b PASS\ntext").is_ok());
    assert_eq!(
        parse("measure row: x PASS\ntext\nmeasure kind: x Timing\n").unwrap()["x"].count,
        Some(false)
    );
}

#[test]
fn retained_evidence_requires_one_bundle_and_take_owned_logs() {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("b14-rank-evidence-")
        .tempdir()
        .unwrap();
    let fixture = |name: &str, default: bool| {
        let dir = tmp.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        let mut inputs = json!({"source_commit": name, "astream_commit": "astream", "bundle_sha256": "one-bundle", "image": "one-image", "lane": "measure", "guest": {"cpus": 8}, "driver": "App", "compiler": "16387"});
        inputs.as_object_mut().unwrap().insert(
            if default {
                "count_policy"
            } else {
                "count_mode"
            }
            .into(),
            json!(if default { "qualified" } else { "interleaved" }),
        );
        let mut rows = Vec::new();
        for i in 1..=if default { 5 } else { 10 } {
            let log = format!("take-{i}.verifier.log");
            std::fs::write(
                dir.join(&log),
                "measure row: x PASS\nREWIND idle_ms=10000 rewind_wakes=0 frames=0 timer_wakes=0\n",
            )
            .unwrap();
            let mut row =
                json!({"take": i, "exit": 0, "success": true, "artifacts": {"verifier_log": log}});
            if !default {
                row.as_object_mut().unwrap().insert(
                    "count_mode".into(),
                    json!(if i <= 5 { "sequential" } else { "together" }),
                );
            }
            rows.push(row);
        }
        let mut summary = inputs.clone();
        summary
            .as_object_mut()
            .unwrap()
            .insert("complete".into(), json!(true));
        summary
            .as_object_mut()
            .unwrap()
            .insert("all_exits_zero".into(), json!(true));
        summary
            .as_object_mut()
            .unwrap()
            .insert("exit".into(), json!(0));
        summary
            .as_object_mut()
            .unwrap()
            .insert("rows".into(), json!(rows));
        std::fs::write(dir.join("inputs.json"), inputs.to_string()).unwrap();
        std::fs::write(dir.join("rows.json"), summary.to_string()).unwrap();
        dir
    };
    let controls = fixture("controls", false);
    let candidate = fixture("candidate", true);
    assert_eq!(
        records(&controls, None).unwrap()["rows"][0]["kind"].as_str(),
        Some("Count")
    );
    assert_eq!(
        records(&controls, Some(&candidate)).unwrap()["rows"][0]["kind"].as_str(),
        Some("Count")
    );
    let focused = fixture("focused", false);
    for lane in ["count-qualify", "unknown"] {
        for file in ["inputs.json", "rows.json"] {
            let path = focused.join(file);
            let mut record = read_json(&path).unwrap();
            record
                .as_object_mut()
                .unwrap()
                .insert("lane".into(), json!(lane));
            std::fs::write(path, record.to_string()).unwrap();
        }
        if lane == "count-qualify" {
            assert_eq!(
                records(&focused, None).unwrap()["rows"][0]["kind"].as_str(),
                Some("Count")
            );
        } else {
            assert!(records(&focused, None).is_err());
        }
    }
    let failed = fixture("failed-driver", false);
    let path = failed.join("rows.json");
    let mut summary = read_json(&path).unwrap();
    summary
        .as_object_mut()
        .unwrap()
        .insert("all_exits_zero".into(), json!(false));
    std::fs::write(&path, summary.to_string()).unwrap();
    assert!(records(&failed, None).is_err());
    summary
        .as_object_mut()
        .unwrap()
        .insert("all_exits_zero".into(), json!(true));
    summary
        .as_object_mut()
        .unwrap()
        .get_mut("rows")
        .unwrap()
        .as_array_mut()
        .unwrap()[0]
        .as_object_mut()
        .unwrap()
        .insert("exit".into(), json!(1));
    std::fs::write(path, summary.to_string()).unwrap();
    assert!(records(&failed, None).is_err());
    let mut input = read_json(&candidate.join("inputs.json")).unwrap();
    input
        .as_object_mut()
        .unwrap()
        .insert("bundle_sha256".into(), json!("another-bundle"));
    std::fs::write(candidate.join("inputs.json"), input.to_string()).unwrap();
    // A mixed snapshot refuses even before the cross-experiment bundle check.
    assert!(records(&controls, Some(&candidate)).is_err());
    let mut summary = read_json(&candidate.join("rows.json")).unwrap();
    summary
        .as_object_mut()
        .unwrap()
        .insert("bundle_sha256".into(), json!("another-bundle"));
    std::fs::write(candidate.join("rows.json"), summary.to_string()).unwrap();
    assert!(records(&controls, Some(&candidate)).is_err());
    let mut summary = read_json(&controls.join("rows.json")).unwrap();
    std::fs::copy(
        controls.join("take-1.verifier.log"),
        tmp.path().join("outside"),
    )
    .unwrap();
    summary
        .as_object_mut()
        .unwrap()
        .get_mut("rows")
        .unwrap()
        .as_array_mut()
        .unwrap()[0]
        .as_object_mut()
        .unwrap()
        .get_mut("artifacts")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("verifier_log".into(), json!("../outside"));
    std::fs::write(controls.join("rows.json"), summary.to_string()).unwrap();
    assert!(records(&controls, None).is_err());
}
