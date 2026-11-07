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
            scope: None,
            fetch: &|_: &Env, source: &str| Err(format!("no fetch in a test: {source}")),
            spawn_renewal: &inline,
            sync_cooldown: &self.cooldown,
            rediscovery: &self.rediscovery,
            renew_guard: &self.guard,
            find_browser,
            vault_sync: |home, _scope| not_a_member(home),
            spawn_vault: &inline,
            default_check: |_: &Path, _scope| {
                crate::harness::cli::LoginCheck::CouldNotRun(String::new())
            },
            mailbox: &ladder_mailbox_files,
            log: Arc::new(move |line: &str| logged.lock().unwrap().push(line.to_owned())),
            save_oauth: |_, _| Ok("inert".into()),
            shared: shared_renewal::Effects::default(),
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

/// A session here runs on `label`, and its login is signed out here: the upkeep's door for a
/// sign-in (`upkeep_sign_in`, `DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 4).
fn runs_signed_out(desk: &Desk, label: &str, at: i64) {
    desk.store()
        .transact(at, &mut None, |state| {
            let mut events = accounts::note_session(state, &format!("s-{label}"), label, None, at);
            events.extend(accounts::note_auth(
                state,
                label,
                accounts::Auth::SignedOut,
                at,
            ));
            Ok(events)
        })
        .unwrap();
}

