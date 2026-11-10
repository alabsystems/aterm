// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The vault's sheet reaching every Mac (m22, 2026-10-27): the ALab vault delivers
//! `~/.secrets/ai-accounts.csv`, and a window's sync wrote aterm's own copy of the fleet file to
//! `~/.secrets/fleet-accounts.toml`, which then outranked the sheet, so a sheet published again
//! never reached the roster, while `accounts` called that copy the vault's. A Mac whose machine
//! the sheet gives no row retried and logged the same failure at every upkeep. A sync now copies
//! only a list found on this Mac into `~/.secrets`; the sheet outranks a copy aterm wrote, in any
//! format any aterm wrote it, wherever the sheet gives this machine rows (the sync that reads the
//! sheet retires its automatic use and preserves it); an initially selected fleet keeps
//! precedence, but delivery bookkeeping cannot reactivate a retired one. A failing sync
//! waits for its inputs to change. Every fixture email
//! is `example.test`.

use std::os::unix::fs::PermissionsExt;
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
fn source_less_manual_sync_preserves_an_active_legacy_source() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    let copy = secrets_dir(&desk).join(FLEET_ROSTER_FILE);
    fs::write(
        &copy,
        "[[account]]\nlabel='m27'\nemail='a1@example.test'\nmachines=['m27']\n",
    )
    .unwrap();
    sync(&desk, NOW);
    manual(&desk, None, &fetch_source_text).unwrap();
    assert!(
        !accounts::Store::at(&desk.state)
            .source_policy()
            .unwrap()
            .retired()
    );
    assert!(matches!(
        origin(&desk),
        FleetOrigin::Vault(_) | FleetOrigin::Local { .. }
    ));
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
                    secrets_dir(&desk).join(input)
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
                let why = manual(&desk, requested.to_str(), &fetch_source_text)
                    .err()
                    .expect("aliased input refused");
                assert!(
                    why.contains("aliases"),
                    "{input} roster={roster} hard={hard}: {why}"
                );
                assert_eq!(fs::read(&protected).unwrap(), before);
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
fn a_new_automatic_source_cannot_retire_itself_through_an_alias() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    let legacy = secrets_dir(&desk).join(FLEET_ROSTER_FILE);
    fs::write(
        &legacy,
        "[[account]]\nlabel='m27'\nemail='a1@example.test'\nmachines=['m27']\n",
    )
    .unwrap();
    let alias = desk.home.join("source.toml");
    fs::hard_link(&legacy, &alias).unwrap();
    fs::write(
        desk.config.join("aterm.toml"),
        format!(
            "[fleet]\nmachine='m27'\naccounts_source='{}'\n",
            alias.display()
        ),
    )
    .unwrap();
    let why = sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).unwrap_err();
    assert!(why.contains("another path"), "{why}");
    assert!(
        !accounts::Store::at(&desk.state)
            .source_policy()
            .unwrap()
            .retired()
    );
}

#[test]
fn a_dangling_retired_path_does_not_disable_live_sources() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    let secrets = secrets_dir(&desk);
    fs::write(secrets.join(ACCOUNTS_CSV_FILE), SHEET).unwrap();
    sync(&desk, NOW);
    std::os::unix::fs::symlink(desk.home.join("gone"), secrets.join(FLEET_ROSTER_FILE)).unwrap();
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
        let secrets = secrets_dir(&desk);
        fs::write(secrets.join(cli_fleet_file(cli)), "retired source").unwrap();
        if cli == Cli::Claude {
            fs::write(secrets.join(FLEET_ROSTER_FILE), "retired source").unwrap();
        }
        let sheet = secrets.join(ACCOUNTS_CSV_FILE);
        fs::write(&sheet, SHEET).unwrap();
        let store = accounts::Store::at_for(&desk.state, cli, &desk.home);
        let held = store.acquire().unwrap();
        store.retire_source_held(&held, NOW).unwrap();
        drop(held);
        let selected = local_cli_source(cli, &env, &desk.home, &mut |path, _| {
            source_entry_present(path)
        })
        .unwrap()
        .unwrap();
        assert_eq!(selected.path, sheet);
        assert!(selected.csv);
        fs::remove_file(store.source_policy_file()).unwrap();
        assert!(
            local_cli_source(cli, &env, &desk.home, &mut |path, _| source_entry_present(
                path
            ))
            .is_err()
        );
    }
}

