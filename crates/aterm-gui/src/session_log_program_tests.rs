// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PROGRAM MARKS (T24's
//! reversal): each move of what the program in front says about itself
//! (OSC 7501) that the status sweep's gate shows is a `program` mark in the
//! session's log, written through its run's bucket ([`PROGRAM_BURST`], then
//! one each [`PROGRAM_REFILL_MS`]), and a move that finds the bucket empty is
//! coalesced, never dropped: the newest state is written once a token frees,
//! with `coalesced=` the states it replaced. Driven by explicit calls on
//! hand-set clocks (the sweep's tick is [`SessionLog::program_tick`]).
//!
//! NEGATIVE CONTROLS (scratch, 6F-4's commit): a bucket that drops the
//! newest move ends the burst on a stale state; no bucket at all writes
//! some 14,400 marks in the flapping hour.

use std::time::{Duration, Instant};

use aterm_core::terminal::program_state::{
    ProgramBlockedKind, ProgramState, ProgramStateWord as W,
};

use super::super::tests::{lines_of, opened, with_agent};
use super::*;
use crate::session_log::{LogQuery, render_log};
use crate::session_timeline::SessionTimeline;
use crate::{App, WindowId};

fn report(state: W, kind: Option<ProgramBlockedKind>) -> Option<ProgramState> {
    Some(ProgramState { state, kind })
}

/// The log's program marks, at the host tier.
fn program_lines(tl: &std::sync::Mutex<SessionTimeline>) -> Vec<String> {
    lines_of(tl)
        .into_iter()
        .filter(|l| l.contains(" program "))
        .collect()
}

/// The status sweep's tick, at the log's now.
fn tick(tl: &std::sync::Mutex<SessionTimeline>) {
    let mut g = tl.lock().unwrap();
    let at = g.slog().now();
    g.slog_mut().program_tick(at);
}

/// A BURST OF 50 MOVES AT ONE HAND-SET SECOND, ENDING IN `done`: the first
/// [`PROGRAM_BURST`] are written at once, the rest coalesce into the run's
/// one pending state, which the same second's sweep cannot write and the
/// next second's does: `log kinds=program` ends on `state=done
/// coalesced=29`. Nothing is written twice, the ring holds every row, and
/// no program mark is bus class (it stays on this host).
#[test]
fn a_burst_of_50_moves_ends_on_its_newest_state_coalesced() {
    let (tl, hand) = opened();
    with_agent(&tl, "codex");
    hand.set(5_000);
    {
        let mut g = tl.lock().unwrap();
        for i in 0..49 {
            let state = [W::Working, W::Blocked][i % 2];
            let kind = (state == W::Blocked).then_some(ProgramBlockedKind::Permission);
            let _ = g.note_program_state(report(state, kind));
        }
        let _ = g.note_program_state(report(W::Done, None));
    }
    assert_eq!(
        program_lines(&tl).len(),
        PROGRAM_BURST as usize,
        "the burst: written at once"
    );
    tick(&tl);
    assert_eq!(
        program_lines(&tl).len(),
        PROGRAM_BURST as usize,
        "the same second: no token"
    );
    hand.set(6_000);
    tick(&tl);
    let g = tl.lock().unwrap();
    let query = LogQuery::parse("kinds=program").expect("a query");
    let reply = render_log(g.slog().log(), &query);
    let last = reply.lines().last().expect("a record");
    assert!(
        last.contains(" program state=done coalesced=29 src=engine conf=claim "),
        "the newest state, the 29 it replaced counted: {reply}"
    );
    assert_eq!(reply.lines().count(), 1 + PROGRAM_BURST as usize + 1);
    let rows = g
        .since(None)
        .filter(|e| e.kind == crate::session_timeline::PROGRAM_STATE)
        .count();
    assert_eq!(rows, 50, "the ring keeps every row");
    assert!(
        g.slog()
            .log()
            .window()
            .filter(|r| r.k == Kind::Program)
            .all(|r| !r.is_bus_class()),
        "local: no program mark is the bus mirror's"
    );
    drop(g);
    // Nothing pending: another second writes nothing.
    hand.set(7_000);
    tick(&tl);
    assert_eq!(program_lines(&tl).len(), PROGRAM_BURST as usize + 1);
}

