// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const CORRUPT: &[u8] = b"{\"profile\":";

fn plan_with(settings: &[Setting]) -> Plan {
    let mut selection = Selection::none();
    for setting in settings {
        selection.set(*setting, true);
    }
    Plan {
        selection,
        vault_url: "http://office.local:7484/v1/team".into(),
    }
}

fn setting_up(root: &Path, plan: &Plan) {
    write_json(&root.join(PLAN), &plan_record(plan, false)).unwrap();
    write_json(
        &root.join(DECISION),
        &json!({ "profile": PROFILE, "state": "applied", "accounts_verified": true }),
    )
    .unwrap();
    std::fs::write(root.join(PROGRESS), CORRUPT).unwrap();
}

fn saved(root: &Path) -> Plan {
    plan_from(&std::fs::read(root.join(PLAN)).unwrap())
        .unwrap()
        .0
}

/// The outcome a set-aside leaves, for comparing one cut short and finished with one that
/// ran through.
fn outcome(root: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.ends_with(".lock"))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let bytes = std::fs::read(root.join(&name)).ok();
            (name, bytes)
        })
        .collect()
}

/// A progress record that does not parse is kept beside itself and the setup goes on as
/// from a fresh record: the Mac preference groups left as is, aterm's own permissions set
/// up, the vault and repositories as the plan had them, the decision pending so the next
/// startup admits the plan by itself, and a fresh checkpoint that only Stop can use.
#[test]
fn a_corrupt_progress_record_is_kept_aside_and_setup_goes_on_from_a_fresh_one() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let chosen = plan_with(&[
        Setting::Dock,
        Setting::Messages,
        Setting::Permissions,
        Setting::Secrets,
    ]);
    setting_up(root, &chosen);

    let set = set_aside_unreadable_setup_at(root, false).unwrap().unwrap();
    assert_eq!(set.kept, ["initial-setup.json.unreadable"]);
    assert_eq!(
        std::fs::read(root.join("initial-setup.json.unreadable")).unwrap(),
        CORRUPT
    );
    let recovered = saved(root);
    assert_eq!(
        recovered,
        plan_with(&[Setting::Permissions, Setting::Secrets])
    );
    assert_eq!(decision_at(root).unwrap(), Decision::Pending);
    assert_eq!(
        progress_file(root).unwrap(),
        Some(Progress {
            revision: set.stop_target,
            ..Progress::for_plan(&recovered)
        })
    );
    assert!(admission_at(root).unwrap().is_none());
    assert!(!saved_plan_provisional(root).unwrap(), "never widened");
    assert!(shown(root));
    // Taken again, it finds nothing to do.
    assert_eq!(set_aside_unreadable_setup_at(root, false).unwrap(), None);
    // A person's choice since takes the note away.
    forget_at(root).unwrap();
    assert!(!shown(root));
    assert!(root.join("initial-setup.json.unreadable").exists(), "kept");
}

/// A lost plan is replaced by a fresh install's: the vault and repositories set up on an
/// ALab Mac only; a lost decision becomes pending.
#[test]
fn a_lost_plan_or_decision_goes_on_as_a_fresh_install_would() {
    for alab in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        std::fs::write(root.join(PLAN), CORRUPT).unwrap();
        std::fs::write(root.join(DECISION), b"[]").unwrap();
        let set = set_aside_unreadable_setup_at(root, alab).unwrap().unwrap();
        assert_eq!(
            set.kept,
            [
                "automatic-plan.json.unreadable",
                "first-run.json.unreadable"
            ]
        );
        let mut fresh = vec![Setting::Permissions];
        if alab {
            fresh.extend([Setting::Secrets, Setting::Repositories]);
        }
        let recovered = saved(root);
        assert_eq!(recovered.selection, plan_with(&fresh).selection, "{alab}");
        assert_eq!(
            recovered.vault_url,
            aterm_types::macos_setup::DEFAULT_VAULT_URL
        );
        assert_eq!(decision_at(root).unwrap(), Decision::Pending);
    }
}

