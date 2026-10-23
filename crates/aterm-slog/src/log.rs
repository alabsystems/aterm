// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One session's log (design §2.3, §5.1): a ring bounded by BOTH a record count and an encoded size, evicting LEVEL-AWARE and counting every loss exactly, with the open/close discipline of §2.3 enforced by the API and the fold kept beside the ring.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use crate::field::{FieldError, Fields, Key, Tier, Val};
use crate::fold::{Fold, SessionState};
use crate::line::{EncodeError, MAX_RECORD_BYTES, encode_bus, encoded_size};
use crate::rec::{Rec, Stamp};
use crate::vocab::{Actor, CLS_WORDS, Conf, Kind, Lvl, Node, Op, ShortId, Src};
use crate::words::{Out, RunOut, StepOut, TurnOut};

#[path = "carry.rs"]
mod carry;
#[path = "journal_log.rs"]
mod journal_log;
#[path = "mirror.rs"]
mod mirror;
#[path = "summary.rs"]
mod summary;
pub use carry::{
    CARRY_MAX_BYTES, CARRY_MAX_HOST_BYTES, CARRY_MAX_HOST_LINES, CARRY_MAX_LINE, CARRY_MAX_LINES,
    CARRY_SKELETON_BYTES, CARRY_TOTAL_BYTES, CarryAdopt, CarryBudget, CarryError, CarryOut,
    CarrySnapshot, CarryStats, PreparedCarry,
};
pub use journal_log::{JOURNAL_IMAGE_MAX_RECORDS, JournalImage};
pub use mirror::{
    BusCopy, BusCursor, CoverWhy, ENTRY_BYTES, LOG_MIRROR_BYTES, LandRefused, Landed, Prior,
    SNAP_EVERY, Slot, SlotKind, TAKE_MAX, Take, cover_body, is_bus_step,
};
#[path = "log_quota.rs"]
mod quota;
pub use quota::{PROGRAM_SHARE_MAX, mark_quota, program_share};
#[path = "log_rows.rs"]
mod rows;
pub use rows::{ROW_SPAN_MAX, Row, RowRule, row_floor};
pub use summary::CLOSED_SUMMARY_BYTES;

/// A session ring's record bound (§5.1).
pub const MAX_RECORDS: usize = 2048;
/// A session ring's encoded-size bound (§5.1): 256 KiB.
pub const MAX_BYTES: usize = 256 * 1024;
/// The most nodes open at once; a deeper open is refused.
pub const MAX_OPEN: usize = 64;
/// How many closed nodes' parents the log remembers, to find a closed
/// parent's nearest open ancestor.
const CLOSED_PARENTS: usize = 64;
/// How many update overlaps (seq ranges this log never held, [`Log::skip_to`]) a log remembers.
pub const MAX_HOLES: usize = 64;
/// THE SEQ LIMIT: no log's seq passes it by much.
pub const SEQ_LIMIT: u64 = 1 << 62;
/// The longest parent chain the log walks (a run under a run under a run …
/// is opened only under open nodes, so no chain is longer than the most nodes
/// open at once).
const MAX_DEPTH: usize = MAX_OPEN + 2;

/// A ring's bounds: whichever is reached first evicts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most records held.
    pub records: usize,
    /// The most encoded bytes held.
    pub bytes: usize,
}

impl Limits {
    /// A session's: 2048 records or 256 KiB.
    pub const SESSION: Limits = Limits {
        records: MAX_RECORDS,
        bytes: MAX_BYTES,
    };

    /// Bounds of at least one record and one largest record's bytes (so the record just appended is never the one evicted), or `None`.
    #[must_use]
    pub const fn new(records: usize, bytes: usize) -> Option<Limits> {
        if records == 0 || bytes < MAX_RECORD_BYTES {
            None
        } else {
            Some(Limits { records, bytes })
        }
    }
}

/// Whether a ring holding `records` records of `bytes` encoded bytes is past `limits`: when an append starts an eviction batch.
#[must_use]
pub const fn over_limits(records: usize, bytes: usize, limits: Limits) -> bool {
    records > limits.records || bytes > limits.bytes
}

/// Where an eviction batch stops: an eighth under each bound (at least one record and one largest record's bytes), so a full ring classifies its records once per eighth of its budget, not on every append.
#[must_use]
pub const fn low_water(l: Limits) -> Limits {
    let slack_records = if l.records / 8 > 1 { l.records / 8 } else { 1 };
    let slack_bytes = if l.bytes / 8 > MAX_RECORD_BYTES {
        l.bytes / 8
    } else {
        MAX_RECORD_BYTES
    };
    Limits {
        records: l.records.saturating_sub(slack_records),
        bytes: l.bytes.saturating_sub(slack_bytes),
    }
}

/// Whether the live close records of runs and turns (`count` of them, `bytes` encoded) alone pass half of `l`: only then is a close evicted before current detail (§5.1's third class).
#[must_use]
pub const fn closes_over_half(count: usize, bytes: usize, l: Limits) -> bool {
    count.saturating_mul(2) > l.records || bytes.saturating_mul(2) > l.bytes
}

/// A record's eviction class (§5.1), in the order a batch takes them: [`EvictClass::ClosedDetail`] first, [`EvictClass::Pinned`] never.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvictClass {
    /// Detail of closed structure: marks, amends, gaps and step records under a closed turn (or a closed run, or a node gone from the index), and detail hanging from an open run or the session that is older than the newest run or turn closed beneath it.
    ClosedDetail,
    /// The open record of a closed run or turn: its close carries the rollup.
    ClosedOpen,
    /// The close record of a run or turn (taken before current detail only while the closes pass half the budget, [`closes_over_half`]).
    CloseRecord,
    /// Current detail: under an open turn, or hanging from an open run or the session and newer than every run or turn closed beneath it.
    OpenDetail,
    /// Never evicted: the open record of an open node, the session's own open and close.
    Pinned,
}

/// What [`evict_class`] needs to know of a node the log indexes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeView {
    /// Its parent ([`Node::INSTANCE`]: not known, a closed node whose open record and parent this log no longer holds).
    pub parent: Node,
    /// Whether it is open.
    pub open: bool,
    /// The seq of the newest close of a run or turn beneath it (0: none).
    pub closed_below: u64,
}

/// THE EVICTION CLASS of `r` (§5.1), given the log's index of nodes (`node`: `None` for a node the log no longer indexes, which is closed).
#[must_use]
pub fn evict_class(r: &Rec, node: impl Fn(Node) -> Option<NodeView>) -> EvictClass {
    if r.node == Node::SESSION && matches!(r.op, Op::Open | Op::Close) {
        return EvictClass::Pinned;
    }
    match r.op {
        Op::Open if node(r.node).is_some_and(|v| v.open) => return EvictClass::Pinned,
        Op::Open if matches!(r.node.lvl, Lvl::Run | Lvl::Turn) => return EvictClass::ClosedOpen,
        Op::Close if matches!(r.node.lvl, Lvl::Run | Lvl::Turn) => {
            return EvictClass::CloseRecord;
        }
        _ => {}
    }
    let mut at = r.node;
    for _ in 0..MAX_DEPTH {
        let v = node(at);
        match at.lvl {
            Lvl::Turn => {
                return if v.is_some_and(|v| v.open) {
                    EvictClass::OpenDetail
                } else {
                    EvictClass::ClosedDetail
                };
            }
            Lvl::Run | Lvl::Session => {
                return match v {
                    Some(v) if v.open && r.seq > v.closed_below => EvictClass::OpenDetail,
                    _ => EvictClass::ClosedDetail,
                };
            }
            Lvl::Step => match v {
                Some(v) if v.parent != Node::INSTANCE => at = v.parent,
                _ => return EvictClass::ClosedDetail,
            },
            Lvl::Instance => return EvictClass::ClosedDetail,
        }
    }
    EvictClass::ClosedDetail
}

/// Why an append was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The node is not open (a node closes exactly once).
    NotOpen(Node),
    /// The node id was never issued by this log.
    NotIssued(Node),
    /// The kind does not fit the operation and level.
    KindDoesNotFit(Kind),
    /// The outcome is not one of the node's level.
    OutDoesNotFit(Node),
    /// The record would pass [`MAX_RECORD_BYTES`].
    Encode(EncodeError),
    /// A field did not fit.
    Field(FieldError),
    /// [`MAX_OPEN`] nodes are open (or a level's ids ran out).
    TooManyOpen,
    /// The session has closed.
    SessionClosed,
    /// The session was already opened.
    AlreadyOpen,
    /// A closed node the log no longer places: no record of its subtree is live and its close is older than the last [`CLOSED_PARENTS`] closes, so where it hung is not known, and an amend of it could be classed and counted only by a guess ([`Log::amend`]).
    Forgotten(Node),
}

impl From<FieldError> for Refused {
    fn from(e: FieldError) -> Refused {
        Refused::Field(e)
    }
}

/// What opens a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Open {
    /// The node's kind.
    pub kind: Kind,
    /// The parent asked for; `None` is the deepest open node that is a natural parent of the kind's level.
    pub under: Option<Node>,
    /// Where the fact was observed.
    pub src: Src,
    /// How sure.
    pub conf: Conf,
    /// Attribution.
    pub by: Actor,
    /// Fields.
    pub f: Fields,
}

impl Open {
    /// An open of `kind` with no parent named, no attribution, no fields.
    #[must_use]
    pub const fn new(kind: Kind, src: Src, conf: Conf) -> Open {
        Open {
            kind,
            under: None,
            src,
            conf,
            by: Actor::Nobody,
            f: Fields::new(),
        }
    }

    /// Under `parent`.
    #[must_use]
    pub const fn under(mut self, parent: Node) -> Open {
        self.under = Some(parent);
        self
    }

