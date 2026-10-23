// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUS MIRROR'S ENGINE HALF (design §5.5, phase 4): what a take of a
//! log's bus-class records is, where the bus stands for the log, and the
//! bodies a mirror publishes. Pure: no clock, no I/O; the host takes a copy
//! under its timeline lock ([`Log::bus_copy`]), encodes it after the lock is
//! released ([`BusCopy::encode`]), and moves the cursor only when the bridge
//! says the slots are on the bus ([`Log::land`]).
//!
//! - **`ms`** numbers the bus-class records of one seq space densely from 1,
//!   at append ([`Log::ms_head`]), whether or not anything mirrors. A take
//!   derives every loss from `ms` alone: the next live bus-class
//!   record's `ms` past the cursor's next is a hole, said by ONE cover body
//!   per hole with its exact count (`op=gap why=evicted n=<k>`); the ring's
//!   own gap records are records like any other. A loss of local-class
//!   records only is no loss to the bus, which never held them.
//! - **The cursor** ([`BusCursor`], one name for it) is `(space,
//!   seq, ms)`: the seq space (the log's [`Log::base_ep`], which a carried
//!   update continues), the seq and the `ms` the bus holds everything up to.
//!   It moves only by [`Log::land`], which refuses while the readers are
//!   fenced: an update's export drew the cursor the successor
//!   continues, so no landing may pass it until the update commits or stands
//!   down. Under a fence a take stops at the fence's seq and its `ms` head,
//!   so a tail cover never covers an `ms` the successor will
//!   number again.
//! - **Off is nothing.** A log no bridge ever took from holds no mirror
//!   state ([`Log::mirror_landed`] is `None`): its carry, its ring and every
//!   reply are what they were before phase 4. The cost is heap
//!   alone: the 8-byte `ms` a record ([`ENTRY_BYTES`]) and 24 bytes a log
//!   (the `ms` head, the head at the fence and the mirror's empty box,
//!   [`LOG_MIRROR_BYTES`]). [`Log::footprint`] counts both, so an instance
//!   AT its session-log memory cap gives each log a byte budget smaller by
//!   them and evicts that much sooner than a build without phase 4; below
//!   the cap nothing differs.
//! - **Snapshots** are synthesized by a take, never stored: at
//!   a space's first take and each [`SNAP_EVERY`] bus-class records, a take
//!   that reaches the head adds one `op=snap` body holding the open path and
//!   the open needs, of the bus class only, the mail counts,
//!   and `upto=`, the `ms` head it folds. `n=` is the original bus-needs
//!   count: a seeded reader can detect words omitted at the field cap.
//!   It counts as given only once a
//!   landing reaches its slot ([`Log::land`]): a take the bridge dropped
//!   unlanded (a subject budget's refusal) leaves the next take owing it,
//!   so a reader of a view that lost records meanwhile still gets the seed
//!   past them. A snap is a seed for a reader that
//!   joined late or lost records, not a record: it carries no `ms`, and a
//!   reader seeds from it only a view whose every cover lies at or below its
//!   `upto=` (`aterm_slog::bus::BusSpace::fold_seeded`).
//! - **Identity on the bus** is `(node, sid, ep, seq)` for a record. Covers
//!   are coverage, unioned by a reader, and snaps seeds; neither is deduped
//!   by identity (the reader's view is the law, so a body the bus
//!   holds twice is shown once).

use super::{Entry, Log, entry_heap};
use crate::field::{BoundedToken, Fields, Key, Tier, Val};
use crate::line::{encode_bus, encode_mirror};
use crate::rec::{Rec, Stamp};
use crate::vocab::{Actor, Conf, Kind, Node, Op, ShortId, Src};

/// The most slots one take answers (`log-take n=` is capped at it; the bridge asks half of its pipeline window).
pub const TAKE_MAX: usize = 64;

/// A snapshot every this many bus-class records (§5.5): a reader joining at
/// any offset needs at most this many earlier bodies of the session.
pub const SNAP_EVERY: u64 = 64;

