// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One session's log (design §2.3, §5.1, slice-1 form): a ring bounded by
//! BOTH a record count and an encoded size, dropping the oldest record and
//! counting every loss exactly, with the open/close discipline of §2.3
//! enforced by the API and the fold kept beside the ring.
//!
//! - **Opening.** A child opens only under an open parent; with none, it
//!   attaches to the nearest open ancestor and carries `orphan=1`. A parent
//!   is never invented.
//! - **Closing.** Closing a node first closes its open descendants,
//!   innermost first, with `out=gone`. A node closes exactly once. A new
//!   turn closes an unclosed turn under the same run with `out=superseded`.
//!   Every close carries its node's rollup.
//! - **Ids.** `r<n>`, `t<n>` and `p<n>` count per level and are never
//!   reused while the session lives: a successor continues the
//!   predecessor's counters ([`Log::after_handoff`], [`IdCounters`]).
//! - **Owned keys.** The log writes `orphan=` on an open and `out=`,
//!   `dur_ms=`, `dur=` and the rollup on a close; a caller's fields never
//!   carry them ([`Close::dur_ms`] and [`Close::dur_wall`] give a duration
//!   measured elsewhere, a block's stamps say).
//! - **Readers** pull with a [`Cursor`], the `aterm_buffer` contract: seqs
//!   are monotone, `since` is exclusive, and a reader behind the oldest live
//!   record gets a [`Gap`] with the exact count it lost and where to resync.
//! - **The fold** ([`Fold`]) is applied as each record is appended and lives
//!   outside the ring, so eviction never changes the current state.
//!
//! Slice 1 carries no records across a seamless update: the successor starts
//! a fresh log with [`Log::after_handoff`], whose first record is
//! `gap why=handoff`, and its seqs restart (§10; phase 2 carries `log_seq`).
//! A reader holding a cursor of the old epoch gets [`GapWhy::Reset`] and
//! drops it, which needs the reader's epoch: see [`Log::read`].

use std::collections::VecDeque;

use crate::field::{FieldError, Fields, Key, Tier, Val};
use crate::fold::{Fold, SessionState};
use crate::line::{EncodeError, MAX_RECORD_BYTES, encode_bus, encoded_size};
use crate::rec::{Rec, Stamp};
use crate::vocab::{Actor, CLS_WORDS, Conf, Kind, Lvl, Node, Op, Src};
use crate::words::{Out, RunOut, StepOut, TurnOut};

/// A session ring's record bound (§5.1).
pub const MAX_RECORDS: usize = 2048;
/// A session ring's encoded-size bound (§5.1): 256 KiB.
pub const MAX_BYTES: usize = 256 * 1024;
/// The most nodes open at once; a deeper open is refused.
pub const MAX_OPEN: usize = 64;
/// How many closed nodes' parents the log remembers, to find a closed
/// parent's nearest open ancestor.
const CLOSED_PARENTS: usize = 64;

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

    /// Bounds of at least one record and one largest record's bytes (so the
    /// record just appended is never the one evicted), or `None`.
    #[must_use]
    pub const fn new(records: usize, bytes: usize) -> Option<Limits> {
        if records == 0 || bytes < MAX_RECORD_BYTES {
            None
        } else {
            Some(Limits { records, bytes })
        }
    }
}

/// Whether a ring holding `records` records of `bytes` encoded bytes is past
/// `limits`: THE eviction policy (drop the oldest while this holds).
#[must_use]
pub const fn over_limits(records: usize, bytes: usize, limits: Limits) -> bool {
    records > limits.records || bytes > limits.bytes
}

/// Why an append was refused. A refused append changes nothing.
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
}

impl From<FieldError> for Refused {
    fn from(e: FieldError) -> Refused {
        Refused::Field(e)
    }
}

