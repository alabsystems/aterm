// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code's sync from the lab's preferences on a scratch store; every address is synthetic.

use super::*;
use crate::harness::accounts::rename_tests::{NOW, seed_reference};
use crate::harness::alab_roster::{self, Lab, LabAccount};

fn email(local: &str) -> String {
    format!("{local}@example.com")
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
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Default::default(),
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
    assert_eq!(roster.row("m22").unwrap().class, Class::Preferred);
    assert_eq!(
        roster.row("m20-m22").unwrap().class,
        Class::Pool,
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
    crate::test_config::write(&toml, "").unwrap();
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
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Default::default(),
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

/// `accounts sync --cli kimi alab` on a Mac the lab names that prefers no Kimi account is no
/// error: it adds nothing, keeps every row, says so (`prefers_none`), and still clears the
/// `held_by` an old `ai-pool.toml` left. Another source naming no row of this machine's stays an
/// error, since there it means the wrong machine or the wrong file.
/// NEGATIVE CONTROL: with the `!lab_fleet` term removed from `sync_cli_with_replan`'s empty-fleet
/// refusal, the sync fails (`no active Kimi accounts found for machine`).
#[test]
fn a_cli_sync_from_a_lab_that_prefers_nothing_here_keeps_every_row_and_clears_holds() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let (home, state_dir, config) = (
        root.path().join("home"),
        root.path().join("state"),
        root.path().join("config"),
    );
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&config).unwrap();
    let toml = config.join("aterm.toml");
    crate::test_config::write(&toml, "").unwrap();
    let cli = Cli::Kimi;
    let region = accounts::Vendor {
        region: Some(crate::ai::connector::Region::Global),
        ..accounts::Vendor::default()
    };
    let roster_path = accounts::roster_beside_for(&toml, cli).unwrap();
    let store = accounts::Store::at_for(&state_dir, cli, &home);
    let login = store.logins_dir().join("kimi-eeeeee");
    fs::create_dir_all(&login).unwrap();
    let rows = vec![Row {
        label: "kimi-eeeeee".into(),
        dir: Some(login.to_str().unwrap().to_owned()),
        email: Some(email("eve")),
        vendor: region,
        held_by: Some("m3".into()),
        ..Row::default()
    }];
    fs::write(
        &roster_path,
        accounts::append_rows_for(cli, "", &rows).unwrap(),
    )
    .unwrap();
    let lab = Lab {
        me: Some("m16".into()),
        settled: true,
        accounts: vec![LabAccount {
            cli,
            vendor: region,
            ..lab_account("eve", "kimi-eeeeee", &[("m20", Some(1))])
        }],
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Default::default(),
    };
    let fetch = |_: &Env, _: &str| -> Result<String, String> { panic!("the lab is the source") };
    let synced = alab_roster::with_lab(lab, || {
        sync_cli_with_replan(
            cli,
            (&home, &state_dir, &toml),
            (Some(alab_roster::SOURCE), None),
            NOW,
            (&fetch, &[]),
            || {},
        )
    });
    let Ok(CliSync::Synced(synced)) = synced else {
        panic!("an empty lab fleet is no error: {synced:?}");
    };
    assert!(
        synced.prefers_none,
        "the report says the lab prefers nothing here"
    );
    assert!(synced.added.is_empty());
    let roster = Roster::load_for(cli, &roster_path).unwrap();
    let row = roster.row("kimi-eeeeee").expect("the row stays");
    assert_eq!(
        row.held_by, None,
        "an old pool's hold does not outlive its source"
    );
    assert_eq!(row.dir.as_deref(), login.to_str());
    assert_eq!(row.class, Class::Pool, "preferred elsewhere");
}

/// A scratch HOME for Claude Code's sync as the upkeep and `accounts sync` run it
/// ([`sync_accounts_with`]), with an empty `aterm.toml` and, where `default` names one, a default
/// login signed in as that address.
struct Bed {
    _root: aterm_tempfile::TempDir,
    env: Env,
    store: accounts::Store,
    roster: PathBuf,
}

