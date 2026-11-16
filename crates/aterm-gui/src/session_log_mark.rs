// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-side consumer marks and rate limiting for the session log.

use super::SessionLog;

impl SessionLog {
    /// THE `status` FIELDS (phase 3A), printed before the `gen=`/`seq=`/
    /// `hash=` stamp: `log_seq=<n> log_ep=<e> log_base=<b> run=<r|-> turn=<t|-> step=<p|->
    /// need=<w|-> need_since_ms=<ms|-> opened_ms=<unix ms|->` (`log_base=` the log's identity,
    /// `Log::base_ep`, which a reader passes back as `base=` beside `since=`/`ep=` so another
    /// log's cursor resets, audit fix C13; `opened_ms=` the lineage's first open,
    /// [`Self::opened_ms`], after the eight words a supervise log wait reads from `log_seq=` on).
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
            "log_seq={} log_ep={} log_base={} run={} turn={} step={} need={} need_since_ms={} \
             opened_ms={}",
            log.reader_last_seq(),
            log.ep(),
            log.base_ep(),
            node(st.run()),
            node(st.turn()),
            node(st.step()),
            need.map_or("-", |n| n.word),
            need.map_or_else(
                || "-".to_string(),
                |n| now_wall.saturating_sub(n.t).to_string()
            ),
            self.opened_ms()
                .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
        )
    }
}

use aterm_slog::{Actor, Conf, Fields, Key, Kind, Mark, Node, ShortId, Src, Stamp};

/// What an acted control input's mark says: its `act=`, and
/// `re=` the step it was admitted under when that step closed before its
/// bytes were written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InputAct {
    pub(crate) act: Option<aterm_slog::ActWord>,
    pub(crate) re: Option<Node>,
}

