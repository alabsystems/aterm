// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The time-signed reports a device keeps to pass on (tracker LT-04C): the newest report of each
//! roster member ([`Reports`], kept in `<commons>/reports`), what it offers a peer and takes
//! from one, and the four flights two peers trade them in.
//!
//! The initiator names what it holds that the peer has not been offered ([`offer_flight`]); the
//! responder answers which of those it wants and names its own ([`answer_flight`]); the
//! initiator sends what was wanted and says which of the responder's it wants
//! ([`give_flight`]); the responder sends those ([`last_flight`]). An entry is named by a
//! 16-byte [`Tag`] and its signed time, 24 bytes, and a whole report crosses only where it was
//! wanted. A device accepts a report only for a member of its own roster, under that member's
//! binding, after the report's signature verified ([`super::relayed`]): the peer that hands one
//! over vouches for nothing.

use super::{REFRESH, RELAY_BATCH, REPORT2_LEN, Relayed, STEP, Version, fields, relayed, timely};
use crate::commons::{
    binding::Binding, bootstrap::Bootstrap, constraints::MAX_BINDINGS, journal::Trust,
};
use crate::{Error, Result, fsx};
use astream_wire::sync::Actor;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// A member's name in an offer: the first 16 bytes of the SHA-256 of [`TAG_DOMAIN`] and its
/// whole 48-byte actor ([`tag`]), so two installations under one writer key have different tags.
pub type Tag = [u8; 16];
const TAG_DOMAIN: &[u8] = b"aterm/commons/relay-tag/1\0";
const FILE_MAGIC: &[u8; 8] = b"ACVRPT1\0";
/// The file's bytes before its reports: the magic, the trust's network, authority and genesis,
/// and the count.
const FILE_HEAD: usize = 8 + 16 + 32 + 32 + 2;
const MAX_FILE: usize = 1 << 20;
// The most reports a store keeps fit the file a store reads back.
const _: () = assert!(FILE_HEAD + MAX_BINDINGS * REPORT2_LEN <= MAX_FILE);

/// One offered entry on the wire: a [`Tag`] and a signed time.
const ENTRY_LEN: usize = 16 + 8;
/// The largest [`offer_flight`]: read no more than this for one.
pub const OFFER_MAX: usize = 4 + 2 + RELAY_BATCH * ENTRY_LEN;
/// The largest [`answer_flight`].
pub const ANSWER_MAX: usize = 4 + RELAY_BATCH.div_ceil(8) + 2 + RELAY_BATCH * ENTRY_LEN;
/// The largest [`give_flight`].
pub const GIVE_MAX: usize = 4 + RELAY_BATCH.div_ceil(8) + 2 + RELAY_BATCH * REPORT2_LEN;
/// The largest [`last_flight`].
pub const LAST_MAX: usize = 4 + 2 + RELAY_BATCH * REPORT2_LEN;

/// Each store this process makes has its own epoch, from 1: a [`Mark`] another store made (one
/// from before a restart of the node service, kept by a cache that outlived it) covers nothing
/// of this one's.
static EPOCH: AtomicU64 = AtomicU64::new(1);

/// `actor`'s name in an offer.
pub fn tag(actor: &Actor) -> Tag {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(TAG_DOMAIN);
    hash.update(actor);
    let mut tag = [0; 16];
    tag.copy_from_slice(&hash.finalize()[..16]);
    tag
}

/// How far one peer has been offered a store's reports: the store's epoch and the stamp of the
/// last report examined for that peer. The default covers nothing of any store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mark {
    epoch: u64,
    stamp: u64,
}

/// What a store offers one peer in one round ([`Reports::offer`]).
#[derive(Clone, Debug)]
pub struct Offer {
    entries: Vec<(Tag, u64)>,
    actors: Vec<Actor>,
    covers: Mark,
}

impl Offer {
    /// An offer of nothing, for a peer whose mark is `mark`: it covers what the mark did and no
    /// more, so answering it moves nothing. A device sends it to learn whether a peer that left
    /// an offer unanswered trades at all, before it names its reports again.
    pub fn none(mark: Mark) -> Self {
        Self {
            entries: Vec::new(),
            actors: Vec::new(),
            covers: mark,
        }
    }
    /// Each offered member's tag and the time its report was signed, in the order offered.
    pub fn entries(&self) -> &[(Tag, u64)] {
        &self.entries
    }
    /// The peer's mark once this offer was answered and what the answer wanted was sent.
    pub fn covers(&self) -> Mark {
        self.covers
    }
}

