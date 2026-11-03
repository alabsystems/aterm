// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the claim act ([`super::step`]): its gate, its ledger and its worker's order. No
//! spawn, no network and no browser: the act's `start` is a recorder.

use std::cell::Cell;
use std::path::Path;
use std::time::Duration;

use aterm_agent::harness::credit_offers::{self, Offer};
use aterm_agent::harness::profile::NoProfile;
use aterm_agent::supervise::Autopilot;

use super::*;

const ORG: &str = "0f6c2b8e-1d3a-4c5e-9b7f-2a4d6e8c0b1a";

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

const NOW_MS: i64 = 1_792_022_400_000;

/// A profile whose cloud session credit is live (or claimed, where `claimed`).
fn profile_text(claimed: bool) -> String {
    format!(
        r#"{{"oauthAccount":{{"accountUuid":"acct-1","organizationUuid":"{ORG}","emailAddress":"owner@example.com"}},
"cachedGrowthBookFeatures":{{"tengu_swift_lynx":{{"endpoint":"/v1/code/promo/cloud_credit","webPath":"/code/claim-credit","claimLinkIncludesOrganization":true,"startup":{{"claimPageLine":"Claim your $250 cloud session credit"}}}}}},
"promoStartupStatusCache":{{"accountUuid":"acct-1","endpoint":"/v1/code/promo/cloud_credit","eligible":true,"claimed":{claimed}}}}}"#
    )
}

const OPEN: Gate = Gate {
    on: true,
    claude: true,
    autopilot: Autopilot::Land,
    claim_credits: true,
};

/// A scratch home: its profile and the harness state the ledger goes under.
struct Fixture {
    _dir: aterm_tempfile::TempDir,
    profile: std::path::PathBuf,
    ledger: std::path::PathBuf,
    cache: ProfileCache,
    parses: Cell<u32>,
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
        }
    }

    /// One look under `gate` at `now_ms`, and the offers it started.
    fn look(&self, gate: Gate, now_ms: i64) -> (Stepped, Vec<Offer>) {
        let read = |path: &Path| -> Result<Vec<Offer>, NoProfile> {
            self.parses.set(self.parses.get() + 1);
            credit_offers::read(path)
        };
        let mut started = Vec::new();
        let stepped = step(
            gate,
            &self.profile,
            &self.cache,
            &read,
            &self.ledger,
            now_ms,
            &mut |offer| started.push(offer),
        );
        (stepped, started)
    }
}

#[test]
fn a_shut_gate_looks_at_nothing_and_starts_nothing() {
    let fx = Fixture::new(&profile_text(false));
    for gate in [
        Gate { on: false, ..OPEN },
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
    assert_eq!(fx.look(OPEN, NOW_MS).1.len(), 1);
    let (stepped, _) = fx.look(OPEN, NOW_MS + 7 * DAY_MS - 1);
    assert_eq!(stepped, Stepped::NothingDue, "not before the week is out");
    let (_, again) = fx.look(OPEN, NOW_MS + 7 * DAY_MS);
    assert_eq!(again.len(), 1, "the one retry");
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
        task.contains("End with one reply starting `Nothing left:`"),
        "{task}"
    );

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
    let line = aterm_agent::harness::worker::line(
        claude,
        aterm_agent::harness::upgrade::Dialect::Zsh,
        &admitted,
        &crate::task_launch::worker_fresh(None, false, order.chrome),
    )
    .expect("a line");
    assert!(line.contains(" '--chrome' "), "{line}");
    assert!(
        line.contains(" '--permission-mode' 'default' "),
        "every box raised, whatever the person's settings: {line}"
    );
    assert_eq!(
        crate::task_launch::worker_fresh(None, false, true).permission_mode,
        Some("default")
    );
    let plain = aterm_agent::harness::worker::line(
        claude,
        aterm_agent::harness::upgrade::Dialect::Zsh,
        &admitted,
        &crate::task_launch::worker_fresh(None, false, false),
    )
    .expect("a line");
    assert!(
        !plain.contains("--chrome") && !plain.contains("--permission-mode"),
        "CONTROL, only a claim worker's: {plain}"
    );
}

/// A WORKER RUNS OUT OF ITS TIME BY ITS TASK RECORD: its trusted folder says it is a claim worker
/// and its record says how long it has stood, both of which a seamless update carries. CONTROL: a
/// moment short of its time; a worker of another folder; no trusted folder.
#[test]
fn a_claim_worker_runs_out_of_its_time_by_its_record() {
    assert_eq!(WORKER_FOR, Duration::from_secs(30 * 60));
    let claim = format!(
        "/state/{}/{}",
        aterm_agent::harness::cli::HARNESS,
        credit_offers::CLAIM_FOLDER
    );
    let moment = Duration::from_millis(1);
    assert!(ran_out(Some(&claim), WORKER_FOR));
    assert!(!ran_out(Some(&claim), WORKER_FOR - moment));
    assert!(!ran_out(Some("/Users//me/project"), WORKER_FOR));
    assert!(!ran_out(None, WORKER_FOR));
}

