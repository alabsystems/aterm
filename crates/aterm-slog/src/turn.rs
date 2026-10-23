// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The turn tracker (design §2.4): at most one open turn per agent run, driven by the screen verdict word.

use crate::field::{BoundedText, BoundedToken, Fields, Key, Val};
use crate::log::{Amend, Close, Log, Mark, Open};
use crate::rec::Stamp;
use crate::vocab::{Actor, CLS_WORDS, Conf, Kind, Lvl, Node, Op, ShortId, Src, lookup};
use crate::words::{BoxWord, Out, StepOut, TurnOut, WallKind};

/// How long a resting word must hold before it closes the turn.
pub const SETTLE_MS: u64 = 1000;
/// How recent an input must be to be the turn's `by=` (§2.5).
pub const ATTRIBUTION_MS: u64 = 5000;
/// How recent an input's acceptance must be, at an approval or question step's close, to be the input that dismissed it.
pub const STEP_ACT_MS: u64 = 500;
/// How soon after an input's admission, with no approval or question step open then, the first such step must open for the input to bind it.
pub const STEP_INPUT_LEAD_MS: u64 = 500;

/// What an input says it means for the step it dismisses (`act=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActWord {
    /// `approve`
    Approve,
    /// `deny`
    Deny,
    /// `answer`
    Answer,
}

impl ActWord {
    /// Every act word, in wire order.
    pub const ALL: [ActWord; 3] = [ActWord::Approve, ActWord::Deny, ActWord::Answer];

    /// The wire word (an input mark's `act=`, one of [`crate::vocab::ACT_WORDS`]).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ActWord::Approve => "approve",
            ActWord::Deny => "deny",
            ActWord::Answer => "answer",
        }
    }

    /// The act a wire word names, or `None`.
    #[must_use]
    pub fn from_word(w: &str) -> Option<ActWord> {
        ActWord::ALL.into_iter().find(|a| a.word() == w)
    }

    /// The outcome it gives a step of `what` that it dismissed, `None` for a step it does not answer (an approval's act on a question, or the reverse).
    #[must_use]
    pub const fn out_for(self, what: ScreenStep) -> Option<StepOut> {
        match (self, what) {
            (ActWord::Approve, ScreenStep::Approval(_)) => Some(StepOut::Approved),
            (ActWord::Deny, ScreenStep::Approval(_)) => Some(StepOut::Declined),
            (ActWord::Answer, ScreenStep::Question) => Some(StepOut::Answered),
            _ => None,
        }
    }
}

/// Whether an input accepted at `accepted` is recent enough at a step's close at `close` to be the input that dismissed it ([`STEP_ACT_MS`]).
#[must_use]
pub const fn dismisses(accepted: Stamp, close: Stamp) -> bool {
    close.mono_ms >= accepted.mono_ms && close.mono_ms - accepted.mono_ms <= STEP_ACT_MS
}

/// Whether a step opened at `opened` is within the lead of an input admitted at `admitted` ([`STEP_INPUT_LEAD_MS`]): THE lead rule.
#[must_use]
pub const fn within_lead(admitted: Stamp, opened: Stamp) -> bool {
    opened.mono_ms >= admitted.mono_ms && opened.mono_ms - admitted.mono_ms <= STEP_INPUT_LEAD_MS
}

/// What the host admitted an input under ([`TurnTracker::admit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Admission {
    run: Node,
    step: Option<Node>,
    at: Stamp,
    opens: u64,
    closes: u64,
}

impl Admission {
    /// The approval or question step open at the admission, if any.
    #[must_use]
    pub const fn step(&self) -> Option<Node> {
        self.step
    }

    /// When it was admitted.
    #[must_use]
    pub const fn at(&self) -> Stamp {
        self.at
    }
}

/// What an accepted input came to ([`TurnTracker::accept`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    /// Bound to this open step: its close may be the input's.
    Step(Node),
    /// Admitted with no step open: it binds the first approval or question step to open within the lead, if one does.
    Lead,
    /// Its step closed before it was accepted: it dismissed nothing the tracker can say (a late acceptance; the input mark says `re=`).
    Late(Node),
    /// Nothing: no step, and the lead is past or broken.
    Unbound,
}

/// What the tracker holds of the inputs bound to its steps, for a conformance projection or a debugger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttributionView {
    /// The latest input bound to the open approval or question step: the step, its author, its act and when it was accepted.
    pub bound: Option<(Node, Actor, Option<ActWord>, Stamp)>,
    /// An accepted input waiting on the lead.
    pub lead: Option<(Actor, Option<ActWord>, Stamp, Stamp, u64, u64)>,
    /// Approval and question steps opened so far.
    pub opens: u64,
    /// Approval and question steps closed (or forgotten) so far.
    pub closes: u64,
}

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
    /// The verdict a published agent word and detail name (`AgentPublication::word`/`detail`).
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

/// The outcome and confidence a settle closes its turn with: THE settle policy.
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

/// Whether a resting word seen at `since_mono` has held long enough at `now_mono` to close the turn: THE settle rule.
#[must_use]
pub const fn settle_due(since_mono: u64, now_mono: u64) -> bool {
    now_mono >= since_mono.saturating_add(SETTLE_MS)
}

/// Whether an input at `input` is recent enough at `now` to be the opening turn's `by=` (§2.5), by BOTH clocks.
#[must_use]
pub const fn attributes(input: Stamp, now: Stamp) -> bool {
    now.mono_ms >= input.mono_ms
        && now.mono_ms - input.mono_ms <= ATTRIBUTION_MS
        && now.wall_ms.saturating_sub(input.wall_ms) <= ATTRIBUTION_MS
}

/// `s`, read in another process, on this process's clocks at `now`.
#[must_use]
pub const fn rebase(wall_ms: u64, now: Stamp) -> Stamp {
    Stamp::new(
        wall_ms,
        now.mono_ms
            .saturating_sub(now.wall_ms.saturating_sub(wall_ms)),
    )
}

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
    const fn is_due(self, now_mono: u64) -> bool {
        now_mono >= self.due_mono
    }

    const fn held(self, now_mono: u64) -> u64 {
        now_mono
            .saturating_sub(self.due_mono.saturating_sub(SETTLE_MS))
            .saturating_add(self.prior_ms)
    }
}

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
    /// When it opened in this process (`None`: carried, never a lead's).
    opened: Option<Stamp>,
}

const fn binds(what: ScreenStep) -> bool {
    matches!(what, ScreenStep::Approval(_) | ScreenStep::Question)
}