/// One ring entry's size: the shared record's pointer, its encoded bytes, its
/// `ms` and its process-clock reading. What the instance's heap charge counts
/// per record beside the shared record itself ([`Log::footprint`]); phase 4
/// grew it by exactly the 8 bytes of `ms`, mirror on or off, and Phase 6 (6F-1)
/// by the 8 bytes of the record's `mono`.
pub const ENTRY_BYTES: usize = std::mem::size_of::<Entry>();

/// What phase 4 added to every [`Log`]'s own size, mirror on or off: the
/// bus class's `ms` head, the head at the readers' fence and the mirror's
/// box (empty while nothing mirrors). [`Log::footprint`] counts it.
pub const LOG_MIRROR_BYTES: usize =
    2 * std::mem::size_of::<u64>() + std::mem::size_of::<Option<Box<Mirror>>>();

/// Where the bus holds a session's log up to: its seq space, the seq and
/// the bus-class count `ms`. A slot's cursor is where the session lands once
/// that slot and every slot before it are on the bus. `<ep>:<seq>:<ms>` on
/// the wire.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BusCursor {
    /// The seq space ([`Log::base_ep`]).
    pub ep: u32,
    /// The seq.
    pub seq: u64,
    /// The bus-class count: every `ms` at or below it is on the bus.
    pub ms: u64,
}

impl BusCursor {
    /// A cursor.
    #[must_use]
    pub const fn new(ep: u32, seq: u64, ms: u64) -> BusCursor {
        BusCursor { ep, seq, ms }
    }

    /// `<ep>:<seq>:<ms>`.
    #[must_use]
    pub fn word(&self) -> String {
        format!("{}:{}:{}", self.ep, self.seq, self.ms)
    }

    /// A cursor's word read back, or `None`.
    #[must_use]
    pub fn parse(s: &str) -> Option<BusCursor> {
        let mut it = s.split(':');
        let ep = it.next()?.parse().ok()?;
        let seq = it.next()?.parse().ok()?;
        let ms = it.next()?.parse().ok()?;
        it.next().is_none().then_some(BusCursor { ep, seq, ms })
    }
}

/// A seq space an update that carried no log ended: where the
/// bus held it and its `ms` head, which the successor's first take says the
/// rest of as one `gap why=reset n=<k>` in that space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Prior {
    /// Where the bus held the old space.
    pub landed: BusCursor,
    /// The old space's bus-class count.
    pub ms_head: u64,
}

/// The mirror's state in a log, once a take engaged it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Mirror {
    /// Where the bus holds this space up to.
    pub(super) landed: BusCursor,
    /// The `ms` head the last snapshot ON THE BUS was taken at (0: none in
    /// this space): one counts once its landing reaches it ([`Log::land`]).
    pub(super) snap_ms: u64,
    /// The space an uncarried update ended, while its tail is owed.
    pub(super) prior: Option<Prior>,
    /// The snapshot the take in flight handed out (its slot's cursor and its
    /// `upto=`), until its landing reaches it. Each take replaces it (one
    /// batch is in flight): a take the bridge dropped unlanded (a subject
    /// budget's refusal) leaves the next one owing its snapshot again.
    pub(super) snap_owed: Option<(BusCursor, u64)>,
}

impl Mirror {
    /// A landing at `at` reached the snapshot in flight: it is on the bus.
    fn land_snap(&mut self, at: BusCursor) {
        if let Some((c, upto)) = self.snap_owed
            && at.ep == c.ep
            && (at.ms, at.seq) >= (c.ms, c.seq)
        {
            self.snap_ms = upto;
            self.snap_owed = None;
        }
    }
}

/// Why a cover body says records are not on the bus (`why=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CoverWhy {
    /// The ring evicted them before a take reached them.
    Evicted,
    /// An update carried no log: the old space's tail.
    Reset,
    /// Dropped whole: a closed session's tail past the retired bound, a
    /// record whose body does not fit, an old space nothing carried.
    Dropped,
}

impl CoverWhy {
    /// The `why=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            CoverWhy::Evicted => "evicted",
            CoverWhy::Reset => "reset",
            CoverWhy::Dropped => "dropped",
        }
    }
}

