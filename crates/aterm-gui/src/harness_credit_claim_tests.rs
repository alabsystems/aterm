// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the claim act ([`super::step`]): its gate, its ledger, the links its workers hold,
//! its quota and readiness holds, and its worker's order. No spawn, no network and no browser:
//! the act's `start` is a recorder, and its seams are the fixture's own.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::Duration;

use aterm_agent::harness::credit_offers::{self, Offer};
use aterm_agent::harness::profile::NoProfile;
use aterm_agent::supervise::Autopilot;

use super::*;
use crate::worker_reaper::tests::{
    claim_folder, in_front, live, new_tab, of, set_task, worker_tab,
};
use crate::worker_reaper::{Ending, LINGER};

const ORG: &str = "0f6c2b8e-1d3a-4c5e-9b7f-2a4d6e8c0b1a";

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

const NOW_MS: i64 = 1_792_022_400_000;

/// A profile whose cloud session credit is live (or claimed, where `claimed`).
fn profile_text(claimed: bool) -> String {
    profile_of("acct-1", claimed, false)
}

/// A profile of account `account` whose cloud session credit is live (or claimed, where
/// `claimed`), and, where `api`, the plan's API credits too.
fn profile_of(account: &str, claimed: bool, api: bool) -> String {
    let (api_flag, api_status) = if api {
        (
            r#","tengu_snug_storm":{"text":"Your Max plan includes $200 a month in API credits. Claim them at https://clau.de/api-credits"}"#.to_string(),
            format!(r#","apiCreditLinkStatusCache":{{"{ORG}":{{"eligibleSince":1792022400000}}}}"#),
        )
    } else {
        (String::new(), String::new())
    };
    format!(
        r#"{{"oauthAccount":{{"accountUuid":"{account}","organizationUuid":"{ORG}","emailAddress":"owner@example.com"}},
"cachedGrowthBookFeatures":{{"tengu_swift_lynx":{{"endpoint":"/v1/code/promo/cloud_credit","webPath":"/code/claim-credit","claimLinkIncludesOrganization":true,"startup":{{"claimPageLine":"Claim your $250 cloud session credit"}}}}{api_flag}}},
"promoStartupStatusCache":{{"accountUuid":"{account}","endpoint":"/v1/code/promo/cloud_credit","eligible":true,"claimed":{claimed}}}{api_status}}}"#
    )
}

const OPEN: Gate = Gate {
    on: true,
    claude: true,
    autopilot: Autopilot::Land,
    claim_credits: true,
    own_worker: false,
};

/// A scratch home: its profile and the harness state the ledger goes under, and the look's
/// other seams: the claims it holds, the live claim workers, the worker's login's wall and what
/// the claim waits on, each read counted.
struct Fixture {
    _dir: aterm_tempfile::TempDir,
    profile: std::path::PathBuf,
    ledger: std::path::PathBuf,
    cache: ProfileCache,
    parses: Cell<u32>,
    claims: Claims,
    workers: RefCell<Vec<ClaimWorker>>,
    walled: Cell<Option<i64>>,
    left: RefCell<Option<String>>,
    holds_read: Cell<u32>,
    t0: Instant,
}

impl Fixture {
    fn new(text: &str) -> Self {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let profile = dir.path().join(".claude.json");
        std::fs::write(&profile, text).expect("profile");
        let ledger = ledger_dir(&dir.path().join("state"));
        Self {
            _dir: dir,
            profile,
            ledger,
            cache: ProfileCache::new(),
            parses: Cell::new(0),
            claims: Claims::new(),
            workers: RefCell::new(Vec::new()),
            walled: Cell::new(None),
            left: RefCell::new(None),
            holds_read: Cell::new(0),
            t0: Instant::now(),
        }
    }

    /// One look under `gate` at `now_ms`, and the offers it started.
    fn look(&self, gate: Gate, now_ms: i64) -> (Stepped, Vec<Offer>) {
        self.look_in(&self.claims, gate, now_ms, self.t0)
    }

    /// [`Self::look`] holding links in `claims`, at `now` on the monotonic clock.
    fn look_in(
        &self,
        claims: &Claims,
        gate: Gate,
        now_ms: i64,
        now: Instant,
    ) -> (Stepped, Vec<Offer>) {
        let read = |path: &Path| -> Result<Vec<Offer>, NoProfile> {
            self.parses.set(self.parses.get() + 1);
            credit_offers::read(path)
        };
        let workers = || self.workers.borrow().clone();
        let walled = || {
            self.holds_read.set(self.holds_read.get() + 1);
            self.walled.get()
        };
        let left = || {
            self.holds_read.set(self.holds_read.get() + 1);
            self.left.borrow().clone()
        };
        let seams = Seams {
            cache: &self.cache,
            read: &read,
            claims,
            workers: &workers,
            walled: &walled,
            left: &left,
        };
        let mut started = Vec::new();
        let stepped = step(
            gate,
            &self.profile,
            &self.ledger,
            now_ms,
            now,
            &seams,
            &mut |offer| started.push(offer),
        );
        (stepped, started)
    }

    /// Each offer in `started` as a worker whose tab landed and whose task has since ended, as
    /// [`App::claim_worker_landed`] and the worker's own end leave it: its link is free again.
    #[cfg(unix)]
    fn finish(&self, started: &[Offer]) {
        let mut workers = self.workers.borrow_mut();
        for offer in started {
            let session = 1000 + u64::try_from(workers.len()).expect("few workers");
            self.claims.spawned(offer.id, &offer.url, session);
            workers.push(ended(session));
        }
    }

    /// The ledger file's bytes, or `None` where none was written.
    #[cfg(unix)]
    fn ledger_bytes(&self) -> Option<Vec<u8>> {
        std::fs::read(self.ledger.join("attempts")).ok()
    }
}

