// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

#[test]
fn a_checked_sign_in_does_not_release_preparation_only_credentials() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(root.path());
    let path = root.path().join("accounts.toml");
    let row = Row {
        label: "account".into(),
        ..Row::default()
    };
    fs::write(
        &path,
        accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap(),
    )
    .unwrap();
    store
        .transact_logins(NOW, &mut None, |state, _| {
            state.prepared_accounts.insert("account".into());
            Ok(accounts::note_auth(
                state,
                "account",
                accounts::Auth::SignedOut,
                NOW - 1,
            ))
        })
        .unwrap();
    let witness = capture_default_login(&store, &path, "account", NOW).unwrap();
    assert_eq!(
        record_default_sign_in(&store, Cli::Claude, &path, &witness, root.path(), NOW),
        Ok(DefaultSignIn::Accepted)
    );
    let state = store.load(NOW).unwrap().0;
    assert_eq!(state.accounts["account"].auth, accounts::Auth::Ok);
    assert_eq!(
        state.lease_verdict("account", NOW),
        Err(accounts::WhyNot::PreparationOnly)
    );
    assert!(
        store
            .load_logins(NOW)
            .unwrap()
            .0
            .sign_in_receipts
            .contains_key("account")
    );
}

#[test]
fn shared_custody_refuses_unscoped_default_sign_in_before_and_after_capture() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let login_home = root.path().join(".claude");
    fs::create_dir(&login_home).unwrap();
    let store = accounts::Store::at(root.path());
    let path = root.path().join("accounts.toml");
    let row = Row {
        label: "account".into(),
        email: Some("owner@example.test".into()),
        ..Row::default()
    };
    fs::write(
        &path,
        accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap(),
    )
    .unwrap();
    fs::write(
        root.path().join(".claude.json"),
        r#"{"oauthAccount":{"emailAddress":"owner@example.test"}}"#,
    )
    .unwrap();
    store
        .transact_logins(NOW, &mut None, |_, logins| {
            accounts::note_served_login(logins, "account", NOW - 100);
            Ok(Vec::new())
        })
        .unwrap();
    let witness = capture_default_login(&store, &path, "account", NOW).unwrap();
    assert_eq!(
        record_default_sign_in(&store, Cli::Claude, &path, &witness, &login_home, NOW),
        Ok(DefaultSignIn::Accepted)
    );
    let witness = capture_default_login(&store, &path, "account", NOW).unwrap();
    store
        .transact_logins(NOW, &mut None, |_, logins| {
            logins.shared.begin(
                accounts::shared::Intent {
                    source: accounts::shared::Source {
                        network: [1; 32],
                        account: [2; 32],
                    },
                    decision: [3; 32],
                    generation: 1,
                    configuration: [4; 32],
                    request: [5; 16],
                    label: "shared-work".into(),
                    identity: "owner@example.test".into(),
                    staging: root.path().join("staging").to_str().unwrap().into(),
                    before_generation: None,
                    begun_at: NOW,
                },
                None,
            )?;
            Ok(Vec::new())
        })
        .unwrap();
    assert!(capture_default_login(&store, &path, "account", NOW).is_none());
    assert!(
        witness
            .parent
            .matches(&store.load_logins(NOW).unwrap().0, "account"),
        "the unchanged binding must pass its ordinary generation fence"
    );
    let before = [store.state_file(), store.logins_file()].map(|p| fs::read(p).ok());
    assert!(
        record_default_sign_in(&store, Cli::Claude, &path, &witness, &login_home, NOW).is_err()
    );
    assert!(
        record_default_sign_back(&store, Cli::Claude, &path, &witness, &login_home, true, NOW)
            .is_err()
    );
    assert_eq!(
        [store.state_file(), store.logins_file()].map(|p| fs::read(p).ok()),
        before
    );
}

