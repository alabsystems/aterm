// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_prepared_account_is_not_a_next_sign_in_target() {
    let row = Row {
        label: "work".into(),
        email: Some("work@example.test".into()),
        dir: Some(crate::harness::host_path::abs("/unmade/login")),
        ..Default::default()
    };
    let roster = Roster { rows: vec![row] };
    let mut state = accounts::State::default();
    let enrolled = BTreeSet::new();
    let now = 1_800_000_000;
    assert_eq!(
        accounts::renew_next(&roster, &state, &enrolled, None, now),
        Some("work".into())
    );
    assert_eq!(
        accounts::sign_in_next(&roster, &state, &enrolled, None, now),
        Some(("work".into(), true))
    );
    state.prepared_accounts.insert("work".into());
    assert_eq!(
        accounts::renew_next(&roster, &state, &enrolled, None, now),
        None
    );
    assert_eq!(
        accounts::sign_in_next(&roster, &state, &enrolled, None, now),
        None
    );
}

#[test]
fn a_non_home_warm_grant_wakes_a_sync_to_release_prepared_execution() {
    let now = 1_800_000_000;
    for (cli, provider) in [
        (Cli::Claude, "claude"),
        (Cli::Codex, "codex"),
        (Cli::Kimi, "kimi:global"),
    ] {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let config_dir = root.path().join("config");
        let state_dir = root.path().join("state");
        fs::create_dir_all(&home).unwrap();
        aterm_types::fs_restricted::ensure_private_dir(&home.join(".secrets")).unwrap();
        fs::create_dir_all(&config_dir).unwrap();
        let config = config_dir.join("aterm.toml");
        crate::test_config::write(&config, "[fleet]\nmachine='m16'\n").unwrap();
        let source = format!(
            "version=1\n[[account]]\nprovider='{provider}'\nemail='owner@example.test'\n\
             warm=['m1','m16']\nsince=['2026-10-30T00:00:00Z','2026-10-30T00:00:00Z']\n\
             why=['pin','pin']\nused=['2026-10-30','2026-10-30']\n"
        );
        fs::write(
            home.join(".secrets")
                .join(super::super::ai_pool::AI_POOL_FILE),
            &source,
        )
        .unwrap();
        let pool = super::super::ai_pool::Pool::from_toml(&source, "fixture").unwrap();
        let identity =
            super::super::ai_pool::resolve_local_pool_identity(Some(&home), Some(&config), None)
                .unwrap()
                .unwrap();
        let ctx = PoolSyncCtx { pool, identity };
        let row = Row {
            label: "prepared".into(),
            email: Some("owner@example.test".into()),
            dir: Some(root.path().join("login").to_str().unwrap().into()),
            vendor: accounts::Vendor {
                region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
                flow: None,
            },
            ..Default::default()
        };
        assert!(ctx.holder_for_row(cli, &row).is_none());
        assert!(!ctx.is_home_for_row(cli, &row));
        let roster = accounts::roster_beside_for(&config, cli).unwrap();
        fs::write(
            &roster,
            accounts::append_rows_for(cli, "", std::slice::from_ref(&row)).unwrap(),
        )
        .unwrap();
        if cli == Cli::Claude {
            fs::write(
                fleet_file_beside(&roster).unwrap(),
                project_pool_fleet(&ctx.pool, cli, Some(&ctx.identity))
                    .to_toml_for(Some(OptIn::Named)),
            )
            .unwrap();
        }
        let store = accounts::Store::at_for(&state_dir, cli, &home);
        let mut state = accounts::State {
            swarm_label: Some(ctx.identity.primary.clone()),
            swarm_aliases: ctx.identity.aliases.clone(),
            ..Default::default()
        };
        let due = || {
            if cli == Cli::Claude {
                init_sync_needed(&home, &state_dir, &roster, now, false)
            } else {
                // The combined probe may also owe Claude's empty-pool setup. This control
                // checks the actual prepared grant's contribution for this CLI.
                matches!(read_resync(&home, &state_dir, &config_dir.join(accounts::ACCOUNTS_FILE), now), ResyncRead::Due(why) if why.contains(cli.id()))
            }
        };
        store.save(&state).unwrap();
        if cli == Cli::Claude {
            let held = store.acquire().unwrap();
            store.retire_source_held(&held, now).unwrap();
        }
        assert!(!due(), "settled control {cli:?}");
        state.prepared_accounts.insert(row.label.clone());
        store.save(&state).unwrap();
        assert!(due(), "the execution grant must be applied {cli:?}");
        assert!(apply_pool_execution(
            &mut state,
            std::slice::from_ref(&row),
            cli,
            &ctx
        ));
        store.save(&state).unwrap();
        assert!(!due(), "released control {cli:?}");
    }
}

#[test]
fn preparation_only_labels_cannot_be_reused_or_overwritten_by_a_move() {
    let now = 1_800_000_000;
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(root.path());
    let mut state = accounts::State::default();
    state.prepared_accounts.insert("reserved".into());
    state
        .accounts
        .insert("source".into(), accounts::Account::default());
    store.save(&state).unwrap();
    {
        let _held = store.acquire().unwrap();
        assert!(store.check_new_labels(&["reserved"], now).is_err());
        assert!(store.check_new_labels(&["unused"], now).is_ok());
    }
    let mut logins = accounts::Logins::default();
    let before = state.clone();
    assert!(accounts::move_account(&mut state, &mut logins, "source", "reserved").is_err());
    assert_eq!(state, before);
    accounts::move_account(&mut state, &mut logins, "source", "unused").unwrap();
    assert!(state.accounts.contains_key("unused"));
}