impl InputAct {
    /// The mark of an input with `act` whose acceptance came to `bound`.
    pub(crate) const fn of_acceptance(
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

const INPUT_BURST_ACTORS: usize = 8;

const fn reserved_burst(by: &Actor) -> Option<usize> {
    match by {
        Actor::Harness => Some(0),
        Actor::Bridge => Some(1),
        Actor::Ctl => Some(2),
        Actor::Human => Some(3),
        Actor::Aterm | Actor::Vendor | Actor::Sid(_) | Actor::Fleet(_) | Actor::Nobody => None,
    }
}

/// THE CONSUMER HALF's state in a session's log (one `SessionLog` field):
/// the mark limiter, the watchers' wake and the control-input bursts.
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
    /// ([`SessionLog::note_close_due`]).
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
    /// this session's watchers are owed a wake.
    pub(crate) fn take_log_wake(&mut self) -> bool {
        let head = (self.log.ep(), self.log.reader_last_seq());
        let owed = head != self.consumers.notified;
        self.consumers.notified = head;
        owed
    }

    /// THE CLOSE IS DUE (`App::exit_session_logical`, told BEFORE the session
    /// reads `Exited`): its program ended and the exit path is running, so
    /// the retire that writes the session's close (`close - spawn
    /// out=shell-exit code=`) may follow.
    pub(crate) const fn note_close_due(&mut self) {
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
    /// the session.
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
    /// written): a person's key the timeline's hold kept from the
    /// log first (T2), then the tracker binds the input from `by` with its
    /// `act=` to the approval or question step it was admitted under, or
    /// led to, with what its key (`key`: its class and the frame it was
    /// admitted on, [`Self::key_frame`]) chose there (fix E8).
    pub(crate) fn ctl_accepted(
        &mut self,
        by: Actor,
        act: Option<aterm_slog::ActWord>,
        adm: Option<aterm_slog::Admission>,
        key: (super::KeyClass, super::KeyFrame),
        at: Stamp,
    ) -> aterm_slog::Binding {
        self.reconcile_person(at);
        self.resync();
        let (class, frame) = key;
        let chose = frame.chose(class, &self.keys);
        self.keys.reached();
        let Self { log, agent, .. } = self;
        match (agent.as_mut(), adm) {
            (Some(a), Some(adm)) => a.tracker.accept(log, by, act, adm.choosing(chose), at),
            _ => aterm_slog::Binding::Unbound,
        }
    }

    /// THE ADMISSION of a control input about to be written:
    /// what the agent's tracker holds now, read under the timeline lock the
    /// writer takes before a byte.
    pub(crate) fn admit_input(&self, at: Stamp) -> Option<aterm_slog::Admission> {
        self.agent.as_ref().map(|a| a.tracker.admit(at))
    }

    /// The input mark alone, for an input the tracker was already told of (a
    /// `ctl turn`'s first byte, [`SessionLog::ctl_turn`]): written at the first
    /// input of a burst from `by`.
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

    fn names_mail(&self, off: u64) -> bool {
        self.log
            .window()
            .any(|r| r.k == Kind::Mail && r.f.get_u(Key::Off) == Some(off))
    }

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

/// THE CAUSE CHECK: `cause=` names only what THIS
/// session holds — mail it was delivered or a post of its own landed at
/// ([`crate::fabric::SessionFabric::holds_offset`]) or a `mail` record of its
/// log names, or a record seq inside its log's current seq space outside
/// every hole.
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

/// The `mark` verb's usage line.
pub(crate) const MARK_USAGE: &str = "ERR usage: mark <node|-> note|verdict|label|need|clear|act \
     [word=<w>] [text=<pct>] [re=<seq|@off>] [as=<name>] [ttl=<s>] [rule=<id>]; \
     mark <-|r<n>> desc field=<slot> [value=<pct>] [upto=<seq>] [ttl=<s>] [as=<name>] \
     [re=<seq|@off>]\n";

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

const LABEL_MAX_GRAPHEMES: usize = 32;
const LABEL_MAX_BYTES: usize = 40;
const LABEL_TTL_MAX_S: u64 = 3600;

/// The most percent-encoded bytes a description claim's text holds ([`Key::Value`]'s cap): a
/// longer one is refused, never cut.
pub(crate) const DESC_VALUE_MAX: usize = 256;
/// The longest a description claim may ask to stand: a day.
const DESC_TTL_MAX_S: u64 = 86_400;
/// How many more times a redacted, capped description is redacted and capped again before it
/// must have stopped changing ([`redacted_to_cap`]).
const DESC_SETTLE: usize = 4;

/// What a description claim describes, as its writer names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DescOn {
    /// The session's open agent run (the newest still open), else the session: `mark - desc`.
    Front,
    /// The session itself, whatever runs in it: a tab's title, which no agent's exit lapses.
    Session,
    /// One run, which must be an open agent run: `mark r<n> desc`.
    Agent(Node),
}

/// One description claim to write ([`SessionLog::claim_desc`]): `value` is the text as its
/// writer gave it (already percent-decoded), and `None` withdraws the author's own claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DescWrite<'a> {
    pub(crate) on: DescOn,
    pub(crate) slot: &'a str,
    pub(crate) value: Option<&'a str>,
    pub(crate) upto: Option<u64>,
    pub(crate) ttl: Option<u64>,
    pub(crate) as_name: Option<&'a str>,
    pub(crate) re: Option<ShortId>,
}

impl<'a> DescWrite<'a> {
    /// A claim of the session's own `slot` with nothing but its text (`None` withdraws).
    pub(crate) const fn of_session(slot: &'a str, value: Option<&'a str>) -> DescWrite<'a> {
        DescWrite {
            on: DescOn::Session,
            slot,
            value,
            upto: None,
            ttl: None,
            as_name: None,
            re: None,
        }
    }
}

/// Why a description claim was not written ([`SessionLog::claim_desc`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DescRefused {
    /// The slot is not a canonical [`Key::Field`] token.
    Slot,
    /// The text is empty once trimmed (a withdrawal is the absence of a text).
    Empty,
    /// The text is not one line of visible characters.
    Formatting,
    /// The text is over [`DESC_VALUE_MAX`] bytes percent-encoded.
    TooLong,
    /// Nothing but redaction notices is left of the text once redacted, or the text never
    /// stopped changing under the redactor.
    Credential,
    /// No such node was ever issued.
    NoNode(Node),
    /// The node is closed.
    NotOpen(Node),
    /// The node is open and is no agent run.
    NotAgent(Node),
    /// `upto=` names a seq past the record the claim would become.
    UptoAhead(u64),
    /// The author's bucket is short of a token for this many ms.
    Busy(u64),
    /// The log itself refused the record (a closed session's).
    Log(aterm_slog::Refused),
}

