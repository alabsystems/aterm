// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptions as claims (streams design §6): what a session, or one of its agent runs, is said to
//! be about. A `desc` mark fills one slot (the six of the card, [`KNOWN_SLOTS`], or any other word)
//! of one subject, or appends one line to its history list ([`HISTORY`]); the fold keeps each
//! author's claim apart, ranks an author by who it is, and says which claim stands. A claim is
//! never an observation and never an instruction: every one is listed with its author, and the
//! lower-ranked ones are kept, not overwritten.
//!
//! What a claim may say is one pure rule ([`desc_text`] for a writer's raw text, [`desc_admits`]
//! for a stored one), applied where the record is written ([`crate::log::Log::mark`]) and again in
//! the fold, so a record the journal or a carry holds from an earlier build is judged as a new one
//! is.
//!
//! No clock is read here: a claim's `ttl` lapse is judged against the `now_wall` a reader passes, and the fold settles a slot at the time of the record that moved it.

use std::cmp::Reverse;
use std::fmt::Write as _;

use aterm_codec::pct;

use super::snap::{get, node_at, num, opt_num, tok_at};
use super::{Fold, OpenNode, SnapError};
use crate::field::{BoundedText, BoundedToken, Fields, Key, Val};
use crate::rec::Rec;
use crate::vocab::{Actor, Conf, Kind, Lvl, Node, ShortId, Src};

/// The slots this build knows, in the order the card shows them and `described` lists them: the
/// topic, why it matters, what it is doing, how it is going, a declared done, and the tracker
/// packages it works. Any other slot is stored and listed the same way, within
/// [`DESC_UNKNOWN_SLOTS`].
pub const KNOWN_SLOTS: &[&str] = &[
    "title",
    "matters",
    "description",
    "track",
    "done",
    "package",
];
/// The `field=` of a history line: not a slot. A slot holds one claim an author, which its next
/// claim replaces; the history list is appended to ([`HistLine`]).
pub const HISTORY: &str = "history";
/// The event words a history line of aterm's own writer names in its `as=`: the line reports one
/// record (`re=`). Read from no other author's line.
pub const HISTORY_EVENTS: &[&str] = &[
    "commit", "landed", "failed", "question", "blocked", "chapter",
];
/// The slots a turn makes stale: a claim from anyone but a person or aterm's own writer more than
/// [`DESC_STALE_TURNS`] turns behind yields to a fresher one.
pub const PER_TURN_SLOTS: &[&str] = &["description", "track"];
/// The slots a session states for itself: a claim of rank 1 to 3 on the session stands over any
/// claim on its open agent ([`super::shown`]).
pub const STATED_SLOTS: &[&str] = &["title", "matters", "done"];
/// The most history lines one subject keeps, newest last; an older one is counted
/// ([`DescSubject::lines_older`]).
pub const DESC_HISTORY: usize = 16;
/// How many turns behind a per-turn claim may be and still stand over a fresher one
/// ([`PER_TURN_SLOTS`]).
pub const DESC_STALE_TURNS: u32 = 3;
/// A `package` claim's text at most: four ids of 25 characters and the three commas between them.
pub const PACKAGE_BYTES: usize = 103;
/// The most tracker ids one `package` claim names.
pub const PACKAGE_IDS: usize = 4;
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

/// The most slots one subject holds: every known slot and [`DESC_UNKNOWN_SLOTS`] others (6 + 8 =
/// 14).
pub const DESC_SLOTS: usize = KNOWN_SLOTS.len() + DESC_UNKNOWN_SLOTS;
/// The most claims the store holds: the session and [`DESC_AGENTS`] agents, [`DESC_SLOTS`] slots
/// each, [`DESC_CLAIMS`] claims a slot (4 × 14 × 8 = 448).
pub const DESC_CLAIMS_MAX: usize = (1 + DESC_AGENTS) * DESC_SLOTS * DESC_CLAIMS;

/// A claim's text at most: `value=` is capped at 256 percent-encoded bytes (an in-band `title=` at 240), and encoding never shortens.
const VALUE_BYTES: usize = 256;
/// A claim's `as=` name at most (40 encoded bytes).
const AS_BYTES: usize = 40;
/// A slot's name at most (`field=`, 24 bytes).
const SLOT_BYTES: usize = 24;

/// The most text the store owns. Per subject: five known slots of 256-byte values, each its name
/// and eight claims of a value and a name (5 × (24 + 8 × (256 + 40)) = 11,960 B); `package`, whose
/// values are at most [`PACKAGE_BYTES`] (24 + 8 × (103 + 40) = 1,168 B); eight unknown slots (8 ×
/// 2,392 = 19,136 B); and sixteen history lines of a value and a name (16 × (256 + 40) = 4,736 B):
/// 37,000 B. Four subjects: 148,000 B.
pub const DESC_TEXT_BYTES: usize = (1 + DESC_AGENTS)
    * ((KNOWN_SLOTS.len() - 1) * (SLOT_BYTES + DESC_CLAIMS * (VALUE_BYTES + AS_BYTES))
        + (SLOT_BYTES + DESC_CLAIMS * (PACKAGE_BYTES + AS_BYTES))
        + DESC_UNKNOWN_SLOTS * (SLOT_BYTES + DESC_CLAIMS * (VALUE_BYTES + AS_BYTES))
        + DESC_HISTORY * (VALUE_BYTES + AS_BYTES));
/// The most heap [`Described::heap`] reports: [`DESC_TEXT_BYTES`], the 448 claims' and 56 slots'
/// own structs, the 64 history lines' own structs, the three agent subjects (the session's is
/// inline in the state), the turn ring, and each agent subject's own ring of eight seqs (3 × 8 × 8
/// = 192 B).
pub const DESC_HEAP_BYTES: usize = DESC_TEXT_BYTES
    + DESC_CLAIMS_MAX * std::mem::size_of::<DescClaim>()
    + (1 + DESC_AGENTS) * DESC_SLOTS * std::mem::size_of::<DescSlot>()
    + (1 + DESC_AGENTS) * DESC_HISTORY * std::mem::size_of::<HistLine>()
    + DESC_AGENTS * std::mem::size_of::<DescSubject>()
    + DESC_TURN_RING * std::mem::size_of::<(u64, Node)>()
    + DESC_AGENTS * DESC_TURN_RING * std::mem::size_of::<u64>();

