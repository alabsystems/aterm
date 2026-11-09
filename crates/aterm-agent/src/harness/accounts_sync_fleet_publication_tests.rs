// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn first_ordinary_sync_publishes_into_a_missing_configuration_tree() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(&root.path().join("state"));
    let roster = root.path().join("absent/config/accounts.toml");
    let fleet = parse_accounting_csv(
        "email,computer,claude\none@example.test,m8,1\n",
        "source.csv",
    )
    .unwrap();
    assert!(!roster.parent().unwrap().exists());
    let synced = persist_accounts_sync(
        &store,
        &roster,
        (&fleet, OptIn::Named),
        ("m8", Some("one@example.test")),
        &BTreeMap::new(),
        1_790_000_000,
        (&[], &[]),
    )
    .unwrap();
    assert!(synced.changed);
    assert!(
        Roster::load(&roster)
            .unwrap()
            .rows
            .iter()
            .any(|row| row.email.as_deref() == Some("one@example.test"))
    );
    assert_eq!(
        fs::read_to_string(synced.fleet_path).unwrap(),
        fleet.to_toml_for(Some(OptIn::Named))
    );
    assert!(store.load(1_790_000_000).is_ok());
    assert!(!store.recover_sync_within(&[], &roster).unwrap());
}

/// The ordinary caller must pass its fleet image to the receipt, not an auxiliary write.
/// With the former callback publication, the fleet changes before the cut-zero refusal.
#[test]
fn ordinary_sync_keeps_the_fleet_before_replay_and_recovers_it_with_the_roster() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(&root.path().join("state"));
    let roster = root.path().join("config/accounts.toml");
    fs::create_dir_all(roster.parent().unwrap()).unwrap();
    fs::write(
        &roster,
        "[[account]]\nlabel = \"m8\"\nclass = \"dedicated\"\nemail = \"one@example.test\"\n",
    )
    .unwrap();
    let mut fleet = parse_accounting_csv(
        "email,computer,claude\none@example.test,m8,1\n",
        "source.csv",
    )
    .unwrap();
    let walls = BTreeMap::new();
    let now = 1_790_000_000;
    let first = persist_accounts_sync(
        &store,
        &roster,
        (&fleet, OptIn::Named),
        ("m8", Some("one@example.test")),
        &walls,
        now,
        (&[], &[]),
    )
    .unwrap();
    let paths = [
        roster.clone(),
        first.fleet_path,
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
    ];
    let images = || {
        paths
            .iter()
            .map(|path| fs::read(path).ok())
            .collect::<Vec<_>>()
    };
    let before = images();
    fleet.accounts[0].browser = Some("changed profile".into());
    accounts::SYNC_COMMIT_CUT.with(|cut| cut.set(Some(0)));
    let interrupted = persist_accounts_sync(
        &store,
        &roster,
        (&fleet, OptIn::Named),
        ("m8", Some("one@example.test")),
        &walls,
        now,
        (&[], &[]),
    );
    accounts::SYNC_COMMIT_CUT.with(|cut| cut.set(None));
    assert!(
        interrupted
            .err()
            .expect("sync must stop at cut zero")
            .contains("injected interruption")
    );
    assert_eq!(
        images(),
        before,
        "cut zero must precede every image publication"
    );
    assert!(
        store.load(now).is_err(),
        "the pending receipt fences readers"
    );
    assert!(store.recover_sync_within(&[], &roster).unwrap());
    assert_eq!(
        fs::read_to_string(&paths[1]).unwrap(),
        fleet.to_toml_for(Some(OptIn::Named))
    );
    assert!(store.load(now).is_ok());
    assert!(!store.recover_sync_within(&[], &roster).unwrap());
}
