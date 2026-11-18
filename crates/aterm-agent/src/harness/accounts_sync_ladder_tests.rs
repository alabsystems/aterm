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
/// as `ends` and each browser it is handed kept, and every line the ladder logs kept. A `live`
/// bench scripts nothing: each run is the real one, of the `claude` the home holds.
pub(super) struct Bench {
    cooldown: Watched<Option<Instant>>,
    rediscovery: Watched<Rediscovery>,
    pub(super) guard: Arc<Watched<AutoRenewGuard>>,
    pub(super) browser: bool,
    logged: Arc<Mutex<Vec<String>>>,
    handed: Arc<Mutex<Vec<RenewBrowser>>>,
    ends: Renewal,
    /// The vault's sync: this machine is in none unless a test says so.
    pub(super) vault_sync: fn(&Path, Option<&crate::harness::account_work::Scope>) -> Vault,
    /// Which rows run on their vault token here: unknown unless a test says so.
    pub(super) tokens_here: TokensRead,
    /// Each run is the live one ([`UpkeepEffects::run_attempt`] unset): `ends` and `handed` go
    /// unused.
    pub(super) live: bool,
}

impl Bench {
    pub(super) fn ending(ends: Renewal) -> Self {
        Self {
            cooldown: Watched::new("accounts_sync/sync_cooldown", None),
            rediscovery: Watched::new("accounts_sync/rediscovery", Rediscovery::default()),
            guard: AutoRenewGuard::default().shared(),
            browser: false,
            logged: Arc::default(),
            handed: Arc::default(),
            ends,
            vault_sync: |home, _scope| not_a_member(home),
            tokens_here: |_, _, _, _| None,
            live: false,
        }
    }

    pub(super) fn fx(&self) -> UpkeepEffects<'_> {
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
            vault_sync: self.vault_sync,
            spawn_vault: &inline,
            default_check: |_: &Path, _scope| {
                crate::harness::cli::LoginCheck::CouldNotRun(String::new())
            },
            mailbox: &ladder_mailbox_files,
            log: Arc::new(move |line: &str| logged.lock().unwrap().push(line.to_owned())),
            tokens_here: self.tokens_here,
            shared: shared_renewal::Effects::default(),
            run_attempt: (!self.live).then(|| -> RunAttempt {
                Arc::new(move |browser: &RenewBrowser, _headless: bool| {
                    handed.lock().unwrap().push(browser.clone());
                    ends
                })
            }),
        }
    }

    /// One upkeep's sign-in pass at `now`: whether a sign-in started.
    pub(super) fn pass(&self, desk: &Desk, now: i64) -> bool {
        auto_renew_next_account(&desk.home, &desk.state, &desk.roster(), now, &self.fx())
    }

    pub(super) fn handed(&self) -> Vec<RenewBrowser> {
        self.handed.lock().unwrap().clone()
    }

    pub(super) fn logged(&self) -> Vec<String> {
        self.logged.lock().unwrap().clone()
    }

    /// Ends the 15-minute cooldown a started sign-in sets ([`AutoRenewPermit`]), so a pass
    /// that starts nothing is the ladder's own backoff holding.
    pub(super) fn cool(&self) {
        self.guard.lock().unwrap().last_attempt.clear();
    }
}

