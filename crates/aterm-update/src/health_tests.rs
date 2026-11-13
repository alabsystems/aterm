// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

struct Scratch {
    file: std::path::PathBuf,
    _dir: aterm_tempfile::TempDir,
}

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.file
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.file
    }
}

fn tmp(name: &str) -> Scratch {
    let dir = crate::paths::scratch_dir(&format!("health-{name}")).into_tempdir();
    Scratch {
        file: dir.path().join("health.toml"),
        _dir: dir,
    }
}

#[test]
fn a_timed_handoff_capture_is_kept_as_a_prior_and_moves_no_streak() {
    let p = tmp("handoff-capture");
    Health::record_apply_failure(&p, 9, 909, "ActivityRevoked");
    Health::record_apply_refusal(&p, 9, "the terminal was busy");
    let recorded = Health::record_handoff_capture(&p, 9, 22_000, 2, 33);
    let read = Health::read(&p);
    for h in [&recorded, &read] {
        assert_eq!(
            (
                h.handoff_capture_us,
                h.handoff_capture_sessions,
                h.handoff_freeze_seed_ms,
                h.handoff_capture_build
            ),
            (22_000, 2, 33, 9)
        );
        assert!(
            !h.handoff_capture_at.is_empty(),
            "a measurement carries its time"
        );
        assert_eq!(h.apply_failures, 1, "no streak moves");
        assert_eq!(h.apply_failures_for_target, 1);
        assert_eq!(h.last_apply_refusal, "the terminal was busy");
        assert!(!h.is_persistent());
    }
    let newer = Health::record_handoff_capture(&p, 10, 9_000, 1, 20);
    assert_eq!(
        (
            newer.handoff_capture_us,
            newer.handoff_capture_sessions,
            newer.handoff_capture_build
        ),
        (9_000, 1, 10),
        "the newest measurement replaces the last, cheaper or not"
    );
    let zero = Health::record_handoff_capture(&p, 11, 0, 3, 20);
    assert_eq!(
        (zero.handoff_capture_us, zero.handoff_capture_build),
        (9_000, 10),
        "a zero reading cannot be told from none, so it records nothing"
    );

    let parked = Health::record_handoff_park(&p, 12, 22_000);
    assert_eq!(
        (parked.handoff_park_us, parked.handoff_park_build),
        (22_000, 12)
    );
    assert!(!parked.handoff_park_at.is_empty());
    assert_eq!(
        parked.handoff_capture_us, 9_000,
        "the dry run's slot is untouched"
    );
    let zero = Health::record_handoff_park(&p, 13, 0);
    assert_eq!(
        (zero.handoff_park_us, zero.handoff_park_build),
        (22_000, 12)
    );
    let read = Health::read(&p);
    assert_eq!(
        (
            read.handoff_park_us,
            read.handoff_capture_us,
            read.apply_failures
        ),
        (22_000, 9_000, 1)
    );
}

#[test]
fn an_acquisition_only_success_preserves_the_stage_and_apply_streaks() {
    let p = tmp("acquisition-success");
    Health::record_failure(&p, "network", "dns");
    Health::record_failure(&p, "stage", "bundle would not verify");
    Health::record_apply_failure(&p, 9, 900 + 9, "ActivityRevoked");
    let h = Health::read(&p);
    assert_eq!(
        (h.network_failures, h.stage_failures, h.apply_failures),
        (1, 1, 1)
    );

    let h = Health::record_acquisition_success(&p);
    assert_eq!(h.network_failures, 0, "acquisition streak clears");
    assert!(h.network_since.is_empty(), "and so does its clock");
    assert_eq!(h.stage_failures, 1, "the skipped lane keeps its streak");
    assert!(!h.stage_since.is_empty(), "and its clock");
    assert_eq!(
        h.apply_failures, 1,
        "a check never vouches for the apply lane"
    );
    assert_eq!(h.kind, "stage", "the standing streak owns the headline");

    let p2 = tmp("acquisition-success-no-stage");
    Health::record_failure(&p2, "manifest", "bad signature");
    Health::record_apply_failure(&p2, 9, 909, "ActivityRevoked");
    let h2 = Health::record_acquisition_success(&p2);
    assert_eq!(h2.manifest_failures, 0);
    assert_eq!(h2.apply_failures, 1);
    assert_eq!(h2.kind, "apply");
    assert!(h2.last_diagnosis_error.is_empty());
    assert!(h2.last_diagnosis_at.is_empty());
}

#[test]
fn clearing_the_apply_lane_names_the_streak_that_is_still_standing() {
    let p = tmp("apply-clear-names-standing");
    Health::record_failure(&p, "manifest", "bad signature");
    Health::record_apply_failure(&p, 11, 900 + 11, "AdoptionMismatch");
    assert_eq!(Health::read(&p).kind, "apply");

    let h = Health::record_apply_success(&p);
    assert_eq!(h.apply_failures, 0);
    assert_eq!(h.manifest_failures, 1, "the acquisition streak survives");
    assert_eq!(h.kind, "manifest", "and is NAMED, not blanked to healthy");

    let p = tmp("apply-expire-names-standing");
    Health::record_failure(&p, "pipeline", "asset fetch failed");
    Health::record_apply_failure(&p, 11, 900 + 11, "ChildDied");
    let h = Health::expire_stale_apply_streak(&p, 12);
    assert_eq!(h.apply_failures, 0, "a streak from another build is stale");
    assert_eq!(h.pipeline_failures, 1);
    assert_eq!(h.kind, "pipeline", "named, not blanked");
}