/// A PROGRAM FLAPPING AT 4 HZ FOR AN HOUR, driven as 14,400 sweeps on a
/// hand-set clock (each the sweep's tick, then the gate's move): its marks
/// stay within the bucket (the burst, then one a second) and the log keeps
/// its run's share of them (64, T6), and the newest mark is the newest
/// state.
#[test]
fn a_program_flapping_at_4_hz_for_an_hour_stays_within_its_bucket_and_share() {
    let (tl, hand) = opened();
    with_agent(&tl, "codex");
    let seq0 = tl.lock().unwrap().slog().log().last_seq();
    const SWEEPS: u64 = 14_400;
    for i in 0..SWEEPS {
        hand.set(10_000 + 250 * i);
        tick(&tl);
        let state = if i % 2 == 0 { W::Working } else { W::Idle };
        let _ = tl.lock().unwrap().note_program_state(report(state, None));
    }
    let g = tl.lock().unwrap();
    let log = g.slog().log();
    let written = log.last_seq() - seq0;
    let hour_s = SWEEPS * 250 / PROGRAM_REFILL_MS;
    assert!(
        written <= PROGRAM_BURST + hour_s + 1 && written >= hour_s,
        "{written} program marks in the hour: the bucket's {PROGRAM_BURST} and one a second"
    );
    let newest = log
        .window()
        .rfind(|r| r.k == Kind::Program)
        .expect("a program mark");
    let share = aterm_slog::program_share(log.limits());
    assert_eq!(share, 64);
    assert_eq!(log.programs_held(newest.node), share, "the run's share");
    let all: usize = log.window().filter(|r| r.k == Kind::Program).count();
    assert_eq!(all, share, "every program mark is the run's");
    assert!(
        log.lost() >= written - share as u64,
        "the rest counted lost"
    );
}

/// PRESENCE COUNTS `ProgramDone` ONCE beside the program's marks: a codex
/// in front reports `working` then `done` through the real status sweep,
/// the log holds one `program state=done` mark, the gate counts one turn
/// end and presence one story point for it, however many sweeps follow.
/// NEGATIVE CONTROL: a second turn (`working`, `done`) is counted again.
#[test]
fn presence_counts_a_program_done_once_beside_its_mark() {
    const WORKING: &[u8] = b"\x1b]7501;state=working:app=codex\x1b\\";
    const DONE: &[u8] = b"\x1b]7501;state=done:app=codex\x1b\\";
    let mut app = App::headless_for_test();
    let session = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(session));
    let (term, ctx) = {
        let pooled = app.pool.get(session).expect("pooled");
        (
            std::sync::Arc::clone(&pooled.term),
            std::sync::Arc::clone(&pooled.ctx),
        )
    };
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(4242);
        tl.set_program(4242, Some("codex".into()));
    }
    let mut t = Instant::now();
    let mut sweep = |app: &mut App| {
        let _ = app.observe_session_statuses(t);
        app.refresh_presence_session(session);
        t += Duration::from_secs(1);
    };
    let story = |app: &App| -> u64 {
        let tail = app.presence_status_tail(session);
        tail.split(' ')
            .find_map(|w| w.strip_prefix("story="))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no story= in `{tail}`"))
    };
    let dones = || {
        lines_of(&ctx.timeline)
            .iter()
            .filter(|l| l.contains(" program state=done "))
            .count()
    };
    crate::term_lock(&term).process(WORKING);
    sweep(&mut app);
    let before = story(&app);
    crate::term_lock(&term).process(DONE);
    for _ in 0..4 {
        sweep(&mut app);
    }
    assert_eq!(dones(), 1, "one done mark");
    assert_eq!(app.session_status.program_turn_ends(session), 1);
    assert_eq!(story(&app), before + 1, "one story point for the done");
    // NEGATIVE CONTROL: a second turn is a second end.
    crate::term_lock(&term).process(WORKING);
    sweep(&mut app);
    crate::term_lock(&term).process(DONE);
    sweep(&mut app);
    assert_eq!(dones(), 2);
    assert_eq!(app.session_status.program_turn_ends(session), 2);
    assert_eq!(story(&app), before + 2);
}

