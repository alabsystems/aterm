// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The periodic resync's hygiene and `accounts add` against a fleet (the m2 incident of
//! 2026-10-24, part 3): the first-run setup's retries rewrote `accounts.toml` and
//! `fleet-accounts.toml` every pass and threw away the rows the sync left to sign in, and a row
//! `accounts add` listed was dropped at the next sync. A pass now syncs only what changed and
//! writes only what differs, hands the rows it leaves with no login to the automatic sign-in
//! ladder, and `add` lists its row where the next sync reads it, or refuses by the source's name.
//! Every fixture email is `example.test`.

#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::login_tests::{Desk, NOW, upkeep_effects};
use super::*;
use crate::harness::cli::{AccountsCmd, AccountsHost, run_accounts};

/// A host that runs nothing: `add` and the report never reach it.
struct NoHost;

impl AccountsHost for NoHost {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<super::super::align::Captured, String> {
        Err("no auth status in a test".to_owned())
    }
    fn unaskable(&self) -> Option<String> {
        Some("a test".to_owned())
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        Err("no login in a test".to_owned())
    }
    fn now(&self) -> i64 {
        NOW
    }
}

fn env_of(desk: &Desk, now: i64) -> Env {
    quiet_env(
        &desk.home,
        &desk.state,
        &desk.config.join("aterm.toml"),
        now,
    )
}

/// `accounts add <label> [--email <email>]` on `desk`: its exit code, stdout and stderr.
fn add(desk: &Desk, label: &str, email: Option<&str>) -> (ExitCode, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_accounts(
        &AccountsCmd::Add {
            label: label.to_owned(),
            class: Class::Dedicated,
            email: email.map(str::to_owned),
            identity_email: None,
            browser: None,
        },
        &env_of(desk, NOW),
        &NoHost,
        &mut out,
        &mut err,
    );
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

/// `aterm harness accounts --json`'s `list` object on `desk`.
fn listed_origin(desk: &Desk) -> (Option<String>, Option<String>) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_accounts(
        &AccountsCmd::Report { json: true },
        &env_of(desk, NOW),
        &NoHost,
        &mut out,
        &mut err,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
    let report: Value = aterm_json::from_str(String::from_utf8_lossy(&out).trim()).unwrap();
    let list = report.get("list");
    let word = |key: &str| {
        list.and_then(|l| l.get(key))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    (word("origin"), word("source"))
}

/// A desk whose default login and its backup show two accounts of m27: discovery's fleet.
fn discovered_desk() -> Desk {
    let desk = Desk::new("m27");
    desk.default_login("sol-m27@example.test");
    fs::write(
        desk.home.join(".claude.json.backup"),
        "{\"oauthAccount\":{\"emailAddress\":\"sun.m27.2@example.test\"}}\n",
    )
    .unwrap();
    desk
}

/// Each file's inode: a file an atomic write replaced has another.
#[cfg(unix)]
fn inodes(paths: &[PathBuf]) -> Vec<u64> {
    paths
        .iter()
        .map(|p| fs::metadata(p).map_or(0, |m| m.ino()))
        .collect()
}

/// The setup pass's resync over a roster that stands: a pass with nothing new syncs nothing, a
/// sync that finds what is there changes nothing (Unix also checks each file's inode),
/// and the default login's move is a change it syncs at once. CONTROL: the first pass,
/// with no roster, writes both files and reports the row it left with no login.
#[test]
fn a_resync_syncs_only_what_changed_and_writes_only_what_differs() {
    let desk = discovered_desk();
    let fetch = |_: &Env, source: &str| -> Result<String, String> {
        Err(format!("no fetch in a test: {source}"))
    };
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);

    let first = resync_with(&desk.home, &desk.state, (&desk.roster(), NOW, false), &fx);
    assert!(first.synced && first.changed, "{first:?}");
    assert_eq!(first.failed, None);
    assert_eq!(first.to_sign_in, ["m27-2"]);
    #[cfg(unix)]
    let (watched, held) = {
        let watched = [desk.roster(), desk.fleet_file(), desk.store().wake_path()];
        let held = inodes(&watched);
        assert!(held.iter().all(|&ino| ino != 0), "{held:?}");
        (watched, held)
    };

    let quiet = resync_with(
        &desk.home,
        &desk.state,
        (&desk.roster(), NOW + 60, false),
        &fx,
    );
    assert_eq!(
        quiet,
        Resync::default(),
        "nothing changed, so nothing synced"
    );
    let InitSynced { changed, .. } = sync_accounts_for_init_with(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW + 61,
        || {},
        &fetch,
    )
    .expect("the sync finds what is there");
    assert!(!changed);
    #[cfg(unix)]
    assert_eq!(
        inodes(&watched),
        held,
        "no file rewritten, no watcher woken"
    );

    desk.default_login("sun.m27.2@example.test");
    let moved = resync_with(
        &desk.home,
        &desk.state,
        (&desk.roster(), NOW + 120, false),
        &fx,
    );
    assert!(moved.synced && moved.changed, "{moved:?}");
    assert_eq!(
        desk.rows().row_for_login(None).unwrap().label,
        "m27-2",
        "the default login's row is its account's"
    );
    #[cfg(unix)]
    assert_ne!(inodes(&watched)[0], held[0]);
}

/// The rows a resync leaves with no login here go to the automatic ladder at once, and where
/// no route can sign one in that is on record (its routes named), with nothing started and no
/// person asked. CONTROL: a resync that may not sign in (an instance that keeps no fleet
/// accounts of its own) reports the row and records nothing.
#[test]
fn the_rows_a_resync_leaves_without_a_login_go_to_the_ladder_and_on_record() {
    for sign_in in [false, true] {
        let desk = discovered_desk();
        let fetch = |_: &Env, source: &str| -> Result<String, String> {
            Err(format!("no fetch in a test: {source}"))
        };
        let started = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&started);
        let spawn = move |_work: Box<dyn FnOnce() + Send>| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
        let done = resync_with(&desk.home, &desk.state, (&desk.roster(), NOW, sign_in), &fx);
        assert_eq!(done.to_sign_in, ["m27-2"], "{done:?}");
        assert!(!done.renewal);
        assert_eq!(
            started.load(Ordering::SeqCst),
            0,
            "no route, so nothing started"
        );
        let (logins, _) = desk.store().load_logins(NOW).unwrap();
        let attempt = logins.get("m27-2").and_then(|b| b.sign_in_attempt.clone());
        if sign_in {
            let attempt = attempt.expect("the attempt is on record");
            assert_eq!(attempt.outcome, accounts::AttemptOutcome::NoRoute);
            assert!(!attempt.routes.is_empty());
        } else {
            assert_eq!(attempt, None, "CONTROL");
        }
    }
}

