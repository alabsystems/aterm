// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deferred preference attempts must reach the same parked worker as ordinary Mac attempts.

use super::*;
use crate::macos_doctor::tests::{WIDGETS, Withholding};
use std::sync::OnceLock;

fn build() -> &'static str {
    static BUILD: OnceLock<String> = OnceLock::new();
    BUILD.get_or_init(aterm_types::version::build_id).as_str()
}

fn pass(stages: &mut WithholdingStages, root: &Path, revision: u64) -> (Report, SetupNext) {
    execute_initial_for_revision(stages, root, revision, &mut None, SetupHooks::NONE)
        .unwrap()
        .expect("the admitted run is unfinished")
}

fn park(root: &Path, now: u64) -> InitialSetupParkMark {
    initial_setup_park_mark_with(Some(root), None, now, || {
        panic!("these Mac settings runs select no network stage")
    })
}

fn failed_follow_up(stages: &mut WithholdingStages, root: &Path, writes: usize) {
    let report = stages
        .doctor
        .follow_up(root)
        .unwrap()
        .expect("Notification Center remains owed");
    assert!(report.needs_attention(), "{}", report.to_text());
    assert_eq!(stages.doctor.notification_center_writes(), writes);
    assert!(!stages.doctor.notification_center_cleared(), "rolled back");
    let record = load_withheld(root).unwrap();
    assert_eq!(record.items, [WIDGETS]);
    assert_eq!(record.failed.len(), 1);
    assert_eq!(record.failed[0].build, build());
    assert_eq!(record.failed[0].tries as usize, writes / 2);
}

fn first_deferred_failure_after_dock_is_spent()
-> (aterm_tempfile::TempDir, Plan, u64, WithholdingStages, u64) {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Dock]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut stages = WithholdingStages::over(
        Withholding::widgets_and_a_dock_that_never_verifies().with_build(build()),
    );
    // The real host advances its clock by 61 seconds per automatic pass. The
    // ordinary Mac deadline supplies Dock's second try after the first ten passes.
    for _ in 0..12 {
        let (report, next) = pass(&mut stages, root.path(), revision);
        assert!(report.needs_attention());
        assert_eq!(next, SetupNext::Retry);
    }
    assert_eq!(stages.doctor.notification_center_writes(), 0);
    let automatic = Automatic::load(root.path(), revision).unwrap();
    assert_eq!(automatic.failed.len(), 1);
    assert_eq!(automatic.failed[0].tries, 2);
    // Both failed Dock attempts relaunch and roll back; the successful
    // desktop-widget change also relaunches their shared Dock reader once.
    assert_eq!(stages.doctor.dock_relaunches(), 5);
    assert!(load_withheld(root.path()).unwrap().failed.is_empty());

    stages.doctor.grant();
    stages.doctor.notification_center_never_keeps();
    failed_follow_up(&mut stages, root.path(), 2);
    let first = load_withheld(root.path()).unwrap().failed[0].at;
    // An ordinary worker pass now honors that deferred pause. It cannot copy an
    // attempt it never made into Automatic.failed, and its Dock failure is spent.
    let (report, next) = pass(&mut stages, root.path(), revision);
    assert!(report.needs_attention());
    assert_eq!(next, SetupNext::Retry);
    assert_eq!(stages.doctor.notification_center_writes(), 2);
    let automatic = Automatic::load(root.path(), revision).unwrap();
    assert_eq!(automatic.failed.len(), 1);
    assert_eq!(automatic.failed[0].tries, 2);
    assert_eq!(
        progress(root.path()).unwrap().unwrap().done & Stage::Workstation.bit(),
        0
    );
    assert!(!finished_at(root.path()).unwrap());
    (root, plan, revision, stages, first)
}

