// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fold (design §7.1): one pure reduction of a session's records to its
//! current state, the same in the server, in a supervisor and over the bus.
//!
//! - [`Fold::apply`] reads no clock and is idempotent by seq: a record whose
//!   seq was already applied changes nothing. A `gap why=handoff epoch=<e>`
//!   of an epoch newer than the fold's starts that epoch, whose seqs
//!   restart; the same gap seen again is a seq already applied.
//! - Time enters only when the state is read: [`Fold::clause`] and
//!   [`Fold::hints`] take `now_wall`.
//! - **Needs** open when their step opens (`approval`, `question`, `wall`,
//!   `stall` as `unresponsive`), when a run fails (`failure`), when the
//!   session closes (`exited`), and on a `need` mark (`escalated`, `review`,
//!   `decision`, `help`). They close when the step closes, on `seen` (a
//!   failure), or on a `clear` from whoever raised them or from the owner;
//!   a run's failure also closes when a later run of its kind ends `ok` or
//!   `fail`, the newest command's outcome being the one that counts
//!   ([`lifts_failure`]).
//!   A `need` mark raises only `review`, `decision` or `help` (`escalated`
//!   only from `src=meta`, the owner's attention flag): a claim never
//!   raises an observed need's word.
//!   Precedence: approval > question > wall > unresponsive > failure >
//!   escalated > exited > raised by a mark ([`need_precedence`]).
//! - **Silence never raises a need**: [`Hints::quiet_for_ms`] is a hint.
//! - **The confidence law** (§3.7): for each (node, key) the value of the
//!   highest confidence wins, ties to the later seq; a claim never overrides
//!   an observation.

use crate::field::{Key, Val};
use crate::rec::Rec;
use crate::vocab::{Actor, CLS_WORDS, Conf, Kind, Lvl, Node, Op, Src};
use crate::words::{BoxWord, WallKind, need_rank};

/// The most needs the state holds; a less urgent one gives way.
pub const MAX_NEEDS: usize = 8;
/// The most open nodes the fold tracks.
const MAX_TRACKED: usize = 64;
/// A label's default lifetime (§7.4): 600 s.
pub const LABEL_TTL_MS: u64 = 600_000;
/// A label's longest lifetime: 3600 s.
pub const LABEL_TTL_MAX_MS: u64 = 3_600_000;
/// The clause's byte budget (§7.3).
pub const CLAUSE_BYTES: usize = 48;

/// One field of a node as the fold holds it: the value, and the confidence
/// and seq that put it there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// The key.
    pub key: Key,
    /// The value.
    pub val: Val,
    /// The confidence of the record that set it.
    pub conf: Conf,
    /// The seq of that record.
    pub seq: u64,
}

/// A node's fields under the confidence law.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeFields(Vec<Held>);

impl NodeFields {
    /// Offer `val` for `key` at `conf` from record `seq`: kept when no value
    /// is held, or the held one ranks lower, or ranks the same from an
    /// earlier seq ([`confidence_law`]).
    pub fn offer(&mut self, key: Key, val: Val, conf: Conf, seq: u64) {
        if let Some(h) = self.0.iter_mut().find(|h| h.key == key) {
            if confidence_law(h.conf, h.seq, conf, seq) {
                *h = Held {
                    key,
                    val,
                    conf,
                    seq,
                };
            }
            return;
        }
        if self.0.len() < crate::field::MAX_FIELDS * 2 {
            self.0.push(Held {
                key,
                val,
                conf,
                seq,
            });
        }
    }

    /// Forget `key`.
    pub fn remove(&mut self, key: Key) {
        self.0.retain(|h| h.key != key);
    }

    /// The held fields, in the order they were first offered.
    pub fn iter(&self) -> impl Iterator<Item = &Held> {
        self.0.iter()
    }

    /// The held value of `key`.
    #[must_use]
    pub fn get(&self, key: Key) -> Option<&Val> {
        self.0.iter().find(|h| h.key == key).map(|h| &h.val)
    }

    /// The held entry of `key`.
    #[must_use]
    pub fn held(&self, key: Key) -> Option<&Held> {
        self.0.iter().find(|h| h.key == key)
    }

