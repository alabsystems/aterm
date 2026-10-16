// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The turn tracker (design §2.4): at most one open turn per agent run,
//! driven by the screen verdict word.
//!
//! | Word (detail) | Effect |
//! |---|---|
//! | `busy`, no open turn | open a turn, `src=screen conf=strong` |
//! | `prompt` (`question`) | open a `question` step inside the turn |
//! | `prompt` (any other kind) | open an `approval box=<kind>` step; leaving `prompt` closes it |
//! | `wall:<kind>` | open a `wall` step; a wall held through the settle closes the turn `out=wall:<kind>` |
//! | `wall:unresponsive` | open a `stall` step; it never closes the turn |
//! | `survey` | open a `survey` step |
//! | `idle`, held for 1 s | close the turn `out=done` (`interrupted` when the screen said so) |
//! | `question`, held for 1 s | close the turn `out=asked conf=heuristic` |
//! | `-` | close the turn `out=gone` |
//! | `unknown` | nothing: never treated as idle |
//!
//! The 1 s settle absorbs the 220 ms mid-answer `idle` measured on Codex. It
//! is a host-scheduled wake, not a poll: every call returns the monotonic
//! time the tracker wants [`TurnTracker::tick`] at, and the tracker reads no
//! clock. A wake may come early (it changes nothing) or late (the next call,
//! a verdict included, applies the settle first). A settled turn's close is
//! dated when its resting word first held, so `t=` and `dur_ms` describe
//! the turn, not the wait.
//!
//! A box or dialog seen with no open turn opens one first (`conf=heuristic`:
//! the busy verdict was missed between samples). A wall or stall seen with
//! no open turn opens its step on the run (`orphan=1`), and a wall that
//! closes its turn at rest continues there (`cont=` its step in the turn,
//! counted once), so its need outlives the turn and closes when the wall
//! lifts. When the agent leaves (`-`), what it had open closes `out=gone`.
//!
//! A `ctl turn` that arrives while the agent rests closes the resting turn
//! at once (the exchange it starts is the next turn); its `ledger=` labels
//! only a turn that opens within [`ATTRIBUTION_MS`] attributed to its
//! sender.
//!
//! **The transcript seam.** A later phase drives the same tracker from the
//! transcript (a user row confirms the turn, `tool_use` opens `tool` steps,
//! a limit row confirms a wall at `conf=exact`). Those steps go through the
//! same [`Log::open`]/[`Log::close`]/[`Log::amend`] calls under
//! [`TurnTracker::turn`], with `src=transcript`; nothing here needs to
//! change shape for it.

use crate::field::{Fields, Key, Val};
use crate::log::{Close, Log, Open};
use crate::rec::Stamp;
use crate::vocab::{Actor, Conf, Kind, Node, ShortId, Src};
use crate::words::{BoxWord, Out, StepOut, TurnOut, WallKind};

/// How long a resting word must hold before it closes the turn.
pub const SETTLE_MS: u64 = 1000;
/// How recent an input must be to be the turn's `by=` (§2.5).
pub const ATTRIBUTION_MS: u64 = 5000;

/// One screen verdict word, typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// `busy`
    Busy,
    /// `prompt` with its box (the question dialog is [`BoxWord::QUESTION`]).
    Prompt(BoxWord),
    /// `wall:<kind>`
    Wall(WallKind),
    /// `wall:unresponsive`: the input-stall override.
    Unresponsive,
    /// `survey`
    Survey,
    /// `idle`
    Idle,
    /// `question`: the agent asked in prose.
    Question,
    /// `-`: no identified agent.
    Gone,
    /// `unknown`
    Unknown,
}

impl Verdict {
    /// The verdict a published agent word and detail name
    /// (`AgentPublication::word`/`detail`): `prompt`'s detail is its box,
    /// a wall kind this crate does not know reads `unknown`.
    #[must_use]
    pub fn from_agent_word(word: &str, detail: Option<&str>) -> Verdict {
        match word {
            "busy" => Verdict::Busy,
            "prompt" => Verdict::Prompt(detail.map_or(BoxWord::OTHER, BoxWord::from_detail)),
            "question" => Verdict::Question,
            "idle" => Verdict::Idle,
            "survey" => Verdict::Survey,
            "-" => Verdict::Gone,
            "wall:unresponsive" => Verdict::Unresponsive,
            w => WallKind::from_name(w)
                .filter(|_| w.starts_with("wall:"))
                .map_or(Verdict::Unknown, Verdict::Wall),
        }
    }