fn assert_ineligible_debt_does_not_wake(root: &Path, plan: &Plan, due: u64) {
    let pending = progress(root).unwrap().unwrap();
    for retired in [
        Progress {
            completed: true,
            ..pending
        },
        Progress {
            stopped: true,
            ..pending
        },
        Progress {
            admitted: false,
            ..pending
        },
        Progress {
            done: Stage::Workstation.bit(),
            ..pending
        },
    ] {
        save_progress(root, retired).unwrap();
        assert_eq!(
            park(root, due).mac_settings_retry_due(),
            None,
            "{retired:?}"
        );
    }
    save_progress(root, pending).unwrap();
    let mut unselected = plan.clone();
    unselected.selection.set(Setting::Widgets, false);
    save_plan(root, &unselected).unwrap();
    assert_eq!(
        park(root, due).mac_settings_retry_due(),
        None,
        "mismatched plan"
    );
    let mut matched = pending;
    matched.bind(&unselected);
    save_progress(root, matched).unwrap();
    assert_eq!(
        park(root, due).mac_settings_retry_due(),
        None,
        "Widgets is not selected"
    );
    save_plan(root, plan).unwrap();
    save_progress(root, pending).unwrap();

    let record = load_withheld(root).unwrap();
    let mut unrelated = record.clone();
    unrelated.items = vec!["unrelated".into()];
    save_withheld(root, &unrelated).unwrap();
    assert_eq!(
        park(root, due).mac_settings_retry_due(),
        None,
        "the failed item is not owed"
    );
    let mut other_build = record.clone();
    other_build.failed[0].build = "another-build".into();
    save_withheld(root, &other_build).unwrap();
    assert_eq!(
        park(root, due).mac_settings_retry_due(),
        None,
        "another build"
    );
    save_withheld(root, &record).unwrap();
}

#[test]
fn deferred_mac_deadline_wakes_a_workstation_whose_other_attempts_are_spent() {
    let (root, plan, revision, mut stages, first) = first_deferred_failure_after_dock_is_spent();
    let due = first + SECOND_TRY_AFTER.as_secs();
    let before = park(root.path(), due - 1);
    let ready = park(root.path(), due);
    assert_eq!(before.mac_settings_retry_due(), None);
    assert_eq!(before, park(root.path(), due - 2), "no early wake");
    assert!(
        !ready.facts_changed_since(&before),
        "this is a deadline, not a local fact"
    );
    assert_ineligible_debt_does_not_wake(root.path(), &plan, due);

    // The same recorded follow-up really is eligible after ten minutes. These
    // actual effects run before the final missing-wake assertion on old source.
    stages.doctor.wait(SECOND_TRY_AFTER);
    failed_follow_up(&mut stages, root.path(), 4);
    assert_eq!(park(root.path(), due + 60).mac_settings_retry_due(), None);
    stages.doctor.wait(SECOND_TRY_AFTER);
    stages.doctor.follow_up(root.path()).unwrap().unwrap();
    assert_eq!(
        stages.doctor.notification_center_writes(),
        4,
        "no third try"
    );
    assert_eq!(revision_at(root.path()).unwrap(), revision);
    assert_eq!(
        progress(root.path()).unwrap().unwrap().done & Stage::Workstation.bit(),
        0
    );
    request_stop_at(root.path(), revision).unwrap();
    assert_eq!(park(root.path(), due).mac_settings_retry_due(), None);
    assert_eq!(
        ready.mac_settings_retry_due(),
        Some(due),
        "the deferred setting's second allowed try must wake the parked worker"
    );
}

#[test]
fn deferred_mac_deadline_uses_follow_up_scope_after_a_real_input_only_choice() {
    use aterm_types::macos_setup::Group;

    let root = aterm_tempfile::tempdir().unwrap();
    let mut plan = selected(&[Setting::Widgets]);
    let first_revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut stages = WithholdingStages::over(Withholding::widgets().with_build(build()));
    pass(&mut stages, root.path(), first_revision);
    assert!(finished_at(root.path()).unwrap());
    assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);

    for setting in Group::Input.settings() {
        plan.selection.set(*setting, true);
    }
    let revision = system_state_choice(root.path(), &plan);
    assert_ne!(revision, first_revision);
    let pending = progress(root.path()).unwrap().unwrap();
    assert_eq!(pending.only, Some(group_selection(Group::Input)));
    assert!(!pending.only.unwrap().contains(Setting::Widgets));
    assert!(
        withheld_selection(root.path())
            .unwrap()
            .unwrap()
            .contains(Setting::Widgets)
    );
    stages.doctor.grant();
    stages.doctor.notification_center_never_keeps();
    failed_follow_up(&mut stages, root.path(), 2);
    let due = load_withheld(root.path()).unwrap().failed[0].at + SECOND_TRY_AFTER.as_secs();
    let ready = park(root.path(), due);

    stages.doctor.wait(SECOND_TRY_AFTER);
    failed_follow_up(&mut stages, root.path(), 4);
    assert_eq!(park(root.path(), due).mac_settings_retry_due(), None);
    stages.doctor.wait(SECOND_TRY_AFTER);
    stages.doctor.follow_up(root.path()).unwrap().unwrap();
    assert_eq!(
        stages.doctor.notification_center_writes(),
        4,
        "no third try"
    );
    assert_eq!(progress(root.path()).unwrap().unwrap(), pending);
    assert_eq!(
        ready.mac_settings_retry_due(),
        Some(due),
        "the dedicated follow-up's approval is independent of the narrowed Workstation pass"
    );
}

