// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIMIT AN ACCOUNT MOVE LEFT, AT THE LOOP. The live sequence of 2026-11-16 (Claude Code
//! 2.1.323): the turn ended on m2's limit, the host relaunched the conversation under m2-3
//! (`done:resumed`, nothing typed), and the resumed screen drew m2's limit again, its transcript
//! notice written before the move was made. The loop opened a limit episode for it (the host
//! told `limited`, a wall wait booked) and typed "The usage limit has cleared", which no limit
//! had. Here the left limit opens no episode and books no wait, the move's own continuation is
//! typed once in the move's words, and the turn it opens proves the moved-to account; a limit
//! that account meets itself still opens its episode and strikes it. The move's own second dates
//! the left limit, so none of this hangs on when the host names the moved-to process.

use super::*;
use crate::supervise::approvals::{Ledger, RULE_WALL_WAIT, RULE_WALL_WAIT_END};
use crate::supervise::policy::turn_end::{MOVED_TEXT, RULE_LIMIT_RESUME};

/// [`limited_screen`]'s reset, 7:30pm Pacific on [`ACCOUNT_NOW`]'s day.
const RESET: i64 = ACCOUNT_NOW + 8700;

/// The process the move ended, started two hours before the loop's clock.
const OLD_START: i64 = ACCOUNT_NOW - 7200;

/// The left account's limit notice, written in that process's last turn.
const NOTICE: i64 = ACCOUNT_NOW - 600;

/// The moved-to process, started as the move was made (the loop's clock stands at
/// [`ACCOUNT_NOW`] until the continuation's turn).
const NEW_START: i64 = ACCOUNT_NOW;

/// The loop's clock when the continuation's turn ends.
const TURN_END: i64 = ACCOUNT_NOW + 300;

/// The moved-to account's own limit notice, written in the continuation's turn.
const OWN_NOTICE: i64 = TURN_END - 60;

const TO: &str = "m2-3";

