// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The vault's sheet reaching every Mac (m22, 2026-10-27): a window's sync once wrote aterm's
//! own copy of the fleet file to `~/.secrets/fleet-accounts.toml`, which then outranked the
//! sheet. The files in `~/.secrets` are now the owner's backup, which nothing reads or writes:
//! every source is the vault entry this Mac reads in place (`vault:<entry>`), a copy in the
//! backup in any format is never a source, a vault fleet keeps its precedence until the sheet
//! that gives this machine rows retires it, and nothing restores it. A failing sync waits for
//! its inputs to change. Every fixture email is `example.test`.

use std::sync::{Arc, Mutex};

use super::login_tests::{Desk, NOW, upkeep_effects};
use super::*;

const SHEET: &str = "email,computer,claude\na1@example.test,m27,1\na2@example.test,m28,1\n";

fn manual(desk: &Desk, source: Option<&str>, fetch: &Fetch<'_>) -> Result<SyncReport, String> {
    let env = quiet_env(
        &desk.home,
        &desk.state,
        &desk.config.join("aterm.toml"),
        NOW,
    );
    sync_accounts_with(
        &env,
        source,
        None,
        &SyncHow {
            fetch,
            ladder: &[],
            fleet_snapshots: &[],
            heal: None,
            allow_empty_pool: false,
        },
    )
    .map_err(SyncError::into_message)
}

#[test]
fn source_less_manual_sync_preserves_an_active_vault_fleet() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    vault(
        &desk,
        FLEET_ROSTER_FILE,
        "[[account]]\nlabel='m27'\nemail='a1@example.test'\nmachines=['m27']\n",
    );
    sync(&desk, NOW);
    manual(&desk, None, &fetch_source_text).unwrap();
    assert!(
        !accounts::Store::at(&desk.state)
            .source_policy()
            .unwrap()
            .retired()
    );
    assert_eq!(
        origin(&desk),
        FleetOrigin::Vault(FLEET_ROSTER_FILE.to_owned())
    );
}

#[test]
fn automatic_local_source_bytes_are_rechecked_before_publication() {
    for init in [false, true] {
        let desk = Desk::new("m27");
        desk.default_login("a1@example.test");
        let source = desk.home.join("source.csv");
        fs::write(&source, SHEET).unwrap();
        fs::write(
            desk.config.join("aterm.toml"),
            format!(
                "[fleet]\nmachine='m27'\naccounts_source='{}'\n",
                source.display()
            ),
        )
        .unwrap();
        let fetch = |env: &Env, named: &str| {
            let text = fetch_source_text(env, named)?;
            fs::write(&source, SHEET.replace("a1@", "changed@")).unwrap();
            Ok(text)
        };
        let why = if init {
            sync_accounts_for_init_with(&desk.home, &desk.state, &desk.roster(), NOW, || {}, &fetch)
                .unwrap_err()
                .to_string()
        } else {
            manual(&desk, None, &fetch).err().expect("source changed")
        };
        assert!(why.contains("changed"), "{why}");
        assert!(!desk.roster().exists());
        assert!(
            !accounts::Store::at(&desk.state)
                .source_policy()
                .unwrap()
                .retired()
        );
    }
}

