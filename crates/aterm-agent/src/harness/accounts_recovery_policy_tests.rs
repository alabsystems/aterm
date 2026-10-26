// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

pub(crate) fn recovery_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherProvider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Choose" => {
            next.insert("phase", 1);
        }
        "Recover" => {
            let cli = match state["provider"] {
                1 => Cli::Claude,
                2 => Cli::Codex,
                3 => Cli::Grok,
                4 => Cli::Kimi,
                _ => unreachable!(),
            };
            let root = aterm_tempfile::TempDir::new().unwrap();
            let home = root.path().join("home");
            fs::create_dir_all(&home).unwrap();
            let store = accounts::Store::at_for(&root.path().join("state"), cli, &home);
            let path = root.path().join("roster.toml");
            let row = Row {
                label: "account".into(),
                email: Some("owner@example.test".into()),
                dir: Some(store.login_dir("account").unwrap().to_str().unwrap().into()),
                vendor: accounts::Vendor {
                    region: (cli == Cli::Kimi).then_some(Region::Global),
                    ..accounts::Vendor::default()
                },
                ..Row::default()
            };
            let text = accounts::append_rows_for(cli, "", std::slice::from_ref(&row)).unwrap();
            fs::write(&path, &text).unwrap();
            let captured = Roster::load_for(cli, &path).unwrap();
            assert_eq!(
                captured.rows[0].email.as_deref(),
                Some("owner@example.test")
            );
            let renew = |label: &str, at| {
                store
                    .transact_logins(at, &mut None, |state, logins| {
                        Ok(accounts::note_renewed(
                            state,
                            logins,
                            label,
                            None,
                            store.login_dir(label).unwrap().to_str().unwrap(),
                            None,
                            at,
                        ))
                    })
                    .unwrap();
            };
            if matches!(state["case"], 9..=12 | 19 | 22 | 24 | 26) {
                renew("account", NOW - 40);
            }
            if matches!(state["case"], 14 | 15 | 20 | 23 | 25) {
                // Migration can start with zero; current transactions version insertions.
                let mut legacy = accounts::Logins::default();
                let label = if state["case"] == 25 {
                    "source"
                } else {
                    "account"
                };
                legacy
                    .labels
                    .insert(label.into(), accounts::Binding::default());
                store.save_logins(&legacy).unwrap();
            }
            let staging = store.claim_staging_dir("account").unwrap();
            let login_file = staging.join(cli.connector().stores[0]);
            fs::create_dir_all(login_file.parent().unwrap()).unwrap();
            fs::write(&login_file, b"private fixture").unwrap();
            accounts::write_renewal_checkpoint_for(
                cli,
                "account",
                "owner@example.test",
                staging.to_str().unwrap(),
                None,
                Some(accounts::Proof::Status),
                NOW - 1,
                &store.load_logins(NOW).unwrap().0,
            )
            .unwrap();
            let mut expected_dir = staging.clone();
            match state["case"] {
                2 => fs::write(
                    &path,
                    text.replace("owner@example.test", "changed@example.test"),
                )
                .unwrap(),
                3 => fs::write(&path, "").unwrap(),
                4 => fs::write(&path, "[[broken").unwrap(),
                5 => fs::rename(&path, root.path().join("kept-roster")).unwrap(),
                6 | 7 => {
                    let mut changed = row.clone();
                    if state["case"] == 6 {
                        changed.email = None;
                    } else if matches!(cli, Cli::Claude | Cli::Codex) {
                        changed.identity_email = Some("changed@example.test".into());
                    } else {
                        changed.email = Some("changed@example.test".into());
                    }
                    fs::write(
                        &path,
                        accounts::append_rows_for(cli, "", &[changed]).unwrap(),
                    )
                    .unwrap();
                }
                8 => {
                    let live =
                        Path::new(row.dir.as_deref().unwrap()).join(cli.connector().stores[0]);
                    fs::create_dir_all(live.parent().unwrap()).unwrap();
                    fs::write(&live, b"existing live credentials").unwrap();
                }
                10 | 11 => renew(
                    "account",
                    if state["case"] == 10 {
                        NOW - 30
                    } else {
                        NOW - 1
                    },
                ),
                12 => {
                    store
                        .transact_logins(NOW, &mut None, |state, logins| {
                            accounts::move_account(state, logins, "account", "moved")?;
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
                13 | 14 => {
                    renew("account", NOW - 30);
                    store
                        .transact_logins(NOW, &mut None, |_, logins| {
                            logins.labels.remove("account");
                            if state["case"] == 14 {
                                logins
                                    .labels
                                    .insert("account".into(), accounts::Binding::default());
                            }
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
                15 | 19 => renew("unrelated", NOW - 30),
                16..=18 => {
                    let proof_path = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
                    let mut doc: Value =
                        aterm_json::from_str(&fs::read_to_string(&proof_path).unwrap()).unwrap();
                    let object = doc.as_object_mut().unwrap();
                    match state["case"] {
                        16 => {
                            object.remove("parent_generation");
                        }
                        17 => {
                            object.insert("schema".into(), Value::from(1));
                            object.remove("parent_generation");
                            object.remove("parent_clock");
                        }
                        18 => {
                            object.insert("parent_generation".into(), Value::from(1));
                        }
                        _ => unreachable!(),
                    }
                    fs::write(proof_path, doc.to_string()).unwrap();
                }
                21 => {
                    store
                        .transact_logins(NOW, &mut None, |_, logins| {
                            logins
                                .labels
                                .insert("account".into(), accounts::Binding::default());
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
                22 => {
                    // The stale proof sorts first by wall time. Recovery must skip it and
                    // adopt the current parent's completion, even though its clock is lower.
                    renew("account", NOW - 30);
                    expected_dir = store.claim_staging_dir("account").unwrap();
                    let login = expected_dir.join(cli.connector().stores[0]);
                    fs::create_dir_all(login.parent().unwrap()).unwrap();
                    fs::write(login, b"current private fixture").unwrap();
                    accounts::write_renewal_checkpoint_for(
                        cli,
                        "account",
                        "owner@example.test",
                        expected_dir.to_str().unwrap(),
                        None,
                        Some(accounts::Proof::Status),
                        NOW - 2,
                        &store.load_logins(NOW).unwrap().0,
                    )
                    .unwrap();
                }
                _ => {}
            }
            if matches!(state["case"], 23..=25) {
                let (from, to) = if state["case"] == 25 {
                    ("source", "account")
                } else {
                    ("account", "away")
                };
                for (from, to) in [(from, to), (to, from)] {
                    store
                        .transact_logins(NOW - 20, &mut None, |state, logins| {
                            accounts::move_account(state, logins, from, to)?;
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
            }
            if state["case"] == 26 {
                store
                    .transact_logins(NOW - 20, &mut None, |_, logins| {
                        logins.labels.get_mut("account").unwrap().dir =
                            Some(root.path().join("replacement").to_str().unwrap().into());
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            let files = [
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                store.wake_path(),
            ];
            let before = files.each_ref().map(|path| fs::read(path).ok());
            let proof_path = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
            let proof_before = fs::read(&proof_path).unwrap();
            let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
            let recovered =
                recover_interrupted_staging_renewals_for(cli, &store, &path, &guard, NOW);
            if recovered {
                assert_eq!(
                    store
                        .load_logins(NOW)
                        .unwrap()
                        .0
                        .get("account")
                        .unwrap()
                        .dir
                        .as_deref(),
                    expected_dir.to_str()
                );
            } else {
                assert_eq!(files.each_ref().map(|path| fs::read(path).ok()), before);
            }
            assert_eq!(fs::read(&login_file).unwrap(), b"private fixture");
            assert_eq!(fs::read(&proof_path).unwrap(), proof_before);
            next.insert("phase", 2);
            next.insert("recovered", i64::from(recovered));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn checkpoint_recovery_conforms_to_current_policy_for_every_provider() {
    let model = aterm_models_harness::account_recovery_policy_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = recovery_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut changed = model.init_state();
    changed.insert("case", 2);
    changed.insert("phase", 1);
    let mut forged = recovery_transition(&changed, "Recover");
    forged.insert("recovered", 1);
    assert!(!model.check_invariant("AdmissibleRecoveryOnly", &forged));
    let mut current = model.init_state();
    current.insert("phase", 1);
    let mut refused = recovery_transition(&current, "Recover");
    refused.insert("recovered", 0);
    assert!(!model.check_invariant("AdmissibleRecoverySucceeds", &refused));
    let mut rolled_back = model.init_state();
    rolled_back.insert("case", 10);
    rolled_back.insert("phase", 1);
    let mut stale = recovery_transition(&rolled_back, "Recover");
    assert_eq!(stale["recovered"], 0);
    stale.insert("recovered", 1);
    assert!(!model.check_invariant("AdmissibleRecoveryOnly", &stale));
}