/// What opens a node. The level is the kind's ([`Kind::node_level`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Open {
    /// The node's kind.
    pub kind: Kind,
    /// The parent asked for; `None` is the deepest open node that is a
    /// natural parent of the kind's level.
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
    pub fn new(kind: Kind, src: Src, conf: Conf) -> Open {
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
    pub fn under(mut self, parent: Node) -> Open {
        self.under = Some(parent);
        self
    }

    /// Attributed to `by`.
    #[must_use]
    pub fn by(mut self, by: Actor) -> Open {
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

/// A duration the host measured itself, in place of the log's own
/// monotonic difference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dur {
    /// Measured on one process's monotonic clock (`dur_ms=`).
    Mono(u64),
    /// Computed from wall stamps, such as a shell block's (`dur_ms= dur=wall`,
    /// §3.5).
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
    /// The duration, when the host measured it (a block the sweep saw only
    /// complete has its own stamps); `None` is the log's monotonic
    /// difference since the open.
    pub dur: Option<Dur>,
    /// Fields beside the outcome, duration and rollup (`code=`, `blk=`).
    /// The keys the log owns ([`Log::close`]) are refused here.
    pub f: Fields,
}

impl Close {
    /// A close with `out`, no attribution, no fields.
    #[must_use]
    pub fn new(out: Out, src: Src, conf: Conf) -> Close {
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
    pub fn by(mut self, by: Actor) -> Close {
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
    pub fn dur_ms(mut self, ms: u64) -> Close {
        self.dur = Some(Dur::Mono(ms));
        self
    }

    /// With a duration computed from wall stamps (written `dur=wall`): a run
    /// rebuilt from a shell block passes the block's end stamp as the close's
    /// `at` and its start-to-end wall difference here.
    #[must_use]
    pub fn dur_wall(mut self, ms: u64) -> Close {
        self.dur = Some(Dur::Wall(ms));
        self
    }
}

/// Whether the log writes `key` itself on a record of `op`: `orphan=` on an
/// open; the outcome, duration and rollup on a close. A caller's fields
/// carrying one are refused, so a close always says what the log decided.
#[must_use]
pub fn log_owned(op: Op, key: Key) -> bool {
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
    /// The node it attaches to; `None` (or a node not open) is the deepest
    /// open node.
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
    pub fn new(kind: Kind, src: Src, conf: Conf) -> Mark {
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
    pub fn at(mut self, node: Node) -> Mark {
        self.at = Some(node);
        self
    }

    /// Attributed to `by`.
    #[must_use]
    pub fn by(mut self, by: Actor) -> Mark {
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
    pub fn new(kind: Kind, src: Src, conf: Conf, f: Fields) -> Amend {
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
    pub fn by(mut self, by: Actor) -> Amend {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GapWhy {
    /// A seamless update carried nothing (slice 1): seqs restarted.
    Handoff,
    /// The previous process died.
    Crash,
    /// Journal segments were pruned.
    Pruned,
    /// A ring evicted them: the log's own, or the history a producer reads
    /// (the terminal's block list) before the producer reached them.
    Evicted,
    /// The cursor belongs to another epoch (or is ahead of the log).
    Reset,
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
        }
    }
}

/// A reader's position: the epoch it read and the last seq it holds.
/// `since` is exclusive: a read returns records after `seq`.
///
/// `ep` must be the epoch the reader was last told (a reply's `ep=`), never
/// the log's own: in slice 1 seqs restart at an update, so a seq alone
/// cannot say which epoch it counts. A verb that takes `since=` takes `ep=`
/// beside it.
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

/// What a reader that fell behind (or holds another epoch's cursor) is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    /// Evicted, or the cursor was reset.
    pub why: GapWhy,
    /// Exactly how many records the reader will never see.
    pub lost: u64,
    /// The seq to resume after (the records returned begin past it).
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
}

/// The last node id issued per numbered level. A successor's log continues
/// them ([`Log::after_handoff`]), so `t17` names one turn for as long as the
/// session lives (§2.2), across an update that carries no records.
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

#[derive(Clone, Debug)]
struct Entry {
    rec: Rec,
    bytes: usize,
}

/// One session's log.
#[derive(Clone, Debug)]
pub struct Log {
    ep: u32,
    limits: Limits,
    ring: VecDeque<Entry>,
    bytes: usize,
    last_seq: u64,
    lost: u64,
    /// The last id issued per numbered level: run, turn, step.
    issued: [u32; 3],
    open: Vec<OpenEntry>,
    closed_parents: VecDeque<(Node, Node)>,
    session_opened: bool,
    session_closed: bool,
    session_t: u64,
    session_mono: u64,
    session_kind: Kind,
    session_roll: Rollup,
    fold: Fold,
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

/// Whether a node of `parent` level is the natural parent of a `child`
/// level node: a run under the session or a run, a turn under a run, a step
/// under a turn or a step.
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
        Log {
            ep,
            limits,
            ring: VecDeque::new(),
            bytes: 0,
            last_seq: 0,
            lost: 0,
            issued: [0; 3],
            open: Vec::new(),
            closed_parents: VecDeque::new(),
            session_opened: false,
            session_closed: false,
            session_t: 0,
            session_mono: 0,
            session_kind: Kind::Spawn,
            session_roll: Rollup::default(),
            fold: Fold::new(ep),
        }
    }

    /// The successor's log after a seamless update that carried no records
    /// (slice 1): epoch `ep`, its first record `gap - gap why=handoff
    /// epoch=<ep>`, its node ids continuing `ids`, the predecessor's
    /// [`Log::id_counters`] (three integers the update carries beside the
    /// timeline's id). A reader with a cursor of the old epoch is reset.
    #[must_use]
    pub fn after_handoff(ep: u32, limits: Limits, ids: IdCounters, at: Stamp) -> Log {
        let mut log = Log::with_limits(ep, limits);
        log.issued = ids.slots();
        log.gap(GapWhy::Handoff, 0, at);
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

    // ---- reading -----------------------------------------------------

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

    /// Records evicted, exactly.
    #[must_use]
    pub const fn lost(&self) -> u64 {
        self.lost
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
    pub fn window(&self) -> impl Iterator<Item = &Rec> {
        self.ring.iter().map(|e| &e.rec)
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

    /// Records after `cursor`, at most `max` of them (§5.1's pull cursor).
    /// A cursor behind the oldest live record gets a [`Gap`] with the exact
    /// count it lost; a cursor of another epoch, or ahead of the log, gets
    /// [`GapWhy::Reset`] and the epoch's records from its oldest live one.
    ///
    /// The host passes the READER's epoch in `cursor.ep` (the `ep=` the
    /// reader was last told), never [`Log::ep`]: once a successor's seqs
    /// pass an old `since=`, the epoch is the only thing that tells the old
    /// cursor from a current one, so a `log`, `brief` or `subscribe` that
    /// takes `since=` takes `ep=` with it. A reader that names no epoch is
    /// one that has read nothing (`since=0`).
    #[must_use]
    pub fn read(&self, cursor: Cursor, max: usize) -> Read<'_> {
        let oldest = self.oldest_seq();
        let (gap, from) = if cursor.ep != self.ep || cursor.seq > self.last_seq {
            let behind = oldest.saturating_sub(1);
            (
                Some(Gap {
                    why: GapWhy::Reset,
                    lost: behind,
                    resync_to: behind,
                }),
                oldest,
            )
        } else if cursor.seq.saturating_add(1) < oldest {
            let behind = oldest - 1;
            (
                Some(Gap {
                    why: GapWhy::Evicted,
                    lost: behind - cursor.seq,
                    resync_to: behind,
                }),
                oldest,
            )
        } else {
            (None, cursor.seq.saturating_add(1))
        };
        let skip = usize::try_from(from.saturating_sub(oldest)).unwrap_or(usize::MAX);
        let recs: Vec<&Rec> = self
            .ring
            .iter()
            .skip(skip)
            .take(max)
            .map(|e| &e.rec)
            .collect();
        let held = recs
            .last()
            .map(|r| r.seq)
            .or(gap.map(|g| g.resync_to))
            .unwrap_or(cursor.seq);
        Read {
            gap,
            recs,
            next: Cursor::new(self.ep, held),
        }
    }

    // ---- writing -----------------------------------------------------

    /// Open the session node (`open - spawn|adopt|restore`), once.
    ///
    /// # Errors
    ///
    /// [`Refused::AlreadyOpen`], [`Refused::SessionClosed`],
    /// [`Refused::KindDoesNotFit`] for a kind that is not a session's, or an
    /// encoding refusal.
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
        Ok(self.push(rec))
    }

    /// Open a run, turn or step (§2.3). Returns the new node.
    ///
    /// A step that carries `cont=<step>` continues that step (a wall that
    /// outlived its turn reopens on the run): it is the same fact, so no
    /// rollup counts it again.
    ///
    /// # Errors
    ///
    /// [`Refused::KindDoesNotFit`] for a kind that is not a run's, turn's or
    /// step's, [`Refused::TooManyOpen`], [`Refused::SessionClosed`], a field
    /// the log owns ([`log_owned`]), or an encoding or field refusal. A
    /// refused open changes nothing.
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
        if lvl == Lvl::Turn {
            let stale: Vec<Node> = self.open_under(parent, Lvl::Turn).map(|e| e.node).collect();
            for old in stale {
                let end = Ender {
                    out: Out::Turn(TurnOut::Superseded),
                    src: o.src,
                    conf: o.conf,
                    by: o.by,
                    dur: None,
                };
                self.close_cascade(old, end, Fields::new(), at);
            }
        }
        self.issued[slot] = n;
        let cls = rec.f.get_word(Key::Cls);
        let continues = rec.f.get(Key::Cont).is_some();
        self.push(rec);
        if lvl == Lvl::Step && !continues {
            self.count_step(parent, o.kind, cls);
        }
        self.open.push(OpenEntry {
            node,
            kind: o.kind,
            parent,
            t: at.wall_ms,
            mono: at.mono_ms,
            orphan,
            roll: Rollup::default(),
        });
        Ok(node)
    }

    /// Close `node` (§2.3): its open descendants first, innermost first,
    /// with `out=gone`, then the node with `c.out` and its rollup. Closing
    /// the session closes everything. Returns the close record's seq.
    ///
    /// # Errors
    ///
    /// [`Refused::NotOpen`] for a node that is not open (it closes exactly
    /// once), [`Refused::OutDoesNotFit`] for an outcome of another level, a
    /// field of `c` the log owns ([`log_owned`]: the outcome, duration and
    /// rollup are the log's to write), or an encoding or field refusal of
    /// `c`'s own fields (checked before anything is written).
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
        // The close's own fields must fit before anything is written: the
        // rollup is added after, only as room allows.
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
    ///
    /// # Errors
    ///
    /// As [`Log::close`] on [`Node::SESSION`].
    pub fn close_session(&mut self, c: Close, at: Stamp) -> Result<u64, Refused> {
        self.close(Node::SESSION, c, at)
    }

    /// Attach a point fact (§2.1: to the deepest open node unless `m.at`
    /// names an open one). Returns its seq.
    ///
    /// # Errors
    ///
    /// [`Refused::KindDoesNotFit`] for a kind that is no mark's,
    /// [`Refused::SessionClosed`], or an encoding refusal.
    pub fn mark(&mut self, m: Mark, at: Stamp) -> Result<u64, Refused> {
        if self.session_closed {
            return Err(Refused::SessionClosed);
        }
        if !m.kind.is_mark() {
            return Err(Refused::KindDoesNotFit(m.kind));
        }
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
        let seq = self.push(rec);
        if is_mail {
            self.for_open_lineage(node, |r| r.mail = r.mail.saturating_add(1));
        }
        Ok(seq)
    }

    /// Refine an issued node (open or closed) with more reliable fields
    /// (§2.3 "Amending"; the fold accepts them only at equal or higher
    /// confidence, §3.7). Returns its seq.
    ///
    /// # Errors
    ///
    /// [`Refused::NotIssued`] for a node this log never issued,
    /// [`Refused::KindDoesNotFit`] for a kind of another level, or an
    /// encoding refusal.
    pub fn amend(&mut self, node: Node, a: Amend, at: Stamp) -> Result<u64, Refused> {
        if !self.issued(node) {
            return Err(Refused::NotIssued(node));
        }
        if !a.kind.fits(Op::Amend, node.lvl) {
            return Err(Refused::KindDoesNotFit(a.kind));
        }
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
        Ok(self.push(rec))
    }

    /// Record a loss (`gap - gap why=<why> lost=<n>`). Returns its seq.
    pub fn gap(&mut self, why: GapWhy, lost: u64, at: Stamp) -> u64 {
        let mut f = Fields::new();
        // Every value below is of its key's shape and the record is far
        // under the byte bound, so none of these can be refused.
        let _ = f.set(Key::Why, Val::Word(why.word()));
        if lost > 0 {
            let _ = f.set(Key::Lost, Val::U(lost));
        }
        if why == GapWhy::Handoff {
            let _ = f.set(Key::Epoch, Val::U(u64::from(self.ep)));
        }
        self.push(Rec {
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
        })
    }

    // ---- internals ---------------------------------------------------

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

    /// The parent of `n`, open or recently closed.
    fn parent_of(&self, n: Node) -> Option<Node> {
        self.open_entry(n).map(|e| e.parent).or_else(|| {
            self.closed_parents
                .iter()
                .rev()
                .find(|(c, _)| *c == n)
                .map(|(_, p)| *p)
        })
    }

    /// The nearest open ancestor of `n` a `lvl` node may hang from.
    fn nearest_open_ancestor(&self, n: Node, lvl: Lvl) -> Option<Node> {
        let mut at = self.parent_of(n)?;
        for _ in 0..MAX_OPEN {
            if Log::may_hang_from(at.lvl, lvl) && self.is_open(at) {
                return Some(at);
            }
            if at == Node::SESSION {
                return None;
            }
            at = self.parent_of(at)?;
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
    /// then `node` itself: the one path every close takes. Each record is
    /// built to fit (the rollup is added only as room allows), so a
    /// cascade is never refused half-way.
    fn close_cascade(&mut self, node: Node, end: Ender, f: Fields, at: Stamp) -> u64 {
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
        let (kind, parent, mono, roll) = if node == Node::SESSION {
            self.session_closed = true;
            (
                self.session_kind,
                None,
                self.session_mono,
                self.session_roll,
            )
        } else {
            let Some(i) = self.open.iter().position(|e| e.node == node) else {
                return self.last_seq;
            };
            let e = self.open.remove(i);
            (e.kind, Some(e.parent), e.mono, e.roll)
        };
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
        // `close` refused any key the log owns, so these never override the
        // outcome or the duration above.
        for (k, v) in caller.iter() {
            let _ = f.set(*k, v.clone());
        }
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
        self.push(rec)
    }

    /// Append a record that fits: assign its seq, apply it to the fold,
    /// then evict the oldest while the ring is past its limits.
    fn push(&mut self, mut rec: Rec) -> u64 {
        self.last_seq = self.last_seq.saturating_add(1);
        rec.seq = self.last_seq;
        let bytes = encoded_size(&rec, Tier::Content).unwrap_or(MAX_RECORD_BYTES);
        self.fold.apply(&rec);
        self.ring.push_back(Entry { rec, bytes });
        self.bytes = self.bytes.saturating_add(bytes);
        self.evict();
        self.last_seq
    }

    /// Drop the oldest record while the ring is past its limits, counting
    /// each loss in the log and in its node's rollup while that is open.
    fn evict(&mut self) {
        while self.ring.len() > 1 && over_limits(self.ring.len(), self.bytes, self.limits) {
            let Some(old) = self.ring.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(old.bytes);
            self.lost = self.lost.saturating_add(1);
            let node = old.rec.node;
            if node == Node::SESSION {
                self.session_roll.lost = self.session_roll.lost.saturating_add(1);
            } else if let Some(e) = self.open.iter_mut().find(|e| e.node == node) {
                e.roll.lost = e.roll.lost.saturating_add(1);
            }
        }
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