/// The latest input bound to the open approval or question step (T1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bound {
    node: Node,
    by: Actor,
    act: Option<ActWord>,
    accepted: Stamp,
}

/// An accepted input admitted with no step open, waiting for the lead's
/// first step (T1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Lead {
    by: Actor,
    act: Option<ActWord>,
    admitted: Stamp,
    accepted: Stamp,
    opens: u64,
    closes: u64,
}

/// An armed settle: the resting word, and the host's stamp when it first
/// held (the close is dated there; the decision waits for [`SETTLE_MS`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Settle {
    since: Stamp,
    cause: SettleCause,
    /// Elapsed time not represented by the deadline's local origin: before
    /// monotonic zero, or older than a deadline already due at adoption.
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
    /// Whether the transcript closed it (its vendor turn's end, or a newer vendor turn superseding it): [`TurnTracker::transcript`].
    pub tx: bool,
}

/// What the tracker holds, for a conformance projection or a debugger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackerView {
    /// The open turn.
    pub turn: Option<Node>,
    /// The screen step held open, its kind and whether it hangs from the turn (or from the run).
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
    /// The transcript join (phase 3B).
    tx: TxJoin,
    /// The latest input bound to the open approval or question step (T1).
    bound: Option<Bound>,
    /// An accepted input waiting on the lead (T1).
    lead: Option<Lead>,
    /// Approval and question steps opened, and closed or forgotten, in this
    /// process (T1: a lead binds only the first to open, with none closing
    /// between).
    opens: u64,
    closes: u64,
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
            tx: TxJoin::new(),
            bound: None,
            lead: None,
            opens: 0,
            closes: 0,
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

    /// The published verdict moved to `v` (with its detail: a wall's reset, kept on the wall step as `detail=`).
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
        self.tx.refuted = None;
        match v {
            Verdict::Unknown => return None,
            Verdict::Gone => {
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

    /// The host's wake, at any time: close the turn when its resting word has held for [`SETTLE_MS`] ([`settle_due`]).
    pub fn tick(&mut self, log: &mut Log, at: Stamp) -> Option<u64> {
        self.resync(log);
        self.settle_if_due(log, at);
        self.wake_at()
    }

    /// The screen reported the turn interrupted (`aterm_phase::turn::interrupted`): its idle close is `out=interrupted`.
    pub const fn note_interrupted(&mut self) {
        if self.turn.is_some() {
            self.interrupted = true;
        }
    }

    /// Input from `by` reached the session: a turn opening within [`ATTRIBUTION_MS`] is attributed to it.
    pub fn note_input(&mut self, by: Actor, at: Stamp) {
        self.note_input_caused(by, None, at);
    }

    /// [`Self::note_input`] of input that said what it acted on (`cause=@<off>` for mail, `cause=<seq>` for a record, validated by the host).
    pub fn note_input_caused(&mut self, by: Actor, cause: Option<ShortId>, at: Stamp) {
        if self.pending_ledger.is_some_and(|(_, who, ..)| who != by) {
            self.pending_ledger = None;
        }
        self.last_input = Some((by, at, cause));
    }

    /// A `ctl turn` exchange (`ledger` is its `TurnLedger` id) from `by`.
    pub fn ctl_turn(&mut self, log: &mut Log, by: Actor, ledger: u64, at: Stamp) -> Option<u64> {
        self.ctl_turn_caused(log, by, ledger, None, at)
    }

    /// [`Self::ctl_turn`] of an exchange that said what it acted on (`turn cause=`): the turn it opens carries `cause=` too.
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

    /// THE ADMISSION of an input the host is about to write at `at`.
    #[must_use]
    pub fn admit(&self, at: Stamp) -> Admission {
        Admission {
            run: self.run,
            step: self.step.as_ref().filter(|h| binds(h.what)).map(|h| h.node),
            at,
            opens: self.opens,
            closes: self.closes,
        }
    }

    /// THE ACCEPTANCE of an input admitted as `adm` (its bytes written), from `by` with its `act=`, at `at`.
    pub fn accept(
        &mut self,
        log: &Log,
        by: Actor,
        act: Option<ActWord>,
        adm: Admission,
        at: Stamp,
    ) -> Binding {
        self.resync(log);
        if adm.run != self.run {
            return Binding::Unbound;
        }
        if let Some(node) = adm.step {
            if self.step.as_ref().is_some_and(|h| h.node == node) {
                self.bind_input(node, by, act, at);
                return Binding::Step(node);
            }
            return Binding::Late(node);
        }
        if self.closes != adm.closes {
            return Binding::Unbound;
        }
        match self.opens.checked_sub(adm.opens) {
            Some(0) if at.mono_ms.saturating_sub(adm.at.mono_ms) <= STEP_INPUT_LEAD_MS => {
                self.lead = Some(Lead {
                    by,
                    act,
                    admitted: adm.at,
                    accepted: at,
                    opens: adm.opens,
                    closes: adm.closes,
                });
                Binding::Lead
            }
            Some(1) => match self.step.as_ref() {
                Some(h) if binds(h.what) && h.opened.is_some_and(|o| within_lead(adm.at, o)) => {
                    let node = h.node;
                    self.bind_input(node, by, act, at);
                    Binding::Step(node)
                }
                _ => Binding::Unbound,
            },
            _ => Binding::Unbound,
        }
    }

    /// A PERSON's input at `at` (delivery).
    pub fn person(&mut self, log: &Log, at: Stamp) -> Binding {
        self.resync(log);
        match self.step.as_ref() {
            Some(h) if binds(h.what) => {
                let node = h.node;
                self.bind_input(node, Actor::Human, None, at);
                Binding::Step(node)
            }
            _ => {
                self.lead = Some(Lead {
                    by: Actor::Human,
                    act: None,
                    admitted: at,
                    accepted: at,
                    opens: self.opens,
                    closes: self.closes,
                });
                Binding::Lead
            }
        }
    }

    /// What the tracker holds of its bound inputs.
    #[must_use]
    pub fn attribution(&self) -> AttributionView {
        AttributionView {
            bound: self.bound.map(|b| (b.node, b.by, b.act, b.accepted)),
            lead: self
                .lead
                .map(|l| (l.by, l.act, l.admitted, l.accepted, l.opens, l.closes)),
            opens: self.opens,
            closes: self.closes,
        }
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

    /// The tracker as one carry line (design §5.2, phase 2A).
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
        self.tx.carry_into(&mut s);
        s
    }

    /// The tracker a [`TurnTracker::carry_line`] wrote, on this process's clocks at `now` ([`rebase`]).
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
                    opened: None,
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
            tx: TxJoin::from_carry(&get, now),
            bound: None,
            lead: None,
            opens: 0,
            closes: 0,
        })
    }

    fn resync(&mut self, log: &Log) {
        if self.turn.is_some_and(|t| !log.is_open(t)) {
            self.turn = None;
            self.settle = None;
            self.interrupted = false;
        }
        if self.step.as_ref().is_some_and(|s| !log.is_open(s.node)) {
            self.forget_step();
        }
    }

    fn forget_step(&mut self) {
        if let Some(h) = self.step.take()
            && binds(h.what)
        {
            self.closes += 1;
            self.bound = None;
            self.lead = None;
        }
    }

    const fn bind_input(&mut self, node: Node, by: Actor, act: Option<ActWord>, at: Stamp) {
        self.bound = Some(Bound {
            node,
            by,
            act,
            accepted: at,
        });
        self.lead = None;
    }

    fn attribution_of(&self, h: &Held, at: Stamp) -> (Actor, StepOut, Option<ActWord>) {
        match self.bound.filter(|b| b.node == h.node) {
            Some(b) if dismisses(b.accepted, at) => (
                b.by,
                b.act
                    .and_then(|a| a.out_for(h.what))
                    .filter(|_| b.by.is_proven())
                    .unwrap_or(StepOut::Passed),
                None,
            ),
            Some(b) => (Actor::Nobody, StepOut::Passed, b.act),
            None => (Actor::Nobody, StepOut::Passed, None),
        }
    }

    fn arm(&mut self, cause: SettleCause, at: Stamp) {
        let due_mono = at.mono_ms.saturating_add(SETTLE_MS);
        debug_assert!(settle_due(at.mono_ms, due_mono) && !settle_due(at.mono_ms, due_mono - 1));
        self.settle = Some(Settle {
            since: at,
            cause,
            prior_ms: 0,
            due_mono,
        });
    }

    fn settle_if_due(&mut self, log: &mut Log, at: Stamp) {
        if let Some(s) = self.settle
            && s.is_due(at.mono_ms)
        {
            self.settle_now(log, s, at, false);
        }
    }

    fn settle_now(&mut self, log: &mut Log, s: Settle, now: Stamp, ctl: bool) {
        self.settle = None;
        let (out, conf) = settle_out(s.cause, self.interrupted);
        let wall = self
            .step
            .as_ref()
            .filter(|h| matches!(h.what, ScreenStep::Wall(_)))
            .map(|h| (h.node, h.detail.clone()));
        let held = s.held(now.mono_ms);
        let at = self.turn.map_or(s.since, |t| tx_close_at(log, t, s.since));
        self.close_turn(log, out, conf, Some(held), ctl, at);
        if let SettleCause::Wall(k) = s.cause {
            let (cont, detail) = wall.map_or((None, None), |(n, d)| (Some(n), d));
            self.ensure_step(log, ScreenStep::Wall(k), detail, cont, at);
        }
    }

    fn ensure_turn(&mut self, log: &mut Log, conf: Conf, at: Stamp) {
        if self.turn.is_some() {
            return;
        }
        let Some(node) = self.open_turn(log, Src::Screen, conf, Fields::new(), at) else {
            return;
        };
        if let Some(v) = self
            .tx
            .cur
            .filter(|v| v.end_ms.is_none() && v.node.is_none())
            && v.at_ms > 0
            && v.at_ms.abs_diff(at.wall_ms) <= ATTRIBUTION_MS
            && let Some(conf) = self.tx.state.and_then(TxState::conf)
        {
            Self::amend_vref(log, node, v.vref, conf, at);
            self.tx.cur = Some(VTurn {
                node: Some(node),
                ..v
            });
            return;
        }
        if let Some(v) = self.tx.cur.filter(|v| v.end_ms.is_none())
            && let Some(prev) = v.node.filter(|n| !log.is_open(*n))
            && let Some(conf) = self.tx.state.and_then(TxState::conf)
            && let Some(id) = ShortId::new(&prev.to_string())
        {
            let mut f = Fields::new();
            let _ = f.set(Key::Cont, Val::Id(id));
            let amend = Amend::new(Kind::Turn, Src::Transcript, conf, f).by(Actor::Vendor);
            if log.amend(node, amend, at).is_ok() {
                self.tx.cur = Some(VTurn {
                    node: Some(node),
                    ..v
                });
            }
        }
    }

    fn open_turn(
        &mut self,
        log: &mut Log,
        src: Src,
        conf: Conf,
        extra: Fields,
        at: Stamp,
    ) -> Option<Node> {
        let attributed = self.last_input.filter(|(_, t, _)| attributes(*t, at));
        let by = attributed.map_or(Actor::Nobody, |(a, ..)| a);
        let ledger = self
            .pending_ledger
            .filter(|(_, who, t, _)| *who == by && attributes(*t, at))
            .map(|(l, ..)| l);
        let mut f = extra;
        if let Some(ledger) = ledger {
            let _ = f.set(Key::Ledger, Val::U(ledger));
        }
        if let Some(c) = attributed.and_then(|(.., c)| c)
            && let Ok(caused) = f.clone().cause(c.as_str())
        {
            f = caused;
        }
        let open = Open::new(Kind::Turn, src, conf)
            .under(self.run)
            .by(by)
            .fields(f);
        let node = log.open(open, at).ok()?;
        self.turn = Some(node);
        self.interrupted = false;
        self.pending_ledger = None;
        Some(node)
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
                opened: Some(at),
            });
            if binds(what) {
                // An accepted input waiting on the lead binds the first
                // approval or question step to open within it, with none
                // closing between (T1).
                if let Some(l) = self.lead.take()
                    && l.opens == self.opens
                    && l.closes == self.closes
                    && within_lead(l.admitted, at)
                {
                    self.bind_input(node, l.by, l.act, l.accepted);
                }
                self.opens += 1;
            }
        }
    }

    fn close_step(&mut self, log: &mut Log, out: StepOut, at: Stamp) {
        if let Some(h) = self.step.as_ref()
            && log.is_open(h.node)
        {
            let (by, out, orphan) = if out == StepOut::Passed && binds(h.what) {
                self.attribution_of(h, at)
            } else {
                (Actor::Nobody, out, None)
            };
            let f = orphan
                .and_then(|a| Fields::new().word(Key::Act, a.word()).ok())
                .unwrap_or_default();
            let _ = log.close(
                h.node,
                Close::new(Out::Step(out), Src::Screen, Conf::Strong)
                    .by(by)
                    .fields(f),
                at,
            );
        }
        self.forget_step();
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
        let close = Close::new(Out::Turn(out), Src::Screen, conf);
        self.close_turn_with(log, close, out, (settled_ms, ctl, false), at);
    }

    fn close_turn_with(
        &mut self,
        log: &mut Log,
        close: Close,
        out: TurnOut,
        how: (Option<u64>, bool, bool),
        at: Stamp,
    ) {
        let (settled_ms, ctl, tx) = how;
        let Some(turn) = self.turn.take() else {
            return;
        };
        self.settle = None;
        self.interrupted = false;
        if self.step.as_ref().is_some_and(|h| h.under_turn) {
            self.forget_step();
        }
        if log.close(turn, close, at).is_ok() {
            self.last_close = Some(LastClose {
                node: turn,
                out,
                cause: self.word,
                settled_ms,
                ctl,
                tx,
            });
        }
    }
}