/// A person's Stop or Cancel holds aterm's own permissions through a set-aside: the plan it
/// goes on with sets up nothing and the decision stands stopped, so no launch offers it, and
/// the Stop request that named the lost run goes, so it never stops a later run that takes
/// its number.
#[test]
fn a_stop_or_cancel_holds_through_a_set_aside() {
    for cancelled in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        setting_up(root, &plan_with(&[Setting::Permissions, Setting::Dock]));
        if cancelled {
            write_json(
                &root.join(DECISION),
                &json!({ "profile": PROFILE, "state": "cancelled" }),
            )
            .unwrap();
        } else {
            request_stop_at(root, 1).unwrap();
        }
        set_aside_unreadable_setup_at(root, true).unwrap().unwrap();
        assert!(saved(root).selection.is_empty(), "{cancelled}");
        assert!(stop_request_at(root).is_none());
        assert_eq!(
            decision_at(root).unwrap(),
            Decision::Stopped(Stop::Cancelled),
            "{cancelled}"
        );
        assert!(finished_at(root).unwrap());
    }
}

#[test]
fn a_stop_after_an_interrupted_recovery_holds_its_replacement_plan() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    setting_up(root, &plan_with(&[Setting::Permissions, Setting::Secrets]));
    assert!(
        set_aside_at(root, || false, &mut |at| {
            if at == Step::Plan {
                Err("the process ended after saving its marker".into())
            } else {
                Ok(())
            }
        })
        .is_err()
    );
    assert!(!marker_at(root).unwrap().unwrap().held);
    request_stop_at(root, 3).unwrap();

    set_aside_unreadable_setup_at(root, false).unwrap().unwrap();
    assert!(saved(root).selection.is_empty(), "the later Stop holds");
    assert!(marker_at(root).unwrap().unwrap().held);
    assert!(stop_request_at(root).is_none());
    assert_eq!(
        decision_at(root).unwrap(),
        Decision::Stopped(Stop::Cancelled)
    );
    assert_eq!(set_aside_unreadable_setup_at(root, false).unwrap(), None);
}

#[test]
fn a_stop_before_the_recovery_marker_is_saved_holds_its_replacement_plan() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    setting_up(root, &plan_with(&[Setting::Permissions, Setting::Secrets]));
    set_aside_at(root, || false, &mut |at| {
        if at == Step::Mark {
            request_stop_at(root, 3)?;
        }
        Ok(())
    })
    .unwrap()
    .unwrap();
    assert!(saved(root).selection.is_empty(), "the later Stop holds");
    assert!(marker_at(root).unwrap().unwrap().held);
    assert!(stop_request_at(root).is_none());
    assert_eq!(
        decision_at(root).unwrap(),
        Decision::Stopped(Stop::Cancelled)
    );
}

#[test]
fn a_damaged_recovery_marker_cannot_release_a_recorded_stop() {
    for damaged in [
        CORRUPT.to_vec(),
        aterm_json::to_vec(&json!({
            "profile": PROFILE, "kept": [], "held": false,
            "under_way": true, "binding": ["not a checkpoint"],
        }))
        .unwrap(),
    ] {
        damaged_marker_holds(&damaged);
    }
}

fn damaged_marker_holds(damaged: &[u8]) {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    setting_up(root, &plan_with(&[Setting::Permissions, Setting::Secrets]));
    request_stop_at(root, 3).unwrap();
    assert!(
        set_aside_at(root, || false, &mut |at| {
            if at == Step::Done {
                Err("the process ended before finishing its marker".into())
            } else {
                Ok(())
            }
        })
        .is_err()
    );
    assert!(saved(root).selection.is_empty());
    assert!(stop_request_at(root).is_none());
    assert!(!progress_file(root).unwrap().unwrap().admitted);
    std::fs::write(root.join(MARKER), damaged).unwrap();

    // Every record parses: the marker is written again, finished, holding what they hold.
    assert_eq!(set_aside_unreadable_setup_at(root, false).unwrap(), None);
    assert!(saved(root).selection.is_empty(), "the earlier Stop holds");
    let marker = marker_at(root).unwrap().unwrap();
    assert!(marker.held && !marker.under_way);
    assert_eq!(
        decision_at(root).unwrap(),
        Decision::Stopped(Stop::Cancelled)
    );
    assert_eq!(
        std::fs::read(root.join("set-aside.json.unreadable")).unwrap(),
        damaged,
        "the damaged marker is kept before replacement"
    );
}

