// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab's roster on scratch homes and stores; every address is synthetic (`example.test`),
//! and nothing here reads or writes this Mac's real accounts, vault or registry.

use super::*;
use crate::ai::connector::Region;
use crate::harness::accounts::Auth;
use crate::harness::accounts::rename_tests::{NOW, seed_reference};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

fn email(local: &str) -> String {
    format!("{local}@example.test")
}

fn account(local: &str, cli: Cli, label: &str, preferred: &[(&str, Option<u64>)]) -> LabAccount {
    LabAccount {
        pk: format!("ac-{:0>16}", local.len()),
        cli,
        vendor: Vendor {
            region: (cli == Cli::Kimi).then_some(Region::Global),
            flow: None,
        },
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

fn legacy(account: LabAccount, label: &str) -> LabAccount {
    LabAccount {
        legacy_label: Some(label.to_owned()),
        ..account
    }
}

fn row(label: &str, dir: Option<&Path>, local: &str, cli: Cli) -> Row {
    Row {
        label: label.to_owned(),
        dir: dir.map(|dir| dir.to_str().unwrap().to_owned()),
        email: Some(email(local)),
        vendor: Vendor {
            region: (cli == Cli::Kimi).then_some(Region::Global),
            flow: None,
        },
        ..Row::default()
    }
}

/// A Mac's preferred accounts join in rank order with an unranked one last; an account this Mac
/// does not prefer, or one that may not join (paused), is not added; an account the roster already
/// has keeps the label the roster gives it, since only [`migrate`] renames; and a lab that names
/// no machine for this Mac yields no fleet, so the sync reads what it always read.
/// NEGATIVE CONTROL: with `fleet_for` taking the lab's label over the roster's, the kept row is
/// renamed by the sync: `` left: ["m22-2", …] right: ["claude-bbbbbb", …] ``; with the
/// `joinable` filter removed, the paused account joins.
#[test]
fn the_lab_fleet_adds_this_macs_preferred_accounts_in_rank_order() {
    let mut paused = account("dee", Cli::Claude, "claude-dddddd", &[("m22", Some(1))]);
    paused.joinable = false;
    let lab = Lab {
        me: Some("m22".into()),
        settled: false,
        accounts: vec![
            account("ann", Cli::Claude, "claude-aaaaaa", &[("m22", None)]),
            account("bob", Cli::Claude, "claude-bbbbbb", &[("m22", Some(2))]),
            account(
                "cy",
                Cli::Claude,
                "claude-cccccc",
                &[("m22", Some(1)), ("m3", Some(1))],
            ),
            account("eve", Cli::Claude, "claude-eeeeee", &[("m3", Some(1))]),
            account("fay", Cli::Kimi, "kimi-ffffff", &[("m22", Some(1))]),
            paused,
        ],
        aliases: BTreeMap::new(),
    };
    let existing = Roster {
        rows: vec![row(
            "m22-2",
            Some(Path::new("/scratch/m22-2")),
            "bob",
            Cli::Claude,
        )],
    };
    let fleet = fleet_for(&lab, Cli::Claude, Some(&existing)).unwrap();
    let labels: Vec<&str> = fleet.accounts.iter().map(|a| a.label.as_str()).collect();
    assert_eq!(labels, ["claude-cccccc", "m22-2", "claude-aaaaaa"]);
    assert!(fleet.accounts.iter().all(|a| a.machines == ["m22"]));
    assert_eq!(fleet.alab.as_deref(), Some("m22"));
    let kimi = fleet_for(&lab, Cli::Kimi, None).unwrap();
    assert_eq!(kimi.accounts.len(), 1);
    assert_eq!(kimi.accounts[0].vendor.region, Some(Region::Global));
    let unbound = Lab {
        me: None,
        ..lab.clone()
    };
    assert_eq!(fleet_for(&unbound, Cli::Claude, None), None);
}

/// A sync from the lab keeps every row this Mac has, the ones no preference names included, and
/// derives each known row's class from the preferences: preferred here first, every other row
/// after. A row the lab does not know keeps its class.
/// NEGATIVE CONTROL: with `retain`'s put-back loop removed, the unpreferred row and the unknown
/// row are dropped: `` left: ["claude-aaaaaa"] right: ["claude-aaaaaa", "m20-m22", "kept"] ``.
#[test]
fn a_preference_never_drops_a_row() {
    let lab = Lab {
        me: Some("m22".into()),
        settled: true,
        accounts: vec![
            account("ann", Cli::Claude, "claude-aaaaaa", &[("m22", Some(1))]),
            account("cy", Cli::Claude, "claude-cccccc", &[("m20", Some(1))]),
        ],
        aliases: BTreeMap::new(),
    };
    let mut kept = row("kept", Some(Path::new("/scratch/kept")), "dee", Cli::Claude);
    kept.class = Class::Shared;
    let existing = Roster {
        rows: vec![
            row("claude-aaaaaa", None, "ann", Cli::Claude),
            row(
                "m20-m22",
                Some(Path::new("/scratch/m20-m22")),
                "cy",
                Cli::Claude,
            ),
            kept,
        ],
    };
    let rebuilt = Roster {
        rows: vec![row("claude-aaaaaa", None, "ann", Cli::Claude)],
    };
    let roster = retain(rebuilt, &existing, Some(&lab), Cli::Claude, "m22");
    let got: Vec<(&str, Class, Option<&str>)> = roster
        .rows
        .iter()
        .map(|r| (r.label.as_str(), r.class, r.dir.as_deref()))
        .collect();
    assert_eq!(
        got,
        [
            ("claude-aaaaaa", Class::Dedicated, None),
            ("kept", Class::Shared, Some("/scratch/kept")),
            ("m20-m22", Class::Shared, Some("/scratch/m20-m22")),
        ]
    );
}

/// A scratch HOME with old-style rows: Claude Code's `m22` (the default login), `m22-2` and
/// `m20-m22`, an unknown `kept`, and Kimi Code's `kimi-m16`, each login directory holding a file,
/// with state (an `Ok` sign-in word), a session and a binding on record.
struct Home {
    _root: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    config: PathBuf,
}

const CLAUDE_ROWS: [(&str, &str); 4] = [
    ("m22", "ann"),
    ("m22-2", "bob"),
    ("m20-m22", "cy"),
    ("kept", "dee"),
];

impl Home {
    fn new() -> Self {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        let config = root.path().join("config");
        for dir in [&home, &state, &config] {
            fs::create_dir_all(dir).unwrap();
        }
        let bed = Self {
            _root: root,
            home,
            state,
            config,
        };
        for (cli, rows) in [
            (Cli::Claude, CLAUDE_ROWS.to_vec()),
            (Cli::Kimi, vec![("kimi-m16", "eve")]),
        ] {
            let store = bed.store(cli);
            let rows: Vec<Row> = rows
                .iter()
                .map(|(label, local)| {
                    let dir = (*label != "m22").then(|| store.logins_dir().join(label));
                    if let Some(dir) = &dir {
                        fs::create_dir_all(dir).unwrap();
                        fs::write(dir.join("auth.json"), format!("login of {local}")).unwrap();
                    }
                    row(label, dir.as_deref(), local, cli)
                })
                .collect();
            fs::write(
                bed.roster(cli),
                accounts::append_rows_for(cli, "", &rows).unwrap(),
            )
            .unwrap();
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    for row in &rows {
                        for kind in [1, 3, 7] {
                            seed_reference(state, logins, &row.label, kind);
                        }
                    }
                    Ok(Vec::new())
                })
                .unwrap();
        }
        bed
    }

    fn store(&self, cli: Cli) -> Store {
        match cli {
            Cli::Claude => Store::at(&self.state),
            _ => Store::at_for(&self.state, cli, &self.home),
        }
    }

    fn roster(&self, cli: Cli) -> PathBuf {
        accounts::roster_beside_for(&self.config.join("aterm.toml"), cli).unwrap()
    }

    /// The lab of this home's accounts, each recorded under the label its row has (its legacy
    /// label), but `kept`'s account, which the lab does not know.
    fn lab(&self, settled: bool) -> Lab {
        Lab {
            me: Some("m22".into()),
            settled,
            accounts: vec![
                legacy(
                    account("ann", Cli::Claude, "claude-aaaaaa", &[("m22", Some(1))]),
                    "m22",
                ),
                legacy(
                    account("bob", Cli::Claude, "claude-bbbbbb", &[("m22", Some(2))]),
                    "m22-2",
                ),
                legacy(
                    account(
                        "cy",
                        Cli::Claude,
                        "claude-cccccc",
                        &[("m20", None), ("m22", None)],
                    ),
                    "m20-m22",
                ),
                legacy(
                    account("eve", Cli::Kimi, "kimi-eeeeee", &[("m16", Some(1))]),
                    "kimi-m16",
                ),
            ],
            aliases: BTreeMap::new(),
        }
    }

    fn migrate(&self, lab: &Lab, cli: Cli) -> Result<Migrated, String> {
        migrate(lab, cli, &self.store(cli), &self.roster(cli), NOW, &[])
    }

    /// Every file under the scratch root with its bytes.
    fn tree(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        let mut walk = vec![self.home.clone(), self.state.clone(), self.config.clone()];
        while let Some(at) = walk.pop() {
            for entry in fs::read_dir(&at).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk.push(path);
                } else {
                    out.insert(path.clone(), fs::read(&path).unwrap());
                }
            }
        }
        out
    }

    /// The migrated home, as it must read: each row under the lab's label, its login directory
    /// the one it had with its login in it, the default login still the default, its state, its
    /// session and its binding moved with it, and nothing left under an old label.
    fn assert_migrated(&self) {
        for (cli, moved) in [
            (
                Cli::Claude,
                vec![
                    ("m22", "claude-aaaaaa", "ann"),
                    ("m22-2", "claude-bbbbbb", "bob"),
                    ("m20-m22", "claude-cccccc", "cy"),
                    ("kept", "kept", "dee"),
                ],
            ),
            (Cli::Kimi, vec![("kimi-m16", "kimi-eeeeee", "eve")]),
        ] {
            let store = self.store(cli);
            let roster = Roster::load_for(cli, &self.roster(cli)).unwrap();
            let (state, _) = store.load(NOW).unwrap();
            let (logins, _) = store.load_logins(NOW).unwrap();
            for (old, new, local) in moved {
                let row = roster.row(new).unwrap_or_else(|| panic!("{new} is a row"));
                let was = (old != "m22").then(|| store.logins_dir().join(old));
                assert_eq!(
                    row.dir.as_deref(),
                    was.as_deref().map(|dir| dir.to_str().unwrap()),
                    "{new}'s login directory is pinned where {old}'s was"
                );
                if let Some(dir) = &was {
                    assert_eq!(
                        fs::read_to_string(dir.join("auth.json")).unwrap(),
                        format!("login of {local}"),
                        "{old}'s login is where it was"
                    );
                }
                let env_login = was.as_deref().map(|dir| dir.to_str().unwrap());
                assert_eq!(
                    roster.row_for_login(env_login).map(|r| r.label.as_str()),
                    Some(new),
                    "a running session's login environment still names its row"
                );
                assert_eq!(
                    state.accounts.get(new).map(|a| a.auth),
                    Some(Auth::Ok),
                    "{new} keeps its sign-in word: nobody is signed out"
                );
                assert!(state.sessions.values().any(|s| s.account == new));
                assert!(logins.labels.contains_key(new));
                if old != new {
                    assert!(roster.row(old).is_none());
                    assert!(!state.accounts.contains_key(old));
                    assert!(!state.sessions.values().any(|s| s.account == old));
                    assert!(!logins.labels.contains_key(old));
                }
            }
        }
    }
}