/// A WORKER STANDS FROM ITS START, AND ITS TAB FROM ITS EXIT: no later state moves the first, a
/// person's typing moves both, and an exit leaves the last reply its time to be read.
#[test]
fn a_workers_time_counts_from_its_start_its_exit_and_a_persons_typing() {
    use crate::task_record::TaskMark;
    let t0 = Instant::now();
    let later = |secs| t0 + Duration::from_secs(secs);
    let (first, _) = TaskMark::new(TaskState::Working).apply(None, false, t0, 1_000);
    let (flipped, _) =
        TaskMark::new(TaskState::Stalled).apply(Some(&first), false, later(120), 2_000);
    let (back, _) =
        TaskMark::new(TaskState::Working).apply(Some(&flipped), false, later(240), 3_000);
    assert_eq!(stood(&back, later(600), None), Duration::from_secs(600));
    assert_eq!(back.started_unix_ms, 1_000);
    assert_eq!(
        stood(&back, later(600), Some(Duration::from_secs(5))),
        Duration::from_secs(5)
    );
    let (exited, _) = TaskMark::new(TaskState::Exited).apply(Some(&back), false, later(600), 4_000);
    assert_eq!(stood(&exited, later(660), None), Duration::from_secs(60));
}

/// A WORKER'S END TAKES NOTHING FROM ANYONE AND ASKS NOTHING OF A PERSON: a tab in someone's hand
/// and a job that is not the worker's are left, a running program is asked to end and then made
/// to before its pane closes (a job in a closing pane raises a confirmation), its task is marked
/// `exited` once its program is gone so its tab stays to be read, as does a program's someone ran
/// there since, and a window's last tab stays once its program has ended.
#[test]
fn a_worker_ends_without_raising_a_confirmation() {
    use Ending::{ClosePane, Kill, Leave, MarkExited, Term};
    let running = Stand {
        in_hand: false,
        exited: false,
        job: true,
        worker: true,
        ends_window: false,
        updating: false,
        deaf: false,
        reading: false,
    };
    let last = Stand {
        ends_window: true,
        ..running
    };
    let deaf = Stand {
        deaf: true,
        ..running
    };
    let ended = Stand {
        job: false,
        ..running
    };
    assert_eq!(ending(running), Term);
    assert_eq!(ending(last), Term, "even its last tab");
    assert_eq!(ending(deaf), Kill);
    assert_eq!(
        ending(Stand {
            in_hand: true,
            ..deaf
        }),
        Leave
    );
    assert_eq!(
        ending(Stand {
            exited: true,
            ..deaf
        }),
        Leave,
        "not the worker's job"
    );
    assert_eq!(
        ending(Stand {
            worker: false,
            ..deaf
        }),
        Leave,
        "another program's job"
    );
    assert_eq!(ending(ended), MarkExited, "its reading time starts");
    assert_eq!(
        ending(Stand {
            ends_window: true,
            ..ended
        }),
        MarkExited,
        "a mark closes nothing"
    );
    let read = Stand {
        exited: true,
        ..ended
    };
    assert_eq!(ending(read), ClosePane);
    for (stand, why) in [
        (
            Stand {
                in_hand: true,
                ..ended
            },
            "someone has it",
        ),
        (
            Stand {
                in_hand: true,
                ..read
            },
            "someone has it",
        ),
        (
            Stand {
                ends_window: true,
                ..read
            },
            "a last tab asks",
        ),
        (
            Stand {
                reading: true,
                ..read
            },
            "the output of a program someone ran there is still to be read",
        ),
        (
            Stand {
                updating: true,
                ..read
            },
            "a close calls it off",
        ),
        (
            Stand {
                updating: true,
                ..ended
            },
            "a mark waits on its fence",
        ),
    ] {
        assert_eq!(ending(stand), Leave, "{why}");
    }
}

/// A live session's handle.
fn live(app: &crate::App, session: u64) -> crate::session_store::SessionHandle {
    let store = app.store.read().unwrap_or_else(PoisonError::into_inner);
    store
        .live_handles()
        .find(|h| h.local_id == session)
        .cloned()
        .expect("a live session")
}

/// Marks `session` a worker trusted with `folder`, and returns when its record began.
fn set_task(app: &crate::App, session: u64, folder: &str) -> Instant {
    let h = live(app, session);
    crate::task_record::mark_session_task(
        &h.ctx,
        h.local_id,
        &crate::task_record::TaskMark::new(TaskState::Working),
    )
    .expect("unfenced");
    let mut timeline = h
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    timeline.set_task_worker(aterm_phase::Program::Claude, Some(folder.to_owned()));
    timeline.task().expect("its record").started
}

