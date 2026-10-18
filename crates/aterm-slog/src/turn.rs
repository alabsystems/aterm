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

/// Whether an input at `input` is recent enough at `now` to be the opening
/// turn's `by=` (§2.5), by BOTH clocks: in one process they agree, and a
/// stamp carried across a seamless update keeps its wall time while its
/// monotonic one can only be clamped to the successor's clock start (which
/// would make an old input look recent). A wall clock stepped forward fails
/// closed; one stepped back leaves the monotonic clock to decide.
#[must_use]
pub const fn attributes(input: Stamp, now: Stamp) -> bool {
    now.mono_ms >= input.mono_ms
        && now.mono_ms - input.mono_ms <= ATTRIBUTION_MS
        && now.wall_ms.saturating_sub(input.wall_ms) <= ATTRIBUTION_MS
}

/// `s`, read in another process, on this process's clocks at `now`: its
/// wall time kept, its monotonic time moved by the wall age (clamped to this
/// clock's start; a settle so moved comes due later, never earlier, and an
/// attribution is judged by the wall too, [`attributes`]).
#[must_use]
pub const fn rebase(wall_ms: u64, now: Stamp) -> Stamp {
    Stamp::new(
        wall_ms,
        now.mono_ms
            .saturating_sub(now.wall_ms.saturating_sub(wall_ms)),
    )
}

/// A screen step's carry word.
const fn screen_step_word(w: ScreenStep) -> &'static str {
    match w {
        ScreenStep::Approval(_) => "approval",
        ScreenStep::Question => "question",
        ScreenStep::Wall(_) => "wall",
        ScreenStep::Stall => "stall",
        ScreenStep::Survey => "survey",
    }
}

impl Settle {
    /// Whether it is due at `now_mono`: [`settle_due`] of its `since` in one
    /// process, the wall clock's deadline for a carried one.
    const fn is_due(self, now_mono: u64) -> bool {
        now_mono >= self.due_mono
    }

    /// How long its resting word has held at `now_mono`.
    const fn held(self, now_mono: u64) -> u64 {
        now_mono
            .saturating_sub(self.due_mono.saturating_sub(SETTLE_MS))
            .saturating_add(self.prior_ms)
    }
}

/// A settle cause's carry word: its resting word.
const fn settle_word(c: SettleCause) -> &'static str {
    match c {
        SettleCause::Idle => "idle",
        SettleCause::Question => "question",
        SettleCause::Wall(k) => k.wire(),
        SettleCause::Survey => "survey",
    }
}

