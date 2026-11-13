// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::UpdateStatus;

fn staged(outcome: &str) -> UpdateStatus {
    let mut status = UpdateStatus::empty(true, 81, outcome.to_string());
    status.staged_build = Some(85);
    status.staged_version = Some("0.85.0".to_string());
    status
}

#[test]
fn a_staged_summary_preserves_refusals_and_independent_failure_classes() {
    for (outcome, checks, applies) in [
        ("staged 0.85.0 (build 85) — NOT applied: no successor", 0, 0),
        (
            "staged build did not apply: child exited before Ready",
            0,
            1,
        ),
        ("update check failed: network unavailable", 1, 0),
    ] {
        let mut status = staged(outcome);
        status.failing_checks = checks;
        status.failing_applies = applies;
        assert_eq!(status.summary(), outcome);
    }
}

#[test]
fn summary_is_one_bounded_unicode_line_for_outcomes_and_fallbacks() {
    let status = staged(" apply refused:\nkeep\tthese sessions open\r\n");
    assert_eq!(status.summary(), "apply refused: keep these sessions open");
    let long = format!("apply refused: {}", "猫".repeat(600));
    let line = staged(&long).summary();
    assert!(line.starts_with("apply refused: "));
    assert_eq!(line.chars().count(), 512);
    assert!(line.ends_with('…'));
    assert_eq!(staged(&"猫".repeat(512)).summary(), "猫".repeat(512));
    let mut status = staged("");
    status.staged_version = Some("猫\n".repeat(600));
    let line = status.summary();
    assert_eq!(line.chars().count(), 512);
    assert!(!line.contains('\n'));
    assert!(line.ends_with('…'));
}