/// WHY A DESCRIPTION RECORD IS NOT ADMITTED ([`desc_text`], [`desc_admits`]): the writer is told,
/// and nothing is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimRefused {
    /// The slot is not a canonical [`Key::Field`] token.
    Slot,
    /// The text is empty once trimmed (a withdrawal is the absence of a text).
    Empty,
    /// The text holds a character no surface prints ([`aterm_messages::text::is_hidden_char`]): a
    /// line break, a tab, another control, a bidi control, a zero-width or tag character. Refused,
    /// never dropped: what is stored is what was written.
    Formatting,
    /// The text is over its slot's bound ([`slot_bound`]) percent-encoded: refused, never cut.
    TooLong {
        /// The slot's bound.
        max: usize,
    },
    /// A `package` that is not 1 to [`PACKAGE_IDS`] distinct tracker ids joined by commas
    /// ([`desc_text`]).
    Package,
    /// In-band bytes (a title a program printed, [`Rank::Inband`]) claim a slot other than `title`,
    /// or write a history line.
    InBand,
    /// `done` from aterm (its own writer, or a writer it ran): aterm's judgement is the track word,
    /// and a writer's guess that the work is finished is no declaration.
    DoneAuthor,
    /// The harness's turn-end readers read a head in the text
    /// ([`crate::protocol::protocol_line`]) and its author is not a person.
    Protocol,
    /// A history line with no text: a line is never withdrawn.
    NoLine,
}

/// The bound of `slot`'s text in percent-encoded bytes: [`PACKAGE_BYTES`] for `package`, else
/// [`Key::Value`]'s 256.
#[must_use]
pub fn slot_bound(slot: &str) -> usize {
    if slot == "package" {
        PACKAGE_BYTES
    } else {
        VALUE_BYTES
    }
}