    /// The held word of `key`.
    #[must_use]
    pub fn word(&self, key: Key) -> Option<&'static str> {
        match self.get(key) {
            Some(Val::Word(w)) => Some(w),
            _ => None,
        }
    }

    /// The held text, token or id of `key`.
    #[must_use]
    pub fn str(&self, key: Key) -> Option<&str> {
        match self.get(key) {
            Some(Val::Tok(t)) => Some(t.as_str()),
            Some(Val::Text(t)) => Some(t.as_str()),
            Some(Val::Id(id)) => Some(id.as_str()),
            Some(Val::Word(w)) => Some(w),
            _ => None,
        }
    }

    /// The held unsigned number of `key`.
    #[must_use]
    pub fn u(&self, key: Key) -> Option<u64> {
        match self.get(key) {
            Some(Val::U(n)) => Some(*n),
            _ => None,
        }
    }

    /// The held signed number of `key`.
    #[must_use]
    pub fn i(&self, key: Key) -> Option<i64> {
        match self.get(key) {
            Some(Val::I(n)) => Some(*n),
            _ => None,
        }
    }
}

/// THE confidence law (§3.7): does an offered value (`conf`, `seq`) replace
/// the held one (`held_conf`, `held_seq`)? Yes when it ranks higher, or
/// ranks the same and is later. A claim ranks below every observation, so it
/// never overrides one.
#[must_use]
pub const fn confidence_law(held_conf: Conf, held_seq: u64, conf: Conf, seq: u64) -> bool {
    conf.rank() > held_conf.rank() || (conf.rank() == held_conf.rank() && seq > held_seq)
}

/// An open node in the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenNode {
    /// The node.
    pub node: Node,
    /// Its kind.
    pub kind: Kind,
    /// Its parent.
    pub par: Option<Node>,
    /// When it opened (wall ms).
    pub t: u64,
    /// Who opened it.
    pub by: Actor,
    /// Where observed.
    pub src: Src,
    /// How sure.
    pub conf: Conf,
    /// Its fields under the confidence law.
    pub f: NodeFields,
}

/// A closed node in the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedNode {
    /// The node.
    pub node: Node,
    /// Its kind.
    pub kind: Kind,
    /// The `out=` word.
    pub out: &'static str,
    /// Its duration.
    pub dur_ms: u64,
    /// When it opened (wall ms), when known.
    pub opened_t: Option<u64>,
    /// When it closed (wall ms).
    pub t: u64,
    /// Who closed it.
    pub by: Actor,
    /// Who opened it, when known.
    pub opened_by: Actor,
    /// Where the close was observed.
    pub src: Src,
    /// How sure.
    pub conf: Conf,
    /// Its open and close fields under the confidence law.
    pub f: NodeFields,
}

/// Something the session needs from someone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Need {
    /// The need word (one of [`crate::vocab::NEED_WORDS`]).
    pub word: &'static str,
    /// The node that raised it (a step, a run, the session, a mark's node).
    pub node: Node,
    /// The kind of the record that raised it: its node's (`cmd` for a shell
    /// command's failure, `approval` for a box), or `need` for a mark's.
    pub kind: Kind,
    /// The seq that raised it.
    pub since_seq: u64,
    /// When (wall ms).
    pub t: u64,
    /// Where observed.
    pub src: Src,
    /// How sure.
    pub conf: Conf,
    /// Who raised it.
    pub by: Actor,
    /// The wall's or failure's detail: the reset (`3pm`) or the exit code.
    pub detail: Option<String>,
}

/// A consumer's claim (`verdict`, `label`): attributed, never an
/// observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The claim's word (`on-track`), for a verdict.
    pub word: Option<&'static str>,
    /// The claim's text, for a label.
    pub text: Option<String>,
    /// Its author.
    pub by: Actor,
    /// When (wall ms).
    pub t: u64,
    /// When it expires (wall ms), for a label.
    pub until: Option<u64>,
    /// The seq that set it.
    pub seq: u64,
}

/// What the session is doing, by its newest verdict: the agent word, else
/// the shell phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Phase {
    /// The word (`busy`, `idle`, `running`, `-`).
    pub word: &'static str,
    /// Where observed.
    pub src: Src,
    /// How sure.
    pub conf: Conf,
    /// When (wall ms).
    pub t: u64,
}

impl Default for Phase {
    fn default() -> Phase {
        Phase {
            word: "-",
            src: Src::Unknown,
            conf: Conf::Unknown,
            t: 0,
        }
    }
}

