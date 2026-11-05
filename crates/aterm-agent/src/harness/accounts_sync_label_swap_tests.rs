// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A source that trades labels among this Mac's rows (m3, 2026-10-27): the vault's sheet gave the
//! default login's account `m3`, which the roster gave a row not signed in here, and that row's
//! account `m3-2`, the default login's label. Every sync refused ("the previous roster already
//! uses that label"), so `aterm init`'s `base.ai.accounts` stayed open. A sync now moves the
//! default login's state to its new label and the row whose label it takes over to the label the
//! source gives that row's account, along any cycle of labels. A row's login directory goes by
//! whose login it holds, as the profile in it says: a moved row keeps its own where that holds
//! nothing or its own account's login, and takes its new label's only where no other account's
//! login and no other row is there. A row given a directory by its label alone, in this sync or
//! a later one (new here, the default login's previous row, or a label the source gave another
//! account), takes its account's directory, its label's or a new `<label>~<n>`, whichever first
//! holds nothing or its own login and nothing else names; one of these that cannot be read
//! refuses the sync. An account is its provider identity (`identity_email`, else its login
//! address), the one a launch checks the profile against, so a source that lists it by another
//! address is the same account. Every fixture email is `example.test`.

use super::login_tests::{Desk, NOW};
use super::*;

const ANN: &str = "ann@example.test";
const BEA: &str = "bea@example.test";
const CID: &str = "cid@example.test";

/// The sheet as the vault delivers it on m3: Ann `m3`, Bea `m3.2`, Cid `m3.3`, and another
/// machine's row.
const SHEET: &str = "m,email,computer,claude\n\
                     1,ann@example.test,m3,1\n\
                     2,bea@example.test,m3.2,1\n\
                     3,cid@example.test,m3.3,1\n\
                     4,dov@example.test,m22,1\n";

fn row(label: &str, email: &str, dir: Option<PathBuf>) -> Row {
    Row {
        label: label.into(),
        email: Some(email.into()),
        dir: dir.map(|dir| dir.to_string_lossy().into_owned()),
        ..Row::default()
    }
}

fn account(label: &str, email: &str) -> FleetAccount {
    FleetAccount {
        label: label.into(),
        machines: vec!["m3".into()],
        class: Class::Preferred,
        email: email.into(),
        browser: None,
        vendor: accounts::Vendor::default(),
    }
}

/// m3 as it stood: the default login signed in as Ann, whose row is `m3-2` (signed in, a session
/// on it), Bea's row `m3` and Cid's `m3-3` each an empty login directory with a sign-in that found
/// no route; the fleet file beside the roster and aterm's own copy of it in `~/.secrets` (found on
/// this Mac) pair the same labels; and the vault's sheet beside that copy.
fn m3() -> Desk {
    let desk = Desk::new("m3");
    desk.default_login(ANN);
    let store = desk.store();
    let dir = |label: &str| store.login_dir(label).unwrap();
    for label in ["m3", "m3-3"] {
        aterm_types::fs_restricted::ensure_private_dir(&store.logins_dir()).unwrap();
        aterm_types::fs_restricted::ensure_private_dir(&dir(label)).unwrap();
    }
    let roster = Roster {
        rows: vec![
            row("m3-2", ANN, None),
            row("m3", BEA, Some(dir("m3"))),
            row("m3-3", CID, Some(dir("m3-3"))),
        ],
    };
    fs::write(desk.roster(), format_roster_toml(&roster).unwrap()).unwrap();
    let fleet = FleetRoster {
        source: FLEET_ROSTER_FILE.into(),
        accounts: vec![
            account("m3-2", ANN),
            account("m3", BEA),
            account("m3-3", CID),
        ],
        ..FleetRoster::default()
    }
    .to_toml_for(Some(OptIn::Discovered));
    fs::write(desk.fleet_file(), &fleet).unwrap();
    let secrets = desk.home.join(".secrets");
    aterm_types::fs_restricted::ensure_private_dir(&secrets).unwrap();
    fs::write(secrets.join(FLEET_ROSTER_FILE), &fleet).unwrap();
    fs::write(secrets.join(ACCOUNTS_CSV_FILE), SHEET).unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            state.accounts.insert(
                "m3-2".into(),
                accounts::Account {
                    auth: accounts::Auth::Ok,
                    auth_at: Some(NOW - 100),
                    ..accounts::Account::default()
                },
            );
            state.sessions.insert(
                "s-ann".into(),
                accounts::SessionRow {
                    account: "m3-2".into(),
                    at: NOW,
                    held: None,
                },
            );
            accounts::note_served_login(logins, "m3-2", NOW - 50);
            for label in ["m3", "m3-3"] {
                logins.labels.insert(
                    label.into(),
                    accounts::Binding {
                        sign_in_attempt: Some(accounts::SignInAttempt {
                            outcome: accounts::AttemptOutcome::NoRoute,
                            routes: format!("profile(none) for {label}"),
                            since: NOW - 10,
                            tried_at: NOW - 10,
                        }),
                        ..accounts::Binding::default()
                    },
                );
            }
            Ok(Vec::new())
        })
        .unwrap();
    desk
}

fn sync(desk: &Desk, now: i64) -> Result<(String, usize, usize, usize), String> {
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), now)
}

/// What `aterm init --check --only base` reads to call `base.ai.accounts` complete (the
/// `aterm` crate's `ai_accounts_claude_probe`): a roster with rows, the fleet file beside it, a
/// source it opts in by, and nothing new for a sync from init to read.
fn init_item_complete(desk: &Desk, now: i64) -> bool {
    let roster = desk.roster();
    Roster::load(&roster).is_ok_and(|r| !r.rows.is_empty())
        && desk.fleet_file().is_file()
        && configured_source(Some(&desk.config.join("aterm.toml")), Some(&roster)).is_some()
        && !needs_init_accounts_sync(&desk.home, &desk.state, &roster, now)
}

