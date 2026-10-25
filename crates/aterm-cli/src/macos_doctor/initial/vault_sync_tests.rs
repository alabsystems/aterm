// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The subscriber mend's policy is read again where its local effects begin.

use super::*;

#[test]
fn vault_sync_effect_refuses_a_choice_or_stop_after_the_fast_read() {
    for change in ["leave", "stop", "cancel"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = Plan::default();
        let revision = restart_at(root.path(), &plan).unwrap();
        assert!(keeps_vault_sync_at(root.path()).unwrap());
        match change {
            "leave" => {
                let mut without = plan;
                without.selection.set(Setting::Secrets, false);
                restart_at(root.path(), &without).unwrap();
            }
            "stop" => request_stop_at(root.path(), revision).unwrap(),
            "cancel" => super::super::write_legacy_cancel_at(root.path()),
            _ => unreachable!(),
        }
        let before = observation_at(root.path()).unwrap();
        assert_eq!(
            with_vault_sync_at::<()>(root.path(), |_| panic!("a newer {change} must hold repair"))
                .unwrap(),
            None
        );
        assert_eq!(observation_at(root.path()).unwrap(), before);
    }
}

#[test]
fn vault_sync_effect_holds_the_profile_and_releases_it_without_retry() {
    let root = aterm_tempfile::tempdir().unwrap();
    let revision = restart_at(root.path(), &Plan::default()).unwrap();
    let holder = HeldLock::take(root.path()).unwrap();
    assert_eq!(
        with_vault_sync_at::<()>(root.path(), |_| panic!(
            "a busy profile must defer the mend"
        ))
        .unwrap(),
        None
    );
    drop(holder);

    let before = observation_at(root.path()).unwrap();
    let result = with_vault_sync_at(root.path(), |current| {
        assert_eq!(current, revision);
        assert!(
            HeldLock::try_take(root.path()).unwrap().is_none(),
            "another choice cannot pass the actual local repair callback"
        );
        Err::<(), _>("injected local service failure")
    })
    .unwrap();
    assert_eq!(result, Some(Err("injected local service failure")));
    assert_eq!(observation_at(root.path()).unwrap(), before);
    assert!(HeldLock::try_take(root.path()).unwrap().is_some());
}

#[test]
fn vault_sync_effect_refuses_unreadable_or_mismatched_records() {
    for damaged in [
        "automatic-plan.json",
        "initial-setup.json",
        "first-run.json",
        "missing plan",
        "mismatched plan",
        "stop-request.json",
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        restart_at(root.path(), &Plan::default()).unwrap();
        assert!(keeps_vault_sync_at(root.path()).unwrap());
        match damaged {
            "missing plan" => {
                std::fs::remove_file(root.path().join("automatic-plan.json")).unwrap();
            }
            "mismatched plan" => {
                let mut newer = Plan::default();
                newer.selection.set(Setting::Dock, false);
                save_plan(root.path(), &newer).unwrap();
            }
            file => std::fs::write(root.path().join(file), b"incomplete record").unwrap(),
        }
        let result = with_vault_sync_at::<()>(root.path(), |_| {
            panic!("{damaged} cannot authorize subscriber repair")
        });
        if damaged == "stop-request.json" {
            assert!(!keeps_vault_sync_at(root.path()).unwrap());
            assert_eq!(result.unwrap(), None);
        } else {
            assert!(result.is_err(), "{damaged}");
        }
        assert!(HeldLock::try_take(root.path()).unwrap().is_some());
    }
}

#[test]
fn vault_sync_effect_keeps_default_restore_and_completed_stop_semantics() {
    let root = aterm_tempfile::tempdir().unwrap();
    assert_eq!(
        with_vault_sync_at(root.path(), |revision| revision).unwrap(),
        Some(0)
    );

    let revision = restart_at(root.path(), &Plan::default()).unwrap();
    assert_eq!(
        with_vault_sync_at(root.path(), |revision| revision).unwrap(),
        Some(revision)
    );
    save_restore(root.path()).unwrap();
    leave_restored_as_is(root.path()).unwrap();
    let restored = revision_at(root.path()).unwrap();
    assert!(restored > revision);
    assert_eq!(
        with_vault_sync_at(root.path(), |revision| revision).unwrap(),
        Some(restored),
        "Restore preserves the vault group and its admission"
    );

    let revision = restart_at(root.path(), &Plan::default()).unwrap();
    let mut done = progress(root.path()).unwrap().unwrap();
    done.completed = true;
    save_progress(root.path(), done).unwrap();
    request_stop_at(root.path(), revision).unwrap();
    assert_eq!(
        with_vault_sync_at(root.path(), |revision| revision).unwrap(),
        Some(revision),
        "a completed durable Stop remains spent; the caller also checks local Stop"
    );
}