impl DescRefused {
    /// The `mark` verb's reply for this refusal.
    pub(crate) fn mark_reply(self) -> String {
        match self {
            DescRefused::Slot => {
                "ERR mark: field= is a slot name of lower-case [a-z0-9.-], at most 24\n".to_string()
            }
            DescRefused::Empty => "ERR mark: value is empty\n".to_string(),
            DescRefused::Formatting => "ERR mark: value has forbidden formatting\n".to_string(),
            DescRefused::TooLong => {
                format!("ERR mark: value over {DESC_VALUE_MAX} bytes encoded\n")
            }
            DescRefused::Credential => "ERR mark: value reads as a credential\n".to_string(),
            DescRefused::NoNode(node) => format!("ERR mark: no node {node}\n"),
            DescRefused::NotOpen(node) => format!("ERR mark: {node} is not open\n"),
            DescRefused::NotAgent(node) => format!("ERR mark: {node} is not an agent run\n"),
            DescRefused::UptoAhead(upto) => {
                format!("ERR mark: upto={upto} is ahead of the log\n")
            }
            DescRefused::Busy(retry_ms) => format!("ERR busy retry_ms={retry_ms}\n"),
            DescRefused::Log(e) => format!("ERR mark: {e:?}\n"),
        }
    }
}

/// A description's text as its writer gave it, checked for shape before anything judges what
/// it says: trimmed, not empty, one line with no control, bidi or invisible formatting
/// character, and at most [`DESC_VALUE_MAX`] bytes percent-encoded.
fn desc_text(raw: &str) -> Result<&str, DescRefused> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(DescRefused::Empty);
    }
    if crate::session_timeline::metadata_has_forbidden_formatting(text) {
        return Err(DescRefused::Formatting);
    }
    if aterm_slog::field::encoded_len(text) > DESC_VALUE_MAX {
        return Err(DescRefused::TooLong);
    }
    Ok(text)
}

/// `text` with its credentials redacted ([`crate::survey::redact`]) and cut at a character
/// boundary to [`DESC_VALUE_MAX`] encoded bytes, then redacted and cut again until nothing
/// changes: a redaction can lengthen a line, and a cut inside a word can leave one the
/// redactor judges anew. Refused where nothing but redaction notices is left
/// ([`crate::survey::only_redaction`]: the whole-line notice, or word notices alone, which
/// say nothing about the session and would stand over a useful claim of a lower rank), where
/// nothing is left at all, and where the text does not settle.
fn redacted_to_cap(text: &str) -> Result<String, DescRefused> {
    let judged = |t: &str| {
        let redacted = crate::survey::redact(t);
        aterm_slog::field::clamp_encoded(&redacted, DESC_VALUE_MAX)
            .trim()
            .to_string()
    };
    let mut line = judged(text);
    let mut settled = false;
    for _ in 0..DESC_SETTLE {
        let again = judged(&line);
        if again == line {
            settled = true;
            break;
        }
        line = again;
    }
    if settled && !line.is_empty() && !crate::survey::only_redaction(&line) {
        Ok(line)
    } else {
        Err(DescRefused::Credential)
    }
}

/// THE TEXT A DESCRIPTION CLAIM BY `by` HOLDS for the text `raw` its writer gave: a person's
/// as typed, every other author's redacted ([`redacted_to_cap`]), each checked for shape
/// ([`desc_text`]) and held to what [`Key::Value`] stores whole. Pure, so a writer that must
/// know whether a claim would repeat one already held can ask before it writes.
pub(crate) fn desc_stored(raw: &str, by: Actor) -> Result<String, DescRefused> {
    let text = desc_text(raw)?;
    let text = if by == Actor::Human {
        text.to_string()
    } else {
        redacted_to_cap(text)?
    };
    if aterm_slog::BoundedText::new(Key::Value, &text).is_none_or(|t| t.as_str() != text) {
        return Err(DescRefused::Formatting);
    }
    Ok(text)
}