/// No output resolves into the backup and none aliases a local input the sync read. A hard link
/// to a backup file is replaced by rename, never written through, so the backup keeps its bytes.
#[test]
fn source_inputs_cannot_be_overwritten_through_either_output_path() {
    for roster in [false, true] {
        for hard in [false, true] {
            for input in [
                FLEET_ROSTER_FILE,
                ACCOUNTS_CSV_FILE,
                super::super::ai_pool::AI_POOL_FILE,
                "custom",
            ] {
                let desk = Desk::new("m27");
                desk.default_login("a1@example.test");
                let requested = desk.home.join("requested.csv");
                fs::write(&requested, SHEET).unwrap();
                let protected = if input == "custom" {
                    requested.clone()
                } else {
                    backup_dir(&desk).join(input)
                };
                if input != "custom" {
                    fs::write(&protected, "account=[]\n").unwrap();
                }
                let before = fs::read(&protected).unwrap();
                let output = if roster {
                    desk.roster()
                } else {
                    desk.fleet_file()
                };
                if hard {
                    fs::hard_link(&protected, &output).unwrap();
                } else {
                    std::os::unix::fs::symlink(&protected, &output).unwrap();
                }
                let result = manual(&desk, requested.to_str(), &fetch_source_text);
                let tag = format!("{input} roster={roster} hard={hard}");
                if input == "custom" {
                    let why = result.err().expect("aliased input refused");
                    assert!(why.contains("aliases"), "{tag}: {why}");
                } else if !hard {
                    let why = result.err().expect("an output in the backup is refused");
                    assert!(why.contains("~/.secrets"), "{tag}: {why}");
                }
                assert_eq!(fs::read(&protected).unwrap(), before, "{tag}");
            }
        }
    }
}

#[test]
fn explicitly_named_profile_inputs_are_read_only() {
    for configured in [false, true] {
        let desk = Desk::new("m27");
        desk.default_login("a1@example.test");
        let path = desk.fleet_file();
        let text = "[[account]]\nlabel='m27'\nemail='a1@example.test'\nmachines=['m27']\n";
        fs::write(&path, text).unwrap();
        if configured {
            fs::write(
                desk.config.join("aterm.toml"),
                format!(
                    "[fleet]\nmachine='m27'\naccounts_source='{}'\n",
                    path.display()
                ),
            )
            .unwrap();
        }
        let why = manual(
            &desk,
            (!configured).then(|| path.to_str().unwrap()),
            &fetch_source_text,
        )
        .err()
        .expect("explicit input is read-only");
        assert!(why.contains("aliases"), "{why}");
        assert_eq!(fs::read_to_string(path).unwrap(), text);
    }
}

#[test]
fn a_dangling_link_in_the_backup_does_not_disable_live_sources() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    vault(&desk, ACCOUNTS_CSV_FILE, SHEET);
    sync(&desk, NOW);
    std::os::unix::fs::symlink(
        desk.home.join("gone"),
        backup_dir(&desk).join(FLEET_ROSTER_FILE),
    )
    .unwrap();
    sync(&desk, NOW + 1);
    assert_eq!(emails(&desk), ["a1@example.test"]);
    assert!(
        accounts::Store::at(&desk.state)
            .source_policy()
            .unwrap()
            .retired()
    );
}

#[test]
fn empty_manual_source_uses_captured_automatic_policy() {
    for source in [None, Some(""), Some(" \t ")] {
        let desk = Desk::new("m27");
        desk.default_login("a1@example.test");
        let named = desk.home.join("chosen.csv");
        fs::write(&named, SHEET).unwrap();
        let config = desk.config.join("aterm.toml");
        fs::write(
            &config,
            format!(
                "[fleet]\nmachine='m27'\naccounts_source='{}'\n",
                named.display()
            ),
        )
        .unwrap();
        let env = quiet_env(&desk.home, &desk.state, &config, NOW);
        let how = SyncHow {
            fetch: &fetch_source_text,
            ladder: &[],
            fleet_snapshots: &[],
            heal: None,
            allow_empty_pool: false,
        };
        sync_accounts_with(&env, source, None, &how).unwrap();
        assert!(env.accounts_store().source_policy().unwrap().retired());
    }
}

#[test]
fn every_provider_source_reader_preserves_retirement() {
    for cli in std::iter::once(Cli::Claude).chain(OTHER_CLIS) {
        let desk = Desk::new("m27");
        let env = quiet_env(
            &desk.home,
            &desk.state,
            &desk.config.join("aterm.toml"),
            NOW,
        );
        vault(&desk, &cli_fleet_file(cli), "retired source");
        if cli == Cli::Claude {
            vault(&desk, FLEET_ROSTER_FILE, "retired source");
        }
        vault(&desk, ACCOUNTS_CSV_FILE, SHEET);
        let store = accounts::Store::at_for(&desk.state, cli, &desk.home);
        let held = store.acquire().unwrap();
        store.retire_source_held(&held, NOW).unwrap();
        drop(held);
        let selected = local_cli_source(cli, &env, &desk.home).unwrap().unwrap();
        assert_eq!(selected.entry.name, ACCOUNTS_CSV_FILE);
        assert!(selected.csv);
        fs::remove_file(store.source_policy_file()).unwrap();
        assert!(local_cli_source(cli, &env, &desk.home).is_err());
    }
}