    /// Attributed to `by`.
    #[must_use]
    pub const fn by(mut self, by: Actor) -> Open {
        self.by = by;
        self
    }

    /// With fields `f`.
    #[must_use]
    pub fn fields(mut self, f: Fields) -> Open {
        self.f = f;
        self
    }
}

/// A duration the host measured itself, in place of the log's own monotonic difference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dur {
    /// Measured on one process's monotonic clock (`dur_ms=`).
    Mono(u64),
    /// Computed from wall stamps, such as a shell block's (`dur_ms= dur=wall`, §3.5).
    Wall(u64),
}

/// What closes a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Close {
    /// The outcome, of the node's level.
    pub out: Out,
    /// Where the end was observed.
    pub src: Src,
    /// How sure.
    pub conf: Conf,
    /// Attribution.
    pub by: Actor,
    /// The duration, when the host measured it (a block the sweep saw only complete has its own stamps); `None` is the log's monotonic difference since the open.
    pub dur: Option<Dur>,
    /// Fields beside the outcome, duration and rollup (`code=`, `blk=`).
    pub f: Fields,
}

impl Close {
    /// A close with `out`, no attribution, no fields.
    #[must_use]
    pub const fn new(out: Out, src: Src, conf: Conf) -> Close {
        Close {
            out,
            src,
            conf,
            by: Actor::Nobody,
            dur: None,
            f: Fields::new(),
        }
    }

    /// Attributed to `by`.
    #[must_use]
    pub const fn by(mut self, by: Actor) -> Close {
        self.by = by;
        self
    }

    /// With fields `f`.
    #[must_use]
    pub fn fields(mut self, f: Fields) -> Close {
        self.f = f;
        self
    }

    /// With a duration measured on the host's monotonic clock.
    #[must_use]
    pub const fn dur_ms(mut self, ms: u64) -> Close {
        self.dur = Some(Dur::Mono(ms));
        self
    }

    /// With a duration computed from wall stamps (written `dur=wall`): a run rebuilt from a shell block passes the block's end stamp as the close's `at` and its start-to-end wall difference here.
    #[must_use]
    pub const fn dur_wall(mut self, ms: u64) -> Close {
        self.dur = Some(Dur::Wall(ms));
        self
    }
}

/// Whether the log writes `key` itself on a record of `op`: `orphan=` on an open; the outcome, duration and rollup on a close; and `op_by=` (a turn close's restated opener) on none but the log's own close, so no caller writes it on any record.
#[must_use]
pub fn log_owned(op: Op, key: Key) -> bool {
    if key == Key::OpBy {
        return true;
    }
    match op {
        Op::Open => key == Key::Orphan,
        Op::Close => matches!(
            key,
            Key::Out
                | Key::DurMs
                | Key::Dur
                | Key::Steps
                | Key::Appr
                | Key::Qs
                | Key::Walls
                | Key::Stalls
                | Key::Mail
                | Key::Errs
                | Key::Lost
                | Key::Tools
        ),
        Op::Mark | Op::Amend | Op::Gap | Op::Snap => false,
    }
}

/// The first key of `f` the log owns on `op`, as a refusal.
fn refuse_owned(op: Op, f: &Fields) -> Result<(), Refused> {
    match f.iter().find(|(k, _)| log_owned(op, *k)) {
        Some((k, _)) => Err(Refused::Field(FieldError::Shape(*k))),
        None => Ok(()),
    }
}

/// A point fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    /// The mark's kind.
    pub kind: Kind,
    /// The node it attaches to; `None` (or a node not open) is the deepest open node.
    pub at: Option<Node>,
    /// Where observed.
    pub src: Src,
    /// How sure.
    pub conf: Conf,
    /// Attribution.
    pub by: Actor,
    /// Fields.
    pub f: Fields,
}

impl Mark {
    /// A mark of `kind` on the deepest open node.
    #[must_use]
    pub const fn new(kind: Kind, src: Src, conf: Conf) -> Mark {
        Mark {
            kind,
            at: None,
            src,
            conf,
            by: Actor::Nobody,
            f: Fields::new(),
        }
    }

    /// On `node`.
    #[must_use]
    pub const fn at(mut self, node: Node) -> Mark {
        self.at = Some(node);
        self
    }

    /// Attributed to `by`.
    #[must_use]
    pub const fn by(mut self, by: Actor) -> Mark {
        self.by = by;
        self
    }

    /// With fields `f`.
    #[must_use]
    pub fn fields(mut self, f: Fields) -> Mark {
        self.f = f;
        self
    }
}

/// What refines an issued node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Amend {
    /// The node's kind.
    pub kind: Kind,
    /// Where the refinement was observed (a transcript, say).
    pub src: Src,
    /// How sure: the fold keeps a field at the highest confidence.
    pub conf: Conf,
    /// Attribution.
    pub by: Actor,
    /// The refined fields.
    pub f: Fields,
}

impl Amend {
    /// An amend of a `kind` node with fields `f`, no attribution.
    #[must_use]
    pub const fn new(kind: Kind, src: Src, conf: Conf, f: Fields) -> Amend {
        Amend {
            kind,
            src,
            conf,
            by: Actor::Nobody,
            f,
        }
    }

    /// Attributed to `by`.
    #[must_use]
    pub const fn by(mut self, by: Actor) -> Amend {
        self.by = by;
        self
    }
}

/// How a close is written: the outcome, its provenance and a host-measured
/// duration (the named node's only, never a cascaded descendant's).
#[derive(Clone, Copy, Debug)]
struct Ender {
    out: Out,
    src: Src,
    conf: Conf,
    by: Actor,
    dur: Option<Dur>,
}

/// Why records were lost (`gap why=`), and why a reader's cursor broke.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GapWhy {
    /// A seamless update: one that carried nothing (its successor's first record), or, for a reader, a read across the seqs a carried update's outgoing process may have written after the carry was taken ([`Log::skip_to`]): those records are not here and nobody counted them, so the gap's `lost` is a lower bound.
    Handoff,
    /// The previous process died.
    Crash,
    /// Journal segments were pruned.
    Pruned,
    /// A ring evicted them: the log's own, or the history a producer reads (the terminal's block list) before the producer reached them.
    Evicted,
    /// The cursor belongs to another epoch (or is ahead of the log).
    Reset,
    /// A journal read stopped at its byte budget before it reached them (design §5.3, phase 2B): they may be on disk, unread by this reply.
    Unread,
}

impl GapWhy {
    /// The `why=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            GapWhy::Handoff => "handoff",
            GapWhy::Crash => "crash",
            GapWhy::Pruned => "pruned",
            GapWhy::Evicted => "evicted",
            GapWhy::Reset => "reset",
            GapWhy::Unread => "unread",
        }
    }

    /// The why a `why=` word names, or `None` for a word this build does not know.
    #[must_use]
    pub fn from_word(word: &str) -> Option<GapWhy> {
        [
            GapWhy::Handoff,
            GapWhy::Crash,
            GapWhy::Pruned,
            GapWhy::Evicted,
            GapWhy::Reset,
            GapWhy::Unread,
        ]
        .into_iter()
        .find(|w| w.word() == word)
    }
}

/// A reader's position: the epoch it read and the last seq it holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Cursor {
    /// The epoch the seq belongs to.
    pub ep: u32,
    /// The last seq the reader holds (0: none).
    pub seq: u64,
}

impl Cursor {
    /// A cursor at `seq` of epoch `ep`.
    #[must_use]
    pub const fn new(ep: u32, seq: u64) -> Cursor {
        Cursor { ep, seq }
    }
}

/// What a reader is told when the seqs a read covers hold records it will never see, or another process's (or holds another epoch's cursor).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    /// Evicted, across an update's overlap, or the cursor was reset.
    pub why: GapWhy,
    /// Exactly how many records of the covered seqs (the cursor's, exclusive, to the read's `next`) this log held and the reader will never see: the ones before `resync_to` and any evicted from between the records returned (whose seqs show where).
    pub lost: u64,
    /// The seq to resume after: the records returned begin past it (the first record's seq less one; the read's `next` when none is returned).
    pub resync_to: u64,
}

/// A read: an optional gap, then records in seq order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read<'a> {
    /// The gap the reader crossed, if any.
    pub gap: Option<Gap>,
    /// The records, oldest first.
    pub recs: Vec<&'a Rec>,
    /// Where to read next.
    pub next: Cursor,
}

/// A [`Read`] that hands out the ring's own shared records ([`Log::read_shared`]): a reader keeps one past the read with `Arc::clone`, a count bump, never a copy of its fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedRead<'a> {
    /// The gap the reader crossed, if any.
    pub gap: Option<Gap>,
    /// The records, oldest first.
    pub recs: Vec<&'a Arc<Rec>>,
    /// Where to read next.
    pub next: Cursor,
}

/// A node's counters, written into its close record (§2.3 "Rollup").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rollup {
    /// Steps opened beneath it.
    pub steps: u32,
    /// Approval steps beneath it.
    pub appr: u32,
    /// Question steps beneath it.
    pub qs: u32,
    /// Wall steps beneath it.
    pub walls: u32,
    /// Input stalls beneath it.
    pub stalls: u32,
    /// Mail marks on it or beneath it.
    pub mail: u32,
    /// Failed runs, errored turns and steps beneath it.
    pub errs: u32,
    /// Its own records the ring evicted while it was open.
    pub lost: u32,
    /// Tool steps by class, in [`CLS_WORDS`] order.
    pub tools: [u32; 10],
}

