// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The claimed turns' feed across foreground moves (ATERM-KI-1029-1): a real engine's OSC 7501
//! and OSC 133 bytes, each batch at the log's wall clock, read as the status sweep reads them
//! ([`Tab::sweep`]) into a real session log.

use std::sync::Mutex;

use aterm_core::terminal::program_state::{ProgramApp, ProgramState, ProgramStateWord as W};

use super::super::tests::{Engine, Hand, WALL0, lines_of, opened};
use super::ProgramRead;
use crate::session_timeline::SessionTimeline;

/// The session's shell.
const ROOT: i32 = 100;
/// A codex's group.
const CODEX: i32 = 300;
/// The group that takes the front after it.
const NEXT: i32 = 301;

fn codex(state: W) -> Option<ProgramState> {
    Some(ProgramState {
        state,
        kind: None,
        app: ProgramApp::Codex,
    })
}

/// A tab whose shell (group [`ROOT`], named `zsh`) is at its prompt, its log on the hand.
struct Tab {
    tl: Mutex<SessionTimeline>,
    hand: Hand,
    engine: Engine,
    /// The reader of the engine's edges alone, with no resumed beginning (a negative control).
    no_resumed: bool,
}

impl Tab {
    fn new() -> Self {
        let (tl, hand) = opened();
        let mut tab = Self {
            tl,
            hand,
            engine: Engine::new(),
            no_resumed: false,
        };
        tab.bytes(0, b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\");
        tab.sweep(10, ROOT);
        tab.name(ROOT, "zsh");
        tab.sweep(20, ROOT);
        tab
    }

    fn bytes(&mut self, ms: u64, bytes: &[u8]) {
        self.hand.set(ms);
        self.engine.bytes(&self.tl.lock().unwrap(), bytes);
    }

    /// The program's report at `ms` (`None`: `state=clear`).
    fn says(&mut self, ms: u64, shown: Option<ProgramState>) {
        self.hand.set(ms);
        self.engine.parse(&self.tl.lock().unwrap(), shown);
    }

    /// The shell runs `cmd` at `ms` (its OSC 133 C).
    fn runs(&mut self, ms: u64, cmd: &str) {
        self.bytes(ms, format!("{cmd}\r\n\x1b]133;C\x1b\\").as_bytes());
    }

    /// The command ends at `ms` (its D) and the prompt is back.
    fn ends(&mut self, ms: u64) {
        self.bytes(ms, b"\x1b]133;D;0\x1b\\\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\");
    }

    /// The resolver names group `pgid`'s program.
    fn name(&self, pgid: i32, name: &str) {
        let _ = self.tl.lock().unwrap().set_program(pgid, Some(name.into()));
    }

    /// One status sweep at `ms` with group `pgid` in front, in the sweep's order under one hold
    /// of the timeline: the engine read (turn edges and blocks), the session-log step (a move's
    /// edges, the block read, the move), the program bucket's tick, then the edges past the move.
    fn sweep(&mut self, ms: u64, pgid: i32) {
        self.hand.set(ms);
        let mut g = self.tl.lock().unwrap();
        let term = self.engine.term();
        let mut read = ProgramRead::of(term, g.slog().program_cursor());
        if self.no_resumed {
            read.drop_resumed();
        }
        let blocks = crate::session_status::block_read(
            term,
            g.slog().block_cursor(),
            g.slog().opened_wall(),
            &crate::command_ran::Ran::default(),
        );
        let _ = g.sweep_slog_turns(&blocks, false, ROOT, pgid, &mut read);
        let at = g.slog().now();
        g.slog_mut().program_tick(at);
        g.feed_program_turns(&mut read);
    }

    /// The shown report's mark, as a sweep whose gate showed a move writes it.
    fn mark(&self) {
        let r = self.engine.term().program_state();
        self.tl.lock().unwrap().note_program_state(r.state, r.began);
    }

    /// A codex (group [`CODEX`]) started at `ms` from the prompt, in front and named, its agent
    /// run open under its command: the run's node.
    fn codex_started(&mut self, ms: u64) -> String {
        self.runs(ms, "codex");
        self.sweep(ms + 10, CODEX);
        self.name(CODEX, "codex");
        self.sweep(ms + 20, CODEX);
        self.run_of("codex")
    }

    /// The node of the newest run whose `open` names `agent`.
    fn run_of(&self, agent: &str) -> String {
        let lines = self.lines();
        let open = lines
            .iter()
            .rev()
            .find(|l| l.contains(" open r") && l.contains(&format!("agent={agent}")))
            .unwrap_or_else(|| panic!("no {agent} run: {lines:#?}"));
        open.split(' ').nth(2).expect("a node").to_string()
    }

    fn lines(&self) -> Vec<String> {
        lines_of(&self.tl)
    }

    /// The turn opens and closes, oldest first.
    fn turns(&self) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|l| l.contains(" turn ") && (l.contains(" open t") || l.contains(" close t")))
            .collect()
    }