/// The most transcript steps one turn holds open at once: a ninth parallel tool call is counted `dropped=`, never opened.
pub const TX_TOOLS_MAX: usize = 8;

/// What a run's transcript source is (`source field=transcript state=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TxState {
    /// A measured build: exact.
    On,
    /// An unmeasured build: strong, additive only.
    Partial,
    /// Not read.
    Off,
}

impl TxState {
    /// The `state=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            TxState::On => "on",
            TxState::Partial => "partial",
            TxState::Off => "off",
        }
    }

    /// The state a `state=` word names.
    #[must_use]
    pub fn from_word(w: &str) -> Option<TxState> {
        match w {
            "on" => Some(TxState::On),
            "partial" => Some(TxState::Partial),
            "off" => Some(TxState::Off),
            _ => None,
        }
    }

    /// The confidence its records carry, `None` when it writes none.
    #[must_use]
    pub const fn conf(self) -> Option<Conf> {
        match self {
            TxState::On => Some(Conf::Exact),
            TxState::Partial => Some(Conf::Strong),
            TxState::Off => None,
        }
    }
}

/// Where a transcript row was read: the file, as its path's fingerprint and, where the platform has one, its identity (`dev`, `ino`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TxPos {
    /// The path's fingerprint.
    pub fp: u64,
    /// The file's identity, when known.
    pub id: Option<(u64, u64)>,
    /// The end offset of the row's line (for a [`TxEvent::Bind`], the file's length).
    pub off: u64,
    /// The row's time (wall ms).
    pub wall_ms: u64,
}