    /// The agent word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Verdict::Busy => "busy",
            Verdict::Prompt(_) => "prompt",
            Verdict::Wall(k) => k.wire(),
            Verdict::Unresponsive => "wall:unresponsive",
            Verdict::Survey => "survey",
            Verdict::Idle => "idle",
            Verdict::Question => "question",
            Verdict::Gone => "-",
            Verdict::Unknown => "unknown",
        }
    }
}

/// A step the screen holds open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScreenStep {
    /// An approval box.
    Approval(BoxWord),
    /// The question dialog.
    Question,
    /// A wall.
    Wall(WallKind),
    /// An input stall.
    Stall,
    /// The survey.
    Survey,
}

impl ScreenStep {
    /// Whether the word `v` keeps this step open.
    #[must_use]
    pub fn held_by(self, v: Verdict) -> bool {
        match (self, v) {
            (ScreenStep::Approval(a), Verdict::Prompt(b)) => a == b,
            (ScreenStep::Question, Verdict::Prompt(b)) => b.is_question(),
            (ScreenStep::Wall(a), Verdict::Wall(b)) => a == b,
            (ScreenStep::Stall, Verdict::Unresponsive) | (ScreenStep::Survey, Verdict::Survey) => {
                true
            }
            _ => false,
        }
    }

    /// The step's kind.
    #[must_use]
    pub const fn kind(self) -> Kind {
        match self {
            ScreenStep::Approval(_) => Kind::Approval,
            ScreenStep::Question => Kind::Question,
            ScreenStep::Wall(_) => Kind::Wall,
            ScreenStep::Stall => Kind::Stall,
            ScreenStep::Survey => Kind::Survey,
        }
    }
}

/// The resting word a settle waits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettleCause {
    /// `idle`
    Idle,
    /// `question`
    Question,
    /// `wall:<kind>`
    Wall(WallKind),
    /// `survey`
    Survey,
}

/// The outcome and confidence a settle closes its turn with: THE settle
/// policy.
#[must_use]
pub const fn settle_out(cause: SettleCause, interrupted: bool) -> (TurnOut, Conf) {
    match cause {
        SettleCause::Idle | SettleCause::Survey => (
            if interrupted {
                TurnOut::Interrupted
            } else {
                TurnOut::Done
            },
            Conf::Strong,
        ),
        SettleCause::Question => (TurnOut::Asked, Conf::Heuristic),
        SettleCause::Wall(k) => (TurnOut::Wall(k), Conf::Strong),
    }
}

/// Whether a resting word seen at `since_mono` has held long enough at
/// `now_mono` to close the turn: THE settle rule.
#[must_use]
pub const fn settle_due(since_mono: u64, now_mono: u64) -> bool {
    now_mono >= since_mono.saturating_add(SETTLE_MS)
}

/// Whether an input at `input_mono` is recent enough at `now_mono` to be
/// the opening turn's `by=` (§2.5).
#[must_use]
pub const fn attributes(input_mono: u64, now_mono: u64) -> bool {
    now_mono >= input_mono && now_mono - input_mono <= ATTRIBUTION_MS
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    node: Node,
    what: ScreenStep,
    under_turn: bool,
    detail: Option<String>,
}

/// An armed settle: the resting word, and the host's stamp when it first
/// held (the close is dated there; the decision waits for [`SETTLE_MS`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Settle {
    since: Stamp,
    cause: SettleCause,
}

/// How the tracker's last turn closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastClose {
    /// The turn.
    pub node: Node,
    /// Its outcome.
    pub out: TurnOut,
    /// The word that closed it.
    pub cause: Verdict,
    /// How long the resting word had held, for a settle close.
    pub settled_ms: Option<u64>,
    /// Whether a `ctl turn` closed it while it rested, before its settle.
    pub ctl: bool,
}