    /// The turn opens under run `run`.
    fn turns_under(&self, run: &str) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|l| l.contains(" open t") && l.contains(&format!(" par={run} ")))
            .collect()
    }
}

const fn wall(ms: u64) -> u64 {
    WALL0 + ms
}

/// The close of turn `node`, if one was written.
fn close_of<'a>(lines: &'a [String], node: &str) -> Option<&'a String> {
    lines
        .iter()
        .find(|l| l.contains(&format!(" close {node} turn ")))
}

/// The node of an `open` line.
fn node_of(line: &str) -> &str {
    line.split(' ').nth(2).unwrap_or("-")
}

/// R3a, A MOVE FEEDS THE LEAVING CODEX ITS EDGES BEFORE ITS BLOCK END CLOSES IT: a codex in front
/// writes a whole turn, its `clear` at exit and its command's D, and the shell takes the front
/// back, all between two sweeps. The sweep that sees the move reads the D too; the edges before
/// the `clear` go to the codex run still open, so its turn is logged and closed `done` at the
/// rest's own time, before the block read closes the run. A host that feeds after the block read,
/// or drops a move's edges, logs no turn.
#[test]
fn a_move_feeds_the_leaving_codex_its_edges_before_its_block_end_closes_it() {
    let mut tab = Tab::new();
    let run = tab.codex_started(100);
    tab.says(1_000, codex(W::Working));
    tab.says(1_300, codex(W::Done));
    tab.says(1_350, None);
    tab.ends(1_400);
    tab.sweep(1_500, ROOT);
    let lines = tab.lines();
    let turns = tab.turns_under(&run);
    let [open] = turns.as_slice() else {
        panic!("one turn in the codex run: {lines:#?}");
    };
    assert!(
        open.contains(" src=engine conf=claim ") && open.ends_with(&format!(" t={}", wall(1_000))),
        "opened at its batch: {lines:#?}"
    );
    let close = close_of(&lines, node_of(open)).expect("its close");
    assert!(
        close.contains(" turn out=done ") && close.ends_with(&format!(" t={}", wall(1_300))),
        "closed done at its rest: {lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!(" close {run} ")) && !l.contains(" turn ")),
        "PRECONDITION: the block read closed the run: {lines:#?}"
    );
}

/// R3b, EDGES WAIT FOR THE CODEX RUN AND DROP UNDER ANOTHER PROGRAM: a codex's `working` read in
/// the sweep that sees it take the front, and in the next while it is still unnamed, waits; once
/// it is named its run's turn is that beginning's, opened no earlier than the run. Under another
/// program in front the same edges open nothing: a script runtime a codex may run under (`node`,
/// no agent run) counts them (`dropped=1` on the next mark); a claude, and the shell with a stale
/// report, drop them quietly.
#[test]
fn edges_wait_for_the_codex_run_and_drop_under_another_program() {
    let mut tab = Tab::new();
    tab.runs(100, "codex");
    tab.says(105, codex(W::Working));
    tab.sweep(110, CODEX);
    tab.sweep(120, CODEX);
    assert_eq!(tab.turns(), Vec::<String>::new(), "held while unnamed");
    tab.name(CODEX, "codex");
    tab.sweep(130, CODEX);
    let run = tab.run_of("codex");
    let lines = tab.lines();
    let turns = tab.turns_under(&run);
    assert!(
        turns.len() == 1 && turns[0].contains(" src=engine conf=claim "),
        "the held beginning is the codex run's turn: {lines:#?}"
    );
    for (name, counted) in [("node", true), ("claude", false), ("zsh", false)] {
        let mut tab = Tab::new();
        let front = if name == "zsh" {
            tab.says(50, codex(W::Done));
            ROOT
        } else {
            tab.runs(100, name);
            tab.sweep(110, CODEX);
            tab.name(CODEX, name);
            tab.sweep(120, CODEX);
            CODEX
        };
        tab.says(200, codex(W::Working));
        tab.says(250, codex(W::Done));
        tab.sweep(300, front);
        tab.says(400, codex(W::Idle));
        tab.mark();
        let lines = tab.lines();
        assert_eq!(
            tab.turns(),
            Vec::<String>::new(),
            "{name}: no turn: {lines:#?}"
        );
        let said = lines
            .iter()
            .filter(|l| l.contains(" program state=idle "))
            .any(|l| l.contains(" dropped=1 "));
        assert_eq!(
            said, counted,
            "{name}: counted on the next mark: {lines:#?}"
        );
    }
}