/// The steps a look took for `session` alone.
fn of(steps: Vec<(u64, Ending)>, session: u64) -> Vec<(u64, Ending)> {
    steps.into_iter().filter(|(s, _)| *s == session).collect()
}

/// Publishes `program` as the one in front of `session`'s tab, as the program reader does.
fn in_front(app: &crate::App, session: u64, program: &str) {
    let handle = live(app, session);
    let mut timeline = handle
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let group = 4_000 + i32::try_from(session).expect("a small session");
    timeline.note_foreground_group(group);
    timeline.set_program(group, Some(program.to_owned()));
}

/// `session`'s task state and when it entered it.
fn record(app: &crate::App, session: u64) -> (TaskState, Instant) {
    let task = live(app, session)
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .task()
        .cloned()
        .expect("its record");
    (task.state, task.since)
}

/// Who the session log says closed `session`.
fn closed_by(app: &crate::App, session: u64) -> String {
    use aterm_slog::{Node, Op};
    let source = app
        .store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .closed_logs()
        .by_local(session)
        .expect("its closed log")
        .source();
    let log = source.into_log().expect("its log");
    let close = log
        .window()
        .rev()
        .find(|r| r.op == Op::Close && r.node == Node::SESSION)
        .expect("its close");
    close.by.word().to_string()
}

/// A new stub tab in the window.
fn new_tab(app: &mut crate::App) -> u64 {
    let session = app.next_session_id;
    app.push_stub_tab(crate::WindowId(0), crate::stub_session(session));
    session
}

/// THE LOOK ENDS A WORKER WHOSE PROCESS NEVER KNEW IT: once its record has stood its time the
/// worker's program is asked to end (`SIGTERM` first, however long a hand held it past its time;
/// `SIGKILL` follows only one that reached it, which a stub tab's never does); with its program
/// gone and no hand having marked its task, its task is marked `exited`, and its tab goes in the
/// harness's name once that mark has stood its time. CONTROL: a moment before each; a tab a
/// driver has; a `SIGTERM` the verb refused; a tab of another folder; a window's last tab.
#[test]
fn the_look_closes_a_claim_workers_tab_once_its_job_ended() {
    use Ending::{ClosePane, Leave, MarkExited, Term};
    let claim = format!(
        "/state/{}/{}",
        aterm_agent::harness::cli::HARNESS,
        credit_offers::CLAIM_FOLDER
    );
    let (time, deaf) = (WORKER_FOR, DEAF_FOR);
    let moment = Duration::from_millis(1);
    let mut app = crate::App::headless_for_test();
    let front = 0;
    let worker = new_tab(&mut app);
    let other = new_tab(&mut app);
    let since = set_task(&app, worker, &claim);
    let project = set_task(&app, other, "/Users//me/project");
    crate::feed_session_for_test(
        &app,
        worker,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07claude\n\x1b]133;C\x07",
    );
    assert_eq!(
        app.end_claim_workers(since + time),
        [(worker, Leave)],
        "a job no reader named yet is not signalled"
    );
    in_front(&app, worker, "claude");

    assert_eq!(app.end_claim_workers(since + time - moment), []);
    let driven = live(&app, worker);
    let drive = |on: bool| {
        *driven
            .ctx
            .turn_lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = on.then(|| crate::Lease::Drive {
            holder: crate::harness_host::holder(),
            expires_us: u64::MAX,
            conn: None,
            hard: true,
            here: false,
            holder_pid: None,
        });
    };
    drive(true);
    assert_eq!(
        app.end_claim_workers(since + time + deaf),
        [(worker, Leave)]
    );
    drive(false);
    assert_eq!(
        app.end_claim_workers(since + time + deaf),
        [(worker, Term)],
        "a program's first signal is SIGTERM, however long a hand held it past its time"
    );
    // A stub tab has no process group, so the verb refused that SIGTERM: no time to end counts
    // (a delivered one: `the_looks_delivered_sigterm_starts_the_time_its_sigkill_waits_for`).
    assert_eq!(
        app.end_claim_workers(since + time + 3 * deaf),
        [(worker, Term)],
        "a SIGTERM never delivered starts no time to end"
    );
    assert!(app.claim_termed.is_empty());
    assert!(
        app.pool.get(worker).is_some(),
        "a job's pane stays: closing it would raise a confirmation"
    );

    crate::feed_session_for_test(&app, worker, b"\x1b]133;D;0\x07");
    assert_eq!(app.end_claim_workers(since + time - moment), []);
    assert_eq!(app.end_claim_workers(since + time), [(worker, MarkExited)]);
    let (state, exited) = record(&app, worker);
    assert_eq!(state, TaskState::Exited);
    assert_eq!(
        app.end_claim_workers(exited + time - moment),
        [],
        "its tab stays to be read"
    );
    assert!(app.pool.get(worker).is_some());
    assert_eq!(app.end_claim_workers(exited + time), [(worker, ClosePane)]);
    assert!(
        app.pool.get(worker).is_none(),
        "its job ended, its tab went"
    );
    assert_eq!(closed_by(&app, worker), "harness");

    assert_eq!(app.end_claim_workers(project + time), []);
    assert!(app.pool.get(other).is_some(), "a tab of another folder");

    let second = set_task(&app, other, &claim);
    assert_eq!(app.end_claim_workers(second + time), [(other, MarkExited)]);
    let (_, exited) = record(&app, other);
    assert_eq!(app.end_claim_workers(exited + time), [(other, ClosePane)]);
    let last = set_task(&app, front, &claim);
    assert_eq!(app.end_claim_workers(last + time), [(front, MarkExited)]);
    let (_, exited) = record(&app, front);
    assert_eq!(app.end_claim_workers(exited + time), [(front, Leave)]);
    assert!(app.pool.get(front).is_some(), "a window's last tab asks");
}

