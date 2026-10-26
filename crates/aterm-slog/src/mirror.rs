// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUS MIRROR'S ENGINE HALF (design §5.5, phase 4): what a take of a log's bus-class records is, where the bus stands for the log, and the bodies a mirror publishes.

use super::{Entry, Log, entry_heap};
use crate::field::{BoundedToken, Fields, Key, Tier, Val};
use crate::line::{encode_bus, encode_mirror};
use crate::rec::{Rec, Stamp};
use crate::vocab::{Actor, Conf, Kind, Node, Op, ShortId, Src};

/// The most slots one take answers (`log-take n=` is capped at it; the bridge asks half of its pipeline window).
pub const TAKE_MAX: usize = 64;

/// A snapshot every this many bus-class records (§5.5): a reader joining at any offset needs at most this many earlier bodies of the session.
pub const SNAP_EVERY: u64 = 64;

/// A snapshot also when the session's mail counts differ from the last one on the bus, at most one this often by the session's wall clock (§5.5, LOOP-2): mail marks stay local, so a snapshot's `mail_in=`/`mail_out=` are how a count crosses, and a quiet session reaches no 64th record.
pub const MAIL_SNAP_GAP_MS: u64 = 1_000;

/// One ring entry's size: the shared record's pointer, its encoded bytes, its `ms` and its process-clock reading.
pub const ENTRY_BYTES: usize = std::mem::size_of::<Entry>();

/// What phase 4 added to every [`Log`]'s own size, mirror on or off: the bus class's `ms` head, the head at the readers' fence and the mirror's box (empty while nothing mirrors).
pub const LOG_MIRROR_BYTES: usize =
    2 * std::mem::size_of::<u64>() + std::mem::size_of::<Option<Box<Mirror>>>();

/// Where the bus holds a session's log up to: its seq space, the seq and the bus-class count `ms`.
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

/// A seq space an update that carried no log ended: where the bus held it and its `ms` head, which the successor's first take says the rest of as one `gap why=reset n=<k>` in that space.
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
    /// The `ms` head the last snapshot ON THE BUS was taken at (0: none in this space): one counts once its landing reaches it ([`Log::land`]).
    pub(super) snap_ms: u64,
    /// The mail counts (`mail_in`, `mail_out`) the last snapshot ON THE BUS carried, landed with it: while the session's own differ, a snapshot is owed ([`MAIL_SNAP_GAP_MS`]). A carry starts them at none, so a session with mail owes one snapshot after an update.
    pub(super) snap_mail: (u32, u32),
    /// The session's wall clock when a take last handed out a snapshot that moved the mail counts (0: none): the rate [`MAIL_SNAP_GAP_MS`] is counted from it.
    pub(super) mail_snap_wall: u64,
    /// The space an uncarried update ended, while its tail is owed.
    pub(super) prior: Option<Prior>,
    /// The snapshot the take in flight handed out (its slot's cursor, its `upto=` and its mail counts), until its landing reaches it.
    pub(super) snap_owed: Option<(BusCursor, u64, (u32, u32))>,
}