fn explicit_signin_case(bound: bool, case: i64) -> [bool; 5] {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(root.path());
    let path = root.path().join("accounts.toml");
    let row = Row {
        label: "account".into(),
        email: (case == 3).then(|| "expected@example.test".into()),
        ..Row::default()
    };
    fs::write(
        &path,
        accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap(),
    )
    .unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            if bound {
                accounts::note_served_login(logins, "account", NOW - 100);
                accounts::note_login_notice(logins, "account", 1, NOW - 100);
            }
            Ok(accounts::note_auth(
                state,
                "account",
                if case == 1 || case == 2 {
                    accounts::Auth::SignedOut
                } else {
                    accounts::Auth::Ok
                },
                if case == 2 { NOW + 1 } else { NOW - 1 },
            ))
        })
        .unwrap();
    let witness = capture_default_login(&store, &path, "account", NOW).unwrap();
    if case == 4 {
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                let mut events =
                    accounts::note_auth(state, "account", accounts::Auth::SignedOut, NOW);
                events.extend(accounts::note_signed_back(state, logins, "account", NOW));
                Ok(events)
            })
            .unwrap();
    }
    let before = store.load(NOW).unwrap().0;
    let before_logins = store.load_logins(NOW).unwrap().0;
    let outcome = record_default_sign_in(&store, Cli::Claude, &path, &witness, root.path(), NOW);
    let after = store.load(NOW).unwrap().0;
    let after_logins = store.load_logins(NOW).unwrap().0;
    [
        matches!(outcome, Ok(DefaultSignIn::Accepted)),
        before_logins.sign_in_receipts != after_logins.sign_in_receipts,
        after_logins
            .get("account")
            .is_some_and(|binding| binding.renewed_at == Some(NOW))
            && before_logins
                .get("account")
                .and_then(|binding| binding.renewed_at)
                != Some(NOW),
        before_logins.get("account").is_none() && after_logins.get("account").is_some(),
        before == after && before_logins.to_json() == after_logins.to_json(),
    ]
}