impl Bed {
    fn new(default: Option<&str>) -> Self {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let config = home.join(".config/aterm/aterm.toml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        crate::test_config::write(&config, "").unwrap();
        if let Some(local) = default {
            fs::write(
                home.join(".claude.json"),
                format!(
                    "{{\"oauthAccount\":{{\"emailAddress\":\"{}\"}}}}\n",
                    email(local)
                ),
            )
            .unwrap();
        }
        let env = Env {
            state: root.path().join("state"),
            aterm_state: None,
            cwd: home.clone(),
            home: Some(home.clone()),
            claude_dir: Some(home.join(".claude")),
            claude_config_unusable: None,
            claude_profile: Some(home.join(".claude.json")),
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            agy_conversation: None,
            codex_home: None,
            codex_login: None,
            now: NOW,
            tz_s: 0,
            sid: String::new(),
            sock: None,
            config: Some(config),
            claude_tmp: None,
            claude_running: None,
        };
        let roster = env.accounts_roster().unwrap();
        let store = env.accounts_store();
        Self {
            _root: root,
            env,
            store,
            roster,
        }
    }

    /// `label`'s login directory, holding a file, as a path for its row.
    fn login(&self, label: &str) -> String {
        let dir = self.store.logins_dir().join(label);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("login"), label).unwrap();
        dir.to_str().unwrap().to_owned()
    }

    fn seed(&self, rows: &[Row]) {
        fs::create_dir_all(self.roster.parent().unwrap()).unwrap();
        fs::write(
            &self.roster,
            format_roster_toml(&Roster {
                rows: rows.to_vec(),
            })
            .unwrap(),
        )
        .unwrap();
        self.store
            .transact_logins(NOW, &mut None, |state, logins| {
                for row in rows {
                    seed_reference(state, logins, &row.label, 3);
                }
                Ok(Vec::new())
            })
            .unwrap();
    }

    fn sync(&self, lab: &Lab) -> Result<SyncReport, String> {
        let fetch =
            |_: &Env, _: &str| -> Result<String, String> { panic!("the lab is the source") };
        alab_roster::with_lab(lab.clone(), || {
            sync_accounts_with(
                &self.env,
                None,
                None,
                &SyncHow {
                    fetch: &fetch,
                    ladder: &[],
                    fleet_snapshots: &[],
                    heal: None,
                    allow_empty_pool: true,
                },
            )
        })
    }

    fn roster(&self) -> Roster {
        Roster::load(&self.roster).unwrap()
    }
}

fn lab_of(settled: bool, accounts: Vec<LabAccount>) -> Lab {
    Lab {
        me: Some("m22".into()),
        settled,
        accounts,
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Default::default(),
    }
}

/// Claude Code's sync from a settled lab renames an old-style row (`m22-2`) to its account's lab
/// label before it reads the fleet ([`sync_accounts_with`]'s `migrate_before_sync`), keeping the
/// login directory where it is and its records under the new label, so nobody is signed out;
/// with the lab unsettled the row keeps its label.
/// NEGATIVE CONTROL: with `sync_accounts_with`'s `migrate_before_sync` call removed, the settled
/// sync leaves `m22-2` (`renamed before the fleet is read`).
#[test]
fn claude_codes_sync_from_a_settled_lab_migrates_the_label_and_keeps_the_login() {
    let bed = Bed::new(None);
    let login = bed.login("m22-2");
    bed.seed(&[Row {
        label: "m22-2".into(),
        dir: Some(login.clone()),
        email: Some(email("bob")),
        ..Row::default()
    }]);
    let lab = |settled: bool| {
        lab_of(
            settled,
            vec![LabAccount {
                legacy_label: Some("m22-2".into()),
                ..lab_account("bob", "claude-bbbbbb", &[("m22", Some(1))])
            }],
        )
    };
    bed.sync(&lab(false)).unwrap();
    assert!(
        bed.roster().row("m22-2").is_some(),
        "an unsettled lab renames nothing"
    );
    bed.sync(&lab(true)).unwrap();
    let roster = bed.roster();
    assert!(
        roster.row("m22-2").is_none(),
        "renamed before the fleet is read"
    );
    let row = roster.row("claude-bbbbbb").expect("the account's label");
    assert_eq!(row.dir.as_deref(), Some(login.as_str()), "the login stays");
    assert_eq!(fs::read(Path::new(&login).join("login")).unwrap(), b"m22-2");
    let (state, _) = bed.store.load(NOW).unwrap();
    assert_eq!(
        state.accounts.get("claude-bbbbbb").map(|a| a.auth),
        Some(accounts::Auth::Ok),
        "still signed in, under its new label"
    );
}

