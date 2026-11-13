// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Grow-only membership-record wire streams and stores (`docs/DESIGN-fluid-swarm-2026-10-28.md`
//! §4.1 F13).
//!
//! Each [`Record`] is self-delimiting on the wire (`ALM1`), so a stream or store file is the
//! concatenation of signed record bytes: one record's [`Record::bytes`] is a one-record stream.

use super::{
    Body, Kind, Log, MAX_HELD_RECORDS, MAX_RECORD, MAX_WAITING, Record, RecordBudgets, RecordId,
    genesis,
};
use crate::{Error, Result};
use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet};

#[cfg(unix)]
pub(crate) mod framed;

#[cfg(test)]
#[path = "store/writer_tests.rs"]
mod writer_tests;

#[cfg(test)]
#[path = "store/classified_tests.rs"]
pub(crate) mod classified_tests;

/// Largest concatenated record stream [`decode_records`] reads (`64 MiB`).
pub const MAX_RECORDS_STREAM: usize = 64 * 1024 * 1024;

/// The most ids [`Store::have`] names: more than a pull's URL has room for (about 120 of 8 KiB,
/// `transport::records_have_url`), which keeps as many as fit, in order.
pub const HAVE_IDS: usize = 256;

/// Concatenate the signed wire bytes of `records` into one self-delimiting stream.
#[must_use]
pub fn encode_records<'a>(records: impl IntoIterator<Item = &'a Record>) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        out.extend_from_slice(record.bytes());
    }
    out
}

/// Decode and verify every [`Record`] in `bytes`, refusing the whole stream if any record is
/// truncated, malformed, oversized or fails signature verification.
///
/// # Errors
/// `bytes` exceeds [`MAX_RECORDS_STREAM`], holds more than [`MAX_HELD_RECORDS`] records, or
/// contains a malformed or invalidly signed record.
pub fn decode_records(bytes: &[u8]) -> Result<Vec<Record>> {
    decode_records_bounded(bytes, MAX_HELD_RECORDS)
}

/// [`decode_records`] of a stream that holds at most `record_limit` records, refused whole past
/// them before the record past them is verified: a records file under the held budget of the
/// store that reads it ([`Store::recovering`]), a member's join bundle within what both sides
/// hold (`client::admit`), or a witness only a build before the fence could have sent, which
/// holds at most [`super::LEGACY_RECORDS`] whatever this build holds.
///
/// # Errors
/// `bytes` exceeds [`MAX_RECORDS_STREAM`], holds more than `record_limit` records, or contains a
/// malformed or invalidly signed record.
pub fn decode_records_bounded(bytes: &[u8], record_limit: usize) -> Result<Vec<Record>> {
    if bytes.len() > MAX_RECORDS_STREAM {
        return Err(Error::new("membership record stream is too large"));
    }
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        if out.len() >= record_limit {
            return Err(Error::new("membership record stream exceeds record bound"));
        }
        let len = Record::wire_len_prefix(&bytes[at..])?;
        let end = at
            .checked_add(len)
            .filter(|end| *end <= bytes.len() && len <= MAX_RECORD)
            .ok_or_else(|| Error::new("membership record is truncated"))?;
        out.push(Record::decode(&bytes[at..end])?);
        at = end;
    }
    Ok(out)
}

/// [`decode_records`] of a member's own records file, whose writer puts whole records on its end
/// in place (`client::persist_records`): a last record cut short ([`Record::is_cut_frame`]) is
/// what a writer that died mid-append leaves, or what a read that lands inside one finds, and
/// the file holds the records before it, as it did before that append began. Every record
/// before the cut is decoded and verified, and anything else refuses the whole stream as
/// [`decode_records`] does: a whole frame that is malformed or fails its signature, bytes no
/// frame begins with, a stream past its bounds.
///
/// # Errors
/// As [`decode_records`], but for a last record cut short.
#[cfg(any(unix, test))]
pub(crate) fn decode_records_before_cut(bytes: &[u8]) -> Result<Vec<Record>> {
    if bytes.len() > MAX_RECORDS_STREAM {
        return Err(Error::new("membership record stream is too large"));
    }
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        if out.len() >= MAX_HELD_RECORDS {
            return Err(Error::new("membership record stream exceeds record bound"));
        }
        let len = match Record::wire_len_prefix(&bytes[at..]) {
            Ok(len) => len,
            Err(_) if Record::is_cut_frame(&bytes[at..]) => break,
            Err(refused) => return Err(refused),
        };
        out.push(Record::decode(&bytes[at..at + len])?);
        at += len;
    }
    Ok(out)
}

/// One record frame of a stream: its id (the SHA-256 of its bytes, which
/// is [`Record::id`] for a record this build reads) and the parents its bytes name.
#[cfg(unix)]
pub(crate) struct Frame {
    pub(crate) id: RecordId,
    pub(crate) parents: Vec<RecordId>,
    #[cfg(test)]
    pub(crate) range: std::ops::Range<usize>,
}

/// Every record frame of `bytes`, under [`decode_records`]'s bounds, read without judging a
/// frame's kind and without verifying anything ([`Record::frame_prefix`]): a stream holding a
/// record of a kind only a newer build writes still names each of its records. It is for naming
/// ids back to the server that sent them (`untaken`), never for trusting them. One walk, two
/// judgments: [`decode_records`] and the strict HTTP frame reader refuse an unknown kind,
/// while this framing pass retains its identity without trusting it.
///
/// # Errors
/// `bytes` exceeds [`MAX_RECORDS_STREAM`], holds more than [`MAX_HELD_RECORDS`] frames, or holds
/// one this layout cannot frame (truncated, oversized, another layout version).
#[cfg(all(test, unix))]
pub(crate) fn frames(bytes: &[u8]) -> Result<Vec<Frame>> {
    if bytes.len() > MAX_RECORDS_STREAM {
        return Err(Error::new("membership record stream is too large"));
    }
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        if out.len() >= MAX_HELD_RECORDS {
            return Err(Error::new("membership record stream exceeds record bound"));
        }
        let (len, parents) = Record::frame_prefix(&bytes[at..])?;
        let end = at
            .checked_add(len)
            .filter(|end| *end <= bytes.len() && len <= MAX_RECORD)
            .ok_or_else(|| Error::new("membership record is truncated"))?;
        out.push(Frame {
            id: aterm_digest::Sha256::digest(&bytes[at..end]),
            parents,
            range: at..end,
        });
        at = end;
    }
    Ok(out)
}

