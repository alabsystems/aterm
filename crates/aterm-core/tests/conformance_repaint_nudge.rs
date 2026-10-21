// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind of the `RepaintNudge` model (aterm-spec `models_resize_render`) to the real
//! machine and a real alternate-screen `Terminal`, in process: a fake clock and PTY size,
//! scripted program stand-ins (`Standin`), window resizes during the hold, a halt, and a
//! busy terminal lock, including past the host's hard deadline, which the World plays as
//! the host does.
//!
//! Every act the machine asks for must be enabled in the model (`Model::action_enabled`)
//! in the state projected just before it, and leave the machine in the phase the model's
//! move leads to; the owner hears of the end (`Act::GaveUp`) exactly when the move reports
//! `gaveup`. A silent `Dropped::NothingOwed` at a restore is the model's `GiveUp` with no
//! debt. After every schedule the engine is unresized, and an unanswered nudge leaves screen
//! and render debt byte-identical.

use std::collections::BTreeMap;

use aterm_core::terminal::Terminal;
use aterm_core::terminal::repaint_nudge::{
    Act, Dropped, Facts, GiveUp, Mode, PTY_HOLD_CAP_MS, Phase, QUIET_MAX_MS, QUIET_WAIT_MS, Reason,
    RepaintNudge,
};
use aterm_models_render::repaint_nudge_model;
use aterm_spec::derive::Model;

const ROWS: u16 = 12;
const COLS: u16 = 20;

/// How the program behaves when the PTY size it reads changes.
#[derive(Clone, Copy, Debug)]
enum Standin {
    /// Repaints (ED 2 + the whole UI) 5 ms after each size change.
    AnswersAtOnce,
    /// Reads the sizes and draws nothing.
    NeverAnswers,
    /// Repaints 200 ms after each size change.
    AnswersLate,
    /// Draws ordinary frames (cursor addressing, no clear) 20 ms after a size
    /// change and again at 100 ms, then repaints at 250 ms.
    DrawsBeforeAnswering,
    /// Repaints with cursor addressing only, never a full clear.
    NoEd2,
    /// Its SIGWINCH handler is busy: it reads the size this many ms after
    /// each change, then repaints at once if what it reads changed.
    Busy(u64),
    /// Repaints 5 ms after each size change, and draws a diff frame inside a
    /// DEC 2026 frame every 20 ms, forever: its output is never quiet.
    Streams,
}

struct World {
    term: Terminal,
    pty: (u16, u16),
    now: u64,
    /// Pending program output: (due ms, bytes).
    due: Vec<(u64, Vec<u8>)>,
    last_output_ms: u64,
    read: (u16, u16),
    /// Every size the program read as changed.
    sizes: Vec<(u16, u16)>,
    /// When a busy program's handler reads the size.
    reads: Vec<u64>,
    /// The window's resizes during the hold: (ms after the hold, rows).
    after_hold: Vec<(u64, u16)>,
    /// The same, due (host ms).
    resizes: Vec<(u64, u16)>,
    /// A halt that stands from this many ms after the hold.
    halt_after_hold: Option<u64>,
    /// The same, due (host ms).
    halt_at: Option<u64>,
    /// The host finds the terminal lock busy for this many ms after the hold
    /// (its `facts` answer nothing), so the machine is not stepped meanwhile.
    lock_busy_after_hold: Option<u64>,
    /// The same, until (host ms).
    lock_busy_until: Option<u64>,
    /// The host let the hold go with no step: the machine reads its counters
    /// at its next step, and so does the projection.
    rebase: bool,
    /// The act that put the machine at rest.
    ended: Option<Act>,
    /// The engine's size as the window last set it.
    engine: (u16, u16),
    machine: RepaintNudge,
    standin: Standin,
    halted: bool,
    model: Model,
    /// The model transitions the machine took, in order.
    acts: Vec<&'static str>,
    armed_at: u64,
    released_at: u64,
    clears_at_hold: u64,
    replaced_at_hold: u64,
    clears_at_release: u64,
    replaced_at_release: u64,
}

fn ui(tag: &str) -> Vec<u8> {
    let mut b = Vec::new();
    for r in 0..ROWS {
        b.extend_from_slice(format!("\x1b[{};1H{tag}{r}", r + 1).as_bytes());
    }
    b
}

fn with_clear(mut b: Vec<u8>) -> Vec<u8> {
    let mut out = b"\x1b[2J".to_vec();
    out.append(&mut b);
    out
}