pub(crate) fn explicit_signin_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Binding" => {
            next.insert("binding", state["binding"] + 1);
        }
        "Case" => {
            next.insert("case", state["case"] + 1);
        }
        "Complete" => {
            next.insert("phase", 1);
            for (field, value) in ["accepted", "fenced", "refreshed", "created", "preserved"]
                .into_iter()
                .zip(explicit_signin_case(state["binding"] == 1, state["case"]))
            {
                next.insert(field, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn explicit_signin_conforms_to_receipt_and_metadata_laws() {
    let model = aterm_models_harness::account_explicit_signin_model();
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 20);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = explicit_signin_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, field, law) in [
        (0, "accepted", "CurrentAccepted"),
        (3, "accepted", "RejectedClosed"),
        (0, "fenced", "CurrentReceipt"),
        (0, "refreshed", "RefreshExisting"),
        (0, "created", "NoBindingInvented"),
        (4, "preserved", "StalePreserved"),
    ] {
        let mut initial = model.init_state();
        initial.insert("binding", 1);
        initial.insert("case", case);
        let mut bad = explicit_signin_transition(&initial, "Complete");
        bad.insert(field, 1 - bad[field]);
        assert!(!model.check_invariant(law, &bad));
    }
}

fn sign_back_case(operation: i64, case: i64) -> [bool; 4] {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(root.path());
    let path = root.path().join("accounts.toml");
    fs::write(
        &path,
        "[[account]]\nlabel = 'account'\nclass = 'dedicated'\n",
    )
    .unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            if operation == 0 {
                accounts::note_renewed(
                    state,
                    logins,
                    "account",
                    None,
                    root.path().join("login").to_str().unwrap(),
                    None,
                    NOW - 20,
                );
            }
            Ok(accounts::note_auth(
                state,
                "account",
                accounts::Auth::SignedOut,
                NOW - 10,
            ))
        })
        .unwrap();
    if case == 17 {
        let mut before = store.load_logins(NOW).unwrap().0;
        before.sign_in_clock = u64::MAX;
        fs::write(store.logins_file(), before.to_json()).unwrap();
    }
    let before = store.load_logins(NOW).unwrap().0;
    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    for (key, label) in [("local", "account"), ("foreign", "another")] {
        super::recovery_policy_tests::hold_retry(&guard, &store, Cli::Claude, key, label);
        let mut held = guard.lock().unwrap();
        let owner = held.activity_owners.get_mut(key).unwrap();
        *owner = owner.clone().observing(&before);
        let owner = held.ladder_owners.get_mut(key).unwrap();
        *owner = owner.clone().observing(&before);
    }
    let captured = LoginClaims::capture(&guard);
    let blocked = store.state_witness_file();
    if matches!(case, 6 | 7) {
        fs::rename(&blocked, blocked.with_extension("original")).unwrap();
        fs::create_dir(&blocked).unwrap();
    }
    let success = |state: &mut accounts::State, logins: &mut accounts::Logins, at| {
        if operation == 2 {
            accounts::note_default_login_made(state, logins, "account", None, at)
        } else {
            accounts::note_signed_back(state, logins, "account", at)
        }
    };
    let result = store.transact_logins(NOW, &mut None, |state, logins| {
        if case == 2 {
            return Ok(accounts::note_auth(
                state,
                "account",
                accounts::Auth::Ok,
                NOW,
            ));
        }
        if case == 12 {
            logins.sign_in_clock = u64::MAX;
            logins.sign_in_receipts.insert("account".into(), u64::MAX);
            return Ok(Vec::new());
        }
        let mut events = success(
            state,
            logins,
            if case == 1 {
                NOW - 5
            } else if case == 15 {
                NOW - 11
            } else {
                NOW
            },
        );
        if case == 13 {
            events.extend(events.clone());
        }
        if case == 19 {
            logins.labels.entry("account".into()).or_default().dir =
                Some(root.path().join("replacement").to_str().unwrap().into());
        }
        Ok(events)
    });
    assert_eq!(result.is_err(), matches!(case, 6 | 7 | 17));
    let receipt_fenced = if result.is_ok() && case != 19 {
        let completed = store.load_logins(NOW).unwrap().0;
        // Default publication already creates a metadata binding. A receipt
        // must add no fence beyond that existing creation rule.
        let creates_default = operation == 2 && !matches!(case, 2 | 12 | 15);
        completed.generation_clock != before.generation_clock + u64::from(creates_default)
            || if creates_default {
                completed
                    .get("account")
                    .map(|binding| binding.renewal_generation)
                    != Some(before.generation_clock + 1)
            } else {
                completed
                    .get("account")
                    .map(|binding| binding.renewal_generation)
                    != before
                        .get("account")
                        .map(|binding| binding.renewal_generation)
            }
    } else {
        false
    };
    if case == 7 {
        fs::rename(&blocked, blocked.with_extension("obstruction")).unwrap();
        fs::rename(blocked.with_extension("original"), &blocked).unwrap();
        store.read_account_records(Some(&path), NOW).unwrap();
        assert!(!store.sync_needs_recovery());
    }
    if case != 6 {
        match case {
            3 => {
                let after = store.load_logins(NOW).unwrap().0;
                let mut held = guard.lock().unwrap();
                let owner = held.activity_owners.get_mut("local").unwrap();
                *owner = owner.clone().observing(&after);
                let owner = held.ladder_owners.get_mut("local").unwrap();
                *owner = owner.clone().observing(&after);
            }
            4 | 5 => {
                store
                    .transact_logins(NOW, &mut None, |_, logins| {
                        if case == 5 {
                            logins.labels.entry("account".into()).or_default().dir =
                                Some(root.path().join("replacement").to_str().unwrap().into());
                        } else {
                            accounts::note_served_login(logins, "account", NOW);
                        }
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            9..=11 => {
                let mut held = guard.lock().unwrap();
                let revise = |owner: &mut LoginOwner| match case {
                    9 => owner.cli = Cli::Codex,
                    10 => owner.token = std::sync::Arc::new(()),
                    11 => owner.success_generation = None,
                    _ => unreachable!(),
                };
                revise(held.activity_owners.get_mut("local").unwrap());
                revise(held.ladder_owners.get_mut("local").unwrap());
            }
            14 => {
                store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        if operation != 2 {
                            accounts::note_auth(state, "account", accounts::Auth::SignedOut, NOW);
                        }
                        Ok(success(state, logins, NOW))
                    })
                    .unwrap();
            }
            16 => {
                for (from, to) in [("account", "away"), ("away", "account")] {
                    store
                        .transact_logins(NOW, &mut None, |state, logins| {
                            accounts::move_account(state, logins, from, to)?;
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
            }
            18 => {
                let mut after = store.load_logins(NOW).unwrap().0;
                after.sign_in_receipts.clear();
                after.sign_in_clock = 0;
                fs::write(store.logins_file(), after.to_json()).unwrap();
            }
            20..=22 => {
                if case == 20 {
                    store
                        .transact_logins(NOW, &mut None, |state, logins| {
                            Ok(accounts::note_renewed(
                                state,
                                logins,
                                "account",
                                None,
                                root.path().join("new-login").to_str().unwrap(),
                                None,
                                NOW,
                            ))
                        })
                        .unwrap();
                }
                store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        if case == 21 {
                            accounts::move_account(state, logins, "account", "away")?;
                        } else {
                            accounts::forget_account(state, logins, "account");
                        }
                        accounts::note_auth(state, "account", accounts::Auth::SignedOut, NOW);
                        Ok(if case == 22 {
                            success(state, logins, NOW)
                        } else {
                            Vec::new()
                        })
                    })
                    .unwrap();
            }
            _ => {}
        }
    }
    let after = store.load_logins(NOW);
    let fenced = if let Ok((logins, _)) = &after {
        if case == 13 {
            assert_eq!(logins.sign_in_clock, before.sign_in_clock + 1);
        } else if case == 14 {
            assert_eq!(logins.sign_in_clock, before.sign_in_clock + 2);
        }
        receipt_fenced
    } else {
        assert_eq!(case, 6);
        false
    };
    if let Ok((logins, _)) = after {
        let claims = if case == 10 {
            captured
        } else {
            LoginClaims::capture(&guard)
        };
        note_observed_publications(&guard, claims.clone(), &store, Cli::Claude, &logins);
        if case == 8 {
            note_observed_publications(&guard, claims, &store, Cli::Claude, &logins);
        }
    } else {
        observe_committed_logins(&store, Cli::Claude, &guard, NOW);
        assert!(store.sync_needs_recovery());
    }
    let held = guard.lock().unwrap();
    let activity = |key: &str| {
        let remains = held.activity_owners.contains_key(key);
        assert_eq!(held.last_attempt.contains_key(key), remains);
        assert_eq!(held.parked.contains_key(key), remains);
        assert_eq!(held.bot_checked.contains_key(key), remains);
        remains
    };
    let ladder = |key: &str| {
        let remains = held.ladder_owners.contains_key(key);
        assert_eq!(held.ladder.has_backoff(key), remains);
        remains
    };
    [
        activity("local"),
        ladder("local"),
        activity("foreign") && ladder("foreign"),
        fenced,
    ]
}

pub(crate) fn signback_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Operation" => {
            next.insert("operation", state["operation"] + 1);
        }
        "Case" => {
            next.insert("case", state["case"] + 1);
        }
        "Observe" => {
            let [activity, ladder, foreign, fenced] =
                sign_back_case(state["operation"], state["case"]);
            next.insert("phase", 1);
            for (field, value) in [
                ("activity", activity),
                ("ladder", ladder),
                ("foreign", foreign),
                ("fenced", fenced),
            ] {
                next.insert(field, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn in_place_sign_back_receipts_conform_without_credential_replacement() {
    let model = aterm_models_harness::account_signback_receipt_model();
    assert_eq!(model.mutant_dials(), (1..=20).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 138);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = signback_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let correct = signback_transition(&model.init_state(), "Observe");
    for (field, law) in [
        ("activity", "ActivityTracksReceipt"),
        ("ladder", "LadderTracksReceipt"),
        ("foreign", "ForeignSurvives"),
        ("fenced", "NoAdditionalCredentialFence"),
    ] {
        let mut bad = correct.clone();
        bad.insert(field, 1 - correct[field]);
        assert!(!model.check_invariant(law, &bad));
    }
}

#[test]
fn sign_back_receipts_reject_corruption_and_retain_their_clock_after_removal() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(root.path());
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            accounts::note_auth(state, "account", accounts::Auth::SignedOut, NOW);
            Ok(accounts::note_signed_back(state, logins, "account", NOW))
        })
        .unwrap();
    let original = store.load_logins(NOW).unwrap().0;
    assert_eq!(original.sign_in_clock, 1);
    assert_eq!(original.sign_in_receipts.get("account"), Some(&1));
    assert!(original.get("account").is_none());
    let document: Value = aterm_json::from_str(&original.to_json()).unwrap();
    for (clock, receipt, admitted) in [
        (Value::from(1_u64), Value::from(1_u64), true),
        (Value::from(1_u64), Value::from(0_u64), false),
        (Value::from(1_u64), Value::from(2_u64), false),
        (Value::from(1_u64), Value::from(-1_i64), false),
        (Value::from(1_u64), Value::Null, false),
        (Value::from("1"), Value::from(1_u64), false),
    ] {
        let mut changed = document.clone();
        let object = changed.as_object_mut().unwrap();
        object.insert("sign_in_clock".into(), clock);
        object
            .get_mut("sign_in_receipts")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("account".into(), receipt);
        assert_eq!(
            accounts::Logins::from_json(&changed.to_string(), NOW).is_ok(),
            admitted
        );
    }
    store
        .transact_logins(NOW, &mut None, |state, _| {
            state.accounts.remove("account");
            Ok(Vec::new())
        })
        .unwrap();
    let removed = store.load_logins(NOW).unwrap().0;
    assert_eq!(removed.sign_in_clock, 1);
    assert!(removed.sign_in_receipts.is_empty());
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            accounts::note_auth(state, "account", accounts::Auth::SignedOut, NOW);
            Ok(accounts::note_signed_back(state, logins, "account", NOW))
        })
        .unwrap();
    assert_eq!(
        store
            .load_logins(NOW)
            .unwrap()
            .0
            .sign_in_receipts
            .get("account"),
        Some(&2)
    );
}