/// What a slot's body is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotKind {
    /// A record of the bus class.
    Record,
    /// A cover: `op=gap … n=<k>`, `ms=` the first of the `k` it covers.
    Cover,
    /// A snapshot: a late reader's seed.
    Snap,
}

/// A snapshot's facts, copied under the lock.
#[derive(Clone, Debug)]
struct SnapFacts {
    seq: u64,
    upto: u64,
    open: Vec<(Node, Kind)>,
    needs: Vec<(&'static str, Node, Kind, Actor, Src)>,
    mail_in: u32,
    mail_out: u32,
    ep_now: u32,
}

#[derive(Clone, Debug)]
enum Item {
    Rec {
        rec: std::sync::Arc<Rec>,
        ms: u64,
    },
    Cover {
        ep: u32,
        seq: u64,
        lo: u64,
        n: u64,
        why: CoverWhy,
    },
    Snap(SnapFacts),
}

/// A take copied under the timeline's lock ([`Log::bus_copy`]), encoded
/// after it is released ([`BusCopy::encode`]).
#[derive(Clone, Debug)]
pub struct BusCopy {
    space: u32,
    t: u64,
    items: Vec<(Item, BusCursor)>,
    more: bool,
}

impl BusCopy {
    /// How many slots it holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether the log holds more past it (the take stopped at its room).
    #[must_use]
    pub const fn more(&self) -> bool {
        self.more
    }

    /// The slots' bodies at `tier` (held at [`Tier::WireMeta`]: the bus never
    /// carries an H or C key), an edge's sid written as `alias` names it
    /// ([`encode_mirror`]). A record whose body does not fit is one
    /// `why=dropped n=1` cover in its place, so its `ms` is still said.
    #[must_use]
    pub fn encode(self, tier: Tier, alias: &dyn Fn(&ShortId) -> Option<ShortId>) -> Take {
        let mut slots = Vec::with_capacity(self.items.len());
        for (item, at) in self.items {
            let (kind, body) = match item {
                Item::Rec { rec, ms } => {
                    match encode_mirror(&rec, tier, self.space, Some(ms), alias) {
                        Ok(body) => (SlotKind::Record, body),
                        Err(_) => (
                            SlotKind::Cover,
                            cover_body(self.space, rec.seq, ms, 1, CoverWhy::Dropped, self.t),
                        ),
                    }
                }
                Item::Cover {
                    ep,
                    seq,
                    lo,
                    n,
                    why,
                } => (SlotKind::Cover, cover_body(ep, seq, lo, n, why, self.t)),
                Item::Snap(s) => (
                    SlotKind::Snap,
                    snap_body(&s, tier, self.space, self.t, alias),
                ),
            };
            slots.push(Slot { at, kind, body });
        }
        Take {
            slots,
            more: self.more,
        }
    }
}

/// One slot of a take: its body and the cursor the session lands at once it
/// and every slot before it are on the bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    /// Where the session lands.
    pub at: BusCursor,
    /// What the body is.
    pub kind: SlotKind,
    /// The `v=2` body.
    pub body: String,
}

/// An encoded take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Take {
    /// The slots, oldest first.
    pub slots: Vec<Slot>,
    /// Whether the log holds more past them.
    pub more: bool,
}

/// What a landing did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Landed {
    /// The cursor moved.
    Moved,
    /// The cursor was already there or past it, or names another space: on
    /// the bus already, nothing moved.
    Stale,
}

/// Why a landing was refused (nothing moved).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandRefused {
    /// The readers are fenced (an update's export drew the cursor): retry
    /// once the update commits or stands down.
    Fenced,
    /// Past anything this log numbered: not a cursor of this log's.
    Ahead,
}

