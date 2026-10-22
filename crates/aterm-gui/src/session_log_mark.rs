// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SESSION LOG'S CONSUMER HALF, host side (design §7.2-§7.4, phase 3A):
//! what a reader of the log is told beside the log itself, a child module of
//! `session_log` so it reaches [`SessionLog`]'s fields as the carry module
//! does.
//!
//! - [`SessionLog::status_fields`]: the `status` record's `log_seq= log_ep=
//!   run= turn= step= need= need_since_ms=`, from the reader's view (a fenced
//!   log shows its fence).
//! - [`SessionLog::take_log_wake`]: THE WAKE. A producer appends under the
//!   timeline's lock and takes the owed wake, which rings the session's
//!   watchers (`await log`, `subscribe … log`) when this says the reader's
//!   head moved: `SessionTimeline::slog_take_wake` rings them in that same
//!   hold, and a `mark` ([`SessionLog::mark_claim`]) once its hold ends.
//!   Every producer site does so (the status sweep, the settle tick, a
//!   person's burst, the restore lineage, a failure seen, a fence lifted,
//!   control input, a `ctl turn`, a `mark`); the session's close wakes
//!   through `Wake::Exit`.
//! - [`SessionLog::ctl_input`]: control input, by its author, its burst's
//!   `mark - input`; [`validate_cause`] what a `cause=` may name.

use super::SessionLog;

impl SessionLog {
    /// THE `status` FIELDS (phase 3A), printed before the `gen=`/`seq=`/
    /// `hash=` stamp: `log_seq=<n> log_ep=<e> run=<r|-> turn=<t|-> step=<p|->
    /// need=<w|-> need_since_ms=<ms|->`. The head and the epoch are the
    /// reader's ([`aterm_slog::Log::reader_last_seq`]: the fence while an
    /// update's carry stands), so a `since=` taken from them is one every
    /// read accepts; the open path and the need are the fold's as a reader is
    /// shown it. `need=` is the most urgent OBSERVED need: `status` is the
    /// classifier's record and names no author, so a need a claim raised
    /// (`mark … need`) appears only in `brief` (`claim need …`). `need_since_ms=`
    /// is its age at `now_wall`.
    pub(crate) fn status_fields(&self, now_wall: u64) -> String {
        let log = &self.log;
        let st = log.reader_state();
        let node = |n: Option<&aterm_slog::OpenNode>| {
            n.map_or_else(|| "-".to_string(), |o| o.node.to_string())
        };
        let need = st
            .needs
            .iter()
            .flatten()
            .find(|n| n.src != aterm_slog::Src::Mark);
        format!(
            "log_seq={} log_ep={} run={} turn={} step={} need={} need_since_ms={}",
            log.reader_last_seq(),
            log.ep(),
            node(st.run()),
            node(st.turn()),
            node(st.step()),
            need.map_or("-", |n| n.word),
            need.map_or_else(
                || "-".to_string(),
                |n| now_wall.saturating_sub(n.t).to_string()
            ),
        )
    }
}

// Control input and `cause=` (phase 3A, design §2.5-§2.6).

use aterm_slog::{Actor, Conf, Fields, Key, Kind, Mark, Node, ShortId, Src, Stamp};

/// What an acted control input's mark says (ruling T1): its `act=`, and
/// `re=` the step it was admitted under when that step closed before its
/// bytes were written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InputAct {
    pub(crate) act: Option<aterm_slog::ActWord>,
    pub(crate) re: Option<Node>,
}

impl InputAct {
    /// The mark of an input with `act` whose acceptance came to `bound`.
    pub(crate) fn of_acceptance(
        act: Option<aterm_slog::ActWord>,
        bound: aterm_slog::Binding,
    ) -> InputAct {
        let re = match bound {
            aterm_slog::Binding::Late(node) if act.is_some() => Some(node),
            _ => None,
        };
        InputAct { act, re }
    }
}

/// The input verbs the log names (`mark - input verb=`): `send`, `key` (and
/// `ctrl`), `paste` (and `paste-bin`), `feed` (and `feed-bin`), `turn`.
/// `mouse` and `focus` are not recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputVerb {
    /// `send`.
    Send,
    /// `key`, `ctrl`.
    Key,
    /// `paste`, `paste-bin`.
    Paste,
    /// `feed`, `feed-bin`.
    Feed,
    /// `turn`.
    Turn,
}

impl InputVerb {
    /// The verb a control verb's input is, when the log records it.
    pub(crate) fn of(verb: &str) -> Option<InputVerb> {
        match verb {
            "send" => Some(InputVerb::Send),
            "key" | "ctrl" => Some(InputVerb::Key),
            "paste" | "paste-bin" => Some(InputVerb::Paste),
            "feed" | "feed-bin" => Some(InputVerb::Feed),
            "turn" => Some(InputVerb::Turn),
            _ => None,
        }
    }

    /// The `verb=` word.
    const fn word(self) -> &'static str {
        match self {
            InputVerb::Send => "send",
            InputVerb::Key => "key",
            InputVerb::Paste => "paste",
            InputVerb::Feed => "feed",
            InputVerb::Turn => "turn",
        }
    }
}

/// The authors whose control-input bursts are told apart (ruling R11): the
/// trusted actors (the harness, the bridge, `ctl`, the person) each have a
/// burst clock of their own outside every table, so no flood of other
/// authors suppresses their `mark - input`; sessions (edge sources) and the
/// unattributed share a table of eight, each slot reused once its burst
/// ended, and one slot every author past them shares, so they open a burst
/// at most once a window together.
const INPUT_BURST_ACTORS: usize = 8;