/// Every file the sync reads or writes, and which login directories exist.
fn records(desk: &Desk) -> Vec<Option<Vec<u8>>> {
    let store = desk.store();
    let secrets = desk.home.join(".secrets");
    let mut files: Vec<Option<Vec<u8>>> = [
        desk.roster(),
        desk.fleet_file(),
        secrets.join(FLEET_ROSTER_FILE),
        secrets.join(ACCOUNTS_CSV_FILE),
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.state_witness_file(),
        store.logins_witness_file(),
    ]
    .iter()
    .map(|path| fs::read(path).ok())
    .collect();
    let mut dirs: Vec<String> = fs::read_dir(store.logins_dir())
        .map(|entries| {
            entries
                .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    files.push(Some(dirs.join(",").into_bytes()));
    files
}

/// The m3 case: the sheet swaps the default login's label (`m3-2`) with the label of a row not
/// signed in here (`m3`). The sync publishes: Ann's row is `m3`, the default login's, with her
/// auth word, served login and session; Bea's is `m3-2` with her sign-in record and its own empty
/// login directory (`logins/m3`, which nothing names now, is gone); Cid's is unchanged; aterm's
/// copy in `~/.secrets` yields to the sheet; and `base.ai.accounts` reads complete. A second sync
/// writes nothing. CONTROL: before the sync the item is open.
#[test]
fn a_sheet_that_swaps_the_default_logins_label_publishes_and_the_init_item_completes() {
    let desk = m3();
    let store = desk.store();
    assert!(!init_item_complete(&desk, NOW), "CONTROL: the item is open");

    sync(&desk, NOW).expect("a swap with rows not signed in here publishes");

    let roster = desk.rows();
    let default = roster.row_for_login(None).expect("a default row");
    assert_eq!(default.label, "m3");
    assert_eq!(default.email.as_deref(), Some(ANN));
    let bea = roster.row("m3-2").expect("Bea's new row");
    assert_eq!(bea.email.as_deref(), Some(BEA));
    let bea_dir = store.login_dir("m3-2").unwrap();
    assert_eq!(bea.dir.as_deref(), Some(bea_dir.to_str().unwrap()));
    let cid = roster.row("m3-3").expect("Cid's row");
    assert_eq!(cid.email.as_deref(), Some(CID));
    assert_eq!(
        cid.dir.as_deref(),
        Some(store.login_dir("m3-3").unwrap().to_str().unwrap())
    );

    let (state, _) = store.load(NOW).unwrap();
    assert_eq!(
        state.account("m3").auth,
        accounts::Auth::Ok,
        "Ann's auth word"
    );
    assert!(
        !state.accounts.contains_key("m3-2"),
        "nothing of Ann's is left on Bea's label"
    );
    assert_eq!(state.sessions["s-ann"].account, "m3");
    let (logins, _) = store.load_logins(NOW).unwrap();
    assert_eq!(logins.labels["m3"].served_at, Some(NOW - 50), "Ann's login");
    assert!(logins.labels["m3"].sign_in_attempt.is_none());
    assert_eq!(
        logins.labels["m3-2"]
            .sign_in_attempt
            .as_ref()
            .map(|attempt| attempt.routes.as_str()),
        Some("profile(none) for m3"),
        "Bea's sign-in record moved with her"
    );
    assert!(logins.labels["m3-2"].served_at.is_none());

    assert!(bea_dir.is_dir() && !accounts::login_made(&bea_dir));
    assert!(
        !store.login_dir("m3").unwrap().exists(),
        "the empty directory no row names is removed"
    );
    assert!(store.login_dir("m3-3").unwrap().is_dir());
    assert!(!desk.home.join(".secrets").join(FLEET_ROSTER_FILE).exists());

    assert!(init_item_complete(&desk, NOW + 1));
    let after = records(&desk);
    sync(&desk, NOW + 2).expect("a second sync");
    assert_eq!(records(&desk), after, "a second sync writes nothing");
}

/// Rewrites the roster's row `label` to the login directory `dir`.
fn set_dir(desk: &Desk, label: &str, dir: &Path) {
    let mut roster = desk.rows();
    let row = roster
        .rows
        .iter_mut()
        .find(|row| row.label == label)
        .unwrap();
    row.dir = Some(dir.to_string_lossy().into_owned());
    fs::write(desk.roster(), format_roster_toml(&roster).unwrap()).unwrap();
}

fn dir_of(desk: &Desk, label: &str) -> Option<String> {
    desk.rows().row(label).and_then(|row| row.dir.clone())
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Signs `email` in at `dir` as Claude Code's sign-in leaves a login: a profile naming the
/// account. Returns the profile's text.
fn signed_in(dir: &Path, email: &str) -> String {
    fs::create_dir_all(dir.parent().unwrap()).unwrap();
    aterm_types::fs_restricted::ensure_private_dir(dir).unwrap();
    let profile = format!("{{\"oauthAccount\":{{\"emailAddress\":\"{email}\"}}}}\n");
    fs::write(dir.join(".claude.json"), &profile).unwrap();
    profile
}

/// The account the profile in `dir` names, if any.
fn signed_in_as(dir: &str) -> Option<String> {
    profile_email(&Path::new(dir).join(".claude.json"))
}

/// Every row of `desk`'s roster whose login directory's profile names another account than the
/// provider identity the row expects.
fn pointed_at_another(desk: &Desk) -> Vec<(String, String)> {
    desk.rows()
        .rows
        .iter()
        .filter_map(|row| {
            let held = signed_in_as(row.dir.as_deref()?)?;
            (!accounts::email_matches(row.expected_identity_email()?, &held))
                .then(|| (row.label.clone(), held))
        })
        .collect()
}

/// A row whose label is taken over and which holds its own login here keeps its login directory
/// under its new label: a sync moves no login, and roster directories need only be distinct.
/// Bea's login stays where it is, now `m3-2`'s, and `logins/m3-2` is not made. A second sync
/// writes nothing.
#[test]
fn a_moved_row_holding_a_login_keeps_its_directory() {
    let desk = m3();
    let store = desk.store();
    let bea_dir = store.login_dir("m3").unwrap();
    let bea_login = bea_dir.join(".claude.json");
    let bea_text = signed_in(&bea_dir, BEA);

    sync(&desk, NOW).expect("a row holding a login moves without its directory moving");

    assert_eq!(desk.rows().row_for_login(None).unwrap().label, "m3");
    assert_eq!(desk.rows().row("m3-2").unwrap().email.as_deref(), Some(BEA));
    assert_eq!(dir_of(&desk, "m3-2"), Some(text(&bea_dir)));
    assert_eq!(fs::read_to_string(&bea_login).unwrap(), bea_text);
    assert!(!store.login_dir("m3-2").unwrap().exists());
    assert!(init_item_complete(&desk, NOW + 1));
    let after = records(&desk);
    sync(&desk, NOW + 2).expect("a second sync");
    assert_eq!(records(&desk), after, "a second sync writes nothing");
}

/// The directory a moved row arrives at is checked: a stale login left in `logins/m3-2` (the
/// default login's old label, whose row named no directory) is not handed to Bea, who takes that
/// label. Bea keeps her own empty `logins/m3`, and the stale login is left where it is.
#[test]
fn a_stale_login_in_the_label_taken_is_not_handed_to_the_row_that_takes_it() {
    let desk = m3();
    let store = desk.store();
    let stale_dir = store.login_dir("m3-2").unwrap();
    aterm_types::fs_restricted::ensure_private_dir(&stale_dir).unwrap();
    let stale = stale_dir.join(".claude.json");
    let stale_text = format!("{{\"oauthAccount\":{{\"emailAddress\":\"{ANN}\"}}}}\n");
    fs::write(&stale, &stale_text).unwrap();

    sync(&desk, NOW).expect("the swap publishes");

    assert_eq!(desk.rows().row_for_login(None).unwrap().label, "m3");
    assert_eq!(desk.rows().row("m3-2").unwrap().email.as_deref(), Some(BEA));
    let bea_dir = store.login_dir("m3").unwrap();
    assert_eq!(dir_of(&desk, "m3-2"), Some(text(&bea_dir)));
    assert!(bea_dir.is_dir());
    assert_eq!(fs::read_to_string(&stale).unwrap(), stale_text);
}

/// A moved row whose roster directory is empty but whose renewal bound it to a login elsewhere
/// keeps its roster directory, and its binding follows it: the login it is served from is the
/// same directory under its new label.
#[test]
fn a_moved_row_whose_renewal_bound_login_holds_one_keeps_its_directory() {
    let desk = m3();
    let store = desk.store();
    let bea_dir = store.login_dir("m3").unwrap();
    let renewed = desk.home.join("renewed-bea");
    aterm_types::fs_restricted::ensure_private_dir(&renewed).unwrap();
    fs::write(renewed.join(".claude.json"), "{}\n").unwrap();
    store
        .transact_logins(NOW, &mut None, |_, logins| {
            let binding = logins.labels.get_mut("m3").unwrap();
            binding.dir = Some(text(&renewed));
            binding.was = vec![Some(text(&bea_dir))];
            Ok(Vec::new())
        })
        .unwrap();

    sync(&desk, NOW).expect("the swap publishes");

    let roster = desk.rows();
    assert_eq!(roster.row_for_login(None).unwrap().label, "m3");
    assert_eq!(dir_of(&desk, "m3-2"), Some(text(&bea_dir)));
    let bound = store.bound(&roster, NOW).unwrap();
    assert_eq!(
        bound.row("m3-2").unwrap().dir.as_deref(),
        Some(text(&renewed).as_str())
    );
    assert!(accounts::login_made(&renewed));
    assert!(bea_dir.is_dir());
}

/// A moved row whose empty login directory is one a person chose outside the store takes its new
/// label's own directory, and the one it leaves survives: a sync removes only a directory under
/// the store's `logins/`.
#[test]
fn a_moved_rows_empty_directory_outside_the_store_survives() {
    let desk = m3();
    let store = desk.store();
    let custom = desk.home.join("bea-login");
    aterm_types::fs_restricted::ensure_private_dir(&custom).unwrap();
    set_dir(&desk, "m3", &custom);

    sync(&desk, NOW).expect("the swap publishes");

    let bea_dir = store.login_dir("m3-2").unwrap();
    assert_eq!(dir_of(&desk, "m3-2"), Some(text(&bea_dir)));
    assert!(bea_dir.is_dir());
    assert!(custom.is_dir(), "a directory outside the store is kept");
}

/// A moved row's login directory that cannot be read is not read as holding no login: the sync
/// refuses, naming the labels and the directory, with nothing written. CONTROL: once it can be
/// read, the same sync publishes.
#[cfg(unix)]
#[test]
fn a_moved_rows_unreadable_directory_refuses_with_nothing_written() {
    use std::os::unix::fs::PermissionsExt as _;
    let desk = m3();
    let store = desk.store();
    let bea_dir = store.login_dir("m3").unwrap();
    fs::set_permissions(&bea_dir, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        fs::read_dir(&bea_dir).is_err(),
        "CONTROL: the directory cannot be read"
    );
    let before = records(&desk);
    let refused = sync(&desk, NOW);
    // Readable again before any assertion, whatever the sync did to it.
    let _ = fs::set_permissions(&bea_dir, fs::Permissions::from_mode(0o700));
    let why = refused.expect_err("an unreadable directory is not read as empty");
    for named in ["`m3-2`", "`m3`", bea_dir.to_str().unwrap()] {
        assert!(why.contains(named), "{named}: {why}");
    }
    assert_eq!(records(&desk), before, "nothing was written");

    sync(&desk, NOW + 1).expect("CONTROL: a readable directory publishes");
    assert_eq!(desk.rows().row_for_login(None).unwrap().label, "m3");
}

/// Any cycle of labels publishes, not a swap alone: the default login's account moves `m1` to
/// `m1-2`, the account there to `m1-3`, and the one there to `m1`. Each moved row takes its new
/// label's own login directory, and the one the cycle leaves unnamed (`logins/m1-2`) is removed.
/// A source that gives the label taken over to an account it no longer lists on this Mac is
/// still refused: what is on record of that account would have no label.
#[test]
fn a_cycle_of_labels_publishes_and_a_label_whose_account_left_is_refused() {
    let desk = Desk::new("m1");
    desk.default_login(ANN);
    let store = desk.store();
    let dir = |label: &str| store.login_dir(label).unwrap();
    for label in ["m1-2", "m1-3"] {
        aterm_types::fs_restricted::ensure_private_dir(&store.logins_dir()).unwrap();
        aterm_types::fs_restricted::ensure_private_dir(&dir(label)).unwrap();
    }
    let old = Roster {
        rows: vec![
            row("m1", ANN, None),
            row("m1-2", BEA, Some(dir("m1-2"))),
            row("m1-3", CID, Some(dir("m1-3"))),
        ],
    };
    let old_text = format_roster_toml(&old).unwrap();
    fs::write(desk.roster(), &old_text).unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            state.active = Some("m1".into());
            for (label, at) in [("m1", 1), ("m1-2", 2), ("m1-3", 3)] {
                accounts::note_served_login(logins, label, NOW - at);
            }
            Ok(Vec::new())
        })
        .unwrap();
    let refused = parse_accounting_csv(
        "email,computer,claude\nann@example.test,m1.2,1\ncid@example.test,m1.3,1\n\
         eve@example.test,m1,1\n",
        "left.csv",
    )
    .unwrap();
    let persist = |fleet: &FleetRoster| {
        persist_accounts_sync(
            &store,
            &desk.roster(),
            (fleet, OptIn::Named),
            ("m1", Some(ANN)),
            &BTreeMap::new(),
            NOW,
            (&[], &[]),
        )
    };
    let before = records(&desk);
    let why = persist(&refused)
        .err()
        .expect("Bea's account has no label on this Mac");
    for named in ["`m1`", "`m1-2`"] {
        assert!(why.contains(named), "{named}: {why}");
    }
    assert_eq!(records(&desk), before, "nothing was written");
    assert_eq!(fs::read_to_string(desk.roster()).unwrap(), old_text);

    let cycle = parse_accounting_csv(
        "email,computer,claude\nann@example.test,m1.2,1\nbea@example.test,m1.3,1\n\
         cid@example.test,m1,1\n",
        "cycle.csv",
    )
    .unwrap();
    persist(&cycle).expect("a cycle of labels publishes");
    let roster = desk.rows();
    let label_of = |email: &str| {
        roster
            .rows
            .iter()
            .find(|row| row.email.as_deref() == Some(email))
            .map(|row| (row.label.clone(), row.dir.clone()))
            .unwrap()
    };
    let own = |label: &str| Some(dir(label).to_string_lossy().into_owned());
    assert_eq!(label_of(ANN), ("m1-2".into(), None));
    assert_eq!(label_of(BEA), ("m1-3".into(), own("m1-3")));
    assert_eq!(label_of(CID), ("m1".into(), own("m1")));
    let (state, _) = store.load(NOW).unwrap();
    assert_eq!(state.active.as_deref(), Some("m1-2"), "Ann's selection");
    let (logins, _) = store.load_logins(NOW).unwrap();
    for (label, at) in [("m1-2", 1), ("m1-3", 2), ("m1", 3)] {
        assert_eq!(logins.labels[label].served_at, Some(NOW - at), "{label}");
    }
    assert!(dir("m1").is_dir() && dir("m1-3").is_dir());
    assert!(
        !dir("m1-2").exists(),
        "the directory the cycle leaves unnamed"
    );
}

/// A cycle through a row whose login directory is one a person chose and which holds its login: the
/// account taking over that row's label does not get its directory by label. The default login's
/// account moves `m1` to `m1-2`, Bea `m1-2` to `m1-3`, Cid `m1-3` (signed in at a directory of his
/// own) to `m1-4`, and Dov `m1-4` to `m1`. Cid keeps his directory and login; Bea and Dov, whose
/// directories are empty, take their new labels' own; the store directories they leave are gone.
#[test]
fn a_cycle_through_a_custom_directory_holding_a_login_keeps_it_with_its_account() {
    const DOV: &str = "dov@example.test";
    let desk = Desk::new("m1");
    desk.default_login(ANN);
    let store = desk.store();
    let dir = |label: &str| store.login_dir(label).unwrap();
    aterm_types::fs_restricted::ensure_private_dir(&store.logins_dir()).unwrap();
    for label in ["m1-2", "m1-4"] {
        aterm_types::fs_restricted::ensure_private_dir(&dir(label)).unwrap();
    }
    let cid_dir = desk.home.join("cid-login");
    aterm_types::fs_restricted::ensure_private_dir(&cid_dir).unwrap();
    let cid_login = cid_dir.join(".claude.json");
    let cid_text = signed_in(&cid_dir, CID);
    let old = Roster {
        rows: vec![
            row("m1", ANN, None),
            row("m1-2", BEA, Some(dir("m1-2"))),
            row("m1-3", CID, Some(cid_dir.clone())),
            row("m1-4", DOV, Some(dir("m1-4"))),
        ],
    };
    fs::write(desk.roster(), format_roster_toml(&old).unwrap()).unwrap();
    store
        .transact_logins(NOW, &mut None, |_, logins| {
            for (label, at) in [("m1", 1), ("m1-2", 2), ("m1-3", 3), ("m1-4", 4)] {
                accounts::note_served_login(logins, label, NOW - at);
            }
            Ok(Vec::new())
        })
        .unwrap();
    let cycle = parse_accounting_csv(
        "email,computer,claude\nann@example.test,m1.2,1\nbea@example.test,m1.3,1\n\
         cid@example.test,m1.4,1\ndov@example.test,m1,1\n",
        "cycle.csv",
    )
    .unwrap();
    persist_accounts_sync(
        &store,
        &desk.roster(),
        (&cycle, OptIn::Named),
        ("m1", Some(ANN)),
        &BTreeMap::new(),
        NOW,
        (&[], &[]),
    )
    .expect("a cycle through a row holding a login publishes");

    let roster = desk.rows();
    let placed = |email: &str| {
        roster
            .rows
            .iter()
            .find(|row| row.email.as_deref() == Some(email))
            .map(|row| (row.label.clone(), row.dir.clone()))
            .unwrap()
    };
    assert_eq!(placed(ANN), ("m1-2".into(), None));
    assert_eq!(placed(BEA), ("m1-3".into(), Some(text(&dir("m1-3")))));
    assert_eq!(placed(CID), ("m1-4".into(), Some(text(&cid_dir))));
    assert_eq!(placed(DOV), ("m1".into(), Some(text(&dir("m1")))));
    let (logins, _) = store.load_logins(NOW).unwrap();
    for (label, at) in [("m1-2", 1), ("m1-3", 2), ("m1-4", 3), ("m1", 4)] {
        assert_eq!(logins.labels[label].served_at, Some(NOW - at), "{label}");
    }
    assert_eq!(fs::read_to_string(&cid_login).unwrap(), cid_text);
    assert!(dir("m1").is_dir() && dir("m1-3").is_dir());
    assert!(!dir("m1-2").exists() && !dir("m1-4").exists());
}

/// A cycle where no moved row can take its new label's directory: a stale login is left in
/// `logins/m1` (the default login's old label, whose row named no directory), so Cid, moving
/// `m1-3` to `m1`, keeps `logins/m1-3`, and Bea, moving `m1-2` to `m1-3`, keeps `logins/m1-2`
/// rather than share the directory Cid still names. The roster publishes with every directory
/// distinct and the stale login untouched.
#[test]
fn a_cycle_whose_rows_cannot_take_their_new_labels_directories_keeps_their_own() {
    let desk = Desk::new("m1");
    desk.default_login(ANN);
    let store = desk.store();
    let dir = |label: &str| store.login_dir(label).unwrap();
    aterm_types::fs_restricted::ensure_private_dir(&store.logins_dir()).unwrap();
    for label in ["m1", "m1-2", "m1-3"] {
        aterm_types::fs_restricted::ensure_private_dir(&dir(label)).unwrap();
    }
    let stale = dir("m1").join(".claude.json");
    fs::write(&stale, "{}\n").unwrap();
    let old = Roster {
        rows: vec![
            row("m1", ANN, None),
            row("m1-2", BEA, Some(dir("m1-2"))),
            row("m1-3", CID, Some(dir("m1-3"))),
        ],
    };
    fs::write(desk.roster(), format_roster_toml(&old).unwrap()).unwrap();
    let cycle = parse_accounting_csv(
        "email,computer,claude\nann@example.test,m1.2,1\nbea@example.test,m1.3,1\n\
         cid@example.test,m1,1\n",
        "cycle.csv",
    )
    .unwrap();
    persist_accounts_sync(
        &store,
        &desk.roster(),
        (&cycle, OptIn::Named),
        ("m1", Some(ANN)),
        &BTreeMap::new(),
        NOW,
        (&[], &[]),
    )
    .expect("a cycle whose rows keep their directories publishes");

    let roster = desk.rows();
    assert_eq!(roster.row_for_login(None).unwrap().label, "m1-2");
    assert_eq!(roster.row("m1-3").unwrap().email.as_deref(), Some(BEA));
    assert_eq!(roster.row("m1").unwrap().email.as_deref(), Some(CID));
    assert_eq!(dir_of(&desk, "m1-3"), Some(text(&dir("m1-2"))));
    assert_eq!(dir_of(&desk, "m1"), Some(text(&dir("m1-3"))));
    assert_eq!(fs::read_to_string(&stale).unwrap(), "{}\n");
}

/// The directories every row of `desk`'s roster names, each once.
fn distinct_dirs(desk: &Desk) -> bool {
    let roster = desk.rows();
    let dirs: Vec<&str> = roster
        .rows
        .iter()
        .filter_map(|row| row.dir.as_deref())
        .collect();
    dirs.iter().copied().collect::<BTreeSet<_>>().len() == dirs.len()
}

/// A directory a moved row keeps is not handed to the row a later sync gives its old label by
/// that label alone (the hostile re-review's repro, 2026-10-28). The swap leaves Bea's login in
/// `logins/m3` under `m3-2`; then the default login signs in as Bea and the same sheet syncs
/// again, so Bea's row is the default login's (no directory) and Ann's, `m3`, which the previous
/// roster gave none, is given one by its label. Ann's row takes a new empty directory in the
/// store, never `logins/m3`, and is not on record as signed in; Bea's login is where it was; no
/// two rows share a directory; a third sync writes nothing. The same holds where Bea's directory
/// was emptied before the switch: the previous roster still names it.
#[test]
fn a_kept_directory_is_not_given_to_its_old_labels_row_once_its_account_is_the_default_logins() {
    for emptied in [false, true] {
        let desk = m3();
        let store = desk.store();
        let bea_dir = store.login_dir("m3").unwrap();
        let bea_login = bea_dir.join(".claude.json");
        let bea_text = format!("{{\"oauthAccount\":{{\"emailAddress\":\"{BEA}\"}}}}\n");
        fs::write(&bea_login, &bea_text).unwrap();
        sync(&desk, NOW).expect("the swap publishes");
        assert_eq!(
            dir_of(&desk, "m3-2"),
            Some(text(&bea_dir)),
            "CONTROL: Bea keeps her directory"
        );
        if emptied {
            fs::remove_file(&bea_login).unwrap();
        }

        desk.default_login(BEA);
        sync(&desk, NOW + 1).expect("the default login's switch to Bea publishes");

        let roster = desk.rows();
        let default = roster.row_for_login(None).expect("a default row");
        assert_eq!(
            (default.label.as_str(), default.email.as_deref()),
            ("m3-2", Some(BEA)),
            "emptied {emptied}"
        );
        let ann = roster.row("m3").expect("Ann's row");
        assert_eq!(ann.email.as_deref(), Some(ANN));
        let ann_dir = PathBuf::from(ann.dir.clone().expect("Ann's row names a directory"));
        assert_ne!(
            ann_dir, bea_dir,
            "emptied {emptied}: Ann is not given Bea's directory"
        );
        assert_eq!(ann_dir.parent(), Some(store.logins_dir().as_path()));
        assert!(ann_dir.is_dir() && !accounts::login_made(&ann_dir));
        assert!(distinct_dirs(&desk), "emptied {emptied}: {roster:?}");
        if !emptied {
            assert_eq!(fs::read_to_string(&bea_login).unwrap(), bea_text);
        }
        let (state, _) = store.load(NOW + 1).unwrap();
        assert_ne!(
            state.account("m3").auth,
            accounts::Auth::Ok,
            "emptied {emptied}"
        );

        let after = records(&desk);
        sync(&desk, NOW + 2).expect("a third sync");
        assert_eq!(
            records(&desk),
            after,
            "emptied {emptied}: a third sync writes nothing"
        );
    }
}

/// The sheet as it stood before the swap, with a fourth account, Eve, new to this Mac at `m3.4`.
const WITH_EVE: &str = "email,computer,claude\nann@example.test,m3.2,1\nbea@example.test,m3,1\n\
                        cid@example.test,m3.3,1\neve@example.test,m3.4,1\n";

fn persist_sheet(desk: &Desk, csv: &str) -> Result<SyncedAccounts, String> {
    let fleet = parse_accounting_csv(csv, "eve.csv").unwrap();
    persist_accounts_sync(
        &desk.store(),
        &desk.roster(),
        (&fleet, OptIn::Named),
        ("m3", Some(ANN)),
        &BTreeMap::new(),
        NOW,
        (&[], &[]),
    )
}

/// A row new to this Mac whose label's directory holds a login no row names (left by an account
/// that had the label before, whose profile names no account) is not given it: Eve takes the
/// first `logins/m3-4~<n>` that is empty, here `~2` since `~1` holds an entry too, and both are
/// left as they were.
#[test]
fn a_new_rows_label_directory_holding_a_login_is_not_given_to_it() {
    let desk = m3();
    let store = desk.store();
    let stale = |name: &str| {
        let dir = store.logins_dir().join(name);
        aterm_types::fs_restricted::ensure_private_dir(&dir).unwrap();
        fs::write(dir.join(".claude.json"), "{}\n").unwrap();
        dir
    };
    let (own, first) = (stale("m3-4"), stale("m3-4~1"));

    persist_sheet(&desk, WITH_EVE).expect("a new row publishes");

    assert_eq!(
        dir_of(&desk, "m3-4"),
        Some(text(&store.logins_dir().join("m3-4~2")))
    );
    assert!(store.logins_dir().join("m3-4~2").is_dir());
    for dir in [own, first] {
        assert_eq!(
            fs::read_to_string(dir.join(".claude.json")).unwrap(),
            "{}\n"
        );
    }
    assert!(distinct_dirs(&desk));
}

/// A row new to this Mac whose label's empty directory another account's binding names (an
/// earlier login directory of Cid's renewal) is not given it, and the sync is not refused for
/// it: Eve takes `logins/m3-4~1`.
#[test]
fn a_new_rows_label_directory_another_binding_names_is_not_given_to_it() {
    let desk = m3();
    let store = desk.store();
    let named = store.login_dir("m3-4").unwrap();
    store
        .transact_logins(NOW, &mut None, |_, logins| {
            logins.labels.get_mut("m3-3").unwrap().was = vec![Some(text(&named))];
            Ok(Vec::new())
        })
        .unwrap();

    persist_sheet(&desk, WITH_EVE).expect("a new row publishes");

    assert_eq!(
        dir_of(&desk, "m3-4"),
        Some(text(&store.logins_dir().join("m3-4~1")))
    );
    assert!(distinct_dirs(&desk));
}

/// The directory a moved row arrives at is checked against the bindings as they stand once every
/// move is made. In the cycle `m1` to `m1-2`, `m1-2` to `m1-3`, `m1-3` to `m1`, the account
/// leaving `m1-3` (Cid) has a binding whose directory, or an earlier one of which, is
/// `logins/m1-3`. Bea, taking `m1-3`, keeps `logins/m1-2` rather than take a directory Cid's
/// binding names, and the sync publishes; Cid takes `logins/m1`, and `logins/m1-3` is kept.
/// Before the moves that binding is filed under `m1-3`, Bea's new label, where it would not count
/// against her.
#[test]
fn a_moved_row_does_not_arrive_at_a_directory_the_leaving_accounts_binding_names() {
    for in_was in [false, true] {
        let desk = Desk::new("m1");
        desk.default_login(ANN);
        let store = desk.store();
        let dir = |label: &str| store.login_dir(label).unwrap();
        aterm_types::fs_restricted::ensure_private_dir(&store.logins_dir()).unwrap();
        for label in ["m1-2", "m1-3"] {
            aterm_types::fs_restricted::ensure_private_dir(&dir(label)).unwrap();
        }
        let old = Roster {
            rows: vec![
                row("m1", ANN, None),
                row("m1-2", BEA, Some(dir("m1-2"))),
                row("m1-3", CID, Some(dir("m1-3"))),
            ],
        };
        fs::write(desk.roster(), format_roster_toml(&old).unwrap()).unwrap();
        let cid_dir = text(&dir("m1-3"));
        store
            .transact_logins(NOW, &mut None, |_, logins| {
                let binding = if in_was {
                    accounts::Binding {
                        was: vec![Some(cid_dir.clone())],
                        ..accounts::Binding::default()
                    }
                } else {
                    accounts::Binding {
                        dir: Some(cid_dir.clone()),
                        ..accounts::Binding::default()
                    }
                };
                logins.labels.insert("m1-3".into(), binding);
                Ok(Vec::new())
            })
            .unwrap();
        let cycle = parse_accounting_csv(
            "email,computer,claude\nann@example.test,m1.2,1\nbea@example.test,m1.3,1\n\
             cid@example.test,m1,1\n",
            "cycle.csv",
        )
        .unwrap();
        persist_accounts_sync(
            &store,
            &desk.roster(),
            (&cycle, OptIn::Named),
            ("m1", Some(ANN)),
            &BTreeMap::new(),
            NOW,
            (&[], &[]),
        )
        .unwrap_or_else(|why| panic!("in was {in_was}: the cycle publishes: {why}"));

        let roster = desk.rows();
        let placed = |email: &str| {
            roster
                .rows
                .iter()
                .find(|row| row.email.as_deref() == Some(email))
                .map(|row| (row.label.clone(), row.dir.clone()))
                .unwrap()
        };
        assert_eq!(placed(ANN), ("m1-2".into(), None));
        assert_eq!(
            placed(BEA),
            ("m1-3".into(), Some(text(&dir("m1-2")))),
            "in was {in_was}"
        );
        assert_eq!(placed(CID), ("m1".into(), Some(text(&dir("m1")))));
        assert!(dir("m1-3").is_dir(), "a directory a binding names is kept");
    }
}

const EVE: &str = "eve@example.test";

/// A source that gives m3 the accounts `rows`, each `(label, email)`.
fn sheet(rows: &[(&str, &str)]) -> FleetRoster {
    FleetRoster {
        source: "sheet.toml".into(),
        accounts: rows
            .iter()
            .map(|(label, email)| account(label, email))
            .collect(),
        ..FleetRoster::default()
    }
}

/// A sync of `fleet` on `desk` while the default login is signed in as `default`.
fn persist_as(
    desk: &Desk,
    fleet: &FleetRoster,
    default: &str,
    now: i64,
) -> Result<SyncedAccounts, String> {
    desk.default_login(default);
    persist_accounts_sync(
        &desk.store(),
        &desk.roster(),
        (fleet, OptIn::Named),
        ("m3", Some(default)),
        &BTreeMap::new(),
        now,
        (&[], &[]),
    )
}

/// A moved row whose own directory holds nothing arrives at its new label's directory where that
/// holds its own account's login (as the profile there says), not only where it is empty: Bea,
/// taking `m3-2`, finds her own earlier login in `logins/m3-2` and takes it, and the empty
/// `logins/m3` she leaves is removed. CONTROL: the same directory holding Ann's login is not
/// handed to Bea (`a_stale_login_in_the_label_taken_is_not_handed_to_the_row_that_takes_it`).
#[test]
fn a_moved_row_arrives_at_its_new_labels_directory_holding_its_own_login() {
    let desk = m3();
    let store = desk.store();
    let own = store.login_dir("m3-2").unwrap();
    let bea_text = signed_in(&own, BEA);

    sync(&desk, NOW).expect("the swap publishes");

    assert_eq!(desk.rows().row("m3-2").unwrap().email.as_deref(), Some(BEA));
    assert_eq!(dir_of(&desk, "m3-2"), Some(text(&own)));
    assert_eq!(
        fs::read_to_string(own.join(".claude.json")).unwrap(),
        bea_text
    );
    assert!(!store.login_dir("m3").unwrap().exists());
}

/// A moved row whose own directory holds another account's login, or entries whose profile names
/// no account, does not keep it: the row is placed as a row given a directory by its label alone
/// is. Bea, moving `m3` to `m3-2`, takes a new empty `logins/m3-2`; what `logins/m3` holds is left
/// where it is; no row points at another account's login; a second sync writes nothing.
#[test]
fn a_moved_row_whose_directory_holds_another_accounts_login_does_not_keep_it() {
    for held in [Some(CID), None] {
        let desk = m3();
        let store = desk.store();
        let old = store.login_dir("m3").unwrap();
        let left = match held {
            Some(email) => signed_in(&old, email),
            None => {
                let profile = "{\"oauthAccount\":{}}\n".to_owned();
                fs::write(old.join(".claude.json"), &profile).unwrap();
                profile
            }
        };

        sync(&desk, NOW).expect("the swap publishes");

        let bea_dir = store.login_dir("m3-2").unwrap();
        assert_eq!(desk.rows().row("m3-2").unwrap().email.as_deref(), Some(BEA));
        assert_eq!(dir_of(&desk, "m3-2"), Some(text(&bea_dir)), "{held:?}");
        assert!(bea_dir.is_dir() && !accounts::login_made(&bea_dir));
        assert_eq!(fs::read_to_string(old.join(".claude.json")).unwrap(), left);
        assert!(pointed_at_another(&desk).is_empty(), "{held:?}");
        assert!(distinct_dirs(&desk));
        let after = records(&desk);
        sync(&desk, NOW + 1).expect("a second sync");
        assert_eq!(
            records(&desk),
            after,
            "{held:?}: a second sync writes nothing"
        );
    }
}

/// An account the source drops and lists again under the same label finds its own login: Bea,
/// signed in at `logins/m3-2`, leaves the sheet and comes back at `m3-2`, and her row names
/// `logins/m3-2` again, still on record as signed in, her login as it was; a further sync writes
/// nothing. CONTROL: another account given `m3-2` instead does not get Bea's login: Eve takes a
/// new empty `logins/m3-2~1` and is not on record as signed in.
#[test]
fn an_account_that_leaves_and_comes_back_finds_its_own_login() {
    for back in [BEA, EVE] {
        let desk = Desk::new("m3");
        let store = desk.store();
        let all = sheet(&[("m3", ANN), ("m3-2", BEA), ("m3-3", CID)]);
        persist_as(&desk, &all, ANN, NOW).expect("the first sync");
        let bea_dir = store.login_dir("m3-2").unwrap();
        assert_eq!(dir_of(&desk, "m3-2"), Some(text(&bea_dir)));
        let bea_text = signed_in(&bea_dir, BEA);
        store
            .transact_logins(NOW, &mut None, |state, _| {
                Ok(accounts::note_auth(state, "m3-2", accounts::Auth::Ok, NOW))
            })
            .unwrap();
        persist_as(&desk, &sheet(&[("m3", ANN), ("m3-3", CID)]), ANN, NOW + 10)
            .expect("Bea leaves the sheet");
        assert!(desk.rows().row("m3-2").is_none());

        let returned = sheet(&[("m3", ANN), ("m3-2", back), ("m3-3", CID)]);
        persist_as(&desk, &returned, ANN, NOW + 20).expect("the label is listed again");

        let auth = store.load(NOW + 20).unwrap().0.account("m3-2").auth;
        if back == BEA {
            assert_eq!(
                dir_of(&desk, "m3-2"),
                Some(text(&bea_dir)),
                "Bea's own login"
            );
            assert_eq!(auth, accounts::Auth::Ok, "Bea is still signed in");
        } else {
            let fresh = store.logins_dir().join("m3-2~1");
            assert_eq!(dir_of(&desk, "m3-2"), Some(text(&fresh)), "CONTROL: Eve");
            assert!(fresh.is_dir() && !accounts::login_made(&fresh));
            assert_ne!(auth, accounts::Auth::Ok, "CONTROL: Eve is not signed in");
        }
        assert_eq!(
            fs::read_to_string(bea_dir.join(".claude.json")).unwrap(),
            bea_text
        );
        assert!(pointed_at_another(&desk).is_empty(), "{back}");
        assert!(distinct_dirs(&desk));
        let after = records(&desk);
        persist_as(&desk, &returned, ANN, NOW + 30).expect("a further sync");
        assert_eq!(
            records(&desk),
            after,
            "{back}: a further sync writes nothing"
        );
    }
}

/// A roster file that is lost while the store and its logins stay: the same source syncs again,
/// and Bea and Cid each find their own login in their labels' directories, which hold them.
/// CONTROL: where Cid's directory holds entries whose profile names no account, it is not taken
/// (nothing says it is his): his row gets a new empty `logins/m3-3~1` and the directory is left
/// as it was.
#[test]
fn a_lost_roster_finds_each_accounts_own_login_again() {
    for cid_named in [true, false] {
        let desk = Desk::new("m3");
        let store = desk.store();
        let all = sheet(&[("m3", ANN), ("m3-2", BEA), ("m3-3", CID)]);
        persist_as(&desk, &all, ANN, NOW).expect("the first sync");
        let (bea_dir, cid_dir) = (
            store.login_dir("m3-2").unwrap(),
            store.login_dir("m3-3").unwrap(),
        );
        let bea_text = signed_in(&bea_dir, BEA);
        let cid_text = if cid_named {
            signed_in(&cid_dir, CID)
        } else {
            fs::write(cid_dir.join(".claude.json"), "{}\n").unwrap();
            "{}\n".to_owned()
        };
        fs::remove_file(desk.roster()).unwrap();

        persist_as(&desk, &all, ANN, NOW + 10).expect("a sync after the roster was lost");

        assert_eq!(dir_of(&desk, "m3-2"), Some(text(&bea_dir)), "Bea");
        let cid_now = if cid_named {
            cid_dir.clone()
        } else {
            store.logins_dir().join("m3-3~1")
        };
        assert_eq!(dir_of(&desk, "m3-3"), Some(text(&cid_now)), "{cid_named}");
        for (dir, text) in [(&bea_dir, &bea_text), (&cid_dir, &cid_text)] {
            assert_eq!(&fs::read_to_string(dir.join(".claude.json")).unwrap(), text);
        }
        assert!(pointed_at_another(&desk).is_empty());
        assert!(distinct_dirs(&desk));
    }
}

/// A source that trades two rows' labels while the default login stays Ann's: neither row
/// inherits the directory of the label it takes, which holds the other account's login. Cid,
/// now `m3-2`, keeps `logins/m3-3`, and Bea, now `m3-3`, keeps `logins/m3-2`; nobody points at
/// another account's login, each login is as it was, and a second sync writes nothing. The same
/// holds where neither has signed in yet.
#[test]
fn a_label_swap_between_rows_keeps_each_accounts_own_login() {
    for signed in [true, false] {
        let desk = Desk::new("m3");
        let store = desk.store();
        persist_as(
            &desk,
            &sheet(&[("m3", ANN), ("m3-2", BEA), ("m3-3", CID)]),
            ANN,
            NOW,
        )
        .expect("the first sync");
        let (bea_dir, cid_dir) = (
            store.login_dir("m3-2").unwrap(),
            store.login_dir("m3-3").unwrap(),
        );
        let profiles = signed.then(|| (signed_in(&bea_dir, BEA), signed_in(&cid_dir, CID)));
        let swapped = sheet(&[("m3", ANN), ("m3-2", CID), ("m3-3", BEA)]);

        persist_as(&desk, &swapped, ANN, NOW + 10).expect("the swap publishes");

        let roster = desk.rows();
        assert_eq!(roster.row("m3-2").unwrap().email.as_deref(), Some(CID));
        assert_eq!(roster.row("m3-3").unwrap().email.as_deref(), Some(BEA));
        assert_eq!(
            dir_of(&desk, "m3-2"),
            Some(text(&cid_dir)),
            "signed {signed}"
        );
        assert_eq!(
            dir_of(&desk, "m3-3"),
            Some(text(&bea_dir)),
            "signed {signed}"
        );
        assert!(pointed_at_another(&desk).is_empty(), "signed {signed}");
        assert!(distinct_dirs(&desk));
        if let Some((bea, cid)) = profiles {
            assert_eq!(
                fs::read_to_string(bea_dir.join(".claude.json")).unwrap(),
                bea
            );
            assert_eq!(
                fs::read_to_string(cid_dir.join(".claude.json")).unwrap(),
                cid
            );
        }
        let after = records(&desk);
        persist_as(&desk, &swapped, ANN, NOW + 20).expect("a second sync");
        assert_eq!(
            records(&desk),
            after,
            "signed {signed}: a second sync writes nothing"
        );
    }
}

/// The sheet trades `m3` and `m3-2` back while the default login signs in as Bea, in one sync
/// (the hostile review's R3): Bea's row becomes the default login's under `m3`, and Ann's row
/// under `m3-2` is not given the directory that holds Bea's login, whether Bea kept it through an
/// earlier swap (`logins/m3`) or it is the label's own (`logins/m3-2`), nor that directory once
/// emptied, which Bea's previous row still names. No row points at another account's login, and
/// Bea's login is as it was.
#[test]
fn a_swap_back_while_the_default_switches_points_no_row_at_another_accounts_login() {
    for (swapped_first, emptied) in [(true, false), (false, false), (false, true)] {
        let desk = Desk::new("m3");
        let s0 = sheet(&[("m3", ANN), ("m3-2", BEA), ("m3-3", CID)]);
        let s1 = sheet(&[("m3-2", ANN), ("m3", BEA), ("m3-3", CID)]);
        persist_as(&desk, if swapped_first { &s1 } else { &s0 }, ANN, NOW).expect("first");
        let bea_label = if swapped_first { "m3" } else { "m3-2" };
        let bea_dir = dir_of(&desk, bea_label).unwrap();
        let bea_text = signed_in(Path::new(&bea_dir), BEA);
        if swapped_first {
            persist_as(&desk, &s0, ANN, NOW + 10).expect("the swap");
            assert_eq!(
                dir_of(&desk, "m3-2"),
                Some(bea_dir.clone()),
                "CONTROL: kept"
            );
        }

        if emptied {
            fs::remove_file(Path::new(&bea_dir).join(".claude.json")).unwrap();
        }

        persist_as(&desk, &s1, BEA, NOW + 20).expect("the swap back publishes");

        let roster = desk.rows();
        let default = roster.row_for_login(None).unwrap();
        assert_eq!(
            (default.label.as_str(), default.email.as_deref()),
            ("m3", Some(BEA))
        );
        assert_eq!(roster.row("m3-2").unwrap().email.as_deref(), Some(ANN));
        let ann_dir = dir_of(&desk, "m3-2").expect("Ann's row names a directory");
        assert_ne!(
            ann_dir, bea_dir,
            "swapped first {swapped_first}, emptied {emptied}"
        );
        assert!(
            pointed_at_another(&desk).is_empty(),
            "swapped first {swapped_first}: {roster:?}"
        );
        assert!(distinct_dirs(&desk));
        if !emptied {
            assert_eq!(
                fs::read_to_string(Path::new(&bea_dir).join(".claude.json")).unwrap(),
                bea_text
            );
        }
    }
}

/// A new directory a row is given (`logins/<label>~<n>`) bears a name no label can take and no
/// renewal stages under, and its account finds it again by the login in it. The swap leaves
/// Bea's login in `logins/m3`; the default login signs in as Bea, so Ann's `m3` takes a new
/// directory, and Ann signs in there. When the default login is Ann's again that directory is
/// named by no row, and when it is Bea's once more Ann's row finds her login there rather than
/// a new one. While it is named by no row, `accounts add` of its name is refused with nothing
/// written, and `accounts add m3.1` (a label beside it) gets `logins/m3.1`, leaving Ann's login
/// as it was.
#[test]
fn a_new_directory_is_named_apart_from_every_label_and_found_again_by_its_account() {
    let desk = Desk::new("m3");
    let store = desk.store();
    let s0 = sheet(&[("m3-2", ANN), ("m3", BEA), ("m3-3", CID)]);
    let s1 = sheet(&[("m3", ANN), ("m3-2", BEA), ("m3-3", CID)]);
    persist_as(&desk, &s0, ANN, NOW).expect("first");
    signed_in(&store.login_dir("m3").unwrap(), BEA);
    persist_as(&desk, &s1, ANN, NOW + 10).expect("the swap");
    persist_as(&desk, &s1, BEA, NOW + 20).expect("the default login signs in as Bea");
    let fresh = PathBuf::from(dir_of(&desk, "m3").expect("Ann's row names a directory"));
    assert_eq!(fresh.parent(), Some(store.logins_dir().as_path()));
    let ann_text = signed_in(&fresh, ANN);

    persist_as(&desk, &s1, ANN, NOW + 30).expect("the default login is Ann's again");
    assert_eq!(desk.rows().row_for_login(None).unwrap().label, "m3");
    persist_as(&desk, &s1, BEA, NOW + 40).expect("and Bea's once more");
    assert_eq!(
        dir_of(&desk, "m3"),
        Some(text(&fresh)),
        "Ann finds her login"
    );
    assert!(pointed_at_another(&desk).is_empty());
    persist_as(&desk, &s1, ANN, NOW + 50).expect("Ann's again");
    assert!(
        !desk
            .rows()
            .rows
            .iter()
            .any(|row| row.dir.as_deref() == fresh.to_str()),
        "CONTROL: no row names it"
    );

    let name = fresh.file_name().unwrap().to_str().unwrap().to_owned();
    let before = records(&desk);
    let (code, _, why) = super::resync_tests::add(&desk, &name, Some(EVE));
    assert_ne!(code, ExitCode::SUCCESS, "`accounts add {name}` adopted it");
    assert_eq!(records(&desk), before, "{why}");
    let (code, _, why) = super::resync_tests::add(&desk, "m3.1", Some(EVE));
    assert_eq!(code, ExitCode::SUCCESS, "{why}");
    assert_eq!(
        dir_of(&desk, "m3.1"),
        Some(text(&store.logins_dir().join("m3.1")))
    );
    assert_eq!(
        fs::read_to_string(fresh.join(".claude.json")).unwrap(),
        ann_text
    );
    assert_eq!(name, "m3~1");
    assert!(accounts::label_refusal(&name).is_some() && !name.contains('+'));
}

/// Bea's login address as an alias: her provider identity is [`BEA`].
const BEA_ALIAS: &str = "bea+m3@example.test";

/// Gives the roster's row `label` the provider identity `email` (`identity_email`), as `accounts
/// add --identity-email` records an alias's account.
fn set_identity(desk: &Desk, label: &str, email: &str) {
    let mut roster = desk.rows();
    let row = roster
        .rows
        .iter_mut()
        .find(|row| row.label == label)
        .unwrap();
    row.identity_email = Some(email.into());
    fs::write(desk.roster(), format_roster_toml(&roster).unwrap()).unwrap();
}

/// m3 with Ann's default login, Bea's row `m3-2` listed by her alias [`BEA_ALIAS`] with her
/// provider identity [`BEA`], and Cid's `m3-3` signed in as Cid. Returns the directories of
/// Bea's and Cid's rows, `logins/m3-2` (left for each test to fill) and `logins/m3-3`.
fn alias_desk() -> (Desk, PathBuf, PathBuf) {
    let desk = Desk::new("m3");
    let store = desk.store();
    let first = sheet(&[("m3", ANN), ("m3-2", BEA_ALIAS), ("m3-3", CID)]);
    persist_as(&desk, &first, ANN, NOW).expect("the first sync");
    set_identity(&desk, "m3-2", BEA);
    let (bea_dir, cid_dir) = (
        store.login_dir("m3-2").unwrap(),
        store.login_dir("m3-3").unwrap(),
    );
    assert_eq!(dir_of(&desk, "m3-2"), Some(text(&bea_dir)));
    signed_in(&cid_dir, CID);
    (desk, bea_dir, cid_dir)
}

/// A source that lists an alias's account by the address its provider identity is (Bea's row
/// `m3-2`, listed as `bea+m3@` and signed in as `bea@`, now listed as `bea@` at `m3-2`) names
/// the same account: her row keeps `logins/m3-2`, whatever the profile there says (one naming no
/// account too, as a row of an unchanged address keeps its directory), and she is still on
/// record as signed in; her login is as it was and a second sync writes nothing (the hostile
/// review's T8). CONTROL: another account given `m3-2` (Eve) does not get that directory: she
/// takes a new empty `logins/m3-2~1` and is not on record as signed in.
#[test]
fn a_new_login_address_for_the_same_identity_keeps_the_rows_own_login() {
    for (listed_as, profile) in [(BEA, Some(BEA)), (BEA, None), (EVE, Some(BEA))] {
        let (desk, bea_dir, _) = alias_desk();
        let store = desk.store();
        let left = match profile {
            Some(email) => signed_in(&bea_dir, email),
            None => {
                aterm_types::fs_restricted::ensure_private_dir(&bea_dir).unwrap();
                fs::write(bea_dir.join(".claude.json"), "{}\n").unwrap();
                "{}\n".to_owned()
            }
        };
        store
            .transact_logins(NOW, &mut None, |state, _| {
                Ok(accounts::note_auth(state, "m3-2", accounts::Auth::Ok, NOW))
            })
            .unwrap();
        let listed = sheet(&[("m3", ANN), ("m3-2", listed_as), ("m3-3", CID)]);

        persist_as(&desk, &listed, ANN, NOW + 10).expect("the sync publishes");

        let case = format!("{listed_as} over {profile:?}");
        assert_eq!(
            desk.rows().row("m3-2").unwrap().email.as_deref(),
            Some(listed_as)
        );
        let auth = store.load(NOW + 10).unwrap().0.account("m3-2").auth;
        if listed_as == BEA {
            assert_eq!(dir_of(&desk, "m3-2"), Some(text(&bea_dir)), "{case}");
            assert_eq!(auth, accounts::Auth::Ok, "{case}: still signed in");
        } else {
            let fresh = store.logins_dir().join("m3-2~1");
            assert_eq!(dir_of(&desk, "m3-2"), Some(text(&fresh)), "CONTROL: {case}");
            assert!(fresh.is_dir() && !accounts::login_made(&fresh));
            assert_ne!(auth, accounts::Auth::Ok, "CONTROL: {case}: not signed in");
        }
        assert_eq!(
            fs::read_to_string(bea_dir.join(".claude.json")).unwrap(),
            left
        );
        assert!(pointed_at_another(&desk).is_empty(), "{case}");
        assert!(distinct_dirs(&desk));
        let after = records(&desk);
        persist_as(&desk, &listed, ANN, NOW + 20).expect("a second sync");
        assert_eq!(
            records(&desk),
            after,
            "{case}: a second sync writes nothing"
        );
    }
}

/// An account whose login address and label change in one sync finds its own login by the
/// provider identity its row expected: Bea, listed as `bea+m3@` at `m3-2` and signed in there as
/// `bea@`, is now listed as `bea@` at `m3-3`, and Cid takes `m3-2`. Bea's row names
/// `logins/m3-2` and Cid's keeps `logins/m3-3`; each login is as it was and a second sync writes
/// nothing. CONTROL: where `logins/m3-2` holds another account's login (Eve's), Bea's row is not
/// given it and takes a new empty `logins/m3-3~1`.
#[test]
fn an_accounts_own_login_is_found_by_its_identity_across_a_new_address_and_label() {
    for profile in [BEA, EVE] {
        let (desk, bea_dir, cid_dir) = alias_desk();
        let store = desk.store();
        let bea_text = signed_in(&bea_dir, profile);
        let cid_text = fs::read_to_string(cid_dir.join(".claude.json")).unwrap();
        let traded = sheet(&[("m3", ANN), ("m3-2", CID), ("m3-3", BEA)]);

        persist_as(&desk, &traded, ANN, NOW + 10).expect("the trade publishes");

        let roster = desk.rows();
        assert_eq!(roster.row("m3-3").unwrap().email.as_deref(), Some(BEA));
        assert_eq!(dir_of(&desk, "m3-2"), Some(text(&cid_dir)), "{profile}");
        let bea_now = if profile == BEA {
            bea_dir.clone()
        } else {
            store.logins_dir().join("m3-3~1")
        };
        assert_eq!(dir_of(&desk, "m3-3"), Some(text(&bea_now)), "{profile}");
        if profile == EVE {
            assert!(!accounts::login_made(&bea_now), "CONTROL: a new directory");
        }
        for (dir, text) in [(&bea_dir, &bea_text), (&cid_dir, &cid_text)] {
            assert_eq!(&fs::read_to_string(dir.join(".claude.json")).unwrap(), text);
        }
        assert!(pointed_at_another(&desk).is_empty(), "{profile}");
        assert!(distinct_dirs(&desk));
        let after = records(&desk);
        persist_as(&desk, &traded, ANN, NOW + 20).expect("a second sync");
        assert_eq!(
            records(&desk),
            after,
            "{profile}: a second sync writes nothing"
        );
    }
}

/// A label swap where one row is an alias's holds the directory that row keeps to the provider
/// identity a launch checks, not to its login address: Bea's row, listed as `bea+m3@` with the
/// identity `bea@`, trades `m3-2` for Cid's `m3-3` and keeps `logins/m3-2`, whose profile names
/// `bea@`. CONTROL: a profile there naming the address `bea+m3@`, which a launch of her row
/// refuses as another account's, is not taken: her row gets a new empty `logins/m3-3~1`, and the
/// login is left where it is.
#[test]
fn a_label_swap_holds_an_alias_rows_directory_to_the_identity_its_profile_names() {
    for profile in [BEA, BEA_ALIAS] {
        let (desk, bea_dir, cid_dir) = alias_desk();
        let store = desk.store();
        let left = signed_in(&bea_dir, profile);
        let swapped = sheet(&[("m3", ANN), ("m3-2", CID), ("m3-3", BEA_ALIAS)]);

        persist_as(&desk, &swapped, ANN, NOW + 10).expect("the swap publishes");

        let roster = desk.rows();
        let bea = roster.row("m3-3").unwrap();
        assert_eq!(
            (bea.email.as_deref(), bea.identity_email.as_deref()),
            (Some(BEA_ALIAS), Some(BEA))
        );
        assert_eq!(dir_of(&desk, "m3-2"), Some(text(&cid_dir)), "{profile}");
        let bea_now = if profile == BEA {
            bea_dir.clone()
        } else {
            store.logins_dir().join("m3-3~1")
        };
        assert_eq!(dir_of(&desk, "m3-3"), Some(text(&bea_now)), "{profile}");
        if profile == BEA_ALIAS {
            assert!(!accounts::login_made(&bea_now), "CONTROL: a new directory");
        }
        assert_eq!(
            fs::read_to_string(bea_dir.join(".claude.json")).unwrap(),
            left
        );
        assert!(pointed_at_another(&desk).is_empty(), "{profile}");
        assert!(distinct_dirs(&desk));
    }
}

/// A legacy row (one that names no login address) keeps its login directory when a source
/// assigns it an account: Bea, given `m3-2`, whose row named `logins/m3-2` and no address, keeps
/// that directory and the login in it. CONTROL: where the row there was another account's (Eve),
/// whose row names that directory, Bea is not given it and takes a new empty `logins/m3-2~1`.
#[test]
fn a_legacy_rows_directory_stays_with_the_account_a_source_assigns_it() {
    for before in [None, Some(EVE)] {
        let desk = Desk::new("m3");
        let store = desk.store();
        let bea_dir = store.login_dir("m3-2").unwrap();
        let left = signed_in(&bea_dir, BEA);
        let mut was = row("m3-2", EVE, Some(bea_dir.clone()));
        was.email = before.map(str::to_owned);
        let old = Roster {
            rows: vec![row("m3", ANN, None), was],
        };
        fs::write(desk.roster(), format_roster_toml(&old).unwrap()).unwrap();
        let listed = sheet(&[("m3", ANN), ("m3-2", BEA)]);

        persist_as(&desk, &listed, ANN, NOW).expect("the sync publishes");

        assert_eq!(desk.rows().row("m3-2").unwrap().email.as_deref(), Some(BEA));
        let bea_now = match before {
            None => bea_dir.clone(),
            Some(_) => store.logins_dir().join("m3-2~1"),
        };
        assert_eq!(dir_of(&desk, "m3-2"), Some(text(&bea_now)), "{before:?}");
        if before.is_some() {
            assert!(!accounts::login_made(&bea_now), "CONTROL: a new directory");
        }
        assert_eq!(
            fs::read_to_string(bea_dir.join(".claude.json")).unwrap(),
            left
        );
        assert!(distinct_dirs(&desk));
    }
}

/// Makes `path` unreadable to its owner and returns a guard that makes it readable again, before
/// any assertion that follows it fails.
#[cfg(unix)]
struct Locked<'a>(&'a Path, u32);