/// A parsed `mark` request: what it asks to write, checked for shape (the
/// session's own facts — the node, `re=` — are checked under its locks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkReq {
    /// The node named (`None`: `-`, the deepest open node; for a `desc`, the open agent run,
    /// else the session).
    node: Option<Node>,
    kind: Kind,
    word: Option<&'static str>,
    text: Option<String>,
    re: Option<CauseRef>,
    as_name: Option<String>,
    ttl: Option<u64>,
    rule: Option<ShortId>,
    /// A `desc`'s slot (`field=`).
    slot: Option<String>,
    /// A `desc`'s text (`value=`, decoded); with none the `desc` withdraws.
    value: Option<String>,
    /// The seq a `desc`'s writer had read the log up to (`upto=`).
    upto: Option<u64>,
}

fn is_as_name(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

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
    if aterm_slog::BoundedText::new(Key::Text, text).is_none_or(|t| t.as_str() != text) {
        return Err("ERR mark: text has forbidden formatting\n".to_string());
    }
    Ok(text.to_string())
}

impl MarkReq {
    /// Parse `mark`'s arguments (pure): every refusal of shape, before any
    /// lock.
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
            "desc" => Kind::Desc,
            _ => return Err(usage()),
        };
        if kind == Kind::Desc && node.is_some_and(|n| n.lvl != aterm_slog::Lvl::Run) {
            return Err(
                "ERR mark: a desc is the session's (-) or an open agent run's (r<n>)\n".to_string(),
            );
        }
        let mut req = MarkReq {
            node,
            kind,
            word: None,
            text: None,
            re: None,
            as_name: None,
            ttl: None,
            rule: None,
            slot: None,
            value: None,
            upto: None,
        };
        let desc = kind == Kind::Desc;
        let (words, takes_text, ttl_max, takes_rule): (
            Option<&[&'static str]>,
            bool,
            Option<u64>,
            bool,
        ) = match kind {
            Kind::Note => (None, true, None, false),
            Kind::Verdict => (Some(VERDICT_WORDS), false, None, false),
            Kind::Label => (None, true, Some(LABEL_TTL_MAX_S), false),
            Kind::Need | Kind::Clear => (Some(NEED_MARK_WORDS), false, None, false),
            Kind::Desc => (None, false, Some(DESC_TTL_MAX_S), false),
            _ => (Some(ACT_WORDS), false, None, true),
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
                "ttl" if ttl_max.is_some() => {
                    let max = ttl_max.unwrap_or(0);
                    match v
                        .bytes()
                        .all(|b| b.is_ascii_digit())
                        .then(|| v.parse::<u64>().ok())
                        .flatten()
                    {
                        Some(s) if (1..=max).contains(&s) => req.ttl = Some(s),
                        _ => return Err(format!("ERR mark: ttl is 1..={max}\n")),
                    }
                }
                "field" if desc => {
                    if aterm_slog::BoundedToken::new(Key::Field, v).is_none_or(|t| t.as_str() != v)
                    {
                        return Err(DescRefused::Slot.mark_reply());
                    }
                    req.slot = Some(v.to_string());
                }
                "value" if desc => {
                    let decoded = aterm_codec::pct::decode(v);
                    let text = desc_text(&decoded).map_err(DescRefused::mark_reply)?;
                    req.value = Some(text.to_string());
                }
                "upto" if desc => {
                    req.upto = Some(
                        v.bytes()
                            .all(|b| b.is_ascii_digit())
                            .then(|| v.parse::<u64>().ok())
                            .flatten()
                            .ok_or_else(usage)?,
                    );
                }
                "rule" if takes_rule => {
                    req.rule = Some(
                        ShortId::new(v)
                            .filter(|_| !v.contains('@'))
                            .ok_or_else(usage)?,
                    );
                }
                "re" => req.re = Some(CauseRef::parse(v).ok_or_else(usage)?),
                "as" if is_as_name(v) && aterm_slog::field::is_reserved_name(v) => {
                    return Err(format!("ERR mark: as={v} is a session log actor\n"));
                }
                "as" if is_as_name(v) => req.as_name = Some(v.to_string()),
                "as" => return Err("ERR mark: as= is [A-Za-z0-9._-]{1,32}\n".to_string()),
                _ => return Err(usage()),
            }
        }
        let missing = match kind {
            Kind::Note | Kind::Label => req.text.is_none(),
            Kind::Verdict | Kind::Need | Kind::Act => req.word.is_none(),
            Kind::Desc => req.slot.is_none(),
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
    /// THE RECORD STREAM'S ONE WRITE ARM: a mark a harness record stands for, at `node` (the
    /// session with none). Every one is stamped here with all four of `holder=<the claim's
    /// holder>`, `src=harness`, `conf=claim` and `by=<the tab's own session>`, so no writer of
    /// a stream row can leave one out; a holder that is no ID writes nothing. It neither reads
    /// nor spends the mark limiter: the stream's own bucket (`control_stream.rs`) is the only
    /// budget a record has, and a session's `mark` budget is not the harness's to use up.
    pub(crate) fn stream_mark(
        &mut self,
        kind: Kind,
        node: Option<Node>,
        fields: Fields,
        holder: &str,
        by: Actor,
        at: Stamp,
    ) -> Option<u64> {
        let fields = fields.id(Key::Holder, holder).ok()?;
        let mut mark = Mark::new(kind, Src::Harness, aterm_slog::Conf::Claim)
            .by(by)
            .fields(fields);
        if let Some(node) = node {
            mark = mark.at(node);
        }
        self.log.mark(mark, at).ok()
    }

    /// WRITE A CLAIM (design §7.4): `req` by `by` (the connection's actor),
    /// `re` already checked. A `desc` is written by [`Self::claim_desc`].
    pub(crate) fn mark_claim(
        &mut self,
        req: &MarkReq,
        by: Actor,
        re: Option<ShortId>,
        at: Stamp,
    ) -> Result<(u64, Node, bool), String> {
        if req.kind == Kind::Desc {
            let write = DescWrite {
                on: req.node.map_or(DescOn::Front, DescOn::Agent),
                slot: req.slot.as_deref().unwrap_or(""),
                value: req.value.as_deref(),
                upto: req.upto,
                ttl: req.ttl,
                as_name: req.as_name.as_deref(),
                re,
            };
            let (seq, node) = self
                .claim_desc(&write, by, at)
                .map_err(DescRefused::mark_reply)?;
            return Ok((seq, node, self.take_log_wake()));
        }
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

    /// WRITE A DESCRIPTION CLAIM: the one place a `desc` record is appended, for the `mark`
    /// verb and for a title write alike, so no claim reaches the log (and from it the journal
    /// and the `subscribe` log stream) without these checks. The slot is a canonical token and
    /// is never rewritten. The text of every author but a person is redacted and held to
    /// [`DESC_VALUE_MAX`] encoded bytes before the record is built ([`desc_stored`]); a
    /// person's is stored as typed. The claim lands on the session, on its open agent run, or
    /// on a named open agent run ([`DescOn`]). `upto` may not be past the record itself. Every
    /// author but a person spends from the mark limiter. Returns the record's seq and node.
    pub(crate) fn claim_desc(
        &mut self,
        write: &DescWrite<'_>,
        by: Actor,
        at: Stamp,
    ) -> Result<(u64, Node), DescRefused> {
        let slot = aterm_slog::BoundedToken::new(Key::Field, write.slot)
            .filter(|t| t.as_str() == write.slot)
            .ok_or(DescRefused::Slot)?;
        let value = write.value.map(|v| desc_stored(v, by)).transpose()?;
        let node = match write.on {
            DescOn::Session => Node::SESSION,
            DescOn::Front => self
                .log
                .state()
                .desc
                .open_agent()
                .filter(|n| self.log.is_open(*n))
                .unwrap_or(Node::SESSION),
            DescOn::Agent(node) => match self.log.open_entry(node) {
                Some(e) if e.kind == Kind::Agent => node,
                Some(_) => return Err(DescRefused::NotAgent(node)),
                None if self.log.issued(node) => return Err(DescRefused::NotOpen(node)),
                None => return Err(DescRefused::NoNode(node)),
            },
        };
        if let Some(upto) = write.upto
            && upto > self.log.last_seq().saturating_add(1)
        {
            return Err(DescRefused::UptoAhead(upto));
        }
        let limited = by != Actor::Human;
        if limited && let Err(retry_ms) = self.consumers.limiter.peek(&by, at.mono_ms) {
            return Err(DescRefused::Busy(retry_ms));
        }
        let mut f = Fields::new();
        let _ = f.set(Key::Field, aterm_slog::Val::Tok(slot));
        if let Some(t) = value
            .as_deref()
            .and_then(|v| aterm_slog::BoundedText::new(Key::Value, v))
        {
            let _ = f.set(Key::Value, aterm_slog::Val::Text(t));
        }
        if let Some(upto) = write.upto {
            let _ = f.set(Key::Upto, aterm_slog::Val::U(upto));
        }
        if let Some(s) = write.ttl {
            let _ = f.set(Key::Ttl, aterm_slog::Val::U(s));
        }
        if let Some(a) = write
            .as_name
            .and_then(|a| aterm_slog::BoundedText::new(Key::As, a))
        {
            let _ = f.set(Key::As, aterm_slog::Val::Text(a));
        }
        if let Some(r) = write.re {
            let _ = f.set(Key::Re, aterm_slog::Val::Id(r));
        }
        let mark = Mark::new(Kind::Desc, Src::Mark, Conf::Claim)
            .at(node)
            .by(by)
            .fields(f);
        let seq = self.log.mark(mark, at).map_err(|e| match e {
            aterm_slog::Refused::UptoAhead(upto) => DescRefused::UptoAhead(upto),
            e => DescRefused::Log(e),
        })?;
        if limited {
            self.consumers.limiter.spend(&by, at.mono_ms);
        }
        Ok((seq, node))
    }

    /// A SELF-SUPERVISED AGENT'S ACT: what a
    /// proven self-claim's holder told with `meta set harness_act <act>`, as
    /// the log's `mark <node> act word=<w> rule=<act> holder=<h>
    /// harness_act=<act> by=<a> src=mark conf=claim` — the word
    /// [`aterm_agent::harness::acts::self_claim_act`] gives, on the open
    /// approval or question step while one is open (even with a tool open
    /// beside it), else on the deepest open node (`-` only when nothing is),
    /// as the loop's own act marks land ([`aterm_slog::Log::mark`], fix E3).
    /// It never lands on a codex run's claimed box step (GAP 26,
    /// [`Self::codex_claim_step`]), nor closes it: until an act names the box
    /// it answered, an act (a pre-approval among them) cannot say it answered
    /// that box, so the mark lands where it would with no such step standing
    /// ([`Self::act_place_beside`]).
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
        let mut mark = Mark::new(Kind::Act, Src::Mark, aterm_slog::Conf::Claim)
            .by(by)
            .fields(f);
        if let Some(node) = self
            .codex_claim_step()
            .and_then(|c| self.act_place_beside(c))
        {
            mark = mark.at(node);
        }
        let seq = self.log.mark(mark, at).ok()?;
        self.consumers.limiter.spend(&by, at.mono_ms);
        Some(seq)
    }

    /// Where [`aterm_slog::Log::mark`] places a node-less act with the open
    /// node `beside` set aside: the newest other open approval or question
    /// step, else the newest other open node.
    fn act_place_beside(&self, beside: Node) -> Option<Node> {
        let open = self.log.open_nodes();
        let others = || open.iter().rev().filter(|e| e.node != beside);
        others()
            .find(|e| aterm_slog::is_decision_step(e.kind))
            .or_else(|| others().next())
            .map(|e| e.node)
    }
}

