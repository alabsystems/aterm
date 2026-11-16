// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptions as claims (streams design §6): what a session, or one of its agent runs, is said to be about. A `desc` mark fills one slot (`title`, `description`, or any other word) of one subject; the fold keeps each author's claim apart, ranks an author by who it is, and says which claim stands. A claim is never an observation and never an instruction: every one is listed with its author, and the lower-ranked ones are kept, not overwritten.
//!
//! No clock is read here: a claim's `ttl` lapse is judged against the `now_wall` a reader passes, and the fold settles a slot at the time of the record that moved it.

use std::cmp::Reverse;
use std::fmt::Write as _;

use aterm_codec::pct;

use super::snap::{get, node_at, num, opt_num, tok_at};
use super::{Fold, OpenNode, SnapError};
use crate::field::{BoundedText, BoundedToken, Key, Val};
use crate::rec::Rec;
use crate::vocab::{Actor, Conf, Kind, Lvl, Node, ShortId, Src};

/// The slots this build knows, in the order they are listed. Any other slot is stored and listed the same way, within [`DESC_UNKNOWN_SLOTS`].
pub const KNOWN_SLOTS: &[&str] = &["title", "description"];
/// The most agent runs that hold claims at once; a fourth agent's first claim drops the oldest agent's.
pub const DESC_AGENTS: usize = 3;
/// The most slots outside [`KNOWN_SLOTS`] one subject holds; a claim for a ninth is dropped and counted.
pub const DESC_UNKNOWN_SLOTS: usize = 8;
/// The most claims one slot of one subject holds, one per author.
pub const DESC_CLAIMS: usize = 8;
/// The turn closes the store remembers, newest last, and as many again of each agent subject's own: how far back a claim's `upto=` is charged exactly.
pub const DESC_TURN_RING: usize = 8;
/// The most bytes of `fold desc…` lines a carry snapshot writes (each line with its newline). The carry's fold lines are part of its mandatory 32 KiB skeleton, so the claims take a fixed quarter of it and the rest are left out and counted.
pub const DESC_CARRY_BYTES: usize = 8 * 1024;

/// The most slots one subject holds: every known slot and [`DESC_UNKNOWN_SLOTS`] others (2 + 8 = 10).
pub const DESC_SLOTS: usize = KNOWN_SLOTS.len() + DESC_UNKNOWN_SLOTS;
/// The most claims the store holds: the session and [`DESC_AGENTS`] agents, [`DESC_SLOTS`] slots each, [`DESC_CLAIMS`] claims a slot (4 × 10 × 8 = 320).
pub const DESC_CLAIMS_MAX: usize = (1 + DESC_AGENTS) * DESC_SLOTS * DESC_CLAIMS;

/// A claim's text at most: `value=` is capped at 256 percent-encoded bytes (an in-band `title=` at 240), and encoding never shortens.
const VALUE_BYTES: usize = 256;
/// A claim's `as=` name at most (40 encoded bytes).
const AS_BYTES: usize = 40;
/// A slot's name at most (`field=`, 24 bytes).
const SLOT_BYTES: usize = 24;

/// The most text the store owns: per slot its name and eight claims of a value and a name each (24 + 8 × (256 + 40) = 2,392 B), ten slots a subject (23,920 B), four subjects (95,680 B).
pub const DESC_TEXT_BYTES: usize =
    (1 + DESC_AGENTS) * DESC_SLOTS * (SLOT_BYTES + DESC_CLAIMS * (VALUE_BYTES + AS_BYTES));
/// The most heap [`Described::heap`] reports: [`DESC_TEXT_BYTES`], the 320 claims' and 40 slots' own structs, the three agent subjects (the session's is inline in the state), the turn ring, and each agent subject's own ring of eight seqs (3 × 8 × 8 = 192 B).
pub const DESC_HEAP_BYTES: usize = DESC_TEXT_BYTES
    + DESC_CLAIMS_MAX * std::mem::size_of::<DescClaim>()
    + (1 + DESC_AGENTS) * DESC_SLOTS * std::mem::size_of::<DescSlot>()
    + DESC_AGENTS * std::mem::size_of::<DescSubject>()
    + DESC_TURN_RING * std::mem::size_of::<(u64, Node)>()
    + DESC_AGENTS * DESC_TURN_RING * std::mem::size_of::<u64>();

/// Who the session is, for ranking a claim written by a session ([`rank_of`]): no record says it, so the host does ([`crate::log::Log::set_desc_ident`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DescIdent {
    /// The session's own sid: a claim by it is the session's program speaking of itself.
    pub own: Option<ShortId>,
    /// The sid of the session that created this one.
    pub creator: Option<ShortId>,
}

/// How much an author's claim counts. A lower number stands over a higher one (so `Person < Listed` and the best rank is the minimum); [`Rank::Listed`] never stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rank {
    /// The person at the window.
    Person = 1,
    /// The session's own program: the harness's report of it, or the session's own sid.
    Program = 2,
    /// The session that created this one.
    Creator = 3,
    /// A writer aterm ran, or a line aterm wrote on purpose.
    Writer = 4,
    /// The owner's control token, from no session.
    Ctl = 5,
    /// Bytes the program printed (an OSC title).
    Inband = 6,
    /// aterm's own model-free derivation.
    Derived = 7,
    /// Anyone else (another session, the bridge, another machine's driver): stored and listed, never standing.
    Listed = 8,
}