#[cfg(unix)]
impl<'a> Locked<'a> {
    fn new(path: &'a Path) -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
        Self(path, mode)
    }
}

#[cfg(unix)]
impl Drop for Locked<'_> {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = fs::set_permissions(self.0, fs::Permissions::from_mode(self.1));
    }
}

#[cfg(windows)]
struct Locked {
    _file: fs::File,
}

#[cfg(windows)]
impl Locked {
    fn new(path: &Path) -> Self {
        use std::os::windows::fs::OpenOptionsExt as _;
        let _file = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .unwrap();
        Self { _file }
    }
}

/// A directory a row could be given that cannot be read is not passed over for a new one, which
/// would leave the row's own login, if it is there, behind for good (the hostile review's T3b):
/// Bea, signed in at `logins/m3-2`, leaves the sheet and comes back at `m3-2` while that
/// directory, or the profile in it, cannot be read. The sync refuses, naming the label and the
/// directory and asking for it to be made readable, with nothing written. CONTROL: once it can be
/// read, the same sync publishes and Bea's row names her own login again.
#[test]
fn an_unreadable_directory_a_row_could_be_given_refuses_with_nothing_written() {
    #[cfg(unix)]
    let cases: &[bool] = &[true, false];
    #[cfg(not(unix))]
    let cases: &[bool] = &[false];
    for &whole in cases {
        let desk = Desk::new("m3");
        let store = desk.store();
        let all = sheet(&[("m3", ANN), ("m3-2", BEA), ("m3-3", CID)]);
        persist_as(&desk, &all, ANN, NOW).expect("the first sync");
        let bea_dir = store.login_dir("m3-2").unwrap();
        let bea_text = signed_in(&bea_dir, BEA);
        persist_as(&desk, &sheet(&[("m3", ANN), ("m3-3", CID)]), ANN, NOW + 10)
            .expect("Bea leaves the sheet");
        let profile = bea_dir.join(".claude.json");
        let before = records(&desk);
        let refused = {
            let _locked = Locked::new(if whole { &bea_dir } else { &profile });
            persist_as(&desk, &all, ANN, NOW + 20)
        };
        let why = refused
            .err()
            .expect("an unreadable directory is not passed over");
        for named in [
            "`m3-2`",
            bea_dir.to_str().unwrap(),
            "make that directory readable",
        ] {
            assert!(why.contains(named), "whole {whole}: {named}: {why}");
        }
        assert_eq!(records(&desk), before, "whole {whole}: nothing was written");

        persist_as(&desk, &all, ANN, NOW + 30).expect("CONTROL: a readable directory publishes");
        assert_eq!(dir_of(&desk, "m3-2"), Some(text(&bea_dir)), "whole {whole}");
        assert_eq!(fs::read_to_string(&profile).unwrap(), bea_text);
    }
}