/// What the tracker holds, for a conformance projection or a debugger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackerView {
    /// The open turn.
    pub turn: Option<Node>,
    /// The screen step held open, its kind and whether it hangs from the
    /// turn (or from the run).
    pub step: Option<(Node, ScreenStep, bool)>,
    /// The current word.
    pub word: Verdict,
    /// The armed settle and how long it has held at the asked time.
    pub settle: Option<(SettleCause, u64)>,
    /// How the last turn closed.
    pub last_close: Option<LastClose>,
}

/// The turn tracker of one agent run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnTracker {
    run: Node,
    turn: Option<Node>,
    step: Option<Held>,
    word: Verdict,
    settle: Option<Settle>,
    interrupted: bool,
    /// A `ctl turn`'s ledger id, its sender and when it arrived: it labels
    /// the next turn only when that turn opens within [`ATTRIBUTION_MS`]
    /// and is attributed to the same sender.
    pending_ledger: Option<(u64, Actor, u64)>,
    last_input: Option<(Actor, u64)>,
    last_close: Option<LastClose>,
}

impl TurnTracker {
    /// A tracker for the agent run `run` (already open in the log).
    #[must_use]
    pub const fn new(run: Node) -> TurnTracker {
        TurnTracker {
            run,
            turn: None,
            step: None,
            word: Verdict::Unknown,
            settle: None,
            interrupted: false,
            pending_ledger: None,
            last_input: None,
            last_close: None,
        }
    }

    /// The run.
    #[must_use]
    pub const fn run(&self) -> Node {
        self.run
    }

    /// The open turn.
    #[must_use]
    pub const fn turn(&self) -> Option<Node> {
        self.turn
    }

    /// The monotonic time the tracker wants [`TurnTracker::tick`] at, if any.
    #[must_use]
    pub fn wake_at(&self) -> Option<u64> {
        self.settle
            .map(|s| s.since.mono_ms.saturating_add(SETTLE_MS))
    }

    /// The published verdict moved to `v` (with its detail: a wall's reset,
    /// kept on the wall step as `detail=`). Returns when to tick.
    ///
    /// A settle that came due before this call (the host's wake ran late)
    /// is applied first: its resting word did hold, so its turn closes
    /// before `v` is read, and a `busy` then opens the next turn instead of
    /// merging two exchanges.
    pub fn observe(
        &mut self,
        log: &mut Log,
        v: Verdict,
        detail: Option<&str>,
        at: Stamp,
    ) -> Option<u64> {
        self.resync(log);
        if !log.is_open(self.run) {
            return self.wake_at();
        }
        self.settle_if_due(log, at);
        if v == self.word {
            return self.wake_at();
        }
        self.word = v;
        self.settle = None;
        match v {
            Verdict::Unknown => return None,
            Verdict::Gone => {
                // The agent left: the turn closes `out=gone`, its cascade
                // closing a step under it `out=gone`; a step on the run goes
                // the same way. Nothing "passed".
                self.close_turn(log, TurnOut::Gone, Conf::Strong, None, false, at);
                if self.step.is_some() {
                    self.close_step(log, StepOut::Gone, at);
                }
                return self.wake_at();
            }
            _ => {}
        }
        if self.step.as_ref().is_some_and(|s| !s.what.held_by(v)) {
            self.close_step(log, StepOut::Passed, at);
        }
        let wall_detail = || detail.map(str::to_string);
        match v {
            Verdict::Busy => self.ensure_turn(log, Conf::Strong, at),
            Verdict::Prompt(b) => {
                self.ensure_turn(log, Conf::Heuristic, at);
                let what = if b.is_question() {
                    ScreenStep::Question
                } else {
                    ScreenStep::Approval(b)
                };
                self.ensure_step(log, what, None, None, at);
            }
            Verdict::Wall(k) => {
                self.ensure_step(log, ScreenStep::Wall(k), wall_detail(), None, at);
                if self.turn.is_some() {
                    self.arm(SettleCause::Wall(k), at);
                }
            }
            Verdict::Unresponsive => self.ensure_step(log, ScreenStep::Stall, None, None, at),
            Verdict::Survey => {
                if self.turn.is_some() {
                    self.ensure_step(log, ScreenStep::Survey, None, None, at);
                    self.arm(SettleCause::Survey, at);
                }
            }
            Verdict::Idle => {
                if self.turn.is_some() {
                    self.arm(SettleCause::Idle, at);
                }
            }
            Verdict::Question => {
                if self.turn.is_some() {
                    self.arm(SettleCause::Question, at);
                }
            }
            Verdict::Gone | Verdict::Unknown => {}
        }
        self.wake_at()
    }