/// Counters over the whole session, evicted records included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Turns opened.
    pub turns: u32,
    /// Turns closed `done`.
    pub done: u32,
    /// Turns closed `asked`.
    pub asked: u32,
    /// Turns closed on a wall.
    pub walled: u32,
    /// Turns closed `interrupted`.
    pub interrupted: u32,
    /// Turns closed `error`.
    pub errors: u32,
    /// Turns closed `gone`, `superseded`, `retracted` or `unknown`.
    pub other: u32,
    /// Steps opened.
    pub steps: u32,
    /// Approval steps.
    pub appr: u32,
    /// Approvals closed, by who closed them: human, harness, ctl, other.
    pub appr_by: [u32; 4],
    /// Question steps.
    pub qs: u32,
    /// Input stalls.
    pub stalls: u32,
    /// Tool steps by class ([`CLS_WORDS`] order).
    pub tools: [u32; 10],
    /// Mail delivered.
    pub mail_in: u32,
    /// Mail posted.
    pub mail_out: u32,
    /// Human input bursts.
    pub human: u32,
    /// Records reported lost by gaps.
    pub lost: u64,
}

/// A session's current state (§7.1). About 1 KiB; kept outside the ring.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionState {
    /// The last seq applied.
    pub as_of: u64,
    /// The epoch.
    pub ep: u32,
    /// Bumps on every applied record: the chrome's cache key.
    pub rev: u64,
    /// The open path: the deepest open run, the open turn, the deepest open
    /// step.
    pub open: [Option<OpenNode>; 3],
    /// The newest verdict.
    pub phase: Phase,
    /// Open needs, most urgent first.
    pub needs: [Option<Need>; MAX_NEEDS],
    /// The newest closed run, turn and step.
    pub last: [Option<ClosedNode>; 3],
    /// Counters.
    pub totals: Totals,
    /// Mail delivered and not yet marked seen.
    pub mail_open: u16,
    /// The newest verdict claim.
    pub verdict: Option<Claim>,
    /// The newest label claim.
    pub label: Option<Claim>,
    /// The session's own close, once it closed.
    pub closed: Option<ClosedNode>,
    /// The newest record's time (wall ms).
    pub last_t: u64,
}

impl SessionState {
    /// The most urgent need.
    #[must_use]
    pub fn top_need(&self) -> Option<&Need> {
        self.needs.iter().flatten().next()
    }

    /// The open run.
    #[must_use]
    pub fn run(&self) -> Option<&OpenNode> {
        self.open[0].as_ref()
    }

    /// The open turn.
    #[must_use]
    pub fn turn(&self) -> Option<&OpenNode> {
        self.open[1].as_ref()
    }

    /// The deepest open step.
    #[must_use]
    pub fn step(&self) -> Option<&OpenNode> {
        self.open[2].as_ref()
    }
}

/// How the clause is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Someone is needed.
    Attention,
    /// Work is under way.
    Working,
    /// Something failed.
    Failed,
    /// At rest.
    Idle,
}

/// The clause's words: at most [`CLAUSE_BYTES`] bytes, held inline.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Words {
    buf: [u8; CLAUSE_BYTES],
    len: u8,
}

impl Words {
    /// `subject · suffix`, the subject cut (never mid-character, never
    /// before a combining mark or joiner) until the whole fits; `subject`
    /// alone when `suffix` is empty.
    #[must_use]
    pub fn fit(subject: &str, suffix: &str) -> Words {
        let owned;
        let s: &str = if suffix.is_empty() {
            cut_subject(subject, CLAUSE_BYTES)
        } else {
            let tail = format!(" · {suffix}");
            let head = cut_subject(subject, CLAUSE_BYTES.saturating_sub(tail.len()));
            if head.is_empty() || tail.len() >= CLAUSE_BYTES {
                cut_subject(subject, CLAUSE_BYTES)
            } else {
                owned = format!("{head}{tail}");
                &owned
            }
        };
        let mut buf = [0u8; CLAUSE_BYTES];
        buf[..s.len()].copy_from_slice(s.as_bytes());
        Words {
            buf,
            len: s.len() as u8,
        }
    }

    /// The words.
    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..usize::from(self.len)]).unwrap_or("")
    }
}

impl std::fmt::Debug for Words {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Words({:?})", self.as_str())
    }
}

