// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The accounts automation's hardening (2026-10-29): a renewal reads the machine (its
//! environment, the browsers outside its home) only through its guard, so a test's guard reads
//! its scratch home alone; a sign-in's own marks in the mail folder never end its cooldown; a copy
//! aterm wrote to `~/.secrets` that a person's later sync superseded never puts back the accounts
//! that sync dropped; and an Account-menu Auto-rotate obeys the config's switches as the window's
//! upkeep does. Every fixture email is `example.test`.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use super::login_tests::{Desk, NOW, upkeep_effects};
use super::*;

fn set_mtime(path: &Path, at: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(at)
        .unwrap();
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

/// One Claude Code row, `work`, with an email and a login directory of its own, beside the
/// config `desk` wrote, signed out in a session that runs on it: a row a click's rotation signs
/// in first.
fn signed_out_row(desk: &Desk) -> Roster {
    let dir = desk.state.join("logins").join("work");
    let roster = Roster {
        rows: vec![accounts::Row {
            label: "work".into(),
            class: Class::Dedicated,
            dir: Some(dir.to_str().unwrap().into()),
            email: Some("work@example.test".into()),
            ..accounts::Row::default()
        }],
    };
    fs::write(
        desk.roster(),
        accounts::append_rows_for(Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
    accounts::Store::at(&desk.state)
        .transact(NOW, &mut None, |state| {
            state.sessions.insert(
                "sid-work".into(),
                accounts::SessionRow {
                    account: "work".into(),
                    at: NOW,
                    held: None,
                },
            );
            state.accounts.insert(
                "work".into(),
                accounts::Account {
                    auth: accounts::Auth::SignedOut,
                    ..accounts::Account::default()
                },
            );
            Ok(Vec::new())
        })
        .unwrap();
    roster
}

/// The scratch mail folder [`mailbox_named_by_env`] names as `$ATERM_OPEN_URL_MAILBOX`.
static MAILBOX_IN_ENV: OnceLock<PathBuf> = OnceLock::new();

/// An environment whose only variable is the mailbox [`MAILBOX_IN_ENV`] holds.
fn mailbox_named_by_env(name: &str) -> Option<OsString> {
    (name == super::super::open_url::MAILBOX_ENV)
        .then(|| MAILBOX_IN_ENV.get().map(|dir| dir.clone().into_os_string()))
        .flatten()
}

/// A renewal reads the machine through its guard alone: a guard that reads nothing (every
/// guard but the window's) admits no person's renewal on a mailbox named only in an environment
/// and finds no browser outside the home, wherever this process runs. CONTROL: a guard whose
/// environment names a mailbox admits the renewal on it, and a browser in the home's cache is
/// found.
#[test]
fn a_renewal_reads_the_machine_only_through_its_guard() {
    let desk = Desk::new("m27");
    let roster = signed_out_row(&desk);
    let maildir = desk.home.join("mail");
    fs::create_dir_all(maildir.join("new")).unwrap();
    MAILBOX_IN_ENV.set(maildir).unwrap();
    let spawned = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&spawned);
    let spawn = move |_work: Box<dyn FnOnce() + Send>| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    let renew = |guard: &Arc<Mutex<AutoRenewGuard>>| {
        spawn_row_renewal(
            Cli::Claude,
            &desk.home,
            &desk.state,
            &desk.roster(),
            roster.clone(),
            &accounts::Logins::default(),
            roster.rows[0].clone(),
            (None, false),
            NOW,
            guard,
            &spawn,
            None,
        )
    };
    let inert = Arc::new(Mutex::new(AutoRenewGuard::default()));
    assert_eq!(
        renew(&inert),
        RenewalDispatch::Failed,
        "no mailbox, profile or browser in the scratch home, whatever this process's env holds"
    );
    assert_eq!(spawned.load(Ordering::SeqCst), 0);
    let named = Arc::new(Mutex::new(AutoRenewGuard {
        reads: MachineReads {
            var: mailbox_named_by_env,
            system_browsers: false,
        },
        ..AutoRenewGuard::default()
    }));
    assert_eq!(
        renew(&named),
        RenewalDispatch::Started,
        "CONTROL: the guard's environment names a mailbox"
    );
    assert_eq!(spawned.load(Ordering::SeqCst), 1);

    assert!(!MachineReads::default().has_browser(&desk.home));
    let cached = desk
        .home
        .join(".cache/aterm/browser.noindex/chrome-headless-shell-fixture/chrome-headless-shell");
    fs::create_dir_all(cached.parent().unwrap()).unwrap();
    fs::write(&cached, b"inert browser fixture").unwrap();
    assert!(
        MachineReads::default().has_browser(&desk.home),
        "CONTROL: a browser in the home's cache"
    );
}

/// The time that ends a sign-in's cooldown is its mail's, never its mail folder's: the mark a
/// sign-in leaves beside a message it used (`.clicked`) moves the folder's time and not this
/// one. CONTROL: a new message moves it.
#[test]
fn a_sign_ins_own_marks_never_end_its_cooldown() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let store = accounts::Store::at(&tmp.path().join("state"));
    let mailbox = store.dir.join(super::super::open_url::DEFAULT_MAILBOX_DIR);
    fs::create_dir_all(mailbox.join("new")).unwrap();
    fs::create_dir_all(mailbox.join("cur")).unwrap();
    let message = mailbox.join("new").join("1.eml");
    fs::write(&message, "To: a@example.test\r\n\r\nhello\r\n").unwrap();
    let delivered = SystemTime::now() - Duration::from_secs(3600);
    set_mtime(&message, delivered);
    assert_eq!(latest_mailbox_mtime(Some(&home), &store), Some(delivered));
    fs::write(mailbox.join("new").join("1.eml.clicked"), "").unwrap();
    fs::write(mailbox.join("cur").join("2.eml.seen"), "").unwrap();
    assert_eq!(
        latest_mailbox_mtime(Some(&home), &store),
        Some(delivered),
        "a sign-in's marks are not mail"
    );
    let next = mailbox.join("new").join("2.eml");
    fs::write(&next, "To: a@example.test\r\n\r\nagain\r\n").unwrap();
    let later = delivered + Duration::from_secs(60);
    set_mtime(&next, later);
    assert_eq!(
        latest_mailbox_mtime(Some(&home), &store),
        Some(later),
        "CONTROL: new mail"
    );
}

/// A message dated after this read cannot clear a renewal's existing retry hold. Its
/// timestamp becomes usable at the inclusive read boundary, without a timer or another write.
#[test]
fn future_mail_does_not_clear_an_account_retry_hold() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let store = accounts::Store::at(&tmp.path().join("state"));
    let mailbox = store.dir.join(super::super::open_url::DEFAULT_MAILBOX_DIR);
    fs::create_dir_all(mailbox.join("new")).unwrap();
    let at = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let future = at + Duration::from_secs(3600);
    let message = mailbox.join("new").join("future.eml");
    fs::write(&message, "To: work@example.test\r\n\r\nhello\r\n").unwrap();
    set_mtime(&message, future);

    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let clock = Instant::now();
    drop(AutoRenewPermit::claim(&guard, "work", clock, false).expect("first attempt"));
    let observed = latest_mailbox_mtime_at(&|_| None, None, &store, at);
    assert!(
        AutoRenewPermit::claim_with_mailbox(&guard, "work", clock, false, observed).is_none(),
        "a future message is not evidence of changed input: {observed:?}"
    );
    assert_eq!(observed, None);
    assert_eq!(
        latest_mailbox_mtime_at(&|_| None, None, &store, future),
        Some(future),
        "CONTROL: the same timestamp is admitted when its time arrives"
    );
    assert!(
        AutoRenewPermit::claim(&guard, "work", clock, true).is_some(),
        "an explicit choice still bypasses the automatic hold"
    );
}