impl Rank {
    /// The word `described` prints.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Rank::Person => "person",
            Rank::Program => "program",
            Rank::Creator => "creator",
            Rank::Writer => "writer",
            Rank::Ctl => "ctl",
            Rank::Inband => "inband",
            Rank::Derived => "derived",
            Rank::Listed => "listed",
        }
    }

    /// Whether a claim of this rank can stand at all.
    #[must_use]
    pub const fn stands(self) -> bool {
        !matches!(self, Rank::Listed)
    }
}

/// THE RANK OF AN AUTHOR: a pure function of who wrote a claim (`by`), where it was observed (`src`) and who the session is (`ident`), evaluated at every read and never stored, so a session whose identity the host learns later ranks its old claims by it.
#[must_use]
pub fn rank_of(by: Actor, src: Src, ident: &DescIdent) -> Rank {
    match by {
        Actor::Human => Rank::Person,
        Actor::Harness => Rank::Program,
        Actor::Sid(id) if ident.own == Some(id) => Rank::Program,
        Actor::Sid(id) if ident.creator == Some(id) => Rank::Creator,
        Actor::Aterm if src == Src::Aterm => Rank::Derived,
        Actor::Aterm => Rank::Writer,
        Actor::Ctl => Rank::Ctl,
        Actor::Nobody if src == Src::Engine => Rank::Inband,
        Actor::Sid(_) | Actor::Nobody | Actor::Bridge | Actor::Fleet(_) | Actor::Vendor => {
            Rank::Listed
        }
    }
}

/// Whether `by` at `src` is an author of the [`Rank::Writer`] rank, whatever the session's identity: the one rank whose authors are told apart by their `as=` name too, since every writer aterm runs writes as `aterm`.
fn is_writer(by: Actor, src: Src) -> bool {
    by == Actor::Aterm && src != Src::Aterm
}

/// One author's claim for one slot of one subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescClaim {
    /// The claim's text.
    pub value: String,
    /// Its author.
    pub by: Actor,
    /// Where it was observed.
    pub src: Src,
    /// How sure its record said it was.
    pub conf: Conf,
    /// When (wall ms).
    pub t: u64,
    /// The seq of its record.
    pub seq: u64,
    /// The name its author declared for itself (`as=`): listed, never the author.
    pub as_name: Option<String>,
    /// The record or mail it is about (`re=`).
    pub re: Option<ShortId>,
    /// When it lapses (wall ms): its record's time plus its `ttl=`. `None` never lapses by time.
    pub until: Option<u64>,
    /// Whether the text is content tier (a program's own title, carried in `title=`): held in memory only, never written to a carry.
    pub content: bool,
    /// The seq its author said it covers the log up to (`upto=`, at most its own record), or `None` when the record named none.
    pub upto: Option<u64>,
    /// The turns that had closed on its subject as of `upto` (as of its own record when `upto` is `None`): what [`DescClaim::behind`] subtracts from the subject's count.
    pub base: u32,
}

impl DescClaim {
    /// Whether the claim has not lapsed by time at `now_wall`.
    #[must_use]
    pub fn live(&self, now_wall: u64) -> bool {
        self.until.is_none_or(|u| u > now_wall)
    }

    /// The author's rank under `ident` ([`rank_of`]).
    #[must_use]
    pub fn rank(&self, ident: &DescIdent) -> Rank {
        rank_of(self.by, self.src, ident)
    }

    /// TURNS BEHIND: how many turns closed on the claim's subject (`turns` closed in all) after the seq it covers. Known when the record named an `upto=`, or when its author is of a rank whose claim covers the log as it was written (`Program`, `Writer`, `Derived`: behind 0 at the write); otherwise `None`: nobody knows how current the claim was.
    #[must_use]
    pub fn behind(&self, rank: Rank, turns: u32) -> Option<u32> {
        (self.upto.is_some() || matches!(rank, Rank::Program | Rank::Writer | Rank::Derived))
            .then(|| turns.saturating_sub(self.base))
    }

    /// Whether `by` at `src` (declaring `as_name`) is this claim's author: the same actor and source, and for a writer the same declared name.
    fn is_author(&self, by: Actor, src: Src, as_name: Option<&str>) -> bool {
        self.by == by
            && self.src == src
            && (!is_writer(by, src) || self.as_name.as_deref() == as_name)
    }
}

/// One slot of one subject: its claims, one per author in the order each author first claimed, and the one that stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescSlot {
    /// The slot's name (`field=`).
    pub name: String,
    /// The claims, at most [`DESC_CLAIMS`].
    pub claims: Vec<DescClaim>,
    /// The incumbent: the index in [`Self::claims`] of the claim that stood when the slot last settled.
    pub standing: Option<usize>,
    /// Claims dropped to make room for a ninth author.
    pub dropped: u32,
}

impl DescSlot {
    /// WHICH CLAIM STANDS at `now` (`turns` closed on the subject, the session being `ident`), given the incumbent. Only a live claim whose rank stands is a candidate, and only the best rank among the candidates can stand. Inside that rank the incumbent holds, unless it is behind (or its freshness is unknown) while another candidate is current: then the current one that claimed first stands. With no incumbent of the best rank, the candidate that is freshest, then oldest, stands.
    fn pick(&self, turns: u32, ident: &DescIdent, now: u64) -> Option<usize> {
        // A candidate's rank and freshness class: 0 current, 1 behind, 2 unknown.
        let candidate = |c: &DescClaim| -> Option<(Rank, u8)> {
            let rank = c.rank(ident);
            (rank.stands() && c.live(now)).then(|| {
                let class = match c.behind(rank, turns) {
                    Some(0) => 0,
                    Some(_) => 1,
                    None => 2,
                };
                (rank, class)
            })
        };
        let best = self
            .claims
            .iter()
            .filter_map(candidate)
            .map(|(rank, _)| rank)
            .min()?;
        let of_best = || {
            self.claims
                .iter()
                .enumerate()
                .filter_map(|(i, c)| match candidate(c) {
                    Some((rank, class)) if rank == best => Some((i, class, c.seq)),
                    _ => None,
                })
        };
        if let Some(held) = self.standing
            && let Some((_, class, _)) = of_best().find(|(i, ..)| *i == held)
        {
            let current = of_best()
                .filter(|(_, class, _)| *class == 0)
                .min_by_key(|(.., seq)| *seq);
            return match current {
                Some((i, ..)) if class != 0 => Some(i),
                _ => Some(held),
            };
        }
        of_best()
            .min_by_key(|(_, class, seq)| (*class, *seq))
            .map(|(i, ..)| i)
    }