/// The frames of `bytes` (read into `frames`, [`frames`]) that this build reads
/// ([`Record::decode`]) and that descend from no frame of `bytes` it cannot read, in stream order:
/// what a pull still offers its store of a stream [`decode_records`] refuses as a whole. A server
/// of a newer build sends a record of a kind this build does not know beside records it reads
/// (its ancestors, its siblings), and refusing the whole stream kept those from every server
/// that holds it. A frame that descends from one this build cannot read is left out: it could
/// only wait, on a parent that never lands. Each frame is checked on its own, as
/// [`Store::recovering`] reads a records file.
#[cfg(all(test, unix))]
pub(crate) fn readable_part(bytes: &[u8], frames: &[Frame]) -> Vec<u8> {
    let readable = readable_mask(frames, |_, frame| {
        bytes
            .get(frame.range.clone())
            .is_some_and(|wire| Record::decode(wire).is_ok())
    });
    frames
        .iter()
        .zip(readable)
        .filter(|(_, readable)| *readable)
        .filter_map(|(frame, _)| bytes.get(frame.range.clone()))
        .flatten()
        .copied()
        .collect()
}

/// Which frames are readable and descend from no unreadable frame in this stream.
#[cfg(unix)]
pub(crate) fn readable_mask(
    frames: &[Frame],
    mut readable: impl FnMut(usize, &Frame) -> bool,
) -> Vec<bool> {
    let at: BTreeMap<RecordId, usize> = frames
        .iter()
        .enumerate()
        .map(|(i, frame)| (frame.id, i))
        .collect();
    let mut children: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, frame) in frames.iter().enumerate() {
        for parent in &frame.parents {
            if let Some(&j) = at.get(parent)
                && j != i
            {
                children.entry(j).or_default().push(i);
            }
        }
    }
    let mut unread: Vec<bool> = frames
        .iter()
        .enumerate()
        .map(|(i, frame)| !readable(i, frame))
        .collect();
    let mut stack: Vec<usize> = (0..unread.len()).filter(|&i| unread[i]).collect();
    while let Some(j) = stack.pop() {
        for &i in children.get(&j).into_iter().flatten() {
            if !unread[i] {
                unread[i] = true;
                stack.push(i);
            }
        }
    }
    unread.into_iter().map(|unread| !unread).collect()
}

/// `records`, each after every genesis and parent among them, ties by id. A record that will
/// wait in the store (a parent neither among them nor `held`, or one that waits itself) also
/// comes after every `Admit` of its signer among them, so the store meets a seat before the
/// waiting record that needs one. A record that will not wait lands after its parents and
/// finds its seat in its causal past, so it needs no seat order. Parents never loop (a record's
/// id hashes its parents' ids) but seats can: a Mac admitted again after a record of its own
/// has that `Admit` follow the record. When every record left waits for a seat, the lowest id
/// whose parents are out goes next.
pub(crate) fn causal<R: Borrow<Record>>(
    records: impl IntoIterator<Item = R>,
    held: impl Fn(&RecordId) -> bool,
) -> Vec<R> {
    let mut by_id: BTreeMap<RecordId, R> =
        records.into_iter().map(|r| (record_of(&r).id, r)).collect();
    let geneses: Vec<RecordId> = by_id
        .iter()
        .filter(|(_, r)| record_of(*r).kind() == Kind::Genesis)
        .map(|(id, _)| *id)
        .collect();
    // Each record's prerequisites among them: `true` for a parent or a genesis, `false` for a
    // seat.
    let mut before: BTreeMap<RecordId, BTreeMap<RecordId, bool>> = BTreeMap::new();
    for (id, record) in &by_id {
        let record = record_of(record);
        let mut prior: BTreeMap<RecordId, bool> = record
            .parents()
            .iter()
            .filter(|p| *p != id && by_id.contains_key(*p))
            .map(|p| (*p, true))
            .collect();
        if record.kind() != Kind::Genesis {
            prior.extend(geneses.iter().map(|g| (*g, true)));
        }
        before.insert(*id, prior);
    }
    let mut waits = BTreeSet::new();
    for id in release_order(&before) {
        let parents = by_id.get(&id).map_or(&[][..], |r| record_of(r).parents());
        if parents.iter().any(|p| {
            if by_id.contains_key(p) {
                waits.contains(p)
            } else {
                !held(p)
            }
        }) {
            waits.insert(id);
        }
    }
    let mut admits_by_member: BTreeMap<super::MemberId, Vec<RecordId>> = BTreeMap::new();
    for (id, record) in &by_id {
        if let Body::Admit { entrant, .. } = record_of(record).body()
            && record_of(record).actor() != &entrant.member()
        {
            admits_by_member
                .entry(entrant.member())
                .or_default()
                .push(*id);
        }
    }
    for id in &waits {
        let Some(admits) = by_id
            .get(id)
            .and_then(|r| admits_by_member.get(record_of(r).actor()))
        else {
            continue;
        };
        if let Some(prior) = before.get_mut(id) {
            for admit in admits.iter().filter(|a| *a != id) {
                prior.entry(*admit).or_insert(false);
            }
        }
    }
    let mut out = Vec::with_capacity(by_id.len());
    for id in release_order(&before) {
        if let Some(record) = by_id.remove(&id) {
            out.push(record);
        }
    }
    out.extend(by_id.into_values());
    out
}

/// The record `r` holds or is.
fn record_of<R: Borrow<Record>>(r: &R) -> &Record {
    r.borrow()
}

/// Each record of `log` with its depth: the longest path to it from the genesis.
fn depths(log: &Log) -> BTreeMap<RecordId, usize> {
    // `order` holds each record after its parents.
    let mut depth: BTreeMap<RecordId, usize> = BTreeMap::new();
    for record in log.order.iter() {
        let deepest = record
            .parents()
            .iter()
            .filter_map(|parent| depth.get(parent))
            .max()
            .map_or(0, |parent| parent + 1);
        depth.insert(record.id, deepest);
    }
    depth
}

