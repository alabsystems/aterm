// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_792_660_000;

fn login(home: &Path, revision: u32) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "cli_auth_credentials_store='file'\n",
    )
    .unwrap();
    let token = super::super::codex_app_login::tests::make_jwt(&aterm_json::json!({
        "email": "b@example.test", "iat": revision,
        "https://api.openai.com/auth": {"chatgpt_user_id": "b", "chatgpt_account_id": "workspace"}
    }));
    std::fs::write(
        home.join("auth.json"),
        aterm_json::to_vec(&aterm_json::json!({
            "auth_mode": "chatgpt", "tokens": {"id_token": token, "access_token": token}
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn auth_projection_is_read_only_and_raw_load_cannot_supply_current_authority() {
    let scratch = aterm_tempfile::TempDir::new().unwrap();
    let home = scratch.path().join("home");
    let store = Store::at_for(&scratch.path().join("state"), Cli::Codex, &home);
    login(&home.join(".codex"), 1);
    let row = Row {
        label: "b".into(),
        email: Some("b@example.test".into()),
        ..Row::default()
    };
    let context = store.capture_auth_context(&row).unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            stage_auth(state, &row, logins, context, Auth::SignedOut, NOW);
            Ok(Vec::new())
        })
        .unwrap();
    let bytes = std::fs::read(store.state_file()).unwrap();
    let (mut state, _) = store.load(NOW).unwrap();
    assert_eq!(state.account("b").auth, Auth::SignedOut);
    assert_eq!(state.account("b").current_auth(&row), Auth::Unseen);
    let raw = state.clone();
    let encoded = state.to_json();
    let roster = Roster {
        rows: vec![row.clone()],
    };
    store.hydrate_auth_contexts(&mut state, &roster);
    assert_eq!(state.account("b").current_auth(&row), Auth::SignedOut);
    assert_eq!(state, raw, "runtime hydration is not a state edit");
    assert_eq!(state.to_json(), encoded);
    assert_eq!(std::fs::read(store.state_file()).unwrap(), bytes);
    login(&home.join(".codex"), 2);
    store.hydrate_auth_contexts(&mut state, &roster);
    assert_eq!(state.account("b").current_auth(&row), Auth::Unseen);
    assert_eq!(condition(&row, &state, true, NOW), Condition::Unseen);
    assert_eq!(state.account("b").auth, Auth::SignedOut);
    assert_eq!(state.to_json(), encoded);
}

#[test]
fn explicit_same_time_auth_is_checked_against_the_final_roster_before_pin_effects() {
    for word in [Auth::SignedOut, Auth::Ok] {
        for fresh in [false, true] {
            let scratch = aterm_tempfile::TempDir::new().unwrap();
            let home = scratch.path().join("home");
            let store = Store::at_for(&scratch.path().join("state"), Cli::Codex, &home);
            let old = scratch.path().join("old-login");
            login(&old, 1);
            login(&home.join(".codex"), 2);
            let raw = Row {
                label: "b".into(),
                email: Some("b@example.test".into()),
                dir: Some(old.display().to_string()),
                ..Row::default()
            };
            let final_row = Row {
                dir: None,
                ..raw.clone()
            };
            let old_context = store.capture_auth_context(&raw).unwrap();
            let new_context = store.capture_auth_context(&final_row).unwrap();
            assert_ne!(old_context, new_context);
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    stage_auth(state, &raw, logins, old_context.clone(), word, NOW);
                    Ok(Vec::new())
                })
                .unwrap();
            store
                .transact_logins(NOW, &mut None, |state, _| {
                    state.active = Some("b".into());
                    state.pinned = true;
                    Ok(Vec::new())
                })
                .unwrap();
            let roster_path = scratch.path().join("accounts.toml");
            let before = append_rows_for(Cli::Codex, "", std::slice::from_ref(&raw)).unwrap();
            let after = append_rows_for(Cli::Codex, "", std::slice::from_ref(&final_row)).unwrap();
            std::fs::write(&roster_path, &before).unwrap();
            let state_before = std::fs::read(store.state_file()).unwrap();
            let held = store.acquire().unwrap();
            let result = store.transact_sync_held(
                &held,
                NOW,
                &mut None,
                SyncTarget {
                    path: &roster_path,
                    before: Some(&before),
                    after: Some(&after),
                    mode: Some(0o600),
                },
                |state, logins| {
                    stage_auth(
                        state,
                        &raw,
                        logins,
                        if fresh {
                            new_context.clone()
                        } else {
                            old_context.clone()
                        },
                        word,
                        NOW,
                    );
                    Ok(Vec::new())
                },
                || Ok(()),
            );
            drop(held);
            if fresh {
                result.unwrap();
                let records = store.read_account_records(Some(&roster_path), NOW).unwrap();
                assert_eq!(records.state.account("b").current_auth(&final_row), word);
                assert_eq!(records.state.account("b").auth_at, Some(NOW));
                assert_eq!(records.state.pinned, word != Auth::SignedOut);
            } else {
                assert!(result.is_err());
                assert_eq!(std::fs::read(store.state_file()).unwrap(), state_before);
                assert_eq!(std::fs::read(&roster_path).unwrap(), before.as_bytes());
            }
        }
    }
}

