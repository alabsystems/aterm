// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

#[test]
fn legacy_and_retired_words_round_trip_with_history_and_fail_closed_on_bad_scope() {
    let row = Row {
        label: "work".into(),
        ..Default::default()
    };
    let mut state = State::default();
    note_auth(&mut state, "work", Auth::SignedOut, NOW);
    let original = state.to_json();
    assert!(!original.contains("auth_retired"));
    let legacy = State::from_json(&original, NOW).unwrap();
    assert_eq!(legacy.account("work").current_auth(&row), Auth::SignedOut);
    state.accounts.get_mut("work").unwrap().auth_scope.retire();
    let held = State::from_json(&state.to_json(), NOW).unwrap();
    assert_eq!(held.account("work").auth, Auth::SignedOut);
    assert_eq!(held.account("work").auth_at, Some(NOW));
    assert_eq!(held.account("work").current_auth(&row), Auth::Unseen);
    assert!(!held.account("work").observed());
    for bad in [
        Value::Null,
        Value::from("false"),
        Value::from(0),
        Value::Array(Vec::new()),
    ] {
        let mut doc: Value = aterm_json::from_str(&original).unwrap();
        doc.as_object_mut()
            .unwrap()
            .get_mut("accounts")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("work")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("auth_retired".into(), bad);
        let read = State::from_json(&doc.to_string(), NOW).unwrap();
        assert_eq!(read.account("work").auth, Auth::SignedOut);
        assert_eq!(read.account("work").auth_at, Some(NOW));
        assert_eq!(read.account("work").current_auth(&row), Auth::Unseen);
    }
}

fn failed_store(root: &Path) -> (Store, String) {
    let store = Store::at_for(root, Cli::Claude, root);
    let old = root.join("old").to_str().unwrap().to_owned();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            Ok(note_renewed(state, logins, "work", None, &old, None, NOW))
        })
        .unwrap();
    store
        .transact_logins(NOW + 10, &mut None, |state, logins| {
            Ok(note_auth_in_context(
                state,
                logins,
                "work",
                Auth::SignedOut,
                NOW + 10,
            ))
        })
        .unwrap();
    (store, old)
}

fn current_row(store: &Store) -> Row {
    let raw = Roster {
        rows: vec![Row {
            label: "work".into(),
            ..Default::default()
        }],
    };
    let logins = store.load_logins(NOW + 10).unwrap().0;
    bound(&raw, &logins).row("work").unwrap().clone()
}

#[test]
fn state_only_same_word_reactivation_wakes_once_without_an_auth_ledger_duplicate() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (store, _) = failed_store(root.path());
    store
        .transact_logins(NOW + 10, &mut None, |_, logins| {
            logins.labels.get_mut("work").unwrap().dir =
                Some(root.path().join("new").display().to_string());
            Ok(Vec::new())
        })
        .unwrap();
    let retired = store.load(NOW + 10).unwrap().0.accounts["work"].clone();
    assert_eq!(retired.auth, Auth::SignedOut);
    assert_eq!(retired.auth_at, Some(NOW + 10));
    assert!(matches!(retired.auth_projection, AuthProjection::Legacy));
    assert_eq!(retired.current_auth(&current_row(&store)), Auth::Unseen);
    let ledger = std::fs::read(store.ledger_path()).unwrap();
    let receipt_path = store.dir.join("sync-commit.json");
    let retired_receipt = std::fs::read(&receipt_path).unwrap();
    std::fs::rename(store.wake_path(), root.path().join("retired-wake")).unwrap();
    assert!(
        store
            .transact(NOW + 10, &mut None, |state| {
                Ok(note_auth(state, "work", Auth::SignedOut, NOW + 10))
            })
            .unwrap()
            .is_empty()
    );
    let current = store.load(NOW + 10).unwrap().0.accounts["work"].clone();
    assert_eq!(current.current_auth(&current_row(&store)), Auth::SignedOut);
    assert_eq!(current.auth, retired.auth);
    assert_eq!(current.auth_at, retired.auth_at);
    assert_eq!(std::fs::read(store.ledger_path()).unwrap(), ledger);
    let current_receipt = std::fs::read(&receipt_path).unwrap();
    assert_ne!(current_receipt, retired_receipt);
    assert!(
        store.wake_path().is_file(),
        "reactivation creates the previously absent wake marker"
    );
    std::fs::rename(store.wake_path(), root.path().join("reactivated-wake")).unwrap();
    let state = std::fs::read(store.state_file()).unwrap();
    let logins = std::fs::read(store.logins_file()).unwrap();
    let witness = std::fs::read(store.dir.join("sync-commit.seen")).unwrap();
    assert!(
        store
            .transact(NOW + 10, &mut None, |state| {
                Ok(note_auth(state, "work", Auth::SignedOut, NOW + 10))
            })
            .unwrap()
            .is_empty()
    );
    assert!(
        !store.wake_path().exists(),
        "an identical current observation publishes no wake"
    );
    assert_eq!(std::fs::read(store.state_file()).unwrap(), state);
    assert_eq!(std::fs::read(store.logins_file()).unwrap(), logins);
    assert_eq!(std::fs::read(store.ledger_path()).unwrap(), ledger);
    assert_eq!(std::fs::read(receipt_path).unwrap(), current_receipt);
    assert_eq!(
        std::fs::read(store.dir.join("sync-commit.seen")).unwrap(),
        witness
    );
}