/// The time-signed reports this device keeps for one trust context: at most one for each member
/// of its current roster, its own among them. Every method takes the caller's clock; none reads
/// one, a file is read only by [`Self::load`], and none is written here ([`Self::file`]).
#[derive(Debug)]
pub struct Reports {
    trust: Trust,
    local: Actor,
    roster: BTreeMap<Actor, Binding>,
    /// Each roster member's tag; `None` where two members share one, which is then neither
    /// offered nor wanted.
    tags: BTreeMap<Tag, Option<Actor>>,
    /// Each kept report and its stamp: the order this store took them in.
    kept: BTreeMap<Actor, (Relayed, u64)>,
    epoch: u64,
    next_stamp: u64,
    /// The local writer's kept report is the first this store had of it.
    early: bool,
    dirty: bool,
}

impl Reports {
    /// An empty store for `trust`, on the device that writes as `local`. It keeps nothing until
    /// it has a roster ([`Self::set_roster`]).
    pub fn new(trust: Trust, local: &Binding) -> Self {
        Self {
            trust,
            local: local.actor(),
            roster: BTreeMap::new(),
            tags: BTreeMap::new(),
            kept: BTreeMap::new(),
            epoch: EPOCH.fetch_add(1, Ordering::Relaxed),
            next_stamp: 1,
            early: false,
            dirty: false,
        }
    }

