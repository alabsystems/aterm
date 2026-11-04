// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn native_pool_results_and_checkpoints_use_the_declared_provider_identity() {
    let mut row = accounts::Row {
        email: Some("browser-login@example.com".into()),
        identity_email: Some("provider-identity@example.com".into()),
        ..Default::default()
    };
    for cli in [Cli::Codex, Cli::Kimi, Cli::Agy] {
        assert_eq!(
            renewal_identity_email(cli, &row),
            Some("provider-identity@example.com")
        );
    }
    row.identity_email = None;
    for cli in [Cli::Codex, Cli::Kimi, Cli::Agy] {
        assert_eq!(
            renewal_identity_email(cli, &row),
            Some("browser-login@example.com")
        );
    }
}

#[test]
fn native_pool_preparation_survives_diagnostics_for_kimi_and_agy() {
    for cli in [Cli::Kimi, Cli::Agy] {
        let root = aterm_tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let home = root.path().join("home");
        for path in [&state, &home] {
            aterm_secrets::fsx::ensure_private_dir(path).unwrap();
        }
        let store = accounts::Store::at_for(&state, cli, &home);
        let row = accounts::Row {
            label: "one".into(),
            email: Some("person@example.com".into()),
            vendor: accounts::Vendor {
                region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
                ..Default::default()
            },
            ..Default::default()
        };
        let roster = state.join("native-accounts.toml");
        aterm_secrets::fsx::write_atomic(
            &roster,
            accounts::row_text(&row).unwrap().as_bytes(),
            0o600,
        )
        .unwrap();
        let now = aterm_time::unix_secs()
            .and_then(|n| i64::try_from(n).ok())
            .unwrap();
        let initial = store.load_logins(now).unwrap().0;
        let prepared = prepare(&store, &row, &roster, None, now, &initial)
            .unwrap()
            .unwrap();
        let records = store.read_account_records(Some(&roster), now).unwrap();
        let parent = accounts::RenewalParent::capture(&prepared, &row.label);
        let key = super::super::super::login_drive::RecoveryKey(
            parent
                .recovery_scope(&row, &records.state, &prepared)
                .unwrap(),
        );
        store
            .transact_logins(now, &mut None, |_, logins| {
                accounts::note_sign_in_task(
                    logins,
                    &row.label,
                    accounts::SignInAction::Browser,
                    now,
                );
                Ok(Vec::new())
            })
            .unwrap();
        let records = store.read_account_records(Some(&roster), now).unwrap();
        parent.require_current(&records.logins, &row.label).unwrap();
        snapshot_current(&row, &records.state, &records.logins, key).unwrap();
    }
}