impl Rollup {
    /// `tools=shell:9,edit:8`: classes by count, most first, within the
    /// key's 64-byte cap.
    fn tools_word(&self) -> Option<String> {
        let mut by: Vec<(u32, &str)> = self
            .tools
            .iter()
            .zip(CLS_WORDS)
            .filter(|(n, _)| **n > 0)
            .map(|(n, w)| (*n, *w))
            .collect();
        by.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        let mut s = String::new();
        for (n, w) in by {
            let item = format!("{w}:{n}");
            if s.len() + item.len() + 1 > 64 {
                break;
            }
            if !s.is_empty() {
                s.push(',');
            }
            s.push_str(&item);
        }
        (!s.is_empty()).then_some(s)
    }

    /// The rollup's fields in the order a close carries them (zero
    /// counters left out).
    fn fields(&self) -> Vec<(Key, Val)> {
        let mut v = Vec::new();
        for (key, n) in [
            (Key::Steps, self.steps),
            (Key::Appr, self.appr),
            (Key::Qs, self.qs),
            (Key::Walls, self.walls),
            (Key::Stalls, self.stalls),
            (Key::Mail, self.mail),
            (Key::Errs, self.errs),
            (Key::Lost, self.lost),
        ] {
            if n > 0 {
                v.push((key, Val::U(u64::from(n))));
            }
        }
        if let Some(t) = self
            .tools_word()
            .and_then(|s| crate::field::BoundedToken::new(Key::Tools, &s))
        {
            v.push((Key::Tools, Val::Tok(t)));
        }
        v
    }
}

/// An open node: its id, kind, parent and when it opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenEntry {
    /// The node.
    pub node: Node,
    /// Its kind.
    pub kind: Kind,
    /// Its parent (the session for a run under none).
    pub parent: Node,
    /// When it opened (wall ms).
    pub t: u64,
    /// When it opened (the private monotonic clock).
    pub mono: u64,
    /// Whether it attached to an ancestor that was not its natural parent.
    pub orphan: bool,
    /// Its counters so far.
    pub roll: Rollup,
    /// Whether it opened in another process (a seamless update carried it): its `mono` is this process's clock clamped, so a close measures it on the wall clock (`dur_ms= dur=wall`, §3.5).
    pub wall_clock: bool,
}

/// The last node id issued per numbered level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct IdCounters {
    /// The last `r<n>`.
    pub run: u32,
    /// The last `t<n>`.
    pub turn: u32,
    /// The last `p<n>`.
    pub step: u32,
}

impl IdCounters {
    const fn slots(self) -> [u32; 3] {
        [self.run, self.turn, self.step]
    }
}

/// A live record and its encoded size.
#[derive(Clone, Debug)]
struct Entry {
    rec: Arc<Rec>,
    bytes: usize,
    /// The record's place in the bus class's dense count (phase 4, design
    /// §5.5): `1..` for a bus-class record ([`Rec::is_bus_class`]), 0 for a
    /// local one.
    ms: u64,
    /// THE RECORD'S PROCESS-CLOCK READING:
    /// the `mono_ms` of the [`Stamp`] its append was given, so a view that
    /// orders or ages records on this process's monotonic clock (the
    /// timeline's `t_ms`) reads it off the record, never off the wall `t=`.
    mono: u64,
    /// THE ROWS IT STANDS FOR: its dense
    /// row ids, when the host's rule flagged it as it was appended
    /// (`log_rows`).
    row: rows::RowTag,
}

/// One node of the log's index: a node that is open, or whose subtree holds
/// a live record (so every live record's parent chain can be walked, for the
/// eviction class and for `log <node>`'s subtree, after the records that
/// named its parents were evicted).
#[derive(Clone, Debug, PartialEq, Eq)]
struct NodeInfo {
    /// Its parent ([`Node::INSTANCE`] when not known; the session is its own).
    parent: Node,
    open: bool,
    /// Live records in its subtree, its own included.
    live: u32,
    /// The seq of the newest close of a run or turn beneath it.
    closed_below: u64,
    /// Its open record's encoded bytes, while that is live and pinned.
    open_bytes: usize,
}

/// THE JOURNAL'S WAKE (design §5.3): a host's dirty notifier, rung by every append and every eviction of a log it is installed in ([`Log::set_notify`]), so no producer path can write a record the host's journal writer is not woken for.
#[derive(Clone)]
pub struct Notify(std::sync::Arc<dyn Fn() + Send + Sync>);

impl Notify {
    /// A notifier that runs `f`.
    pub fn new(f: impl Fn() + Send + Sync + 'static) -> Notify {
        Notify(std::sync::Arc::new(f))
    }

    /// Run it.
    pub fn ring(&self) {
        (self.0)();
    }

    /// Whether `self` and `other` are one notifier.
    #[must_use]
    pub fn same(&self, other: &Notify) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for Notify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Notify")
    }
}

/// One session's log.
#[derive(Clone, Debug)]
pub struct Log {
    ep: u32,
    limits: Limits,
    /// The byte budget evictions keep to (at most `limits.bytes`; lowered by
    /// the host's instance-wide sweep, [`Log::set_byte_budget`]).
    budget_bytes: usize,
    ring: VecDeque<Entry>,
    bytes: usize,
    last_seq: u64,
    lost: u64,
    /// The newest seq this log held and no longer does (0: none).
    newest_lost: u64,
    /// The live close records of runs and turns, and their bytes.
    close_count: usize,
    close_bytes: usize,
    /// The oldest epoch whose cursors count in this log's seqs.
    base_ep: u32,
    /// The first seq this log accounts for.
    first_seq: u64,
    /// Seq ranges (inclusive) this log never held, oldest first, each with
    /// why a read across it says it was not held: an update's overlap
    /// ([`GapWhy::Handoff`]), or, in a log a journal read built
    /// ([`Log::from_journal`]), segments pruned or left unread.
    holes: VecDeque<(u64, u64, GapWhy)>,
    /// The last id issued per numbered level: run, turn, step.
    issued: [u32; 3],
    open: Vec<OpenEntry>,
    nodes: BTreeMap<Node, NodeInfo>,
    closed_parents: VecDeque<(Node, Node)>,
    session_opened: bool,
    session_closed: bool,
    session_t: u64,
    session_mono: u64,
    session_kind: Kind,
    session_roll: Rollup,
    /// Whether the session node opened in another process (its `mono` is
    /// clamped; its close measures the wall clock).
    session_wall_clock: bool,
    /// The session's own open and close records' bytes (pinned).
    session_bytes: usize,
    /// An operation of several records is under way (a cascade of closes, a
    /// supersede and its open): its batch runs once it is whole, so the
    /// classes it reads are those of the structure the operation leaves.
    hold_batch: bool,
    /// Readers' fence while an update's carry stands ([`Log::fence_reads`]):
    /// the seq they see up to, and the fold as it stood there.
    fence: Option<Box<(u64, Fold)>>,
    /// The live records' estimated heap ([`Log::footprint`]).
    heap: usize,
    fold: Fold,
    /// The host's journal wake ([`Notify`]), if one is installed.
    notify: Option<Notify>,
    /// The newest seq the host's journal writer has taken
    /// ([`Log::note_journaled`]), while one follows this log.
    journaled: Option<u64>,
    /// The bus class's count in this seq space (phase 4): the `ms` of the
    /// newest bus-class record appended, evicted or not.
    ms_head: u64,
    /// [`Self::ms_head`] when the readers were fenced ([`Log::fence_reads`]):
    /// what a take under the fence and its tail gap stop at.
    fence_ms: u64,
    /// THE MIRROR (phase 4, `mirror.rs`): where this log's bus-class records
    /// stand on the bus.
    mirror: Option<Box<mirror::Mirror>>,
    /// THE TIMELINE'S ROWS (`log_rows`): the host's row
    /// rule, the dense row counter and where this log's own records begin.
    rows: rows::Rows,
}

/// One live record's estimated heap: its ring entry, the shared record's own
/// allocation (the record and its two reference counts, [`Entry::rec`]), its
/// fields' vector and the text and tokens they hold.
fn entry_heap(rec: &Rec) -> usize {
    let strings: usize = rec.f.iter().map(|(_, v)| v.heap_len()).sum();
    let shared = std::mem::size_of::<Rec>() + 2 * std::mem::size_of::<usize>();
    std::mem::size_of::<Entry>()
        + shared
        + rec.f.len() * std::mem::size_of::<(Key, Val)>()
        + strings
}

/// The index of a numbered level in [`Log::issued`].
const fn level_slot(lvl: Lvl) -> Option<usize> {
    match lvl {
        Lvl::Run => Some(0),
        Lvl::Turn => Some(1),
        Lvl::Step => Some(2),
        Lvl::Instance | Lvl::Session => None,
    }
}

/// Whether a node of `parent` level is the natural parent of a `child` level node: a run under the session or a run, a turn under a run, a step under a turn or a step.
#[must_use]
pub const fn natural_parent(parent: Lvl, child: Lvl) -> bool {
    match child {
        Lvl::Run => matches!(parent, Lvl::Session | Lvl::Run),
        Lvl::Turn => matches!(parent, Lvl::Run),
        Lvl::Step => matches!(parent, Lvl::Turn | Lvl::Step),
        Lvl::Instance | Lvl::Session => false,
    }
}

impl Log {
    /// An empty session log of epoch `ep` at the session bounds.
    #[must_use]
    pub fn new(ep: u32) -> Log {
        Log::with_limits(ep, Limits::SESSION)
    }