/// THE `mark` VERB (design §7.4): parse, check `re=` against what this session
/// holds (the inbox lock alone, then the timeline's), then write the claim
/// under the timeline's leaf lock alone ([`SessionLog::mark_claim`]). A writer
/// that may not read the log names no node, no `re=` and no `upto=`: each
/// refusal would tell it a position in a log it may not see. It never takes
/// the session's metadata lock, so a `desc field=title` claim does not move
/// the tab's pinned title.
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
    if !reads && (req.re.is_some() || req.node.is_some() || req.upto.is_some()) {
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

/// The `await log` usage line.
pub(crate) const AWAIT_LOG_USAGE: &str = "ERR usage: await log <wait>[,<wait>...] \
     [since=<seq> ep=<e> [base=<log_base>]] [timeout=<ms>] [autopilot=<cruise|land|off|->]; <wait> is \
     need|open=<lvl>|close=<lvl>|kind=<k>, at most 4, any one answers; <lvl> is session|run|turn|step\n";

/// The most predicates one `await log` lists.
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

/// What one `await log` waits for: one to
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
    /// that never happened), and `base=` (`status log_base=`) only beside them: the log the
    /// cursor is of, so a cursor of another log answers `OK gap why=reset`.
    pub(crate) fn parse(args: &[&str]) -> Result<LogAwait, &'static str> {
        let mut what: Option<LogWaits> = None;
        let (mut since, mut ep, mut base) = (None, None, None);
        let digits = |v: &str| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit());
        for tok in args {
            match tok.split_once('=') {
                Some(("since", v)) if since.is_none() && digits(v) => {
                    since = Some(v.parse::<u64>().map_err(|_| AWAIT_LOG_USAGE)?);
                }
                Some(("ep", v)) if ep.is_none() && digits(v) => {
                    ep = Some(v.parse::<u32>().map_err(|_| AWAIT_LOG_USAGE)?);
                }
                Some(("base", v)) if base.is_none() && digits(v) => {
                    base = Some(v.parse::<u32>().map_err(|_| AWAIT_LOG_USAGE)?);
                }
                Some(("since" | "ep" | "base", _)) => return Err(AWAIT_LOG_USAGE),
                _ if what.is_none() => what = Some(LogWaits::parse(tok).ok_or(AWAIT_LOG_USAGE)?),
                _ => return Err(AWAIT_LOG_USAGE),
            }
        }
        let what = what.ok_or(AWAIT_LOG_USAGE)?;
        let since = match (since, ep, base) {
            (None, None, None) => None,
            (Some(s), Some(e), None) => Some(aterm_slog::Cursor::new(e, s)),
            (Some(s), Some(e), Some(b)) => Some(aterm_slog::Cursor::new(e, s).of_log(b)),
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
    /// them (never by replaying the ring, which a gap or a carried
    /// update's hole would answer again and again; a claim's need is left out,
    /// as `status need=` leaves it, or a need nobody can lift would answer
    /// every wait), `OK seq=<raised> ep=<e> as_of=<head> latched=1 <record>`
    /// (the raising record when the ring still holds it, else `need=<w>
    /// node=<n>`); every other wait starts at the reader's head.
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
    /// wait re-armed there reads past the hole and never meets the same gap),
    /// or the cursor to read on from. A cursor that names its log (`base=`)
    /// is answered with this log's `base=` after `ep=` and read on as this
    /// log's, so a wait re-armed from a reset's reply reads on rather than
    /// resetting again (audit fix C13); one that names none is answered as
    /// before.
    pub(crate) fn await_check(&self, what: LogWaits, cursor: aterm_slog::Cursor) -> LogAwaitStep {
        let log = &self.log;
        if log.reader_last_seq() == cursor.seq
            && log.ep() == cursor.ep
            && !cursor.names_another_log(log.base_ep())
        {
            return LogAwaitStep::Wait(cursor);
        }
        let named = cursor
            .base
            .map_or_else(String::new, |_| format!(" base={}", log.base_ep()));
        let read = log.read(cursor, aterm_slog::MAX_RECORDS);
        if let Some(at) = read.recs.iter().position(|r| what.matches(r)) {
            let upto = log.read(cursor, at + 1);
            let r = upto.recs[at];
            let gap = upto
                .gap
                .map_or_else(String::new, |g| format!(" gap={}:{}", g.why.word(), g.lost));
            let line = aterm_slog::encode(r, aterm_slog::Tier::Host)
                .unwrap_or_else(|_| format!("{} {} {}", r.seq, r.op.word(), r.node));
            return LogAwaitStep::Answer(format!(
                "OK seq={} ep={}{named}{gap} {line}\n",
                r.seq, upto.next.ep
            ));
        }
        if let Some(g) = read.gap {
            return LogAwaitStep::Answer(format!(
                "OK gap why={} lost={} resync_to={} ep={}{named}\n",
                g.why.word(),
                g.lost,
                read.next.seq,
                read.next.ep
            ));
        }
        LogAwaitStep::Wait(match cursor.base {
            Some(_) => read.next.of_log(log.base_ep()),
            None => read.next,
        })
    }

    /// ONE LOOK of an `await log` at an ended session (phase 3A review): the
    /// check from `cursor` ([`Self::await_check`]) and whether the close is
    /// still due ([`Self::note_close_due`]), read under one `&self` borrow
    /// and so within one hold of the timeline's lock.
    pub(crate) fn await_look(
        &self,
        what: LogWaits,
        cursor: aterm_slog::Cursor,
    ) -> (LogAwaitStep, bool) {
        (self.await_check(what, cursor), self.consumers.close_due)
    }
}
