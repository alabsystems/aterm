// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn selected() -> Plan {
    let mut selection = Selection::none();
    selection.set(Setting::Secrets, true);
    Plan::from(selection)
}

fn admitted(root: &Path) -> Plan {
    let plan = selected();
    authorize_at(root, &plan).unwrap();
    plan
}

fn unchanged(root: &Path, operation: impl FnOnce()) {
    let before = observation_at(root).unwrap();
    operation();
    assert_eq!(observation_at(root).unwrap(), before);
}

#[test]
fn an_automatic_default_with_no_records_has_read_only_authority() {
    let root = aterm_tempfile::tempdir().unwrap();
    let before = std::fs::read_dir(root.path()).unwrap().count();
    unchanged(root.path(), || {
        let authority = BackgroundVaultAuthorization::capture_at(root.path()).unwrap();
        authority.check_at().unwrap();
    });
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), before);
}

#[test]
fn stop_restore_and_off_withdraw_the_original_background_proof() {
    for change in ["stop", "completed-stop", "restore", "off"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = admitted(root.path());
        if change == "completed-stop" {
            let mut progress = progress(root.path()).unwrap().unwrap();
            progress.done = Stage::Secrets.bit();
            progress.completed = true;
            save_progress(root.path(), progress).unwrap();
        }
        let authority = BackgroundVaultAuthorization::capture_at(root.path()).unwrap();
        authority.check_at().unwrap();
        match change {
            "stop" | "completed-stop" => {
                request_stop_at(root.path(), revision_at(root.path()).unwrap()).unwrap()
            }
            "restore" => save_restore(root.path()).unwrap(),
            "off" => {
                plan.selection.set(Setting::Secrets, false);
                authorize_at(root.path(), &plan).unwrap();
            }
            _ => unreachable!(),
        }
        unchanged(root.path(), || {
            assert!(authority.check_at().is_err(), "{change}");
            assert!(
                BackgroundVaultAuthorization::capture_at(root.path()).is_err(),
                "{change}"
            );
        });
    }
}

#[test]
fn a_new_allowed_revision_cannot_authorize_the_previous_proof() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = admitted(root.path());
    let authority = BackgroundVaultAuthorization::capture_at(root.path()).unwrap();
    let previous = revision_at(root.path()).unwrap();
    authorize_at(root.path(), &plan).unwrap();
    assert_ne!(revision_at(root.path()).unwrap(), previous);
    unchanged(root.path(), || {
        assert!(authority.check_at().is_err());
        BackgroundVaultAuthorization::capture_at(root.path())
            .unwrap()
            .check_at()
            .unwrap();
    });
}

#[test]
fn source_changes_and_unreadable_records_never_lend_old_authority() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut plan = admitted(root.path());
    let authority = BackgroundVaultAuthorization::capture_at(root.path()).unwrap();
    plan.vault_url = "https://chosen.example/v1/personal".into();
    authorize_at(root.path(), &plan).unwrap();
    unchanged(root.path(), || {
        assert!(authority.check_at().is_err());
        assert!(BackgroundVaultAuthorization::capture_at(root.path()).is_err());
    });
    std::fs::write(root.path().join("automatic-plan.json"), b"invalid").unwrap();
    let before = std::fs::read(root.path().join("automatic-plan.json")).unwrap();
    assert!(authority.check_at().is_err());
    assert!(BackgroundVaultAuthorization::capture_at(root.path()).is_err());
    assert_eq!(
        std::fs::read(root.path().join("automatic-plan.json")).unwrap(),
        before
    );
}

#[test]
fn an_old_stage_or_identical_republication_cannot_lend_current_authority() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = admitted(root.path());
    let authority = BackgroundVaultAuthorization::capture_at(root.path()).unwrap();
    save_plan(root.path(), &plan).unwrap();
    assert!(
        authority.check_at().is_err(),
        "same bytes have another publication"
    );
    let mut changed = plan.clone();
    changed.selection.set(Setting::Repositories, true);
    save_plan(root.path(), &changed).unwrap();
    unchanged(root.path(), || {
        assert!(
            BackgroundVaultAuthorization::capture_at(root.path()).is_err(),
            "progress belongs to the previous plan"
        );
    });
    let mut progress = Progress::for_plan(&changed);
    progress.admitted = false;
    save_progress(root.path(), progress).unwrap();
    unchanged(root.path(), || {
        assert!(
            BackgroundVaultAuthorization::capture_at(root.path()).is_err(),
            "staging is not admission"
        );
    });
}

#[test]
fn checking_never_takes_the_profile_lock() {
    let root = aterm_tempfile::tempdir().unwrap();
    admitted(root.path());
    let authority = BackgroundVaultAuthorization::capture_at(root.path()).unwrap();
    let _held = HeldLock::take(root.path()).unwrap();
    unchanged(root.path(), || authority.check_at().unwrap());
}
