// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A table store kept in memory across sessions ([`Kept`]): what a table repair session holds of
//! one table between its batches and its rounds, so a batch reopens what it held where an open
//! replayed every record the table holds.
//!
//! A store's records are two kinds of fact. Its physical facts (each record's wire, identifier,
//! receipt and the custody chain through it, its header, the writer quotas and the causal index
//! they admit) are a function of the journal's bytes, which only grow. Its eligibility facts
//! (each record's payload state: ready, quarantined or awaiting membership, and a ready record's
//! cutoff) are a session's judgment, and a judgment of each record's class alone
//! ([`crate::table::Class`]: its roster, epoch and signer): whether the class's source is
//! discarded under the control head, and whether a historical writer grant and key open its
//! payloads ([`crate::table::Verdict`]). A kept store holds the verdict of each class its records
//! were classified under, asked in the same read batch that classified them.
//!
//! A reopen ([`Kept::open`]) makes each judgment a fresh open makes before it reads a record:
//! the session's source reads, context and (writing) mutation guard, the enrollment claim and its
//! hold on the data folder, through the same [`Store::claimed`]. The claim must be the kept
//! store's custody (anchor and enrollment) acknowledging at least its records, exactly them where
//! it acknowledges as many. Then, in one read batch of its own, with the batch's fences: every
//! class's verdict is asked again, and the kept classification stands only where each answers as
//! it did; a writing reopen of a store holding records that await membership stands on nothing
//! (a writing open refuses them). The records classified at or past the acknowledged witness they
//! were classified under (a writing open's adopted tail, and every record appended since) are
//! replayed again as an open replays them, under the claim's acknowledged witness now. The
//! journal's records after the kept ones are read (a writer's from its log, an observer's from
//! the journal's process cache, `journals`), their chain continued from the kept witness and
//! judged against the claim as an open judges a whole chain, and replayed as an open replays
//! them. A writing reopen then finishes as a writing open does ([`Store::finish_open`]), seals
//! held writes as [`Store::open`] does, and every reopen ends in the store's check.
//!
//! So no classification outlives the evidence it was made under: a verdict that changed
//! (historical evidence removed, or restored, with the control head unchanged; a control head
//! that moved; a key that changed), a payload state an acknowledged witness moves, a record that
//! enters or leaves the pending set, a conflict, a combined, displaced or recovery store, a
//! custody change, and any refusal on the way discard what is kept, and the store is opened
//! afresh, whose own judgment, refusal included, is the answer. What a reopen does not read again
//! is the kept prefix's bytes on disk: the claim's custody chain, the prefix's last record read
//! again, and every [`REVALIDATE`]th reopen, which opens the store afresh instead, stand for that
//! read, as they stand for an observer's read the journal cache answers (`journals`).

use super::{
    Capacity, Log, Region, Scope, Session, Store, StoredWire, WITNESS_AT, group, journals, replay,
};
use crate::{
    Error, Result,
    table::{Class, Verdict, signatures::Identified},
};
use astream_wire::sync::Digest;
use std::{
    collections::{BTreeMap, btree_map::Entry},
    path::Path,
};

/// The verdict of each class a kept store's records were classified under.
pub(crate) type Classes = BTreeMap<Class, Verdict>;

/// Of each run of reopens of a kept store, the one after this many opens the store afresh, as an
/// open after any change does: a read of its whole journal stands for every reopen before it.
const REVALIDATE: u32 = 64;

/// A primary table store kept across sessions, with what its records were classified under.
pub(crate) struct Kept {
    store: Store,
    /// The acknowledged witness count its records were classified under: a record at or past it
    /// was classified as an unwitnessed tail, or appended since, and is classified again.
    acknowledged: u64,
    classes: Classes,
    /// Reopens since the store was opened afresh.
    reopens: u32,
}

impl std::ops::Deref for Kept {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.store
    }
}

impl std::ops::DerefMut for Kept {
    fn deref_mut(&mut self) -> &mut Store {
        &mut self.store
    }
}