/// `accounts add` on a fleet discovery made lists the row in the fleet file too, for this
/// machine, so the next sync (from init, a window's upkeep or a person's `accounts sync`) keeps
/// it with its own login directory; the file still says `discovered`. CONTROL: a row with no
/// email, which a fleet cannot list, is refused and nothing is written.
#[test]
fn a_row_added_beside_a_local_fleet_survives_the_next_sync() {
    let desk = discovered_desk();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("discovered");
    let (roster_was, fleet_was) = (
        fs::read(desk.roster()).unwrap(),
        fs::read(desk.fleet_file()).unwrap(),
    );
    let (code, _, err) = add(&desk, "work", None);
    assert_eq!(code, ExitCode::from(1));
    assert!(
        err.contains(&desk.fleet_file().display().to_string()),
        "{err}"
    );
    assert_eq!(fs::read(desk.roster()).unwrap(), roster_was, "CONTROL");
    assert_eq!(fs::read(desk.fleet_file()).unwrap(), fleet_was, "CONTROL");

    let (code, out, err) = add(&desk, "work", Some("work@example.test"));
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(
        out.contains(&desk.fleet_file().display().to_string()),
        "{out}"
    );
    let fleet_text = fs::read_to_string(desk.fleet_file()).unwrap();
    assert_eq!(opt_in_of(&fleet_text), Some(OptIn::Discovered));
    let fleet = FleetRoster::from_toml(&fleet_text, "fleet").unwrap();
    let listed = fleet.accounts.iter().find(|a| a.label == "work").unwrap();
    assert_eq!(listed.machines, ["m27"]);
    let dir = desk.rows().row("work").and_then(|r| r.dir.clone()).unwrap();

    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW + 10).expect("init");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let synced = run_accounts_sync_with(
        &env_of(&desk, NOW + 20),
        (None, None, true),
        (&mut out, &mut err),
        &SyncHow {
            fetch: &fetch_source_text,
            ladder: &[],
            fleet_snapshots: &[],
            heal: None,
            allow_empty_pool: false,
        },
    );
    assert_eq!(
        synced,
        ExitCode::SUCCESS,
        "{}",
        String::from_utf8_lossy(&err)
    );
    let roster = desk.rows();
    let kept = roster.row("work").expect("the added row survives the sync");
    assert_eq!(kept.email.as_deref(), Some("work@example.test"));
    assert_eq!(kept.dir.as_deref(), Some(dir.as_str()));
    assert_eq!(roster.rows.len(), 3);
}