/// A moved row's new label's directory that cannot be read and that nothing else names is not
/// passed over either: in the m3 swap Bea's own `logins/m3` is empty, and `logins/m3-2`, which
/// holds her earlier login, or the profile in it, cannot be read, so whose login it holds is
/// unknown. The sync refuses, naming the labels and the directory, with nothing written.
/// CONTROL: once it can be read, the same sync publishes and Bea takes it.
#[test]
fn a_moved_rows_unreadable_new_label_directory_refuses_with_nothing_written() {
    #[cfg(unix)]
    let cases: &[bool] = &[true, false];
    #[cfg(not(unix))]
    let cases: &[bool] = &[false];
    for &whole in cases {
        let desk = m3();
        let store = desk.store();
        let wanted = store.login_dir("m3-2").unwrap();
        let bea_text = signed_in(&wanted, BEA);
        let profile = wanted.join(".claude.json");
        let before = records(&desk);
        let refused = {
            let _locked = Locked::new(if whole { &wanted } else { &profile });
            sync(&desk, NOW)
        };
        let why = refused.expect_err("an unreadable directory is not passed over");
        for named in ["`m3-2`", "`m3`", wanted.to_str().unwrap()] {
            assert!(why.contains(named), "whole {whole}: {named}: {why}");
        }
        assert_eq!(records(&desk), before, "whole {whole}: nothing was written");

        sync(&desk, NOW + 1).expect("CONTROL: a readable directory publishes");
        assert_eq!(dir_of(&desk, "m3-2"), Some(text(&wanted)), "whole {whole}");
        assert_eq!(fs::read_to_string(&profile).unwrap(), bea_text);
    }
}