/// A session here runs on `label`, and its login is signed out here: the upkeep's door for a
/// sign-in (`upkeep_sign_in`, `DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 4).
pub(super) fn runs_signed_out(desk: &Desk, label: &str, at: i64) {
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
pub(super) fn hand_roster(desk: &Desk, desk_browser: bool) {
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

pub(super) fn attempt_of(desk: &Desk, label: &str) -> Option<accounts::SignInAttempt> {
    desk.store()
        .load_logins(NOW)
        .unwrap()
        .0
        .get(label)
        .and_then(|b| b.sign_in_attempt.clone())
}

/// A PAGE'S STOP THAT NO BOT PROTECTION MADE (a sign-in window a person closed) HOLDS ITS ROW FOR
/// SIX HOURS, counted from its last try and on record: past a restart, which forgets the ladder's
/// own wait, a changed secret, which ends that wait as a new input, and a move, which is heard at
/// every launch. CONTROL: a person's signed-in browser window ends the hold, and six hours do, in a
/// process that holds its own wait too. A check's stop holds on no clock
/// ([`a_claude_row_a_check_stopped_waits_for_a_person_at_every_door`]).
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
            .transact_pool_logins_within(&[], &desk.roster(), at, &mut None, |raw, _, logins| {
                accounts::admit_workflow(logins, "w3")?;
                Ok(accounts::note_sign_in_stop(
                    logins,
                    raw.row("w3").expect("the row"),
                    false,
                    crate::harness::open_url::MetWord::WindowClosed,
                    "window: a person closed it",
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
            .page_stopped("w3", Some(Instant::now() + 2 * PERSON_BACKOFF));
        bench.cool();
        bench
    };
    let hold = PERSON_BACKOFF_SECS;

    stop(NOW);
    let restarted = aterm();
    assert!(!restarted.pass(&desk, NOW + 60), "a restart");
    crate::vault_entry::seed(&desk.home, &[("synced-from-the-vault", b"new")]);
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

/// A CHECK HOLDS ITS ROW ON NO CLOCK: a sign-in that ended on a page's human check (recorded
/// `blocked` with its page, and as the account's [`accounts::CheckStop`]) is not tried again by
/// the upkeep six hours on, or any time on, past a restart too. CONTROL: a person's signed-in
/// browser window ends the hold.
#[cfg(unix)]
#[test]
fn a_check_holds_its_row_until_a_persons_window_on_no_clock() {
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
    check_stopped(&desk, crate::harness::open_url::MetWord::HumanCheck, NOW);
    let attempt = attempt_of(&desk, "w3").unwrap();
    assert_eq!(
        (attempt.outcome, attempt.stop),
        (
            accounts::AttemptOutcome::Blocked,
            Some(crate::harness::open_url::MetWord::HumanCheck)
        )
    );
    for later in [60, PERSON_BACKOFF_SECS + 1, 10 * PERSON_BACKOFF_SECS] {
        assert!(!aterm().pass(&desk, NOW + later), "held {later}s on");
    }
    let at = NOW + 10 * PERSON_BACKOFF_SECS + 1;
    store
        .transact_pool_logins_within(&[], &desk.roster(), at, &mut None, |_, _, logins| {
            accounts::admit_workflow(logins, "w3")?;
            Ok(vec![
                accounts::Event::new(at, accounts::Kind::SignIn, "w3")
                    .because("claude.ai window done session-kept"),
            ])
        })
        .unwrap();
    assert!(
        aterm().pass(&desk, at + 1),
        "CONTROL: a person's signed-in window ends it"
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

std::thread_local! {
    /// The instant [`scripted`] reads on this thread.
    #[cfg(unix)]
    static SCRIPTED: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// A guard clock a test scripts ([`MachineReads::clock`]): the instant last set on this thread.
/// The bench runs every sign-in inline, so the work a pass starts reads it too.
#[cfg(unix)]
fn scripted() -> Instant {
    SCRIPTED
        .get()
        .expect("the test sets the scripted clock first")
}

/// L6, THE LADDER'S WAIT AS A DEADLINE: A ROW BACKED OFF ASKS EXACTLY ITS RETRY_AT, AND AT THAT
/// DEADLINE ITS RENEWAL RUNS ONCE AND SPAWNS ONCE. A row whose sign-in failed waits out its
/// backoff on the guard's clock, here scripted. Each renewal pass (the work of Claude Code's
/// `ClaudeRenew` duty) while it waits, a minute in and a nanosecond short of its end, starts
/// nothing, and the horizon of its trace asks for exactly the row's `retry_at` as its monotonic
/// deadline: the wake a keeper takes from it. The pass at that deadline starts the row's sign-in
/// once (one more run of `claude auth login`); that run fails too, and a pass after it at the
/// same instant starts nothing and asks for the end of the doubled wait. The duty itself still
/// takes the wall clock raw (`upkeep_claude`), so its trace is poisoned and reruns on the minute
/// as well until that clock moves to `trace::Now`; this law is the deadline recorded beside it.
///
/// NEGATIVE CONTROL (2026-11-15): the design's `Since` recording nothing, made as
/// `Ladder::held` comparing its wait raw (`now < b.next` in place of `!reached(b.next, now)`),
/// the one mutation of its mutant build that reached this test and the ladder's own
/// `a_rows_wait_that_holds_records_when_it_ends_and_one_over_records_nothing`: `the row due at
/// retry_at was not retried: its held pass asked the keeper for None, not its retry_at` failed,
/// left `None`, right the row's `retry_at`; restored, the file touched.
#[cfg(unix)]
#[test]
fn a_backed_off_row_asks_exactly_its_retry_at_and_at_it_renews_once_and_spawns_once() {
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
        guard: guard_reading(MachineReads {
            clock: scripted,
            ..MachineReads::default()
        }),
        ..Bench::ending(Renewal::Failed)
    };
    // One renewal pass at guard clock `at` and wall `wall`: whether it started a sign-in, and
    // the monotonic deadline its trace's horizon asks for.
    let pass_at = |at: Instant, wall: i64| {
        SCRIPTED.set(Some(at));
        let (started, read) = trace::scope(|| bench.pass(&desk, wall));
        (started, read.horizon().mono)
    };
    let first = Instant::now();
    assert!(pass_at(first, NOW).0, "the first try");
    assert_eq!(bench.handed().len(), 1);
    // The first try's permit cooldown ends with the backoff's first wait: clear it, as the
    // other ladder tests do, so the ladder's own wait alone holds the row.
    bench.cool();
    let retry_at = bench
        .guard
        .lock()
        .unwrap()
        .ladder
        .retry_at("w3", first)
        .expect("the failed row waits");
    let wait = crate::harness::sign_in_ladder::BACKOFF_FIRST;
    assert_eq!(retry_at, first + wait);

    for (at, wall) in [
        (first + Duration::from_secs(60), NOW + 60),
        (retry_at - Duration::from_nanos(1), NOW + 899),
    ] {
        let (started, asked) = pass_at(at, wall);
        assert!(!started, "the row waits");
        assert_eq!(
            asked,
            Some(retry_at),
            "the row due at retry_at was not retried: its held pass asked the keeper for \
             {asked:?}, not its retry_at"
        );
    }
    assert_eq!(bench.handed().len(), 1, "a held pass starts nothing");

    assert!(
        pass_at(retry_at, NOW + 900).0,
        "at retry_at the row is tried"
    );
    assert_eq!(bench.handed().len(), 2, "once");
    let (started, asked) = pass_at(retry_at, NOW + 900);
    assert!(!started, "its second failure holds it");
    assert_eq!(
        asked,
        Some(retry_at + wait * 2),
        "the pass asks for the doubled wait's end, not the permit's cooldown"
    );
    assert_eq!(bench.handed().len(), 2);
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
        let guard = AutoRenewGuard::default().shared();
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
        guard.unrecorded_checks.insert(
            "alternate".into(),
            super::login_activity_tests::a_check_met("alternate"),
        );
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
        assert!(!guard.unrecorded_checks.contains_key("alternate"));
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
    // The log names the row by its account's key, which its address makes: the row before the
    // edit and the row after it each have their own.
    let first = logged_as(W3_EMAIL);
    let no_route_count = |logged: &[&str]| {
        bench
            .logged()
            .iter()
            .filter(|line| {
                logged.iter().any(|logged| {
                    line.contains(&format!("account {logged}: automatic sign-in no-route"))
                })
            })
            .count()
    };
    assert!(!bench.pass(&desk, NOW));
    assert_eq!(no_route_count(&[&first]), 1);
    let original = fs::read_to_string(desk.roster()).unwrap();
    fs::write(desk.roster(), format!("{original}\n# unchanged rows\n")).unwrap();
    assert!(!bench.pass(&desk, NOW + 1));
    assert_eq!(
        no_route_count(&[&first]),
        1,
        "rewriting an unchanged roster keeps its wait"
    );

    let replacement = original.replace("w3.m30.3@example.test", "new.m30.3@example.test");
    assert_ne!(replacement, original);
    fs::write(desk.roster(), replacement).unwrap();
    let renamed = logged_as("new.m30.3@example.test");
    assert_ne!(renamed, first, "a new address is a new account");
    assert!(!bench.pass(&desk, NOW + 2));
    assert_eq!(
        no_route_count(&[&first, &renamed]),
        2,
        "the new row is reconsidered immediately"
    );
    assert!(!bench.pass(&desk, NOW + 3));
    assert_eq!(
        no_route_count(&[&first, &renamed]),
        2,
        "the unchanged replacement keeps its own wait"
    );
    assert!(
        bench.handed().is_empty(),
        "no provider request without a route"
    );
    names_only_by_key(&bench.logged(), &[&first, &renamed]);
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
    // NEGATIVE CONTROL: with the ladder naming the row as a message does (`shown_row` for
    // `next_logged`), this fails on `account claude w3.m30.3@example.test: automatic sign-in
    // failed: …` (2026-11-16, by hand, then restored).
    let lines = bench.logged();
    let logged = logged_as(W3_EMAIL);
    let failed: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with(&format!("account {logged}: automatic sign-in failed: ")))
        .collect();
    assert_eq!(failed.len(), 1, "{lines:?}");
    assert!(failed[0].contains("profile:"), "{}", failed[0]);
    assert!(
        lines.contains(&format!(
            "account {logged}: automatic sign-in started by its profile route"
        )),
        "{lines:?}"
    );
    names_only_by_key(&lines, &[&logged]);

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
/// The vault's own bookkeeping, a rewrite of when it heard the vault or a new version that wrote
/// nothing here, neither ends a row's backoff nor resets its step, so the next failure doubles
/// it; an entry the vault delivers ends the backoff at once.
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
    // A real store this home administers; its subscriber's bookkeeping is the `heard` line of
    // its `state.toml`, and a version that writes nothing here is a publication of the same
    // entries.
    crate::vault_entry::seed(&desk.home, &[("vault-fixture", b"fixture\n")]);
    let vault = desk.home.join(".config/aterm/secrets");
    let heard = |republish: bool, at: i64| {
        if republish {
            crate::vault_entry::seed(&desk.home, &[("vault-fixture", b"fixture\n")]);
        }
        let state = vault.join("state.toml");
        let text = fs::read_to_string(&state).unwrap();
        let mut lines: Vec<String> = text
            .lines()
            .filter(|line| !line.starts_with("heard = "))
            .map(str::to_owned)
            .collect();
        lines.push(format!("heard = {at}"));
        fs::write(&state, lines.join("\n") + "\n").unwrap();
        let later =
            std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(at).unwrap_or(0));
        fs::File::options()
            .write(true)
            .open(&state)
            .unwrap()
            .set_modified(later)
            .unwrap();
    };
    heard(false, NOW);
    let bench = Bench::ending(Renewal::AuthFailed);
    assert!(bench.pass(&desk, NOW), "the mailbox route started");
    assert_eq!(bench.handed().len(), 1);
    assert_eq!(
        attempt_of(&desk, "w3").map(|a| a.outcome),
        Some(accounts::AttemptOutcome::Failed)
    );

    heard(false, NOW + 60);
    bench.cool();
    assert!(
        !bench.pass(&desk, NOW + 60),
        "a rewrite of when it heard is no delivery"
    );
    heard(true, NOW + 90);
    bench.cool();
    assert!(
        !bench.pass(&desk, NOW + 90),
        "nor is a version published elsewhere that wrote nothing here"
    );
    assert_eq!(bench.handed().len(), 1);

    bench.guard.lock().unwrap().ladder.moved(Instant::now());
    bench.cool();
    assert!(
        bench.pass(&desk, NOW + 100),
        "its wait over, the route runs"
    );
    let waits: Vec<String> = bench
        .logged()
        .iter()
        .filter(|line| line.contains("tried again in"))
        .cloned()
        .collect();
    assert!(
        waits.len() == 2 && waits[1].contains("tried again in 30m"),
        "the step the bookkeeping left doubles the wait: {waits:?}"
    );

    crate::vault_entry::seed(
        &desk.home,
        &[(
            crate::harness::headless_oauth::SECRETS_MAILBOX_TOML_FILE,
            b"http_url = \"http://127.0.0.1:9/inbox\"\n",
        )],
    );
    assert!(
        bench.pass(&desk, NOW + 120),
        "a delivered entry ends the backoff and the sign-in cooldown"
    );
    assert_eq!(bench.handed().len(), 3);
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
    crate::vault_entry::seed(
        &desk.home,
        &[(
            crate::harness::headless_oauth::SECRETS_MAILBOX_TOML_FILE,
            b"[passwords]\n\"codex.m27.2@example.test\" = \"pw\"\n",
        )],
    );
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
            (&desk.roster(), trace::Now::at(NOW), UpkeepStep::Both),
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
        let no_route = format!(
            "account {}: automatic sign-in no-route",
            logged_as(W3_EMAIL)
        );
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
            bench.logged().iter().any(|line| line.contains(&no_route)),
            !changed
        );
        assert!(bench.handed().is_empty());
    }
}

/// A guard whose renewals read `reads` of the machine.
#[cfg(unix)]
fn guard_reading(reads: MachineReads) -> Arc<Watched<AutoRenewGuard>> {
    AutoRenewGuard {
        reads,
        ..AutoRenewGuard::default()
    }
    .shared()
}

/// What a Mac with a desk reads, with nothing of another machine's in it.
fn at_the_desk() -> MachineReads {
    MachineReads {
        desk: true,
        ..MachineReads::default()
    }
}

/// THE UPKEEP OPENS ATERM'S SIGN-IN WINDOW ONLY AT THIS MAC'S OWN DESK, FOR A ROW WHOSE LOGIN
/// CODE THE WINDOW READS BY ITSELF: no desk (not macOS), an SSH connection, the mark of
/// another machine's request, a session another Mac's drive typed into, a row in shared
/// custody and a mailbox the window cannot read each open none. The mailbox is read only
/// where everything else allows the window.
#[test]
fn the_upkeep_opens_the_window_only_at_this_macs_own_desk_for_a_mailbox_the_window_reads() {
    let desk = at_the_desk();
    assert!(upkeep_opens_window(desk, false, &|| true));
    assert!(
        !upkeep_opens_window(desk, false, &|| false),
        "nobody is there to type a code the window cannot read"
    );
    let unread = || panic!("the mailbox is read only where the window may open");
    assert!(!upkeep_opens_window(
        MachineReads::default(),
        false,
        &unread
    ));
    assert!(!upkeep_opens_window(desk, true, &unread), "shared custody");
    for reads in [
        MachineReads {
            var: |name| (name == "SSH_CONNECTION").then(|| "10.0.0.1 1 10.0.0.2 22".into()),
            ..desk
        },
        MachineReads {
            var: |name| (name == crate::harness::remote_renew::FOR_PEER_ENV).then(|| "1".into()),
            ..desk
        },
        MachineReads {
            driven: || true,
            ..desk
        },
    ] {
        assert!(!upkeep_opens_window(reads, false, &unread));
    }
}

/// The upkeep's row `w3` with a mail folder in the store, which the window reads by itself.
#[cfg(unix)]
fn desk_with_a_mail_folder() -> Desk {
    let desk = Desk::new("m30");
    hand_roster(&desk, false);
    fs::create_dir_all(
        desk.store()
            .dir
            .join(crate::harness::open_url::DEFAULT_MAILBOX_DIR),
    )
    .unwrap();
    desk
}

/// THE UPKEEP'S RUN IS HANDED THE WINDOW IT CHOSE, scripted or live: at this Mac's own desk a
/// row whose mail folder gives the code is handed aterm's own window, told that nobody started
/// the sign-in (`ATERM_OPEN_URL_UNATTENDED`), so the window ends at the read of any check and
/// waits on none for a person who is not there. The same row is handed the headless route where
/// the run carries another machine's mark, and so is a row whose only mailbox is a webmail
/// inbox, which a headless browser alone reads: the window would wait four minutes for a code
/// nobody types.
/// NEGATIVE CONTROL: with `upkeep_browser` handing the window as a person's (`unattended:
/// false`), `` the upkeep's window was not told nobody started it ``.
#[cfg(unix)]
#[test]
fn the_upkeeps_run_is_handed_the_window_only_where_the_window_reads_the_rows_mailbox() {
    let desk = desk_with_a_mail_folder();
    let bench = Bench {
        guard: guard_reading(at_the_desk()),
        ..Bench::ending(Renewal::AuthFailed)
    };
    assert!(bench.pass(&desk, NOW), "the mailbox route started");
    assert!(
        matches!(
            bench.handed().as_slice(),
            [RenewBrowser::Window {
                unattended: true,
                ..
            }]
        ),
        "the upkeep's window was not told nobody started it: {:?}",
        bench.handed()
    );

    let desk = desk_with_a_mail_folder();
    let bench = Bench {
        guard: guard_reading(MachineReads {
            var: |name| (name == crate::harness::remote_renew::FOR_PEER_ENV).then(|| "1".into()),
            ..at_the_desk()
        }),
        ..Bench::ending(Renewal::AuthFailed)
    };
    assert!(bench.pass(&desk, NOW), "the mailbox route started");
    assert!(
        matches!(bench.handed().as_slice(), [RenewBrowser::Remote { .. }]),
        "another machine's run opens nothing here: {:?}",
        bench.handed()
    );

    let desk = Desk::new("m30");
    hand_roster(&desk, false);
    crate::vault_entry::seed(
        &desk.home,
        &[(
            crate::harness::headless_oauth::SECRETS_MAILBOX_TOML_FILE,
            b"webmail_url = \"https://mail.example.test/inbox\"\npassword = \"a fixture's word\"\n",
        )],
    );
    let store = desk.store();
    assert!(has_configured_mailbox_for_row_with(
        &|_| None,
        Some(&desk.home),
        &store,
        Some("w3"),
        Some("w3.m30.3@example.test"),
    ));
    assert!(
        !window_reads_mailbox_for_row_with(
            &|_| None,
            Some(&desk.home),
            &store,
            Some("w3"),
            Some("w3.m30.3@example.test"),
        ),
        "a webmail inbox is read only inside a headless browser"
    );
    let bench = Bench {
        browser: true,
        guard: guard_reading(at_the_desk()),
        ..Bench::ending(Renewal::AuthFailed)
    };
    assert!(bench.pass(&desk, NOW), "the mailbox route started");
    assert!(
        matches!(bench.handed().as_slice(), [RenewBrowser::Remote { .. }]),
        "{:?}",
        bench.handed()
    );
}

/// THE UPKEEP OPENS NO WINDOW AGAIN FOR A ROW ITS LAST SIGN-IN LEFT TO A PERSON: a sign-in
/// that ended at a human check records the row's own renewal as the step a person owes it
/// ([`accounts::SignInAction::Renew`]), and while that stands the upkeep, which would open
/// the window, starts nothing, however long ago it was. CONTROL: with no such step the same
/// row's window opens, and where this run opens no window the step holds nothing here.
#[cfg(unix)]
#[test]
fn the_upkeep_opens_no_window_for_a_row_left_to_a_person() {
    let long_ago = NOW - 2 * PERSON_BACKOFF_SECS;
    let owed = |desk: &Desk| {
        desk.store()
            .transact_pool_logins_within(
                &[],
                &desk.roster(),
                long_ago,
                &mut None,
                |_, _, logins| {
                    accounts::admit_workflow(logins, "w3")?;
                    accounts::note_sign_in_task(
                        logins,
                        "w3",
                        accounts::SignInAction::Renew,
                        long_ago,
                    );
                    Ok(Vec::new())
                },
            )
            .unwrap();
    };
    let desk = desk_with_a_mail_folder();
    owed(&desk);
    let bench = Bench {
        guard: guard_reading(at_the_desk()),
        ..Bench::ending(Renewal::AuthFailed)
    };
    assert!(!bench.pass(&desk, NOW), "left to a person");
    assert!(bench.handed().is_empty(), "{:?}", bench.handed());
    bench.cool();
    assert!(!bench.pass(&desk, NOW + 60), "and at every later upkeep");
    assert!(bench.handed().is_empty(), "{:?}", bench.handed());

    let desk = desk_with_a_mail_folder();
    let bench = Bench {
        guard: guard_reading(at_the_desk()),
        ..Bench::ending(Renewal::AuthFailed)
    };
    assert!(bench.pass(&desk, NOW), "CONTROL: no step owed");
    assert!(matches!(
        bench.handed().as_slice(),
        [RenewBrowser::Window { .. }]
    ));

    let desk = desk_with_a_mail_folder();
    owed(&desk);
    let bench = Bench::ending(Renewal::AuthFailed);
    assert!(bench.pass(&desk, NOW), "CONTROL: no window to hold");
    assert!(matches!(
        bench.handed().as_slice(),
        [RenewBrowser::Remote { .. }]
    ));
}

/// `accounts renew` as a terminal on this Mac runs it: a person's, or with `agent` an agent's
/// (Claude Code's own marker in its environment). Each renewal it starts is noted, opens nothing
/// and ends at what `met` names in aterm's sign-in window.
#[cfg(unix)]
struct RenewAtTheDesk {
    agent: bool,
    now: i64,
    met: crate::harness::open_url::MetWord,
    started: std::cell::RefCell<Vec<RenewBrowser>>,
}

#[cfg(unix)]
impl crate::harness::cli::AccountsHost for RenewAtTheDesk {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, login: &str) -> Result<crate::harness::align::Captured, String> {
        Err(format!("no status check here (asked under {login:?})"))
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        Err("no login here".to_owned())
    }
    fn now(&self) -> i64 {
        self.now
    }
    fn opens_sign_in_window(&self) -> bool {
        true
    }
    fn current_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }
    fn machine_var(&self, name: &str) -> Option<std::ffi::OsString> {
        (self.agent && name == "CLAUDECODE").then(|| "1".into())
    }
    fn renew_outcome(
        &self,
        _home: &str,
        _email: &str,
        browser: &RenewBrowser,
    ) -> Result<crate::harness::cli::RenewalExit, crate::harness::cli::RenewalFailure> {
        self.started.borrow_mut().push(browser.clone());
        Ok(crate::harness::cli::RenewalExit {
            code: 1,
            met: Some(crate::harness::open_url::Met::new(
                self.met,
                Some("claude.ai"),
            )),
        })
    }
}

/// What a verb's lines name the desk's row `w3` by ([`crate::harness::account_names::RowName`]).
/// The desk's renewals run on unix alone, and so do the tests that read their lines.
#[cfg(unix)]
fn w3_named(desk: &Desk) -> crate::harness::account_names::RowName {
    crate::harness::account_names::tests::row_name(Cli::Claude, &desk.roster(), "w3")
}

/// The desk's row `w3`'s address.
const W3_EMAIL: &str = "w3.m30.3@example.test";

/// How a log line names the Claude account of `email` with no lab: its key alone, made here from
/// the address, never the address or the roster label (Decision 3).
fn logged_as(email: &str) -> String {
    aterm_db::typed::keys::account_pk("claude", email).expect("a key")
}

/// Panics where a line the ladder logged about an account (`account <name>: …`) names it by
/// anything but `logged`, or holds an address or the roster label `w3` anywhere.
fn names_only_by_key(lines: &[String], logged: &[&str]) {
    for line in lines.iter().filter(|line| line.starts_with("account ")) {
        assert!(
            logged
                .iter()
                .any(|logged| line.starts_with(&format!("account {logged}: "))),
            "a log line names its account by something but {logged:?}: {line}"
        );
        assert!(!line.contains('@'), "an address in a log line: {line}");
        assert!(
            !line
                .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
                .any(|token| token == "w3"),
            "the roster label in a log line: {line}"
        );
    }
}

/// Runs `accounts renew` at `at` on `desk` as [`RenewAtTheDesk`] does, for `w3` by name or, with
/// `due`, as a `--due` sweep: its exit, the renewals it started and what it said.
#[cfg(unix)]
fn renew_w3(
    desk: &Desk,
    (agent, met): (bool, crate::harness::open_url::MetWord),
    due: bool,
    at: i64,
) -> (ExitCode, Vec<RenewBrowser>, String) {
    let host = RenewAtTheDesk {
        agent,
        now: at,
        met,
        started: std::cell::RefCell::default(),
    };
    let env = quiet_env(&desk.home, &desk.state, &config_beside(&desk.roster()), at);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = super::super::cli::run_accounts(
        &super::super::cli::AccountsCmd::Renew {
            accounts: if due {
                Vec::new()
            } else {
                vec!["w3.m30.3@example.test".to_owned()]
            },
            due,
            on: None,
        },
        &env,
        &host,
        &mut out,
        &mut err,
    );
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    (code, host.started.into_inner(), said)
}

/// A stand-in `claude` in `desk`'s home whose every sign-in ends at `met` (an outcome file's
/// line, `window-human-check claude.ai`); what it returns reads one line per run, saying whether
/// the run was handed aterm's sign-in window.
#[cfg(unix)]
fn claude_ending_at(desk: &Desk, met: &str) -> impl Fn() -> String + use<> {
    use crate::harness::open_url::{OUTCOME_ENV, WINDOW_ENV};
    let ran = desk.home.join("claude-ran.log");
    let bin = desk.home.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    aterm_tempfile::stand_in(
        &bin.join("claude"),
        &format!(
            "#!/bin/sh\necho \"window=${WINDOW_ENV}\" >> '{}'\nprintf 'opener-outcome=1\\nphase=continuing\\nmet={met}\\n' > \"${OUTCOME_ENV}.part\"\nmv \"${OUTCOME_ENV}.part\" \"${OUTCOME_ENV}\"\nexit 1\n",
            ran.display()
        ),
    )
    .unwrap();
    move || fs::read_to_string(&ran).unwrap_or_default()
}

/// The upkeep's bench at this Mac's desk, every run the live one ([`Bench::live`]), on this
/// thread run as the account `aterm-test` ([`shared_renewal::RUN_AS`]) whatever the process's
/// `USER` is: verify's declared test environment has none, and a live run without one starts no
/// child.
#[cfg(unix)]
fn live_upkeep() -> Bench {
    shared_renewal::RUN_AS.with(|person| *person.borrow_mut() = Some("aterm-test".into()));
    Bench {
        guard: guard_reading(at_the_desk()),
        live: true,
        ..Bench::ending(Renewal::AuthFailed)
    }
}

/// The upkeep's bench at this Mac's desk, each run scripted to fail ([`Bench::ending`]): it
/// starts `w3`'s sign-in where nothing holds the row
/// ([`the_upkeeps_run_is_handed_the_window_only_where_the_window_reads_the_rows_mailbox`]).
#[cfg(unix)]
fn desk_upkeep() -> Bench {
    Bench {
        guard: guard_reading(at_the_desk()),
        ..Bench::ending(Renewal::AuthFailed)
    }
}

/// What the ladder answers for `w3` at `at` in `bench`'s process.
#[cfg(unix)]
fn w3_route(desk: &Desk, bench: &Bench, at: i64) -> LadderStep {
    let store = desk.store();
    let records = store
        .read_account_records(Some(&desk.roster()), at)
        .unwrap();
    let row = records.roster.row("w3").unwrap().clone();
    bench.cool();
    let fx = bench.fx();
    let ctx = LadderCtx::of(&fx, &desk.roster());
    ladder_route(&desk.home, &store, &row, at, &bench.guard, &ctx)
}

/// Whether the ladder holds `w3` at `at` in `bench`'s process and in a restarted one, and no pass
/// of the upkeep there starts it. The restarted process's runs are scripted, so a hold that
/// failed signs nothing in.
#[cfg(unix)]
fn w3_held(desk: &Desk, bench: &Bench, at: i64) -> bool {
    [bench, &desk_upkeep()].into_iter().all(|bench| {
        matches!(w3_route(desk, bench, at), LadderStep::Held)
            && !bench.pass(desk, at)
            && bench.handed().is_empty()
    })
}

/// Whether the ladder would start `w3`'s sign-in at `at` with nothing holding it: the CONTROL
/// that the record a test leaves holds nothing without the check beside it.
#[cfg(unix)]
fn w3_goes(desk: &Desk, at: i64) -> bool {
    matches!(w3_route(desk, &desk_upkeep(), at), LadderStep::Go { .. })
}

/// A CLAUDE ROW A CHECK STOPPED WAITS FOR A PERSON AT EVERY DOOR
/// (`docs/DESIGN-claude-sign-in-sitting-2026-11-12.md` §3): the upkeep's own sign-in, live
/// against a stand-in `claude` whose window ends at the consent page's human check, leaves the
/// row one `renew` task, one `blocked` attempt that names the page and the check's stop of its
/// account. From then on nothing with nobody there signs it in, on any clock: the ladder asked
/// seven hours later, past the six hours any other stop holds for, is held, a later upkeep and a
/// restarted one start nothing and run no child, and an agent's `accounts renew` runs none and
/// names the person's step in its own words. CONTROL: with a person at the terminal the same
/// renewal opens aterm's sign-in window once; a sign-in that ended with no desktop to open the
/// window on records no task, says it is tried again in six hours, and is, seven hours on; and a
/// Kimi row whose background renewal met a bot check, its last sign-in `blocked` on record too,
/// is tried again once six hours have passed.
#[cfg(unix)]
#[test]
fn a_claude_row_a_check_stopped_waits_for_a_person_at_every_door() {
    use crate::harness::open_url::MetWord;
    let desk = desk_with_a_mail_folder();
    let store = desk.store();
    // The live run stamps its end with the wall clock, so the fixture keeps to it.
    let now = crate::supervise::limit::unix_now();
    runs_signed_out(&desk, "w3", now - 60);
    let runs = claude_ending_at(&desk, "window-human-check claude.ai");

    let bench = live_upkeep();
    assert!(bench.pass(&desk, now), "the upkeep's sign-in");
    assert_eq!(runs(), "window=1\n", "one run, in aterm's sign-in window");
    let records = store
        .read_account_records(Some(&desk.roster()), now)
        .unwrap();
    assert_eq!(
        accounts::sign_in_tasks(&records.raw_roster, &records.logins),
        [("w3".to_owned(), accounts::SignInAction::Renew)],
        "the row's renewal is a person's"
    );
    let binding = records.logins.get("w3").expect("the row's binding");
    let attempt = binding.sign_in_attempt.clone();
    let attempt = attempt.expect("the attempt is on record");
    assert_eq!(attempt.outcome, accounts::AttemptOutcome::Blocked);
    assert_eq!(attempt.stop, Some(MetWord::WindowHumanCheck));
    assert!(attempt.waits_for_a_person());
    assert_eq!(
        binding
            .check_stop
            .as_ref()
            .and_then(|check| check.account.as_deref()),
        Some("w3.m30.3@example.test"),
        "the check's stop of the row's account"
    );
    assert!(
        bench
            .logged()
            .iter()
            .any(|line| line.contains("not tried again until a person signs it in")),
        "{:?}",
        bench.logged()
    );

    let later = now + PERSON_BACKOFF_SECS + 60 * 60;
    assert!(w3_held(&desk, &bench, later), "seven hours on");
    assert_eq!(runs(), "window=1\n", "no child ran again");

    let (code, started, said) = renew_w3(&desk, (true, MetWord::WindowHumanCheck), false, later);
    assert!(started.is_empty(), "an agent's renewal starts none: {said}");
    assert_eq!(code, ExitCode::from(1), "{said}");
    assert!(
        said.contains(&super::super::cli::left_words(
            Cli::Claude,
            &w3_named(&desk)
        )) && said.contains(&format!(
            "a person runs `aterm harness accounts renew {}`",
            w3_named(&desk).word
        )),
        "it names the person's step, and who takes it: {said}"
    );
    let (_, started, said) = renew_w3(&desk, (false, MetWord::WindowHumanCheck), false, later);
    assert!(
        matches!(
            started.as_slice(),
            [RenewBrowser::Window {
                unattended: false,
                ..
            }]
        ),
        "CONTROL: a person's renewal opens the sign-in window once, for them: {started:?} {said}"
    );
    assert_eq!(runs(), "window=1\n");

    let desk = desk_with_a_mail_folder();
    runs_signed_out(&desk, "w3", now - 60);
    let runs = claude_ending_at(&desk, "window-no-desktop");
    let bench = live_upkeep();
    assert!(
        bench.pass(&desk, now),
        "the upkeep's sign-in with no desktop"
    );
    let records = desk
        .store()
        .read_account_records(Some(&desk.roster()), now)
        .unwrap();
    assert!(
        accounts::sign_in_tasks(&records.raw_roster, &records.logins).is_empty(),
        "CONTROL: no desktop leaves no person's task"
    );
    let binding = records.logins.get("w3").expect("the row's binding");
    assert_eq!(
        binding
            .sign_in_attempt
            .as_ref()
            .and_then(|attempt| attempt.stop),
        Some(MetWord::WindowNoDesktop)
    );
    assert_eq!(binding.check_stop, None, "no bot protection stopped it");
    assert!(
        bench
            .logged()
            .iter()
            .any(|line| line.contains("(tried again in 6h or when a person signs it in")),
        "{:?}",
        bench.logged()
    );
    bench.cool();
    assert!(
        !bench.pass(&desk, now + 60 * 60),
        "CONTROL: held an hour on"
    );
    assert!(
        !live_upkeep().pass(&desk, now + 60 * 60),
        "CONTROL: held an hour on, past a restart"
    );
    bench.cool();
    assert!(
        bench.pass(&desk, later),
        "CONTROL: tried again seven hours on, as it said"
    );
    assert_eq!(runs(), "window=1\nwindow=1\n");

    for cli in [Cli::Kimi, Cli::Codex] {
        let (ended, tried) = super::cli_pool_tests::a_vendor_rows_stop_is_tried(
            cli,
            super::super::login_drive::Outcome::BotCheck(
                "the sign-in page carries Cloudflare's bot check; aterm pressed nothing there"
                    .to_owned(),
            ),
        );
        assert_eq!(ended, Renewal::BotCheck, "{cli:?}");
        assert_eq!(
            tried, [false; 3],
            "a {cli:?} row's bot check holds it as a Claude row's does: an hour, six hours \
             and a minute and a day on, after a restart"
        );
    }
    for cli in [Cli::Kimi, Cli::Codex] {
        let (ended, tried) = super::cli_pool_tests::a_vendor_rows_stop_is_tried(
            cli,
            super::super::login_drive::Outcome::NeedsSignIn("the page asks for a code".to_owned()),
        );
        assert_eq!(ended, Renewal::NeedsPerson, "{cli:?}");
        assert_eq!(
            tried,
            [false, true, true],
            "CONTROL {cli:?}: a stop no bot protection made is tried again after its six hours"
        );
    }
}

/// A CHECK MET BY A SIGN-IN THE LADDER DID NOT START HOLDS THE ROW TOO: a person's click at this
/// desk, live against a stand-in `claude` whose window ends at the consent page's human check,
/// records the row's `blocked` attempt by hand with the page, the check's stop and the row's
/// `renew` task, as the upkeep's own sign-in would; seven hours on, the ladder and the upkeep, in
/// this process and in a restarted one, start nothing.
#[cfg(unix)]
#[test]
fn a_check_a_click_met_holds_the_row_for_a_person() {
    use crate::harness::open_url::MetWord;
    let desk = desk_with_a_mail_folder();
    let now = crate::supervise::limit::unix_now();
    runs_signed_out(&desk, "w3", now - 60);
    let runs = claude_ending_at(&desk, "window-human-check claude.ai");
    let bench = live_upkeep();
    assert!(
        auto_renew_next_account_interactive_with(
            Cli::Claude,
            &desk.home,
            &desk.state,
            &desk.roster(),
            now,
            &bench.fx()
        ),
        "the click's sign-in"
    );
    assert_eq!(runs(), "window=1\n");
    let records = desk
        .store()
        .read_account_records(Some(&desk.roster()), now)
        .unwrap();
    assert_eq!(
        accounts::sign_in_tasks(&records.raw_roster, &records.logins),
        [("w3".to_owned(), accounts::SignInAction::Renew)]
    );
    let binding = records.logins.get("w3").expect("the row's binding");
    let attempt = binding.sign_in_attempt.clone().expect("the attempt");
    assert_eq!(
        (attempt.outcome, attempt.stop),
        (
            accounts::AttemptOutcome::Blocked,
            Some(MetWord::WindowHumanCheck)
        )
    );
    assert!(
        attempt.routes.starts_with("by hand: "),
        "{}",
        attempt.routes
    );
    assert!(binding.check_stop.is_some(), "the check's stop");
    let later = now + PERSON_BACKOFF_SECS + 60 * 60;
    assert!(w3_held(&desk, &bench, later), "seven hours on");
    assert_eq!(runs(), "window=1\n", "no child ran again");
}

/// Records on `desk` that a bot protection, `word`, stopped `w3`'s sign-in at `at`, as the
/// upkeep's own run records it.
#[cfg(unix)]
fn check_stopped(desk: &Desk, word: crate::harness::open_url::MetWord, at: i64) {
    check_stopped_row(desk, "w3", word, at);
}

/// Records on `desk` that a bot protection, `word`, stopped the row `label`'s sign-in at `at`,
/// as the upkeep's own run records it.
#[cfg(unix)]
fn check_stopped_row(desk: &Desk, label: &str, word: crate::harness::open_url::MetWord, at: i64) {
    desk.store()
        .transact_pool_logins_within(&[], &desk.roster(), at, &mut None, |raw, _, logins| {
            accounts::admit_workflow(logins, label)?;
            Ok(accounts::note_sign_in_stop(
                logins,
                raw.row(label).expect("the row"),
                false,
                word,
                "mailbox: it met Cloudflare's bot check",
                at,
            ))
        })
        .unwrap();
}

/// The roster of `desk` with `w3` moved from preferred to pool: a new sign-in workflow for the
/// same account.
#[cfg(unix)]
fn pool_w3(desk: &Desk) {
    let text = fs::read_to_string(desk.roster()).unwrap();
    let records = desk
        .store()
        .read_account_records(Some(&desk.roster()), NOW)
        .unwrap();
    let w3 = records.raw_roster.row("w3").unwrap();
    let line = accounts::row_text(w3).unwrap();
    assert!(text.contains(&line), "{text}");
    let pooled = accounts::Row {
        class: Class::Pool,
        ..w3.clone()
    };
    fs::write(
        desk.roster(),
        text.replace(&line, &accounts::row_text(&pooled).unwrap()),
    )
    .unwrap();
}

/// Records on `desk` that the ladder found `w3`'s token in use here at `at` ([`note_attempt`]
/// with [`accounts::AttemptOutcome::OnToken`]), over whatever attempt stood, under the row's
/// admitted workflow as the ladder's own pass holds it.
#[cfg(unix)]
fn on_token_noted(desk: &Desk, at: i64) {
    desk.store()
        .transact_pool_logins_within(&[], &desk.roster(), at, &mut None, |_, _, logins| {
            accounts::admit_workflow(logins, "w3")?;
            Ok(Vec::new())
        })
        .unwrap();
    let records = desk
        .store()
        .read_account_records(Some(&desk.roster()), at)
        .unwrap();
    let row = records.roster.row("w3").unwrap().clone();
    assert!(note_attempt(
        &desk.store(),
        (&desk.roster(), &row, &records.logins),
        (accounts::AttemptOutcome::OnToken, None),
        "vault",
        at,
    ));
}

/// Whether `w3`'s record on `desk` at `at` leaves its next sign-in to a person
/// ([`left_to_a_person`]).
#[cfg(unix)]
fn check_stands(desk: &Desk, at: i64) -> bool {
    let store = desk.store();
    let records = store
        .read_account_records(Some(&desk.roster()), at)
        .unwrap();
    left_to_a_person(
        &store,
        records.raw_roster.row("w3").unwrap(),
        &records.logins,
    )
}

/// A CHECK'S STOP OUTLASTS WHAT ITS ATTEMPT DOES NOT: the row's run on its token (an `on-token`
/// attempt written over it), a later stop no bot protection made (a person closed the window in
/// front of the check), and a change of the row's policy that keeps its account (the lab moving
/// it from preferred to pool) each leave it standing, so the ladder seven hours on is held, in
/// this process and a restarted one, and an agent's `accounts renew` starts nothing, the row's
/// token in use here or not. CONTROL: each of those records alone holds nothing seven hours on;
/// with the token in use, the ladder's vault route alone goes on, and no route that loads a page;
/// a row that names another account is not held by the first account's check; and an earlier
/// build's `blocked` record that names no page becomes the account's check when a run on the
/// token is written over it.
#[cfg(unix)]
#[test]
fn a_checks_stop_outlasts_what_its_attempt_does_not() {
    use crate::harness::open_url::MetWord;
    let later = NOW + PERSON_BACKOFF_SECS + 60 * 60;

    // A run on the row's token is written over the check's attempt.
    let desk = desk_with_a_mail_folder();
    check_stopped(&desk, MetWord::BotCheck, NOW);
    on_token_noted(&desk, NOW + 60);
    assert_eq!(
        attempt_of(&desk, "w3").map(|attempt| attempt.outcome),
        Some(accounts::AttemptOutcome::OnToken)
    );
    assert!(
        check_stands(&desk, later),
        "the token ended: the check stands"
    );
    let bench = desk_upkeep();
    assert!(w3_held(&desk, &bench, later), "on the token's end");
    let raw = desk
        .store()
        .read_account_records(Some(&desk.roster()), later)
        .unwrap()
        .raw_roster
        .row("w3")
        .unwrap()
        .clone();
    let other = accounts::Row {
        email: Some("another.m30@example.test".to_owned()),
        ..raw
    };
    assert_eq!(
        accounts::check_stopped_at(&other, &desk.store().load_logins(later).unwrap().0),
        None,
        "CONTROL: another account is not held by this one's check"
    );

    // The stored reading names the row's token in use here.
    let mut reading = std::collections::BTreeMap::new();
    reading.insert("w3".to_owned(), u64::try_from(later + 86_400).unwrap());
    desk.store()
        .note_tokens_here(Some(&reading), later)
        .unwrap();
    assert!(
        check_stands(&desk, later),
        "a token in use lifts nothing at a door that loads a page"
    );
    let (code, started, said) = renew_w3(&desk, (true, MetWord::BotCheck), false, later);
    assert!(started.is_empty(), "{said}");
    assert_eq!(code, ExitCode::from(1), "{said}");
    assert!(
        said.contains(&super::super::cli::left_words(
            Cli::Claude,
            &w3_named(&desk)
        )),
        "{said}"
    );
    let bench = desk_upkeep();
    assert!(
        matches!(
            w3_route(&desk, &bench, later),
            LadderStep::Held | LadderStep::OnToken
        ),
        "CONTROL: the vault route alone"
    );
    assert!(!bench.pass(&desk, later + 60));
    assert!(bench.handed().is_empty());
    assert!(
        check_stands(&desk, later + 60),
        "and the check still stands"
    );

    // A person closes the window in front of the check.
    let desk = desk_with_a_mail_folder();
    check_stopped(&desk, MetWord::BotCheck, NOW);
    let (_, started, said) = renew_w3(&desk, (false, MetWord::WindowClosed), false, NOW + 60);
    assert_eq!(started.len(), 1, "a person's renewal runs: {said}");
    assert_eq!(
        attempt_of(&desk, "w3").and_then(|attempt| attempt.stop),
        Some(MetWord::WindowClosed)
    );
    assert!(
        check_stands(&desk, later),
        "a closed window passes no check"
    );
    assert!(w3_held(&desk, &desk_upkeep(), later), "seven hours on");

    // The lab moves the row from preferred to pool: a new workflow, the same account.
    let desk = desk_with_a_mail_folder();
    check_stopped(&desk, MetWord::BlockPage, NOW);
    pool_w3(&desk);
    let records = desk
        .store()
        .read_account_records(Some(&desk.roster()), later)
        .unwrap();
    assert_eq!(
        accounts::current_sign_in_attempt(records.raw_roster.row("w3").unwrap(), &records.logins),
        None,
        "the attempt was the old workflow's"
    );
    assert!(check_stands(&desk, later), "the account's check stands");
    assert!(w3_held(&desk, &desk_upkeep(), later), "seven hours on");

    // CONTROL: each record alone holds nothing seven hours on.
    let desk = desk_with_a_mail_folder();
    on_token_noted(&desk, NOW + 60);
    assert!(w3_goes(&desk, later), "CONTROL: a run on the token alone");
    let desk = desk_with_a_mail_folder();
    let (_, started, said) = renew_w3(&desk, (false, MetWord::WindowClosed), false, NOW + 60);
    assert_eq!(started.len(), 1, "{said}");
    assert!(w3_goes(&desk, later), "CONTROL: a closed window alone");
    let desk = desk_with_a_mail_folder();
    pool_w3(&desk);
    assert!(w3_goes(&desk, later), "CONTROL: the move to pool alone");

    // An earlier build's record: `blocked`, with no page named and no check kept beside it.
    let desk = desk_with_a_mail_folder();
    desk.store()
        .transact_pool_logins_within(&[], &desk.roster(), NOW, &mut None, |_, _, logins| {
            accounts::admit_workflow(logins, "w3")?;
            Ok(accounts::note_sign_in_attempt(
                logins,
                "w3",
                accounts::AttemptOutcome::Blocked,
                "mailbox: it met Cloudflare's bot check",
                NOW,
            ))
        })
        .unwrap();
    on_token_noted(&desk, NOW + 60);
    assert_eq!(
        desk.store()
            .load_logins(NOW + 60)
            .unwrap()
            .0
            .get("w3")
            .and_then(|binding| binding.check_stop.clone())
            .map(|check| check.at),
        Some(NOW),
        "CONTROL: the earlier build's hold is kept as the account's check"
    );
    assert!(check_stands(&desk, later));
}

/// A CHECK WHOSE RUN COULD NOT RECORD IT IS HELD ANYWAY: the account's check is written under no
/// fence but the account's own, where the run's own record was refused; and where the records
/// cannot be written at all, this process keeps it and holds the row until a ladder pass has
/// written it. CONTROL: a check of a row that a login was made for since its run started, or
/// that names another account now, is no longer the row's to hold.
#[cfg(unix)]
#[test]
fn a_check_its_run_could_not_record_is_held_anyway() {
    let desk = desk_with_a_mail_folder();
    let store = desk.store();
    let records = store
        .read_account_records(Some(&desk.roster()), NOW)
        .unwrap();
    let row = records.roster.row("w3").unwrap().clone();
    let generation = records.logins.get("w3").map(|b| b.renewal_generation);
    let check = |roster_path: PathBuf, generation: Option<u64>| PendingCheck {
        roster_path,
        row: row.clone(),
        generation,
        at: NOW,
    };
    let unwritable = desk.home.join("no-such-dir").join("accounts.toml");
    let bench = desk_upkeep();
    hold_for_a_person(
        &bench.guard,
        &store,
        "w3".to_owned(),
        check(unwritable.clone(), generation),
    );
    assert!(
        bench
            .guard
            .lock()
            .unwrap()
            .unrecorded_checks
            .contains_key("w3"),
        "kept by this process"
    );
    let later = NOW + PERSON_BACKOFF_SECS + 60 * 60;
    let fx = bench.fx();
    let ctx = LadderCtx::of(&fx, &desk.roster());
    assert!(matches!(
        ladder_route(&desk.home, &store, &row, later, &bench.guard, &ctx),
        LadderStep::Held
    ));
    assert!(
        bench
            .guard
            .lock()
            .unwrap()
            .unrecorded_checks
            .contains_key("w3"),
        "still unwritten, still kept"
    );
    bench
        .guard
        .lock()
        .unwrap()
        .unrecorded_checks
        .insert("w3".to_owned(), check(desk.roster(), generation));
    assert!(matches!(
        ladder_route(&desk.home, &store, &row, later, &bench.guard, &ctx),
        LadderStep::Held
    ));
    assert!(
        bench.guard.lock().unwrap().unrecorded_checks.is_empty(),
        "written by the pass"
    );
    let raw = records.raw_roster.row("w3").unwrap();
    assert!(left_to_a_person(
        &store,
        raw,
        &store.load_logins(later).unwrap().0
    ));
    assert!(w3_held(&desk, &desk_upkeep(), later), "and held on record");

    {
        // A login made for the row after its run started: the check is no longer its to hold.
        // Any other write since, which moves the row's generation too, leaves it standing (the
        // run's own admission that made the binding, `cli_pool_tests::
        // a_check_met_by_a_renewal_whose_record_did_not_land_is_recorded_alone`).
        let desk = desk_with_a_mail_folder();
        let store = desk.store();
        let login = desk.home.join("w3-login-made-since");
        store
            .transact_logins(NOW + 1, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "w3",
                    row.dir.as_deref(),
                    login.to_str().unwrap(),
                    None,
                    NOW + 1,
                ))
            })
            .unwrap();
        assert_eq!(
            write_check_stop(&store, &check(desk.roster(), generation)),
            CheckWrite::Moot,
            "CONTROL: a login was made since"
        );
    }
    let desk = desk_with_a_mail_folder();
    let store = desk.store();
    let other = PendingCheck {
        row: accounts::Row {
            email: Some("another.m30@example.test".to_owned()),
            ..row.clone()
        },
        ..check(desk.roster(), generation)
    };
    assert_eq!(
        write_check_stop(&store, &other),
        CheckWrite::Moot,
        "CONTROL: the row names another account"
    );
    assert_eq!(
        store
            .load_logins(NOW)
            .unwrap()
            .0
            .get("w3")
            .and_then(|b| b.check_stop.clone()),
        None
    );
    assert!(w3_goes(&desk, later), "CONTROL: a moot check holds nothing");
}