#[test]
fn pooled_authority_rechecks_policy_parent_authentication_and_staging_custody() {
    for case in 0..14 {
        let root = aterm_tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let home = root.path().join("home");
        for path in [&state, &home] {
            aterm_secrets::fsx::ensure_private_dir(path).unwrap();
        }
        let store = accounts::Store::at_for(&state, Cli::Kimi, &home);
        let roster = state.join("kimi-accounts.toml");
        let row = accounts::Row {
            label: "one".into(),
            email: Some("person@example.com".into()),
            vendor: accounts::Vendor {
                region: Some(crate::ai::connector::Region::Global),
                ..Default::default()
            },
            ..Default::default()
        };
        aterm_secrets::fsx::write_atomic(
            &roster,
            accounts::row_text(&row).unwrap().as_bytes(),
            0o600,
        )
        .unwrap();
        let now = aterm_time::unix_secs()
            .and_then(|n| i64::try_from(n).ok())
            .unwrap();
        store
            .transact_logins(now, &mut None, |_, _| Ok(Vec::new()))
            .unwrap();
        let initial = store.load_logins(now).unwrap().0;
        let prepared = prepare(&store, &row, &roster, None, now, &initial)
            .unwrap()
            .unwrap();
        assert!(prepared.get(&row.label).unwrap().renewal_generation > 0);
        assert!(
            prepare(&store, &row, &roster, None, now, &prepared)
                .unwrap()
                .is_none()
        );
        store
            .transact_pool_logins_within(
                &accounts::LOCK_LADDER,
                &roster,
                now,
                &mut None,
                |_, _, logins| {
                    accounts::admit_workflow(logins, &row.label)?;
                    Ok(Vec::new())
                },
            )
            .unwrap();
        let observed = store.read_account_records(Some(&roster), now).unwrap();
        let workflow = accounts::current_workflow(&row, &observed.logins).unwrap();
        let key = super::super::super::login_drive::RecoveryKey(
            accounts::RenewalParent::capture(&observed.logins, &row.label)
                .recovery_scope(&row, &observed.state, &observed.logins)
                .unwrap(),
        );
        let staging = store.claim_staging_dir(&row.label).unwrap();
        let admission = Current {
            store: store.clone(),
            row: row.clone(),
            roster: roster.clone(),
            own_dir: None,
            key,
            workflow,
            staging: staging.clone(),
            location: Location::Staging,
            now,
        }
        .admit();
        assert!(!admission.is_shared_root());
        admission.current(None).unwrap();
        match case {
            13 => {
                let mut changed = row.clone();
                changed.email = Some("replacement@example.com".into());
                for policy in [&changed, &row] {
                    aterm_secrets::fsx::write_atomic(
                        &roster,
                        accounts::row_text(policy).unwrap().as_bytes(),
                        0o600,
                    )
                    .unwrap();
                    store
                        .transact_pool_logins_within(
                            &accounts::LOCK_LADDER,
                            &roster,
                            now,
                            &mut None,
                            |_, _, logins| {
                                accounts::admit_workflow(logins, &row.label)?;
                                Ok(Vec::new())
                            },
                        )
                        .unwrap();
                }
                let final_records = store.read_account_records(Some(&roster), now).unwrap();
                snapshot_current(&row, &final_records.state, &final_records.logins, key).unwrap();
                assert_ne!(
                    accounts::current_workflow(&row, &final_records.logins),
                    Some(workflow)
                );
            }
            1 | 2 => {
                let mut changed = row.clone();
                if case == 1 {
                    changed.held_by = Some("another-machine".into());
                } else {
                    changed.vendor.region = Some(crate::ai::connector::Region::MainlandCn);
                }
                aterm_secrets::fsx::write_atomic(
                    &roster,
                    accounts::row_text(&changed).unwrap().as_bytes(),
                    0o600,
                )
                .unwrap();
            }
            3 => {
                store
                    .transact_logins(now, &mut None, |_, logins| {
                        logins.labels.insert(
                            row.label.clone(),
                            accounts::Binding {
                                renewal_generation: 2,
                                ..Default::default()
                            },
                        );
                        logins.generation_clock = 2;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            4 => {
                store
                    .transact_logins(now, &mut None, |state, _| {
                        let account = state.accounts.entry(row.label.clone()).or_default();
                        account.auth = Auth::SignedOut;
                        account.auth_at = Some(now);
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            5 => {
                std::fs::remove_dir(&staging).unwrap();
            }
            6 => {
                let other = accounts::Row {
                    label: "other".into(),
                    email: Some("other@example.com".into()),
                    dir: Some(staging.to_str().unwrap().into()),
                    ..row.clone()
                };
                let text = accounts::row_text(&row).unwrap() + &accounts::row_text(&other).unwrap();
                aterm_secrets::fsx::write_atomic(&roster, text.as_bytes(), 0o600).unwrap();
            }
            7 => {
                store
                    .transact_logins(now, &mut None, |_, logins| {
                        accounts::note_sign_in_task(
                            logins,
                            &row.label,
                            accounts::SignInAction::Browser,
                            now,
                        );
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            8 => {
                std::fs::remove_file(&roster).unwrap();
            }
            9 => {
                aterm_secrets::fsx::write_atomic(&roster, b"not valid TOML!", 0o600).unwrap();
            }
            10..=12 => {
                store
                    .transact_logins(now, &mut None, |_, logins| {
                        logins.shared.begin(
                            accounts::shared::Intent {
                                source: accounts::shared::Source {
                                    network: [1; 32],
                                    account: [2; 32],
                                },
                                decision: [3; 32],
                                generation: 1,
                                configuration: [4; 32],
                                request: [6; 16],
                                label: if case == 10 {
                                    row.label.clone()
                                } else {
                                    "protected".into()
                                },
                                identity: if case == 10 {
                                    row.email.clone().unwrap()
                                } else {
                                    "protected@example.com".into()
                                },
                                staging: if case == 12 {
                                    store
                                        .logins_dir()
                                        .join("protected")
                                        .to_str()
                                        .unwrap()
                                        .into()
                                } else {
                                    staging.to_str().unwrap().into()
                                },
                                before_generation: Some(0),
                                begun_at: now,
                            },
                            Some([5; 32]),
                        )?;
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            _ => {}
        }
        assert_eq!(
            admission.current(None).is_ok(),
            matches!(case, 0 | 7 | 12),
            "case {case}"
        );
        if case == 12 {
            assert!(admission.current(Some([5; 32])).is_err());
        }
        if case == 0 {
            admission.current(Some([9; 32])).unwrap();
        }
    }
}

#[test]
fn native_canonical_aliases_round_trip_and_refuse_duplicate_provider_identity() {
    for cli in [Cli::Kimi, Cli::Agy] {
        let row = accounts::Row {
            label: "one".into(),
            email: Some("browser@example.com".into()),
            identity_email: Some("provider@example.com".into()),
            vendor: accounts::Vendor {
                region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
                ..Default::default()
            },
            ..Default::default()
        };
        let text = accounts::append_rows_for(cli, "", std::slice::from_ref(&row)).unwrap();
        let roster = accounts::Roster::parse_for(cli, &text).unwrap();
        assert_eq!(
            roster.row("one").unwrap().expected_identity_email(),
            Some("provider@example.com")
        );
        let other = accounts::Row {
            label: "two".into(),
            email: Some("another-browser@example.com".into()),
            ..row
        };
        let duplicate = text + &accounts::row_text(&other).unwrap();
        assert!(accounts::Roster::parse_for(cli, &duplicate).is_err());
        if cli == Cli::Kimi {
            let mut mainland = other.clone();
            mainland.vendor.region = Some(crate::ai::connector::Region::MainlandCn);
            let mut global = roster.rows[0].clone();
            global.dir = Some("/synthetic/global".into());
            mainland.dir = Some("/synthetic/mainland".into());
            let realms =
                accounts::row_text(&global).unwrap() + &accounts::row_text(&mainland).unwrap();
            assert_eq!(
                accounts::Roster::parse_for(cli, &realms)
                    .unwrap()
                    .rows
                    .len(),
                2
            );
        }
    }
}

#[test]
fn a_canonical_kimi_identity_requires_a_region_even_without_a_login_alias() {
    let mut row = accounts::Row {
        label: "canonical".into(),
        identity_email: Some("owner@example.com".into()),
        ..Default::default()
    };
    let text = accounts::row_text(&row).unwrap();
    assert!(
        accounts::Roster::parse_for(Cli::Kimi, &text)
            .unwrap_err()
            .contains("no region")
    );
    row.vendor.region = Some(crate::ai::connector::Region::Global);
    let text = accounts::row_text(&row).unwrap();
    let roster = accounts::Roster::parse_for(Cli::Kimi, &text).unwrap();
    assert_eq!(
        roster.row("canonical").unwrap().expected_identity_email(),
        Some("owner@example.com")
    );
}