/// Two renewed accounts trade labels while the default stays Ann's (ATERM-KI-1028-6): each
/// account's record and renewal binding move with it, so each is still on record as signed in,
/// its row keeps its own login directory, and a launch is served its own staging login.
/// Before, each was forgotten under its new label, given a new empty directory, and served
/// the other's renewal. CONTROL: no row is named or served another account's login, and a
/// second sync writes nothing.
#[test]
fn a_label_trade_between_renewed_rows_moves_each_accounts_record_and_renewal() {
    let desk = Desk::new("m3");
    let store = desk.store();
    let all = sheet(&[("m3", ANN), ("m3-2", BEA), ("m3-3", CID)]);
    persist_as(&desk, &all, ANN, NOW).expect("the first sync");
    let mut staged_of = Vec::new();
    for (label, email) in [("m3-2", BEA), ("m3-3", CID)] {
        let own = store.login_dir(label).unwrap();
        signed_in(&own, email);
        let staged = store.logins_dir().join(format!("{label}+1"));
        signed_in(&staged, email);
        staged_of.push((label, own, staged));
    }
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            for (label, own, staged) in &staged_of {
                logins.labels.insert(
                    (*label).into(),
                    accounts::Binding {
                        dir: Some(text(staged)),
                        was: vec![Some(text(own))],
                        renewed_at: Some(NOW),
                        ..accounts::Binding::default()
                    },
                );
                let _ =
                    accounts::note_auth_in_context(state, logins, label, accounts::Auth::Ok, NOW);
            }
            Ok(Vec::new())
        })
        .unwrap();

    let swapped = sheet(&[("m3", ANN), ("m3-2", CID), ("m3-3", BEA)]);
    persist_as(&desk, &swapped, ANN, NOW + 10).expect("the trade publishes");

    let served = store.bound(&desk.rows(), NOW + 10).unwrap();
    let state = store.load(NOW + 10).unwrap().0;
    assert_eq!(
        dir_of(&desk, "m3-2"),
        Some(text(&staged_of[1].1)),
        "Cid's row keeps his directory"
    );
    assert_eq!(
        dir_of(&desk, "m3-3"),
        Some(text(&staged_of[0].1)),
        "Bea's row keeps hers"
    );
    assert_eq!(
        served.row("m3-2").unwrap().dir.as_deref(),
        Some(text(&staged_of[1].2).as_str()),
        "Cid is served his own renewal"
    );
    assert_eq!(
        served.row("m3-3").unwrap().dir.as_deref(),
        Some(text(&staged_of[0].2).as_str()),
        "Bea is served hers"
    );
    assert_eq!(
        state.account("m3-2").auth,
        accounts::Auth::Ok,
        "Cid is still signed in"
    );
    assert_eq!(
        state.account("m3-3").auth,
        accounts::Auth::Ok,
        "Bea is still signed in"
    );
    assert!(pointed_at_another(&desk).is_empty());
    assert!(distinct_dirs(&desk));
    let after = records(&desk);
    persist_as(&desk, &swapped, ANN, NOW + 20).expect("a second sync");
    assert_eq!(records(&desk), after, "a second sync writes nothing");
}