#[test]
fn an_observation_before_replacement_cannot_certify_the_later_login() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (store, _) = failed_store(root.path());
    let files = [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let before = files.each_ref().map(|path| std::fs::read(path).ok());
    let result = store.transact_logins(NOW + 10, &mut None, |state, logins| {
        let events = note_auth_in_context(state, logins, "work", Auth::SignedOut, NOW + 10);
        assert!(events.is_empty());
        logins.labels.get_mut("work").unwrap().dir =
            Some(root.path().join("new").display().to_string());
        Ok(events)
    });
    assert!(result.is_err());
    assert_eq!(
        files.each_ref().map(|path| std::fs::read(path).ok()),
        before
    );
    let state = store.load(NOW + 10).unwrap().0;
    assert_eq!(state.account("work").auth, Auth::SignedOut);
    assert_eq!(state.account("work").auth_at, Some(NOW + 10));
    assert_eq!(
        state.account("work").current_auth(&current_row(&store)),
        Auth::SignedOut
    );
}

#[test]
fn retired_auth_does_not_reuse_the_previous_logins_served_or_due_status() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (store, _) = failed_store(root.path());
    store
        .transact_logins(NOW + 10, &mut None, |_, logins| {
            let binding = logins.labels.get_mut("work").unwrap();
            binding.served_at = Some(NOW);
            binding.notice_end = Some(NOW + 1);
            binding.dir = Some(root.path().join("new").display().to_string());
            Ok(Vec::new())
        })
        .unwrap();
    let state = store.load(NOW + 10).unwrap().0;
    let logins = store.load_logins(NOW + 10).unwrap().0;
    let row = Row {
        label: "work".into(),
        ..Default::default()
    };
    assert_eq!(
        state.account("work").current_auth(&current_row(&store)),
        Auth::Unseen
    );
    assert_eq!(
        login_state(&row, &logins, &state, NOW + 10),
        LoginState::Idle
    );
    let binding = logins.get("work").unwrap();
    assert_eq!(binding.served_at, Some(NOW));
    assert_eq!(binding.renewed_at, Some(NOW));
    assert_eq!(binding.notice_end, Some(NOW + 1));
}

#[test]
fn a_new_context_accepts_a_fresh_result_behind_the_old_contexts_clock() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (store, _) = failed_store(root.path());
    store
        .transact_logins(NOW + 10, &mut None, |state, logins| {
            logins.labels.get_mut("work").unwrap().dir =
                Some(root.path().join("new").display().to_string());
            Ok(note_auth_in_context(
                state,
                logins,
                "work",
                Auth::SignedOut,
                NOW,
            ))
        })
        .unwrap();
    let state = store.load(NOW + 10).unwrap().0;
    assert_eq!(state.account("work").auth_at, Some(NOW));
    assert_eq!(
        state.account("work").current_auth(&current_row(&store)),
        Auth::SignedOut
    );
}

#[test]
fn a_verified_replacement_at_the_same_path_and_clock_has_current_success() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (store, old) = failed_store(root.path());
    store
        .transact_logins(NOW + 10, &mut None, |state, logins| {
            let events = note_renewed(state, logins, "work", Some(&old), &old, None, NOW);
            note_identity(logins, "work", Proof::Status, Some("owner@example.test"));
            Ok(events)
        })
        .unwrap();
    let state = store.load(NOW + 10).unwrap().0;
    assert_eq!(state.account("work").auth_at, Some(NOW));
    assert_eq!(
        state.account("work").current_auth(&current_row(&store)),
        Auth::Ok
    );
}

#[test]
fn checked_sign_in_after_rebinding_uses_the_new_context_and_proof() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (store, _) = failed_store(root.path());
    store
        .transact_logins(NOW + 10, &mut None, |state, logins| {
            logins.labels.get_mut("work").unwrap().dir =
                Some(root.path().join("new").display().to_string());
            let events = note_checked_sign_in(state, logins, "work", NOW);
            note_identity(logins, "work", Proof::Status, Some("owner@example.test"));
            Ok(events)
        })
        .unwrap();
    let state = store.load(NOW + 10).unwrap().0;
    assert_eq!(state.account("work").auth_at, Some(NOW));
    assert_eq!(
        state.account("work").current_auth(&current_row(&store)),
        Auth::Ok
    );
}