/// Under the lab the default login's row takes the account's lab label, never a label with a
/// machine in it nor a slot read from its address: an account another Mac prefers is the
/// default login's row here as `claude-aaaaaa`, the default login (no login directory), tried
/// after this Mac's preferred ones. A default login the lab does not know gets no row, since
/// nothing names it; a sync that adds nothing writes nothing, and keeps every row it had.
/// NEGATIVE CONTROL: with `build_machine_roster`'s `alab_default_account` arm replaced by the
/// sheet path's `local_account`, the default login is labelled `m22` (`no machine label`); with
/// `persist_accounts_sync_with_pool`'s empty-roster return removed, the bare Mac's sync is refused
/// (`an empty lab roster is no error`).
#[test]
fn the_default_login_takes_its_lab_label_and_an_unknown_one_none() {
    let bed = Bed::new(Some("ann"));
    let lab = lab_of(
        false,
        vec![
            lab_account("ann", "claude-aaaaaa", &[("m20", Some(1))]),
            lab_account("gilbert", "claude-gggggg", &[("m22", Some(1))]),
        ],
    );
    bed.sync(&lab).unwrap();
    let roster = bed.roster();
    assert!(
        roster.rows.iter().all(|row| !row.label.starts_with("m22")),
        "no machine label: {:?}",
        roster.rows.iter().map(|r| &r.label).collect::<Vec<_>>()
    );
    let default = roster.row("claude-aaaaaa").expect("the lab's label");
    assert_eq!(default.dir, None, "the default login");
    assert_eq!(default.class, Class::Pool, "another Mac prefers it");
    assert!(roster.row("claude-gggggg").is_some_and(|r| r.dir.is_some()));

    let unknown = Bed::new(Some("zed"));
    let lab = lab_of(
        false,
        vec![lab_account("gilbert", "claude-gggggg", &[("m22", Some(1))])],
    );
    unknown.sync(&lab).unwrap();
    let roster = unknown.roster();
    let labels: Vec<&str> = roster.rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["claude-gggggg"], "nothing names the unknown login");

    let empty = Bed::new(Some("zed"));
    let kept = empty.login("kept");
    empty.seed(&[Row {
        label: "kept".into(),
        dir: Some(kept),
        email: Some(email("dee")),
        ..Row::default()
    }]);
    let before = fs::read(&empty.roster).unwrap();
    let lab = lab_of(
        false,
        vec![lab_account("gilbert", "claude-gggggg", &[("m20", Some(1))])],
    );
    empty.sync(&lab).unwrap();
    assert_eq!(
        fs::read(&empty.roster).unwrap(),
        before,
        "a lab that prefers nothing here keeps every row as it was"
    );
    let bare = Bed::new(Some("zed"));
    bare.sync(&lab).expect("an empty lab roster is no error");
    assert!(
        !bare.roster.exists(),
        "an empty lab roster stays empty: the sync writes nothing"
    );
}