#[test]
fn a_local_list_without_a_legacy_input_does_not_retire_or_seed_a_retired_slot() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    let secrets = secrets_dir(&desk);
    fs::write(desk.fleet_file(), "opt_in='discovered'\n[[account]]\nlabel='m27'\nemail='a1@example.test'\nmachines=['m27']\n").unwrap();
    sync(&desk, NOW);
    assert!(
        !accounts::Store::at(&desk.state)
            .source_policy()
            .unwrap()
            .retired()
    );
    assert!(secrets.join(FLEET_ROSTER_FILE).exists());
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
        let sheet = secrets_dir(&desk).join(ACCOUNTS_CSV_FILE);
        fs::write(&sheet, SHEET).unwrap();
        let result = sync_accounts_for_init_with(
            &desk.home,
            &desk.state,
            &desk.roster(),
            NOW,
            || {
                if changed {
                    fs::write(&sheet, "email,computer,claude\na1@example.test,m99,1\n").unwrap();
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
    let secrets = secrets_dir(&desk);
    let sheet = secrets.join(ACCOUNTS_CSV_FILE);
    fs::write(&sheet, SHEET).unwrap();
    sync(&desk, NOW);
    let before = sync_inputs(&desk.home, &desk.state, &desk.roster(), NOW);
    fs::write(secrets.join(FLEET_ROSTER_FILE), "ignored retired source").unwrap();
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
    fs::remove_file(sheet).unwrap();
    assert!(!has_fleet_source(&secrets, &env.accounts_store()));
}

#[test]
fn linked_local_projections_neither_overwrite_nor_reactivate_a_legacy_source() {
    for hard in [false, true] {
        for retired in [false, true] {
            let desk = Desk::new("m27");
            desk.default_login("a1@example.test");
            let secrets = secrets_dir(&desk);
            let sheet = secrets.join(ACCOUNTS_CSV_FILE);
            let copy = secrets.join(FLEET_ROSTER_FILE);
            fs::write(&sheet, SHEET).unwrap();
            if retired {
                sync(&desk, NOW);
                fs::remove_file(desk.fleet_file()).unwrap();
                fs::remove_file(&sheet).unwrap();
            }
            let text = format!(
                "{FLEET_HEAD_NAMED}\nopt_in='named'\n[[account]]\nlabel='m27'\nemail='a1@example.test'\nmachines=['m27']\n"
            );
            fs::write(&copy, &text).unwrap();
            if hard {
                fs::hard_link(&copy, desk.fleet_file()).unwrap();
            } else {
                std::os::unix::fs::symlink(&copy, desk.fleet_file()).unwrap();
            }
            let why = sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW + 1)
                .unwrap_err();
            assert!(
                why.contains("aliases") || why.contains("another path"),
                "{why}"
            );
            assert_eq!(fs::read_to_string(&copy).unwrap(), text);
            assert_eq!(fs::read_to_string(desk.fleet_file()).unwrap(), text);
        }
    }
}

#[test]
fn an_automatic_key_cannot_name_the_retired_source_through_an_alias() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    let secrets = secrets_dir(&desk);
    fs::write(secrets.join(ACCOUNTS_CSV_FILE), SHEET).unwrap();
    sync(&desk, NOW);
    let copy = secrets.join(FLEET_ROSTER_FILE);
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
    assert!(why.contains("another path"), "{why}");
    assert!(matches!(origin(&desk), FleetOrigin::SourcePending(_)));
}

