// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session log `program` mark tests ([`PROGRAM_BURST`], [`PROGRAM_REFILL_MS`],
//! [`SessionLog::program_tick`]).

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
    Some(ProgramState {
        state,
        kind,
        app: aterm_core::terminal::program_state::ProgramApp::Codex,
    })
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
/// coalesced=29`.
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
            g.note_program_state(report(state, kind), 1);
        }
        g.note_program_state(report(W::Done, None), 1);
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
    assert_eq!(
        rows,
        PROGRAM_BURST as usize + 1,
        "the rows are the marks: the burst and the coalesced newest state"
    );
    assert!(
        g.slog()
            .log()
            .window()
            .filter(|r| r.k == Kind::Program)
            .all(|r| !r.is_bus_class()),
        "local: no program mark is the bus mirror's"
    );
    drop(g);
    hand.set(7_000);
    tick(&tl);
    assert_eq!(program_lines(&tl).len(), PROGRAM_BURST as usize + 1);
}

/// A PROGRAM FLAPPING AT 4 HZ FOR AN HOUR, driven as 14,400 sweeps on a
/// hand-set clock (each the sweep's tick, then the gate's move): its marks
/// stay within the bucket (the burst, then one a second) and the log keeps
/// its run's share of them (64, T6), and the newest mark is the newest
/// state. The codex run's claimed turns are moved by its marks alone, since
/// no engine edge reaches this rig, so they open and close while the burst
/// lasts and then stay put, the bucket writing every fourth state, the same
/// one each time.
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
        tl.lock()
            .unwrap()
            .note_program_state(report(state, None), 1);
    }
    let g = tl.lock().unwrap();
    let log = g.slog().log();
    let turns = log.window().filter(|r| r.k == Kind::Turn).count() as u64;
    assert!(
        turns > 0 && turns <= 2 * PROGRAM_BURST,
        "{turns} turn records: with no engine edge fed, a turn moves only with a written mark, \
         each a state the burst and its refill wrote"
    );
    let written = log.last_seq() - seq0 - turns;
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
        g.note_program_state(report([W::Working, W::Idle][i % 2], None), 1);
    }
    g.note_program_state(report(last, None), 1);
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
/// before the session's close.
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
/// it (the successor's bucket starts full and empty).
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
/// the session's pending state on `-`, then its own on its run.
#[test]
fn a_run_change_writes_the_last_runs_pending_state_first() {
    let (tl, hand) = opened();
    hand.set(5_000);
    burst(&tl, 30, W::Working);
    with_agent(&tl, "codex");
    let run = codex_run(&tl);
    tl.lock()
        .unwrap()
        .note_program_state(report(W::Done, None), 1);
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

/// WHERE A PROGRAM MARK LANDS ONCE ITS RUN CLOSES: on that run, before the close. A codex
/// run in front leaves `done` pending, beside it a run the producers do not
/// hold is open with a turn under it (the deepest open node); the front
/// moves back to the shell, which closes the codex run.
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
        g.note_program_state(None, 1);
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
/// the deepest open node's.
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
/// first [`PROGRAM_BURST`] and holds the rest.
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

/// A CODEX-ALAB'S CLAIMED TURN THROUGH ITS BOX AND ITS ERROR: `working`, `blocked` on an approval,
/// `working` again and `error`, each written as its program mark, are one turn (`src=engine
/// conf=claim`) that closes `out=error`: a `blocked` claim is a turn waiting on a person, never
/// its end. NEGATIVE CONTROL: with `idle` where the box was, the rest closes the first turn
/// `out=done` and the next `working` opens a second.
#[test]
fn a_codex_alabs_blocked_claim_keeps_its_turn_and_an_error_closes_it() {
    let turns = |middle: (W, Option<ProgramBlockedKind>)| {
        let (tl, hand) = opened();
        with_agent(&tl, "codex");
        let moves = [
            (W::Working, None),
            middle,
            (W::Working, None),
            (W::Error, None),
        ];
        for (i, (state, kind)) in (0_u64..).zip(moves) {
            hand.set(5_000 + 1_000 * i);
            tl.lock()
                .unwrap()
                .note_program_state(report(state, kind), 1);
        }
        lines_of(&tl)
            .into_iter()
            .filter(|l| l.contains(" turn ") && (l.contains(" open t") || l.contains(" close t")))
            .collect::<Vec<_>>()
    };
    let boxed = turns((W::Blocked, Some(ProgramBlockedKind::Permission)));
    assert!(
        boxed.len() == 2
            && boxed[0].contains(" open t1 turn ")
            && boxed[0].contains(" src=engine conf=claim ")
            && boxed[1].contains(" close t1 turn out=error ")
            && boxed[1].contains(" src=engine conf=claim "),
        "{boxed:#?}"
    );
    let rested = turns((W::Idle, None));
    assert!(
        rested.iter().filter(|l| l.contains(" open t")).count() == 2
            && rested
                .iter()
                .any(|l| l.contains(" close t1 turn out=done "))
            && rested
                .iter()
                .any(|l| l.contains(" close t2 turn out=error ")),
        "NEGATIVE CONTROL: {rested:#?}"
    );
}

const WORKING: &[u8] = b"\x1b]7501;state=working:app=codex\x1b\\";
const DONE: &[u8] = b"\x1b]7501;state=done:app=codex\x1b\\";

/// A codex in front of a real tab under the real status sweep (`App::observe_session_statuses`),
/// its log on a hand clock counted on from the event loop's monotonic now (`base`), each sweep a
/// second past the last on the sweep's own clock.
struct CodexTab {
    app: App,
    term: std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>>,
    ctx: std::sync::Arc<crate::SessionCtx>,
    hand: super::super::tests::Hand,
    base: u64,
    t: Instant,
}

impl CodexTab {
    fn new() -> Self {
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
        let base = crate::turn_ledger::now_ms();
        let hand = super::super::tests::hand_clock_from(&ctx.timeline, base);
        {
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(4242);
            tl.set_program(4242, Some("codex".into()));
        }
        CodexTab {
            app,
            term,
            ctx,
            hand,
            base,
            t: Instant::now(),
        }
    }

    /// The wall clock `ms` into the test, on the hand.
    const fn wall(&self, ms: u64) -> u64 {
        super::super::tests::WALL0 + self.base + ms
    }

    /// `bytes` in one parse batch whose wall clock reads `ms` into the test.
    fn says_at(&self, bytes: &[u8], ms: u64) {
        crate::term_lock(&self.term).process_at(
            bytes,
            aterm_core::terminal::ClockReading {
                wall_ms: Some(self.wall(ms)),
                ..aterm_core::terminal::ClockReading::now()
            },
        );
    }

    /// One status sweep, the log's hand at `ms`.
    fn sweep_at(&mut self, ms: u64) {
        self.hand.set(ms);
        let _ = self.app.observe_session_statuses(self.t);
        self.t += Duration::from_secs(1);
    }

    /// The turns the log opened, evicted ones counted, and whether one is open.
    fn turns(&self) -> (u32, bool) {
        let g = self.ctx.timeline.lock().unwrap();
        let st = g.slog().log().state();
        (st.totals.turns, st.turn().is_some())
    }

    /// The live turn opens and closes, oldest first.
    fn turn_lines(&self) -> Vec<String> {
        lines_of(&self.ctx.timeline)
            .into_iter()
            .filter(|l| l.contains(" turn ") && (l.contains(" open t") || l.contains(" close t")))
            .collect()
    }
}

/// A CODEX TURN BETWEEN TWO SWEEPS IS ONE CLAIMED TURN AT ITS EDGES (ATERM-KI-1029-1): a codex
/// in front reads `done` at a sweep, then writes `working` and `done` 17 ms apart before the next
/// sweep, which shows no move and writes no mark; the log holds that turn once, opened and closed
/// at its batches' wall clocks (`src=engine conf=claim`, `dur_ms=17`), and `log kinds=program`
/// still holds the one `state=done`. CONTROL: a sweep between the two writes shows the `working`
/// (its mark opens the turn) and the turn still closes at its own end, once.
#[test]
fn a_codex_turn_between_two_sweeps_is_one_claimed_turn_at_its_edges() {
    for between in [false, true] {
        let mut tab = CodexTab::new();
        tab.says_at(DONE, 900);
        tab.sweep_at(1_000);
        let (turns0, _) = tab.turns();
        tab.says_at(WORKING, 1_010);
        if between {
            tab.sweep_at(1_015);
        }
        tab.says_at(DONE, 1_027);
        tab.sweep_at(1_250);
        let lines = tab.turn_lines();
        assert_eq!(tab.turns(), (turns0 + 1, false), "one turn: {lines:#?}");
        let [open, close] = lines.as_slice() else {
            panic!("one open and its close: {lines:#?}");
        };
        let opened = if between { 1_015 } else { 1_010 };
        assert!(
            open.contains(" src=engine conf=claim ")
                && open.ends_with(&format!(" t={}", tab.wall(opened))),
            "{between}: {lines:#?}"
        );
        assert!(
            close.contains(" turn out=done ")
                && close.contains(" src=engine conf=claim ")
                && close.ends_with(&format!(" t={}", tab.wall(1_027))),
            "{between}: closed at its own end: {lines:#?}"
        );
        let programs = program_lines(&tab.ctx.timeline);
        if between {
            assert_eq!(
                programs.len(),
                3,
                "CONTROL: done, working, done: {programs:#?}"
            );
        } else {
            assert!(
                close.contains(" dur_ms=17 "),
                "timed by its edges: {lines:#?}"
            );
            assert!(
                programs.len() == 1 && programs[0].contains(" program state=done "),
                "the edges write no mark: {programs:#?}"
            );
        }
    }
}

/// A HELD `working` THE TICK WRITES IS CLOSED BY ITS EDGE: the run's program bucket is empty (a
/// burst), so the sweep that shows a codex's `working` holds its mark; its `done` comes before a
/// token frees, and the next sweep's tick writes the held `working` (opening its turn) before the
/// `done` edge is fed (closing it), so the log holds one turn and none is left open, while the
/// shown `done` waits for the next token.
#[test]
fn a_held_working_the_tick_writes_is_closed_by_its_edge() {
    let mut tab = CodexTab::new();
    tab.says_at(DONE, 900);
    tab.sweep_at(1_000);
    burst(&tab.ctx.timeline, PROGRAM_BURST as usize - 1, W::Done);
    assert_eq!(
        program_lines(&tab.ctx.timeline).len(),
        PROGRAM_BURST as usize,
        "PRECONDITION: the bucket is empty"
    );
    let (turns0, _) = tab.turns();
    tab.says_at(WORKING, 1_010);
    tab.sweep_at(1_015);
    assert_eq!(
        tab.turns(),
        (turns0, false),
        "PRECONDITION: the held working opened none"
    );
    tab.says_at(DONE, 1_027);
    tab.sweep_at(2_015);
    let programs = program_lines(&tab.ctx.timeline);
    assert!(
        programs.len() == PROGRAM_BURST as usize + 1
            && programs[PROGRAM_BURST as usize].contains(" program state=working "),
        "PRECONDITION: the tick wrote the held working, the done waits: {programs:#?}"
    );
    assert_eq!(
        tab.turns(),
        (turns0 + 1, false),
        "one turn, closed by its edge: {:#?}",
        tab.turn_lines()
    );
}

/// A stalled sweep must not detach a held claim from the partial rollout's turns.
/// The second beginning is genuinely unseen by the gate; the first was already shown.
#[test]
fn a_delayed_program_tick_keeps_the_observed_claims_rollout_span() {
    use aterm_slog::{ShortId, TxEvent, TxPos, TxState};

    let mut tab = CodexTab::new();
    tab.says_at(DONE, 900);
    tab.sweep_at(1_000);
    {
        let mut g = tab.ctx.timeline.lock().unwrap();
        for i in 1..PROGRAM_BURST {
            let shown = if i % 2 == 1 {
                None
            } else {
                report(W::Done, None)
            };
            g.note_program_state(shown, 1);
        }
    }
    assert_eq!(
        program_lines(&tab.ctx.timeline).len(),
        PROGRAM_BURST as usize
    );
    let (turns0, _) = tab.turns();
    tab.says_at(WORKING, 1_008);
    tab.sweep_at(1_010);
    assert_eq!(
        tab.turns(),
        (turns0, false),
        "working is held without a token"
    );

    tab.says_at(DONE, 1_020);
    tab.says_at(WORKING, 1_030);
    tab.says_at(DONE, 1_040);
    // The rollout is read while the main thread is stalled, before its next tick.
    tab.hand.set(1_050);
    {
        let mut g = tab.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        let slog = g.slog_mut();
        let tracker = &mut slog.agent.as_mut().expect("codex run").tracker;
        tracker.transcript(
            &mut slog.log,
            TxEvent::Bind {
                len: 0,
                floor_ms: tab.wall(1_000),
            },
            TxPos {
                fp: 1,
                id: None,
                off: 0,
                wall_ms: tab.wall(1_000),
            },
            TxState::Partial,
            now,
        );
        for (index, (ms, event)) in [
            (
                1_008,
                TxEvent::Prompt {
                    vref: ShortId::new("a").unwrap(),
                },
            ),
            (
                1_020,
                TxEvent::TurnEnd {
                    vdur_ms: 12,
                    tok_in: 0,
                    tok_out: 0,
                },
            ),
            (
                1_030,
                TxEvent::Prompt {
                    vref: ShortId::new("b").unwrap(),
                },
            ),
            (
                1_040,
                TxEvent::TurnEnd {
                    vdur_ms: 10,
                    tok_in: 0,
                    tok_out: 0,
                },
            ),
        ]
        .into_iter()
        .enumerate()
        {
            tracker.transcript(
                &mut slog.log,
                event,
                TxPos {
                    fp: 1,
                    id: None,
                    off: index as u64 + 1,
                    wall_ms: tab.wall(ms),
                },
                TxState::Partial,
                now,
            );
        }
    }
    assert_eq!(
        tab.turns(),
        (turns0 + 2, false),
        "two completed rollout spans"
    );
    let before = tab.turn_lines();
    for (id, opened, ended) in [("a", 1_008, 1_020), ("b", 1_030, 1_040)] {
        assert!(
            before.iter().any(|line| {
                line.contains(" open t")
                    && line.contains(&format!(" vref={id} "))
                    && line.contains(" src=transcript conf=strong ")
                    && line.ends_with(&format!(" t={}", tab.wall(opened)))
            }),
            "partial rollout opens {id} at its own row: {before:#?}"
        );
        assert!(
            before.iter().any(|line| {
                line.contains(" close t")
                    && line.contains(" turn out=done ")
                    && line.ends_with(&format!(" t={}", tab.wall(ended)))
            }),
            "partial rollout closes {id} at its own row: {before:#?}"
        );
    }
    tab.sweep_at(8_000);
    assert_eq!(
        tab.turns(),
        (turns0 + 2, false),
        "the held first claim and retained second beginning must reuse their spans: {:#?}",
        tab.turn_lines()
    );
    assert_eq!(
        tab.turn_lines(),
        before,
        "no duplicated or redated turn edges"
    );
    let programs = program_lines(&tab.ctx.timeline);
    let flushed = &programs[PROGRAM_BURST as usize];
    assert!(
        flushed.contains(" program state=working ")
            && flushed.ends_with(&format!(" t={}", tab.wall(8_000))),
        "the mark retains its actual flush time: {flushed}"
    );
}

/// An interruption read for the next vendor turn must not change the previous claim's end.
/// Direct tracker facts seed the partial rollout; the terminal and real sweep supply both edges.
#[test]
fn a_partial_rollout_interrupt_keeps_the_next_claims_outcome() {
    use aterm_slog::{ShortId, TxEvent, TxPos, TxState};

    for interrupted in [false, true] {
        let mut tab = CodexTab::new();
        tab.says_at(DONE, 900);
        tab.sweep_at(1_000);
        tab.says_at(WORKING, 1_010);
        tab.sweep_at(1_015);
        assert_eq!(tab.turns(), (1, true), "the first claim is running");
        tab.says_at(DONE, 1_020);
        tab.says_at(WORKING, 1_030);
        tab.says_at(DONE, 1_040);
        tab.hand.set(1_045);
        {
            let mut g = tab.ctx.timeline.lock().unwrap();
            let now = g.slog().now();
            let slog = g.slog_mut();
            let tracker = &mut slog.agent.as_mut().expect("codex run").tracker;
            let pos = |off, ms| TxPos {
                fp: 1,
                id: None,
                off,
                wall_ms: tab.wall(ms),
            };
            tracker.transcript(
                &mut slog.log,
                TxEvent::Bind {
                    len: 0,
                    floor_ms: tab.wall(1_000),
                },
                pos(0, 1_000),
                TxState::Partial,
                now,
            );
            tracker.transcript(
                &mut slog.log,
                TxEvent::Prompt {
                    vref: ShortId::new("next").unwrap(),
                },
                pos(1, 1_030),
                TxState::Partial,
                now,
            );
            if interrupted {
                tracker.transcript(
                    &mut slog.log,
                    TxEvent::Interrupt,
                    pos(2, 1_040),
                    TxState::Partial,
                    now,
                );
            }
        }
        assert_eq!(tab.turns(), (1, true), "no engine edge was swept yet");
        tab.sweep_at(1_050);
        let lines = tab.turn_lines();
        assert_eq!(tab.turns(), (2, false), "two completed claims: {lines:#?}");
        let opens: Vec<_> = lines
            .iter()
            .filter(|line| line.contains(" open t"))
            .collect();
        let closes: Vec<_> = lines
            .iter()
            .filter(|line| line.contains(" close t"))
            .collect();
        assert_eq!((opens.len(), closes.len()), (2, 2), "{lines:#?}");
        for ((open, close), (began, ended)) in opens
            .iter()
            .zip(&closes)
            .zip([(1_015, 1_020), (1_030, 1_040)])
        {
            assert_eq!(node_of(open), node_of(close), "matching claim: {lines:#?}");
            assert!(
                open.ends_with(&format!(" t={}", tab.wall(began))),
                "{lines:#?}"
            );
            assert!(
                close.ends_with(&format!(" t={}", tab.wall(ended))),
                "{lines:#?}"
            );
            assert!(close.contains(" src=engine conf=claim "), "{lines:#?}");
        }
        assert!(
            closes[0].contains(" turn out=done "),
            "the earlier turn was not interrupted ({interrupted}): {lines:#?}"
        );
        let expected = if interrupted { "interrupted" } else { "done" };
        assert!(
            closes[1].contains(&format!(" turn out={expected} ")),
            "the next turn keeps its own outcome ({interrupted}): {lines:#?}"
        );
    }
}

/// Distinct vendor turns must remain distinguishable when their engine reports one working span.
/// A single vendor turn is the control; partial input carries no extra outcome authority here.
#[test]
fn partial_vendor_turns_keep_distinct_identities_inside_one_claimed_span() {
    use aterm_slog::{Key, Kind, ShortId, TxEvent, TxPos, TxState, Val};

    for vendor_turns in [1, 2] {
        let mut tab = CodexTab::new();
        tab.says_at(DONE, 900);
        tab.sweep_at(1_000);
        tab.says_at(WORKING, 1_010);
        tab.sweep_at(1_015);
        assert_eq!(tab.turns(), (1, true));
        tab.hand.set(1_055);
        {
            let mut g = tab.ctx.timeline.lock().unwrap();
            let now = g.slog().now();
            let slog = g.slog_mut();
            let tracker = &mut slog.agent.as_mut().expect("codex run").tracker;
            tracker.transcript(
                &mut slog.log,
                TxEvent::Bind {
                    len: 0,
                    floor_ms: tab.wall(1_000),
                },
                TxPos {
                    fp: 1,
                    id: None,
                    off: 0,
                    wall_ms: tab.wall(1_000),
                },
                TxState::Partial,
                now,
            );
            for (index, (id, began, ended)) in [("first", 1_020, 1_030), ("second", 1_040, 1_052)]
                .into_iter()
                .take(vendor_turns)
                .enumerate()
            {
                for (offset, ms, event) in [
                    (
                        1,
                        began,
                        TxEvent::Prompt {
                            vref: ShortId::new(id).unwrap(),
                        },
                    ),
                    (
                        2,
                        ended,
                        TxEvent::TurnEnd {
                            vdur_ms: ended - began,
                            tok_in: 11 + 6 * index as u64,
                            tok_out: 13 + 6 * index as u64,
                        },
                    ),
                ] {
                    let pos = TxPos {
                        fp: 1,
                        id: None,
                        off: 2 * index as u64 + offset,
                        wall_ms: tab.wall(ms),
                    };
                    tracker.transcript(&mut slog.log, event, pos, TxState::Partial, now);
                    // Re-reading the same accepted row cannot duplicate its evidence.
                    tracker.transcript(&mut slog.log, event, pos, TxState::Partial, now);
                }
            }
        }
        tab.says_at(DONE, 1_060);
        tab.sweep_at(1_065);
        assert_eq!(
            tab.turns(),
            (1, false),
            "the engine's one working span ended"
        );
        let lines = lines_of(&tab.ctx.timeline);
        let g = tab.ctx.timeline.lock().unwrap();
        let evidence = g
            .slog()
            .log()
            .window()
            .filter(|record| {
                record.k == Kind::Meta
                    && record.f.get(Key::Field).is_some_and(
                        |value| matches!(value, Val::Tok(field) if field.as_str() == "vendor-turn"),
                    )
            })
            .collect::<Vec<_>>();
        let first = ShortId::new("first").unwrap();
        assert!(
            evidence
                .iter()
                .any(|record| record.f.get(Key::Vref) == Some(&Val::Id(first))),
            "the first prompt has a recorded identity"
        );
        if vendor_turns == 2 {
            let second = ShortId::new("second").unwrap();
            assert!(
                evidence
                    .iter()
                    .any(|record| record.f.get(Key::Vref) == Some(&Val::Id(second))),
                "the second vendor identity must remain available without guessing a span split: {lines:#?}"
            );
        }
        assert_eq!(
            evidence.len(),
            2 * vendor_turns,
            "duplicate rows add no observations"
        );
        for record in &evidence {
            assert!(
                crate::session_timeline_view::rows_of(record).is_empty(),
                "raw vendor facts must not become generic metadata changes"
            );
            assert_eq!(crate::session_timeline_view::row_span(record), 0);
        }
        let mut other_source = (*evidence[0]).clone();
        other_source.src = aterm_slog::Src::Meta;
        let mut other_actor = (*evidence[0]).clone();
        other_actor.by = aterm_slog::Actor::Ctl;
        for ordinary in [other_source, other_actor] {
            let rows = crate::session_timeline_view::rows_of(&ordinary);
            assert_eq!(
                rows.len(),
                1,
                "the same field from another source still projects"
            );
            assert_eq!(rows[0].0, crate::session_timeline::META_CHANGE);
        }
        for (index, id) in ["first", "second"]
            .into_iter()
            .take(vendor_turns)
            .enumerate()
        {
            let id = ShortId::new(id).unwrap();
            let records = evidence
                .iter()
                .filter(|record| record.f.get(Key::Vref) == Some(&Val::Id(id)))
                .collect::<Vec<_>>();
            assert_eq!(records.len(), 2);
            assert_eq!(records[0].f.get(Key::State), Some(&Val::Word("working")));
            assert_eq!(records[1].f.get(Key::State), Some(&Val::Word("done")));
            assert_eq!(
                records[0].t,
                tab.wall(if index == 0 { 1_020 } else { 1_040 })
            );
            assert_eq!(
                records[1].t,
                tab.wall(if index == 0 { 1_030 } else { 1_052 })
            );
            assert_eq!(
                records[1].f.get(Key::VdurMs),
                Some(&Val::U(if index == 0 { 10 } else { 12 }))
            );
            assert_eq!(
                records[1].f.get(Key::TokIn),
                Some(&Val::U(11 + 6 * index as u64))
            );
            assert_eq!(
                records[1].f.get(Key::TokOut),
                Some(&Val::U(13 + 6 * index as u64))
            );
            assert_eq!(
                records[0].f.get(Key::Why),
                (index == 1).then_some(&Val::Word("ambiguous"))
            );
            assert!(!records[0].is_bus_class());
        }
        let turn = g.slog().log().state().last[1].as_ref().unwrap();
        assert_eq!(turn.f.get(Key::Vref), Some(&Val::Id(first)));
        assert_eq!(turn.f.get(Key::VdurMs), Some(&Val::U(10)));
        assert_eq!(turn.f.get(Key::TokIn), Some(&Val::U(11)));
        assert_eq!(turn.f.get(Key::TokOut), Some(&Val::U(13)));
    }
}

#[test]
fn a_second_partial_vendor_interrupt_does_not_own_the_first_named_claim() {
    use aterm_slog::{ShortId, TxEvent, TxPos, TxState};
    let mut tab = CodexTab::new();
    tab.says_at(DONE, 900);
    tab.sweep_at(1_000);
    tab.says_at(WORKING, 1_010);
    tab.sweep_at(1_015);
    tab.hand.set(1_055);
    {
        let mut g = tab.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        let slog = g.slog_mut();
        let tracker = &mut slog.agent.as_mut().unwrap().tracker;
        for (off, ms, event) in [
            (
                0,
                1_000,
                TxEvent::Bind {
                    len: 0,
                    floor_ms: tab.wall(1_000),
                },
            ),
            (
                1,
                1_020,
                TxEvent::Prompt {
                    vref: ShortId::new("first").unwrap(),
                },
            ),
            (
                2,
                1_030,
                TxEvent::TurnEnd {
                    vdur_ms: 10,
                    tok_in: 11,
                    tok_out: 13,
                },
            ),
            (
                3,
                1_040,
                TxEvent::Prompt {
                    vref: ShortId::new("second").unwrap(),
                },
            ),
            (4, 1_050, TxEvent::Interrupt),
        ] {
            tracker.transcript(
                &mut slog.log,
                event,
                TxPos {
                    fp: 1,
                    id: None,
                    off,
                    wall_ms: tab.wall(ms),
                },
                TxState::Partial,
                now,
            );
        }
    }
    tab.says_at(DONE, 1_060);
    tab.sweep_at(1_065);
    let lines = tab.turn_lines();
    assert_eq!(tab.turns(), (1, false), "one engine span: {lines:#?}");
    assert_eq!(lines.len(), 2);
    assert_eq!(node_of(&lines[0]), node_of(&lines[1]));
    assert!(lines[0].ends_with(&format!(" t={}", tab.wall(1_015))));
    assert!(lines[1].ends_with(&format!(" t={}", tab.wall(1_060))));
    assert!(
        lines[1].contains(" turn out=done "),
        "a contained interruption from another vendor identity is not this claim's outcome: {lines:#?}"
    );
}

/// THE EDGES' TURNS SPEND THE RUN'S PROGRAM BUCKET: 2,000 sweeps 50 ms apart on the log's clock,
/// a whole codex turn between each two that no sweep shows, open no more turns than the bucket
/// pays for (its burst, then one a second: 100 s), and still about one a second.
#[test]
fn edge_turns_spend_the_runs_program_bucket() {
    let mut tab = CodexTab::new();
    tab.says_at(DONE, 900);
    tab.sweep_at(1_000);
    let (turns0, _) = tab.turns();
    const SWEEPS: u64 = 2_000;
    for i in 1..=SWEEPS {
        let ms = 1_000 + 50 * i;
        tab.says_at(WORKING, ms - 40);
        tab.says_at(DONE, ms - 20);
        tab.sweep_at(ms);
    }
    let (turns, open) = tab.turns();
    let opened = u64::from(turns - turns0);
    let refills = SWEEPS * 50 / PROGRAM_REFILL_MS;
    assert!(
        (refills..=PROGRAM_BURST + refills + 1).contains(&opened) && !open,
        "{opened} turns opened in {refills} s"
    );
}

/// EDGES MOVE ONLY A CODEX RUN'S TURNS, AND RING ITS WATCHERS: a beginning and a failed end fed
/// under a claude in front open no turn and wake nobody. CONTROL: under a codex they are one turn,
/// closed `out=error` at the end's time, and the watchers are rung once for it, so `await log
/// close=turn` hears it on that wake.
#[test]
fn edges_move_no_turn_but_a_codex_runs() {
    use crate::program_state::FedEdge;
    for name in ["claude", "codex"] {
        let (tl, hand) = opened();
        with_agent(&tl, name);
        hand.set(1_000);
        let totals = || tl.lock().unwrap().slog().log().state().totals.turns;
        let rung = || tl.lock().unwrap().watchers().records_rung();
        let (turns0, rung0) = (totals(), rung());
        let end = super::super::tests::WALL0 + 700;
        tl.lock().unwrap().note_program_edges(&[
            FedEdge::Began(Some(super::super::tests::WALL0 + 500)),
            FedEdge::Ended(W::Error, Some(end)),
        ]);
        let codex = name == "codex";
        let lines = lines_of(&tl);
        assert_eq!(totals() - turns0, u32::from(codex), "{name}: {lines:#?}");
        assert_eq!(rung() - rung0, u64::from(codex), "{name}: rung once");
        assert_eq!(
            lines
                .iter()
                .any(|l| l.contains(" turn out=error ") && l.ends_with(&format!(" t={end}"))),
            codex,
            "{name}: closed out=error at its end: {lines:#?}"
        );
    }
}