/// Which reserved burst clock a trusted actor's input keeps
/// ([`Consumers::reserved`]), or `None`: a session or nobody, which share
/// the table.
const fn reserved_burst(by: &Actor) -> Option<usize> {
    match by {
        Actor::Harness => Some(0),
        Actor::Bridge => Some(1),
        Actor::Ctl => Some(2),
        Actor::Human => Some(3),
        Actor::Aterm | Actor::Vendor | Actor::Sid(_) | Actor::Nobody => None,
    }
}

/// THE CONSUMER HALF's state in a session's log (one `SessionLog` field):
/// the mark limiter, the watchers' wake and the control-input bursts. Under
/// 2 KiB a session, and under 4 KiB while the limiter's owed-only table is
/// full (32 entries of 56 bytes on the heap, ruling U18); not counted by
/// `Log::footprint`.
#[derive(Debug)]
pub(crate) struct Consumers {
    /// `mark`'s per-author limiter ([`aterm_slog::MarkLimiter`]): one claim a
    /// second, bursts of twenty, the trusted authors' buckets their own.
    limiter: aterm_slog::MarkLimiter,
    /// The reader's head (epoch, seq) the watchers were last woken for
    /// ([`SessionLog::take_log_wake`]).
    notified: (u32, u64),
    /// When each trusted actor last sent control input (monotonic ms), by
    /// [`reserved_burst`]: never shared, never taken by another author.
    reserved: [Option<u64>; 4],
    /// Which session (or nobody) last sent control input, and when.
    bursts: [Option<(Actor, u64)>; INPUT_BURST_ACTORS],
    /// When an author past the table last sent input.
    other: Option<u64>,
    /// The exit path is running and the retire that writes the session's
    /// close may follow it: a parked `await log` waits for that close
    /// ([`SessionLog::note_close_due`]). Cleared by the close itself and by
    /// the end of the exit handling ([`SessionLog::take_close_due`]).
    close_due: bool,
}

impl Default for Consumers {
    fn default() -> Consumers {
        Consumers {
            limiter: aterm_slog::MarkLimiter::new(0),
            notified: (0, 0),
            reserved: [None; 4],
            bursts: [None; INPUT_BURST_ACTORS],
            other: None,
            close_due: false,
        }
    }
}

impl Consumers {
    /// Whether input from `by` at `now_mono` opens a burst: its first after
    /// [`crate::session_timeline::HUMAN_BURST_GAP_MS`] without one. A
    /// trusted actor keeps its own clock ([`reserved_burst`]); any other
    /// author keeps its slot while it types, and a new one takes a free slot
    /// or one whose burst ended, else the shared one.
    fn burst_starts(&mut self, by: Actor, now_mono: u64) -> bool {
        let gap = crate::session_timeline::HUMAN_BURST_GAP_MS;
        let ended = |t: u64| now_mono.saturating_sub(t) >= gap;
        if let Some(i) = reserved_burst(&by) {
            let starts = self.reserved[i].is_none_or(ended);
            self.reserved[i] = Some(now_mono);
            return starts;
        }
        if let Some((_, t)) = self.bursts.iter_mut().flatten().find(|(a, _)| *a == by) {
            let starts = ended(*t);
            *t = now_mono;
            return starts;
        }
        let free = self
            .bursts
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_none_or(|(_, t)| ended(t)))
            .min_by_key(|(_, s)| s.map_or(0, |(_, t)| t))
            .map(|(i, _)| i);
        if let Some(i) = free {
            self.bursts[i] = Some((by, now_mono));
            return true;
        }
        let starts = self.other.is_none_or(ended);
        self.other = Some(now_mono);
        starts
    }
}

impl SessionLog {
    /// THE OWED WAKE (phase 3A, design §2.2): whether what a reader is shown
    /// moved since the watchers were last woken — the reader's head
    /// ([`aterm_slog::Log::reader_last_seq`]) or its epoch — and so whether
    /// this session's watchers are owed a wake. `SessionTimeline::slog_take_wake`
    /// takes it and rings them in the same timeline hold, as
    /// `SessionTimeline::record` does: the watchers' `slots` lock is the leaf
    /// under the timeline's lock. [`Self::mark_claim`] hands it to the `mark`
    /// arm, which rings them once that hold ends. One comparison of two
    /// numbers, so no append path (this phase's, an earlier one's, or a later
    /// one's) can be forgotten: every append moves the head, a fence lifting
    /// moves it past the overlap, an adoption moves the epoch.
    pub(crate) fn take_log_wake(&mut self) -> bool {
        let head = (self.log.ep(), self.log.reader_last_seq());
        let owed = head != self.consumers.notified;
        self.consumers.notified = head;
        owed
    }

    /// THE CLOSE IS DUE (`App::exit_session_logical`, told BEFORE the session
    /// reads `Exited`): its program ended and the exit path is running, so
    /// the retire that writes the session's close (`close - spawn
    /// out=shell-exit code=`) may follow. Only while this holds does an
    /// `await log` parked on an ended session wait for that close
    /// (`control_session::await_log`); every other ended session answers
    /// `ERR exited` at once — a pane kept after its program ended (`--hold`,
    /// a shell that failed at start, a held pane restored across an update),
    /// or one any later path marks `Exited`, writes no close until someone
    /// closes it. The close clears it ([`Self::close_session`]), and so does
    /// the end of the exit handling when no retire came
    /// ([`Self::take_close_due`]).
    pub(crate) fn note_close_due(&mut self) {
        self.consumers.close_due = true;
    }

    /// Whether the session's close is due ([`Self::note_close_due`]). Only for
    /// tests: production reads the flag only inside `await_look`, together
    /// with the log, so no waiter can read the two in separate lock holds.
    #[cfg(test)]
    pub(crate) fn close_due(&self) -> bool {
        self.consumers.close_due
    }