/// A cover body (`op=gap why=<w> n=<k>`, `ms=` the first `ms` it covers): the
/// statement that the `k` bus-class records from `lo` of space `ep` are not
/// on the bus, and never will be.
#[must_use]
pub fn cover_body(ep: u32, seq: u64, lo: u64, n: u64, why: CoverWhy, t: u64) -> String {
    let mut f = Fields::new();
    // Each value is of its key's shape and the record far under the byte
    // bound: none of these can be refused.
    let _ = f.set(Key::Why, Val::Word(why.word()));
    let _ = f.set(Key::N, Val::U(n));
    let rec = Rec {
        seq,
        t,
        op: Op::Gap,
        node: Node::SESSION,
        par: None,
        k: Kind::Gap,
        src: Src::Aterm,
        conf: Conf::Exact,
        by: Actor::Aterm,
        f,
    };
    encode_bus(&rec, Tier::Wire, ep, Some(lo)).unwrap_or_default()
}

/// Whether a step of `kind` is of the bus class (a need is raised from it).
#[must_use]
pub const fn is_bus_step(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Approval | Kind::Question | Kind::Wall | Kind::Stall
    )
}

/// An actor as the bus names it: an edge's sid by its alias, `-` with none.
fn bus_actor(by: Actor, alias: &dyn Fn(&ShortId) -> Option<ShortId>) -> Actor {
    match by {
        Actor::Sid(sid) => alias(&sid).map_or(Actor::Nobody, Actor::sid),
        other => other,
    }
}

fn snap_body(
    s: &SnapFacts,
    tier: Tier,
    space: u32,
    t: u64,
    alias: &dyn Fn(&ShortId) -> Option<ShortId>,
) -> String {
    let open: Vec<String> = s
        .open
        .iter()
        .map(|(n, k)| format!("{n}:{}", k.word()))
        .collect();
    // The needs, most urgent first, each whole: one that would pass the
    // key's cap is left out with every less urgent one after it.
    let cap = match Key::Needs.cap() {
        crate::field::Cap::Tok(_, cap) => cap,
        _ => 0,
    };
    let mut needs = String::new();
    for (word, node, kind, by, src) in &s.needs {
        let item = format!(
            "{word}.{node}.{}.{}.{}",
            kind.word(),
            bus_actor(*by, alias).word(),
            src.word()
        );
        if needs.len() + item.len() + usize::from(!needs.is_empty()) > cap {
            break;
        }
        if !needs.is_empty() {
            needs.push(',');
        }
        needs.push_str(&item);
    }
    let mut f = Fields::new();
    if let Some(v) = BoundedToken::new(Key::Open, &open.join(",")).filter(|_| !open.is_empty()) {
        let _ = f.set(Key::Open, Val::Tok(v));
    }
    if let Some(v) = BoundedToken::new(Key::Needs, &needs).filter(|_| !needs.is_empty()) {
        let _ = f.set(Key::Needs, Val::Tok(v));
    }
    let _ = f.set(Key::MailIn, Val::U(u64::from(s.mail_in)));
    let _ = f.set(Key::MailOut, Val::U(u64::from(s.mail_out)));
    let _ = f.set(Key::Epoch, Val::U(u64::from(s.ep_now)));
    // The original bus-needs count proves whether the bounded words held
    // every need. Older snapshots lack it and remain usable but partial.
    let _ = f.set(Key::N, Val::U(s.needs.len() as u64));
    let _ = f.set(Key::Upto, Val::U(s.upto));
    let rec = Rec {
        seq: s.seq,
        t,
        op: Op::Snap,
        node: Node::SESSION,
        par: None,
        k: Kind::Snap,
        src: Src::Aterm,
        conf: Conf::Exact,
        by: Actor::Aterm,
        f,
    };
    encode_bus(&rec, tier.min(Tier::WireMeta), space, None).unwrap_or_default()
}

impl Log {
    /// The bus class's count in this seq space: the `ms` of the newest
    /// bus-class record appended, evicted or not.
    #[must_use]
    pub const fn ms_head(&self) -> u64 {
        self.ms_head
    }

    /// The `ms` head a reader is shown: [`Log::ms_head`], or, while the
    /// readers are fenced, the head at the fence.
    #[must_use]
    pub const fn reader_ms_head(&self) -> u64 {
        if self.fence.is_some() {
            self.fence_ms
        } else {
            self.ms_head
        }
    }