    /// Remove claim `i`, keeping the incumbent's index true.
    fn remove(&mut self, i: usize) {
        self.claims.remove(i);
        self.standing = match self.standing {
            Some(s) if s == i => None,
            Some(s) if s > i => Some(s - 1),
            s => s,
        };
    }
}

/// What claims describe: the session, or one of its agent runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescSubject {
    /// The session ([`Node::SESSION`]) or the agent run.
    pub node: Node,
    /// Turns closed on this subject: every turn for the session, an agent's own for an agent (counted from its first claim, seeded with those the turn ring still held).
    pub closed_turns: u32,
    /// An agent's own last [`DESC_TURN_RING`] turn closes, by seq, oldest first: what a claim on it that names an older `upto=` is charged, whatever turns other agents close meanwhile. Seeded, as the count is, with the closes the store's turn ring still held for the agent at its first claim. Empty for the session, whose claims are charged from the store's ring of every turn.
    pub closes: Vec<u64>,
    /// The slots that hold a claim, in the order each was first claimed.
    pub slots: Vec<DescSlot>,
    /// Claims dropped because their slot would have been a ninth unknown one.
    pub dropped_slots: u32,
}

impl DescSubject {
    const fn new(node: Node, closed_turns: u32) -> DescSubject {
        DescSubject {
            node,
            closed_turns,
            closes: Vec::new(),
            slots: Vec::new(),
            dropped_slots: 0,
        }
    }

    /// The slot `name`, if it holds a claim.
    #[must_use]
    pub fn slot(&self, name: &str) -> Option<&DescSlot> {
        self.slots.iter().find(|s| s.name == name)
    }

    /// Settle every slot at `now`: the claim [`DescSlot::pick`] names becomes the incumbent.
    fn settle(&mut self, ident: &DescIdent, now: u64) {
        let turns = self.closed_turns;
        for slot in &mut self.slots {
            slot.standing = slot.pick(turns, ident, now);
        }
    }

    fn shown_in<'a>(&'a self, slot: &str, ident: &DescIdent, now: u64) -> Option<Shown<'a>> {
        let s = self.slot(slot)?;
        let claim = &s.claims[s.pick(self.closed_turns, ident, now)?];
        let rank = claim.rank(ident);
        Some(Shown {
            scope: self.node,
            claim,
            rank,
            behind: claim.behind(rank, self.closed_turns),
        })
    }
}

/// THE CLAIM THAT STANDS for a slot, as a reader is shown it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown<'a> {
    /// The subject it describes: the session, or an agent run.
    pub scope: Node,
    /// The claim.
    pub claim: &'a DescClaim,
    /// Its author's rank.
    pub rank: Rank,
    /// Turns closed on its subject since the seq it covers; `None` when that is not known.
    pub behind: Option<u32>,
}

/// One claim as [`Described::listing`] lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DescRow<'a> {
    /// The subject it describes.
    pub scope: Node,
    /// Its slot.
    pub slot: &'a str,
    /// The claim.
    pub claim: &'a DescClaim,
    /// Its author's rank.
    pub rank: Rank,
    /// Turns behind; `None` when not known.
    pub behind: Option<u32>,
    /// Whether it stands in its own scope at the listing's time.
    pub standing: bool,
    /// Whether it has not lapsed by time.
    pub live: bool,
    /// The claims its slot dropped for a ninth author.
    pub dropped: u32,
}

/// Every description claim a session holds: the session's own and its agent runs', with who the session is and the turn closes a late claim is charged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Described {
    ident: DescIdent,
    session: DescSubject,
    /// The agent runs that hold a claim, oldest subject first, at most [`DESC_AGENTS`].
    agents: Vec<DescSubject>,
    /// Agent subjects dropped for a fourth agent.
    dropped_agents: u32,
    /// The last [`DESC_TURN_RING`] turn closes, oldest first: the close's seq and the agent run the turn ran under (the session when none). What a claim on the session is charged, and what an agent subject's own ring ([`DescSubject::closes`]) starts with at the agent's first claim.
    turns: Vec<(u64, Node)>,
    /// The most recently opened agent run still open.
    open_agent: Option<Node>,
    /// Claims a carry left out for room, in every update this session crossed.
    uncarried: u32,
}

impl Default for Described {
    fn default() -> Described {
        Described {
            ident: DescIdent::default(),
            session: DescSubject::new(Node::SESSION, 0),
            agents: Vec::new(),
            dropped_agents: 0,
            turns: Vec::new(),
            open_agent: None,
            uncarried: 0,
        }
    }
}

impl Described {
    /// Who the session is ([`crate::log::Log::set_desc_ident`]).
    #[must_use]
    pub const fn ident(&self) -> &DescIdent {
        &self.ident
    }