#[test]
fn sign_back_clears_persisted_bot_hold_only_after_a_committed_success() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(root.path());
    store
        .transact_logins(NOW, &mut None, |state, _| {
            let mut events = accounts::note_auth(state, "account", accounts::Auth::SignedOut, NOW);
            events.push(
                accounts::Event::new(NOW, accounts::Kind::SignIn, "account")
                    .because("provider browser bot-check"),
            );
            Ok(events)
        })
        .unwrap();
    assert_eq!(bot_check_on_record_at(&store, "account"), Some(NOW));
    fs::rename(store.ledger_path(), root.path().join("retained-ledger")).unwrap();
    fs::create_dir(store.ledger_path()).unwrap();
    assert!(
        store
            .transact_logins(NOW + 1, &mut None, |state, logins| {
                Ok(accounts::note_signed_back(
                    state,
                    logins,
                    "account",
                    NOW + 1,
                ))
            })
            .is_err()
    );
    assert!(
        store
            .load_logins(NOW + 1)
            .unwrap()
            .0
            .sign_in_receipts
            .is_empty()
    );
    fs::rename(store.ledger_path(), root.path().join("obstruction")).unwrap();
    fs::rename(root.path().join("retained-ledger"), store.ledger_path()).unwrap();
    assert_eq!(bot_check_on_record_at(&store, "account"), Some(NOW));
    store
        .transact_logins(NOW + 1, &mut None, |state, logins| {
            Ok(accounts::note_signed_back(
                state,
                logins,
                "account",
                NOW + 1,
            ))
        })
        .unwrap();
    assert_eq!(bot_check_on_record_at(&store, "account"), None);
    assert_eq!(
        store
            .load_logins(NOW + 1)
            .unwrap()
            .0
            .sign_in_receipts
            .get("account"),
        Some(&1)
    );
}