/// Whether `c` extends the character before it (a combining mark, a joiner,
/// a variation selector, an emoji modifier): a cut is never placed before
/// one.
const fn is_extend(c: char) -> bool {
    matches!(c,
        '\u{0300}'..='\u{036f}'
        | '\u{1ab0}'..='\u{1aff}'
        | '\u{1dc0}'..='\u{1dff}'
        | '\u{200c}'..='\u{200d}'
        | '\u{20d0}'..='\u{20ff}'
        | '\u{fe00}'..='\u{fe0f}'
        | '\u{fe20}'..='\u{fe2f}'
        | '\u{1f3fb}'..='\u{1f3ff}'
        | '\u{e0100}'..='\u{e01ef}')
}

/// The longest prefix of `s` within `room` bytes cut between clusters.
fn cut_subject(s: &str, room: usize) -> &str {
    if s.len() <= room {
        return s;
    }
    let mut end = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let after = at + c.len_utf8();
        if after > room {
            break;
        }
        // An extending char joins the cluster before it: a cut falls only
        // where the next char starts a cluster of its own.
        if !chars.peek().is_some_and(|&(_, n)| is_extend(n)) {
            end = after;
        }
    }
    &s[..end]
}

/// The tab's "what is it doing" clause (§7.3): drawn as is, never parsed
/// back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clause {
    /// The words, at most 48 bytes.
    pub words: Words,
    /// How long the state has held.
    pub age_ms: u64,
    /// How it is drawn.
    pub tone: Tone,
    /// Whose state it is (a label's author, a need's raiser).
    pub by: Actor,
}

/// What the fold can say without claiming a need.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hints {
    /// How long since the newest record. A hint, never a need: long quiet is
    /// not stuck.
    pub quiet_for_ms: u64,
}

/// A coarse age for the clause: `40s`, `4m`, `3h`.
#[must_use]
pub fn age_short(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3600)
    }
}

/// The fold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fold {
    st: SessionState,
    tracked: Vec<OpenNode>,
    /// The shell phase, kept apart from the agent word.
    shell_phase: Option<Phase>,
}

impl Fold {
    /// An empty fold of epoch `ep`.
    #[must_use]
    pub fn new(ep: u32) -> Fold {
        let mut f = Fold::default();
        f.st.ep = ep;
        f
    }

    /// The state.
    #[must_use]
    pub const fn state(&self) -> &SessionState {
        &self.st
    }

    /// Apply one record. Pure and idempotent by seq: a record at or below
    /// `as_of` changes nothing, unless it starts an epoch newer than the
    /// fold's ([`starts_epoch`]), whose seqs restart.
    pub fn apply(&mut self, r: &Rec) {
        let reset = starts_epoch(r, self.st.ep);
        if r.seq <= self.st.as_of && !reset {
            return;
        }
        self.st.as_of = r.seq;
        self.st.rev = self.st.rev.saturating_add(1);
        self.st.last_t = self.st.last_t.max(r.t);
        match r.op {
            Op::Open => self.on_open(r),
            Op::Close => self.on_close(r),
            Op::Mark => self.on_mark(r),
            Op::Amend => self.on_amend(r),
            Op::Gap => self.on_gap(r, reset),
            Op::Snap => {}
        }
    }

    /// The tab clause at `now_wall` (§7.3): the first matching row wins.
    #[must_use]
    pub fn clause(&self, now_wall: u64) -> Option<Clause> {
        clause_of(&self.st, now_wall)
    }

    /// Hints at `now_wall`: never a need.
    #[must_use]
    pub fn hints(&self, now_wall: u64) -> Hints {
        Hints {
            quiet_for_ms: now_wall.saturating_sub(self.st.last_t),
        }
    }

    fn reproject(&mut self) {
        let deepest = |lvl: Lvl| {
            self.tracked
                .iter()
                .rev()
                .find(|o| o.node.lvl == lvl)
                .cloned()
        };
        self.st.open = [deepest(Lvl::Run), deepest(Lvl::Turn), deepest(Lvl::Step)];
    }