#[test]
fn a_shut_gate_looks_at_nothing_and_starts_nothing() {
    let fx = Fixture::new(&profile_text(false));
    for gate in [
        Gate { on: false, ..OPEN },
        Gate {
            own_worker: true,
            ..OPEN
        },
        Gate {
            autopilot: Autopilot::Off,
            ..OPEN
        },
        Gate {
            claim_credits: false,
            ..OPEN
        },
        Gate {
            claude: false,
            ..OPEN
        },
    ] {
        let (stepped, started) = fx.look(gate, NOW_MS);
        assert_eq!(stepped, Stepped::Shut, "{gate:?}");
        assert!(started.is_empty(), "{gate:?}");
    }
    assert_eq!(fx.parses.get(), 0, "no profile read under a shut gate");
    assert!(!fx.ledger.exists(), "no ledger written");
    assert_eq!(fx.holds_read.get(), 0, "no hold read");
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        assert!(Gate { autopilot, ..OPEN }.open(), "{autopilot:?}");
    }
}

/// Unix only, as the owner-only ledger a try is recorded in is (`read_ledger`); off Unix,
/// `a_live_offer_starts_no_worker_where_no_ledger_can_be_kept` holds the refusal.
#[cfg(unix)]
#[test]
fn a_live_offer_is_recorded_before_its_worker_and_tried_once() {
    let fx = Fixture::new(&profile_text(false));
    let (stepped, started) = fx.look(OPEN, NOW_MS);
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].id, credit_offers::OfferId::CloudCredit);
    assert_eq!(stepped, Stepped::Started(started.clone()));
    let book = read_ledger(&fx.ledger).expect("a ledger");
    assert_eq!(
        book.rows,
        vec![Tried {
            key: started[0].key.clone(),
            count: 1,
            first_ms: NOW_MS,
            last_ms: NOW_MS,
        }]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&fx.ledger.join("attempts")), 0o600);
        assert_eq!(mode(&fx.ledger), 0o700);
    }
    fx.finish(&started);
    let (stepped, started) = fx.look(OPEN, NOW_MS + DAY_MS);
    assert_eq!(stepped, Stepped::NothingDue);
    assert!(started.is_empty(), "already tried");
    assert_eq!(
        fx.parses.get(),
        1,
        "the profile's stamp stood: no second parse"
    );
}

/// Unix only, as the owner-only ledger a try is recorded in is (`read_ledger`); off Unix,
/// `a_live_offer_starts_no_worker_where_no_ledger_can_be_kept` holds the refusal.
#[cfg(unix)]
#[test]
fn an_offer_is_tried_again_once_after_a_week_while_it_stands() {
    let fx = Fixture::new(&profile_text(false));
    let first = fx.look(OPEN, NOW_MS).1;
    assert_eq!(first.len(), 1);
    fx.finish(&first);
    let (stepped, _) = fx.look(OPEN, NOW_MS + 7 * DAY_MS - 1);
    assert_eq!(stepped, Stepped::NothingDue, "not before the week is out");
    let (_, again) = fx.look(OPEN, NOW_MS + 7 * DAY_MS);
    assert_eq!(again.len(), 1, "the one retry");
    fx.finish(&again);
    let book = read_ledger(&fx.ledger).expect("a ledger");
    assert_eq!(book.rows[0].count, 2);
    assert_eq!(book.rows[0].first_ms, NOW_MS);
    assert_eq!(book.rows[0].last_ms, NOW_MS + 7 * DAY_MS);
    for later in [8, 15, 400] {
        let (stepped, started) = fx.look(OPEN, NOW_MS + later * DAY_MS);
        assert_eq!(
            stepped,
            Stepped::NothingDue,
            "day {later}: two tries at most"
        );
        assert!(started.is_empty());
    }
}

/// Unix only, as the owner-only ledger a try is recorded in is (`read_ledger`); off Unix,
/// `a_live_offer_starts_no_worker_where_no_ledger_can_be_kept` holds the refusal.
#[cfg(unix)]
#[test]
fn an_offer_gone_by_its_retry_is_not_tried_again() {
    let fx = Fixture::new(&profile_text(false));
    assert_eq!(fx.look(OPEN, NOW_MS).1.len(), 1);
    std::fs::write(&fx.profile, profile_text(true)).expect("claimed");
    let (stepped, started) = fx.look(OPEN, NOW_MS + 8 * DAY_MS);
    assert_eq!(stepped, Stepped::NothingLive);
    assert!(started.is_empty());
    assert_eq!(fx.parses.get(), 2, "the profile moved: read again");
}

#[test]
fn nothing_live_starts_nothing_and_an_unmoved_profile_is_not_parsed_again() {
    let fx = Fixture::new(&profile_text(true));
    for at in 0..3 {
        let (stepped, started) = fx.look(OPEN, NOW_MS + at * DAY_MS);
        assert_eq!(stepped, Stepped::NothingLive);
        assert!(started.is_empty());
    }
    assert_eq!(fx.parses.get(), 1, "one parse; each later look a stat");
    assert!(!fx.ledger.exists(), "nothing recorded");
    std::fs::remove_file(&fx.profile).expect("gone");
    assert_eq!(fx.look(OPEN, NOW_MS).0, Stepped::NothingLive);
    assert_eq!(fx.parses.get(), 1, "no file: nothing to parse");
}