/// A PERSON'S TAB IS THEIRS: their typing gives a worker its time again, its Autopilot goes Off
/// even with the tab left in front, and a job in the tab of a worker whose program exited is
/// theirs and never signalled, nor is another program's job in a worker's tab that nothing
/// marked exited.
#[test]
fn the_look_leaves_what_a_person_has_in_a_workers_tab() {
    use Ending::Leave;
    let claim = format!(
        "/state/{}/{}",
        aterm_agent::harness::cli::HARNESS,
        credit_offers::CLAIM_FOLDER
    );
    let mut app = crate::App::headless_for_test();
    let typed = new_tab(&mut app);
    let since = set_task(&app, typed, &claim);
    live(&app, typed)
        .ctx
        .human_input
        .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
    assert_eq!(app.end_claim_workers(since + WORKER_FOR), []);

    let exited = new_tab(&mut app);
    set_task(&app, exited, &claim);
    let h = live(&app, exited);
    crate::task_record::mark_session_task(
        &h.ctx,
        h.local_id,
        &crate::task_record::TaskMark::new(TaskState::Exited),
    )
    .expect("unfenced");
    crate::feed_session_for_test(
        &app,
        exited,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07make\n\x1b]133;C\x07",
    );
    let at = h
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .task()
        .expect("its record")
        .since;
    assert_eq!(app.end_claim_workers(at + WORKER_FOR), [(exited, Leave)]);

    let taken = new_tab(&mut app);
    let began = set_task(&app, taken, &claim);
    crate::feed_session_for_test(
        &app,
        taken,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07npm run dev\n\x1b]133;C\x07",
    );
    // npm's leader names itself `node`, and the worker's last Claude Code frame, still on the
    // screen, names the reader.
    in_front(&app, taken, "node");
    {
        let handle = live(&app, taken);
        let mut timeline = handle
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        timeline.publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
        assert_eq!(
            crate::harness_host::identified_agent(timeline.agent()),
            Some(aterm_phase::Program::Claude),
            "PRECONDITION: the screen names Claude Code for the runtime in front"
        );
    }
    assert_eq!(
        of(app.end_claim_workers(began + WORKER_FOR + DEAF_FOR), taken),
        [(taken, Leave)],
        "a person's own job, with nothing marked exited before it and the worker's frame on screen"
    );
    assert_eq!(record(&app, taken).0, TaskState::Working);
    let autopilot = h
        .ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .autopilot_word();
    assert_eq!(autopilot, Some(Autopilot::Off));
}