    fn on_open(&mut self, r: &Rec) {
        let mut f = NodeFields::default();
        for (k, v) in r.f.iter() {
            f.offer(*k, v.clone(), r.conf, r.seq);
        }
        if r.node.lvl == Lvl::Session {
            self.st.closed = None;
            return;
        }
        if self.tracked.len() >= MAX_TRACKED {
            self.tracked.remove(0);
        }
        let open = OpenNode {
            node: r.node,
            kind: r.k,
            par: r.par,
            t: r.t,
            by: r.by,
            src: r.src,
            conf: r.conf,
            f,
        };
        // A step that continues another (`cont=`, a wall that outlived its
        // turn) is the same fact: counted once, and its need keeps the time
        // the continued step opened.
        let continued = (r.node.lvl == Lvl::Step)
            .then(|| r.f.get_str(Key::Cont).and_then(Node::parse))
            .flatten();
        match r.k {
            Kind::Turn => self.st.totals.turns = self.st.totals.turns.saturating_add(1),
            _ if continued.is_some() => {}
            Kind::Approval | Kind::Question | Kind::Wall | Kind::Stall | Kind::Tool => {
                self.count_step(r);
            }
            _ if r.node.lvl == Lvl::Step => {
                self.st.totals.steps = self.st.totals.steps.saturating_add(1);
            }
            _ => {}
        }
        if let Some(word) = step_need(r) {
            let detail = r.f.get_str(Key::Detail).map(str::to_string);
            let since_t = continued
                .and_then(|c| {
                    self.st.last[2]
                        .as_ref()
                        .filter(|l| l.node == c)
                        .and_then(|l| l.opened_t)
                })
                .unwrap_or(r.t);
            self.raise(Need {
                word,
                node: r.node,
                kind: r.k,
                since_seq: r.seq,
                t: since_t,
                src: r.src,
                conf: r.conf,
                by: r.by,
                detail,
            });
        }
        self.tracked.push(open);
        self.reproject();
    }

    fn count_step(&mut self, r: &Rec) {
        let t = &mut self.st.totals;
        t.steps = t.steps.saturating_add(1);
        match r.k {
            Kind::Approval => t.appr = t.appr.saturating_add(1),
            Kind::Question => t.qs = t.qs.saturating_add(1),
            Kind::Stall => t.stalls = t.stalls.saturating_add(1),
            Kind::Tool => {
                if let Some(i) =
                    r.f.get_word(Key::Cls)
                        .and_then(|c| CLS_WORDS.iter().position(|w| *w == c))
                {
                    t.tools[i] = t.tools[i].saturating_add(1);
                }
            }
            _ => {}
        }
    }

    fn on_close(&mut self, r: &Rec) {
        let out = r.out().unwrap_or(crate::vocab::UNKNOWN);
        let opened = self
            .tracked
            .iter()
            .position(|o| o.node == r.node)
            .map(|i| self.tracked.remove(i));
        // A reader that joined late may hold children the log already
        // closed before it; none outlives its parent.
        let node = r.node;
        let mut ended = vec![node];
        let mut walk = vec![node];
        while let Some(parent) = walk.pop() {
            let children: Vec<Node> = self
                .tracked
                .iter()
                .filter(|o| o.par == Some(parent))
                .map(|o| o.node)
                .collect();
            self.tracked.retain(|o| o.par != Some(parent));
            ended.extend(&children);
            walk.extend(children);
        }
        let mut f = opened.as_ref().map(|o| o.f.clone()).unwrap_or_default();
        for (k, v) in r.f.iter() {
            f.offer(*k, v.clone(), r.conf, r.seq);
        }
        let closed = ClosedNode {
            node,
            kind: r.k,
            out,
            dur_ms: r.f.get_u(Key::DurMs).unwrap_or(0),
            opened_t: opened.as_ref().map(|o| o.t),
            t: r.t,
            by: r.by,
            opened_by: opened.as_ref().map_or(Actor::Nobody, |o| o.by),
            src: r.src,
            conf: r.conf,
            f,
        };
        self.drop_needs(|n| ended.contains(&n.node) && n.word != "exited");
        match node.lvl {
            Lvl::Run => {
                let kind = r.k;
                self.drop_needs(|n| lifts_failure(kind, out, node, n));
                if out == "fail" {
                    let detail = r.f.get(Key::Code).and_then(|v| match v {
                        Val::I(c) => Some(c.to_string()),
                        _ => None,
                    });
                    self.raise(Need {
                        word: "failure",
                        node,
                        kind,
                        since_seq: r.seq,
                        t: r.t,
                        src: r.src,
                        conf: r.conf,
                        by: r.by,
                        detail,
                    });
                }
                self.st.last[0] = Some(closed);
            }
            Lvl::Turn => {
                let t = &mut self.st.totals;
                match out {
                    "done" => t.done = t.done.saturating_add(1),
                    "asked" => t.asked = t.asked.saturating_add(1),
                    "interrupted" => t.interrupted = t.interrupted.saturating_add(1),
                    "error" => t.errors = t.errors.saturating_add(1),
                    w if w.starts_with("wall:") => t.walled = t.walled.saturating_add(1),
                    _ => t.other = t.other.saturating_add(1),
                }
                self.st.last[1] = Some(closed);
            }
            Lvl::Step => {
                if r.k == Kind::Approval {
                    let slot = match r.by {
                        Actor::Human => 0,
                        Actor::Harness => 1,
                        Actor::Ctl => 2,
                        _ => 3,
                    };
                    let by = &mut self.st.totals.appr_by[slot];
                    *by = by.saturating_add(1);
                }
                self.st.last[2] = Some(closed);
            }
            Lvl::Session => {
                self.tracked.clear();
                self.drop_needs(|n| n.word != "exited");
                self.raise(Need {
                    word: "exited",
                    node,
                    kind: r.k,
                    since_seq: r.seq,
                    t: r.t,
                    src: r.src,
                    conf: r.conf,
                    by: r.by,
                    detail: None,
                });
                self.st.closed = Some(closed);
            }
            Lvl::Instance => {}
        }
        self.reproject();
    }