/// What a pull names of a log's held records ([`Store::frontier`], [`Store::have`]), derived once
/// for them: the log keeps it until it holds another record, and its clones share it.
#[derive(Debug)]
pub(super) struct Advertised {
    frontier: Vec<RecordId>,
    have: Vec<RecordId>,
}

/// What a pull names of `log`'s held records, derived once for them ([`Advertised`]).
fn pull_names(log: &Log) -> &Advertised {
    log.advertised.get_or_init(|| {
        let depth = depths(log);
        let frontier = frontier_of(log, &depth);
        let have = have_of(log, &depth, &frontier);
        Advertised { frontier, have }
    })
}

/// `log`'s heads, deepest first ([`depths`]), ties by id ([`Store::frontier`]).
fn frontier_of(log: &Log, depth: &BTreeMap<RecordId, usize>) -> Vec<RecordId> {
    let mut heads = log.heads();
    heads.sort_by(|a, b| depth.get(b).cmp(&depth.get(a)).then_with(|| a.cmp(b)));
    heads
}

/// [`Store::have`] of `log`, whose heads deepest first are `frontier` ([`frontier_of`]).
fn have_of(log: &Log, depth: &BTreeMap<RecordId, usize>, frontier: &[RecordId]) -> Vec<RecordId> {
    let deepest_parent = |id: &RecordId| {
        log.get(id)?
            .parents()
            .iter()
            .filter(|parent| depth.contains_key(*parent))
            .max_by(|a, b| depth.get(*a).cmp(&depth.get(*b)).then_with(|| b.cmp(a)))
            .copied()
    };
    let (mut named, mut walked) = (BTreeSet::new(), BTreeSet::new());
    let mut out = Vec::new();
    for &head in frontier {
        if out.len() >= HAVE_IDS {
            break;
        }
        if named.insert(head) {
            out.push(head);
        }
        let (mut at, mut back, mut next) = (head, 0usize, 1usize);
        while out.len() < HAVE_IDS
            && walked.insert(at)
            && let Some(parent) = deepest_parent(&at)
        {
            at = parent;
            back += 1;
            if back == next {
                next *= 2;
                if named.insert(at) {
                    out.push(at);
                }
            }
        }
    }
    out
}