/// The read position the join has applied through: a row at or below it is never applied again (watermark).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TxMark {
    /// The path's fingerprint.
    pub fp: u64,
    /// The file's identity, when known.
    pub id: Option<(u64, u64)>,
    /// The offset applied through.
    pub off: u64,
}

impl TxMark {
    const fn of(pos: TxPos) -> TxMark {
        TxMark {
            fp: pos.fp,
            id: pos.id,
            off: pos.off,
        }
    }

    fn admits(self, pos: TxPos) -> bool {
        self.fp == pos.fp && self.id == pos.id && pos.off > self.off
    }
}

/// The wall judge's answer for the screen's open wall step (§2.6: the harness's own rule, `steps::judge_wall`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WallJudge {
    /// The transcript's main chain ends on the vendor's notice of the same class: `amend confirmed=1` at exact.
    Confirm,
    /// It ends elsewhere, newer than the wall: `close out=retracted`.
    Refute,
}

/// One transcript fact, typed: an id, a tool's name (`tool=`, host tier, at most 64 bytes), a class word and numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxEvent<'a> {
    /// The glue bound a transcript file of `len` bytes (its first read, a `/clear` or `/resume` that moved the path, a cursor restart) for a Claude Code process that started at `floor_ms` (wall).
    Bind {
        /// The file's length (bytes).
        len: u64,
        /// The process's start (wall ms; `footer::floor_of` × 1000).
        floor_ms: u64,
    },
    /// A user row that starts a vendor turn (S3's classifier: never an interrupt, a bash or task-notification row, a meta or compaction row), with its `promptId` (or `uuid`).
    Prompt {
        /// The vendor turn's id.
        vref: ShortId,
    },
    /// `[Request interrupted by user…`.
    Interrupt,
    /// A `tool_use` part of an assistant row.
    ToolUse {
        /// The call's id.
        id: ShortId,
        /// The vendor tool's name (`Bash`, `mcp__<server>__<tool>`).
        tool: &'a str,
        /// Its class (one of `CLS_WORDS`).
        cls: &'a str,
        /// A subagent call (`Task`).
        subagent: bool,
    },
    /// A `tool_result` part of a user row.
    ToolResult {
        /// The call's id.
        id: ShortId,
        /// Its `is_error`.
        error: bool,
    },
    /// `system subtype=turn_duration`: the vendor turn ended.
    TurnEnd {
        /// Its `durationMs`.
        vdur_ms: u64,
        /// Input tokens over its rows (the usage fold's dedupe).
        tok_in: u64,
        /// Output tokens over its rows.
        tok_out: u64,
    },
    /// `system subtype=compact_boundary` (S24).
    Compact,
    /// An assistant row whose model or effort differs from the last (S21: never a `<synthetic>` or API-error row), or the registry's version.
    Model {
        /// `message.model`.
        model: Option<&'a str>,
        /// The row's `effort`.
        effort: Option<&'a str>,
        /// The registry's version.
        ver: Option<&'a str>,
    },
    /// The wall judge's answer for the screen's wall of `kind`.
    Wall {
        /// The screen's wall.
        kind: WallKind,
        /// The answer.
        judge: WallJudge,
    },
    /// The vendor turn mapped to a turn the screen closed ended on the vendor's own notice of `kind`: its outcome is refined to the wall.
    EndWall {
        /// The notice's wall.
        kind: WallKind,
    },
    /// `n` rows the glue could not hand on (an over-long line, a restart's gap): counted `dropped=` (S23, S27: never a ring loss).
    Lost {
        /// How many.
        n: u64,
    },
    /// The glue's queue overflowed by `n`.
    Overflow {
        /// How many.
        n: u64,
    },
    /// The glue's batch ended.
    Flush,
}

