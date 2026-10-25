// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Completion-record failures preserve a partial choice's remaining Restore history.

use super::*;
use aterm_types::macos_setup::Group;

fn group(group: Group) -> Selection {
    let mut selected = Selection::none();
    for setting in group.settings() {
        selected.set(*setting, true);
    }
    selected
}

/// The real interrupted choice, admitted by a fresh startup ticket: Desktop is queued to set
/// again, while Keyboard & Trackpad stays restored even though the saved plan selects it.
fn admitted_partial_choice() -> aterm_tempfile::TempDir {
    let (root, plan) = super::tests::staged_restored_choice();
    let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    let revision = ticket
        .persist_at(root.path(), &plan)
        .result
        .unwrap()
        .unwrap();
    let admitted = progress(root.path()).unwrap().unwrap();
    assert!(admitted.admitted && admitted.belongs_to(&plan));
    assert_eq!(admitted.revision, revision);
    assert_eq!(admitted.only, Some(group(Group::Desktop)));
    let (decision, restored) = decision_record(root.path()).unwrap();
    assert_eq!(decision, Decision::Pending);
    let restored = restored.unwrap();
    assert_eq!(restored.queued, Some(group(Group::Desktop)));
    assert!(
        Group::Input.settings().iter().all(|setting| {
            plan.selection.contains(*setting) && restored.left.contains(*setting)
        })
    );
    root
}

#[test]
fn a_completion_read_failure_keeps_the_other_restored_group_until_recovery() {
    let root = admitted_partial_choice();
    let path = root.path().join("first-run.json");
    let before = std::fs::read(&path).unwrap();
    let completed = record_applied_with(root.path(), group(Group::Desktop), |_| {
        Err("injected temporary completion decision read failure".into())
    });
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "a failed completion read must not discard the remaining restored group"
    );
    assert!(completed.is_err());

    record_applied(root.path(), group(Group::Desktop)).unwrap();
    let (decision, restored) = decision_record(root.path()).unwrap();
    assert_eq!(decision, Decision::Pending);
    let restored = restored.unwrap();
    assert_eq!(restored.left, group(Group::Input));
    assert_eq!(restored.queued, Some(Selection::none()));
    assert!(!first_run_complete(root.path()).unwrap());

    // Only completing the remaining group allows the profile's applied marker.
    record_applied(root.path(), group(Group::Input)).unwrap();
    assert_eq!(
        decision_record(root.path()).unwrap(),
        (Decision::Applied(true), None)
    );
    assert!(first_run_complete(root.path()).unwrap());
}

#[test]
fn a_pending_read_failure_preserves_the_real_restored_choice() {
    let root = admitted_partial_choice();
    let path = root.path().join("first-run.json");
    let before = std::fs::read(&path).unwrap();
    let original = decision_record(root.path()).unwrap();
    let pending = record_pending_with(root.path(), |_| {
        Err("injected temporary pre-pass decision read failure".into())
    });
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "a failed pre-pass read must not replace Restore history with plain pending"
    );
    assert!(pending.is_err());

    record_pending(root.path()).unwrap();
    assert_eq!(decision_record(root.path()).unwrap(), original);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    record_applied(root.path(), group(Group::Desktop)).unwrap();
    let (decision, restored) = decision_record(root.path()).unwrap();
    assert_eq!(decision, Decision::Pending);
    assert_eq!(restored.unwrap().left, group(Group::Input));
}

/// Every input that establishes completion must be readable. Damaging a saved plan, its
/// progress or the decision itself cannot consume a partial pass's Restore history; repair
/// permits that same scoped completion and still leaves the other group restored.
#[test]
fn damaged_completion_inputs_preserve_the_choice_until_they_are_repaired() {
    for name in [
        "automatic-plan.json",
        "initial-setup.json",
        "first-run.json",
    ] {
        let root = admitted_partial_choice();
        let decision = root.path().join("first-run.json");
        let original_decision = std::fs::read(&decision).unwrap();
        let path = root.path().join(name);
        let original = std::fs::read(&path).unwrap();
        let damaged = b"{\"profile\":";
        std::fs::write(&path, damaged).unwrap();

        let completed = record_applied(root.path(), group(Group::Desktop));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            damaged,
            "{name}: unreadable input must not be replaced by a completion guess"
        );
        if name != "first-run.json" {
            assert_eq!(
                std::fs::read(&decision).unwrap(),
                original_decision,
                "{name}: completion cannot consume Restore history on an input error"
            );
        }
        assert!(completed.is_err(), "{name}");

        std::fs::write(&path, original).unwrap();
        record_applied(root.path(), group(Group::Desktop)).unwrap();
        let (decision, restored) = decision_record(root.path()).unwrap();
        assert_eq!(decision, Decision::Pending, "{name}");
        let restored = restored.unwrap();
        assert_eq!(restored.left, group(Group::Input), "{name}");
        assert_eq!(restored.queued, Some(Selection::none()), "{name}");
        assert!(!first_run_complete(root.path()).unwrap(), "{name}");
    }
}

#[test]
fn a_missing_legacy_completion_record_can_still_be_created() {
    for mark_pending in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        assert_eq!(
            decision_record(root.path()).unwrap(),
            (Decision::Missing, None)
        );
        if mark_pending {
            record_pending(root.path()).unwrap();
            assert_eq!(
                decision_record(root.path()).unwrap(),
                (Decision::Pending, None)
            );
        }
        record_applied(root.path(), Selection::all()).unwrap();
        assert_eq!(
            decision_record(root.path()).unwrap(),
            (Decision::Applied(true), None)
        );
        assert!(first_run_complete(root.path()).unwrap());
    }
}