/// AN AGENT'S `--due` SWEEP PROMISES A ROW LEFT TO A PERSON NO UPKEEP RENEWAL: inside another
/// account's sign-in spacing, the row a check stopped is named with the person's step, before any
/// spacing is judged or any sign-in lock is waited for, never as one a window's upkeep renews
/// later.
#[cfg(unix)]
#[test]
fn an_agents_due_sweep_names_the_persons_step_for_a_row_left_to_a_person() {
    use crate::harness::open_url::MetWord;
    let desk = desk_with_a_mail_folder();
    check_stopped(&desk, MetWord::BotCheck, NOW);
    let store = desk.store();
    fs::create_dir_all(store.shared_dir()).unwrap();
    fs::write(
        store
            .shared_dir()
            .join(crate::harness::sign_in_ladder::SIGNIN_SPACING_FILE),
        format!("aterm-signin-spacing-v1\nclaude main {}\n", NOW + 30),
    )
    .unwrap();
    let (_, started, said) = renew_w3(&desk, (true, MetWord::BotCheck), true, NOW + 60);
    assert!(started.is_empty(), "{said}");
    assert!(
        said.contains(&super::super::cli::left_words(
            Cli::Claude,
            &w3_named(&desk)
        )),
        "the person's step: {said}"
    );
    assert!(
        !said.contains(&format!("{}: not run yet", w3_named(&desk).shown)),
        "{said}"
    );
}