/// Migration gives every old-style row (`m22-2`, `m20-m22`, `kimi-m16`, and the default login's
/// `m22`) the label the lab calls its account by, with its login directory pinned at the old path
/// through `Row.dir`, and its state, sign-in word, session and binding moved by the rename
/// machinery: nobody is signed out and a running session's login environment still resolves. A
/// row the lab does not know stays as it is. A second run writes nothing, and a lab whose
/// inventory has not equalled a peer's renames nothing.
/// NEGATIVE CONTROL: with `migrate` giving a renamed row `store.login_dir(new label)` instead of
/// keeping `dir`, `assert_migrated` fails on `claude-bbbbbb`'s directory; with the
/// `move_accounts` call removed from the commit, it fails on `claude-aaaaaa`'s sign-in word
/// (`left: None right: Some(Ok)`).
#[test]
fn migration_pins_every_login_and_moves_its_records() {
    let bed = Home::new();
    let untouched = bed.tree();
    for cli in [Cli::Claude, Cli::Kimi] {
        assert_eq!(
            bed.migrate(&bed.lab(false), cli).unwrap(),
            Migrated::default()
        );
    }
    assert_eq!(bed.tree(), untouched, "an unsettled lab renames nothing");

    let claude = bed.migrate(&bed.lab(true), Cli::Claude).unwrap();
    assert_eq!(claude.moved.len(), 3);
    let kimi = bed.migrate(&bed.lab(true), Cli::Kimi).unwrap();
    assert_eq!(
        kimi.moved,
        [("kimi-m16".to_owned(), "kimi-eeeeee".to_owned())]
    );
    bed.assert_migrated();

    let settled = bed.tree();
    for cli in [Cli::Claude, Cli::Kimi] {
        assert_eq!(
            bed.migrate(&bed.lab(true), cli).unwrap(),
            Migrated::default()
        );
    }
    assert_eq!(bed.tree(), settled, "a second run writes nothing");
}

