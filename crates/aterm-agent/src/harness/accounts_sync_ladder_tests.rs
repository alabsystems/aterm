// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The automatic sign-in ladder's runs, end to end through the upkeep with `claude auth login`
//! scripted ([`UpkeepEffects::run_attempt`]): what an unattended run is handed, what it leaves on
//! record and in the log, how its backoff holds over the vault's own bookkeeping and ends when a
//! file a route reads arrives, which rows no session needs it leaves alone, and what a person's
//! off switch stops. Every fixture email is `example.test`.

use std::sync::{Arc, Mutex};

use super::login_tests::{Desk, NOW};
use super::*;
use crate::harness::cli::{RenewBrowser, Renewal};
use crate::harness::sign_in_ladder::Vault;

fn found(_: &Path) -> bool {
    true
}

fn none_found(_: &Path) -> bool {
    false
}

fn not_a_member(_: &Path) -> Vault {
    Vault::NotMember
}

/// A spawn seam that runs its work at once, on the caller's thread.
fn inline(work: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    work();
    Ok(())
}

/// The upkeep's effects with every sign-in run inline, each run of `claude auth login` ending
/// as `ends` and each browser it is handed kept, and every line the ladder logs kept.
struct Bench {
    cooldown: Mutex<Option<Instant>>,
    rediscovery: Mutex<Rediscovery>,
    guard: Arc<Mutex<AutoRenewGuard>>,
    browser: bool,
    logged: Arc<Mutex<Vec<String>>>,
    handed: Arc<Mutex<Vec<RenewBrowser>>>,
    ends: Renewal,
}

impl Bench {
    fn ending(ends: Renewal) -> Self {
        Self {
            cooldown: Mutex::new(None),
            rediscovery: Mutex::new(Rediscovery::default()),
            guard: Arc::new(Mutex::new(AutoRenewGuard::default())),
            browser: false,
            logged: Arc::default(),
            handed: Arc::default(),
            ends,
        }
    }

    fn fx(&self) -> UpkeepEffects<'_> {
        let logged = Arc::clone(&self.logged);
        let handed = Arc::clone(&self.handed);
        let ends = self.ends;
        let find_browser: &(dyn Fn(&Path) -> bool + Sync) =
            if self.browser { &found } else { &none_found };
        UpkeepEffects {
            fetch: &|_: &Env, source: &str| Err(format!("no fetch in a test: {source}")),
            spawn_renewal: &inline,
            sync_cooldown: &self.cooldown,
            rediscovery: &self.rediscovery,
            renew_guard: &self.guard,
            find_browser,
            vault_sync: not_a_member,
            spawn_vault: &inline,
            mailbox: &ladder_mailbox_files,
            log: Arc::new(move |line: &str| logged.lock().unwrap().push(line.to_owned())),
            run_attempt: Some(Arc::new(move |browser: &RenewBrowser, _headless: bool| {
                handed.lock().unwrap().push(browser.clone());
                ends
            })),
        }
    }

    /// One upkeep's sign-in pass at `now`: whether a sign-in started.
    fn pass(&self, desk: &Desk, now: i64) -> bool {
        auto_renew_next_account(&desk.home, &desk.state, &desk.roster(), now, &self.fx())
    }

    fn handed(&self) -> Vec<RenewBrowser> {
        self.handed.lock().unwrap().clone()
    }

    fn logged(&self) -> Vec<String> {
        self.logged.lock().unwrap().clone()
    }

    /// Ends the 15-minute cooldown a started sign-in sets ([`AutoRenewPermit`]), so a pass
    /// that starts nothing is the ladder's own backoff holding.
    fn cool(&self) {
        self.guard.lock().unwrap().last_attempt.clear();
    }
}

/// A roster written by hand: the default login `main`, and `w3`, a row with an email and an
/// empty login directory, its desk browser profile named where `desk_browser` asks.
fn hand_roster(desk: &Desk, desk_browser: bool) {
    let store = desk.store();
    let w3_dir = store.logins_dir().join("w3");
    fs::create_dir_all(&w3_dir).unwrap();
    desk.default_login("main.m30@example.test");
    let rows = [
        accounts::Row {
            label: "main".to_owned(),
            class: Class::Dedicated,
            email: Some("main.m30@example.test".to_owned()),
            ..accounts::Row::default()
        },
        accounts::Row {
            label: "w3".to_owned(),
            class: Class::Dedicated,
            dir: Some(w3_dir.to_string_lossy().into_owned()),
            email: Some("w3.m30.3@example.test".to_owned()),
            browser: desk_browser.then(|| "Profile 1".to_owned()),
            ..accounts::Row::default()
        },
    ];
    let text: String = rows
        .iter()
        .map(|row| accounts::row_text(row).unwrap() + "\n")
        .collect();
    fs::write(desk.roster(), text).unwrap();
}

