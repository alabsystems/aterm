// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CARRY (design §5.2, phase 2A): a session's log as text lines a
//! seamless update hands the process that adopts the session, and back.
//!
//! Records alone are not the log: the successor needs the ring's accounting
//! (seqs, losses, the seqs it never held), the open nodes with their rollups
//! so far, the node index (parents of nodes whose open records are gone),
//! the fold as it stands ([`Fold::snapshot`]: a refold of a shed subset would
//! change totals, needs and claims), and the host's producer state, which
//! this crate passes through unread (`host` lines). So a carry is:
//!
//! ```text
//! slog-carry v=1 ep=<e> base=<b> first=<f> last=<l> lost=<n> newest_lost=<m> ids=<r>,<t>,<s> carried=<k> shed=<j> lines=<n>
//! sess kind=<spawn|adopt|restore> opened=<0|1> t=<ms> roll=<18 counts>
//! hole <lo> <hi>
//! cpar <node> <parent>
//! node <node> par=<node|?> open=<0|1> below=<seq>   (the session, open nodes)
//! kids <parent|?> <node>,<node>,…          (closed nodes whose open record does not cross)
//! open <node> kind=<k> par=<node> t=<ms> orphan=<0|1> roll=<18 counts>
//! fold …                                    (Fold::snapshot)
//! host <opaque>                             (at most 16 lines, 4 KiB)
//! r <record>                                (encoded at the host tier, oldest first)
//! R <ms> <record>                           (a bus-class record of a mirrored log, phase 4)
//! ```
//!
//! THE MIRROR (phase 4) adds nothing to a log no bridge ever took from: its
//! carry is the bytes it was (ruling Q5). A mirrored log's header ends
//! ` ms=<head> mirror=<ep>:<seq>:<ms> snap=<ms>[ prior=<ep>:<seq>:<ms>:<head>]`
//! and each of its bus-class records crosses as `R <ms> <record>`, so the
//! successor goes on numbering the bus class where the predecessor stopped
//! and takes from the cursor the export drew. A carry without `ms=` (a 2A or
//! 2B writer's, or an unmirrored log's) is read as it always was: its
//! bus-class records are numbered densely from 1 in seq order, `ms`'s head
//! is their count, and no cursor crosses (ruling Q7).
//!
//! THE CUT. The open records of open nodes, the session's own open and the
//! record appended last (with the closes above it) always cross; the rest
//! is cut to the budget in the eviction order ([`super::evict_class`]):
//! closed detail first, then the opens of closed runs and turns, closes
//! while they pass half, current detail, the remaining closes. So the carry
//! sheds exactly what memory would, and each record it sheds is counted as
//! the ring counts an eviction: into `lost=` and its nearest open node's
//! rollup. A carry whose mandatory part passes its skeleton budget is not
//! made: the update carries no records for that session ([`Log::carry`]).
//!
//! THE CONTENT TIER DOES NOT CROSS: records are encoded at the host tier, so
//! a carried title mark keeps its record without its `title=` (§3.6 keeps the
//! OSC title in RAM only).
//!
//! READING BACK ([`Log::from_carry`]) is bounded before anything is
//! allocated past the lines themselves, and fail-closed: a version, tag or
//! value this build does not read, seqs out of order, a node past the
//! carried counters, an open node whose parent is not open or whose open
//! record is missing, more than a ring or a fold holds — any of it refuses
//! the whole carry, and the update then carries no records for that session
//! (the host's uncarried path). The successor's log continues the carried one
//! at its next epoch: it writes `mark - handoff carried=<k> shed=<j>
//! epoch=<e>` above the seqs the predecessor could still have used, and
//! leaves every carried run, turn and step OPEN, measured from then on by
//! the wall clock (§3.5).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;

use super::{
    CLOSED_PARENTS, EvictClass, GapWhy, IdCounters, Limits, Log, MAX_DEPTH, MAX_HOLES, MAX_OPEN,
    NodeInfo, OpenEntry, Rollup, closes_over_half, evict_class,
};
use crate::field::{Fields, Tier, Val};
use crate::fold::{Fold, SnapError};
use crate::line::{decode, encode, encoded_size};
use crate::rec::{Rec, Stamp};
use crate::turn::rebase;
use crate::vocab::{Actor, Conf, Kind, Lvl, Node, Op, Src};

/// A carry's byte budget (design §5.2): 128 KiB.
pub const CARRY_TOTAL_BYTES: usize = 128 * 1024;
/// The most the mandatory part of a carry may take (the open path, the
/// skeleton's state, the fold and the host's lines): 32 KiB.
pub const CARRY_SKELETON_BYTES: usize = 32 * 1024;
/// The longest carry line a receiver reads.
pub const CARRY_MAX_LINE: usize = 1024;
/// The most carry lines a receiver reads: a full ring, a full fold's fields
/// and the skeleton's lines.
pub const CARRY_MAX_LINES: usize = 8192;
/// The most carry bytes a receiver reads: the budget, and the node lines the
/// cut adds for the records it keeps.
pub const CARRY_MAX_BYTES: usize = CARRY_TOTAL_BYTES + 16 * 1024;
/// The most host lines a carry passes through, and their bytes.
pub const CARRY_MAX_HOST_LINES: usize = 16;
/// See [`CARRY_MAX_HOST_LINES`].
pub const CARRY_MAX_HOST_BYTES: usize = 4096;

/// How much a carry may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CarryBudget {
    /// All of it, in bytes.
    pub total: usize,
    /// Its mandatory part; past it the session's log is not carried.
    pub skeleton: usize,
    /// The most records it holds.
    pub records: usize,
}

impl CarryBudget {
    /// A session's (§5.2): 128 KiB, the skeleton within 32 KiB, a ring's
    /// records.
    pub const SESSION: CarryBudget = CarryBudget {
        total: CARRY_TOTAL_BYTES,
        skeleton: CARRY_SKELETON_BYTES,
        records: super::MAX_RECORDS,
    };
}

/// What a carry made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarryOut {
    /// The lines.
    pub lines: Vec<String>,
    /// Records carried.
    pub carried: u64,
    /// Records the cut shed (counted as lost).
    pub shed: u64,
    /// The lines' bytes (a newline each).
    pub bytes: usize,
    /// The conservative byte reservation for its mandatory part.
    pub skeleton_bytes: usize,
    /// The carried log's last seq: its readers' fence ([`Log::fence_reads`]).
    pub last_seq: u64,
}

