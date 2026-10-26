// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn spent_init_follow_ups(root: &Path) -> Withholding {
    use aterm_machines::init::Held as _;

    let hold = init_hold_at(root, true, &|_| panic!("uncontended init hold")).unwrap();
    hold.withheld(&[WIDGETS.into()]).unwrap();
    drop(hold); // The raw Desktop lane was interrupted before it verified.
    let snapshot = initial::initial_setup_snapshot_at(root, Default::default()).unwrap();
    assert!(snapshot.run_allowed && !snapshot.finished);
    let progress: Value =
        aterm_json::from_slice(&std::fs::read(root.join("initial-setup.json")).unwrap()).unwrap();
    assert_eq!(
        progress["done"].as_u64(),
        Some(0),
        "no Mac stage was verified"
    );
    assert!(
        !root.join(HELD_RECORD).exists(),
        "raw init has no automatic settled cache"
    );

    let mut doctor = Withholding::widgets_and_a_dock_that_never_verifies();
    doctor.grant();
    doctor.notification_center_never_keeps();
    for attempt in 1..=2 {
        let report = doctor
            .follow_up(root)
            .unwrap()
            .expect("the admitted run owes its withheld item");
        assert!(report.needs_attention(), "{}", report.to_text());
        assert_eq!(
            doctor.notification_center_writes(),
            attempt * 2,
            "the attempt and its rollback"
        );
        assert!(!doctor.notification_center_cleared());
        if attempt == 1 {
            doctor.wait(SECOND_TRY_AFTER);
        }
    }
    let owed = load_withheld(root).unwrap();
    assert_eq!(owed.items, [WIDGETS]);
    assert_eq!(owed.failed.len(), 1);
    assert_eq!(owed.failed[0].tries, 2);
    assert!(!root.join(HELD_RECORD).exists());
    let after = initial::initial_setup_snapshot_at(root, Default::default()).unwrap();
    assert!(after.run_allowed && !after.finished);
    assert_eq!(after.revision, snapshot.revision);
    doctor
}

fn automatic_preferences(doctor: &mut Withholding, root: &Path) -> Result<Option<Report>, String> {
    apply_part_with(
        root,
        &Inert::default(),
        &mut doctor.host,
        doctor.build,
        doctor.now,
        Part::Preferences { only: None },
    )
}

#[test]
fn automatic_withheld_read_failure_cannot_repeat_a_spent_init_follow_up() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut doctor = spent_init_follow_ups(root.path());
    let _profile = HeldLock::take(root.path()).unwrap();
    let path = root.path().join(WITHHELD_RECORD);
    let parked = root.path().join("withheld.saved");
    let bytes = std::fs::read(&path).unwrap();
    let backup = std::fs::read(root.path().join("original-preferences.json")).unwrap();
    let before = initial::observation_at(root.path()).unwrap();
    let events = doctor.host.events.len();
    std::fs::rename(&path, &parked).unwrap();
    std::fs::create_dir(&path).unwrap();

    let result = automatic_preferences(&mut doctor, root.path());
    assert_eq!(
        doctor.notification_center_writes(),
        4,
        "an unreadable retry record cannot authorize a third attempt"
    );
    assert!(
        result.is_err(),
        "the real owed-record I/O failure must reach the caller"
    );
    assert!(doctor.host.events[events..].is_empty());
    assert_eq!(
        std::fs::read(root.path().join("original-preferences.json")).unwrap(),
        backup
    );
    assert_eq!(initial::observation_at(root.path()).unwrap(), before);
    assert!(!root.path().join(HELD_RECORD).exists());
    assert_eq!(std::fs::read(&parked).unwrap(), bytes);

    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&parked, &path).unwrap();
    doctor.now = std::fs::metadata(root.path().join("automatic-attempt.json"))
        .unwrap()
        .modified()
        .unwrap()
        + std::time::Duration::from_secs(60);
    let report = automatic_preferences(&mut doctor, root.path())
        .unwrap()
        .unwrap();
    assert!(report.needs_attention());
    assert_eq!(
        doctor.notification_center_writes(),
        4,
        "repair restores the same spent budget"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn automatic_withheld_readable_debt_holds_the_same_admitted_run() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut doctor = spent_init_follow_ups(root.path());
    let _profile = HeldLock::take(root.path()).unwrap();
    let before = std::fs::read(root.path().join(WITHHELD_RECORD)).unwrap();
    let report = automatic_preferences(&mut doctor, root.path())
        .unwrap()
        .unwrap();
    assert!(report.needs_attention());
    let row = report.rows.iter().find(|row| row.name == WIDGETS).unwrap();
    assert_eq!(
        (row.status.as_str(), row.detail.as_str()),
        ("blocked", HELD)
    );
    assert_eq!(doctor.notification_center_writes(), 4);
    assert_eq!(
        std::fs::read(root.path().join(WITHHELD_RECORD)).unwrap(),
        before
    );
    assert!(
        doctor.window_manager_writes() > 0,
        "independent preferences still run"
    );
}

#[test]
fn automatic_withheld_without_fda_keeps_contained_writes_off_and_runs_other_preferences() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut doctor = spent_init_follow_ups(root.path());
    let _profile = HeldLock::take(root.path()).unwrap();
    doctor.revoke();
    let path = root.path().join(WITHHELD_RECORD);
    let parked = root.path().join("withheld.saved");
    let before = std::fs::read(&path).unwrap();
    std::fs::rename(&path, &parked).unwrap();
    std::fs::create_dir(&path).unwrap();
    let report = automatic_preferences(&mut doctor, root.path())
        .unwrap()
        .unwrap();
    assert!(
        report.needs_attention(),
        "the record error remains in the report"
    );
    assert_eq!(
        doctor.notification_center_writes(),
        4,
        "the FDA boundary already forbids contained writes"
    );
    assert!(
        doctor.window_manager_writes() > 0,
        "no owed read is needed for unrelated reachable preferences"
    );
    assert_eq!(std::fs::read(&parked).unwrap(), before);
}