impl Kept {
    /// The primary store of `scope` at `dir`, opened in `session` as [`Store::open`] opens it
    /// (writing where the session holds the control writer, else observing): `kept` reopened
    /// where it is that store and its classification stands, else opened afresh.
    ///
    /// # Errors
    /// As [`Store::open`]: a reopen that refuses, or does not stand, is never the answer.
    pub(crate) fn open(
        session: &Session<'_>,
        dir: &Path,
        scope: Scope,
        kept: Option<Self>,
    ) -> Result<Self> {
        let read_only = !session.has_control_writer();
        if let Some(kept) = kept
            && kept.store.data_dir == dir
            && kept.store.scope == scope
            && kept.reopens < REVALIDATE
            && let Ok(Some(reopened)) = kept.reopen(session, read_only)
        {
            return Ok(reopened);
        }
        let claimed = Store::claimed(
            session,
            dir,
            scope,
            Capacity::default(),
            read_only,
            Region::Primary,
        )?;
        let acknowledged = super::Witness::decode(&claimed.claim[WITNESS_AT..])?.count;
        let mut classes = Classes::new();
        let mut store = Store::open_claimed_classified(
            session,
            dir,
            scope,
            false,
            claimed,
            Some(&mut classes),
        )?;
        if !read_only {
            store.seal_held(session)?;
        }
        Ok(Self {
            store,
            acknowledged,
            classes,
            reopens: 0,
        })
    }

    /// This store, its journal let go, to keep across network I/O: none for one whose append is
    /// uncertain, or one a reopen cannot continue (a combined, displaced or recovery store, or
    /// one holding conflicts, whose certificates' judgments are not a class's).
    pub(crate) fn keep(mut self) -> Option<Self> {
        let store = &self.store;
        if store.poisoned
            || store.log.as_ref().is_some_and(Log::is_poisoned)
            || store.observed_primary.is_some()
            || store.displaced.is_some()
            || store.region != Region::Primary
            || !store.conflicts.is_empty()
        {
            return None;
        }
        self.store.log = None;
        Some(self)
    }

    /// Whether this kept store is the one at `dir`.
    pub(crate) fn is_at(&self, dir: &Path) -> bool {
        self.store.data_dir == dir
    }

    /// The folder of this kept store's data.
    pub(crate) fn data_dir(&self) -> &Path {
        &self.store.data_dir
    }

