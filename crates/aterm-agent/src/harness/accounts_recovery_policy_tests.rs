// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

#[cfg(unix)]
#[test]
fn google_checkpoint_recovery_requires_its_original_binding_and_root() {
    const REGISTRY: &str = "[[identities]]\nid='one'\nemail='person@example.com'\ncredential_ref='registered-one'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";
    for case in 0..11 {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
        aterm_secrets::fsx::ensure_private_dir(&state).unwrap();
        let registry_path = state.join("google-identities.toml");
        aterm_secrets::fsx::write_atomic(&registry_path, REGISTRY.as_bytes(), 0o600).unwrap();
        let store = accounts::Store::at_for(&state, Cli::Agy, &home);
        let roster = root.path().join("roster.toml");
        let row = Row {
            label: "one".into(),
            email: Some("person@example.com".into()),
            dir: Some(store.login_dir("one").unwrap().to_str().unwrap().into()),
            ..Default::default()
        };
        fs::write(
            &roster,
            accounts::append_rows_for(Cli::Agy, "", &[row]).unwrap(),
        )
        .unwrap();
        let staging = store.claim_staging_dir("one").unwrap();
        let other_root = root.path().join("other-state");
        if case == 6 {
            aterm_secrets::fsx::ensure_private_dir(&other_root).unwrap();
            aterm_secrets::fsx::write_atomic(
                &other_root.join("google-identities.toml"),
                REGISTRY.as_bytes(),
                0o600,
            )
            .unwrap();
        }
        let verified = super::super::agy_signin::publication_fixture(
            if case == 6 { &other_root } else { &state },
            &staging,
            "one",
            "person@example.com",
        );
        let logins = store.load_logins(NOW).unwrap().0;
        accounts::write_google_renewal_checkpoint(
            "one",
            staging.to_str().unwrap(),
            &verified,
            NOW - 1,
            &logins,
        )
        .unwrap();
        drop(verified);
        let checkpoint_path = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
        match case {
            1 => fs::remove_file(&registry_path).unwrap(),
            2 => aterm_secrets::fsx::write_atomic(&registry_path, REGISTRY.replace("registered-one", "registered-replacement").as_bytes(), 0o600).unwrap(),
            3 => aterm_secrets::fsx::write_atomic(&registry_path, REGISTRY.replace("id='one'", "id='replacement'").replace("identity='one'", "identity='replacement'").as_bytes(), 0o600).unwrap(),
            4 => aterm_secrets::fsx::write_atomic(&registry_path, REGISTRY.replace("credential_ref=", "verification_email='verification@example.com'\ncredential_ref=").as_bytes(), 0o600).unwrap(),
            5 => aterm_secrets::fsx::write_atomic(&registry_path, REGISTRY.replace("expected_email='person@example.com'", "expected_email='other@example.com'").as_bytes(), 0o600).unwrap(),
            7 | 8 => {
                let mut doc: Value = aterm_json::from_str(&fs::read_to_string(&checkpoint_path).unwrap()).unwrap();
                let object = doc.as_object_mut().unwrap();
                if case == 7 { object.insert("schema".into(), Value::from(4)); }
                else { object.remove("google_binding_scope"); }
                fs::write(&checkpoint_path, doc.to_string()).unwrap();
            },
            10 => aterm_secrets::fsx::write_atomic(&registry_path, format!("{REGISTRY}\n[[identities]]\nid='unrelated'\nemail='another@example.com'\n[[bindings]]\nprovider='agy'\naccount='another'\nidentity='unrelated'\nexpected_email='another@example.com'\n").as_bytes(), 0o600).unwrap(),
            _ => {},
        }
        let checkpoint_before = fs::read(&checkpoint_path).unwrap();
        let files = [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
        ];
        let before = files.each_ref().map(|path| fs::read(path).ok());
        let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
        let mut admitted = false;
        let recovered = recover_interrupted_staging_renewals_checked(
            Cli::Agy,
            &store,
            &roster,
            &guard,
            NOW,
            (
                &[Duration::ZERO],
                &shared_renewal::CheckpointAuthority::Unavailable,
                None,
            ),
            || {
                if !admitted && case == 9 {
                    aterm_secrets::fsx::write_atomic(
                        &registry_path,
                        REGISTRY
                            .replace("registered-one", "changed-after-admission")
                            .as_bytes(),
                        0o600,
                    )
                    .unwrap();
                }
                admitted = true;
            },
        );
        assert_eq!(recovered, matches!(case, 0 | 10), "case {case}");
        let after = store.load_logins(NOW).unwrap().0;
        if recovered {
            assert_eq!(after.labels["one"].dir.as_deref(), staging.to_str());
            assert_eq!(after.labels["one"].google_principal, Some([1; 32]));
        } else {
            assert_eq!(after, logins, "case {case}");
            assert_eq!(
                files.each_ref().map(|path| fs::read(path).ok()),
                before,
                "case {case}"
            );
        }
        assert_eq!(fs::read(&checkpoint_path).unwrap(), checkpoint_before);
    }
}