/// A crash midway through a migration's commit, at every step of its replay, leaves a receipt
/// that fences every reader until it completes; the next run completes it, and the home reads
/// exactly as an uninterrupted migration leaves it. A crash between two CLIs' migrations leaves the
/// first done and the second untouched, which the next run finishes.
/// NEGATIVE CONTROL: with `migrate` skipping `recover_sync_held`, the second run is refused by the
/// pending receipt (`a sync commit is pending`) instead of completing it.
#[test]
fn a_crash_midway_through_a_migration_is_completed_by_the_next_run() {
    for cut in 0..=5 {
        let bed = Home::new();
        accounts::SYNC_COMMIT_CUT.with(|c| c.set(Some(cut)));
        let why = bed.migrate(&bed.lab(true), Cli::Claude).unwrap_err();
        assert_eq!(why, "injected interruption", "cut {cut}");
        assert!(
            bed.store(Cli::Claude).load(NOW).is_err(),
            "cut {cut}: a pending migration fences its readers"
        );
        let resumed = bed.migrate(&bed.lab(true), Cli::Claude).unwrap();
        assert!(
            resumed.moved.is_empty(),
            "cut {cut}: the receipt finished it"
        );
        bed.migrate(&bed.lab(true), Cli::Kimi).unwrap();
        bed.assert_migrated();
    }
}