fn settle_from_word(w: &str) -> Option<SettleCause> {
    match w {
        "idle" => Some(SettleCause::Idle),
        "question" => Some(SettleCause::Question),
        "survey" => Some(SettleCause::Survey),
        w if w.starts_with("wall:") => WallKind::from_name(w).map(SettleCause::Wall),
        _ => None,
    }
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
    /// Elapsed time not represented by the deadline's local origin: before
    /// monotonic zero, or older than a deadline already due at adoption.
    /// Zero for a local settle or a carried future origin.
    prior_ms: u64,
    /// When it comes due, on this process's monotonic clock: `since` plus
    /// [`SETTLE_MS`] ([`settle_due`]), or, for a settle carried across an
    /// update, when the wall clock says it does (its `since` is another
    /// process's, its monotonic part clamped).
    due_mono: u64,
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
    /// A `ctl turn`'s ledger id, its sender, when it arrived and what it
    /// said it acted on (`cause=`): it labels the next turn only when that
    /// turn opens within [`ATTRIBUTION_MS`] and is attributed to the same
    /// sender.
    pending_ledger: Option<(u64, Actor, Stamp, Option<ShortId>)>,
    /// The latest input: its sender, when it came, and the mail or record it
    /// said it acted on (`cause=`, phase 3A), which the turn it opens names.
    last_input: Option<(Actor, Stamp, Option<ShortId>)>,
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
        self.settle.map(|s| s.due_mono)
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
        self.note_input_caused(by, None, at);
    }

    /// [`Self::note_input`] of input that said what it acted on (`cause=@<off>`
    /// for mail, `cause=<seq>` for a record, validated by the host): a turn it
    /// opens carries `cause=` and `cause_k=`. Input from anyone after it
    /// takes the turn without the cause.
    pub fn note_input_caused(&mut self, by: Actor, cause: Option<ShortId>, at: Stamp) {
        if self.pending_ledger.is_some_and(|(_, who, ..)| who != by) {
            self.pending_ledger = None;
        }
        self.last_input = Some((by, at, cause));
    }

    /// A `ctl turn` exchange (`ledger` is its `TurnLedger` id) from `by`:
    /// the next turn to open within [`ATTRIBUTION_MS`], attributed to `by`,
    /// carries `by=` and `ledger=`. When the agent is resting (a settle is
    /// armed), the exchange starts a new turn, so the resting one closes now
    /// with its settle's outcome, dated when it came to rest. Returns when
    /// to tick.
    pub fn ctl_turn(&mut self, log: &mut Log, by: Actor, ledger: u64, at: Stamp) -> Option<u64> {
        self.ctl_turn_caused(log, by, ledger, None, at)
    }

    /// [`Self::ctl_turn`] of an exchange that said what it acted on
    /// (`turn cause=`): the turn it opens carries `cause=` too.
    pub fn ctl_turn_caused(
        &mut self,
        log: &mut Log,
        by: Actor,
        ledger: u64,
        cause: Option<ShortId>,
        at: Stamp,
    ) -> Option<u64> {
        self.resync(log);
        self.settle_if_due(log, at);
        if let Some(s) = self.settle {
            self.settle_now(log, s, at, true);
        }
        self.note_input_caused(by, cause, at);
        self.pending_ledger = Some((ledger, by, at, cause));
        self.wake_at()
    }

    /// What the tracker holds at `now_mono`.
    #[must_use]
    pub fn view(&self, now_mono: u64) -> TrackerView {
        TrackerView {
            turn: self.turn,
            step: self.step.as_ref().map(|h| (h.node, h.what, h.under_turn)),
            word: self.word,
            settle: self.settle.map(|s| (s.cause, s.held(now_mono))),
            last_close: self.last_close,
        }
    }

    // ---- the seamless carry ------------------------------------------

    /// The tracker as one carry line (design §5.2, phase 2A): its run, turn
    /// and screen step, the current word, an armed settle (its cause and the
    /// wall time its word first held), whether the turn was interrupted, and
    /// a pending `ctl turn` ledger and the last input, each with its wall
    /// time. An input or ledger older than [`ATTRIBUTION_MS`] at `now` is
    /// left out: it can attribute nothing any more. The last close (a
    /// view's) does not cross.
    #[must_use]
    pub fn carry_line(&self, now: Stamp) -> String {
        let mut s = format!(
            "tracker run={} turn={} word={} interrupted={}",
            self.run,
            self.turn
                .map_or_else(|| "none".to_string(), |t| t.to_string()),
            self.word.word(),
            u8::from(self.interrupted)
        );
        if let Verdict::Prompt(b) = self.word {
            s.push_str(" box=");
            s.push_str(b.word());
        }
        if let Some(h) = &self.step {
            s.push_str(&format!(
                " step={} step_under={} step_what={}",
                h.node,
                u8::from(h.under_turn),
                screen_step_word(h.what)
            ));
            match h.what {
                ScreenStep::Approval(b) => {
                    s.push_str(" step_box=");
                    s.push_str(b.word());
                }
                ScreenStep::Wall(k) => {
                    s.push_str(" step_wall=");
                    s.push_str(k.name());
                }
                _ => {}
            }
            // As the step's own `detail=` field holds it (a token of at most
            // its key's bytes), so the line stays within a carry line's bound
            // whatever the screen said, and a step it continues writes the
            // same field.
            if let Some(d) = h
                .detail
                .as_deref()
                .and_then(|d| crate::field::BoundedToken::new(Key::Detail, d))
            {
                s.push_str(" step_detail=");
                aterm_codec::pct::encode_into(&mut s, d.as_str());
            }
        }
        if let Some(st) = self.settle {
            s.push_str(&format!(
                " settle={} settle_wall={}",
                settle_word(st.cause),
                st.since.wall_ms
            ));
        }
        if let Some((id, by, at, cause)) = self
            .pending_ledger
            .filter(|(_, _, t, _)| attributes(*t, now))
        {
            s.push_str(&format!(
                " ledger={id} ledger_by={} ledger_wall={}",
                by.word(),
                at.wall_ms
            ));
            if let Some(c) = cause {
                s.push_str(&format!(" ledger_cause={c}"));
            }
        }
        if let Some((by, at, cause)) = self.last_input.filter(|(_, t, _)| attributes(*t, now)) {
            s.push_str(&format!(
                " input_by={} input_wall={}",
                by.word(),
                at.wall_ms
            ));
            if let Some(c) = cause {
                s.push_str(&format!(" input_cause={c}"));
            }
        }
        s
    }

    /// The tracker a [`TurnTracker::carry_line`] wrote, on this process's
    /// clocks at `now` ([`rebase`]): `None` for a line this build does not
    /// read. An input or ledger older than [`ATTRIBUTION_MS`] by the wall
    /// clock at `now` is dropped (fail closed: it attributes nothing).
    #[must_use]
    pub fn from_carry_line(line: &str, now: Stamp) -> Option<TurnTracker> {
        let mut toks = line.split(' ').filter(|t| !t.is_empty());
        if toks.next() != Some("tracker") {
            return None;
        }
        let kv: Vec<(&str, &str)> = toks.filter_map(|t| t.split_once('=')).collect();
        let get = |k: &str| kv.iter().find(|(key, _)| *key == k).map(|(_, v)| *v);
        let wall = |k: &str| get(k).and_then(|v| v.parse::<u64>().ok());
        let run = Node::parse(get("run")?).filter(|n| n.lvl == crate::vocab::Lvl::Run)?;
        let turn = match get("turn")? {
            "none" => None,
            t => Some(Node::parse(t).filter(|n| n.lvl == crate::vocab::Lvl::Turn)?),
        };
        let word = match get("word")? {
            "prompt" => Verdict::Prompt(BoxWord::from_wire(get("box")?)),
            w => Verdict::from_agent_word(w, None),
        };
        let interrupted = match get("interrupted")? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        let step = match get("step") {
            None => None,
            Some(n) => {
                let node = Node::parse(n).filter(|n| n.lvl == crate::vocab::Lvl::Step)?;
                let what = match get("step_what")? {
                    "approval" => ScreenStep::Approval(BoxWord::from_wire(get("step_box")?)),
                    "question" => ScreenStep::Question,
                    "wall" => ScreenStep::Wall(WallKind::from_name(get("step_wall")?)?),
                    "stall" => ScreenStep::Stall,
                    "survey" => ScreenStep::Survey,
                    _ => return None,
                };
                let under_turn = match get("step_under")? {
                    "0" => false,
                    "1" => true,
                    _ => return None,
                };
                let detail = get("step_detail").map(aterm_codec::pct::decode);
                Some(Held {
                    node,
                    what,
                    under_turn,
                    detail,
                })
            }
        };
        let settle = match get("settle") {
            None => None,
            Some(c) => {
                let w = wall("settle_wall")?;
                let due_mono = now
                    .mono_ms
                    .saturating_add(w.saturating_add(SETTLE_MS).saturating_sub(now.wall_ms));
                let represented = now
                    .mono_ms
                    .saturating_sub(due_mono.saturating_sub(SETTLE_MS));
                Some(Settle {
                    since: rebase(w, now),
                    cause: settle_from_word(c)?,
                    prior_ms: now.wall_ms.saturating_sub(w).saturating_sub(represented),
                    due_mono,
                })
            }
        };
        let fresh = |w: u64| now.wall_ms.saturating_sub(w) <= ATTRIBUTION_MS;
        // A cause is additive (phase 3A): absent on a 2A line; one that does
        // not read as an id refuses the line, as any malformed value does.
        let cause = |k: &str| -> Option<Option<ShortId>> {
            match get(k) {
                None => Some(None),
                Some(c) => ShortId::new(c).map(Some),
            }
        };
        let pending_ledger = match get("ledger") {
            None => None,
            Some(id) => {
                let id: u64 = id.parse().ok()?;
                let by = Actor::from_word(get("ledger_by")?);
                let w = wall("ledger_wall")?;
                let c = cause("ledger_cause")?;
                fresh(w).then(|| (id, by, rebase(w, now), c))
            }
        };
        let last_input = match get("input_by") {
            None => None,
            Some(by) => {
                let w = wall("input_wall")?;
                let c = cause("input_cause")?;
                fresh(w).then(|| (Actor::from_word(by), rebase(w, now), c))
            }
        };
        Some(TurnTracker {
            run,
            turn,
            step,
            word,
            settle,
            interrupted,
            pending_ledger,
            last_input,
            last_close: None,
        })
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
        // THE settle rule: due once `settle_due(since, now)` holds.
        let due_mono = at.mono_ms.saturating_add(SETTLE_MS);
        debug_assert!(settle_due(at.mono_ms, due_mono) && !settle_due(at.mono_ms, due_mono - 1));
        self.settle = Some(Settle {
            since: at,
            cause,
            prior_ms: 0,
            due_mono,
        });
    }

    /// Apply the armed settle when it is due at `at` ([`settle_due`]).
    fn settle_if_due(&mut self, log: &mut Log, at: Stamp) {
        if let Some(s) = self.settle
            && s.is_due(at.mono_ms)
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
        let held = s.held(now.mono_ms);
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
        let attributed = self.last_input.filter(|(_, t, _)| attributes(*t, at));
        let by = attributed.map_or(Actor::Nobody, |(a, ..)| a);
        // A ledger labels only the turn its exchange opened: recent, and
        // attributed to its sender.
        let ledger = self
            .pending_ledger
            .filter(|(_, who, t, _)| *who == by && attributes(*t, at))
            .map(|(l, ..)| l);
        let mut f = Fields::new();
        if let Some(ledger) = ledger {
            let _ = f.set(Key::Ledger, Val::U(ledger));
        }
        // The attributed input's `cause=` (phase 3A): the turn names the mail
        // or record its opening input said it acted on.
        if let Some(c) = attributed.and_then(|(.., c)| c)
            && let Ok(caused) = f.clone().cause(c.as_str())
        {
            f = caused;
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
