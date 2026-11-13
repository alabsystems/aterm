// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn measurement_requires_one_ordered_report() {
    let valid = "JOURNAL-NOTE n=20000 p50_us=3 p95_us=5 p99_us=9";
    assert_eq!(parse_report(valid).unwrap()["p99_us"], json!(9));
    let after_name = format!("test note_latency_reports_its_percentiles ... {valid}");
    assert_eq!(parse_report(&after_name).unwrap()["p50_us"], json!(3));
    assert!(parse_report("running 0 tests").is_err());
    assert!(parse_report(&format!("{valid}\n{valid}")).is_err());
    assert!(parse_report("JOURNAL-NOTE n=20000 p50_us=9 p95_us=5 p99_us=9").is_err());
    assert!(parse_report("JOURNAL-NOTE n=0 p50_us=1 p95_us=1 p99_us=1").is_err());
    assert!(parse_report("JOURNAL-NOTE n=20000 p50_us=1 p95_us=1").is_err());
}
