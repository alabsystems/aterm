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
    let lab = lab_of(vec![LabAccount {
        legacy_label: Some("legacy".into()),
        identity: Some(email("canonical")),
        ..lab_account("alias", "claude-canonical", &[("m22", Some(1))])
    }]);
    bed.sync(&lab).unwrap();
    let roster = bed.roster();
    let current = roster.row("legacy").unwrap();
    assert!(
        roster.row("claude-canonical").is_none(),
        "no sync renames a row"
    );
    assert_eq!(current.dir, None);
    assert_eq!(
        current.expected_identity_email(),
        Some(email("canonical").as_str())
    );
    let state = bed.store.load(NOW).unwrap().0;
    assert_eq!(
        state.accounts["legacy"].current_auth(current),
        accounts::Auth::Ok
    );
    assert_eq!(
        state.accounts["legacy"].ran_at,
        before.accounts["legacy"].ran_at
    );
    bed.sync(&lab).unwrap();
    assert_eq!(
        bed.store.load(NOW).unwrap().0.accounts["legacy"].current_auth(current),
        accounts::Auth::Ok
    );
}

#[path = "accounts_identity_completion_tests.rs"]
mod completion;

#[test]
fn native_alias_completion_keeps_the_label_records_and_directory() {
    let bed = Bed::new(None);
    let login = bed.login("legacy");
    bed.seed(&[Row {
        label: "legacy".into(),
        dir: Some(login.clone()),
        email: Some(email("canonical")),
        ..Row::default()
    }]);
    let lab = lab_of(vec![LabAccount {
        legacy_label: Some("legacy".into()),
        identity: Some(email("canonical")),
        ..lab_account("alias", "claude-canonical", &[("m22", Some(1))])
    }]);
    bed.sync(&lab).unwrap();
    let roster = bed.roster();
    let row = roster.row("legacy").unwrap();
    assert_eq!(row.email.as_deref(), Some(email("alias").as_str()));
    assert_eq!(
        row.expected_identity_email(),
        Some(email("canonical").as_str())
    );
    assert_eq!(row.dir.as_deref(), Some(login.as_str()));
    assert!(roster.row("claude-canonical").is_none());
    let (state, _) = bed.store.load(NOW).unwrap();
    assert_eq!(state.accounts["legacy"].auth, accounts::Auth::Ok);
    assert!(!state.accounts.contains_key("claude-canonical"));
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
    let lab = lab_of(vec![LabAccount {
        legacy_label: Some("legacy".into()),
        identity: Some(email("canonical")),
        ..lab_account("alias", "claude-canonical", &[("m22", Some(1))])
    }]);
    bed.sync(&lab).unwrap();
    let roster = bed.roster();
    let row = roster.row("legacy").unwrap();
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
    let account = &state.accounts["legacy"];
    assert_eq!(account.ran_at, before.accounts["legacy"].ran_at);
    assert_eq!(account.walls, before.accounts["legacy"].walls);
    assert_eq!(account.current_auth(row), accounts::Auth::Unseen);
    assert!(!state.accounts.contains_key("claude-canonical"));
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
    let lab = lab_of(vec![LabAccount {
        identity: Some(email("canonical")),
        ..lab_account("alias", "occupied", &[("m20", Some(1))])
    }]);
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
fn default_displacement_moves_the_default_session_off_a_legacy_row() {
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
    let lab = lab_of(vec![
        LabAccount {
            legacy_label: Some("legacy".into()),
            ..lab_account("old", "claude-old", &[("m22", Some(1))])
        },
        lab_account("new", "claude-new", &[("m22", Some(2))]),
    ]);
    bed.sync(&lab).unwrap();
    let roster = bed.roster();
    assert_eq!(roster.row_for_login(None).unwrap().label, "claude-new");
    assert!(roster.row("legacy").unwrap().dir.is_some());
    assert!(roster.row("claude-old").is_none(), "no sync renames a row");
    let (state, _) = bed.store.load(NOW).unwrap();
    assert_eq!(state.sessions["default-session"].account, "claude-new");
    assert_eq!(state.accounts["legacy"].auth, accounts::Auth::SignedOut);
}

#[test]
fn a_native_sync_keeps_quota_walls_and_reports_hold_and_order_changes() {
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
    let lab = lab_of(vec![LabAccount {
        legacy_label: Some("legacy".into()),
        ..lab_account("bob", "claude-bob", &[("m22", Some(1))])
    }]);
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
        state.accounts["legacy"].walls["seven_day"].until,
        Some(NOW + 3600)
    );
    assert!(
        result
            .rows_changed
            .holds
            .contains(&("legacy".into(), Some("m20".into()), None))
    );
    assert!(
        result
            .rows_changed
            .orders
            .contains(&("legacy".into(), Class::Pool, Class::Preferred))
    );
}

/// A native sync never rewrites a row in shared custody, not even with the provider identity it
/// completes in memory: the lab knows the row's account by an identity the row does not record,
/// the composed roster agrees with the completed row, and the original roster refuses it, so
/// nothing is written.
/// NEGATIVE CONTROL: with `persist_accounts_sync_with_pool`'s `retain_shared_rows` against the
/// original roster skipped, the sync is not refused for the shared row (the refusal assertion
/// fails).
#[test]
fn a_native_sync_cannot_complete_the_identity_of_a_shared_custody_row() {
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
    let lab = lab_of(vec![LabAccount {
        legacy_label: Some("legacy".into()),
        identity: Some(email("canonical")),
        ..lab_account("bob", "claude-bob", &[("m22", Some(1))])
    }]);
    let error = bed
        .sync(&lab)
        .err()
        .expect("shared custody must preserve its pinned configuration");
    assert!(
        error.contains("source would change a locally retained shared account"),
        "{error}"
    );
    assert_eq!(image(), before);
}