#[test]
fn a_local_list_without_a_vault_input_does_not_retire_or_seed_a_retired_slot() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    fs::write(desk.fleet_file(), "opt_in='discovered'\n[[account]]\nlabel='m27'\nemail='a1@example.test'\nmachines=['m27']\n").unwrap();
    sync(&desk, NOW);
    assert!(
        !accounts::Store::at(&desk.state)
            .source_policy()
            .unwrap()
            .retired()
    );
    assert!(!desk.home.join(".secrets").join(FLEET_ROSTER_FILE).exists());
    manual(&desk, None, &fetch_source_text).unwrap();
    assert!(
        !accounts::Store::at(&desk.state)
            .source_policy()
            .unwrap()
            .retired()
    );
    sync(&desk, NOW + 1);
    assert!(
        !accounts::Store::at(&desk.state)
            .source_policy()
            .unwrap()
            .retired()
    );
}

#[test]
fn unannotated_local_list_is_not_scheduled_for_automatic_import() {
    let desk = Desk::new("m27");
    fs::write(
        desk.fleet_file(),
        "[[account]]\nlabel='m27'\nemail='a1@example.test'\nmachines=['m27']\n",
    )
    .unwrap();
    assert!(!needs_sync(&desk, NOW));
    assert!(
        !accounts::Store::at(&desk.state)
            .source_policy()
            .unwrap()
            .retired()
    );
}

#[test]
fn an_unassigned_sheet_neither_retires_nor_commits_a_stale_assignment() {
    for changed in [false, true] {
        let desk = Desk::new("m99");
        vault(&desk, ACCOUNTS_CSV_FILE, SHEET);
        let result = sync_accounts_for_init_with(
            &desk.home,
            &desk.state,
            &desk.roster(),
            NOW,
            || {
                if changed {
                    vault(
                        &desk,
                        ACCOUNTS_CSV_FILE,
                        "email,computer,claude\na1@example.test,m99,1\n",
                    );
                }
            },
            &fetch_source_text,
        );
        assert_eq!(result.is_err(), changed);
        assert!(
            !accounts::Store::at(&desk.state)
                .source_policy()
                .unwrap()
                .retired()
        );
        assert!(!desk.roster().exists());
    }
}

#[test]
fn retired_source_changes_do_not_change_sync_inputs_or_source_presence() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    vault(&desk, ACCOUNTS_CSV_FILE, SHEET);
    sync(&desk, NOW);
    let before = sync_inputs(&desk.home, &desk.state, &desk.roster(), NOW);
    vault(&desk, FLEET_ROSTER_FILE, "ignored retired source");
    assert_eq!(
        sync_inputs(&desk.home, &desk.state, &desk.roster(), NOW),
        before
    );
    let env = quiet_env(
        &desk.home,
        &desk.state,
        &desk.config.join("aterm.toml"),
        NOW,
    );
    let table = account_source_table(Cli::Claude, &env, None).unwrap();
    assert!(
        table
            .roster
            .accounts
            .iter()
            .any(|row| row.email == "a1@example.test")
    );
    vault(&desk, ACCOUNTS_CSV_FILE, "");
    assert!(!has_fleet_source(&desk.home, &env.accounts_store()));
}