impl Mirror {
    /// A landing at `at` reached the snapshot in flight: it is on the bus.
    fn land_snap(&mut self, at: BusCursor) {
        if let Some((c, upto, mail)) = self.snap_owed
            && at.ep == c.ep
            && (at.ms, at.seq) >= (c.ms, c.seq)
        {
            self.snap_ms = upto;
            self.snap_mail = mail;
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
    /// Dropped whole: a closed session's tail past the retired bound, a record whose body does not fit, an old space nothing carried.
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

/// A take copied under the timeline's lock ([`Log::bus_copy`]), encoded after it is released ([`BusCopy::encode`]).
#[derive(Clone, Debug)]
pub struct BusCopy {
    space: u32,
    t: u64,
    items: Vec<(Item, BusCursor)>,
    more: bool,
    due: Option<u64>,
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

    /// How many ms of the session's wall clock until the snapshot of a moved mail count that the rate held back ([`MAIL_SNAP_GAP_MS`]) is due, when this take left one owed: nothing else appends to wake a taker, so it takes again then.
    #[must_use]
    pub const fn due_in_ms(&self) -> Option<u64> {
        self.due
    }

    /// The slots' bodies at `tier` (held at [`Tier::WireMeta`]: the bus never carries an H or C key), an edge's sid written as `alias` names it ([`encode_mirror`]).
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

/// One slot of a take: its body and the cursor the session lands at once it and every slot before it are on the bus.
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
    /// The cursor was already there or past it, or names another space: on the bus already, nothing moved.
    Stale,
}

/// Why a landing was refused (nothing moved).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandRefused {
    /// The readers are fenced (an update's export drew the cursor): retry once the update commits or stands down.
    Fenced,
    /// Past anything this log numbered: not a cursor of this log's.
    Ahead,
}

/// A cover body (`op=gap why=<w> n=<k>`, `ms=` the first `ms` it covers): the statement that the `k` bus-class records from `lo` of space `ep` are not on the bus, and never will be.
#[must_use]
pub fn cover_body(ep: u32, seq: u64, lo: u64, n: u64, why: CoverWhy, t: u64) -> String {
    let mut f = Fields::new();
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
    /// The bus class's count in this seq space: the `ms` of the newest bus-class record appended, evicted or not.
    #[must_use]
    pub const fn ms_head(&self) -> u64 {
        self.ms_head
    }

    /// The `ms` head a reader is shown: [`Log::ms_head`], or, while the readers are fenced, the head at the fence.
    #[must_use]
    pub const fn reader_ms_head(&self) -> u64 {
        if self.fence.is_some() {
            self.fence_ms
        } else {
            self.ms_head
        }
    }

    /// Where the bus holds this log up to, once a take engaged the mirror; `None` for a log nothing ever mirrored (off is nothing).
    #[must_use]
    pub fn mirror_landed(&self) -> Option<BusCursor> {
        self.mirror.as_ref().map(|m| m.landed)
    }

    /// The space an uncarried update ended, while its tail is owed.
    #[must_use]
    pub fn mirror_prior(&self) -> Option<Prior> {
        self.mirror.as_ref().and_then(|m| m.prior)
    }

    /// The `ms` of the live record at `seq` (0 for a local-class one), or `None` when no live record has that seq.
    #[must_use]
    pub fn ms_of(&self, seq: u64) -> Option<u64> {
        let i = self.ring.partition_point(|e| e.rec.seq < seq);
        self.ring.get(i).filter(|e| e.rec.seq == seq).map(|e| e.ms)
    }

    /// The `ms` of every live bus-class record, oldest first: what a model's projection reads of the ring.
    pub fn live_ms(&self) -> impl Iterator<Item = u64> + '_ {
        self.ring.iter().map(|e| e.ms).filter(|ms| *ms > 0)
    }

    /// Whether the mirror has nothing left to take: engaged, no old space owed, and the cursor at the reader's `ms` head.
    #[must_use]
    pub fn mirror_settled(&self) -> bool {
        self.mirror.as_ref().is_some_and(|m| {
            m.prior.is_none() && m.landed.ep == self.base_ep && m.landed.ms >= self.reader_ms_head()
        })
    }

    /// Whether the bus holds this log whole: engaged, no old space owed, and the cursor at the LIVE `ms` head — under a fence too, past which a closing session's records are still owed once the update stands down (what a closed session's tail is kept for) — and, once the session closed, its last mail counts landed in a snapshot (mail marks stay local, so its last snapshot is how they cross).
    #[must_use]
    pub fn mirror_complete(&self) -> bool {
        self.mirror.as_ref().is_some_and(|m| {
            m.prior.is_none()
                && m.landed.ep == self.base_ep
                && m.landed.ms >= self.ms_head
                && !(self.session_closed && self.ms_head > 0 && {
                    let t = &self.fold.state().totals;
                    (t.mail_in, t.mail_out) != m.snap_mail
                })
        })
    }

    fn engage(&mut self) -> &mut Mirror {
        let start = BusCursor::new(self.base_ep, self.first_seq.saturating_sub(1), 0);
        self.mirror.get_or_insert_with(|| {
            Box::new(Mirror {
                landed: start,
                snap_ms: 0,
                snap_mail: (0, 0),
                mail_snap_wall: 0,
                prior: None,
                snap_owed: None,
            })
        })
    }

    /// ENGAGE THE MIRROR from the space's first record if nothing has (the host's, while its node mirrors): from now on the log carries its cursor and its `ms`, and its first take says what the ring no longer holds as one exact cover.
    pub fn engage_mirror(&mut self) {
        self.engage();
    }

    /// An update that carried no log ended the space `prior` names (the host's `LogAdopt`): the successor's first take says its tail.
    pub fn note_prior_space(&mut self, prior: Prior) {
        if prior.ms_head > prior.landed.ms {
            self.engage().prior = Some(prior);
        }
    }

    /// THE TAKE, copied under the timeline's lock: at most `room` slots (capped at [`TAKE_MAX`]) past the landed cursor, oldest first, then a snapshot when one is due: at the space's first take, every [`SNAP_EVERY`] bus-class records, and when the mail counts moved, at most one every [`MAIL_SNAP_GAP_MS`] of `at`'s wall clock (a closed session's last counts are owed at once), alone at the landed cursor when nothing else is past it; a moved count the rate holds back says when it is due ([`BusCopy::due_in_ms`]).
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
        m.snap_owed = None;
        let mail = {
            let t = &self.reader_fold().state().totals;
            (t.mail_in, t.mail_out)
        };
        let snap_due = fh > 0 && (m.snap_ms == 0 || fh >= m.snap_ms.saturating_add(SNAP_EVERY));
        let moved = fh > 0 && mail != m.snap_mail;
        let since = at.wall_ms.abs_diff(m.mail_snap_wall);
        let mail_due =
            moved && (self.session_closed || m.mail_snap_wall == 0 || since >= MAIL_SNAP_GAP_MS);
        if !more && (snap_due || mail_due) && items.len() < room {
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
                    mail_in: mail.0,
                    mail_out: mail.1,
                    ep_now: st.ep,
                }),
                last_at,
            ));
            if mail != m.snap_mail {
                m.mail_snap_wall = at.wall_ms;
            }
            m.snap_owed = Some((last_at, fh, mail));
        }
        let due = (moved && !mail_due && m.snap_owed.is_none())
            .then(|| MAIL_SNAP_GAP_MS.saturating_sub(since).max(1));
        if let Some(mine) = self.mirror.as_deref_mut() {
            mine.snap_owed = m.snap_owed;
            mine.mail_snap_wall = m.mail_snap_wall;
            mine.landed = m.landed;
        }
        BusCopy {
            space,
            t: at.wall_ms,
            items,
            more,
            due,
        }
    }

    /// THE LANDING: the bridge says every slot up to `at` is on the bus.
    /// # Errors
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

    /// A CLOSED SESSION'S BUS TAIL: a copy holding only the bus-class records past the landed cursor, with the cursor, the accounting a take reads and the fold its snapshot reads.
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

    /// The fold a reader is shown: the log's, or, while fenced, the fold as it stood at the fence.
    #[must_use]
    pub fn reader_fold(&self) -> &crate::fold::Fold {
        self.fence.as_ref().map_or(&self.fold, |f| &f.1)
    }

    /// Write the mirror's state (a carry read it back).
    pub(super) fn set_mirror(&mut self, landed: BusCursor, snap_ms: u64, prior: Option<Prior>) {
        self.mirror = Some(Box::new(Mirror {
            landed,
            snap_ms,
            snap_mail: (0, 0),
            mail_snap_wall: 0,
            prior,
            snap_owed: None,
        }));
    }

    /// FORGET THE MIRROR (the bus log was replaced, the host's `link log`): the next take engages it afresh and starts the space over from its first record, one exact cover for what the ring no longer holds.
    pub fn forget_mirror(&mut self) {
        self.mirror = None;
    }

    /// The `upto=` of the last snapshot a landing reached (0: none in this space, or not engaged).
    #[must_use]
    pub fn mirror_snap_ms(&self) -> u64 {
        self.mirror.as_ref().map_or(0, |m| m.snap_ms)
    }
}