pub(super) fn hold_retry(
    guard: &std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    store: &accounts::Store,
    cli: Cli,
    key: &str,
    label: &str,
) {
    let clock = Instant::now();
    let mut state = guard.lock().unwrap();
    state.last_attempt.insert(key.into(), clock);
    state.parked.insert(key.into(), clock);
    state.bot_checked.insert(key.into(), clock);
    state.ladder.failed(key, "fixture", clock);
    state
        .activity_owners
        .insert(key.into(), LoginOwner::new(store, cli, label, NOW));
    state
        .ladder_owners
        .insert(key.into(), LoginOwner::new(store, cli, label, NOW));
}

fn retry_is_held(state: &AutoRenewGuard, key: &str) -> bool {
    let held = [
        state.last_attempt.contains_key(key),
        state.parked.contains_key(key),
        state.bot_checked.contains_key(key),
        state.ladder.has_backoff(key),
        state.activity_owners.contains_key(key),
        state.ladder_owners.contains_key(key),
    ];
    assert!(
        held.iter().all(|value| *value == held[0]),
        "partial cleanup: {held:?}"
    );
    held[0]
}

fn lifetime_checkpoint_case(
    cli: Cli,
    case: i64,
    has_auth: bool,
    clock_at: i64,
    notice_at: i64,
) -> [i64; 3] {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let store = accounts::Store::at_for(&root.path().join("state"), cli, &home);
    store.machine_id("0123456789abcdef").unwrap();
    let path = root.path().join("roster.toml");
    let row = Row {
        label: "account".into(),
        email: Some("owner@example.test".into()),
        dir: Some(store.login_dir("account").unwrap().display().to_string()),
        vendor: accounts::Vendor {
            region: (cli == Cli::Kimi).then_some(Region::Global),
            ..Default::default()
        },
        ..Default::default()
    };
    fs::write(
        &path,
        accounts::append_rows_for(cli, "", std::slice::from_ref(&row)).unwrap(),
    )
    .unwrap();
    let history = |logins: &accounts::Logins| {
        let binding = logins.get("account").unwrap();
        (
            binding.renewed_at,
            binding.notice_end,
            binding.served_at,
            binding.ended_at,
            binding.wrong_at,
            binding.lifetime_s,
        )
    };
    store
        .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, state, logins| {
            let mut events = accounts::note_renewed(
                state,
                logins,
                "account",
                None,
                row.dir.as_deref().unwrap(),
                None,
                if matches!(case, 35 | 38) {
                    clock_at
                } else {
                    NOW - 40
                },
            );
            match case {
                32..=34 => {
                    accounts::note_login_notice(logins, "account", 1, notice_at);
                }
                36 | 39 => {
                    accounts::note_served_login(logins, "account", clock_at);
                }
                37 | 40 => {
                    events.extend(accounts::note_login_end(state, logins, "account", clock_at));
                }
                _ => {}
            }
            state.accounts.remove("account");
            Ok(events)
        })
        .unwrap();
    let original = store.read_account_records(Some(&path), NOW).unwrap();
    let original_history = history(&original.logins);
    let original_generation = original.logins.get("account").unwrap().renewal_generation;
    let replacement = root.path().join("replacement").display().to_string();
    store
        .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, _, logins| {
            logins.labels.get_mut("account").unwrap().dir = Some(replacement.clone());
            Ok(Vec::new())
        })
        .unwrap();
    let replaced = store.read_account_records(Some(&path), NOW).unwrap();
    assert_eq!(history(&replaced.logins), original_history);
    assert!(replaced.logins.get("account").unwrap().renewal_generation > original_generation);
    if cli != Cli::Codex {
        let row = replaced.roster.row("account").unwrap();
        assert_eq!(
            accounts::login_forecast(row, &replaced.logins, &replaced.state),
            None
        );
        assert_eq!(
            accounts::login_served_at(row, &replaced.logins, &replaced.state),
            None
        );
        assert_eq!(
            accounts::login_ended_at(row, &replaced.logins, &replaced.state),
            None
        );
    }
    store
        .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, state, logins| {
            let mut events = Vec::new();
            match case {
                33 => {
                    accounts::note_login_notice(logins, "account", 1, notice_at);
                }
                38 => {
                    events.extend(accounts::note_renewed(
                        state,
                        logins,
                        "account",
                        None,
                        &replacement,
                        None,
                        clock_at,
                    ));
                }
                39 => {
                    accounts::note_served_login(logins, "account", clock_at);
                }
                40 => {
                    events.extend(accounts::note_login_end(state, logins, "account", clock_at));
                }
                _ => {}
            }
            // Isolate lifetime fences from a later current authentication word.
            state.accounts.remove("account");
            if has_auth {
                events.extend(accounts::note_auth_in_context(
                    state,
                    logins,
                    "account",
                    accounts::Auth::Ok,
                    NOW - 10,
                ));
            }
            Ok(events)
        })
        .unwrap();
    let before_checkpoint = store.read_account_records(Some(&path), NOW).unwrap();
    assert_eq!(history(&before_checkpoint.logins), original_history);
    if has_auth && cli != Cli::Codex {
        assert_eq!(
            before_checkpoint
                .state
                .account("account")
                .current_auth(before_checkpoint.roster.row("account").unwrap(),),
            accounts::Auth::Ok,
        );
    }
    let staging = store.claim_staging_dir("account").unwrap();
    let login_file = staging.join(cli.connector().stores[0]);
    fs::create_dir_all(login_file.parent().unwrap()).unwrap();
    fs::write(&login_file, b"private lifetime checkpoint fixture").unwrap();
    accounts::write_renewal_checkpoint_for(
        cli,
        row.vendor.region,
        "account",
        "owner@example.test",
        staging.to_str().unwrap(),
        None,
        Some(accounts::Proof::Status),
        NOW - 1,
        &before_checkpoint.logins,
    )
    .unwrap();
    let actual_dir = fs::canonicalize(&staging).unwrap();
    let proof = accounts::read_renewal_checkpoint_for(cli, &staging, &actual_dir, NOW).unwrap();
    if case == 34 || case == 41 {
        store
            .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, _, logins| {
                if case == 34 {
                    logins.labels.get_mut("account").unwrap().dir = Some(
                        root.path()
                            .join("subsequent-replacement")
                            .display()
                            .to_string(),
                    );
                    Ok(Vec::new())
                } else {
                    Ok(accounts::note_wrong_account(logins, "account", clock_at))
                }
            })
            .unwrap();
    }
    let records = store.read_account_records(Some(&path), NOW).unwrap();
    let current_row = records.roster.row("account").unwrap();
    assert!(!accounts::enrolled_for(cli, &records.roster).contains("account"));
    assert_eq!(records.state.accounts.contains_key("account"), has_auth);
    assert!(
        records
            .state
            .account("account")
            .auth_at
            .is_none_or(|at| at < proof.checked_at)
    );
    let mut expected_history = original_history;
    if case == 41 {
        expected_history.4 = Some(clock_at);
    }
    assert_eq!(history(&records.logins), expected_history);
    let current = accounts::checkpoint_current(
        current_row,
        &records.state,
        &records.logins,
        &proof,
        store.dir.parent(),
    );
    let allowed = matches!(case, 32 | 35..=37) && cli != Cli::Codex;
    assert_eq!(current, allowed, "{cli:?} case {case}");
    if allowed {
        let mut raw_veto = records.logins.clone();
        raw_veto.labels.get_mut("account").unwrap().lifetime_scope = Default::default();
        assert!(
            !accounts::checkpoint_current(
                current_row,
                &records.state,
                &raw_veto,
                &proof,
                store.dir.parent(),
            ),
            "restoring the original raw veto loses the checked new-parent recovery"
        );
    }
    if matches!(case, 33 | 38..=41) {
        let mut ignored = records.logins.clone();
        let binding = ignored.labels.get_mut("account").unwrap();
        match case {
            33 => binding.notice_end = None,
            38 => binding.renewed_at = None,
            39 => binding.served_at = None,
            40 => binding.ended_at = None,
            41 => binding.wrong_at = None,
            _ => unreachable!(),
        }
        assert!(
            accounts::checkpoint_current(
                current_row,
                &records.state,
                &ignored,
                &proof,
                store.dir.parent(),
            ),
            "the current observation independently fences case {case}"
        );
    } else if case == 34 && cli != Cli::Codex {
        let mut ignored = proof.clone();
        ignored.parent = accounts::RenewalParent::capture(&records.logins, "account");
        assert!(
            accounts::checkpoint_current(
                current_row,
                &records.state,
                &records.logins,
                &ignored,
                store.dir.parent(),
            ),
            "the stale parent independently fences recovery"
        );
    }
    let images = || {
        [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.state_witness_file(),
            store.logins_witness_file(),
            store.wake_path(),
            store.machine_file(),
            path.clone(),
            store.dir.join("sync-commit.json"),
            store.dir.join("sync-commit.seen"),
        ]
        .map(|path| fs::read(path).ok())
    };
    let before = images();
    let proof_path = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
    let proof_before = fs::read(&proof_path).unwrap();
    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    hold_retry(&guard, &store, cli, "local", "account");
    hold_retry(&guard, &store, cli, "foreign", "another-account");
    let recovered = recover_interrupted_staging_renewals_for(
        cli,
        &store,
        &path,
        &guard,
        NOW,
        shared_renewal::CheckpointAuthority::Unavailable,
    );
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
            staging.to_str()
        );
    } else {
        assert_eq!(images(), before, "{cli:?} case {case}");
        assert_eq!(
            history(&store.load_logins(NOW).unwrap().0),
            expected_history
        );
    }
    assert_eq!(
        fs::read(&login_file).unwrap(),
        b"private lifetime checkpoint fixture"
    );
    assert_eq!(fs::read(&proof_path).unwrap(), proof_before);
    let held = guard.lock().unwrap();
    [
        i64::from(recovered),
        i64::from(retry_is_held(&held, "local")),
        i64::from(retry_is_held(&held, "foreign")),
    ]
}

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
            if state["case"] >= 32 {
                let mut outcome = None;
                for has_auth in [false, true] {
                    for newer in [false, true] {
                        let clock_at = NOW - 1 + i64::from(newer);
                        let notice_at = if newer { NOW - 2 * 86_400 } else { NOW - 20 };
                        let observed = lifetime_checkpoint_case(
                            cli,
                            state["case"],
                            has_auth,
                            clock_at,
                            notice_at,
                        );
                        if let Some(expected) = outcome {
                            assert_eq!(observed, expected);
                        } else {
                            outcome = Some(observed);
                        }
                    }
                }
                let [recovered, local_hold, foreign_hold] = outcome.unwrap();
                next.insert("phase", 2);
                next.insert("recovered", recovered);
                next.insert("local_hold", local_hold);
                next.insert("foreign_hold", foreign_hold);
                return next;
            }
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
                    region: (cli == Cli::Kimi).then_some(if matches!(state["case"], 29 | 30) {
                        Region::MainlandCn
                    } else {
                        Region::Global
                    }),
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
                row.vendor.region,
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
                        row.vendor.region,
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
                27 | 29 if cli == Cli::Kimi => {
                    let mut changed = row.clone();
                    changed.vendor.region = Some(if state["case"] == 27 {
                        Region::MainlandCn
                    } else {
                        Region::Global
                    });
                    fs::write(
                        &path,
                        accounts::append_rows_for(cli, "", &[changed]).unwrap(),
                    )
                    .unwrap();
                }
                28 => {
                    let path = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
                    let mut doc: Value =
                        aterm_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
                    let object = doc.as_object_mut().unwrap();
                    object.insert("schema".into(), Value::from(2));
                    object.remove("region");
                    fs::write(path, doc.to_string()).unwrap();
                }
                31 => {
                    // A completion receipt must not substitute for the vendor files it
                    // describes. Preserve the removed file outside the candidate home.
                    fs::rename(&login_file, root.path().join("retained-login")).unwrap();
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
            hold_retry(&guard, &store, cli, "local", "account");
            hold_retry(&guard, &store, cli, "foreign", "another-account");
            let recovered = recover_interrupted_staging_renewals_for(
                cli,
                &store,
                &path,
                &guard,
                NOW,
                shared_renewal::CheckpointAuthority::Unavailable,
            );
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
            let retained_login = if state["case"] == 31 {
                root.path().join("retained-login")
            } else {
                login_file
            };
            assert_eq!(fs::read(retained_login).unwrap(), b"private fixture");
            assert_eq!(fs::read(&proof_path).unwrap(), proof_before);
            next.insert("phase", 2);
            next.insert("recovered", i64::from(recovered));
            let held = guard.lock().unwrap();
            next.insert("local_hold", i64::from(retry_is_held(&held, "local")));
            next.insert("foreign_hold", i64::from(retry_is_held(&held, "foreign")));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn checkpoint_recovery_conforms_to_current_policy_for_every_provider() {
    let model = aterm_models_harness::account_recovery_policy_model();
    assert_eq!(model.mutant_dials(), (1..=50).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 492);
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
    let mut cleared_retry = forged.clone();
    cleared_retry.insert("local_hold", 0);
    assert!(!model.check_invariant("RecoveryReleasesOwnRetry", &cleared_retry));
    forged.insert("recovered", 1);
    assert!(!model.check_invariant("AdmissibleRecoveryOnly", &forged));
    let mut current = model.init_state();
    current.insert("phase", 1);
    let mut refused = recovery_transition(&current, "Recover");
    let mut stale_retry = refused.clone();
    stale_retry.insert("local_hold", 1);
    assert!(!model.check_invariant("RecoveryReleasesOwnRetry", &stale_retry));
    let mut foreign_retry = refused.clone();
    foreign_retry.insert("foreign_hold", 0);
    assert!(!model.check_invariant("ForeignRetryPreserved", &foreign_retry));
    refused.insert("recovered", 0);
    assert!(!model.check_invariant("AdmissibleRecoverySucceeds", &refused));
    let mut rolled_back = model.init_state();
    rolled_back.insert("case", 10);
    rolled_back.insert("phase", 1);
    let mut stale = recovery_transition(&rolled_back, "Recover");
    assert_eq!(stale["recovered"], 0);
    stale.insert("recovered", 1);
    assert!(!model.check_invariant("AdmissibleRecoveryOnly", &stale));
    for case in [27, 28, 29] {
        let mut rejected = model.init_state();
        rejected.insert("provider", 4);
        rejected.insert("case", case);
        rejected.insert("phase", 1);
        let mut forged = recovery_transition(&rejected, "Recover");
        forged.insert("recovered", 1);
        assert!(!model.check_invariant("AdmissibleRecoveryOnly", &forged));
        rejected.insert("provider", 2);
        let mut refused = recovery_transition(&rejected, "Recover");
        refused.insert("recovered", 0);
        assert!(!model.check_invariant("AdmissibleRecoverySucceeds", &refused));
    }
    for (case, recovered, law) in [
        (32, 0, "RetiredLifetimeAllowsNewParent"),
        (33, 1, "CurrentLifetimeBlocksRecovery"),
        (34, 1, "ParentFenceSurvivesRetirement"),
        (41, 1, "WrongAccountFencePreserved"),
    ] {
        let mut before = model.init_state();
        before.insert("case", case);
        before.insert("phase", 1);
        let mut forged = recovery_transition(&before, "Recover");
        forged.insert("recovered", recovered);
        assert!(!model.check_invariant(law, &forged));
    }
}

#[test]
fn checkpoint_alone_cannot_recover_a_login() {
    for provider in 1..=4 {
        let mut state = aterm_models_harness::account_recovery_policy_model().init_state();
        state.insert("provider", provider);
        state.insert("case", 31);
        state.insert("phase", 1);
        let next = recovery_transition(&state, "Recover");
        assert_eq!(next["recovered"], 0, "provider {provider}");
    }
}

#[test]
fn checkpoint_recovery_refuses_a_changed_kimi_account_region() {
    for case in [27, 29] {
        let mut state = aterm_models_harness::account_recovery_policy_model().init_state();
        state.insert("provider", 4);
        state.insert("case", case);
        state.insert("phase", 1);
        let next = recovery_transition(&state, "Recover");
        assert_eq!(next["recovered"], 0, "case {case}");
    }
}

#[test]
fn checkpoint_region_encoding_rejects_missing_or_foreign_scope() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let mut index = 0;
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        for region in [None, Some(Region::Global), Some(Region::MainlandCn)] {
            let staging = root.path().join(index.to_string());
            index += 1;
            fs::create_dir(&staging).unwrap();
            let checkpoint_path = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
            let result = accounts::write_renewal_checkpoint_for(
                cli,
                region,
                "account",
                "owner@example.test",
                staging.to_str().unwrap(),
                None,
                Some(accounts::Proof::Status),
                NOW,
                &accounts::Logins::default(),
            );
            let valid = (cli == Cli::Kimi) == region.is_some();
            assert_eq!(result.is_ok(), valid, "{cli:?}: {region:?}");
            if !valid {
                assert!(!checkpoint_path.exists());
                continue;
            }
            let actual = fs::canonicalize(&staging).unwrap();
            assert_eq!(
                accounts::read_renewal_checkpoint_for(cli, &staging, &actual, NOW)
                    .unwrap()
                    .region,
                region
            );
            let original: Value =
                aterm_json::from_str(&fs::read_to_string(&checkpoint_path).unwrap()).unwrap();
            assert_eq!(
                original["schema"].as_u64(),
                Some(if cli == Cli::Kimi { 3 } else { 2 })
            );
            if cli != Cli::Kimi {
                assert!(original.get("region").is_none());
                continue;
            }
            for case in 0..7 {
                let mut invalid = original.clone();
                let object = invalid.as_object_mut().unwrap();
                match case {
                    0 => {
                        object.remove("region");
                    }
                    1 => {
                        object.insert("region".into(), Value::Null);
                    }
                    2 => {
                        object.insert("region".into(), Value::from("unknown"));
                    }
                    3 => {
                        object.insert("region".into(), Value::from(1));
                    }
                    4 => {
                        object.insert("schema".into(), Value::from(2));
                    }
                    5 => {
                        object.insert("schema".into(), Value::from(4));
                    }
                    6 => {
                        object.insert("program".into(), Value::from("codex"));
                    }
                    _ => unreachable!(),
                }
                let bytes = invalid.to_string();
                fs::write(&checkpoint_path, &bytes).unwrap();
                assert!(
                    accounts::read_renewal_checkpoint_for(cli, &staging, &actual, NOW).is_none(),
                    "{region:?}: invalid case {case}"
                );
                assert_eq!(fs::read_to_string(&checkpoint_path).unwrap(), bytes);
            }
        }
    }
}