/// Why a carry did not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CarryError {
    /// Past a byte, line or count bound.
    Bound(&'static str),
    /// Not a version this build reads.
    Version,
    /// A line this build does not read.
    Line(&'static str),
    /// Records that do not decode, out of seq order or past the head.
    Record(&'static str),
    /// The open path or the index does not hold together.
    Shape(&'static str),
    /// The fold's snapshot.
    Fold(SnapError),
}

impl CarryError {
    /// Why, in a few words, for the host's WARN line.
    #[must_use]
    pub const fn why(self) -> &'static str {
        match self {
            CarryError::Bound(w)
            | CarryError::Line(w)
            | CarryError::Record(w)
            | CarryError::Shape(w) => w,
            CarryError::Version => "a carry version this build does not read",
            CarryError::Fold(_) => "a fold snapshot this build does not read",
        }
    }
}

/// What the successor continues a carry with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CarryAdopt {
    /// Its epoch: above the carried one.
    pub ep: u32,
    /// Its first seq: above every seq the predecessor could have used (the
    /// manifest's count plus one). A carry whose head is not below it, or
    /// either past [`super::SEQ_LIMIT`], is refused: the manifest's count is
    /// drawn before the carry is taken and a Commit that lands has kept to
    /// it, so a head at or above it is a file that does not hold together.
    pub next_seq: u64,
    /// The node-id counters the manifest carried (with their reserve).
    pub ids: IdCounters,
    /// Its ring's bounds.
    pub limits: Limits,
    /// Now, on this process's clocks.
    pub at: Stamp,
}

/// What a read carry continued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CarryStats {
    /// Records carried.
    pub carried: u64,
    /// Records the predecessor's cut shed.
    pub shed: u64,
    /// The carried log's last seq (its `last=`): the predecessor's readers'
    /// fence, which a successor's journal starts after when the predecessor
    /// journaled up to it.
    pub head: u64,
}

/// One record of a carry snapshot past the ones that always cross, in the
/// order the cut sheds them.
#[derive(Clone, Debug)]
struct Victim {
    rec: Rec,
    /// Whose rollup counts it when it is shed (`None`: the session's).
    target: Option<Node>,
    /// Its carry line's bytes at most (its newline counted): the ring's own
    /// encoded size of it, at the content tier, which the host tier's line
    /// never passes, plus the `r ` tag. A cut sized by these is within its
    /// budget, and only the records that cross are ever encoded.
    est: usize,
}

/// Everything a carry needs of a log, copied under its lock: the records that
/// always cross are encoded after the lock is released, once
/// ([`CarrySnapshot::prepare`]); the rest are cut by the sizes the ring
/// already knows and encoded only when a cut keeps them, so the lock is held
/// only for the copy, and a session whose detail does not cross pays for no
/// encoding of it.
#[derive(Clone, Debug)]
pub struct CarrySnapshot {
    closed: bool,
    ep: u32,
    base_ep: u32,
    first_seq: u64,
    last_seq: u64,
    lost: u64,
    newest_lost: u64,
    issued: [u32; 3],
    holes: VecDeque<(u64, u64, GapWhy)>,
    closed_parents: VecDeque<(Node, Node)>,
    index: BTreeMap<Node, (Node, bool, u64)>,
    open: Vec<OpenEntry>,
    session: (Kind, bool, u64, Rollup),
    fold_lines: Vec<String>,
    keep: Vec<Rec>,
    victims: Vec<Victim>,
    /// The bus class's count, the mirror's state and each live bus-class
    /// record's `ms` by seq: only for a mirrored log (none otherwise, so its
    /// carry is the bytes it was).
    ms_head: u64,
    mirror: Option<super::mirror::Mirror>,
    ms_of: BTreeMap<u64, u64>,
}

/// One `hole <lo> <hi>` line: an update's overlap as slice 2A wrote it, any
/// other range (a journal read's, `Log::from_journal`) with its ` why=<w>`,
/// which a reader that predates it skips as an unknown token.
pub(super) fn hole_line(lo: u64, hi: u64, why: GapWhy) -> String {
    if why == GapWhy::Handoff {
        format!("hole {lo} {hi}")
    } else {
        format!("hole {lo} {hi} why={}", why.word())
    }
}

/// The why of a `hole` line's remaining tokens: its ` why=<w>`, else (an
/// older writer's, or a word this build does not know) an update's overlap.
pub(super) fn hole_why<'a>(mut rest: impl Iterator<Item = &'a str>) -> GapWhy {
    rest.find_map(|t| t.strip_prefix("why="))
        .and_then(GapWhy::from_word)
        .unwrap_or(GapWhy::Handoff)
}

/// A rollup as one `roll=` value: eight counters, then the ten tool counts.
fn roll_word(r: &Rollup) -> String {
    let mut s = format!(
        "{},{},{},{},{},{},{},{}",
        r.steps, r.appr, r.qs, r.walls, r.stalls, r.mail, r.errs, r.lost
    );
    for t in r.tools {
        let _ = write!(s, ",{t}");
    }
    s
}

fn read_roll(raw: &str) -> Option<Rollup> {
    let n: Vec<u32> = raw
        .split(',')
        .map(|v| v.parse().ok())
        .collect::<Option<Vec<u32>>>()?;
    if n.len() != 18 {
        return None;
    }
    let mut tools = [0u32; 10];
    tools.copy_from_slice(&n[8..]);
    Some(Rollup {
        steps: n[0],
        appr: n[1],
        qs: n[2],
        walls: n[3],
        stalls: n[4],
        mail: n[5],
        errs: n[6],
        lost: n[7],
        tools,
    })
}

/// The carry line of `r`, or `None` when it no longer fits (never for a
/// record the log accepted): `R <ms> <record>` for a mirrored log's
/// bus-class record, `r <record>` otherwise.
fn rec_line(r: &Rec, ms: Option<u64>) -> Option<String> {
    let l = encode(r, Tier::Host).ok()?;
    Some(match ms {
        Some(ms) => format!("R {ms} {l}"),
        None => format!("r {l}"),
    })
}

/// The decimal digits of `n`.
const fn digits(mut n: u64) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

fn line_bytes(lines: &[String]) -> usize {
    lines.iter().map(|l| l.len() + 1).sum()
}