/// A LANDED WORKER KEEPS ITS RESULT, AND A DEADLINE'S ASK GOES WITH ITS PROGRAM: the look's exit
/// mark carries a done record's result as the harness's own exit mark does, so `result` still
/// answers while its tab stays to be read, and takes down the held ask a program that ended no
/// longer owes. CONTROL: a record that moved since the look read it is not marked.
#[test]
fn the_looks_exit_mark_keeps_a_result_and_takes_down_a_held_ask() {
    let claim = format!(
        "/state/{}/{}",
        aterm_agent::harness::cli::HARNESS,
        credit_offers::CLAIM_FOLDER
    );
    let mut app = crate::App::headless_for_test();
    let landed = new_tab(&mut app);
    let began = set_task(&app, landed, &claim);
    let h = live(&app, landed);
    let mut done = crate::task_record::TaskMark::new(TaskState::Done);
    done.why = Some(crate::task_record::WHY_LANDED);
    done.result_bytes = 17;
    done.result_path = Some(std::path::PathBuf::from("/s/tasks/s-1-1.result.md"));
    crate::task_record::mark_session_task(&h.ctx, landed, &done).expect("unfenced");
    assert_eq!(
        app.end_claim_workers(began + WORKER_FOR),
        [(landed, Ending::MarkExited)]
    );
    let after = h
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .task()
        .cloned()
        .expect("its record");
    assert_eq!(after.state, TaskState::Exited);
    assert_eq!(after.result_bytes, 17);
    assert_eq!(
        after.result_path.as_deref(),
        Some(Path::new("/s/tasks/s-1-1.result.md"))
    );

    // A failed conclusion's reply is kept as a landing's is, and its `why=` with it.
    let failed = new_tab(&mut app);
    let began = set_task(&app, failed, &claim);
    let h = live(&app, failed);
    let mut gave_up = crate::task_record::TaskMark::new(TaskState::Failed);
    gave_up.why = Some(crate::task_record::WHY_REPEATED);
    gave_up.result_bytes = 23;
    gave_up.result_path = Some(std::path::PathBuf::from("/s/tasks/s-2-1.result.md"));
    crate::task_record::mark_session_task(&h.ctx, failed, &gave_up).expect("unfenced");
    assert_eq!(
        of(app.end_claim_workers(began + WORKER_FOR), failed),
        [(failed, Ending::MarkExited)]
    );
    let after = h
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .task()
        .cloned()
        .expect("its record");
    assert_eq!(
        (after.state, after.why, after.result_bytes),
        (
            TaskState::Exited,
            Some(crate::task_record::WHY_REPEATED),
            23
        )
    );
    assert_eq!(
        after.result_path.as_deref(),
        Some(Path::new("/s/tasks/s-2-1.result.md"))
    );

    let held = new_tab(&mut app);
    let began = set_task(&app, held, &claim);
    let h = live(&app, held);
    let mut stalled = crate::task_record::TaskMark::new(TaskState::Stalled);
    stalled.why = Some("held");
    let ask = format!("{}a box", aterm_agent::supervise::HELD_HEAD);
    crate::task_record::mark_session_task_with(
        &h.ctx,
        held,
        Some(&stalled),
        crate::task_record::AttentionEdit::Raise(&ask),
        Duration::ZERO,
        |_, _| true,
    )
    .expect("unfenced");
    let attention = |ctx: &crate::SessionCtx| {
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attention_owners
            .get(crate::harness_host::ATTENTION_OWNER)
            .map(str::to_owned)
    };
    assert_eq!(
        attention(&h.ctx).as_deref(),
        Some(ask.as_str()),
        "PRECONDITION"
    );
    assert_eq!(
        of(app.end_claim_workers(began + WORKER_FOR), held),
        [(held, Ending::MarkExited)]
    );
    assert_eq!(attention(&h.ctx), None, "the ask went with its program");
    let ended = live(&app, held)
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .task()
        .cloned()
        .expect("its record");
    assert_eq!(
        (ended.state, ended.why),
        (TaskState::Exited, Some("held")),
        "it ended where it stalled, and its report says so"
    );
    assert_eq!(
        crate::task_record::failed_words(ended.state, ended.why),
        "held at a screen aterm does not know"
    );

    let moved = new_tab(&mut app);
    set_task(&app, moved, &claim);
    let h = live(&app, moved);
    let read = h
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .task()
        .cloned()
        .expect("its record");
    let mut turn = crate::task_record::TaskMark::new(TaskState::Working);
    turn.why = Some("turn");
    crate::task_record::mark_session_task(&h.ctx, moved, &turn).expect("unfenced");
    assert_eq!(super::mark_ended(&h.ctx, moved, &read), Ok(None));
    assert_eq!(record(&app, moved).0, TaskState::Working);
}

/// A WORKER YOU CAN SEE IS LEFT ALONE: in a focused window, a claim worker in a split beside the
/// focused pane is on screen and is not ended. CONTROL: the same worker in a tab behind.
#[test]
fn a_claim_worker_in_a_visible_split_is_left_alone() {
    let claim = format!(
        "/state/{}/{}",
        aterm_agent::harness::cli::HARNESS,
        credit_offers::CLAIM_FOLDER
    );
    let mut app = crate::App::headless_for_test();
    let window = crate::WindowId(0);
    let ws = app.windows.get_mut(&window).expect("its window");
    ws.focused = true;
    ws.os_window_for_test = true;
    let worker = 0;
    let began = set_task(&app, worker, &claim);
    let beside = app.split_active_stub_tab(window);
    assert_eq!(app.focused_session_id(window), Some(beside), "PRECONDITION");
    assert_eq!(
        app.end_claim_workers(began + WORKER_FOR),
        [(worker, Ending::Leave)]
    );

    let behind = new_tab(&mut app);
    let began = set_task(&app, behind, &claim);
    app.switch_tab_in(window, 0);
    assert_eq!(
        of(app.end_claim_workers(began + WORKER_FOR), behind),
        [(behind, Ending::MarkExited)]
    );
}