    /// THE EXIT HANDLING IS OVER (`App::settle_log_close`): clear
    /// [`Self::note_close_due`] and say whether it still held, i.e. whether
    /// the waiters parked on it must be woken to read that no close is coming.
    pub(crate) fn take_close_due(&mut self) -> bool {
        std::mem::take(&mut self.consumers.close_due)
    }

    /// CONTROL INPUT reached the session (`send`, `key`, `paste`, `feed`, and
    /// their binary twins, once it moved bytes): the tracker attributes a turn
    /// that opens within 5 s to `by` (the connection's actor, never a
    /// parameter) with its `cause=`, a run a waiting name opens later is told
    /// it too (`Typed`), and the first input of a burst from `by` writes
    /// `mark - input verb=<v> [cause= cause_k=] by=<a> src=ctl conf=strong` on
    /// the session. The bytes are never recorded.
    /// An input that carries `act=` (ruling T1, `acted`) is marked at
    /// every input, with its act and a late one's step, within its author's
    /// mark tokens ([`Self::ctl_input_mark`]).
    pub(crate) fn ctl_input(
        &mut self,
        by: Actor,
        verb: InputVerb,
        cause: Option<ShortId>,
        acted: InputAct,
        at: Stamp,
    ) {
        self.resync();
        self.arrivals += 1;
        self.typed.input = Some((self.arrivals, by, cause, at));
        if let Some(a) = self.agent.as_mut() {
            a.tracker.note_input_caused(by, cause, at);
        }
        self.ctl_input_mark(by, verb, cause, acted, at);
    }

    /// THE ACCEPTANCE of a control input admitted as `adm` (its bytes fully
    /// written; ruling T1): a person's key the timeline's hold kept from the
    /// log first (T2), then the tracker binds the input from `by` with its
    /// `act=` to the approval or question step it was admitted under, or
    /// led to. What it came to (a late input's step, for its mark's `re=`).
    pub(crate) fn ctl_accepted(
        &mut self,
        by: Actor,
        act: Option<aterm_slog::ActWord>,
        adm: Option<aterm_slog::Admission>,
        at: Stamp,
    ) -> aterm_slog::Binding {
        self.reconcile_person(at);
        self.resync();
        let Self { log, agent, .. } = self;
        match (agent.as_mut(), adm) {
            (Some(a), Some(adm)) => a.tracker.accept(log, by, act, adm, at),
            _ => aterm_slog::Binding::Unbound,
        }
    }

    /// THE ADMISSION of a control input about to be written (ruling T1):
    /// what the agent's tracker holds now, read under the timeline lock the
    /// writer takes before a byte. `None` with no agent run.
    pub(crate) fn admit_input(&self, at: Stamp) -> Option<aterm_slog::Admission> {
        self.agent.as_ref().map(|a| a.tracker.admit(at))
    }

    /// The input mark alone, for an input the tracker was already told of (a
    /// `ctl turn`'s first byte, [`SessionLog::ctl_turn`]): written at the first
    /// input of a burst from `by`.
    /// An acted input (ruling T1) is marked whatever the burst, `mark -
    /// input verb=<v> act=<w> [re=<step>] [dropped=<n> [pool=shared]]
    /// by=<it>`, `re=` the
    /// step a late one was admitted under (it closed before the bytes were
    /// written), within its author's mark tokens: one past its burst's first
    /// spends one ([`aterm_slog::MarkLimiter`], as a claim does: twenty, then
    /// one a second), and one its bucket is short of is counted, not marked,
    /// the author's next acted mark saying how many (`dropped=`): its own
    /// count, a trusted actor's, a named slot's holder's, or an overflowed
    /// author's owed-only entry's (Phase 6 step 6C-6, ruling U18); past
    /// that table, the shared pool's, said as the pool's (`dropped=<n>
    /// pool=shared`) by an overflowed author with none of its own
    /// ([`aterm_slog::MarkLimiter::owed`]). So no author's acted keys push
    /// the session's other records out of its ring, and no author's count
    /// rides another author's mark as that author's.
    pub(crate) fn ctl_input_mark(
        &mut self,
        by: Actor,
        verb: InputVerb,
        cause: Option<ShortId>,
        acted: InputAct,
        at: Stamp,
    ) {
        let starts = self.consumers.burst_starts(by, at.mono_ms);
        if !starts && acted.act.is_none() {
            return;
        }
        let spends = !starts;
        if spends && self.consumers.limiter.peek(&by, at.mono_ms).is_err() {
            self.consumers.limiter.note_short(&by, at.mono_ms);
            return;
        }
        let f = Fields::new()
            .word(Key::Verb, verb.word())
            .unwrap_or_default();
        let f = match acted.act {
            Some(a) => f.clone().word(Key::Act, a.word()).unwrap_or(f),
            None => f,
        };
        let f = match cause {
            Some(c) => f.clone().cause(c.as_str()).unwrap_or(f),
            None => f,
        };
        let f = match acted.re {
            Some(n) => f.clone().id(Key::Re, &n.to_string()).unwrap_or(f),
            None => f,
        };
        let owed = if acted.act.is_some() {
            self.consumers.limiter.owed(&by, at.mono_ms)
        } else {
            aterm_slog::Owed::Own(0)
        };
        let dropped = owed.count();
        let mut f = f;
        if dropped > 0 {
            let _ = f.set(Key::Dropped, aterm_slog::Val::U(dropped));
            // The shared pool's count is said as the pool's (ruling U18).
            if owed.is_shared() {
                f = f.clone().word(Key::Pool, "shared").unwrap_or(f);
            }
        }
        let wrote = self
            .log
            .mark(
                Mark::new(Kind::Input, Src::Ctl, Conf::Strong)
                    .at(Node::SESSION)
                    .by(by)
                    .fields(f),
                at,
            )
            .is_ok();
        if wrote && dropped > 0 {
            self.consumers.limiter.said(&by, at.mono_ms);
        }
        if wrote && spends {
            self.consumers.limiter.spend(&by, at.mono_ms);
        }
    }