/// A read that fails for another reason than a parse says nothing of the record: it is an
/// error the caller tries again, and nothing is set aside.
#[test]
fn a_record_that_cannot_be_read_just_now_is_never_set_aside() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir(root.join(DECISION)).unwrap();
    std::fs::write(root.join(PROGRESS), CORRUPT).unwrap();
    assert!(set_aside_unreadable_setup_at(root, false).is_err());
    assert!(!root.join("initial-setup.json.unreadable").exists());
    assert_eq!(std::fs::read(root.join(PROGRESS)).unwrap(), CORRUPT);
}

#[test]
fn a_recovered_checkpoint_carries_no_old_stage_or_permission_authority() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    let old_plan = plan_with(&[Setting::Dock, Setting::Permissions, Setting::Secrets]);
    write_json(&root.join(PLAN), &plan_record(&old_plan, false)).unwrap();
    save_progress(
        root,
        Progress {
            revision: 8,
            access: Some(3),
            done: Stage::KNOWN,
            completed: true,
            admitted: true,
            ..Progress::for_plan(&old_plan)
        },
    )
    .unwrap();
    std::fs::write(root.join(DECISION), CORRUPT).unwrap();

    let set = set_aside_unreadable_setup_at(root, false).unwrap().unwrap();
    let recovered = saved(root);
    let staged = progress_file(root).unwrap().unwrap();
    assert_eq!(
        recovered,
        plan_with(&[Setting::Permissions, Setting::Secrets])
    );
    assert!(set.stop_target > 8);
    assert_eq!(
        staged,
        Progress {
            revision: set.stop_target,
            ..Progress::for_plan(&recovered)
        }
    );
    assert_eq!(access_revision_at(root).unwrap(), set.stop_target);
    assert!(admission_at(root).unwrap().is_none());
    assert!(!finished_at(root).unwrap());

    let admitted = InitialSetupAuthorization::capture_at(root)
        .unwrap()
        .persist_at(root, &recovered)
        .result
        .unwrap()
        .expect("without Stop the recovered plan is offered and admitted");
    assert!(admitted > set.stop_target);
    assert_eq!(admission_at(root).unwrap().unwrap().revision, admitted);
}

#[test]
fn a_recovered_checkpoint_stop_holds_relaunch_but_not_a_later_manual_choice() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    setting_up(
        root,
        &plan_with(&[Setting::Dock, Setting::Permissions, Setting::Secrets]),
    );
    let set = set_aside_unreadable_setup_at(root, false).unwrap().unwrap();
    let recovered = saved(root);
    request_stop_at(root, set.stop_target).unwrap();

    let snapshot = snapshot_at(root, Plan::default()).unwrap();
    assert!(snapshot.finished && !snapshot.permissions_allowed);
    assert_eq!(snapshot.plan, recovered);
    assert_eq!(saved(root), recovered, "Stop preserves every saved group");
    assert!(
        InitialSetupAuthorization::capture_at(root)
            .unwrap()
            .persist_at(root, &recovered)
            .result
            .unwrap()
            .is_none(),
        "a fresh startup cannot admit the stopped checkpoint"
    );

    let later = observe_mutation_at(root, None, Err("busy".into()), |root| {
        InitialSetupRestart::chosen().persist_with(
            root,
            &recovered,
            false,
            || Ok(None),
            save_progress,
            |root| observation_at(root).map_err(|error| error.to_string()),
        )
    })
    .result
    .unwrap()
    .expect("a later manual choice has independent authority");
    assert!(later > set.stop_target);
    // A delayed old saver still names only the recovered checkpoint.
    request_stop_at(root, set.stop_target).unwrap();
    assert!(!finished_at(root).unwrap());
    assert_eq!(admission_at(root).unwrap().unwrap().revision, later);
    assert_eq!(saved(root), recovered);
}