/// A SHELL-TO-CODEX MOVE'S EDGES ARE THE ARRIVING CODEX'S: no `clear` past the cursor and the shell
/// left, so the `working` and `done` the codex wrote before the sweep saw it take the front are
/// its own: held until it is named, then its run's one turn, closed `done`. A host that judges a
/// move's edges by the program that left drops them.
#[test]
fn a_shell_to_codex_moves_edges_are_the_arriving_codexs() {
    let mut tab = Tab::new();
    tab.runs(100, "codex");
    tab.says(105, codex(W::Working));
    tab.says(140, codex(W::Done));
    tab.sweep(150, CODEX);
    tab.name(CODEX, "codex");
    tab.sweep(160, CODEX);
    let run = tab.run_of("codex");
    let lines = tab.lines();
    let turns = tab.turns_under(&run);
    let [open] = turns.as_slice() else {
        panic!("one turn in the arriving codex's run: {lines:#?}");
    };
    let close = close_of(&lines, node_of(open)).expect("its close");
    assert!(close.contains(" turn out=done "), "{lines:#?}");
}

/// A KILLED CODEX'S EDGES ARE SAID, NOT GIVEN TO ITS SUCCESSOR: codex A, in front with a turn
/// logged, writes `working` and is killed (no `clear`), and codex B takes the front before a sweep
/// read it. OSC 7501 names the app, not the process, so the edge cannot be told to either: it is
/// counted (`dropped=1` on A's close, which had no mark pending to say it), A's run holds only
/// its first turn, and B's run, once named, holds none until its own beginning.
#[test]
fn a_killed_codexs_edges_are_said_not_given_to_its_successor() {
    let mut tab = Tab::new();
    let a = tab.codex_started(100);
    tab.says(900, codex(W::Working));
    tab.says(950, codex(W::Done));
    tab.sweep(1_000, CODEX);
    assert_eq!(tab.turns_under(&a).len(), 1, "PRECONDITION: A's turn");
    tab.says(1_010, codex(W::Working));
    tab.sweep(1_200, NEXT);
    tab.name(NEXT, "codex");
    tab.sweep(1_300, NEXT);
    let b = tab.run_of("codex");
    assert_ne!(a, b, "PRECONDITION: B's own run");
    let lines = tab.lines();
    assert_eq!(
        tab.turns_under(&a).len(),
        1,
        "A's first turn alone: {lines:#?}"
    );
    assert_eq!(
        tab.turns_under(&b),
        Vec::<String>::new(),
        "none for B: {lines:#?}"
    );
    let closed = lines
        .iter()
        .find(|l| l.contains(&format!(" close {a} ")) && !l.contains(" turn "))
        .expect("A's run closed");
    assert!(
        closed.contains(" dropped=1 "),
        "said on A's close: {closed}"
    );
    tab.says(1_400, codex(W::Done));
    tab.says(1_500, codex(W::Working));
    tab.sweep(1_600, NEXT);
    assert_eq!(
        tab.turns_under(&b).len(),
        1,
        "B's own beginning: {:#?}",
        tab.lines()
    );
}