    /// Whether this log's mail records name broker offset `off` (a `mail`
    /// mark's `off=`).
    fn names_mail(&self, off: u64) -> bool {
        self.log
            .window()
            .any(|r| r.k == Kind::Mail && r.f.get_u(Key::Off) == Some(off))
    }

    /// Whether seq `n` lies in this log's current seq space: from its first
    /// accounted seq to the head a reader is shown, outside every seq range it
    /// never held.
    fn in_seq_space(&self, n: u64) -> bool {
        let log = &self.log;
        n >= log.first_seq().max(1)
            && n <= log.reader_last_seq()
            && !log.holes().any(|(lo, hi)| lo <= n && n <= hi)
    }
}

/// What a `cause=` names: mail by its broker offset, or a record by its seq.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CauseRef {
    /// `@<off>`.
    Mail(u64),
    /// `<seq>`, at least 1.
    Rec(u64),
}

impl CauseRef {
    /// `@<off>` or `<seq>` (decimal, no sign), else `None`.
    pub(crate) fn parse(raw: &str) -> Option<CauseRef> {
        let digits =
            |s: &str| !s.is_empty() && s.len() <= 19 && s.bytes().all(|b| b.is_ascii_digit());
        match raw.strip_prefix('@') {
            Some(off) if digits(off) => off.parse().ok().map(CauseRef::Mail),
            Some(_) => None,
            None if digits(raw) => raw.parse().ok().filter(|n| *n >= 1).map(CauseRef::Rec),
            None => None,
        }
    }
}

/// The usage refusal of a malformed or repeated `cause=`.
pub(crate) const CAUSE_USAGE: &str = "ERR usage: cause=@<off>|<seq> names the mail (@broker offset) or record (seq) acted on, once\n";

/// THE CAUSE CHECK (phase 3A ruling R7): `cause=` names only what THIS
/// session holds — mail it was delivered or a post of its own landed at
/// ([`crate::fabric::SessionFabric::holds_offset`]) or a `mail` record of its
/// log names, or a record seq inside its log's current seq space outside
/// every hole. The inbox lock and then the timeline's leaf lock, each alone
/// (no new lock order). `Ok` is the id the turn will name; `Err` the reply,
/// answered before any byte is written.
pub(crate) fn validate_cause(ctx: &crate::SessionCtx, cause: CauseRef) -> Result<ShortId, String> {
    match cause {
        CauseRef::Mail(off) => {
            let held = ctx.fabric.holds_offset(off) || {
                let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                timeline.slog().names_mail(off)
            };
            if !held {
                return Err(format!(
                    "ERR cause: @{off} names no mail this session holds\n"
                ));
            }
            ShortId::new(&format!("@{off}")).ok_or_else(|| CAUSE_USAGE.to_string())
        }
        CauseRef::Rec(n) => {
            let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            if !timeline.slog().in_seq_space(n) {
                return Err(format!("ERR cause: {n} names no record of this session\n"));
            }
            ShortId::new(&n.to_string()).ok_or_else(|| CAUSE_USAGE.to_string())
        }
    }
}

// `mark` (phase 3A, design §7.4): attributed, rate-limited claims that never
// act.

/// The `mark` verb's usage line.
pub(crate) const MARK_USAGE: &str = "ERR usage: mark <node|-> note|verdict|label|need|clear|act \
     [word=<w>] [text=<pct>] [re=<seq|@off>] [as=<name>] [ttl=<s>] [rule=<id>]\n";

/// The words each kind's `word=` takes (design §7.4's table).
const VERDICT_WORDS: &[&str] = &[
    "on-track",
    "off-track",
    "blocked",
    "done",
    "failed",
    "needs-human",
    "unsure",
];
const NEED_MARK_WORDS: &[&str] = &["review", "decision", "help"];
const ACT_WORDS: &[&str] = &[
    "approve",
    "deny",
    "answer",
    "continue",
    "relaunch",
    "escalate",
    "interrupt",
    "dispatch",
];

/// A `label`'s bounds: what the clause can show beside ` · <age>` (§7.3).
const LABEL_MAX_GRAPHEMES: usize = 32;
const LABEL_MAX_BYTES: usize = 40;
/// A label's longest life, seconds.
const LABEL_TTL_MAX_S: u64 = 3600;

/// A parsed `mark` request: what it asks to write, checked for shape (the
/// session's own facts — the node, `re=` — are checked under its locks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkReq {
    /// The node named (`None`: `-`, the deepest open node).
    node: Option<Node>,
    kind: Kind,
    word: Option<&'static str>,
    text: Option<String>,
    re: Option<CauseRef>,
    as_name: Option<String>,
    ttl: Option<u64>,
    rule: Option<ShortId>,
}