#[test]
fn retired_copy_is_preserved_and_does_not_return_after_sheet_withdrawal() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    let secrets = secrets_dir(&desk);
    let sheet = secrets.join(ACCOUNTS_CSV_FILE);
    let copy = secrets.join(FLEET_ROSTER_FILE);
    fs::write(&sheet, SHEET).unwrap();
    sync(&desk, NOW);
    let store = accounts::Store::at(&desk.state);
    let policy = fs::read(store.source_policy_file()).unwrap();
    let stale = "opt_in='named'\n[[account]]\nlabel='stale'\nemail='stale@example.test'\nmachines=['m27']\n";
    fs::write(&copy, stale).unwrap();
    fs::remove_file(&sheet).unwrap();
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
    sync_accounts_with(&env, Some(copy.to_str().unwrap()), None, &how).unwrap();
    assert_eq!(
        fs::read(store.source_policy_file()).unwrap(),
        policy,
        "explicit import grants no future automatic use"
    );
    assert!(store.source_policy().unwrap().retired());
    assert!(matches!(origin(&desk), FleetOrigin::Local { files, .. } if !files.contains(&copy)));
}

#[test]
fn a_lost_or_corrupt_policy_refuses_automatic_sync_without_touching_accounts() {
    for replacement in [None, Some("broken policy")] {
        let desk = Desk::new("m27");
        desk.default_login("a1@example.test");
        let sheet = secrets_dir(&desk).join(ACCOUNTS_CSV_FILE);
        fs::write(&sheet, SHEET).unwrap();
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
    let copy = secrets_dir(&desk).join(FLEET_ROSTER_FILE);
    let text = "[[account]]\nlabel='old'\nemail='a1@example.test'\nmachines=['m27']\n";
    fs::write(&copy, text).unwrap();
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
    assert_eq!(fs::read_to_string(copy).unwrap(), text);
}

fn secrets_dir(desk: &Desk) -> PathBuf {
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

/// The vault's own record that it wrote `~/.secrets/<name>` on this Mac, as its subscriber
/// keeps it (`~/.config/aterm/secrets/state.toml`, private to this user).
fn vault_wrote(desk: &Desk, name: &str) {
    let dir = desk.home.join(".config/aterm/secrets");
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let digest = aterm_secrets::materialize::digest_hex(
        &fs::read(desk.home.join(".secrets").join(name)).unwrap(),
    );
    let state = dir.join("state.toml");
    fs::write(
        &state,
        format!("membership = \"in\"\nseq = 1\n\n[manifest]\n\"{name}\" = \"{digest}\"\n"),
    )
    .unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
}

/// A sync from the vault's sheet writes no copy into `~/.secrets`; a copy an earlier build wrote
/// there (byte for byte the fleet file beside the roster) is outranked by the sheet, named as
/// the sheet by `accounts`, and durably excluded without deleting it; every
/// later edit of the sheet, a row added or dropped, reaches the roster; and a sheet withdrawn
/// leaves its rows out of `~/.secrets`. CONTROL: the first sync, before any copy, takes the sheet
/// and needs no other.
#[test]
fn the_sheet_outranks_the_copy_aterm_wrote_and_each_edit_reaches_the_roster() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    let secrets = secrets_dir(&desk);
    let (sheet, copy) = (
        secrets.join(ACCOUNTS_CSV_FILE),
        secrets.join(FLEET_ROSTER_FILE),
    );
    fs::write(&sheet, SHEET).unwrap();
    sync(&desk, NOW);
    assert_eq!(emails(&desk), ["a1@example.test"], "CONTROL");
    assert!(!copy.exists(), "a sync from the sheet copies nothing there");
    assert!(!needs_sync(&desk, NOW + 1), "CONTROL: converged");

    // What an earlier build wrote: the fleet file's own bytes, beside the sheet.
    fs::copy(desk.fleet_file(), &copy).unwrap();
    assert_eq!(origin(&desk), FleetOrigin::Vault(sheet.clone()));
    assert!(
        !needs_sync(&desk, NOW + 2),
        "the retired copy cannot request another sync"
    );

    let edited = format!("{SHEET}a3@example.test,m27.2,1\n");
    fs::write(&sheet, &edited).unwrap();
    sync(&desk, NOW + 3);
    assert_eq!(emails(&desk), ["a1@example.test", "a3@example.test"]);
    assert!(copy.exists(), "the retired copy is preserved");
    assert!(!needs_sync(&desk, NOW + 4));

    fs::write(&sheet, SHEET).unwrap();
    assert!(needs_sync(&desk, NOW + 5), "a dropped row is an edit too");
    sync(&desk, NOW + 6);
    assert_eq!(emails(&desk), ["a1@example.test"]);
    assert!(copy.exists(), "the retired copy stays preserved");

    fs::remove_file(&sheet).unwrap();
    sync(&desk, NOW + 7);
    assert_eq!(emails(&desk), ["a1@example.test"]);
    assert!(
        copy.exists(),
        "withdrawal does not remove the retired source"
    );
}

/// A copy an older aterm wrote yields to the sheet though its bytes are not the fleet file's: one
/// with the comments of builds before 0.137 and one that records `opt_in = "key"`, as a sync from
/// a URL wrote it. One an older aterm writes again after retirement is still excluded.
/// With no sheet, such a copy calls for no sync at every
/// upkeep: it holds the fleet file's accounts.
#[test]
fn a_copy_an_older_aterm_wrote_yields_to_the_sheet_whenever_it_writes_it() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    let secrets = secrets_dir(&desk);
    let (sheet, copy) = (
        secrets.join(ACCOUNTS_CSV_FILE),
        secrets.join(FLEET_ROSTER_FILE),
    );
    fs::write(&sheet, SHEET).unwrap();
    sync(&desk, NOW);
    let beside = fs::read_to_string(desk.fleet_file()).unwrap();
    let fleet = FleetRoster::from_toml(&beside, "beside").unwrap();
    let as_0_136_wrote_it = format!(
        "{FLEET_HEAD_NAMED}\n\
         # Run `aterm harness accounts sync <path-or-url>` on any machine to materialize\n\
         # its local `accounts.toml`.\n\n\
         # Why this machine keeps its fleet accounts: `named`, a sync handed its source\n\
         # (delete this file to stop); `key`, `[fleet] accounts_source` in aterm.toml.\n\
         opt_in = \"named\"\n\n{}",
        fleet.to_toml().split_once("\n\n").unwrap().1
    );
    let from_a_url = fleet.to_toml_for(Some(OptIn::Key));
    for (now, older) in [(NOW + 10, &as_0_136_wrote_it), (NOW + 20, &from_a_url)] {
        assert_ne!(older.as_str(), beside, "CONTROL: another format");
        fs::write(&copy, older).unwrap();
        assert_eq!(origin(&desk), FleetOrigin::Vault(sheet.clone()), "{older}");
        assert!(!needs_sync(&desk, now));
        sync(&desk, now + 1);
        assert_eq!(fs::read_to_string(&copy).unwrap(), *older, "{older}");
        assert!(!needs_sync(&desk, now + 2));
    }

    // An older aterm's sync from the sheet, after the removal: it copies the fleet file there.
    fs::copy(desk.fleet_file(), &copy).unwrap();
    fs::write(&sheet, format!("{SHEET}a3@example.test,m27.2,1\n")).unwrap();
    sync(&desk, NOW + 30);
    assert!(copy.exists(), "retirement preserves the source");
    assert_eq!(emails(&desk), ["a1@example.test", "a3@example.test"]);

    fs::remove_file(&sheet).unwrap();
    let kept = fs::read_to_string(desk.fleet_file()).unwrap();
    let older = FleetRoster::from_toml(&kept, "kept")
        .unwrap()
        .to_toml_for(Some(OptIn::Key));
    fs::write(&copy, &older).unwrap();
    assert_ne!(older, kept, "CONTROL: another format");
    assert!(!needs_sync(&desk, NOW + 40), "nothing new to read from it");
}