/// The upkeep's lab trigger ([`alab_roster::sync_due`]) calls for a sync when the lab moved a
/// browser profile of an account a row already holds, and the sync it calls for settles it (the
/// next look is not due); a preferred account the roster's cap leaves out calls for none, so a
/// full roster is not synced at every upkeep, while one row of room calls for it.
/// NEGATIVE CONTROLS: with `sync_due`'s browser arm removed, the moved profile is not due
/// (`a moved browser is due`); with its `room` gate removed, the full roster is due
/// (`a full roster is not due`).
#[test]
fn the_lab_trigger_sees_a_moved_browser_and_growth_of_a_large_roster() {
    let bed = Bed::new(None);
    let gil = bed.login("claude-gggggg");
    bed.seed(&[Row {
        label: "claude-gggggg".into(),
        dir: Some(gil),
        email: Some(email("gil")),
        browser: Some("old".into()),
        ..Row::default()
    }]);
    let mut moved = lab_account("gil", "claude-gggggg", &[("m22", Some(1))]);
    moved.browser = Some("new".into());
    let lab = lab_of(false, vec![moved]);
    let due = |lab: &Lab| {
        alab_roster::with_lab(lab.clone(), || {
            alab_roster::sync_due(Cli::Claude, &bed.roster, |_| false)
        })
    };
    assert!(due(&lab), "a moved browser is due");
    bed.sync(&lab).unwrap();
    assert_eq!(
        bed.roster()
            .row("claude-gggggg")
            .and_then(|r| r.browser.clone()),
        Some("new".into())
    );
    assert!(!due(&lab), "the sync settles it");

    let full = Bed::new(None);
    let rows = |n: usize| -> Vec<Row> {
        (0..n)
            .map(|i| {
                let label = format!("kept-{i}");
                Row {
                    dir: Some(full.login(&label)),
                    email: Some(email(&label)),
                    class: Class::Pool,
                    label,
                    ..Row::default()
                }
            })
            .collect()
    };
    let lab = lab_of(
        false,
        vec![lab_account("gil", "claude-gggggg", &[("m22", Some(1))])],
    );
    let due = |lab: &Lab| {
        alab_roster::with_lab(lab.clone(), || {
            alab_roster::sync_due(Cli::Claude, &full.roster, |_| false)
        })
    };
    full.seed(&rows(96));
    assert!(due(&lab), "a large roster still imports a new account");
    full.sync(&lab).unwrap();
    assert_eq!(full.roster().rows.len(), 97);
    assert!(full.roster().row("kept-0").is_some());
    assert!(full.roster().row("kept-95").is_some());
    full.seed(&rows(96 - 1));
    assert!(due(&lab), "a roster with room is due");
}

/// A default login whose account the lab says may not be signed in (paused, retired or barred)
/// gets no new row, as `accounts add` refuses it by name: the renewal ladder never tries a
/// sign-in the lab bars. The login itself is untouched.
/// NEGATIVE CONTROL: with `fleet_for`'s `joinable` filter on `known` removed, the retired
/// account is listed as the default login (`no row for a retired account`).
#[test]
fn a_retired_default_login_gets_no_row() {
    let bed = Bed::new(Some("rex"));
    let mut retired = lab_account("rex", "claude-rrrrrr", &[]);
    retired.joinable = false;
    let lab = lab_of(
        false,
        vec![
            retired,
            lab_account("gil", "claude-gggggg", &[("m22", Some(1))]),
        ],
    );
    bed.sync(&lab).unwrap();
    let roster = bed.roster();
    let labels: Vec<&str> = roster.rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["claude-gggggg"], "no row for a retired account");
}

/// The first `add` lists the default login as `main`, with no login directory and no address.
/// Under the lab that row already lists the default login: a sync keeps it as it is, and the
/// account the lab knows the default login as is not listed beside it as a second row with no
/// login directory (a roster `Roster::parse` refuses); a repeat changes nothing.
/// NEGATIVE CONTROL: with `build_machine_roster`'s `listed` return removed, the sync is refused
/// (`both name no dir`).
#[test]
fn the_first_adds_main_is_the_default_login_under_the_lab() {
    let bed = Bed::new(Some("dee"));
    let asked = bed.login("claude-aaaaaa");
    bed.seed(&[
        Row {
            label: "main".into(),
            ..Row::default()
        },
        Row {
            label: "claude-aaaaaa".into(),
            class: Class::Pool,
            dir: Some(asked.clone()),
            email: Some(email("ann")),
            ..Row::default()
        },
    ]);
    let lab = lab_of(
        false,
        vec![
            lab_account("ann", "claude-aaaaaa", &[("m20", Some(1))]),
            lab_account("dee", "claude-dddddd", &[("m21", Some(1))]),
        ],
    );
    bed.sync(&lab).expect("the sync is not refused");
    let roster = bed.roster();
    let dirless: Vec<&str> = roster
        .rows
        .iter()
        .filter(|row| row.dir.is_none())
        .map(|row| row.label.as_str())
        .collect();
    assert_eq!(dirless, ["main"], "one default login");
    assert_eq!(roster.row("main").and_then(|row| row.email.clone()), None);
    assert_eq!(
        roster.row("claude-aaaaaa").and_then(|row| row.dir.clone()),
        Some(asked)
    );
    let text = fs::read(&bed.roster).unwrap();
    bed.sync(&lab).expect("a repeat");
    assert_eq!(
        fs::read(&bed.roster).unwrap(),
        text,
        "a repeat changes nothing"
    );
}