#[test]
fn each_failure_class_carries_its_own_clock() {
    let p = tmp("per-class-clock");
    Health::record_apply_failure(&p, 5, 900 + 5, "ActivityRevoked");
    let after_apply = Health::read(&p);
    assert_eq!(after_apply.apply_failures, 1);
    assert!(!after_apply.failing_since.is_empty());
    let unhealthy_since = after_apply.failing_since.clone();
    assert_eq!(after_apply.class_since("apply"), unhealthy_since);

    Health::record_failure(&p, "manifest", "bad signature");
    let h = Health::read(&p);
    assert_eq!(
        h.failing_since, unhealthy_since,
        "the any-class stamp is sticky until the ledger is clean"
    );
    assert_eq!(h.manifest_failures, 1);
    assert_eq!(
        h.class_since("apply"),
        unhealthy_since,
        "the apply streak still dates from when IT started"
    );
    assert!(
        !h.manifest_since.is_empty(),
        "the manifest class started its own clock"
    );

    Health::record_success(&p);
    let h = Health::read(&p);
    assert_eq!(h.manifest_failures, 0);
    assert!(
        h.manifest_since.is_empty(),
        "a cleared class must not keep a start time"
    );
    assert_eq!(h.apply_failures, 1);
    assert_eq!(h.class_since("apply"), unhealthy_since);

    Health::record_apply_success(&p);
    let h = Health::read(&p);
    assert_eq!(h.apply_failures, 0);
    assert!(h.apply_since.is_empty());
}

#[test]
fn a_pre_upgrade_ledger_falls_back_to_the_any_class_clock() {
    let h = Health {
        manifest_failures: 6,
        failing_since: "2026-08-11T07:16:17Z".to_string(),
        ..Health::default()
    };
    assert!(
        h.manifest_since.is_empty(),
        "the field is new; old files lack it"
    );
    assert_eq!(h.class_since("manifest"), "2026-08-11T07:16:17Z");
    assert_eq!(
        h.class_since("not-a-class"),
        "2026-08-11T07:16:17Z",
        "an unrecognised kind falls back too"
    );
}

#[test]
fn a_healthy_check_never_clears_the_apply_streak() {
    let p = tmp("apply-survives-check");
    Health::record_apply_failure(&p, 77, 900 + 77, "AdoptionMismatch");
    Health::record_apply_failure(&p, 77, 900 + 77, "ActivityRevoked");
    let h = Health::read(&p);
    assert_eq!(h.apply_failures, 2);
    assert_eq!(h.kind, "apply");
    assert_eq!(h.last_error, "ActivityRevoked");

    Health::record_failure(&p, "network", "dns");
    Health::record_success(&p);
    let h = Health::read(&p);
    assert_eq!(h.network_failures, 0, "acquisition streak clears");
    assert_eq!(
        h.apply_failures, 2,
        "a healthy check must NOT vouch for the apply lane"
    );
    assert_eq!(
        h.kind, "apply",
        "the standing failure is still the apply one"
    );
    assert_eq!(
        h.last_error, "ActivityRevoked",
        "the standing streak must be described by ITS reason, not the network blip"
    );

    Health::record_apply_success(&p);
    let h = Health::read(&p);
    assert_eq!(h.apply_failures, 0);
    assert_eq!(h.total_failures(), 0);
    assert!(h.kind.is_empty(), "fully healthy ledger reports no class");
}