/// What the join holds, for a conformance projection, the glue or a debugger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxView {
    /// The watermark.
    pub through: Option<TxMark>,
    /// The floor (wall ms): the latest of the bound process's start and the adoption; the run's open is added at each read.
    pub floor_ms: u64,
    /// The running vendor turn's id.
    pub vref: Option<ShortId>,
    /// The log turn it maps to (open or closed).
    pub mapped: Option<Node>,
    /// Whether its turn end was read.
    pub ended: bool,
    /// Transcript steps held (open, or closed by a cascade and still owed a result).
    pub tools: usize,
    /// The state the last `source` mark said.
    pub state: Option<TxState>,
    /// A wall the transcript refuted, latched until the screen's word moves (S2).
    pub refuted: Option<WallKind>,
    /// Desynced by an overflow, until the next prompt.
    pub desync: bool,
    /// Rows counted lost.
    pub dropped: u64,
    /// A loss counted that no `source` mark said yet ([`TxEvent::Flush`]).
    pub unsaid: bool,
    /// The running vendor turn's prompt time (wall ms; 0 when not known, a carry from before it was carried).
    pub prompt_ms: u64,
}

/// The running vendor turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VTurn {
    vref: ShortId,
    /// The log turn it maps to: the open one, or one closed since.
    node: Option<Node>,
    /// Its turn end's time (wall ms), once read: its turn end's, or its
    /// interrupt's.
    end_ms: Option<u64>,
    /// Its prompt's time (wall ms; 0: not known).
    at_ms: u64,
}

/// The join's state, in the tracker (so one with the screen's, under the
/// same lock, and on the same carry line).
#[derive(Clone, Debug, PartialEq, Eq)]
struct TxJoin {
    through: Option<TxMark>,
    floor_ms: u64,
    cur: Option<VTurn>,
    /// Transcript steps by call id: open, or closed by their turn's cascade
    /// and owed a result (A1: never carried; rebuilt from the log).
    tools: Vec<(ShortId, Node, Kind)>,
    state: Option<TxState>,
    refuted: Option<WallKind>,
    desync: bool,
    dropped: u64,
    /// `dropped` moved since a `source` mark last said it.
    unsaid: bool,
    /// Rebuild `tools` from the log at the next read (an adoption).
    rebuild: bool,
}

impl TxJoin {
    const fn new() -> TxJoin {
        TxJoin {
            through: None,
            floor_ms: 0,
            cur: None,
            tools: Vec::new(),
            state: None,
            refuted: None,
            desync: false,
            dropped: 0,
            unsaid: false,
            rebuild: false,
        }
    }

    const fn lose(&mut self, n: u64) {
        self.dropped = self.dropped.saturating_add(n);
        self.unsaid |= n > 0;
    }

    fn carry_into(&self, s: &mut String) {
        if let Some(m) = self.through {
            s.push_str(&format!(" tx_fp={:016x} tx_off={}", m.fp, m.off));
            if let Some((dev, ino)) = m.id {
                s.push_str(&format!(" tx_id={dev:x}:{ino:x}"));
            }
        }
        if let Some(v) = self.cur {
            s.push_str(&format!(" tx_v={}", v.vref));
            if let Some(n) = v.node {
                s.push_str(&format!(" tx_vt={n}"));
            }
            if let Some(e) = v.end_ms {
                s.push_str(&format!(" tx_ve={e}"));
            }
            if v.at_ms > 0 {
                s.push_str(&format!(" tx_vp={}", v.at_ms));
            }
        }
        if let Some(k) = self.refuted {
            s.push_str(" tx_ref=");
            s.push_str(k.name());
        }
        if let Some(st) = self.state {
            s.push_str(" tx_st=");
            s.push_str(st.word());
        }
        if self.desync {
            s.push_str(" tx_ds=1");
        }
        if self.dropped > 0 {
            s.push_str(&format!(" tx_dr={}", self.dropped));
        }
        if self.unsaid {
            s.push_str(" tx_du=1");
        }
    }

    fn from_carry<'l>(get: &dyn Fn(&str) -> Option<&'l str>, now: Stamp) -> TxJoin {
        let hex = |v: &str| u64::from_str_radix(v, 16).ok();
        let through = (|| {
            let fp = hex(get("tx_fp")?)?;
            let off = get("tx_off")?.parse().ok()?;
            let id = match get("tx_id") {
                None => None,
                Some(v) => {
                    let (d, i) = v.split_once(':')?;
                    Some((hex(d)?, hex(i)?))
                }
            };
            Some(TxMark { fp, id, off })
        })();
        let cur = get("tx_v").and_then(ShortId::new).map(|vref| VTurn {
            vref,
            node: get("tx_vt")
                .and_then(Node::parse)
                .filter(|n| n.lvl == Lvl::Turn),
            end_ms: get("tx_ve").and_then(|v| v.parse().ok()),
            at_ms: get("tx_vp").and_then(|v| v.parse().ok()).unwrap_or(0),
        });
        TxJoin {
            through,
            floor_ms: now.wall_ms,
            cur: cur.filter(|_| through.is_some()),
            tools: Vec::new(),
            state: get("tx_st").and_then(TxState::from_word),
            refuted: get("tx_ref").and_then(WallKind::from_name),
            desync: get("tx_ds") == Some("1"),
            dropped: get("tx_dr").and_then(|v| v.parse().ok()).unwrap_or(0),
            unsaid: get("tx_du") == Some("1"),
            rebuild: true,
        }
    }
}

const fn rest_cause(v: Verdict) -> Option<SettleCause> {
    match v {
        Verdict::Idle => Some(SettleCause::Idle),
        Verdict::Question => Some(SettleCause::Question),
        Verdict::Survey => Some(SettleCause::Survey),
        _ => None,
    }
}

const fn no_earlier(at: Stamp, wall_ms: u64) -> Stamp {
    if wall_ms <= at.wall_ms {
        at
    } else {
        Stamp::new(wall_ms, at.mono_ms.saturating_add(wall_ms - at.wall_ms))
    }
}

/// THE DATE OF A TRANSCRIPT CLOSE of `node`: `chosen`, or later.
#[must_use]
pub fn tx_close_at(log: &Log, node: Node, chosen: Stamp) -> Stamp {
    let mut newest = log.open_entry(node).map_or(0, |e| e.t);
    let mut kids: Vec<Node> = Vec::new();
    for r in log.window() {
        if r.op == Op::Open && r.par == Some(node) {
            kids.push(r.node);
            newest = newest.max(r.t);
        } else if r.op == Op::Close && kids.contains(&r.node) {
            newest = newest.max(r.t);
        }
    }
    for e in log.open_under(node, Lvl::Step) {
        newest = newest.max(e.t);
    }
    no_earlier(chosen, newest)
}