    /// The subjects: the session first, then the agent runs that hold a claim, oldest first.
    pub fn subjects(&self) -> impl Iterator<Item = &DescSubject> {
        std::iter::once(&self.session).chain(self.agents.iter())
    }

    /// The subject `scope` (the session, or an agent run that holds a claim).
    #[must_use]
    pub fn subject(&self, scope: Node) -> Option<&DescSubject> {
        self.subjects().find(|s| s.node == scope)
    }

    /// The most recently opened agent run still open: the second scope [`super::shown`] reads.
    #[must_use]
    pub const fn open_agent(&self) -> Option<Node> {
        self.open_agent
    }

    /// Agent subjects dropped, with their claims, for a fourth agent's first claim.
    #[must_use]
    pub const fn dropped_agents(&self) -> u32 {
        self.dropped_agents
    }

    /// Claims the seamless updates this session crossed left out of their carries for room.
    #[must_use]
    pub const fn uncarried(&self) -> u32 {
        self.uncarried
    }

    /// The claim `by` at `src` (declaring `as_name`) holds for `slot` of `scope`, by the rule that tells authors apart: the same actor and source, and for a writer aterm ran the same declared name. What a withdrawal by that author would remove, and what its next claim would replace.
    #[must_use]
    pub fn claim_of(
        &self,
        scope: Node,
        slot: &str,
        by: Actor,
        src: Src,
        as_name: Option<&str>,
    ) -> Option<&DescClaim> {
        let slot = self.subject(scope)?.slot(slot)?;
        slot.claims.iter().find(|c| c.is_author(by, src, as_name))
    }