impl Log {
    /// The snapshot a carry is cut from: the log's state, its fold's
    /// snapshot, and its records in the order the cut sheds them. O(n)
    /// clones; call it under the log's lock and [`CarrySnapshot::encode`]
    /// after.
    #[must_use]
    pub fn carry_snapshot(&self) -> CarrySnapshot {
        let n = self.ring.len();
        let newest = self.ring.back().map(|e| e.rec.node);
        let shelter: Vec<Node> = newest.map(|nd| self.chain(nd)).unwrap_or_default();
        let mut keep = Vec::new();
        let mut buckets: [Vec<usize>; 4] = Default::default();
        for (i, e) in self.ring.iter().enumerate() {
            let class = evict_class(&e.rec, |nd| self.view(nd));
            let slot = match class {
                _ if i + 1 == n => None,
                EvictClass::Pinned => None,
                EvictClass::CloseRecord if shelter.contains(&e.rec.node) => None,
                EvictClass::ClosedDetail => Some(0),
                EvictClass::ClosedOpen => Some(1),
                EvictClass::CloseRecord => Some(2),
                EvictClass::OpenDetail => Some(3),
            };
            match slot {
                Some(s) => buckets[s].push(i),
                None => keep.push(i),
            }
        }
        // How many of the oldest closes go before current detail: those that
        // take the closes down to half (§5.1's third class).
        let lim = self.effective_limits();
        let (mut count, mut bytes, mut early) = (self.close_count, self.close_bytes, 0);
        for &i in &buckets[2] {
            if !closes_over_half(count, bytes, lim) {
                break;
            }
            count = count.saturating_sub(1);
            bytes = bytes.saturating_sub(self.ring[i].bytes);
            early += 1;
        }
        let order = buckets[0]
            .iter()
            .chain(&buckets[1])
            .chain(&buckets[2][..early])
            .chain(&buckets[3])
            .chain(&buckets[2][early..]);
        // A mirrored log's bus-class record crosses with its `ms` (`R <ms> `).
        let mirrored = self.mirror.is_some();
        let ms_cost = |e: &super::Entry| {
            if mirrored && e.ms > 0 {
                digits(e.ms) + 1
            } else {
                0
            }
        };
        let victims = order
            .map(|&i| {
                let e = &self.ring[i];
                Victim {
                    rec: e.rec.clone(),
                    target: self.loss_target(e.rec.node),
                    est: e.bytes + "r ".len() + 1 + ms_cost(e),
                }
            })
            .collect();
        let ms_of = if mirrored {
            self.ring
                .iter()
                .filter(|e| e.ms > 0)
                .map(|e| (e.rec.seq, e.ms))
                .collect()
        } else {
            BTreeMap::new()
        };
        CarrySnapshot {
            closed: self.session_closed,
            ep: self.ep,
            base_ep: self.base_ep,
            first_seq: self.first_seq,
            last_seq: self.last_seq,
            lost: self.lost,
            newest_lost: self.newest_lost,
            issued: self.issued,
            holes: self.holes.clone(),
            closed_parents: self.closed_parents.clone(),
            index: self
                .nodes
                .iter()
                .map(|(n, i)| (*n, (i.parent, i.open, i.closed_below)))
                .collect(),
            open: self.open.clone(),
            session: (
                self.session_kind,
                self.session_opened,
                self.session_t,
                self.session_roll,
            ),
            fold_lines: self.fold.snapshot(),
            keep: keep.into_iter().map(|i| self.ring[i].rec.clone()).collect(),
            victims,
            ms_head: self.ms_head,
            mirror: self.mirror.as_deref().cloned(),
            ms_of,
        }
    }

    /// The nearest open ancestor-or-self of `node` (`None`: the session):
    /// whose rollup counts a loss of its record.
    fn loss_target(&self, node: Node) -> Option<Node> {
        let mut at = node;
        for _ in 0..MAX_DEPTH {
            if at == Node::SESSION {
                return None;
            }
            if self.open.iter().any(|e| e.node == at) {
                return Some(at);
            }
            match self.nodes.get(&at).map(|i| i.parent) {
                Some(p) if p != Node::INSTANCE => at = p,
                _ => return None,
            }
        }
        None
    }

    /// This log as a carry within `budget`, the host's `host` lines passed
    /// through: [`Log::carry_snapshot`] then [`CarrySnapshot::encode`].
    /// `None` for a closed session or when the mandatory part exceeds any
    /// requested bound: the session then hands off with no records.
    #[must_use]
    pub fn carry(&self, budget: CarryBudget, host: &[String]) -> Option<CarryOut> {
        self.carry_snapshot().prepare().encode(budget, host)
    }

    /// Fence this log's readers at its head (the carry an update just took,
    /// design §5.2): until [`Log::lift_read_fence`], a read sees no record
    /// past it and [`Log::reader_state`] is the fold as it stood. The
    /// outgoing process goes on writing to its Commit, and its successor
    /// continues the carry, not those records: a reader shown them would see
    /// a turn closed here and again there, or a step that never closes. The
    /// fenced records are not evicted meanwhile (what a reader is shown is
    /// what the carry holds, but for what its cut shed, which the successor
    /// counts lost); the records after the fence are, so the ring stays
    /// within its bounds plus the one record just appended.
    pub fn fence_reads(&mut self) {
        self.fence = Some(Box::new((self.last_seq, self.fold.clone())));
        self.fence_ms = self.ms_head;
        self.rang();
    }

    /// Lift the readers' fence (the update stood down; this process goes on).
    pub fn lift_read_fence(&mut self) {
        self.fence = None;
        // The marks the fence kept past their quota go now (`log_quota`).
        self.enforce_mark_quota();
        self.rang();
    }

    /// The fenced seq, while readers are fenced.
    #[must_use]
    pub fn read_fence(&self) -> Option<u64> {
        self.fence.as_ref().map(|f| f.0)
    }

    /// The state a reader is shown: the fold's, or, while fenced, the fold
    /// as it stood at the fence.
    #[must_use]
    pub fn reader_state(&self) -> &crate::fold::SessionState {
        self.fence
            .as_ref()
            .map_or(self.fold.state(), |f| f.1.state())
    }

    /// The newest seq a reader is shown ([`Log::last_seq`], or the fence).
    #[must_use]
    pub fn reader_last_seq(&self) -> u64 {
        self.read_fence().unwrap_or(self.last_seq)
    }