impl TurnTracker {
    /// What the transcript join holds.
    #[must_use]
    pub fn tx_view(&self) -> TxView {
        TxView {
            through: self.tx.through,
            floor_ms: self.tx.floor_ms,
            vref: self.tx.cur.map(|v| v.vref),
            mapped: self.tx.cur.and_then(|v| v.node),
            ended: self.tx.cur.is_some_and(|v| v.end_ms.is_some()),
            tools: self.tx.tools.len(),
            state: self.tx.state,
            refuted: self.tx.refuted,
            desync: self.tx.desync,
            dropped: self.tx.dropped,
            unsaid: self.tx.unsaid,
            prompt_ms: self.tx.cur.map_or(0, |v| v.at_ms),
        }
    }

    /// ONE TRANSCRIPT FACT (§2.5, §2.6): `ev`, read at `pos` while the source is `state`, applied at the host's `now`.
    pub fn transcript(
        &mut self,
        log: &mut Log,
        ev: TxEvent<'_>,
        pos: TxPos,
        state: TxState,
        now: Stamp,
    ) -> Option<u64> {
        self.resync(log);
        if !log.is_open(self.run) {
            return self.wake_at();
        }
        self.settle_if_due(log, now);
        if self.tx.rebuild {
            self.rebuild_tools(log);
        }
        if let TxEvent::Bind { len, floor_ms } = ev {
            self.bind(log, pos, len, floor_ms, state, now);
            return self.wake_at();
        }
        let row = !matches!(
            ev,
            TxEvent::Wall { .. } | TxEvent::Lost { .. } | TxEvent::Overflow { .. } | TxEvent::Flush
        );
        if self.tx.through.is_none() {
            return self.wake_at();
        }
        if row {
            if !self.tx.through.is_some_and(|m| m.admits(pos)) {
                return self.wake_at();
            }
            self.tx.through = Some(TxMark::of(pos));
            let run_t = log.open_entry(self.run).map_or(0, |e| e.t);
            if pos.wall_ms < self.tx.floor_ms.max(run_t) {
                return self.wake_at();
            }
        }
        match ev {
            TxEvent::Lost { n } => self.tx.lose(n),
            TxEvent::Overflow { n } => {
                self.tx.lose(n);
                self.tx.desync = true;
            }
            TxEvent::Flush => {
                if self.tx.unsaid
                    && let Some(st) = self.tx.state
                {
                    self.write_source(log, st, None, now);
                }
                return self.wake_at();
            }
            _ => {}
        }
        self.say_state(log, state, None, now);
        if self.tx.unsaid && matches!(ev, TxEvent::Overflow { .. }) {
            self.write_source(log, state, None, now);
        }
        let Some(conf) = state.conf() else {
            return self.wake_at();
        };
        let row = rebase(pos.wall_ms, now);
        match ev {
            TxEvent::Bind { .. }
            | TxEvent::Lost { .. }
            | TxEvent::Overflow { .. }
            | TxEvent::Flush => {}
            TxEvent::Prompt { vref } => self.tx_prompt(log, vref, state, conf, row, now),
            TxEvent::Interrupt => {
                // An unmeasured source may end its own vendor turn below,
                // but must not change the screen turn's eventual outcome.
                if state == TxState::On {
                    self.note_interrupted();
                }
                if let Some(v) = self.tx.cur.filter(|v| v.end_ms.is_none()) {
                    self.tx.cur = Some(VTurn {
                        end_ms: Some(row.wall_ms),
                        ..v
                    });
                }
            }
            TxEvent::ToolUse {
                id,
                tool,
                cls,
                subagent,
            } => self.tx_tool(log, id, (tool, cls, subagent), conf, row),
            TxEvent::ToolResult { id, error } => self.tx_result(log, id, error, state, row),
            TxEvent::TurnEnd {
                vdur_ms,
                tok_in,
                tok_out,
            } => self.tx_end(log, [vdur_ms, tok_in, tok_out], state, conf, row),
            TxEvent::Compact => self.tx_compact(log, conf, row),
            TxEvent::Model { model, effort, ver } => {
                self.tx_model(log, [model, effort, ver], conf, row);
            }
            TxEvent::Wall { kind, judge } => self.tx_wall(log, kind, judge, state, now),
            TxEvent::EndWall { kind } => self.tx_end_wall(log, kind, state, now),
        }
        self.wake_at()
    }

    fn bind(
        &mut self,
        log: &mut Log,
        pos: TxPos,
        len: u64,
        floor_ms: u64,
        state: TxState,
        now: Stamp,
    ) {
        self.tx.floor_ms = self.tx.floor_ms.max(floor_ms);
        let kept = self
            .tx
            .through
            .is_some_and(|m| m.fp == pos.fp && m.id == pos.id && m.off <= len);
        if kept {
            self.say_state(log, state, None, now);
            return;
        }
        self.tx.through = Some(TxMark {
            fp: pos.fp,
            id: pos.id,
            off: len,
        });
        self.tx.cur = None;
        self.tx.desync = false;
        self.write_source(log, state, Some(len), now);
    }

    fn say_state(&mut self, log: &mut Log, state: TxState, skipped: Option<u64>, now: Stamp) {
        if skipped.is_some() || self.tx.state != Some(state) {
            self.write_source(log, state, skipped, now);
        }
    }

    fn write_source(&mut self, log: &mut Log, state: TxState, skipped: Option<u64>, now: Stamp) {
        self.tx.state = Some(state);
        self.tx.unsaid = false;
        let mut f = Fields::new();
        if let Some(t) = BoundedToken::new(Key::Field, "transcript") {
            let _ = f.set(Key::Field, Val::Tok(t));
        }
        let _ = f.set(Key::State, Val::Word(state.word()));
        if let Some(n) = skipped {
            let _ = f.set(Key::Skipped, Val::U(n));
        }
        if self.tx.dropped > 0 {
            let _ = f.set(Key::Dropped, Val::U(self.tx.dropped));
        }
        let mark = Mark::new(Kind::Source, Src::Transcript, Conf::Exact)
            .at(self.run)
            .by(Actor::Aterm)
            .fields(f);
        let _ = log.mark(mark, now);
    }