    /// THE CLAIM THAT STANDS in one subject's `slot` at `now_wall`: the session's for [`Node::SESSION`], an agent run's for its node. The same choice the fold settled, judged again at the reader's time so a `ttl` lapse shows without a timer; nothing is changed.
    #[must_use]
    pub fn shown_in(&self, scope: Node, slot: &str, now_wall: u64) -> Option<Shown<'_>> {
        self.subject(scope)?.shown_in(slot, &self.ident, now_wall)
    }

    /// EVERY CLAIM, standing or not, at `now_wall`: the session's subject first, then each agent's; inside a subject the known slots in [`KNOWN_SLOTS`] order, then the others by name; inside a slot the standing claim first, then the rest by rank and, inside a rank, oldest first. A lapsed claim is listed too, flagged ([`DescRow::live`]).
    #[must_use]
    pub fn listing(&self, now_wall: u64) -> Vec<DescRow<'_>> {
        let mut out = Vec::new();
        for sub in self.subjects() {
            let mut slots: Vec<&DescSlot> = sub.slots.iter().collect();
            slots.sort_by_key(|s| (slot_order(&s.name), s.name.as_str()));
            for slot in slots {
                let stands = slot.pick(sub.closed_turns, &self.ident, now_wall);
                let mut rows: Vec<DescRow<'_>> = slot
                    .claims
                    .iter()
                    .enumerate()
                    .map(|(i, claim)| {
                        let rank = claim.rank(&self.ident);
                        DescRow {
                            scope: sub.node,
                            slot: &slot.name,
                            claim,
                            rank,
                            behind: claim.behind(rank, sub.closed_turns),
                            standing: stands == Some(i),
                            live: claim.live(now_wall),
                            dropped: slot.dropped,
                        }
                    })
                    .collect();
                rows.sort_by_key(|r| (!r.standing, r.rank, r.claim.seq));
                out.extend(rows);
            }
        }
        out
    }

    /// The store's heap: the agent subjects with their own rings of turn closes, every slot and claim with the text it owns, and the turn ring (the session's subject is inline in the state). At most [`DESC_HEAP_BYTES`].
    #[must_use]
    pub fn heap(&self) -> usize {
        let slots = |sub: &DescSubject| {
            sub.slots
                .iter()
                .map(|s| {
                    std::mem::size_of::<DescSlot>()
                        + s.name.len()
                        + s.claims
                            .iter()
                            .map(|c| {
                                std::mem::size_of::<DescClaim>()
                                    + c.value.len()
                                    + c.as_name.as_ref().map_or(0, String::len)
                            })
                            .sum::<usize>()
                })
                .sum::<usize>()
        };
        let closes: usize = self.agents.iter().map(|s| s.closes.len()).sum();
        self.subjects().map(slots).sum::<usize>()
            + self.agents.len() * std::mem::size_of::<DescSubject>()
            + closes * std::mem::size_of::<u64>()
            + self.turns.len() * std::mem::size_of::<(u64, Node)>()
    }

    /// The turns that closed on `scope` after seq `upto`, as far back as a ring remembers: an agent subject's own ring, which no other agent's turns push a close out of, and the store's ring for the session and for an agent run that holds no claim yet.
    fn turns_after(&self, scope: Node, upto: u64) -> u32 {
        let n = match self.agents.iter().find(|s| s.node == scope) {
            Some(sub) => sub.closes.iter().filter(|seq| **seq > upto).count(),
            None => self
                .turns
                .iter()
                .filter(|(seq, agent)| *seq > upto && (scope == Node::SESSION || *agent == scope))
                .count(),
        };
        u32::try_from(n).unwrap_or(u32::MAX)
    }

    fn subject_mut(&mut self, scope: Node) -> Option<&mut DescSubject> {
        if scope == Node::SESSION {
            Some(&mut self.session)
        } else {
            self.agents.iter_mut().find(|s| s.node == scope)
        }
    }

    /// The subject `scope`, made if it is an agent's first claim: a fourth agent takes the oldest agent subject's place, and its own ring and its counter start with the turns the store's ring still holds for it, so a first claim that covers an older seq is charged them.
    fn subject_or_new(&mut self, scope: Node) -> &mut DescSubject {
        if scope == Node::SESSION {
            return &mut self.session;
        }
        if let Some(i) = self.agents.iter().position(|s| s.node == scope) {
            return &mut self.agents[i];
        }
        if self.agents.len() >= DESC_AGENTS {
            self.agents.remove(0);
            self.dropped_agents = self.dropped_agents.saturating_add(1);
        }
        let closes: Vec<u64> = self
            .turns
            .iter()
            .filter(|(_, agent)| *agent == scope)
            .map(|(seq, _)| *seq)
            .collect();
        let seen = u32::try_from(closes.len()).unwrap_or(u32::MAX);
        let mut sub = DescSubject::new(scope, seen);
        sub.closes = closes;
        self.agents.push(sub);
        let last = self.agents.len() - 1;
        &mut self.agents[last]
    }

    /// An agent subject that holds no claim is no subject.
    fn drop_empty_agents(&mut self) {
        self.agents.retain(|s| !s.slots.is_empty());
    }

    /// A claim: `r`'s author says `slot` of `scope` is `value`. It replaces that author's earlier claim in place; a ninth author's is inserted and the lowest-ranked claim, the oldest of that rank, is dropped and counted.
    fn claim(&mut self, scope: Node, slot: &str, value: &str, content: bool, r: &Rec) {
        let ident = self.ident;
        let upto = r.f.get_u(Key::Upto).map(|u| u.min(r.seq));
        let charged = upto.map_or(0, |u| self.turns_after(scope, u));
        let sub = self.subject_or_new(scope);
        let at = match sub.slots.iter().position(|s| s.name == slot) {
            Some(i) => i,
            None => {
                let unknown = sub
                    .slots
                    .iter()
                    .filter(|s| !KNOWN_SLOTS.contains(&s.name.as_str()))
                    .count();
                if !KNOWN_SLOTS.contains(&slot) && unknown >= DESC_UNKNOWN_SLOTS {
                    sub.dropped_slots = sub.dropped_slots.saturating_add(1);
                    return;
                }
                sub.slots.push(DescSlot {
                    name: slot.to_string(),
                    claims: Vec::new(),
                    standing: None,
                    dropped: 0,
                });
                sub.slots.len() - 1
            }
        };
        let turns = sub.closed_turns;
        let claim = DescClaim {
            value: value.to_string(),
            by: r.by,
            src: r.src,
            conf: r.conf,
            t: r.t,
            seq: r.seq,
            as_name: r.f.get_str(Key::As).map(str::to_string),
            re: match r.f.get(Key::Re) {
                Some(Val::Id(id)) => Some(*id),
                _ => None,
            },
            until: r
                .f
                .get_u(Key::Ttl)
                .map(|s| r.t.saturating_add(s.saturating_mul(1000))),
            content,
            upto,
            base: turns.saturating_sub(charged),
        };
        let s = &mut sub.slots[at];
        let author = s
            .claims
            .iter()
            .position(|c| c.is_author(claim.by, claim.src, claim.as_name.as_deref()));
        match author {
            Some(i) => s.claims[i] = claim,
            None => {
                s.claims.push(claim);
                if s.claims.len() > DESC_CLAIMS {
                    let lowest = (0..s.claims.len())
                        .max_by_key(|&i| (s.claims[i].rank(&ident), Reverse(s.claims[i].seq)));
                    if let Some(i) = lowest {
                        s.remove(i);
                        s.dropped = s.dropped.saturating_add(1);
                    }
                }
            }
        }
        s.standing = s.pick(turns, &ident, r.t);
    }

    /// A withdrawal: `r`'s author takes back its own claim for `slot` of `scope`, and no other author's.
    fn withdraw(&mut self, scope: Node, slot: &str, r: &Rec) {
        let ident = self.ident;
        let Some(sub) = self.subject_mut(scope) else {
            return;
        };
        let Some(at) = sub.slots.iter().position(|s| s.name == slot) else {
            return;
        };
        let s = &mut sub.slots[at];
        let as_name = r.f.get_str(Key::As);
        let Some(i) = s
            .claims
            .iter()
            .position(|c| c.is_author(r.by, r.src, as_name))
        else {
            return;
        };
        s.remove(i);
        if s.claims.is_empty() {
            sub.slots.remove(at);
        } else {
            s.standing = s.pick(sub.closed_turns, &ident, r.t);
        }
        self.drop_empty_agents();
    }

    /// A turn that ran under `agent` (the session when under none) closed at `seq`: the session and that agent count it, the store's ring and that agent's own remember it, and their slots settle at `now`.
    fn turn_closed(&mut self, seq: u64, agent: Node, now: u64) {
        if self.turns.len() >= DESC_TURN_RING {
            self.turns.remove(0);
        }
        self.turns.push((seq, agent));
        let ident = self.ident;
        let count = |sub: &mut DescSubject| {
            sub.closed_turns = sub.closed_turns.saturating_add(1);
            sub.settle(&ident, now);
        };
        count(&mut self.session);
        if let Some(sub) = self.agents.iter_mut().find(|s| s.node == agent) {
            if sub.closes.len() >= DESC_TURN_RING {
                sub.closes.remove(0);
            }
            sub.closes.push(seq);
            count(sub);
        }
    }

    /// An epoch ended at the reset record `seq`, and its runs are forgotten: a turn the ring remembers stays one of the session's and is no agent's, since a later epoch may number its runs again, and a turn the ring holds at or past `seq` is dropped, since the new epoch's records count on from there and the ring stays in seq order. An agent subject's own ring needs nothing here: the subject goes with its run (`Fold::desc_track`).
    fn forget_runs(&mut self, seq: u64) {
        self.turns.retain(|(closed, _)| *closed < seq);
        for (_, agent) in &mut self.turns {
            *agent = Node::SESSION;
        }
    }

    /// The head line's ` dtr=<seq>:<agent>,…`: the turn ring, when it holds a close.
    pub(super) fn push_turns(&self, head: &mut String) {
        for (i, (seq, agent)) in self.turns.iter().enumerate() {
            let _ = write!(head, "{}{seq}:{agent}", if i == 0 { " dtr=" } else { "," });
        }
    }

    /// The turn ring from the head line's `dtr=` token.
    pub(super) fn read_turns(&mut self, head: &str) -> Result<(), SnapError> {
        let Some(raw) = get(head, 2, "dtr") else {
            return Ok(());
        };
        for pair in raw.split(',') {
            if self.turns.len() >= DESC_TURN_RING {
                return Err(SnapError::Bound);
            }
            let (seq, agent) = pair.split_once(':').ok_or(SnapError::Line)?;
            let seq: u64 = crate::line::parse_u(seq).ok_or(SnapError::Line)?;
            let agent = Node::parse(agent)
                .filter(|n| matches!(n.lvl, Lvl::Session | Lvl::Run))
                .ok_or(SnapError::Line)?;
            if self.turns.last().is_some_and(|(newest, _)| *newest >= seq) {
                return Err(SnapError::Line);
            }
            self.turns.push((seq, agent));
        }
        Ok(())
    }

    /// THE CLAIMS A CARRY CROSSES AN UPDATE WITH, as `fold desc…` lines of at most [`DESC_CARRY_BYTES`] in all:
    ///
    /// - `fold desc-sub <subject> [n=<turns closed>] [tr=<seq>,…] [lost_slots=<n>] [lost_agents=<n>]`, one per agent subject (`tr=` its own ring of turn closes, when it holds one), and one for the session when it has a count to carry (its turn count is the totals', its ring the head line's);
    /// - `fold desc <subject> <slot> by= src= conf= t= seq= base= [upto=] [until=] [as=] [re=] [st=1] [dropped=<n>] value=<text>`, one per claim, `st=1` on the incumbent;
    /// - `fold desc-left n=<claims>` when claims were left out for room, by this carry or an earlier one.
    ///
    /// A claim line is whole or absent, never cut. The longest is under 640 bytes (a 24-byte slot, 40-byte actor, name and `re=`, six 20-digit numbers and a 256-byte value with their keys), inside a carry line's 1024. Room goes to the standing claim of every slot first, then to the others; in each pass the session before the agents, known slots before unknown ones, a better rank before a worse one, older before newer. A claim whose text is content tier is never written.
    pub(super) fn carry_lines(&self, out: &mut Vec<String>) {
        const LEFT_LINE: usize = "fold desc-left n=4294967295\n".len();
        let mut subs: Vec<Option<String>> = Vec::new();
        let mut claims: Vec<CarryClaim<'_>> = Vec::new();
        for (sub_at, sub) in self.subjects().enumerate() {
            let session = sub.node == Node::SESSION;
            let mut line = format!("fold desc-sub {}", sub.node);
            if !session {
                let _ = write!(line, " n={}", sub.closed_turns);
                for (i, seq) in sub.closes.iter().enumerate() {
                    let _ = write!(line, "{}{seq}", if i == 0 { " tr=" } else { "," });
                }
            }
            if sub.dropped_slots > 0 {
                let _ = write!(line, " lost_slots={}", sub.dropped_slots);
            }
            if session && self.dropped_agents > 0 {
                let _ = write!(line, " lost_agents={}", self.dropped_agents);
            }
            let said = !session || sub.dropped_slots > 0 || self.dropped_agents > 0;
            subs.push(said.then_some(line));
            for slot in &sub.slots {
                for (i, c) in slot.claims.iter().enumerate().filter(|(_, c)| !c.content) {
                    let stands = slot.standing == Some(i);
                    claims.push(CarryClaim {
                        sub: sub_at,
                        stands,
                        slot: &slot.name,
                        rank: c.rank(&self.ident),
                        seq: c.seq,
                        line: claim_line(sub.node, slot, c, stands),
                        kept: false,
                    });
                }
            }
        }
        let fixed: usize = subs.iter().flatten().map(|l| l.len() + 1).sum();
        let mut room = DESC_CARRY_BYTES.saturating_sub(fixed + LEFT_LINE);
        let mut left = self.uncarried;
        let mut by_room: Vec<usize> = (0..claims.len()).collect();
        by_room.sort_by_key(|&i| {
            let c = &claims[i];
            (!c.stands, c.sub, slot_order(c.slot), c.slot, c.rank, c.seq)
        });
        for i in by_room {
            let need = claims[i].line.len() + 1;
            if need <= room && need <= crate::log::CARRY_MAX_LINE {
                room -= need;
                claims[i].kept = true;
            } else {
                left = left.saturating_add(1);
            }
        }
        let mut claims = claims.into_iter().filter(|c| c.kept).peekable();
        for (sub_at, sub) in subs.into_iter().enumerate() {
            out.extend(sub);
            while let Some(c) = claims.next_if(|c| c.sub == sub_at) {
                out.push(c.line);
            }
        }
        if left > 0 {
            out.push(format!("fold desc-left n={left}"));
        }
    }

    /// One `fold desc…` line of a snapshot (`tag` its second word), read into the store. `tracked` is the fold's open nodes, which the snapshot's lines above gave: an agent subject must be an agent run they hold open.
    pub(super) fn read_line(
        &mut self,
        tag: &str,
        line: &str,
        tracked: &[OpenNode],
    ) -> Result<(), SnapError> {
        match tag {
            "desc-sub" => {
                let node = node_at(line, 2)?;
                let lost_slots = opt_num(line, 3, "lost_slots")?.unwrap_or(0);
                if node == Node::SESSION {
                    self.session.dropped_slots = lost_slots;
                    self.dropped_agents = opt_num(line, 3, "lost_agents")?.unwrap_or(0);
                    return Ok(());
                }
                if !tracked.iter().any(|o| is_agent_run(o) && o.node == node)
                    || self.agents.iter().any(|s| s.node == node)
                {
                    return Err(SnapError::Line);
                }
                if self.agents.len() >= DESC_AGENTS {
                    return Err(SnapError::Bound);
                }
                let mut sub = DescSubject::new(node, num(line, 3, "n")?);
                sub.closes = read_closes(line)?;
                sub.dropped_slots = lost_slots;
                self.agents.push(sub);
                Ok(())
            }
            "desc" => {
                let node = node_at(line, 2)?;
                let name = tok_at(line, 3)?;
                if BoundedToken::new(Key::Field, name).is_none_or(|t| t.as_str() != name) {
                    return Err(SnapError::Line);
                }
                let claim = read_claim(line)?;
                let stands = match get(line, 4, "st") {
                    None => false,
                    Some("1") => true,
                    Some(_) => return Err(SnapError::Line),
                };
                let dropped: u32 = opt_num(line, 4, "dropped")?.unwrap_or(0);
                let sub = self.subject_mut(node).ok_or(SnapError::Line)?;
                let at = match sub.slots.iter().position(|s| s.name == name) {
                    Some(i) => i,
                    None => {
                        let unknown = sub
                            .slots
                            .iter()
                            .filter(|s| !KNOWN_SLOTS.contains(&s.name.as_str()))
                            .count();
                        if !KNOWN_SLOTS.contains(&name) && unknown >= DESC_UNKNOWN_SLOTS {
                            return Err(SnapError::Bound);
                        }
                        sub.slots.push(DescSlot {
                            name: name.to_string(),
                            claims: Vec::new(),
                            standing: None,
                            dropped,
                        });
                        sub.slots.len() - 1
                    }
                };
                let slot = &mut sub.slots[at];
                if slot.claims.len() >= DESC_CLAIMS {
                    return Err(SnapError::Bound);
                }
                let twice = slot
                    .claims
                    .iter()
                    .any(|c| c.is_author(claim.by, claim.src, claim.as_name.as_deref()));
                if twice || slot.dropped != dropped || (stands && slot.standing.is_some()) {
                    return Err(SnapError::Line);
                }
                if stands {
                    slot.standing = Some(slot.claims.len());
                }
                slot.claims.push(claim);
                Ok(())
            }
            "desc-left" => {
                self.uncarried = num(line, 2, "n")?;
                Ok(())
            }
            _ => Err(SnapError::Line),
        }
    }

    /// After a snapshot's last line: the session has closed `session_turns` turns (the totals'), and an agent subject none of whose claims crossed is no subject.
    pub(super) fn finish_read(&mut self, session_turns: u32) {
        self.session.closed_turns = session_turns;
        self.drop_empty_agents();
    }
}

