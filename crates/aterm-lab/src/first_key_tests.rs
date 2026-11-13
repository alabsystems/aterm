// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn arm_names(rows: &[Row]) -> Vec<String> {
    rows.iter().filter_map(|r| r.get("arm").cloned()).collect()
}

#[test]
fn an_arm_reads_its_rest_presses_and_burst() {
    assert_eq!(
        Arm::parse("R=30:200").unwrap(),
        Arm {
            name: "R".into(),
            rest_secs: 30,
            presses: 200,
            burst: false,
            toml: Vec::new(),
        }
    );
    assert!(Arm::parse("Rb=30:50:burst").unwrap().burst);
    for bad in [
        "R",
        "R=30",
        "=30:1",
        "R=x:1",
        "R=30:1:loud",
        "R=30:150:1",
        "R-1=30:1",
    ] {
        assert!(Arm::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn the_schedule_spreads_each_arm_and_reverses_every_other_round() {
    let arm = |s: &str| Arm::parse(s).unwrap();
    let arms = [arm("A=0:4"), arm("H=30:4"), arm("R=30:2")];
    let names: Vec<&str> = schedule(&arms)
        .into_iter()
        .map(|i| arms[i].name.as_str())
        .collect();
    assert_eq!(names, ["A", "H", "R", "H", "A", "A", "H", "R", "H", "A"]);
    let two = [arm("A=0:3"), arm("H=30:3")];
    let names: Vec<&str> = schedule(&two)
        .into_iter()
        .map(|i| two[i].name.as_str())
        .collect();
    assert_eq!(names, ["A", "H", "H", "A", "A", "H"], "ABBA");
}

#[test]
fn nearest_rank_quantiles() {
    let v: Vec<f64> = (1..=100).map(f64::from).collect();
    assert_eq!(rank(&v, 0.5), 50.0);
    assert_eq!(rank(&v, 0.95), 95.0);
    assert_eq!(rank(&v, 0.99), 99.0);
    assert_eq!(rank(&v[..24], 0.99), 24.0, "of 24, p99 is the worst");
    assert!(rank(&[], 0.5).is_nan());
}

#[test]
fn the_exact_test_matches_the_hypergeometric_tail() {
    // 5 of 24 against 0 of 24: the design's figure, one-sided p 0.025.
    let p = fisher_greater(5, 24, 0, 24);
    assert!((p - 0.0250).abs() < 0.0005, "{p}");
    assert!((fisher_greater(0, 10, 5, 10) - 1.0).abs() < 1e-9);
    let even = fisher_greater(3, 10, 3, 10);
    assert!(even > 0.5 && even < 1.0, "{even}");
}

#[test]
fn a_bootstrap_of_two_equal_samples_straddles_zero() {
    let a: Vec<f64> = (0..200).map(|i| f64::from(i % 17)).collect();
    let mut rng = Rng::new(7);
    let (lo, hi) = bootstrap_diff(&sorted(a.clone()), &sorted(a), 0.5, 2000, &mut rng);
    assert!(lo <= 0.0 && hi >= 0.0, "{lo}..{hi}");
}

#[test]
fn rows_round_trip_through_the_tsv_and_summarize() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("presses.tsv");
    let mut text = format!("{}\n", COLUMNS.join("\t"));
    for (i, (arm, input)) in [("A", 1.5), ("R", 4.0), ("R", 2.0), ("A", 1.7)]
        .into_iter()
        .enumerate()
    {
        let row: Vec<String> = COLUMNS
            .iter()
            .map(|c| match *c {
                "press" => i.to_string(),
                "arm" => arm.to_string(),
                "input_ms" => input.to_string(),
                "burst" => "0".to_string(),
                _ => "1".to_string(),
            })
            .collect();
        let _ = writeln!(text, "{}", row.join("\t"));
    }
    std::fs::write(&path, text).unwrap();
    let rows = read_rows(&[path]).unwrap();
    assert_eq!(arm_names(&rows), ["A", "R", "R", "A"]);
    let s = summarize(&rows);
    assert!(s.contains("A  2  1.50 / 1.70 / 1.70"), "{s}");
    assert!(s.contains("R - A:"), "{s}");
    assert!(s.contains("input p99 +2.30 ms"), "{s}");
}

#[test]
fn burst_keys_are_compared_with_the_first_burst_arm_alone() {
    let row = |arm: &str, burst: &str, input: f64| -> Row {
        [("arm", arm), ("burst", burst)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .chain([("input_ms".to_string(), input.to_string())])
            .collect()
    };
    let rows = [
        row("A", "0", 1.0),
        row("Ab", "2", 5.0),
        row("R", "0", 2.0),
        row("Rb", "3", 7.0),
    ];
    let s = summarize(&rows);
    assert!(s.contains("R - A:"), "{s}");
    assert!(s.contains("Rb - Ab:"), "{s}");
    assert!(
        s.contains("input p50 +2.00 ms"),
        "Rb's 7 against Ab's 5: {s}"
    );
    assert!(
        !s.contains("Ab - A:") && !s.contains("Rb - A:"),
        "a burst key is never set against a first key: {s}"
    );
}

#[test]
fn a_reply_s_fields_are_read_by_key() {
    let f = fields("OK metrics n_input=1 last_input_present_ms=2.10 note");
    assert_eq!(num(&f, "n_input"), Some(1.0));
    assert_eq!(num(&f, "last_input_present_ms"), Some(2.1));
    assert_eq!(num(&f, "note"), None);
}