impl World {
    /// The residue the nudge repairs: an alternate screen the program drew, and a flap
    /// with a draw between its halves.
    fn new(standin: Standin) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b[?1049h\x1b[2J");
        term.process(&ui("a"));
        term.resize(ROWS - 1, COLS);
        term.process(&ui("b")[..40]);
        term.resize(ROWS, COLS);
        assert!(term.render_debt().owed(), "the flap left a residue");
        Self {
            term,
            pty: (ROWS, COLS),
            now: 1_000_000,
            due: Vec::new(),
            last_output_ms: 0,
            read: (ROWS, COLS),
            sizes: Vec::new(),
            reads: Vec::new(),
            after_hold: Vec::new(),
            resizes: Vec::new(),
            halt_after_hold: None,
            halt_at: None,
            lock_busy_after_hold: None,
            lock_busy_until: None,
            rebase: false,
            ended: None,
            engine: (ROWS, COLS),
            machine: RepaintNudge::new(),
            standin,
            halted: false,
            model: repaint_nudge_model(),
            acts: Vec::new(),
            armed_at: 0,
            released_at: 0,
            clears_at_hold: 0,
            replaced_at_hold: 0,
            clears_at_release: 0,
            replaced_at_release: 0,
        }
    }

    fn set_pty(&mut self, size: (u16, u16)) {
        if size != self.pty {
            self.pty = size;
            self.on_winch();
        }
    }

    /// The program's SIGWINCH handler runs: it reads the size (a busy one,
    /// later) and, if it changed, reacts as its class does.
    fn on_winch(&mut self) {
        match self.standin {
            Standin::Busy(after) => self.reads.push(self.now + after),
            _ => self.read_size(),
        }
    }

    fn read_size(&mut self) {
        if self.pty == self.read {
            return;
        }
        self.read = self.pty;
        self.sizes.push(self.pty);
        let tag = if self.pty.0 < ROWS { "s" } else { "t" };
        match self.standin {
            Standin::AnswersAtOnce | Standin::Busy(_) | Standin::Streams => {
                self.due.push((self.now + 5, with_clear(ui(tag))));
            }
            Standin::NeverAnswers => {}
            Standin::AnswersLate => self.due.push((self.now + 200, with_clear(ui(tag)))),
            Standin::DrawsBeforeAnswering => {
                self.due.push((self.now + 20, b"\x1b[3;1Hdiff".to_vec()));
                self.due.push((self.now + 100, b"\x1b[4;1Hdiff".to_vec()));
                self.due.push((self.now + 250, with_clear(ui(tag))));
            }
            Standin::NoEd2 => self.due.push((self.now + 5, ui(tag))),
        }
    }

    /// The window resizes the engine, and its layout pass sets the PTY.
    fn window_resizes(&mut self) {
        let now = self.now;
        let (ready, later): (Vec<_>, Vec<_>) = self.resizes.drain(..).partition(|(t, _)| *t <= now);
        self.resizes = later;
        for (_, rows) in ready {
            self.term.resize(rows, COLS);
            self.engine = (rows, COLS);
            self.set_pty(self.engine);
        }
    }

    /// The program's own clock: a busy handler's reads, a stream's frames.
    fn program(&mut self) {
        let now = self.now;
        let (ready, later): (Vec<_>, Vec<_>) = self.reads.drain(..).partition(|t| *t <= now);
        self.reads = later;
        for _ in ready {
            self.read_size();
        }
        if matches!(self.standin, Standin::Streams) && now.is_multiple_of(20) {
            let row = self.read.0;
            let frame = format!("\x1b[?2026h\x1b[{row};1Hstream\x1b[K\x1b[?2026l");
            self.due.push((now, frame.into_bytes()));
        }
    }

    fn deliver(&mut self) {
        let now = self.now;
        let (ready, later): (Vec<_>, Vec<_>) = self.due.drain(..).partition(|(t, _)| *t <= now);
        self.due = later;
        for (_, bytes) in ready {
            self.term.process(&bytes);
            self.last_output_ms = now;
        }
    }

    fn facts(&self) -> Facts {
        let mut f = Facts::eligible(Mode::On, (self.term.rows(), self.term.cols()));
        f.owed = self.term.render_debt().owed();
        f.halted = self.halted;
        f.output_quiet_ms = self.now - self.last_output_ms;
        f.frame_open = self.term.modes().synchronized_output;
        f.full_clears = self.term.full_clear_count();
        f.screen_replaced = self.term.screen_replaced_count();
        f.pty = self.pty;
        f
    }

    /// The model state the real world is in, just before a step.
    fn project(&self, f: &Facts) -> BTreeMap<&'static str, i64> {
        let mut s = self.model.init_state();
        let phase = model_phase(self.machine.phase());
        let waited = |since: u64| self.now.saturating_sub(since);
        let answered =
            f.full_clears > self.clears_at_hold || f.screen_replaced > self.replaced_at_hold;
        s.insert("phase", phase);
        s.insert("held", i64::from(f.pty != f.engine));
        s.insert("engine", i64::from(f.engine != (ROWS, COLS)));
        s.insert("debt", i64::from(f.owed));
        s.insert(
            "safe",
            i64::from(f.output_quiet_ms >= QUIET_WAIT_MS && !f.frame_open),
        );
        s.insert("halt", i64::from(f.halted));
        s.insert("tries", i64::from(self.machine.nudged()));
        s.insert(
            "moved",
            i64::from(
                matches!(self.machine.phase(), Phase::Short | Phase::Restoring)
                    && f.engine != self.machine.hold_engine(),
            ),
        );
        // `tick > Lim - 1` is "the wait is spent".
        let spent = match self.machine.phase() {
            Phase::Armed => waited(self.armed_at) >= QUIET_MAX_MS,
            Phase::Short => waited(self.machine.hold_started_ms()) >= 1_500,
            Phase::Restoring => waited(self.released_at) >= 1_500,
            _ => false,
        };
        s.insert("tick", if spent { 2 } else { 0 });
        let ack1 = match self.machine.phase() {
            Phase::Short => answered,
            Phase::Restoring => self.machine.first_answer_ms().is_some(),
            _ => false,
        };
        s.insert("ack1", i64::from(ack1));
        s.insert(
            "ack2",
            i64::from(
                matches!(self.machine.phase(), Phase::Restoring)
                    && (f.full_clears > self.clears_at_release
                        || f.screen_replaced > self.replaced_at_release),
            ),
        );
        s
    }
}