/// Where the lab names this Mac the upkeep reads no sheet: an edit to `ai-accounts.csv` moves
/// neither Claude Code's sync inputs nor a CLI's, the sheet counts as no fleet source and names
/// nothing, and the init trigger asks the lab, which is satisfied by a roster that holds what it
/// prefers. With no lab held, the same edit moves both digests, as it always did; while the lab
/// is pending (held, not read yet) the sheet is no fleet source either, and an edit to it moves
/// no CLI's inputs.
/// NEGATIVE CONTROL: with `sync_inputs`' `lab_chosen` arm removed, the sheet edit moves Claude
/// Code's inputs under the lab (`reads no sheet`); with `has_fleet_source`'s `sheet_read_here`
/// gate removed, the sheet is a source (`no fleet source`); with `cli_sync_inputs` reading the
/// sheet ungated, the pending digest moves (`while the lab is not read yet`).
#[test]
fn a_mac_the_lab_names_reads_no_sheet_in_its_upkeep() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let home = root.path().join("home");
    let state_dir = root.path().join("state");
    let config_dir = home.join(".config/aterm");
    fs::create_dir_all(&config_dir).unwrap();
    fs::create_dir_all(home.join(".secrets")).unwrap();
    let toml = config_dir.join("aterm.toml");
    crate::test_config::write(&toml, "").unwrap();
    let roster_path = config_dir.join(accounts::ACCOUNTS_FILE);
    fs::write(
        &roster_path,
        format_roster_toml(&Roster {
            rows: vec![Row {
                label: "claude-aaaaaa".into(),
                email: Some(email("ann")),
                ..Row::default()
            }],
        })
        .unwrap(),
    )
    .unwrap();
    let sheet = home.join(".secrets").join(ACCOUNTS_CSV_FILE);
    let edit = |text: &str| fs::write(&sheet, text).unwrap();
    let lab = lab_of(
        false,
        vec![lab_account("ann", "claude-aaaaaa", &[("m22", Some(1))])],
    );
    let digests = || {
        (
            sync_inputs(&home, &state_dir, &roster_path, NOW),
            cli_sync_inputs(Cli::Kimi, &home, &toml),
        )
    };
    edit("email,computer,claude,kimi\none@example.com,m22,1,global\n");
    let (claude, kimi) = alab_roster::with_lab(lab.clone(), || {
        assert!(!has_fleet_source(&home.join(".secrets")), "no fleet source");
        assert!(!sheet_names_this_machine(Some(&toml), &sheet));
        assert!(
            !init_sync_needed(&home, &state_dir, &roster_path, NOW, true),
            "the roster holds what the lab prefers"
        );
        digests()
    });
    let (bare_claude, bare_kimi) = digests();
    edit("email,computer,claude,kimi\ntwo@example.com,m22,1,global\n");
    alab_roster::with_lab(lab, || {
        assert_eq!(digests().0, claude, "reads no sheet");
        assert_eq!(digests().1, kimi, "a CLI's upkeep reads no sheet");
    });
    let (moved_claude, moved_kimi) = digests();
    assert_ne!(moved_claude, bare_claude, "with no lab the sheet is read");
    assert_ne!(moved_kimi, bare_kimi);
    assert!(has_fleet_source(&home.join(".secrets")));
    let pending_kimi = alab_roster::while_pending(|| {
        assert!(
            !has_fleet_source(&home.join(".secrets")),
            "a lab not read yet reads no sheet"
        );
        digests().1
    });
    edit("email,computer,claude,kimi\nthree@example.test,m22,1,global\n");
    assert_ne!(digests().1, moved_kimi, "with no lab the edit moves it");
    assert_eq!(
        alab_roster::while_pending(|| digests().1),
        pending_kimi,
        "a CLI's upkeep reads no sheet while the lab is not read yet"
    );
}