/// One claim's carry line, and what orders it for the carry's room ([`Described::carry_lines`]).
struct CarryClaim<'a> {
    /// Its subject's place: the session, then the agents.
    sub: usize,
    /// Whether it is its slot's incumbent.
    stands: bool,
    slot: &'a str,
    rank: Rank,
    seq: u64,
    line: String,
    /// Whether it fit.
    kept: bool,
}

/// A slot's place in a listing: a known slot by its place in [`KNOWN_SLOTS`], every other after them.
fn slot_order(name: &str) -> usize {
    KNOWN_SLOTS
        .iter()
        .position(|k| *k == name)
        .unwrap_or(KNOWN_SLOTS.len())
}

fn is_agent_run(o: &OpenNode) -> bool {
    o.node.lvl == Lvl::Run && o.kind == Kind::Agent
}

/// An agent subject's own ring of turn closes, from its `fold desc-sub` line's `tr=` token ([`Described::carry_lines`]), held to what the head line's ring is ([`Described::read_turns`]): at most [`DESC_TURN_RING`] seqs, each past the one before it.
fn read_closes(line: &str) -> Result<Vec<u64>, SnapError> {
    let mut closes: Vec<u64> = Vec::new();
    let Some(raw) = get(line, 3, "tr") else {
        return Ok(closes);
    };
    for seq in raw.split(',') {
        if closes.len() >= DESC_TURN_RING {
            return Err(SnapError::Bound);
        }
        let seq: u64 = crate::line::parse_u(seq).ok_or(SnapError::Line)?;
        if closes.last().is_some_and(|newest| *newest >= seq) {
            return Err(SnapError::Line);
        }
        closes.push(seq);
    }
    Ok(closes)
}