fn interrupt_recovery(root: &Path, cut: Step) -> Option<u64> {
    let prior_plan = plan_with(&[Setting::Permissions, Setting::Secrets]);
    setting_up(root, &prior_plan);
    if cut != Step::Done {
        save_progress(
            root,
            Progress {
                revision: 8,
                admitted: true,
                ..Progress::for_plan(&prior_plan)
            },
        )
        .unwrap();
        if cut == Step::Decision {
            std::fs::write(root.join(PLAN), CORRUPT).unwrap();
        } else {
            std::fs::write(root.join(DECISION), CORRUPT).unwrap();
        }
    }
    assert!(
        set_aside_at(root, || false, &mut |step| {
            if step == cut {
                Err("the recovery process ended".into())
            } else {
                Ok(())
            }
        })
        .is_err()
    );
    marker_at(root).unwrap().unwrap().revision
}

fn newer_choice_during_recovery(cut: Step, interrupted: bool) {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    let old_target = interrupt_recovery(root, cut);
    let chosen = plan_with(&[Setting::Dock]);
    let result = observe_mutation_at(root, None, Err("busy".into()), |root| {
        if interrupted {
            stage_restart_with(
                root,
                &chosen,
                Restart {
                    chosen: true,
                    joins: false,
                    automatic: false,
                },
                AdmittedVault::None,
                save_progress,
                |step| {
                    if step == RestartWrite::Progress {
                        Err("the later choice ended after its plan publication".into())
                    } else {
                        Ok(())
                    }
                },
            )
            .map(|progress| Some(progress.revision))
        } else {
            InitialSetupRestart::chosen().persist_with(
                root,
                &chosen,
                false,
                || Ok(None),
                save_progress,
                |root| observation_at(root).map_err(|error| error.to_string()),
            )
        }
    })
    .result;
    assert_eq!(result.is_err(), interrupted);
    let current = progress_file(root).unwrap().unwrap();
    if interrupted {
        if cut == Step::Done {
            assert_eq!(Some(current.revision), old_target);
        } else {
            assert_eq!(current.revision, 8);
        }
        assert!(!current.belongs_to(&chosen));
    } else {
        assert!(current.revision > old_target.unwrap_or(8) && current.admitted);
    }
    assert!(marker_at(root).unwrap().unwrap().under_way);
    let records = || {
        [PLAN, PROGRESS, DECISION, "run-revision.json"]
            .map(|name| std::fs::read(root.join(name)).ok())
    };
    let before = records();

    assert!(
        set_aside_unreadable_setup_at(root, false)
            .unwrap()
            .is_none()
    );
    assert_eq!(saved(root), chosen, "the person's later selection survives");
    assert_eq!(
        records(),
        before,
        "retired recovery cannot rewrite its successor"
    );
}

#[test]
fn an_interrupted_recovery_does_not_overwrite_a_later_admitted_choice() {
    newer_choice_during_recovery(Step::Done, false);
}

#[test]
fn an_interrupted_recovery_does_not_overwrite_a_later_partially_saved_choice() {
    newer_choice_during_recovery(Step::Done, true);
}

#[test]
fn an_unpublished_recovery_does_not_overwrite_a_later_partially_saved_choice() {
    newer_choice_during_recovery(Step::Progress, true);
}

#[test]
fn an_unreserved_recovery_does_not_overwrite_a_later_partially_saved_choice() {
    newer_choice_during_recovery(Step::Revision, true);
}

#[test]
fn an_unfinished_recovery_decision_does_not_overwrite_a_later_partially_saved_choice() {
    newer_choice_during_recovery(Step::Decision, true);
}