fn unreadable_record<T>(root: &Path, name: &str, read: impl FnOnce() -> T) -> T {
    let path = root.join(name);
    let retained = root.join(format!("{name}.retained"));
    let before = std::fs::read(&path).unwrap();
    std::fs::rename(&path, &retained).unwrap();
    std::fs::create_dir(&path).unwrap();
    let result = read();
    assert!(path.is_dir(), "the failed read did not replace its record");
    assert_eq!(std::fs::read(&retained).unwrap(), before);
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&retained, &path).unwrap();
    result
}

#[test]
fn deferred_mac_deadline_known_debt_survives_an_unreadable_automatic_cache() {
    let (root, _, revision, stages, first) = first_deferred_failure_after_dock_is_spent();
    let due = first + SECOND_TRY_AFTER.as_secs();
    let pending = progress(root.path()).unwrap().unwrap();
    assert_eq!(park(root.path(), due).mac_settings_retry_due(), Some(due));

    unreadable_record(root.path(), HELD_RECORD, || {
        assert_eq!(
            park(root.path(), due).mac_settings_retry_due(),
            Some(due),
            "the actual deferred attempt remains due while the ordinary cache cannot be read"
        );
        unreadable_record(root.path(), WITHHELD_RECORD, || {
            assert_eq!(park(root.path(), due).mac_settings_retry_due(), None);
        });
    });
    unreadable_record(root.path(), WITHHELD_RECORD, || {
        assert_eq!(
            park(root.path(), due).mac_settings_retry_due(),
            None,
            "the known ordinary record is spent; an unknown deferred record proves no due try"
        );
    });
    assert_eq!(park(root.path(), due).mac_settings_retry_due(), Some(due));
    assert_eq!(progress(root.path()).unwrap().unwrap(), pending);
    assert_eq!(revision_at(root.path()).unwrap(), revision);
    assert_eq!(stages.doctor.notification_center_writes(), 2);
    assert_eq!(stages.doctor.dock_relaunches(), 5);
}

#[test]
fn deferred_mac_deadline_known_ordinary_try_survives_an_unreadable_debt_record() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Dock]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut stages = WithholdingStages::over(
        Withholding::widgets_and_a_dock_that_never_verifies().with_build(build()),
    );
    let (_, next) = pass(&mut stages, root.path(), revision);
    assert_eq!(next, SetupNext::Retry);
    let automatic = Automatic::load(root.path(), revision).unwrap();
    assert_eq!(automatic.failed.len(), 1);
    assert_eq!(automatic.failed[0].tries, 1);
    assert!(load_withheld(root.path()).unwrap().failed.is_empty());
    let pending = progress(root.path()).unwrap().unwrap();
    let due = automatic.failed[0].at + SECOND_TRY_AFTER.as_secs();
    assert_eq!(park(root.path(), due - 1).mac_settings_retry_due(), None);

    unreadable_record(root.path(), WITHHELD_RECORD, || {
        assert_eq!(
            park(root.path(), due).mac_settings_retry_due(),
            Some(due),
            "the actual ordinary attempt remains due while deferred metadata cannot be read"
        );
    });
    unreadable_record(root.path(), HELD_RECORD, || {
        assert_eq!(
            park(root.path(), due).mac_settings_retry_due(),
            None,
            "the readable deferred record has no attempt; an unknown ordinary record supplies none"
        );
    });
    assert_eq!(park(root.path(), due).mac_settings_retry_due(), Some(due));
    assert_eq!(progress(root.path()).unwrap().unwrap(), pending);
    assert_eq!(revision_at(root.path()).unwrap(), revision);
    assert_eq!(stages.doctor.notification_center_writes(), 0);
    assert_eq!(stages.doctor.dock_relaunches(), 3);
}