#[test]
fn a_local_projection_linked_into_the_backup_is_neither_read_nor_written() {
    for retired in [false, true] {
        let desk = Desk::new("m27");
        desk.default_login("a1@example.test");
        let copy = backup_dir(&desk).join(FLEET_ROSTER_FILE);
        vault(&desk, ACCOUNTS_CSV_FILE, SHEET);
        if retired {
            sync(&desk, NOW);
            fs::remove_file(desk.fleet_file()).unwrap();
            vault(&desk, ACCOUNTS_CSV_FILE, "");
        }
        let text = format!(
            "{FLEET_HEAD_NAMED}\nopt_in='named'\n[[account]]\nlabel='m27'\nemail='a1@example.test'\nmachines=['m27']\n"
        );
        fs::write(&copy, &text).unwrap();
        std::os::unix::fs::symlink(&copy, desk.fleet_file()).unwrap();
        assert!(
            sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW + 1).is_err(),
            "a projection linked into the backup is refused"
        );
        assert_eq!(fs::read_to_string(&copy).unwrap(), text);
    }
}

#[test]
fn an_automatic_key_cannot_name_the_backup_through_a_link() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    vault(&desk, ACCOUNTS_CSV_FILE, SHEET);
    sync(&desk, NOW);
    let copy = backup_dir(&desk).join(FLEET_ROSTER_FILE);
    fs::write(&copy, "account=[]\n").unwrap();
    fs::remove_file(desk.fleet_file()).unwrap();
    let alias = desk.home.join("old-fleet.toml");
    std::os::unix::fs::symlink(&copy, &alias).unwrap();
    fs::write(
        desk.config.join("aterm.toml"),
        format!(
            "[fleet]\nmachine='m27'\naccounts_source='{}'\n",
            alias.display()
        ),
    )
    .unwrap();
    let why = sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW + 1).unwrap_err();
    assert!(why.contains("~/.secrets"), "{why}");
    assert_eq!(fs::read_to_string(&copy).unwrap(), "account=[]\n");
}

/// A copy in the backup is never a source: a withdrawn sheet leaves its rows in place, and an
/// import that names the copy is refused, naming the vault entry instead.
#[test]
fn a_backup_copy_is_preserved_and_never_returns_after_sheet_withdrawal() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    let copy = backup_dir(&desk).join(FLEET_ROSTER_FILE);
    vault(&desk, ACCOUNTS_CSV_FILE, SHEET);
    sync(&desk, NOW);
    let store = accounts::Store::at(&desk.state);
    let policy = fs::read(store.source_policy_file()).unwrap();
    let stale = "opt_in='named'\n[[account]]\nlabel='stale'\nemail='stale@example.test'\nmachines=['m27']\n";
    fs::write(&copy, stale).unwrap();
    vault(&desk, ACCOUNTS_CSV_FILE, "");
    sync(&desk, NOW + 1);
    assert_eq!(fs::read_to_string(&copy).unwrap(), stale);
    assert_eq!(emails(&desk), ["a1@example.test"]);
    assert_eq!(fs::read(store.source_policy_file()).unwrap(), policy);
    assert!(store.source_policy().unwrap().retired());

    let env = quiet_env(
        &desk.home,
        &desk.state,
        &desk.config.join("aterm.toml"),
        NOW + 2,
    );
    let how = SyncHow {
        fetch: &fetch_source_text,
        ladder: &sync_lock_ladder(),
        fleet_snapshots: &[],
        heal: None,
        allow_empty_pool: false,
    };
    let why = sync_accounts_with(&env, Some(copy.to_str().unwrap()), None, &how)
        .map_err(SyncError::into_message)
        .err()
        .expect("an import that names the backup copy is refused");
    assert!(why.contains("vault:fleet-accounts.toml"), "{why}");
    assert_eq!(fs::read(store.source_policy_file()).unwrap(), policy);
    assert_eq!(emails(&desk), ["a1@example.test"]);
    assert!(matches!(origin(&desk), FleetOrigin::Local { files, .. } if !files.contains(&copy)));
}

#[test]
fn a_lost_or_corrupt_policy_refuses_automatic_sync_without_touching_accounts() {
    for replacement in [None, Some("broken policy")] {
        let desk = Desk::new("m27");
        desk.default_login("a1@example.test");
        vault(&desk, ACCOUNTS_CSV_FILE, SHEET);
        sync(&desk, NOW);
        let store = accounts::Store::at(&desk.state);
        let roster = fs::read(desk.roster()).unwrap();
        match replacement {
            None => fs::remove_file(store.source_policy_file()).unwrap(),
            Some(text) => fs::write(store.source_policy_file(), text).unwrap(),
        }
        assert!(matches!(origin(&desk), FleetOrigin::SourcePending(_)));
        assert!(sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW + 1).is_err());
        assert_eq!(fs::read(desk.roster()).unwrap(), roster);
    }
}

