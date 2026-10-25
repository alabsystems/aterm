// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ciphertext-only table persistence. The independently stored enrollment claim
//! prevents loss of the data directory from silently resetting a writer. A full
//! rollback of both custody and data needs an external witness to detect.

use super::{
    Record, Scope, Session,
    conflict::{Conflict, MAX_CONFLICT},
};
use crate::{
    Error, Result,
    crypto::{self, Causal},
};
use astream_wire::sync::{Actor, Digest, Header, Index, MAX_PARENTS, Projection};
use aterm_journal::{Limits, Log};
use aterm_secrets::{
    commons::{constraints::Fact, roster::Role},
    fsx,
};
use aterm_uds::private_file::{self, PrivateFileError};
use std::{
    collections::{BTreeMap, BTreeSet},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

mod replay;
mod witness;
use witness::Witness;

const MAX_RECORDS: usize = 8192;
const MAX_BYTES: usize = 128 * 1024 * 1024;
const MAX_CONFLICTS: usize = aterm_secrets::commons::constraints::MAX_BINDINGS;
const JOURNAL_RECORDS: usize = MAX_RECORDS + MAX_CONFLICTS;

#[derive(Clone, Copy)]
struct Capacity {
    operations: usize,
    bytes: usize,
    conflicts: usize,
}
impl Default for Capacity {
    fn default() -> Self {
        Self {
            operations: MAX_RECORDS,
            bytes: MAX_BYTES,
            conflicts: MAX_CONFLICTS,
        }
    }
}
impl Capacity {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableConflict",
            action = "Capacity",
            project = "aterm_db::table::store::replay::tests::conflict::conflict_transition"
        )
    )]
    fn journal(self) -> Limits {
        Limits {
            records: self.operations + self.conflicts,
            bytes: self.bytes
                + self.operations * charge(SUBJECT, 0)
                + self.conflicts * charge(CONFLICT_SUBJECT, MAX_CONFLICT),
            record_bytes: MAX_CONFLICT + 8,
        }
    }
}
fn charge(subject: &str, wire: usize) -> usize {
    wire.saturating_add(8 + subject.len() + aterm_journal::APPEND_OVERHEAD)
}
const WRITER_RECORDS: usize = 4096;
const WRITER_BYTES: usize = 8 * 1024 * 1024;
const SUBJECT: &str = "commons/table/1";
const CONFLICT_SUBJECT: &str = "commons/table-conflict/1";
const ANCHOR_LEN: usize = 5 + super::SCOPE_LEN + 48 + 16;
const WITNESS_AT: usize = ANCHOR_LEN + 16;
const CLAIM_LEN: usize = WITNESS_AT + 40;

/// First local durable observation; neither field orders distributed history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub id: Digest,
    pub received_ms: u64,
    pub offset: u64,
}
struct Stored {
    wire: Vec<u8>,
    header: Header,
    receipt: Receipt,
    quarantine: Option<(Digest, u64, u64)>,
}

struct StoredConflict {
    conflict: Conflict,
    receipt: Receipt,
}

/// Previously witnessed ciphertext whose source was discarded by control
/// recovery. Retained custody is not a current grant or a spendable balance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quarantined {
    pub header: Header,
    pub receipt: Receipt,
    pub roster: Digest,
    pub epoch: u64,
    /// Signed author clock claim, never an ordering or permission input.
    pub authored_ms: u64,
}