#[test]
fn forgetting_an_account_also_retires_its_staged_auth_observation() {
    let scratch = aterm_tempfile::TempDir::new().unwrap();
    let home = scratch.path().join("home");
    let store = Store::at_for(&scratch.path().join("state"), Cli::Codex, &home);
    login(&home.join(".codex"), 1);
    let row = Row {
        label: "b".into(),
        email: Some("b@example.test".into()),
        ..Row::default()
    };
    let context = store.capture_auth_context(&row).unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            stage_auth(state, &row, logins, context, Auth::SignedOut, NOW);
            forget_account(state, logins, "b");
            Ok(Vec::new())
        })
        .unwrap();
    let state = store.load(NOW).unwrap().0;
    assert!(!state.accounts.contains_key("b"));
    assert!(state.pending_auth.is_empty());
}

#[test]
fn replacement_credentials_do_not_inherit_renewal_or_served_metadata() {
    for due in [false, true] {
        let scratch = aterm_tempfile::TempDir::new().unwrap();
        let home = scratch.path().join("home");
        let store = Store::at_for(&scratch.path().join("state"), Cli::Codex, &home);
        login(&home.join(".codex"), 1);
        let row = Row {
            label: "b".into(),
            email: Some("b@example.test".into()),
            ..Row::default()
        };
        let roster_path = scratch.path().join("accounts.toml");
        std::fs::write(
            &roster_path,
            append_rows_for(Cli::Codex, "", std::slice::from_ref(&row)).unwrap(),
        )
        .unwrap();
        let context = store.capture_auth_context(&row).unwrap();
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                logins.labels.insert("b".into(), Binding::default());
                stage_checked_sign_in(state, &row, logins, context, NOW);
                Ok(Vec::new())
            })
            .unwrap();
        if due {
            store
                .transact_logins(NOW, &mut None, |_, logins| {
                    logins.labels.get_mut("b").unwrap().notice_end = Some(NOW);
                    Ok(Vec::new())
                })
                .unwrap();
        }
        let before = store.read_account_records(Some(&roster_path), NOW).unwrap();
        assert_eq!(
            login_state(&row, &before.logins, &before.state, NOW),
            if due {
                LoginState::Due { by: NOW }
            } else {
                LoginState::Ok
            }
        );
        let binding_before = before.logins.get("b").unwrap().clone();
        login(&home.join(".codex"), 2);
        let replaced = store.read_account_records(Some(&roster_path), NOW).unwrap();
        assert_eq!(
            login_state(&row, &replaced.logins, &replaced.state, NOW),
            LoginState::Idle
        );
        let context = store.capture_auth_context(&row).unwrap();
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                stage_auth(state, &row, logins, context.clone(), Auth::Ok, NOW);
                Ok(Vec::new())
            })
            .unwrap();
        let observed = store.read_account_records(Some(&roster_path), NOW).unwrap();
        assert_eq!(observed.state.account("b").current_auth(&row), Auth::Ok);
        assert_eq!(observed.logins.get("b"), Some(&binding_before));
        assert_eq!(
            login_state(&row, &observed.logins, &observed.state, NOW),
            LoginState::Idle
        );
        assert!(!observed.state.account("b").renewal_metadata);
        assert!(!observed.state.account("b").served_metadata);
        store
            .transact_logins(NOW + 1, &mut None, |state, logins| {
                stage_signed_back(state, &row, logins, context.clone(), NOW + 1);
                Ok(Vec::new())
            })
            .unwrap();
        let served = store
            .read_account_records(Some(&roster_path), NOW + 1)
            .unwrap();
        assert!(served.state.account("b").served_metadata);
        assert!(!served.state.account("b").renewal_metadata);
        assert_eq!(
            served.logins.get("b").unwrap().notice_end,
            binding_before.notice_end
        );
        assert_eq!(
            login_state(&row, &served.logins, &served.state, NOW + 1),
            LoginState::Ok
        );
        store
            .transact_logins(NOW + 2, &mut None, |state, logins| {
                stage_checked_sign_in(state, &row, logins, context, NOW + 2);
                Ok(Vec::new())
            })
            .unwrap();
        let checked = store
            .read_account_records(Some(&roster_path), NOW + 2)
            .unwrap();
        assert!(checked.state.account("b").served_metadata);
        assert!(checked.state.account("b").renewal_metadata);
        assert_eq!(checked.logins.get("b").unwrap().renewed_at, Some(NOW + 2));
        assert_eq!(checked.logins.get("b").unwrap().notice_end, None);
        assert_eq!(
            login_state(&row, &checked.logins, &checked.state, NOW + 2),
            LoginState::Ok
        );
    }
}