#[test]
fn a_source_retired_after_capture_cannot_publish_old_rows() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    vault(
        &desk,
        FLEET_ROSTER_FILE,
        "[[account]]\nlabel='old'\nemail='a1@example.test'\nmachines=['m27']\n",
    );
    let result = sync_accounts_for_init_with(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW,
        || {
            let store = accounts::Store::at(&desk.state);
            let held = store.acquire().unwrap();
            store.retire_source_held(&held, NOW).unwrap();
        },
        &fetch_source_text,
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("source policy changed")
    );
    assert!(!desk.roster().exists());
}

/// Publishes `text` as the vault entry `name` this Mac reads; empty text removes it.
fn vault(desk: &Desk, name: &str, text: &str) {
    crate::vault_entry::seed(&desk.home, &[(name, text.as_bytes())]);
}

/// The owner's backup folder, `~/.secrets`, which nothing reads or writes.
fn backup_dir(desk: &Desk) -> PathBuf {
    let secrets = desk.home.join(".secrets");
    aterm_types::fs_restricted::ensure_private_dir(&secrets).unwrap();
    secrets
}

fn sync(desk: &Desk, now: i64) {
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), now).expect("sync");
}

fn needs_sync(desk: &Desk, now: i64) -> bool {
    needs_init_accounts_sync(&desk.home, &desk.state, &desk.roster(), now)
}

fn origin(desk: &Desk) -> FleetOrigin {
    fleet_origin(
        &accounts::Store::at(&desk.state),
        Some(&desk.home),
        Some(&desk.config.join("aterm.toml")),
        &desk.roster(),
    )
}

fn emails(desk: &Desk) -> Vec<String> {
    let mut emails: Vec<String> = desk
        .rows()
        .rows
        .into_iter()
        .filter_map(|row| row.email)
        .collect();
    emails.sort();
    emails
}

/// A sync from the vault's sheet writes no copy of the fleet into `~/.secrets`; a copy an earlier
/// build wrote there, in any format, is never a source, nor named by `accounts`, nor calls for a
/// sync; every later edit of the sheet, a row added or dropped, reaches the roster; and a sheet
/// withdrawn leaves the copy as it was. CONTROL: the first sync takes the sheet and needs no
/// other.
#[test]
fn the_sheet_reaches_the_roster_at_each_edit_and_a_backup_copy_is_never_read() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    let copy = backup_dir(&desk).join(FLEET_ROSTER_FILE);
    vault(&desk, ACCOUNTS_CSV_FILE, SHEET);
    sync(&desk, NOW);
    assert_eq!(emails(&desk), ["a1@example.test"], "CONTROL");
    assert!(!copy.exists(), "a sync from the sheet copies nothing there");
    assert!(!needs_sync(&desk, NOW + 1), "CONTROL: converged");

    // What earlier builds wrote: the fleet file's own bytes, and one recording `opt_in = "key"`.
    let beside = fs::read_to_string(desk.fleet_file()).unwrap();
    let from_a_url = FleetRoster::from_toml(&beside, "beside")
        .unwrap()
        .to_toml_for(Some(OptIn::Key));
    for (now, older) in [(NOW + 2, &beside), (NOW + 4, &from_a_url)] {
        fs::write(&copy, older).unwrap();
        assert_eq!(
            origin(&desk),
            FleetOrigin::Vault(ACCOUNTS_CSV_FILE.to_owned())
        );
        assert!(!needs_sync(&desk, now), "a backup copy calls for no sync");
        sync(&desk, now + 1);
        assert_eq!(fs::read_to_string(&copy).unwrap(), *older);
    }

    let edited = format!("{SHEET}a3@example.test,m27.2,1\n");
    vault(&desk, ACCOUNTS_CSV_FILE, &edited);
    sync(&desk, NOW + 10);
    assert_eq!(emails(&desk), ["a1@example.test", "a3@example.test"]);
    assert!(!needs_sync(&desk, NOW + 11));

    vault(&desk, ACCOUNTS_CSV_FILE, SHEET);
    assert!(needs_sync(&desk, NOW + 12), "a dropped row is an edit too");
    sync(&desk, NOW + 13);
    assert_eq!(emails(&desk), ["a1@example.test"]);

    vault(&desk, ACCOUNTS_CSV_FILE, "");
    sync(&desk, NOW + 14);
    assert_eq!(emails(&desk), ["a1@example.test"]);
    assert_eq!(
        fs::read_to_string(&copy).unwrap(),
        from_a_url,
        "the backup is untouched"
    );
}

