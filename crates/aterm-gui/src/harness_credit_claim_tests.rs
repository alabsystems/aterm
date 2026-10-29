// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the claim act ([`super::step`]): its gate, its ledger and its worker's order. No
//! spawn, no network and no browser: the act's `start` is a recorder.

use std::cell::Cell;
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_agent::harness::credit_offers::{self, Offer, OfferId};
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

#[test]
fn a_live_offer_is_recorded_before_its_worker_and_tried_once() {
    let fx = Fixture::new(&profile_text(false));
    let (stepped, started) = fx.look(OPEN, NOW_MS);
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].id, OfferId::CloudCredit);
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

/// THE CLAIM WORKERS THIS PROCESS STARTED RUN [`WORKER_FOR`]: a worker is taken as expired once
/// it has run its time, and once only; past [`RUNNING_ROWS`] the earliest started is taken too.
/// CONTROL: a moment short of its time, and a worker started later, are not.
#[test]
fn a_claim_worker_runs_thirty_minutes() {
    assert_eq!(WORKER_FOR, Duration::from_secs(30 * 60));
    let running = Running::new();
    let t0 = Instant::now();
    running.started("s-1", t0);
    running.started("s-2", t0 + Duration::from_secs(600));
    assert!(
        running
            .expired(t0 + WORKER_FOR - Duration::from_secs(1))
            .is_empty()
    );
    assert_eq!(running.expired(t0 + WORKER_FOR), vec!["s-1".to_owned()]);
    assert!(running.expired(t0 + WORKER_FOR).is_empty(), "told once");
    assert_eq!(
        running.expired(t0 + WORKER_FOR + Duration::from_secs(600)),
        vec!["s-2".to_owned()]
    );

    let running = Running::new();
    for n in 0..=RUNNING_ROWS as u64 {
        running.started(&format!("s-{n}"), t0 + Duration::from_millis(n));
    }
    assert_eq!(running.expired(t0), vec!["s-0".to_owned()], "past the cap");
    assert!(running.expired(t0 + Duration::from_secs(1)).is_empty());
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
        &crate::task_launch::worker_fresh(None, false, false, order.chrome),
    )
    .expect("a line");
    assert!(line.contains(" '--chrome' "), "{line}");
    assert!(
        line.contains(" '--permission-mode' 'default' "),
        "every box raised, whatever the person's settings: {line}"
    );
    assert_eq!(
        crate::task_launch::worker_fresh(None, false, false, true).permission_mode,
        Some("default")
    );
    let plain = aterm_agent::harness::worker::line(
        claude,
        aterm_agent::harness::upgrade::Dialect::Zsh,
        &admitted,
        &crate::task_launch::worker_fresh(None, false, false, false),
    )
    .expect("a line");
    assert!(
        !plain.contains("--chrome") && !plain.contains("--permission-mode"),
        "CONTROL, only a claim worker's: {plain}"
    );
    assert_eq!(
        crate::task_launch::worker_fresh(None, false, true, false).permission_mode,
        Some("default"),
        "the disk-free worker's, as before"
    );
}

/// The claim worker's folder is the claim folder under the harness state directory, its owner's
/// alone, and the supervisor reads the order's trusted folder as a claim worker's, which fences
/// its boxes. CONTROL: the harness state directory itself is no claim worker's.
#[test]
fn a_claim_worker_folder_is_its_owners_alone() {
    let root = aterm_tempfile::tempdir().expect("state");
    let state = root
        .path()
        .join("harness")
        .join(aterm_agent::harness::cli::HARNESS);
    let folder = worker_folder_under(&state).expect("a folder");
    assert!(folder.starts_with(&state) && folder.is_dir());
    assert!(credit_offers::is_claim_folder(&folder), "{folder:?}");
    assert!(!credit_offers::is_claim_folder(&state), "CONTROL");
    let offer = credit_offers::parse(profile_text(false).as_bytes())
        .expect("parses")
        .remove(0);
    let trusted = claim_order(&offer, Some(&folder))
        .trusted_cwd
        .expect("a trusted folder");
    assert!(
        credit_offers::is_claim_folder(Path::new(&trusted)),
        "{trusted}"
    );
    assert!(!aterm_agent::harness::disk::fence::is_worker_folder(
        Path::new(&trusted)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&folder)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }
}

#[test]
fn a_claim_is_a_fact_for_later_naming_the_offer_and_the_tab() {
    let offer = credit_offers::parse(profile_text(false).as_bytes())
        .expect("parses")
        .remove(0);
    let entry = crate::message_reporters::credit_claim_started_entry(&offer, "s-42");
    assert_eq!(
        entry.family().mode(),
        aterm_messages::exceptions::Mode::Event
    );
    let msg = entry.message().expect("an entry that reads");
    let shown = format!("{msg:?}");
    assert!(shown.contains("@s-42"), "{shown}");
    assert!(
        shown.contains("Claim your $250 cloud session credit"),
        "{shown}"
    );
    let failed =
        crate::message_reporters::credit_claim_not_started_entry(&offer, "needs shell integration");
    let shown = format!("{:?}", failed.message().expect("an entry that reads"));
    assert!(shown.contains(&offer.url), "{shown}");
    assert_ne!(entry.key(), failed.key());
}