/// A future maximum left by an earlier observation cannot hide mail that has arrived since
/// that attempt. A new valid message clears the hold once; the unchanged read then holds.
#[test]
fn future_mail_does_not_hide_a_valid_new_input_from_the_retry_guard() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let store = accounts::Store::at(&tmp.path().join("state"));
    let mailbox = store.dir.join(super::super::open_url::DEFAULT_MAILBOX_DIR);
    fs::create_dir_all(mailbox.join("new")).unwrap();
    let at = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let future = at + Duration::from_secs(3600);
    let message = mailbox.join("new").join("future.eml");
    fs::write(&message, "To: work@example.test\r\n\r\nfuture\r\n").unwrap();
    set_mtime(&message, future);

    // Exercise recovery of the old reader's raw maximum, through the actual permit.
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let clock = Instant::now();
    drop(
        AutoRenewPermit::claim_with_mailbox(&guard, "work", clock, false, Some(future))
            .expect("the earlier attempt's observation"),
    );
    let arrived = at - Duration::from_secs(10);
    let valid = mailbox.join("new").join("arrived.eml");
    fs::write(&valid, "To: work@example.test\r\n\r\narrived\r\n").unwrap();
    set_mtime(&valid, arrived);
    let observed = latest_mailbox_mtime_at(&|_| None, None, &store, at);
    let renewed = AutoRenewPermit::claim_with_mailbox(&guard, "work", clock, false, observed);
    assert!(
        renewed.is_some(),
        "the future maximum must not hide the valid new input: {observed:?}"
    );
    assert_eq!(observed, Some(arrived));
    drop(renewed);
    assert!(
        AutoRenewPermit::claim_with_mailbox(
            &guard,
            "work",
            clock,
            false,
            latest_mailbox_mtime_at(&|_| None, None, &store, at),
        )
        .is_none(),
        "CONTROL: the unchanged valid message respects the retry hold"
    );
    assert_eq!(fs::metadata(&message).unwrap().modified().unwrap(), future);
}

