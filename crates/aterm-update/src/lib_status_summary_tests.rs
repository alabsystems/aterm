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

/// What every status report ends with ([`super::hold_report`]): a live hold on a newer build, a
/// lapsed one waiting for its next check, and a newer offer with no hold each turn an "up to
/// date" outcome into words that say so. NEGATIVE CONTROL: a record with nothing held and no
/// newer offer keeps "up to date", so the predicate is not vacuous.
#[test]
fn a_held_or_behind_copy_never_reports_up_to_date() {
    use aterm_update_core::update_record::{Cause, Failure, Sha, UpdateRecord, Witness};
    const NOW: u64 = 1_800_000_000;
    let up_to_date = || "up to date (latest release build 11)".to_string();
    let held_at = |at: u64, version: &str| {
        let mut record = UpdateRecord {
            running: 10,
            ..UpdateRecord::fresh()
        };
        record.set_hold(
            Failure {
                build: 11,
                version: version.into(),
                sha: Sha([7; 32]),
                cause: Cause::CrashLoop,
                witness: Witness::Headless,
                why: "it did not start cleanly 3 times".into(),
            },
            at,
        );
        record
    };

    let (hold, behind, live) = super::hold_report(&held_at(NOW, "0.11.0"), 10, up_to_date(), NOW);
    assert!(hold.as_ref().is_some_and(|hold| hold.live) && behind);
    assert!(!super::says_up_to_date(&live), "{live}");
    assert!(
        live.starts_with("held: aterm 0.11.0 is held until "),
        "{live}"
    );
    assert!(
        live.contains("try 1") && live.contains("`aterm update retry`"),
        "{live}"
    );
    assert!(
        !live.contains(":00Z"),
        "a clock time, not an RFC 3339 stamp: {live}"
    );

    let (_, _, unnamed) = super::hold_report(&held_at(NOW, ""), 10, up_to_date(), NOW);
    assert!(
        unnamed.contains("aterm build 11 is held"),
        "never a blank name: {unnamed}"
    );

    let lapsed_at = NOW - 2 * 3600;
    let (hold, _, lapsed) =
        super::hold_report(&held_at(lapsed_at, "0.11.0"), 10, up_to_date(), NOW);
    assert!(hold.is_some_and(|hold| !hold.live));
    assert!(!super::says_up_to_date(&lapsed), "{lapsed}");
    assert!(lapsed.contains("tried again"), "{lapsed}");

    let offered = UpdateRecord {
        running: 10,
        offered: Some(11),
        ..UpdateRecord::fresh()
    };
    let (hold, behind, behind_words) = super::hold_report(&offered, 10, up_to_date(), NOW);
    assert!(hold.is_none() && behind);
    assert!(!super::says_up_to_date(&behind_words), "{behind_words}");

    let current = UpdateRecord {
        running: 11,
        offered: Some(11),
        ..UpdateRecord::fresh()
    };
    let (hold, behind, kept) = super::hold_report(&current, 11, up_to_date(), NOW);
    assert!(hold.is_none() && !behind);
    assert!(
        super::says_up_to_date(&kept),
        "the negative control: {kept}"
    );
    let (_, _, other) = super::hold_report(&offered, 10, "no update is staged".into(), NOW);
    assert_eq!(other, "no update is staged", "any other outcome is kept");
}