/// Runs `accounts token claude <w3's address>` in `mode` at `at` on `desk` as [`RenewAtTheDesk`]
/// does, at this Mac's desk, with no vault and no `claude` to run, under a test key for the lab's
/// journal: its exit and what it said.
#[cfg(unix)]
fn token_w3(
    desk: &Desk,
    agent: bool,
    mode: super::super::cli::TokenMode,
    at: i64,
) -> (ExitCode, String) {
    let host = RenewAtTheDesk {
        agent,
        now: at,
        met: crate::harness::open_url::MetWord::BotCheck,
        started: std::cell::RefCell::default(),
    };
    let env = quiet_env(&desk.home, &desk.state, &config_beside(&desk.roster()), at);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    // The lab's journal keys the account as the token verb's own tests key it.
    let code =
        crate::harness::journal::with_keyer(crate::harness::accounts_tokens::tests::keyer, || {
            super::super::cli::run_accounts(
                &super::super::cli::AccountsCmd::Token {
                    // The verb names the row's account by its address, never its label.
                    name: "claude w3.m30.3@example.test".to_owned(),
                    mode,
                },
                &env,
                &host,
                &mut out,
                &mut err,
            )
        });
    assert!(
        host.started.into_inner().is_empty(),
        "a token run renews nothing"
    );
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    (code, said)
}