#[test]
fn default_heal_reports_only_a_committed_receipt() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(root.path());
    let roster_path = root.path().join("accounts.toml");
    fs::write(
        &roster_path,
        "[[account]]\nlabel = 'account'\nclass = 'dedicated'\nemail = 'owner@example.test'\n",
    )
    .unwrap();
    fs::write(
        root.path().join(".claude.json"),
        r#"{"oauthAccount":{"emailAddress":"owner@example.test"}}"#,
    )
    .unwrap();
    store
        .transact_logins(NOW, &mut None, |state, _| {
            Ok(accounts::note_auth(
                state,
                "account",
                accounts::Auth::SignedOut,
                NOW,
            ))
        })
        .unwrap();
    let mut logins = store.load_logins(NOW).unwrap().0;
    logins.sign_in_clock = u64::MAX;
    fs::write(store.logins_file(), logins.to_json()).unwrap();
    let heal = DefaultHeal {
        label: "account".into(),
        expected_identity: "owner@example.test".into(),
        signed_out_at: NOW,
        was: None,
    };
    assert!(!apply_default_heal(
        &store,
        root.path(),
        &roster_path,
        &heal,
        &super::super::cli::LoginCheck::SignedIn,
        NOW + 1
    ));
    assert_eq!(
        store.load(NOW + 1).unwrap().0.account("account").auth,
        accounts::Auth::SignedOut
    );
    assert!(
        store
            .load_logins(NOW + 1)
            .unwrap()
            .0
            .sign_in_receipts
            .is_empty()
    );
}