/// Unix only, as the owner-only ledger a try is recorded in is (`read_ledger`); off Unix,
/// `a_live_offer_starts_no_worker_where_no_ledger_can_be_kept` holds the refusal.
#[cfg(unix)]
#[test]
fn an_unrecordable_try_starts_no_worker() {
    let fx = Fixture::new(&profile_text(false));
    std::fs::create_dir_all(fx.ledger.parent().expect("state")).expect("state");
    std::fs::write(&fx.ledger, b"a file where the ledger's folder goes").expect("blocker");
    let (stepped, started) = fx.look(OPEN, NOW_MS);
    assert!(matches!(stepped, Stepped::Unrecorded(_)), "{stepped:?}");
    assert!(started.is_empty(), "no record, no worker");

    let fx = Fixture::new(&profile_text(false));
    write_ledger(&fx.ledger, &Ledger::default()).expect("a ledger");
    std::fs::write(fx.ledger.join("attempts"), b"not a ledger\n").expect("garbage");
    let (stepped, started) = fx.look(OPEN, NOW_MS);
    assert!(matches!(stepped, Stepped::Unrecorded(_)), "{stepped:?}");
    assert!(started.is_empty(), "an unreadable ledger tries nothing");
}

/// Where no owner-only file can be kept (off Unix), no try can be recorded, so a live offer is
/// never tried: the look is unrecorded, starts no worker and writes nothing.
#[cfg(not(unix))]
#[test]
fn a_live_offer_starts_no_worker_where_no_ledger_can_be_kept() {
    let fx = Fixture::new(&profile_text(false));
    let (stepped, started) = fx.look(OPEN, NOW_MS);
    assert!(matches!(stepped, Stepped::Unrecorded(_)), "{stepped:?}");
    assert!(started.is_empty(), "no record, no worker");
    assert!(!fx.ledger.exists(), "nothing written");
}

#[test]
fn the_ledger_reads_back_what_it_wrote_and_stays_small() {
    let mut book = Ledger::default();
    for n in 0..(LEDGER_ROWS as u8 + 4) {
        book.tried(&[key(n)], i64::from(n));
    }
    assert_eq!(book.rows.len(), LEDGER_ROWS);
    assert!(
        !book.rows.iter().any(|r| r.key == key(0)),
        "the oldest went"
    );
    assert_eq!(Ledger::parse(&book.text()), Some(book.clone()));
    assert!(book.text().len() < LEDGER_MAX);
    for bad in [
        "",
        "aterm-credit-claims v2\n",
        "aterm-credit-claims v1\nzz 1 2 3\n",
        "aterm-credit-claims v1\n00000000000000000000000000000000 1 2\n",
        "aterm-credit-claims v1\n00000000000000000000000000000000 1 2 3 4\n",
    ] {
        assert_eq!(Ledger::parse(bad), None, "{bad:?}");
    }
}

/// A key of its own for row `n`.
fn key(n: u8) -> credit_offers::OfferKey {
    credit_offers::OfferKey::parse(&format!("{n:02x}").repeat(16)).expect("key")
}

/// A full ledger: [`LEDGER_ROWS`] offers each tried once, row `n` at `base_ms + n`.
fn full_ledger(base_ms: i64) -> Ledger {
    let mut book = Ledger::default();
    for n in 0..LEDGER_ROWS as u8 {
        book.tried(&[key(n)], base_ms + i64::from(n));
    }
    assert_eq!(book.rows.len(), LEDGER_ROWS);
    book
}

/// A TRY IS NEVER UNRECORDED AS IT IS MADE: on a full ledger and a clock gone backwards (the new
/// row the least recently tried), the new key stays and the least recently tried other row goes.
/// NEGATIVE CONTROL: dropping the least recently tried row of all, the rule before, drops the key
/// just tried, which the next look would try again.
#[test]
fn a_full_ledger_on_a_backwards_clock_keeps_the_key_just_tried() {
    let mut book = full_ledger(NOW_MS);
    let new = key(0xee);
    let mut before = book.clone();
    before.rows.push(Tried {
        key: new.clone(),
        count: 1,
        first_ms: NOW_MS - DAY_MS,
        last_ms: NOW_MS - DAY_MS,
    });
    let oldest = before
        .rows
        .iter()
        .min_by_key(|row| row.last_ms)
        .expect("rows");
    assert_eq!(oldest.key, new, "CONTROL: the old rule dropped the new row");
    book.tried(std::slice::from_ref(&new), NOW_MS - DAY_MS);
    assert_eq!(book.rows.len(), LEDGER_ROWS);
    assert!(book.rows.iter().any(|row| row.key == new), "the try stays");
    assert!(!book.due(&new, NOW_MS - DAY_MS), "and is not due again");
    assert!(!book.rows.iter().any(|row| row.key == key(0)), "row 0 went");
}

/// A FULL LEDGER DROPS A SPENT ROW FIRST: with one row's tries spent, a seventeenth key drops it,
/// though other rows were tried less recently; with none spent, the least recently tried. Two
/// keys tried at one look, the clock backwards, both stay. CONTROL: the spent row's offer is the
/// one [`Ledger::due`] never tries again, and the dropped unspent row's is due again.
#[test]
fn a_full_ledger_drops_a_spent_row_first_and_never_a_key_of_its_look() {
    let mut book = full_ledger(NOW_MS);
    book.tried(&[key(9)], NOW_MS + 7 * DAY_MS);
    assert!(
        !book.due(&key(9), NOW_MS + 30 * DAY_MS),
        "CONTROL: row 9 is spent"
    );
    book.tried(&[key(16)], NOW_MS + 8 * DAY_MS);
    assert_eq!(book.rows.len(), LEDGER_ROWS);
    assert!(
        !book.rows.iter().any(|row| row.key == key(9)),
        "the spent row went"
    );
    assert!(book.rows.iter().any(|row| row.key == key(0)), "row 0 stays");
    book.tried(&[key(17)], NOW_MS + 9 * DAY_MS);
    assert!(
        !book.rows.iter().any(|row| row.key == key(0)),
        "then the least recent"
    );
    assert!(
        book.due(&key(0), NOW_MS + 9 * DAY_MS),
        "CONTROL: row 0's offer is due again"
    );

    let mut book = full_ledger(NOW_MS);
    let (a, b) = (key(0xaa), key(0xbb));
    book.tried(&[a.clone(), b.clone()], NOW_MS - DAY_MS);
    assert_eq!(book.rows.len(), LEDGER_ROWS);
    for k in [&a, &b] {
        assert!(book.rows.iter().any(|row| &row.key == k), "{k}");
    }
    assert!(Ledger::parse(&book.text()).is_some(), "it reads back");
}