/// A RESTORE GIVES A WORKER ITS LIMIT BACK: the record that held its time did not cross, so a
/// claim worker it brings back starts a new one, `working why=restored`, which no startup or held
/// deadline reads as a worker just spawned, and ends by it. CONTROL: an agent of another folder
/// gets no record.
#[test]
fn a_restored_claim_worker_has_a_limit_again() {
    use crate::harness_host::StartupState;
    let claim = format!(
        "/state/{}/{}",
        aterm_agent::harness::cli::HARNESS,
        credit_offers::CLAIM_FOLDER
    );
    let mut app = crate::App::headless_for_test();
    let worker = new_tab(&mut app);
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
        app.end_claim_workers(record.started + WORKER_FOR),
        [(worker, Ending::MarkExited)]
    );
    assert_eq!(claim_worker(&ctx), Some((claim, true)), "its worker ended");
}

/// A TAB WHOSE WORKER HAD ENDED STAYS A PERSON'S ACROSS A RESTORE: the layout names each claim
/// worker's folder and whether its task was marked `exited`, so a restore gives an ended
/// worker's tab an `exited` record, not a new worker's time, an agent on its leaf or not (the
/// restore's own path: `app_restore_tests::a_restored_claim_worker_gets_a_task_record`), and a
/// Claude Code found or brought back there is not signalled; the tab closes once that mark has
/// stood its time and that Claude Code was last seen running [`WORKER_FOR`] ago. CONTROL: the
/// layout of a running worker's tab names its folder and nothing ended; a tab with no worker and
/// a task that ended in another folder name none.
#[test]
fn a_restore_leaves_a_program_in_an_ended_workers_tab() {
    use Ending::{ClosePane, Leave};
    let claim = format!(
        "/state/{}/{}",
        aterm_agent::harness::cli::HARNESS,
        credit_offers::CLAIM_FOLDER
    );
    let mut app = crate::App::headless_for_test();
    let front = 0;
    let running = new_tab(&mut app);
    let ended = new_tab(&mut app);
    let began = set_task(&app, ended, &claim);
    assert_eq!(
        app.end_claim_workers(began + WORKER_FOR),
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
            stored_login: false,
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

    let restored = new_tab(&mut app);
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
        of(
            app.end_claim_workers(record.since + WORKER_FOR + DEAF_FOR),
            restored
        ),
        [(restored, Leave)],
        "a person's Claude Code in a tab whose worker had ended"
    );
    crate::feed_session_for_test(&app, restored, b"\x1b]133;D;0\x07");
    let seen = record.since + WORKER_FOR + DEAF_FOR;
    assert_eq!(
        of(
            app.end_claim_workers(seen + WORKER_FOR - Duration::from_secs(1)),
            restored
        ),
        [(restored, Leave)],
        "its output is still to be read"
    );
    assert_eq!(
        of(app.end_claim_workers(seen + WORKER_FOR), restored),
        [(restored, ClosePane)]
    );
}

/// A PROGRAM SOMEONE RUNS IN AN ENDED WORKER'S TAB KEEPS ITS OUTPUT TO BE READ: every look sees it
/// running, its tab's time out or not, and the tab closes only [`WORKER_FOR`] after the last look
/// that saw it, not as soon as it ends. CONTROL: a tab with nothing run there closes once its exit
/// mark has stood that time.
#[test]
fn a_program_run_in_an_ended_workers_tab_keeps_its_output_to_be_read() {
    use Ending::{ClosePane, Leave, MarkExited};
    let claim = format!(
        "/state/{}/{}",
        aterm_agent::harness::cli::HARNESS,
        credit_offers::CLAIM_FOLDER
    );
    let minutes = |n: u64| Duration::from_secs(n * 60);
    let mut app = crate::App::headless_for_test();
    let used = new_tab(&mut app);
    let idle = new_tab(&mut app);
    let began = set_task(&app, used, &claim);
    let idle_began = set_task(&app, idle, &claim);
    let marked = app.end_claim_workers(began.max(idle_began) + WORKER_FOR);
    for tab in [used, idle] {
        assert_eq!(of(marked.clone(), tab), [(tab, MarkExited)], "PRECONDITION");
    }
    let exited = record(&app, used).1;
    let idle_exited = record(&app, idle).1;
    crate::feed_session_for_test(
        &app,
        used,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07make\n\x1b]133;C\x07",
    );
    assert_eq!(
        of(app.end_claim_workers(exited + minutes(5)), used),
        [],
        "its time is not out, and the look sees the program"
    );
    crate::feed_session_for_test(&app, used, b"\x1b]133;D;0\x07");
    let steps = app.end_claim_workers(exited.max(idle_exited) + WORKER_FOR);
    assert_eq!(
        of(steps.clone(), used),
        [(used, Leave)],
        "ended within its time: its output is still to be read"
    );
    assert_eq!(
        of(steps, idle),
        [(idle, ClosePane)],
        "CONTROL: nothing run there"
    );
    assert_eq!(
        of(
            app.end_claim_workers(exited + minutes(5) + WORKER_FOR),
            used
        ),
        [(used, ClosePane)]
    );
}