/// The model's `phase` for the machine's.
const fn model_phase(phase: Phase) -> i64 {
    match phase {
        Phase::Idle => 0,
        Phase::Armed => 1,
        Phase::Short => 2,
        Phase::Restoring => 3,
        Phase::Rest => 4,
    }
}

impl World {
    fn run(&mut self, horizon_ms: u64) {
        assert!(self.machine.arm(Mode::On, Reason::Debt, 1, self.now));
        self.armed_at = self.now;
        for _ in 0..horizon_ms / 10 {
            self.now += 10;
            self.window_resizes();
            if self.halt_at.is_some_and(|t| t <= self.now) {
                self.halted = true;
            }
            self.program();
            self.deliver();
            assert_eq!(
                (self.term.rows(), self.term.cols()),
                self.engine,
                "the nudge never resizes the engine"
            );
            if self.lock_busy_until.is_some_and(|t| self.now < t) {
                self.host_deadline();
                continue;
            }
            let f = self.facts();
            if std::mem::take(&mut self.rebase) {
                self.clears_at_release = f.full_clears;
                self.replaced_at_release = f.screen_replaced;
            }
            let pre = self.project(&f);
            let was = self.machine.phase();
            let act = self.machine.step(&f, self.now);
            let name: Option<&'static str> = match act {
                Act::Hold => Some("Hold"),
                Act::Release => Some("Release"),
                Act::Forget => Some("Forget"),
                Act::Healed { .. } => Some("Finish"),
                Act::GaveUp(GiveUp::NoAnswer) => Some("GiveUp"),
                // The restore's wait spent on a tab that owes nothing: the
                // model's give-up with no debt, which reports nothing.
                Act::Dropped(Dropped::NothingOwed) if was == Phase::Restoring => Some("GiveUp"),
                Act::Dropped(Dropped::Superseded) => Some("Supersede"),
                _ => None,
            };
            if let Some(name) = name {
                assert!(
                    self.model.action_enabled(name, &pre),
                    "{name} is not enabled in the model at {pre:?} (act {act:?})"
                );
                let mut post = pre.clone();
                assert!(self.model.fire(name, &mut post));
                assert_eq!(
                    model_phase(self.machine.phase()),
                    post["phase"],
                    "{name} leaves the machine where the model's move does (act {act:?})"
                );
                // The owner hears of an end exactly when the model reports it.
                assert_eq!(
                    matches!(act, Act::GaveUp(_)),
                    post["gaveup"] == 1,
                    "{name} reports what the model's move does (act {act:?}, debt {})",
                    pre["debt"]
                );
                self.acts.push(name);
            }
            if self.ended.is_none() && self.machine.phase() == Phase::Rest {
                self.ended = Some(act);
            }
            match act {
                Act::Hold => {
                    self.clears_at_hold = f.full_clears;
                    self.replaced_at_hold = f.screen_replaced;
                    let now = self.now;
                    self.resizes = self.after_hold.iter().map(|(t, r)| (now + t, *r)).collect();
                    self.halt_at = self.halt_after_hold.map(|t| now + t);
                    self.lock_busy_until = self.lock_busy_after_hold.map(|t| now + t);
                    self.set_pty((f.engine.0 - 1, f.engine.1));
                }
                Act::Release | Act::Forget => {
                    self.clears_at_release = f.full_clears;
                    self.replaced_at_release = f.screen_replaced;
                    self.released_at = self.now;
                    if act == Act::Release {
                        self.set_pty(f.engine);
                    }
                }
                _ => {}
            }
            // The headline safety property: the PTY is short only while the machine holds it.
            if self.pty != (self.term.rows(), self.term.cols()) {
                assert_eq!(self.machine.phase(), Phase::Short, "a short PTY is held");
            }
        }
        // Whatever happened, the PTY ends at the engine's dims.
        assert_eq!(self.pty, (self.term.rows(), self.term.cols()));
    }

    /// The host's hard deadline (`NudgeHost::tick`'s cap sweep), run whether or not the
    /// machine could be stepped: past `PTY_HOLD_CAP_MS` it lets the hold go and the attempt
    /// goes on as after a release (the model's `ReleaseByDeadline`).
    fn host_deadline(&mut self) {
        if self.machine.phase() != Phase::Short
            || self.now - self.machine.hold_started_ms() < PTY_HOLD_CAP_MS
        {
            return;
        }
        let f = self.facts();
        let mut pre = self.project(&f);
        // `age > Cap - 1` is "past the deadline".
        let cap = self.model.consts.iter().find(|(name, _)| *name == "Cap");
        pre.insert("age", cap.map_or(i64::MAX, |(_, cap)| *cap));
        assert!(
            self.model.action_enabled("ReleaseByDeadline", &pre),
            "ReleaseByDeadline is not enabled in the model at {pre:?}"
        );
        let mut post = pre.clone();
        assert!(self.model.fire("ReleaseByDeadline", &mut post));
        assert!(self.machine.let_go(self.now), "a hold in flight is let go");
        assert_eq!(model_phase(self.machine.phase()), post["phase"]);
        self.acts.push("ReleaseByDeadline");
        self.released_at = self.now;
        self.rebase = true;
        self.set_pty(f.engine);
    }
}