/// The ids `before` keys, each after every id its map names, ties by id. When every id left
/// still waits for one, the lowest id whose `true` prerequisites are all out goes next: those
/// never loop, and only the `false` ones can.
fn release_order(before: &BTreeMap<RecordId, BTreeMap<RecordId, bool>>) -> Vec<RecordId> {
    let mut children: BTreeMap<RecordId, Vec<(RecordId, bool)>> = BTreeMap::new();
    let mut unmet: BTreeMap<RecordId, (usize, usize)> = BTreeMap::new();
    for (id, prior) in before {
        for (p, hard) in prior {
            children.entry(*p).or_default().push((*id, *hard));
        }
        unmet.insert(*id, (prior.len(), prior.values().filter(|h| **h).count()));
    }
    let mut ready: BTreeSet<RecordId> = unmet
        .iter()
        .filter(|(_, (all, _))| *all == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut stalled: BTreeSet<RecordId> = unmet
        .iter()
        .filter(|(_, (all, hard))| *all > 0 && *hard == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut released = BTreeSet::new();
    let mut out = Vec::with_capacity(before.len());
    while let Some(id) = ready.pop_first().or_else(|| stalled.pop_first()) {
        if !released.insert(id) {
            continue;
        }
        out.push(id);
        for (child, hard) in children.get(&id).into_iter().flatten() {
            if let Some((all, hard_left)) = unmet.get_mut(child) {
                *all -= 1;
                if *hard {
                    *hard_left -= 1;
                }
                if *all == 0 {
                    ready.insert(*child);
                } else if *hard && *hard_left == 0 {
                    stalled.insert(*child);
                }
            }
        }
    }
    out
}

fn alab_lineage() -> [u8; 32] {
    crate::b64::decode_padded(crate::ALAB_SIGNER)
        .and_then(|key| <[u8; 32]>::try_from(key.as_slice()).ok())
        .unwrap_or([0; 32])
}

/// The ALab vault's keys this build names: [`crate::ALAB_SIGNER`] and every key it retired
/// ([`crate::ALAB_RETIRED`]). The release a rotation asks for names the new key and retires the
/// old one, and ALab's genesis may be the old key's.
fn alab_keys() -> Vec<[u8; 32]> {
    let named = crate::envelope::Retired::compiled();
    named.current.into_iter().chain(named.keys).collect()
}

/// A grow-only set of verified membership records for one vault's network.
///
/// Once a genesis arrives, records whose causal parents are present are checked and ordered by
/// [`Log`]; verified records that arrive before genesis wait in `unrooted` (bounded by
/// [`MAX_WAITING`]) and enter [`Log`] when genesis arrives. Its log holds its records within the
/// [`RecordBudgets`] the store was made with, and the store holds at most their held total in
/// all, waiting records counted: past that, the genesis or a record whose parents are held takes
/// the place of a record waiting on a missing parent.
#[derive(Clone, Debug, Default)]
pub struct Store {
    compiled: Option<RecordId>,
    network: Option<RecordId>,
    log: Option<Log>,
    unrooted: BTreeMap<RecordId, Record>,
    lineage: Option<Vec<[u8; 32]>>,
    /// The budgets its log takes when its genesis arrives ([`RecordBudgets::current`] when the
    /// store was made).
    budgets: RecordBudgets,
}

/// Drop records waiting on a missing parent, highest id first, until `log` holds at most its
/// held total ([`RecordBudgets::held`]) of records and [`MAX_RECORDS_STREAM`] bytes in all, so
/// its stream stays one [`decode_records`] reads.
fn make_room(log: &mut Log) {
    while log.order.len() + log.waiting.len() > log.budgets.held
        || log.total_bytes() > MAX_RECORDS_STREAM
    {
        if !log.drop_last_waiting() {
            return;
        }
    }
}

impl Store {
    /// An empty store for vault `name`. ALab's pins [`genesis::compiled`], or is
    /// [`Store::dormant`] under the ALab vault's keys this build names (its current key and every
    /// retired one) on a build that compiles no digest.
    ///
    /// # Errors
    /// The compiled ALab genesis digest is malformed.
    pub fn for_vault(name: &str) -> Result<Self> {
        if name != crate::ALAB_VAULT {
            return Ok(Self::with_compiled(None));
        }
        Ok(match genesis::compiled()? {
            Some(compiled) => Self::with_compiled(Some(compiled)),
            None => Self::dormant(&alab_keys()),
        })
    }

    /// An empty store pinned to `compiled` when `Some`, within [`RecordBudgets::current`].
    #[must_use]
    pub fn with_compiled(compiled: Option<RecordId>) -> Self {
        Self {
            compiled,
            network: compiled,
            log: None,
            unrooted: BTreeMap::new(),
            lineage: None,
            budgets: RecordBudgets::current(),
        }
    }

    /// ALab's store on a build that compiles no ALab genesis digest (until F12b): the one genesis
    /// it takes is signed with one of `lineage`, the ALab vault's keys, which every ALab Mac
    /// already trusts for the envelope and one of which `aterm secrets genesis` signs Genesis v2
    /// with. With no digest to check, the first genesis a store takes binds it for good, and any
    /// LAN host can sign one with a seed it made up and the real Commons root copied in: a store
    /// bound so would refuse ALab's real genesis as another network's. Once a digest is compiled
    /// the store checks the genesis by it alone ([`Log::alab`]).
    ///
    /// `lineage` is every key of the vault's line, not one: the genesis outlives the key that
    /// signed it. After a rotation the vault's envelope is signed by a new key whose handoff chain
    /// names the old one, and a store keyed to the new key alone would refuse the genesis the old
    /// key signed, root nothing, and drop that genesis from the file at its next write.
    #[must_use]
    pub fn dormant(lineage: &[[u8; 32]]) -> Self {
        Self {
            compiled: None,
            network: None,
            log: None,
            unrooted: BTreeMap::new(),
            lineage: Some(lineage.to_vec()),
            budgets: RecordBudgets::current(),
        }
    }

    /// This store, with an uncompiled store's keys set to `lineage` (replacing a
    /// [`Store::dormant`] store's keys, or pinning a store that has no compiled genesis digest);
    /// a store with a compiled genesis digest is unchanged. A server's record store follows the
    /// keys of the envelope the server holds (its signer and every key its handoff chain names)
    /// or the key it pins for that vault.
    #[must_use]
    pub fn under(mut self, lineage: &[[u8; 32]]) -> Self {
        if self.compiled.is_none() {
            self.lineage = Some(lineage.to_vec());
        }
        self
    }

    /// This store, with a [`Store::dormant`] store also taking a genesis that any of `keys`
    /// signed; any other store is unchanged. A Mac's ALab store adds the key its verified config
    /// pins and every key it reached that pin through: a Mac a release behind a rotation names
    /// only the old key, and a genesis signed after the rotation is the new key's.
    #[must_use]
    pub fn also_under(mut self, keys: &[[u8; 32]]) -> Self {
        if let Some(lineage) = &mut self.lineage {
            for key in keys {
                if !lineage.contains(key) {
                    lineage.push(*key);
                }
            }
        }
        self
    }

    /// The budgets its log holds its records within ([`RecordBudgets::current`] when the store
    /// was made).
    #[must_use]
    pub const fn budgets(&self) -> RecordBudgets {
        self.budgets
    }

    /// The keys one of which must have signed the genesis this store takes, when it checks the
    /// genesis by its signer ([`Store::dormant`], [`Store::under`]) rather than by a compiled
    /// digest or not at all.
    #[must_use]
    pub fn keys(&self) -> Option<&[[u8; 32]]> {
        self.lineage.as_deref()
    }

    /// The exact genesis this empty store requires, independently of signer-key admission.
    #[must_use]
    pub const fn pinned_network(&self) -> Option<RecordId> {
        self.compiled
    }

    /// Keep an already-verified causal projection with this store's admission context.
    #[cfg(unix)]
    pub(crate) fn projected(mut self, log: Log) -> Result<Self> {
        if self
            .log()
            .is_none_or(|held| held.network() != log.network())
            || self.budgets != log.budgets()
        {
            return Err(Error::new(
                "membership projection changed its root or budgets",
            ));
        }
        self.network = Some(*log.network());
        self.log = Some(log);
        self.unrooted.clear();
        Ok(self)
    }

    /// Decode and verify a persisted record stream for vault `name`.
    ///
    /// # Errors
    /// `name`'s compiled digest is malformed, or any record in `bytes` fails verification, or the
    /// store takes none of them and refuses one, as one of another network ([`Store::ingest`]).
    pub fn decode(name: &str, bytes: &[u8]) -> Result<Self> {
        let mut store = Self::for_vault(name)?;
        store.ingest(bytes)?;
        Ok(store)
    }

    /// [`Store::decode`] for a stream this Mac wrote itself, keeping what this build takes of one
    /// it refuses as a whole: every record that still verifies and that [`Store::insert`] accepts,
    /// each after the genesis, seats and parents among them, as [`Store::ingest`] orders a
    /// stream. A stream that stops reading part-way keeps what came before. A newer build can
    /// refuse what an older one stored (another genesis, a record that no longer verifies), a
    /// write can be torn, and one refused record must not stop the store taking records.
    ///
    /// # Errors
    /// `name`'s compiled digest is malformed.
    pub fn recover(name: &str, bytes: &[u8]) -> Result<Self> {
        Ok(Self::for_vault(name)?.recovering(bytes))
    }

    /// [`Store::recover`] into this empty store, as [`Store::for_vault`] and
    /// [`Store::also_under`] made it, acquiring at most this store's held budget of verified
    /// frames from the first [`MAX_RECORDS_STREAM`] bytes. Duplicate frames count too.
    #[must_use]
    pub fn recovering(self, bytes: &[u8]) -> Self {
        let bytes = &bytes[..bytes.len().min(MAX_RECORDS_STREAM)];
        if let Ok(records) = decode_records_bounded(bytes, self.budgets.held) {
            return self.recover_decoded(records, true);
        }
        let mut records = Vec::new();
        let mut at = 0usize;
        while at < bytes.len() && records.len() < self.budgets.held {
            let Ok(len) = Record::wire_len_prefix(&bytes[at..]) else {
                break;
            };
            let Some(end) = at
                .checked_add(len)
                .filter(|end| *end > at && *end <= bytes.len() && len <= MAX_RECORD)
            else {
                break;
            };
            if let Ok(record) = Record::decode(&bytes[at..end]) {
                records.push(record);
            }
            at = end;
        }
        self.recover_decoded(records, false)
    }

    fn recover_decoded(mut self, records: Vec<Record>, strict: bool) -> Self {
        if !(strict && self.ingest_records(records.clone()).is_ok()) {
            for record in causal(records, |_| false) {
                let _ = self.insert(record);
            }
        }
        // Holding records one by one leaves the held list up to twice their length, and a
        // recovered store is often kept as it is for the process's life (`records_cache`), its
        // clones sharing that list. Where no clone shares it yet, let the spare room go.
        if let Some(order) = self
            .log
            .as_mut()
            .and_then(|log| std::sync::Arc::get_mut(&mut log.order))
        {
            order.shrink_to_fit();
        }
        self
    }

    /// Encode every stored record in deterministic order: causally complete records from genesis
    /// first, then records waiting on a missing parent by id.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.encoded_records()
            .flat_map(Record::bytes)
            .copied()
            .collect()
    }

    /// Write exactly [`Store::encode`]'s canonical signed stream without a whole-stream buffer.
    ///
    /// # Errors
    /// The writer fails before the entire stream is written.
    pub fn write_encoded<W: std::io::Write + ?Sized>(&self, writer: &mut W) -> std::io::Result<()> {
        for record in self.encoded_records() {
            writer.write_all(record.bytes())?;
        }
        Ok(())
    }

    fn encoded_records(&self) -> impl Iterator<Item = &Record> {
        let pending = if let Some(log) = &self.log {
            causal(log.waiting.values(), |id| log.held.contains_key(id))
        } else {
            causal(self.unrooted.values(), |_| false)
        };
        self.held_from(0).iter().chain(pending)
    }

    /// The bytes of the log's records, each after its parents ([`Store::encode`]'s first part):
    /// what a vault server's records file holds (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1 F23,
    /// rule 1). Empty for a store with no genesis. A record waiting on a parent is held in memory
    /// only: a server serves complete records alone ([`Store::missing`]), the complete part of a
    /// log only grows, so the file changes only by an append of what the log holds past it, and
    /// it stays within what every reader takes.
    #[must_use]
    pub fn encode_held(&self) -> Vec<u8> {
        self.encode_held_from(0)
    }

    /// [`Store::encode_held`] past the log's first `from` records: what goes on the end of a
    /// records file that holds those, already in causal order.
    #[must_use]
    pub fn encode_held_from(&self, from: usize) -> Vec<u8> {
        self.held_from(from)
            .iter()
            .flat_map(Record::bytes)
            .copied()
            .collect()
    }

    /// Write exactly [`Store::encode_held_from`]'s complete-record suffix without collecting it.
    ///
    /// # Errors
    /// The writer fails before every byte is written.
    pub fn write_held_from<W: std::io::Write + ?Sized>(
        &self,
        from: usize,
        writer: &mut W,
    ) -> std::io::Result<()> {
        for record in self.held_from(from) {
            writer.write_all(record.bytes())?;
        }
        Ok(())
    }

    /// Exact byte length of [`Store::encode_held_from`]'s complete-record suffix.
    ///
    /// # Errors
    /// The sum cannot be represented as a byte length.
    pub fn held_len_from(&self, from: usize) -> std::io::Result<usize> {
        self.held_from(from).iter().try_fold(0usize, |sum, record| {
            sum.checked_add(record.bytes().len()).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "held record length overflow",
                )
            })
        })
    }

    /// How many records `on_file` holds, where this store is `on_file` with complete records
    /// held after its own and nothing else: each holds a log and no record waiting on a parent,
    /// and this log begins with every record of `on_file`'s, in `on_file`'s order. A file that
    /// holds exactly `on_file`'s records then holds exactly this store's once
    /// [`Store::write_held_from`] that count is on its end. `None` for any other pair: a file
    /// of them is written whole.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn extends(&self, on_file: &Self) -> Option<usize> {
        let (log, base) = (self.log.as_ref()?, on_file.log.as_ref()?);
        if !(log.waiting.is_empty()
            && base.waiting.is_empty()
            && self.unrooted.is_empty()
            && on_file.unrooted.is_empty())
        {
            return None;
        }
        let held = base.order.len();
        let begins = log.order.get(..held)?;
        (std::sync::Arc::ptr_eq(&log.order, &base.order)
            || begins
                .iter()
                .zip(base.order.iter())
                .all(|(ours, theirs)| ours.id == theirs.id))
        .then_some(held)
    }

    fn held_from(&self, from: usize) -> &[Record] {
        self.log
            .as_ref()
            .and_then(|log| log.order.get(from..))
            .unwrap_or_default()
    }

    /// Whether the store holds no records.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total distinct verified records held (causally complete plus waiting).
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.log {
            Some(log) => log.order.len() + log.waiting.len(),
            None => self.unrooted.len(),
        }
    }

    /// Bytes in the existing wire encoding, without allocating another copy.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn encoded_len(&self) -> usize {
        self.log.as_ref().map_or_else(
            || {
                self.unrooted
                    .values()
                    .map(|record| record.bytes().len())
                    .sum()
            },
            |log| log.total_bytes(),
        )
    }

    /// The network's [`Log`] once its genesis is held.
    #[must_use]
    pub const fn log(&self) -> Option<&Log> {
        self.log.as_ref()
    }

    /// Whether `id` is already held (complete or waiting).
    #[must_use]
    pub fn contains(&self, id: &RecordId) -> bool {
        match &self.log {
            Some(log) => log.held.contains_key(id) || log.waiting.contains_key(id),
            None => self.unrooted.contains_key(id),
        }
    }

    /// Whether `id` is held with every parent (not waiting on one).
    pub(crate) fn holds(&self, id: &RecordId) -> bool {
        self.log
            .as_ref()
            .is_some_and(|log| log.held.contains_key(id))
    }

    /// The record `id` when it is held but not complete: waiting on a parent, or held before a
    /// genesis. A store at its bound may let it go for another record ([`make_room`]), so a
    /// caller that offers it again with its parent (`server::apply_records`) still lands it.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the unix vault server offers records again")
    )]
    pub(crate) fn uncomplete(&self, id: &RecordId) -> Option<&Record> {
        match &self.log {
            Some(log) => log.waiting.get(id),
            None => self.unrooted.get(id),
        }
    }

    /// Exact signed wire bytes of `id` when held.
    #[must_use]
    pub fn get_bytes(&self, id: &RecordId) -> Option<&[u8]> {
        match &self.log {
            Some(log) => log
                .get(id)
                .or_else(|| log.waiting.get(id))
                .map(Record::bytes),
            None => self.unrooted.get(id).map(Record::bytes),
        }
    }

    /// Exact signed wire bytes of `id` when held as a causally complete record from genesis.
    #[must_use]
    pub fn complete_bytes(&self, id: &RecordId) -> Option<&[u8]> {
        self.log.as_ref()?.get(id).map(Record::bytes)
    }

    /// What this store keeps in memory, as the records cache counts it: the wire bytes of every
    /// record it holds, keeps waiting or keeps before a genesis, and its reachability index's
    /// bytes.
    #[cfg(any(unix, test))]
    pub(crate) fn footprint(&self) -> usize {
        self.log.as_ref().map_or_else(
            || self.unrooted.values().map(|r| r.bytes().len()).sum(),
            Log::footprint,
        )
    }

    /// The most [`Store::footprint`] reaches: a store keeps at most [`MAX_RECORDS_STREAM`] bytes
    /// of records, and an index of at most [`MAX_HELD_RECORDS`] records.
    #[cfg(unix)]
    pub(crate) const MAX_FOOTPRINT: usize =
        MAX_RECORDS_STREAM + super::reach::reserved_for(MAX_HELD_RECORDS);

    /// Every held record id, sorted.
    #[must_use]
    pub fn inventory(&self) -> BTreeSet<RecordId> {
        let mut ids = BTreeSet::new();
        if let Some(log) = &self.log {
            ids.extend(log.order.iter().map(|r| r.id));
            ids.extend(log.waiting.keys().copied());
        } else {
            ids.extend(self.unrooted.keys().copied());
        }
        ids
    }

    /// Every causally complete record id held from genesis, sorted. Waiting records missing a
    /// parent and records held before a genesis are omitted so peers never offer orphans.
    #[must_use]
    pub fn complete_inventory(&self) -> BTreeSet<RecordId> {
        self.log.as_ref().map_or_else(BTreeSet::new, |log| {
            log.order.iter().map(|r| r.id).collect()
        })
    }

    /// Up to `limit` causally complete record ids held here that `remote` lacks, in causal order
    /// after `after` (wrapping around to genesis) so every parent precedes its children without
    /// starving later records behind a deferred one.
    #[must_use]
    pub fn complete_missing(
        &self,
        remote: &BTreeSet<RecordId>,
        after: Option<&RecordId>,
        limit: usize,
    ) -> Vec<RecordId> {
        let Some(log) = &self.log else {
            return Vec::new();
        };
        let split = after
            .and_then(|cursor| log.order.iter().position(|r| &r.id == cursor))
            .map_or(0, |idx| idx + 1);
        log.order[split..]
            .iter()
            .chain(&log.order[..split])
            .map(|r| r.id)
            .filter(|id| !remote.contains(id))
            .take(limit)
            .collect()
    }

    /// Insert each decoded record in `records` after any genesis, seat and parent among them.
    pub fn insert_many(&mut self, records: impl IntoIterator<Item = Record>) {
        for record in causal(records, |id| self.holds(id)) {
            let _ = self.insert(record);
        }
    }

    /// Maximal causally complete record ids (`?have=` frontier), deepest first (the longest path
    /// from the genesis), ties by id. A pull names them in this order ([`Store::have`]), only as
    /// many as its URL has room for (`transport::records_have_url`), and a deeper head names more
    /// of the log: a seated key's forks at sequence one, each a head right after its seat, come
    /// after an honest chain's head. Waiting records whose ancestry to genesis is incomplete are
    /// omitted so peers still send their missing ancestors.
    #[must_use]
    pub fn frontier(&self) -> Vec<RecordId> {
        self.log
            .as_ref()
            .map_or_else(Vec::new, |log| pull_names(log).frontier.clone())
    }

    /// What a pull names in `?have=`: the [`Store::frontier`], each head followed by its
    /// ancestors 1, 2, 4, 8, … records back along its deepest parents, at most [`HAVE_IDS`] ids in
    /// all. A server answers the records past the ids it holds ([`Store::missing`]), so one that
    /// lacks a head (a record its build refuses, a store at its bound) still finds a recent
    /// ancestor among them and answers the records past it, where the heads alone would have it
    /// answer its whole log on every pull. A head's walk ends where an earlier head's walked, so
    /// the walks cover each record once.
    #[must_use]
    pub fn have(&self) -> Vec<RecordId> {
        self.log
            .as_ref()
            .map_or_else(Vec::new, |log| pull_names(log).have.clone())
    }

    /// Records held here that a peer advertising causal frontier `have` lacks, in causal order
    /// from genesis. Only causally complete records travel, none before a genesis: `have` names
    /// complete heads alone, so a waiting record would go out again on every pull. One waiting
    /// on a parent travels once the parent lands; one whose parent exists nowhere never does.
    #[must_use]
    pub fn missing(&self, have: &[RecordId]) -> Vec<Record> {
        let Some(log) = &self.log else {
            return Vec::new();
        };
        let known = log.reach.closure(log.indexes(have));
        log.order
            .iter()
            .enumerate()
            .filter(|(i, _)| (known[i / 64] >> (i % 64)) & 1 == 0)
            .map(|(_, r)| r.clone())
            .collect()
    }

    /// Insert one decoded [`Record`], returning `true` when it was new to this store.
    ///
    /// # Errors
    /// The record belongs to another network, is a genesis none of a [`Store::dormant`] store's
    /// keys signed, or fails [`Log::against`] or [`Log::insert`], or it misses a parent and the store
    /// already holds its held total ([`RecordBudgets::held`]) of records, or one short of
    /// [`MAX_WAITING`] before a genesis.
    /// It also fails when holding or keeping the record waiting would make the store's wire stream
    /// exceed [`MAX_RECORDS_STREAM`] (reserving [`MAX_RECORD`] bytes before a genesis).
    pub fn insert(&mut self, record: Record) -> Result<bool> {
        if let Some(compiled) = &self.compiled
            && record.network_id() != compiled
        {
            if record.kind() == Kind::Genesis {
                Log::against(record, Some(compiled), &alab_lineage())?;
            }
            return Err(Error::new("a membership record of another network"));
        }
        if let Some(network) = &self.network
            && record.network_id() != network
        {
            return Err(Error::new(if record.kind() == Kind::Genesis {
                "a record of another network's genesis"
            } else {
                "a membership record of another network"
            }));
        }
        if self.contains(&record.id) {
            return Ok(false);
        }
        // Records that wait on a missing parent cost a seated key (a removed Mac's among them)
        // nothing to make, so they never shut out the genesis or a record whose parents are
        // held: once the store holds its held total, such a record takes the place of a waiting
        // one, and a record missing a parent finds no room. The genesis comes first, before any
        // bound.
        if let Some(log) = &mut self.log {
            let complete = record.parents().iter().all(|p| log.held.contains_key(p));
            if !complete
                && (log.order.len() + log.waiting.len() >= log.budgets.held
                    || log.total_bytes().saturating_add(record.bytes().len()) > MAX_RECORDS_STREAM)
            {
                return Err(Error::of_kind(
                    crate::ErrorKind::MembershipCapacity,
                    "membership log holds its limit of records",
                ));
            }
            let inserted = log.insert(record)?;
            make_room(log);
            return Ok(inserted != super::Inserted::Known);
        }
        if record.kind() == Kind::Genesis {
            if let Some(lineage) = &self.lineage
                && !matches!(record.body(), Body::Genesis { signer, .. } if lineage.contains(signer))
            {
                return Err(Error::new(
                    "a genesis no key of the ALab vault signed: with no ALab genesis digest compiled, ALab's genesis is the one a key of its line signed",
                ));
            }
            let mut log = Log::against(record, self.compiled.as_ref(), &alab_lineage())?;
            log.budgets = self.budgets;
            log.require_seats();
            self.network = Some(*log.network());
            // Each after its parents among them, so the log meets every seat before the records
            // that need it: a record whose parents are missing waits only for a seated signer.
            let unrooted = std::mem::take(&mut self.unrooted).into_values();
            for pending in causal(unrooted, |id| log.held.contains_key(id)) {
                if pending.network_id() == log.network() {
                    let _ = log.insert(pending);
                    make_room(&mut log);
                }
            }
            make_room(&mut log);
            self.log = Some(log);
            return Ok(true);
        }
        // One short of `MAX_WAITING`, and `MAX_RECORD` bytes short of `MAX_RECORDS_STREAM`: a
        // store with no genesis yet always has room for it, however many records arrived first,
        // and once it lands the store holds no more than its held total and `MAX_RECORDS_STREAM`
        // (what `decode_records` reads back).
        let unrooted_bytes: usize = self.unrooted.values().map(|r| r.bytes().len()).sum();
        if self.unrooted.len() >= MAX_WAITING - 1
            || unrooted_bytes.saturating_add(record.bytes().len()) > MAX_RECORDS_STREAM - MAX_RECORD
        {
            return Err(Error::of_kind(
                crate::ErrorKind::MembershipCapacity,
                "membership log holds its limit of waiting records",
            ));
        }
        self.unrooted.insert(record.id, record);
        Ok(true)
    }

    /// Decode and verify every record in `bytes`, then insert each one: if any record in the
    /// stream does not decode or verify, the store is left unchanged. A record [`Store::insert`]
    /// refuses is skipped and the rest are kept: which records a store refuses at its bounds
    /// (its [`RecordBudgets`], [`MAX_WAITING`], [`MAX_RECORDS_STREAM`]) depends on what reached it
    /// first, and a peer's stream that carries a record this store refuses still brings every
    /// other record. It works in place: [`Store::insert`] changes nothing when it refuses a
    /// record, and gives a waiting record's place only to a record it keeps, so a stream that
    /// adds nothing leaves the store as it was without a copy of a store that can reach
    /// [`MAX_RECORDS_STREAM`].
    /// Returns how many new records entered the store.
    ///
    /// # Errors
    /// `bytes` fails [`decode_records`], or no record entered the store and [`Store::insert`]
    /// refused one.
    pub fn ingest(&mut self, bytes: &[u8]) -> Result<usize> {
        self.ingest_records(decode_records(bytes)?)
    }

    /// [`Store::ingest`] of `records` already decoded and verified ([`Record::decode`]): each is
    /// inserted after any genesis, seat and parent among them, and a record [`Store::insert`]
    /// refuses is skipped. Returns how many new records entered the store.
    ///
    /// # Errors
    /// No record entered the store and [`Store::insert`] refused one.
    pub fn ingest_records(&mut self, records: Vec<Record>) -> Result<usize> {
        if records.is_empty() {
            return Ok(0);
        }
        let before = self.inventory();
        let mut refused = None;
        for record in causal(records, |id| self.holds(id)) {
            if let Err(e) = self.insert(record) {
                refused = Some(e);
            }
        }
        let added = self.inventory().difference(&before).count();
        if added == 0
            && let Some(e) = refused
        {
            return Err(e);
        }
        Ok(added)
    }

    /// Ingest `records` (causal order as [`Store::ingest_records`]), and say per record: held
    /// now or before ([`MemberVerdict::Applied`]); held waiting, or refused for a parent or seat
    /// this view lacks, or capacity ([`MemberVerdict::Waiting`]); refused for good
    /// ([`MemberVerdict::Refused`]). One entry per offered record, in the order offered.
    ///
    /// A record of another network is refused for good. A refused record with a parent this
    /// store does not hold complete waits, whatever [`Store::insert`] said: its parents (and a
    /// seat its signer takes in one) can still arrive, and then the same record lands. A refused
    /// record whose parents are all held complete was judged on its whole causal past, which
    /// never changes, so it is refused for good.
    pub fn ingest_classified(&mut self, records: Vec<Record>) -> Vec<(RecordId, MemberVerdict)> {
        let offered: Vec<(RecordId, RecordId, Vec<RecordId>)> = records
            .iter()
            .map(|r| (r.id, *r.network_id(), r.parents().to_vec()))
            .collect();
        let mut refused: BTreeMap<RecordId, Error> = BTreeMap::new();
        for record in causal(records, |id| self.holds(id)) {
            let id = record.id;
            if let Err(e) = self.insert(record) {
                refused.insert(id, e);
            }
        }
        offered
            .into_iter()
            .map(|(id, network, parents)| {
                let verdict = self.classify(&id, &network, &parents, refused.get(&id));
                (id, verdict)
            })
            .collect()
    }

    /// [`Store::ingest_classified`]'s verdict on record `id` of `network` with `parents`, after
    /// the ingest, where [`Store::insert`] answered it `refused`.
    fn classify(
        &self,
        id: &RecordId,
        network: &RecordId,
        parents: &[RecordId],
        refused: Option<&Error>,
    ) -> MemberVerdict {
        if self.complete_bytes(id).is_some() {
            return MemberVerdict::Applied;
        }
        if self.contains(id) {
            return MemberVerdict::Waiting(
                if self.log.is_some() {
                    "held waiting on a parent"
                } else {
                    "held before a genesis"
                }
                .to_owned(),
            );
        }
        let foreign = self.compiled.is_some_and(|c| c != *network)
            || self.network.is_some_and(|n| n != *network);
        let why = refused.map_or_else(|| "let go for another record".to_owned(), Error::to_string);
        if foreign {
            return MemberVerdict::Refused(why);
        }
        if refused.is_some_and(|e| e.kind() == crate::ErrorKind::MembershipCapacity)
            || parents.iter().any(|p| self.complete_bytes(p).is_none())
        {
            return MemberVerdict::Waiting(why);
        }
        MemberVerdict::Refused(why)
    }
}