    /// Where the bus holds this log up to, once a take engaged the mirror;
    /// `None` for a log nothing ever mirrored (off is nothing).
    #[must_use]
    pub fn mirror_landed(&self) -> Option<BusCursor> {
        self.mirror.as_ref().map(|m| m.landed)
    }

    /// The space an uncarried update ended, while its tail is owed.
    #[must_use]
    pub fn mirror_prior(&self) -> Option<Prior> {
        self.mirror.as_ref().and_then(|m| m.prior)
    }

    /// The `ms` of the live record at `seq` (0 for a local-class one), or
    /// `None` when no live record has that seq.
    #[must_use]
    pub fn ms_of(&self, seq: u64) -> Option<u64> {
        let i = self.ring.partition_point(|e| e.rec.seq < seq);
        self.ring.get(i).filter(|e| e.rec.seq == seq).map(|e| e.ms)
    }

    /// The `ms` of every live bus-class record, oldest first: what a model's
    /// projection reads of the ring.
    pub fn live_ms(&self) -> impl Iterator<Item = u64> + '_ {
        self.ring.iter().map(|e| e.ms).filter(|ms| *ms > 0)
    }

    /// Whether the mirror has nothing left to take: engaged, no old space
    /// owed, and the cursor at the reader's `ms` head.
    #[must_use]
    pub fn mirror_settled(&self) -> bool {
        self.mirror.as_ref().is_some_and(|m| {
            m.prior.is_none() && m.landed.ep == self.base_ep && m.landed.ms >= self.reader_ms_head()
        })
    }

    /// Whether the bus holds this log whole: engaged, no old space owed, and
    /// the cursor at the LIVE `ms` head — under a fence too, past which a
    /// closing session's records are still owed once the update stands down
    /// (what a closed session's tail is kept for).
    #[must_use]
    pub fn mirror_complete(&self) -> bool {
        self.mirror.as_ref().is_some_and(|m| {
            m.prior.is_none() && m.landed.ep == self.base_ep && m.landed.ms >= self.ms_head
        })
    }

    fn engage(&mut self) -> &mut Mirror {
        let start = BusCursor::new(self.base_ep, self.first_seq.saturating_sub(1), 0);
        self.mirror.get_or_insert_with(|| {
            Box::new(Mirror {
                landed: start,
                snap_ms: 0,
                prior: None,
                snap_owed: None,
            })
        })
    }

    /// ENGAGE THE MIRROR from the space's first record if nothing has (the
    /// host's, while its node mirrors): from now on the log carries its
    /// cursor and its `ms`, and its first take says what the ring no longer
    /// holds as one exact cover. Engaged already, nothing changes.
    pub fn engage_mirror(&mut self) {
        self.engage();
    }

    /// An update that carried no log ended the space `prior` names (the host's `LogAdopt`):
    /// the successor's first take says its tail.
    /// Nothing is owed when the bus held all of it.
    pub fn note_prior_space(&mut self, prior: Prior) {
        if prior.ms_head > prior.landed.ms {
            self.engage().prior = Some(prior);
        }
    }

    /// THE TAKE, copied under the timeline's lock: at most `room` slots
    /// (capped at [`TAKE_MAX`]) past the landed cursor, oldest first. The old
    /// space's tail first (an uncarried update's, one `why=reset` cover);
    /// then this space's bus-class records with one cover before each live
    /// record whose `ms` is past the next one owed (the hole between,
    /// exact), and, once the take reaches the reader's head, one tail cover
    /// for the `ms` up to the reader's `ms` head no live record holds; then a
    /// snapshot when one is due. While fenced, nothing past the fence.
    /// Engages the mirror: from now on the log carries its cursor.
    pub fn bus_copy(&mut self, room: usize, at: Stamp) -> BusCopy {
        let room = room.min(TAKE_MAX);
        let space = self.base_ep;
        let head = self.reader_last_seq();
        let fh = self.reader_ms_head();
        self.engage();
        let mut m = self.mirror.as_deref().cloned().unwrap_or_default();
        let mut items: Vec<(Item, BusCursor)> = Vec::new();
        let mut more = false;
        if let Some(p) = m.prior.filter(|p| p.ms_head > p.landed.ms) {
            if room == 0 {
                more = true;
            } else {
                let lo = p.landed.ms + 1;
                items.push((
                    Item::Cover {
                        ep: p.landed.ep,
                        seq: p.landed.seq,
                        lo,
                        n: p.ms_head - p.landed.ms,
                        why: CoverWhy::Reset,
                    },
                    BusCursor::new(p.landed.ep, p.landed.seq, p.ms_head),
                ));
            }
        }
        if m.landed.ep != space {
            m.landed = BusCursor::new(space, self.first_seq.saturating_sub(1), 0);
        }
        let mut expected = m.landed.ms.saturating_add(1);
        let mut last_at = m.landed;
        if !more {
            let start = self.ring.partition_point(|e| e.rec.seq <= m.landed.seq);
            for e in self.ring.range(start..) {
                if e.rec.seq > head {
                    break;
                }
                if e.ms == 0 || e.ms < expected {
                    continue;
                }
                if e.ms > expected {
                    if items.len() >= room {
                        more = true;
                        break;
                    }
                    let cover_at = BusCursor::new(space, e.rec.seq.saturating_sub(1), e.ms - 1);
                    items.push((
                        Item::Cover {
                            ep: space,
                            seq: cover_at.seq,
                            lo: expected,
                            n: e.ms - expected,
                            why: CoverWhy::Evicted,
                        },
                        cover_at,
                    ));
                    last_at = cover_at;
                    expected = e.ms;
                }
                if items.len() >= room {
                    more = true;
                    break;
                }
                let rec_at = BusCursor::new(space, e.rec.seq, e.ms);
                items.push((
                    Item::Rec {
                        rec: std::sync::Arc::clone(&e.rec),
                        ms: e.ms,
                    },
                    rec_at,
                ));
                last_at = rec_at;
                expected = e.ms + 1;
            }
            if !more && expected <= fh {
                if items.len() >= room {
                    more = true;
                } else {
                    let tail_at = BusCursor::new(space, head.max(last_at.seq), fh);
                    items.push((
                        Item::Cover {
                            ep: space,
                            seq: tail_at.seq,
                            lo: expected,
                            n: fh - expected + 1,
                            why: CoverWhy::Evicted,
                        },
                        tail_at,
                    ));
                    last_at = tail_at;
                }
            }
        }
        // Due past the last snapshot ON THE BUS: one a dropped take handed
        // out is owed again.
        m.snap_owed = None;
        let snap_due = fh > 0 && (m.snap_ms == 0 || fh >= m.snap_ms.saturating_add(SNAP_EVERY));
        if !more && snap_due && items.len() < room {
            let fold = self.reader_fold();
            let st = fold.state();
            let open = fold.bus_open_path().into_iter().flatten().collect();
            let needs = st
                .needs
                .iter()
                .flatten()
                .filter(|n| n.node.lvl != crate::vocab::Lvl::Step || is_bus_step(n.kind))
                .map(|n| (n.word, n.node, n.kind, n.by, n.src))
                .collect();
            items.push((
                Item::Snap(SnapFacts {
                    seq: head,
                    upto: fh,
                    open,
                    needs,
                    mail_in: st.totals.mail_in,
                    mail_out: st.totals.mail_out,
                    ep_now: st.ep,
                }),
                last_at,
            ));
            m.snap_owed = Some((last_at, fh));
        }
        if let Some(mine) = self.mirror.as_deref_mut() {
            mine.snap_owed = m.snap_owed;
            mine.landed = m.landed;
        }
        BusCopy {
            space,
            t: at.wall_ms,
            items,
            more,
        }
    }

    /// THE LANDING: the bridge says every slot up to `at` is on the bus.
    /// Monotone (a cursor at or below the landed one, or of another space,
    /// moves nothing); refused while the readers are fenced (the export drew the cursor the successor continues)
    /// and for a cursor
    /// past anything this log numbered. A cursor of the old space an
    /// uncarried update ended moves that space's owed tail.
    ///
    /// # Errors
    ///
    /// [`LandRefused`]; nothing moved.
    pub fn land(&mut self, at: BusCursor) -> Result<Landed, LandRefused> {
        if self.read_fence().is_some() {
            return Err(LandRefused::Fenced);
        }
        let (space, ms_head, last_seq) = (self.base_ep, self.ms_head, self.last_seq);
        let m = self.engage();
        if let Some(p) = m.prior.as_mut()
            && at.ep == p.landed.ep
            && at.ep != space
        {
            if at.ms <= p.landed.ms {
                return Ok(Landed::Stale);
            }
            if at.ms > p.ms_head {
                return Err(LandRefused::Ahead);
            }
            p.landed = at;
            if p.landed.ms >= p.ms_head {
                m.prior = None;
            }
            return Ok(Landed::Moved);
        }
        if at.ep != space || (at.ms, at.seq) <= (m.landed.ms, m.landed.seq) {
            // A take that held only a snapshot lands at the cursor it stood at.
            m.land_snap(at);
            return Ok(Landed::Stale);
        }
        if at.ms > ms_head || at.seq > last_seq {
            return Err(LandRefused::Ahead);
        }
        m.landed = at;
        m.land_snap(at);
        Ok(Landed::Moved)
    }

    /// A CLOSED SESSION'S BUS TAIL: a copy holding only the
    /// bus-class records past the landed cursor, with the cursor, the
    /// accounting a take reads and the fold its snapshot reads. Only a take
    /// and a landing are meant of it. ALWAYS ENGAGED: a log no take ever
    /// reached (its whole life inside a broker outage, or before the bridge's
    /// first take) is its tail from its first record, so its take says every
    /// `ms` the ring no longer holds as one exact cover and nothing of it is
    /// lost unsaid. The readers' fence is copied (an update's export drew the
    /// live log's cursor): the host lifts it when the update stands down.
    #[must_use]
    pub fn bus_tail(&self) -> Log {
        let mut t = Log::with_limits(self.ep, self.limits);
        t.base_ep = self.base_ep;
        t.first_seq = self.first_seq;
        t.last_seq = self.last_seq;
        t.ms_head = self.ms_head;
        t.fence_ms = self.fence_ms;
        t.fence.clone_from(&self.fence);
        t.mirror.clone_from(&self.mirror);
        t.engage();
        t.fold = self.fold.clone();
        t.session_opened = self.session_opened;
        t.session_closed = self.session_closed;
        let from = self.mirror.as_ref().map_or(0, |m| m.landed.ms);
        for e in self.ring.iter().filter(|e| e.ms > from) {
            t.bytes = t.bytes.saturating_add(e.bytes);
            t.heap = t.heap.saturating_add(entry_heap(&e.rec));
            t.ring.push_back(e.clone());
        }
        t
    }

    /// The fold a reader is shown: the log's, or, while fenced, the fold as
    /// it stood at the fence.
    #[must_use]
    pub fn reader_fold(&self) -> &crate::fold::Fold {
        self.fence.as_ref().map_or(&self.fold, |f| &f.1)
    }

    /// Write the mirror's state (a carry read it back).
    pub(super) fn set_mirror(&mut self, landed: BusCursor, snap_ms: u64, prior: Option<Prior>) {
        self.mirror = Some(Box::new(Mirror {
            landed,
            snap_ms,
            prior,
            snap_owed: None,
        }));
    }

    /// FORGET THE MIRROR (the bus log was replaced, the host's `link log`):
    /// the next take engages it afresh and starts the space over from its
    /// first record, one exact cover for what the ring no longer holds.
    pub fn forget_mirror(&mut self) {
        self.mirror = None;
    }

    /// The `upto=` of the last snapshot a landing reached (0: none in this
    /// space, or not engaged).
    #[must_use]
    pub fn mirror_snap_ms(&self) -> u64 {
        self.mirror.as_ref().map_or(0, |m| m.snap_ms)
    }
}