    fn on_mark(&mut self, r: &Rec) {
        let word = r.f.get_word(Key::Word);
        match r.k {
            Kind::Need => {
                // A mark's need is the session's: it outlives the node the
                // mark attached to, and only a `clear` lifts it.
                if let Some(w) = word.filter(|w| mark_may_raise(r.src, w)) {
                    self.raise(Need {
                        word: w,
                        node: Node::SESSION,
                        kind: r.k,
                        since_seq: r.seq,
                        t: r.t,
                        src: r.src,
                        conf: r.conf,
                        by: r.by,
                        detail: None,
                    });
                }
            }
            Kind::Clear => {
                let by = r.by;
                // A step's need lives as long as its step; a clear lifts the
                // others, from whoever raised them or from the owner.
                self.drop_needs(|n| {
                    word.is_none_or(|w| n.word == w)
                        && n.node.lvl != Lvl::Step
                        && (n.by == by || by.is_owner())
                });
            }
            Kind::Seen => self.drop_needs(|n| n.word == "failure"),
            Kind::Verdict => {
                if word.is_some() {
                    self.st.verdict = Some(Claim {
                        word,
                        text: None,
                        by: r.by,
                        t: r.t,
                        until: None,
                        seq: r.seq,
                    });
                }
            }
            Kind::Label => {
                if let Some(text) = r.f.get_str(Key::Text) {
                    let ttl =
                        r.f.get_u(Key::Ttl)
                            .map_or(LABEL_TTL_MS, |s| s.saturating_mul(1000))
                            .min(LABEL_TTL_MAX_MS);
                    self.st.label = Some(Claim {
                        word: None,
                        text: Some(text.to_string()),
                        by: r.by,
                        t: r.t,
                        until: Some(r.t.saturating_add(ttl)),
                        seq: r.seq,
                    });
                }
            }
            Kind::Mail => match r.f.get_word(Key::Dir) {
                Some("in") => {
                    self.st.totals.mail_in = self.st.totals.mail_in.saturating_add(1);
                    self.st.mail_open = self.st.mail_open.saturating_add(1);
                }
                Some("out") => {
                    self.st.totals.mail_out = self.st.totals.mail_out.saturating_add(1);
                }
                _ => {}
            },
            Kind::MailSeen => self.st.mail_open = self.st.mail_open.saturating_sub(1),
            Kind::Human => self.st.totals.human = self.st.totals.human.saturating_add(1),
            Kind::Agent => {
                if let Some(w) = word {
                    self.st.phase = Phase {
                        word: w,
                        src: r.src,
                        conf: r.conf,
                        t: r.t,
                    };
                }
            }
            Kind::Status => {
                if let Some(p) = r.f.get_word(Key::Phase) {
                    let phase = Phase {
                        word: p,
                        src: r.src,
                        conf: r.conf,
                        t: r.t,
                    };
                    if self.st.phase.src != Src::Screen || self.st.phase.word == "-" {
                        self.st.phase = phase.clone();
                    }
                    self.shell_phase = Some(phase);
                }
            }
            _ => {}
        }
    }

    fn on_amend(&mut self, r: &Rec) {
        let node = r.node;
        let target = self
            .tracked
            .iter_mut()
            .find(|o| o.node == node)
            .map(|o| &mut o.f)
            .or_else(|| {
                self.st
                    .last
                    .iter_mut()
                    .flatten()
                    .find(|c| c.node == node)
                    .map(|c| &mut c.f)
            });
        if let Some(f) = target {
            for (k, v) in r.f.iter() {
                f.offer(*k, v.clone(), r.conf, r.seq);
            }
        }
        self.reproject();
    }