#[test]
fn a_claim_worker_is_claude_code_with_the_browser_and_the_guarded_task() {
    let offer = credit_offers::parse(profile_text(false).as_bytes())
        .expect("parses")
        .remove(0);
    let order = claim_order(&offer, None);
    assert!(order.chrome, "Claude in Chrome");
    assert_eq!(order.agent, aterm_phase::Program::Claude);
    assert_eq!(order.autopilot, Autopilot::Land);
    assert!(!order.fact_line && order.caller.is_none() && order.model.is_none());
    let task = String::from_utf8(order.task).expect("UTF-8");
    assert_eq!(task, credit_offers::claim_task(&offer));
    assert!(
        task.starts_with("[aterm harness] Claim a free Claude credit"),
        "{task}"
    );
    assert!(task.contains(&offer.url), "{task}");
    assert!(
        task.contains("Never enter, add or change payment details"),
        "{task}"
    );
    assert!(task.contains("never turn on extra usage"), "{task}");
    assert!(task.contains("Never ask a question."), "{task}");
    assert!(
        task.contains("end with one reply starting `Nothing left: CLAIMED:")
            && task.contains("first line is `Could not finish: NOT CLAIMED: <why>`"),
        "{task}"
    );
    assert!(!task.contains("sleep"), "no command to wait with: {task}");

    struct Present;
    impl crate::task_launch::ProgramStore for Present {
        fn present(&self, _: &str) -> bool {
            true
        }
        fn held(&self, _: &str) -> Option<String> {
            None
        }
        fn install(&self, _: &str) -> Result<(), String> {
            Err("no install in a test".to_string())
        }
    }
    let claude =
        aterm_agent::harness::worker::worker_cli(aterm_phase::Program::Claude).expect("a row");
    let admitted =
        crate::task_launch::admit_worker(claude, claude.plain(), None, &|_| {}, &|| (), &Present)
            .expect("admitted");
    use aterm_agent::supervise::config::Approve;
    let line_of = |chrome: bool, approve: Approve| {
        aterm_agent::harness::worker::line(
            claude,
            aterm_agent::harness::upgrade::Dialect::Zsh,
            &admitted,
            &crate::task_launch::worker_fresh(None, false, chrome, approve),
        )
        .expect("a line")
    };
    for approve in [Approve::All, Approve::Safe] {
        let line = line_of(order.chrome, approve);
        assert!(line.contains(" '--chrome' "), "{line}");
        assert!(
            line.contains(" '--permission-mode' 'auto' "),
            "auto mode, whatever the person's Claude Code settings (owner, 2026-11-04): {line}"
        );
        assert!(
            line.contains(" '--tools' 'ToolSearch' "),
            "no built-in tool that acts or asks, in any mode: {line}"
        );
        assert!(
            line.contains(&format!(
                " '--settings' '{}' ",
                credit_offers::claim_settings()
            )),
            "auto mode's classifier reads the claim's hard rules: {line}"
        );
    }
    let held = line_of(order.chrome, Approve::None);
    assert!(
        held.contains(" '--permission-mode' 'default' ") && !held.contains("'auto'"),
        "CONTROL: `approve = \"none\"` keeps every box the person's: {held}"
    );
    assert!(held.contains(" '--tools' 'ToolSearch' "), "{held}");
    for approve in [Approve::All, Approve::Safe, Approve::None] {
        let mode = crate::task_launch::claim_worker_mode(approve);
        assert!(
            aterm_agent::harness::worker::CLAUDE_PERMISSION_MODE
                .modes
                .contains(&mode),
            "a mode Claude Code's flag takes: {mode}"
        );
    }
    let plain = line_of(false, Approve::All);
    assert!(
        !plain.contains("--chrome")
            && !plain.contains("--permission-mode")
            && !plain.contains("--tools")
            && !plain.contains("--settings"),
        "CONTROL, only a claim worker's: {plain}"
    );
}

/// A WORKER RUNS OUT OF ITS TIME BY ITS TASK RECORD: its trusted folder says it is a claim worker
/// and its record says how long it has stood, both of which a seamless update carries. CONTROL: a
/// moment short of its time; a worker of another folder; no trusted folder.
#[test]
fn a_claim_worker_runs_out_of_its_time_by_its_record() {
    assert_eq!(WORKER_FOR, Duration::from_secs(30 * 60));
    let claim = claim_folder();
    let moment = Duration::from_millis(1);
    assert!(ran_out(Some(&claim), WORKER_FOR));
    assert!(!ran_out(Some(&claim), WORKER_FOR - moment));
    assert!(!ran_out(Some("/Users//me/project"), WORKER_FOR));
    assert!(!ran_out(None, WORKER_FOR));
}