/// Whether `s` is an `as=` name: `[A-Za-z0-9._-]{1,32}`.
fn is_as_name(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A `text=` value through the `meta set` ladder (design §7.4): percent-
/// decoded, trimmed, never empty, never with control, bidi or invisible
/// formatting (refused, not stripped), within the field's 160 encoded bytes
/// (refused, not clamped).
fn mark_text(raw: &str) -> Result<String, String> {
    let decoded = aterm_codec::pct::decode(raw);
    let text = decoded.trim();
    if text.is_empty() {
        return Err("ERR mark: text is empty\n".to_string());
    }
    if crate::session_timeline::metadata_has_forbidden_formatting(text) {
        return Err("ERR mark: text has forbidden formatting\n".to_string());
    }
    if aterm_slog::field::encoded_len(text) > 160 {
        return Err("ERR mark: text over 160 bytes encoded\n".to_string());
    }
    // What the field keeps is what was asked: a character its own filter
    // drops (a hidden one) is refused, never stripped.
    if aterm_slog::BoundedText::new(Key::Text, text).is_none_or(|t| t.as_str() != text) {
        return Err("ERR mark: text has forbidden formatting\n".to_string());
    }
    Ok(text.to_string())
}

impl MarkReq {
    /// Parse `mark`'s arguments (pure): every refusal of shape, before any
    /// lock. A kind's words and options are §7.4's table; an option the kind
    /// does not take, an option given twice or an unknown one is the usage.
    pub(crate) fn parse(rest: &str) -> Result<MarkReq, String> {
        use aterm_grapheme::GraphemeClusters as _;
        let usage = || MARK_USAGE.to_string();
        let mut toks = rest.split_whitespace();
        let node = match toks.next().ok_or_else(usage)? {
            "-" => None,
            n => match Node::parse(n) {
                Some(node)
                    if !matches!(
                        node.lvl,
                        aterm_slog::Lvl::Instance | aterm_slog::Lvl::Session
                    ) =>
                {
                    Some(node)
                }
                _ => return Err(usage()),
            },
        };
        let kind = match toks.next().ok_or_else(usage)? {
            "note" => Kind::Note,
            "verdict" => Kind::Verdict,
            "label" => Kind::Label,
            "need" => Kind::Need,
            "clear" => Kind::Clear,
            "act" => Kind::Act,
            _ => return Err(usage()),
        };
        let mut req = MarkReq {
            node,
            kind,
            word: None,
            text: None,
            re: None,
            as_name: None,
            ttl: None,
            rule: None,
        };
        // What each kind takes: (word list, text, ttl, rule).
        let (words, takes_text, takes_ttl, takes_rule): (
            Option<&[&'static str]>,
            bool,
            bool,
            bool,
        ) = match kind {
            Kind::Note => (None, true, false, false),
            Kind::Verdict => (Some(VERDICT_WORDS), false, false, false),
            Kind::Label => (None, true, true, false),
            Kind::Need | Kind::Clear => (Some(NEED_MARK_WORDS), false, false, false),
            _ => (Some(ACT_WORDS), false, false, true),
        };
        let mut seen: Vec<&str> = Vec::new();
        for tok in toks {
            let (k, v) = tok.split_once('=').ok_or_else(usage)?;
            if seen.contains(&k) || v.is_empty() {
                return Err(usage());
            }
            seen.push(k);
            match k {
                "word" => {
                    let list = words.ok_or_else(usage)?;
                    let w = list.iter().copied().find(|w| *w == v).ok_or_else(|| {
                        format!(
                            "ERR mark: word={v} is not a {} word ({})\n",
                            kind.word(),
                            list.join(" ")
                        )
                    })?;
                    req.word = Some(w);
                }
                "text" if takes_text => req.text = Some(mark_text(v)?),
                "ttl" if takes_ttl => match v.parse::<u64>() {
                    Ok(s) if (1..=LABEL_TTL_MAX_S).contains(&s) => req.ttl = Some(s),
                    _ => return Err("ERR mark: ttl is 1..=3600\n".to_string()),
                },
                "rule" if takes_rule => {
                    req.rule = Some(
                        ShortId::new(v)
                            .filter(|_| !v.contains('@'))
                            .ok_or_else(usage)?,
                    );
                }
                "re" => req.re = Some(CauseRef::parse(v).ok_or_else(usage)?),
                // A chosen name is never an actor word a connection's class
                // names (ruling R1's class): `by=- as=harness` would read as
                // the harness to whoever reads the log.
                "as" if is_as_name(v) && aterm_slog::field::is_reserved_name(v) => {
                    return Err(format!("ERR mark: as={v} is a session log actor\n"));
                }
                "as" if is_as_name(v) => req.as_name = Some(v.to_string()),
                "as" => return Err("ERR mark: as= is [A-Za-z0-9._-]{1,32}\n".to_string()),
                _ => return Err(usage()),
            }
        }
        // What each kind requires.
        let missing = match kind {
            Kind::Note | Kind::Label => req.text.is_none(),
            Kind::Verdict | Kind::Need | Kind::Act => req.word.is_none(),
            _ => false,
        };
        if missing {
            return Err(usage());
        }
        if kind == Kind::Label
            && let Some(t) = &req.text
            && (t.len() > LABEL_MAX_BYTES || t.graphemes().count() > LABEL_MAX_GRAPHEMES)
        {
            return Err("ERR mark: label over 32 graphemes or 40 bytes\n".to_string());
        }
        Ok(req)
    }

    /// The record's fields: `word= text= ttl= rule= re= as=`, existing keys
    /// only (at most six, inside a record's bound).
    fn fields(&self, re: Option<ShortId>) -> Fields {
        let mut f = Fields::new();
        if let Some(w) = self.word {
            let _ = f.set(Key::Word, aterm_slog::Val::Word(w));
        }
        if let Some(t) = self
            .text
            .as_deref()
            .and_then(|t| aterm_slog::BoundedText::new(Key::Text, t))
        {
            let _ = f.set(Key::Text, aterm_slog::Val::Text(t));
        }
        if let Some(s) = self.ttl {
            let _ = f.set(Key::Ttl, aterm_slog::Val::U(s));
        }
        if let Some(r) = self.rule {
            let _ = f.set(Key::Rule, aterm_slog::Val::Id(r));
        }
        if let Some(r) = re {
            let _ = f.set(Key::Re, aterm_slog::Val::Id(r));
        }
        if let Some(a) = self
            .as_name
            .as_deref()
            .and_then(|a| aterm_slog::BoundedText::new(Key::As, a))
        {
            let _ = f.set(Key::As, aterm_slog::Val::Text(a));
        }
        f
    }
}

impl SessionLog {
    /// WRITE A CLAIM (design §7.4): `req` by `by` (the connection's actor),
    /// `re` already checked. Refused when the named node is not open (a mark
    /// never moves to another node in silence) or was never issued, or when
    /// `by`'s bucket is short (`Err` carries `retry_ms`). Appended as `src=mark
    /// conf=claim`, the token spent only then. Returns the seq, the node it
    /// landed on and whether the readers are owed a wake.
    pub(crate) fn mark_claim(
        &mut self,
        req: &MarkReq,
        by: Actor,
        re: Option<ShortId>,
        at: Stamp,
    ) -> Result<(u64, Node, bool), String> {
        if let Some(node) = req.node
            && !self.log.is_open(node)
        {
            return Err(if self.log.issued(node) {
                format!("ERR mark: {node} is not open\n")
            } else {
                format!("ERR mark: no node {node}\n")
            });
        }
        if let Err(retry_ms) = self.consumers.limiter.peek(&by, at.mono_ms) {
            return Err(format!("ERR busy retry_ms={retry_ms}\n"));
        }
        let mut mark = Mark::new(req.kind, Src::Mark, aterm_slog::Conf::Claim)
            .by(by)
            .fields(req.fields(re));
        if let Some(node) = req.node {
            mark = mark.at(node);
        }
        let seq = self
            .log
            .mark(mark, at)
            .map_err(|e| format!("ERR mark: {e:?}\n"))?;
        self.consumers.limiter.spend(&by, at.mono_ms);
        let node = self
            .log
            .window()
            .rev()
            .find(|r| r.seq == seq)
            .map_or(Node::SESSION, |r| r.node);
        Ok((seq, node, self.take_log_wake()))
    }

    /// A SELF-SUPERVISED AGENT'S ACT (plan 5C-2; rulings T21, T24): what a
    /// proven self-claim's holder told with `meta set harness_act <act>`, as
    /// the log's `mark <node> act word=<w> rule=<act> holder=<h>
    /// harness_act=<act> by=<a> src=mark conf=claim` — the word
    /// [`aterm_agent::harness::acts::self_claim_act`] gives, on the deepest
    /// open node (the approval's step while one is open; `-` only when
    /// nothing is), as the loop's own act marks land. `by` is the
    /// connection's actor (`log_actor`), never a parameter of the request;
    /// `holder` the proven claim's holder the server read from the session's
    /// meta (host tier: an id the server sets, never caller text; phase 6,
    /// 6F-7), left out when it is no id; and the act as the holder told it
    /// (`harness_act=`, host tier, the `harness-act` row's whole, which a
    /// wire `rule=` cannot hold). A claim like any mark: the act is the
    /// holder's word, spent from `by`'s mark bucket and held to the log's
    /// mark quota. EVERY ACT IS A RECORD (the review of 6F-7): an act with no
    /// word is `word=unknown`, with no `rule=`, so no `harness-act` row stands
    /// alone. Returns the seq, if one was written.
    pub(crate) fn self_claim_act(
        &mut self,
        act: &str,
        holder: &str,
        by: Actor,
        at: Stamp,
    ) -> Option<u64> {
        self.consumers.limiter.peek(&by, at.mono_ms).ok()?;
        let worded = aterm_agent::harness::acts::self_claim_act(act);
        let word = worded.as_ref().map_or("unknown", |(w, _)| *w);
        let f = Fields::new().word(Key::Word, word).ok()?;
        let f = match &worded {
            Some((_, rule)) => f.clone().id(Key::Rule, rule).unwrap_or(f),
            None => f,
        };
        let f = f.clone().id(Key::Holder, holder).unwrap_or(f);
        let f = f.clone().text(Key::HarnessAct, act).unwrap_or(f);
        let seq = self
            .log
            .mark(
                Mark::new(Kind::Act, Src::Mark, aterm_slog::Conf::Claim)
                    .by(by)
                    .fields(f),
                at,
            )
            .ok()?;
        self.consumers.limiter.spend(&by, at.mono_ms);
        Some(seq)
    }
}

/// THE `mark` VERB (design §7.4): parse, check `re=` against what this session
/// holds (the inbox lock alone, then the timeline's), then write the claim
/// under the timeline's leaf lock alone ([`SessionLog::mark_claim`]). `by` is
/// the connection's actor ([`crate::control`]'s `log_actor`); `reads` whether
/// the connection may read the session. Returns the reply and whether the
/// session's watchers are owed a wake. A mark is a record and nothing else:
/// this path names no input, signal or session write (the census's
/// `slog-inert`).
///
/// WHAT A WRITER WHO MAY NOT READ LEARNS: nothing of the session. Naming a
/// node or a `re=` is reading it — the answer says whether that node is open
/// or that record or mail exists — so both need read, as `cause=` does
/// (`ERR denied`); and its `OK` names neither the seq its claim took (the
/// log's head) nor the node the claim landed on: `OK by=<actor>`.
pub(crate) fn cmd_mark(
    ctx: &crate::SessionCtx,
    by: Actor,
    reads: bool,
    rest: &str,
) -> (String, bool) {
    let req = match MarkReq::parse(rest) {
        Ok(req) => req,
        Err(refusal) => return (refusal, false),
    };
    if !reads && (req.re.is_some() || req.node.is_some()) {
        return ("ERR denied\n".to_string(), false);
    }
    let re = match req.re {
        None => None,
        Some(r) => match validate_cause(ctx, r) {
            Ok(id) => Some(id),
            Err(_) => {
                let what = match r {
                    CauseRef::Mail(off) => format!("@{off} names no mail"),
                    CauseRef::Rec(n) => format!("{n} names no record"),
                };
                return (format!("ERR mark: re={what}\n"), false);
            }
        },
    };
    let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    let at = timeline.slog().now();
    match timeline.slog_mut().mark_claim(&req, by, re, at) {
        Ok((seq, node, owed)) if reads => {
            (format!("OK seq={seq} node={node} by={}\n", by.word()), owed)
        }
        Ok((_, _, owed)) => (format!("OK by={}\n", by.word()), owed),
        Err(refusal) => (refusal, false),
    }
}

// `await log` (phase 3A, design §7.2; ruling R2): park on the session log
// until a need, an open, a close or a kind, never polling and never spinning.

/// The `await log` usage line.
pub(crate) const AWAIT_LOG_USAGE: &str = "ERR usage: await log <wait>[,<wait>...] \
     [since=<seq> ep=<e>] [timeout=<ms>]; <wait> is need|open=<lvl>|close=<lvl>|kind=<k>, at \
     most 4, any one answers; <lvl> is session|run|turn|step\n";

/// The most predicates one `await log` lists (session log 5F-1).
pub(crate) const LOG_WAITS_MAX: usize = 4;

/// What `await log` waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LogWait {
    /// A record that raises a need ([`aterm_slog::need_raised`]): a step's open
    /// (`approval`, `question`, `wall`, `stall`), a run's close `out=fail`,
    /// the session's close, a `need` mark (a claim's shows `conf=claim`).
    Need,
    /// A node of the level opens.
    Open(aterm_slog::Lvl),
    /// A node of the level closes.
    Close(aterm_slog::Lvl),
    /// A record of the kind.
    Kind(Kind),
}

impl LogWait {
    /// The predicate one comma-separated member names, or `None`.
    fn parse(member: &str) -> Option<LogWait> {
        match member.split_once('=') {
            None if member == "need" => Some(LogWait::Need),
            Some(("open", l)) => await_lvl(l).map(LogWait::Open),
            Some(("close", l)) => await_lvl(l).map(LogWait::Close),
            Some(("kind", k)) => match Kind::from_word(k) {
                Kind::Unknown => None,
                kind => Some(LogWait::Kind(kind)),
            },
            _ => None,
        }
    }

    /// Whether `r` satisfies the wait.
    fn matches(self, r: &aterm_slog::Rec) -> bool {
        use aterm_slog::Op;
        match self {
            LogWait::Need => aterm_slog::need_raised(r).is_some(),
            LogWait::Open(l) => r.op == Op::Open && r.node.lvl == l,
            LogWait::Close(l) => r.op == Op::Close && r.node.lvl == l,
            LogWait::Kind(k) => r.k == k,
        }
    }
}

/// What one `await log` waits for (session log 5F-1): one to
/// [`LOG_WAITS_MAX`] distinct predicates, written `need,close=turn`; a record
/// that satisfies ANY of them answers the wait, and the wait is one waiter
/// however many it lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LogWaits {
    list: [LogWait; LOG_WAITS_MAX],
    len: usize,
}