/// Where a source this machine reads and does not write makes the roster (the vault's fleet of
/// several machines, `[fleet] accounts_source`), `accounts add` refuses by that source's name and
/// writes nothing: the row would be dropped at the next sync.
#[test]
fn a_row_added_under_a_source_this_machine_does_not_write_is_refused_by_name() {
    let desk = Desk::new("m27");
    desk.default_login("sol-m27@example.test");
    let secrets = desk.home.join(".secrets");
    aterm_types::fs_restricted::ensure_private_dir(&secrets).unwrap();
    let vault = concat!(
        "[[account]]\nlabel = \"m27\"\nmachines = [\"m27\"]\nemail = \"sol-m27@example.test\"\n\n",
        "[[account]]\nlabel = \"m28\"\nmachines = [\"m28\"]\nemail = \"m28@example.test\"\n",
    );
    let vault_path = secrets.join(FLEET_ROSTER_FILE);
    fs::write(&vault_path, vault).unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("vault fleet");
    let roster_was = fs::read(desk.roster()).unwrap();
    let (code, _, err) = add(&desk, "work", Some("work@example.test"));
    assert_eq!(code, ExitCode::from(1));
    assert!(err.contains(&vault_path.display().to_string()), "{err}");
    assert_eq!(fs::read(desk.roster()).unwrap(), roster_was);
    assert_eq!(fs::read_to_string(&vault_path).unwrap(), vault);
    assert!(!desk.store().login_dir("work").unwrap().exists());
    assert_eq!(
        listed_origin(&desk),
        (
            Some("vault".to_owned()),
            Some(vault_path.display().to_string())
        )
    );

    let keyed = Desk::new("m27");
    let source = "https://fleet.example.test/fleet-accounts.toml";
    fs::write(
        keyed.config.join("aterm.toml"),
        format!("[fleet]\nmachine = \"m27\"\naccounts_source = \"{source}\"\n"),
    )
    .unwrap();
    let (code, _, err) = add(&keyed, "work", Some("work@example.test"));
    assert_eq!(code, ExitCode::from(1));
    assert!(err.contains(source), "{err}");
    assert!(!keyed.roster().exists());
}

/// A fleet discovery made reports itself as found on this Mac (`discovered`), the file and the
/// report alike; an older file that says `named` is still read, and reported as it says.
#[test]
fn a_discovered_list_says_it_was_found_on_this_mac() {
    let desk = discovered_desk();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("discovered");
    assert_eq!(
        fleet_file_opt_in(&desk.fleet_file()),
        Some(OptIn::Discovered)
    );
    assert_eq!(
        listed_origin(&desk),
        (
            Some("discovered".to_owned()),
            Some(desk.fleet_file().display().to_string())
        )
    );

    let older = Desk::new("m2");
    older.default_login("fb@example.test");
    let fleet = FleetRoster::from_toml(
        concat!(
            "[[account]]\nlabel = \"m2\"\nmachines = [\"m2\"]\nemail = \"fb@example.test\"\n\n",
            "[[account]]\nlabel = \"m2-2\"\nmachines = [\"m2\"]\nemail = \"m2@example.test\"\n",
        ),
        "fixture",
    )
    .unwrap();
    fs::write(older.fleet_file(), fleet.to_toml_for(Some(OptIn::Named))).unwrap();
    sync_accounts_for_init(&older.home, &older.state, &older.roster(), NOW).expect("named");
    assert_eq!(older.rows().rows.len(), 2);
    assert_eq!(listed_origin(&older).0.as_deref(), Some("named"));
}