/// An initially selected fleet keeps precedence. A manifest entry alone cannot
/// restore automatic use after a sheet has retired the legacy source slot.
#[test]
fn initial_fleet_precedence_does_not_make_delivery_bookkeeping_a_reactivation_grant() {
    let desk = Desk::new("m27");
    desk.default_login("v1@example.test");
    let secrets = secrets_dir(&desk);
    let (sheet, copy) = (
        secrets.join(ACCOUNTS_CSV_FILE),
        secrets.join(FLEET_ROSTER_FILE),
    );
    let written = concat!(
        "[[account]]\nlabel = \"m27\"\nmachines = [\"m27\"]\nemail = \"v1@example.test\"\n\n",
        "[[account]]\nlabel = \"m28\"\nmachines = [\"m28\"]\nemail = \"v2@example.test\"\n",
    );
    fs::write(&copy, written).unwrap();
    fs::write(&sheet, SHEET).unwrap();
    sync(&desk, NOW);
    assert_eq!(emails(&desk), ["v1@example.test"]);
    assert_eq!(fs::read_to_string(&copy).unwrap(), written);
    assert_eq!(origin(&desk), FleetOrigin::Vault(copy.clone()));

    let delivered = Desk::new("m27");
    delivered.default_login("a1@example.test");
    let secrets = secrets_dir(&delivered);
    let (sheet, copy) = (
        secrets.join(ACCOUNTS_CSV_FILE),
        secrets.join(FLEET_ROSTER_FILE),
    );
    fs::write(&sheet, SHEET).unwrap();
    sync(&delivered, NOW);
    fs::copy(delivered.fleet_file(), &copy).unwrap();
    vault_wrote(&delivered, FLEET_ROSTER_FILE);
    let bytes = fs::read(&copy).unwrap();
    assert!(!needs_sync(&delivered, NOW + 1));
    fs::write(&sheet, format!("{SHEET}a3@example.test,m27.2,1\n")).unwrap();
    sync(&delivered, NOW + 2);
    assert_eq!(
        fs::read(&copy).ok(),
        Some(bytes),
        "the vault's fleet is kept"
    );
    assert_eq!(
        emails(&delivered),
        ["a1@example.test", "a3@example.test"],
        "the retired fleet cannot outrank the sheet"
    );
    assert_eq!(origin(&delivered), FleetOrigin::Vault(sheet.clone()));
    fs::remove_file(&sheet).unwrap();
    assert!(
        matches!(origin(&delivered), FleetOrigin::Local { files, .. } if !files.contains(&copy))
    );
}