/// A row is renamed only from the label the lab recorded its account under, since that is the
/// label its `<label>`-keyed sign-in secrets name and the mailbox config maps from the lab
/// (`legacy_labels`); and never to a label the store still keeps records under, which
/// `move_accounts` would refuse and so stall every later sync. Each such row keeps its label and
/// the rest move.
/// NEGATIVE CONTROL: with `renames`' `bridged` check removed, `m22` moves too (`moved` lists
/// it); with its `retained` check removed, the migration is refused (`the destination retains
/// local login or account state`).
#[test]
fn a_rename_waits_where_its_secrets_or_records_would_not_follow() {
    let bed = Home::new();
    bed.store(Cli::Claude)
        .transact_logins(NOW, &mut None, |state, logins| {
            seed_reference(state, logins, "claude-bbbbbb", 3);
            Ok(Vec::new())
        })
        .unwrap();
    let mut lab = bed.lab(true);
    lab.accounts[0].legacy_label = None;
    let done = bed.migrate(&lab, Cli::Claude).unwrap();
    assert_eq!(
        done.moved,
        [("m20-m22".to_owned(), "claude-cccccc".to_owned())]
    );
    assert_eq!(done.kept, ["m22", "m22-2"]);
    let roster = Roster::load_for(Cli::Claude, &bed.roster(Cli::Claude)).unwrap();
    for label in ["m22", "m22-2", "claude-cccccc", "kept"] {
        assert!(roster.row(label).is_some(), "{label} is a row");
    }
}