#[test]
fn default_identity_is_rechecked_with_the_current_roster_before_sign_back() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let home = root.path().join(".codex");
    fs::create_dir(&home).unwrap();
    let store = accounts::Store::at_for(root.path(), Cli::Codex, root.path());
    let path = root.path().join("accounts-codex.toml");
    let row = Row {
        label: "account".into(),
        email: Some("alias@example.test".into()),
        identity_email: Some("owner@example.test".into()),
        ..Row::default()
    };
    fs::write(
        &path,
        accounts::append_rows_for(Cli::Codex, "", &[row]).unwrap(),
    )
    .unwrap();
    store
        .transact_logins(NOW, &mut None, |state, _| {
            Ok(accounts::note_auth(
                state,
                "account",
                accounts::Auth::SignedOut,
                NOW,
            ))
        })
        .unwrap();
    let witness = capture_default_login(&store, &path, "account", NOW).unwrap();
    let write_identity = |email| {
        let token =
            super::super::codex_app_login::tests::make_chatgpt_jwt(Some(email), "plus", NOW + 3600);
        fs::write(
            home.join("auth.json"),
            aterm_json::json!({"tokens":{"access_token":token}}).to_string(),
        )
        .unwrap();
    };
    write_identity("other@example.test");
    assert!(
        record_default_sign_back(&store, Cli::Codex, &path, &witness, &home, false, NOW + 1)
            .is_err()
    );
    assert!(
        store
            .load_logins(NOW + 1)
            .unwrap()
            .0
            .sign_in_receipts
            .is_empty()
    );
    write_identity("owner@example.test");
    // A captured label cannot authorize a row whose identity changed meanwhile.
    fs::write(
        &path,
        "[[account]]\nlabel = 'account'\nclass = 'dedicated'\nemail = 'replacement@example.test'\n",
    )
    .unwrap();
    write_identity("replacement@example.test");
    assert!(
        record_default_sign_back(&store, Cli::Codex, &path, &witness, &home, false, NOW + 1)
            .is_err()
    );
    fs::write(
        &path,
        "[[account]]\nlabel = 'account'\nclass = 'dedicated'\n",
    )
    .unwrap();
    assert!(
        record_default_sign_back(&store, Cli::Codex, &path, &witness, &home, false, NOW + 1)
            .is_err()
    );
    write_identity("owner@example.test");
    fs::write(&path, "[[account]]\nlabel = 'account'\nclass = 'dedicated'\nemail = 'alias@example.test'\nidentity_email = 'owner@example.test'\n").unwrap();
    assert!(
        record_default_sign_back(&store, Cli::Codex, &path, &witness, &home, false, NOW + 1)
            .is_ok()
    );
    assert_eq!(
        store
            .load_logins(NOW + 1)
            .unwrap()
            .0
            .sign_in_receipts
            .get("account"),
        Some(&1)
    );
}

