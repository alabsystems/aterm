// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn line_json(line: &str) -> aterm_json::Value {
    let json = line
        .strip_prefix("verify-event: ")
        .and_then(|l| l.strip_suffix('\n'))
        .expect("one newline-terminated verify-event line");
    assert!(!json.contains('\n'), "a single line");
    aterm_json::from_slice(json.as_bytes()).unwrap()
}

/// A check's `check-finished` event names its outcome by the word its line opens with, with
/// its rows, and stays far under the release reader's 64 KiB line limit however long and
/// however escaped what it carries is. Negative control: [`clip_json`] counting each
/// character as one byte lets the escaped rows run past the bound.
#[test]
fn a_check_finished_event_is_one_bounded_line_whatever_its_rows_say() {
    let mut report = Report::new("Test");
    report.pass("compiled");
    report.cannot_run("the disk ran out");
    let result = judge(
        Check::Test,
        &[&report],
        12.5,
        "",
        "verify test --all".into(),
        None,
    );
    let event = line_json(&check_event(&result, &[&report]));
    assert_eq!(event["event"].as_str(), Some("check-finished"));
    assert_eq!(event["check"].as_str(), Some("test"));
    assert_eq!(event["outcome"].as_str(), Some("NOT RUN"));
    assert_eq!(event["seconds"].as_f64(), Some(12.5));
    assert_eq!(event["rows"][1]["outcome"].as_str(), Some("NOT RUN"));
    assert_eq!(event["rows_total"].as_u64(), Some(2));

    let mut worst = Report::new("Lint");
    let hostile = "\u{1}\"\\".repeat(10_000);
    for _ in 0..1_000 {
        worst.fail(hostile.clone());
    }
    let mut result = judge(Check::Lint, &[&worst], 1.0, "", hostile.clone(), None);
    result.detail.clone_from(&hostile);
    for _ in 0..100 {
        result.findings.push(crate::ladder::Finding {
            id: hostile.clone(),
            rerun: None,
            message: vec![hostile.clone()],
        });
    }
    let line = check_event(&result, &[&worst]);
    assert!(line.len() < 48 * 1024, "{} bytes", line.len());
    let event = line_json(&line);
    assert_eq!(event["outcome"].as_str(), Some("FAIL"));
    assert_eq!(event["rows"].as_array().map(Vec::len), Some(EVENT_ROWS));
    assert_eq!(event["rows_total"].as_u64(), Some(1_000));
    assert_eq!(
        event["findings"].as_array().map(Vec::len),
        Some(FINDINGS_SHOWN)
    );
    assert!(event["detail"].as_str().unwrap().ends_with('…'));
    assert_eq!(clip_json("short", 512), "short");
}
