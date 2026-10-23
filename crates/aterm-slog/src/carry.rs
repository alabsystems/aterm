// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session log carry encoding and decoding across seamless updates.

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
/// The most the mandatory part of a carry may take (the open path, the skeleton's state, the fold
/// and the host's lines): 32 KiB.
pub const CARRY_SKELETON_BYTES: usize = 32 * 1024;
/// The longest carry line a receiver reads.
pub const CARRY_MAX_LINE: usize = 1024;
/// The most carry lines a receiver reads.
pub const CARRY_MAX_LINES: usize = 8192;
/// The most carry bytes a receiver reads.
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
    /// A session's (§5.2): 128 KiB, the skeleton within 32 KiB, a ring's records.
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
            CarryError::Fold(SnapError::Log) => "a fold that disagrees with its log",
            CarryError::Fold(_) => "a fold snapshot this build does not read",
        }
    }
}

/// What the successor continues a carry with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CarryAdopt {
    /// Its epoch: above the carried one.
    pub ep: u32,
    /// Its first seq: above every seq the predecessor could have used (the manifest's count plus
    /// one).
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
    /// The carried log's last seq (its `last=`).
    pub head: u64,
}

#[derive(Clone, Debug)]
struct Victim {
    rec: Rec,
    target: Option<Node>,
    est: usize,
}

/// Everything a carry needs of a log, copied under its lock.
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
    ms_head: u64,
    mirror: Option<super::mirror::Mirror>,
    ms_of: BTreeMap<u64, u64>,
}

/// One `hole <lo> <hi>` line: an update's overlap as before, any other range (a journal read's,
/// `Log::from_journal`) with its ` why=<w>`, which a reader that predates it skips as an unknown
/// token.
pub(super) fn hole_line(lo: u64, hi: u64, why: GapWhy) -> String {
    if why == GapWhy::Handoff {
        format!("hole {lo} {hi}")
    } else {
        format!("hole {lo} {hi} why={}", why.word())
    }
}

/// The why of a `hole` line's remaining tokens.
pub(super) fn hole_why<'a>(mut rest: impl Iterator<Item = &'a str>) -> GapWhy {
    rest.find_map(|t| t.strip_prefix("why="))
        .and_then(GapWhy::from_word)
        .unwrap_or(GapWhy::Handoff)
}

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

