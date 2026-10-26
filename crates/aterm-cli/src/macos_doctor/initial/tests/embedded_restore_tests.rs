// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A run's deferred Restore remains a Restore beside its other stage results.

use super::*;
use crate::macos_doctor::tests::Withholding;
use aterm_types::macos_setup::{Group, PutBack};

#[derive(Clone, Copy, Debug)]
enum PutBackCase {
    Back,
    Failed,
    AtLogin,
}

fn embedded_restore(case: PutBackCase) {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut plan = Plan::from(group_selection(Group::Desktop));
    plan.selection.set(Setting::Permissions, true);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut stages = WithholdingStages::over(Withholding::widgets());
    stages.doctor.grant();
    execute_initial_for_revision(
        &mut stages,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .expect("the original setup ran");
    assert!(stages.doctor.notification_center_cleared());

    // A supported older pending setup has no modern Progress.
    // Restore brings only the run it needs for aterm's permissions forward.
    std::fs::remove_file(root.path().join("initial-setup.json")).unwrap();
    save_automatic_plan(root.path(), Selection::all().workstation()).unwrap();
    save_workstation_decision(root.path(), false).unwrap();
    stages.doctor.revoke();
    let waiting = stages.doctor.restore(root.path()).unwrap();
    assert!(
        waiting
            .restore
            .unwrap()
            .outcome
            .rows
            .contains(&(Some(Setting::Widgets), PutBack::Waits,))
    );
    assert_eq!(
        super::super::super::restore_waiting(root.path())
            .unwrap()
            .len(),
        1
    );
    let backup = std::fs::read(root.path().join("original-preferences.json")).unwrap();
    assert!(bring_forward_restored_at(root.path()).unwrap().is_some());
    let revision = launch_startup(root.path()).expect("startup admits the brought-forward run");
    let carried = saved_plan(root.path()).unwrap().unwrap();
    let pending = progress(root.path()).unwrap().unwrap();
    assert!(carried.selection.contains(Setting::Permissions));
    assert!(!carried.selection.contains(Setting::Widgets));
    assert_eq!(
        Stage::ALL
            .into_iter()
            .filter(|stage| stage.selected(&carried) && pending.done & stage.bit() == 0)
            .collect::<Vec<_>>(),
        [Stage::Permissions],
        "the retained sign-outs are already finished"
    );

    match case {
        PutBackCase::Back => {}
        PutBackCase::Failed => stages.doctor.refuse_notification_center_restore(),
        PutBackCase::AtLogin => stages.doctor.fail_next_relaunch(),
    }
    stages.grant_in_permissions = true;
    let (writes, relaunches, desktop, calls) = (
        stages.doctor.notification_center_writes(),
        stages.doctor.notification_center_relaunches(),
        stages.doctor.window_manager_writes(),
        stages.rest.calls.len(),
    );
    let (report, next) = execute_initial_for_revision(
        &mut stages,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .expect("the permissions run performs its own deferred follow-up");
    assert_eq!(&stages.rest.calls[calls..], &[Stage::Permissions]);
    assert_eq!(next, SetupNext::Done);
    assert!(finished_at(root.path()).unwrap());
    assert!(
        super::super::super::restore_waiting(root.path())
            .unwrap()
            .is_empty()
    );
    assert_eq!(stages.doctor.window_manager_writes(), desktop);
    assert_eq!(
        std::fs::read(root.path().join("original-preferences.json")).unwrap(),
        backup
    );
    let failed = matches!(case, PutBackCase::Failed);
    assert_eq!(stages.doctor.notification_center_cleared(), failed);
    assert_eq!(
        stages.doctor.notification_center_writes(),
        writes + usize::from(!failed)
    );
    assert_eq!(
        stages.doctor.notification_center_relaunches(),
        relaunches + usize::from(!failed)
    );
    assert_eq!(report.needs_attention(), !matches!(case, PutBackCase::Back));
    assert!(
        report
            .rows
            .iter()
            .any(|row| row.name.contains("com.apple.notificationcenterui"))
    );

    let effects = (
        stages.doctor.notification_center_writes(),
        stages.doctor.notification_center_relaunches(),
    );
    assert!(stages.doctor.follow_up(root.path()).unwrap().is_none());
    assert_eq!(
        (
            stages.doctor.notification_center_writes(),
            stages.doctor.notification_center_relaunches()
        ),
        effects
    );
    assert!(
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE
        )
        .unwrap()
        .is_none()
    );

    let expected = match case {
        PutBackCase::Back => PutBack::Back,
        PutBackCase::Failed => PutBack::Failed,
        PutBackCase::AtLogin => PutBack::AtLogin,
    };
    assert_eq!(
        report
            .restore
            .as_ref()
            .map(|restore| restore.outcome.rows.as_slice()),
        Some([(Some(Setting::Widgets), expected)].as_slice()),
        "the enclosing pass must retain the actual deferred Restore outcome: {case:?}"
    );
    let page = report.page.as_ref().expect("the run keeps its stage page");
    assert!(
        page.rows
            .iter()
            .any(|row| row.name == Stage::Permissions.name() && row.status == "ok")
    );
    assert!(
        page.rows
            .iter()
            .all(|row| { !row.name.contains("com.apple.") && !row.name.starts_with("Reload ") }),
        "raw Restore keys and reader failures cannot replace its structured outcome"
    );
}

#[test]
fn embedded_restore_keeps_its_success_beside_the_permissions_stage() {
    embedded_restore(PutBackCase::Back);
}

#[test]
fn embedded_restore_keeps_its_failed_put_back_beside_the_permissions_stage() {
    embedded_restore(PutBackCase::Failed);
}

#[test]
fn embedded_restore_keeps_its_at_login_result_beside_the_permissions_stage() {
    embedded_restore(PutBackCase::AtLogin);
}
