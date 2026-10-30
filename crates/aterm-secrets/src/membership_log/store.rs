// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Grow-only membership-record wire streams and stores (`docs/DESIGN-fluid-swarm-2026-10-28.md`
//! §4.1 F13).
//!
//! Each [`Record`] is self-delimiting on the wire (`ALM1`), so a stream or store file is the
//! concatenation of signed record bytes: one record's [`Record::bytes`] is a one-record stream.

use super::{Body, Kind, Log, MAX_RECORD, MAX_RECORDS, MAX_WAITING, Record, RecordId, genesis};
use crate::{Error, Result};
use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet};

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
/// `bytes` exceeds [`MAX_RECORDS_STREAM`], holds more than [`MAX_RECORDS`] records, or contains a
/// malformed or invalidly signed record.
pub fn decode_records(bytes: &[u8]) -> Result<Vec<Record>> {
    if bytes.len() > MAX_RECORDS_STREAM {
        return Err(Error::new("membership record stream is too large"));
    }
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        if out.len() >= MAX_RECORDS {
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
    for record in &log.order {
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

/// `log`'s heads, deepest first ([`depths`]), ties by id ([`Store::frontier`]).
fn frontier_of(log: &Log, depth: &BTreeMap<RecordId, usize>) -> Vec<RecordId> {
    let mut heads = log.heads();
    heads.sort_by(|a, b| depth.get(b).cmp(&depth.get(a)).then_with(|| a.cmp(b)));
    heads
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
/// [`MAX_WAITING`]) and enter [`Log`] when genesis arrives. It holds at most [`MAX_RECORDS`]
/// records in all: past that, the genesis or a record whose parents are held takes the place of a
/// record waiting on a missing parent.
#[derive(Clone, Debug, Default)]
pub struct Store {
    compiled: Option<RecordId>,
    network: Option<RecordId>,
    log: Option<Log>,
    unrooted: BTreeMap<RecordId, Record>,
    lineage: Option<Vec<[u8; 32]>>,
}

/// Drop records waiting on a missing parent, highest id first, until `log` holds at most
/// [`MAX_RECORDS`] records and [`MAX_RECORDS_STREAM`] bytes in all, so its stream stays one
/// [`decode_records`] reads.
fn make_room(log: &mut Log) {
    while log.order.len() + log.waiting.len() > MAX_RECORDS
        || log.total_bytes() > MAX_RECORDS_STREAM
    {
        if log.waiting.pop_last().is_none() {
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

    /// An empty store pinned to `compiled` when `Some`.
    #[must_use]
    pub const fn with_compiled(compiled: Option<RecordId>) -> Self {
        Self {
            compiled,
            network: compiled,
            log: None,
            unrooted: BTreeMap::new(),
            lineage: None,
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

    /// The keys one of which must have signed the genesis this store takes, when it checks the
    /// genesis by its signer ([`Store::dormant`], [`Store::under`]) rather than by a compiled
    /// digest or not at all.
    #[must_use]
    pub fn keys(&self) -> Option<&[[u8; 32]]> {
        self.lineage.as_deref()
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
    /// [`Store::also_under`] made it.
    #[must_use]
    pub fn recovering(self, bytes: &[u8]) -> Self {
        let mut store = self;
        if store.ingest(bytes).is_ok() {
            return store;
        }
        let mut records = Vec::new();
        let mut at = 0usize;
        while at < bytes.len() && records.len() < MAX_RECORDS {
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
        for record in causal(records, |_| false) {
            let _ = store.insert(record);
        }
        store
    }

    /// Encode every stored record in deterministic order: causally complete records from genesis
    /// first, then records waiting on a missing parent by id.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(log) = &self.log {
            for record in &log.order {
                out.extend_from_slice(record.bytes());
            }
            for record in causal(log.waiting.values(), |id| log.held.contains_key(id)) {
                out.extend_from_slice(record.bytes());
            }
        } else {
            for record in causal(self.unrooted.values(), |_| false) {
                out.extend_from_slice(record.bytes());
            }
        }
        out
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
    fn holds(&self, id: &RecordId) -> bool {
        self.log
            .as_ref()
            .is_some_and(|log| log.held.contains_key(id))
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
        let Some(log) = &self.log else {
            return Vec::new();
        };
        frontier_of(log, &depths(log))
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
        let Some(log) = &self.log else {
            return Vec::new();
        };
        let depth = depths(log);
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
        for head in frontier_of(log, &depth) {
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

    /// Records held here that a peer advertising causal frontier `have` lacks, in causal order
    /// from genesis. Only causally complete records travel, none before a genesis: `have` names
    /// complete heads alone, so a waiting record would go out again on every pull. One waiting
    /// on a parent travels once the parent lands; one whose parent exists nowhere never does.
    #[must_use]
    pub fn missing(&self, have: &[RecordId]) -> Vec<Record> {
        let Some(log) = &self.log else {
            return Vec::new();
        };
        let mut known = BTreeSet::new();
        let mut stack: Vec<RecordId> = have
            .iter()
            .filter(|id| log.get(id).is_some())
            .copied()
            .collect();
        while let Some(id) = stack.pop() {
            if known.insert(id)
                && let Some(record) = log.get(&id)
            {
                stack.extend(record.parents().iter().copied());
            }
        }
        log.order
            .iter()
            .filter(|r| !known.contains(&r.id))
            .cloned()
            .collect()
    }

    /// Insert one decoded [`Record`], returning `true` when it was new to this store.
    ///
    /// # Errors
    /// The record belongs to another network, is a genesis none of a [`Store::dormant`] store's
    /// keys signed, or fails [`Log::against`] or [`Log::insert`], or it misses a parent and the store
    /// already holds [`MAX_RECORDS`] records, or one short of [`MAX_WAITING`] before a genesis.
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
        // held: once the store holds `MAX_RECORDS`, such a record takes the place of a waiting
        // one, and a record missing a parent finds no room. The genesis comes first, before any
        // bound.
        if let Some(log) = &mut self.log {
            let complete = record.parents().iter().all(|p| log.held.contains_key(p));
            if !complete
                && (log.order.len() + log.waiting.len() >= MAX_RECORDS
                    || log.total_bytes().saturating_add(record.bytes().len()) > MAX_RECORDS_STREAM)
            {
                return Err(Error::new("membership log holds its limit of records"));
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
        // and once it lands the store holds no more than `MAX_RECORDS` and `MAX_RECORDS_STREAM`
        // (what `decode_records` reads back).
        let unrooted_bytes: usize = self.unrooted.values().map(|r| r.bytes().len()).sum();
        if self.unrooted.len() >= MAX_WAITING - 1
            || unrooted_bytes.saturating_add(record.bytes().len()) > MAX_RECORDS_STREAM - MAX_RECORD
        {
            return Err(Error::new(
                "membership log holds its limit of waiting records",
            ));
        }
        self.unrooted.insert(record.id, record);
        Ok(true)
    }

    /// Decode and verify every record in `bytes`, then insert each one: if any record in the
    /// stream does not decode or verify, the store is left unchanged. A record [`Store::insert`]
    /// refuses is skipped and the rest are kept: which records a store refuses at its bounds
    /// ([`MAX_RECORDS`], [`MAX_WAITING`], [`MAX_RECORDS_STREAM`]) depends on what reached it
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
        let records = decode_records(bytes)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::age::Identity;
    use crate::commons::binding::Request;
    use crate::membership_log::{Body, Entrant, Evidence, MemberId};

    const NETWORK: [u8; 16] = [0x11; 16];
    const AUTHORITY: [u8; 32] = [0x22; 32];

    fn entrant(tag: u8, label: &str) -> ([u8; 32], [u8; 16], MemberId, Entrant) {
        let writer = [tag; 32];
        let machine = [tag.wrapping_add(0x20); 16];
        let incarnation = [tag; 16];
        let identity = Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
        let request = Request::create(NETWORK, AUTHORITY, machine, incarnation, &identity, &writer)
            .expect("request");
        let member = request.binding().actor();
        let entrant = Entrant {
            request,
            node_key: [tag.wrapping_add(0x80); 32],
            name: format!("mac-{tag}"),
            label: label.to_owned(),
        };
        (writer, incarnation, member, entrant)
    }

    /// A store's copy shares its records' bytes. A server copies its store to try each POST
    /// against it (`server::post_records`), which any LAN host may send, and that copy held
    /// every record's bytes again, up to `MAX_RECORDS_STREAM` (about 64 MiB), once per POST.
    /// NEGATIVE CONTROL: a `Record` that owns its bytes (`wire: Vec<u8>`, main before this fix)
    /// copies them with the store, and the test fails "the copy holds the record's bytes again".
    #[test]
    fn a_stores_copy_shares_every_records_bytes() {
        let (w1, inc1, m1, e1) = entrant(1, "m1");
        let (_, _, _, e2) = entrant(2, "m2");
        let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
        let admit = Log::new(genesis.clone())
            .expect("log")
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit");
        let mut store = Store::for_vault("team").expect("store");
        assert!(store.insert(genesis).expect("genesis"));
        assert!(store.insert(admit).expect("admitted"));
        let copy = store.clone();
        assert_eq!(copy.len(), 2);
        for id in store.inventory() {
            let held = store.get_bytes(&id).expect("held");
            let copied = copy.get_bytes(&id).expect("copied");
            assert!(
                std::ptr::eq(held, copied),
                "the copy holds the record's bytes again"
            );
        }
    }

    #[test]
    fn stream_codec_and_store_converge_across_orders_and_refuse_corruption() {
        let (w1, inc1, m1, e1) = entrant(1, "m1");
        let (_, _, _, e2) = entrant(2, "m2");
        let (_, _, _, e3) = entrant(3, "m3");
        let (w_other, inc_other, _, e_other) = entrant(9, "m9");
        let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
        let other_genesis = Record::found(&[8; 32], vec![e_other], 1_000).expect("other genesis");

        // A store with a compiled genesis digest accepts only that genesis and refuses another.
        let mut pinned = Store::with_compiled(Some(*genesis.id()));
        assert!(pinned.insert(other_genesis).is_err());
        assert!(pinned.insert(genesis.clone()).expect("matching genesis"));

        let left_log = Log::new(genesis.clone()).expect("log");
        let admit_two = left_log
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit 2");
        let right_log = Log::new(genesis.clone()).expect("log");
        let admit_three = right_log
            .draft(
                &m1,
                1_002,
                Body::Admit {
                    entrant: e3,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit 3");

        // Pre-genesis bogus network unrooted record cannot squat the store's network, and
        // valid out-of-order records land once genesis arrives.
        let bogus_unrooted = super::super::Draft {
            network: [0xde; 32],
            sequence: 1,
            previous: None,
            parents: vec![[0xde; 32]],
            authored_ms: 999,
            body: Body::Name {
                name: "squatter".to_owned(),
            },
        }
        .sign(&w_other, inc_other)
        .expect("signed");
        let mut out_of_order = Store::default();
        assert!(out_of_order.insert(bogus_unrooted).expect("unrooted"));
        assert!(out_of_order.insert(admit_two.clone()).expect("unrooted"));
        assert!(out_of_order.frontier().is_empty());
        assert!(out_of_order.insert(genesis.clone()).expect("genesis"));
        assert_eq!(out_of_order.frontier(), vec![*admit_two.id()]);
        assert_eq!(
            out_of_order
                .missing(&[*genesis.id()])
                .iter()
                .map(|r| *r.id())
                .collect::<Vec<_>>(),
            vec![*admit_two.id()]
        );

        // Divergent frontier diff converges to the union, and batch corruption leaves store intact.
        let mut right_store = Store::default();
        right_store
            .ingest(&encode_records([&genesis, &admit_three]))
            .expect("ingest");
        let diff = out_of_order.missing(&right_store.frontier());

        let mut bad_batch = encode_records([&admit_two]);
        let flip = bad_batch.len() - 1;
        bad_batch[flip] ^= 0x01;
        let before = right_store.inventory();
        assert!(right_store.ingest(&bad_batch).is_err());
        assert_eq!(right_store.inventory(), before);

        assert_eq!(right_store.ingest(&encode_records(&diff)).expect("pull"), 1);
        assert_eq!(
            right_store.inventory(),
            BTreeSet::from([*genesis.id(), *admit_two.id(), *admit_three.id()])
        );

        // An unseated signer's record is refused even while its parent is missing, and never
        // held when the parent arrives.
        let unseated_waiting = super::super::Draft {
            network: *genesis.id(),
            sequence: 1,
            previous: None,
            parents: vec![*admit_three.id()],
            authored_ms: 1_500,
            body: Body::Name {
                name: "forger".to_owned(),
            },
        }
        .sign(&w_other, inc_other)
        .expect("signed");
        assert!(out_of_order.insert(unseated_waiting.clone()).is_err());
        assert!(
            out_of_order
                .insert(admit_three.clone())
                .expect("parent lands")
        );
        assert!(!out_of_order.contains(unseated_waiting.id()));
    }

    /// A record signed by a key `tag` names, which no founder or `Admit` seats, under `network`,
    /// following a parent nobody holds.
    fn unseated(network: RecordId, tag: u8, i: usize) -> Record {
        super::super::Draft {
            network,
            sequence: 1,
            previous: None,
            parents: vec![[0x77; 32]],
            authored_ms: 5_000 + i as u64,
            body: Body::Name {
                name: format!("junk-{i}"),
            },
        }
        .sign(&[tag; 32], [tag; 16])
        .expect("signed")
    }

    #[test]
    fn a_keyless_flood_of_waiting_records_leaves_room_for_members() {
        let (w1, inc1, m1, e1) = entrant(1, "m1");
        let (_, _, m2, e2) = entrant(2, "m2");
        let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
        let mut store = Store::for_vault("team").expect("store");
        store.ingest(genesis.bytes()).expect("genesis");

        // Any host that reaches a server can POST records signed by a key it made, under the
        // network's public id, waiting on parents that never arrive.
        let flood: Vec<Record> = (0..MAX_RECORDS - 1)
            .map(|i| unseated(*genesis.id(), 0xEE, i))
            .collect();
        assert!(store.ingest(&encode_records(&flood)).is_err());
        for record in flood.iter().take(3) {
            assert!(store.insert(record.clone()).is_err());
        }
        assert_eq!(store.len(), 1);

        let admit = store
            .log()
            .expect("log")
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit");
        assert!(store.insert(admit).expect("a member's admit lands"));
        assert!(store.log().expect("log").membership().contains(&m2));
    }

    #[test]
    fn a_store_with_no_genesis_yet_always_has_room_for_it() {
        let (w1, inc1, m1, e1) = entrant(1, "m1");
        let (w2, inc2, m2, e2) = entrant(2, "m2");
        let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
        // ALab's store while no digest is compiled: nothing pins its network before the genesis.
        let mut store = Store::with_compiled(None);

        // A member's records that came before the genesis: an `Admit`, and a record of the Mac
        // it lets in whose id sorts first.
        let mut log = Log::new(genesis.clone()).expect("log");
        let admit = log
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit");
        log.insert(admit.clone()).expect("admit");
        let name = (0..64)
            .map(|i| {
                log.draft(
                    &m2,
                    1_002,
                    Body::Name {
                        name: format!("two-{i}"),
                    },
                )
                .sign(&w2, inc2)
                .expect("name")
            })
            .find(|name| name.id() < admit.id())
            .expect("one of 64 ids sorts first");
        assert!(store.insert(name.clone()).expect("waits for the genesis"));
        assert!(store.insert(admit.clone()).expect("waits for the genesis"));

        // Then a flood under any network id, before the genesis arrives.
        let mut accepted = 2;
        for i in 0..MAX_WAITING {
            let network = if i % 2 == 0 {
                [0xAB; 32]
            } else {
                *genesis.id()
            };
            if store.insert(unseated(network, 0xEE, i)).is_ok() {
                accepted += 1;
            }
        }
        assert_eq!(accepted, MAX_WAITING - 1);

        assert!(store.insert(genesis.clone()).expect("the genesis lands"));
        assert_eq!(
            store.inventory(),
            BTreeSet::from([*genesis.id(), *admit.id(), *name.id()])
        );
        let membership = store.log().expect("log").membership();
        assert_eq!(
            membership.get(&m2).map(|m| m.name.as_str()),
            Some(match name.body() {
                Body::Name { name } => name.as_str(),
                _ => unreachable!("a name"),
            })
        );
    }

    #[test]
    fn a_peer_stream_with_a_record_this_store_refuses_still_brings_the_rest() {
        let (wa, inca, ma, ea) = entrant(1, "m1");
        let (_, _, _, er) = entrant(2, "m2");
        let (_, _, mc, ec) = entrant(3, "m3");
        let (wx, incx, _, _) = entrant(9, "m9");
        let genesis = Record::found(&[9; 32], vec![ea, er], 1_000).expect("genesis");
        let mut here = Store::for_vault("team").expect("store");
        assert!(here.insert(genesis.clone()).expect("genesis"));
        let admit = here
            .log()
            .expect("log")
            .draft(
                &ma,
                1_001,
                Body::Admit {
                    entrant: ec,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&wa, inca)
            .expect("admit");

        // A record from a key no record seats, ahead of a member's `Admit`, in one stream.
        let stray = super::super::Draft {
            network: *genesis.id(),
            sequence: 1,
            previous: None,
            parents: vec![*genesis.id()],
            authored_ms: 1_002,
            body: Body::Name {
                name: "stray".to_owned(),
            },
        }
        .sign(&wx, incx)
        .expect("signed");
        let stream = encode_records([&stray, &admit]);
        assert_eq!(here.ingest(&stream).expect("the admit enters"), 1);
        assert!(here.log().expect("log").membership().contains(&mc));
        assert!(!here.contains(stray.id()));
        // A stream the store takes nothing from still fails, as one record a POST sends alone.
        assert!(here.ingest(stray.bytes()).is_err());
    }

    #[test]
    fn a_made_up_genesis_never_binds_alabs_store_while_this_build_compiles_no_digest() {
        // Any host can sign a genesis with a seed it makes up and copy the real Commons root into
        // it, and the first genesis a store takes binds it. With no digest compiled (until F12b)
        // ALab's store takes only the genesis the ALab vault's key signed; after F12b the digest
        // refuses the made-up one instead.
        let (_, _, _, e1) = entrant(1, "m1");
        let (_, _, _, e2) = entrant(2, "m2");
        let made_up = Record::found(&[0x66; 32], vec![e1], 1_000).expect("genesis");
        let mut alab = Store::for_vault(crate::ALAB_VAULT).expect("alab store");
        assert!(
            alab.insert(made_up.clone()).is_err(),
            "a made-up genesis bound ALab's store"
        );
        assert!(alab.ingest(made_up.bytes()).is_err());
        assert!(Store::decode(crate::ALAB_VAULT, made_up.bytes()).is_err());
        assert!(alab.is_empty() && alab.log().is_none());

        // A dormant or held-key store under a vault key takes the genesis that key signed, and
        // only that.
        let ours = Record::found(&[0x55; 32], vec![e2], 1_000).expect("genesis");
        let Body::Genesis { signer, .. } = ours.body() else {
            unreachable!("a genesis");
        };
        let mut dormant = Store::dormant(&[*signer]);
        assert!(dormant.insert(made_up.clone()).is_err());
        assert!(
            dormant
                .insert(ours.clone())
                .expect("the vault key's genesis")
        );
        let mut held_team = Store::for_vault("team")
            .expect("team store")
            .under(std::slice::from_ref(signer));
        assert!(held_team.insert(made_up.clone()).is_err());
        assert!(held_team.insert(ours).expect("held team key's genesis"));

        // An unheld non-ALab vault's store still takes its first genesis: its server takes one
        // only from its own Mac or under a pin.
        let mut team = Store::for_vault("team").expect("team store");
        assert!(team.insert(made_up).expect("team genesis"));
    }

    /// A rotation of the ALab vault's key asks the next release to name the new key
    /// (`ALAB_SIGNER`) and retire the old one (`ALAB_RETIRED`); until F12b compiles a digest,
    /// ALab's store is dormant. Keyed to the new key alone, it refused the Genesis v2 the old key
    /// signed on every Mac on that release (`Store::recover` in `client::load_records`), rooted
    /// nothing, and the next write erased the genesis from the records file, which no dormant
    /// build could take back. A dormant ALab store now takes a genesis that any key this build
    /// names for the vault signed: the current one or a retired one.
    /// NEGATIVE CONTROL: `Store::for_vault` keyed to `ALAB_SIGNER` alone (main before this fix)
    /// roots nothing.
    #[test]
    fn a_release_that_retires_the_key_that_signed_alabs_genesis_keeps_it() {
        use crate::envelope::{Retired, TEST_RETIRED, signer_public};
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
            }
        }
        let old_seed = [0x57; 32];
        let old = signer_public(&old_seed).expect("public");
        let new = signer_public(&[0x58; 32]).expect("public");
        TEST_RETIRED.with(|cell| {
            *cell.borrow_mut() = Some(Retired {
                keys: vec![old],
                current: Some(new),
            });
        });
        let _reset = Reset;

        let (w1, inc1, m1, e1) = entrant(1, "m1");
        let (_, _, _, e2) = entrant(2, "m2");
        let (_, _, _, e3) = entrant(3, "m3");
        let genesis = Record::found(&old_seed, vec![e1], 1_000).expect("genesis");
        let admit = Log::new(genesis.clone())
            .expect("log")
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit");
        let file = encode_records([&genesis, &admit]);
        let held = Store::recover(crate::ALAB_VAULT, &file).expect("recover");
        assert!(
            held.log().is_some(),
            "the release that retired the genesis key refused ALab's genesis"
        );
        assert!(
            Store::decode(crate::ALAB_VAULT, &held.encode())
                .expect("the next write keeps the genesis")
                .contains(genesis.id())
        );

        // A genesis signed by a key this build does not name still binds nothing.
        let made_up = Record::found(&[0x66; 32], vec![e3], 1_000).expect("genesis");
        let mut alab = Store::for_vault(crate::ALAB_VAULT).expect("alab store");
        assert!(alab.insert(made_up).is_err());
    }

    /// A pull names only the heads its URL has room for (`transport::records_have_url`), so the
    /// frontier lists first the heads that name most of the log: the deepest. A key's forks at
    /// sequence one, each a head right after its seat, come after an honest chain's head.
    /// NEGATIVE CONTROL: heads in id order (`Log::heads`, main before this fix) put the fork
    /// first.
    #[test]
    fn the_frontier_lists_the_deepest_head_first() {
        let (w1, inc1, m1, e1) = entrant(1, "m1");
        let (w2, inc2, _, e2) = entrant(2, "m2");
        let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
        let mut log = Log::new(genesis.clone()).expect("log");
        let admit = log
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit");
        log.insert(admit.clone()).expect("admit");
        let mut chain = Vec::new();
        for n in 0..2u64 {
            let next = log
                .draft(
                    &m1,
                    1_002 + n,
                    Body::Name {
                        name: format!("m1-{n}"),
                    },
                )
                .sign(&w1, inc1)
                .expect("chain");
            log.insert(next.clone()).expect("chain");
            chain.push(next);
        }
        let deep = chain.last().expect("a chain head").clone();
        let fork = (0..4_096u64)
            .map(|salt| {
                super::super::Draft {
                    network: *genesis.id(),
                    sequence: 1,
                    previous: None,
                    parents: vec![*admit.id()],
                    authored_ms: 5_000 + salt,
                    body: Body::Name {
                        name: "fork".to_owned(),
                    },
                }
                .sign(&w2, inc2)
                .expect("fork")
            })
            .find(|fork| fork.id() < deep.id())
            .expect("a fork whose id sorts first");
        let mut store = Store::for_vault("team").expect("store");
        store.insert_many([genesis, admit, fork.clone()].into_iter().chain(chain));
        assert_eq!(store.frontier(), vec![*deep.id(), *fork.id()]);
    }

    #[test]
    fn records_missing_a_parent_never_shut_out_the_genesis_or_a_record_whose_parents_are_held() {
        let (w1, inc1, m1, e1) = entrant(1, "m1");
        let (_, _, _, e2) = entrant(2, "m2");
        // A founder's key, so its records wait in a store's log: a Mac removed later keeps its
        // seat, and its key still signs.
        let (w_junk, inc_junk, _, e_junk) = entrant(9, "m9");
        let genesis = Record::found(&[9; 32], vec![e1, e_junk], 1_000).expect("genesis");
        let admit_two = Log::new(genesis.clone())
            .expect("log")
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit 2");
        // A seated key's records naming the genesis's network (in every binary once F12b compiles
        // its digest), each after a parent that exists nowhere: more than a store keeps.
        let mut junk: Vec<Record> = (0..=MAX_WAITING)
            .map(|i| {
                let mut parent = [0x77; 32];
                parent[..8].copy_from_slice(&(i as u64).to_le_bytes());
                super::super::Draft {
                    network: *genesis.id(),
                    sequence: 1,
                    previous: None,
                    parents: vec![parent],
                    authored_ms: 2_000 + i as u64,
                    body: Body::Name {
                        name: format!("junk-{i}"),
                    },
                }
                .sign(&w_junk, inc_junk)
                .expect("signed")
            })
            .collect();
        let extra = junk.pop().expect("one more");

        // A store with no digest and an F12b store pinned to the genesis both take the junk
        // before the genesis, where nothing can tell it from honest early records.
        for compiled in [None, Some(*genesis.id())] {
            let mut store = Store::with_compiled(compiled);
            for record in &junk {
                let _ = store.insert(record.clone());
            }
            assert_eq!(store.len(), MAX_WAITING - 1);
            assert!(
                store
                    .insert(genesis.clone())
                    .expect("the genesis finds room")
            );
            assert!(
                store
                    .insert(admit_two.clone())
                    .expect("a record whose parents are held finds room")
            );
            assert!(store.log().expect("rooted").get(admit_two.id()).is_some());
            assert_eq!(store.len(), MAX_RECORDS);
            assert!(
                store.insert(extra.clone()).is_err(),
                "a record missing a parent finds no room in a full store"
            );
            // The full store still reads back whole from its own stream.
            let mut reloaded = Store::with_compiled(compiled);
            reloaded.ingest(&store.encode()).expect("reloads");
            assert_eq!(reloaded.inventory(), store.inventory());
        }
    }

    #[test]
    fn only_causally_complete_records_travel() {
        let (w1, inc1, m1, e1) = entrant(1, "m1");
        let (_, _, _, e2) = entrant(2, "m2");
        // A founder's key: only a seated signer's record waits for its parents.
        let (w_junk, inc_junk, _, e_junk) = entrant(9, "m9");
        let genesis = Record::found(&[9; 32], vec![e1, e_junk], 1_000).expect("genesis");
        let admit_two = Log::new(genesis.clone())
            .expect("log")
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit 2");
        let orphan = super::super::Draft {
            network: *genesis.id(),
            sequence: 1,
            previous: None,
            parents: vec![[0x77; 32]],
            authored_ms: 2_000,
            body: Body::Name {
                name: "orphan".to_owned(),
            },
        }
        .sign(&w_junk, inc_junk)
        .expect("signed");

        let mut store = Store::default();
        assert!(store.insert(orphan.clone()).expect("waits for a genesis"));
        assert!(
            store.missing(&[]).is_empty(),
            "nothing travels before a genesis"
        );
        assert!(store.insert(genesis.clone()).expect("genesis"));
        assert!(store.insert(admit_two.clone()).expect("admit 2"));
        assert!(
            store.contains(orphan.id()),
            "the orphan waits for its parent"
        );
        // `have` names complete heads only, so a waiting record in a reply would go out again on
        // every pull: a puller at this store's frontier gets nothing.
        assert!(store.missing(&store.frontier()).is_empty());
        assert_eq!(
            store
                .missing(&[])
                .iter()
                .map(|r| *r.id())
                .collect::<Vec<_>>(),
            vec![*genesis.id(), *admit_two.id()]
        );
    }

    #[test]
    fn waiting_records_stop_at_max_waiting() {
        let (w1, inc1, m1, e1) = entrant(1, "m1");
        let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
        let mut log = Log::new(genesis.clone()).expect("log");
        let missing_parent = [0x55; 32];
        for i in 0..MAX_WAITING {
            let draft = super::super::Draft {
                network: *genesis.id(),
                sequence: 1,
                previous: None,
                parents: vec![missing_parent],
                authored_ms: 2_000 + i as u64,
                body: Body::Name {
                    name: format!("host-{i}"),
                },
            };
            let record = draft.sign(&w1, inc1).expect("signed");
            assert_eq!(
                log.insert(record).expect("within cap"),
                super::super::Inserted::Waiting
            );
        }
        let over = super::super::Draft {
            network: *genesis.id(),
            sequence: 1,
            previous: None,
            parents: vec![missing_parent],
            authored_ms: 99_999,
            body: Body::Name {
                name: "overflow".to_owned(),
            },
        }
        .sign(&w1, inc1)
        .expect("signed");
        let _ = m1;
        let err = log.insert(over).expect_err("past MAX_WAITING");
        assert!(err.to_string().contains("waiting records"), "{err}");
    }

    #[test]
    fn ingest_and_waiting_seat_keep_out_of_order_entrant_records() {
        let (w1, inc1, m1, e1) = entrant(1, "m1");
        let (w2, inc2, m2, e2) = entrant(2, "m2");
        let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
        let mut author = Log::new(genesis.clone()).expect("log");
        let first_by_m1 = author
            .draft(
                &m1,
                1_001,
                Body::Name {
                    name: "m1-renamed".to_owned(),
                },
            )
            .sign(&w1, inc1)
            .expect("first by m1");
        author.insert(first_by_m1.clone()).expect("hold first");
        let admit = author
            .draft(
                &m1,
                1_002,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit");
        author.insert(admit.clone()).expect("hold admit");
        let name = (0..64)
            .map(|i| {
                author
                    .draft(
                        &m2,
                        1_003 + i,
                        Body::Name {
                            name: format!("m2-renamed-{i}"),
                        },
                    )
                    .sign(&w2, inc2)
                    .expect("name")
            })
            .find(|r| r.id() < admit.id())
            .expect("one of 64 ids sorts before admit");

        // 1. Out-of-order stream `[name, genesis, first_by_m1, admit]` into an empty store.
        let mut from_empty = Store::for_vault("team").expect("store");
        let full_stream = encode_records([&name, &genesis, &first_by_m1, &admit]);
        assert_eq!(from_empty.ingest(&full_stream).expect("ingest all 4"), 4);
        assert!(from_empty.log().expect("log").membership().contains(&m2));

        // 2. Out-of-order stream `[name, admit]` after genesis and `first_by_m1` are held.
        let mut seeded = Store::for_vault("team").expect("store");
        assert!(seeded.insert(genesis.clone()).expect("genesis"));
        assert!(seeded.insert(first_by_m1.clone()).expect("first_by_m1"));
        let child_before_seat = encode_records([&name, &admit]);
        assert_eq!(seeded.ingest(&child_before_seat).expect("ingest 2"), 2);
        assert!(seeded.log().expect("log").membership().contains(&m2));

        // 3. `admit` waits on `first_by_m1` while `name` arrives, then `Log::merge` copies both
        //    waiting records even though `name.id() < admit.id()`.
        let mut waiting = Store::for_vault("team").expect("store");
        assert!(waiting.insert(genesis.clone()).expect("genesis"));
        assert!(waiting.insert(admit.clone()).expect("admit waits"));
        assert!(
            waiting
                .insert(name.clone())
                .expect("name waits under waiting seat")
        );
        // Neither travels while it waits: `have` names complete heads only, so a waiting record
        // would go out again on every pull. Both go once `first_by_m1` lands.
        assert!(waiting.missing(&[*genesis.id()]).is_empty());
        let mut merged = Log::new(genesis).expect("merged log");
        merged.require_seats();
        merged
            .merge(waiting.log().expect("waiting log"))
            .expect("merge waiting records");
        assert_eq!(
            merged.insert(first_by_m1).expect("missing parent lands"),
            super::super::Inserted::Held
        );
        assert_eq!(
            merged.membership().get(&m2).map(|m| m.name.as_str()),
            Some(match name.body() {
                Body::Name { name } => name.as_str(),
                _ => unreachable!("a name"),
            })
        );
    }
    #[test]
    fn complete_inventory_and_complete_bytes_omit_waiting_and_unrooted_records() {
        let (w1, inc1, m1, e1) = entrant(1, "m1");
        let (_, _, _, e2) = entrant(2, "m2");
        let (w_junk, inc_junk, _, e_junk) = entrant(9, "m9");
        let genesis = Record::found(&[9; 32], vec![e1, e_junk], 1_000).expect("genesis");
        let admit_two = Log::new(genesis.clone())
            .expect("log")
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit 2");
        let orphan = super::super::Draft {
            network: *genesis.id(),
            sequence: 1,
            previous: None,
            parents: vec![[0x77; 32]],
            authored_ms: 2_000,
            body: Body::Name {
                name: "orphan".to_owned(),
            },
        }
        .sign(&w_junk, inc_junk)
        .expect("signed");

        let mut store = Store::default();
        assert!(store.insert(orphan.clone()).expect("unrooted"));
        assert!(store.complete_inventory().is_empty());
        assert!(store.complete_bytes(orphan.id()).is_none());
        assert!(
            store
                .complete_missing(&BTreeSet::new(), None, 16)
                .is_empty()
        );

        assert!(store.insert(genesis.clone()).expect("genesis"));
        assert!(store.insert(admit_two.clone()).expect("admit 2"));
        assert!(store.contains(orphan.id()));
        assert_eq!(
            store.complete_inventory(),
            BTreeSet::from([*genesis.id(), *admit_two.id()])
        );
        assert_eq!(store.complete_bytes(genesis.id()), Some(genesis.bytes()));
        assert_eq!(
            store.complete_bytes(admit_two.id()),
            Some(admit_two.bytes())
        );
        assert!(store.complete_bytes(orphan.id()).is_none());
        assert_eq!(
            store.complete_missing(&BTreeSet::new(), None, 16),
            vec![*genesis.id(), *admit_two.id()]
        );
        assert_eq!(
            store.complete_missing(&BTreeSet::new(), Some(genesis.id()), 16),
            vec![*admit_two.id(), *genesis.id()]
        );
    }

    #[test]
    fn oversized_waiting_records_stay_within_stream_bound_and_yield_to_complete_records() {
        let (w1, inc1, m1, e1) = entrant(1, "m1");
        let (_, _, _, e2) = entrant(2, "m2");
        let (w_junk, inc_junk, _, e_junk) = entrant(9, "m9");
        let genesis = Record::found(&[9; 32], vec![e1, e_junk], 1_000).expect("genesis");
        let mut log = Log::new(genesis.clone()).expect("log");
        let admit_two = log
            .draft(
                &m1,
                1_001,
                Body::Admit {
                    entrant: e2,
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&w1, inc1)
            .expect("admit 2");
        log.insert(admit_two.clone()).expect("admit 2");
        let (draft_ep, ep_key) = log
            .draft_epoch(&m1, 1_002, super::super::EpochAudience::Everyone, 1)
            .expect("draft epoch");
        let ep_rec = draft_ep.sign(&w1, inc1).expect("epoch");
        log.insert(ep_rec.clone()).expect("epoch");
        let big_entry = crate::text::Entry {
            name: "BIG_SECRET".to_owned(),
            value: crate::SecretBytes::new(vec![b'x'; crate::text::MAX_VALUE]),
            if_absent: false,
            anchor: None,
            audience: crate::text::Audience::Everyone,
        };
        let complete_put = log
            .draft_put(&m1, 1_003, *ep_rec.id(), &ep_key, &big_entry)
            .expect("draft put")
            .sign(&w1, inc1)
            .expect("complete put");

        // 65 waiting `Put` records (~1 MiB each) from a seated key, each missing its parent:
        // far fewer than `MAX_WAITING` (4096), yet > `MAX_RECORDS_STREAM` (64 MiB) in bytes.
        let ciphertext = vec![0xAAu8; crate::text::MAX_VALUE + 32];
        let mut waiting_puts: Vec<Record> = (0..66)
            .map(|i| {
                let mut parent = [0x77; 32];
                parent[..8].copy_from_slice(&(i as u64).to_le_bytes());
                super::super::Draft {
                    network: *genesis.id(),
                    sequence: 1,
                    previous: None,
                    parents: vec![parent],
                    authored_ms: 3_000 + i as u64,
                    body: Body::Put {
                        audience: super::super::EpochAudience::Everyone,
                        epoch: parent,
                        salt: [i as u8; 32],
                        ciphertext: ciphertext.clone(),
                    },
                }
                .sign(&w_junk, inc_junk)
                .expect("waiting put")
            })
            .collect();
        let extra = waiting_puts.pop().expect("extra waiting put");

        let mut store = Store::with_compiled(Some(*genesis.id()));
        for record in &waiting_puts {
            let _ = store.insert(record.clone());
        }
        assert!(store.encode().len() <= MAX_RECORDS_STREAM - MAX_RECORD);
        assert!(store.insert(extra.clone()).is_err());

        // The genesis and complete records (including a 1 MiB Put) land and displace waiting
        // records by byte size so `store.encode()` stays readable by `decode_records`.
        assert!(store.insert(genesis).expect("genesis lands"));
        for record in &waiting_puts {
            let _ = store.insert(record.clone());
        }
        assert!(store.insert(extra.clone()).is_err());
        assert!(store.insert(admit_two.clone()).expect("admit lands"));
        assert!(store.insert(ep_rec.clone()).expect("epoch lands"));
        assert!(store.insert(complete_put.clone()).expect("1 MiB Put lands"));
        let encoded = store.encode();
        assert!(
            encoded.len() <= MAX_RECORDS_STREAM,
            "encoded len {} exceeded MAX_RECORDS_STREAM {}",
            encoded.len(),
            MAX_RECORDS_STREAM
        );
        assert!(store.insert(extra).is_err());

        let mut reloaded = Store::with_compiled(store.compiled);
        reloaded
            .ingest(&encoded)
            .expect("stream reloads within MAX_RECORDS_STREAM");
        assert!(
            reloaded
                .log()
                .expect("log")
                .get(complete_put.id())
                .is_some()
        );
        assert_eq!(reloaded.inventory(), store.inventory());
    }

    /// A Mac admitted again after a record of its own (F16's Allow again, or any seated key with
    /// its public request): `causal` put every record after each `Admit` of its signer, and that
    /// `Admit` follows the record, so the order looped and fell back to id order. A record of a
    /// Mac whose `Admit` sorts later in that tail then waited with no seat and was refused, on
    /// every reload (`decode`, `recover`, a fresh store pulling `missing`), and the next write
    /// dropped it for good.
    /// NEGATIVE CONTROL: a seat edge from every record to each `Admit` of its signer, and the
    /// records that never release appended in id order (main before this fix), hold 5 of the 6.
    #[test]
    fn a_mac_admitted_again_after_its_own_record_keeps_every_record_across_a_reload() {
        fn signed(
            store: &Store,
            actor: &MemberId,
            body: Body,
            key: &[u8; 32],
            inc: [u8; 16],
        ) -> Record {
            store
                .log()
                .expect("log")
                .draft(actor, 1_001, body)
                .sign(key, inc)
                .expect("signed")
        }
        let (w1, inc1, m1, e1) = entrant(1, "m1");
        let (w2, inc2, m2, e2) = entrant(2, "m2");
        let (w3, inc3, m3, e3) = entrant(3, "m3");
        let genesis = Record::found(&[9; 32], vec![e1], 1_000).expect("genesis");
        let mut store = Store::for_vault("team").expect("store");
        assert!(store.insert(genesis).expect("genesis"));
        let admit = |entrant: &Entrant| Body::Admit {
            entrant: entrant.clone(),
            evidence: Evidence::Phrase,
        };
        let admit_two = signed(&store, &m1, admit(&e2), &w1, inc1);
        assert!(store.insert(admit_two).expect("m2 admitted"));
        let name_two = signed(
            &store,
            &m2,
            Body::Name {
                name: "two".to_owned(),
            },
            &w2,
            inc2,
        );
        assert!(store.insert(name_two.clone()).expect("m2 names itself"));
        let again = signed(&store, &m1, admit(&e2), &w1, inc1);
        assert!(
            again.parents().contains(name_two.id()),
            "the Admit follows m2's own record"
        );
        assert!(store.insert(again).expect("m2 admitted again"));
        let admit_three = signed(&store, &m2, admit(&e3), &w2, inc2);
        assert!(store.insert(admit_three.clone()).expect("m3 admitted"));
        let name_three = (0..64)
            .map(|i| {
                signed(
                    &store,
                    &m3,
                    Body::Name {
                        name: format!("three-{i}"),
                    },
                    &w3,
                    inc3,
                )
            })
            .find(|r| r.id() < admit_three.id())
            .expect("one of 64 ids sorts before m3's Admit");
        assert!(store.insert(name_three.clone()).expect("m3 names itself"));
        assert_eq!(store.len(), 6);
        let shown = |s: &Store| {
            s.log()
                .expect("log")
                .membership()
                .get(&m3)
                .map(|m| m.name.clone())
        };
        let Body::Name { name } = name_three.body() else {
            unreachable!("a name");
        };
        assert_eq!(shown(&store).as_deref(), Some(name.as_str()));

        let bytes = store.encode();
        let decoded = Store::decode("team", &bytes).expect("decode");
        let recovered = Store::recover("team", &bytes).expect("recover");
        let mut pulled = Store::for_vault("team").expect("store");
        pulled
            .ingest(&encode_records(&store.missing(&[])))
            .expect("a fresh store pulls everything");
        for (how, reloaded) in [
            ("decode", &decoded),
            ("recover", &recovered),
            ("pull", &pulled),
        ] {
            assert_eq!(reloaded.inventory(), store.inventory(), "{how}");
            assert_eq!(shown(reloaded), shown(&store), "{how}");
        }
    }
}