/// AN AGENT'S `accounts token` OPENS NO SIGN-IN PAGE FOR A ROW LEFT TO A PERSON: a token is made
/// on claude.ai's own page, in aterm's sign-in window, which a check that stands meets again, so
/// for a row a bot protection stopped an agent's run, making or replacing, refuses before any
/// vault or `claude` is asked for and names the person's step. CONTROL: a person at this
/// terminal, an agent's removal, which loads no page, and an agent's run for a row no check
/// stopped each go past it.
#[cfg(unix)]
#[test]
fn an_agents_token_run_opens_nothing_for_a_row_left_to_a_person() {
    use super::super::cli::TokenMode;
    let desk = desk_with_a_mail_folder();
    let refused = format!(
        "aterm harness accounts token: {}",
        super::super::cli::left_words(Cli::Claude, &w3_named(&desk))
    );
    check_stopped(&desk, crate::harness::open_url::MetWord::BotCheck, NOW);
    for mode in [TokenMode::Make, TokenMode::Replace] {
        let (code, said) = token_w3(&desk, true, mode, NOW + 60);
        assert_eq!(code, ExitCode::from(1), "{said}");
        assert!(said.contains(&refused), "{said}");
    }
    let (_, said) = token_w3(&desk, false, TokenMode::Make, NOW + 60);
    assert!(!said.contains(&refused), "CONTROL: a person's run: {said}");
    let (_, said) = token_w3(&desk, true, TokenMode::Remove, NOW + 60);
    assert!(!said.contains(&refused), "CONTROL: a removal: {said}");
    let desk = desk_with_a_mail_folder();
    let (_, said) = token_w3(&desk, true, TokenMode::Make, NOW + 60);
    assert!(!said.contains(&refused), "CONTROL: no check stands: {said}");
}

/// The log's line for a token step after a sign-in holds the row's label and no address (the
/// token verb names the account to a person, and the upkeep's log is read by nobody who asked),
/// and says where the token was made: in the sign-in window, or on the row's headless profile.
/// NEGATIVE CONTROL: with the line naming the window whatever the sign-in was, the headless
/// step reads `made in the sign-in window`.
#[cfg(unix)]
#[test]
fn the_token_steps_log_line_holds_no_address() {
    use crate::harness::accounts_tokens::SignedIn;
    assert_eq!(
        token_step_line("w3", SignedIn::Window, "  \n"),
        "w3: token: made in the sign-in window"
    );
    assert_eq!(
        token_step_line("w3", SignedIn::Headless, ""),
        "w3: token: made on its headless browser profile"
    );
    let line = token_step_line(
        "w3",
        SignedIn::Headless,
        "aterm harness accounts token: a token for w3.m30.3@example.test is being made in \
         another tab\n",
    );
    assert_eq!(
        line,
        "w3: token: aterm harness accounts token: a token for <address> is being made in \
         another tab"
    );
}

/// The upkeep's log line for a capture it ran (plan package FL-138) is judged by the verb's exit:
/// a token made says where, with any warning the verb wrote after it, and every address in that
/// warning written `<address>`; a refusal says what the verb refused, and one with nothing to
/// say that nothing was made.
/// NEGATIVE CONTROL: with the line judged by whether the verb wrote to its error stream, the
/// made token with a warning reads as the warning alone: `` w3: token (none): aterm harness
/// accounts token: tokens-tried.json ``.
#[cfg(unix)]
#[test]
fn the_token_upkeeps_log_line_is_judged_by_the_verbs_exit() {
    use crate::harness::accounts_tokens::{Finding, Gap, Via};
    let finding = Finding {
        key: "ac-0000000000000001".to_owned(),
        gap: Gap::None,
        minted_at: None,
        via: Via::Unattended,
    };
    let warned = "aterm harness accounts token: tokens-tried.json: the lock stayed held for \
                  w3.m30.3@example.test\n";
    let line = backfill_line("w3", &finding, ExitCode::SUCCESS, warned);
    assert!(
        line.starts_with("w3: token (none): made in the sign-in window with nobody there; "),
        "{line}"
    );
    assert!(line.contains("<address>") && !line.contains('@'), "{line}");
    assert_eq!(
        backfill_line("w3", &finding, ExitCode::SUCCESS, " \n"),
        "w3: token (none): made in the sign-in window with nobody there"
    );
    let headless = Finding {
        gap: Gap::Soon,
        via: Via::Headless,
        ..finding.clone()
    };
    assert_eq!(
        backfill_line("w3", &headless, ExitCode::SUCCESS, ""),
        "w3: token (ends soon): made on its headless browser profile"
    );
    let refused = backfill_line(
        "w3",
        &finding,
        ExitCode::from(1),
        "aterm harness accounts token: a human check held the consent page for \
         w3.m30.3@example.test\n",
    );
    assert_eq!(
        refused,
        "w3: token (none): aterm harness accounts token: a human check held the consent page \
         for <address>"
    );
    assert_eq!(
        backfill_line("w3", &finding, ExitCode::from(1), ""),
        "w3: token (none): not made"
    );
}

// THE TOKEN UPKEEP (plan package FL-138): `upkeep_tokens` end to end through the upkeep's pass,
// with `claude setup-token` stood in (`setup_token::tests::stand_in`), the vault stood in by a
// keyed fake (`FleetVault`) and this Mac's vault refresh scripted (`tier_sync`).

/// The window route's account, as the stand-in signs it in.
#[cfg(target_os = "macos")]
const WORK: &str = "person@example.com";

/// A second account the stand-in signs in as.
#[cfg(target_os = "macos")]
const OTHER: &str = crate::harness::setup_token::tests::OTHER_EMAIL;

#[cfg(target_os = "macos")]
const WORK_KEY: &str = "ac-0000000000000001";

#[cfg(target_os = "macos")]
const OTHER_KEY: &str = "ac-0000000000000002";

/// This Mac's platform id, as the tier's reads give it.
#[cfg(target_os = "macos")]
const MACHINE: &str = "00000000-0000-4000-8000-000000000030";

/// The upkeep's clock in these tests: the token fixtures' (2027-01-15).
#[cfg(target_os = "macos")]
const T0: i64 = 1_800_000_000;

#[cfg(target_os = "macos")]
fn tier_keyer(_realm: &str, email: &str) -> Option<String> {
    match email {
        WORK => Some(WORK_KEY.to_owned()),
        OTHER => Some(OTHER_KEY.to_owned()),
        _ => None,
    }
}

/// The vault, stood in for, by account key: its route, the standings it holds, the status of
/// each refusal this Mac recorded, and every write it took.
#[cfg(target_os = "macos")]
#[derive(Clone)]
struct FleetVault {
    route: std::rc::Rc<std::cell::Cell<aterm_secrets::client::WriteRoute>>,
    held: std::rc::Rc<std::cell::RefCell<BTreeMap<String, aterm_secrets::claude_token::Standing>>>,
    refused: std::rc::Rc<std::cell::RefCell<BTreeMap<String, u16>>>,
    writes: std::rc::Rc<std::cell::RefCell<Vec<(String, aterm_secrets::claude_token::Meta)>>>,
}

#[cfg(target_os = "macos")]
impl FleetVault {
    fn seated() -> Self {
        Self {
            route: std::rc::Rc::new(std::cell::Cell::new(
                aterm_secrets::client::WriteRoute::Seated,
            )),
            held: std::rc::Rc::default(),
            refused: std::rc::Rc::default(),
            writes: std::rc::Rc::default(),
        }
    }

    fn writes(&self) -> Vec<(String, aterm_secrets::claude_token::Meta)> {
        self.writes.borrow().clone()
    }

    fn hold(&self, standing: aterm_secrets::claude_token::Standing) {
        self.held
            .borrow_mut()
            .insert(standing.key.as_str().to_owned(), standing);
    }
}

#[cfg(target_os = "macos")]
impl crate::harness::accounts_tokens::TokenVault for FleetVault {
    fn route(&self, _by: Instant) -> Result<aterm_secrets::client::WriteRoute, String> {
        Ok(self.route.get())
    }
    fn standing(
        &self,
        key: &aterm_secrets::claude_token::Key,
        _by: Instant,
    ) -> Result<aterm_secrets::claude_token::Lookup<aterm_secrets::claude_token::Standing>, String>
    {
        use aterm_secrets::claude_token::Lookup;
        Ok(self
            .held
            .borrow()
            .get(key.as_str())
            .cloned()
            .map_or(Lookup::Absent, Lookup::Found))
    }
    fn listed(
        &self,
    ) -> Result<
        aterm_secrets::claude_token::Lookup<Vec<aterm_secrets::claude_token::Standing>>,
        String,
    > {
        Ok(aterm_secrets::claude_token::Lookup::Found(
            self.held.borrow().values().cloned().collect(),
        ))
    }
    fn waiting_here(&self, _key: &aterm_secrets::claude_token::Key) -> Result<bool, String> {
        Ok(false)
    }
    fn seat_here(&self) -> Result<Option<String>, String> {
        Ok(Some("m30".to_owned()))
    }
    fn write(
        &self,
        key: &aterm_secrets::claude_token::Key,
        token: &aterm_secrets::SecretBytes,
        meta: &aterm_secrets::claude_token::Meta,
        _on_busy: aterm_secrets::client::OnBusy,
    ) -> Result<aterm_secrets::client::MemberWritten, String> {
        assert_eq!(
            token.as_slice(),
            crate::harness::setup_token::tests::token().as_bytes(),
            "the token made"
        );
        self.writes
            .borrow_mut()
            .push((key.as_str().to_owned(), meta.clone()));
        self.refused.borrow_mut().remove(key.as_str());
        self.hold(aterm_secrets::claude_token::Standing {
            key: key.clone(),
            meta: Some(meta.clone()),
            check: aterm_secrets::claude_token::Check::Readable,
            signer: Some("m30".to_owned()),
            minted_here: true,
            exposed: Vec::new(),
            refused_at: None,
        });
        Ok(aterm_secrets::client::MemberWritten::Posted)
    }
    fn remove(
        &self,
        _key: &aterm_secrets::claude_token::Key,
        _by: Instant,
    ) -> Result<aterm_secrets::client::MemberUnset, String> {
        panic!("the upkeep removes no token")
    }
    fn verify(
        &self,
        _key: &aterm_secrets::claude_token::Key,
        _minted_at: u64,
        _evidence: aterm_secrets::claude_token::Verified,
        _on_busy: aterm_secrets::client::OnBusy,
    ) -> Result<aterm_secrets::client::MemberWritten, String> {
        Ok(aterm_secrets::client::MemberWritten::Refused(
            "the fake vault verifies nothing".to_owned(),
        ))
    }
    fn refused_status(&self, standing: &aterm_secrets::claude_token::Standing) -> Option<u16> {
        self.refused.borrow().get(standing.key.as_str()).copied()
    }
}