/// A vault fleet selected first keeps its precedence over the sheet. Once the sheet retired the
/// fleet slot, a fleet the vault delivers later cannot restore it.
#[test]
fn initial_fleet_precedence_does_not_make_a_later_delivery_a_reactivation_grant() {
    let desk = Desk::new("m27");
    desk.default_login("v1@example.test");
    let written = concat!(
        "[[account]]\nlabel = \"m27\"\nmachines = [\"m27\"]\nemail = \"v1@example.test\"\n\n",
        "[[account]]\nlabel = \"m28\"\nmachines = [\"m28\"]\nemail = \"v2@example.test\"\n",
    );
    vault(&desk, FLEET_ROSTER_FILE, written);
    vault(&desk, ACCOUNTS_CSV_FILE, SHEET);
    sync(&desk, NOW);
    assert_eq!(emails(&desk), ["v1@example.test"]);
    assert_eq!(
        origin(&desk),
        FleetOrigin::Vault(FLEET_ROSTER_FILE.to_owned())
    );

    let delivered = Desk::new("m27");
    delivered.default_login("a1@example.test");
    vault(&delivered, ACCOUNTS_CSV_FILE, SHEET);
    sync(&delivered, NOW);
    let beside = fs::read_to_string(delivered.fleet_file()).unwrap();
    vault(&delivered, FLEET_ROSTER_FILE, &beside);
    assert!(!needs_sync(&delivered, NOW + 1));
    vault(
        &delivered,
        ACCOUNTS_CSV_FILE,
        &format!("{SHEET}a3@example.test,m27.2,1\n"),
    );
    sync(&delivered, NOW + 2);
    assert_eq!(
        emails(&delivered),
        ["a1@example.test", "a3@example.test"],
        "the retired fleet cannot outrank the sheet"
    );
    assert_eq!(
        origin(&delivered),
        FleetOrigin::Vault(ACCOUNTS_CSV_FILE.to_owned())
    );
    vault(&delivered, ACCOUNTS_CSV_FILE, "");
    assert!(matches!(origin(&delivered), FleetOrigin::Local { .. }));
}

/// A list found on this Mac yields to a sheet the vault delivers later that gives this machine
/// rows. The sync copies nothing into the backup.
#[test]
fn a_list_found_here_yields_to_a_sheet_delivered_later() {
    let desk = Desk::new("m27");
    desk.default_login("sol-m27@example.test");
    fs::write(
        desk.home.join(".claude.json.backup"),
        "{\"oauthAccount\":{\"emailAddress\":\"sun.m27.2@example.test\"}}\n",
    )
    .unwrap();
    sync(&desk, NOW);
    assert!(!desk.home.join(".secrets").join(FLEET_ROSTER_FILE).exists());
    assert_eq!(
        fleet_file_opt_in(&desk.fleet_file()),
        Some(OptIn::Discovered)
    );
    vault(
        &desk,
        ACCOUNTS_CSV_FILE,
        "email,computer,claude\nsol-m27@example.test,m27,1\nstar@example.test,m27.3,1\n",
    );
    assert!(needs_sync(&desk, NOW + 1));
    sync(&desk, NOW + 2);
    assert_eq!(emails(&desk), ["sol-m27@example.test", "star@example.test"]);
}

