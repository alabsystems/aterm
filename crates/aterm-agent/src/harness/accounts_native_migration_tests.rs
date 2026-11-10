// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn native_alias_default_observation_admits_the_completed_canonical_identity() {
    let bed = Bed::new(Some("canonical"));
    let old = Row {
        label: "legacy".into(),
        email: Some(email("alias")),
        ..Row::default()
    };
    bed.seed(std::slice::from_ref(&old));
    let before = bed.store.load(NOW).unwrap().0;
    assert_eq!(
        before.accounts["legacy"].current_auth(&old),
        accounts::Auth::Ok
    );
    let lab = lab_of(
        true,
        vec![LabAccount {
            legacy_label: Some("legacy".into()),
            identity: Some(email("canonical")),
            ..lab_account("alias", "claude-canonical", &[("m22", Some(1))])
        }],
    );
    bed.sync(&lab).unwrap();
    let roster = bed.roster();
    let current = roster.row("claude-canonical").unwrap();
    assert_eq!(current.dir, None);
    assert_eq!(
        current.expected_identity_email(),
        Some(email("canonical").as_str())
    );
    let state = bed.store.load(NOW).unwrap().0;
    assert_eq!(
        state.accounts["claude-canonical"].current_auth(current),
        accounts::Auth::Ok
    );
    assert_eq!(
        state.accounts["claude-canonical"].ran_at,
        before.accounts["legacy"].ran_at
    );
    bed.sync(&lab).unwrap();
    assert_eq!(
        bed.store.load(NOW).unwrap().0.accounts["claude-canonical"].current_auth(current),
        accounts::Auth::Ok
    );
}

#[path = "accounts_identity_completion_tests.rs"]
mod completion;

#[test]
fn native_alias_migration_keeps_canonical_identity_records_and_directory() {
    let bed = Bed::new(None);
    let login = bed.login("legacy");
    bed.seed(&[Row {
        label: "legacy".into(),
        dir: Some(login.clone()),
        email: Some(email("canonical")),
        ..Row::default()
    }]);
    let lab = lab_of(
        true,
        vec![LabAccount {
            legacy_label: Some("legacy".into()),
            identity: Some(email("canonical")),
            ..lab_account("alias", "claude-canonical", &[("m22", Some(1))])
        }],
    );
    bed.sync(&lab).unwrap();
    let roster = bed.roster();
    let row = roster.row("claude-canonical").unwrap();
    assert_eq!(row.email.as_deref(), Some(email("alias").as_str()));
    assert_eq!(
        row.expected_identity_email(),
        Some(email("canonical").as_str())
    );
    assert_eq!(row.dir.as_deref(), Some(login.as_str()));
    assert!(roster.row("legacy").is_none());
    let (state, _) = bed.store.load(NOW).unwrap();
    assert_eq!(state.accounts["claude-canonical"].auth, accounts::Auth::Ok);
    assert!(!state.accounts.contains_key("legacy"));
}

#[test]
fn native_alias_identity_completion_keeps_usage_but_retires_old_auth() {
    let bed = Bed::new(None);
    let login = bed.login("legacy");
    let old = Row {
        label: "legacy".into(),
        dir: Some(login.clone()),
        email: Some(email("alias")),
        ..Row::default()
    };
    bed.seed(std::slice::from_ref(&old));
    bed.store
        .transact(NOW, &mut None, |state| {
            Ok(accounts::note_wall(
                state,
                "legacy",
                "seven_day",
                Some(NOW + 3600),
                NOW,
                NOW,
            ))
        })
        .unwrap();
    let (before, _) = bed.store.load(NOW).unwrap();
    assert_eq!(
        before.accounts["legacy"].current_auth(&old),
        accounts::Auth::Ok
    );
    let lab = lab_of(
        true,
        vec![LabAccount {
            legacy_label: Some("legacy".into()),
            identity: Some(email("canonical")),
            ..lab_account("alias", "claude-canonical", &[("m22", Some(1))])
        }],
    );
    bed.sync(&lab).unwrap();
    let roster = bed.roster();
    let row = roster.row("claude-canonical").unwrap();
    assert_eq!(row.email.as_deref(), Some(email("alias").as_str()));
    assert_eq!(
        row.identity_email.as_deref(),
        Some(email("canonical").as_str())
    );
    assert_eq!(row.dir.as_deref(), Some(login.as_str()));
    assert_eq!(
        fs::read_to_string(Path::new(&login).join("login")).unwrap(),
        "legacy"
    );
    let (state, _) = bed.store.load(NOW).unwrap();
    let account = &state.accounts["claude-canonical"];
    assert_eq!(account.ran_at, before.accounts["legacy"].ran_at);
    assert_eq!(account.walls, before.accounts["legacy"].walls);
    assert_eq!(account.current_auth(row), accounts::Auth::Unseen);
    assert!(!state.accounts.contains_key("legacy"));
}