/// A claim worker's execution budget counts from its start or a person's typing. A later task
/// state does not move that clock; a concluded task keeps its pane regardless of the clock.
#[test]
fn a_workers_time_counts_from_its_start_and_a_persons_typing() {
    use crate::task_record::TaskMark;
    let t0 = Instant::now();
    let later = |secs| t0 + Duration::from_secs(secs);
    let (first, _) = TaskMark::new(TaskState::Working).apply(None, &Default::default(), t0, 1_000);
    let (flipped, _) = TaskMark::new(TaskState::Stalled).apply(
        Some(&first),
        &Default::default(),
        later(120),
        2_000,
    );
    let (back, _) = TaskMark::new(TaskState::Working).apply(
        Some(&flipped),
        &Default::default(),
        later(240),
        3_000,
    );
    assert_eq!(stood(&back, later(600), None), Duration::from_secs(600));
    assert_eq!(back.started_unix_ms, 1_000);
    assert_eq!(
        stood(&back, later(600), Some(Duration::from_secs(5))),
        Duration::from_secs(5)
    );
    let (exited, _) =
        TaskMark::new(TaskState::Exited).apply(Some(&back), &Default::default(), later(600), 4_000);
    assert_eq!(stood(&exited, later(660), None), Duration::from_secs(660));
}

/// A RESTORE GIVES A WORKER ITS LIMIT BACK: the record that held its time did not cross, so a
/// claim worker it brings back starts a new one, `working why=restored`, which no startup or held
/// deadline reads as a worker just spawned, and ends by it. CONTROL: an agent of another folder
/// gets no record.
#[test]
fn a_restored_claim_worker_has_a_limit_again() {
    use crate::harness_host::StartupState;
    let claim = claim_folder();
    let mut app = crate::App::headless_for_test();
    let worker = worker_tab(&mut app);
    let ctx = live(&app, worker).ctx;
    let task = |ctx: &crate::SessionCtx| {
        ctx.timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .task()
            .cloned()
    };

    note_restored_worker(&ctx, worker, "/Users//me/project", false);
    assert_eq!(task(&ctx), None);

    note_restored_worker(&ctx, worker, &claim, false);
    let record = task(&ctx).expect("a new record");
    assert_eq!(record.trusted_cwd.as_deref(), Some(claim.as_str()));
    assert_eq!(
        (record.state, record.why),
        (TaskState::Working, Some(crate::task_record::WHY_RESTORED))
    );
    let sid = live(&app, worker).sid.as_str().to_owned();
    let read = super::super::startups_of(&app.store, &std::collections::HashSet::new());
    assert_eq!(
        read.iter()
            .find(|startup| startup.sid == sid)
            .map(|startup| (startup.state, startup.held.is_some())),
        Some((StartupState::Other, false)),
        "no startup or held deadline reads a worker that started before the restore"
    );
    assert_eq!(claim_worker(&ctx), Some((claim.clone(), false)));
    assert_eq!(
        app.reap_workers(record.started + WORKER_FOR),
        [(worker, Ending::MarkExited)]
    );
    assert_eq!(claim_worker(&ctx), Some((claim, true)), "its worker ended");
}

/// A TAB WHOSE WORKER HAD ENDED STAYS A PERSON'S ACROSS A RESTORE: the layout names each claim
/// worker's folder and whether its task was marked `exited`, so a restore gives an ended
/// worker's tab an `exited` record, not a new worker's time, an agent on its leaf or not (the
/// restore's own path: `app_restore_tests::a_restored_claim_worker_gets_a_task_record`), and a
/// Claude Code found or brought back there is not signalled, and its pane stays. CONTROL: the
/// layout of a running worker's tab names its folder and nothing ended; a tab with no worker and
/// a task that ended in another folder name none.
#[test]
fn a_restore_leaves_a_program_in_an_ended_workers_tab() {
    let claim = claim_folder();
    let mut app = crate::App::headless_for_test();
    let front = 0;
    let running = worker_tab(&mut app);
    let ended = worker_tab(&mut app);
    let began = set_task(&app, ended, &claim);
    assert_eq!(
        app.reap_workers(began + WORKER_FOR),
        [(ended, Ending::MarkExited)],
        "PRECONDITION"
    );
    set_task(&app, running, &claim);
    let elsewhere = new_tab(&mut app);
    set_task(&app, elsewhere, "/Users//me/project");
    let h = live(&app, elsewhere);
    crate::task_record::mark_session_task(
        &h.ctx,
        elsewhere,
        &crate::task_record::TaskMark::new(TaskState::Exited),
    )
    .expect("unfenced");
    // Someone's own Claude Code in the claim folder (a tab opened from the worker's), and the
    // running worker's own, both hosted there.
    let someones = new_tab(&mut app);
    let hosted = |session: u64| {
        let sid = live(&app, session).sid.as_str().to_string();
        let agent = aterm_agent::harness::relaunch::Snapshot {
            launch_env: None,
            auth_observed: true,
            route: aterm_agent::harness::accounts::AuthRoute::Outside,
            profile: None,
            tab: sid.clone(),
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".into(),
            shell: 4343,
            program: std::path::PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: None,
            cwd: claim.clone(),
            version: None,
            run: aterm_agent::harness::relaunch::AgentRun::Claude,
            dialect: None,
            login: None,
            home: None,
            claude_dir: None,
        };
        crate::harness_host::publish_snapshot_for_test(&sid, Some(&agent));
        sid
    };
    let published = [hosted(someones), hosted(running)];
    // As the crash journal and a quit write it: captured, then each leaf given its agent.
    let ended_of = |app: &mut crate::App| {
        let mut manifest = app.capture_restore_manifest();
        app.fill_desk_agents(&mut manifest);
        let mut leaves: Vec<_> = manifest
            .handed_off_leaves()
            .into_iter()
            .map(|leaf| {
                (
                    leaf.local_id.expect("named"),
                    leaf.claim_worker,
                    leaf.claim_ended,
                    leaf.not_claim_worker,
                )
            })
            .collect();
        leaves.sort_unstable();
        leaves
    };
    let layout = ended_of(&mut app);
    for sid in &published {
        crate::harness_host::publish_snapshot_for_test(sid, None);
    }
    assert_eq!(
        layout,
        [
            (front, None, false, false),
            (running, Some(claim.clone()), false, false),
            (ended, Some(claim.clone()), true, false),
            (elsewhere, None, false, false),
            (someones, None, false, true),
        ],
        "the layout names each claim worker's folder and which had ended, and an agent in the \
         claim folder that is no worker's"
    );

    let restored = worker_tab(&mut app);
    let ctx = live(&app, restored).ctx;
    note_restored_worker(&ctx, restored, &claim, true);
    let record = ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .task()
        .cloned()
        .expect("a new record");
    assert_eq!(
        (record.state, record.why, record.trusted_cwd.as_deref()),
        (TaskState::Exited, Some("exit"), Some(claim.as_str()))
    );
    crate::feed_session_for_test(
        &app,
        restored,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07claude\n\x1b]133;C\x07",
    );
    in_front(&app, restored, "claude");
    assert_eq!(
        of(app.reap_workers(record.since + WORKER_FOR), restored),
        [],
        "a person's Claude Code in a tab whose worker had ended"
    );
    crate::feed_session_for_test(&app, restored, b"\x1b]133;D;0\x07");
    let seen = record.since + WORKER_FOR;
    assert_eq!(
        of(
            app.reap_workers(seen + LINGER - Duration::from_secs(1)),
            restored
        ),
        [],
        "its output is still to be read"
    );
    assert_eq!(of(app.reap_workers(seen + LINGER), restored), []);
}