#[test]
fn an_unscoped_codex_failure_keeps_history_without_ending_the_current_pin() {
    let scratch = aterm_tempfile::TempDir::new().unwrap();
    let home = scratch.path().join("home");
    let store = Store::at_for(&scratch.path().join("state"), Cli::Codex, &home);
    login(&home.join(".codex"), 2);
    let row = Row {
        label: "b".into(),
        email: Some("b@example.test".into()),
        ..Row::default()
    };
    let context = store.capture_auth_context(&row).unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            logins.labels.insert("b".into(), Binding::default());
            stage_checked_sign_in(state, &row, logins, context.clone(), NOW);
            Ok(Vec::new())
        })
        .unwrap();
    store
        .transact_logins(NOW, &mut None, |state, _| {
            state.active = Some("b".into());
            state.pinned = true;
            Ok(Vec::new())
        })
        .unwrap();
    let events = store
        .transact_logins(NOW + 1, &mut None, |state, _| {
            Ok(note_auth(state, "b", Auth::SignedOut, NOW + 1))
        })
        .unwrap();
    assert!(events.iter().all(|event| event.kind != Kind::Unpin));
    let (mut state, _) = store.load(NOW + 1).unwrap();
    store.hydrate_auth_contexts(
        &mut state,
        &Roster {
            rows: vec![row.clone()],
        },
    );
    assert!(state.pinned);
    assert_eq!(state.account("b").auth, Auth::SignedOut);
    assert_eq!(state.account("b").auth_at, Some(NOW + 1));
    assert_eq!(state.account("b").current_auth(&row), Auth::Unseen);
    let events = store
        .transact_logins(NOW + 1, &mut None, |state, logins| {
            stage_auth(state, &row, logins, context, Auth::SignedOut, NOW + 1);
            Ok(Vec::new())
        })
        .unwrap();
    assert!(events.iter().any(|event| event.kind == Kind::Unpin));
    let (mut state, _) = store.load(NOW + 1).unwrap();
    store.hydrate_auth_contexts(
        &mut state,
        &Roster {
            rows: vec![row.clone()],
        },
    );
    assert!(!state.pinned);
    assert_eq!(state.account("b").current_auth(&row), Auth::SignedOut);
    let roster = Roster { rows: vec![row] };
    let roster_path = scratch.path().join("accounts.toml");
    std::fs::write(
        &roster_path,
        append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        store
            .select_account_for(Cli::Codex, &roster, &roster_path, "b", NOW + 1)
            .unwrap(),
        PinSelection::Renew(_)
    ));
    let state = store.load(NOW + 1).unwrap().0;
    assert!(
        !state.pinned,
        "the writer must not pin a currently rejected login"
    );
    assert!(state.pending_pin.is_some());
}