/// A label an existing row of one account holds stays that row's: the lab's account of another
/// address under the same label is not added, a higher-ranked one included, and a rebuild that
/// put another address under the row's label gets the row back as it was, its login directory
/// with it. Under the lab no old pool hold (`held_by`) survives a sync.
/// NEGATIVE CONTROL: with `fleet_for`'s `held_by_another` check removed, `work` is listed with
/// `gil`'s address; with `retain`'s `Some(at)` arm keeping the rebuilt row, `work` names `gil`.
#[test]
fn a_label_another_account_holds_stays_with_its_row() {
    let lab = Lab {
        me: Some("m22".into()),
        settled: false,
        accounts: vec![
            account("gil", Cli::Claude, "work", &[("m22", Some(1))]),
            account("ann", Cli::Claude, "claude-aaaaaa", &[("m22", Some(2))]),
        ],
        aliases: BTreeMap::new(),
    };
    let mut work = row("work", Some(Path::new("/scratch/work")), "ann", Cli::Claude);
    work.held_by = Some("m3".into());
    let existing = Roster { rows: vec![work] };
    let fleet = fleet_for(&lab, Cli::Claude, Some(&existing)).unwrap();
    let listed: Vec<(&str, &str)> = fleet
        .accounts
        .iter()
        .map(|a| (a.label.as_str(), a.email.as_str()))
        .collect();
    assert_eq!(listed, [("work", email("ann").as_str())]);

    let rebuilt = Roster {
        rows: vec![row(
            "work",
            Some(Path::new("/scratch/work")),
            "gil",
            Cli::Claude,
        )],
    };
    let roster = retain(rebuilt, &existing, Some(&lab), Cli::Claude, "m22");
    assert_eq!(roster.rows.len(), 1);
    let kept = roster.row("work").unwrap();
    assert_eq!(kept.email, Some(email("ann")));
    assert_eq!(kept.dir.as_deref(), Some("/scratch/work"));
    assert_eq!(kept.held_by, None, "no old pool hold limits it");
}

/// The `[fleet] accounts_source` the owner set in `aterm.toml` wins over the lab for Claude
/// Code, whose fleet it names; the other CLIs, which that key does not name, still read the lab.
/// NEGATIVE CONTROL: with `chosen`'s `source_key` check removed, the keyed config reads the lab
/// and the first assertion fails.
#[test]
fn a_source_the_owner_set_wins_over_the_lab() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let keyed = dir.path().join("keyed.toml");
    fs::write(
        &keyed,
        "[fleet]\naccounts_source = \"https://fleet.example.test/accounts.csv\"\n",
    )
    .unwrap();
    let plain = dir.path().join("plain.toml");
    fs::write(&plain, "").unwrap();
    let lab = Lab {
        me: Some("m22".into()),
        settled: false,
        accounts: vec![
            account("ann", Cli::Claude, "claude-aaaaaa", &[("m22", Some(1))]),
            account("fay", Cli::Kimi, "kimi-ffffff", &[("m22", Some(1))]),
        ],
        aliases: BTreeMap::new(),
    };
    with_lab(lab, || {
        assert!(matches!(
            chosen(Cli::Claude, None, None, Some(&keyed)),
            Ok(None)
        ));
        assert!(matches!(
            chosen(Cli::Claude, None, None, Some(&plain)),
            Ok(Some(_))
        ));
        assert!(matches!(
            chosen(Cli::Kimi, None, None, Some(&keyed)),
            Ok(Some(_))
        ));
        assert!(
            matches!(
                chosen(Cli::Claude, None, Some(SOURCE), Some(&keyed)),
                Ok(Some(_))
            ),
            "naming `alab` still reads the lab"
        );
    });
}