#[test]
fn a_refusal_is_recorded_without_inventing_a_failure_streak() {
    let p = tmp("apply-refusal");
    let quiet = "terminal input/output is still inside the quiet epoch";
    let detail = format!(
        "Transient: handoff proof ended TimedOut\n\u{1b}[31m\u{202e} {} {} {}",
        crate::PASSING_REFUSAL_KEY,
        crate::ROSTER_UNCONFIRMED_KEY,
        "界".repeat(crate::MAX_APPLY_REFUSAL_DETAIL_CHARS)
    );
    Health::record_apply_refusal_with_detail(&p, 812, quiet, &detail);
    let h = Health::read(&p);
    assert_eq!(h.last_apply_refusal, quiet);
    assert_eq!(
        h.last_apply_refusal_detail.chars().count(),
        crate::MAX_APPLY_REFUSAL_DETAIL_CHARS
    );
    let shown = diagnostic_detail(&h.last_apply_refusal_detail);
    assert!(shown.contains("TimedOut"));
    assert!(shown.chars().count() <= crate::MAX_APPLY_REFUSAL_DETAIL_CHARS);
    assert!(!shown.chars().any(char::is_control));
    assert!(!shown.contains('\u{202e}'));
    assert!(!crate::is_passing_refusal(&h.last_apply_refusal));
    for key in [crate::PASSING_REFUSAL_KEY, crate::ROSTER_UNCONFIRMED_KEY] {
        assert_eq!(
            Health::withdraw_apply_refusal_containing(&p, key).last_apply_refusal,
            quiet,
            "diagnostic words never withdraw or classify the standing reason"
        );
    }
    assert!(!h.last_apply_refusal_at.is_empty(), "a refusal is timed");
    assert!(h.apply_refusal_applies_to(812));
    assert_eq!(h.total_failures(), 0, "a refusal is not a failure");
    assert!(!h.is_persistent(), "a refusal never escalates on its own");
    assert!(
        h.kind.is_empty(),
        "a refusal does not claim a failure class"
    );

    assert!(
        !h.apply_refusal_applies_to(813),
        "a refusal must not follow the machine into the build that overcame it"
    );

    Health::record_failure(&p, "network", "dns");
    Health::record_success(&p);
    assert_eq!(Health::read(&p).last_apply_refusal, quiet);

    Health::record_apply_refusal(&p, 812, quiet);
    assert!(Health::read(&p).last_apply_refusal_detail.is_empty());
    Health::record_apply_refusal_with_detail(&p, 812, quiet, &detail);

    Health::record_apply_failure(&p, 77, 900 + 77, "ChildDied");
    let h = Health::read(&p);
    assert!(!h.apply_refusal_applies_to(812));
    assert!(h.last_apply_refusal.is_empty());
    assert!(h.last_apply_refusal_detail.is_empty());
    assert!(h.last_apply_refusal_at.is_empty());
    assert_eq!(h.last_apply_error, "ChildDied");

    Health::record_apply_refusal_with_detail(&p, 812, "updater work is in flight", &detail);
    Health::record_apply_success(&p);
    let h = Health::read(&p);
    assert!(h.last_apply_refusal.is_empty());
    assert!(h.last_apply_refusal_detail.is_empty());
    assert!(h.last_apply_refusal_at.is_empty());
    assert_eq!(h.last_apply_refusal_build, 0);
    assert_eq!(h.apply_failures, 0);
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn the_artifact_scoped_count_restarts_with_the_artifact_and_the_escalation_does_not() {
    let p = tmp("apply-target-scope");
    Health::record_apply_failure(&p, 77, 500, "handoff proof ended ChildDied");
    Health::record_apply_failure(&p, 77, 500, "handoff proof ended ChildDied");
    let h = Health::read(&p);
    assert_eq!(h.apply_failures, 2);
    assert_eq!(h.apply_failures_for_target, 2);
    assert_eq!(h.last_apply_failure_target_build, 500);

    Health::record_apply_failure(&p, 77, 501, "handoff proof ended TimedOut");
    let h = Health::read(&p);
    assert_eq!(h.apply_failures, 3, "the machine still has not moved");
    assert_eq!(
        h.apply_failures_for_target, 1,
        "…but THIS build has been tried exactly once"
    );
    assert_eq!(h.last_apply_failure_target_build, 501);

    Health::expire_stale_apply_streak(&p, 78);
    let h = Health::read(&p);
    assert_eq!(h.apply_failures, 0);
    assert_eq!(h.apply_failures_for_target, 0);
    assert_eq!(h.last_apply_failure_target_build, 0);

    Health::record_apply_failure(&p, 78, 900, "handoff proof ended ChildDied");
    Health::record_apply_success(&p);
    let h = Health::read(&p);
    assert_eq!(h.apply_failures_for_target, 0);
    assert_eq!(h.last_apply_failure_target_build, 0);
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn an_apply_streak_expires_once_a_different_build_is_running() {
    let p = tmp("apply-expiry");
    for _ in 0..PERSISTENT_AFTER {
        Health::record_apply_failure(&p, 77, 900 + 77, "handoff proof ended TimedOut");
    }
    let h = Health::read(&p);
    assert!(h.is_persistent());
    assert_eq!(h.last_apply_failure_build, 77);
    assert!(
        !h.last_apply_failure_at.is_empty(),
        "the lane owns its clock"
    );
    for straggler_or_same in [77, 76] {
        let h = Health::expire_stale_apply_streak(&p, straggler_or_same);
        assert!(h.is_persistent(), "build {straggler_or_same}");
        assert_eq!(h.apply_failures_for_target, PERSISTENT_AFTER);
    }
    Health::expire_stale_apply_streak(&p, 78);
    let h = Health::read(&p);
    assert_eq!(h.apply_failures, 0, "stale streak expires");
    assert!(!h.is_persistent());
    assert!(h.last_apply_error.is_empty());
    assert!(h.last_apply_failure_at.is_empty());
    assert_eq!(h.last_apply_failure_build, 0);
    assert!(
        h.kind.is_empty(),
        "an expired lane stops owning the headline"
    );
    Health::record_failure(&p, "network", "dns");
    Health::record_apply_failure(&p, 78, 900 + 78, "ChildDied");
    Health::expire_stale_apply_streak(&p, 79);
    let h = Health::read(&p);
    assert_eq!(h.apply_failures, 0);
    assert_eq!(h.network_failures, 1, "the network streak survives");
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

/// A build on trial that a window failed to switch to expires the escalation streak, so the
/// persistent verdict reads as the plain expiry leaves it, and keeps the record of that failed
/// switch; once its trial settles, the plain expiry clears that record too. A failure against
/// another build expires whole, trial or not.
#[test]
fn a_build_on_trial_expires_the_escalation_and_keeps_a_window_failed_switch_to_it() {
    let p = tmp("apply-expiry-on-trial");
    for _ in 0..PERSISTENT_AFTER {
        Health::record_apply_failure(&p, 10, 11, "handoff proof ended TimedOut");
    }
    assert!(Health::read(&p).is_persistent());
    let h = Health::expire_stale_apply_streak_on_trial(&p, 11);
    assert!(!h.is_persistent(), "{h:?}");
    assert_eq!((h.apply_failures, h.apply_since.as_str()), (0, ""));
    assert!(
        h.kind.is_empty(),
        "the expired lane stops owning the headline"
    );
    assert_eq!(
        (
            h.last_apply_failure_build,
            h.last_apply_failure_target_build,
            h.apply_failures_for_target
        ),
        (10, 11, PERSISTENT_AFTER),
        "the window's failed switch to the build on trial stands"
    );
    assert!(!h.last_apply_failure_at.is_empty() && !h.last_apply_error.is_empty());
    let kept = std::fs::read_to_string(&p).unwrap();
    Health::expire_stale_apply_streak_on_trial(&p, 11);
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        kept,
        "nothing more expires while the trial stands"
    );
    let h = Health::expire_stale_apply_streak(&p, 11);
    assert_eq!(
        (
            h.last_apply_failure_build,
            h.last_apply_failure_target_build,
            h.apply_failures_for_target
        ),
        (0, 0, 0),
        "a settled build has moved past the failed switch"
    );
    assert!(h.last_apply_failure_at.is_empty() && h.last_apply_error.is_empty());

    let other = tmp("apply-expiry-on-trial-other-target");
    Health::record_apply_failure(&other, 10, 12, "ChildDied");
    let h = Health::expire_stale_apply_streak_on_trial(&other, 11);
    assert_eq!(
        (
            h.apply_failures,
            h.apply_failures_for_target,
            h.last_apply_failure_build
        ),
        (0, 0, 0),
        "a failure against another build expires whole"
    );
}

#[test]
fn each_stranding_class_escalates_under_its_own_name_and_network_never_does() {
    for (class, escalates) in [
        ("manifest", true),
        ("stage", true),
        ("pipeline", true),
        ("network", false),
    ] {
        let p = tmp(&format!("{class}-persistent"));
        for _ in 0..PERSISTENT_AFTER * 4 {
            Health::record_failure(&p, class, "reason");
        }
        backdate_streaks(&p, PERSISTENT_SPAN * 4);
        let h = Health::read(&p);
        assert_eq!(h.is_persistent(), escalates, "{class}");
        assert_eq!(
            h.persistent_class(),
            escalates.then_some((class, PERSISTENT_AFTER * 4)),
            "{class}"
        );
    }
}

#[test]
fn a_check_class_streak_is_persistent_only_once_it_spans_half_an_hour() {
    let streak = |class: &str, count: u32, newest: &str| {
        let mut h = Health {
            last_failure_at: newest.to_string(),
            last_diagnosis_at: newest.to_string(),
            kind: class.to_string(),
            ..Health::default()
        };
        let (n, since) = h.class_streak_mut(class).expect("a ledger class");
        *n = count;
        *since = "2026-10-15T10:00:00Z".to_string();
        h
    };
    for class in ["pipeline", "manifest", "stage"] {
        let quick = streak(class, PERSISTENT_AFTER, "2026-10-15T10:06:00Z");
        assert_eq!(
            quick.persistent_class(),
            None,
            "{class}: three inside 7 minutes"
        );
        let long = streak(class, PERSISTENT_AFTER, "2026-10-15T10:30:00Z");
        assert_eq!(
            long.persistent_class(),
            Some((class, PERSISTENT_AFTER)),
            "{class}: three spanning 30 minutes"
        );
        let few = streak(class, PERSISTENT_AFTER - 1, "2026-10-15T14:00:00Z");
        assert_eq!(few.persistent_class(), None, "{class}: two in four hours");
        let mut blipped = quick.clone();
        blipped.network_failures = 1;
        blipped.last_failure_at = "2026-10-15T18:00:00Z".to_string();
        assert_eq!(
            blipped.persistent_class(),
            None,
            "{class}: weather stretches nothing"
        );
        let mut older = long.clone();
        older.last_diagnosis_at.clear();
        assert_eq!(older.persistent_class(), None);
        older.last_failure_at = "2026-10-15T10:06:00Z".to_string();
        assert_eq!(older.persistent_class(), None);
    }
    let apply = streak("apply", PERSISTENT_AFTER, "2026-10-15T10:06:00Z");
    assert_eq!(apply.persistent_class(), Some(("apply", PERSISTENT_AFTER)));

    let p = tmp("persistent-span");
    for _ in 0..PERSISTENT_AFTER {
        Health::record_failure(&p, "pipeline", "asset fetch failed");
    }
    let h = Health::read(&p);
    assert_eq!(h.pipeline_failures, PERSISTENT_AFTER);
    assert!(!h.is_persistent(), "{h:?}");
    assert_eq!(
        h.last_diagnosis_at, h.last_failure_at,
        "the class's own clock"
    );
    backdate_streaks(&p, PERSISTENT_SPAN);
    assert_eq!(Health::read(&p).persistent_class(), Some(("pipeline", 3)));
    let h = Health::record_success(&p);
    assert!(
        h.last_diagnosis_at.is_empty() && !h.is_persistent(),
        "{h:?}"
    );
}

#[test]
fn a_relabelled_legacy_diagnosis_has_no_proven_end_stamp() {
    let captured = format!(
        r#"kind = "stage"
failing_since = "2026-10-15T10:00:00Z"
last_failure_at = "2026-10-15T18:00:00Z"
last_error = "standing stage verdict"
last_diagnosis_error = "standing stage verdict"
stage_failures = {PERSISTENT_AFTER}
stage_since = "2026-10-15T10:00:00Z"
"#
    );
    let legacy: Health = aterm_toml::from_str(&captured).unwrap();
    let mut known_long = legacy.clone();
    known_long.last_diagnosis_at = "2026-10-15T10:30:00Z".into();
    assert_eq!(
        known_long.persistent_class(),
        Some(("stage", PERSISTENT_AFTER)),
        "a dedicated half-hour span still escalates"
    );
    let mut known_quick = known_long.clone();
    known_quick.last_diagnosis_at = "2026-10-15T10:06:00Z".into();
    assert_eq!(
        known_quick.persistent_class(),
        None,
        "weather extends nothing"
    );
    let apply: Health = aterm_toml::from_str(&format!(
        r#"kind = "apply"
apply_failures = {PERSISTENT_AFTER}
"#
    ))
    .unwrap();
    assert_eq!(apply.persistent_class(), Some(("apply", PERSISTENT_AFTER)));
    let mut weather_label = legacy.clone();
    weather_label.kind = "network".into();
    let weather = weather_label.persistent_class();
    Health::relabel_by_standing(&mut weather_label);
    assert_eq!(weather_label.kind, "stage");
    assert_eq!(weather_label.last_failure_at, legacy.last_failure_at);
    assert_eq!(
        [
            legacy.persistent_class(),
            weather,
            weather_label.persistent_class()
        ],
        [None, None, None],
        "a legacy class label is not a diagnosis end stamp"
    );
}

#[test]
fn failure_streaks_count_per_class() {
    let p = tmp("streaks");
    assert!(
        !Health::read(&p).is_persistent(),
        "absent ledger reads healthy"
    );
    Health::record_failure(&p, "network", "dns");
    Health::record_failure(&p, "network", "dns");
    let h = Health::read(&p);
    assert_eq!((h.network_failures, h.kind.as_str()), (2, "network"));
    assert!(
        !h.is_persistent(),
        "network failures never escalate to persistent"
    );

    Health::record_failure(&p, "pipeline", "asset fetch failed");
    Health::record_failure(&p, "pipeline", "asset fetch failed");
    Health::record_failure(&p, "network", "blip");
    backdate_streaks(&p, PERSISTENT_SPAN);
    let h = Health::record_failure(&p, "pipeline", "asset fetch failed");
    assert_eq!(h.pipeline_failures, PERSISTENT_AFTER);
    assert!(
        h.is_persistent(),
        "3 pipeline failures are persistent despite the blip"
    );
    assert_eq!(h.total_failures(), 3 + 3);
    assert!(!h.failing_since.is_empty() && !h.last_error.is_empty());
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn success_clears_all_failure_streaks() {
    let p = tmp("success");
    Health::record_failure(&p, "pipeline", "x");
    Health::record_failure(&p, "stage", "y");
    let h = Health::record_success(&p);
    assert_eq!(h.total_failures(), 0);
    assert!(h.kind.is_empty() && h.last_error.is_empty() && h.failing_since.is_empty());
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn success_on_clean_ledger_does_not_write() {
    let p = tmp("clean-noop");
    assert!(!p.exists());
    let h = Health::record_success(&p);
    assert_eq!(h.total_failures(), 0);
    assert!(
        !p.exists(),
        "success on an already-clean ledger must not write the file"
    );

    Health::record_failure(&p, "network", "blip");
    Health::record_success(&p);
    assert!(p.exists());
    let before = std::fs::metadata(&p).unwrap().modified().unwrap();
    let h = Health::record_success(&p);
    assert_eq!(h.total_failures(), 0);
    let after = std::fs::metadata(&p).unwrap().modified().unwrap();
    assert_eq!(
        before, after,
        "success on an already-clean ledger must not rewrite it"
    );
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn corrupt_ledger_reads_healthy_and_error_is_capped() {
    let p = tmp("corrupt");
    std::fs::write(&p, "not = [valid").unwrap();
    assert_eq!(Health::read(&p).total_failures(), 0);
    let long = "e".repeat(2000);
    let h = Health::record_failure(&p, "pipeline", &long);
    assert_eq!(h.last_error.len(), 400, "stored error text is capped");
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn a_long_passing_refusal_keeps_its_key_in_the_ledger() {
    let p = tmp("long-passing");
    let attempt = "hdiutil attach did not finish within 120s; treating as a failure";
    let error = crate::verify::mark_passing(format!(
        "hdiutil attach failed after 4 attempts (/Users//someone/Library/Application \
         Support/aterm/Updates/download/aterm-0.93.0.dmg, 52428800 bytes): {}",
        [attempt; 4].join(" | ")
    ));
    assert!(error.chars().count() > 400, "{}", error.len());
    assert!(crate::is_passing_refusal(&error));
    for _ in 0..crate::PERSISTENT_AFTER {
        Health::record_failure(&p, "stage", &error);
    }
    let h = Health::read(&p);
    assert!(h.last_error.chars().count() <= 400, "still capped");
    assert!(
        h.last_error
            .starts_with("hdiutil attach failed after 4 attempts")
    );
    assert!(crate::is_passing_refusal(&h.last_error), "{}", h.last_error);
    let h = Health::record_failure(&p, "network", "could not resolve the host");
    assert!(h.stage_failures >= crate::PERSISTENT_AFTER, "{h:?}");
    assert!(
        crate::is_passing_refusal(&h.last_diagnosis_error),
        "{}",
        h.last_diagnosis_error
    );
    let h = Health::record_apply_failure(&p, 1, 2, &error);
    assert!(crate::is_passing_refusal(&h.last_apply_error));
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn a_pipeline_failure_clears_its_sibling_diagnoses_and_the_count_is_its_own() {
    let p = tmp("sibling-clear");
    let legacy = Health {
        manifest_failures: 999,
        pipeline_failures: 19,
        kind: "pipeline".to_string(),
        failing_since: "2026-08-27T22:04:36Z".to_string(),
        ..Health::default()
    };
    legacy.write(&p);
    let h = Health::record_failure(&p, "pipeline", "HTTP 404 on the appcast");
    assert_eq!(h.manifest_failures, 0, "the sibling diagnosis is retired");
    assert!(h.manifest_since.is_empty());
    assert_eq!(
        h.pipeline_failures, 1,
        "a clock-less count is another build's; this ledger's streak starts at one"
    );
    assert!(
        !h.pipeline_since.is_empty() && h.pipeline_since != "2026-08-27T22:04:36Z",
        "the pipeline clock is its own, not the any-class fallback: {}",
        h.pipeline_since
    );
    assert_eq!(
        h.acquisition_failures(),
        1,
        "the count is the standing class's"
    );
    assert_eq!(h.standing_acquisition_class(), Some("pipeline"));

    Health::record_failure(&p, "pipeline", "again");
    let h = Health::record_failure(&p, "network", "dns");
    assert_eq!(h.pipeline_failures, 2);
    assert_eq!(
        h.acquisition_failures(),
        2,
        "the pipeline streak outranks the blip and is reported alone, not summed"
    );
    assert_eq!(h.kind, "network", "the last failure was the blip");
    assert_eq!(
        h.standing_acquisition_class(),
        Some("pipeline"),
        "the label beside the count is the standing class, not the last blip"
    );
    assert_eq!(h.network_failures, 1);
    assert_ne!(
        (h.acquisition_failures(), h.kind.as_str()),
        (h.network_failures, "network"),
        "count and kind must not be read as one pair"
    );
    let h = Health::record_failure(&p, "manifest", "bad signature");
    assert_eq!((h.pipeline_failures, h.manifest_failures), (0, 1));
    assert!(h.pipeline_since.is_empty());
    assert_eq!(h.acquisition_failures(), 1);
    assert_eq!(
        h.network_failures, 1,
        "weather is neither reset nor summed in"
    );
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn a_standing_diagnosis_keeps_its_own_reason_across_a_blip() {
    let p = tmp("diagnosis-reason");
    let stage_reason =
        "staged bundle failed verification: codesign --verify (team-pinned requirement) failed";
    Health::record_failure(&p, "stage", stage_reason);
    Health::record_failure(&p, "stage", stage_reason);
    Health::record_failure(&p, "stage", stage_reason);
    backdate_streaks(&p, PERSISTENT_SPAN);
    let h = Health::record_failure(&p, "network", "curl: (6) could not resolve host");
    assert_eq!(h.standing_acquisition_class(), Some("stage"));
    assert_eq!(h.persistent_class(), Some(("stage", 3)));

    let h = Health::record_acquisition_success(&p);
    assert_eq!(h.kind, "stage");
    assert_eq!(h.stage_failures, 3);
    assert!(
        h.last_error.contains("codesign"),
        "the standing stage diagnosis is described by a network blip: {:?}",
        h.last_error
    );
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn expiring_a_stale_apply_streak_leaves_the_standing_diagnosis_its_reason() {
    let p = tmp("expire-keeps-reason");
    let stage_reason = "ditto zip extract failed (exit status: 1)";
    Health::record_failure(&p, "stage", stage_reason);
    Health::record_apply_failure(&p, 11, 900 + 11, "ChildDied");
    assert_eq!(Health::read(&p).kind, "apply");
    let h = Health::expire_stale_apply_streak(&p, 12);
    assert_eq!(h.apply_failures, 0);
    assert_eq!(h.kind, "stage", "named, not blanked");
    assert!(
        h.last_error.contains("ditto"),
        "a named standing class must carry its reason, not an empty string: {:?}",
        h.last_error
    );
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn the_pending_clock_is_the_first_sighting_and_survives_a_restage() {
    let p = tmp("pending-clock");
    let first = Health::note_pending_update(&p, 10, Some(11));
    assert_eq!(first.pending_build, 11);
    assert!(
        !first.pending_since.is_empty(),
        "the first sighting is stamped"
    );
    let mut aged = Health::read(&p);
    aged.pending_since = "2026-09-22T00:10:45Z".to_string();
    aged.write(&p);
    let again = Health::note_pending_update(&p, 10, Some(11));
    assert_eq!(
        again.pending_since, "2026-09-22T00:10:45Z",
        "re-observing the same build must never move its clock"
    );
    let gone = Health::note_pending_update(&p, 10, None);
    assert_eq!(
        (gone.pending_build, gone.pending_since.as_str()),
        (11, "2026-09-22T00:10:45Z")
    );
    let stale = Health::note_pending_update(&p, 10, Some(10));
    assert_eq!(
        stale.pending_build, 11,
        "a non-newer observation changes nothing"
    );
    let newer = Health::note_pending_update(&p, 10, Some(12));
    assert_eq!(newer.pending_build, 12);
    assert_ne!(newer.pending_since, "2026-09-22T00:10:45Z");
    let landed = Health::note_pending_update(&p, 12, None);
    assert_eq!(
        (landed.pending_build, landed.pending_since.as_str()),
        (0, "")
    );
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

/// THE STOPPED LANE'S RECORD: kept for the build that wrote it, through a later failure of the
/// re-sample it names, replaced by the next record and cleared by a landed apply. NEGATIVE CONTROL:
/// without `clear_apply_lane` in `record_apply_success` the record outlives the apply that ended it
/// ("a landed apply ends the record").
#[test]
fn the_stopped_lanes_record_is_scoped_to_its_build_and_ends_with_a_landed_apply() {
    let p = tmp("apply-lane");
    let me = crate::LaneWriter {
        pid: 4242,
        birth: 1_790_000_000_000_001,
    };
    let record = crate::ApplyLaneRecord {
        lane: "resampling".into(),
        target_build: 901,
        failure_signature: "structural/handoff proof ended ChildDied".into(),
        next_action: "re-samples build 901 at 2026-11-12T00:00:00Z".into(),
        at: "ignored".into(),
    };
    assert_eq!(Health::read(&p).apply_lane_for(900, me), None);
    Health::record_apply_lane(&p, 900, me, &record);
    let read = Health::read(&p).apply_lane_for(900, me).expect("recorded");
    assert_eq!(
        (
            read.lane.as_str(),
            read.target_build,
            read.failure_signature.as_str(),
            read.next_action.as_str()
        ),
        (
            "resampling",
            901,
            record.failure_signature.as_str(),
            record.next_action.as_str()
        )
    );
    assert!(read.at.ends_with('Z'), "timed when written: {}", read.at);
    assert_eq!(
        Health::read(&p).apply_lane_for(902, me),
        None,
        "another running build's record is not this one's"
    );
    Health::record_apply_failure(&p, 900, 901, "handoff proof ended ChildDied");
    assert!(
        Health::read(&p).apply_lane_for(900, me).is_some(),
        "a failed re-sample keeps the lane's record"
    );
    let newer = crate::ApplyLaneRecord {
        lane: "awaiting-newer".into(),
        ..record.clone()
    };
    Health::record_apply_lane(&p, 900, me, &newer);
    assert_eq!(
        Health::read(&p).apply_lane_for(900, me).map(|r| r.lane),
        Some("awaiting-newer".into()),
        "the next record replaces it"
    );
    Health::record_apply_success(&p);
    assert_eq!(
        Health::read(&p).apply_lane_for(900, me),
        None,
        "a landed apply ends the record"
    );
}

/// A RECORD AN EARLIER PROCESS LEFT IS NOT THIS ONE'S: the record reports its writer's in-memory
/// latch, so another process of the same build (a restart, a swap, a pid reused by a later
/// process) reads none, a record written before the writer was kept reads none, and only the
/// writer withdraws it, and only about the build it names. NEGATIVE CONTROL: without the writer
/// check in `apply_lane_for` the earlier process's record reads as current ("an earlier
/// process's record is not this one's").
#[test]
fn a_record_an_earlier_process_left_does_not_read_as_this_ones() {
    let p = tmp("apply-lane-writer");
    let earlier = crate::LaneWriter {
        pid: 4242,
        birth: 1_790_000_000_000_001,
    };
    let record = crate::ApplyLaneRecord {
        lane: "resampling".into(),
        target_build: 901,
        failure_signature: "structural/handoff proof ended ChildDied".into(),
        next_action: "re-samples build 901 at 2026-11-12T00:00:00Z".into(),
        at: String::new(),
    };
    Health::record_apply_lane(&p, 900, earlier, &record);
    let later = crate::LaneWriter {
        pid: 4343,
        birth: 1_790_000_100_000_000,
    };
    let reused_pid = crate::LaneWriter {
        pid: earlier.pid,
        birth: later.birth,
    };
    for reader in [later, reused_pid] {
        assert_eq!(
            Health::read(&p).apply_lane_for(900, reader),
            None,
            "an earlier process's record is not this one's: {reader:?}"
        );
    }
    assert!(
        Health::read(&p).apply_lane_for(900, earlier).is_some(),
        "its writer still reads it"
    );

    Health::withdraw_apply_lane(&p, later, 901);
    Health::withdraw_apply_lane(&p, earlier, 902);
    assert!(
        Health::read(&p).apply_lane_for(900, earlier).is_some(),
        "another process, or a withdrawal about another build, leaves it"
    );
    Health::withdraw_apply_lane(&p, earlier, 901);
    assert_eq!(
        Health::read(&p).apply_lane_for(900, earlier),
        None,
        "its writer withdraws it"
    );

    std::fs::write(
        &p,
        "apply_lane = \"resampling\"\napply_lane_target_build = 901\napply_lane_build = 900\n",
    )
    .expect("an older ledger");
    let old = Health::read(&p);
    assert_eq!(
        (old.apply_lane.as_str(), old.apply_lane_writer_pid),
        ("resampling", 0),
        "PRECONDITION: an older ledger still reads, its writer unkept"
    );
    assert_eq!(
        old.apply_lane_for(900, crate::LaneWriter::this_process()),
        None,
        "a record from before the writer was kept is no process's"
    );
}

/// A LANDED APPLY LEAVES A LIVE WRITER'S RECORD: the node job's boot apply lands through
/// `record_apply_success`, which cleared the stopped lane's record whoever wrote it, so a window
/// whose latch still stood printed no `apply_lane=` or `next_action=` in `update status` until its
/// plan changed; and once that kept another process's record, the writer's own trial settling
/// still cleared its own. Only the writer withdraws its record, whichever process landed the
/// apply. One whose writer has exited (its pid now another process's) still ends with a landed
/// apply. NEGATIVE CONTROL: with `record_apply_success` clearing this process's own record, as
/// the first fix had it, the writer's record goes with its own landed apply ("the writer's own
/// landed apply leaves its record").
#[cfg(unix)]
#[test]
fn a_landed_apply_leaves_a_live_writers_record() {
    let p = tmp("apply-lane-live-writer");
    // The test's runner stands in for a live window: another process that wrote its record.
    let runner = std::os::unix::process::parent_id();
    let window = crate::LaneWriter {
        pid: runner,
        birth: i32::try_from(runner)
            .ok()
            .and_then(aterm_uds::process::read_birth)
            .map_or(0, |birth| birth.0),
    };
    assert!(
        window.runs() && window != crate::LaneWriter::this_process(),
        "PRECONDITION: the runner is another live process: {window:?}"
    );
    let record = crate::ApplyLaneRecord {
        lane: "resampling".into(),
        target_build: 901,
        failure_signature: "unexplained/handoff proof ended Stalled".into(),
        next_action: "re-samples build 901 at 2026-11-12T00:00:00Z".into(),
        at: String::new(),
    };
    Health::record_apply_lane(&p, 900, window, &record);
    Health::record_apply_failure(&p, 900, 901, "handoff proof ended Stalled");
    let landed = Health::record_apply_success(&p);
    assert_eq!(
        (landed.apply_failures, landed.last_apply_error.as_str()),
        (0, ""),
        "PRECONDITION: the success landed"
    );
    assert_eq!(
        Health::read(&p)
            .apply_lane_for(900, window)
            .map(|read| (read.lane, read.next_action)),
        Some((record.lane.clone(), record.next_action.clone())),
        "a success another process landed leaves a live writer's record"
    );

    let me = crate::LaneWriter::this_process();
    Health::record_apply_lane(&p, 900, me, &record);
    Health::record_apply_success(&p);
    assert_eq!(
        Health::read(&p)
            .apply_lane_for(900, me)
            .map(|read| (read.lane, read.next_action)),
        Some((record.lane.clone(), record.next_action.clone())),
        "the writer's own landed apply leaves its record"
    );
    Health::withdraw_apply_lane(&p, me, 901);
    assert_eq!(
        Health::read(&p).apply_lane_for(900, me),
        None,
        "its writer withdraws it"
    );

    let exited = crate::LaneWriter {
        pid: window.pid,
        birth: window.birth.wrapping_add(1),
    };
    assert!(
        !exited.runs(),
        "PRECONDITION: a pid reused is not its writer"
    );
    Health::record_apply_lane(&p, 900, exited, &record);
    Health::record_apply_success(&p);
    assert_eq!(
        Health::read(&p).apply_lane_for(900, exited),
        None,
        "a record whose writer has exited ends with a landed apply"
    );
}

/// This process names itself the same way on every read, with a kernel birth where one is kept.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn this_process_is_named_by_its_pid_and_birth() {
    let me = crate::LaneWriter::this_process();
    assert_eq!(me.pid, std::process::id());
    assert_ne!(me.birth, 0, "the kernel keeps this process's birth");
    assert_eq!(
        crate::LaneWriter::this_process(),
        me,
        "and reads it the same way"
    );
}