#[cfg(target_os = "macos")]
thread_local! {
    /// The vault and the stand-in's environment the tier's host on this thread runs on
    /// ([`tier_host`]): the upkeep runs its capture on this thread ([`inline`]).
    static TIER: std::cell::RefCell<Option<(FleetVault, Vec<(OsString, OsString)>)>> =
        const { std::cell::RefCell::new(None) };
    /// How many vault refreshes this thread's upkeep ran ([`tier_sync`]).
    static TIER_SYNCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(target_os = "macos")]
use std::ffi::OsString;

/// The tier's vault refresh: it read the whole vault.
#[cfg(target_os = "macos")]
fn tier_sync(_home: &Path, _scope: Option<&crate::harness::account_work::Scope>) -> Vault {
    TIER_SYNCS.with(|syncs| syncs.set(syncs.get() + 1));
    Vault::Applied(1)
}

/// The accounts host a capture the upkeep starts runs on: this thread's [`TIER`].
#[cfg(target_os = "macos")]
fn tier_host(
    _scope: Option<crate::harness::account_work::Scope>,
) -> Box<dyn crate::harness::cli::AccountsHost> {
    let (vault, env) = TIER.with(|tier| tier.borrow().clone()).expect("a tier");
    Box::new(TierHost { vault, env })
}

#[cfg(target_os = "macos")]
struct TierHost {
    vault: FleetVault,
    env: Vec<(OsString, OsString)>,
}

#[cfg(target_os = "macos")]
impl crate::harness::cli::AccountsHost for TierHost {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, login: &str) -> Result<crate::harness::align::Captured, String> {
        panic!("the token upkeep runs no `claude auth status` (under {login:?})")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _dir: Option<&str>) -> Result<i32, String> {
        panic!("the token upkeep runs no `claude auth login`")
    }
    fn now(&self) -> i64 {
        0
    }
    fn on_macos(&self) -> bool {
        true
    }
    fn opens_sign_in_window(&self) -> bool {
        true
    }
    fn current_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }
    fn verb_env(&self) -> Vec<(OsString, OsString)> {
        self.env.clone()
    }
    fn token_vault(&self) -> Option<Box<dyn crate::harness::accounts_tokens::TokenVault>> {
        Some(Box::new(self.vault.clone()))
    }
    fn setup_token_program(&self) -> Option<(crate::harness::setup_token::Program, String)> {
        Some((
            crate::harness::setup_token::tests::World::program(),
            "2.1.318".to_owned(),
        ))
    }
    fn token_hang(&self) -> Duration {
        aterm_spec::wait::MUST_HAPPEN
    }
    fn session_logins(&self, _env: &Env) -> Vec<crate::harness::accounts_tokens::SessionLogin> {
        Vec::new()
    }
}

/// One Mac's token upkeep: its desk, its rows (each signed in here, with its window store
/// holding the account's session), the stand-in's world, the vault, and the upkeep's bench.
#[cfg(target_os = "macos")]
struct Tier {
    desk: Desk,
    world: crate::harness::setup_token::tests::World,
    vault: FleetVault,
    bench: Bench,
}

#[cfg(target_os = "macos")]
impl Tier {
    /// A Mac whose rows are `rows` (label, email), whose stand-in plays `mode`.
    fn new(mode: &str, rows: &[(&str, &str)]) -> Self {
        TIER_SYNCS.with(|syncs| syncs.set(0));
        Self::on("m30", mode, rows, FleetVault::seated(), Self::bench())
    }

    /// The Mac `machine` of a fleet whose vault is `vault`, its rows `rows` (label, email), its
    /// stand-in playing `mode`, its upkeep's bench `bench`.
    fn on(
        machine: &str,
        mode: &str,
        rows: &[(&str, &str)],
        vault: FleetVault,
        bench: Bench,
    ) -> Self {
        let desk = Desk::new(machine);
        let store = desk.store();
        let mut text = String::new();
        for (label, email) in rows {
            let dir = store.logins_dir().join(label);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("settings.json"), "{}").unwrap();
            text += &(accounts::row_text(&accounts::Row {
                label: (*label).to_owned(),
                class: Class::Preferred,
                dir: Some(dir.to_string_lossy().into_owned()),
                email: Some((*email).to_owned()),
                ..accounts::Row::default()
            })
            .unwrap()
                + "\n");
            let marker = crate::harness::signin_window::store_marker(&store.dir, email);
            fs::create_dir_all(marker.parent().unwrap()).unwrap();
            fs::write(&marker, email).unwrap();
        }
        fs::write(desk.roster(), text).unwrap();
        let source = desk.home.join(".config/aterm/secrets/source.toml");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(source, "# vault A\n").unwrap();
        let world = crate::harness::setup_token::tests::World::new(mode, false);
        fs::write(world.home().join("go"), "").unwrap();
        Self {
            desk,
            world,
            vault,
            bench,
        }
    }

    /// The upkeep's bench as a new process has it, with nothing remembered, on the machine
    /// whose platform id is [`MACHINE`].
    fn bench() -> Bench {
        Self::bench_on(|| Some(MACHINE.to_owned()))
    }

    /// [`Self::bench`] on the machine whose platform id `machine` reads.
    fn bench_on(machine: fn() -> Option<String>) -> Bench {
        Bench {
            guard: guard_reading(MachineReads {
                desk: true,
                graphic: || true,
                machine,
                tokens: Some(tier_host),
                ..MachineReads::default()
            }),
            vault_sync: tier_sync,
            ..Bench::ending(Renewal::Failed)
        }
    }

    /// One upkeep pass at `at`, its captures on this Mac's vault and stand-in.
    fn pass(&self, at: i64) {
        TIER.with(|tier| *tier.borrow_mut() = Some((self.vault.clone(), self.world.env(&[]))));
        crate::harness::journal::with_keyer(tier_keyer, || self.bench.pass(&self.desk, at));
    }

    /// This Mac in an account pool (its `swarm_label`), the home Mac (`warm[0]`) of the row
    /// `label` where `home`, else one of its other warm Macs.
    fn in_pool(&self, label: &str, home: bool) {
        self.desk
            .store()
            .transact(T0 - 60, &mut None, |state| {
                state.swarm_label = Some("m30".to_owned());
                if home {
                    state.home_accounts.insert(label.to_owned());
                } else {
                    state.home_accounts.remove(label);
                }
                Ok(Vec::new())
            })
            .unwrap();
    }

    /// Ends the ladder's wait for its next vault refresh, which runs on the real clock, as the
    /// minutes between two passes the fixture's clock skips would: the next pass that wants a
    /// fresh read of the vault starts one.
    fn refresh_due(&self) {
        self.bench
            .guard
            .lock()
            .unwrap()
            .ladder
            .moved(Instant::now());
    }

    /// The passes at `at` and at the end of the account `key`'s wait from it, each after the
    /// ladder's refresh wait ended ([`Self::refresh_due`]).
    fn found_and_ripe(&self, key: &str, at: i64) {
        self.refresh_due();
        self.pass(at);
        self.refresh_due();
        self.pass(at + token_delay_s(key, MACHINE));
    }

    /// The gaps this Mac's upkeep ran a capture for, on record.
    fn tried(&self) -> BTreeMap<String, crate::harness::token_steps::Tried> {
        crate::harness::token_steps::read_tried(&self.desk.store()).expect("the record reads")
    }

    /// The passes at the account `key`'s gap's finding at `T0` and at its wait's end: the time
    /// its capture may start.
    fn ripen(&self, key: &str) -> i64 {
        self.pass(T0);
        let ripe = T0 + token_delay_s(key, MACHINE);
        self.pass(ripe);
        ripe
    }

    fn syncs() -> usize {
        TIER_SYNCS.with(std::cell::Cell::get)
    }

    /// The account `key`'s step record, where a stop left one.
    fn stopped(&self, key: &str) -> Option<crate::harness::token_steps::Stopped> {
        crate::harness::token_steps::read(&self.desk.store())
            .expect("the steps file reads")
            .get(key)
            .cloned()
    }
}

/// Another Mac's token of the account `key` (`email`): made in m7's sign-in window a day ago,
/// verified there, readable here, ending in a year.
#[cfg(target_os = "macos")]
fn fleet_token(key: &str, email: &str) -> aterm_secrets::claude_token::Standing {
    use aterm_secrets::claude_token::{Check, ExpiryFrom, Key, Meta, MintedBy, Standing, Verified};
    let now = u64::try_from(T0).unwrap();
    Standing {
        key: Key::parse(key).unwrap(),
        meta: Some(Meta {
            email: email.to_owned(),
            account_id: "c-0123456789ab".to_owned(),
            minted_at: now - 86_400,
            expires_at: now + 365 * 86_400,
            expiry_from: ExpiryFrom::Printed,
            minted_on: "m7".to_owned(),
            cli: "2.1.318".to_owned(),
            minted_by: Some(MintedBy::Window),
            verified: Some(Verified {
                on: "m7".to_owned(),
                at: now - 3_600,
                by: aterm_secrets::claude_token::BY_WINDOWS.to_owned(),
                seven_day_resets_at: None,
                five_hour_resets_at: None,
            }),
        }),
        check: Check::Readable,
        signer: Some("m7".to_owned()),
        minted_here: false,
        exposed: Vec::new(),
        refused_at: None,
    }
}

/// A SIGNED-IN ROW WITH NO FLEET TOKEN GETS ONE WITH NOBODY THERE, ONCE, AND A CHECK HOLDS IT
/// (plan package FL-138; sitting design §6 S4): a row signed in here whose window store holds
/// its session, with no token in the vault, waits its account's delay (under an hour, fixed by
/// the account and the machine), then a vault refresh runs first and the token is made in aterm's
/// window with nobody there (the window told so, `ATERM_OPEN_URL_UNATTENDED`): one vault write,
/// whose meta says the window minted it (`minted_by = window`, which the fleet serves), and one
/// log line with no address. The next tick, an hour later and a day later, make none. On a Mac
/// whose capture meets the consent page's human check, nothing is written, one step record is
/// left beside the account's check, and a new process 25 hours later runs nothing, held by that
/// record alone once a login made for the row has ended the account's check and the upkeep's
/// record of the gap's run is gone (as a new Claude Code build lets it through).
/// NEGATIVE CONTROL: with `step_held` reading no record (`steps.get(..)` as `None`), the new
/// process runs the stopped capture again: `` the held step ran again ``.
#[cfg(target_os = "macos")]
#[test]
fn a_signed_in_row_with_no_fleet_token_gets_one_with_nobody_there_once_and_a_check_holds_it() {
    use aterm_secrets::claude_token::MintedBy;
    let tier = Tier::new("renew-window-token", &[("work", WORK)]);
    let ripe = T0 + token_delay_s(WORK_KEY, MACHINE);
    assert!(
        ripe > T0 + 60,
        "the fixture's delay is a wait: {}",
        ripe - T0
    );
    tier.pass(T0);
    tier.pass(ripe - 1);
    assert_eq!(
        tier.world.runs(),
        0,
        "a capture ran before its wait was over"
    );
    assert_eq!(
        Tier::syncs(),
        0,
        "the vault was refreshed before the wait was over"
    );
    assert!(tier.vault.writes().is_empty());

    tier.pass(ripe);
    assert_eq!(
        Tier::syncs(),
        1,
        "the vault is refreshed once, before the capture"
    );
    assert_eq!(tier.world.runs(), 1);
    let writes = tier.vault.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    let (key, meta) = &writes[0];
    assert_eq!(key, WORK_KEY);
    assert_eq!(meta.email, WORK);
    assert_eq!(meta.minted_on, "m30");
    assert_eq!(
        meta.minted_by,
        Some(MintedBy::Window),
        "a token aterm's window made proves its account, so every Mac serves it"
    );
    let env = fs::read_to_string(tier.world.home().join("env")).expect("the stand-in's env");
    assert!(
        env.lines()
            .any(|line| line == "ATERM_OPEN_URL_UNATTENDED=1"),
        "the window was not told nobody is there: {env}"
    );
    let logged = tier.bench.logged();
    assert!(
        logged
            .iter()
            .any(|line| line == "work: token (none): made in the sign-in window with nobody there"),
        "{logged:?}"
    );
    assert!(logged.iter().all(|line| !line.contains('@')), "{logged:?}");
    assert!(tier.stopped(WORK_KEY).is_none());

    for at in [ripe + 1, ripe + 3_600, ripe + 86_400] {
        tier.pass(at);
    }
    assert_eq!(tier.vault.writes().len(), 1, "a later tick wrote the vault");
    assert_eq!(tier.world.runs(), 1);

    // A check holds it.
    let held = Tier::new("window-human-check", &[("work", WORK)]);
    let ripe = held.ripen(WORK_KEY);
    assert_eq!(held.world.runs(), 1);
    assert!(
        held.vault.writes().is_empty(),
        "a stopped capture wrote the vault"
    );
    let stopped = held.stopped(WORK_KEY).expect("the stop is on record");
    assert_eq!(
        stopped.word,
        crate::harness::open_url::MetWord::WindowHumanCheck
    );
    let logged = held.bench.logged();
    assert!(
        logged
            .iter()
            .any(|line| line.starts_with("work: token (none): ") && line.contains("human check")),
        "{logged:?}"
    );
    for at in [ripe + 1, ripe + 3_600] {
        held.pass(at);
    }
    assert_eq!(held.world.runs(), 1);
    // The check is the account's too. A login made for the row ends that, and a new build lets
    // the upkeep's record of the gap's run through: neither holds what follows.
    let store = held.desk.store();
    let mut logins = store.load_logins(T0).expect("the logins").0;
    let binding = logins.labels.get_mut("work").expect("the row's binding");
    assert!(
        binding.check_stop.take().is_some(),
        "the account's check was not recorded"
    );
    store.save_logins(&logins).expect("the login");
    crate::harness::token_steps::put_tried(&store, WORK_KEY, None).expect("the run's record");
    // A new process, 25 hours later: only the record holds it.
    let held = Tier {
        bench: Tier::bench(),
        ..held
    };
    let later = T0 + 25 * 3_600;
    held.pass(later);
    held.pass(later + token_delay_s(WORK_KEY, MACHINE));
    held.pass(later + 3_600);
    assert_eq!(held.world.runs(), 1, "the held step ran again");
    assert_eq!(held.stopped(WORK_KEY), Some(stopped));
}