/// An alias's codes are looked for where the lab says its auth login is received: the mail route
/// that names its linked address wins over a `*@domain` route, while a route naming the alias
/// itself still wins over the link; with no lab held, the alias reads the domain route.
/// NEGATIVE CONTROL: with `resolve_index` not reading `mail_targets`, the alias resolves to the
/// domain route under the lab too: `` left: Some(1) right: Some(0) ``.
#[test]
fn an_alias_reads_the_mail_route_of_the_address_it_is_linked_with() {
    use super::super::mailbox_routes::{MailboxRoute, resolve_index};
    let route = |receives: &str| MailboxRoute {
        receives: vec![receives.to_owned()],
        ..MailboxRoute::default()
    };
    let routes = [
        route(&email("hub")),
        route("*@example.test"),
        route(&email("own")),
    ];
    assert!(mail_targets(&email("ann")).is_empty());
    assert_eq!(resolve_index(&routes, Some(&email("ann"))), Some(1));
    let lab = Lab {
        aliases: BTreeMap::from([
            (email("ann"), vec![email("hub")]),
            (email("own"), vec![email("hub")]),
        ]),
        ..Lab::default()
    };
    with_lab(lab, || {
        assert_eq!(mail_targets(&email("ANN")), [email("hub")]);
        assert_eq!(resolve_index(&routes, Some(&email("ann"))), Some(0));
        assert_eq!(resolve_index(&routes, Some(&email("own"))), Some(2));
    });
}

/// What a sync of this Mac (`m1`) makes of one account in `AlabCells`'s preference area, given
/// the lab `lab` (`pref`, `joinable`) and the roster `existing` (`row`, `held`), through the sync's own composition: [`fleet_for`], then
/// [`accounts_sync::build_machine_roster`] on a scratch store, then [`retain`].
fn preference_sync(lab: &Lab, existing: &Roster) -> Roster {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(&root.path().join("state"));
    let fleet = fleet_for(lab, Cli::Claude, Some(existing)).expect("the lab names m1");
    let built = accounts_sync::build_machine_roster(&fleet, "m1", &store, Some(existing), None);
    retain(built, existing, Some(lab), Cli::Claude, "m1")
}

/// The lab and roster an area-5 `state` stands for: account `x` (`example.test`), preferred on
/// `m1` where `pref`, else on `m2`; joinable where `joinable`; a row `m1-2` at `/scratch/x` where
/// `row`, held by `m9` where `held`.
fn preference_bed(state: &aterm_spec::interp::State) -> (Lab, Roster) {
    let on = if state["pref"] == 1 { "m1" } else { "m2" };
    let mut x = account("x", Cli::Claude, "claude-xxxxxx", &[(on, Some(1))]);
    x.joinable = state["joinable"] == 1;
    let lab = Lab {
        me: Some("m1".into()),
        settled: false,
        accounts: vec![x],
        aliases: BTreeMap::new(),
    };
    let mut rows = Vec::new();
    if state["row"] == 1 {
        let mut r = row("m1-2", Some(Path::new("/scratch/x")), "x", Cli::Claude);
        r.held_by = (state["held"] == 1).then(|| "m9".to_owned());
        rows.push(r);
    }
    (lab, Roster { rows })
}