#[test]
fn an_interrupted_recovery_does_not_overwrite_a_later_restore_decision() {
    for cut in [Step::Revision, Step::Done] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        interrupt_recovery(root, cut);
        let progress_before = progress_file(root).unwrap();
        observe_mutation_at(root, None, Err("busy".into()), save_restore)
            .result
            .unwrap();
        assert_eq!(progress_file(root).unwrap(), progress_before);
        let before = [PLAN, PROGRESS, DECISION, "run-revision.json"]
            .map(|name| std::fs::read(root.join(name)).ok());

        assert!(
            set_aside_unreadable_setup_at(root, false)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            [PLAN, PROGRESS, DECISION, "run-revision.json"]
                .map(|name| std::fs::read(root.join(name)).ok()),
            before,
            "the full Restore decision and its plan survive: {cut:?}"
        );
    }
}

#[test]
fn a_published_recovery_target_is_reported_even_when_done_returns_an_error() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    setting_up(root, &plan_with(&[Setting::Permissions, Setting::Secrets]));
    let mut target = None;
    assert!(
        set_aside_unreadable_setup_at_observing_done_error(root, false, |revision| {
            target = Some(revision);
        })
        .is_err()
    );
    let target = target.expect("Stop evidence precedes the failed finalization result");
    assert_eq!(progress_file(root).unwrap().unwrap().revision, target);
    assert!(!marker_at(root).unwrap().unwrap().under_way);
    assert!(
        set_aside_unreadable_setup_at_observing(root, false, |_| {
            panic!("a later retry cannot adopt an earlier repair's target")
        })
        .unwrap()
        .is_none()
    );
    let plan = saved(root);
    request_stop_at(root, target).unwrap();
    assert!(finished_at(root).unwrap());
    assert_eq!(saved(root), plan);
    assert!(
        InitialSetupAuthorization::capture_at(root)
            .unwrap()
            .persist_at(root, &plan)
            .result
            .unwrap()
            .is_none()
    );
}

/// A set-aside cut short before any step finishes on the next try exactly as one that ran
/// through, the hold it found included, though the Stop request it found is gone by then.
#[test]
fn a_set_aside_cut_short_at_any_step_finishes_as_one_that_ran_through() {
    let steps = [
        Step::Keep,
        Step::Mark,
        Step::Plan,
        Step::Decision,
        Step::Revision,
        Step::Stop,
        Step::Progress,
        Step::Done,
    ];
    for stopped in [false, true] {
        let start = |root: &Path| {
            std::fs::write(root.join(PLAN), CORRUPT).unwrap();
            write_json(
                &root.join(DECISION),
                &json!({ "profile": PROFILE, "state": "applied", "accounts_verified": true }),
            )
            .unwrap();
            std::fs::write(root.join(PROGRESS), b"not json").unwrap();
            if stopped {
                request_stop_at(root, 3).unwrap();
            }
        };
        let through = aterm_tempfile::tempdir().unwrap();
        start(through.path());
        set_aside_at(through.path(), || true, &mut |_| Ok(()))
            .unwrap()
            .unwrap();
        let expected = outcome(through.path());
        for cut in steps {
            let root = aterm_tempfile::tempdir().unwrap();
            let root = root.path();
            start(root);
            let crashed = set_aside_at(root, || true, &mut |at| {
                if at == cut {
                    Err("the process ended here".into())
                } else {
                    Ok(())
                }
            });
            assert!(crashed.is_err(), "{cut:?}");
            assert!(
                !shown(root),
                "no note before the set-aside is done: {cut:?}"
            );
            let reserved = marker_at(root).unwrap().and_then(|marker| marker.revision);
            let finished = set_aside_at(root, || true, &mut |_| Ok(())).unwrap();
            let finished = finished.expect("the interrupted recovery finishes");
            if let Some(reserved) = reserved {
                assert_eq!(finished.stop_target, reserved, "{cut:?}");
            }
            assert_eq!(
                progress_file(root).unwrap().unwrap().revision,
                finished.stop_target,
                "the returned Stop target is already published: {cut:?}"
            );
            assert_eq!(
                outcome(root),
                expected,
                "cut before {cut:?}, stop {stopped}"
            );
            assert_eq!(set_aside_at(root, || true, &mut |_| Ok(())).unwrap(), None);
        }
    }
}