/// What a sync says of the rows it added to a pool no upkeep tries names `renew` alone: an
/// Antigravity row's browser window leaves it no route. CONTROL: Kimi Code CLI's names `browser`.
#[test]
fn a_synced_antigravity_row_is_told_only_renew() {
    for (cli, names_browser) in [(Cli::Agy, false), (Cli::Kimi, true)] {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        let config = root.path().join("config");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&state).unwrap();
        let toml = config.join("aterm.toml");
        aterm_uds::atomic_file::write(&toml, b"", 0o600).unwrap();
        let region = if cli == Cli::Kimi {
            "region = \"global\"\n"
        } else {
            ""
        };
        let fetch = |_: &Env, _: &str| -> Result<String, String> {
            Ok(format!(
                "[[account]]\nlabel = \"ann\"\nemail = \"ann@example.com\"\n\
                 machines = [\"m16\"]\n{region}"
            ))
        };
        let env = Env {
            state,
            aterm_state: None,
            cwd: home.clone(),
            home: Some(home.clone()),
            claude_dir: Some(home.join(".claude")),
            claude_config_unusable: None,
            claude_profile: Some(home.join(".claude.json")),
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            agy_conversation: None,
            codex_home: None,
            codex_login: None,
            now: NOW,
            tz_s: 0,
            sid: String::new(),
            sock: None,
            config: Some(toml),
            claude_tmp: None,
            claude_running: None,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_cli_accounts_sync_with(
            cli,
            (&env, None),
            (Some("accounts.toml"), Some("m16"), false),
            (&mut out, &mut err),
            &fetch,
        );
        let (out, err) = (
            String::from_utf8(out).unwrap(),
            String::from_utf8_lossy(&err).into_owned(),
        );
        assert_eq!(code, ExitCode::SUCCESS, "{}: {err}", cli.id());
        let said = out
            .lines()
            .find(|line| line.starts_with("not signed in yet: "))
            .unwrap_or_else(|| panic!("{}: {out}", cli.id()));
        assert!(said.contains("accounts renew"), "{}: {out}", cli.id());
        assert!(!said.contains("by itself"), "{}: {out}", cli.id());
        assert_eq!(
            said.contains("accounts browser"),
            names_browser,
            "{}: {out}",
            cli.id()
        );
    }
}