/// A `[fleet] accounts_source` naming a local file is synced from each time it changes: at the
/// first upkeep pass that reads it (beside a roster an earlier sync made), then when edited,
/// and the row it gained appears. CONTROL: a pass before revalidation syncs nothing.
#[test]
fn a_local_accounts_source_is_synced_again_each_time_it_changes() {
    let desk = Desk::new("m27");
    desk.default_login("sol-m27@example.test");
    let source = desk.config.join("team-fleet.toml");
    let write = |rows: &[(&str, &str)], at: u64| {
        let text: String = rows
            .iter()
            .map(|(label, email)| {
                format!(
                    "[[account]]\nlabel = \"{label}\"\nmachines = [\"m27\"]\nemail = \"{email}\"\n\n"
                )
            })
            .collect();
        fs::write(&source, text).unwrap();
        fs::File::options()
            .write(true)
            .open(&source)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(at))
            .unwrap();
    };
    write(&[("m27", "sol-m27@example.test")], 1_000);
    fs::write(
        desk.config.join("aterm.toml"),
        format!(
            "[fleet]\nmachine = \"m27\"\naccounts_source = {}\n",
            aterm_toml::Value::from(source.to_string_lossy().into_owned())
        ),
    )
    .unwrap();
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let fx = upkeep_effects(&fetch_source_text, &spawn, &cooldown, &rediscovery, &guard);
    let pass = |now: i64| resync_with(&desk.home, &desk.state, (&desk.roster(), now, false), &fx);

    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW - 60).expect("roster");
    let first = pass(NOW);
    assert!(first.synced, "{first:?}");
    assert_eq!(first.failed, None);
    assert_eq!(desk.rows().rows.len(), 1);
    let quiet = pass(NOW + 60);
    assert!(!quiet.synced, "CONTROL: {quiet:?}");

    write(
        &[
            ("m27", "sol-m27@example.test"),
            ("m27-2", "sun.m27.2@example.test"),
        ],
        2_000,
    );
    let edited = pass(NOW + 120);
    assert!(edited.synced && edited.changed, "{edited:?}");
    assert!(
        desk.rows()
            .rows
            .iter()
            .any(|r| r.email.as_deref() == Some("sun.m27.2@example.test"))
    );
}

/// THE CONVERGE READ OF A RESYNC is due wherever the resync syncs: a roster that is missing,
/// before the first sync and again once removed, never reads as in step; after each sync that
/// wrote it, the read in the same process reads nothing left. CONTROL: `[harness] accounts =
/// "off"` reads nothing due over the same missing roster.
/// NEGATIVE CONTROL: a read that leaves the missing roster to `needs_init_accounts_sync` reads
/// the removed roster as in step.
#[test]
fn the_converge_read_is_due_where_the_resync_syncs_a_missing_roster() {
    let desk = discovered_desk();
    let fetch = |_: &Env, source: &str| -> Result<String, String> {
        Err(format!("no fetch in a test: {source}"))
    };
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let read = Mutex::new(Rediscovery::default());
    let due = |now: i64| resync_due_with(&desk.home, &desk.state, &desk.roster(), now, &read);
    // Each runner is a process of its own: a resync with fresh effects, as the pass's apply.
    let resync = |now: i64| {
        let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
        let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
        resync_with(&desk.home, &desk.state, (&desk.roster(), now, false), &fx)
    };

    assert!(
        due(NOW).is_some_and(|why| why.contains("the roster is missing")),
        "no roster yet"
    );
    let first = resync(NOW);
    assert!(first.synced && first.changed, "{first:?}");
    assert_eq!(
        due(NOW + 1),
        None,
        "the read after the sync reads it in step"
    );

    fs::remove_file(desk.roster()).unwrap();
    assert!(
        due(NOW + 2).is_some_and(|why| why.contains("the roster is missing")),
        "a removed roster is never read as in step"
    );
    let back = resync(NOW + 3);
    assert!(back.synced && desk.roster().is_file(), "{back:?}");
    assert_eq!(due(NOW + 4), None);

    fs::remove_file(desk.roster()).unwrap();
    fs::write(
        desk.config.join("aterm.toml"),
        "[fleet]\nmachine = \"m27\"\n[harness]\naccounts = \"off\"\n",
    )
    .unwrap();
    assert_eq!(due(NOW + 5), None, "CONTROL: off reads nothing due");
}

#[test]
fn a_kimi_only_resync_reaches_current_without_a_claude_roster_or_source() {
    let desk = Desk::new("m16");
    let config = desk.config.join("aterm.toml");
    let kimi = accounts::roster_beside_for(&config, Cli::Kimi).unwrap();
    fs::write(&kimi, "").unwrap();
    let secrets = desk.home.join(".secrets");
    aterm_types::fs_restricted::ensure_private_dir(&secrets).unwrap();
    let source = secrets.join(cli_fleet_file(Cli::Kimi));
    let fleet = "[[account]]\nlabel='kimi-1'\nemail='kimi-one@example.test'\n\
                 machines=['m16']\nregion='global'\n";
    fs::write(&source, fleet).unwrap();
    assert!(!desk.roster().exists() && !desk.fleet_file().exists());
    assert!(!has_init_accounts_source(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW
    ));
    assert!(has_resync_accounts_source(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW
    ));

    let read = Mutex::new(Rediscovery::default());
    let due = |now| resync_due_with(&desk.home, &desk.state, &desk.roster(), now, &read);
    assert!(due(NOW).is_some_and(|why| why.contains("kimi")));
    let fetch = |_: &Env, source: &str| -> Result<String, String> {
        Err(format!("no fetch in a test: {source}"))
    };
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
    let synced = resync_with(&desk.home, &desk.state, (&desk.roster(), NOW, false), &fx);
    let after = due(NOW + 1);

    assert!(synced.synced && synced.changed, "{synced:?}");
    assert!(fs::read_to_string(&kimi).unwrap().contains("kimi-1"));
    assert_eq!(
        cli_sync_due(Cli::Kimi, &desk.home, &desk.state, &config, NOW + 1),
        Ok(Vec::new())
    );
    assert!(synced.cli_failed.is_empty(), "{synced:?}");
    assert!(!desk.roster().exists() && !desk.fleet_file().exists());
    assert_eq!(fs::read_to_string(source).unwrap(), fleet);
    assert!(
        synced.failed.is_none() && after.is_none(),
        "resync={synced:?}; read after actual pool sync={after:?}"
    );
}