/// Claude Code 2.1.294's measured session-limit transcript, worded as [`limited_screen`] and its
/// rows written at `at`.
fn limit_tail(at: i64) -> String {
    let stamp = crate::harness::usage::rfc3339_utc(at);
    include_str!("fixtures/claude-2.1.294-transcript-session-limit.jsonl")
        .lines()
        .skip(1)
        .filter(|line| !line.contains("\"subtype\":\"informational\""))
        .map(|line| {
            let mut row = line
                .replace("resets 11:33am", "resets 7:30pm")
                .replace("1791484405", &RESET.to_string());
            for ms in ["921", "951", "952", "953"] {
                row = row.replace(
                    &format!("\"2026-10-08T16:34:58.{ms}Z\""),
                    &format!("\"{stamp}\""),
                );
            }
            row
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What one run came to.
struct Run<T> {
    /// What the steps returned.
    out: T,
    /// What the host was told of limit episodes ([`IdleHost::limited`]), in order.
    limits: Vec<bool>,
    /// What the host was told of the accounts, in order.
    said: Vec<String>,
    /// What the steps said and noted.
    heard: Vec<String>,
    /// The ledger's wall-wait rows and their ends, in order.
    waits: Vec<String>,
    /// The turns typed.
    turns: usize,
}

impl<T> Run<T> {
    fn log(&self) -> String {
        format!("{:#?}\n{:#?}\n{:#?}", self.heard, self.said, self.waits)
    }

    /// The `limited(true)` the host was told.
    fn opened(&self) -> usize {
        self.limits.iter().filter(|open| **open).count()
    }

    /// Whether the ledger's last wall-wait row is an end: no wait stands booked.
    fn no_wait_stands(&self) -> bool {
        self.waits
            .last()
            .is_none_or(|l| l.contains(&format!("\"rule_id\":\"{RULE_WALL_WAIT_END}\"")))
    }
}

/// A session over `host`, the transcript's limit notice written at [`NOTICE`], its steps `steps`
/// run on the loop's clock at [`ACCOUNT_NOW`].
fn rig<T>(
    tag: &str,
    host: &Arc<AccountHost>,
    steps: impl FnOnce(&mut Session<'_, Mock>, &SuperviseOpts, &mut Heard) -> T,
) -> Run<T> {
    let (dir, ledger) = ledger_at(tag);
    *host.tail.lock().unwrap() = Some(limit_tail(NOTICE));
    let opts = account_opts(host);
    let mut m = Mock::new(vec![limited_screen(); 12]);
    let mut heard = Heard::default();
    let out = {
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.stall_host.clone_from(&opts.idle_host);
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        s.set_approval_ledger(Some(ledger.clone()));
        steps(&mut s, &opts, &mut heard)
    };
    let waits = std::fs::read_to_string(&ledger)
        .unwrap_or_default()
        .lines()
        .filter(|l| {
            l.contains(&format!("\"rule_id\":\"{RULE_WALL_WAIT}\""))
                || l.contains(&format!("\"rule_id\":\"{RULE_WALL_WAIT_END}\""))
        })
        .map(str::to_string)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    let limits = host.limits.lock().unwrap().clone();
    let mut said_and_noted = heard.said;
    said_and_noted.extend(heard.noted);
    Run {
        out,
        limits,
        said: host.all(),
        heard: said_and_noted,
        waits,
        turns: count(&m, "@s-1 turn"),
    }
}

/// The point `seq` showing `rows`: decided, its episode settled (`limit_after`), then its
/// decision executed. The decision, and whether an episode stood after the point's settling.
fn step(
    s: &mut Session<'_, Mock>,
    opts: &SuperviseOpts,
    heard: &mut Heard,
    rows: Vec<String>,
    seq: u64,
    worked: Option<Duration>,
) -> (TurnEndAction, bool) {
    let mut deadline = Instant::now() + Duration::from_secs(3600);
    let point = point_at(s, rows, seq);
    let action = s
        .turn_end_at_point(&point, worked, opts)
        .expect("a decision");
    s.limit_after(&point, &mut deadline, true, heard)
        .expect("the point's episode");
    let episode = s.limit.is_some();
    s.turn_end_execute(&point, action.clone(), opts, &[], heard)
        .expect("the decision is executed");
    (action, episode)
}

/// A's own limit at point 101, its turn worked: the host moves the session there.
fn at_as_limit(s: &mut Session<'_, Mock>, opts: &SuperviseOpts, heard: &mut Heard) {
    let (action, episode) = step(
        s,
        opts,
        heard,
        limited_screen(),
        101,
        Some(Duration::from_secs(180)),
    );
    assert!(
        matches!(&action, TurnEndAction::Restart { why: Restart::Account { to, at: AccountAt::Wall }, .. } if to == TO),
        "A's own limit moves the session: {action:?}"
    );
    assert!(episode, "A met its limit: its episode opens");
}

fn moved_on() -> TurnEndAction {
    TurnEndAction::Type {
        text: MOVED_TEXT.to_string(),
        rule_id: RULE_LIMIT_RESUME,
    }
}

/// Whether `run` typed the move's continuation once at point `seq`, in the move's words.
fn continued_in_the_moves_words<T>(run: &Run<T>, seq: u64) -> bool {
    run.heard.iter().any(|l| {
        l.starts_with(&format!("CONTINUED seq={seq} "))
            && l.contains(&format!("rule={RULE_LIMIT_RESUME}"))
            && l.contains(MOVED_TEXT)
            && !l.contains("cleared")
    }) && run.turns == 1
}

/// The continuation's turn, ended at point 107 on a served reply (`own`: on the moved-to
/// account's own limit, its notice written in that turn, at [`TURN_END`]).
fn the_turn_it_opens(
    s: &mut Session<'_, Mock>,
    opts: &SuperviseOpts,
    heard: &mut Heard,
    host: &AccountHost,
    own: bool,
) {
    s.set_clock(TURN_END, PDT);
    if own {
        *host.tail.lock().unwrap() = Some(limit_tail(OWN_NOTICE));
        let mut deadline = Instant::now() + Duration::from_secs(3600);
        let met = point_at(s, limited_screen(), 107);
        let _ = s
            .turn_end_at_point(&met, Some(Duration::from_secs(120)), opts)
            .expect("a decision");
        s.limit_after(&met, &mut deadline, true, heard)
            .expect("B's episode");
    } else {
        let served = point_at(s, ended("Stage 4 resumed; the suites are green."), 107);
        let _ = s
            .turn_end_at_point(&served, Some(Duration::from_secs(240)), opts)
            .expect("a decision");
    }
}

/// A session at A's limit moved to [`TO`] by the host (`done:resumed`), then the resumed screen
/// with A's limit drawn again, decided and executed; then the turn the continuation opened ends,
/// on a served reply or (`own`) on the moved-to account's own limit. The host names the
/// moved-to process from the restart on.
fn moved_past(tag: &str, own: bool) -> Run<(TurnEndAction, bool, Vec<bool>)> {
    let host = AccountHost::moving(TO, "done:resumed");
    *host.started.lock().unwrap() = Some(OLD_START);
    *host.started_on_restart.lock().unwrap() = Some(NEW_START);
    rig(tag, &host, |s, opts, heard| {
        at_as_limit(s, opts, heard);
        assert!(s.limit.is_none(), "the move ends A's episode");
        let (replayed, episode) = step(s, opts, heard, limited_screen(), 103, None);
        let limits = host.limits.lock().unwrap().clone();
        the_turn_it_opens(s, opts, heard, &host, own);
        (replayed, episode, limits)
    })
}

/// THE LIVE SEQUENCE: A's limit, the host's move to B (`done:resumed`), A's limit drawn again on
/// the resumed screen. The left limit opens no episode (the host is told no `limited`) and books
/// no wall wait; the one continuation typed is the move's, in its words, never that a limit
/// cleared; the turn it opens proves B at its served point.
#[test]
fn the_limit_a_move_left_opens_no_episode_and_the_move_continues_once_and_proves_its_account() {
    let run = moved_past("move-left-limit", false);
    let log = run.log();
    let (replayed, episode, limits_at_replay) = &run.out;
    assert_eq!(*replayed, moved_on(), "{log}");
    assert!(!*episode, "no episode for the left limit: {log}");
    assert_eq!(
        limits_at_replay.iter().filter(|open| **open).count(),
        1,
        "only A's own limit told the host `limited`: {limits_at_replay:?}\n{log}"
    );
    assert!(
        run.heard.iter().any(|l| l.starts_with("SKIPPED seq=103 ")
            && l.contains(&format!("limit left by the move to {TO}"))),
        "{log}"
    );
    assert_eq!(
        run.waits
            .iter()
            .filter(|l| l.contains(&format!("\"rule_id\":\"{RULE_WALL_WAIT}\"")))
            .count(),
        1,
        "A's wall wait alone; none booked for the left limit: {log}"
    );
    assert!(run.no_wait_stands(), "{log}");
    assert!(continued_in_the_moves_words(&run, 103), "{log}");
    assert_eq!(
        run.said
            .iter()
            .filter(|s| s.starts_with("restart:"))
            .collect::<Vec<_>>(),
        [&format!("restart:account:{TO}")],
        "{log}"
    );
    assert!(
        run.said.iter().any(|s| s == "served"),
        "B proven at its served point: {log}"
    );
    assert!(
        !run.said.iter().any(|s| s.starts_with("answered:")),
        "{log}"
    );
}

/// RISK A AT THE LOOP: B's own limit at the end of the continuation's turn, its notice written
/// in that turn, opens B's episode (the host is told `limited` again) and strikes B
/// (`answered:wall`); the left limit before it latched nothing that swallows it.
#[test]
fn the_moved_to_accounts_own_limit_after_the_left_one_opens_its_episode_and_strikes_it() {
    let run = moved_past("move-own-limit", true);
    let log = run.log();
    assert!(!run.out.1, "{log}");
    assert_eq!(
        run.said
            .iter()
            .filter(|s| s.starts_with("answered:"))
            .collect::<Vec<_>>(),
        ["answered:wall"],
        "B's own limit strikes B: {log}"
    );
    assert!(!run.said.iter().any(|s| s == "served"), "{log}");
    assert_eq!(
        run.opened(),
        2,
        "A's limit and B's own each told the host `limited`: {:?}\n{log}",
        run.limits
    );
    assert_eq!(run.turns, 1, "{log}");
}

/// REVIEW FINDING 1 OF 2026-11-16: the relaunch took, but the host could not read its landing
/// (`wait:account-unverified`), so the loop booked no move; the host named the moved-to process
/// at the next read, and at A's limit drawn again told the move as made, the loop's second ask
/// adopting it (`adopted`). The move is booked after the moved-to process was named. The left
/// limit at the next point is still no wall: no episode and no wall wait for it, and the one
/// continuation is the move's.
#[test]
fn a_move_told_as_made_after_its_process_was_named_still_leaves_the_limit() {
    let host = AccountHost::scripted(
        &[Some(AccountAdvice::Move {
            to: TO.to_string(),
            yields: None,
        })],
        &["wait:account-unverified", "adopted"],
    );
    *host.started.lock().unwrap() = Some(OLD_START);
    *host.started_on_restart.lock().unwrap() = Some(NEW_START);
    let run = rig("move-told-late", &host, |s, opts, heard| {
        at_as_limit(s, opts, heard);
        assert!(s.limit.is_some(), "no move booked: A's episode stands");
        let (asked, _) = step(s, opts, heard, limited_screen(), 103, None);
        assert_eq!(
            s.turn_end.agent_start_unix(),
            Some(NEW_START),
            "the moved-to process is named before the move is booked"
        );
        assert!(
            matches!(&asked, TurnEndAction::Restart { why: Restart::Account { to, .. }, .. } if to == TO),
            "the move asked again: {asked:?}"
        );
        assert!(s.turn_end.account_move().is_some(), "booked: adopted");
        assert!(s.limit.is_none(), "the move ends A's episode");
        step(s, opts, heard, limited_screen(), 105, None)
    });
    let log = run.log();
    let (replayed, episode) = &run.out;
    assert_eq!(*replayed, moved_on(), "{log}");
    assert!(!*episode, "no episode for the left limit: {log}");
    assert_eq!(run.opened(), 1, "A's limit alone: {:?}\n{log}", run.limits);
    assert!(run.no_wait_stands(), "no wall wait stands: {log}");
    assert!(continued_in_the_moves_words(&run, 105), "{log}");
    assert_eq!(host.said("restart").len(), 2, "{log}");
}

/// REVIEW FINDING 3 OF 2026-11-16: the host's kept snapshot still names the ended process after
/// the relaunch (`agent_started` stays at [`OLD_START`]), and names the moved-to process only
/// once the point asks whether the agent resumes (`agent_resumed`, after the decision's read
/// judged the wall). The loop's verdict and the policy's still agree: no episode, no wall wait,
/// and the move's continuation.
#[test]
fn a_snapshot_that_lags_the_relaunch_still_leaves_the_limit() {
    let host = AccountHost::moving(TO, "done:resumed");
    *host.started.lock().unwrap() = Some(OLD_START);
    let run = rig("move-late-snapshot", &host, |s, opts, heard| {
        at_as_limit(s, opts, heard);
        *host.late_agent.lock().unwrap() = Some((4242, NEW_START, None));
        let out = step(s, opts, heard, limited_screen(), 103, None);
        assert_eq!(
            s.turn_end.agent_start_unix(),
            Some(NEW_START),
            "the moved-to process named at the point's fold"
        );
        out
    });
    let log = run.log();
    let (replayed, episode) = &run.out;
    assert_eq!(*replayed, moved_on(), "{log}");
    assert!(!*episode, "no episode for the left limit: {log}");
    assert_eq!(run.opened(), 1, "A's limit alone: {:?}\n{log}", run.limits);
    assert!(run.no_wait_stands(), "no wall wait stands: {log}");
    assert!(continued_in_the_moves_words(&run, 103), "{log}");
}

/// The left limit's first point read no transcript (the host could read none then), so its
/// notice was undated and an episode opened for it; the next point dates it before the move and
/// ends that episode (the host told `limited(false)`), with no wall wait left standing.
#[test]
fn an_episode_opened_before_the_left_limit_was_dated_ends_when_it_is() {
    let host = AccountHost::moving(TO, "done:resumed");
    *host.started.lock().unwrap() = Some(OLD_START);
    *host.started_on_restart.lock().unwrap() = Some(NEW_START);
    let run = rig("move-undated-first", &host, |s, opts, heard| {
        at_as_limit(s, opts, heard);
        *host.tail.lock().unwrap() = None;
        let (first, undated) = step(s, opts, heard, limited_screen(), 103, None);
        *host.tail.lock().unwrap() = Some(limit_tail(NOTICE));
        let (_, dated) = step(s, opts, heard, limited_screen(), 105, None);
        (first, undated, dated)
    });
    let log = run.log();
    let (first, undated, dated) = &run.out;
    assert_eq!(
        *first,
        moved_on(),
        "undated, the move's continuation all the same: {log}"
    );
    assert!(
        *undated,
        "the control: the undated limit opened an episode: {log}"
    );
    assert!(!*dated, "dated before the move, it ends: {log}");
    assert_eq!(
        run.limits.last(),
        Some(&false),
        "the host is told the episode ended: {:?}\n{log}",
        run.limits
    );
    assert!(run.no_wait_stands(), "{log}");
    assert!(
        run.heard.iter().any(|l| l.starts_with("CLEARED seq=105 ")
            && l.contains(&format!("the limit the move to {TO} left"))),
        "{log}"
    );
}