#[test]
fn unscoped_response_proof_does_not_reuse_the_old_credential_context() {
    let scratch = aterm_tempfile::TempDir::new().unwrap();
    let home = scratch.path().join("home");
    let store = Store::at_for(&scratch.path().join("state"), Cli::Codex, &home);
    login(&home.join(".codex"), 1);
    let row = Row {
        label: "b".into(),
        email: Some("b@example.test".into()),
        ..Row::default()
    };
    let context = store.capture_auth_context(&row).unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            stage_auth(state, &row, logins, context, Auth::SignedOut, NOW);
            state.active = Some("b".into());
            state.proven = false;
            Ok(Vec::new())
        })
        .unwrap();
    store
        .transact_logins(NOW + 1, &mut None, |state, _| {
            Ok(prove(state, "b", NOW + 1).into_iter().collect())
        })
        .unwrap();
    let (mut state, _) = store.load(NOW + 1).unwrap();
    store.hydrate_auth_contexts(
        &mut state,
        &Roster {
            rows: vec![row.clone()],
        },
    );
    assert!(
        state.proven,
        "the response still proves the account's canary"
    );
    let account = state.account("b");
    assert_eq!(account.auth, Auth::Ok);
    assert_eq!(account.auth_at, Some(NOW + 1));
    assert_eq!(account.auth_context, None);
    assert_eq!(account.current_auth(&row), Auth::Unseen);
    assert!(!account.renewal_metadata);
    assert!(!account.served_metadata);
}

#[test]
fn a_failed_preparation_excludes_selection_without_fabricating_auth_or_policy() {
    let roster = Roster {
        rows: vec![
            Row {
                label: "a".into(),
                ..Row::default()
            },
            Row {
                label: "b".into(),
                ..Row::default()
            },
        ],
    };
    let enrolled = BTreeSet::from(["a".to_owned(), "b".to_owned()]);
    let original = State {
        active: Some("a".into()),
        ..State::default()
    };
    assert_eq!(
        launch_row(&roster, &original, &enrolled, NOW)
            .unwrap()
            .label,
        "a"
    );
    let mut selection = original.clone();
    selection.excluded_accounts.insert("a".into());
    assert_eq!(
        selection.execution_verdict("a"),
        Err(WhyNot::PreparationFailed)
    );
    assert_eq!(
        selection.lease_verdict("a", NOW),
        Err(WhyNot::PreparationFailed)
    );
    assert!(!row_available_here(
        roster.row("a").unwrap(),
        &selection,
        NOW
    ));
    assert_eq!(
        pin_refusal(&selection, &roster, &enrolled, "a"),
        Some(PinRefusal::PreparationFailed)
    );
    assert_eq!(
        launch_row(&roster, &selection, &enrolled, NOW)
            .unwrap()
            .label,
        "b"
    );
    assert_eq!(
        selection, original,
        "operation-local exclusion is not persisted state"
    );
    assert_eq!(selection.to_json(), original.to_json());
    assert_eq!(selection.accounts, original.accounts);
    assert_eq!(selection.prepared_accounts, original.prepared_accounts);
    assert_eq!(selection.leases, original.leases);
    assert_eq!(
        State::from_json(&selection.to_json(), NOW)
            .unwrap()
            .excluded_accounts,
        BTreeSet::new()
    );
}