#[test]
fn a_checked_turn_and_a_sign_in_preserve_distinct_metadata_semantics() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        for known_ok in [false, true] {
            for served in [false, true] {
                let root = aterm_tempfile::TempDir::new().unwrap();
                let store = accounts::Store::at_for(root.path(), cli, root.path());
                let path = root.path().join(cli.accounts().roster_file);
                let row = Row {
                    label: "account".into(),
                    email: Some("owner@example.test".into()),
                    vendor: accounts::Vendor {
                        region: (cli == Cli::Kimi).then_some(Region::Global),
                        ..accounts::Vendor::default()
                    },
                    ..Row::default()
                };
                fs::write(&path, accounts::append_rows_for(cli, "", &[row]).unwrap()).unwrap();
                if known_ok {
                    store
                        .transact_logins(NOW, &mut None, |state, _| {
                            Ok(accounts::note_auth(
                                state,
                                "account",
                                accounts::Auth::Ok,
                                NOW,
                            ))
                        })
                        .unwrap();
                }
                let witness = capture_default_login(&store, &path, "account", NOW).unwrap();
                record_default_sign_back(
                    &store,
                    cli,
                    &path,
                    &witness,
                    root.path(),
                    served,
                    NOW + 1,
                )
                .unwrap();
                let logins = store.load_logins(NOW + 1).unwrap().0;
                assert_eq!(logins.get("account").is_some(), served);
                assert!(logins.sign_in_receipts.is_empty());
                if served {
                    assert_eq!(logins.get("account").unwrap().served_at, Some(NOW + 1));
                }
            }
        }
    }
}

#[test]
fn explicit_default_sign_in_refreshes_bindings_after_clock_normalization() {
    for auth in [accounts::Auth::Ok, accounts::Auth::SignedOut] {
        for bound in [false, true] {
            for future in [false, true] {
                let root = aterm_tempfile::TempDir::new().unwrap();
                let store = accounts::Store::at(root.path());
                let path = root.path().join("accounts.toml");
                fs::write(
                    &path,
                    "[[account]]\nlabel = 'account'\nclass = 'dedicated'\n",
                )
                .unwrap();
                store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        if bound {
                            accounts::note_served_login(logins, "account", NOW - 100);
                            accounts::note_login_notice(logins, "account", 1, NOW - 100);
                        }
                        Ok(accounts::note_auth(
                            state,
                            "account",
                            auth,
                            if future { NOW + 1 } else { NOW - 1 },
                        ))
                    })
                    .unwrap();
                let witness = capture_default_login(&store, &path, "account", NOW).unwrap();
                assert_eq!(
                    store.load(NOW).unwrap().0.accounts["account"].auth_at,
                    Some(if future { NOW } else { NOW - 1 })
                );
                let outcome =
                    record_default_sign_in(&store, Cli::Claude, &path, &witness, root.path(), NOW);
                assert_eq!(outcome.unwrap(), DefaultSignIn::Accepted);
                let state = store.load(NOW).unwrap().0;
                let logins = store.load_logins(NOW).unwrap().0;
                assert_eq!(state.accounts["account"].auth, accounts::Auth::Ok);
                assert_eq!(logins.sign_in_receipts.get("account"), Some(&1));
                assert_eq!(logins.get("account").is_some(), bound);
                if let Some(binding) = logins.get("account") {
                    assert_eq!(binding.renewed_at, Some(NOW));
                    assert_eq!(binding.notice_end, None);
                    assert_eq!(binding.served_at, Some(NOW));
                }
                assert!(
                    record_default_sign_in(&store, Cli::Claude, &path, &witness, root.path(), NOW)
                        .is_err()
                );
            }
        }
    }
}