/// A running claim worker in session `session`.
const fn running(session: u64) -> ClaimWorker {
    ClaimWorker {
        session,
        ended: false,
    }
}

/// A claim worker in session `session` whose task is marked `exited`.
const fn ended(session: u64) -> ClaimWorker {
    ClaimWorker {
        session,
        ended: true,
    }
}

/// A HARNESS WORKER'S OWN SESSION NEVER RUNS THE ACT: a tab whose task record trusts a claim
/// folder or the disk worker's folder is one of aterm's own workers, and so is a session the
/// store no longer holds (fail closed). NEGATIVE CONTROL: a person's tab with no record, and a
/// worker trusted a repository, run it.
#[test]
fn a_harness_workers_own_session_never_runs_the_act() {
    let mut app = crate::App::headless_for_test();
    let person = new_tab(&mut app);
    let claim = new_tab(&mut app);
    let disk = new_tab(&mut app);
    let repo = new_tab(&mut app);
    set_task(&app, claim, &claim_folder());
    let disk_folder = format!(
        "/state/{}/{}",
        aterm_agent::harness::cli::HARNESS,
        aterm_agent::harness::disk::fence::WORKER_FOLDER
    );
    set_task(&app, disk, &disk_folder);
    set_task(&app, repo, "/Users//me/project");
    let sid = |session| live(&app, session).sid.as_str().to_owned();
    assert!(own_worker(&app.store, &sid(claim)), "a claim worker");
    assert!(own_worker(&app.store, &sid(disk)), "the disk worker");
    assert!(
        own_worker(&app.store, "s-0000000000000000dead"),
        "a session the store lacks"
    );
    assert!(
        !own_worker(&app.store, &sid(person)),
        "CONTROL: a person's tab"
    );
    assert!(
        !own_worker(&app.store, &sid(repo)),
        "CONTROL: a worker in a repository"
    );
    assert_eq!(
        claim_workers(&app.store),
        [running(claim)],
        "the one claim worker, running"
    );
}

/// A LINK A LIVE CLAIM WORKER COVERS STARTS NO SECOND WORKER, WHATEVER THE ACCOUNT: the incident
/// of 2026-11-04, where the default login moved between two looks, so the same link was a new
/// offer key and the ledger tried it again while its first worker still ran. Covered while it is
/// asked for, and while its worker runs; once that worker ended, the new account's key is due.
/// NEGATIVE CONTROL: the account change does make a new key for the same link, so the ledger
/// alone would have started it.
#[cfg(unix)]
#[test]
fn a_link_a_live_claim_worker_covers_starts_no_second_worker_whatever_the_account() {
    let fx = Fixture::new(&profile_of("acct-1", false, false));
    let (stepped, first) = fx.look(OPEN, NOW_MS);
    assert_eq!(stepped, Stepped::Started(first.clone()));
    assert_eq!(first.len(), 1);
    std::fs::write(&fx.profile, profile_of("acct-2", false, false)).expect("the login moved");
    let moved = credit_offers::read(&fx.profile).expect("reads").remove(0);
    assert_ne!(moved.key, first[0].key, "CONTROL: another account's key");
    assert_eq!(moved.url, first[0].url, "for the same link");
    assert_eq!(moved.id, first[0].id);

    let ledger = fx.ledger_bytes();
    let (stepped, started) = fx.look(OPEN, NOW_MS + 1);
    assert_eq!(stepped, Stepped::Covered, "its worker is asked for");
    assert!(started.is_empty());
    assert_eq!(fx.ledger_bytes(), ledger, "no try recorded");

    fx.claims.spawned(moved.id, &moved.url, 7);
    *fx.workers.borrow_mut() = vec![running(7)];
    assert_eq!(
        fx.look(OPEN, NOW_MS + 2).0,
        Stepped::Covered,
        "its worker runs"
    );
    assert_eq!(fx.ledger_bytes(), ledger);

    *fx.workers.borrow_mut() = vec![ended(7)];
    let (stepped, started) = fx.look(OPEN, NOW_MS + 3);
    assert_eq!(
        stepped,
        Stepped::Started(started.clone()),
        "its worker ended"
    );
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].key, moved.key);
}