    /// Take `members` as the current roster: those of this trust's network and authority, at
    /// most [`MAX_BINDINGS`]. A kept report is dropped when its member left, when it was signed
    /// [`super::RECENT`] or longer before `now_ms`, and when it was signed more than
    /// [`super::AHEAD`] past it. Whether the roster's set of actors changed.
    pub fn set_roster<'a>(
        &mut self,
        members: impl IntoIterator<Item = &'a Binding>,
        now_ms: u64,
    ) -> bool {
        let roster: BTreeMap<Actor, Binding> = members
            .into_iter()
            .filter(|member| {
                *member.network() == self.trust.network
                    && *member.authority() == self.trust.authority
            })
            .take(MAX_BINDINGS)
            .map(|member| (member.actor(), member.clone()))
            .collect();
        let changed = !roster.keys().eq(self.roster.keys());
        if changed {
            self.tags.clear();
            for actor in roster.keys() {
                self.tags
                    .entry(tag(actor))
                    .and_modify(|named| *named = None)
                    .or_insert(Some(*actor));
            }
        }
        self.roster = roster;
        let before = self.kept.len();
        self.kept.retain(|actor, (report, _)| {
            self.roster.contains_key(actor) && timely(report.signed_ms, now_ms)
        });
        self.dirty |= self.kept.len() != before;
        changed
    }

    /// Whether this store has a roster: a member of its trust to keep a report of. Without one
    /// it keeps, offers and wants nothing, and an answer it gave a peer's offer would say it
    /// wants none of reports it takes once its roster is set.
    pub fn enrolled(&self) -> bool {
        !self.roster.is_empty()
    }

    /// Read the reports kept beside `bootstrap`'s journal into this store, whose roster is set.
    /// Each is taken only as [`Self::accept`] takes one from a peer, its signature verified
    /// under its member's roster binding; the local writer's own report is taken here and
    /// nowhere else. A missing, unreadable, unsafe, foreign or malformed file supplies none.
    pub fn load(&mut self, bootstrap: &Bootstrap, now_ms: u64) {
        self.load_from(&bootstrap.reports_file(), now_ms);
    }

    pub(super) fn load_from(&mut self, path: &Path, now_ms: u64) {
        let Some(bytes) = fsx::read_private(path, MAX_FILE).ok().flatten() else {
            return;
        };
        let Some(wires) = self.parse(bytes.as_slice()) else {
            return;
        };
        let dirty = self.dirty;
        let mut dropped = false;
        for wire in wires {
            let taken = self
                .screened(wire, now_ms, true)
                .and_then(|member| relayed(wire, &member, now_ms).ok())
                .is_some_and(|report| self.accepted(report, now_ms, true));
            dropped |= !taken;
        }
        self.dirty = dirty || dropped;
    }

    /// The reports of a file written for this store's trust.
    fn parse<'a>(&self, bytes: &'a [u8]) -> Option<std::slice::ChunksExact<'a, u8>> {
        let (head, wires) = bytes.split_at_checked(FILE_HEAD)?;
        let mut pins = FILE_MAGIC.to_vec();
        pins.extend_from_slice(&self.trust.network);
        pins.extend_from_slice(&self.trust.authority);
        pins.extend_from_slice(&self.trust.genesis);
        let count = usize::from(u16::from_le_bytes([
            head[FILE_HEAD - 2],
            head[FILE_HEAD - 1],
        ]));
        (head[..FILE_HEAD - 2] == pins[..] && wires.len() == count * REPORT2_LEN)
            .then(|| wires.chunks_exact(REPORT2_LEN))
    }

    /// The bytes to keep in `<commons>/reports` when the kept reports changed since the last
    /// call, which the caller writes with no lock held; `None` when they did not.
    pub fn file(&mut self) -> Option<Vec<u8>> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        let mut out = Vec::with_capacity(FILE_HEAD + self.kept.len() * REPORT2_LEN);
        out.extend_from_slice(FILE_MAGIC);
        out.extend_from_slice(&self.trust.network);
        out.extend_from_slice(&self.trust.authority);
        out.extend_from_slice(&self.trust.genesis);
        out.extend_from_slice(&count(self.kept.len()));
        for (report, _) in self.kept.values() {
            out.extend_from_slice(&report.wire);
        }
        Some(out)
    }

    /// Whether the local writer, on the roster and running `version`, signs a report at
    /// `now_ms`: it keeps none of its own; its kept one is [`REFRESH`] old; `version` is not the
    /// one it signed and [`STEP`] has passed; or that one was this store's first and [`STEP`] has
    /// passed, which happens once.
    pub fn own_due(&self, version: Version, now_ms: u64) -> bool {
        if !self.roster.contains_key(&self.local) {
            return false;
        }
        let Some((kept, _)) = self.kept.get(&self.local) else {
            return true;
        };
        let past = |gap: u64| now_ms >= kept.signed_ms.saturating_add(gap);
        past(REFRESH) || ((kept.version != version || self.early) && past(STEP))
    }

    /// Sign `version` at `now_ms` as the local writer, whose key `seed` must hold, and keep the
    /// report under a new stamp, where one is due ([`Self::own_due`]). Whether it signed.
    ///
    /// # Errors
    /// `seed` is not the local writer's key.
    pub fn sign_own(&mut self, seed: &[u8; 32], version: Version, now_ms: u64) -> Result<bool> {
        if !self.own_due(version, now_ms) {
            return Ok(false);
        }
        let Some(member) = self.roster.get(&self.local) else {
            return Ok(false);
        };
        let wire = super::report_at(seed, self.trust.network, &self.local, version, now_ms)?;
        let report = relayed(&wire, member, now_ms)?;
        Ok(self.accept_own(report, now_ms))
    }

    /// Keep `report`, the local writer's own, under a new stamp: one a caller signed
    /// ([`super::report_at`]) and verified ([`super::relayed`]) with no lock on this store
    /// held. It is kept where a report of its version is due at `now_ms` ([`Self::own_due`])
    /// and its time is one [`Self::accept`] would take. Whether it was kept.
    pub fn accept_own(&mut self, report: Relayed, now_ms: u64) -> bool {
        if report.actor != self.local
            || report.network != self.trust.network
            || report.authority != self.trust.authority
            || !self.own_due(report.version, now_ms)
            || !self.takes(&report.actor, report.signed_ms, now_ms, true)
        {
            return false;
        }
        self.early = !self.kept.contains_key(&self.local);
        self.keep(report);
        true
    }

    fn keep(&mut self, report: Relayed) {
        let stamp = self.next_stamp;
        self.next_stamp += 1;
        self.kept.insert(report.actor, (report, stamp));
        self.dirty = true;
    }

    /// The kept reports whose stamp is above `above`, in stamp order.
    fn newer(&self, above: u64) -> Vec<(u64, &Relayed)> {
        let mut newer: Vec<_> = self
            .kept
            .values()
            .filter(|(_, stamp)| *stamp > above)
            .map(|(report, stamp)| (*stamp, report))
            .collect();
        newer.sort_unstable_by_key(|(stamp, _)| *stamp);
        newer
    }

    /// The reports kept since `stamp`, the local writer's own among them, in the order they
    /// were taken, and the stamp to pass next time; 0 reads them all.
    pub fn since(&self, stamp: u64) -> (Vec<Relayed>, u64) {
        let newer = self.newer(stamp);
        (
            newer
                .into_iter()
                .map(|(_, report)| report.clone())
                .collect(),
            self.next_stamp - 1,
        )
    }

    /// What to offer `peer`, whose mark is `mark`, at `now_ms`: the kept reports taken since
    /// the mark (every one where another store made the mark), in the order they were taken, at
    /// most [`RELAY_BATCH`]. It leaves out `peer`'s own report, one signed [`super::RECENT`] or
    /// longer ago, one whose tag another roster member shares, and one `theirs`, the peer's own
    /// offer in this round, names at the same or a later time.
    pub fn offer(&self, mark: Mark, peer: &Actor, theirs: &[(Tag, u64)], now_ms: u64) -> Offer {
        let above = if mark.epoch == self.epoch {
            mark.stamp
        } else {
            0
        };
        let mut held: BTreeMap<Tag, u64> = BTreeMap::new();
        for (name, signed_ms) in theirs {
            let at = held.entry(*name).or_insert(*signed_ms);
            *at = (*at).max(*signed_ms);
        }
        let mut offer = Offer {
            entries: Vec::new(),
            actors: Vec::new(),
            covers: Mark {
                epoch: self.epoch,
                stamp: above,
            },
        };
        for (stamp, report) in self.newer(above) {
            if offer.entries.len() == RELAY_BATCH {
                break;
            }
            offer.covers.stamp = stamp;
            let name = tag(&report.actor);
            if report.actor == *peer
                || report.signed_ms.saturating_add(super::RECENT) <= now_ms
                || self.tags.get(&name) != Some(&Some(report.actor))
                || held.get(&name).is_some_and(|at| *at >= report.signed_ms)
            {
                continue;
            }
            offer.entries.push((name, report.signed_ms));
            offer.actors.push(report.actor);
        }
        offer
    }

    /// Which of a peer's `offered` entries this store wants: one whose tag names exactly one
    /// roster member, not the local writer, and whose time [`Self::accept`] would take.
    pub fn want(&self, offered: &[(Tag, u64)], now_ms: u64) -> Vec<bool> {
        offered
            .iter()
            .map(|(name, signed_ms)| {
                self.tags
                    .get(name)
                    .copied()
                    .flatten()
                    .is_some_and(|actor| self.takes(&actor, *signed_ms, now_ms, false))
            })
            .collect()
    }

    /// The reports to send for `offer`, which the peer answered with `want`: the one now kept
    /// for each wanted entry, in the order offered. An answer of another length is given none.
    pub fn give(&self, offer: &Offer, want: &[bool]) -> Vec<[u8; REPORT2_LEN]> {
        if want.len() != offer.actors.len() {
            return Vec::new();
        }
        offer
            .actors
            .iter()
            .zip(want)
            .filter(|(_, want)| **want)
            .filter_map(|(actor, _)| self.kept.get(actor).map(|(report, _)| report.wire))
            .collect()
    }

    /// Whether this store takes a report `actor` signed at `signed_ms`: `actor` is on the roster
    /// and, unless `own`, is not the local writer; the time counts at `now_ms`; and nothing is
    /// kept for it, or what is kept was signed [`STEP`] or longer before.
    fn takes(&self, actor: &Actor, signed_ms: u64, now_ms: u64, own: bool) -> bool {
        self.roster.contains_key(actor)
            && (own || *actor != self.local)
            && timely(signed_ms, now_ms)
            && self
                .kept
                .get(actor)
                .is_none_or(|(kept, _)| signed_ms >= kept.signed_ms.saturating_add(STEP))
    }

    /// The roster binding to verify `wire` under ([`super::relayed`]), where [`Self::accept`]
    /// could still take the report it names; no signature is checked here, so a caller checks
    /// one with no lock on this store held.
    pub fn screen(&self, wire: &[u8], now_ms: u64) -> Option<Binding> {
        self.screened(wire, now_ms, false)
    }

    fn screened(&self, wire: &[u8], now_ms: u64, own: bool) -> Option<Binding> {
        let (actor, _, signed_ms) = fields(wire)?;
        self.takes(&actor, signed_ms, now_ms, own)
            .then(|| self.roster.get(&actor).cloned())
            .flatten()
    }

    /// Keep a verified `report` a peer handed over, under a new stamp: one of this trust's
    /// network and authority, whose member is on the roster and is not the local writer, whose
    /// time counts at `now_ms`, and which was signed [`STEP`] or longer after the one kept for
    /// that member, if any. Whether it was kept.
    pub fn accept(&mut self, report: Relayed, now_ms: u64) -> bool {
        self.accepted(report, now_ms, false)
    }

    fn accepted(&mut self, report: Relayed, now_ms: u64, own: bool) -> bool {
        if report.network != self.trust.network
            || report.authority != self.trust.authority
            || !self.takes(&report.actor, report.signed_ms, now_ms, own)
        {
            return false;
        }
        self.keep(report);
        true
    }
}