/// One claim's carry line ([`Described::carry_lines`]).
fn claim_line(scope: Node, slot: &DescSlot, c: &DescClaim, stands: bool) -> String {
    let mut s = format!(
        "fold desc {scope} {} by={} src={} conf={} t={} seq={} base={}",
        slot.name,
        c.by.word(),
        c.src.word(),
        c.conf.word(),
        c.t,
        c.seq,
        c.base
    );
    if let Some(upto) = c.upto {
        let _ = write!(s, " upto={upto}");
    }
    if let Some(until) = c.until {
        let _ = write!(s, " until={until}");
    }
    if let Some(name) = &c.as_name {
        s.push_str(" as=");
        pct::encode_into(&mut s, name);
    }
    if let Some(re) = &c.re {
        let _ = write!(s, " re={re}");
    }
    if stands {
        s.push_str(" st=1");
    }
    if slot.dropped > 0 {
        let _ = write!(s, " dropped={}", slot.dropped);
    }
    s.push_str(" value=");
    pct::encode_into(&mut s, &c.value);
    s
}

/// The claim a carry line ([`claim_line`]) holds: its text and name must be what their keys' own bounds keep, as the record that made the claim was held to.
fn read_claim(line: &str) -> Result<DescClaim, SnapError> {
    let text = |key: Key, word: &str| -> Result<Option<String>, SnapError> {
        match get(line, 4, word) {
            None => Ok(None),
            Some(raw) => {
                let raw = pct::decode(raw);
                match BoundedText::new(key, &raw) {
                    Some(t) if t.as_str() == raw => Ok(Some(raw)),
                    _ => Err(SnapError::Line),
                }
            }
        }
    };
    Ok(DescClaim {
        value: text(Key::Value, "value")?.ok_or(SnapError::Line)?,
        by: Actor::from_word(get(line, 4, "by").ok_or(SnapError::Line)?),
        src: Src::from_word(get(line, 4, "src").ok_or(SnapError::Line)?),
        conf: Conf::from_word(get(line, 4, "conf").ok_or(SnapError::Line)?),
        t: num(line, 4, "t")?,
        seq: num(line, 4, "seq")?,
        as_name: text(Key::As, "as")?,
        re: match get(line, 4, "re") {
            None => None,
            Some(raw) => Some(ShortId::new(raw).ok_or(SnapError::Line)?),
        },
        until: opt_num(line, 4, "until")?,
        content: false,
        upto: opt_num(line, 4, "upto")?,
        base: num(line, 4, "base")?,
    })
}