/// A FIRST LAB SYNC LISTS ONE ACCOUNT ONCE: where Codex's default login names an account the lab
/// prefers here, that row is the default login's (its lab label, its address, no login directory),
/// so no `main` stands beside it; where the lab knows the account and prefers it elsewhere, it joins
/// as the default login's row beside what the lab prefers here. CONTROL: where the lab does not know
/// the account, and where the login names none, the default login is `main`, and every row the lab
/// prefers keeps a directory of its own. The report's words say which row is on this machine's own
/// login, and never list it as not signed in yet.
#[test]
fn a_first_lab_sync_lists_the_default_login_under_its_lab_label() {
    let codex = |local: &str, label: &str, preferred: &[(&str, Option<u64>)]| LabAccount {
        cli: Cli::Codex,
        ..lab_account(local, label, preferred)
    };
    let run_kept = |default: Option<&str>, keyring: bool| -> (Vec<Row>, String) {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        let config = root.path().join("config");
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&state).unwrap();
        let toml = config.join("aterm.toml");
        crate::test_config::write(&toml, "").unwrap();
        if keyring {
            fs::write(
                home.join(".codex/config.toml"),
                "cli_auth_credentials_store = \"keyring\"\n",
            )
            .unwrap();
        }
        if let Some(default) = default {
            let token = crate::harness::codex_app_login::tests::make_chatgpt_jwt(
                Some(default),
                "plus",
                NOW + 3600,
            );
            fs::write(
                home.join(".codex/auth.json"),
                aterm_json::json!({"tokens": {"access_token": token}}).to_string(),
            )
            .unwrap();
        }
        let lab = Lab {
            me: Some("m16".into()),
            settled: true,
            accounts: vec![
                codex("ann", "codex-aaaaaa", &[("m16", Some(1))]),
                codex("bea", "codex-bbbbbb", &[("m16", Some(2))]),
                codex("cid", "codex-cccccc", &[("m3", Some(1))]),
            ],
            aliases: BTreeMap::new(),
            mailboxes: BTreeMap::new(),
            #[cfg(unix)]
            machine_name: None,
            places: Default::default(),
        };
        let fetch =
            |_: &Env, _: &str| -> Result<String, String> { panic!("the lab is the source") };
        let env = Env {
            state,
            aterm_state: None,
            cwd: home.clone(),
            home: Some(home.clone()),
            claude_dir: Some(home.join(".claude")),
            claude_config_unusable: None,
            claude_profile: Some(home.join(".claude.json")),
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            agy_conversation: None,
            codex_home: None,
            codex_login: None,
            now: NOW,
            tz_s: 0,
            sid: String::new(),
            sock: None,
            config: Some(toml.clone()),
            claude_tmp: None,
            claude_running: None,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = alab_roster::with_lab(lab, || {
            run_cli_accounts_sync_with(
                Cli::Codex,
                (&env, None),
                (None, None, false),
                (&mut out, &mut err),
                &fetch,
            )
        });
        assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
        let roster = Roster::load_for(
            Cli::Codex,
            &accounts::roster_beside_for(&toml, Cli::Codex).unwrap(),
        )
        .unwrap();
        (roster.rows, String::from_utf8(out).unwrap())
    };
    let run = |default: Option<&str>| run_kept(default, false);
    let shape = |rows: &[Row]| -> Vec<(String, bool)> {
        rows.iter()
            .map(|row| (row.label.clone(), row.dir.is_some()))
            .collect()
    };

    // Under the keyring store a leftover `auth.json` is no login of this machine's: the row it
    // names keeps a directory of its own.
    let (rows, said) = run_kept(Some(&email("ann")), true);
    assert_eq!(
        shape(&rows),
        vec![
            ("main".to_string(), false),
            ("codex-aaaaaa".to_string(), true),
            ("codex-bbbbbb".to_string(), true)
        ],
        "{said}"
    );
    assert!(!said.contains("on this machine's own login:"), "{said}");

    let (rows, said) = run(Some(&email("ann")));
    assert_eq!(
        shape(&rows),
        vec![
            ("codex-aaaaaa".to_string(), false),
            ("codex-bbbbbb".to_string(), true)
        ],
        "{said}"
    );
    assert_eq!(rows[0].email.as_deref(), Some(email("ann").as_str()));
    assert!(
        said.contains("on this machine's own login: codex-aaaaaa"),
        "{said}"
    );
    let unsigned = said
        .lines()
        .find(|line| line.starts_with("not signed in yet: "))
        .expect("the row with no login yet");
    assert!(unsigned.contains("codex-bbbbbb"), "{said}");
    assert!(!unsigned.contains("codex-aaaaaa"), "{said}");
    assert!(said.contains("the lab prefers here"), "{said}");
    assert!(!said.contains("across the fleet"), "{said}");

    let (rows, said) = run(Some(&email("cid")));
    assert_eq!(
        shape(&rows),
        vec![
            ("codex-cccccc".to_string(), false),
            ("codex-aaaaaa".to_string(), true),
            ("codex-bbbbbb".to_string(), true)
        ],
        "{said}"
    );
    assert_eq!(rows[0].email.as_deref(), Some(email("cid").as_str()));

    for default in [Some(email("nobody")), None] {
        let (rows, said) = run(default.as_deref());
        assert_eq!(
            shape(&rows),
            vec![
                ("main".to_string(), false),
                ("codex-aaaaaa".to_string(), true),
                ("codex-bbbbbb".to_string(), true)
            ],
            "CONTROL: {default:?}: {said}"
        );
        assert!(
            !said.contains("on this machine's own login:"),
            "CONTROL: {said}"
        );
    }
}

/// A SHEET NAMED AS A SOURCE ON A MAC THE LAB NAMES IS REFUSED WITH THE VERB THAT SYNCS THAT POOL,
/// `--cli` INCLUDED WHERE THE POOL TAKES ONE.
#[test]
fn a_refused_sheet_names_the_sync_of_its_own_pool() {
    assert!(
        sheet_refused("a.csv", Cli::Kimi).contains("`aterm harness accounts sync alab --cli kimi`")
    );
    assert!(sheet_refused("a.csv", Cli::Claude).contains("`aterm harness accounts sync alab`"));
}