/// `n` moves at the hand's second, alternating `working` and `idle`, the
/// last `last`: the first [`PROGRAM_BURST`] are written, the rest coalesce
/// into the run's one pending state (`coalesced=` `n - BURST - 1`).
fn burst(tl: &std::sync::Mutex<SessionTimeline>, n: usize, last: W) {
    let mut g = tl.lock().unwrap();
    for i in 0..n - 1 {
        let _ = g.note_program_state(report([W::Working, W::Idle][i % 2], None));
    }
    let _ = g.note_program_state(report(last, None));
}

/// The node word of a host-tier record line (`<seq> <op> <node> <kind> …`).
fn node_of(line: &str) -> &str {
    line.split(' ')
        .nth(2)
        .expect("a record line names its node")
}

/// The codex run's node, from its `open` record.
fn codex_run(tl: &std::sync::Mutex<SessionTimeline>) -> String {
    let lines = lines_of(tl);
    let open = lines
        .iter()
        .find(|l| l.contains(" open ") && l.contains("codex"))
        .unwrap_or_else(|| panic!("no codex run: {lines:#?}"));
    node_of(open).to_string()
}

/// THE NEWEST IS NEVER DROPPED AT A SESSION'S CLOSE (U44): a burst that
/// leaves `done` pending, then the session closes in the same second (no
/// token): the pending state is written on its run, `coalesced=` counted,
/// before the session's close. NEGATIVE CONTROL (scratch): `close_session`
/// without its flush loses the `done`, and this fails.
#[test]
fn a_pending_state_is_written_before_the_session_closes() {
    let (tl, hand) = opened();
    with_agent(&tl, "codex");
    let run = codex_run(&tl);
    hand.set(5_000);
    burst(&tl, 30, W::Done);
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().close_session(
            aterm_slog::SessionOut::ShellExit,
            aterm_slog::Actor::Nobody,
            "-",
            Some(0),
            now,
        );
    }
    let lines = lines_of(&tl);
    let last = lines
        .iter()
        .rposition(|l| l.contains(" program "))
        .expect("program marks");
    assert!(
        lines[last].contains(" program state=done coalesced=9 src=engine conf=claim "),
        "the newest state, its 9 replaced counted: {lines:#?}"
    );
    assert_eq!(node_of(&lines[last]), run, "on its own run");
    let close = lines
        .iter()
        .position(|l| l.contains(" close - "))
        .expect("the session's close");
    assert!(last < close, "before the session closed: {lines:#?}");
}

/// THE NEWEST IS NEVER DROPPED ACROSS AN UPDATE (U44): a burst that leaves
/// `done` pending, then the handoff worker takes the log in the same second:
/// the pending state is written before the fence, so the carry's lines hold
/// it (the successor's bucket starts full and empty). NEGATIVE CONTROL
/// (scratch): `carry_source` without its flush carries no `done`.
#[test]
fn a_pending_state_is_written_before_the_carry() {
    let (tl, hand) = opened();
    with_agent(&tl, "codex");
    hand.set(5_000);
    burst(&tl, 30, W::Done);
    let carried = tl
        .lock()
        .unwrap()
        .slog_mut()
        .carry_source()
        .encode(aterm_slog::CarryBudget::SESSION)
        .expect("carried")
        .lines;
    assert!(
        carried
            .iter()
            .any(|l| l.contains(" program state=done coalesced=9 ")),
        "the carry holds the newest state: {carried:#?}"
    );
}