fn attempt_of(desk: &Desk, label: &str) -> Option<accounts::SignInAttempt> {
    desk.store()
        .load_logins(NOW)
        .unwrap()
        .0
        .get(label)
        .and_then(|b| b.sign_in_attempt.clone())
}

/// A row with a stored headless profile and a desk browser profile: the ladder drives the
/// profile headless, and when that run makes no login, nothing that opens a window (the desk
/// profile, the default browser) is tried in its place. The run that failed is on record with
/// its route, how it ended and every route read, is one log line, and sets the row's backoff:
/// the next pass, past the sign-in cooldown, starts nothing and logs nothing.
#[test]
fn an_unattended_run_is_never_handed_a_window_and_its_failure_is_on_record_and_held() {
    let desk = Desk::new("m30");
    hand_roster(&desk, true);
    let store = desk.store();
    fs::create_dir_all(crate::harness::headless_oauth::browser_profile_dir(
        &store.dir,
        Some("w3"),
    ))
    .unwrap();
    let bench = Bench {
        browser: true,
        ..Bench::ending(Renewal::AuthFailed)
    };
    assert!(bench.pass(&desk, NOW), "the profile route started");
    let handed = bench.handed();
    assert_eq!(handed.len(), 1, "{handed:?}");
    assert!(
        handed
            .iter()
            .all(|b| matches!(b, RenewBrowser::Remote { .. })),
        "never a window: {handed:?}"
    );
    let attempt = attempt_of(&desk, "w3").expect("the run is on record");
    assert_eq!(attempt.outcome, accounts::AttemptOutcome::Failed);
    for part in [
        "profile:",
        "no login made",
        "desk-browser(",
        "headless-browser(",
    ] {
        assert!(attempt.routes.contains(part), "{part}: {}", attempt.routes);
    }
    let lines = bench.logged();
    let failed: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("w3") && l.contains("failed"))
        .collect();
    assert_eq!(failed.len(), 1, "{lines:?}");
    assert!(failed[0].contains("profile:"), "{}", failed[0]);

    bench.cool();
    assert!(!bench.pass(&desk, NOW + 60), "the backoff holds");
    assert_eq!(bench.handed().len(), 1);
    assert_eq!(bench.logged(), lines, "nothing logged while held");
}

/// On a vault member whose subscriber rewrites `state.toml` to say when it last heard the vault,
/// a row whose mailbox route ran and failed stays in its backoff: the rewrite delivered nothing.
/// A file the vault delivers to `~/.secrets` ends the backoff at once.
#[test]
fn the_vaults_own_bookkeeping_never_ends_a_backoff_and_a_delivery_does() {
    let desk = Desk::new("m30");
    hand_roster(&desk, false);
    let store = desk.store();
    fs::create_dir_all(
        store
            .dir
            .join(crate::harness::open_url::DEFAULT_MAILBOX_DIR),
    )
    .unwrap();
    let vault = desk.home.join(".config/aterm/secrets");
    fs::create_dir_all(&vault).unwrap();
    fs::write(vault.join("source.toml"), "# a vault member\n").unwrap();
    let heard = |at: i64| {
        let state = vault.join("state.toml");
        fs::write(
            &state,
            format!("membership = \"member\"\nseq = 51\nheard = {at}\n"),
        )
        .unwrap();
        let later =
            std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(at).unwrap_or(0));
        fs::File::options()
            .write(true)
            .open(&state)
            .unwrap()
            .set_modified(later)
            .unwrap();
    };
    heard(NOW);
    let bench = Bench::ending(Renewal::AuthFailed);
    assert!(bench.pass(&desk, NOW), "the mailbox route started");
    assert_eq!(bench.handed().len(), 1);
    assert_eq!(
        attempt_of(&desk, "w3").map(|a| a.outcome),
        Some(accounts::AttemptOutcome::Failed)
    );

    heard(NOW + 60);
    bench.cool();
    assert!(
        !bench.pass(&desk, NOW + 60),
        "a rewrite of when it heard is no delivery"
    );
    assert_eq!(bench.handed().len(), 1);

    let secrets = desk.home.join(".secrets");
    fs::create_dir_all(&secrets).unwrap();
    fs::write(
        secrets.join(crate::harness::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
        "http_url = \"http://127.0.0.1:9/inbox\"\n",
    )
    .unwrap();
    assert!(
        bench.pass(&desk, NOW + 120),
        "a delivered file ends the backoff and the sign-in cooldown"
    );
    assert_eq!(bench.handed().len(), 2);
}

