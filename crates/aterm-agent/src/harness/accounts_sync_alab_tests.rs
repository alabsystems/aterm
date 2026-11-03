// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code's sync from the lab's preferences on a scratch store; every address is synthetic.

use super::*;
use crate::harness::accounts::rename_tests::{NOW, seed_reference};
use crate::harness::alab_roster::{self, Lab, LabAccount};

fn email(local: &str) -> String {
    format!("{local}@example.test")
}

fn lab_account(local: &str, label: &str, preferred: &[(&str, Option<u64>)]) -> LabAccount {
    LabAccount {
        pk: format!("ac-{:0>16}", local.len()),
        cli: Cli::Claude,
        vendor: accounts::Vendor::default(),
        label: label.to_owned(),
        legacy_label: None,
        email: email(local),
        identity: None,
        browser: None,
        joinable: true,
        preferred: preferred
            .iter()
            .map(|(machine, rank)| ((*machine).to_owned(), *rank))
            .collect(),
    }
}

/// A sync from the lab adds the accounts this Mac prefers and keeps every row it had: the
/// default login's, one another Mac prefers, and one the lab does not know, each with its login
/// directory and its sign-in word. It writes no fleet file beside the roster, since the lab is
/// the source, and a repeat changes nothing.
/// A kept row that names no address (`old`) keeps its login binding too.
/// NEGATIVE CONTROL: with `persist_accounts_sync_with_pool`'s `alab_roster::retain` call removed,
/// the rebuild drops `kept` and `m20-m22` (`kept is a row`); with the departed filter removed,
/// `old` is treated as departed and loses its binding's directory (`keeps its login binding`).
#[test]
fn a_sync_from_the_lab_adds_preferred_accounts_and_drops_no_row() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = accounts::Store::at(&root.path().join("state"));
    let roster_path = root.path().join("config").join(accounts::ACCOUNTS_FILE);
    fs::create_dir_all(roster_path.parent().unwrap()).unwrap();
    let dir = |label: &str| {
        let dir = store.logins_dir().join(label);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("login"), label).unwrap();
        Some(dir.to_str().unwrap().to_owned())
    };
    let row = |label: &str, local: &str, dir: Option<String>| Row {
        label: label.into(),
        dir,
        email: Some(email(local)),
        ..Row::default()
    };
    let existing = Roster {
        rows: vec![
            row("m22", "ann", None),
            row("m20-m22", "cy", dir("m20-m22")),
            row("kept", "dee", dir("kept")),
            Row {
                label: "old".into(),
                dir: dir("old"),
                ..Row::default()
            },
        ],
    };
    fs::write(&roster_path, format_roster_toml(&existing).unwrap()).unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            for row in &existing.rows {
                seed_reference(state, logins, &row.label, 3);
            }
            logins.labels.insert(
                "old".into(),
                accounts::Binding {
                    dir: existing.row("old").and_then(|row| row.dir.clone()),
                    ..accounts::Binding::default()
                },
            );
            Ok(Vec::new())
        })
        .unwrap();
    let lab = Lab {
        me: Some("m22".into()),
        settled: false,
        accounts: vec![
            lab_account("ann", "claude-aaaaaa", &[("m22", Some(1))]),
            lab_account("cy", "claude-cccccc", &[("m20", Some(1))]),
            lab_account("gil", "claude-gggggg", &[("m22", Some(2))]),
        ],
        aliases: BTreeMap::new(),
    };
    let sync = || {
        alab_roster::with_lab(lab.clone(), || {
            let fleet = alab_roster::fleet_for(&lab, Cli::Claude, Some(&existing)).unwrap();
            persist_accounts_sync(
                &store,
                &roster_path,
                (&fleet, OptIn::Named),
                ("m22", Some(&email("ann"))),
                &BTreeMap::new(),
                NOW,
                (&[], &[]),
            )
        })
    };
    sync().unwrap();
    let roster = Roster::load(&roster_path).unwrap();
    for (label, dir) in [
        ("m22", None),
        ("m20-m22", existing.row("m20-m22").unwrap().dir.clone()),
        ("kept", existing.row("kept").unwrap().dir.clone()),
    ] {
        let row = roster
            .row(label)
            .unwrap_or_else(|| panic!("{label} is a row"));
        assert_eq!(row.dir, dir, "{label} keeps its login directory");
    }
    let added = roster
        .row("claude-gggggg")
        .expect("the preferred account joins");
    assert_eq!(
        added.dir.as_deref(),
        store.login_dir("claude-gggggg").unwrap().to_str()
    );
    assert_eq!(roster.row("m22").unwrap().class, Class::Dedicated);
    assert_eq!(
        roster.row("m20-m22").unwrap().class,
        Class::Shared,
        "preferred elsewhere: tried after this Mac's own"
    );
    let (state, _) = store.load(NOW).unwrap();
    for label in ["m22", "m20-m22", "kept", "old"] {
        assert_eq!(
            state.accounts.get(label).map(|a| a.auth),
            Some(accounts::Auth::Ok),
            "{label} is still signed in"
        );
    }
    let (logins, _) = store.load_logins(NOW).unwrap();
    assert!(
        logins.labels.get("old").is_some_and(|b| b.dir.is_some()),
        "a kept row that names no address keeps its login binding"
    );
    assert!(
        !roster_path.with_file_name(FLEET_ROSTER_FILE).exists(),
        "the lab is the source: no fleet file is written beside the roster"
    );
    let text = fs::read(&roster_path).unwrap();
    sync().unwrap();
    assert_eq!(
        fs::read(&roster_path).unwrap(),
        text,
        "a repeat changes nothing"
    );
}

