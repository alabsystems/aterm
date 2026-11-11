// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn complete_tapes_qualify_and_a_wrong_row_or_shift_cannot_borrow_their_pass() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let passed =
        "measure row: rewind::an_idle_recording_pane_adds_at_most_one_panes_budget_rss PASS\n";
    for name in ["warmup.stdout", "warmup-fixed.stdout"] {
        std::fs::write(dir.path().join(name), passed).unwrap();
    }
    let mut rows = Vec::new();
    let log = |a: bool, value: u64, identity: &str| {
        let stopped = if a { 6 } else { 4 };
        format!(
            "{passed}COST idle_rss pairs={} planned_pairs=3 stopping_samples_per_arm={stopped} off_median_bytes={value} on_median_bytes={value} extra_bytes={value} off_app_median_bytes={value} on_app_median_bytes={value}\nverify-event: {}\n",
            stopped / 2,
            json!({"event":"measure-row-finished","matrix":"rewind","row":identity,"seconds":if a {30.0} else {20.0}})
        )
    };
    let identity = "an_idle_recording_pane_adds_at_most_one_panes_budget_rss";
    for i in 0..10 {
        let a = [0, 3, 4, 7, 8].contains(&i);
        rows.push(json!({"take":i+1,"arm":if a {"A"} else {"B"},"exit":0,
            "usage":{"real":if a {32.0} else {22.0},"user":1.0,"sys":1.0},"host_max_load_1m":5.0}));
        std::fs::write(
            dir.path().join(format!("take-{}.stdout", i + 1)),
            log(a, 7, identity),
        )
        .unwrap();
    }
    std::fs::write(
        dir.path().join("rows.json"),
        json!({"all_exits_zero":true,"rows":rows}).to_string(),
    )
    .unwrap();
    let result = run(dir.path()).unwrap();
    assert_eq!(result["metrics_retain_ranks"].as_bool(), Some(true));
    assert_eq!(result["wall_criterion_met"].as_bool(), Some(true));
    for i in 0..10 {
        let a = [0, 3, 4, 7, 8].contains(&i);
        std::fs::write(
            dir.path().join(format!("take-{}.stdout", i + 1)),
            log(a, if a { 7 } else { 100 }, identity),
        )
        .unwrap();
    }
    assert_eq!(
        run(dir.path()).unwrap()["metrics_retain_ranks"].as_bool(),
        Some(false)
    );
    std::fs::write(
        dir.path().join("take-1.stdout"),
        log(true, 7, "another_row"),
    )
    .unwrap();
    assert!(run(dir.path()).is_err());
    std::fs::write(
        dir.path().join("take-1.stdout"),
        log(true, 7, identity).replace("planned_pairs=3", "planned_pairs=2"),
    )
    .unwrap();
    assert!(run(dir.path()).is_err());
}

#[test]
fn exact_ranks_keep_ties_and_detect_a_planted_value_shift() {
    assert_eq!(super::ranks(&[7; 10]), (252, 252));
    assert_eq!(
        super::ranks(&[0, 100, 100, 0, 0, 100, 100, 0, 0, 100]),
        (2, 252)
    );
    assert!(super::passed(
        "measure row: rewind::an_idle_recording_pane_adds_at_most_one_panes_budget_rss PASS\n"
    ));
    assert!(!super::passed(
        "measure row: rewind::an_idle_recording_pane_adds_at_most_one_panes_budget_rss FAIL\n"
    ));
    assert!(super::metric("COST idle_rss extra_bytes=9\n", "extra_bytes").is_ok());
    assert!(super::metric("COST idle_rss extra_bytes=NaN\n", "extra_bytes").is_err());
    assert!(super::metric("", "extra_bytes").is_err());
}