    /// This store reopened in `session` ([`Self::open`]): `None` where what is kept does not
    /// stand, and the caller opens afresh.
    ///
    /// # Errors
    /// A judgment refuses; the caller opens afresh, whose refusal is the answer.
    fn reopen(self, session: &Session<'_>, read_only: bool) -> Result<Option<Self>> {
        let Self {
            mut store,
            acknowledged: classified,
            mut classes,
            reopens,
        } = self;
        let dir = store.data_dir.clone();
        let scope = store.scope;
        let capacity = store.capacity;
        let claimed = Store::claimed(session, &dir, scope, capacity, read_only, store.region)?;
        if claimed.pending || claimed.displaced.is_some() || store.actor != session.actor() {
            return Ok(None);
        }
        let super::Claimed {
            sources,
            claim_dir,
            claim_name,
            mut claim,
            ..
        } = claimed;
        claim[4] = b'1';
        if claim[..WITNESS_AT] != store.claim[..WITNESS_AT] {
            return Ok(None);
        }
        let acknowledged = super::Witness::decode(&claim[WITNESS_AT..])?;
        let held = store.witness;
        if acknowledged.count < held.count {
            return Ok(None);
        }
        let boundary = group::read(&claim_dir, &claim_name, &claim)?;
        let (log, tail, permitted) = if read_only {
            let journal = journals::read(&dir, &claim[..super::ANCHOR_LEN], capacity.journal())?;
            let Some(tail) = journal.witnessed_after(boundary, held)? else {
                return Ok(None);
            };
            (None, tail, false)
        } else {
            let mut log = Log::open(
                &dir,
                "table",
                &claim[..super::ANCHOR_LEN],
                true,
                capacity.journal(),
            )
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    Error::busy()
                } else {
                    error.into()
                }
            })?;
            let before = held.count.saturating_sub(1);
            let tail =
                replay::read_after(&mut log, held, boundary, |id, received_ms, conflict| {
                    !conflict
                        && store.records.get(id).is_some_and(|stored| {
                            stored.receipt.offset == before
                                && stored.receipt.received_ms == received_ms
                        })
                })?;
            (Some(log), tail, boundary.end().is_some())
        };
        #[cfg(test)]
        super::replay_observed();
        if tail.entries.iter().any(|entry| entry.conflict) {
            return Ok(None);
        }
        let adopted: Vec<_> = tail
            .entries
            .iter()
            .filter(|entry| entry.offset >= acknowledged.count)
            .map(|entry| (entry.offset, entry.conflict, entry.wire.clone()))
            .collect();
        let witness = tail.witness;
        let catalog = scope == crate::catalog::scope(scope.trust());
        let stands = sources.with_read_batch(scope, |batch| {
            for (class, verdict) in &classes {
                if !batch.class_verdict(class)?.same(verdict) {
                    return Ok(false);
                }
            }
            if !read_only && classes.values().any(Verdict::awaiting) {
                return Ok(false);
            }
            // The records classified at or past the witness they were classified under, as an
            // open classifies them under the witness the claim acknowledges now.
            let recent: Vec<Digest> = store
                .records
                .iter()
                .filter(|(_, stored)| stored.receipt.offset >= classified)
                .map(|(id, _)| *id)
                .collect();
            for id in recent {
                let stored = &store.records[&id];
                let wire = match &stored.wire {
                    StoredWire::Shared(wire) => wire.clone(),
                    StoredWire::Owned(wire) => Identified::new(wire.clone()),
                };
                let (fresh, class) = replay::restore(
                    batch,
                    stored.receipt,
                    wire,
                    acknowledged.count,
                    catalog,
                    read_only,
                )?;
                if fresh.payload.awaiting_membership() != stored.payload.awaiting_membership() {
                    return Ok(false);
                }
                if let Entry::Vacant(slot) = classes.entry(class) {
                    slot.insert(batch.class_verdict(&class)?);
                }
                let held = store
                    .records
                    .get_mut(&id)
                    .ok_or_else(|| Error::new("kept table record disappeared"))?;
                held.wire = fresh.wire;
                held.payload = fresh.payload;
                held.cut = fresh.cut;
                held.checkpoint = fresh.checkpoint;
            }
            // The journal's records after the kept ones, as an open replays them.
            for entry in tail.entries {
                let receipt = entry.receipt();
                let (stored, class) = replay::restore(
                    batch,
                    receipt,
                    entry.wire,
                    acknowledged.count,
                    catalog,
                    read_only,
                )?;
                if store.records.contains_key(&receipt.id) {
                    return Err(Error::new("duplicate physical table record"));
                }
                // The kept store's floor is the one its open read; a floor only moves up, so an
                // older one counts more records against the quotas and covers fewer kept
                // payloads, never fewer and more (LT-B10C1, LT-B10C3).
                store.admit(
                    &stored.header,
                    stored.wire.len(),
                    stored.checkpoint.is_some(),
                )?;
                if !stored.payload.awaiting_membership() {
                    store
                        .index()?
                        .check_insert(&stored.header)
                        .map_err(|_| Error::new("invalid table replay header"))?;
                    store
                        .index
                        .get_mut()
                        .ok_or_else(|| Error::new("table index unavailable after replay"))?
                        .insert(stored.header.clone())
                        .map_err(|_| Error::new("invalid table replay header"))?;
                }
                store.operation_bytes += stored.wire.len();
                store.hold(receipt.id, stored);
                if let Entry::Vacant(slot) = classes.entry(class) {
                    slot.insert(batch.class_verdict(&class)?);
                }
            }
            Ok(true)
        })?;
        if !stands {
            return Ok(None);
        }
        store.claim = claim;
        store.claim_dir = claim_dir;
        store.claim_name = claim_name;
        store.witness = witness;
        store.log = log;
        store.observation = session.access_observation();
        store.finish_open(session, false, acknowledged, &adopted, permitted)?;
        #[cfg(test)]
        super::OPENS_AND_CHECKS.with(|counts| counts.set((counts.get().0 + 1, counts.get().1)));
        store.check(session)?;
        if !read_only {
            store.seal_held(session)?;
        }
        Ok(Some(Self {
            store,
            acknowledged: acknowledged.count,
            classes,
            reopens: reopens + 1,
        }))
    }
}
