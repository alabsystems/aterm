// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

fn publication_case(cli: Cli, case: i64) -> [bool; 3] {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at_for(&root.path().join("state"), cli, &root.path().join("home"));
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
    let renew = |at| {
        store.transact_logins(at, &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "account",
                None,
                root.path().join("login").to_str().unwrap(),
                None,
                at,
            ))
        })
    };
    if case != 18 {
        renew(NOW).unwrap();
    }
    let before = store.load_logins(NOW).unwrap().0;
    if case != 18 {
        assert_ne!(before.get("account").unwrap().last_success_generation, 0);
    }
    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    super::recovery_policy_tests::hold_retry(&guard, &store, cli, "local", "account");
    super::recovery_policy_tests::hold_retry(&guard, &store, cli, "foreign", "another");
    {
        let mut held = guard.lock().unwrap();
        for key in ["local", "foreign"] {
            let owner = held.activity_owners.get_mut(key).unwrap();
            *owner = owner.clone().observing(&before);
            let owner = held.ladder_owners.get_mut(key).unwrap();
            *owner = owner.clone().observing(&before);
        }
    }
    let mut captured = LoginClaims::capture(&guard);
    let blocked = store.state_witness_file();
    if matches!(case, 1 | 2 | 9 | 19 | 22) {
        fs::rename(&blocked, blocked.with_extension("original")).unwrap();
        fs::create_dir(&blocked).unwrap();
        assert!(renew(NOW + 1).is_err());
        if case != 1 {
            fs::rename(&blocked, blocked.with_extension("obstruction")).unwrap();
            fs::rename(blocked.with_extension("original"), &blocked).unwrap();
        }
        if case == 2 {
            // A report read consumes the pending journal before upkeep sees it.
            store.read_account_records(Some(&path), NOW + 2).unwrap();
            assert!(!store.sync_needs_recovery());
        } else if case == 9 {
            fs::write(store.logins_file(), "conflicting private work").unwrap();
        }
    } else if case != 16 {
        renew(if case == 15 { NOW - 100 } else { NOW + 1 }).unwrap();
    }
    match case {
        3 => {
            store
                .transact_logins(NOW + 2, &mut None, |_, logins| {
                    logins.labels.get_mut("account").unwrap().dir = Some(
                        root.path()
                            .join("changed-identity")
                            .to_str()
                            .unwrap()
                            .into(),
                    );
                    Ok(Vec::new())
                })
                .unwrap();
        }
        4 | 13 | 14 => {
            let after = store.load_logins(NOW + 2).unwrap().0;
            let mut held = guard.lock().unwrap();
            if case != 13 {
                let owner = held.activity_owners.get_mut("local").unwrap();
                *owner = owner.clone().observing(&after);
            }
            if case != 14 {
                let owner = held.ladder_owners.get_mut("local").unwrap();
                *owner = owner.clone().observing(&after);
            }
        }
        5 => {
            let mut held = guard.lock().unwrap();
            held.activity_owners.get_mut("local").unwrap().store = root.path().join("elsewhere");
            held.ladder_owners.get_mut("local").unwrap().cli = if cli == Cli::Claude {
                Cli::Codex
            } else {
                Cli::Claude
            };
        }
        6 => {
            let mut held = guard.lock().unwrap();
            held.activity_owners
                .get_mut("local")
                .unwrap()
                .success_generation = None;
            held.ladder_owners
                .get_mut("local")
                .unwrap()
                .success_generation = None;
        }
        7 => {
            // Replacement claims with the same baseline and clocks are new work.
            let mut held = guard.lock().unwrap();
            held.activity_owners.get_mut("local").unwrap().token = std::sync::Arc::new(());
            held.ladder_owners.get_mut("local").unwrap().token = std::sync::Arc::new(());
        }
        8 | 17 => {
            let mut doc: Value =
                aterm_json::from_str(&fs::read_to_string(store.logins_file()).unwrap()).unwrap();
            let binding = doc
                .as_object_mut()
                .unwrap()
                .get_mut("labels")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .get_mut("account")
                .unwrap()
                .as_object_mut()
                .unwrap();
            if case == 8 {
                binding.remove("last_success_generation");
            } else {
                binding.insert("last_success_generation".into(), Value::from("invalid"));
            }
            fs::write(store.logins_file(), doc.to_string()).unwrap();
        }
        11 | 16 => {
            store
                .transact_logins(NOW + 2, &mut None, |_, logins| {
                    let binding = logins.labels.get_mut("account").unwrap();
                    if case == 16 {
                        binding.last_success_generation = u64::MAX;
                    } else {
                        binding.subscription = Some("updated metadata".into());
                    }
                    Ok(Vec::new())
                })
                .unwrap();
        }
        12 => {
            for (from, to) in [("account", "away"), ("away", "account")] {
                store
                    .transact_logins(NOW + 2, &mut None, |state, logins| {
                        accounts::move_account(state, logins, from, to)?;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
        }
        _ => {}
    }
    if matches!(case, 19..=22) {
        let setting = match case {
            19 | 22 => "logins = \"off\"",
            20 => "enabled = false",
            _ => "accounts = \"off\"",
        };
        fs::write(config_beside(&path), format!("[harness]\n{setting}\n")).unwrap();
        let cooldown = std::sync::Mutex::new(None);
        let rediscovery = std::sync::Mutex::new(Rediscovery::default());
        let fx = UpkeepEffects {
            fetch: &|_, _| panic!("disabled upkeep fetched a source"),
            spawn_renewal: &|_| panic!("disabled upkeep started a login"),
            sync_cooldown: &cooldown,
            rediscovery: &rediscovery,
            renew_guard: &guard,
            find_browser: &|_| false,
            vault_sync: |_| super::super::sign_in_ladder::Vault::NotMember,
            spawn_vault: &|_| panic!("disabled upkeep started a vault worker"),
            default_check: |_| panic!("disabled upkeep checked a login"),
            mailbox: &ladder_mailbox_files,
            log: std::sync::Arc::new(|_| {}),
            run_attempt: None,
            shared: shared_renewal::Effects::default(),
        };
        let before = fs::read(store.logins_file()).unwrap();
        if case == 22 {
            assert!(store.sync_needs_recovery());
        }
        if case == 22 {
            observe_committed_logins(&store, cli, &guard, NOW + 2);
        } else {
            let result = upkeep(
                &root.path().join("home"),
                &root.path().join("state"),
                (&path, NOW + 2, UpkeepStep::Renew),
                &fx,
            );
            assert_eq!(result == Upkept::Off, case == 21);
        }
        if case == 19 {
            assert!(!store.sync_needs_recovery());
        } else {
            assert_eq!(fs::read(store.logins_file()).unwrap(), before);
        }
        if case == 22 {
            assert!(store.sync_needs_recovery());
        }
    } else if case == 7 {
        let after = store.load_logins(NOW + 2).unwrap().0;
        note_observed_publications(&guard, captured, &store, cli, &after);
    } else {
        // Production captures current ownership at entry, including independent baselines.
        captured = LoginClaims::capture(&guard);
        let _ = recover_interrupted_staging_renewals_with(
            cli,
            &store,
            &path,
            &guard,
            NOW + 2,
            &[],
            || {},
        );
        if case == 10 {
            let after = store.load_logins(NOW + 2).unwrap().0;
            note_observed_publications(&guard, captured, &store, cli, &after);
        }
    }
    let held = guard.lock().unwrap();
    let activity = |key: &str| {
        let values = [
            held.last_attempt.contains_key(key),
            held.parked.contains_key(key),
            held.bot_checked.contains_key(key),
            held.activity_owners.contains_key(key),
        ];
        assert!(
            values.iter().all(|value| *value == values[0]),
            "partial activity cleanup {case}: {values:?}"
        );
        values[0]
    };
    let ladder = |key: &str| {
        assert_eq!(
            held.ladder.has_backoff(key),
            held.ladder_owners.contains_key(key)
        );
        held.ladder.has_backoff(key)
    };
    [
        activity("local"),
        ladder("local"),
        activity("foreign") && ladder("foreign"),
    ]
}

pub(crate) fn publication_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Provider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "Case" => {
            next.insert("case", state["case"] + 1);
        }
        "Observe" => {
            let cli = [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi]
                [usize::try_from(state["provider"] - 1).unwrap()];
            let [activity, ladder, foreign] = publication_case(cli, state["case"]);
            next.insert("phase", 1);
            next.insert("activity", i64::from(activity));
            next.insert("ladder", i64::from(ladder));
            next.insert("foreign", i64::from(foreign));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn durable_publication_observation_conforms_across_providers() {
    let model = aterm_models_harness::account_publication_activity_model();
    assert_eq!(model.mutant_dials(), (1..=13).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 184);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = publication_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let correct = publication_transition(&model.init_state(), "Observe");
    for (field, law) in [
        ("activity", "ActivityTracksSuccess"),
        ("ladder", "LadderTracksSuccess"),
        ("foreign", "ForeignSurvives"),
    ] {
        let mut bad = correct.clone();
        bad.insert(field, 1 - correct[field]);
        assert!(!model.check_invariant(law, &bad));
    }
}

#[test]
fn success_generation_is_optional_but_never_exceeds_its_binding() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(root.path());
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "account",
                None,
                root.path().join("login").to_str().unwrap(),
                None,
                NOW,
            ))
        })
        .unwrap();
    let logins = store.load_logins(NOW).unwrap().0;
    let generation = logins.get("account").unwrap().renewal_generation;
    assert_eq!(
        logins.get("account").unwrap().last_success_generation,
        generation
    );
    let original: Value = aterm_json::from_str(&logins.to_json()).unwrap();
    for (value, admitted) in [
        (Some(Value::from(0_u64)), true),
        (Some(Value::from(generation)), true),
        (None, true),
        (Some(Value::from(generation + 1)), false),
        (Some(Value::from(-1_i64)), false),
        (Some(Value::Null), false),
        (Some(Value::from("1")), false),
    ] {
        let mut document = original.clone();
        let object = document
            .as_object_mut()
            .unwrap()
            .get_mut("labels")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("account")
            .unwrap()
            .as_object_mut()
            .unwrap();
        if let Some(value) = value {
            object.insert("last_success_generation".into(), value);
        } else {
            object.remove("last_success_generation");
        }
        assert_eq!(
            accounts::Logins::from_json(&document.to_string(), NOW).is_ok(),
            admitted
        );
    }
}