    fn on_gap(&mut self, r: &Rec, reset: bool) {
        self.st.totals.lost = self
            .st
            .totals
            .lost
            .saturating_add(r.f.get_u(Key::Lost).unwrap_or(0));
        if reset {
            if let Some(ep) = r.f.get_u(Key::Epoch) {
                self.st.ep = u32::try_from(ep).unwrap_or(u32::MAX);
            }
            // Slice 1 carries no open node across an update: what was open
            // is unknown now, and a step's need cannot be closed any more.
            self.tracked.clear();
            self.drop_needs(|n| n.node.lvl == Lvl::Step);
            self.reproject();
        }
    }

    /// Add a need in precedence order; when full, the least urgent (newest
    /// among equals) gives way to a more urgent one.
    fn raise(&mut self, need: Need) {
        let mut all: Vec<Need> = self.st.needs.iter_mut().filter_map(Option::take).collect();
        all.retain(|n| !(n.node == need.node && n.word == need.word));
        all.push(need);
        all.sort_by_key(|n| (need_precedence(n), n.since_seq));
        all.truncate(MAX_NEEDS);
        for (slot, n) in self.st.needs.iter_mut().zip(all) {
            *slot = Some(n);
        }
    }

    fn drop_needs(&mut self, mut gone: impl FnMut(&Need) -> bool) {
        let kept: Vec<Need> = self
            .st
            .needs
            .iter_mut()
            .filter_map(Option::take)
            .filter(|n| !gone(n))
            .collect();
        for (slot, n) in self.st.needs.iter_mut().zip(kept) {
            *slot = Some(n);
        }
    }
}

/// Whether the close of run `closed` (kind `kind`, outcome `out`) lifts
/// `need`: THE failure-supersede rule. A shell command's failure is the
/// newest command's outcome, as `status outcome=` holds it, so a later run
/// of the same kind that ends `ok` or `fail` replaces it (a `fail` raises
/// its own). A run that ended `unknown` (interrupted, its block gone) says
/// nothing about the earlier failure, and a run of another kind (a program
/// in front) is not the next command.
#[must_use]
pub fn lifts_failure(kind: Kind, out: &str, closed: Node, need: &Need) -> bool {
    matches!(out, "ok" | "fail")
        && need.word == "failure"
        && need.node.lvl == Lvl::Run
        && need.kind == kind
        && need.node != closed
}

/// Whether `r` starts an epoch newer than `ep`: a `gap why=handoff` (or
/// `why=reset`) naming `epoch=` above it. Epochs only grow (§2.2), so the
/// same gap applied again, or a stale one, starts nothing.
#[must_use]
pub fn starts_epoch(r: &Rec, ep: u32) -> bool {
    r.op == Op::Gap
        && matches!(r.f.get_word(Key::Why), Some("handoff" | "reset"))
        && r.f.get_u(Key::Epoch).is_some_and(|e| e > u64::from(ep))
}

/// The need words a `need` mark may raise (§7.4): `review`, `decision`,
/// `help`, and `escalated` from the owner's attention flag (`src=meta`,
/// §7.1). An observed need's word (`approval:*`, `unresponsive`,
/// `failure`, `exited`) is never a mark's to raise.
#[must_use]
pub fn mark_may_raise(src: Src, word: &str) -> bool {
    matches!(word, "review" | "decision" | "help") || (word == "escalated" && src == Src::Meta)
}

/// THE NEED PRECEDENCE (§7.1), lower first: a need raised by a mark ranks
/// last whatever its word, so a claim never outranks an observation; every
/// other need ranks by its word ([`need_rank`]).
#[must_use]
pub fn need_precedence(n: &Need) -> u8 {
    if n.src == Src::Mark || (n.conf == Conf::Claim && n.src != Src::Meta) {
        7
    } else {
        need_rank(n.word)
    }
}

/// The need a step's open raises (§7.1): `approval:<kind>`, `question`,
/// `wall:<kind>`, or `unresponsive` for an input stall.
#[must_use]
pub fn step_need(r: &Rec) -> Option<&'static str> {
    if r.op != Op::Open || r.node.lvl != Lvl::Step {
        return None;
    }
    match r.k {
        Kind::Approval => Some(
            r.f.get_word(Key::Box)
                .map_or("approval:other", |b| BoxWord::from_wire(b).need()),
        ),
        Kind::Question => Some("question"),
        Kind::Wall => {
            r.f.get_word(Key::Wall)
                .and_then(WallKind::from_name)
                .map(WallKind::wire)
        }
        Kind::Stall => Some("unresponsive"),
        _ => None,
    }
}