    fn rebuild_tools(&mut self, log: &Log) {
        self.tx.rebuild = false;
        let Some(turn) = self.turn else {
            return;
        };
        for e in log
            .open_under(turn, Lvl::Step)
            .filter(|e| matches!(e.kind, Kind::Tool | Kind::Subagent))
        {
            let id = log
                .window()
                .find(|r| r.op == Op::Open && r.node == e.node && r.src == Src::Transcript)
                .and_then(|r| match r.f.get(Key::Vref) {
                    Some(Val::Id(id)) => Some(*id),
                    _ => None,
                });
            if let Some(id) = id
                && self.tx.tools.len() < TX_TOOLS_MAX
                && !self.tx.tools.iter().any(|(v, ..)| *v == id)
            {
                self.tx.tools.push((id, e.node, e.kind));
            }
        }
    }

    fn amend_vref(log: &mut Log, node: Node, vref: ShortId, conf: Conf, at: Stamp) {
        let mut f = Fields::new();
        let _ = f.set(Key::Vref, Val::Id(vref));
        let amend = Amend::new(Kind::Turn, Src::Transcript, conf, f).by(Actor::Vendor);
        let _ = log.amend(node, amend, at);
    }

    fn tx_prompt(
        &mut self,
        log: &mut Log,
        vref: ShortId,
        state: TxState,
        conf: Conf,
        row: Stamp,
        now: Stamp,
    ) {
        self.tx.desync = false;
        let tp = row.wall_ms;
        let prev_end = self.tx.cur.and_then(|v| v.end_ms);
        let run_t = log.open_entry(self.run).map_or(0, |e| e.t);
        let mapped = |node: Node| VTurn {
            vref,
            node: Some(node),
            end_ms: None,
            at_ms: tp,
        };
        let unmapped = VTurn {
            vref,
            node: None,
            end_ms: None,
            at_ms: tp,
        };
        let late = log.state().last[1]
            .as_ref()
            .map(|c| (c.node, c.opened_t, c.f.get(Key::Vref).is_some()))
            .and_then(|(node, opened, held)| {
                opened
                    .filter(|o| !held && *o >= run_t && o.abs_diff(tp) <= ATTRIBUTION_MS)
                    .map(|o| (node, o))
            });
        if let Some(t) = self.turn {
            let opened = log.open_entry(t).map_or(0, |e| e.t);
            let held =
                log.state()
                    .turn()
                    .filter(|o| o.node == t)
                    .and_then(|o| match o.f.get(Key::Vref) {
                        Some(Val::Id(id)) => Some(*id),
                        _ => None,
                    });
            let near = opened.abs_diff(tp);
            if let Some((node, o)) = late.filter(|(_, o)| o.abs_diff(tp) < near) {
                debug_assert!(o.abs_diff(tp) <= ATTRIBUTION_MS);
                Self::amend_vref(log, node, vref, conf, row);
                self.tx.cur = Some(mapped(node));
                return;
            }
            if held.is_none() && near <= ATTRIBUTION_MS {
                Self::amend_vref(log, t, vref, conf, row);
                self.tx.cur = Some(mapped(t));
                return;
            }
            if tp.saturating_add(ATTRIBUTION_MS) < opened {
                self.tx.cur = Some(unmapped);
                return;
            }
            let screen_held = self.step.as_ref().is_some_and(|h| h.under_turn);
            if held == Some(vref) || screen_held || state != TxState::On {
                self.tx.cur = Some(mapped(t));
                return;
            }
            let at = tx_close_at(log, t, rebase(prev_end.unwrap_or(tp), now));
            let close =
                Close::new(Out::Turn(TurnOut::Superseded), Src::Transcript, conf).by(Actor::Vendor);
            self.close_turn_with(log, close, TurnOut::Superseded, (None, false, true), at);
        } else if let Some((node, _)) = late {
            Self::amend_vref(log, node, vref, conf, row);
            self.tx.cur = Some(mapped(node));
            return;
        }
        if state != TxState::On {
            self.tx.cur = Some(unmapped);
            return;
        }
        let mut f = Fields::new();
        let _ = f.set(Key::Vref, Val::Id(vref));
        let at = no_earlier(row, run_t);
        match self.open_turn(log, Src::Transcript, conf, f, at) {
            Some(node) => {
                if let Some(cause) = rest_cause(self.word) {
                    self.arm(cause, now);
                }
                self.tx.cur = Some(mapped(node));
            }
            None => self.tx.cur = None,
        }
    }

    fn tx_tool(
        &mut self,
        log: &mut Log,
        id: ShortId,
        (tool, cls, subagent): (&str, &str, bool),
        conf: Conf,
        row: Stamp,
    ) {
        let turn = self
            .tx
            .cur
            .and_then(|v| v.node)
            .filter(|n| Some(*n) == self.turn && log.is_open(*n));
        let Some(turn) = turn.filter(|_| !self.tx.desync) else {
            self.tx.lose(1);
            return;
        };
        if self.tx.tools.iter().any(|(v, ..)| *v == id) {
            return;
        }
        if self.tx.tools.len() >= TX_TOOLS_MAX {
            self.tx.tools.retain(|(_, n, _)| log.is_open(*n));
        }
        if self.tx.tools.len() >= TX_TOOLS_MAX {
            self.tx.lose(1);
            return;
        }
        let kind = if subagent { Kind::Subagent } else { Kind::Tool };
        let mut f = Fields::new();
        if let Some(w) = lookup(CLS_WORDS, cls) {
            let _ = f.set(Key::Cls, Val::Word(w));
        }
        if let Some(t) = BoundedText::new(Key::Tool, tool) {
            let _ = f.set(Key::Tool, Val::Text(t));
        }
        let _ = f.set(Key::Vref, Val::Id(id));
        let at = no_earlier(row, log.open_entry(turn).map_or(0, |e| e.t));
        let open = Open::new(kind, Src::Transcript, conf)
            .under(turn)
            .by(Actor::Vendor)
            .fields(f);
        if let Ok(p) = log.open(open, at) {
            self.tx.tools.push((id, p, kind));
        }
    }