/// A CLICK IS NOT TYPING: the worker's time starts again when a person types in its tab, not when
/// they click or scroll there, after a seamless update from a build that carried no awake typing
/// row too, and the typing age is awake time, as the record's is, a typing carried across a
/// seamless update after the Mac slept too. CONTROL: a key press.
#[test]
fn only_typing_gives_a_claim_worker_its_time_again() {
    use crate::human_input::Gesture;
    let claim = format!(
        "/state/{}/{}",
        aterm_agent::harness::cli::HARNESS,
        credit_offers::CLAIM_FOLDER
    );
    let mut app = crate::App::headless_for_test();
    for (gesture, after) in [
        (Gesture::Other, vec![Ending::MarkExited]),
        (Gesture::Typed, vec![]),
    ] {
        let tab = new_tab(&mut app);
        let since = set_task(&app, tab, &claim);
        live(&app, tab)
            .ctx
            .human_input
            .note(crate::metrics::now_us(), gesture);
        assert_eq!(
            of(app.end_claim_workers(since + WORKER_FOR), tab)
                .into_iter()
                .map(|(_, step)| step)
                .collect::<Vec<_>>(),
            after,
            "{gesture:?}"
        );
    }
    // Carried across a seamless update that followed a night's sleep: the gesture and typing ages
    // the outgoing process carried (the typing's counting the sleep), its awake typing row where
    // it wrote one, and a gesture made here; the worker's record crossed on awake time.
    const SLEPT_MS: u64 = 8 * 3600 * 1000;
    let minutes = |n: u64| n * 60_000;
    let mut carried =
        |gesture_ms: u64, typed_ms: u64, awake_ms: Option<u64>, here: Option<Gesture>| {
            let tab = new_tab(&mut app);
            let since = set_task(&app, tab, &claim);
            let now_us = crate::metrics::now_us();
            let input = &live(&app, tab).ctx.human_input;
            input.carry(gesture_ms, now_us);
            input.carry_typed_at(typed_ms + SLEPT_MS, now_us, None);
            if let Some(awake_ms) = awake_ms {
                input.carry_typed_awake(awake_ms, now_us);
            }
            if let Some(gesture) = here {
                input.note(now_us, gesture);
            }
            of(app.end_claim_workers(since + WORKER_FOR), tab)
                .into_iter()
                .map(|(_, step)| step)
                .collect::<Vec<_>>()
        };
    assert_eq!(
        carried(minutes(1), minutes(1), Some(minutes(1)), None),
        [],
        "typed a minute ago in awake time, whatever the sleep before the update"
    );
    assert_eq!(
        carried(10_000, minutes(31), Some(minutes(31)), None),
        [Ending::MarkExited],
        "a click younger than the typing: the awake row, not the click, ends the worker"
    );
    assert_eq!(
        carried(10_000, minutes(31), None, None),
        [],
        "CONTROL: no awake row (an older outgoing build): the typing is bounded by the click it \
         carried, so the worker lives longer, never ends early"
    );
    assert_eq!(
        carried(minutes(31), minutes(31), None, Some(Gesture::Other)),
        [Ending::MarkExited],
        "a click made here after an older build's carry is no typing"
    );
    assert_eq!(
        carried(minutes(31), minutes(31), None, Some(Gesture::Typed)),
        [],
        "CONTROL: typing here"
    );
}