fn screen(t: &Terminal) -> Vec<String> {
    (0..usize::from(t.rows()))
        .map(|r| t.row_text(r).unwrap_or_default().trim_end().to_string())
        .collect()
}

#[test]
fn a_program_that_answers_at_once_is_repaired_with_the_engine_untouched() {
    let mut w = World::new(Standin::AnswersAtOnce);
    w.run(4_000);
    assert_eq!(w.acts, ["Hold", "Release", "Finish"]);
    assert!(
        !w.term.render_debt().owed(),
        "the second repaint cleared the debt"
    );
    assert_eq!(
        screen(&w.term)[0],
        "t0",
        "the program's repaint at the true size"
    );
}

#[test]
fn a_program_that_never_answers_leaves_the_screen_and_the_debt_as_they_were() {
    let mut w = World::new(Standin::NeverAnswers);
    let (text, debt) = (screen(&w.term), w.term.render_debt());
    w.run(8_000);
    assert_eq!(w.acts, ["Hold", "Release", "GiveUp"]);
    assert_eq!(screen(&w.term), text, "no answer, no change");
    assert_eq!(w.term.render_debt(), debt);
    assert_eq!(w.machine.nudged(), 1, "one try");
    // The same when the terminal lock stays busy past the host's deadline: the deadline
    // lets the hold go, and the attempt is judged as after any release.
    let mut w = World::new(Standin::NeverAnswers);
    w.lock_busy_after_hold = Some(PTY_HOLD_CAP_MS + 500);
    w.run(8_000);
    assert_eq!(w.acts, ["Hold", "ReleaseByDeadline", "GiveUp"]);
    assert_eq!(screen(&w.term), text);
    assert_eq!(w.term.render_debt(), debt);
}