#[test]
fn a_preparation_hold_cannot_be_pinned_or_queued_for_login() {
    let now = 1_800_000_000;
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(root.path());
    let row = Row {
        label: "held".into(),
        email: Some("owner@example.test".into()),
        dir: Some(root.path().join("login").to_str().unwrap().into()),
        ..Default::default()
    };
    let roster = Roster { rows: vec![row] };
    let path = root.path().join("accounts.toml");
    fs::write(
        &path,
        accounts::append_rows_for(Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
    store
        .transact(now, &mut None, |state| {
            state.prepared_accounts.insert("held".into());
            Ok(Vec::new())
        })
        .unwrap();
    assert!(
        store
            .select_account_for(Cli::Claude, &roster, &path, "held", now)
            .is_err()
    );
    let state = store.load(now).unwrap().0;
    assert!(state.pending_pin.is_none());
    assert!(!state.pinned);
    store
        .transact(now, &mut None, |state| {
            state.prepared_accounts.remove("held");
            Ok(Vec::new())
        })
        .unwrap();
    assert!(matches!(
        store
            .select_account_for(Cli::Claude, &roster, &path, "held", now)
            .unwrap(),
        accounts::PinSelection::Renew(_)
    ));
}

#[test]
fn only_an_explicit_warm_pool_assignment_releases_prepared_execution() {
    for (cli, provider) in [
        (Cli::Claude, "claude"),
        (Cli::Codex, "codex"),
        (Cli::Kimi, "kimi:global"),
    ] {
        let row = Row {
            label: "prepared".into(),
            email: Some("owner@example.test".into()),
            vendor: accounts::Vendor {
                region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
                flow: None,
            },
            ..Default::default()
        };
        let mut state = accounts::State::default();
        state.prepared_accounts.insert(row.label.clone());
        assert_eq!(
            state.lease_verdict(&row.label, 1),
            Err(accounts::WhyNot::PreparationOnly)
        );
        let pool = |warm: &str| {
            PoolSyncCtx {
            pool: super::super::ai_pool::Pool::from_toml(&format!(
                "version=1\n[[account]]\nprovider='{provider}'\nemail='owner@example.test'\nwarm=['{warm}']\nsince=['2026-10-30T00:00:00Z']\nwhy=['pin']\nused=['2026-10-30']\n"), "fixture").unwrap(),
            identity: super::super::ai_pool::resolve_pool_identity_pure(&[], None, Some("m1"), None, &[]).unwrap(),
        }
        };
        assert!(!apply_pool_execution(
            &mut state,
            std::slice::from_ref(&row),
            cli,
            &pool("m2")
        ));
        assert_eq!(
            state.lease_verdict(&row.label, 1),
            Err(accounts::WhyNot::PreparationOnly)
        );
        assert!(
            apply_pool_execution(&mut state, std::slice::from_ref(&row), cli, &pool("m1")),
            "{cli:?}"
        );
        assert!(state.lease_verdict(&row.label, 1).is_ok());
    }
}

#[test]
fn legacy_source_sync_preserves_only_owned_shared_rows_without_rewriting_conflicts() {
    let row = Row {
        label: "shared-one".into(),
        email: Some("one@example.test".into()),
        dir: Some(crate::harness::host_path::abs("/private/shared-one")),
        ..Default::default()
    };
    let existing = Roster {
        rows: vec![row.clone()],
    };
    let mut logins = accounts::Logins::default();
    let mut next = Roster::default();
    retain_shared_rows(&mut next, &existing, &logins, false).unwrap();
    assert!(
        next.rows.is_empty(),
        "ordinary source removal remains effective"
    );
    logins
        .shared
        .begin(
            accounts::shared::Intent {
                source: accounts::shared::Source {
                    network: [1; 32],
                    account: [2; 32],
                },
                decision: [3; 32],
                generation: 1,
                configuration: [4; 32],
                request: [5; 16],
                label: row.label.clone(),
                identity: row.email.clone().unwrap(),
                staging: crate::harness::host_path::abs("/private/staging"),
                before_generation: None,
                begun_at: 1,
            },
            None,
        )
        .unwrap();
    retain_shared_rows(&mut next, &existing, &logins, false).unwrap();
    assert_eq!(next.rows, existing.rows);
    let mut changed = row.clone();
    changed.email = Some("other@example.test".into());
    let mut conflict = Roster {
        rows: vec![changed.clone()],
    };
    assert!(retain_shared_rows(&mut conflict, &existing, &logins, false).is_err());
    assert_eq!(conflict.rows, vec![changed]);
    let mut held = row.clone();
    held.held_by = Some("m2".into());
    let mut owned_elsewhere = Roster {
        rows: vec![held.clone()],
    };
    retain_shared_rows(&mut owned_elsewhere, &existing, &logins, false).unwrap();
    assert_eq!(owned_elsewhere.rows, vec![held]);
    // A sheet's class is part of the retained configuration; the lab's derived class, which
    // only orders the roster, is not (NEGATIVE CONTROL: without the `class_derived` arm the
    // lab's reclassification is refused as a change to the retained account).
    let mut reclassed = row.clone();
    reclassed.class = match row.class {
        accounts::Class::Preferred => accounts::Class::Pool,
        accounts::Class::Pool => accounts::Class::Preferred,
    };
    let mut sheet = Roster {
        rows: vec![reclassed.clone()],
    };
    assert!(retain_shared_rows(&mut sheet, &existing, &logins, false).is_err());
    let mut lab = Roster {
        rows: vec![reclassed.clone()],
    };
    retain_shared_rows(&mut lab, &existing, &logins, true).unwrap();
    assert_eq!(lab.rows, vec![reclassed]);
}