/// What a tool class reads as in the clause.
fn tool_words(cls: Option<&str>) -> &'static str {
    match cls {
        Some("edit" | "write") => "editing",
        Some("shell") => "running shell",
        Some("read") => "reading",
        Some("search") => "searching",
        Some("web") => "browsing",
        Some("agent") => "subagent",
        Some("plan") => "planning",
        Some("mcp") => "using a tool",
        _ => "working",
    }
}

/// THE clause policy (§7.3), a pure function of the state and `now_wall`.
#[must_use]
pub fn clause_of(st: &SessionState, now_wall: u64) -> Option<Clause> {
    let age = |t: u64| now_wall.saturating_sub(t);
    let mk = |words: Words, age_ms: u64, tone: Tone, by: Actor| Clause {
        words,
        age_ms,
        tone,
        by,
    };
    if let Some(n) = st.top_need() {
        let found = match n.word {
            w if w.starts_with("approval:") => Some((
                Words::fit("approval", w.trim_start_matches("approval:")),
                Tone::Attention,
            )),
            "question" => Some((Words::fit("question", ""), Tone::Attention)),
            "wall:auth" => Some((Words::fit("logged out", ""), Tone::Attention)),
            w if w.starts_with("wall:") => {
                let kind = WallKind::from_name(w);
                if kind.is_some_and(WallKind::is_limit) {
                    Some((
                        Words::fit("limit", n.detail.as_deref().unwrap_or("")),
                        Tone::Attention,
                    ))
                } else {
                    Some((
                        Words::fit("wall", w.trim_start_matches("wall:")),
                        Tone::Attention,
                    ))
                }
            }
            "unresponsive" => Some((Words::fit("not responding", ""), Tone::Attention)),
            "failure" => {
                let words = match n.detail.as_deref() {
                    Some(code) => Words::fit("failed", &format!("exit {code}")),
                    None => Words::fit("failed", ""),
                };
                Some((words, Tone::Failed))
            }
            _ => None,
        };
        if let Some((words, tone)) = found {
            return Some(mk(words, age(n.t), tone, n.by));
        }
    }
    if let Some(label) = &st.label
        && matches!(label.by, Actor::Harness | Actor::Human | Actor::Ctl)
        && label.until.is_some_and(|u| u > now_wall)
        && let Some(text) = &label.text
    {
        let tone = if st.turn().is_some() {
            Tone::Working
        } else {
            Tone::Idle
        };
        let a = age(label.t);
        return Some(mk(Words::fit(text, &age_short(a)), a, tone, label.by));
    }
    if let Some(step) = st.step() {
        match step.kind {
            Kind::Tool => {
                let a = age(step.t);
                let words = Words::fit(tool_words(step.f.word(Key::Cls)), &age_short(a));
                return Some(mk(words, a, Tone::Working, step.by));
            }
            Kind::Subagent => {
                return Some(mk(
                    Words::fit("subagent", ""),
                    age(step.t),
                    Tone::Working,
                    step.by,
                ));
            }
            _ => {}
        }
    }
    if let Some(turn) = st.turn() {
        let a = age(turn.t);
        return Some(mk(
            Words::fit("working", &age_short(a)),
            a,
            Tone::Working,
            turn.by,
        ));
    }
    if let Some(run) = st.run() {
        if run.kind == Kind::Cmd {
            let a = age(run.t);
            let detail = run
                .f
                .str(Key::Detail)
                .or(run.f.str(Key::Prog))
                .unwrap_or("running");
            return Some(mk(
                Words::fit(detail, &age_short(a)),
                a,
                Tone::Working,
                run.by,
            ));
        }
        if run.kind == Kind::Agent {
            let interrupted = st.last[1].as_ref().is_some_and(|t| t.out == "interrupted");
            let (word, since) = match &st.last[1] {
                Some(t) => (if interrupted { "interrupted" } else { "idle" }, t.t),
                None => ("idle", run.t),
            };
            return Some(mk(Words::fit(word, ""), age(since), Tone::Idle, run.by));
        }
    }
    None
}