impl LogWaits {
    /// A wait for `w` alone.
    pub(crate) const fn one(w: LogWait) -> LogWaits {
        LogWaits {
            list: [w; LOG_WAITS_MAX],
            len: 1,
        }
    }

    /// The comma-separated list `token` names: each member a predicate, none
    /// twice, at most [`LOG_WAITS_MAX`]; `None` otherwise.
    fn parse(token: &str) -> Option<LogWaits> {
        let mut waits: Option<LogWaits> = None;
        for member in token.split(',') {
            let w = LogWait::parse(member)?;
            match waits.as_mut() {
                None => waits = Some(LogWaits::one(w)),
                Some(ws) if ws.len < LOG_WAITS_MAX && !ws.members().contains(&w) => {
                    ws.list[ws.len] = w;
                    ws.len += 1;
                }
                Some(_) => return None,
            }
        }
        waits
    }

    /// The predicates, in the order written.
    pub(crate) fn members(&self) -> &[LogWait] {
        &self.list[..self.len]
    }

    /// Whether `need` is among them: a since-less wait then latches as `need`
    /// alone does ([`SessionLog::await_start`]).
    pub(crate) fn has_need(&self) -> bool {
        self.members().contains(&LogWait::Need)
    }

    /// Whether `r` satisfies any of them.
    fn matches(&self, r: &aterm_slog::Rec) -> bool {
        self.members().iter().any(|w| w.matches(r))
    }
}