/// A roster written by hand: the default login `main`, and `w3`, a row with an email and an
/// empty login directory that a session here runs on, signed out ([`runs_signed_out`]), its desk
/// browser profile named where `desk_browser` asks.
fn hand_roster(desk: &Desk, desk_browser: bool) {
    let store = desk.store();
    let w3_dir = store.logins_dir().join("w3");
    fs::create_dir_all(&w3_dir).unwrap();
    desk.default_login("main.m30@example.test");
    let rows = [
        accounts::Row {
            label: "main".to_owned(),
            class: Class::Preferred,
            email: Some("main.m30@example.test".to_owned()),
            ..accounts::Row::default()
        },
        accounts::Row {
            label: "w3".to_owned(),
            class: Class::Preferred,
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
    runs_signed_out(desk, "w3", NOW - 60);
}

fn attempt_of(desk: &Desk, label: &str) -> Option<accounts::SignInAttempt> {
    desk.store()
        .load_logins(NOW)
        .unwrap()
        .0
        .get(label)
        .and_then(|b| b.sign_in_attempt.clone())
}

/// A PAGE'S STOP HOLDS ITS ROW FOR SIX HOURS, counted from its last try and on record: past a
/// restart, which forgets the ladder's own wait, a changed secret, which ends that wait as a new
/// input, and a move, which is heard at every launch. CONTROL: a person's signed-in browser window
/// ends the hold, and six hours do, in a process that holds its own wait too.
#[cfg(unix)]
#[test]
fn a_page_stop_holds_its_row_until_six_hours_or_a_persons_window() {
    let desk = Desk::new("m30");
    hand_roster(&desk, false);
    let store = desk.store();
    fs::create_dir_all(crate::harness::headless_oauth::browser_profile_dir(
        &store.dir,
        Some("w3"),
    ))
    .unwrap();
    let aterm = || Bench {
        browser: true,
        ..Bench::ending(Renewal::Failed)
    };
    let stop = |at: i64| {
        store
            .transact_pool_logins_within(&[], &desk.roster(), at, &mut None, |_, _, logins| {
                accounts::admit_workflow(logins, "w3")?;
                Ok(accounts::note_sign_in_attempt(
                    logins,
                    "w3",
                    accounts::AttemptOutcome::Blocked,
                    "profile: it met a bot check",
                    at,
                ))
            })
            .unwrap();
    };
    let window = |at: i64| {
        store
            .transact_pool_logins_within(&[], &desk.roster(), at, &mut None, |_, _, logins| {
                accounts::admit_workflow(logins, "w3")?;
                Ok(vec![
                    accounts::Event::new(at, accounts::Kind::SignIn, "w3")
                        .because("claude.ai window done session-kept"),
                ])
            })
            .unwrap();
    };
    // The process that met the page. Its own wait stands under the row's real inputs, since `held`
    // drops one under any others. It counts uptime, so a sleeping Mac's outlasts the record's, and
    // it hears of no other process's window.
    let met_page = |at: i64| {
        let bench = aterm();
        assert!(bench.pass(&desk, at), "the try");
        stop(at);
        bench
            .guard
            .lock()
            .unwrap()
            .ladder
            .page_stopped("w3", Instant::now() + 2 * PERSON_BACKOFF);
        bench.cool();
        bench
    };
    let hold = PERSON_BACKOFF_SECS;

    stop(NOW);
    let restarted = aterm();
    assert!(!restarted.pass(&desk, NOW + 60), "a restart");
    let secrets = desk.home.join(".secrets");
    fs::create_dir_all(&secrets).unwrap();
    fs::write(secrets.join("synced-from-the-vault"), "new").unwrap();
    assert!(!restarted.pass(&desk, NOW + 120), "a changed secret");
    restarted.guard.lock().unwrap().ladder.moved(Instant::now());
    assert!(!restarted.pass(&desk, NOW + 180), "a move");
    assert!(!aterm().pass(&desk, NOW + 240), "another restart");
    assert!(restarted.handed().is_empty());

    let last_try = NOW + hold - 60;
    stop(last_try);
    let (logins, _) = store.load_logins(last_try).unwrap();
    let same = logins.get("w3").unwrap().sign_in_attempt.clone().unwrap();
    assert_eq!(
        (same.since, same.tried_at),
        (NOW, last_try),
        "an unchanged stop keeps when it was first recorded, and is saved with its last try"
    );
    assert!(!aterm().pass(&desk, NOW + hold), "counts from the last try");

    let met_at = last_try + hold;
    let live = met_page(met_at);
    assert!(!live.pass(&desk, met_at + 60), "both hold");
    assert!(
        live.pass(&desk, met_at + hold),
        "CONTROL: six hours end it, and its process's own wait with it"
    );

    let met_at = met_at + 2 * hold;
    let live = met_page(met_at);
    assert!(!live.pass(&desk, met_at + 1), "both hold");
    window(met_at + 2);
    assert!(
        live.pass(&desk, met_at + 3),
        "CONTROL: a person's signed-in window ends it, and its process's own wait with it"
    );
}

/// A wait a page did not set stands whatever the record says: a failed sign-in waits on its own
/// backoff, as a `Blocked` record's end says nothing of it.
#[cfg(unix)]
#[test]
fn a_failed_sign_in_keeps_its_own_short_wait() {
    let desk = Desk::new("m30");
    hand_roster(&desk, false);
    let store = desk.store();
    fs::create_dir_all(crate::harness::headless_oauth::browser_profile_dir(
        &store.dir,
        Some("w3"),
    ))
    .unwrap();
    let bench = Bench {
        browser: true,
        ..Bench::ending(Renewal::Failed)
    };
    assert!(bench.pass(&desk, NOW), "the first try");
    assert_eq!(
        attempt_of(&desk, "w3").unwrap().outcome,
        accounts::AttemptOutcome::Failed
    );
    bench.cool();
    assert!(!bench.pass(&desk, NOW + 60), "its own wait");
    bench.guard.lock().unwrap().ladder.moved(Instant::now());
    bench.cool();
    assert!(
        bench.pass(&desk, NOW + 120),
        "CONTROL: a move ends that one"
    );
}

#[cfg(unix)]
#[test]
fn a_repaired_profile_reopens_the_ladder_inside_its_wrong_account_wait_and_backoff() {
    let desk = Desk::new("m30");
    hand_roster(&desk, false);
    let store = desk.store();
    fs::create_dir_all(crate::harness::headless_oauth::browser_profile_dir(
        &store.dir,
        Some("w3"),
    ))
    .unwrap();
    store
        .transact_pool_logins_within(&[], &desk.roster(), NOW, &mut None, |_, state, logins| {
            let mut events = accounts::note_auth(state, "w3", accounts::Auth::SignedOut, NOW);
            events.extend(accounts::note_wrong_account(logins, "w3", NOW));
            Ok(events)
        })
        .unwrap();
    assert!(latest_mailbox_mtime(Some(&desk.home), &store).is_none());
    let bench = Bench {
        browser: true,
        ..Bench::ending(Renewal::Failed)
    };
    assert!(!bench.pass(&desk, NOW));
    assert!(bench.handed().is_empty());
    assert_eq!(
        attempt_of(&desk, "w3").unwrap().outcome,
        accounts::AttemptOutcome::NoRoute
    );
    {
        let mut guard = bench.guard.lock().unwrap();
        let clock = Instant::now();
        assert!(guard.ladder.retry_at("w3", clock).is_some());
        guard.last_attempt.insert("w3".into(), clock);
        guard.parked.insert("w3".into(), clock);
        guard.bot_checked.insert("w3".into(), clock);
        let owner = guard.ladder_owners["w3"].clone();
        guard.activity_owners.insert("w3".into(), owner);
    }
    store
        .transact_pool_logins_within(&[], &desk.roster(), NOW + 1, &mut None, |_, _, logins| {
            accounts::admit_workflow(logins, "w3")?;
            Ok(vec![
                accounts::Event::new(NOW + 1, accounts::Kind::SignIn, "w3")
                    .because("claude.ai window done session-kept"),
            ])
        })
        .unwrap();

    assert!(
        bench.pass(&desk, NOW + 2),
        "the repaired profile starts a fresh check"
    );
    assert!(matches!(
        bench.handed().as_slice(),
        [RenewBrowser::Remote { .. }]
    ));
    let (state, _) = store.load(NOW + 2).unwrap();
    let (logins, _) = store.load_logins(NOW + 2).unwrap();
    assert_eq!(state.account("w3").auth, accounts::Auth::SignedOut);
    assert_eq!(logins.get("w3").unwrap().wrong_at, Some(NOW));
    assert_eq!(
        attempt_of(&desk, "w3").unwrap().outcome,
        accounts::AttemptOutcome::Failed
    );
    assert!(
        !bench.pass(&desk, NOW + 3),
        "the new attempt has its own backoff"
    );
    assert_eq!(bench.handed().len(), 1);
}

#[test]
fn a_repaired_profile_reopens_non_ladder_admission_inside_the_wrong_account_wait() {
    for repaired in [false, true] {
        let desk = Desk::new("m30");
        hand_roster(&desk, false);
        let store = desk.store();
        fs::create_dir_all(crate::harness::headless_oauth::browser_profile_dir(
            &store.dir,
            Some("w3"),
        ))
        .unwrap();
        let browser = desk.home.join(
            ".cache/aterm/browser.noindex/chrome-headless-shell-fixture/chrome-headless-shell",
        );
        fs::create_dir_all(browser.parent().unwrap()).unwrap();
        fs::write(browser, b"inert browser marker; no queued work is run").unwrap();
        store
            .transact_pool_logins_within(&[], &desk.roster(), NOW, &mut None, |_, state, logins| {
                let mut events = accounts::note_auth(state, "w3", accounts::Auth::SignedOut, NOW);
                events.extend(accounts::note_wrong_account(logins, "w3", NOW));
                if repaired {
                    accounts::admit_workflow(logins, "w3")?;
                    events.push(
                        accounts::Event::new(NOW + 1, accounts::Kind::SignIn, "w3")
                            .because("claude.ai window quit session-unknown"),
                    );
                }
                Ok(events)
            })
            .unwrap();
        assert!(latest_mailbox_mtime(Some(&desk.home), &store).is_none());
        let raw = Roster::load(&desk.roster()).unwrap();
        let logins = store.load_logins(NOW + 2).unwrap().0;
        let row = accounts::bound(&raw, &logins).row("w3").unwrap().clone();
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let queued = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let queued_work = Arc::clone(&queued);
        let spawn = move |work: Box<dyn FnOnce() + Send>| {
            queued_work.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            drop(work);
            Ok(())
        };
        let admitted = spawn_row_renewal(
            Cli::Claude,
            &desk.home,
            &desk.state,
            &desk.roster(),
            raw,
            &logins,
            (
                row.clone(),
                crate::harness::sign_in_ladder::asked_fixture(Cli::Claude, &row.label),
            ),
            (None, false),
            NOW + 2,
            &guard,
            &spawn,
            None,
            None,
            &mut None,
        );
        assert_eq!(admitted.progressed(), repaired);
        assert_eq!(
            queued.load(std::sync::atomic::Ordering::SeqCst),
            usize::from(repaired)
        );
        assert_eq!(store.load_logins(NOW + 2).unwrap().0, logins);
        assert_eq!(
            store.load(NOW + 2).unwrap().0.account("w3").auth,
            accounts::Auth::SignedOut
        );
    }
}

#[test]
fn a_browser_completion_at_its_tasks_timestamp_still_answers_that_task() {
    let desk = Desk::new("m30");
    let store = desk.store();
    store
        .transact_logins(NOW, &mut None, |_, logins| {
            accounts::note_sign_in_task(logins, "w3", accounts::SignInAction::Browser, NOW);
            Ok(vec![
                accounts::Event::new(NOW, accounts::Kind::SignIn, "w3")
                    .because("claude.ai window done session-unknown"),
            ])
        })
        .unwrap();
    let logins = store.load_logins(NOW).unwrap().0;
    assert_eq!(
        browser_window_signed_in_token(&store, "w3", NOW),
        Some((NOW, 1))
    );
    // This fixture has no configured roster.
    let raw = accounts::Row {
        label: "w3".into(),
        ..accounts::Row::default()
    };
    assert_eq!(unaddressed_sign_in_task_at(&store, &logins, &raw), None);
}

#[test]
fn a_damaged_ledger_suffix_cannot_leave_an_earlier_browser_completion_usable() {
    let desk = Desk::new("m30");
    let store = desk.store();
    let accepted = accounts::Event::new(NOW, accounts::Kind::SignIn, "w3")
        .because("claude.ai window done session-kept");
    store.append(std::slice::from_ref(&accepted)).unwrap();
    assert_eq!(
        browser_window_signed_in_token(&store, "w3", NOW),
        Some((NOW, 1))
    );
    let prefix = fs::read(store.ledger_path()).unwrap();
    for suffix in [
        "not an event\n".to_owned(),
        "{\"t\":".to_owned(),
        format!(
            r#"{{"t":{},"kind":"sign-in","account":"w3","why":7}}"#,
            NOW + 1
        ),
    ] {
        let mut damaged = prefix.clone();
        damaged.extend_from_slice(suffix.as_bytes());
        fs::write(store.ledger_path(), damaged).unwrap();
        let paths = [
            store.state_file(),
            store.state_witness_file(),
            store.logins_file(),
            store.logins_witness_file(),
            store.ledger_path(),
            store.wake_path(),
        ];
        let before = paths.each_ref().map(|path| fs::read(path).ok());
        assert!(store.tail(10).unwrap().contains(&accepted));
        assert_eq!(
            store.tail_validated(10).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(browser_window_signed_in_token(&store, "w3", NOW), None);
        assert_eq!(paths.each_ref().map(|path| fs::read(path).ok()), before);
    }
    let mut legacy = prefix;
    legacy.extend_from_slice(
        format!(r#"{{"t":{},"kind":"sign-in","account":"w3"}}"#, NOW + 1).as_bytes(),
    );
    fs::write(store.ledger_path(), &legacy).unwrap();
    assert_eq!(store.tail_validated(10).unwrap().last().unwrap().why, "");
    assert_eq!(browser_window_signed_in_token(&store, "w3", NOW), None);
    assert_eq!(fs::read(store.ledger_path()).unwrap(), legacy);
}

/// Unix only, where aterm links the opener an unattended run's page is handed to
/// (`cli::opener_link`); off Unix `an_unattended_run_is_handed_nothing_where_aterm_links_no_opener`
/// holds what runs.
#[cfg(unix)]
#[test]
fn an_expired_retry_keeps_its_owner_when_another_sign_in_is_running() {
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
    let path = desk.roster();
    let roster = Roster::load(&path).unwrap();
    let fx = bench.fx();
    let ctx = LadderCtx::of(&fx, &path);
    let clock = Instant::now();
    let row = roster.row("w3").unwrap();
    let LadderStep::Go { inputs, .. } =
        ladder_route_with_clock(&desk.home, &store, row, NOW, &bench.guard, &ctx, &|| clock)
    else {
        panic!("the configured profile provides a route");
    };
    let later = {
        let mut guard = bench.guard.lock().unwrap();
        let wait = guard.ladder.failed("w3", &inputs, clock);
        guard
            .ladder_owners
            .insert("w3".into(), LoginOwner::new(&store, Cli::Claude, "w3", NOW));
        guard.in_flight = true;
        clock + wait
    };
    assert!(matches!(
        ladder_route_with_clock(&desk.home, &store, row, NOW, &bench.guard, &ctx, &|| later),
        LadderStep::Go { .. }
    ));
    assert!(AutoRenewPermit::claim(&bench.guard, "w3", later, false).is_none());
    let guard = bench.guard.lock().unwrap();
    assert!(guard.ladder.retry_at("w3", later).is_none());
    assert!(guard.ladder.has_backoff("w3"));
    assert!(guard.ladder_owners.contains_key("w3"));
}

#[cfg(unix)]
#[test]
fn a_failed_login_reports_the_last_hold_before_its_ladder_can_retry() {
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
    let path = desk.roster();
    let fx = bench.fx();
    let ctx = LadderCtx::of(&fx, &path);
    let started = Instant::now();
    let raw = Roster::load(&path).unwrap();
    let row = raw.row("w3").unwrap();
    let LadderStep::Go { inputs, .. } =
        ladder_route_with_clock(&desk.home, &store, row, NOW, &bench.guard, &ctx, &|| {
            started
        })
    else {
        panic!("the unchanged configured profile provides a route");
    };
    let records = store.read_account_records(Some(&path), NOW).unwrap();
    let row = records.roster.row("w3").unwrap();
    let raw = records.raw_roster.row("w3").unwrap();
    let permit = AutoRenewPermit::claim(&bench.guard, "w3", started, false).unwrap();
    permit.attribute_row(&store, Cli::Claude, row, raw, NOW, &records.logins);
    let finished = started + Duration::from_secs(60);
    let retry = {
        let mut guard = bench.guard.lock().unwrap();
        let wait = guard.ladder.failed("w3", &inputs, finished);
        guard.ladder_owners.insert(
            "w3".into(),
            LoginOwner::new(&store, Cli::Claude, "w3", NOW + 60)
                .observing(&records.logins)
                .for_row(row)
                .for_workflow(raw, &records.logins),
        );
        finished + wait
    };
    drop(permit);
    let paths = [
        store.state_file(),
        store.state_witness_file(),
        store.logins_file(),
        store.logins_witness_file(),
        store.ledger_path(),
        store.wake_path(),
        store.machine_file(),
        store.dir.join("sync-commit.json"),
        store.dir.join("sync-commit.seen"),
        path.clone(),
    ];
    let images = paths.each_ref().map(|path| fs::read(path).ok());
    let activity = {
        let guard = bench.guard.lock().unwrap();
        assert_eq!(guard.last_attempt.get("w3"), Some(&started));
        assert_eq!(guard.ladder.retry_at("w3", finished), Some(retry));
        login_activity_of(&guard, &store, Cli::Claude, NOW + 60, finished)
    };
    let cooldown = started + Duration::from_secs(900);
    assert!(retry > cooldown, "the ladder starts at completion");
    assert!(
        AutoRenewPermit::claim(&bench.guard, "w3", cooldown - Duration::from_secs(1), false,)
            .is_none()
    );
    // No mailbox, preparation, route or workflow input changes, and no explicit bypass:
    // the automatic route still refuses when only the launch cooldown has expired.
    assert!(matches!(
        ladder_route_with_clock(
            &desk.home,
            &store,
            row,
            NOW + 900,
            &bench.guard,
            &ctx,
            &|| cooldown,
        ),
        LadderStep::Held
    ));
    assert!(matches!(
        ladder_route_with_clock(
            &desk.home,
            &store,
            row,
            NOW + 960,
            &bench.guard,
            &ctx,
            &|| retry,
        ),
        LadderStep::Go { .. }
    ));
    assert!(AutoRenewPermit::claim(&bench.guard, "w3", retry, false).is_some());
    assert_eq!(paths.each_ref().map(|path| fs::read(path).ok()), images);
    assert_eq!(activity.len(), 1);
    assert!(!activity[0].active);
    assert!(activity[0].matches_row(row));
    assert_eq!(
        activity[0].next_at,
        Some(NOW + 60 + i64::try_from(retry.duration_since(finished).as_secs()).unwrap()),
        "a retry cannot precede the last hold on this account"
    );
}

#[cfg(unix)]
#[test]
fn a_published_alternate_keeps_the_requested_retry_and_the_actual_login() {
    let desk = Desk::new("m30");
    hand_roster(&desk, true);
    let store = desk.store();
    let path = desk.roster();
    let mut roster = Roster::load(&path).unwrap();
    roster.rows.push(accounts::Row {
        label: "alternate".into(),
        email: Some("alternate@example.test".into()),
        dir: Some(
            store
                .login_dir("alternate")
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
        ),
        ..accounts::Row::default()
    });
    fs::write(&path, format_roster_toml(&roster).unwrap()).unwrap();
    fs::create_dir_all(crate::harness::headless_oauth::browser_profile_dir(
        &store.dir,
        Some("w3"),
    ))
    .unwrap();
    let bench = Bench {
        browser: true,
        ..Bench::ending(Renewal::AuthFailed)
    };
    {
        let mut guard = bench.guard.lock().unwrap();
        let clock = Instant::now();
        guard.last_attempt.insert("alternate".into(), clock);
        guard.parked.insert("alternate".into(), clock);
        guard.bot_checked.insert("alternate".into(), clock);
        guard.activity_owners.insert(
            "alternate".into(),
            LoginOwner::new(&store, Cli::Claude, "alternate", NOW - 1),
        );
        guard.ladder.failed("alternate", "old inputs", clock);
        guard.ladder_owners.insert(
            "alternate".into(),
            LoginOwner::new(&store, Cli::Claude, "alternate", NOW - 1),
        );
    }
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let called = Arc::clone(&calls);
    let retained = Arc::new(Mutex::new(None));
    let saved = Arc::clone(&retained);
    let completion_store = desk.store();
    let completion_guard = Arc::clone(&bench.guard);
    let mut fx = bench.fx();
    fx.run_attempt = Some(Arc::new(move |_, _| {
        called.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let logins = completion_store.load_logins(NOW).unwrap().0;
        let rows = accounts::bound(&roster, &logins);
        let staging = completion_store.claim_staging_dir("w3").unwrap();
        fs::write(staging.join(".claude.json"),
            br#"{"oauthAccount":{"emailAddress":"alternate@example.test","accountUuid":"alternate"}}"#).unwrap();
        let result = complete_auto_staging_renewal(
            &completion_store,
            accounts::RenewalPolicy {
                roster_path: &path, roster: &roster,
                row: rows.row("w3").unwrap(), logins: &logins,
            },
            &staging, staging.to_str().unwrap(),
            (&super::super::align::Captured {
                stdout: br#"{"loggedIn":true,"authMethod":"claude.ai","email":"alternate@example.test"}"#.to_vec(),
                code: Some(0),
            }, Some("profile")), NOW,
            (&completion_guard, Cli::Claude),
        );
        *saved.lock().unwrap() = Some(staging);
        result
    }));
    assert!(auto_renew_next_account(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW,
        &fx
    ));
    let staging = retained.lock().unwrap().clone().unwrap();
    assert!(staging.join(".claude.json").is_file());
    assert!(staging.join(accounts::RENEWAL_CHECKPOINT_FILE).is_file());
    let logins = store.load_logins(NOW).unwrap().0;
    assert_eq!(
        logins.get("alternate").unwrap().dir.as_deref(),
        staging.to_str()
    );
    assert_eq!(logins.get("w3").unwrap().wrong_at, Some(NOW));
    assert_eq!(
        attempt_of(&desk, "w3").unwrap().outcome,
        accounts::AttemptOutcome::Failed
    );
    assert_eq!(
        attempt_of(&desk, "alternate").unwrap().outcome,
        accounts::AttemptOutcome::SignedIn
    );
    {
        let guard = bench.guard.lock().unwrap();
        let activity = login_activity_of(&guard, &store, Cli::Claude, NOW, Instant::now());
        assert!(
            activity.iter().all(|entry| entry.label != "alternate"),
            "the published alternate must no longer report a local retry: {activity:?}"
        );
        assert!(activity.iter().any(|entry| entry.label == "w3"));
        assert!(!guard.last_attempt.contains_key("alternate"));
        assert!(!guard.parked.contains_key("alternate"));
        assert!(!guard.bot_checked.contains_key("alternate"));
        assert!(!guard.activity_owners.contains_key("alternate"));
        assert!(!guard.ladder_owners.contains_key("alternate"));
        assert!(guard.ladder.retry_at("alternate", Instant::now()).is_none());
    }
    assert!(
        bench
            .guard
            .lock()
            .unwrap()
            .ladder
            .retry_at("w3", Instant::now())
            .is_some()
    );
    bench.cool();
    assert!(!auto_renew_next_account(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW + 1,
        &fx
    ));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn a_roster_identity_edit_reconsiders_the_row_without_roster_rewrite_noise() {
    let desk = Desk::new("m30");
    hand_roster(&desk, false);
    let bench = Bench::ending(Renewal::AuthFailed);
    let no_route_count = || {
        bench
            .logged()
            .iter()
            .filter(|line| line.contains("account w3: automatic sign-in no-route"))
            .count()
    };
    assert!(!bench.pass(&desk, NOW));
    assert_eq!(no_route_count(), 1);
    let original = fs::read_to_string(desk.roster()).unwrap();
    fs::write(desk.roster(), format!("{original}\n# unchanged rows\n")).unwrap();
    assert!(!bench.pass(&desk, NOW + 1));
    assert_eq!(
        no_route_count(),
        1,
        "rewriting an unchanged roster keeps its wait"
    );

    let replacement = original.replace("w3.m30.3@example.test", "new.m30.3@example.test");
    assert_ne!(replacement, original);
    fs::write(desk.roster(), replacement).unwrap();
    assert!(!bench.pass(&desk, NOW + 2));
    assert_eq!(
        no_route_count(),
        2,
        "the new row is reconsidered immediately"
    );
    assert!(!bench.pass(&desk, NOW + 3));
    assert_eq!(
        no_route_count(),
        2,
        "the unchanged replacement keeps its own wait"
    );
    assert!(
        bench.handed().is_empty(),
        "no provider request without a route"
    );
}

#[cfg(unix)]
#[test]
fn a_changed_account_does_not_receive_an_earlier_attempts_failure() {
    for changed in ["same", "other", "email", "identity", "browser", "directory"] {
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
        let after = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&after);
        let path = desk.roster();
        let changed_store = desk.store();
        let mut fx = bench.fx();
        fx.run_attempt = Some(Arc::new(move |_, _| {
            let mut roster = Roster::load(&path).unwrap();
            let row = roster
                .rows
                .iter_mut()
                .find(|row| row.label == "w3")
                .unwrap();
            match changed {
                "same" | "other" => {}
                "email" => row.email = Some("replacement@example.test".into()),
                "identity" => row.identity_email = Some("provider@example.test".into()),
                "browser" => row.browser = Some("Profile 2".into()),
                "directory" => {
                    row.dir = Some(
                        path.with_file_name("replacement")
                            .to_string_lossy()
                            .into_owned(),
                    )
                }
                _ => unreachable!(),
            }
            if changed == "other" {
                roster
                    .rows
                    .iter_mut()
                    .find(|row| row.label == "main")
                    .unwrap()
                    .browser = Some("Profile 9".into());
            }
            fs::write(&path, format_roster_toml(&roster).unwrap()).unwrap();
            *captured.lock().unwrap() = Some(
                [
                    changed_store.state_file(),
                    changed_store.logins_file(),
                    changed_store.ledger_path(),
                ]
                .map(|path| fs::read(path).ok()),
            );
            Renewal::AuthFailed
        }));
        assert!(auto_renew_next_account(
            &desk.home,
            &desk.state,
            &desk.roster(),
            NOW,
            &fx
        ));
        if matches!(changed, "same" | "other") {
            assert_eq!(
                attempt_of(&desk, "w3").unwrap().outcome,
                accounts::AttemptOutcome::Failed
            );
        } else {
            assert_eq!(
                [store.state_file(), store.logins_file(), store.ledger_path()]
                    .map(|path| fs::read(path).ok()),
                after.lock().unwrap().clone().unwrap(),
                "a changed {changed} must not inherit an earlier attempt's record"
            );
            assert!(attempt_of(&desk, "w3").is_none());
            assert!(
                !bench
                    .logged()
                    .iter()
                    .any(|line| line.contains("automatic sign-in failed"))
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn a_replaced_login_stops_the_automatic_retry_and_failure_summary() {
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
    let called = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let after = Arc::new(Mutex::new(None));
    let mut fx = bench.fx();
    let changed_store = store.clone();
    let count = Arc::clone(&called);
    let changed_after = Arc::clone(&after);
    fx.run_attempt = Some(Arc::new(move |_, _| {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = changed_store.login_dir("w3").unwrap();
        changed_store
            .transact_logins(NOW, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "w3",
                    dir.to_str(),
                    dir.to_str().unwrap(),
                    None,
                    NOW,
                ))
            })
            .unwrap();
        *changed_after.lock().unwrap() = Some(
            [
                changed_store.state_file(),
                changed_store.logins_file(),
                changed_store.ledger_path(),
            ]
            .map(|path| fs::read(path).ok()),
        );
        Renewal::AuthFailed
    }));
    assert!(auto_renew_next_account(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW,
        &fx
    ));
    assert_eq!(called.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        [store.state_file(), store.logins_file(), store.ledger_path(),]
            .map(|path| fs::read(path).ok()),
        after.lock().unwrap().clone().unwrap()
    );
    assert!(attempt_of(&desk, "w3").is_none());
}

/// A row with a stored headless profile and a desk browser profile: the ladder drives the
/// profile headless, and when that run makes no login, nothing that opens a window (the desk
/// profile, the default browser) is tried in its place. The run that failed is on record with
/// its route, how it ended and every route read, is one log line, and sets the row's backoff:
/// the next pass, past the sign-in cooldown, starts nothing and logs nothing.
///
/// Unix only, where aterm links the opener an unattended run's page is handed to
/// (`cli::opener_link`); off Unix `an_unattended_run_is_handed_nothing_where_aterm_links_no_opener`
/// holds what runs.
#[cfg(unix)]
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

/// Off Unix aterm links no opener for a sign-in's page (`cli::opener_link`), so an unattended
/// run is handed no browser at all, never a window in its place: the row is not signed in, its
/// failure is one log line saying why, and the row's backoff holds the next pass.
#[cfg(not(unix))]
#[test]
fn an_unattended_run_is_handed_nothing_where_aterm_links_no_opener() {
    let desk = Desk::new("m30");
    hand_roster(&desk, true);
    fs::create_dir_all(crate::harness::headless_oauth::browser_profile_dir(
        &desk.store().dir,
        Some("w3"),
    ))
    .unwrap();
    let bench = Bench {
        browser: true,
        ..Bench::ending(Renewal::Renewed)
    };
    assert!(bench.pass(&desk, NOW), "the profile route started");
    assert!(bench.handed().is_empty(), "{:?}", bench.handed());
    assert_ne!(
        attempt_of(&desk, "w3").map(|a| a.outcome),
        Some(accounts::AttemptOutcome::SignedIn)
    );
    let lines = bench.logged();
    let cause = crate::harness::cli::opener_link(
        &desk.store(),
        &crate::harness::cli::LiveAccounts::default(),
    )
    .unwrap_err()
    .to_string();
    let failed: Vec<&String> = lines.iter().filter(|l| l.contains(&cause)).collect();
    assert_eq!(failed.len(), 1, "{lines:?}");

    bench.cool();
    assert!(!bench.pass(&desk, NOW + 60), "the backoff holds");
    assert_eq!(bench.logged(), lines, "nothing logged while held");
}

/// On a vault member whose subscriber rewrites `state.toml` to say when it last heard the vault,
/// a row whose mailbox route ran and failed stays in its backoff: the rewrite delivered nothing.
/// A file the vault delivers to `~/.secrets` ends the backoff at once.
///
/// Unix only, where aterm links the opener an unattended run's page is handed to
/// (`cli::opener_link`); off Unix `an_unattended_run_is_handed_nothing_where_aterm_links_no_opener`
/// holds what runs.
#[cfg(unix)]
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
///
/// Unix only, where aterm links the opener an unattended run's page is handed to
/// (`cli::opener_link`); off Unix `an_unattended_run_is_handed_nothing_where_aterm_links_no_opener`
/// holds what runs.
#[cfg(unix)]
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
    // CONTROL: the row a session here runs on, signed out, is signed in.
    runs_signed_out(&desk, &sun, NOW + 5);
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
///
/// Unix only, where aterm links the opener an unattended run's page is handed to
/// (`cli::opener_link`); off Unix `an_unattended_run_is_handed_nothing_where_aterm_links_no_opener`
/// holds what runs.
#[cfg(unix)]
#[test]
fn a_person_turning_logins_off_stops_the_ladder() {
    for logins in ["off", "renew"] {
        let desk = Desk::new("m30");
        hand_roster(&desk, false);
        aterm_uds::atomic_file::write(
            &(desk.config.join("aterm.toml")),
            (format!("[fleet]\nmachine = \"m30\"\n\n[harness]\nlogins = \"{logins}\"\n")).as_ref(),
            0o600,
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
#[test]
fn a_row_changed_during_route_read_gets_no_stale_no_route_summary() {
    for changed in [false, true] {
        let desk = Desk::new("m30");
        hand_roster(&desk, false);
        let path = desk.roster();
        let mailbox = move |_: &Path, _: &accounts::Store, _: &Row| {
            if changed {
                let mut roster = Roster::load(&path).unwrap();
                roster
                    .rows
                    .iter_mut()
                    .find(|row| row.label == "w3")
                    .unwrap()
                    .email = Some("replacement@example.test".into());
                fs::write(&path, format_roster_toml(&roster).unwrap()).unwrap();
            }
            (crate::harness::sign_in_ladder::Mailbox::None, None)
        };
        let bench = Bench::ending(Renewal::AuthFailed);
        let mut fx = bench.fx();
        fx.mailbox = &mailbox;
        assert!(!auto_renew_next_account(
            &desk.home,
            &desk.state,
            &desk.roster(),
            NOW,
            &fx
        ));
        assert_eq!(attempt_of(&desk, "w3").is_some(), !changed);
        assert_eq!(
            bench
                .logged()
                .iter()
                .any(|line| line.contains("account w3: automatic sign-in no-route")),
            !changed
        );
        assert!(bench.handed().is_empty());
    }
}