fn rec_line(r: &Rec, ms: Option<u64>) -> Option<String> {
    let l = encode(r, Tier::Host).ok()?;
    Some(match ms {
        Some(ms) => format!("R {ms} {l}"),
        None => format!("r {l}"),
    })
}

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
    /// The snapshot a carry is cut from: the log's state, its fold's snapshot, and its records in
    /// the order the cut sheds them.
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
                    rec: Rec::clone(&e.rec),
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
            keep: keep
                .into_iter()
                .map(|i| Rec::clone(&self.ring[i].rec))
                .collect(),
            victims,
            ms_head: self.ms_head,
            mirror: self.mirror.as_deref().cloned(),
            ms_of,
        }
    }

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

    /// This log as a carry within `budget`, the host's `host` lines passed through.
    #[must_use]
    pub fn carry(&self, budget: CarryBudget, host: &[String]) -> Option<CarryOut> {
        self.carry_snapshot().prepare().encode(budget, host)
    }

    /// Fence this log's readers at its head (the carry an update just took, design §5.2).
    pub fn fence_reads(&mut self) {
        self.fence = Some(Box::new((self.last_seq, self.fold.clone())));
        self.fence_ms = self.ms_head;
        self.rang();
    }

    /// Lift the readers' fence (the update stood down; this process goes on).
    pub fn lift_read_fence(&mut self) {
        self.fence = None;
        self.enforce_mark_quota();
        self.enforce_program_share(None);
        self.rang();
    }

    /// The fenced seq, while readers are fenced.
    #[must_use]
    pub fn read_fence(&self) -> Option<u64> {
        self.fence.as_ref().map(|f| f.0)
    }

    /// The state a reader is shown: the fold's, or, while fenced, the fold as it stood at the
    /// fence.
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

    /// Reconstruct the successor's log from a carry `lines` wrote ([`Log::carry`]).
    /// # Errors
    /// A [`CarryError`] for anything this build does not read or that does not hold together.
    pub fn from_carry(
        lines: &[String],
        a: CarryAdopt,
    ) -> Result<(Log, Vec<String>, CarryStats), CarryError> {
        Log::from_carry_built(lines, a, None)
    }

    /// Reconstruct the successor's log from `lines`, recording optional predecessor `build`.
    /// # Errors
    /// As [`Log::from_carry`].
    pub fn from_carry_built(
        lines: &[String],
        a: CarryAdopt,
        build: Option<u64>,
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
        if last > super::SEQ_LIMIT || a.next_seq > super::SEQ_LIMIT.saturating_add(1) {
            return Err(CarryError::Bound("a seq past the limit"));
        }
        if ms_head.is_some_and(|head| head > last) {
            return Err(CarryError::Bound("a mirror count past the sequence head"));
        }
        if let Some((_, snap_ms, prior)) = mirror {
            if snap_ms > ms_head.unwrap_or(0) {
                return Err(CarryError::Shape("a snapshot past the mirror head"));
            }
            if let Some(p) = prior
                && (p.ms_head > super::SEQ_LIMIT
                    || p.landed.seq > super::SEQ_LIMIT
                    || p.landed.ms > p.ms_head)
            {
                return Err(CarryError::Bound("a prior mirror space past its domain"));
            }
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
        fold.agrees_with_open(&open).map_err(CarryError::Fold)?;
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
            let mono = rebase(rec.t, a.at).mono_ms;
            log.ring.push_back(super::Entry {
                rec: rec.into(),
                bytes,
                ms,
                mono,
                row: super::rows::RowTag::NONE,
            });
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
        let _ = f.set(crate::field::Key::Carried, Val::U(carried));
        let _ = f.set(crate::field::Key::Shed, Val::U(shed));
        let _ = f.set(crate::field::Key::Epoch, Val::U(u64::from(a.ep)));
        if let Some(build) = build {
            let _ = f.set(crate::field::Key::Build, Val::U(build));
        }
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
            && rec.node != Node::SESSION
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

    fn line_of(&self, r: &Rec) -> Option<String> {
        rec_line(r, self.ms_of.get(&r.seq).copied())
    }

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

    /// Encode the carry lines of the records that always cross, once (after the log's lock is
    /// released): the [`PreparedCarry`] every cut is made from.
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

    /// The carry within `budget`, the host's lines passed through.
    #[must_use]
    pub fn encode(&self, budget: CarryBudget, host: &[String]) -> Option<CarryOut> {
        self.clone().prepare().encode(budget, host)
    }

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
    pub const fn records(&self) -> usize {
        self.keep.len() + self.victims.len()
    }
}

/// A carry snapshot whose records that always cross are encoded ([`CarrySnapshot::prepare`]).
#[derive(Clone, Debug)]
pub struct PreparedCarry {
    snap: CarrySnapshot,
    keep_lines: Vec<String>,
}

impl PreparedCarry {
    /// Only the mandatory part, within the session's skeleton byte bound.
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

    /// The bytes the records past the skeleton take as carry lines, at most.
    #[must_use]
    pub fn detail_bytes(&self) -> usize {
        self.snap.victims.iter().map(|v| v.est).sum()
    }

    /// How many records past the skeleton a carry with room for all of them adds.
    #[must_use]
    pub const fn detail_records(&self) -> usize {
        self.snap.victims.len()
    }

    /// Encode the carry within `budget` with `host` lines passed through, or `None` if over budget.
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
            let mut freed = 0;
            while cut < n && freed < over {
                freed += victim_bytes(cut);
                rest_bytes -= victim_bytes(cut);
                cut += 1;
            }
        }
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

    /// Landed cursor and `ms` head for a mirrored log, or `None` if never mirrored.
    #[must_use]
    pub fn mirror_prior(&self) -> Option<super::Prior> {
        self.snap.mirror.as_ref().map(|m| super::Prior {
            landed: m.landed,
            ms_head: self.snap.ms_head,
        })
    }
}