    /// An empty log with `limits` (tests and models use small rings).
    #[must_use]
    pub fn with_limits(ep: u32, limits: Limits) -> Log {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            Node::SESSION,
            NodeInfo {
                parent: Node::SESSION,
                open: true,
                live: 0,
                closed_below: 0,
                open_bytes: 0,
            },
        );
        Log {
            ep,
            limits,
            budget_bytes: limits.bytes,
            ring: VecDeque::new(),
            bytes: 0,
            last_seq: 0,
            lost: 0,
            newest_lost: 0,
            close_count: 0,
            close_bytes: 0,
            base_ep: ep,
            first_seq: 1,
            holes: VecDeque::new(),
            issued: [0; 3],
            open: Vec::new(),
            nodes,
            closed_parents: VecDeque::new(),
            session_opened: false,
            session_closed: false,
            session_t: 0,
            session_mono: 0,
            session_kind: Kind::Spawn,
            session_roll: Rollup::default(),
            session_wall_clock: false,
            session_bytes: 0,
            hold_batch: false,
            fence: None,
            heap: 0,
            fold: Fold::new(ep),
            notify: None,
            journaled: None,
            ms_head: 0,
            fence_ms: 0,
            mirror: None,
            rows: rows::Rows::NEW,
        }
    }

    /// The successor's log after a seamless update that carried no records: epoch `ep`, its first record `gap - gap why=handoff epoch=<ep>` at seq `next_seq` (at least 1), its node ids continuing `ids`, the predecessor's [`Log::id_counters`] (three integers the update carries beside the timeline's id).
    #[must_use]
    pub fn after_handoff(
        ep: u32,
        limits: Limits,
        ids: IdCounters,
        next_seq: u64,
        at: Stamp,
    ) -> Log {
        Log::after_handoff_built(ep, limits, ids, next_seq, None, at)
    }

    /// [`Log::after_handoff`], the gap naming the predecessor's build (`gap why=handoff epoch=<ep> build=<n>`) when the update said it.
    #[must_use]
    pub fn after_handoff_built(
        ep: u32,
        limits: Limits,
        ids: IdCounters,
        next_seq: u64,
        build: Option<u64>,
        at: Stamp,
    ) -> Log {
        let mut log = Log::with_limits(ep, limits);
        log.issued = ids.slots();
        let next_seq = if next_seq > SEQ_LIMIT.saturating_add(1) {
            1
        } else {
            next_seq.max(1)
        };
        log.last_seq = next_seq - 1;
        log.first_seq = next_seq;
        log.gap_built(GapWhy::Handoff, 0, build, at);
        log
    }

    /// The last node id issued per level: what a successor continues.
    #[must_use]
    pub const fn id_counters(&self) -> IdCounters {
        IdCounters {
            run: self.issued[0],
            turn: self.issued[1],
            step: self.issued[2],
        }
    }

    /// The epoch.
    #[must_use]
    pub const fn ep(&self) -> u32 {
        self.ep
    }

    /// The bounds.
    #[must_use]
    pub const fn limits(&self) -> Limits {
        self.limits
    }

    /// The last seq assigned (`as_of`); 0 before any record.
    #[must_use]
    pub const fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// The oldest live seq; `last_seq + 1` when the ring is empty.
    #[must_use]
    pub fn oldest_seq(&self) -> u64 {
        self.ring
            .front()
            .map_or(self.last_seq.saturating_add(1), |e| e.rec.seq)
    }

    /// The seq from which the window is whole: every seq this log held from it on is live, so a reader at `since=<complete_from - 1>` crosses no loss of this log's (`log`'s `oldest=`).
    #[must_use]
    pub fn complete_from(&self) -> u64 {
        self.newest_lost.saturating_add(1).max(self.first_seq)
    }

    /// The first seq this log accounts for: 1, or where an update that carried no records started it, or past the oldest overlap it forgot ([`MAX_HOLES`]).
    #[must_use]
    pub const fn first_seq(&self) -> u64 {
        self.first_seq
    }

    /// The oldest epoch whose cursors count in this log's seqs: this log's own, or the earliest a chain of carried updates continued.
    #[must_use]
    pub const fn base_ep(&self) -> u32 {
        self.base_ep
    }

    /// The seq ranges (inclusive) this log never held, oldest first.
    pub fn holes(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.holes.iter().map(|&(lo, hi, _)| (lo, hi))
    }

    /// [`Log::holes`] with why each was not held.
    pub fn holes_why(&self) -> impl Iterator<Item = (u64, u64, GapWhy)> + '_ {
        self.holes.iter().copied()
    }

    /// Install (or remove) the host's journal wake ([`Notify`]): from now on every append, eviction, overlap and fence change of this log rings it.
    pub fn set_notify(&mut self, notify: Option<Notify>) {
        self.notify = notify;
    }

    /// The host's journal wake, if one is installed.
    #[must_use]
    pub const fn notify(&self) -> Option<&Notify> {
        self.notify.as_ref()
    }

    /// The host's journal writer has taken every record up to `seq` (phase 2B): from now on an eviction batch takes the records it holds before any it has not reached, each in the policy's order, so a burst between two of its passes loses nothing it could still write while the ring keeps its bounds.
    pub fn note_journaled(&mut self, seq: u64) {
        self.journaled = Some(self.journaled.map_or(seq, |j| j.max(seq)));
    }

    /// The newest seq the journal writer has taken, while one follows.
    #[must_use]
    pub const fn journaled(&self) -> Option<u64> {
        self.journaled
    }

    /// Ring the host's journal wake, if one is installed.
    fn rang(&self) {
        if let Some(n) = &self.notify {
            n.ring();
        }
    }

    /// Records this log held and no longer does (evicted), exactly.
    #[must_use]
    pub const fn lost(&self) -> u64 {
        self.lost
    }

    /// The log's estimated heap, in bytes: its live records (their entries, field vectors and text), its node index, open entries and overlaps, and its fold.
    #[must_use]
    pub fn footprint(&self) -> usize {
        let node = std::mem::size_of::<(Node, NodeInfo)>() + 2 * std::mem::size_of::<usize>();
        std::mem::size_of::<Log>()
            .saturating_add(self.heap)
            .saturating_add(self.nodes.len() * node)
            .saturating_add(self.open.len() * std::mem::size_of::<OpenEntry>())
            .saturating_add(self.closed_parents.len() * std::mem::size_of::<(Node, Node)>())
            .saturating_add(self.holes.len() * std::mem::size_of::<(u64, u64, GapWhy)>())
            .saturating_add(self.fold.footprint())
            .saturating_add(self.fence.as_ref().map_or(0, |f| f.1.footprint()))
            .saturating_add(
                self.mirror
                    .as_ref()
                    .map_or(0, |_| std::mem::size_of::<mirror::Mirror>()),
            )
    }

    /// The byte budget evictions keep to ([`Log::set_byte_budget`]).
    #[must_use]
    pub const fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// The bounds evictions keep to: the record bound, and the byte budget.
    #[must_use]
    pub const fn effective_limits(&self) -> Limits {
        Limits {
            records: self.limits.records,
            bytes: self.budget_bytes,
        }
    }

    /// The encoded bytes of the records no eviction takes: the open records of open nodes and the session's own open and close.
    #[must_use]
    pub fn pinned_bytes(&self) -> usize {
        self.nodes
            .values()
            .filter(|n| n.open)
            .map(|n| n.open_bytes)
            .sum::<usize>()
            .saturating_add(self.session_bytes)
    }

    /// Set the byte budget to `bytes` (clamped to one largest record's bytes and to the session bound), evicting level-aware at once when the ring is past it: the host's instance-wide cap (design §5.1) shares memory out this way, never inside an append to another session's log.
    pub fn set_byte_budget(&mut self, bytes: usize) {
        self.budget_bytes = bytes.clamp(MAX_RECORD_BYTES, self.limits.bytes);
        let quota = self.enforce_mark_quota();
        if over_limits(self.ring.len(), self.bytes, self.effective_limits()) {
            self.evict_batch();
            self.rang();
        } else if quota > 0 {
            self.rang();
        }
    }

    /// What a closed session keeps (a host's closed-session index, design §6): a copy of this log evicted level-aware to `bytes` encoded, by the policy an append past its budget runs, never below what no eviction takes (the session's own open and close, and an open log's open path).
    #[must_use]
    pub fn kept_within(&self, bytes: usize) -> Log {
        let mut kept = self.clone();
        kept.fence = None;
        kept.notify = None;
        kept.journaled = None;
        kept.hold_batch = false;
        kept.closed_parents = VecDeque::new();
        kept.budget_bytes = bytes.min(kept.limits.bytes);
        if over_limits(kept.ring.len(), kept.bytes, kept.effective_limits()) {
            kept.evict_batch();
        }
        kept.ring.shrink_to_fit();
        kept
    }

    /// The parent of `n`, as far as the log still knows it: an open node's, an indexed node's (one whose subtree holds a live record), or a recently closed node's.
    #[must_use]
    pub fn parent_of(&self, n: Node) -> Option<Node> {
        if n == Node::SESSION {
            return None;
        }
        self.open_entry(n)
            .map(|e| e.parent)
            .or_else(|| {
                self.nodes
                    .get(&n)
                    .map(|i| i.parent)
                    .filter(|p| *p != Node::INSTANCE)
            })
            .or_else(|| {
                self.closed_parents
                    .iter()
                    .rev()
                    .find(|(c, _)| *c == n)
                    .map(|(_, p)| *p)
            })
    }

    /// The index's view of `n` (an open node, or one whose subtree holds a live record): what [`evict_class`] reads.
    #[must_use]
    pub fn node_view(&self, n: Node) -> Option<NodeView> {
        self.view(n)
    }

    /// The eviction class of the live record `r` as the log stands now.
    #[must_use]
    pub fn class_of(&self, r: &Rec) -> EvictClass {
        evict_class(r, |n| self.view(n))
    }

    /// How many seqs in `(lo, hi` this log held: the seqs it accounts for (`Log::first_seq` on), less the ones it never held (its holes).
    #[must_use]
    pub fn held_between(&self, lo: u64, hi: u64) -> u64 {
        let lo = lo.max(self.first_seq.saturating_sub(1));
        if hi <= lo {
            return 0;
        }
        let mut n = hi - lo;
        for &(a, b, _) in &self.holes {
            let (a, b) = (a.max(lo.saturating_add(1)), b.min(hi));
            if b >= a {
                n = n.saturating_sub(b - a + 1);
            }
        }
        n
    }

    /// How many live records have a seq in `(lo, hi]`, counting only the
    /// seqs this log accounts for.
    fn live_between(&self, lo: u64, hi: u64) -> u64 {
        let lo = lo.max(self.first_seq.saturating_sub(1));
        if hi <= lo {
            return 0;
        }
        let a = self.ring.partition_point(|e| e.rec.seq <= lo);
        let b = self.ring.partition_point(|e| e.rec.seq <= hi);
        b.saturating_sub(a) as u64
    }

    /// Why the oldest seq range this log never held that `(lo, hi]` meets
    /// was not held, if it meets one.
    fn crosses_hole(&self, lo: u64, hi: u64) -> Option<GapWhy> {
        self.holes
            .iter()
            .find(|&&(a, b, _)| a <= hi && b > lo)
            .map(|&(.., why)| why)
    }

    /// Live records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// Whether no record is live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Live records' encoded bytes.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// The live records, oldest first.
    pub fn window(&self) -> impl DoubleEndedIterator<Item = &Rec> + ExactSizeIterator {
        self.ring.iter().map(|e| &*e.rec)
    }

    /// The live records, oldest first, each with its process-clock reading (6F-1: the `mono_ms` of the stamp its append was given; a carried record's wall time on this clock, a journaled or summarized one's 0).
    pub fn window_mono(&self) -> impl DoubleEndedIterator<Item = (&Rec, u64)> + ExactSizeIterator {
        self.ring.iter().map(|e| (&*e.rec, e.mono))
    }

    /// The process-clock reading of the live record at `seq` (as [`Log::window_mono`] gives it), or `None` when no live record has that seq.
    #[must_use]
    pub fn mono_of(&self, seq: u64) -> Option<u64> {
        let i = self.ring.partition_point(|e| e.rec.seq < seq);
        self.ring
            .get(i)
            .filter(|e| e.rec.seq == seq)
            .map(|e| e.mono)
    }

    /// The fold of every record ever appended, evicted or not.
    #[must_use]
    pub const fn fold(&self) -> &Fold {
        &self.fold
    }

    /// The fold's state.
    #[must_use]
    pub const fn state(&self) -> &SessionState {
        self.fold.state()
    }

    /// Whether `node` is open (the session until it closes).
    #[must_use]
    pub fn is_open(&self, node: Node) -> bool {
        if node == Node::SESSION {
            return !self.session_closed;
        }
        self.open.iter().any(|e| e.node == node)
    }

    /// The open nodes, in the order they opened.
    #[must_use]
    pub fn open_nodes(&self) -> &[OpenEntry] {
        &self.open
    }

    /// The open entry of `node`.
    #[must_use]
    pub fn open_entry(&self, node: Node) -> Option<&OpenEntry> {
        self.open.iter().find(|e| e.node == node)
    }

    /// The open nodes of `lvl` whose parent is `parent`.
    pub fn open_under(&self, parent: Node, lvl: Lvl) -> impl Iterator<Item = &OpenEntry> {
        self.open
            .iter()
            .filter(move |e| e.parent == parent && e.node.lvl == lvl)
    }

    /// The most recently opened node of `lvl` still open.
    #[must_use]
    pub fn deepest_open(&self, lvl: Lvl) -> Option<Node> {
        self.open
            .iter()
            .rev()
            .find(|e| e.node.lvl == lvl)
            .map(|e| e.node)
    }

    /// Whether this log issued `node`.
    #[must_use]
    pub fn issued(&self, node: Node) -> bool {
        match level_slot(node.lvl) {
            Some(i) => node.n >= 1 && node.n <= self.issued[i],
            None => node == Node::SESSION,
        }
    }

    /// Records after `cursor`, at most `max` of them (§5.1's pull cursor), and the gap the read crossed.
    #[must_use]
    pub fn read(&self, cursor: Cursor, max: usize) -> Read<'_> {
        let (gap, range, next) = self.page(cursor, max);
        Read {
            gap,
            recs: self.ring.range(range).map(|e| &*e.rec).collect(),
            next,
        }
    }

    /// [`Log::read`] handing out the ring's shared records: the same gap, records and next cursor.
    #[must_use]
    pub fn read_shared(&self, cursor: Cursor, max: usize) -> SharedRead<'_> {
        let (gap, range, next) = self.page(cursor, max);
        SharedRead {
            gap,
            recs: self.ring.range(range).map(|e| &e.rec).collect(),
            next,
        }
    }

    /// The page a read from `cursor` returns, as [`Log::read`] states it: the
    /// gap it crossed, the ring positions of the live records it takes (at
    /// most `max`), and where to read next.
    fn page(&self, cursor: Cursor, max: usize) -> (Option<Gap>, std::ops::Range<usize>, Cursor) {
        let head = self.reader_last_seq();
        let reset = cursor.ep > self.ep
            || cursor.ep < self.base_ep
            || cursor.seq > head
            || (cursor.ep < self.ep && cursor.seq.saturating_add(1) < self.first_seq);
        let from = if reset { 0 } else { cursor.seq };
        let start = self.ring.partition_point(|e| e.rec.seq <= from);
        let end = self.ring.partition_point(|e| e.rec.seq <= head).max(start);
        let range = start..end.min(start.saturating_add(max));
        let first = self.ring.get(range.start).filter(|_| !range.is_empty());
        let last = range.end.checked_sub(1).and_then(|i| self.ring.get(i));
        let next = match (max, last.filter(|_| !range.is_empty())) {
            (0, _) => self
                .ring
                .get(start)
                .filter(|e| e.rec.seq <= head)
                .map_or(head, |e| e.rec.seq.saturating_sub(1))
                .max(from),
            (_, Some(last)) if range.len() == max => last.rec.seq,
            _ => head,
        };
        let lost = self
            .held_between(from, next)
            .saturating_sub(self.live_between(from, next));
        let hole = self.crosses_hole(from, next);
        let gap = (reset || lost > 0 || hole.is_some()).then(|| Gap {
            why: if reset {
                GapWhy::Reset
            } else if let Some(why) = hole {
                why
            } else {
                GapWhy::Evicted
            },
            lost,
            resync_to: first.map_or(next, |e| e.rec.seq.saturating_sub(1)),
        });
        (gap, range, Cursor::new(self.ep, next))
    }

    /// Continue at `next_seq`: the seqs from the head up to it are ones this log never held (a carried update's overlap, in which the outgoing process may have gone on writing after its carry was taken).
    pub fn skip_to(&mut self, next_seq: u64) {
        if next_seq <= self.last_seq.saturating_add(1) {
            return;
        }
        self.holes
            .push_back((self.last_seq + 1, next_seq - 1, GapWhy::Handoff));
        self.last_seq = next_seq - 1;
        while self.holes.len() > MAX_HOLES {
            if let Some((_, hi, _)) = self.holes.pop_front() {
                self.first_seq = self.first_seq.max(hi.saturating_add(1));
            }
        }
        self.rang();
    }

    /// Open the session node (`open - spawn|adopt|restore`), once.
    /// # Errors
    /// [`Refused::AlreadyOpen`], [`Refused::SessionClosed`], [`Refused::KindDoesNotFit`] for a kind that is not a session's, or an encoding refusal.
    pub fn open_session(
        &mut self,
        kind: Kind,
        src: Src,
        conf: Conf,
        by: Actor,
        f: Fields,
        at: Stamp,
    ) -> Result<u64, Refused> {
        if self.session_closed {
            return Err(Refused::SessionClosed);
        }
        if self.session_opened {
            return Err(Refused::AlreadyOpen);
        }
        if !kind.fits(Op::Open, Lvl::Session) {
            return Err(Refused::KindDoesNotFit(kind));
        }
        let rec = Rec {
            seq: 0,
            t: at.wall_ms,
            op: Op::Open,
            node: Node::SESSION,
            par: None,
            k: kind,
            src,
            conf,
            by,
            f,
        };
        fits(&rec)?;
        self.session_opened = true;
        self.session_kind = kind;
        self.session_t = at.wall_ms;
        self.session_mono = at.mono_ms;
        Ok(self.push(rec, at.mono_ms))
    }

    /// Open a run, turn or step (§2.3).
    /// # Errors
    /// [`Refused::KindDoesNotFit`] for a kind that is not a run's, turn's or step's, [`Refused::TooManyOpen`], [`Refused::SessionClosed`], a field the log owns ([`log_owned`]), or an encoding or field refusal.
    pub fn open(&mut self, o: Open, at: Stamp) -> Result<Node, Refused> {
        if self.session_closed {
            return Err(Refused::SessionClosed);
        }
        let lvl = match o.kind.node_level() {
            Some(l @ (Lvl::Run | Lvl::Turn | Lvl::Step)) => l,
            _ => return Err(Refused::KindDoesNotFit(o.kind)),
        };
        refuse_owned(Op::Open, &o.f)?;
        if self.open.len() >= MAX_OPEN {
            return Err(Refused::TooManyOpen);
        }
        let slot = level_slot(lvl).ok_or(Refused::KindDoesNotFit(o.kind))?;
        let n = self.issued[slot]
            .checked_add(1)
            .ok_or(Refused::TooManyOpen)?;
        let (parent, orphan) = self.parent_for(lvl, o.under);
        let node = Node::new(lvl, n);
        let mut f = o.f;
        if orphan {
            f.set(Key::Orphan, Val::U(1))?;
        }
        let rec = Rec {
            seq: 0,
            t: at.wall_ms,
            op: Op::Open,
            node,
            par: Some(parent),
            k: o.kind,
            src: o.src,
            conf: o.conf,
            by: o.by,
            f,
        };
        fits(&rec)?;
        self.whole(|log| {
            if lvl == Lvl::Turn {
                let stale: Vec<Node> = log.open_under(parent, Lvl::Turn).map(|e| e.node).collect();
                for old in stale {
                    let end = Ender {
                        out: Out::Turn(TurnOut::Superseded),
                        src: o.src,
                        conf: o.conf,
                        by: o.by,
                        dur: None,
                    };
                    log.close_cascade_held(old, end, Fields::new(), at);
                }
            }
            log.issued[slot] = n;
            let cls = rec.f.get_word(Key::Cls);
            let continues = rec.f.get(Key::Cont).is_some();
            log.push(rec, at.mono_ms);
            if lvl == Lvl::Step && !continues {
                log.count_step(parent, o.kind, cls);
            }
            log.open.push(OpenEntry {
                node,
                kind: o.kind,
                parent,
                t: at.wall_ms,
                mono: at.mono_ms,
                orphan,
                roll: Rollup::default(),
                wall_clock: false,
            });
        });
        Ok(node)
    }

    /// Close `node` (§2.3): its open descendants first, innermost first, with `out=gone`, then the node with `c.out` and its rollup.
    /// # Errors
    /// [`Refused::NotOpen`] for a node that is not open (it closes exactly once), [`Refused::OutDoesNotFit`] for an outcome of another level, a field of `c` the log owns ([`log_owned`]: the outcome, duration and rollup are the log's to write), or an encoding or field refusal of `c`'s own fields (checked before anything is written).
    pub fn close(&mut self, node: Node, c: Close, at: Stamp) -> Result<u64, Refused> {
        if !self.is_open(node) {
            return Err(Refused::NotOpen(node));
        }
        if c.out.lvl() != node.lvl {
            return Err(Refused::OutDoesNotFit(node));
        }
        refuse_owned(Op::Close, &c.f)?;
        let kind = if node == Node::SESSION {
            self.session_kind
        } else {
            self.open_entry(node).map_or(Kind::Unknown, |e| e.kind)
        };
        let mut probe = Fields::new();
        probe.set(Key::Out, Val::Word(c.out.word()))?;
        probe.set(Key::DurMs, Val::U(u64::MAX))?;
        if matches!(c.dur, Some(Dur::Wall(_))) {
            probe.set(Key::Dur, Val::Word("wall"))?;
        }
        for (k, v) in c.f.iter() {
            probe.set(*k, v.clone())?;
        }
        fits(&Rec {
            seq: 0,
            t: at.wall_ms,
            op: Op::Close,
            node,
            par: None,
            k: kind,
            src: c.src,
            conf: c.conf,
            by: c.by,
            f: probe,
        })?;
        let end = Ender {
            out: c.out,
            src: c.src,
            conf: c.conf,
            by: c.by,
            dur: c.dur,
        };
        Ok(self.close_cascade(node, end, c.f, at))
    }

    /// Close the session and everything open under it.
    /// # Errors
    /// As [`Log::close`] on [`Node::SESSION`].
    pub fn close_session(&mut self, c: Close, at: Stamp) -> Result<u64, Refused> {
        self.close(Node::SESSION, c, at)
    }

    /// Attach a point fact (§2.1: to the deepest open node unless `m.at` names an open one).
    /// # Errors
    /// [`Refused::KindDoesNotFit`] for a kind that is no mark's, [`Refused::SessionClosed`], or an encoding refusal.
    pub fn mark(&mut self, m: Mark, at: Stamp) -> Result<u64, Refused> {
        if self.session_closed {
            return Err(Refused::SessionClosed);
        }
        if !m.kind.is_mark() {
            return Err(Refused::KindDoesNotFit(m.kind));
        }
        refuse_owned(Op::Mark, &m.f)?;
        let node = match m.at {
            Some(n) if self.is_open(n) => n,
            _ => self.open.last().map_or(Node::SESSION, |e| e.node),
        };
        let is_mail = m.kind == Kind::Mail;
        let rec = Rec {
            seq: 0,
            t: at.wall_ms,
            op: Op::Mark,
            node,
            par: None,
            k: m.kind,
            src: m.src,
            conf: m.conf,
            by: m.by,
            f: m.f,
        };
        fits(&rec)?;
        let seq = self.push(rec, at.mono_ms);
        if is_mail {
            self.for_open_lineage(node, |r| r.mail = r.mail.saturating_add(1));
        }
        Ok(seq)
    }

    /// Refine an issued node (open or closed) with more reliable fields (§2.3 "Amending"; the fold accepts them only at equal or higher confidence, §3.7).
    /// # Errors
    /// [`Refused::NotIssued`] for a node this log never issued, [`Refused::Forgotten`] for a closed node it no longer places, [`Refused::KindDoesNotFit`] for a kind of another level, or an encoding refusal.
    pub fn amend(&mut self, node: Node, a: Amend, at: Stamp) -> Result<u64, Refused> {
        if !self.issued(node) {
            return Err(Refused::NotIssued(node));
        }
        if !a.kind.fits(Op::Amend, node.lvl) {
            return Err(Refused::KindDoesNotFit(a.kind));
        }
        if !self.nodes.contains_key(&node) && self.structural_parent(node).is_none() {
            return Err(Refused::Forgotten(node));
        }
        refuse_owned(Op::Amend, &a.f)?;
        let rec = Rec {
            seq: 0,
            t: at.wall_ms,
            op: Op::Amend,
            node,
            par: None,
            k: a.kind,
            src: a.src,
            conf: a.conf,
            by: a.by,
            f: a.f,
        };
        fits(&rec)?;
        Ok(self.push(rec, at.mono_ms))
    }

    /// Record a loss (`gap - gap why=<why> lost=<n>`).
    pub fn gap(&mut self, why: GapWhy, lost: u64, at: Stamp) -> u64 {
        self.gap_built(why, lost, None, at)
    }

    /// [`Log::gap`], naming the build of the process the loss came across from (`build=<n>`, a handoff's or a crash adoption's predecessor) when `build` says it.
    pub fn gap_built(&mut self, why: GapWhy, lost: u64, build: Option<u64>, at: Stamp) -> u64 {
        let mut f = Fields::new();
        let _ = f.set(Key::Why, Val::Word(why.word()));
        if lost > 0 {
            let _ = f.set(Key::Lost, Val::U(lost));
        }
        if why == GapWhy::Handoff {
            let _ = f.set(Key::Epoch, Val::U(u64::from(self.ep)));
        }
        if let Some(build) = build {
            let _ = f.set(Key::Build, Val::U(build));
        }
        self.push(
            Rec {
                seq: 0,
                t: at.wall_ms,
                op: Op::Gap,
                node: Node::SESSION,
                par: None,
                k: Kind::Gap,
                src: Src::Aterm,
                conf: Conf::Exact,
                by: Actor::Aterm,
                f,
            },
            at.mono_ms,
        )
    }

    /// Whether a `lvl` node may hang from `parent` at all: a level above it,
    /// or its own level where that is natural (a run under a run, §2.1; a
    /// step under a step, §2.7).
    const fn may_hang_from(parent: Lvl, lvl: Lvl) -> bool {
        (parent as u8) < (lvl as u8) || natural_parent(parent, lvl)
    }

    /// The parent a new `lvl` node attaches to, and whether it is an orphan
    /// (§2.3): the named parent when it is open (an orphan when it is not a
    /// natural parent); else the nearest open ancestor of the named parent;
    /// else the deepest open natural parent; else the deepest open node
    /// above `lvl`; else the session.
    fn parent_for(&self, lvl: Lvl, under: Option<Node>) -> (Node, bool) {
        if let Some(p) = under {
            if self.is_open(p) && Log::may_hang_from(p.lvl, lvl) {
                return (p, !natural_parent(p.lvl, lvl));
            }
            if let Some(a) = self.nearest_open_ancestor(p, lvl) {
                return (a, true);
            }
        }
        if let Some(e) = self
            .open
            .iter()
            .rev()
            .find(|e| natural_parent(e.node.lvl, lvl))
        {
            return (e.node, under.is_some());
        }
        if let Some(e) = self.open.iter().rev().find(|e| e.node.lvl < lvl) {
            return (e.node, true);
        }
        (Node::SESSION, under.is_some() || lvl != Lvl::Run)
    }

    /// The parent of `n` as the open path and the last [`CLOSED_PARENTS`]
    /// closes know it: what an append places a node by.
    fn structural_parent(&self, n: Node) -> Option<Node> {
        self.open_entry(n).map(|e| e.parent).or_else(|| {
            self.closed_parents
                .iter()
                .rev()
                .find(|(c, _)| *c == n)
                .map(|(_, p)| *p)
        })
    }

    /// The nearest open ancestor of `n` a `lvl` node may hang from, by
    /// [`Log::structural_parent`]: the same whatever was evicted.
    fn nearest_open_ancestor(&self, n: Node, lvl: Lvl) -> Option<Node> {
        let mut at = self.structural_parent(n)?;
        for _ in 0..MAX_OPEN {
            if Log::may_hang_from(at.lvl, lvl) && self.is_open(at) {
                return Some(at);
            }
            if at == Node::SESSION {
                return None;
            }
            at = self.structural_parent(at)?;
        }
        None
    }

    /// Run `f` on the rollup of `node` and each open ancestor.
    fn for_open_lineage(&mut self, node: Node, mut f: impl FnMut(&mut Rollup)) {
        let mut at = Some(node);
        for _ in 0..=MAX_OPEN {
            let Some(n) = at else { break };
            if n == Node::SESSION {
                f(&mut self.session_roll);
                break;
            }
            let next = self.open_entry(n).map(|e| e.parent);
            if let Some(e) = self.open.iter_mut().find(|e| e.node == n) {
                f(&mut e.roll);
            }
            at = next;
        }
    }

    /// Count a new step in its ancestors' rollups.
    fn count_step(&mut self, parent: Node, kind: Kind, cls: Option<&'static str>) {
        let cls_slot = cls.and_then(|c| CLS_WORDS.iter().position(|w| *w == c));
        self.for_open_lineage(parent, |r| {
            r.steps = r.steps.saturating_add(1);
            match kind {
                Kind::Approval => r.appr = r.appr.saturating_add(1),
                Kind::Question => r.qs = r.qs.saturating_add(1),
                Kind::Wall => r.walls = r.walls.saturating_add(1),
                Kind::Stall => r.stalls = r.stalls.saturating_add(1),
                Kind::Tool => {
                    if let Some(i) = cls_slot {
                        r.tools[i] = r.tools[i].saturating_add(1);
                    }
                }
                _ => {}
            }
        });
    }

    /// Close `node`'s open descendants innermost first with `out=gone`,
    /// then `node` itself: the one path every close takes.
    fn close_cascade(&mut self, node: Node, end: Ender, f: Fields, at: Stamp) -> u64 {
        self.whole(|log| log.close_cascade_held(node, end, f, at))
    }

    /// [`Log::close_cascade`]'s records, its batch held by the caller.
    fn close_cascade_held(&mut self, node: Node, end: Ender, f: Fields, at: Stamp) -> u64 {
        let descendants: Vec<Node> = if node == Node::SESSION {
            self.open.iter().map(|e| e.node).collect()
        } else {
            self.open
                .iter()
                .filter(|e| self.descends_from(e.node, node))
                .map(|e| e.node)
                .collect()
        };
        for d in descendants.into_iter().rev() {
            let gone = Ender {
                out: Out::gone(d.lvl),
                dur: None,
                ..end
            };
            self.close_one(d, gone, Fields::new(), at);
        }
        self.close_one(node, end, f, at)
    }

    /// Whether `n` lies under `ancestor` among the open nodes.
    fn descends_from(&self, n: Node, ancestor: Node) -> bool {
        let mut at = n;
        for _ in 0..=MAX_OPEN {
            let Some(p) = self.open_entry(at).map(|e| e.parent) else {
                return false;
            };
            if p == ancestor {
                return true;
            }
            at = p;
        }
        false
    }

    /// Write one close record and forget the node.
    fn close_one(&mut self, node: Node, end: Ender, caller: Fields, at: Stamp) -> u64 {
        let Ender {
            out,
            src,
            conf,
            by,
            dur,
        } = end;
        let (kind, parent, mono, wall_from, roll) = if node == Node::SESSION {
            self.session_closed = true;
            (
                self.session_kind,
                None,
                self.session_mono,
                self.session_wall_clock.then_some(self.session_t),
                self.session_roll,
            )
        } else {
            let Some(i) = self.open.iter().position(|e| e.node == node) else {
                return self.last_seq;
            };
            let e = self.open.remove(i);
            (
                e.kind,
                Some(e.parent),
                e.mono,
                e.wall_clock.then_some(e.t),
                e.roll,
            )
        };
        let dur = dur.or_else(|| wall_from.map(|t| Dur::Wall(at.wall_ms.saturating_sub(t))));
        self.index_close(node, self.last_seq.saturating_add(1));
        let mut f = Fields::new();
        let _ = f.set(Key::Out, Val::Word(out.word()));
        let ms = match dur {
            Some(Dur::Mono(ms) | Dur::Wall(ms)) => ms,
            None => at.mono_ms.saturating_sub(mono),
        };
        let _ = f.set(Key::DurMs, Val::U(ms));
        if matches!(dur, Some(Dur::Wall(_))) {
            let _ = f.set(Key::Dur, Val::Word("wall"));
        }
        for (k, v) in caller.iter() {
            let _ = f.set(*k, v.clone());
        }
        let restated = self.restated_ledger(node, &f);
        let opener = self.restated_opener(node);
        let mut rec = Rec {
            seq: 0,
            t: at.wall_ms,
            op: Op::Close,
            node,
            par: None,
            k: kind,
            src,
            conf,
            by,
            f,
        };
        if let Some(ledger) = restated {
            let mut grown = rec.f.clone();
            if grown.set(Key::Ledger, Val::U(ledger)).is_ok() {
                let before = std::mem::replace(&mut rec.f, grown);
                if fits(&rec).is_err() {
                    rec.f = before;
                }
            }
        }
        for (k, v) in roll.fields() {
            let mut grown = rec.f.clone();
            if grown.set(k, v).is_err() {
                break;
            }
            let before = std::mem::replace(&mut rec.f, grown);
            if fits(&rec).is_err() {
                rec.f = before;
                break;
            }
        }
        if let Some(by) = opener {
            let mut grown = rec.f.clone();
            if grown.set(Key::OpBy, Val::Id(by)).is_ok() {
                let before = std::mem::replace(&mut rec.f, grown);
                if fits(&rec).is_err() {
                    rec.f = before;
                }
            }
        }
        if let Some(p) = parent {
            self.closed_parents.push_back((node, p));
            if self.closed_parents.len() > CLOSED_PARENTS {
                self.closed_parents.pop_front();
            }
            let failed = matches!(
                out,
                Out::Run(RunOut::Fail) | Out::Turn(TurnOut::Error) | Out::Step(StepOut::Error)
            );
            if failed {
                self.for_open_lineage(p, |r| r.errs = r.errs.saturating_add(1));
            }
        }
        self.push(rec, at.mono_ms)
    }

    /// A TURN'S CLOSE RESTATES ITS `ledger=`: the
    /// `ledger=` of turn `node`'s open record, which the ring still holds
    /// (an open node's open is pinned, and its close is not appended yet),
    /// unless the close's own fields `f` already name one.
    fn restated_ledger(&self, node: Node, f: &Fields) -> Option<u64> {
        if node.lvl != Lvl::Turn || f.get(Key::Ledger).is_some() {
            return None;
        }
        self.ring
            .iter()
            .rev()
            .find(|e| e.rec.node == node && e.rec.op == Op::Open)
            .and_then(|e| e.rec.f.get_u(Key::Ledger))
    }

    /// A TURN'S CLOSE RESTATES WHO OPENED IT:
    /// the `by=` of turn `node`'s open record, which the ring still holds (as
    /// [`Self::restated_ledger`] reads it), as the id `op_by=` takes, so a
    /// `brief` holding the close alone still names the turn's author.
    fn restated_opener(&self, node: Node) -> Option<ShortId> {
        if node.lvl != Lvl::Turn {
            return None;
        }
        let by = self
            .ring
            .iter()
            .rev()
            .find(|e| e.rec.node == node && e.rec.op == Op::Open)
            .map(|e| e.rec.by)?;
        match by.word() {
            "-" => None,
            word => ShortId::new(word),
        }
    }

    /// Append a record that fits, appended at the process clock's `mono`:
    /// assign its seq, apply it to the fold, index it, then evict a batch
    /// when the ring is past its limits.
    fn push(&mut self, mut rec: Rec, mono: u64) -> u64 {
        self.last_seq = self.last_seq.saturating_add(1);
        rec.seq = self.last_seq;
        let bytes = encoded_size(&rec, Tier::Content).unwrap_or(MAX_RECORD_BYTES);
        let claim = rec.src == Src::Mark;
        let program = (rec.op == Op::Mark && rec.k == Kind::Program).then_some(rec.node);
        let ms = if rec.is_bus_class() {
            self.ms_head = self.ms_head.saturating_add(1);
            self.ms_head
        } else {
            0
        };
        self.fold.apply(&rec);
        self.index_push(&rec, bytes);
        self.heap = self.heap.saturating_add(entry_heap(&rec));
        let row = self.tag_own(&rec);
        self.ring.push_back(Entry {
            rec: Arc::new(rec),
            bytes,
            ms,
            mono,
            row,
        });
        self.bytes = self.bytes.saturating_add(bytes);
        if claim {
            self.enforce_mark_quota();
        }
        if let Some(node) = program {
            self.enforce_program_share(Some(node));
        }
        if !self.hold_batch {
            self.settle_batch();
        }
        self.rang();
        self.last_seq
    }

    /// Run the eviction batch when the ring is past its limits.
    fn settle_batch(&mut self) {
        if over_limits(self.ring.len(), self.bytes, self.effective_limits()) {
            self.evict_batch();
        }
    }

    /// Run `f`, an operation of several records, with its eviction batch held
    /// until it is whole.
    fn whole<T>(&mut self, f: impl FnOnce(&mut Log) -> T) -> T {
        let held = std::mem::replace(&mut self.hold_batch, true);
        let out = f(self);
        self.hold_batch = held;
        if !held {
            self.settle_batch();
        }
        out
    }

    /// The index's view of `n`, for [`evict_class`].
    fn view(&self, n: Node) -> Option<NodeView> {
        self.nodes.get(&n).map(|i| NodeView {
            parent: i.parent,
            open: i.open,
            closed_below: i.closed_below,
        })
    }

    /// `n` and its ancestors in the index, nearest first (at most
    /// [`MAX_DEPTH`]).
    fn chain(&self, n: Node) -> Vec<Node> {
        let mut out = Vec::new();
        let mut at = n;
        for _ in 0..MAX_DEPTH {
            let Some(info) = self.nodes.get(&at) else {
                break;
            };
            out.push(at);
            if at == Node::SESSION || info.parent == Node::INSTANCE {
                break;
            }
            at = info.parent;
        }
        out
    }

    /// Index a record about to enter the ring: an open adds its node (with
    /// its parent, pinned bytes counted), and every record counts as live in
    /// its node's subtree and each ancestor's.
    fn index_push(&mut self, rec: &Rec, bytes: usize) {
        if rec.op == Op::Open && rec.node != Node::SESSION {
            self.nodes.insert(
                rec.node,
                NodeInfo {
                    parent: rec.par.unwrap_or(Node::SESSION),
                    open: true,
                    live: 0,
                    closed_below: 0,
                    open_bytes: bytes,
                },
            );
        } else {
            let mut at = rec.node;
            for _ in 0..MAX_DEPTH {
                if self.nodes.contains_key(&at) {
                    break;
                }
                let parent = self.structural_parent(at).unwrap_or(Node::INSTANCE);
                self.nodes.insert(
                    at,
                    NodeInfo {
                        parent,
                        open: false,
                        live: 0,
                        closed_below: 0,
                        open_bytes: 0,
                    },
                );
                if parent == Node::INSTANCE {
                    break;
                }
                at = parent;
            }
        }
        if rec.node == Node::SESSION && matches!(rec.op, Op::Open | Op::Close) {
            self.session_bytes = self.session_bytes.saturating_add(bytes);
        }
        if rec.op == Op::Close && matches!(rec.node.lvl, Lvl::Run | Lvl::Turn) {
            self.close_count += 1;
            self.close_bytes = self.close_bytes.saturating_add(bytes);
        }
        self.bump_chain(rec.node, true);
    }

    /// Count one live record more (or less) in `n`'s subtree and each
    /// ancestor's, walking the index without allocating.
    fn bump_chain(&mut self, n: Node, more: bool) {
        let mut at = n;
        for _ in 0..MAX_DEPTH {
            let Some(info) = self.nodes.get_mut(&at) else {
                return;
            };
            info.live = if more {
                info.live.saturating_add(1)
            } else {
                info.live.saturating_sub(1)
            };
            if at == Node::SESSION || info.parent == Node::INSTANCE {
                return;
            }
            at = info.parent;
        }
    }

    /// Mark `node` closed in the index at the close record `seq` (the close
    /// of a run or turn is newer than all detail of each ancestor written so
    /// far: [`EvictClass::ClosedDetail`]'s time rule), its open record no
    /// longer pinned.
    fn index_close(&mut self, node: Node, seq: u64) {
        let structural = matches!(node.lvl, Lvl::Run | Lvl::Turn);
        let ancestors: Vec<Node> = self.chain(node).into_iter().skip(1).collect();
        if let Some(info) = self.nodes.get_mut(&node) {
            info.open = false;
            info.open_bytes = 0;
        }
        if structural {
            for a in ancestors {
                if let Some(info) = self.nodes.get_mut(&a) {
                    info.closed_below = info.closed_below.max(seq);
                }
            }
        }
        self.forget_if_spent(node);
    }

    /// Drop `n` (and any ancestor left the same) from the index once it is
    /// closed and nothing in its subtree is live.
    fn forget_if_spent(&mut self, n: Node) {
        let mut at = n;
        for _ in 0..MAX_DEPTH {
            if at == Node::SESSION {
                return;
            }
            let Some(info) = self.nodes.get(&at) else {
                return;
            };
            if info.open || info.live > 0 {
                return;
            }
            let parent = info.parent;
            self.nodes.remove(&at);
            if parent == Node::INSTANCE {
                return;
            }
            at = parent;
        }
    }

    /// Count one lost record of `node` into the rollup of its nearest open
    /// ancestor-or-self (the session's, at the root): a closed node's rollup
    /// is already written.
    fn count_loss(&mut self, node: Node) {
        let mut at = node;
        for _ in 0..MAX_DEPTH {
            if at == Node::SESSION {
                break;
            }
            if let Some(e) = self.open.iter_mut().find(|e| e.node == at) {
                e.roll.lost = e.roll.lost.saturating_add(1);
                return;
            }
            match self.nodes.get(&at).map(|i| i.parent) {
                Some(p) if p != Node::INSTANCE => at = p,
                _ => break,
            }
        }
        self.session_roll.lost = self.session_roll.lost.saturating_add(1);
    }

    /// THE EVICTION BATCH (§5.1): classify every live record but the newest
    /// ([`evict_class`]) and take victims, oldest first within each class, in
    /// policy order until the ring is at its low-water mark ([`low_water`]):
    /// closed detail, the opens of closed runs and turns, closes while those
    /// pass half the budget ([`closes_over_half`]), current detail, then the
    /// remaining closes.
    fn evict_batch(&mut self) {
        let n = self.ring.len();
        if n <= 1 {
            return;
        }
        let lim = self.effective_limits();
        let low = low_water(lim);
        let newest = self.ring.back().map(|e| e.rec.node);
        let shelter: Vec<Node> = newest.map(|nd| self.chain(nd)).unwrap_or_default();
        let frozen = self.read_fence().unwrap_or(0);
        let mut buckets: [Vec<usize>; 4] = Default::default();
        for (i, e) in self.ring.iter().enumerate().take(n - 1) {
            if e.rec.seq <= frozen {
                continue;
            }
            let class = evict_class(&e.rec, |nd| self.view(nd));
            let slot = match class {
                EvictClass::ClosedDetail => 0,
                EvictClass::ClosedOpen => 1,
                EvictClass::CloseRecord if !shelter.contains(&e.rec.node) => 2,
                EvictClass::OpenDetail => 3,
                EvictClass::CloseRecord | EvictClass::Pinned => continue,
            };
            buckets[slot].push(i);
        }
        let mut victim = vec![false; n];
        let (mut len, mut bytes) = (n, self.bytes);
        let (mut closes, mut close_bytes) = (self.close_count, self.close_bytes);
        let entry_bytes: Vec<usize> = self.ring.iter().map(|e| e.bytes).collect();
        let seqs: Vec<u64> = self.ring.iter().map(|e| e.rec.seq).collect();
        let reached = |len: usize, bytes: usize| !over_limits(len, bytes, low);
        let tiers: &[Option<u64>] = match self.journaled {
            Some(j) => &[Some(j), None],
            None => &[None],
        };
        let floor = self.row_floor_members();
        let floors: &[bool] = if floor.contains(&true) {
            &[false, true]
        } else {
            &[false]
        };
        let passes = tiers
            .iter()
            .flat_map(|&tier| floors.iter().map(move |&in_floor| (in_floor, tier)));
        'policy: for (in_floor, tier) in passes {
            let open = |i: usize, victim: &[bool]| {
                !victim[i] && floor[i] == in_floor && tier.is_none_or(|j| seqs[i] <= j)
            };
            for slot in [0, 1] {
                for &i in &buckets[slot] {
                    if reached(len, bytes) {
                        break 'policy;
                    }
                    if open(i, &victim) {
                        victim[i] = true;
                        len -= 1;
                        bytes = bytes.saturating_sub(entry_bytes[i]);
                    }
                }
            }
            for &i in &buckets[2] {
                if reached(len, bytes) || !closes_over_half(closes, close_bytes, lim) {
                    break;
                }
                if open(i, &victim) {
                    victim[i] = true;
                    len -= 1;
                    bytes = bytes.saturating_sub(entry_bytes[i]);
                    closes -= 1;
                    close_bytes = close_bytes.saturating_sub(entry_bytes[i]);
                }
            }
            for slot in [3, 2] {
                for &i in &buckets[slot] {
                    if reached(len, bytes) {
                        break 'policy;
                    }
                    if open(i, &victim) {
                        victim[i] = true;
                        len -= 1;
                        bytes = bytes.saturating_sub(entry_bytes[i]);
                    }
                }
            }
        }
        if !victim.iter().any(|v| *v) {
            return;
        }
        let mut gone: Vec<Entry> = Vec::new();
        let old = std::mem::take(&mut self.ring);
        for (e, v) in old.into_iter().zip(victim) {
            if v {
                gone.push(e);
            } else {
                self.ring.push_back(e);
            }
        }
        for e in gone {
            self.evicted(&e);
        }
    }

    /// Account for one record leaving the ring.
    fn evicted(&mut self, e: &Entry) {
        self.bytes = self.bytes.saturating_sub(e.bytes);
        self.heap = self.heap.saturating_sub(entry_heap(&e.rec));
        self.lost = self.lost.saturating_add(1);
        self.newest_lost = self.newest_lost.max(e.rec.seq);
        if e.rec.op == Op::Close && matches!(e.rec.node.lvl, Lvl::Run | Lvl::Turn) {
            self.close_count = self.close_count.saturating_sub(1);
            self.close_bytes = self.close_bytes.saturating_sub(e.bytes);
        }
        self.count_loss(e.rec.node);
        self.bump_chain(e.rec.node, false);
        self.forget_if_spent(e.rec.node);
    }
}

/// Whether `rec` encodes within [`MAX_RECORD_BYTES`] whatever seq it is
/// given (the check runs with the widest seq, so a record that fits here
/// fits when it is appended), and, for a turn-class record the bus mirror
/// carries (§5.5), whether its `v=2` body fits too at the widest header
/// (`ep`, `seq` and `ms` at their maxima): every record the log accepts can
/// be mirrored.
fn fits(rec: &Rec) -> Result<(), Refused> {
    let mut probe = rec.clone();
    probe.seq = u64::MAX;
    encoded_size(&probe, Tier::Content).map_err(Refused::Encode)?;
    if probe.is_turn_class() {
        encode_bus(&probe, Tier::WireMeta, u32::MAX, Some(u64::MAX)).map_err(Refused::Encode)?;
    }
    Ok(())
}