/// ONLY THE SAME OFFER'S SAME LINK IS COVERED, AND A REFUSED SPAWN GIVES IT BACK. NEGATIVE
/// CONTROLS: a row for the other offer, or for another link, covers nothing; a row of a worker
/// that ended, or is gone, covers nothing, and the look drops it.
#[test]
fn only_the_same_offers_same_link_is_covered_and_a_refused_spawn_frees_it() {
    let claims = Claims::new();
    let t0 = Instant::now();
    let url = "https://clau.de/api-credits";
    claims.asked(OfferId::ApiCredits, url, t0);
    assert!(claims.covers(OfferId::ApiCredits, url, &[], t0));
    assert!(
        !claims.covers(OfferId::CloudCredit, url, &[], t0),
        "CONTROL: the other offer"
    );
    assert!(
        !claims.covers(OfferId::ApiCredits, "https://clau.de/other", &[], t0),
        "CONTROL: another link"
    );
    claims.refused(OfferId::ApiCredits, url);
    assert!(
        !claims.covers(OfferId::ApiCredits, url, &[], t0),
        "given back"
    );

    claims.asked(OfferId::ApiCredits, url, t0);
    claims.spawned(OfferId::ApiCredits, url, 4);
    claims.refused(OfferId::ApiCredits, url);
    assert!(
        claims.covers(OfferId::ApiCredits, url, &[running(4)], t0),
        "a landed worker's hold is no refusal's to take"
    );
    assert!(
        !claims.covers(OfferId::ApiCredits, url, &[ended(4)], t0),
        "CONTROL: its worker ended"
    );
    claims.spawned(OfferId::CloudCredit, "https://claude.ai/cloud", 5);
    claims.asked(OfferId::ApiCredits, url, t0);
    claims.spawned(OfferId::ApiCredits, url, 6);
    assert!(
        !claims.covers(OfferId::ApiCredits, url, &[running(5)], t0),
        "CONTROL: its worker gone"
    );
}

/// A RUNNING CLAIM WORKER NO ROW NAMES COVERS EVERY LINK: one a restore or a seamless update
/// brought back, whose offer this process never knew. CONTROL: once it ended, both offers start.
#[cfg(unix)]
#[test]
fn a_running_claim_worker_no_row_names_covers_every_link() {
    let fx = Fixture::new(&profile_of("acct-1", false, true));
    *fx.workers.borrow_mut() = vec![running(9)];
    let (stepped, started) = fx.look(OPEN, NOW_MS);
    assert_eq!(stepped, Stepped::Covered);
    assert!(started.is_empty());
    assert_eq!(fx.ledger_bytes(), None, "nothing recorded");
    *fx.workers.borrow_mut() = vec![ended(9)];
    let (stepped, started) = fx.look(OPEN, NOW_MS);
    assert_eq!(stepped, Stepped::Started(started.clone()), "CONTROL");
    assert_eq!(
        started.iter().map(|o| o.id).collect::<Vec<_>>(),
        [OfferId::CloudCredit, OfferId::ApiCredits]
    );
}

/// TWO SESSIONS, ONE LINK, ONE WORKER: two tabs on two accounts' profiles, one link, looking one
/// after the other (the ledger's lock orders any two looks, and the first's hold is written under
/// it before it returns), start one worker between them. CONTROL: with no shared hold, each
/// account's ledger would start its own.
#[cfg(unix)]
#[test]
fn two_sessions_one_link_start_one_worker() {
    let claims = Claims::new();
    let a = Fixture::new(&profile_of("acct-1", false, false));
    let b = Fixture::new(&profile_of("acct-2", false, false));
    let (first, _) = a.look_in(&claims, OPEN, NOW_MS, a.t0);
    let (second, _) = b.look_in(&claims, OPEN, NOW_MS, a.t0);
    assert!(
        matches!(first, Stepped::Started(ref s) if s.len() == 1),
        "{first:?}"
    );
    assert_eq!(second, Stepped::Covered);
    let (alone, _) = b.look_in(&Claims::new(), OPEN, NOW_MS, a.t0);
    assert!(
        matches!(alone, Stepped::Started(ref s) if s.len() == 1),
        "CONTROL: {alone:?}"
    );
}

/// AN ASK WHOSE TAB NEVER LANDED STOPS HOLDING ITS LINK after [`WORKER_FOR`]: a spawn that never
/// answered keeps no link forever. CONTROL: a moment before, it still covers.
#[test]
fn an_ask_whose_tab_never_landed_holds_its_link_for_the_workers_time() {
    let claims = Claims::new();
    let t0 = Instant::now();
    let url = "https://claude.ai/code/claim-credit/6/x";
    claims.asked(OfferId::CloudCredit, url, t0);
    let moment = Duration::from_millis(1);
    assert!(
        claims.covers(OfferId::CloudCredit, url, &[], t0 + WORKER_FOR - moment),
        "CONTROL"
    );
    assert!(!claims.covers(OfferId::CloudCredit, url, &[], t0 + WORKER_FOR));
}

/// A WALLED LOGIN SPENDS NO TRY: where the login a worker would start on is at a usage limit, the
/// look starts nothing, writes no ledger, holds no link and says until when; a look once it
/// runs again starts the claim. NEGATIVE CONTROL: the same look unwalled records a try.
#[cfg(unix)]
#[test]
fn a_walled_login_spends_no_try() {
    let fx = Fixture::new(&profile_text(false));
    fx.walled.set(Some(1_792_100_000));
    let (stepped, started) = fx.look(OPEN, NOW_MS);
    assert_eq!(stepped, Stepped::Walled(1_792_100_000));
    assert!(started.is_empty());
    assert_eq!(fx.ledger_bytes(), None, "no try spent");
    let offer = credit_offers::read(&fx.profile).expect("reads").remove(0);
    assert!(
        !fx.claims.covers(offer.id, &offer.url, &[], fx.t0),
        "no link held"
    );
    fx.walled.set(None);
    let (stepped, started) = fx.look(OPEN, NOW_MS + DAY_MS);
    assert_eq!(stepped, Stepped::Started(started.clone()), "CONTROL");
    assert_eq!(read_ledger(&fx.ledger).expect("a ledger").rows.len(), 1);
}