/// `state` after a sync that left `synced`: its `row` and `held` read back, and the ghosts
/// (`dropped`, `unasked`, `missing`, `limited`) set where the roster breaks the law each names.
fn preference_project(
    state: &aterm_spec::interp::State,
    synced: &Roster,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    let rows: Vec<&Row> = synced
        .rows
        .iter()
        .filter(|r| r.matches_email(&email("x")))
        .collect();
    let row = i64::from(!rows.is_empty());
    let held = i64::from(rows.iter().any(|r| r.held_by.is_some()));
    let was_row = state["row"] == 1;
    let invited = state["pref"] == 1 && state["joinable"] == 1;
    if was_row && row == 0 {
        next.insert("dropped", 1);
    }
    if !was_row && row == 1 && state["pref"] == 0 {
        next.insert("unasked", 1);
    }
    if row == 0 && invited {
        next.insert("missing", 1);
    }
    if held == 1 {
        next.insert("limited", 1);
    }
    next.insert("row", row);
    next.insert("held", held);
    next
}

/// The `AlabCells` projection of `action` from an area-5 `state`: a `Sync` is the sync's
/// composition on the bed `state` stands for; every other action is the owner's verb or a
/// person's act, which sets the bed it names.
pub(crate) fn preference_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Sync" => {
            let (lab, existing) = preference_bed(state);
            return preference_project(state, &preference_sync(&lab, &existing));
        }
        "Prefer" => next.insert("pref", 1),
        "Unprefer" => next.insert("pref", 0),
        "Pause" => next.insert("joinable", 0),
        "Ask" => next.insert("row", 1),
        "Hold" => next.insert("held", 1),
        other => panic!("{other} is no preference action"),
    };
    next
}

/// Every step a sync from the lab takes is an `AlabCells` successor from every state the model
/// reaches in its preference area (§0: a preference never limits who may run an account): the
/// sync keeps a row this Mac does not prefer or that is paused, adds an account only where this
/// Mac prefers it and it may join, and clears every old hold.
/// NEGATIVE CONTROLS: a sync that skips [`retain`] keeps the hold an old pool file set on a row
/// this Mac does not prefer (the rebuild returns the roster as it was), breaking `NoHoldUnderLab`,
/// and is no `Sync` successor; a lab that ignores a pause (the account still
/// `joinable`) adds the paused account, no `Sync` successor either.
#[test]
fn a_sync_takes_the_preference_models_steps() {
    let model = aterm_models_db::alab_cells_model();
    let states: Vec<aterm_spec::interp::State> = aterm_spec::interp::reachable_states(&model)
        .into_iter()
        .filter(|state| state["area"] == 5)
        .collect();
    assert!(states.len() > 8, "{} preference states", states.len());
    for state in &states {
        for action in &model.actions {
            if !model.action_enabled(action.name, state) {
                continue;
            }
            let next = preference_transition(state, action.name);
            assert!(
                model.successors(action.name, state).contains(&next),
                "{} from {state:?} steps to {next:?}",
                action.name
            );
        }
    }

    // Without `retain`, the old hold stays.
    let held = states
        .iter()
        .find(|s| s["row"] == 1 && s["held"] == 1 && s["pref"] == 0)
        .expect("a held row this Mac does not prefer");
    let (lab, existing) = preference_bed(held);
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(&root.path().join("state"));
    let fleet = fleet_for(&lab, Cli::Claude, Some(&existing)).unwrap();
    let unretained =
        accounts_sync::build_machine_roster(&fleet, "m1", &store, Some(&existing), None);
    let next = preference_project(held, &unretained);
    assert_eq!(next["limited"], 1);
    assert!(!model.successors("Sync", held).contains(&next));

    // A pause the lab does not carry: the paused account joins.
    let paused = states
        .iter()
        .find(|s| s["row"] == 0 && s["pref"] == 1 && s["joinable"] == 0)
        .expect("a paused account this Mac prefers");
    let (mut lab, existing) = preference_bed(paused);
    lab.accounts[0].joinable = true;
    let next = preference_project(paused, &preference_sync(&lab, &existing));
    assert_eq!(next["row"], 1);
    assert!(!model.successors("Sync", paused).contains(&next));
}