/// A renewed account the source drops, and a new account listed at its label (ATERM-KI-1028-6):
/// the departed binding keeps its staging directory only in `was`, so the label's new account
/// is neither served that renewal nor on record as signed in on the departed account's record,
/// and the staging login stands where it was. CONTROL: the departed account's own record
/// stands under nothing the new account reads, and no row points at another's login.
#[test]
fn a_new_account_at_a_departed_accounts_label_is_not_served_its_renewal() {
    let desk = Desk::new("m3");
    let store = desk.store();
    let all = sheet(&[("m3", ANN), ("m3-2", BEA), ("m3-3", CID)]);
    persist_as(&desk, &all, ANN, NOW).expect("the first sync");
    let own = store.login_dir("m3-2").unwrap();
    signed_in(&own, BEA);
    let staged = store.logins_dir().join("m3-2+1");
    let staged_text = signed_in(&staged, BEA);
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            logins.labels.insert(
                "m3-2".into(),
                accounts::Binding {
                    dir: Some(text(&staged)),
                    was: vec![Some(text(&own))],
                    renewed_at: Some(NOW),
                    ..accounts::Binding::default()
                },
            );
            let _ = accounts::note_auth_in_context(state, logins, "m3-2", accounts::Auth::Ok, NOW);
            Ok(Vec::new())
        })
        .unwrap();

    persist_as(&desk, &sheet(&[("m3", ANN), ("m3-3", CID)]), ANN, NOW + 10)
        .expect("Bea leaves the sheet");
    let (logins, _) = store.load_logins(NOW + 10).unwrap();
    let binding = logins.get("m3-2").expect("Bea's binding stands");
    assert_eq!(binding.dir, None, "her renewal is no longer served");
    assert!(
        binding.was.contains(&Some(text(&staged))),
        "kept in was: {:?}",
        binding.was
    );

    let returned = sheet(&[("m3", ANN), ("m3-2", EVE), ("m3-3", CID), ("m3-4", BEA)]);
    persist_as(&desk, &returned, ANN, NOW + 20).expect("Eve is listed");
    let served = store.bound(&desk.rows(), NOW + 20).unwrap();
    assert_ne!(
        served.row("m3-2").unwrap().dir.as_deref(),
        Some(text(&staged).as_str()),
        "CONTROL: Eve is not served Bea's renewal"
    );
    assert_ne!(
        store.load(NOW + 20).unwrap().0.account("m3-2").auth,
        accounts::Auth::Ok,
        "CONTROL: Eve is not on Bea's record"
    );
    assert_eq!(
        fs::read_to_string(staged.join(".claude.json")).unwrap(),
        staged_text
    );
    assert!(pointed_at_another(&desk).is_empty());
    assert!(distinct_dirs(&desk));
}

const RETURNED_ID: &str = "00000000-0000-0000-0000-0000000000b2";

fn verified_account(staged: bool) -> (Desk, PathBuf, super::super::profile::AccountKey, String) {
    let desk = Desk::new("m3");
    let store = desk.store();
    let all = sheet(&[("m3", ANN), ("m3-2", BEA), ("m3-3", CID)]);
    persist_as(&desk, &all, ANN, NOW).unwrap();
    let dir = if staged {
        store.logins_dir().join("m3-2+1")
    } else {
        store.login_dir("m3-2").unwrap()
    };
    signed_in(&dir, BEA);
    let id = RETURNED_ID;
    let profile =
        format!("{{\"oauthAccount\":{{\"emailAddress\":\"{BEA}\",\"accountUuid\":\"{id}\"}}}}\n");
    fs::write(dir.join(".claude.json"), &profile).unwrap();
    let key = super::super::profile::AccountKey::claude(id).unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            logins.labels.insert(
                "m3-2".into(),
                accounts::Binding {
                    dir: Some(text(&dir)),
                    renewed_at: Some(NOW),
                    was: if staged {
                        vec![store.login_dir("m3-2").map(|dir| text(&dir))]
                    } else {
                        Vec::new()
                    },
                    ..accounts::Binding::default()
                },
            );
            let _ = accounts::note_auth_in_context(state, logins, "m3-2", accounts::Auth::Ok, NOW);
            let record = state.accounts.get_mut("m3-2").unwrap();
            record.key = Some(key);
            record.walls.insert(
                "weekly".into(),
                accounts::Wall {
                    until: Some(NOW + 3600),
                    at: NOW,
                    by: "m3".into(),
                    attempts: 1,
                    cleared: false,
                },
            );
            Ok(Vec::new())
        })
        .unwrap();
    (desk, dir, key, profile)
}

fn departed_verified_account() -> (Desk, PathBuf, super::super::profile::AccountKey, String) {
    let seed = verified_account(true);
    persist_as(
        &seed.0,
        &sheet(&[("m3", ANN), ("m3-3", CID)]),
        ANN,
        NOW + 10,
    )
    .unwrap();
    seed
}

const NEW_BEA: &str = "bea-new@example.test";

fn changed_address_source(to: &str) -> FleetRoster {
    if to == "m3-2" {
        sheet(&[("m3", ANN), ("m3-2", NEW_BEA), ("m3-3", CID)])
    } else {
        sheet(&[("m3", ANN), ("m3-2", EVE), ("m3-3", CID), (to, NEW_BEA)])
    }
}