/// A Mac the sheet gives no row keeps the list found on it (a sync removes nothing, `accounts`
/// names the list as this Mac's own), until the sheet, edited, gives this machine a row, which
/// then outranks it.
#[test]
fn a_mac_the_sheet_does_not_name_keeps_the_list_found_on_it() {
    let desk = Desk::new("m27");
    desk.default_login("sol-m27@example.test");
    fs::write(
        desk.home.join(".claude.json.backup"),
        "{\"oauthAccount\":{\"emailAddress\":\"sun.m27.2@example.test\"}}\n",
    )
    .unwrap();
    sync(&desk, NOW);
    let found = fs::read(desk.fleet_file()).unwrap();
    let roster = fs::read(desk.roster()).unwrap();

    vault(
        &desk,
        ACCOUNTS_CSV_FILE,
        "email,computer,claude\nb1@example.test,m28,1\n",
    );
    sync(&desk, NOW + 2);
    assert!(!needs_sync(&desk, NOW + 2));
    assert_eq!(fs::read(desk.fleet_file()).unwrap(), found);
    assert_eq!(fs::read(desk.roster()).unwrap(), roster);
    assert_eq!(
        origin(&desk),
        FleetOrigin::Local {
            files: vec![desk.fleet_file()],
            opt_in: Some(OptIn::Discovered),
        }
    );

    vault(
        &desk,
        ACCOUNTS_CSV_FILE,
        "email,computer,claude\nb1@example.test,m28,1\nsol-m27@example.test,m27,1\n",
    );
    assert_eq!(
        origin(&desk),
        FleetOrigin::Vault(ACCOUNTS_CSV_FILE.to_owned())
    );
    assert!(needs_sync(&desk, NOW + 3));
    sync(&desk, NOW + 4);
    assert_eq!(emails(&desk), ["sol-m27@example.test"]);
}

/// With the sheet gone, a copy in the backup is not reported as a selected source.
#[test]
fn a_backup_copy_is_never_reported_as_the_vaults() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    vault(&desk, ACCOUNTS_CSV_FILE, SHEET);
    sync(&desk, NOW);
    fs::copy(desk.fleet_file(), backup_dir(&desk).join(FLEET_ROSTER_FILE)).unwrap();
    vault(&desk, ACCOUNTS_CSV_FILE, "");
    assert_eq!(
        origin(&desk),
        FleetOrigin::Local {
            files: vec![desk.fleet_file()],
            opt_in: Some(OptIn::Named),
        }
    );
}