/// A CODEX RESUMED AFTER CTRL+Z LOGS ITS TURN IN ITS NEW RUN: mid-turn, codex-alab writes its
/// `clear` as it stops; the shell takes the front (the run closes), `fg` gives it back, and once
/// named the codex re-sends `working`, then `done`. The engine draws a resumed beginning for the
/// re-sent `working` (a turn re-entered while no report stood, which `turns` does not count), so
/// the new run holds one claimed turn, opened at that batch and closed `done`. NEGATIVE CONTROL:
/// the same feed without the resumed entry logs no turn in the new run.
#[test]
fn a_codex_resumed_after_ctrl_z_logs_its_turn_in_its_new_run() {
    for no_resumed in [false, true] {
        let mut tab = Tab::new();
        tab.no_resumed = no_resumed;
        let first = tab.codex_started(100);
        tab.says(1_000, codex(W::Working));
        tab.sweep(1_050, CODEX);
        assert_eq!(tab.turns_under(&first).len(), 1, "PRECONDITION: its turn");
        tab.says(1_100, None);
        tab.bytes(1_110, b"\r\n\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\");
        tab.sweep(1_150, ROOT);
        tab.bytes(1_200, b"fg\r\n");
        tab.sweep(1_250, CODEX);
        tab.name(CODEX, "codex");
        tab.sweep(1_300, CODEX);
        let next = tab.run_of("codex");
        assert_ne!(first, next, "PRECONDITION: a new run");
        let turns0 = tab.engine.term().program_state().turns;
        tab.says(1_350, codex(W::Working));
        assert_eq!(
            tab.engine.term().program_state().turns,
            turns0,
            "PRECONDITION: the re-sent working is no counted turn"
        );
        tab.sweep(1_400, CODEX);
        tab.says(1_500, codex(W::Done));
        tab.sweep(1_550, CODEX);
        let lines = tab.lines();
        let turns = tab.turns_under(&next);
        if no_resumed {
            assert_eq!(turns, Vec::<String>::new(), "NEGATIVE CONTROL: {lines:#?}");
            continue;
        }
        let [open] = turns.as_slice() else {
            panic!("one turn in the new run: {lines:#?}");
        };
        assert!(
            open.contains(" src=engine conf=claim ")
                && open.ends_with(&format!(" t={}", wall(1_350))),
            "opened at the re-sent working: {lines:#?}"
        );
        let close = close_of(&lines, node_of(open)).expect("its close");
        assert!(
            close.contains(" turn out=done ") && close.ends_with(&format!(" t={}", wall(1_500))),
            "{lines:#?}"
        );
    }
}

/// R1 AND R2, A RING OVERFLOW IS A GAP AND A REFUSED TURN IS SAID: 40 whole turns between two
/// sweeps outrun the engine's ring of 32, so the log writes `gap why=evicted lost=8`; of the 32
/// it reads the turn bucket opens its burst and refuses the rest, which the next `program` mark
/// says (`dropped=12`). A turn refused later, with no mark pending when the run closes, is said on
/// the run's close (`dropped=1`) and adds no `program-state` row.
#[test]
fn a_ring_overflow_is_a_gap_and_a_refused_turn_is_said_on_the_next_program_mark() {
    let burst = crate::session_log::observed::PROGRAM_BURST;
    let kept = aterm_core::terminal::program_state::PROGRAM_TURNS_KEPT as u64;
    let mut tab = Tab::new();
    let run = tab.codex_started(100);
    for i in 0..40 {
        tab.says(1_000 + 2 * i, codex(W::Working));
        tab.says(1_001 + 2 * i, codex(W::Done));
    }
    tab.sweep(1_200, CODEX);
    let lines = tab.lines();
    assert!(
        lines.iter().any(
            |l| l.contains(" gap why=evicted ") && l.contains(&format!(" lost={} ", 40 - kept))
        ),
        "the ring's loss is a gap: {lines:#?}"
    );
    assert_eq!(
        tab.turns_under(&run).len() as u64,
        burst,
        "the turn bucket's burst: {lines:#?}"
    );
    tab.says(1_300, codex(W::Idle));
    tab.mark();
    let lines = tab.lines();
    let mark = lines
        .iter()
        .rev()
        .find(|l| l.contains(" program state=idle "))
        .expect("the next mark");
    assert!(
        mark.contains(&format!(" dropped={} ", kept - burst)),
        "the refused turns, said: {mark}"
    );
    tab.says(1_400, codex(W::Working));
    tab.says(1_410, codex(W::Done));
    tab.sweep(1_420, CODEX);
    let rows = |tab: &Tab| {
        tab.tl
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == crate::session_timeline::PROGRAM_STATE)
            .count()
    };
    let rows0 = rows(&tab);
    tab.says(1_450, None);
    tab.ends(1_460);
    tab.sweep(1_500, ROOT);
    let lines = tab.lines();
    let closed = lines
        .iter()
        .find(|l| l.contains(&format!(" close {run} ")) && !l.contains(" turn "))
        .expect("the run closed");
    assert!(
        closed.contains(" dropped=1 "),
        "said on the close: {closed}"
    );
    assert_eq!(rows(&tab), rows0, "no program-state row for it");
}