/// A fleet discovery made lists every address this Mac's files show, a mailbox password's
/// among them; a row no session needs yet is signed in only where Claude Code's own files show
/// its account, never for an address only a credential names. CONTROL: the row a Claude Code
/// backup shows is signed in.
#[test]
fn a_row_only_a_credential_names_is_not_signed_in_before_a_session_needs_it() {
    let desk = Desk::new("m27");
    desk.default_login("sol-m27@example.test");
    fs::write(
        desk.home.join(".claude.json.backup"),
        "{\"oauthAccount\":{\"emailAddress\":\"sun.m27.3@example.test\"}}\n",
    )
    .unwrap();
    let secrets = desk.home.join(".secrets");
    aterm_types::fs_restricted::ensure_private_dir(&secrets).unwrap();
    fs::write(
        secrets.join(crate::harness::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
        "[passwords]\n\"codex.m27.2@example.test\" = \"pw\"\n",
    )
    .unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).expect("discovered");
    assert_eq!(
        fleet_file_opt_in(&desk.fleet_file()),
        Some(OptIn::Discovered)
    );
    let roster = desk.rows();
    let label_of = |email: &str| {
        roster
            .rows
            .iter()
            .find(|r| r.email.as_deref() == Some(email))
            .map(|r| r.label.clone())
            .unwrap_or_else(|| panic!("{email}: {roster:?}"))
    };
    let (codex, sun) = (
        label_of("codex.m27.2@example.test"),
        label_of("sun.m27.3@example.test"),
    );
    let store = desk.store();
    fs::create_dir_all(
        store
            .dir
            .join(crate::harness::open_url::DEFAULT_MAILBOX_DIR),
    )
    .unwrap();
    let bench = Bench::ending(Renewal::AuthFailed);
    assert!(bench.pass(&desk, NOW + 10));
    assert_eq!(bench.handed().len(), 1);
    assert!(attempt_of(&desk, &sun).is_some(), "CONTROL");
    bench.cool();
    let _ = bench.pass(&desk, NOW + 20);
    assert_eq!(attempt_of(&desk, &codex), None, "never tried");
    assert_eq!(bench.handed().len(), 1);
}

/// `[harness] logins = "off"` stops the ladder: a window's upkeep starts no sign-in and records
/// none. CONTROL: with the switch as it ships, the same upkeep starts one.
#[test]
fn a_person_turning_logins_off_stops_the_ladder() {
    for logins in ["off", "renew"] {
        let desk = Desk::new("m30");
        hand_roster(&desk, false);
        fs::write(
            desk.config.join("aterm.toml"),
            format!("[fleet]\nmachine = \"m30\"\n\n[harness]\nlogins = \"{logins}\"\n"),
        )
        .unwrap();
        let store = desk.store();
        fs::create_dir_all(
            store
                .dir
                .join(crate::harness::open_url::DEFAULT_MAILBOX_DIR),
        )
        .unwrap();
        let bench = Bench::ending(Renewal::AuthFailed);
        let upkept = upkeep(
            &desk.home,
            &desk.state,
            (&desk.roster(), NOW, UpkeepStep::Both),
            &bench.fx(),
        );
        let started = logins == "renew";
        assert_eq!(
            upkept,
            Upkept::Unconfigured {
                fleet_file_ignored: false,
                renewal: started,
            },
            "{logins}"
        );
        assert_eq!(bench.handed().len(), usize::from(started), "{logins}");
        assert_eq!(attempt_of(&desk, "w3").is_some(), started, "{logins}");
    }
}