/// The reports a side takes of those it was `given`, after it answered `offered` with `want`:
/// one for each tag it wanted, in the order given. A report it did not want, and a second one
/// for the same tag, are left out, so it takes at most as many as it wanted.
pub fn taken<'a>(
    offered: &[(Tag, u64)],
    want: &[bool],
    given: &'a [[u8; REPORT2_LEN]],
) -> Vec<&'a [u8; REPORT2_LEN]> {
    let mut wanted: BTreeSet<Tag> = offered
        .iter()
        .zip(want)
        .filter(|(_, want)| **want)
        .map(|((name, _), _)| *name)
        .collect();
    given
        .iter()
        .filter(|wire| fields(&wire[..]).is_some_and(|(actor, _, _)| wanted.remove(&tag(&actor))))
        .collect()
}

/// A count on the wire or in the file; no caller passes more than [`MAX_BINDINGS`].
fn count(len: usize) -> [u8; 2] {
    u16::try_from(len).unwrap_or(u16::MAX).to_le_bytes()
}

fn invalid() -> Error {
    Error::new("invalid Commons relay flight")
}

/// A flight's body from `magic` on.
fn flight(magic: &[u8; 4], capacity: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(capacity);
    out.extend_from_slice(magic);
    out
}

fn put_entries(out: &mut Vec<u8>, entries: &[(Tag, u64)]) {
    let entries = &entries[..entries.len().min(RELAY_BATCH)];
    out.extend_from_slice(&count(entries.len()));
    for (name, signed_ms) in entries {
        out.extend_from_slice(name);
        out.extend_from_slice(&signed_ms.to_le_bytes());
    }
}

