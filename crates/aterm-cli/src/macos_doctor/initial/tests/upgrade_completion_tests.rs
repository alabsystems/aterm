// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Automatic widening carries completed settings, never completion of added settings.

use super::*;

fn partial(root: &Path, applied: Selection) -> Plan {
    let explicit = save_explicit_workstation_plan(root, applied).unwrap();
    complete_explicit_workstation_plan_for_test(root, explicit).unwrap();
    let plan = saved_plan(root).unwrap().unwrap();
    assert!(saved_plan_provisional(root).unwrap());
    assert_eq!(plan.selection.workstation(), applied);
    plan
}

fn upgrade(root: &Path) -> (InitialSetupAuthorization, Plan) {
    let records = observation_at(root).unwrap().records;
    assert!(scope::upgrade_of(&records, || false).is_none());
    let full = scope::upgrade_of(&records, || true).unwrap();
    let mut ticket = InitialSetupAuthorization::capture_at(root).unwrap();
    ticket.offer_reading(root, || Ok(None)).unwrap();
    (ticket, full)
}

#[test]
fn automatic_widening_runs_added_settings_after_a_completed_partial_profile() {
    for applied in [
        selected(&[Setting::Dock]).selection,
        selected(&[Setting::Messages]).selection,
        preferences(Selection::all()),
    ] {
        for interrupted in [false, true] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let root = dir.path();
            let previous = partial(root, applied);
            // Finish the base stages through their real coordinator, leaving a completed
            // automatic partial plan of the same form the explicit writer publishes.
            execute_initial(&mut Fake::default(), root, &mut None).unwrap();
            let before = progress(root).unwrap().unwrap();
            assert!(before.admitted && before.completed);
            let (mut ticket, full) = upgrade(root);
            let added = filtered(preferences(full.selection), |setting| {
                !previous.selection.contains(setting)
            });
            if interrupted {
                assert!(
                    ticket
                        .persist_for_watch_test(root, &full, true, Some((0, true)))
                        .is_err(),
                    "cut after the plan and recovery checkpoint were published"
                );
                let mut early = Fake::default();
                execute_initial(&mut early, root, &mut None).unwrap();
                assert!(
                    early.calls.is_empty(),
                    "an unadmitted checkpoint runs nothing"
                );
            }
            let revision = ticket.upgrade_at(root, &full).result.unwrap().unwrap();
            let widened = progress(root).unwrap().unwrap();
            assert!(revision > before.revision);
            assert_eq!(
                widened.done & Stage::Workstation.bit() != 0,
                added.is_empty(),
                "new preferences have not run: {applied:?}, cut={interrupted}"
            );
            assert_eq!(
                widened.only,
                (!added.is_empty()).then_some(added),
                "already completed preferences must not be reapplied"
            );
            assert_eq!(
                widened.done & Stage::Accounts.bit(),
                0,
                "a newly selected sign-out has not run"
            );
            for unchanged in [
                Stage::Spotlight,
                Stage::Permissions,
                Stage::Secrets,
                Stage::Repositories,
            ] {
                assert_ne!(widened.done & unchanged.bit(), 0, "{unchanged:?}");
            }
            let mut worker = Fake::default();
            let report = execute_initial(&mut worker, root, &mut None)
                .unwrap()
                .unwrap();
            assert!(report.complete(), "{}", report.to_text());
            assert_eq!(
                worker.only,
                if added.is_empty() {
                    vec![]
                } else {
                    vec![Some(added)]
                }
            );
            assert_eq!(
                worker.calls,
                if added.is_empty() {
                    vec![Stage::Accounts]
                } else {
                    vec![Stage::Workstation, Stage::Accounts]
                }
            );
            assert!(progress(root).unwrap().unwrap().completed);
        }
    }
}

#[test]
fn an_automatic_partial_upgrade_cannot_cross_a_later_hold_or_choice() {
    for held in ["stop", "restore", "choice"] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let previous = partial(root, selected(&[Setting::Dock]).selection);
        let revision = progress(root).unwrap().unwrap().revision;
        let (mut ticket, full) = upgrade(root);
        match held {
            "stop" => request_stop_at(root, revision).unwrap(),
            "restore" => save_restore(root).unwrap(),
            "choice" => {
                system_state_choice(root, &previous);
            }
            _ => unreachable!(),
        }
        let before = observation_at(root).unwrap();
        assert!(scope::upgrade_of(&before.records, || true).is_none());
        assert_eq!(ticket.upgrade_at(root, &full).result.unwrap(), None);
        assert_eq!(observation_at(root).unwrap(), before, "{held}");
    }
}