/// A parsed `await log` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LogAwait {
    /// The predicates, any of which answers.
    pub(crate) what: LogWaits,
    /// Where to read from (`since=` and the `ep=` it came with), or `None`:
    /// from what the log holds now.
    pub(crate) since: Option<aterm_slog::Cursor>,
}

fn await_lvl(w: &str) -> Option<aterm_slog::Lvl> {
    use aterm_slog::Lvl;
    match w {
        "session" => Some(Lvl::Session),
        "run" => Some(Lvl::Run),
        "turn" => Some(Lvl::Turn),
        "step" => Some(Lvl::Step),
        _ => None,
    }
}

impl LogAwait {
    /// Parse `await log`'s arguments (the `timeout` already taken off):
    /// exactly one predicate list (`need,close=turn`: up to
    /// [`LOG_WAITS_MAX`] distinct members); `since=` with the `ep=` it came with, always
    /// (as `log` and `subscribe … since-log= log-ep=` take them: a cursor
    /// with no epoch is one no read can place, which would answer a reset gap
    /// that never happened). Anything else is the usage, answered at once and
    /// parking nothing.
    pub(crate) fn parse(args: &[&str]) -> Result<LogAwait, &'static str> {
        let mut what: Option<LogWaits> = None;
        let (mut since, mut ep) = (None, None);
        for tok in args {
            match tok.split_once('=') {
                Some(("since", v)) if since.is_none() => {
                    since = Some(v.parse::<u64>().map_err(|_| AWAIT_LOG_USAGE)?);
                }
                Some(("ep", v)) if ep.is_none() => {
                    ep = Some(v.parse::<u32>().map_err(|_| AWAIT_LOG_USAGE)?);
                }
                Some(("since" | "ep", _)) => return Err(AWAIT_LOG_USAGE),
                _ if what.is_none() => what = Some(LogWaits::parse(tok).ok_or(AWAIT_LOG_USAGE)?),
                _ => return Err(AWAIT_LOG_USAGE),
            }
        }
        let what = what.ok_or(AWAIT_LOG_USAGE)?;
        let since = match (since, ep) {
            (None, None) => None,
            (Some(s), Some(e)) => Some(aterm_slog::Cursor::new(e, s)),
            _ => return Err(AWAIT_LOG_USAGE),
        };
        Ok(LogAwait { what, since })
    }
}