/// What an earlier sync left in `~/.secrets`: aterm's own copy of a list found on this Mac,
/// written an hour ago, naming `old@` beside `a1@`.
fn stale_copy(desk: &Desk) -> PathBuf {
    let secrets = desk.home.join(".secrets");
    aterm_types::fs_restricted::ensure_private_dir(&secrets).unwrap();
    let copy = secrets.join(FLEET_ROSTER_FILE);
    let found = FleetRoster::from_toml(
        "[[account]]\nlabel = \"m27\"\nmachines = [\"m27\"]\nemail = \"a1@example.test\"\n\n\
         [[account]]\nlabel = \"m27-2\"\nmachines = [\"m27\"]\nemail = \"old@example.test\"\n",
        "fixture",
    )
    .unwrap();
    fs::write(&copy, found.to_toml_for(Some(OptIn::Discovered))).unwrap();
    set_mtime(&copy, SystemTime::now() - Duration::from_secs(3600));
    copy
}

/// A person's sync of a named list, after `stale_copy`.
fn named_sync(desk: &Desk) {
    let named = desk.config.join("named.csv");
    fs::write(
        &named,
        "email,computer,claude\na1@example.test,m27,1\nnew@example.test,m27.2,1\n",
    )
    .unwrap();
    let env = quiet_env(
        &desk.home,
        &desk.state,
        &desk.config.join("aterm.toml"),
        NOW,
    );
    let how = SyncHow {
        fetch: &fetch_source_text,
        ladder: &sync_lock_ladder(),
        fleet_snapshots: &[],
        heal: None,
        allow_empty_pool: false,
    };
    sync_accounts_with(&env, Some(named.to_str().unwrap()), None, &how).expect("named sync");
}