/// THE NEWEST IS NEVER DROPPED AT A RUN CHANGE (U44): moves with no run
/// open fill the session's bucket and leave `working` pending; a codex run
/// comes to the front in the same second, and its first move first writes
/// the session's pending state on `-`, then its own on its run. NEGATIVE
/// CONTROL (scratch): a run change that starts the new bucket without
/// writing the old one's pending state loses the `working`.
#[test]
fn a_run_change_writes_the_last_runs_pending_state_first() {
    let (tl, hand) = opened();
    hand.set(5_000);
    burst(&tl, 30, W::Working);
    with_agent(&tl, "codex");
    let run = codex_run(&tl);
    let _ = tl.lock().unwrap().note_program_state(report(W::Done, None));
    let lines = program_lines(&tl);
    let n = lines.len();
    assert_eq!(n, PROGRAM_BURST as usize + 2, "{lines:#?}");
    assert!(
        lines[n - 2].contains(" mark - program state=working coalesced=9 "),
        "the session's pending state, on `-`: {lines:#?}"
    );
    assert!(
        lines[n - 1].contains(" program state=done src=engine ") && node_of(&lines[n - 1]) == run,
        "then the new run's first move, on its run: {lines:#?}"
    );
}

/// WHERE A PROGRAM MARK LANDS ONCE ITS RUN CLOSES (the module doc): a codex
/// run in front leaves `done` pending, beside it a run the producers do not
/// hold is open with a turn under it (the deepest open node); the front
/// moves back to the shell, which closes the codex run. Its pending `done`
/// is written on it before its close, and the cleared report the gate then
/// shows (`state=none`) is the session's (`-`, the shell holds no run):
/// no program mark lands on the deepest open node. NEGATIVE CONTROL
/// (scratch): `close_node` without `settle_program` writes the `done` after
/// the close, on `-`, and this fails.
#[test]
fn a_runs_pending_state_is_written_on_it_before_it_closes() {
    let (tl, hand) = opened();
    with_agent(&tl, "codex");
    let run = codex_run(&tl);
    let beside = {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        let log = g.slog_mut().log_mut_for_journal();
        let other = log
            .open(
                aterm_slog::Open::new(Kind::Agent, Src::Status, Conf::Strong).under(Node::SESSION),
                now,
            )
            .expect("a run opens");
        let turn = log
            .open(
                aterm_slog::Open::new(Kind::Turn, Src::Screen, Conf::Strong)
                    .under(other)
                    .by(aterm_slog::Actor::Vendor),
                now,
            )
            .expect("a turn opens");
        [other.to_string(), turn.to_string()]
    };
    hand.set(5_000);
    burst(&tl, 30, W::Done);
    {
        let mut g = tl.lock().unwrap();
        let _ = g.sweep_slog(&crate::session_log::BlockRead::default(), false, 100, 100);
        let _ = g.note_program_state(None);
    }
    let lines = lines_of(&tl);
    let done = lines
        .iter()
        .position(|l| l.contains(" program state=done coalesced=9 "))
        .unwrap_or_else(|| panic!("the pending done: {lines:#?}"));
    assert_eq!(node_of(&lines[done]), run, "on its run: {lines:#?}");
    let closed = lines
        .iter()
        .position(|l| l.contains(" close ") && node_of(l) == run)
        .unwrap_or_else(|| panic!("the codex run's close: {lines:#?}"));
    assert!(done < closed, "before its close: {lines:#?}");
    let none = lines
        .iter()
        .find(|l| l.contains(" program state=none "))
        .unwrap_or_else(|| panic!("the cleared report: {lines:#?}"));
    assert_eq!(node_of(none), "-", "the session's: {none}");
    assert!(
        lines
            .iter()
            .filter(|l| l.contains(" program "))
            .all(|l| !beside.iter().any(|b| b == node_of(l))),
        "no program mark on a run the producers do not hold: {lines:#?}"
    );
}