/// A window's sync that fails (this machine's assignment conflicts) is logged once and not run
/// again while what it reads is unchanged, by the window or the setup pass; after 15 minutes it
/// runs once more, quietly; an edited sheet is read at once, its new failure logged, and the
/// sheet that gives this machine a row writes the roster. CONTROL: the first upkeep runs and
/// logs.
#[test]
fn a_failing_sync_waits_for_its_inputs_to_change() {
    let desk = Desk::new("m27");
    vault(
        &desk,
        ACCOUNTS_CSV_FILE,
        "email,computer,claude\nb1@example.test,m27,1\nb1@example.test,m28,1\n",
    );
    let fetch = |_: &Env, source: &str| -> Result<String, String> {
        Err(format!("no fetch in a test: {source}"))
    };
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let mut fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
    let logged = Arc::new(Mutex::new(Vec::<String>::new()));
    let into = Arc::clone(&logged);
    fx.log = Arc::new(move |line| into.lock().unwrap().push(line.to_owned()));
    let upkeep = |now: i64| {
        upkeep_claude(
            &desk.home,
            &desk.state,
            (&desk.roster(), now, UpkeepStep::Sync),
            &fx,
        );
    };
    let failed_at = || {
        rediscovery
            .lock()
            .unwrap()
            .failed
            .as_ref()
            .map(|failed| failed.at)
    };
    let logs = || logged.lock().unwrap().len();

    upkeep(NOW);
    assert_eq!(logs(), 1, "CONTROL: {:?}", logged.lock().unwrap());
    assert!(logged.lock().unwrap()[0].contains("no active Claude accounts"));
    let first = failed_at().expect("the failure is on record");
    upkeep(NOW + 5);
    upkeep(NOW + 65);
    let setup = resync_with(
        &desk.home,
        &desk.state,
        (&desk.roster(), NOW + 70, false),
        &fx,
    );
    assert!(!setup.synced, "{setup:?}");
    assert_eq!(failed_at(), Some(first), "no attempt on the same inputs");
    assert_eq!(logs(), 1);

    if let Some(failed) = rediscovery.lock().unwrap().failed.as_mut() {
        failed.at = Instant::now() - REDISCOVER_EVERY;
    }
    upkeep(NOW + 900);
    assert!(
        failed_at().is_some_and(|at| at > first),
        "retried after 15 minutes"
    );
    assert_eq!(logs(), 1, "the same failure is not logged again");
    let retried = failed_at().unwrap();

    vault(
        &desk,
        ACCOUNTS_CSV_FILE,
        "email,computer,claude\nb2@example.test,m27,1\nb2@example.test,m29,1\n",
    );
    upkeep(NOW + 905);
    assert!(
        failed_at().is_some_and(|at| at > retried),
        "an edit is read at once"
    );
    assert_eq!(logs(), 2);

    vault(
        &desk,
        ACCOUNTS_CSV_FILE,
        "email,computer,claude\nb1@example.test,m28,1\nb3@example.test,m27,1\n",
    );
    upkeep(NOW + 910);
    assert_eq!(failed_at(), None);
    assert_eq!(emails(&desk), ["b3@example.test"]);
    assert_eq!(logs(), 2);
}

/// A failure because another process held the accounts lock says nothing of what the sync read:
/// it holds no retry back, and the failure that follows on the same inputs for another reason is
/// news, logged and held. A success forgets the failure.
#[test]
fn a_held_lock_holds_no_retry_back_and_a_new_reason_is_news() {
    let (inputs, other) = ([1; 32], [2; 32]);
    let held = SyncError::retry(
        Retry::Busy,
        format!("{} at /state after 8000 ms", accounts::LOCK_HELD),
    );
    let refused =
        SyncError::from("no active Claude accounts found for machine `m27` in ai-accounts.csv");
    let mut failed = None;
    assert!(note_sync_result(&mut failed, inputs, Err(&held)), "CONTROL");
    assert!(!failed.as_ref().unwrap().holds_back(&inputs), "the lock's");
    assert!(!note_sync_result(&mut failed, inputs, Err(&held)));
    assert!(note_sync_result(&mut failed, inputs, Err(&refused)), "news");
    assert!(failed.as_ref().unwrap().holds_back(&inputs));
    assert!(!failed.as_ref().unwrap().holds_back(&other));
    assert!(!note_sync_result(&mut failed, inputs, Err(&refused)));
    assert!(note_sync_result(&mut failed, other, Err(&refused)));
    assert!(!note_sync_result(&mut failed, other, Ok(())));
    assert_eq!(failed, None);
}

/// A mailbox entry the vault delivers later is among what a failed sync waits on, so a
/// sync that found no account runs at once when one arrives. CONTROL: the digest is stable.
#[test]
fn a_mailbox_entry_delivered_later_is_read_at_once() {
    let desk = Desk::new("m27");
    let before = sync_inputs(&desk.home, &desk.state, &desk.roster(), NOW);
    assert_eq!(
        sync_inputs(&desk.home, &desk.state, &desk.roster(), NOW + 1),
        before,
        "CONTROL"
    );
    for name in [
        super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE,
        super::super::headless_oauth::MAILBOX_TOML_FILE,
    ] {
        let was = sync_inputs(&desk.home, &desk.state, &desk.roster(), NOW);
        vault(&desk, name, "[mailbox]\nuser = \"box@example.test\"\n");
        assert_ne!(
            sync_inputs(&desk.home, &desk.state, &desk.roster(), NOW),
            was,
            "{name}"
        );
    }
}