/// WHAT IS NOT THIS MAC'S TO MAKE, THE UPKEEP LEAVES ALONE (plan package FL-138), each through
/// the upkeep's passes past the account's wait: a token that does not read here, a refusal
/// this Mac recorded with no `401` (Cloudflare's `403`) or no status (an earlier build's), a
/// draft whose meta is not its token's (the vault's to settle), another Mac's window or headless
/// token in use, one ending soon, verified or not, on a Mac that is not the account's home Mac, a
/// row another Mac holds and a row signed out here: no capture runs and nothing is written.
/// CONTROL: a refusal with a `401`, a token minted here by no route (m2's two), and a year-long
/// one ending soon on its home Mac, verified or not, are each replaced once, by a token the
/// window made.
/// NEGATIVE CONTROL: with `gap_of` taking every `rejected` reading for `Gap::Refused`, the 403
/// is replaced: `` 403: ``; with `gap_of` reading `soon` off `TokenState::Soon` alone (a verified
/// token's), the unverified one is kept: `` its home Mac replaces an unverified token that ends
/// soon ``.
#[cfg(target_os = "macos")]
#[test]
fn the_token_upkeep_leaves_alone_what_is_not_this_macs_to_make() {
    use aterm_secrets::claude_token::{Check, MintedBy, Mismatch};
    let now = u64::try_from(T0).unwrap();
    let run = |case: &str, setup: &dyn Fn(&Tier)| {
        let tier = Tier::new("renew-window-token", &[("work", WORK)]);
        setup(&tier);
        let ripe = tier.ripen(WORK_KEY);
        tier.pass(ripe + 1);
        let writes = tier.vault.writes();
        assert!(writes.len() <= 1, "{case}: {writes:?}");
        assert_eq!(tier.world.runs(), writes.len(), "{case}");
        writes.into_iter().next().map(|(key, meta)| {
            assert_eq!(key, WORK_KEY, "{case}");
            assert_eq!(meta.minted_by, Some(MintedBy::Window), "{case}");
            meta
        })
    };
    let refused = |status: Option<u16>| {
        move |tier: &Tier| {
            let mut token = fleet_token(WORK_KEY, WORK);
            token.refused_at = Some(now - 60);
            tier.vault.hold(token);
            if let Some(status) = status {
                tier.vault
                    .refused
                    .borrow_mut()
                    .insert(WORK_KEY.to_owned(), status);
            }
        }
    };
    // A year-long token in its last ten days, verified, or not (`unverified_soon`).
    let soon_token = |verified: bool| {
        let mut token = fleet_token(WORK_KEY, WORK);
        let meta = token.meta.as_mut().unwrap();
        meta.minted_at = now - 355 * 86_400;
        meta.expires_at = now + 10 * 86_400;
        if !verified {
            meta.verified = None;
        }
        token
    };
    let soon = move |tier: &Tier| tier.vault.hold(soon_token(true));
    let unverified_soon = move |tier: &Tier| tier.vault.hold(soon_token(false));
    let home = |tier: &Tier| {
        tier.desk
            .store()
            .transact(T0 - 60, &mut None, |state| {
                state.home_accounts.insert("work".to_owned());
                Ok(Vec::new())
            })
            .unwrap();
    };
    let unverified_by = |by: MintedBy| {
        move |tier: &Tier| {
            let mut token = fleet_token(WORK_KEY, WORK);
            let meta = token.meta.as_mut().unwrap();
            meta.verified = None;
            meta.minted_by = Some(by);
            tier.vault.hold(token);
        }
    };
    let left_alone: [(&str, Box<dyn Fn(&Tier)>); 10] = [
        (
            "unreadable",
            Box::new(|tier: &Tier| {
                let mut token = fleet_token(WORK_KEY, WORK);
                token.check = Check::Unreadable;
                tier.vault.hold(token);
            }),
        ),
        ("403", Box::new(refused(Some(403)))),
        ("no status", Box::new(refused(None))),
        (
            "draft",
            Box::new(|tier: &Tier| {
                let mut token = fleet_token(WORK_KEY, WORK);
                token.check = Check::Mismatched(Mismatch::Draft);
                tier.vault.hold(token);
            }),
        ),
        (
            "another Mac's window token",
            Box::new(unverified_by(MintedBy::Window)),
        ),
        (
            "another Mac's headless token",
            Box::new(unverified_by(MintedBy::Headless)),
        ),
        ("ends soon, not the home Mac", Box::new(soon)),
        (
            "ends soon unverified, not the home Mac",
            Box::new(unverified_soon),
        ),
        (
            "held by m7",
            Box::new(|tier: &Tier| {
                let text = fs::read_to_string(tier.desk.roster()).unwrap();
                fs::write(tier.desk.roster(), text + "held_by = \"m7\"\n").unwrap();
            }),
        ),
        (
            "signed out here",
            Box::new(|tier: &Tier| {
                tier.desk
                    .store()
                    .transact(T0 - 60, &mut None, |state| {
                        Ok(accounts::note_auth(
                            state,
                            "work",
                            accounts::Auth::SignedOut,
                            T0 - 60,
                        ))
                    })
                    .unwrap();
            }),
        ),
    ];
    for (case, setup) in &left_alone {
        assert!(
            run(case, setup.as_ref()).is_none(),
            "{case}: a token was made"
        );
    }

    let replaced = run("401", &refused(Some(401))).expect("401: a refused token is replaced");
    assert_ne!(
        replaced.minted_at,
        now - 86_400,
        "the refused token was kept: {replaced:?}"
    );
    run("minted here by no route", &|tier: &Tier| {
        let mut token = fleet_token(WORK_KEY, WORK);
        let meta = token.meta.as_mut().unwrap();
        meta.verified = None;
        meta.minted_by = None;
        meta.minted_on = "m30".to_owned();
        token.minted_here = true;
        token.signer = Some("m30".to_owned());
        tier.vault.hold(token);
    })
    .expect("a token minted here by no route is replaced");
    run("ends soon, the home Mac", &|tier: &Tier| {
        soon(tier);
        home(tier);
    })
    .expect("its home Mac replaces a token that ends soon");
    // A token the upkeep made is not verified until a comparison verifies it, which may never
    // come: its end, not its verification, says it ends soon.
    run("ends soon unverified, the home Mac", &|tier: &Tier| {
        unverified_soon(tier);
        home(tier);
    })
    .expect("its home Mac replaces an unverified token that ends soon");
}

/// THE CODE PAGE NO MAILBOX ANSWERS ENDS A CAPTURE NOBODY STARTED, ON RECORD (plan package
/// FL-138): the window's stop on the code page is the account's step record (`code-no-mailbox`),
/// nothing is written, no need or sign-in task is posted, and the step is held. A mailbox
/// configuration written after the stop, in the same process, runs it exactly once more.
/// NEGATIVE CONTROL: with the upkeep keeping, in memory, every gap it ran as tried for the
/// process's life, the mailbox's event runs nothing: `` a mailbox written after the stop ``.
#[cfg(target_os = "macos")]
#[test]
fn a_code_page_no_mailbox_answers_holds_the_step_and_asks_nobody() {
    let tier = Tier::new("window-code-no-mailbox", &[("work", WORK)]);
    let tasks = || {
        let records = tier
            .desk
            .store()
            .read_account_records(Some(&tier.desk.roster()), T0)
            .unwrap();
        accounts::sign_in_tasks(&records.roster, &records.logins)
    };
    let before = tasks();
    let ripe = tier.ripen(WORK_KEY);
    assert_eq!(tier.world.runs(), 1);
    assert!(tier.vault.writes().is_empty());
    let stopped = tier.stopped(WORK_KEY).expect("the stop is on record");
    assert_eq!(
        stopped.word,
        crate::harness::open_url::MetWord::WindowCodeNoMailbox
    );
    assert_eq!(stopped.why(), "code-no-mailbox");
    assert_eq!(tasks(), before, "the stop posted a sign-in task");
    assert!(
        tier.bench
            .logged()
            .iter()
            .all(|line| !line.contains("NEEDS YOU")),
        "{:?}",
        tier.bench.logged()
    );
    tier.found_and_ripe(WORK_KEY, ripe + 86_400);
    assert_eq!(tier.world.runs(), 1, "the held step ran again");

    // An event the record waits for, in the same process: a mailbox configuration written after
    // the stop. The step runs exactly once more, which the code page stops again.
    let config = tier
        .desk
        .store()
        .dir
        .join(crate::harness::headless_oauth::MAILBOX_TOML_FILE);
    fs::write(&config, "").unwrap();
    fs::File::options()
        .write(true)
        .open(&config)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(stopped.at + 60))
        .unwrap();
    let event = ripe + 2 * 86_400;
    tier.found_and_ripe(WORK_KEY, event);
    assert_eq!(
        tier.world.runs(),
        2,
        "a mailbox written after the stop did not run the step once more"
    );
    for at in [event + 86_400, event + 30 * 86_400] {
        tier.found_and_ripe(WORK_KEY, at);
    }
    assert_eq!(tier.world.runs(), 2, "the event ran the step twice");
    assert!(tier.vault.writes().is_empty());
}

/// A SHORT-LIVED TOKEN IS NOT REPLACED ON ITS OWN CLOCK (plan package FL-138,
/// [`crate::harness::accounts_tokens::SHORT_LIVED_SECS`]): where Claude Code makes a token for
/// 30 days, the upkeep on the account's home Mac makes it once; its end, which the fixture's clock
/// has passed, is no gap to the Claude Code build that made it, so passes an hour, a day and a
/// month later, each past the account's wait, run no capture and write nothing.
/// NEGATIVE CONTROL: with `gap_of` taking every `ended` reading for `Gap::Ended`, the next pass
/// makes another: `` a short-lived token was replaced by the build that made it ``.
#[cfg(target_os = "macos")]
#[test]
fn a_short_lived_token_is_not_replaced_on_its_own_clock() {
    let tier = Tier::new("renew-window-30-days", &[("work", WORK)]);
    tier.in_pool("work", true);
    let ripe = tier.ripen(WORK_KEY);
    assert_eq!(tier.world.runs(), 1);
    let writes = tier.vault.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    let meta = &writes[0].1;
    assert!(
        meta.expires_at - meta.minted_at <= 30 * 86_400,
        "the stand-in's token lasts 30 days: {meta:?}"
    );
    for at in [ripe + 3_600, ripe + 86_400, ripe + 31 * 86_400] {
        tier.found_and_ripe(WORK_KEY, at);
    }
    assert_eq!(
        tier.world.runs(),
        1,
        "a short-lived token was replaced by the build that made it"
    );
    assert_eq!(tier.vault.writes().len(), 1);
}

/// A second Mac's platform id.
#[cfg(target_os = "macos")]
const MACHINE_B: &str = "00000000-0000-4000-8000-000000000031";