/// WHETHER `text` IS A WELL-FORMED `package`: 1 to [`PACKAGE_IDS`] ids joined by `,` with nothing
/// else (no space, no empty element, no trailing comma), none repeated; each id an upper-case ASCII
/// prefix of 1 to 8 letters, a `-`, and 1 to 16 of `[A-Za-z0-9._-]` (split at the first `-`). The
/// bound is [`desc_text`]'s, apart.
fn package_ok(text: &str) -> bool {
    let ids: Vec<&str> = text.split(',').collect();
    let id_ok = |id: &str| {
        id.split_once('-').is_some_and(|(prefix, rest)| {
            (1..=8).contains(&prefix.len())
                && prefix.bytes().all(|b| b.is_ascii_uppercase())
                && (1..=16).contains(&rest.len())
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
    };
    (1..=PACKAGE_IDS).contains(&ids.len())
        && ids.iter().all(|id| id_ok(id))
        && ids.iter().enumerate().all(|(i, id)| !ids[..i].contains(id))
}

/// A WRITER'S RAW TEXT FOR `slot`, judged before anything else touches it: the slot must be a
/// canonical [`Key::Field`] token ([`ClaimRefused::Slot`]); the text is trimmed (a shell's trailing
/// newline is trimmed, not refused), then refused, in this order, when it is empty, holds a hidden
/// character, is over [`slot_bound`] encoded, or is a malformed `package`. Returns the trimmed
/// text, never a cut one. Takes no author: it binds a person too. A [`Fields`] value has already
/// dropped hidden characters ([`BoundedText`]), so this is the one place a line break is seen.
/// # Errors
/// The first [`ClaimRefused`] of those, in that order.
pub fn desc_text(slot: &str, raw: &str) -> Result<String, ClaimRefused> {
    if BoundedToken::new(Key::Field, slot).is_none_or(|t| t.as_str() != slot) {
        return Err(ClaimRefused::Slot);
    }
    let text = raw.trim();
    if text.is_empty() {
        return Err(ClaimRefused::Empty);
    }
    if text.chars().any(aterm_messages::text::is_hidden_char) {
        return Err(ClaimRefused::Formatting);
    }
    let max = slot_bound(slot);
    if crate::field::encoded_len(text) > max {
        return Err(ClaimRefused::TooLong { max });
    }
    if slot == "package" && !package_ok(text) {
        return Err(ClaimRefused::Package);
    }
    Ok(text.to_string())
}

/// WHETHER A STORED TEXT MAY STAND IN `slot` (or be a history line) by `by` at `src`: refused, in
/// this order, from in-band bytes for any slot but `title` ([`ClaimRefused::InBand`]), as a `done`
/// from aterm ([`ClaimRefused::DoneAuthor`]), as a malformed `package`, over the slot's bound, and,
/// from any author but a person, where the harness's turn-end readers read a head in it
/// ([`ClaimRefused::Protocol`]). An in-band title is held to the protocol rule too: its writer
/// withdraws its claim instead.
/// # Errors
/// The first [`ClaimRefused`] of those, in that order.
pub fn desc_admits(slot: &str, text: &str, by: Actor, src: Src) -> Result<(), ClaimRefused> {
    author_admits(slot, by, src)?;
    if slot == "package" && !package_ok(text) {
        return Err(ClaimRefused::Package);
    }
    let max = slot_bound(slot);
    if crate::field::encoded_len(text) > max {
        return Err(ClaimRefused::TooLong { max });
    }
    if by != Actor::Human && crate::protocol::protocol_line(text) {
        return Err(ClaimRefused::Protocol);
    }
    Ok(())
}

/// Whether `by` at `src` may write `slot` at all, whatever the text: in-band bytes state a title
/// only, and aterm never declares `done`.
fn author_admits(slot: &str, by: Actor, src: Src) -> Result<(), ClaimRefused> {
    if slot != "title" && rank_of(by, src, &DescIdent::default()) == Rank::Inband {
        return Err(ClaimRefused::InBand);
    }
    if slot == "done" && by == Actor::Aterm {
        return Err(ClaimRefused::DoneAuthor);
    }
    Ok(())
}

/// WHETHER A `desc` RECORD'S FIELDS BY `by` AT `src` ARE ADMITTED: one naming no slot says nothing
/// and is; the author's own refusals hold whatever the text, so a withdrawal of a slot the author
/// may not hold is refused too; a history line is its `value=` (host tier: a line is never content
/// tier), and one without is [`ClaimRefused::NoLine`]; any other text, `value=` or else `title=`,
/// is [`desc_admits`]'s to judge.
/// # Errors
/// The [`ClaimRefused`] that refuses the record.
pub(crate) fn admits_record(f: &Fields, by: Actor, src: Src) -> Result<(), ClaimRefused> {
    let Some(slot) = f.get_str(Key::Field) else {
        return Ok(());
    };
    author_admits(slot, by, src)?;
    let text = match f.get_str(Key::Value) {
        Some(value) => Some(value),
        None if slot == HISTORY => return Err(ClaimRefused::NoLine),
        None => f.get_str(Key::Title),
    };
    text.map_or(Ok(()), |text| desc_admits(slot, text, by, src))
}

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

    /// The rank [`Rank::word`] names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Rank> {
        [
            Rank::Person,
            Rank::Program,
            Rank::Creator,
            Rank::Writer,
            Rank::Ctl,
            Rank::Inband,
            Rank::Derived,
            Rank::Listed,
        ]
        .into_iter()
        .find(|r| r.word() == word)
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

/// WHAT A WITHDRAWAL REMOVES: whether one by `by` at `src` (declaring `as_name`) takes `claim`
/// away, the session being `ident`. A person's takes every claim that was put on the slot: the
/// person's own, and each made over the control socket or by another session (the ranks
/// program, creator, writer and ctl, and the listed ones). It leaves what aterm observes of the
/// program (an in-band title) and aterm's own derived value: those are not anyone's statement to
/// take back, and their writers state them again. Anyone else's takes that author's own claim
/// ([`DescClaim::is_author`]) and nobody else's, so a person's claim is cleared only by a person.
fn withdrawn_by(
    claim: &DescClaim,
    by: Actor,
    src: Src,
    as_name: Option<&str>,
    ident: &DescIdent,
) -> bool {
    if rank_of(by, src, ident) == Rank::Person {
        !matches!(claim.rank(ident), Rank::Inband | Rank::Derived)
    } else {
        claim.is_author(by, src, as_name)
    }
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
    /// The seq of the record from which this author's claim has held its present text and `as=`
    /// name: a claim that repeats the author's own (the same text, name and tier) keeps it, any
    /// other takes its own record's seq. What tells the card's revision a repeat from a change
    /// ([`Described::card_rev`]).
    pub since: u64,
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

    /// TURNS BEHIND: how many turns closed on the claim's subject (`turns` closed in all) after the
    /// seq it covers. Known when the record named an `upto=`, or when its author is of a rank whose
    /// claim covers the log as it was written (`Program`, `Writer`: behind 0 at the write);
    /// otherwise `None`: nobody knows how current the claim was. aterm's own derived claim is
    /// always 0: its writer derives the line again at every trigger and writes it only when it
    /// changed, so the line it holds is the current one.
    #[must_use]
    pub fn behind(&self, rank: Rank, turns: u32) -> Option<u32> {
        if rank == Rank::Derived {
            return Some(0);
        }
        (self.upto.is_some() || matches!(rank, Rank::Program | Rank::Writer))
            .then(|| turns.saturating_sub(self.base))
    }

    /// WHETHER THE CLAIM IS STALE in a per-turn slot ([`PER_TURN_SLOTS`]): more than
    /// [`DESC_STALE_TURNS`] of its subject's `turns` closed after its base, and its author is one
    /// whose line a turn ages (the session's program, its creator, a writer, a control client,
    /// in-band bytes). `turns − base` is its turns behind where `upto=` is known, and a lower bound
    /// on them where it is not: a claim naming no `upto` covers at most its own record. A person's
    /// claim and aterm's own derived one are never stale.
    fn stale(&self, rank: Rank, turns: u32) -> bool {
        matches!(
            rank,
            Rank::Program | Rank::Creator | Rank::Writer | Rank::Ctl | Rank::Inband
        ) && turns.saturating_sub(self.base) > DESC_STALE_TURNS
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
    /// WHICH CLAIM STANDS at `now` (`turns` closed on the subject, the session being `ident`),
    /// given the incumbent. Only a live claim whose rank stands is a candidate. In a per-turn slot
    /// ([`PER_TURN_SLOTS`]) a stale claim ([`DescClaim::stale`]) is no candidate while one that is
    /// not stale is; where every one is stale they all stay, since a stale line is better than
    /// none. Only the best rank among the candidates can stand. Inside that rank the incumbent
    /// holds, unless it is behind (or its freshness is unknown) while another candidate is current:
    /// then the current one that claimed first stands. With no incumbent of the best rank, the
    /// candidate that is freshest, then oldest, stands.
    fn pick(&self, turns: u32, ident: &DescIdent, now: u64) -> Option<usize> {
        let ages = PER_TURN_SLOTS.contains(&self.name.as_str());
        let live = |c: &DescClaim| {
            let rank = c.rank(ident);
            (rank.stands() && c.live(now)).then_some(rank)
        };
        let fresh = ages
            && self
                .claims
                .iter()
                .any(|c| live(c).is_some_and(|rank| !c.stale(rank, turns)));
        // A candidate's rank and freshness class: 0 current, 1 behind, 2 unknown.
        let candidate = |c: &DescClaim| -> Option<(Rank, u8)> {
            let rank = live(c).filter(|rank| !(fresh && c.stale(*rank, turns)))?;
            let class = match c.behind(rank, turns) {
                Some(0) => 0,
                Some(_) => 1,
                None => 2,
            };
            Some((rank, class))
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
    /// Its history list, oldest first, at most [`DESC_HISTORY`] lines: one line a turn and one for
    /// each record aterm's own writer reports ([`Described::line`]).
    pub history: Vec<HistLine>,
    /// History lines the newest [`DESC_HISTORY`] pushed out.
    pub lines_older: u32,
}

impl DescSubject {
    const fn new(node: Node, closed_turns: u32) -> DescSubject {
        DescSubject {
            node,
            closed_turns,
            closes: Vec::new(),
            slots: Vec::new(),
            dropped_slots: 0,
            history: Vec::new(),
            lines_older: 0,
        }
    }

    /// The claim that stood in `slot` when the slot last settled, as a reader is shown it: what the
    /// fold holds, never picked again at a reader's time.
    fn settled<'a>(&'a self, slot: &str, ident: &DescIdent) -> Option<Shown<'a>> {
        let s = self.slot(slot)?;
        let claim = s.claims.get(s.standing?)?;
        let rank = claim.rank(ident);
        Some(Shown {
            scope: self.node,
            claim,
            rank,
            behind: claim.behind(rank, self.closed_turns),
        })
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

/// ONE HISTORY LINE: what happened, in one line, with its author and time. A line is an event in
/// time: it is never stale, never withdrawn, and its rank is the one its author had when it was
/// admitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistLine {
    /// The line's text (host tier).
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
    /// The name its author declared (`as=`): for aterm's own writer, the event it reports.
    pub as_name: Option<String>,
    /// The record it reports (`re=`).
    pub re: Option<ShortId>,
    /// The turn it is its subject's line of: the turns closed on the subject at its record, and one
    /// more where a turn was open there, so a program's line during a turn and aterm's right after
    /// its close are the same turn's.
    pub turn: u32,
    /// Its author's rank when it was admitted, under who the session was then: never judged again,
    /// since that would rewrite history.
    pub rank: Rank,
}

impl HistLine {
    /// THE EVENT WORD, read only from a line aterm's own writer wrote ([`Rank::Derived`]: `by=aterm
    /// src=aterm`) whose `as=` is one of [`HISTORY_EVENTS`]; `None` for every other line, whatever
    /// its `as=` says.
    #[must_use]
    pub fn event(&self) -> Option<&'static str> {
        if self.by != Actor::Aterm || self.src != Src::Aterm {
            return None;
        }
        let name = self.as_name.as_deref()?;
        HISTORY_EVENTS.iter().copied().find(|w| *w == name)
    }

    /// Whether `by` at `src` (declaring `as_name`) wrote this line, by the rule that tells claims'
    /// authors apart ([`DescClaim::is_author`]).
    fn is_author(&self, by: Actor, src: Src, as_name: Option<&str>) -> bool {
        self.by == by
            && self.src == src
            && (!is_writer(by, src) || self.as_name.as_deref() == as_name)
    }
}

/// One history line as [`super::shown_history`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistRow<'a> {
    /// The subject it is a line of.
    pub scope: Node,
    /// The line.
    pub line: &'a HistLine,
    /// Its author's rank when it was admitted.
    pub rank: Rank,
    /// The event it reports ([`HistLine::event`]).
    pub event: Option<&'static str>,
}

/// WHAT THE CARD SHOWS, as its revision compares it ([`Described::card_rev`]): for each known slot,
/// the claim that stands by the two scopes' rule ([`two_scopes`]) over each subject's settled
/// incumbent, as its scope, its `since` and its rank, so a new text, a new author or a new rank
/// changes it and a repeat does not; and the newest history line of the session and the open agent,
/// as its scope and seq.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CardKey {
    slots: [Option<(Node, u64, Rank)>; KNOWN_SLOTS.len()],
    line: Option<(Node, u64)>,
}

/// THE TWO SCOPES' RULE (R1): which of the claim standing for the session and the one standing for
/// its open agent a reader is shown for `slot`. A `title`, `matters` or `done` claim of rank 1 to 3
/// on the session is the session's stated task and stands over any claim on its open agent, a
/// person's there included. Otherwise, and always for every other slot, the better rank stands and
/// a tie goes to the session. One claim stands, never a union of the two.
pub(super) fn two_scopes<'a>(
    slot: &str,
    session: Option<Shown<'a>>,
    agent: Option<Shown<'a>>,
) -> Option<Shown<'a>> {
    match (session, agent) {
        (Some(s), _) if STATED_SLOTS.contains(&slot) && s.rank <= Rank::Creator => Some(s),
        (Some(s), Some(a)) if a.rank < s.rank => Some(a),
        (None, a) => a,
        (s, _) => s,
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
    /// History lines a carry left out for room, in every update this session crossed.
    uncarried_lines: u32,
    /// Records the fold did not admit ([`admits_record`]): ones an earlier build journaled or
    /// carried, or applied to a fold directly, since [`crate::log::Log::mark`] appends none.
    refused: u32,
    /// History lines no list keeps: outranked in their turn, or by an author who never stands.
    lines_dropped: u32,
    /// What the card showed when its revision last moved.
    card: CardKey,
    /// THE CARD'S REVISION: moves when a standing value or its author changes (a standing claim's
    /// `ttl=` running out included, seen at the next record), or the card's newest history line
    /// does, and with nothing else; never below the seq of the newest record that changed the card.
    card_rev: u64,
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
            uncarried_lines: 0,
            refused: 0,
            lines_dropped: 0,
            card: CardKey::default(),
            card_rev: 0,
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

    /// History lines the seamless updates this session crossed left out of their carries for room.
    #[must_use]
    pub const fn uncarried_lines(&self) -> u32 {
        self.uncarried_lines
    }

    /// Records the fold did not admit and never stored ([`admits_record`]): an in-band claim of
    /// any slot but `title` among them (any file a program prints could carry the sequence), and
    /// any record an earlier build journaled or carried that this build's rule refuses.
    #[must_use]
    pub const fn refused(&self) -> u32 {
        self.refused
    }

    /// History lines no list keeps: outranked in their turn ([`Described::line`]), or written by
    /// an author whose claims never stand.
    #[must_use]
    pub const fn lines_dropped(&self) -> u32 {
        self.lines_dropped
    }

    /// THE CARD'S REVISION ([`CardKey`]): what a reader caches the card by. It moves with a new
    /// standing value or author of a known slot and with a new newest history line of the
    /// session or its open agent, and with nothing else; it is never below the seq of the newest
    /// record that changed the card, so a replay that holds that record gives it again.
    #[must_use]
    pub const fn card_rev(&self) -> u64 {
        self.card_rev
    }

    /// The history list of `scope`, oldest first: empty where it holds none.
    #[must_use]
    pub fn history(&self, scope: Node) -> &[HistLine] {
        self.subject(scope).map_or(&[], |s| s.history.as_slice())
    }

    /// The claim `by` at `src` (declaring `as_name`) holds for `slot` of `scope`, by the rule that
    /// tells authors apart: the same actor and source, and for a writer aterm ran the same declared
    /// name. What its next claim would replace; what its withdrawal would remove is
    /// [`Self::withdraws`]'s to say, since a person's removes more than the person's own.
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

    /// The store's heap: the agent subjects with their own rings of turn closes, every slot and
    /// claim and every history line with the text it owns, and the turn ring (the session's subject
    /// is inline in the state). At most [`DESC_HEAP_BYTES`].
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
                + sub
                    .history
                    .iter()
                    .map(|l| {
                        std::mem::size_of::<HistLine>()
                            + l.value.len()
                            + l.as_name.as_ref().map_or(0, String::len)
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

    /// An agent subject that holds no claim and no history line is no subject.
    fn drop_empty_agents(&mut self) {
        self.agents
            .retain(|s| !s.slots.is_empty() || !s.history.is_empty());
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
            since: r.seq,
        };
        let s = &mut sub.slots[at];
        let author = s
            .claims
            .iter()
            .position(|c| c.is_author(claim.by, claim.src, claim.as_name.as_deref()));
        match author {
            Some(i) => {
                let held = &s.claims[i];
                let repeats = held.value == claim.value
                    && held.as_name == claim.as_name
                    && held.content == claim.content;
                let since = if repeats { held.since } else { claim.since };
                s.claims[i] = DescClaim { since, ..claim };
            }
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

    /// A HISTORY LINE: `r`'s author says `text` happened on `scope`, while a turn was `open` there
    /// or not. Its rank is judged now and kept ([`HistLine::rank`]); an author whose claims never
    /// stand writes no line (it could never be shown, and would only push shown ones out). An
    /// event line of aterm's own writer ([`HistLine::event`]) takes no turn's place: it replaces
    /// the line reporting the same event of the same record, else it is appended. Every other
    /// line is its turn's ([`HistLine::turn`]), and the list keeps one a turn: its author's next
    /// line in the turn replaces it, a better rank's replaces it, and an equal rank of another
    /// author or a worse one is dropped (within a rank the incumbent holds). A line that replaces
    /// one or is appended goes newest last; past [`DESC_HISTORY`] the oldest goes and is counted.
    fn line(&mut self, scope: Node, text: &str, r: &Rec, open: bool) {
        let rank = rank_of(r.by, r.src, &self.ident);
        if !rank.stands() {
            self.lines_dropped = self.lines_dropped.saturating_add(1);
            return;
        }
        let sub = self.subject_or_new(scope);
        let line = HistLine {
            value: text.to_string(),
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
            turn: sub.closed_turns.saturating_add(u32::from(open)),
            rank,
        };
        let held = match line.event() {
            Some(event) => line.re.and_then(|re| {
                sub.history
                    .iter()
                    .position(|l| l.event() == Some(event) && l.re == Some(re))
            }),
            None => sub
                .history
                .iter()
                .position(|l| l.event().is_none() && l.turn == line.turn),
        };
        // A turn line against the one its turn holds: its own author's replaces it, and of two
        // authors the better rank's stands and the other is dropped (the newcomer on a tie).
        let outranked = held.is_some_and(|i| {
            let incumbent = &sub.history[i];
            line.event().is_none()
                && !incumbent.is_author(line.by, line.src, line.as_name.as_deref())
        });
        if outranked && held.is_some_and(|i| line.rank >= sub.history[i].rank) {
            self.lines_dropped = self.lines_dropped.saturating_add(1);
            return;
        }
        if let Some(i) = held {
            sub.history.remove(i);
        }
        sub.history.push(line);
        if sub.history.len() > DESC_HISTORY {
            sub.history.remove(0);
            sub.lines_older = sub.lines_older.saturating_add(1);
        }
        if outranked {
            self.lines_dropped = self.lines_dropped.saturating_add(1);
        }
    }

    /// Whether a withdrawal by `by` at `src` (declaring `as_name`) for `slot` of `scope` would
    /// remove a claim ([`withdrawn_by`]): what tells a writer that its withdrawal states
    /// something from one that repeats what the store already holds.
    #[must_use]
    pub fn withdraws(
        &self,
        scope: Node,
        slot: &str,
        by: Actor,
        src: Src,
        as_name: Option<&str>,
    ) -> bool {
        self.subject(scope)
            .and_then(|sub| sub.slot(slot))
            .is_some_and(|s| {
                s.claims
                    .iter()
                    .any(|c| withdrawn_by(c, by, src, as_name, &self.ident))
            })
    }

    /// A withdrawal: `r`'s author takes back its claim for `slot` of `scope`. Anyone but a
    /// person takes back its own claim and no other author's. A person's withdrawal clears the
    /// slot of every claim somebody put there ([`withdrawn_by`]): a person is the one author
    /// above every other, and a row's name is what a person steers by, so a person who empties
    /// it must be able to remove what a program or another session wrote on it.
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
        let before = s.claims.len();
        for i in (0..before).rev() {
            if withdrawn_by(&s.claims[i], r.by, r.src, as_name, &ident) {
                s.remove(i);
            }
        }
        if s.claims.len() == before {
            return;
        }
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

    /// THE CARD AS THE STORE HOLDS IT NOW ([`CardKey`]): each known slot's settled claim by the two
    /// scopes' rule, and the newest history line of the session and of the open agent.
    fn card_key(&self) -> CardKey {
        let agent = self.open_agent.and_then(|a| self.subject(a));
        let mut slots = [None; KNOWN_SLOTS.len()];
        for (at, slot) in KNOWN_SLOTS.iter().enumerate() {
            let session = self.session.settled(slot, &self.ident);
            let agent = agent.and_then(|sub| sub.settled(slot, &self.ident));
            slots[at] = two_scopes(slot, session, agent).map(|s| (s.scope, s.claim.since, s.rank));
        }
        let line = std::iter::once(&self.session)
            .chain(agent)
            .filter_map(|sub| sub.history.last().map(|l| (sub.node, l.seq)))
            .max_by_key(|(_, seq)| *seq);
        CardKey { slots, line }
    }

    /// PICK AGAIN, AT `now`, EACH SLOT WHOSE INCUMBENT HAS LAPSED by its `ttl=`: the fold reads no
    /// clock, so a lapse is seen at the next record. Only the incumbent's lapse can change a pick:
    /// every other input (a claim, a withdrawal, a turn close, the identity) settles the slot when
    /// it changes, and a claim that lapses without standing displaced nothing.
    fn settle_lapsed(&mut self, now: u64) {
        let ident = self.ident;
        for sub in std::iter::once(&mut self.session).chain(self.agents.iter_mut()) {
            let turns = sub.closed_turns;
            for slot in &mut sub.slots {
                if slot.standing.is_some_and(|i| !slot.claims[i].live(now)) {
                    slot.standing = slot.pick(turns, &ident, now);
                }
            }
        }
    }

    /// The card after a record (`seq`, or none for a change no record made): where it differs from
    /// what the revision last saw, the revision moves, to at least `seq`.
    fn card_moved(&mut self, seq: Option<u64>) {
        let card = self.card_key();
        if card != self.card {
            self.card = card;
            self.card_rev = self.card_rev.saturating_add(1).max(seq.unwrap_or(0));
        }
    }

    /// THE CLAIMS AND LINES A CARRY CROSSES AN UPDATE WITH, as `fold desc…` lines of at most
    /// [`DESC_CARRY_BYTES`] in all:
    ///
    /// - `fold desc-sub - [card_rev=<n>] [refused=<n>] [lines_dropped=<n>] [lines_older=<n>]
    ///   [lost_slots=<n>] [lost_agents=<n>]` for the session, when it has a count to carry (its
    ///   turn count is the totals', its ring the head line's), and `fold desc-sub r<n> n=<turns
    ///   closed> [tr=<seq>,…] [lines_older=<n>] [lost_slots=<n>]` for each agent subject (`tr=` its
    ///   own ring of turn closes, when it holds one);
    /// - `fold desc <subject> <slot> by= src= conf= t= seq= base= [since=] [upto=] [until=] [as=]
    ///   [re=] [st=1] [dropped=<n>] value=<text>`, one per claim, `st=1` on the incumbent, `since=`
    ///   where it is not the claim's own seq;
    /// - `fold desc-line <subject> turn=<k> rank=<rank> by= src= conf= t= seq= [as=] [re=]
    ///   value=<text>`, one per history line, oldest first, after its subject's claims;
    /// - `fold desc-left n=<claims>[ lines=<lines>]` when claims or lines were left out for room,
    ///   by this carry or an earlier one.
    ///
    /// A line is whole or absent, never cut. The longest claim line is under 640 bytes (a 24-byte
    /// slot, 40-byte actor, name and `re=`, seven 20-digit numbers and a 256-byte value with their
    /// keys), inside a carry line's 1024, and a history line is shorter. Room goes first to the
    /// standing claim of every slot (the session before the agents, known slots in order, then the
    /// others by name), then to the history lines (the session's newest first, then each agent's),
    /// then to every other claim (in the same order as the standing ones, a better rank before a
    /// worse one, older before newer): what the card shows first, and its recent history next. A
    /// claim whose text is content tier is never written.
    pub(super) fn carry_lines(&self, out: &mut Vec<String>) {
        const LEFT_LINE: usize = "fold desc-left n=4294967295 lines=4294967295\n".len();
        let mut subs: Vec<Option<String>> = Vec::new();
        let mut items: Vec<CarryItem<'_>> = Vec::new();
        for (sub_at, sub) in self.subjects().enumerate() {
            let session = sub.node == Node::SESSION;
            let head = format!("fold desc-sub {}", sub.node);
            let mut line = head.clone();
            if session {
                for (key, n) in [
                    ("card_rev", self.card_rev),
                    ("refused", u64::from(self.refused)),
                    ("lines_dropped", u64::from(self.lines_dropped)),
                ] {
                    if n > 0 {
                        let _ = write!(line, " {key}={n}");
                    }
                }
            } else {
                let _ = write!(line, " n={}", sub.closed_turns);
                for (i, seq) in sub.closes.iter().enumerate() {
                    let _ = write!(line, "{}{seq}", if i == 0 { " tr=" } else { "," });
                }
            }
            if sub.lines_older > 0 {
                let _ = write!(line, " lines_older={}", sub.lines_older);
            }
            if sub.dropped_slots > 0 {
                let _ = write!(line, " lost_slots={}", sub.dropped_slots);
            }
            if session && self.dropped_agents > 0 {
                let _ = write!(line, " lost_agents={}", self.dropped_agents);
            }
            subs.push((line != head).then_some(line));
            for slot in &sub.slots {
                for (i, c) in slot.claims.iter().enumerate().filter(|(_, c)| !c.content) {
                    let stands = slot.standing == Some(i);
                    items.push(CarryItem {
                        sub: sub_at,
                        rank: (
                            if stands { 0 } else { 2 },
                            sub_at,
                            slot_order(&slot.name),
                            slot.name.as_str(),
                            c.rank(&self.ident),
                            c.seq,
                        ),
                        line: claim_line(sub.node, slot, c, stands),
                        is_line: false,
                        kept: false,
                    });
                }
            }
            for l in &sub.history {
                items.push(CarryItem {
                    sub: sub_at,
                    // A line's place is its subject's and then its recency alone, newest first,
                    // whoever wrote it: the card shows the newest lines whatever their ranks.
                    rank: (1, sub_at, 0, "", Rank::Person, u64::MAX - l.seq),
                    line: hist_line(sub.node, l),
                    is_line: true,
                    kept: false,
                });
            }
        }
        let fixed: usize = subs.iter().flatten().map(|l| l.len() + 1).sum();
        let mut room = DESC_CARRY_BYTES.saturating_sub(fixed + LEFT_LINE);
        let (mut left, mut left_lines) = (self.uncarried, self.uncarried_lines);
        let mut by_room: Vec<usize> = (0..items.len()).collect();
        by_room.sort_by_key(|&i| items[i].rank);
        for i in by_room {
            let need = items[i].line.len() + 1;
            if need <= room && need <= crate::log::CARRY_MAX_LINE {
                room -= need;
                items[i].kept = true;
            } else if items[i].is_line {
                left_lines = left_lines.saturating_add(1);
            } else {
                left = left.saturating_add(1);
            }
        }
        let mut items = items.into_iter().filter(|c| c.kept).peekable();
        for (sub_at, sub) in subs.into_iter().enumerate() {
            out.extend(sub);
            while let Some(c) = items.next_if(|c| c.sub == sub_at) {
                out.push(c.line);
            }
        }
        if left > 0 || left_lines > 0 {
            let mut line = format!("fold desc-left n={left}");
            if left_lines > 0 {
                let _ = write!(line, " lines={left_lines}");
            }
            out.push(line);
        }
    }

    /// One `fold desc…` line of a snapshot (`tag` its second word), read into the store. `tracked`
    /// is the fold's open nodes, which the snapshot's lines above gave: an agent subject must be an
    /// agent run they hold open. A claim or a history line that reads but that this build's rule
    /// refuses ([`desc_admits`], one an earlier build carried) is left out and counted in
    /// [`Described::refused`], as the fold refuses a replayed record; one this build cannot read
    /// refuses the whole snapshot.
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
                let lines_older = opt_num(line, 3, "lines_older")?.unwrap_or(0);
                if node == Node::SESSION {
                    self.session.dropped_slots = lost_slots;
                    self.session.lines_older = lines_older;
                    self.dropped_agents = opt_num(line, 3, "lost_agents")?.unwrap_or(0);
                    self.card_rev = opt_num(line, 3, "card_rev")?.unwrap_or(0);
                    self.refused = opt_num(line, 3, "refused")?.unwrap_or(0);
                    self.lines_dropped = opt_num(line, 3, "lines_dropped")?.unwrap_or(0);
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
                sub.lines_older = lines_older;
                self.agents.push(sub);
                Ok(())
            }
            "desc" => {
                let node = node_at(line, 2)?;
                let name = tok_at(line, 3)?;
                if name == HISTORY
                    || BoundedToken::new(Key::Field, name).is_none_or(|t| t.as_str() != name)
                {
                    return Err(SnapError::Line);
                }
                let claim = read_claim(line)?;
                let stands = match get(line, 4, "st") {
                    None => false,
                    Some("1") => true,
                    Some(_) => return Err(SnapError::Line),
                };
                let dropped: u32 = opt_num(line, 4, "dropped")?.unwrap_or(0);
                if self.subject(node).is_none() {
                    return Err(SnapError::Line);
                }
                if desc_admits(name, &claim.value, claim.by, claim.src).is_err() {
                    self.refused = self.refused.saturating_add(1);
                    return Ok(());
                }
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
            "desc-line" => {
                let node = node_at(line, 2)?;
                let read = read_hist(line)?;
                let sub = self.subject(node).ok_or(SnapError::Line)?;
                if sub.history.len() >= DESC_HISTORY {
                    return Err(SnapError::Bound);
                }
                let twice = read.event().is_none()
                    && sub
                        .history
                        .iter()
                        .any(|l| l.event().is_none() && l.turn == read.turn);
                if twice || sub.history.last().is_some_and(|l| l.seq >= read.seq) {
                    return Err(SnapError::Line);
                }
                if desc_admits(HISTORY, &read.value, read.by, read.src).is_err() {
                    self.refused = self.refused.saturating_add(1);
                    return Ok(());
                }
                self.subject_mut(node)
                    .ok_or(SnapError::Line)?
                    .history
                    .push(read);
                Ok(())
            }
            "desc-left" => {
                self.uncarried = num(line, 2, "n")?;
                self.uncarried_lines = opt_num(line, 2, "lines")?.unwrap_or(0);
                Ok(())
            }
            _ => Err(SnapError::Line),
        }
    }

    /// After a snapshot's last line: the session has closed `session_turns` turns (the totals'), a
    /// slot whose incumbent did not cross (this build refused it) settles again at `now`, and an
    /// agent subject none of whose claims or lines crossed is no subject.
    pub(super) fn finish_read(&mut self, session_turns: u32, now: u64) {
        self.session.closed_turns = session_turns;
        let ident = self.ident;
        for sub in std::iter::once(&mut self.session).chain(self.agents.iter_mut()) {
            let turns = sub.closed_turns;
            for slot in sub.slots.iter_mut().filter(|s| s.standing.is_none()) {
                slot.standing = slot.pick(turns, &ident, now);
            }
        }
        self.drop_empty_agents();
    }
}

/// One claim's or history line's carry line, and what orders it for the carry's room
/// ([`Described::carry_lines`]).
struct CarryItem<'a> {
    /// Its subject's place: the session, then the agents.
    sub: usize,
    /// Its place in the queue for room: 0 a standing claim, 1 a history line, 2 any other claim;
    /// then its subject's place; for a claim its slot's place, its name, its rank (better first)
    /// and its seq (older first); for a line its seq alone, newest first.
    rank: (u8, usize, usize, &'a str, Rank, u64),
    line: String,
    /// Whether it is a history line, counted apart when it does not fit.
    is_line: bool,
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

/// The ` as=… re=…` a claim's or a line's carry line holds where it names them.
fn push_name(s: &mut String, as_name: Option<&String>, re: Option<&ShortId>) {
    if let Some(name) = as_name {
        s.push_str(" as=");
        pct::encode_into(s, name);
    }
    if let Some(re) = re {
        let _ = write!(s, " re={re}");
    }
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
    if c.since != c.seq {
        let _ = write!(s, " since={}", c.since);
    }
    if let Some(upto) = c.upto {
        let _ = write!(s, " upto={upto}");
    }
    if let Some(until) = c.until {
        let _ = write!(s, " until={until}");
    }
    push_name(&mut s, c.as_name.as_ref(), c.re.as_ref());
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

/// One history line's carry line ([`Described::carry_lines`]).
fn hist_line(scope: Node, l: &HistLine) -> String {
    let mut s = format!(
        "fold desc-line {scope} turn={} rank={} by={} src={} conf={} t={} seq={}",
        l.turn,
        l.rank.word(),
        l.by.word(),
        l.src.word(),
        l.conf.word(),
        l.t,
        l.seq
    );
    push_name(&mut s, l.as_name.as_ref(), l.re.as_ref());
    s.push_str(" value=");
    pct::encode_into(&mut s, &l.value);
    s
}

/// The text a carry line holds under `word` (its first `skip` words skipped): it must be what
/// `key`'s own bound keeps, as the record that made it was held to.
fn fixed_text(line: &str, skip: usize, key: Key, word: &str) -> Result<Option<String>, SnapError> {
    match get(line, skip, word) {
        None => Ok(None),
        Some(raw) => {
            let raw = pct::decode(raw);
            match BoundedText::new(key, &raw) {
                Some(t) if t.as_str() == raw => Ok(Some(raw)),
                _ => Err(SnapError::Line),
            }
        }
    }
}

/// The `re=` a carry line holds, with its first `skip` words skipped.
fn read_re(line: &str, skip: usize) -> Result<Option<ShortId>, SnapError> {
    match get(line, skip, "re") {
        None => Ok(None),
        Some(raw) => Ok(Some(ShortId::new(raw).ok_or(SnapError::Line)?)),
    }
}

/// The claim a carry line ([`claim_line`]) holds: its text and name must be what their keys' own
/// bounds keep, as the record that made the claim was held to.
fn read_claim(line: &str) -> Result<DescClaim, SnapError> {
    let seq = num(line, 4, "seq")?;
    Ok(DescClaim {
        value: fixed_text(line, 4, Key::Value, "value")?.ok_or(SnapError::Line)?,
        by: Actor::from_word(get(line, 4, "by").ok_or(SnapError::Line)?),
        src: Src::from_word(get(line, 4, "src").ok_or(SnapError::Line)?),
        conf: Conf::from_word(get(line, 4, "conf").ok_or(SnapError::Line)?),
        t: num(line, 4, "t")?,
        seq,
        as_name: fixed_text(line, 4, Key::As, "as")?,
        re: read_re(line, 4)?,
        until: opt_num(line, 4, "until")?,
        content: false,
        upto: opt_num(line, 4, "upto")?,
        base: num(line, 4, "base")?,
        since: opt_num(line, 4, "since")?.unwrap_or(seq),
    })
}

/// The history line a carry line ([`hist_line`]) holds: its text and name are held as a claim's
/// are, and its rank must be one its author can have (a session's sid ranks as the session's own
/// program or its creator; every other author has one rank).
fn read_hist(line: &str) -> Result<HistLine, SnapError> {
    let by = Actor::from_word(get(line, 3, "by").ok_or(SnapError::Line)?);
    let src = Src::from_word(get(line, 3, "src").ok_or(SnapError::Line)?);
    let rank = Rank::from_word(get(line, 3, "rank").ok_or(SnapError::Line)?)
        .filter(|rank| match by {
            Actor::Sid(_) => matches!(rank, Rank::Program | Rank::Creator),
            _ => *rank == rank_of(by, src, &DescIdent::default()),
        })
        .filter(|rank| rank.stands())
        .ok_or(SnapError::Line)?;
    Ok(HistLine {
        value: fixed_text(line, 3, Key::Value, "value")?.ok_or(SnapError::Line)?,
        by,
        src,
        conf: Conf::from_word(get(line, 3, "conf").ok_or(SnapError::Line)?),
        t: num(line, 3, "t")?,
        seq: num(line, 3, "seq")?,
        as_name: fixed_text(line, 3, Key::As, "as")?,
        re: read_re(line, 3)?,
        turn: num(line, 3, "turn")?,
        rank,
    })
}

impl Fold {
    /// WHO THE SESSION IS ([`DescIdent`]), set by the host: a session's own claims and its
    /// creator's rank by it ([`rank_of`]). Every slot settles again under the new identity, at the
    /// newest record's time; a change moves `rev` where the store holds a claim, since what a
    /// reader is shown may have moved, and moves the card's revision by one where what the card
    /// shows did ([`Described::card_rev`]). With no claim held nothing shown can move and `rev`
    /// stays: the host says who the session is as it registers it, before anyone has described
    /// it, and to a carry or a journal replay as it is read ([`Fold::from_snapshot_as`],
    /// [`crate::log::Log::from_journal`]), so a tell that repeats it after the read returns at
    /// once. A history line keeps the rank it was admitted at ([`HistLine::rank`]).
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
        d.card_moved(None);
        if d.subjects().any(|sub| !sub.slots.is_empty()) {
            self.st.rev = self.st.rev.saturating_add(1);
        }
    }

    /// THE CARD AFTER RECORD `r` ([`Described::card_rev`]): a standing claim whose `ttl=` ran out
    /// by the newest record's time yields first ([`Described::settle_lapsed`]); then, where what
    /// the card shows moved, its revision moves, to at least `r`'s seq. Run at the end of every
    /// applied record, after its arm and the open path's projection. The lapse is judged at
    /// `last_t`, never below an earlier record's time, so a record stamped out of order cannot
    /// bring a lapsed claim back.
    pub(super) fn desc_card(&mut self, r: &Rec) {
        let now = self.st.last_t;
        self.st.desc.settle_lapsed(now);
        self.st.desc.card_moved(Some(r.seq));
    }

    /// The card a snapshot's store shows, kept as what the card's revision last saw without
    /// moving the revision: the carry restates the card, it does not change it.
    pub(super) fn desc_card_read(&mut self) {
        let d = &mut self.st.desc;
        d.card = d.card_key();
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

    /// Fold a `desc` mark. The record is judged first by the rule the log writes by
    /// ([`admits_record`]): one it refuses (an earlier build journaled or carried it, or it was
    /// applied to a fold directly) is not stored and is counted ([`Described::refused`]). A
    /// history line ([`HISTORY`]) is appended to its subject's list ([`Described::line`]), keyed
    /// by the turn open on that subject at the record, if any. Any other mark is a claim when it
    /// carries a text (`value=`, or a program's own `title=`, which stays content tier), else
    /// its author's withdrawal. A mark that names no slot says nothing.
    ///
    /// An in-band claim's text is content tier, which the journal does not hold, so the claim
    /// read back from the journal names no text and folds as the in-band author's withdrawal.
    /// That takes nothing away: the in-band claims before it were read back the same way, so
    /// the author holds none, and an in-band withdrawal never removes another author's claim.
    pub(super) fn on_desc(&mut self, r: &Rec) {
        let Some(slot) = r.f.get_str(Key::Field) else {
            return;
        };
        if admits_record(&r.f, r.by, r.src).is_err() {
            let d = &mut self.st.desc;
            d.refused = d.refused.saturating_add(1);
            return;
        }
        let scope = self.desc_scope(r.node);
        if slot == HISTORY {
            let open = self.tracked.iter().any(|o| {
                o.node.lvl == Lvl::Turn && (scope == Node::SESSION || o.par == Some(scope))
            });
            if let Some(text) = r.f.get_str(Key::Value) {
                self.st.desc.line(scope, text, r, open);
            }
            return;
        }
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

    /// Follow the open nodes: an agent's claims and history lines lapse when its run is no longer
    /// tracked open (it closed, the session did, an epoch reset or the tracked bound forgot it),
    /// and the open agent is the newest agent run still open. The session's claims and lines
    /// outlive its agents.
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