fn put_want(out: &mut Vec<u8>, want: &[bool]) {
    let mut bits = vec![0u8; want.len().div_ceil(8)];
    for (at, _) in want.iter().enumerate().filter(|(_, want)| **want) {
        bits[at / 8] |= 1 << (at % 8);
    }
    out.extend_from_slice(&bits);
}

fn put_wires(out: &mut Vec<u8>, wires: &[[u8; REPORT2_LEN]]) {
    let wires = &wires[..wires.len().min(RELAY_BATCH)];
    out.extend_from_slice(&count(wires.len()));
    for wire in wires {
        out.extend_from_slice(wire);
    }
}

/// What is left of a flight's body as it is read.
struct Rest<'a>(&'a [u8]);

impl<'a> Rest<'a> {
    fn start(body: &'a [u8], magic: &[u8; 4]) -> Result<Self> {
        let mut rest = Self(body);
        (rest.take(4)? == magic).then_some(rest).ok_or_else(invalid)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(len).ok_or_else(invalid)?;
        self.0 = rest;
        Ok(head)
    }

    /// A count of at most `most`.
    fn count(&mut self, most: usize) -> Result<usize> {
        let bytes = self.take(2)?;
        let count = usize::from(u16::from_le_bytes([bytes[0], bytes[1]]));
        (count <= most).then_some(count).ok_or_else(invalid)
    }

    fn entries(&mut self) -> Result<Vec<(Tag, u64)>> {
        let count = self.count(RELAY_BATCH)?;
        Ok(self
            .take(count * ENTRY_LEN)?
            .chunks_exact(ENTRY_LEN)
            .map(|entry| {
                (
                    entry[..16].try_into().unwrap(),
                    u64::from_le_bytes(entry[16..].try_into().unwrap()),
                )
            })
            .collect())
    }