#[test]
fn a_departing_accounts_provider_key_preserves_its_record_before_label_reuse() {
    for staged in [false, true] {
        for to in ["m3-2", "m3-4"] {
            let (desk, dir, key, profile) = verified_account(staged);
            let store = desk.store();
            store
                .transact_logins(NOW + 1, &mut None, |state, logins| {
                    Ok(accounts::note_checked_sign_in(
                        state,
                        logins,
                        "m3-2",
                        NOW + 1,
                    ))
                })
                .unwrap();
            let before_state = store.load(NOW + 1).unwrap().0;
            let before_logins = store.load_logins(NOW + 1).unwrap().0;
            let before_binding = before_logins.labels["m3-2"].clone();
            assert!(before_logins.sign_in_receipts.contains_key("m3-2"));
            let old_row = store
                .bound(&desk.rows(), NOW + 1)
                .unwrap()
                .row("m3-2")
                .unwrap()
                .clone();
            let before_forecast = accounts::login_forecast(&old_row, &before_logins, &before_state);
            assert!(before_forecast.is_some());
            assert_eq!(
                accounts::login_served_at(&old_row, &before_logins, &before_state),
                Some(NOW + 1)
            );
            let ann = desk.rows().row("m3").unwrap().clone();
            let profile = profile.replace(BEA, NEW_BEA);
            fs::write(dir.join(".claude.json"), &profile).unwrap();

            let source = changed_address_source(to);
            persist_as(&desk, &source, ANN, NOW + 10).unwrap();

            let state = store.load(NOW + 10).unwrap().0;
            assert_eq!(state.account(to).key, Some(key));
            assert_eq!(state.account(to).auth, accounts::Auth::Ok);
            assert_eq!(
                state.account(to).auth_at,
                before_state.account("m3-2").auth_at
            );
            assert_eq!(state.account(to).walls, before_state.account("m3-2").walls);
            assert_eq!(state.account("m3"), before_state.account("m3"));
            assert_eq!(desk.rows().row("m3"), Some(&ann));
            let logins = store.load_logins(NOW + 10).unwrap().0;
            let binding = &logins.labels[to];
            assert!(binding.renewal_generation > before_logins.generation_clock);
            let mut expected = before_binding;
            expected.renewal_generation = binding.renewal_generation;
            expected.last_success_generation = 0;
            let mut retained = binding.clone();
            expected.lifetime_scope = Default::default();
            retained.lifetime_scope = Default::default();
            assert_eq!(
                retained, expected,
                "the account retains its binding history"
            );
            assert!(!logins.sign_in_receipts.contains_key("m3-2"));
            assert!(!logins.sign_in_receipts.contains_key(to));
            let bound = store.bound(&desk.rows(), NOW + 10).unwrap();
            let row = bound.row(to).unwrap();
            assert_eq!(row.dir.as_deref(), Some(text(&dir).as_str()));
            assert_eq!(accounts::forecast(binding), before_forecast);
            assert_eq!(accounts::login_forecast(row, &logins, &state), None);
            assert_eq!(accounts::login_served_at(row, &logins, &state), None);
            if to != "m3-2" {
                assert_ne!(state.account("m3-2").auth, accounts::Auth::Ok);
                assert!(state.account("m3-2").walls.is_empty());
                assert_ne!(
                    bound.row("m3-2").unwrap().dir.as_deref(),
                    Some(text(&dir).as_str())
                );
            }
            let after = records(&desk);
            let mut called = false;
            assert!(
                store
                    .observe_login_within(
                        &[],
                        NOW + 10,
                        accounts::LoginObservation {
                            roster_path: &desk.roster(),
                            row: &old_row,
                            who: &accounts::ProcessLogin {
                                login: old_row.dir.clone(),
                                default_home: false,
                                stored_login: true,
                            },
                        },
                        |_, _| {
                            called = true;
                            Ok(Vec::new())
                        }
                    )
                    .is_err()
            );
            assert!(!called);
            assert_eq!(records(&desk), after);
            persist_as(&desk, &source, ANN, NOW + 20).unwrap();
            assert_eq!(records(&desk), after, "a settled recovery writes nothing");
            assert_eq!(
                fs::read_to_string(dir.join(".claude.json")).unwrap(),
                profile
            );
            assert!(pointed_at_another(&desk).is_empty());
            assert!(distinct_dirs(&desk));
        }
    }
}