/// THE ACCOUNT'S HOME MAC TAKES THE FIRST TURN AT A GAP ANY MAC MAY FIND (plan package FL-138,
/// [`HOME_TURN_S`]): two Macs of a pool, both signed in to one account whose token the vault does
/// not hold, find the gap at one pass. The Mac that is not the account's home waits the home
/// Mac's turn beside its own spread, though its spread is the shorter of the two: the home Mac's
/// capture runs first, and the other Mac, reading the vault's token at the end of its wait, runs
/// none, so the fleet makes one token. CONTROL: that Mac alone, with no home Mac signed in, makes
/// it once at the end of the turn and its spread, and not a second before.
/// NEGATIVE CONTROL: with `token_wait_s` giving no home turn, the other Mac runs its capture
/// first: `` the other Mac ran a capture before the home Mac's turn ended ``.
#[cfg(target_os = "macos")]
#[test]
fn the_accounts_home_mac_takes_the_first_turn_at_a_gap_any_mac_may_find() {
    let rows = [("work", WORK)];
    let a = Tier::new("renew-window-token", &rows);
    let b = Tier::on(
        "m31",
        "renew-window-token",
        &rows,
        a.vault.clone(),
        Tier::bench_on(|| Some(MACHINE_B.to_owned())),
    );
    let (da, db) = (
        token_delay_s(WORK_KEY, MACHINE),
        token_delay_s(WORK_KEY, MACHINE_B),
    );
    assert_ne!(da, db, "the fixture's two Macs spread apart");
    // The Mac with the shorter spread is the one that is not the account's home.
    let ((home, dh), (away, dw)) = if da > db {
        ((&a, da), (&b, db))
    } else {
        ((&b, db), (&a, da))
    };
    home.in_pool("work", true);
    away.in_pool("work", false);
    away.pass(T0);
    home.pass(T0);
    away.pass(T0 + dw);
    assert_eq!(
        away.world.runs(),
        0,
        "the other Mac ran a capture before the home Mac's turn ended"
    );
    home.pass(T0 + dh);
    assert_eq!(home.world.runs(), 1);
    assert_eq!(a.vault.writes().len(), 1);
    for at in [T0 + HOME_TURN_S + dw, T0 + HOME_TURN_S + dw + 3_600] {
        away.refresh_due();
        away.pass(at);
    }
    assert_eq!(away.world.runs(), 0, "the other Mac made a second token");
    assert_eq!(a.vault.writes().len(), 1, "{:?}", a.vault.writes());

    // CONTROL: alone, the other Mac makes it at the end of the home Mac's turn and its spread.
    let alone = Tier::new("renew-window-token", &rows);
    alone.in_pool("work", false);
    alone.pass(T0);
    alone.pass(T0 + HOME_TURN_S + da - 1);
    assert_eq!(alone.world.runs(), 0, "a capture ran before the turn ended");
    alone.refresh_due();
    alone.pass(T0 + HOME_TURN_S + da);
    assert_eq!(alone.world.runs(), 1);
    assert_eq!(alone.vault.writes().len(), 1);
}

/// A GAP THE UPKEEP RAN IS NOT RUN AGAIN BY A NEW PROCESS (plan package FL-138,
/// [`crate::harness::token_steps::Tried`]): a capture in aterm's window whose browser did not
/// start leaves no stop record (nothing a person passes stopped it), and the upkeep's record that
/// it ran that gap; a new process 25 hours later, and a month later, past the account's wait each
/// time, runs no capture.
/// NEGATIVE CONTROL: with `step_held` reading no tried record (`tried.get(..)` as `None`), the
/// new process runs it again: `` a new process ran the gap again ``.
#[cfg(target_os = "macos")]
#[test]
fn a_gap_the_upkeep_ran_is_not_run_again_by_a_new_process() {
    use crate::harness::accounts_tokens::Gap;
    let tier = Tier::new("window-browser-start", &[("work", WORK)]);
    let ripe = tier.ripen(WORK_KEY);
    assert_eq!(tier.world.runs(), 1);
    assert!(tier.vault.writes().is_empty());
    assert!(
        tier.stopped(WORK_KEY).is_none(),
        "a browser that did not start left a stop record"
    );
    let tried = tier.tried();
    assert_eq!(tried[WORK_KEY].gap, Gap::None, "{tried:?}");
    assert_eq!(tried[WORK_KEY].minted_at, None);
    let logged = tier.bench.logged();
    assert!(
        logged
            .iter()
            .any(|line| line.starts_with("work: token (none): ")
                && !line.contains("made in the sign-in window")),
        "{logged:?}"
    );
    assert!(logged.iter().all(|line| !line.contains('@')), "{logged:?}");
    tier.found_and_ripe(WORK_KEY, ripe + 3_600);
    let tier = Tier {
        bench: Tier::bench(),
        ..tier
    };
    for at in [T0 + 25 * 3_600, T0 + 30 * 86_400] {
        tier.found_and_ripe(WORK_KEY, at);
    }
    assert_eq!(tier.world.runs(), 1, "a new process ran the gap again");
    assert_eq!(tier.tried(), tried);
}

/// A SIGNED-IN ROW LEFT TO A PERSON SPENDS NO TOKEN CAPTURE (plan package FL-138;
/// `docs/DESIGN-claude-sign-in-sitting-2026-11-12.md` §3): a row signed in here with no fleet
/// token, whose account a bot protection stopped (its `CheckStop`) or whose renewal is a
/// person's task, is no gap the upkeep fills, since the verb refuses it to a run with nobody
/// there. Past the account's wait, an hour and a day on and after a restart, no capture runs, no
/// sign-in spacing record is written (no other account waits on it), no record of a run is left
/// and nothing is logged of the row's token. CONTROL: the same row with nothing standing gets
/// its token, its capture spaced on that record.
/// NEGATIVE CONTROL: with `backfill_due` not asking `left_to_a_person_here`, the upkeep spaces
/// the refused run: `` check stop: the upkeep spaced a capture the verb refuses ``.
#[cfg(target_os = "macos")]
#[test]
fn a_signed_in_row_left_to_a_person_spends_no_token_capture() {
    use crate::harness::sign_in_ladder::last_started;
    let check_stop = |tier: &Tier| {
        check_stopped_row(
            &tier.desk,
            "work",
            crate::harness::open_url::MetWord::BotCheck,
            T0 - 60,
        );
    };
    let renew_task = |tier: &Tier| {
        tier.desk
            .store()
            .transact_pool_logins_within(
                &[],
                &tier.desk.roster(),
                T0 - 60,
                &mut None,
                |_, _, logins| {
                    accounts::admit_workflow(logins, "work")?;
                    accounts::note_sign_in_task(
                        logins,
                        "work",
                        accounts::SignInAction::Renew,
                        T0 - 60,
                    );
                    Ok(Vec::new())
                },
            )
            .unwrap();
    };
    let cases: [(&str, &dyn Fn(&Tier)); 2] =
        [("check stop", &check_stop), ("renew task", &renew_task)];
    for (case, left) in cases {
        let tier = Tier::new("renew-window-token", &[("work", WORK)]);
        left(&tier);
        let ripe = tier.ripen(WORK_KEY);
        tier.found_and_ripe(WORK_KEY, ripe + 3_600);
        let tier = Tier {
            bench: Tier::bench(),
            ..tier
        };
        tier.found_and_ripe(WORK_KEY, ripe + 86_400);
        assert_eq!(tier.world.runs(), 0, "{case}: a capture ran");
        assert!(tier.vault.writes().is_empty(), "{case}");
        assert_eq!(
            last_started(&tier.desk.store()).expect("the spacing record reads"),
            None,
            "{case}: the upkeep spaced a capture the verb refuses"
        );
        assert!(
            tier.tried().is_empty(),
            "{case}: a refused run was recorded as run: {:?}",
            tier.tried()
        );
        let logged = tier.bench.logged();
        assert!(
            logged.iter().all(|line| !line.starts_with("work: token")),
            "{case}: {logged:?}"
        );
    }

    let tier = Tier::new("renew-window-token", &[("work", WORK)]);
    tier.ripen(WORK_KEY);
    assert_eq!(tier.world.runs(), 1, "CONTROL: nothing stands");
    assert_eq!(tier.vault.writes().len(), 1);
    assert!(
        last_started(&tier.desk.store())
            .expect("the spacing record reads")
            .is_some(),
        "CONTROL: the capture is spaced"
    );
}

/// A SECOND DUE ACCOUNT WAITS THE SIGN-IN SPACING (plan package FL-138): two rows' gaps ripe at
/// the same pass make one capture there; the second account's waits
/// [`super::super::sign_in_ladder::SPACING_S`] after the first's start, on the one spacing record
/// every sign-in here keeps, and then runs. One account per pass.
#[cfg(target_os = "macos")]
#[test]
fn a_second_due_account_waits_the_sign_in_spacing() {
    use crate::harness::sign_in_ladder::SPACING_S;
    let tier = Tier::new("renew-window-token", &[("work", WORK), ("other", OTHER)]);
    tier.pass(T0);
    let both = T0 + token_delay_s(WORK_KEY, MACHINE).max(token_delay_s(OTHER_KEY, MACHINE));
    tier.pass(both);
    let keys = || -> Vec<String> {
        tier.vault
            .writes()
            .into_iter()
            .map(|(key, _)| key)
            .collect()
    };
    assert_eq!(keys(), [WORK_KEY], "one account per pass");
    tier.pass(both + 1);
    tier.pass(both + SPACING_S - 1);
    assert_eq!(
        keys(),
        [WORK_KEY],
        "the second capture did not wait the spacing"
    );
    tier.pass(both + SPACING_S);
    assert_eq!(keys(), [WORK_KEY, OTHER_KEY]);
    assert_eq!(tier.world.runs(), 2);
}

/// A MAC NOT SEATED IN THE VAULT'S LOG MAKES NO TOKEN (plan package FL-138), and says so once
/// in the log (`vault-not-seated`), with no clock: no capture runs and no refresh is asked for.
#[cfg(target_os = "macos")]
#[test]
fn a_mac_not_seated_in_the_vault_makes_no_token_and_says_so_once() {
    let tier = Tier::new("renew-window-token", &[("work", WORK)]);
    tier.vault
        .route
        .set(aterm_secrets::client::WriteRoute::NotSeated);
    let ripe = tier.ripen(WORK_KEY);
    tier.pass(ripe + 86_400);
    assert_eq!(tier.world.runs(), 0);
    assert!(tier.vault.writes().is_empty());
    assert_eq!(Tier::syncs(), 0);
    let said: Vec<_> = tier
        .bench
        .logged()
        .into_iter()
        .filter(|line| line.contains("vault-not-seated"))
        .collect();
    assert_eq!(said.len(), 1, "{said:?}");
}

/// A SAVED CHOICE A TICK RESUMES WITH NOBODY AT THIS DESK DOES NOT SIGN IN A ROW WHOSE CHECK
/// THIS PROCESS KEEPS UNRECORDED (sitting design §3): a bot protection this process met while
/// the account's records stayed busy ([`AutoRenewGuard::unrecorded_checks`]) holds the pin's door
/// as it holds the ladder's every route; the choice fails, and the pass writes the check, which
/// then holds the row on record. CONTROL: with nothing kept, the same choice starts the row's
/// sign-in.
/// NEGATIVE CONTROL: with the pin's door reading only the records (`left_to_a_person`, the
/// merge's form), `` a saved choice signed in a row whose check this process keeps ``.
#[cfg(unix)]
#[test]
fn a_saved_choice_with_nobody_here_holds_a_check_this_process_keeps() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for kept in [true, false] {
        let desk = desk_with_a_mail_folder();
        let store = desk.store();
        let records = store
            .read_account_records(Some(&desk.roster()), NOW)
            .unwrap();
        let guard = AutoRenewGuard::default().shared();
        if kept {
            guard.lock().unwrap().unrecorded_checks.insert(
                "w3".to_owned(),
                PendingCheck {
                    roster_path: desk.roster(),
                    row: records.roster.row("w3").unwrap().clone(),
                    generation: records.logins.get("w3").map(|b| b.renewal_generation),
                    at: NOW,
                },
            );
        }
        let started = AtomicBool::new(false);
        let spawn = |_work: Box<dyn FnOnce() + Send>| {
            started.store(true, Ordering::SeqCst);
            Ok(())
        };
        let at = NOW + 60;
        let request = store.request_pin("w3", at).unwrap();
        let dispatch = renew_requested_pin_at(
            Cli::Claude,
            (&desk.home, &desk.state, &desk.roster()),
            request,
            at,
            (&guard, &spawn, None),
            None,
            false,
        );
        if kept {
            assert_eq!(
                (dispatch, started.load(Ordering::SeqCst)),
                (RenewalDispatch::Failed, false),
                "a saved choice signed in a row whose check this process keeps"
            );
            assert!(
                guard.lock().unwrap().unrecorded_checks.is_empty(),
                "written by the pass"
            );
            assert!(check_stands(&desk, at), "and held on record");
        } else {
            assert!(!check_stands(&desk, at));
            assert_eq!(
                (dispatch, started.load(Ordering::SeqCst)),
                (RenewalDispatch::Started, true),
                "CONTROL: a saved choice with nothing kept"
            );
        }
    }
}