/// A CLAIM NOT READY SPENDS NO TRY AND OPENS NO TAB: where Chrome or its extension is not in
/// place, the look says what is left, starts nothing, writes no ledger and holds no link; the
/// next look checks again. NEGATIVE CONTROL: ready, the same look records its try and starts.
#[cfg(unix)]
#[test]
fn a_claim_not_ready_spends_no_try_and_opens_no_tab() {
    let fx = Fixture::new(&profile_text(false));
    *fx.left.borrow_mut() = Some("Left: sign in to claude.ai in Chrome.".to_string());
    for at in 0..2 {
        let (stepped, started) = fx.look(OPEN, NOW_MS + at);
        assert_eq!(
            stepped,
            Stepped::NotReady("Left: sign in to claude.ai in Chrome.".to_string())
        );
        assert!(started.is_empty());
    }
    assert_eq!(fx.ledger_bytes(), None, "no try spent");
    *fx.left.borrow_mut() = None;
    let (stepped, started) = fx.look(OPEN, NOW_MS + 2);
    assert_eq!(stepped, Stepped::Started(started.clone()), "CONTROL");
    assert_eq!(started.len(), 1);
}

/// THE HOLDS ARE READ ONLY FOR A DUE OFFER: a covered look and one with nothing due read neither
/// the accounts nor the machine. CONTROL: a due one reads both.
#[cfg(unix)]
#[test]
fn the_holds_are_read_only_for_a_due_offer() {
    let fx = Fixture::new(&profile_text(false));
    assert!(matches!(fx.look(OPEN, NOW_MS).0, Stepped::Started(_)));
    assert_eq!(
        fx.holds_read.get(),
        2,
        "CONTROL: the wall and the readiness"
    );
    assert_eq!(fx.look(OPEN, NOW_MS + 1).0, Stepped::Covered);
    let offer = credit_offers::read(&fx.profile).expect("reads").remove(0);
    fx.claims.refused(offer.id, &offer.url);
    assert_eq!(fx.look(OPEN, NOW_MS + 2).0, Stepped::NothingDue);
    assert_eq!(fx.holds_read.get(), 2, "neither read again");
}

/// A CLAIM WORKER IS NAMED FOR ITS OFFER AS IT LANDS: its tab's role, which the Workers chip and
/// every message read, so none reads the line its shell types; and it holds its link by its
/// session. CONTROL: before, the chip read no such name.
#[test]
fn a_landed_claim_worker_is_named_for_its_offer_and_holds_its_link() {
    let mut app = crate::App::headless_for_test();
    let session = app.next_session_id;
    app.next_session_id += 1;
    let parent = app.new_tab_parent(crate::WindowId(0));
    app.install_tab_session_parked(crate::WindowId(0), crate::stub_session(session), parent);
    app.sync_window(crate::WindowId(0));
    let sid = live(&app, session).sid.as_str().to_owned();
    let offer = credit_offers::parse(profile_text(false).as_bytes())
        .expect("parses")
        .remove(0);
    let before = app.upgrade_spot(&sid).place;
    assert!(!before.contains("Claim cloud credit"), "CONTROL: {before}");
    app.claim_worker_landed(&sid, &offer);
    let place = app.upgrade_spot(&sid).place;
    assert!(place.contains("Workers ▸ Claim cloud credit"), "{place}");
    assert_eq!(app.tab_label_of_session(session), "Claim cloud credit");
    assert_eq!(worker_role(OfferId::ApiCredits), "Claim API credit");
    let now = Instant::now();
    assert!(CLAIMS.covers(offer.id, &offer.url, &[running(session)], now));
    assert!(
        !CLAIMS.covers(offer.id, &offer.url, &[ended(session)], now),
        "its hold goes with it"
    );
}

/// WHAT A CLAIM WAITS ON IS ONE RECORD IN MESSAGES: the same words again record nothing, and new
/// words reword that record rather than add one. Never an alert: it is a record.
#[test]
fn what_a_claim_waits_on_is_one_record_reworded_not_repeated() {
    let mut app = crate::App::headless_for_test();
    let ours = |app: &crate::App| -> Vec<(String, Vec<String>)> {
        app.messages
            .log()
            .records()
            .filter(|r| {
                r.key.as_deref() == Some(crate::message_reporters::CREDIT_CLAIM_NOT_READY_KEY)
            })
            .map(|r| (r.title.clone(), r.detail.clone()))
            .collect()
    };
    let first = "In place: Google Chrome.\nLeft: open Chrome and enable the Claude extension it \
                 adds; sign in to claude.ai in Chrome.";
    app.credit_claim_not_ready(first);
    app.credit_claim_not_ready(first);
    let records = ours(&app);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(
        records[0].1.len(),
        2,
        "a line for what is in place, one for what is left"
    );
    assert!(app.messages.live_rows().all(|row| {
        row.msg.key.as_deref() != Some(crate::message_reporters::CREDIT_CLAIM_NOT_READY_KEY)
    }));
    let second = "Left: Google Chrome, which atpkg's brew is installing now; sign in to \
                  claude.ai in Chrome.";
    app.credit_claim_not_ready(second);
    let records = ours(&app);
    assert_eq!(records.len(), 1, "reworded, not repeated: {records:?}");
    assert_eq!(records[0].1, [second.to_string()]);
}