#[test]
fn a_departing_accounts_missing_mismatched_or_ambiguous_key_grants_no_recovery() {
    for staged in [false, true] {
        for case in ["missing-key", "changed-key", "changed-email", "ambiguous"] {
            let (desk, dir, _, profile) = verified_account(staged);
            let store = desk.store();
            let profile = profile.replace(BEA, NEW_BEA);
            fs::write(dir.join(".claude.json"), &profile).unwrap();
            match case {
                "missing-key" => {
                    store
                        .transact_logins(NOW + 1, &mut None, |state, _| {
                            state.accounts.get_mut("m3-2").unwrap().key = None;
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
                "changed-key" => fs::write(
                    dir.join(".claude.json"),
                    profile.replace(RETURNED_ID, "00000000-0000-0000-0000-0000000000c3"),
                )
                .unwrap(),
                "changed-email" => fs::write(
                    dir.join(".claude.json"),
                    profile.replace(NEW_BEA, "unlisted@example.test"),
                )
                .unwrap(),
                _ => {
                    let other = store.logins_dir().join("retained-bea");
                    signed_in(&other, NEW_BEA);
                    fs::write(other.join(".claude.json"), &profile).unwrap();
                    store
                        .transact_logins(NOW + 1, &mut None, |state, logins| {
                            let record = state.account("m3-2").clone();
                            state.accounts.insert("retained-bea".into(), record);
                            logins.labels.insert(
                                "retained-bea".into(),
                                accounts::Binding {
                                    dir: Some(text(&other)),
                                    ..accounts::Binding::default()
                                },
                            );
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
            }
            let held_profile = fs::read(dir.join(".claude.json")).unwrap();
            persist_as(&desk, &changed_address_source("m3-4"), ANN, NOW + 10).unwrap();
            let state = store.load(NOW + 10).unwrap().0;
            let bound = store.bound(&desk.rows(), NOW + 10).unwrap();
            for label in ["m3-2", "m3-4"] {
                assert_ne!(state.account(label).auth, accounts::Auth::Ok, "{case}");
                assert!(state.account(label).walls.is_empty(), "{case}");
                assert_ne!(
                    bound.row(label).unwrap().dir.as_deref(),
                    Some(text(&dir).as_str()),
                    "{case}"
                );
            }
            assert_eq!(fs::read(dir.join(".claude.json")).unwrap(), held_profile);
        }
    }
}

#[test]
fn a_departing_account_cannot_take_another_bindings_current_or_retained_directory() {
    for staged in [false, true] {
        for current in [false, true] {
            let (desk, dir, _, profile) = verified_account(staged);
            let store = desk.store();
            fs::write(dir.join(".claude.json"), profile.replace(BEA, NEW_BEA)).unwrap();
            store
                .transact_logins(NOW + 1, &mut None, |_, logins| {
                    logins.labels.insert(
                        "retained-owner".into(),
                        accounts::Binding {
                            dir: current.then(|| text(&dir)),
                            was: if current {
                                Vec::new()
                            } else {
                                vec![Some(text(&dir))]
                            },
                            ..accounts::Binding::default()
                        },
                    );
                    Ok(Vec::new())
                })
                .unwrap();
            persist_as(&desk, &changed_address_source("m3-4"), ANN, NOW + 10).unwrap();
            let state = store.load(NOW + 10).unwrap().0;
            assert_ne!(state.account("m3-4").auth, accounts::Auth::Ok);
            assert!(state.account("m3-4").walls.is_empty());
            assert_ne!(
                store
                    .bound(&desk.rows(), NOW + 10)
                    .unwrap()
                    .row("m3-4")
                    .unwrap()
                    .dir
                    .as_deref(),
                Some(text(&dir).as_str())
            );
        }
    }
}

#[test]
fn a_still_listed_account_is_not_reinterpreted_by_a_changed_profile_address() {
    let (desk, dir, key, profile) = verified_account(false);
    let store = desk.store();
    fs::write(dir.join(".claude.json"), profile.replace(BEA, NEW_BEA)).unwrap();
    let mut source = changed_address_source("m3-4");
    source.accounts.push(account("m3-5", BEA));
    persist_as(&desk, &source, ANN, NOW + 10).unwrap();
    let state = store.load(NOW + 10).unwrap().0;
    assert_eq!(state.account("m3-5").key, Some(key));
    assert!(!state.account("m3-5").walls.is_empty());
    assert_ne!(state.account("m3-4").auth, accounts::Auth::Ok);
    assert!(state.account("m3-4").walls.is_empty());
}

#[test]
fn a_departing_accounts_profile_and_retained_key_are_rechecked_before_restore() {
    for change in ["profile-key", "profile-email", "record-key"] {
        let (desk, dir, _, profile) = verified_account(false);
        let store = desk.store();
        let profile = profile.replace(BEA, NEW_BEA);
        fs::write(dir.join(".claude.json"), &profile).unwrap();
        let existing = desk.rows();
        let source = changed_address_source("m3-4");
        let next = build_machine_roster(&source, "m3", &store, Some(&existing), Some(ANN));
        let mut state = store.load(NOW + 1).unwrap().0;
        let logins = store.load_logins(NOW + 1).unwrap().0;
        let plans = returned::find(&store, &existing, &next, &state, &logins, &[]).unwrap();
        assert_eq!(plans.len(), 1);
        match change {
            "profile-key" => fs::write(
                dir.join(".claude.json"),
                profile.replace(RETURNED_ID, "00000000-0000-0000-0000-0000000000c3"),
            )
            .unwrap(),
            "profile-email" => fs::write(
                dir.join(".claude.json"),
                profile.replace(NEW_BEA, "unlisted@example.test"),
            )
            .unwrap(),
            _ => state.accounts.get_mut("m3-2").unwrap().key = None,
        }
        assert!(plans[0].recheck(&state, &next).is_err(), "{change}");
    }
}

#[test]
fn departing_provider_keys_can_trade_labels_without_erasing_either_source() {
    let (desk, bea_dir, bea_key, profile) = verified_account(false);
    let store = desk.store();
    fs::write(bea_dir.join(".claude.json"), profile.replace(BEA, NEW_BEA)).unwrap();
    let cid_dir = store.login_dir("m3-3").unwrap();
    let cid_id = "00000000-0000-0000-0000-0000000000c3";
    let new_cid = "cid-new@example.test";
    signed_in(&cid_dir, new_cid);
    fs::write(
        cid_dir.join(".claude.json"),
        format!(
            "{{\"oauthAccount\":{{\"emailAddress\":\"{new_cid}\",\"accountUuid\":\"{cid_id}\"}}}}\n"
        ),
    )
    .unwrap();
    let cid_key = super::super::profile::AccountKey::claude(cid_id).unwrap();
    store
        .transact_logins(NOW + 1, &mut None, |state, logins| {
            logins.labels.insert(
                "m3-3".into(),
                accounts::Binding {
                    dir: Some(text(&cid_dir)),
                    ..accounts::Binding::default()
                },
            );
            let events =
                accounts::note_auth_in_context(state, logins, "m3-3", accounts::Auth::Ok, NOW + 1);
            let walls = state.account("m3-2").walls.clone();
            let record = state.accounts.get_mut("m3-3").unwrap();
            record.key = Some(cid_key);
            record.walls = walls;
            Ok(events)
        })
        .unwrap();
    let source = sheet(&[("m3", ANN), ("m3-2", new_cid), ("m3-3", NEW_BEA)]);
    persist_as(&desk, &source, ANN, NOW + 10).unwrap();
    let state = store.load(NOW + 10).unwrap().0;
    let bound = store.bound(&desk.rows(), NOW + 10).unwrap();
    for (label, key, dir) in [("m3-2", cid_key, cid_dir), ("m3-3", bea_key, bea_dir)] {
        assert_eq!(state.account(label).key, Some(key));
        assert_eq!(state.account(label).auth, accounts::Auth::Ok);
        assert!(!state.account(label).walls.is_empty());
        assert_eq!(
            bound.row(label).unwrap().dir.as_deref(),
            Some(text(&dir).as_str())
        );
    }
    let after = records(&desk);
    persist_as(&desk, &source, ANN, NOW + 20).unwrap();
    assert_eq!(records(&desk), after);
}

#[test]
fn a_returning_account_recovers_its_verified_renewal_and_quota_state_at_a_new_label() {
    let (desk, staged, key, profile) = departed_verified_account();
    let store = desk.store();
    let returning = sheet(&[("m3", ANN), ("m3-3", CID), ("m3-4", BEA)]);
    persist_as(&desk, &returning, ANN, NOW + 20).unwrap();

    let served = store.bound(&desk.rows(), NOW + 20).unwrap();
    assert_eq!(
        served.row("m3-4").unwrap().dir.as_deref(),
        Some(text(&staged).as_str())
    );
    let state = store.load(NOW + 20).unwrap().0;
    assert_eq!(state.account("m3-4").auth, accounts::Auth::Ok);
    assert_eq!(state.account("m3-4").key, Some(key));
    assert_eq!(
        state.account("m3-4").walls["weekly"].until,
        Some(NOW + 3600)
    );
    assert!(!state.accounts.contains_key("m3-2"));
    assert_eq!(
        fs::read_to_string(staged.join(".claude.json")).unwrap(),
        profile
    );
    assert!(pointed_at_another(&desk).is_empty());
    assert!(distinct_dirs(&desk));
    let after = records(&desk);
    persist_as(&desk, &returning, ANN, NOW + 30).unwrap();
    assert_eq!(
        records(&desk),
        after,
        "the recovered account remains stable"
    );
}

#[test]
fn a_returning_account_can_leave_its_old_label_for_a_different_account() {
    let (desk, staged, key, _) = departed_verified_account();
    let store = desk.store();
    let returning = sheet(&[("m3", ANN), ("m3-2", EVE), ("m3-3", CID), ("m3-4", BEA)]);
    persist_as(&desk, &returning, ANN, NOW + 20).unwrap();
    let served = store.bound(&desk.rows(), NOW + 20).unwrap();
    assert_eq!(
        served.row("m3-4").unwrap().dir.as_deref(),
        Some(text(&staged).as_str())
    );
    assert_ne!(
        served.row("m3-2").unwrap().dir.as_deref(),
        Some(text(&staged).as_str())
    );
    let state = store.load(NOW + 20).unwrap().0;
    assert_eq!(state.account("m3-4").key, Some(key));
    assert_eq!(state.account("m3-4").auth, accounts::Auth::Ok);
    assert_ne!(state.account("m3-2").auth, accounts::Auth::Ok);
    assert!(pointed_at_another(&desk).is_empty());
    assert!(distinct_dirs(&desk));
}

#[test]
fn unknown_or_contradictory_returned_identity_does_not_transfer_account_state() {
    for case in ["missing-key", "changed-key", "changed-email"] {
        let (desk, staged, _, profile) = departed_verified_account();
        let store = desk.store();
        match case {
            "missing-key" => store
                .transact_logins(NOW + 11, &mut None, |state, _| {
                    state.accounts.get_mut("m3-2").unwrap().key = None;
                    Ok(Vec::new())
                })
                .map(|_| ())
                .unwrap(),
            "changed-key" => fs::write(
                staged.join(".claude.json"),
                profile.replace(RETURNED_ID, "00000000-0000-0000-0000-0000000000c3"),
            )
            .unwrap(),
            _ => fs::write(
                staged.join(".claude.json"),
                profile.replace(BEA, "unlisted@example.test"),
            )
            .unwrap(),
        }
        let before = fs::read(staged.join(".claude.json")).unwrap();
        let returning = sheet(&[("m3", ANN), ("m3-3", CID), ("m3-4", BEA)]);
        persist_as(&desk, &returning, ANN, NOW + 20).unwrap();
        let state = store.load(NOW + 20).unwrap().0;
        assert_ne!(state.account("m3-4").auth, accounts::Auth::Ok, "{case}");
        assert!(state.accounts.contains_key("m3-2"), "{case}");
        assert_ne!(
            store
                .bound(&desk.rows(), NOW + 20)
                .unwrap()
                .row("m3-4")
                .unwrap()
                .dir
                .as_deref(),
            Some(text(&staged).as_str()),
            "{case}"
        );
        assert_eq!(fs::read(staged.join(".claude.json")).unwrap(), before);
    }
}

#[test]
fn two_retained_matches_leave_the_returning_account_unassigned() {
    let (desk, _, _, profile) = departed_verified_account();
    let store = desk.store();
    let second = store.logins_dir().join("retained-bea+1");
    signed_in(&second, BEA);
    fs::write(second.join(".claude.json"), profile).unwrap();
    store
        .transact_logins(NOW + 11, &mut None, |state, logins| {
            state
                .accounts
                .insert("retained-bea".into(), state.accounts["m3-2"].clone());
            logins.labels.insert(
                "retained-bea".into(),
                accounts::Binding {
                    dir: Some(text(&second)),
                    ..accounts::Binding::default()
                },
            );
            Ok(Vec::new())
        })
        .unwrap();
    let returning = sheet(&[("m3", ANN), ("m3-3", CID), ("m3-4", BEA)]);
    persist_as(&desk, &returning, ANN, NOW + 20).unwrap();
    let state = store.load(NOW + 20).unwrap().0;
    assert_ne!(state.account("m3-4").auth, accounts::Auth::Ok);
    for retained in ["m3-2", "retained-bea"] {
        assert_eq!(state.account(retained).auth, accounts::Auth::Ok);
    }
}

#[test]
fn a_returned_profile_is_rechecked_before_moving_its_state() {
    let (desk, staged, _, profile) = departed_verified_account();
    let store = desk.store();
    let existing = desk.rows();
    let returning = sheet(&[("m3", ANN), ("m3-3", CID), ("m3-4", BEA)]);
    let next = build_machine_roster(&returning, "m3", &store, Some(&existing), Some(ANN));
    let state = store.load(NOW + 11).unwrap().0;
    let logins = store.load_logins(NOW + 11).unwrap().0;
    let plans = returned::find(&store, &existing, &next, &state, &logins, &[]).unwrap();
    assert_eq!(plans.len(), 1);
    fs::write(
        staged.join(".claude.json"),
        profile.replace(RETURNED_ID, "00000000-0000-0000-0000-0000000000c3"),
    )
    .unwrap();
    assert!(plans[0].recheck(&state, &next).is_err());
}

#[test]
fn a_returning_account_does_not_replace_its_new_labels_existing_record() {
    let (desk, staged, _, profile) = departed_verified_account();
    let store = desk.store();
    fs::write(
        staged.join(".claude.json"),
        profile.replace(RETURNED_ID, "00000000-0000-0000-0000-0000000000c3"),
    )
    .unwrap();
    let returning = sheet(&[("m3", ANN), ("m3-3", CID), ("m3-4", BEA)]);
    persist_as(&desk, &returning, ANN, NOW + 20).unwrap();
    store
        .transact_logins(NOW + 21, &mut None, |state, _| {
            Ok(accounts::note_auth(
                state,
                "m3-4",
                accounts::Auth::SignedOut,
                NOW + 21,
            ))
        })
        .unwrap();
    fs::write(staged.join(".claude.json"), profile).unwrap();
    let before = records(&desk);
    persist_as(&desk, &returning, ANN, NOW + 30).unwrap();
    assert_eq!(records(&desk), before);
    let state = store.load(NOW + 30).unwrap().0;
    assert_eq!(state.account("m3-4").auth, accounts::Auth::SignedOut);
    assert_eq!(state.account("m3-2").auth, accounts::Auth::Ok);
}

#[test]
fn a_returning_account_cannot_claim_another_bindings_retained_directory() {
    for current in [true, false] {
        let (desk, staged, _, _) = departed_verified_account();
        let store = desk.store();
        store
            .transact_logins(NOW + 11, &mut None, |_, logins| {
                logins.labels.insert(
                    "retained-owner".into(),
                    accounts::Binding {
                        dir: current.then(|| text(&staged)),
                        was: if current {
                            Vec::new()
                        } else {
                            vec![Some(text(&staged))]
                        },
                        ..accounts::Binding::default()
                    },
                );
                Ok(Vec::new())
            })
            .unwrap();
        let returning = sheet(&[("m3", ANN), ("m3-3", CID), ("m3-4", BEA)]);
        persist_as(&desk, &returning, ANN, NOW + 20).unwrap();
        let state = store.load(NOW + 20).unwrap().0;
        assert_ne!(state.account("m3-4").auth, accounts::Auth::Ok);
        assert_eq!(state.account("m3-2").auth, accounts::Auth::Ok);
        assert_ne!(
            store
                .bound(&desk.rows(), NOW + 20)
                .unwrap()
                .row("m3-4")
                .unwrap()
                .dir
                .as_deref(),
            Some(text(&staged).as_str())
        );
    }
}

#[test]
fn two_returning_accounts_can_exchange_their_retained_labels() {
    let (desk, staged, bea_key, _) = departed_verified_account();
    let store = desk.store();
    let cid_dir = store.login_dir("m3-3").unwrap();
    let cid_id = "00000000-0000-0000-0000-0000000000c3";
    signed_in(&cid_dir, CID);
    fs::write(
        cid_dir.join(".claude.json"),
        format!(
            "{{\"oauthAccount\":{{\"emailAddress\":\"{CID}\",\"accountUuid\":\"{cid_id}\"}}}}\n"
        ),
    )
    .unwrap();
    let cid_key = super::super::profile::AccountKey::claude(cid_id).unwrap();
    store
        .transact_logins(NOW + 11, &mut None, |state, _| {
            let events = accounts::note_auth(state, "m3-3", accounts::Auth::Ok, NOW + 11);
            state.accounts.get_mut("m3-3").unwrap().key = Some(cid_key);
            Ok(events)
        })
        .unwrap();
    persist_as(&desk, &sheet(&[("m3", ANN)]), ANN, NOW + 20).unwrap();
    persist_as(
        &desk,
        &sheet(&[("m3", ANN), ("m3-2", CID), ("m3-3", BEA)]),
        ANN,
        NOW + 30,
    )
    .unwrap();
    let state = store.load(NOW + 30).unwrap().0;
    assert_eq!(state.account("m3-2").key, Some(cid_key));
    assert_eq!(state.account("m3-3").key, Some(bea_key));
    for label in ["m3-2", "m3-3"] {
        assert_eq!(state.account(label).auth, accounts::Auth::Ok);
    }
    let served = store.bound(&desk.rows(), NOW + 30).unwrap();
    assert_eq!(served.row("m3-2").unwrap().dir, Some(text(&cid_dir)));
    assert_eq!(served.row("m3-3").unwrap().dir, Some(text(&staged)));
    assert!(pointed_at_another(&desk).is_empty());
    assert!(distinct_dirs(&desk));
}

fn depart_with_full_history(current_already_retained: bool) {
    let (desk, staged, key, profile) = departed_verified_account();
    let store = desk.store();
    let all = sheet(&[("m3", ANN), ("m3-2", BEA), ("m3-3", CID)]);
    persist_as(&desk, &all, ANN, NOW + 20).unwrap();
    let mut history = Vec::new();
    for index in 0..accounts::MAX_LEFT {
        let path = store.logins_dir().join(format!("old-bea-{index}"));
        signed_in(&path, BEA);
        fs::write(path.join(".claude.json"), &profile).unwrap();
        history.push(Some(text(&path)));
    }
    if current_already_retained {
        history[1] = Some(text(&staged));
    }
    store
        .transact_logins(NOW + 21, &mut None, |_, logins| {
            logins.labels.get_mut("m3-2").unwrap().was = history.clone();
            Ok(Vec::new())
        })
        .unwrap();
    persist_as(&desk, &sheet(&[("m3", ANN), ("m3-3", CID)]), ANN, NOW + 30)
        .expect("a departure does not publish more history than the store admits");
    let (logins, _) = store.load_logins(NOW + 30).unwrap();
    let retained = logins.get("m3-2").unwrap();
    assert_eq!(retained.was.len(), accounts::MAX_LEFT);
    assert_eq!(retained.was.first(), history.first());
    assert_eq!(retained.dir, None);
    assert_eq!(retained.was.last(), Some(&Some(text(&staged))));
    assert_eq!(
        retained
            .was
            .iter()
            .filter(|path| path.as_deref() == Some(text(&staged).as_str()))
            .count(),
        1
    );
    for path in history.iter().flatten() {
        assert_eq!(
            fs::read_to_string(Path::new(path).join(".claude.json")).unwrap(),
            profile,
            "retiring metadata does not remove a login file"
        );
    }
    let returned = sheet(&[("m3", ANN), ("m3-3", CID), ("m3-4", BEA)]);
    persist_as(&desk, &returned, ANN, NOW + 40).unwrap();
    assert_eq!(
        store
            .bound(&desk.rows(), NOW + 40)
            .unwrap()
            .row("m3-4")
            .unwrap()
            .dir,
        Some(text(&staged))
    );
    let state = store.load(NOW + 40).unwrap().0;
    assert_eq!(state.account("m3-4").key, Some(key));
    assert_eq!(state.account("m3-4").auth, accounts::Auth::Ok);
}

#[test]
fn a_departure_at_history_capacity_keeps_its_binding_readable() {
    depart_with_full_history(false);
}

#[test]
fn a_departure_moves_its_current_directory_to_the_recovery_position() {
    depart_with_full_history(true);
}