/// What became of one record a [`Store::ingest_classified`] call offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberVerdict {
    /// Held complete, now or before.
    Applied,
    /// Held waiting, or refused for a parent or seat this view lacks, or for capacity: offer it
    /// again after a membership change.
    Waiting(String),
    /// Refused for good.
    Refused(String),
}

/// The membership log's records made durable: called after every write of a client's records
/// file ([`crate::client::Paths::records_file`]), with the store as written. Not blocking. A vault
/// server's records files are not this hook's, and a writer that ends between the durable write
/// and the call (a crash) calls nothing.
pub struct Written<'a> {
    /// The vault folder whose records file was written ([`crate::client::Paths::dir`]).
    pub paths_dir: &'a std::path::Path,
    /// The store the file now holds.
    pub store: &'a Store,
}

/// The process's [`Written`] hook ([`on_written`]).
type WrittenHook = Box<dyn Fn(&Written<'_>) + Send + Sync>;

static WRITTEN: std::sync::OnceLock<WrittenHook> = std::sync::OnceLock::new();

/// Install the process's hook for records-file writes ([`Written`]); false where one is installed
/// already. The hook runs on the writing thread under the writer's lock, after the durable
/// write: it must not block (no I/O round trip).
pub fn on_written(hook: impl Fn(&Written<'_>) + Send + Sync + 'static) -> bool {
    WRITTEN.set(Box::new(hook)).is_ok()
}

/// A thread's override of the process's [`Written`] hook ([`with_written`]).
#[cfg(any(test, debug_assertions))]
type WrittenHere = std::rc::Rc<dyn Fn(&Written<'_>)>;

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    /// This thread's override of the process's hook ([`with_written`]).
    static WRITTEN_HERE: std::cell::RefCell<Option<WrittenHere>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
std::thread_local! {
    /// MUTANT ([`without_written_call`]): the writer's hook call removed.
    static WRITTEN_CALL_REMOVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `run` with `hook` in place of the process's [`on_written`] hook for writes on this
/// thread, so tests in one binary do not share one hook.
#[cfg(any(test, debug_assertions))]
pub fn with_written<T>(hook: impl Fn(&Written<'_>) + 'static, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<WrittenHere>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let before = self.0.take();
            WRITTEN_HERE.with(|here| *here.borrow_mut() = before);
        }
    }
    let hook: WrittenHere = std::rc::Rc::new(hook);
    let _restore = Restore(WRITTEN_HERE.with(|here| here.borrow_mut().replace(hook)));
    run()
}

/// Run `run` with the writer's hook call removed on this thread: the negative control of the
/// hook's tests.
#[cfg(test)]
pub(crate) fn without_written_call<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            WRITTEN_CALL_REMOVED.with(|removed| removed.set(self.0));
        }
    }
    let _restore = Restore(WRITTEN_CALL_REMOVED.with(|removed| removed.replace(true)));
    run()
}

/// Hand `w` to this thread's [`with_written`] override, or else the process's [`on_written`]
/// hook, if any. Its writers are the Unix client's records-file write ([`crate::client`]) and the
/// tests.
#[cfg(any(unix, test))]
pub(crate) fn written(w: &Written<'_>) {
    #[cfg(test)]
    if WRITTEN_CALL_REMOVED.with(std::cell::Cell::get) {
        return;
    }
    #[cfg(any(test, debug_assertions))]
    if let Some(hook) = WRITTEN_HERE.with(|here| here.borrow().clone()) {
        hook(w);
        return;
    }
    if let Some(hook) = WRITTEN.get() {
        hook(w);
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