#[test]
fn a_program_that_answers_200ms_late_is_still_repaired() {
    let mut w = World::new(Standin::AnswersLate);
    w.run(6_000);
    assert_eq!(w.acts, ["Hold", "Release", "Finish"]);
    assert!(!w.term.render_debt().owed());
}

#[test]
fn ordinary_frames_before_a_late_answer_are_not_taken_for_it() {
    let mut w = World::new(Standin::DrawsBeforeAnswering);
    w.run(8_000);
    // The two diff frames did not release the hold: the ED 2 did.
    assert_eq!(w.acts, ["Hold", "Release", "Finish"]);
    assert!(!w.term.render_debt().owed());
}

#[test]
fn a_program_that_repaints_without_a_full_clear_is_tried_once_and_abandoned() {
    let mut w = World::new(Standin::NoEd2);
    w.run(8_000);
    assert_eq!(w.acts, ["Hold", "Release", "GiveUp"]);
    assert_eq!(w.machine.nudged(), 1);
    assert!(w.term.render_debt().owed(), "no ED 2 clears no debt");
    assert_eq!(w.machine.phase(), Phase::Rest);
}

/// A program that streams through its repaints (its output never quiet for
/// `QUIET_WAIT_MS`) still answered both sizes: healed, not a give-up.
#[test]
fn a_program_that_streams_through_both_answers_is_healed() {
    let mut w = World::new(Standin::Streams);
    w.run(8_000);
    assert_eq!(w.acts, ["Hold", "Release", "Finish"]);
    assert_eq!(w.sizes, [(ROWS - 1, COLS), (ROWS, COLS)]);
    assert!(!w.term.render_debt().owed());
}

/// The window flaps during the hold and a busy program reads neither half: the hold is
/// forgotten and the attempt, its one try spent, is a give-up the owner hears. A size that
/// lasts supersedes it. A program that answered both sizes while the lock was busy repaired
/// the tab, so the attempt ends silently with nothing owed.
#[test]
fn a_flap_the_program_never_saw_during_the_hold_is_a_give_up() {
    let busy = |resizes: &[(u64, u16)]| {
        let mut w = World::new(Standin::Busy(2_000));
        w.after_hold = resizes.to_vec();
        w.run(6_000);
        assert!(
            !w.sizes.contains(&(ROWS - 1, COLS)),
            "it never read the short size: {:?}",
            w.sizes
        );
        w
    };
    let before = World::new(Standin::Busy(2_000));
    // The flap's halves between two steps, and with a step on its middle.
    for flap in [
        [(200, ROWS - 1), (200, ROWS)],
        [(200, ROWS - 1), (250, ROWS)],
    ] {
        let w = busy(&flap);
        assert_eq!(w.acts, ["Hold", "Forget", "GiveUp"], "{flap:?}");
        assert_eq!(screen(&w.term), screen(&before.term), "still displaced");
        assert_eq!(w.term.render_debt(), before.term.render_debt());
    }
    let w = busy(&[(200, ROWS - 2)]);
    assert_eq!(w.acts, ["Hold", "Forget", "Supersede"]);
    let mut w = World::new(Standin::AnswersAtOnce);
    w.after_hold = vec![(20, ROWS - 1), (30, ROWS)];
    w.lock_busy_after_hold = Some(100);
    w.run(6_000);
    assert_eq!(w.sizes, [(ROWS - 1, COLS), (ROWS, COLS)], "both answered");
    assert_eq!(w.acts, ["Hold", "Forget", "GiveUp"]);
    assert_eq!(w.ended, Some(Act::Dropped(Dropped::NothingOwed)));
    assert!(!w.term.render_debt().owed());
}

/// A halt that stands before the hold blocks it. One that lands during the hold lifts it
/// at once, and the attempt is judged as after any release: a busy program that read
/// neither size is a give-up the owner hears.
#[test]
fn a_halt_blocks_the_hold_and_one_during_it_is_judged() {
    let mut w = World::new(Standin::AnswersAtOnce);
    w.halted = true;
    w.run(3_000);
    assert!(w.acts.is_empty(), "{:?}", w.acts);
    assert_eq!(w.machine.phase(), Phase::Armed);
    let f = w.facts();
    let pre = w.project(&f);
    assert!(!w.model.action_enabled("Hold", &pre));
    let mut w = World::new(Standin::Busy(2_000));
    w.halt_after_hold = Some(200);
    w.run(6_000);
    assert_eq!(w.acts, ["Hold", "Release", "GiveUp"]);
}