    /// The host's wake, at any time: close the turn when its resting word
    /// has held for [`SETTLE_MS`] ([`settle_due`]); an early wake changes
    /// nothing. Returns when to tick next.
    pub fn tick(&mut self, log: &mut Log, at: Stamp) -> Option<u64> {
        self.resync(log);
        self.settle_if_due(log, at);
        self.wake_at()
    }

    /// The screen reported the turn interrupted
    /// (`aterm_phase::turn::interrupted`): its idle close is
    /// `out=interrupted`.
    pub fn note_interrupted(&mut self) {
        if self.turn.is_some() {
            self.interrupted = true;
        }
    }

    /// Input from `by` reached the session: a turn opening within
    /// [`ATTRIBUTION_MS`] is attributed to it. Input from anyone else drops
    /// a pending `ctl turn` ledger: the turn it opens is not that exchange.
    pub fn note_input(&mut self, by: Actor, at: Stamp) {
        if self.pending_ledger.is_some_and(|(_, who, _)| who != by) {
            self.pending_ledger = None;
        }
        self.last_input = Some((by, at.mono_ms));
    }

    /// A `ctl turn` exchange (`ledger` is its `TurnLedger` id) from `by`:
    /// the next turn to open within [`ATTRIBUTION_MS`], attributed to `by`,
    /// carries `by=` and `ledger=`. When the agent is resting (a settle is
    /// armed), the exchange starts a new turn, so the resting one closes now
    /// with its settle's outcome, dated when it came to rest. Returns when
    /// to tick.
    pub fn ctl_turn(&mut self, log: &mut Log, by: Actor, ledger: u64, at: Stamp) -> Option<u64> {
        self.resync(log);
        self.settle_if_due(log, at);
        if let Some(s) = self.settle {
            self.settle_now(log, s, at, true);
        }
        self.note_input(by, at);
        self.pending_ledger = Some((ledger, by, at.mono_ms));
        self.wake_at()
    }

    /// What the tracker holds at `now_mono`.
    #[must_use]
    pub fn view(&self, now_mono: u64) -> TrackerView {
        TrackerView {
            turn: self.turn,
            step: self.step.as_ref().map(|h| (h.node, h.what, h.under_turn)),
            word: self.word,
            settle: self
                .settle
                .map(|s| (s.cause, now_mono.saturating_sub(s.since.mono_ms))),
            last_close: self.last_close,
        }
    }

    // ---- internals ---------------------------------------------------

    /// Forget a turn or step the log no longer holds open (the run closed,
    /// a supersede, a cascade).
    fn resync(&mut self, log: &Log) {
        if self.turn.is_some_and(|t| !log.is_open(t)) {
            self.turn = None;
            self.settle = None;
            self.interrupted = false;
        }
        if self.step.as_ref().is_some_and(|s| !log.is_open(s.node)) {
            self.step = None;
        }
    }

    fn arm(&mut self, cause: SettleCause, at: Stamp) {
        self.settle = Some(Settle { since: at, cause });
    }

    /// Apply the armed settle when it is due at `at` ([`settle_due`]).
    fn settle_if_due(&mut self, log: &mut Log, at: Stamp) {
        if let Some(s) = self.settle
            && settle_due(s.since.mono_ms, at.mono_ms)
        {
            self.settle_now(log, s, at, false);
        }
    }