    /// The successor's log from a carry `lines` wrote ([`Log::carry`]), at
    /// `a.ep`, its seqs from `a.next_seq`, its node ids at least `a.ids`,
    /// its first new record `mark - handoff carried=<k> shed=<j>
    /// epoch=<ep>`. Every carried open node stays open, measured by the
    /// wall clock. Returns the log, the host's lines (their `host ` tag
    /// taken off), and what crossed.
    ///
    /// # Errors
    ///
    /// A [`CarryError`] for anything this build does not read or that does
    /// not hold together; nothing is kept of a refused carry.
    pub fn from_carry(
        lines: &[String],
        a: CarryAdopt,
    ) -> Result<(Log, Vec<String>, CarryStats), CarryError> {
        if lines.len() > CARRY_MAX_LINES {
            return Err(CarryError::Bound("more carry lines than a carry holds"));
        }
        if line_bytes(lines) > CARRY_MAX_BYTES {
            return Err(CarryError::Bound("a carry past its byte bound"));
        }
        if lines.iter().any(|l| l.len() > CARRY_MAX_LINE) {
            return Err(CarryError::Bound("a carry line past its bound"));
        }
        let head = lines.first().ok_or(CarryError::Version)?;
        if !head.starts_with("slog-carry v=1 ") {
            return Err(CarryError::Version);
        }
        let hk = |k: &str| -> Result<u64, CarryError> {
            kv_of(head, 2, k)
                .and_then(|v| v.parse().ok())
                .ok_or(CarryError::Line("a carry header field"))
        };
        let (ep, base) = (hk("ep")?, hk("base")?);
        let (first, last) = (hk("first")?, hk("last")?);
        let (lost, newest_lost) = (hk("lost")?, hk("newest_lost")?);
        let (carried, shed) = (hk("carried")?, hk("shed")?);
        let count = hk("lines")?;
        if count != lines.len() as u64 {
            return Err(CarryError::Shape("a carry missing lines"));
        }
        let ids: Vec<u32> = kv_of(head, 2, "ids")
            .ok_or(CarryError::Line("a carry header's ids"))?
            .split(',')
            .map(|v| v.parse().ok())
            .collect::<Option<Vec<u32>>>()
            .filter(|v| v.len() == 3)
            .ok_or(CarryError::Line("a carry header's ids"))?;
        let ep = u32::try_from(ep).map_err(|_| CarryError::Line("an epoch"))?;
        let base = u32::try_from(base).map_err(|_| CarryError::Line("an epoch"))?;
        // THE MIRROR'S KEYS (phase 4): present only in a mirrored log's
        // carry. Without them the carried bus class is numbered afresh
        // (ruling Q7); with them each bus-class record crosses as an `R` line.
        let ms_head: Option<u64> = match kv_of(head, 2, "ms") {
            Some(v) => Some(
                v.parse()
                    .map_err(|_| CarryError::Line("a carry's ms head"))?,
            ),
            None => None,
        };
        let mirror = match ms_head {
            Some(_) => {
                let landed = kv_of(head, 2, "mirror")
                    .and_then(super::BusCursor::parse)
                    .ok_or(CarryError::Line("a carry's mirror cursor"))?;
                let snap_ms: u64 = kv_of(head, 2, "snap")
                    .and_then(|v| v.parse().ok())
                    .ok_or(CarryError::Line("a carry's snapshot head"))?;
                let prior = match kv_of(head, 2, "prior") {
                    Some(v) => {
                        let (c, h) = v
                            .rsplit_once(':')
                            .ok_or(CarryError::Line("a carry's prior space"))?;
                        Some(super::Prior {
                            landed: super::BusCursor::parse(c)
                                .ok_or(CarryError::Line("a carry's prior space"))?,
                            ms_head: h
                                .parse()
                                .map_err(|_| CarryError::Line("a carry's prior space"))?,
                        })
                    }
                    None => None,
                };
                Some((landed, snap_ms, prior))
            }
            None => None,
        };
        if !(base <= ep && ep < a.ep) || first == 0 || first > last.saturating_add(1) {
            return Err(CarryError::Shape("a carry's epochs or seqs out of order"));
        }
        // Seqs no log reaches, or a head the manifest's count does not clear:
        // continuing either would reuse a seq or draw a ceiling below the
        // head, which stands every later update's Commit down.
        if last > super::SEQ_LIMIT || a.next_seq > super::SEQ_LIMIT.saturating_add(1) {
            return Err(CarryError::Bound("a seq past the limit"));
        }
        if last >= a.next_seq {
            return Err(CarryError::Shape(
                "a carry's head at or past the manifest's seq count",
            ));
        }
        let issued = [ids[0], ids[1], ids[2]];
        let is_issued = |n: Node| match n.lvl {
            Lvl::Run => n.n >= 1 && n.n <= issued[0],
            Lvl::Turn => n.n >= 1 && n.n <= issued[1],
            Lvl::Step => n.n >= 1 && n.n <= issued[2],
            Lvl::Session => true,
            Lvl::Instance => false,
        };
        let mut sess: Option<(Kind, bool, u64, Rollup)> = None;
        let mut holes: VecDeque<(u64, u64, GapWhy)> = VecDeque::new();
        let mut cpar: VecDeque<(Node, Node)> = VecDeque::new();
        let mut index: BTreeMap<Node, NodeInfo> = BTreeMap::new();
        let mut open: Vec<OpenEntry> = Vec::new();
        let mut fold_lines: Vec<&str> = Vec::new();
        let mut host: Vec<String> = Vec::new();
        let mut recs: Vec<Rec> = Vec::new();
        // Each carried record's `ms` (0 for a local-class one).
        let mut rec_ms: Vec<u64> = Vec::new();
        for line in &lines[1..] {
            let (tag, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
            match tag {
                "sess" => {
                    if sess.is_some() {
                        return Err(CarryError::Shape("two session lines"));
                    }
                    let g = |k: &str| kv_of(line, 1, k).ok_or(CarryError::Line("a session line"));
                    let kind = Kind::from_word(g("kind")?);
                    if !kind.fits(Op::Open, Lvl::Session) {
                        return Err(CarryError::Line("a session's kind"));
                    }
                    let opened = flag(g("opened")?)?;
                    let t: u64 = g("t")?.parse().map_err(|_| CarryError::Line("a time"))?;
                    let roll = read_roll(g("roll")?).ok_or(CarryError::Line("a rollup"))?;
                    if g("closed")? != "0" {
                        return Err(CarryError::Shape("a closed session is not carried"));
                    }
                    sess = Some((kind, opened, t, roll));
                }
                "hole" => {
                    let mut it = rest.split(' ');
                    let lo: u64 = num_of(it.next())?;
                    let hi: u64 = num_of(it.next())?;
                    if lo == 0 || lo > hi || hi > last || holes.back().is_some_and(|h| h.1 >= lo) {
                        return Err(CarryError::Shape("holes out of order"));
                    }
                    holes.push_back((lo, hi, hole_why(it)));
                    if holes.len() > MAX_HOLES {
                        return Err(CarryError::Bound("more holes than a log keeps"));
                    }
                }
                "cpar" => {
                    let mut it = rest.split(' ');
                    let n = node_of(it.next())?;
                    let p = node_of(it.next())?;
                    if !is_issued(n) || !is_issued(p) {
                        return Err(CarryError::Shape("a node past the carried counters"));
                    }
                    cpar.push_back((n, p));
                    if cpar.len() > CLOSED_PARENTS {
                        return Err(CarryError::Bound("more closed parents than a log keeps"));
                    }
                }
                "node" => {
                    let n = node_of(rest.split(' ').next())?;
                    let par = match kv_of(line, 2, "par") {
                        Some("?") => Node::INSTANCE,
                        p => node_of(p)?,
                    };
                    let below: u64 = num_of(kv_of(line, 2, "below"))?;
                    if !is_issued(n) || (par != Node::INSTANCE && !is_issued(par)) {
                        return Err(CarryError::Shape("a node past the carried counters"));
                    }
                    if index.len() > super::MAX_RECORDS + MAX_OPEN + 1 {
                        return Err(CarryError::Bound("more nodes than a log indexes"));
                    }
                    index.insert(
                        n,
                        NodeInfo {
                            parent: if n == Node::SESSION {
                                Node::SESSION
                            } else {
                                par
                            },
                            open: false,
                            live: 0,
                            closed_below: below,
                            open_bytes: 0,
                        },
                    );
                }
                "kids" => {
                    let mut it = rest.split(' ');
                    let par = match it.next() {
                        Some("?") => Node::INSTANCE,
                        p => node_of(p)?,
                    };
                    if par != Node::INSTANCE && !is_issued(par) {
                        return Err(CarryError::Shape("a node past the carried counters"));
                    }
                    for n in it.next().ok_or(CarryError::Line("a kids line"))?.split(',') {
                        let n = node_of(Some(n))?;
                        if !is_issued(n) || n == Node::SESSION {
                            return Err(CarryError::Shape("a node past the carried counters"));
                        }
                        if index.len() > super::MAX_RECORDS + MAX_OPEN + 1 {
                            return Err(CarryError::Bound("more nodes than a log indexes"));
                        }
                        index.insert(
                            n,
                            NodeInfo {
                                parent: par,
                                open: false,
                                live: 0,
                                closed_below: 0,
                                open_bytes: 0,
                            },
                        );
                    }
                }
                "open" => {
                    let n = node_of(rest.split(' ').next())?;
                    let g = |k: &str| kv_of(line, 2, k).ok_or(CarryError::Line("an open line"));
                    let kind = Kind::from_word(g("kind")?);
                    let parent = node_of(Some(g("par")?))?;
                    let t: u64 = g("t")?.parse().map_err(|_| CarryError::Line("a time"))?;
                    let orphan = flag(g("orphan")?)?;
                    let roll = read_roll(g("roll")?).ok_or(CarryError::Line("a rollup"))?;
                    if !matches!(n.lvl, Lvl::Run | Lvl::Turn | Lvl::Step)
                        || kind.node_level() != Some(n.lvl)
                        || !is_issued(n)
                    {
                        return Err(CarryError::Shape("an open node of the wrong level"));
                    }
                    if parent != Node::SESSION && !open.iter().any(|e| e.node == parent) {
                        return Err(CarryError::Shape("an open node whose parent is not open"));
                    }
                    if open.iter().any(|e| e.node == n) {
                        return Err(CarryError::Shape("a node open twice"));
                    }
                    open.push(OpenEntry {
                        node: n,
                        kind,
                        parent,
                        t,
                        mono: rebase(t, a.at).mono_ms,
                        orphan,
                        roll,
                        wall_clock: true,
                    });
                    if open.len() > MAX_OPEN {
                        return Err(CarryError::Bound("more open nodes than a log holds"));
                    }
                }
                "fold" => fold_lines.push(line.as_str()),
                "host" => {
                    host.push(rest.to_string());
                    if host.len() > CARRY_MAX_HOST_LINES || line_bytes(&host) > CARRY_MAX_HOST_BYTES
                    {
                        return Err(CarryError::Bound("more host lines than a carry passes"));
                    }
                }
                "r" | "R" => {
                    let (ms, rest) = if tag == "R" {
                        let (n, rest) = rest
                            .split_once(' ')
                            .ok_or(CarryError::Record("a record line without its ms"))?;
                        let n: u64 = n
                            .parse()
                            .map_err(|_| CarryError::Record("a record line without its ms"))?;
                        (n, rest)
                    } else {
                        (0, rest)
                    };
                    let r = decode(rest)
                        .map_err(|_| CarryError::Record("a record that does not decode"))?;
                    match ms_head {
                        // A mirrored carry's numbers rise and stay within its
                        // head: the format's own law. WHICH records carry one
                        // is the writer's bus class, which a newer build may
                        // have grown or narrowed, so it is not judged by this
                        // build's: a record this build calls bus class with
                        // no `ms` stays unnumbered (its writer never mirrored
                        // it), and one it calls local keeps its `ms` (said on
                        // the bus as a cover when a take reaches it, never a
                        // hole).
                        Some(h) => {
                            let prev = rec_ms.iter().rev().find(|m| **m > 0).copied().unwrap_or(0);
                            if ms > h || (ms > 0 && ms <= prev) {
                                return Err(CarryError::Record("a record's ms out of order"));
                            }
                        }
                        None if tag == "R" => {
                            return Err(CarryError::Record("an ms in an unmirrored carry"));
                        }
                        None => {}
                    }
                    if recs.last().is_some_and(|p| p.seq >= r.seq) || r.seq == 0 || r.seq > last {
                        return Err(CarryError::Record("records out of seq order"));
                    }
                    if !is_issued(r.node) || r.par.is_some_and(|p| !is_issued(p)) {
                        return Err(CarryError::Shape("a node past the carried counters"));
                    }
                    // A legal ring may retain its open path, the newest record and
                    // ancestor closes above a tiny cap. Bound decoding first;
                    // validate that every excess record is mandatory below.
                    let ceiling = a.limits.records.max(MAX_OPEN + MAX_DEPTH + 3);
                    if recs.len() >= ceiling {
                        return Err(CarryError::Bound("more records than a ring holds"));
                    }
                    recs.push(r);
                    rec_ms.push(ms);
                }
                _ => return Err(CarryError::Line("a carry line this build does not read")),
            }
        }
        let (kind, opened, session_t, session_roll) =
            sess.ok_or(CarryError::Shape("no session line"))?;
        if recs
            .iter()
            .any(|r| holes.iter().any(|&(lo, hi, _)| lo <= r.seq && r.seq <= hi))
        {
            return Err(CarryError::Record("a record in a seq range never held"));
        }
        for e in &open {
            if !recs.iter().any(|r| r.node == e.node && r.op == Op::Open) {
                return Err(CarryError::Shape("an open node without its open record"));
            }
        }
        if recs.len() as u64 != carried {
            return Err(CarryError::Shape("a carry missing records"));
        }
        let fold = Fold::from_snapshot(&fold_lines).map_err(CarryError::Fold)?;
        if fold.state().closed.is_some()
            || recs
                .iter()
                .any(|r| r.node == Node::SESSION && r.op == Op::Close)
        {
            return Err(CarryError::Shape("a closed session is not carried"));
        }
        if fold.state().as_of > last {
            return Err(CarryError::Shape("a fold past its log's head"));
        }

        let mut log = Log::with_limits(a.ep, a.limits);
        log.base_ep = base;
        log.first_seq = first;
        log.last_seq = last;
        log.lost = lost;
        log.newest_lost = newest_lost.min(last);
        log.holes = holes;
        log.closed_parents = cpar;
        log.issued = [
            issued[0].max(a.ids.run),
            issued[1].max(a.ids.turn),
            issued[2].max(a.ids.step),
        ];
        log.session_opened = opened;
        log.session_kind = kind;
        log.session_t = session_t;
        log.session_mono = rebase(session_t, a.at).mono_ms;
        log.session_wall_clock = true;
        log.session_roll = session_roll;
        log.fold = fold;
        let mut session_info = index.remove(&Node::SESSION).unwrap_or(NodeInfo {
            parent: Node::SESSION,
            open: true,
            live: 0,
            closed_below: 0,
            open_bytes: 0,
        });
        session_info.parent = Node::SESSION;
        session_info.open = true;
        index.insert(Node::SESSION, session_info);
        for e in &open {
            index
                .entry(e.node)
                .and_modify(|i| {
                    i.open = true;
                    i.parent = e.parent;
                })
                .or_insert(NodeInfo {
                    parent: e.parent,
                    open: true,
                    live: 0,
                    closed_below: 0,
                    open_bytes: 0,
                });
        }
        log.nodes = index;
        log.open = open;
        // The bus class's numbering: carried, or (a carry without it, ruling
        // Q7) densely from 1 in seq order over the carried bus-class records.
        let mut dense = 0u64;
        for (rec, ms) in recs.into_iter().zip(rec_ms) {
            let bytes = encoded_size(&rec, Tier::Content)
                .map_err(|_| CarryError::Record("a record past its byte bound"))?;
            let ms = match ms_head {
                Some(_) => ms,
                None if rec.is_bus_class() => {
                    dense += 1;
                    dense
                }
                None => 0,
            };
            log.index_carried(&rec, bytes);
            log.heap = log.heap.saturating_add(super::entry_heap(&rec));
            log.ring.push_back(super::Entry { rec, bytes, ms });
            log.bytes = log.bytes.saturating_add(bytes);
        }
        log.ms_head = ms_head.unwrap_or(dense);
        if let Some((landed, snap_ms, prior)) = mirror {
            if landed.ep != base || landed.ms > log.ms_head || landed.seq > last {
                return Err(CarryError::Shape("a mirror cursor past its log"));
            }
            log.set_mirror(landed, snap_ms, prior);
        }
        if (log.len() > a.limits.records || log.bytes > a.limits.bytes)
            && !log.only_mandatory_records()
        {
            return Err(CarryError::Bound(
                "nonmandatory records past the ring bounds",
            ));
        }
        let spent: Vec<Node> = log
            .nodes
            .iter()
            .filter(|(n, i)| **n != Node::SESSION && !i.open && i.live == 0)
            .map(|(n, _)| *n)
            .collect();
        for n in spent {
            log.nodes.remove(&n);
        }
        log.skip_to(a.next_seq);
        let mut f = Fields::new();
        // Each value is of its key's shape and the record far under the byte
        // bound: none of these can be refused.
        let _ = f.set(crate::field::Key::Carried, Val::U(carried));
        let _ = f.set(crate::field::Key::Shed, Val::U(shed));
        let _ = f.set(crate::field::Key::Epoch, Val::U(u64::from(a.ep)));
        let _ = log.mark(
            super::Mark::new(Kind::Handoff, Src::Aterm, Conf::Exact)
                .at(Node::SESSION)
                .by(Actor::Aterm)
                .fields(f),
            a.at,
        );
        Ok((
            log,
            host,
            CarryStats {
                carried,
                shed,
                head: last,
            },
        ))
    }

    /// A ring over its ordinary limits may hold only the open path, the
    /// newest record and its ancestor closes. Match opens to the carried
    /// skeleton and reject duplicate opens/closes: repeated purported pinned
    /// records are not an exemption from the receiver's bounds.
    fn only_mandatory_records(&self) -> bool {
        let Some(newest) = self.ring.back() else {
            return true;
        };
        let shelter = self.chain(newest.rec.node);
        let mut opens = BTreeSet::new();
        let mut closes = BTreeSet::new();
        self.ring.iter().all(|e| {
            let r = &e.rec;
            if !r.k.fits(r.op, r.node.lvl) {
                return false;
            }
            match r.op {
                Op::Open => {
                    if !opens.insert(r.node) {
                        return false;
                    }
                    if r.node == Node::SESSION {
                        self.session_opened
                            && r.k == self.session_kind
                            && r.t == self.session_t
                            && r.par.is_none()
                    } else {
                        self.open_entry(r.node).is_some_and(|o| {
                            r.k == o.kind
                                && r.par == Some(o.parent)
                                && r.t == o.t
                                && r.f.get_u(crate::field::Key::Orphan).unwrap_or(0)
                                    == u64::from(o.orphan)
                        })
                    }
                }
                Op::Close => {
                    closes.insert(r.node)
                        && (r.seq == self.last_seq
                            || (matches!(r.node.lvl, Lvl::Run | Lvl::Turn)
                                && shelter.contains(&r.node)))
                }
                _ => r.seq == self.last_seq,
            }
        })
    }

    /// Index a carried record (its node's entry from the carry's own lines,
    /// or a new one), without applying it to the fold: the fold crossed as
    /// it stood.
    fn index_carried(&mut self, rec: &Rec, bytes: usize) {
        if !self.nodes.contains_key(&rec.node) {
            let parent = rec
                .par
                .or_else(|| self.parent_of(rec.node))
                .unwrap_or(Node::INSTANCE);
            self.nodes.insert(
                rec.node,
                NodeInfo {
                    parent,
                    open: false,
                    live: 0,
                    closed_below: 0,
                    open_bytes: 0,
                },
            );
        }
        if rec.op == Op::Open
            && let Some(info) = self.nodes.get_mut(&rec.node)
            && info.open
        {
            info.open_bytes = bytes;
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
}

fn kv_of<'a>(line: &'a str, skip: usize, key: &str) -> Option<&'a str> {
    line.split(' ')
        .filter(|t| !t.is_empty())
        .skip(skip)
        .filter_map(|t| t.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

fn num_of(raw: Option<&str>) -> Result<u64, CarryError> {
    raw.and_then(|v| v.parse().ok())
        .ok_or(CarryError::Line("a number"))
}

fn node_of(raw: Option<&str>) -> Result<Node, CarryError> {
    raw.and_then(Node::parse)
        .filter(|n| *n != Node::INSTANCE)
        .ok_or(CarryError::Line("a node"))
}

fn flag(raw: &str) -> Result<bool, CarryError> {
    match raw {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(CarryError::Line("a flag")),
    }
}

/// The host's lines a carry passes, in order: each one the receiver reads
/// (with its `host ` tag, within [`CARRY_MAX_LINE`]), as many as fit
/// [`CARRY_MAX_HOST_LINES`] and [`CARRY_MAX_HOST_BYTES`]. A line past them is
/// left out, never written: the receiver refuses a whole carry for one line
/// past its bound, where the host reads each of its lines on its own and a
/// missing one costs only that piece of its state.
fn host_within_bounds(host: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    for h in host {
        if out.len() == CARRY_MAX_HOST_LINES {
            break;
        }
        if "host ".len() + h.len() > CARRY_MAX_LINE || bytes + h.len() + 1 > CARRY_MAX_HOST_BYTES {
            continue;
        }
        bytes += h.len() + 1;
        out.push(h.clone());
    }
    out
}

impl CarrySnapshot {
    /// One header format for both byte reservation and final output.
    fn header(&self, carried: usize, shed: usize, newest_lost: u64, lines: usize) -> String {
        let mut h = format!(
            "slog-carry v=1 ep={} base={} first={} last={} lost={} newest_lost={} ids={},{},{} carried={carried} shed={shed} lines={lines}",
            self.ep,
            self.base_ep,
            self.first_seq,
            self.last_seq,
            self.lost.saturating_add(shed as u64),
            newest_lost,
            self.issued[0],
            self.issued[1],
            self.issued[2],
        );
        // Only a mirrored log's header says more (ruling Q5): an older
        // reader reads the keys it knows and skips these.
        if let Some(m) = &self.mirror {
            let _ = write!(
                h,
                " ms={} mirror={} snap={}",
                self.ms_head,
                m.landed.word(),
                m.snap_ms
            );
            if let Some(p) = m.prior {
                let _ = write!(h, " prior={}:{}", p.landed.word(), p.ms_head);
            }
        }
        h
    }

    /// A record's carry line, with its `ms` when the log is mirrored.
    fn line_of(&self, r: &Rec) -> Option<String> {
        rec_line(r, self.ms_of.get(&r.seq).copied())
    }

    /// The bytes of the lines that always cross: the header, the session,
    /// the holes, closed parents and open path, the fold, the host's lines
    /// and the records that are never cut (the open path's own, the newest).
    /// The node lines the kept records need are counted with them as the cut
    /// keeps them.
    fn fixed_lines(&self, host: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        out.push(format!(
            "sess kind={} opened={} closed=0 t={} roll={}",
            self.session.0.word(),
            u8::from(self.session.1),
            self.session.2,
            roll_word(&self.session.3)
        ));
        for &(lo, hi, why) in &self.holes {
            out.push(hole_line(lo, hi, why));
        }
        for (n, p) in &self.closed_parents {
            out.push(format!("cpar {n} {p}"));
        }
        out.extend(self.fold_lines.iter().cloned());
        for h in host {
            out.push(format!("host {h}"));
        }
        out
    }

    /// The index lines the records `recs` need: a `node` line for the
    /// session and each open node (its parent, and the newest close beneath
    /// it, which the eviction class reads), and `kids <parent> <node>,…`
    /// lines naming the parent of every other node on the records' parent
    /// chains whose own open record (which names it) does not cross.
    fn index_lines<'a>(&self, recs: impl Iterator<Item = &'a Rec> + Clone) -> Vec<String> {
        let opened: std::collections::BTreeSet<Node> = recs
            .clone()
            .filter(|r| r.op == Op::Open)
            .map(|r| r.node)
            .collect();
        let mut need: std::collections::BTreeSet<Node> = std::collections::BTreeSet::new();
        need.insert(Node::SESSION);
        for e in &self.open {
            need.insert(e.node);
        }
        for r in recs {
            let mut at = r.node;
            for _ in 0..MAX_DEPTH {
                if !need.insert(at) || at == Node::SESSION {
                    break;
                }
                match self.index.get(&at) {
                    Some((p, ..)) if *p != Node::INSTANCE => at = *p,
                    _ => break,
                }
            }
        }
        let mut out = Vec::new();
        let mut kids: BTreeMap<Node, Vec<Node>> = BTreeMap::new();
        for n in need {
            let Some((par, open, below)) = self.index.get(&n) else {
                continue;
            };
            if n == Node::SESSION || *open {
                let par = if *par == Node::INSTANCE {
                    "?".to_string()
                } else {
                    par.to_string()
                };
                out.push(format!(
                    "node {n} par={par} open={} below={below}",
                    u8::from(*open)
                ));
            } else if !opened.contains(&n) {
                kids.entry(*par).or_default().push(n);
            }
        }
        for (par, nodes) in kids {
            let head = if par == Node::INSTANCE {
                "kids ?".to_string()
            } else {
                format!("kids {par}")
            };
            let mut line = head.clone();
            let mut first = true;
            for n in nodes {
                let item = n.to_string();
                if line.len() + item.len() + 1 > CARRY_MAX_LINE - 8 {
                    out.push(std::mem::replace(&mut line, head.clone()));
                    first = true;
                }
                line.push(if first { ' ' } else { ',' });
                line.push_str(&item);
                first = false;
            }
            out.push(line);
        }
        out
    }

    /// Encode the carry lines of the records that always cross, once (after
    /// the log's lock is released): the [`PreparedCarry`] every cut is made
    /// from. A record that no longer encodes (never one the log accepted) is
    /// left out.
    #[must_use]
    pub fn prepare(mut self) -> PreparedCarry {
        let mut keep_lines = Vec::with_capacity(self.keep.len());
        let ms_of = std::mem::take(&mut self.ms_of);
        self.keep
            .retain(|r| match rec_line(r, ms_of.get(&r.seq).copied()) {
                Some(l) => {
                    keep_lines.push(l);
                    true
                }
                None => false,
            });
        self.ms_of = ms_of;
        PreparedCarry {
            snap: self,
            keep_lines,
        }
    }

    /// The carry within `budget`, the host's lines passed through: the
    /// snapshot prepared ([`CarrySnapshot::prepare`]) and cut once
    /// ([`PreparedCarry::encode`]).
    #[must_use]
    pub fn encode(&self, budget: CarryBudget, host: &[String]) -> Option<CarryOut> {
        self.clone().prepare().encode(budget, host)
    }

    /// The session line with `roll` as its rollup.
    fn clone_session_with(&self, roll: Rollup) -> Vec<String> {
        vec![format!(
            "sess kind={} opened={} closed=0 t={} roll={}",
            self.session.0.word(),
            u8::from(self.session.1),
            self.session.2,
            roll_word(&roll)
        )]
    }

    /// The last seq the snapshot holds.
    #[must_use]
    pub const fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// The records it holds, the ones that always cross and the cut's.
    #[must_use]
    pub fn records(&self) -> usize {
        self.keep.len() + self.victims.len()
    }
}

/// A carry snapshot whose records that always cross are encoded
/// ([`CarrySnapshot::prepare`]): carries of any budget are cut from it, each
/// encoding only the other records it keeps.
#[derive(Clone, Debug)]
pub struct PreparedCarry {
    snap: CarrySnapshot,
    /// The carry line of each of `snap.keep`, in its order.
    keep_lines: Vec<String>,
}

impl PreparedCarry {
    /// Only the mandatory part, within the session's skeleton byte bound.
    /// No optional record crosses; `None` for a closed session or a skeleton
    /// that cannot fit. Hosts use this to reserve each session's minimum.
    #[must_use]
    pub fn skeleton(&self, host: &[String]) -> Option<CarryOut> {
        self.encode(
            CarryBudget {
                total: CARRY_SKELETON_BYTES,
                skeleton: CARRY_SKELETON_BYTES,
                records: self.snap.keep.len(),
            },
            host,
        )
    }

    /// The bytes the records past the skeleton take as carry lines, at most:
    /// what a carry with room for all of them adds to its skeleton's (the
    /// node lines they need aside).
    #[must_use]
    pub fn detail_bytes(&self) -> usize {
        self.snap.victims.iter().map(|v| v.est).sum()
    }

    /// How many records past the skeleton a carry with room for all of them
    /// adds.
    #[must_use]
    pub fn detail_records(&self) -> usize {
        self.snap.victims.len()
    }

    /// The carry within `budget`, the host's lines passed through (each one
    /// the receiver reads: [`host_within_bounds`]), or `None` when the part
    /// that always crosses exceeds any requested bound, or the session is
    /// closed. Every line it writes is
    /// within [`CARRY_MAX_LINE`]: a record within [`crate::MAX_RECORD_BYTES`],
    /// a fold line within its keys' caps, a `kids` line cut to the bound.
    #[must_use]
    pub fn encode(&self, budget: CarryBudget, host: &[String]) -> Option<CarryOut> {
        let snap = &self.snap;
        if snap.closed {
            return None;
        }
        let host = host_within_bounds(host);
        let fixed = snap.fixed_lines(&host);
        let keep_lines = &self.keep_lines;
        let open_lines = |snap: &CarrySnapshot, shed: &[u32]| -> Vec<String> {
            snap.open
                .iter()
                .zip(shed)
                .map(|(e, extra)| {
                    let mut roll = e.roll;
                    roll.lost = roll.lost.saturating_add(*extra);
                    format!(
                        "open {} kind={} par={} t={} orphan={} roll={}",
                        e.node,
                        e.kind.word(),
                        e.parent,
                        e.t,
                        u8::from(e.orphan),
                        roll_word(&roll)
                    )
                })
                .collect()
        };
        // Reserve the header and rollup widths for any cut. Each loss counter
        // can grow by at most the number of optional records; line count uses
        // its type's maximum so no fixed numeric-width estimate can underflow.
        let maximum_loss = u32::try_from(snap.victims.len()).unwrap_or(u32::MAX);
        let maximum_extra = vec![maximum_loss; snap.open.len()];
        let mut maximum_session = snap.session.3;
        maximum_session.lost = maximum_session.lost.saturating_add(maximum_loss);
        let header_room = snap
            .header(
                snap.records(),
                snap.victims.len(),
                snap.last_seq,
                usize::MAX,
            )
            .len()
            + 1;
        let skeleton_nodes = snap.index_lines(snap.keep.iter());
        let skeleton = header_room
            + line_bytes(&fixed[1..])
            + line_bytes(&snap.clone_session_with(maximum_session))
            + line_bytes(keep_lines)
            + line_bytes(&open_lines(snap, &maximum_extra))
            + line_bytes(&skeleton_nodes);
        if skeleton > budget.skeleton || skeleton > budget.total || snap.keep.len() > budget.records
        {
            return None;
        }
        // The cut: shed victims, first first, until the rest fits.
        let mut cut = 0usize;
        let room = budget.total.saturating_sub(skeleton);
        let records_room = budget.records.saturating_sub(snap.keep.len());
        let victim_bytes = |i: usize| snap.victims[i].est;
        let mut rest_bytes: usize = self.detail_bytes();
        let node_cost = |snap: &CarrySnapshot, cut: usize| -> usize {
            line_bytes(
                &snap.index_lines(
                    snap.keep
                        .iter()
                        .chain(snap.victims[cut..].iter().map(|v| &v.rec)),
                ),
            )
        };
        let skeleton_node_bytes = line_bytes(&skeleton_nodes);
        let n = snap.victims.len();
        loop {
            while cut < n && (rest_bytes > room || n - cut > records_room) {
                rest_bytes -= victim_bytes(cut);
                cut += 1;
            }
            let extra_nodes = node_cost(snap, cut).saturating_sub(skeleton_node_bytes);
            let over = (rest_bytes + extra_nodes).saturating_sub(room);
            if over == 0 || cut == n {
                break;
            }
            // The nodes the kept records need did not fit: shed as many more
            // as free that much (shedding them frees node lines too, so a
            // pass or two settles it).
            let mut freed = 0;
            while cut < n && freed < over {
                freed += victim_bytes(cut);
                rest_bytes -= victim_bytes(cut);
                cut += 1;
            }
        }
        // What the cut shed counts as an eviction does: in `lost=`, the
        // newest such seq, and its nearest open node's rollup.
        let shed = &snap.victims[..cut];
        let mut extra = vec![0u32; snap.open.len()];
        let mut session_roll = snap.session.3;
        for v in shed {
            match v
                .target
                .and_then(|t| snap.open.iter().position(|e| e.node == t))
            {
                Some(i) => extra[i] = extra[i].saturating_add(1),
                None => session_roll.lost = session_roll.lost.saturating_add(1),
            }
        }
        let newest_lost = shed
            .iter()
            .map(|v| v.rec.seq)
            .max()
            .unwrap_or(0)
            .max(snap.newest_lost);
        // The records that cross, in seq order, each with its line: the
        // cut's own encoded now, the only time they are.
        let crossing: Vec<(&Rec, String)> = snap.victims[cut..]
            .iter()
            .filter_map(|v| snap.line_of(&v.rec).map(|l| (&v.rec, l)))
            .collect();
        let mut recs: Vec<(&Rec, &String)> = snap
            .keep
            .iter()
            .zip(keep_lines)
            .chain(crossing.iter().map(|(r, l)| (*r, l)))
            .collect();
        recs.sort_by_key(|(r, _)| r.seq);
        let node_lines = snap.index_lines(recs.iter().map(|(r, _)| *r));
        let mut body: Vec<String> = Vec::new();
        let mut snap_session = snap.clone_session_with(session_roll);
        body.append(&mut snap_session);
        body.extend(fixed.into_iter().skip(1));
        body.extend(node_lines);
        body.extend(open_lines(snap, &extra));
        let carried = recs.len() as u64;
        body.extend(recs.into_iter().map(|(_, l)| l.clone()));
        let head = snap.header(carried as usize, cut, newest_lost, body.len() + 1);
        let mut lines = Vec::with_capacity(body.len() + 1);
        lines.push(head);
        lines.extend(body);
        let bytes = line_bytes(&lines);
        if bytes > budget.total || carried as usize > budget.records {
            return None;
        }
        Some(CarryOut {
            lines,
            carried,
            shed: cut as u64,
            bytes,
            skeleton_bytes: skeleton,
            last_seq: snap.last_seq,
        })
    }

    /// The last seq the snapshot holds.
    #[must_use]
    pub const fn last_seq(&self) -> u64 {
        self.snap.last_seq
    }

    /// THE MIRROR UNDER THE FENCE (phase 4, ruling Q7): the landed cursor and
    /// the `ms` head as the snapshot (and the readers' fence beside it) drew
    /// them, for a mirrored log; `None` for one nothing ever mirrored. What a
    /// successor that does not get this carry says of the old space's tail:
    /// exact, because no landing passes the fence.
    #[must_use]
    pub fn mirror_prior(&self) -> Option<super::Prior> {
        self.snap.mirror.as_ref().map(|m| super::Prior {
            landed: m.landed,
            ms_head: self.snap.ms_head,
        })
    }
}