/// One exclusive local replica of one immutable table generation. The index
/// retains authenticated records and previously witnessed quarantined headers.
/// Plaintext is decrypted per read. Reopen after a control change; open for a
/// short local batch, then drop the store and session before network I/O.
pub struct Store {
    scope: Scope,
    actor: Actor,
    data_dir: PathBuf,
    claim_dir: PathBuf,
    claim_name: String,
    claim: Vec<u8>,
    log: Log,
    witness: Witness,
    poisoned: bool,
    capacity: Capacity,
    operation_bytes: usize,
    index: Index,
    records: BTreeMap<Digest, Stored>,
    conflicts: BTreeMap<Actor, StoredConflict>,
    observation: [u8; 64],
}
impl Store {
    /// Create a fresh table identity and enroll this replica before any write.
    pub fn create(session: &Session<'_>, dir: &Path) -> Result<Self> {
        Self::join(session, dir, session.scope()?)
    }
    /// Enroll once. The caller obtains the scope from authenticated table
    /// discovery. Repeated calls reopen the existing replica; they never reset it.
    /// An interrupted, still-empty first initialization is resumable.
    pub fn join(session: &Session<'_>, dir: &Path, scope: Scope) -> Result<Self> {
        session.context(scope)?;
        let claim_dir = claims(session)?;
        aterm_journal::create_private_dir_durable(&claim_dir)?;
        let claim_name = format!("{}.claim", aterm_codec::hex::encode_lower(&scope.digest()));
        if matches!(
            private_file::read_private(&claim_dir, &claim_name, CLAIM_LEN),
            Err(PrivateFileError::Missing)
        ) {
            let mut claim = b"ATSC0".to_vec();
            claim.extend_from_slice(&scope.bytes());
            claim.extend_from_slice(&session.actor());
            claim.extend_from_slice(&crypto::random::<16>()?);
            claim.extend_from_slice(&[0; 56]);
            fsx::write_new(&claim_dir.join(&claim_name), &claim, 0o600).map_err(super::failure)?;
            // write_new's best-effort directory barrier is insufficient for this
            // enrollment: publishing a writer depends on its name surviving.
            aterm_durable::durable_dir(&claim_dir)?;
        }
        Self::open_mode(session, dir, scope, true)
    }
    /// Reopen with the independently retained ready enrollment claim. Missing
    /// custody, anchor, checkpoint or acknowledged bytes is damage, not a join.
    pub fn open(session: &Session<'_>, dir: &Path, scope: Scope) -> Result<Self> {
        Self::open_mode(session, dir, scope, false)
    }
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableDurability",
            action = "Restart",
            project = "aterm_db::table::tests::storage::storage_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableDurability",
            action = "Truncate",
            project = "aterm_db::table::tests::storage::storage_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableDurability",
            action = "LostLog",
            project = "aterm_db::table::tests::storage::storage_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableDurability",
            action = "LostClaim",
            project = "aterm_db::table::tests::storage::storage_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableDurability",
            action = "ChangedScope",
            project = "aterm_db::table::tests::storage::storage_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableDurability",
            action = "TornSuffix",
            project = "aterm_db::table::tests::storage::storage_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableDurability",
            action = "UncertainAppend",
            project = "aterm_db::table::tests::storage::storage_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableDurability",
            action = "CopiedStore",
            project = "aterm_db::table::tests::storage::storage_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableDurability",
            action = "RestoreBackup",
            project = "aterm_db::table::tests::storage::storage_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableConflict",
            action = "Restart",
            project = "aterm_db::table::store::replay::tests::conflict::conflict_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableConflict",
            action = "Tail",
            project = "aterm_db::table::store::replay::tests::conflict::conflict_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableConflict",
            action = "Rollback",
            project = "aterm_db::table::store::replay::tests::conflict::conflict_transition"
        )
    )]
    fn open_mode(
        session: &Session<'_>,
        dir: &Path,
        scope: Scope,
        initialize: bool,
    ) -> Result<Self> {
        Self::open_capacity(session, dir, scope, initialize, Capacity::default())
    }
    fn open_capacity(
        session: &Session<'_>,
        dir: &Path,
        scope: Scope,
        initialize: bool,
        capacity: Capacity,
    ) -> Result<Self> {
        if capacity.operations > MAX_RECORDS
            || capacity.bytes > MAX_BYTES
            || capacity.conflicts > MAX_CONFLICTS
        {
            return Err(Error::new("table capacity exceeds custody bounds"));
        }
        session.context(scope)?;
        let claim_dir = claims(session)?;
        let claim_name = format!("{}.claim", aterm_codec::hex::encode_lower(&scope.digest()));
        let mut claim = private_file::read_private(&claim_dir, &claim_name, CLAIM_LEN)
            .map_err(|_| Error::new("table enrollment claim is missing or damaged"))?;
        if claim.len() != CLAIM_LEN
            || &claim[..4] != b"ATSC"
            || !matches!(claim[4], b'0' | b'1')
            || claim[5..5 + super::SCOPE_LEN] != scope.bytes()
            || claim[5 + super::SCOPE_LEN..5 + super::SCOPE_LEN + 48] != session.actor()
        {
            return Err(Error::new(
                "table enrollment claim has another scope or writer",
            ));
        }
        let pending = claim[4] == b'0';
        if pending && claim[ANCHOR_LEN..] != [0; 56] {
            return Err(Error::new(
                "pending table enrollment carries committed state",
            ));
        }
        if !pending && claim[ANCHOR_LEN..WITNESS_AT] != directory_identity(dir)? {
            return Err(Error::new(
                "table directory was replaced or copied; explicit recovery required",
            ));
        }
        if pending && !initialize {
            return Err(Error::new("table enrollment is not yet complete"));
        }
        // The stable token binds the data directory even while the claim changes
        // from pending to ready. No record can be authored in the pending state.
        claim[4] = b'1';
        let log = Log::open(
            dir,
            "table",
            &claim[..ANCHOR_LEN],
            !pending,
            capacity.journal(),
        )?;
        let acknowledged = Witness::decode(&claim[WITNESS_AT..])?;
        let mut store = Self {
            scope,
            actor: session.actor(),
            data_dir: dir.into(),
            claim_dir,
            claim_name,
            claim,
            log,
            witness: Witness::default(),
            poisoned: false,
            capacity,
            operation_bytes: 0,
            index: Index::new(MAX_RECORDS),
            records: BTreeMap::new(),
            conflicts: BTreeMap::new(),
            observation: session.observation()?,
        };
        // The complete acknowledged prefix is verified before any opaque
        // record is considered for quarantine. The tail has no such authority.
        let replay = replay::read(&mut store.log, acknowledged, pending)?;
        store.witness = replay.witness;
        for entry in replay.entries {
            let replay::Entry {
                receipt,
                wire,
                conflict,
            } = entry;
            if conflict {
                let conflict = Conflict::authenticate(session, scope, &wire)?;
                if store.conflicts.len() >= store.capacity.conflicts {
                    return Err(Error::new("table conflict capacity exhausted"));
                }
                if store
                    .conflicts
                    .insert(conflict.actor(), StoredConflict { conflict, receipt })
                    .is_some()
                {
                    return Err(Error::new("duplicate physical table conflict"));
                }
            } else {
                let stored = replay::restore(session, scope, receipt, wire, acknowledged.count)?;
                store.admit(&stored.header, stored.wire.len())?;
                store.operation_bytes += stored.wire.len();
                store
                    .index
                    .insert(stored.header.clone())
                    .map_err(|_| Error::new("invalid table replay header"))?;
                if store.records.insert(receipt.id, stored).is_some() {
                    return Err(Error::new("duplicate physical table record"));
                }
            }
        }
        store.log.finish_open()?;
        if pending || store.witness != acknowledged {
            store.claim[ANCHOR_LEN..WITNESS_AT].copy_from_slice(&directory_identity(dir)?);
            store.publish_witness(store.witness)?;
        }
        // Complete a possible earlier rename whose directory barrier failed.
        aterm_durable::durable_dir(&store.claim_dir)?;
        store.check(session)?;
        Ok(store)
    }
    pub const fn scope(&self) -> Scope {
        self.scope
    }
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableQuarantine",
            action = "ChangedObservation",
            project = "aterm_db::table::store::replay::tests::quarantine_transition"
        )
    )]
    fn check(&self, session: &Session<'_>) -> Result<()> {
        session.context(self.scope)?;
        if self.actor != session.actor()
            || self.observation != session.observation()?
            || self.claim[ANCHOR_LEN..WITNESS_AT] != directory_identity(&self.data_dir)?
            || self.poisoned
            || self.log.is_poisoned()
            || private_file::read_private(&self.claim_dir, &self.claim_name, CLAIM_LEN)
                .ok()
                .as_deref()
                != Some(&self.claim)
        {
            return Err(Error::new(
                "table custody changed or append is uncertain; reopen",
            ));
        }
        Ok(())
    }
    fn admit(&self, header: &Header, bytes: usize) -> Result<()> {
        if self.records.len() >= self.capacity.operations
            || self.operation_bytes.saturating_add(bytes) > self.capacity.bytes
        {
            return Err(Error::new(
                "table operation capacity exhausted; conflict space is reserved",
            ));
        }
        let mut count = 0;
        let mut size = bytes;
        for stored in self
            .records
            .values()
            .filter(|r| r.header.actor == header.actor)
        {
            count += 1;
            size = size.saturating_add(stored.wire.len());
        }
        if count >= WRITER_RECORDS || size > WRITER_BYTES {
            return Err(Error::new("table writer evidence quota exhausted"));
        }
        Ok(())
    }
    /// Retain authenticated evidence even when current catalog policy excludes
    /// it. Peers may acknowledge only the returned durable receipt.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableQuarantine",
            action = "RemoteDiscarded",
            project = "aterm_db::table::store::replay::tests::quarantine_transition"
        )
    )]
    pub fn ingest(&mut self, session: &Session<'_>, wire: &[u8]) -> Result<Receipt> {
        self.check(session)?;
        let record = session.authenticate(self.scope, wire)?;
        let id = record.header().id;
        if let Some(stored) = self.records.get(&id) {
            if stored.wire != wire {
                return Err(Error::new("different bytes for a retained table digest"));
            }
            return Ok(stored.receipt);
        }
        if self.conflicted().contains(&record.header().actor) {
            return Err(Error::new("table actor has contradictory signed history"));
        }
        self.admit(record.header(), wire.len())?;
        let mut proposed = self.index.clone();
        proposed
            .insert(record.header().clone())
            .map_err(|_| Error::new("table index refused header or capacity"))?;
        let receipt = self.append(SUBJECT, wire, id)?;
        self.operation_bytes += wire.len();
        self.index = proposed;
        self.records.insert(
            id,
            Stored {
                wire: wire.into(),
                header: record.header().clone(),
                receipt,
                quarantine: None,
            },
        );
        self.poisoned = false;
        Ok(receipt)
    }
    // Publish the independently retained witness before returning any receipt.
    fn append(&mut self, subject: &str, wire: &[u8], id: Digest) -> Result<Receipt> {
        let received_ms =
            aterm_time::unix_millis().ok_or_else(|| Error::new("receipt clock unavailable"))?;
        let mut body = received_ms.to_le_bytes().to_vec();
        body.extend_from_slice(wire);
        let witness = self.witness.advance(self.witness.count, id, received_ms)?;
        self.poisoned = true;
        let offset = self.log.append(subject, body)?.0;
        if offset != self.witness.count {
            return Err(Error::new("table log and custody witness diverged"));
        }
        self.publish_witness(witness)?;
        Ok(Receipt {
            id,
            received_ms,
            offset,
        })
    }

    /// Retain negative-only signed evidence independently of operation admission.
    /// An exact retry returns the original durable receipt. One certificate per
    /// actor bounds permanent evidence; alternative certificates cannot grow it.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableConflict",
            action = "Receive",
            project = "aterm_db::table::store::replay::tests::conflict::conflict_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableConflict",
            action = "KnownOperation",
            project = "aterm_db::table::store::replay::tests::conflict::conflict_transition"
        )
    )]
    pub fn ingest_conflict(&mut self, session: &Session<'_>, wire: &[u8]) -> Result<Receipt> {
        self.check(session)?;
        let conflict = Conflict::authenticate(session, self.scope, wire)?;
        if let Some(stored) = self.conflicts.get(&conflict.actor()) {
            if stored.conflict.wire() != wire {
                return Err(Error::new("table actor already has a conflict certificate"));
            }
            return Ok(stored.receipt);
        }
        if self.conflicts.len() >= self.capacity.conflicts {
            return Err(Error::new("table conflict capacity exhausted"));
        }
        let receipt = self.append(CONFLICT_SUBJECT, wire, conflict.id())?;
        self.conflicts
            .insert(conflict.actor(), StoredConflict { conflict, receipt });
        self.poisoned = false;
        Ok(receipt)
    }

    fn conflicted(&self) -> BTreeSet<Actor> {
        self.index
            .forks()
            .into_keys()
            .chain(self.conflicts.keys().copied())
            .collect()
    }

    /// Identities restricted by native forks or separately retained certificates.
    /// This inventory is distinct from the positive operation inventory.
    pub fn conflict_inventory(&self, session: &Session<'_>) -> Result<BTreeSet<Actor>> {
        self.check(session)?;
        Ok(self.conflicted())
    }

    /// Export signed negative evidence without granting either source operation
    /// admission. Native witnesses are derived only from durable retained bytes.
    pub fn conflict_wire(&self, session: &Session<'_>, actor: &Actor) -> Result<Option<Vec<u8>>> {
        self.check(session)?;
        if let Some(stored) = self.conflicts.get(actor) {
            return Ok(Some(stored.conflict.wire().to_vec()));
        }
        self.index
            .forks()
            .get(actor)
            .map(|fork| {
                Conflict::from_pair(
                    session,
                    self.scope,
                    &self.records[&fork.first].wire,
                    &self.records[&fork.second].wire,
                )
                .map(|conflict| conflict.wire().to_vec())
            })
            .transpose()
    }

    fn publish_witness(&mut self, witness: Witness) -> Result<()> {
        self.claim[WITNESS_AT..].copy_from_slice(&witness.bytes());
        if let Err(error) =
            aterm_journal::write_private_durable(&self.claim_dir, &self.claim_name, &self.claim)
        {
            self.poisoned = true;
            return Err(error.into());
        }
        self.witness = witness;
        Ok(())
    }
    /// Author catalog data from the exact retained writer chain, persisting it
    /// before returning its identity. Budget debits need a separate admission
    /// policy and must never use catalog visibility as their liability ledger.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableDurability",
            action = "OwnFork",
            project = "aterm_db::table::tests::storage::storage_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableQuarantine",
            action = "SelectedDependent",
            project = "aterm_db::table::store::replay::tests::quarantine_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableConflict",
            action = "OwnReset",
            project = "aterm_db::table::store::replay::tests::conflict::conflict_transition"
        )
    )]
    pub fn commit_catalog(&mut self, session: &Session<'_>, payload: &[u8]) -> Result<Receipt> {
        self.check(session)?;
        session.writer()?;
        if self.conflicted().contains(&self.actor) {
            return Err(Error::new(
                "local table writer has contradictory signed history",
            ));
        }
        let projection = self.catalog_projection(session)?;
        let previous = projection
            .ordered
            .iter()
            .filter_map(|id| self.index.get(id))
            .filter(|header| header.actor == self.actor)
            .max_by_key(|header| header.sequence);
        let sequence = previous
            .map_or(Some(1), |h| h.sequence.checked_add(1))
            .ok_or_else(|| Error::new("table writer sequence exhausted"))?;
        let own = self
            .records
            .values()
            .filter(|record| record.header.actor == self.actor);
        let visible: BTreeSet<_> = projection.ordered.iter().copied().collect();
        if own
            .clone()
            .any(|record| !visible.contains(&record.header.id))
            || own.count() as u64 != sequence - 1
        {
            return Err(Error::new(
                "local table writer has withheld or forked history; repair required",
            ));
        }
        if session.journal.constraints().map_err(super::failure)?.facts().any(|fact|
            matches!(fact, Fact::Cutoff { table, actor, .. } if *table == self.scope.digest() && *actor == self.actor)) {
            return Err(Error::new("local catalog writer is cut off in this table"));
        }
        let previous = previous.map(|h| h.id);
        let mut parents = projection.heads;
        if let Some(id) = previous {
            parents.insert(id);
        }
        if parents.len() > MAX_PARENTS {
            return Err(Error::new(
                "table frontier requires a bounded merge before writing",
            ));
        }
        let parents: Vec<_> = parents.into_iter().collect();
        // No sealed bytes escape before the local acknowledgement. After a crash
        // an unacknowledged suffix is archived, so reuse cannot publish two forks.
        let wire = session.seal(
            self.scope,
            Causal {
                sequence,
                previous,
                parents: &parents,
            },
            payload,
        )?;
        self.ingest(session, &wire)
    }
    fn catalog_projection(&self, session: &Session<'_>) -> Result<Projection> {
        self.check(session)?;
        // Peers cannot admit quarantine, so it and its withheld dependents do
        // not form transferable cutoff evidence. Exclude those local-only
        // frontiers; the full index below still retains restrictive forks.
        let index = self.replication_index()?;
        let constraints = session.journal.constraints().map_err(super::failure)?;
        let table = self.scope.digest();
        let allowed = constraints.catalog(table, &index);
        let writers: BTreeSet<_> = session
            .access
            .members()
            .iter()
            .filter(|m| m.role() == Role::Writer)
            .map(|m| m.binding().actor())
            .collect();
        let cutoffs: BTreeSet<_> = constraints
            .facts()
            .filter_map(|fact| match fact {
                Fact::Cutoff {
                    table: t, actor, ..
                } if *t == table => Some(*actor),
                _ => None,
            })
            .collect();
        let withheld = self.quarantine_closure();
        let excluded = self
            .records
            .iter()
            .filter(|(id, stored)| {
                withheld.contains(*id)
                    || !allowed.permits(id)
                    || stored.quarantine.is_some()
                    || (!writers.contains(&stored.header.actor)
                        && !cutoffs.contains(&stored.header.actor))
            })
            .map(|(id, _)| *id)
            .collect();
        // Keep all witnessed fork evidence restrictive. Filtering the cutoff
        // input must not make a conflicting selected writer valid again.
        Ok(self.index.project_excluding(&excluded))
    }
    /// Revalidate current membership and all table-specific cutoffs, including
    /// causal dependents. Old keys and author timestamps cannot bypass demotion.
    /// This snapshot is for catalog data only, never outstanding budget liability.
    pub fn catalog(&self, session: &Session<'_>) -> Result<Vec<Record>> {
        self.catalog_projection(session)?
            .ordered
            .iter()
            .map(|id| session.authenticate(self.scope, &self.records[id].wire))
            .collect()
    }
    /// Local operation inventory, including pending, forked and quarantined
    /// records. Certificates have a separate inventory. Neither is a claim of
    /// complete network history.
    pub fn inventory(&self, session: &Session<'_>) -> Result<impl Iterator<Item = &Digest>> {
        self.check(session)?;
        Ok(self.records.keys())
    }
    /// Operations eligible for strict peer ingestion. Quarantined records,
    /// conflicting actors and their causal dependents are withheld; negative
    /// certificates have a separate transfer path.
    pub fn replication_inventory(
        &self,
        session: &Session<'_>,
    ) -> Result<impl Iterator<Item = &Digest>> {
        self.check(session)?;
        let excluded = self.quarantine_closure();
        Ok(self
            .records
            .iter()
            .filter(move |(id, _)| !excluded.contains(*id))
            .map(|(id, _)| id))
    }
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableQuarantineClosure",
            action = "TransitiveFork",
            project = "aterm_db::table::store::replay::tests::closure::closure_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableQuarantineClosure",
            action = "GappedSuffix",
            project = "aterm_db::table::store::replay::tests::closure::closure_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableQuarantineClosure",
            action = "ImpliedFork",
            project = "aterm_db::table::store::replay::tests::closure::closure_transition"
        )
    )]
    fn quarantine_closure(&self) -> BTreeSet<Digest> {
        let mut children: BTreeMap<Digest, Vec<Digest>> = BTreeMap::new();
        let mut writers = BTreeMap::<Actor, BTreeMap<u64, Vec<Digest>>>::new();
        let mut pending: Vec<_> = self
            .conflicts
            .values()
            .flat_map(|stored| stored.conflict.headers().iter().map(|header| header.id))
            .collect();
        let forked = self.conflicted();
        for (id, stored) in &self.records {
            if stored.quarantine.is_some() || forked.contains(&stored.header.actor) {
                pending.push(*id);
            }
            writers
                .entry(stored.header.actor)
                .or_default()
                .entry(stored.header.sequence)
                .or_default()
                .push(*id);
            for parent in &stored.header.parents {
                children.entry(*parent).or_default().push(*id);
            }
        }
        let mut excluded = BTreeSet::new();
        let mut floors = BTreeMap::<Actor, u64>::new();
        while let Some(id) = pending.pop() {
            if excluded.insert(id) {
                pending.extend(children.get(&id).into_iter().flatten().copied());
                let Some(stored) = self.records.get(&id) else {
                    continue;
                };
                let header = &stored.header;
                // Dense sequences above an excluded record either depend on it
                // through a missing link or belong to an unproved competing
                // branch. If any branch is known to fork, retain all its claims
                // together: removing one may otherwise turn another into proof.
                let floor = if forked.contains(&header.actor) {
                    0
                } else {
                    header.sequence
                };
                let old = floors.get(&header.actor).copied();
                if old.is_none_or(|old| floor < old) {
                    let end = old.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
                    pending.extend(
                        writers[&header.actor]
                            .range((std::ops::Bound::Included(floor), end))
                            .flat_map(|(_, ids)| ids.iter().copied()),
                    );
                    floors.insert(header.actor, floor);
                }
            }
        }
        excluded
    }
    fn replication_index(&self) -> Result<Index> {
        let excluded = self.quarantine_closure();
        let mut index = Index::new(MAX_RECORDS);
        for (id, stored) in &self.records {
            if !excluded.contains(id) {
                index
                    .insert(stored.header.clone())
                    .map_err(|_| Error::new("invalid retained table header"))?;
            }
        }
        Ok(index)
    }
    /// Explicit status for retained records excluded by control recovery.
    pub fn quarantined(&self, session: &Session<'_>) -> Result<Vec<Quarantined>> {
        self.check(session)?;
        Ok(self
            .records
            .values()
            .filter_map(|stored| {
                stored
                    .quarantine
                    .map(|(roster, epoch, authored_ms)| Quarantined {
                        header: stored.header.clone(),
                        receipt: stored.receipt,
                        roster,
                        epoch,
                        authored_ms,
                    })
            })
            .collect())
    }
    /// All retained authenticated records, including catalog-withheld debits.
    /// Quarantine, missing causal history or a missing cutoff frontier makes
    /// this view unavailable. Keep allocations reserved until reconciliation
    /// instead of inferring a refund from incomplete or conflicting evidence.
    /// A certificate with no retained operation by its actor adds no local
    /// liability. This view never establishes complete network history.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableQuarantine",
            action = "MissingParent",
            project = "aterm_db::table::store::replay::tests::quarantine_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableQuarantine",
            action = "RepairedParent",
            project = "aterm_db::table::store::replay::tests::quarantine_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableQuarantineClosure",
            action = "ValidFrontier",
            project = "aterm_db::table::store::replay::tests::closure::closure_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableQuarantineClosure",
            action = "WrongActor",
            project = "aterm_db::table::store::replay::tests::closure::closure_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableQuarantineClosure",
            action = "WrongSequence",
            project = "aterm_db::table::store::replay::tests::closure::closure_transition"
        )
    )]
    pub fn liability_evidence(&self, session: &Session<'_>) -> Result<Vec<Record>> {
        self.check(session)?;
        if self
            .records
            .values()
            .any(|stored| stored.quarantine.is_some())
        {
            return Err(Error::new(
                "table liability evidence is quarantined; keep allocations reserved",
            ));
        }
        // A bound reader can contradict itself without ever having written a
        // table operation. It must not freeze unrelated liabilities. Affected
        // retained operations still block interpretation, never disappear.
        let conflicted = self.conflicted();
        if self
            .records
            .values()
            .any(|stored| conflicted.contains(&stored.header.actor))
            || !self.index.project().pending.is_empty()
        {
            return Err(Error::new(
                "table liability graph is incomplete or conflicting; keep allocations reserved",
            ));
        }
        let constraints = session.journal.constraints().map_err(super::failure)?;
        // A complete, non-forking graph has at most one digest per writer dot.
        // Exact frontier matches therefore also reject conflicting cutoffs.
        for fact in constraints.facts() {
            if let Fact::Cutoff {
                table,
                actor,
                through: Some(frontier),
            } = fact
            {
                if *table != self.scope.digest() {
                    continue;
                }
                let matches = self.records.get(&frontier.digest).is_some_and(|stored| {
                    stored.header.actor == *actor && stored.header.sequence == frontier.sequence
                });
                if !matches {
                    return Err(Error::new(
                        "table liability frontier is unavailable or inconsistent; keep allocations reserved",
                    ));
                }
            }
        }
        self.records
            .values()
            .map(|stored| session.authenticate(self.scope, &stored.wire))
            .collect()
    }
    /// Exact acknowledged ciphertext for anti-entropy. Never export prepared bytes.
    pub fn wire(&self, session: &Session<'_>, id: &Digest) -> Result<Option<&[u8]>> {
        self.check(session)?;
        Ok(self.records.get(id).map(|stored| stored.wire.as_slice()))
    }
    pub fn receipt(&self, session: &Session<'_>, id: &Digest) -> Result<Option<Receipt>> {
        self.check(session)?;
        Ok(self.records.get(id).map(|stored| stored.receipt))
    }
}
fn claims(session: &Session<'_>) -> Result<PathBuf> {
    session
        .bootstrap
        .journal_dir()
        .parent()
        .map(|p| p.join("table-claims"))
        .ok_or_else(|| Error::new("Commons custody has no parent"))
}

fn directory_identity(dir: &Path) -> Result<[u8; 16]> {
    let metadata = std::fs::symlink_metadata(dir)?;
    if !metadata.is_dir() {
        return Err(Error::new("table directory is not a directory"));
    }
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&metadata.dev().to_le_bytes());
    bytes[8..].copy_from_slice(&metadata.ino().to_le_bytes());
    Ok(bytes)
}