/// The reply a check came to, and the cursor to read on from.
pub(crate) enum LogAwaitStep {
    /// The wait is answered: this line.
    Answer(String),
    /// Nothing yet: read on after this cursor.
    Wait(aterm_slog::Cursor),
}

impl SessionLog {
    /// Where an `await log` with no `since=` starts, and a latched answer:
    /// `need` LATCHES from the fold's open OBSERVED needs as a reader is shown
    /// them (ruling R2: never by replaying the ring, which a gap or a carried
    /// update's hole would answer again and again; a claim's need is left out,
    /// as `status need=` leaves it, or a need nobody can lift would answer
    /// every wait), `OK seq=<raised> ep=<e> as_of=<head> latched=1 <record>`
    /// (the raising record when the ring still holds it, else `need=<w>
    /// node=<n>`); every other wait starts at the reader's head. A list
    /// holding `need` latches as `need` alone does (5F-1).
    pub(crate) fn await_start(&self, what: LogWaits) -> LogAwaitStep {
        let log = &self.log;
        let head = aterm_slog::Cursor::new(log.ep(), log.reader_last_seq());
        if what.has_need()
            && let Some(n) = log
                .reader_state()
                .needs
                .iter()
                .flatten()
                .find(|n| n.src != Src::Mark)
        {
            let tail = log
                .window()
                .rev()
                .find(|r| r.seq == n.since_seq && r.seq <= head.seq)
                .and_then(|r| aterm_slog::encode(r, aterm_slog::Tier::Host).ok())
                .unwrap_or_else(|| format!("need={} node={}", n.word, n.node));
            return LogAwaitStep::Answer(format!(
                "OK seq={} ep={} as_of={} latched=1 {tail}\n",
                n.since_seq, head.ep, head.seq
            ));
        }
        LogAwaitStep::Wait(head)
    }

    /// ONE CHECK of an `await log` from `cursor` (the timeline's leaf lock is
    /// the caller's): nothing new (an O(1) compare of the head), the first
    /// record past it that satisfies `what` (`OK seq=<n> ep=<e>[ gap=<why>:<lost>]
    /// <record>`, the gap the records before it crossed, counted exactly up to
    /// it), a gap with no match among what the read returned (`OK gap why=
    /// lost= resync_to=<next> ep=` — `resync_to` the read's own `next`, so a
    /// wait re-armed there reads past the hole and never meets the same gap,
    /// ruling R2), or the cursor to read on from.
    pub(crate) fn await_check(&self, what: LogWaits, cursor: aterm_slog::Cursor) -> LogAwaitStep {
        let log = &self.log;
        if log.reader_last_seq() == cursor.seq && log.ep() == cursor.ep {
            return LogAwaitStep::Wait(cursor);
        }
        let read = log.read(cursor, aterm_slog::MAX_RECORDS);
        if let Some(at) = read.recs.iter().position(|r| what.matches(r)) {
            // The read up to the match alone: its gap is exactly the loss the
            // records before the match crossed (a loss after it is the next
            // wait's to say).
            let upto = log.read(cursor, at + 1);
            let r = upto.recs[at];
            let gap = upto
                .gap
                .map_or_else(String::new, |g| format!(" gap={}:{}", g.why.word(), g.lost));
            let line = aterm_slog::encode(r, aterm_slog::Tier::Host)
                .unwrap_or_else(|_| format!("{} {} {}", r.seq, r.op.word(), r.node));
            return LogAwaitStep::Answer(format!(
                "OK seq={} ep={}{gap} {line}\n",
                r.seq, upto.next.ep
            ));
        }
        if let Some(g) = read.gap {
            return LogAwaitStep::Answer(format!(
                "OK gap why={} lost={} resync_to={} ep={}\n",
                g.why.word(),
                g.lost,
                read.next.seq,
                read.next.ep
            ));
        }
        LogAwaitStep::Wait(read.next)
    }

    /// ONE LOOK of an `await log` at an ended session (phase 3A review): the
    /// check from `cursor` ([`Self::await_check`]) and whether the close is
    /// still due ([`Self::note_close_due`]), read under one `&self` borrow
    /// and so within one hold of the timeline's lock. The close and the
    /// clearing of the flag happen in one hold ([`Self::close_session`]). So
    /// if a look finds the close not due, the close is in what it read
    /// whenever one was written: it never gets `(Wait, false)` over a close
    /// the log holds. It is one method so that a waiter cannot split the look
    /// across two holds, which would let a retire land between them and the
    /// waiter answer `ERR exited` over its close. Outside tests nothing else
    /// reads the flag.
    pub(crate) fn await_look(
        &self,
        what: LogWaits,
        cursor: aterm_slog::Cursor,
    ) -> (LogAwaitStep, bool) {
        (self.await_check(what, cursor), self.consumers.close_due)
    }
}