#[test]
fn known_native_alias_default_collision_refuses_without_publication() {
    let bed = Bed::new(Some("canonical"));
    let login = bed.login("occupied");
    bed.seed(&[Row {
        label: "occupied".into(),
        dir: Some(login),
        email: Some(email("stranger")),
        ..Row::default()
    }]);
    let paths = [
        bed.roster.clone(),
        bed.store.state_file(),
        bed.store.logins_file(),
        bed.store.ledger_path(),
    ];
    let image = || {
        paths
            .iter()
            .map(|path| fs::read(path).ok())
            .collect::<Vec<_>>()
    };
    let before = image();
    let lab = lab_of(
        true,
        vec![LabAccount {
            identity: Some(email("canonical")),
            ..lab_account("alias", "occupied", &[("m20", Some(1))])
        }],
    );
    let error = bed
        .sync(&lab)
        .err()
        .expect("a known default needs a representable row");
    assert!(
        error.contains("cannot represent the known default login"),
        "{error}"
    );
    assert_eq!(image(), before);
}

#[test]
fn native_migration_and_default_displacement_move_the_same_session() {
    let bed = Bed::new(Some("new"));
    bed.seed(&[Row {
        label: "legacy".into(),
        email: Some(email("old")),
        ..Row::default()
    }]);
    bed.store
        .transact(NOW - 1, &mut None, |state| {
            Ok(accounts::note_session(
                state,
                "default-session",
                "legacy",
                None,
                NOW - 1,
            ))
        })
        .unwrap();
    let lab = lab_of(
        true,
        vec![
            LabAccount {
                legacy_label: Some("legacy".into()),
                ..lab_account("old", "claude-old", &[("m22", Some(1))])
            },
            lab_account("new", "claude-new", &[("m22", Some(2))]),
        ],
    );
    bed.sync(&lab).unwrap();
    let roster = bed.roster();
    assert_eq!(roster.row_for_login(None).unwrap().label, "claude-new");
    assert!(roster.row("claude-old").unwrap().dir.is_some());
    let (state, _) = bed.store.load(NOW).unwrap();
    assert_eq!(state.sessions["default-session"].account, "claude-new");
    assert_eq!(state.accounts["claude-old"].auth, accounts::Auth::SignedOut);
}

#[test]
fn native_migration_keeps_quota_walls_and_reports_hold_and_order_changes() {
    let bed = Bed::new(None);
    let login = bed.login("legacy");
    bed.seed(&[Row {
        label: "legacy".into(),
        email: Some(email("bob")),
        dir: Some(login),
        held_by: Some("m20".into()),
        class: Class::Pool,
        ..Row::default()
    }]);
    let lab = lab_of(
        true,
        vec![LabAccount {
            legacy_label: Some("legacy".into()),
            ..lab_account("bob", "claude-bob", &[("m22", Some(1))])
        }],
    );
    let walls = BTreeMap::from([(
        "legacy".to_owned(),
        upgrade::StandingWall {
            at: u64::try_from(NOW).unwrap(),
            until: Some(u64::try_from(NOW + 3600).unwrap()),
            window: Some("seven_day".into()),
            credits: None,
        },
    )]);
    let result = alab_roster::with_lab(lab, || {
        let admission = AutomaticSource::capture_requested(bed.env.config.as_ref().unwrap())?
            .with_policy(bed.store.source_policy()?, false);
        let fleet = admission
            .chosen(Cli::Claude, &bed.roster, None)?
            .ok_or("the native test source is missing")?;
        persist_accounts_sync_with_pool(
            (&bed.store, &SourceProtection::default()),
            &bed.roster,
            (&fleet, OptIn::Named, None),
            (fleet.alab.as_ref().unwrap().key.as_str(), None),
            &walls,
            NOW,
            (&[], &[], Some(&admission)),
        )
    })
    .unwrap();
    let (state, _) = bed.store.load(NOW).unwrap();
    assert_eq!(
        state.accounts["claude-bob"].walls["seven_day"].until,
        Some(NOW + 3600)
    );
    assert!(
        result
            .rows_changed
            .holds
            .contains(&("claude-bob".into(), Some("m20".into()), None))
    );
    assert!(result.rows_changed.orders.contains(&(
        "claude-bob".into(),
        Class::Pool,
        Class::Preferred
    )));
}

#[test]
fn native_migration_cannot_rename_a_shared_custody_row() {
    let bed = Bed::new(None);
    let login = bed.login("legacy");
    bed.seed(&[Row {
        label: "legacy".into(),
        email: Some(email("bob")),
        dir: Some(login),
        ..Row::default()
    }]);
    let (mut logins, _) = bed.store.load_logins(NOW).unwrap();
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
                label: "legacy".into(),
                identity: email("bob"),
                staging: bed._root.path().join("staging").to_str().unwrap().into(),
                before_generation: None,
                begun_at: NOW,
            },
            None,
        )
        .unwrap();
    bed.store.save_logins(&logins).unwrap();
    let paths = [
        bed.roster.clone(),
        bed.store.state_file(),
        bed.store.logins_file(),
        bed.store.ledger_path(),
    ];
    let image = || {
        paths
            .iter()
            .map(|path| fs::read(path).ok())
            .collect::<Vec<_>>()
    };
    let before = image();
    let lab = lab_of(
        true,
        vec![LabAccount {
            legacy_label: Some("legacy".into()),
            ..lab_account("bob", "claude-bob", &[("m22", Some(1))])
        }],
    );
    let error = bed
        .sync(&lab)
        .err()
        .expect("shared custody must preserve its pinned configuration");
    assert!(error.contains("shared"), "{error}");
    assert_eq!(image(), before);
}