#[test]
fn a_missing_claude_roster_keeps_its_unannotated_local_source_and_its_refusal() {
    for valid in [true, false] {
        let desk = Desk::new("m16");
        let source = if valid {
            "[[account]]\nlabel='claude-1'\nemail='claude-one@example.test'\nmachines=['m16']\n"
        } else {
            "[[account]\n"
        };
        fs::write(desk.fleet_file(), source).unwrap();
        assert!(!desk.roster().exists());
        assert!(!has_init_accounts_source(
            &desk.home,
            &desk.state,
            &desk.roster(),
            NOW
        ));
        let read = Mutex::new(Rediscovery::default());
        let due = |now| resync_due_with(&desk.home, &desk.state, &desk.roster(), now, &read);
        assert!(due(NOW).is_some_and(|why| why.contains("the roster is missing")));
        let fetch = |_: &Env, source: &str| -> Result<String, String> {
            Err(format!("no fetch in a test: {source}"))
        };
        let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
        let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
        let synced = resync_with(&desk.home, &desk.state, (&desk.roster(), NOW, false), &fx);
        assert!(synced.synced, "{synced:?}");
        if valid {
            assert!(synced.changed && synced.failed.is_none(), "{synced:?}");
            assert!(
                fs::read_to_string(desk.roster())
                    .unwrap()
                    .contains("claude-1")
            );
            assert_eq!(due(NOW + 1), None);
        } else {
            assert!(synced.failed.is_some(), "{synced:?}");
            assert!(!desk.roster().exists());
            assert_eq!(fs::read_to_string(desk.fleet_file()).unwrap(), source);
            assert!(due(NOW + 1).is_some());
        }
    }
}

#[test]
fn an_ai_pool_without_claude_accounts_for_this_machine_reaches_current() {
    let desk = Desk::new("m16");
    let config = desk.config.join("aterm.toml");
    let codex = accounts::roster_beside_for(&config, Cli::Codex).unwrap();
    let secrets = desk.home.join(".secrets");
    aterm_types::fs_restricted::ensure_private_dir(&secrets).unwrap();
    fs::write(
        secrets.join(super::super::ai_pool::AI_POOL_FILE),
        "version = 1\n\n[[account]]\nprovider = \"codex\"\nemail = \"codex-one@example.test\"\nwarm = [\"m16\"]\n",
    )
    .unwrap();
    assert!(has_resync_accounts_source(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW
    ));
    assert_eq!(
        cli_pools_due(&config, &desk.state, &desk.home, NOW),
        vec![Cli::Codex]
    );

    let read = Mutex::new(Rediscovery::default());
    let due = |now| resync_due_with(&desk.home, &desk.state, &desk.roster(), now, &read);
    assert!(
        due(NOW)
            .as_deref()
            .is_some_and(|why| why.contains("codex's pool has accounts to add (codex-m16)")),
        "before sync: {:?}",
        due(NOW)
    );
    let fetch = |_: &Env, source: &str| -> Result<String, String> {
        Err(format!("no fetch in a test: {source}"))
    };
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
    let synced = resync_with(&desk.home, &desk.state, (&desk.roster(), NOW, false), &fx);
    assert!(
        synced.synced && synced.changed && synced.failed.is_none() && synced.cli_failed.is_empty(),
        "{synced:?}"
    );
    assert!(!desk.roster().exists(), "no Claude accounts for m16");
    assert!(fs::read_to_string(&codex).unwrap().contains("codex-m16"));
    assert_eq!(
        due(NOW + 1),
        None,
        "empty Claude pool is settled and Codex pool is synced"
    );
    let quiet = resync_with(
        &desk.home,
        &desk.state,
        (&desk.roster(), NOW + 60, false),
        &fx,
    );
    assert!(!quiet.changed && quiet.failed.is_none(), "{quiet:?}");
}
