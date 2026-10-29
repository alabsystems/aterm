// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ciphertext-only table persistence. The independently stored enrollment claim
//! prevents loss of the data directory from silently resetting a writer. A full
//! rollback of both custody and data needs an external witness to detect.

use super::{
    Record, Scope, Session,
    conflict::{Conflict, MAX_CONFLICT, ProofKey},
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
const DISCARDED_OVERHEAD: usize = 5 + 32 + 4;
const JOURNAL_RECORDS: usize = MAX_RECORDS * 2 + MAX_CONFLICTS;

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
            records: self.operations * 2 + self.conflicts,
            bytes: self.bytes * 2
                + self.operations
                    * (charge(SUBJECT, 0) + charge(CONFLICT_SUBJECT, DISCARDED_OVERHEAD))
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

fn proof_key(actor: Actor, source: Option<Digest>) -> ProofKey {
    let mut key = [0; 81];
    key[..48].copy_from_slice(&actor);
    match source {
        Some(source) => key[49..].copy_from_slice(&source),
        None => key[48] = 1,
    }
    key
}

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

/// Retained evidence for metadata-level restrictions. A decrypted historical
/// record is not permission to apply it: `catalog_eligible` is the current gate.
/// An unavailable payload requires conservative handling of that author's
/// unknown requests. Use `liability_evidence` for finite resource accounting.
pub struct MetadataEvidence {
    pub header: Header,
    /// Direct custody receipt or the negative certificate carrying this source.
    pub receipt: Receipt,
    pub roster: Digest,
    pub epoch: u64,
    pub authored_ms: u64,
    pub record: Option<Record>,
    pub catalog_eligible: bool,
}

/// A local handle for one immutable table generation. Writers are exclusive;
/// observations retain a verified committed snapshot. The index retains
/// authenticated records and previously witnessed quarantined headers.
/// Plaintext is decrypted per read. Reopen after a control change; open for a
/// short local batch, then drop the store and session before network I/O.
pub struct Store {
    scope: Scope,
    actor: Actor,
    data_dir: PathBuf,
    claim_dir: PathBuf,
    claim_name: String,
    claim: Vec<u8>,
    log: Option<Log>,
    witness: Witness,
    poisoned: bool,
    capacity: Capacity,
    operation_bytes: usize,
    index: Index,
    records: BTreeMap<Digest, Stored>,
    conflicts: BTreeMap<ProofKey, StoredConflict>,
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
        if !session.has_control_writer() {
            return Err(Error::new(
                "table enrollment requires a locked control observation",
            ));
        }
        let claim_dir = claim_dir(session)?;
        aterm_journal::create_private_dir_durable(&claim_dir)?;
        let claim_name = claim_name(scope);
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
    /// An observation session reads without acquiring the data journal's writer.
    pub fn open(session: &Session<'_>, dir: &Path, scope: Scope) -> Result<Self> {
        if !session.has_control_writer() {
            return Self::observe(session, dir, scope);
        }
        Self::open_mode(session, dir, scope, false)
    }

    /// Read the independently witnessed committed prefix beside any live writer.
    /// This handle cannot append, repair a tail, publish a witness or enroll storage.
    /// A custody change during the read is refused, so no unchecked snapshot escapes.
    pub fn observe(session: &Session<'_>, dir: &Path, scope: Scope) -> Result<Self> {
        Self::open_capacity_mode(session, dir, scope, false, Capacity::default(), true)
    }

    /// An absent table, an empty discovery reservation, or this replica's first
    /// enrollment cut short after its pending claim (`join`, which resumes it) has
    /// no enrolled storage: a pending claim holds no record, because none is
    /// authored or ingested before `open_capacity_mode` makes the claim ready.
    /// A ready claim or nonempty storage still requires intact established custody.
    pub(crate) fn observe_if_exists(
        session: &Session<'_>,
        dir: &Path,
        scope: Scope,
    ) -> Result<Option<Self>> {
        session.context(scope)?;
        let claim_dir = claim_dir(session)?;
        let claim_name = claim_name(scope);
        match private_file::read_private(&claim_dir, &claim_name, CLAIM_LEN) {
            Ok(claim) if pending_claim(&claim, scope, &session.actor()) => Ok(None),
            Err(PrivateFileError::Missing) => {
                match private_file::judge_private_dir(dir)? {
                    private_file::PrivateDir::Missing => return Ok(None),
                    private_file::PrivateDir::Private
                        if std::fs::read_dir(dir)?.next().transpose()?.is_none() =>
                    {
                        // subscriptions() reserves a slot before Store::join creates
                        // its independent enrollment claim and journal.
                        return Ok(None);
                    }
                    _ => {}
                }
                Self::observe(session, dir, scope).map(Some)
            }
            _ => Self::observe(session, dir, scope).map(Some),
        }
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
        Self::open_capacity_mode(session, dir, scope, initialize, capacity, false)
    }

    fn open_capacity_mode(
        session: &Session<'_>,
        dir: &Path,
        scope: Scope,
        initialize: bool,
        capacity: Capacity,
        read_only: bool,
    ) -> Result<Self> {
        if capacity.operations > MAX_RECORDS
            || capacity.bytes > MAX_BYTES
            || capacity.conflicts > MAX_CONFLICTS
        {
            return Err(Error::new("table capacity exceeds custody bounds"));
        }
        session.context(scope)?;
        let claim_dir = claim_dir(session)?;
        let claim_name = claim_name(scope);
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
        let acknowledged = Witness::decode(&claim[WITNESS_AT..])?;
        let (log, replay) = if read_only {
            let records = match aterm_journal::read_committed(
                dir,
                "table",
                &claim[..ANCHOR_LEN],
                capacity.journal(),
                None,
            )? {
                aterm_journal::Committed::Rebuilt { records, .. } => records,
                _ => {
                    return Err(Error::new(
                        "table observation did not provide its complete prefix",
                    ));
                }
            };
            (None, replay::read_observed(records, acknowledged)?)
        } else {
            let mut log = Log::open(
                dir,
                "table",
                &claim[..ANCHOR_LEN],
                !pending,
                capacity.journal(),
            )
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    Error::busy()
                } else {
                    error.into()
                }
            })?;
            let replay = replay::read(&mut log, acknowledged, pending)?;
            (Some(log), replay)
        };
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
            index: Index::new(MAX_RECORDS * 2),
            records: BTreeMap::new(),
            conflicts: BTreeMap::new(),
            observation: session.observation()?,
        };
        // The complete acknowledged prefix is verified before any opaque
        // record is considered for quarantine. The tail has no such authority.
        store.witness = replay.witness;
        for entry in replay.entries {
            let replay::Entry {
                receipt,
                wire,
                conflict,
            } = entry;
            if conflict {
                let conflict = Conflict::restore(session, scope, &wire)?;
                store.admit_proof(&conflict)?;
                if let Some(source) = conflict.discarded() {
                    store
                        .index
                        .insert(source.header.clone())
                        .map_err(|_| Error::new("invalid discarded-source header"))?;
                }
                if store
                    .conflicts
                    .insert(conflict.key(), StoredConflict { conflict, receipt })
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
        if let Some(log) = &mut store.log {
            log.finish_open()?;
            if pending || store.witness != acknowledged {
                store.claim[ANCHOR_LEN..WITNESS_AT].copy_from_slice(&directory_identity(dir)?);
                store.publish_witness(store.witness)?;
            }
            // Complete a possible earlier rename whose directory barrier failed.
            aterm_durable::durable_dir(&store.claim_dir)?;
        }
        store.check(session)?;
        Ok(store)
    }
    pub const fn scope(&self) -> Scope {
        self.scope
    }
    /// Stable identity of this local enrollment, excluding its advancing
    /// receipt witness. An application pin can detect replacement custody even
    /// when generic discovery has already enrolled an empty table again.
    pub(crate) fn custody_id(&self, session: &Session<'_>) -> Result<Digest> {
        self.check(session)?;
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm/commons/table-custody/1\0");
        hash.update(&self.claim[..WITNESS_AT]);
        Ok(hash.finalize())
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
            || self.log.as_ref().is_some_and(Log::is_poisoned)
        {
            return Err(Error::new(
                "table custody changed or append is uncertain; reopen",
            ));
        }
        let current = private_file::read_private(&self.claim_dir, &self.claim_name, CLAIM_LEN).ok();
        if current.as_deref() == Some(&self.claim) {
            return Ok(());
        }
        // An observer still refuses every changed claim. A same-custody advance
        // can be read afresh; rollback, changed identity and unreadable custody
        // remain hard failures. No unverified new receipt is exposed here.
        if self.log.is_none()
            && current.as_ref().is_some_and(|claim| {
                claim.len() == CLAIM_LEN
                    && claim[..WITNESS_AT] == self.claim[..WITNESS_AT]
                    && Witness::decode(&claim[WITNESS_AT..])
                        .is_ok_and(|witness| witness.count > self.witness.count)
            })
        {
            return Err(Error::snapshot_changed());
        }
        Err(Error::new(
            "table custody changed or append is uncertain; reopen",
        ))
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
        let records = if self.scope == crate::catalog::scope(self.scope.trust()) {
            crate::catalog::RECORDS_PER_CREATOR
        } else {
            WRITER_RECORDS
        };
        if count >= records || size > WRITER_BYTES {
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
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbSourceReselection",
            action = "Admit",
            project = "aterm_db::table::store::replay::tests::conflict::reselection_transition"
        )
    )]
    pub fn ingest(&mut self, session: &Session<'_>, wire: &[u8]) -> Result<Receipt> {
        self.check(session)?;
        let record = session.authenticate(self.scope, wire)?;
        operation_policy(self.scope, record.header(), record.payload().len())?;
        let id = record.header().id;
        if let Some(stored) = self.records.get(&id) {
            if stored.wire != wire {
                return Err(Error::new("different bytes for a retained table digest"));
            }
            return Ok(stored.receipt);
        }
        if self.conflicted().contains(&record.header().actor) {
            return Err(Error::new("table actor has restricted signed history"));
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
        let log = self
            .log
            .as_mut()
            .ok_or_else(|| Error::new("read-only table observation cannot append"))?;
        let received_ms =
            aterm_time::unix_millis().ok_or_else(|| Error::new("receipt clock unavailable"))?;
        let mut body = received_ms.to_le_bytes().to_vec();
        body.extend_from_slice(wire);
        let witness = self.witness.advance(self.witness.count, id, received_ms)?;
        self.poisoned = true;
        let offset = log.append(subject, body)?.0;
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
    /// An exact retry returns its durable receipt. Forks coalesce by actor;
    /// discarded sources coalesce by source identity under separate quotas.
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
        let key = conflict.key();
        if let Some(stored) = self.conflicts.get(&key) {
            if stored.conflict.wire() != wire {
                return Err(Error::new("table actor already has a conflict certificate"));
            }
            return Ok(stored.receipt);
        }
        self.admit_proof(&conflict)?;
        let mut index = self.index.clone();
        if let Some(source) = conflict.discarded() {
            index
                .insert(source.header.clone())
                .map_err(|_| Error::new("invalid discarded-source header"))?;
        }
        let receipt = self.append(CONFLICT_SUBJECT, wire, conflict.id())?;
        self.conflicts
            .insert(key, StoredConflict { conflict, receipt });
        self.index = index;
        self.poisoned = false;
        Ok(receipt)
    }

    fn conflicted(&self) -> BTreeSet<Actor> {
        self.index
            .forks()
            .into_keys()
            .chain(
                self.conflicts
                    .values()
                    .filter(|stored| stored.conflict.discarded().is_none())
                    .map(|stored| stored.conflict.actor()),
            )
            .collect()
    }

    fn quarantined_actors(&self) -> BTreeSet<Actor> {
        self.records
            .values()
            .filter(|record| record.quarantine.is_some())
            .map(|record| record.header.actor)
            .chain(
                self.conflicts
                    .values()
                    .filter(|stored| {
                        stored.conflict.active() && stored.conflict.discarded().is_some()
                    })
                    .map(|stored| stored.conflict.actor()),
            )
            .collect()
    }

    fn admit_proof(&self, proof: &Conflict) -> Result<()> {
        if proof.discarded().is_none() {
            if self
                .conflicts
                .values()
                .filter(|stored| stored.conflict.discarded().is_none())
                .count()
                >= self.capacity.conflicts
            {
                return Err(Error::new("table fork evidence capacity exhausted"));
            }
            return Ok(());
        }
        let sources: Vec<_> = self
            .conflicts
            .values()
            .filter(|stored| stored.conflict.discarded().is_some())
            .collect();
        let own: Vec<_> = sources
            .iter()
            .filter(|stored| stored.conflict.actor() == proof.actor())
            .collect();
        if sources.len() >= self.capacity.operations
            || sources
                .iter()
                .map(|stored| stored.conflict.wire().len())
                .sum::<usize>()
                .saturating_add(proof.wire().len())
                > self.capacity.bytes + self.capacity.operations * DISCARDED_OVERHEAD
            || own.len() >= WRITER_RECORDS
            || own
                .iter()
                .map(|stored| stored.conflict.wire().len())
                .sum::<usize>()
                .saturating_add(proof.wire().len())
                > WRITER_BYTES + WRITER_RECORDS * DISCARDED_OVERHEAD
        {
            return Err(Error::new(
                "table discarded-source evidence capacity exhausted",
            ));
        }
        Ok(())
    }

    /// Identities restricted by native forks or separately retained certificates.
    /// This inventory is distinct from the positive operation inventory.
    pub fn conflict_inventory(&self, session: &Session<'_>) -> Result<BTreeSet<Actor>> {
        self.check(session)?;
        Ok(self
            .conflicted()
            .into_iter()
            .chain(self.quarantined_actors())
            .collect())
    }

    /// Proof identity preserves every discarded source and lets a later fork
    /// supersede quarantine. Knowing one source is not knowing the whole actor.
    pub fn proof_inventory(&self, session: &Session<'_>) -> Result<BTreeSet<ProofKey>> {
        self.check(session)?;
        let forks = self.conflicted();
        let mut keys: BTreeSet<_> = forks.iter().map(|actor| proof_key(*actor, None)).collect();
        keys.extend(
            self.conflicts
                .values()
                .filter(|stored| {
                    stored.conflict.active()
                        && stored.conflict.discarded().is_some()
                        && !forks.contains(&stored.conflict.actor())
                })
                .map(|stored| stored.conflict.key()),
        );
        keys.extend(
            self.records
                .values()
                .filter(|record| {
                    record.quarantine.is_some() && !forks.contains(&record.header.actor)
                })
                .map(|record| proof_key(record.header.actor, Some(record.header.id))),
        );
        Ok(keys)
    }

    /// Export signed negative evidence without granting either source operation
    /// admission. Native witnesses are derived only from durable retained bytes.
    pub fn conflict_wire(&self, session: &Session<'_>, actor: &Actor) -> Result<Option<Vec<u8>>> {
        self.check(session)?;
        let key = self
            .proof_inventory(session)?
            .into_iter()
            .find(|key| &key[..48] == actor);
        key.map(|key| self.proof_wire(session, &key))
            .transpose()
            .map(Option::flatten)
    }

    pub fn proof_wire(&self, session: &Session<'_>, key: &ProofKey) -> Result<Option<Vec<u8>>> {
        self.check(session)?;
        if let Some(stored) = self
            .conflicts
            .get(key)
            .filter(|stored| stored.conflict.active())
        {
            return Ok(Some(stored.conflict.wire().to_vec()));
        }
        let actor: Actor = key[..48].try_into().unwrap();
        if key[48] == 1 {
            return self
                .index
                .forks()
                .get(&actor)
                .map(|fork| {
                    let a = self
                        .source_wire(&fork.first)
                        .ok_or_else(|| Error::new("missing fork source"))?;
                    let b = self
                        .source_wire(&fork.second)
                        .ok_or_else(|| Error::new("missing fork source"))?;
                    Conflict::from_pair(session, self.scope, a, b)
                        .map(|proof| proof.wire().to_vec())
                })
                .transpose();
        }
        let id: Digest = key[49..].try_into().unwrap();
        self.records
            .get(&id)
            .filter(|stored| stored.quarantine.is_some() && stored.header.actor == actor)
            .map(|stored| {
                Conflict::from_discarded(session, self.scope, &stored.wire)
                    .map(|proof| proof.wire().to_vec())
            })
            .transpose()
    }

    fn source_wire(&self, id: &Digest) -> Option<&[u8]> {
        self.records
            .get(id)
            .map(|stored| stored.wire.as_slice())
            .or_else(|| {
                self.conflicts
                    .values()
                    .find_map(|stored| stored.conflict.source_for(id))
            })
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
        let projection = self.catalog_projection(session)?;
        let parents = if self.scope == crate::catalog::scope(self.scope.trust()) {
            BTreeSet::new()
        } else {
            projection.heads
        };
        self.commit_from(session, payload, &projection.ordered, parents)
    }

    /// Append against complete retained liability evidence, including history
    /// hidden by peer retirement. The application must validate its allocation
    /// and payload before calling this; storage admission grants no capacity.
    /// Only this writer's predecessor becomes a new causal dependency: fixed
    /// shares never depend on another writer's current catalog visibility.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceRetirement",
            action = "Reserve",
            project = "aterm_db::resources::tests::retirement_tests::retirement_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceRetirement",
            action = "Dispatch",
            project = "aterm_db::resources::tests::retirement_tests::retirement_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbResourceRetirement",
            action = "Record",
            project = "aterm_db::resources::tests::retirement_tests::retirement_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbResourceRetirement",
            action = "LegacyPeer",
            reason = "The fixture installs legacy membership and retires a peer before appending."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbResourceRetirement",
            action = "LogPeer",
            reason = "The fixture installs log-backed membership and removes a peer before appending."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbResourceRetirement",
            action = "PeerCutoff",
            reason = "The fixture retains an administrator cutoff of the peer's catalog history."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbResourceRetirement",
            action = "LogCutoff",
            reason = "The fixture retains a peer cutoff with log-backed membership."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbResourceRetirement",
            action = "LocalCutoff",
            reason = "The fixture retains an administrator cutoff of the local writer."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbResourceRetirement",
            action = "MissingFrontier",
            reason = "The fixture retains a cutoff naming an unavailable frontier."
        )
    )]
    pub(crate) fn commit_liability(
        &mut self,
        session: &Session<'_>,
        payload: &[u8],
    ) -> Result<Receipt> {
        let retained = self
            .liability_evidence(session)?
            .iter()
            .map(|record| record.header().id)
            .collect::<Vec<_>>();
        self.commit_from(session, payload, &retained, BTreeSet::new())
    }

    fn commit_from(
        &mut self,
        session: &Session<'_>,
        payload: &[u8],
        retained: &[Digest],
        mut parents: BTreeSet<Digest>,
    ) -> Result<Receipt> {
        self.check(session)?;
        session.writer()?;
        if self.conflicted().contains(&self.actor) {
            return Err(Error::new(
                "local table writer has contradictory signed history",
            ));
        }
        if self.quarantined_actors().contains(&self.actor)
            || self.conflicts.values().any(|stored| {
                stored.conflict.discarded().is_some_and(|source| {
                    source.header.actor == self.actor
                        && !self.records.contains_key(&source.header.id)
                })
            })
        {
            return Err(Error::new(
                "local table writer has unavailable source history",
            ));
        }
        let previous = retained
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
        let visible: BTreeSet<_> = retained.iter().copied().collect();
        if own
            .clone()
            .any(|record| !visible.contains(&record.header.id))
            || own.count() as u64 != sequence - 1
        {
            return Err(Error::new(
                "local table writer has withheld or forked history; repair required",
            ));
        }
        if session_constraints(session)?.facts().any(|fact| {
            matches!(
                fact,
                Fact::Cutoff { table, actor, .. }
                    if *table == self.scope.digest() && *actor == self.actor
            )
        }) {
            return Err(Error::new("local table writer is cut off in this table"));
        }
        let previous = previous.map(|h| h.id);
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
        let constraints = session_constraints(session)?;
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
        let mut excluded: BTreeSet<_> = self
            .records
            .iter()
            .filter(|(id, stored)| {
                let authorized_writer = writers.contains(&stored.header.actor)
                    || cutoffs.contains(&stored.header.actor)
                    || (session.is_log_backed()
                        && super::envelope_roster(&stored.wire).is_some_and(|roster| {
                            session.historical_writer_in(&roster, &stored.header.actor)
                        }));
                withheld.contains(*id)
                    || !allowed.permits(id)
                    || stored.quarantine.is_some()
                    || !authorized_writer
            })
            .map(|(id, _)| *id)
            .collect();
        excluded.extend(withheld);
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
    /// Keep negative application evidence when membership or causal filtering
    /// withdraws a positive record. Quarantined payloads stay unavailable, with
    /// their independently witnessed header metadata. Callers must not interpret
    /// an unavailable payload as absence of a prior request or restriction.
    pub fn metadata_evidence(&self, session: &Session<'_>) -> Result<Vec<MetadataEvidence>> {
        let eligible: BTreeSet<_> = self
            .catalog_projection(session)?
            .ordered
            .into_iter()
            .collect();
        let mut evidence = self
            .records
            .values()
            .map(|stored| {
                let record = if stored.quarantine.is_none() {
                    Some(session.authenticate(self.scope, &stored.wire)?)
                } else {
                    None
                };
                let (roster, epoch, authored_ms) = match &record {
                    Some(record) => (*record.roster(), record.epoch(), record.authored_ms()),
                    None => stored
                        .quarantine
                        .expect("unavailable payload has witnessed quarantine metadata"),
                };
                Ok(MetadataEvidence {
                    header: stored.header.clone(),
                    receipt: stored.receipt,
                    roster,
                    epoch,
                    authored_ms,
                    record,
                    catalog_eligible: eligible.contains(&stored.header.id),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        for stored in self.conflicts.values() {
            let Some(source) = stored.conflict.discarded() else {
                continue;
            };
            if self.records.contains_key(&source.header.id) {
                continue;
            }
            let record = if stored.conflict.active() {
                None
            } else {
                let Some(record) = session.negative_record(
                    self.scope,
                    stored
                        .conflict
                        .source_for(&source.header.id)
                        .ok_or_else(|| Error::new("discarded source is missing"))?,
                )?
                else {
                    continue;
                };
                Some(record)
            };
            evidence.push(MetadataEvidence {
                header: source.header.clone(),
                receipt: stored.receipt,
                roster: source.roster,
                epoch: source.epoch,
                authored_ms: source.authored_ms,
                record,
                catalog_eligible: false,
            });
        }
        Ok(evidence)
    }
    /// Local operation inventory, including pending, forked and quarantined
    /// records. Certificates have a separate inventory. Neither is a claim of
    /// complete network history.
    pub fn inventory(&self, session: &Session<'_>) -> Result<impl Iterator<Item = &Digest>> {
        self.check(session)?;
        Ok(self.records.keys())
    }
    /// Retained positive operations from an actor, including withheld records.
    /// Application admission limits must not reset when projection hides data.
    pub fn retained_by(&self, session: &Session<'_>, actor: Actor) -> Result<usize> {
        self.check(session)?;
        Ok(self
            .records
            .values()
            .filter(|record| record.header.actor == actor)
            .count())
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
        let mut pending = Vec::new();
        for stored in self.conflicts.values() {
            for header in stored.conflict.headers() {
                if stored.conflict.discarded().is_none()
                    || stored.conflict.active()
                    || !self.records.contains_key(&header.id)
                {
                    pending.push(header.id);
                }
            }
        }
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
                let Some(header) = self.index.get(&id) else {
                    continue;
                };
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
                        writers
                            .get(&header.actor)
                            .into_iter()
                            .flat_map(|rows| rows.range((std::ops::Bound::Included(floor), end)))
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
        let mut quarantined: BTreeMap<_, _> = self
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
            .map(|record| (record.header.id, record))
            .collect();
        for stored in self.conflicts.values() {
            if !stored.conflict.active() {
                continue;
            }
            if let Some(source) = stored.conflict.discarded() {
                quarantined
                    .entry(source.header.id)
                    .or_insert_with(|| Quarantined {
                        header: source.header.clone(),
                        receipt: stored.receipt,
                        roster: source.roster,
                        epoch: source.epoch,
                        authored_ms: source.authored_ms,
                    });
            }
        }
        Ok(quarantined.into_values().collect())
    }
    /// All retained authenticated records, including catalog-withheld debits.
    /// Quarantine, missing causal history or a missing cutoff frontier makes
    /// this view unavailable. Keep allocations reserved until reconciliation
    /// instead of inferring a refund from incomplete or conflicting evidence.
    /// A fork certificate with no retained operation by its actor adds no local
    /// liability. An opaque discarded source might itself contain a debit, so
    /// it keeps this coarse table-wide view unavailable. Once selected history
    /// proves that source had no valid payload or writer grant it is harmless.
    /// This view never establishes complete network history.
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
        for stored in self.conflicts.values() {
            let Some(source) = stored.conflict.discarded() else {
                continue;
            };
            if self.records.contains_key(&source.header.id) {
                continue;
            }
            // Absence of positive custody cannot refund a potential debit.
            // An obsolete proof can be ignored only after selected history
            // establishes that its source was never a valid writer payload.
            if stored.conflict.active()
                || session
                    .negative_record(
                        self.scope,
                        stored
                            .conflict
                            .source_for(&source.header.id)
                            .ok_or_else(|| Error::new("discarded source is missing"))?,
                    )?
                    .is_some()
            {
                return Err(Error::new(
                    "table liability source is unresolved; keep allocations reserved",
                ));
            }
        }
        // A bound reader's bare fork need not imply any table operation. That
        // alone cannot freeze unrelated liabilities. Affected retained
        // operations still block interpretation, never disappear.
        let conflicted: BTreeSet<_> = self
            .conflicted()
            .into_iter()
            .chain(self.quarantined_actors())
            .collect();
        let mut positive = Index::new(MAX_RECORDS);
        for stored in self.records.values() {
            positive
                .insert(stored.header.clone())
                .map_err(|_| Error::new("invalid liability header"))?;
        }
        if self
            .records
            .values()
            .any(|stored| conflicted.contains(&stored.header.actor))
            || !positive.project().pending.is_empty()
        {
            return Err(Error::new(
                "table liability graph is incomplete or conflicting; keep allocations reserved",
            ));
        }
        let constraints = session_constraints(session)?;
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

fn session_constraints(
    session: &Session<'_>,
) -> Result<aterm_secrets::commons::constraints::Constraints> {
    if session.is_log_backed() {
        session
            .journal
            .retained_constraints()
            .map_err(super::failure)
    } else {
        session.journal.constraints().map_err(super::failure)
    }
}

fn operation_policy(scope: Scope, header: &Header, payload: usize) -> Result<()> {
    if scope == crate::catalog::scope(scope.trust())
        && (payload > crate::catalog::MAX_PAYLOAD
            || header.sequence > crate::catalog::RECORDS_PER_CREATOR as u64
            || header.parents.iter().copied().ne(header.previous))
    {
        return Err(Error::new(
            "catalog operation exceeds its schema or causal bounds",
        ));
    }
    Ok(())
}
/// The directory that holds each enrolled table's independent claim, beside Commons custody.
pub(crate) fn claim_dir(session: &Session<'_>) -> Result<PathBuf> {
    session
        .bootstrap
        .journal_dir()
        .parent()
        .map(|p| p.join("table-claims"))
        .ok_or_else(|| Error::new("Commons custody has no parent"))
}

/// Whether `claim` is `writer`'s well-formed pending enrollment claim for `scope`, exactly as
/// `Store::join` writes one before the table's journal exists.
fn pending_claim(claim: &[u8], scope: Scope, writer: &[u8]) -> bool {
    claim.len() == CLAIM_LEN
        && claim[..5] == *b"ATSC0"
        && claim[5..5 + super::SCOPE_LEN] == scope.bytes()
        && claim[5 + super::SCOPE_LEN..5 + super::SCOPE_LEN + 48] == *writer
        && claim[ANCHOR_LEN..].iter().all(|byte| *byte == 0)
}

/// The file name of `scope`'s enrollment claim inside [`claim_dir`].
pub(crate) fn claim_name(scope: Scope) -> String {
    format!("{}.claim", aterm_codec::hex::encode_lower(&scope.digest()))
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
