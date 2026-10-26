// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A failed revision read cannot spend a withheld item's retry budget again.

use super::*;

fn admitted_withheld_widgets(root: &Path) -> (Withholding, u64) {
    let mut doctor = Withholding::widgets();
    doctor.explicit_apply(root).unwrap();
    let revision = initial::revision_at(root).unwrap();
    assert_ne!(revision, 0, "the explicit doctor pass admitted a real run");
    assert!(
        initial::initial_setup_snapshot_at(root, Default::default())
            .unwrap()
            .run_allowed
    );
    assert_eq!(Withholding::withheld(root), [WIDGETS]);
    doctor.grant();
    doctor.notification_center_never_keeps();
    (doctor, revision)
}

fn failed_follow_up(doctor: &mut Withholding, root: &Path) {
    let before = doctor.notification_center_writes();
    let report = doctor.follow_up(root).unwrap().expect("the follow-up runs");
    assert!(report.needs_attention(), "{}", report.to_text());
    assert_eq!(doctor.notification_center_writes(), before + 2);
    assert!(
        !doctor.notification_center_cleared(),
        "the try was rolled back"
    );
    assert_eq!(Withholding::withheld(root), [WIDGETS]);
}

fn spent_follow_ups(root: &Path) -> (Withholding, u64) {
    let (mut doctor, revision) = admitted_withheld_widgets(root);
    failed_follow_up(&mut doctor, root);
    doctor.wait(SECOND_TRY_AFTER);
    failed_follow_up(&mut doctor, root);
    let owed = load_withheld(root).unwrap();
    assert_eq!(owed.failed.len(), 1);
    assert_eq!(owed.failed[0].tries, 2);
    (doctor, revision)
}

#[test]
fn withheld_revision_a_read_failure_cannot_repeat_a_spent_same_run_follow_up() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (mut doctor, revision) = spent_follow_ups(root.path());
    let path = root.path().join(WITHHELD_RECORD);
    let before = std::fs::read(&path).unwrap();
    let events = doctor.host.events.len();
    let reads = std::cell::Cell::new(0);

    let result = follow_up_setup_reading_revision(
        root.path(),
        &mut doctor.host,
        doctor.build,
        doctor.now,
        || {
            reads.set(reads.get() + 1);
            Err("injected revision read failure".into())
        },
    );

    assert_eq!(reads.get(), 1);
    assert_eq!(
        doctor.notification_center_writes(),
        4,
        "a failed read did not prove a new run and must not allow a third try"
    );
    assert!(result.is_err(), "the revision read must remain unreadable");
    assert!(doctor.host.events[events..].is_empty());
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert_eq!(initial::revision_at(root.path()).unwrap(), revision);
    assert_eq!(load_withheld(root.path()).unwrap().failed[0].tries, 2);
    doctor.follow_up(root.path()).unwrap().unwrap();
    assert_eq!(
        doctor.notification_center_writes(),
        4,
        "still held after recovery"
    );
}

#[test]
fn withheld_revision_a_confirmed_new_run_reopens_the_follow_up_budget() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (mut doctor, revision) = spent_follow_ups(root.path());
    let events = doctor.host.events.len();
    doctor.follow_up(root.path()).unwrap().unwrap();
    assert!(
        doctor.host.events[events..].is_empty(),
        "the same run is held"
    );

    initial::request_stop_at(root.path(), revision).unwrap();
    initial::save_explicit_workstation_plan(root.path()).unwrap();
    let next = initial::revision_at(root.path()).unwrap();
    assert!(
        next > revision,
        "a real explicit admission replaced the stopped run"
    );
    assert!(
        initial::initial_setup_snapshot_at(root.path(), Default::default())
            .unwrap()
            .run_allowed
    );
    let owed = load_withheld(root.path()).unwrap();
    assert_eq!(owed.items, [WIDGETS]);
    assert!(owed.failed.is_empty());

    failed_follow_up(&mut doctor, root.path());
    assert_eq!(doctor.notification_center_writes(), 6);
    assert_eq!(load_withheld(root.path()).unwrap().failed[0].tries, 1);
    assert_eq!(initial::revision_at(root.path()).unwrap(), next);
}

#[test]
fn withheld_revision_a_failed_save_read_cannot_publish_an_unknown_retry_identity() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (mut doctor, revision) = admitted_withheld_widgets(root.path());
    failed_follow_up(&mut doctor, root.path());
    let path = root.path().join(WITHHELD_RECORD);
    let before = std::fs::read(&path).unwrap();
    let mut record = load_withheld(root.path()).unwrap();
    assert_eq!(record.failed[0].tries, 1);
    record.failed[0].tries = 2;

    let result = save_withheld_reading_revision(root.path(), &record, || {
        Err("injected revision read failure".into())
    });

    assert!(
        result.is_err(),
        "an unknown run must not be written as revision null"
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert_eq!(load_withheld(root.path()).unwrap().failed[0].tries, 1);
    assert_eq!(initial::revision_at(root.path()).unwrap(), revision);
    assert_eq!(doctor.notification_center_writes(), 2);
}

#[test]
fn withheld_revision_absent_legacy_progress_is_a_known_zero_revision() {
    let root = aterm_tempfile::tempdir().unwrap();
    assert_eq!(initial::revision_at(root.path()).unwrap(), 0);
    record_withheld(root.path(), &[WIDGETS.to_owned()], &[], &[], ("b", 0)).unwrap();
    record_withheld(root.path(), &[], &[], &[WIDGETS.to_owned()], ("b", 0)).unwrap();
    let record = load_withheld(root.path()).unwrap();
    assert_eq!(record.failed[0].tries, 1);
    let bytes = std::fs::read(root.path().join(WITHHELD_RECORD)).unwrap();
    let saved: Value = aterm_json::from_slice(&bytes).unwrap();
    assert_eq!(saved["revision"].as_u64(), Some(0));
}