impl Fold {
    /// WHO THE SESSION IS ([`DescIdent`]), set by the host: a session's own claims and its creator's rank by it ([`rank_of`]). Every slot settles again under the new identity, at the newest record's time; a change moves `rev`, since what a reader is shown may have moved.
    pub fn set_desc_ident(&mut self, own: Option<ShortId>, creator: Option<ShortId>) {
        let ident = DescIdent { own, creator };
        if self.st.desc.ident == ident {
            return;
        }
        let now = self.st.last_t;
        let d = &mut self.st.desc;
        d.ident = ident;
        d.session.settle(&ident, now);
        for sub in &mut d.agents {
            sub.settle(&ident, now);
        }
        self.st.rev = self.st.rev.saturating_add(1);
    }

    /// THE SUBJECT A `desc` MARK ON `node` DESCRIBES: the session for the session's node, an agent run for its own node, and for any other node (a turn, a step, a command) the nearest agent run it hangs under, else the session.
    fn desc_scope(&self, node: Node) -> Node {
        let mut at = node;
        for _ in 0..=self.tracked.len() {
            match self.tracked.iter().find(|o| o.node == at) {
                Some(o) if is_agent_run(o) => return at,
                Some(OpenNode { par: Some(p), .. }) => at = *p,
                _ => break,
            }
        }
        Node::SESSION
    }

    /// Fold a `desc` mark: a claim when it carries a text (`value=`, or a program's own `title=`, which stays content tier), else its author's withdrawal. A mark that names no slot says nothing.
    pub(super) fn on_desc(&mut self, r: &Rec) {
        let Some(slot) = r.f.get_str(Key::Field) else {
            return;
        };
        let scope = self.desc_scope(r.node);
        let d = &mut self.st.desc;
        match (r.f.get_str(Key::Value), r.f.get_str(Key::Title)) {
            (Some(value), _) => d.claim(scope, slot, value, false, r),
            (None, Some(title)) => d.claim(scope, slot, title, true, r),
            (None, None) => d.withdraw(scope, slot, r),
        }
    }

    /// A turn closed (record `r`; `par` the node it ran under): the claims of the session and of its agent fall one turn behind.
    pub(super) fn desc_turn_closed(&mut self, r: &Rec, par: Option<Node>) {
        let agent = par.filter(|p| p.lvl == Lvl::Run).unwrap_or(Node::SESSION);
        self.st.desc.turn_closed(r.seq, agent, r.t);
    }

    /// The epoch reset `r` forgot every run ([`Described::forget_runs`]).
    pub(super) fn desc_reset(&mut self, r: &Rec) {
        self.st.desc.forget_runs(r.seq);
    }

    /// Follow the open nodes: an agent's claims lapse when its run is no longer tracked open (it closed, the session did, an epoch reset or the tracked bound forgot it), and the open agent is the newest agent run still open. The session's claims outlive its agents.
    pub(super) fn desc_track(&mut self) {
        let tracked = &self.tracked;
        let d = &mut self.st.desc;
        d.agents
            .retain(|s| tracked.iter().any(|o| is_agent_run(o) && o.node == s.node));
        d.open_agent = tracked
            .iter()
            .rev()
            .find(|o| is_agent_run(o))
            .map(|o| o.node);
    }
}