    fn tx_result(&mut self, log: &mut Log, id: ShortId, error: bool, state: TxState, row: Stamp) {
        let at = self.tx.tools.iter().position(|(v, ..)| *v == id);
        let Some(i) = at.filter(|_| !self.tx.desync) else {
            self.tx.lose(1);
            return;
        };
        let (_, p, kind) = self.tx.tools.remove(i);
        let out = Out::Step(if error { StepOut::Error } else { StepOut::Ok });
        let Some(conf) = state.conf() else {
            return;
        };
        if let Some(opened) = log.open_entry(p).map(|e| e.t) {
            let at = no_earlier(row, opened);
            let close = Close::new(out, Src::Transcript, conf)
                .by(Actor::Vendor)
                .dur_wall(at.wall_ms.saturating_sub(opened));
            let _ = log.close(p, close, at);
        } else if state == TxState::On {
            let mut f = Fields::new();
            let _ = f.set(Key::Out, Val::Word(out.word()));
            let amend = Amend::new(kind, Src::Transcript, Conf::Exact, f).by(Actor::Vendor);
            let _ = log.amend(p, amend, row);
        }
    }

    fn tx_end(&mut self, log: &mut Log, n: [u64; 3], state: TxState, conf: Conf, row: Stamp) {
        if self.tx.desync {
            self.tx.lose(1);
            return;
        }
        let Some(v) = self.tx.cur.filter(|v| v.end_ms.is_none()) else {
            return;
        };
        self.tx.cur = Some(VTurn {
            end_ms: Some(row.wall_ms),
            ..v
        });
        let Some(node) = v.node else {
            return;
        };
        let mut at = row;
        let open = Some(node) == self.turn && log.is_open(node);
        let screen_held = self.step.as_ref().is_some_and(|h| h.under_turn);
        if open && !screen_held && state == TxState::On {
            let (out, oconf) = match self.settle.map(|s| s.cause) {
                Some(c) => match settle_out(c, self.interrupted) {
                    (o, Conf::Heuristic) => (o, Conf::Heuristic),
                    (o, _) => (o, conf),
                },
                None if self.interrupted => (TurnOut::Interrupted, conf),
                None => (TurnOut::Done, conf),
            };
            at = tx_close_at(log, node, row);
            let close = Close::new(Out::Turn(out), Src::Transcript, oconf).by(Actor::Vendor);
            self.close_turn_with(log, close, out, (None, false, true), at);
        }
        let mut f = Fields::new();
        for (k, v) in [(Key::VdurMs, n[0]), (Key::TokIn, n[1]), (Key::TokOut, n[2])] {
            let _ = f.set(k, Val::U(v));
        }
        let amend = Amend::new(Kind::Turn, Src::Transcript, conf, f).by(Actor::Vendor);
        let _ = log.amend(node, amend, at);
    }

    fn tx_compact(&mut self, log: &mut Log, conf: Conf, row: Stamp) {
        if self.tx.desync {
            self.tx.lose(1);
            return;
        }
        let parent = self.turn.filter(|t| log.is_open(*t)).unwrap_or(self.run);
        let at = no_earlier(row, log.open_entry(parent).map_or(0, |e| e.t));
        let open = Open::new(Kind::Compact, Src::Transcript, conf)
            .under(parent)
            .by(Actor::Vendor);
        if let Ok(p) = log.open(open, at) {
            let close = Close::new(Out::Step(StepOut::Ok), Src::Transcript, conf)
                .by(Actor::Vendor)
                .dur_wall(0);
            let _ = log.close(p, close, at);
        }
    }

    fn tx_model(&mut self, log: &mut Log, vals: [Option<&str>; 3], conf: Conf, row: Stamp) {
        let held = log
            .state()
            .run()
            .filter(|o| o.node == self.run)
            .map(|o| o.f.clone())
            .unwrap_or_default();
        let mut f = Fields::new();
        for (k, v) in [Key::Model, Key::Effort, Key::Ver].into_iter().zip(vals) {
            if let Some(t) = v.and_then(|v| BoundedToken::new(k, v))
                && held.str(k) != Some(t.as_str())
            {
                let _ = f.set(k, Val::Tok(t));
            }
        }
        if f.iter().next().is_some() {
            let amend = Amend::new(Kind::Agent, Src::Transcript, conf, f).by(Actor::Vendor);
            let _ = log.amend(self.run, amend, row);
        }
    }

    fn tx_wall(
        &mut self,
        log: &mut Log,
        kind: WallKind,
        judge: WallJudge,
        state: TxState,
        now: Stamp,
    ) {
        if state != TxState::On {
            return;
        }
        let Some(h) = self
            .step
            .clone()
            .filter(|h| h.what == ScreenStep::Wall(kind))
        else {
            return;
        };
        match judge {
            WallJudge::Confirm => {
                let done = log.window().any(|r| {
                    r.op == Op::Amend && r.node == h.node && r.f.get_u(Key::Confirmed) == Some(1)
                });
                if !done {
                    let mut f = Fields::new();
                    let _ = f.set(Key::Confirmed, Val::U(1));
                    let amend =
                        Amend::new(Kind::Wall, Src::Transcript, Conf::Exact, f).by(Actor::Vendor);
                    let _ = log.amend(h.node, amend, now);
                }
            }
            WallJudge::Refute => {
                let at = no_earlier(now, log.open_entry(h.node).map_or(0, |e| e.t));
                let close = Close::new(Out::Step(StepOut::Retracted), Src::Transcript, Conf::Exact)
                    .by(Actor::Vendor);
                let _ = log.close(h.node, close, at);
                self.forget_step();
                if self
                    .settle
                    .is_some_and(|s| s.cause == SettleCause::Wall(kind))
                {
                    self.settle = None;
                }
                self.tx.refuted = Some(kind);
                if !h.under_turn
                    && let Some(c) = self.last_close.filter(|c| c.out == TurnOut::Wall(kind))
                {
                    let mut f = Fields::new();
                    let _ = f.set(Key::Out, Val::Word("done"));
                    let amend =
                        Amend::new(Kind::Turn, Src::Transcript, Conf::Exact, f).by(Actor::Vendor);
                    let _ = log.amend(c.node, amend, at);
                }
            }
        }
    }

    fn tx_end_wall(&mut self, log: &mut Log, kind: WallKind, state: TxState, now: Stamp) {
        if state != TxState::On {
            return;
        }
        let Some(node) = self
            .tx
            .cur
            .and_then(|v| v.node)
            .filter(|n| !log.is_open(*n))
        else {
            return;
        };
        let mut f = Fields::new();
        let _ = f.set(Key::Out, Val::Word(kind.wire()));
        let amend = Amend::new(Kind::Turn, Src::Transcript, Conf::Exact, f).by(Actor::Vendor);
        let _ = log.amend(node, amend, now);
    }
}