/// `SIGKILL` IS NEVER A PROGRAM'S FIRST SIGNAL: the time to end counts from the first `SIGTERM`
/// the verb delivered to the process group now in front, for the record now standing. CONTROL:
/// a refused one records nothing; a later one to the same group keeps the first; another
/// program come to the front, or a new record, starts again; no foreground group is never deaf.
#[test]
fn the_time_to_end_counts_from_the_first_delivered_sigterm() {
    let mut termed = super::TermedBySession::new();
    let t0 = Instant::now();
    let (record, newer) = (t0, t0 + Duration::from_secs(1));
    let at = t0 + WORKER_FOR;
    let moment = Duration::from_millis(1);
    let deaf = |termed: &super::TermedBySession, session, record, group, now| {
        super::deaf(termed.get(&session), record, group, now)
    };
    super::note_term(
        &mut termed,
        7,
        record,
        at,
        "ERR no foreground process group\n",
    );
    assert!(termed.is_empty(), "a refused SIGTERM");
    assert!(!deaf(&termed, 7, record, 4001, at + DEAF_FOR * 9));

    super::note_term(
        &mut termed,
        7,
        record,
        at,
        "OK signalled pgrp 4001 discarded=3\n",
    );
    super::note_term(
        &mut termed,
        7,
        record,
        at + DEAF_FOR / 2,
        "OK signalled pgrp 4001\n",
    );
    assert_eq!(
        termed.get(&7),
        Some(&super::Termed {
            record,
            group: 4001,
            at
        })
    );
    assert!(!deaf(&termed, 7, record, 4001, at + DEAF_FOR - moment));
    assert!(
        deaf(&termed, 7, record, 4001, at + DEAF_FOR),
        "from the first"
    );
    assert!(
        !deaf(&termed, 8, record, 4001, at + DEAF_FOR),
        "another tab"
    );
    assert!(
        !deaf(&termed, 7, newer, 4001, at + DEAF_FOR),
        "a new record's program was sent nothing"
    );
    assert!(
        !deaf(&termed, 7, record, 4002, at + DEAF_FOR),
        "another program came to the front: it was sent nothing"
    );
    assert!(
        !deaf(&termed, 7, record, -1, at + DEAF_FOR),
        "no foreground group"
    );

    let later = at + DEAF_FOR * 3;
    super::note_term(&mut termed, 7, record, later, "OK signalled pgrp 4002\n");
    assert!(!deaf(&termed, 7, record, 4002, later + DEAF_FOR - moment));
    assert!(
        deaf(&termed, 7, record, 4002, later + DEAF_FOR),
        "the new program's own first SIGTERM"
    );
    super::note_term(&mut termed, 7, newer, later, "OK signalled pgrp 4002\n");
    assert!(deaf(&termed, 7, newer, 4002, later + DEAF_FOR));
    assert!(!deaf(&termed, 7, record, 4002, later + DEAF_FOR));
    super::note_term(&mut termed, 7, newer, later, "OK signalled pid 4003\n");
    assert_eq!(
        termed.get(&7).map(|sent| sent.group),
        Some(4002),
        "a reply that names no group records nothing"
    );
}

/// THE LOOK'S OWN DELIVERED SIGTERM STARTS THE TIME ITS SIGKILL WAITS FOR: against a real pty
/// whose foreground program ignores `SIGTERM`, the look sends `SIGTERM` first and records the
/// group it reached, sends it again until [`DEAF_FOR`] has passed since, then `SIGKILL`, which
/// ends it; once a person types in the tab, nothing of that signal stays.
#[test]
#[cfg(target_os = "macos")]
fn the_looks_delivered_sigterm_starts_the_time_its_sigkill_waits_for() {
    use Ending::{Kill, Term};
    let claim = format!(
        "/state/{}/{}",
        aterm_agent::harness::cli::HARNESS,
        credit_offers::CLAIM_FOLDER
    );
    let (master, slave, mut job, group) =
        crate::input_stall::tests::ready_job("trap '' TERM; printf R; exec /bin/sleep 120");
    let mut app = crate::App::headless_for_test();
    let worker = app.next_session_id;
    let mut stub = crate::stub_session_with_sink(
        worker,
        std::sync::Arc::new(aterm_session::sink::SinkWriter::new(master)),
    );
    stub.master = master;
    app.push_stub_tab(crate::WindowId(0), stub);
    assert_eq!(live(&app, worker).master, master, "PRECONDITION");
    let since = set_task(&app, worker, &claim);
    crate::feed_session_for_test(
        &app,
        worker,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07claude\n\x1b]133;C\x07",
    );
    in_front(&app, worker, "claude");
    let moment = Duration::from_millis(1);

    let termed = since + WORKER_FOR;
    assert_eq!(app.end_claim_workers(termed), [(worker, Term)]);
    assert_eq!(
        app.claim_termed.get(&worker),
        Some(&super::Termed {
            record: since,
            group,
            at: termed
        }),
        "the group the SIGTERM reached"
    );
    assert_eq!(
        job.0.try_wait().expect("its state"),
        None,
        "it ignores SIGTERM"
    );
    assert_eq!(
        app.end_claim_workers(termed + DEAF_FOR - moment),
        [(worker, Term)]
    );
    assert_eq!(app.end_claim_workers(termed + DEAF_FOR), [(worker, Kill)]);
    let ended = job.0.wait().expect("reaped");
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&ended),
        Some(libc::SIGKILL)
    );

    live(&app, worker)
        .ctx
        .human_input
        .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
    assert_eq!(app.end_claim_workers(termed + DEAF_FOR), []);
    assert!(
        app.claim_termed.is_empty(),
        "a worker a person typed back into its time owes no signal"
    );
    // SAFETY: the test's own pty fds, open until here and used by nothing after.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}