/// A list found on this Mac is still copied into `~/.secrets`, and a sheet the vault delivers
/// later that gives this machine rows outranks that copy, whose sync retires it and takes the
/// sheet's rows.
#[test]
fn a_copy_of_a_list_found_here_yields_to_a_sheet_delivered_later() {
    let desk = Desk::new("m27");
    desk.default_login("sol-m27@example.test");
    fs::write(
        desk.home.join(".claude.json.backup"),
        "{\"oauthAccount\":{\"emailAddress\":\"sun.m27.2@example.test\"}}\n",
    )
    .unwrap();
    let secrets = secrets_dir(&desk);
    let copy = secrets.join(FLEET_ROSTER_FILE);
    sync(&desk, NOW);
    assert_eq!(
        fleet_file_opt_in(&desk.fleet_file()),
        Some(OptIn::Discovered)
    );
    assert_eq!(
        fs::read(&copy).unwrap(),
        fs::read(desk.fleet_file()).unwrap()
    );

    fs::write(
        secrets.join(ACCOUNTS_CSV_FILE),
        "email,computer,claude\nsol-m27@example.test,m27,1\nstar@example.test,m27.3,1\n",
    )
    .unwrap();
    assert!(needs_sync(&desk, NOW + 1));
    sync(&desk, NOW + 2);
    assert_eq!(emails(&desk), ["sol-m27@example.test", "star@example.test"]);
    assert!(copy.exists());
}