    /// The want bits for `offered` entries; a bit past the last entry is refused.
    fn want(&mut self, offered: usize) -> Result<Vec<bool>> {
        let bits = self.take(offered.div_ceil(8))?;
        let bit = |at: usize| bits[at / 8] & (1 << (at % 8)) != 0;
        if (offered..bits.len() * 8).any(bit) {
            return Err(invalid());
        }
        Ok((0..offered).map(bit).collect())
    }

    /// At most `wanted` reports.
    fn wires(&mut self, wanted: usize) -> Result<Vec<[u8; REPORT2_LEN]>> {
        let count = self.count(wanted.min(RELAY_BATCH))?;
        Ok(self
            .take(count * REPORT2_LEN)?
            .chunks_exact(REPORT2_LEN)
            .map(|wire| wire.try_into().unwrap())
            .collect())
    }

    fn end<T>(self, read: T) -> Result<T> {
        self.0.is_empty().then_some(read).ok_or_else(invalid)
    }
}

/// The first flight, initiator to responder: `"ACG1"`, a count, and that many entries of a tag
/// and a signed time ([`Offer::entries`]).
pub fn offer_flight(entries: &[(Tag, u64)]) -> Vec<u8> {
    let mut out = flight(b"ACG1", OFFER_MAX);
    put_entries(&mut out, entries);
    out
}

/// The entries of an [`offer_flight`].
///
/// # Errors
/// Another magic, more than [`RELAY_BATCH`] entries, or a length that is not the count's.
pub fn read_offer(body: &[u8]) -> Result<Vec<(Tag, u64)>> {
    let mut rest = Rest::start(body, b"ACG1")?;
    let entries = rest.entries()?;
    rest.end(entries)
}

/// The second flight, responder to initiator: `"ACG2"`, one want bit for each entry of the
/// initiator's offer (the first entry is the lowest bit of the first byte), then the
/// responder's own offer as in [`offer_flight`].
pub fn answer_flight(want: &[bool], entries: &[(Tag, u64)]) -> Vec<u8> {
    let mut out = flight(b"ACG2", ANSWER_MAX);
    put_want(&mut out, want);
    put_entries(&mut out, entries);
    out
}

/// The want bits and the entries of an [`answer_flight`] to an offer of `offered` entries.
///
/// # Errors
/// Another magic, want bits that are not `offered` long, more than [`RELAY_BATCH`] entries, or
/// a length that is not the counts'.
pub fn read_answer(body: &[u8], offered: usize) -> Result<(Vec<bool>, Vec<(Tag, u64)>)> {
    let mut rest = Rest::start(body, b"ACG2")?;
    let read = (rest.want(offered)?, rest.entries()?);
    rest.end(read)
}

/// The third flight, initiator to responder: `"ACG3"`, one want bit for each entry of the
/// responder's offer, a count, and that many reports the responder wanted.
pub fn give_flight(want: &[bool], wires: &[[u8; REPORT2_LEN]]) -> Vec<u8> {
    let mut out = flight(b"ACG3", GIVE_MAX);
    put_want(&mut out, want);
    put_wires(&mut out, wires);
    out
}

/// The want bits and the reports of a [`give_flight`], read by the side that offered `offered`
/// entries and wanted `wanted` reports.
///
/// # Errors
/// Another magic, want bits that are not `offered` long, more than `wanted` reports, or a
/// length that is not the counts'.
pub fn read_give(
    body: &[u8],
    offered: usize,
    wanted: usize,
) -> Result<(Vec<bool>, Vec<[u8; REPORT2_LEN]>)> {
    let mut rest = Rest::start(body, b"ACG3")?;
    let read = (rest.want(offered)?, rest.wires(wanted)?);
    rest.end(read)
}

/// The fourth flight, responder to initiator: `"ACG4"`, a count, and that many reports the
/// initiator wanted.
pub fn last_flight(wires: &[[u8; REPORT2_LEN]]) -> Vec<u8> {
    let mut out = flight(b"ACG4", LAST_MAX);
    put_wires(&mut out, wires);
    out
}

/// The reports of a [`last_flight`], read by the side that wanted `wanted` reports.
///
/// # Errors
/// Another magic, more than `wanted` reports, or a length that is not the count's.
pub fn read_last(body: &[u8], wanted: usize) -> Result<Vec<[u8; REPORT2_LEN]>> {
    let mut rest = Rest::start(body, b"ACG4")?;
    let wires = rest.wires(wanted)?;
    rest.end(wires)
}