/// An evicted prefix is acknowledged even when every retained edge is after the clear and
/// waits for an unnamed successor. Naming it must not report the same lost turn again.
#[test]
fn a_lost_prefix_is_said_once_while_an_unnamed_successors_edges_wait() {
    let kept = aterm_core::terminal::program_state::PROGRAM_TURNS_KEPT as u64;
    let mut tab = Tab::new();
    let first = tab.codex_started(100);
    tab.says(200, codex(W::Working));
    tab.says(210, codex(W::Done));
    tab.says(220, None);
    tab.ends(230);
    tab.runs(240, "codex");
    for i in 0..kept {
        tab.says(300 + 2 * i, codex(W::Working));
        tab.says(301 + 2 * i, codex(W::Done));
    }
    tab.sweep(400, NEXT);
    let gaps = |tab: &Tab| {
        tab.lines()
            .into_iter()
            .filter(|line| line.contains(" gap why=evicted "))
            .collect::<Vec<_>>()
    };
    let said = gaps(&tab);
    assert_eq!(said.len(), 1, "the leaving turn was evicted: {said:#?}");
    assert!(said[0].contains(" lost=1 "), "one lost turn: {said:#?}");
    assert!(
        tab.turns_under(&first).is_empty(),
        "the evicted turn has no retained edge"
    );
    tab.name(NEXT, "codex");
    tab.sweep(500, NEXT);
    let successor = tab.run_of("codex");
    assert_ne!(first, successor, "the successor has its own run");
    assert_eq!(
        gaps(&tab),
        said,
        "an acknowledged loss is not reported twice"
    );
    assert_eq!(
        tab.turns_under(&successor).len() as u64,
        crate::session_log::observed::PROGRAM_BURST,
        "retained successor turns reach their own run"
    );
    tab.sweep(600, NEXT);
    assert_eq!(gaps(&tab), said, "a later sweep reports no old loss");
}

/// A SUCCESSOR'S FIRST READING FEEDS ITS END ONLY, AND RESUMED EDGES (R3c): across a seamless
/// update the engine restored from its carry keeps no edge of the turn its predecessor logged,
/// only a resumed beginning, so the successor's first reading of the turns (no cursor yet) opens
/// no second turn for the carried one, and the `done` after it closes that turn. A turn begun
/// during the update, which no predecessor sweep read, is the successor's resumed beginning:
/// logged once. A host that feeds nothing at a first reading logs no turn begun during the
/// update.
#[test]
fn a_successors_first_reading_feeds_its_end_only_and_resumed_edges() {
    use super::super::tests::{hand_over, producer, successors_first_sweep, swept};
    for begun_during in [false, true] {
        let (p, store, hand) = producer();
        let mut engine = Engine::new();
        {
            let mut g = p.ctx.timeline.lock().unwrap();
            hand.set(10);
            assert!(g.note_foreground_group(300));
            assert!(g.set_program(300, Some("codex".into())));
            swept(&mut g);
            hand.set(1_000);
            engine.writes(&mut g, codex(W::Working));
            if begun_during {
                hand.set(1_100);
                engine.writes(&mut g, codex(W::Done));
                hand.set(1_200);
                engine.parse(&g, codex(W::Working));
            }
        }
        let carried = engine.term().checkpoint_carry(0).expect("a carry");
        let mut successor = Engine::of(aterm_core::terminal::Terminal::from_checkpoint(&carried));
        let (s, _store2, hand2) = hand_over(&p, &store, 2_000, true);
        let seq0 = s.ctx.timeline.lock().unwrap().slog().log().last_seq();
        {
            let mut g = s.ctx.timeline.lock().unwrap();
            successors_first_sweep(&mut g, "codex");
            hand2.set(100);
            successor.feed(&mut g);
            hand2.set(150);
            successor.parse(&g, codex(W::Done));
            hand2.set(200);
            successor.feed(&mut g);
        }
        let lines: Vec<String> = lines_of(&s.ctx.timeline)
            .into_iter()
            .filter(|l| {
                l.split(' ')
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .is_some_and(|s| s > seq0)
            })
            .collect();
        let opens: Vec<&String> = lines.iter().filter(|l| l.contains(" open t")).collect();
        let closes: Vec<&String> = lines
            .iter()
            .filter(|l| l.contains(" close t") && l.contains(" turn out=done "))
            .collect();
        assert_eq!(
            (opens.len(), closes.len()),
            (usize::from(begun_during), 1),
            "begun during the update {begun_during}: {lines:#?}"
        );
    }
}