/// A copy aterm wrote to `~/.secrets` before a person synced a named list is superseded by it:
/// no later sync reads the copy, so the account the named list dropped stays dropped, and the
/// copy is left as it was. CONTROL: an edit that makes the copy the newer file makes it a source
/// again, and a copy the vault delivered keeps its precedence however old it is.
#[test]
fn a_named_sync_supersedes_the_copy_aterm_wrote_before_it() {
    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    let copy = stale_copy(&desk);
    let kept = fs::read(&copy).unwrap();
    named_sync(&desk);
    let named = ["a1@example.test", "new@example.test"];
    assert_eq!(emails(&desk), named);
    assert!(
        !needs_init_accounts_sync(&desk.home, &desk.state, &desk.roster(), NOW + 1),
        "nothing newer than the named list"
    );
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW + 2).expect("sync");
    assert_eq!(emails(&desk), named, "`old@` stays dropped");
    assert_eq!(fs::read(&copy).unwrap(), kept, "the copy is left as it was");
    let config = desk.config.join("aterm.toml");
    assert_eq!(
        fleet_origin(Some(&desk.home), Some(&config), &desk.roster()),
        FleetOrigin::Local {
            files: vec![desk.fleet_file()],
            opt_in: Some(OptIn::Named),
        }
    );

    set_mtime(&copy, SystemTime::now() + Duration::from_secs(60));
    assert!(
        needs_init_accounts_sync(&desk.home, &desk.state, &desk.roster(), NOW + 3),
        "CONTROL: the newer copy is read again"
    );

    let desk = Desk::new("m27");
    desk.default_login("a1@example.test");
    let copy = stale_copy(&desk);
    let dir = desk.home.join(".config/aterm/secrets");
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let state = dir.join("state.toml");
    fs::write(
        &state,
        format!(
            "membership = \"in\"\nseq = 1\n\n[manifest]\n\"{FLEET_ROSTER_FILE}\" = \"{}\"\n",
            aterm_secrets::materialize::digest_hex(&fs::read(&copy).unwrap())
        ),
    )
    .unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
    named_sync(&desk);
    assert!(
        needs_init_accounts_sync(&desk.home, &desk.state, &desk.roster(), NOW + 1),
        "CONTROL: the vault's fleet keeps its precedence"
    );
}

/// An Account-menu Auto-rotate obeys the config beside the roster as the window's upkeep does:
/// `[harness] accounts = "off"`, `logins = "off"` and `enabled = false` each start no sign-in of
/// its own, and under `accounts = "off"` a saved choice waits too. CONTROL: with none of them,
/// the click signs the session's signed-out row in, and under `logins = "off"` a person's saved
/// choice still completes.
#[test]
fn an_auto_rotate_click_obeys_the_configs_switches() {
    for (switch, saved, starts) in [
        ("", false, true),
        ("[harness]\naccounts = \"off\"\n", false, false),
        ("[harness]\nlogins = \"off\"\n", false, false),
        ("[harness]\nenabled = false\n", false, false),
        ("[harness]\nlogins = \"off\"\n", true, true),
        ("[harness]\naccounts = \"off\"\n", true, false),
    ] {
        let desk = Desk::new("m27");
        fs::write(
            desk.config.join("aterm.toml"),
            format!("[fleet]\nmachine = \"m27\"\n{switch}"),
        )
        .unwrap();
        let roster = signed_out_row(&desk);
        if saved {
            let chosen = accounts::Store::at(&desk.state)
                .select_account_for(Cli::Claude, &roster, &desk.roster(), "work", NOW)
                .unwrap();
            assert!(matches!(chosen, accounts::PinSelection::Renew(_)));
        }
        let spawned = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&spawned);
        let spawn = move |_work: Box<dyn FnOnce() + Send>| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let fetch = |_: &Env, _: &str| -> Result<String, String> { panic!("no fetch") };
        let cooldown = Mutex::new(None);
        let rediscovery = Mutex::new(Rediscovery::default());
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
        assert_eq!(
            auto_renew_next_account_interactive_with(
                Cli::Claude,
                &desk.home,
                &desk.state,
                &desk.roster(),
                NOW,
                &fx
            ),
            starts,
            "{switch:?} saved={saved}"
        );
        assert_eq!(
            spawned.load(Ordering::SeqCst),
            usize::from(starts),
            "{switch:?} saved={saved}"
        );
    }
}