/// A RUN THE LOG CLOSED BEHIND THE PRODUCERS' BACK (no `close_node`): its
/// pending state, written at the next tick, is the session's (`-`), never
/// the deepest open node's. NEGATIVE CONTROL (scratch): writing at the
/// bucket's run unfiltered lets [`aterm_slog::Log::mark`] hang it on the
/// open turn beside it, and this fails.
#[test]
fn a_pending_state_whose_run_is_gone_is_the_sessions() {
    let (tl, hand) = opened();
    with_agent(&tl, "codex");
    let run = aterm_slog::Node::parse(&codex_run(&tl)).expect("a node");
    let turn = {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        let log = g.slog_mut().log_mut_for_journal();
        let other = log
            .open(
                aterm_slog::Open::new(Kind::Agent, Src::Status, Conf::Strong).under(Node::SESSION),
                now,
            )
            .expect("a run opens");
        log.open(
            aterm_slog::Open::new(Kind::Turn, Src::Screen, Conf::Strong)
                .under(other)
                .by(aterm_slog::Actor::Vendor),
            now,
        )
        .expect("a turn opens")
    };
    hand.set(5_000);
    burst(&tl, 30, W::Done);
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut()
            .log_mut_for_journal()
            .close(
                run,
                aterm_slog::Close::new(
                    aterm_slog::Out::Run(aterm_slog::RunOut::Unknown),
                    Src::Status,
                    Conf::Strong,
                ),
                now,
            )
            .expect("the run closes");
    }
    hand.set(6_000);
    tick(&tl);
    let lines = program_lines(&tl);
    let last = lines.last().expect("program marks");
    assert!(
        last.contains(" mark - program state=done coalesced=9 "),
        "the session's: {lines:#?}"
    );
    assert!(
        lines.iter().all(|l| node_of(l) != turn.to_string()),
        "never the open turn's: {lines:#?}"
    );
}

/// THE STATUS SWEEP'S TICK WRITES A COALESCED STATE (the production
/// wiring, U17): a codex in front reports 30 moves, one a sweep, all in one
/// hand-set second of its log's clock, ending `done`; the bucket writes the
/// first [`PROGRAM_BURST`] and holds the rest. The program then goes quiet,
/// and the next sweep, a second later, writes the held `done` with
/// `coalesced=`. NEGATIVE CONTROL (scratch): the sweep without its
/// `program_tick` leaves the `done` unwritten, and this fails.
#[test]
fn the_status_sweep_writes_a_held_state_once_a_token_frees() {
    let mut app = App::headless_for_test();
    let session = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(session));
    let (term, ctx) = {
        let pooled = app.pool.get(session).expect("pooled");
        (
            std::sync::Arc::clone(&pooled.term),
            std::sync::Arc::clone(&pooled.ctx),
        )
    };
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(4242);
        tl.set_program(4242, Some("codex".into()));
    }
    let hand = super::super::tests::hand_clock_from(&ctx.timeline, crate::turn_ledger::now_ms());
    let mut t = Instant::now();
    let mut sweep = |app: &mut App| {
        let _ = app.observe_session_statuses(t);
        t += Duration::from_secs(1);
    };
    sweep(&mut app);
    hand.set(5_000);
    for i in 0..30 {
        let report: &[u8] = match (i, i % 2) {
            (29, _) => b"\x1b]7501;state=done:app=codex\x1b\\",
            (_, 0) => b"\x1b]7501;state=working:app=codex\x1b\\",
            _ => b"\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\",
        };
        crate::term_lock(&term).process(report);
        sweep(&mut app);
    }
    let held = program_lines(&ctx.timeline);
    assert_eq!(held.len(), PROGRAM_BURST as usize, "the burst: {held:#?}");
    hand.set(6_000);
    sweep(&mut app);
    let lines = program_lines(&ctx.timeline);
    assert_eq!(lines.len(), PROGRAM_BURST as usize + 1, "{lines:#?}");
    assert!(
        lines[PROGRAM_BURST as usize]
            .contains(" program state=done coalesced=9 src=engine conf=claim "),
        "the held done, written by the quiet sweep: {lines:#?}"
    );
}