    /// Close the turn as settle `s` says ([`settle_out`]), decided at `now`
    /// and dated at `s.since`, when the resting word first held: `t=` and
    /// `dur_ms` say when the turn ended, not when the wait for it did. A
    /// wall outlives its turn: it continues on the run (`cont=` the turn's
    /// wall step, counted once), so its need holds until the wall lifts.
    /// `ctl`: a `ctl turn` decided it, before the settle came due.
    fn settle_now(&mut self, log: &mut Log, s: Settle, now: Stamp, ctl: bool) {
        self.settle = None;
        let (out, conf) = settle_out(s.cause, self.interrupted);
        let wall = self
            .step
            .as_ref()
            .filter(|h| matches!(h.what, ScreenStep::Wall(_)))
            .map(|h| (h.node, h.detail.clone()));
        let held = now.mono_ms.saturating_sub(s.since.mono_ms);
        self.close_turn(log, out, conf, Some(held), ctl, s.since);
        if let SettleCause::Wall(k) = s.cause {
            let (cont, detail) = wall.map_or((None, None), |(n, d)| (Some(n), d));
            self.ensure_step(log, ScreenStep::Wall(k), detail, cont, s.since);
        }
    }

    fn ensure_turn(&mut self, log: &mut Log, conf: Conf, at: Stamp) {
        if self.turn.is_some() {
            return;
        }
        let by = self
            .last_input
            .filter(|(_, t)| attributes(*t, at.mono_ms))
            .map_or(Actor::Nobody, |(a, _)| a);
        // A ledger labels only the turn its exchange opened: recent, and
        // attributed to its sender.
        let ledger = self
            .pending_ledger
            .filter(|(_, who, t)| *who == by && attributes(*t, at.mono_ms))
            .map(|(l, ..)| l);
        let mut f = Fields::new();
        if let Some(ledger) = ledger {
            let _ = f.set(Key::Ledger, Val::U(ledger));
        }
        let open = Open::new(Kind::Turn, Src::Screen, conf)
            .under(self.run)
            .by(by)
            .fields(f);
        if let Ok(node) = log.open(open, at) {
            self.turn = Some(node);
            self.interrupted = false;
            self.pending_ledger = None;
        }
    }

    fn ensure_step(
        &mut self,
        log: &mut Log,
        what: ScreenStep,
        detail: Option<String>,
        cont: Option<Node>,
        at: Stamp,
    ) {
        if self.step.as_ref().is_some_and(|h| h.what == what) {
            return;
        }
        let under_turn = self.turn.is_some();
        let parent = self.turn.unwrap_or(self.run);
        let mut f = Fields::new();
        match what {
            ScreenStep::Approval(b) => {
                let _ = f.set(Key::Box, Val::Word(b.word()));
            }
            ScreenStep::Wall(k) => {
                let _ = f.set(Key::Wall, Val::Word(k.name()));
                if let Some(t) = detail
                    .as_deref()
                    .and_then(|d| crate::field::BoundedToken::new(Key::Detail, d))
                {
                    let _ = f.set(Key::Detail, Val::Tok(t));
                }
            }
            ScreenStep::Question | ScreenStep::Stall | ScreenStep::Survey => {}
        }
        if let Some(id) = cont.and_then(|n| ShortId::new(&n.to_string())) {
            let _ = f.set(Key::Cont, Val::Id(id));
        }
        let open = Open::new(what.kind(), Src::Screen, Conf::Strong)
            .under(parent)
            .fields(f);
        if let Ok(node) = log.open(open, at) {
            self.step = Some(Held {
                node,
                what,
                under_turn,
                detail,
            });
        }
    }

    fn close_step(&mut self, log: &mut Log, out: StepOut, at: Stamp) {
        if let Some(h) = self.step.take()
            && log.is_open(h.node)
        {
            let _ = log.close(
                h.node,
                Close::new(Out::Step(out), Src::Screen, Conf::Strong),
                at,
            );
        }
    }

    fn close_turn(
        &mut self,
        log: &mut Log,
        out: TurnOut,
        conf: Conf,
        settled_ms: Option<u64>,
        ctl: bool,
        at: Stamp,
    ) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        self.settle = None;
        self.interrupted = false;
        if self.step.as_ref().is_some_and(|h| h.under_turn) {
            // The cascade closes it, innermost first, `out=gone`.
            self.step = None;
        }
        if log
            .close(turn, Close::new(Out::Turn(out), Src::Screen, conf), at)
            .is_ok()
        {
            self.last_close = Some(LastClose {
                node: turn,
                out,
                cause: self.word,
                settled_ms,
                ctl,
            });
        }
    }
}