/// A Mac the sheet gives no row keeps the list found on it: aterm's copy stays its source (no
/// sync is called for, a sync removes nothing, `accounts` names the list as this Mac's own),
/// until the sheet, edited, gives this machine a row, which then outranks the copy.
#[test]
fn a_mac_the_sheet_does_not_name_keeps_the_list_found_on_it() {
    let desk = Desk::new("m27");
    desk.default_login("sol-m27@example.test");
    fs::write(
        desk.home.join(".claude.json.backup"),
        "{\"oauthAccount\":{\"emailAddress\":\"sun.m27.2@example.test\"}}\n",
    )
    .unwrap();
    let secrets = secrets_dir(&desk);
    let (sheet, copy) = (
        secrets.join(ACCOUNTS_CSV_FILE),
        secrets.join(FLEET_ROSTER_FILE),
    );
    sync(&desk, NOW);
    let found = fs::read(desk.fleet_file()).unwrap();
    assert_eq!(fs::read(&copy).unwrap(), found, "CONTROL: aterm's copy");
    let roster = fs::read(desk.roster()).unwrap();

    fs::write(&sheet, "email,computer,claude\nb1@example.test,m28,1\n").unwrap();
    assert!(!needs_sync(&desk, NOW + 1));
    sync(&desk, NOW + 2);
    assert_eq!(fs::read(&copy).unwrap(), found, "the copy stays");
    assert_eq!(fs::read(desk.fleet_file()).unwrap(), found);
    assert_eq!(fs::read(desk.roster()).unwrap(), roster);
    assert_eq!(
        origin(&desk),
        FleetOrigin::Local {
            files: vec![desk.fleet_file(), copy.clone()],
            opt_in: Some(OptIn::Discovered),
        }
    );

    fs::write(
        &sheet,
        "email,computer,claude\nb1@example.test,m28,1\nsol-m27@example.test,m27,1\n",
    )
    .unwrap();
    assert_eq!(origin(&desk), FleetOrigin::Vault(sheet.clone()));
    assert!(needs_sync(&desk, NOW + 3));
    sync(&desk, NOW + 4);
    assert!(copy.exists());
    assert_eq!(emails(&desk), ["sol-m27@example.test"]);
}

/// With the sheet gone, a retired legacy copy is not reported as a selected source.
#[test]
fn the_copy_aterm_wrote_is_never_reported_as_the_vaults() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    let secrets = secrets_dir(&desk);
    let (sheet, copy) = (
        secrets.join(ACCOUNTS_CSV_FILE),
        secrets.join(FLEET_ROSTER_FILE),
    );
    fs::write(&sheet, SHEET).unwrap();
    sync(&desk, NOW);
    fs::copy(desk.fleet_file(), &copy).unwrap();
    fs::remove_file(&sheet).unwrap();
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
    let sheet = secrets_dir(&desk).join(ACCOUNTS_CSV_FILE);
    fs::write(
        &sheet,
        "email,computer,claude\nb1@example.test,m27,1\nb1@example.test,m28,1\n",
    )
    .unwrap();
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

    fs::write(
        &sheet,
        "email,computer,claude\nb2@example.test,m27,1\nb2@example.test,m29,1\n",
    )
    .unwrap();
    upkeep(NOW + 905);
    assert!(
        failed_at().is_some_and(|at| at > retried),
        "an edit is read at once"
    );
    assert_eq!(logs(), 2);

    fs::write(
        &sheet,
        "email,computer,claude\nb1@example.test,m28,1\nb3@example.test,m27,1\n",
    )
    .unwrap();
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

/// A mailbox file the vault delivers to `~/.secrets` is among what a failed sync waits on, so a
/// sync that found no account runs at once when one arrives. CONTROL: the digest is stable.
#[test]
fn a_mailbox_file_delivered_later_is_read_at_once() {
    let desk = Desk::new("m27");
    let secrets = secrets_dir(&desk);
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
        fs::write(
            secrets.join(name),
            "[mailbox]\nuser = \"box@example.test\"\n",
        )
        .unwrap();
        assert_ne!(
            sync_inputs(&desk.home, &desk.state, &desk.roster(), NOW),
            was,
            "{name}"
        );
    }
}