#[test]
fn vendor_completion_clears_only_the_successful_accounts_captured_retry() {
    use super::super::cli::Renewal;
    for cli in [Cli::Codex, Cli::Kimi] {
        for outcome in [Renewal::Renewed, Renewal::AuthFailed, Renewal::Busy] {
            let root = aterm_tempfile::TempDir::new().unwrap();
            let store = accounts::Store::at_for(root.path(), cli, &root.path().join("home"));
            let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
            super::recovery_policy_tests::hold_retry(
                &guard,
                &store,
                cli,
                "vendor:account",
                "account",
            );
            super::recovery_policy_tests::hold_retry(&guard, &store, cli, "foreign", "other");
            let claims = LoginClaims::capture(&guard);
            note_vendor_renewal_end(
                &guard,
                &claims,
                &store,
                cli,
                ("vendor:account", "account"),
                outcome,
            );
            let held = guard.lock().unwrap();
            assert_eq!(
                held.last_attempt.contains_key("vendor:account"),
                outcome != Renewal::Renewed
            );
            assert_eq!(
                held.activity_owners.contains_key("vendor:account"),
                outcome != Renewal::Renewed
            );
            assert_eq!(
                held.ladder.has_backoff("vendor:account"),
                outcome != Renewal::Renewed
            );
            assert!(held.last_attempt.contains_key("foreign"));
            assert!(held.parked.contains_key("foreign"));
            assert!(held.bot_checked.contains_key("foreign"));
            assert!(held.ladder.has_backoff("foreign"));
        }
    }
}