#[test]
fn an_in_memory_sign_in_cannot_replace_a_future_authentication_observation() {
    let mut state = accounts::State::default();
    let mut logins = accounts::Logins::default();
    accounts::note_auth(&mut state, "account", accounts::Auth::SignedOut, NOW + 1);
    let before = state.clone();
    assert!(accounts::note_checked_sign_in(&mut state, &mut logins, "account", NOW).is_empty());
    assert_eq!(state, before);
    assert!(logins.sign_in_receipts.is_empty());
    assert!(logins.get("account").is_none());
}

#[test]
fn default_heal_rechecks_identity_after_the_status_check() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(root.path());
    let path = root.path().join("accounts.toml");
    fs::write(
        &path,
        "[[account]]\nlabel = 'account'\nclass = 'dedicated'\nemail = 'owner@example.test'\n",
    )
    .unwrap();
    store
        .transact_logins(NOW, &mut None, |state, _| {
            Ok(accounts::note_auth(
                state,
                "account",
                accounts::Auth::SignedOut,
                NOW,
            ))
        })
        .unwrap();
    let heal = DefaultHeal {
        label: "account".into(),
        expected_identity: "owner@example.test".into(),
        signed_out_at: NOW,
        was: None,
    };
    let profile = root.path().join(".claude.json");
    fs::write(
        &profile,
        r#"{"oauthAccount":{"emailAddress":"other@example.test"}}"#,
    )
    .unwrap();
    assert!(!apply_default_heal(
        &store,
        root.path(),
        &path,
        &heal,
        &super::super::cli::LoginCheck::SignedIn,
        NOW + 1
    ));
    assert!(
        store
            .load_logins(NOW + 1)
            .unwrap()
            .0
            .sign_in_receipts
            .is_empty()
    );
    fs::write(
        &path,
        "[[account]]\nlabel = 'account'\nclass = 'dedicated'\nemail = 'other@example.test'\n",
    )
    .unwrap();
    assert!(!apply_default_heal(
        &store,
        root.path(),
        &path,
        &heal,
        &super::super::cli::LoginCheck::SignedIn,
        NOW + 1
    ));
    fs::write(
        &path,
        "[[account]]\nlabel = 'account'\nclass = 'dedicated'\nemail = 'owner@example.test'\n",
    )
    .unwrap();
    fs::write(
        &profile,
        r#"{"oauthAccount":{"emailAddress":"owner@example.test"}}"#,
    )
    .unwrap();
    assert!(apply_default_heal(
        &store,
        root.path(),
        &path,
        &heal,
        &super::super::cli::LoginCheck::SignedIn,
        NOW + 1
    ));
    assert_eq!(
        store
            .load_logins(NOW + 1)
            .unwrap()
            .0
            .sign_in_receipts
            .get("account"),
        Some(&1)
    );
}

#[test]
fn receipt_capacity_tracks_live_accounts_after_eviction() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(root.path());
    for index in 0..=accounts::MAX_ACCOUNTS {
        let label = format!("account-{index}");
        let at = NOW + i64::try_from(index).unwrap();
        store
            .transact_logins(at, &mut None, |state, logins| {
                accounts::note_auth(state, &label, accounts::Auth::SignedOut, at);
                Ok(accounts::note_signed_back(state, logins, &label, at))
            })
            .unwrap();
    }
    let logins = store.load_logins(NOW + 100).unwrap().0;
    assert_eq!(
        logins.sign_in_clock,
        u64::try_from(accounts::MAX_ACCOUNTS + 1).unwrap()
    );
    assert_eq!(logins.sign_in_receipts.len(), accounts::MAX_ACCOUNTS);
    assert!(!logins.sign_in_receipts.contains_key("account-0"));
    assert!(
        logins
            .sign_in_receipts
            .contains_key(&format!("account-{}", accounts::MAX_ACCOUNTS))
    );
}