/// A CLI pool's sync with a settled lab renames an old-style row (`kimi-m16`) to its account's
/// label before it reads the fleet, keeping the row's login directory and its records, and
/// signs nobody out; with the lab unsettled the row keeps its label. The `held_by` an old
/// `ai-pool.toml` left on the row is cleared by the first sync from the lab.
/// NEGATIVE CONTROL: with `sync_cli_with_replan`'s `migrate_before_sync` call removed, the
/// settled sync leaves `kimi-m16` (`renamed before the fleet is read`); with its `alab` arm that
/// clears `held_by` removed, the hold stays (`left: Some("m3") right: None`).
#[test]
fn a_cli_sync_from_a_settled_lab_migrates_the_label_and_keeps_the_login() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let (home, state_dir, config) = (
        root.path().join("home"),
        root.path().join("state"),
        root.path().join("config"),
    );
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&config).unwrap();
    let toml = config.join("aterm.toml");
    fs::write(&toml, "").unwrap();
    let cli = Cli::Kimi;
    let roster_path = accounts::roster_beside_for(&toml, cli).unwrap();
    let store = accounts::Store::at_for(&state_dir, cli, &home);
    let login = store.logins_dir().join("kimi-m16");
    fs::create_dir_all(&login).unwrap();
    fs::write(login.join("login"), "kimi-m16").unwrap();
    let old = Roster {
        rows: vec![Row {
            label: "kimi-m16".into(),
            dir: Some(login.to_str().unwrap().to_owned()),
            email: Some(email("eve")),
            vendor: accounts::Vendor {
                region: Some(crate::ai::connector::Region::Global),
                ..accounts::Vendor::default()
            },
            held_by: Some("m3".into()),
            ..Row::default()
        }],
    };
    fs::write(
        &roster_path,
        accounts::append_rows_for(cli, "", &old.rows).unwrap(),
    )
    .unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            seed_reference(state, logins, "kimi-m16", 3);
            Ok(Vec::new())
        })
        .unwrap();
    let lab = |settled: bool| Lab {
        me: Some("m16".into()),
        settled,
        accounts: vec![LabAccount {
            cli,
            vendor: accounts::Vendor {
                region: Some(crate::ai::connector::Region::Global),
                ..accounts::Vendor::default()
            },
            legacy_label: Some("kimi-m16".into()),
            ..lab_account("eve", "kimi-eeeeee", &[("m16", Some(1))])
        }],
        aliases: BTreeMap::new(),
    };
    let fetch = |_: &Env, _: &str| -> Result<String, String> { panic!("the lab is the source") };
    let sync = |settled: bool| {
        alab_roster::with_lab(lab(settled), || {
            sync_cli_with_replan(
                cli,
                (&home, &state_dir, &toml),
                (None, None),
                NOW,
                (&fetch, &[]),
                || {},
            )
        })
    };
    sync(false).unwrap();
    let roster = Roster::load_for(cli, &roster_path).unwrap();
    let unsettled = roster
        .row("kimi-m16")
        .expect("an unsettled lab renames nothing");
    assert_eq!(
        unsettled.held_by, None,
        "an old pool's hold does not outlive its source"
    );
    sync(true).unwrap();
    let roster = Roster::load_for(cli, &roster_path).unwrap();
    assert!(
        roster.row("kimi-m16").is_none(),
        "renamed before the fleet is read"
    );
    let row = roster.row("kimi-eeeeee").expect("the account's label");
    assert_eq!(
        row.dir.as_deref(),
        login.to_str(),
        "the login stays where it was"
    );
    assert_eq!(fs::read(login.join("login")).unwrap(), b"kimi-m16");
    let (state, _) = store.load(NOW).unwrap();
    assert_eq!(
        state.accounts.get("kimi-eeeeee").map(|a| a.auth),
        Some(accounts::Auth::Ok),
        "still signed in, under its new label"
    );
    let text = fs::read(&roster_path).unwrap();
    sync(true).unwrap();
    assert_eq!(
        fs::read(&roster_path).unwrap(),
        text,
        "a repeat changes nothing"
    );
}
