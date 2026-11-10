// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ciphertext-only table persistence. The independently stored enrollment claim
//! prevents loss of the data directory from silently resetting a writer. A full
//! rollback of both custody and data needs an external witness to detect.

use super::{
    ReadBatch, Record, Scope, Session,
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
    path::{Path, PathBuf},
};

#[cfg(test)]
mod append_tests;
pub mod custody;
mod cutoff;
#[cfg(all(test, unix))]
pub(crate) use cutoff::Signing;
#[cfg(unix)]
pub use cutoff::cut_removed_writer;
mod directory;
mod held;
#[cfg(test)]
mod hook_tests;
#[cfg(unix)]
mod pending;
#[cfg(test)]
mod recovery_tests;
mod replay;
mod replicas;
mod witness;
use witness::Witness;

pub use held::{Held, held_length, held_lengths, read_held};

#[cfg(test)]
type ReplayObserver = Option<Box<dyn FnOnce()>>;
#[cfg(test)]
thread_local! {
    static REPLAY_OBSERVER: std::cell::RefCell<ReplayObserver> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_replay_observer<T>(
    observe: impl FnOnce() + 'static,
    read: impl FnOnce() -> T,
) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            REPLAY_OBSERVER.with(|slot| slot.borrow_mut().take());
        }
    }
    REPLAY_OBSERVER.with(|slot| assert!(slot.borrow_mut().replace(Box::new(observe)).is_none()));
    let _reset = Reset;
    read()
}

#[cfg(test)]
thread_local! {
    /// While set, this thread's appends fail after the handle is poisoned and before the log is
    /// written: an append whose outcome is uncertain.
    pub(crate) static APPEND_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    static PRIMARY_CHECKED: std::cell::RefCell<ReplayObserver> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    static OPENS_AND_CHECKS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// How many stores this thread has opened and how many times it has checked one
/// ([`Store::check`]). Every open ends `open_region` by checking its store, so a read that stands
/// on that check adds none.
#[cfg(test)]
pub(crate) fn test_opens_and_checks() -> (usize, usize) {
    OPENS_AND_CHECKS.get()
}

/// `read`, with `observe` run once a combined store's check has checked its primary and before it
/// reads its own claim: where a writer that primary's waiting check let in has written.
#[cfg(test)]
pub(super) fn with_primary_checked<T>(
    observe: impl FnOnce() + 'static,
    read: impl FnOnce() -> T,
) -> T {
    PRIMARY_CHECKED.with(|slot| assert!(slot.borrow_mut().replace(Box::new(observe)).is_none()));
    let read = read();
    PRIMARY_CHECKED.with(|slot| assert!(slot.borrow_mut().take().is_none(), "no primary checked"));
    read
}

/// [`with_replay_observer`], with `observe` run at each replay `read` does, not only the first. A
/// replay `observe` itself does sees no observer.
#[cfg(test)]
pub(crate) fn with_each_replay_observed<T>(
    observe: impl Fn() + 'static,
    read: impl FnOnce() -> T,
) -> T {
    fn armed(observe: std::rc::Rc<dyn Fn()>) -> Box<dyn FnOnce()> {
        Box::new(move || {
            observe();
            let next = armed(observe);
            REPLAY_OBSERVER.with(|slot| *slot.borrow_mut() = Some(next));
        })
    }
    with_replay_observer(armed(std::rc::Rc::new(observe)), read)
}

pub(crate) const MAX_RECORDS: usize = 8192;
pub(crate) const MAX_BYTES: usize = 128 * 1024 * 1024;
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
pub(crate) const WRITER_RECORDS: usize = 4096;
pub(crate) const WRITER_BYTES: usize = 8 * 1024 * 1024;
const SUBJECT: &str = "commons/table/1";
const CONFLICT_SUBJECT: &str = "commons/table-conflict/1";
const ANCHOR_LEN: usize = 5 + super::SCOPE_LEN + 48 + 16;
const WITNESS_AT: usize = ANCHOR_LEN + 16;
const CLAIM_LEN: usize = WITNESS_AT + 40;

/// Recovery replicas can receive and relay authenticated records, but never author
/// one. Their distinct claim/anchor domain and separate location cannot resume the
/// original writer or stand in for its receipt commitment.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Region {
    Primary,
    Recovery,
}
impl Region {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRecovery",
            action = "WrongScope",
            project = "aterm_db::table::store::recovery_tests::recovery_transition"
        )
    )]
    fn validate_directory(self, session: &Session<'_>, dir: &Path, scope: Scope) -> Result<()> {
        if self == Self::Recovery {
            let expected = session
                .bootstrap
                .journal_dir()
                .parent()
                .ok_or_else(|| Error::new("Commons recovery custody has no parent"))?
                .join("recovery/tables")
                .join(aterm_codec::hex::encode_lower(&scope.digest()));
            if dir != expected {
                return Err(Error::new(
                    "recovery replica has a noncanonical local location",
                ));
            }
        }
        Ok(())
    }
    const fn magic(self) -> &'static [u8; 4] {
        match self {
            Self::Primary => b"ATSC",
            Self::Recovery => b"ATSR",
        }
    }
    pub(crate) fn claims(self, session: &Session<'_>) -> Result<PathBuf> {
        let primary = claim_dir(session)?;
        match self {
            Self::Primary => Ok(primary),
            Self::Recovery => primary
                .parent()
                .map(|parent| parent.join("recovery/table-claims"))
                .ok_or_else(|| Error::new("Commons recovery custody has no parent")),
        }
    }
}

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

/// Where a record that enters a table store came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// A local write, or a record an older peer delivered by an old path (a repair round,
    /// recovery, custody import): the egress publishes it.
    Local,
    /// A record the mesh delivered (its relay or `Inventory`): never published again.
    Mesh,
}

/// Which journal subject a record took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppendKind {
    Operation,
    Conflict,
}

/// One durable append to a table store, handed to the process's sink after the journal append
/// and the witness publish succeeded (the end of the store's one append path, or a writing open
/// that adopts the unwitnessed append a cut-off writer left). Fired exactly once per new record;
/// never for a duplicate, which an ingest answers with its stored receipt.
#[derive(Debug)]
pub struct Appended<'a> {
    /// The table's identifier ([`Scope::digest`]).
    pub table: Digest,
    /// Whether the store is a recovery replica (its own claim and data directories).
    pub recovery: bool,
    pub kind: AppendKind,
    /// The record's offset ([`Receipt::offset`]).
    pub offset: u64,
    /// The witnessed record count after the append (`offset + 1`).
    pub length: u64,
    pub via: Via,
    /// The operation or conflict wire, without the journal's receipt time.
    pub wire: &'a [u8],
}

type AppendSink = Box<dyn Fn(&Appended<'_>) + Send + Sync>;
static APPEND_SINK: std::sync::OnceLock<AppendSink> = std::sync::OnceLock::new();

/// Install the process's append sink; false where one is installed already. The sink runs under
/// the control journal lock and the table flock: it must not block (no I/O round trip).
pub fn set_append_sink(sink: impl Fn(&Appended<'_>) + Send + Sync + 'static) -> bool {
    APPEND_SINK.set(Box::new(sink)).is_ok()
}

#[cfg(any(test, debug_assertions))]
type ThreadSink = std::rc::Rc<dyn Fn(&Appended<'_>)>;
#[cfg(any(test, debug_assertions))]
thread_local! {
    static THREAD_SINK: std::cell::RefCell<Option<ThreadSink>> = const { std::cell::RefCell::new(None) };
}

/// Run `run` with `sink` taking this thread's appends in place of the process's sink, so tests
/// in one binary do not share one. The previous override comes back when `run` returns.
#[cfg(any(test, debug_assertions))]
pub fn with_append_sink<T>(sink: impl Fn(&Appended<'_>) + 'static, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<ThreadSink>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            THREAD_SINK.with(|slot| *slot.borrow_mut() = previous);
        }
    }
    let _restore =
        Restore(THREAD_SINK.with(|slot| slot.borrow_mut().replace(std::rc::Rc::new(sink))));
    run()
}

fn appended(record: &Appended<'_>) {
    #[cfg(any(test, debug_assertions))]
    if let Some(sink) = THREAD_SINK.with(|slot| slot.borrow().clone()) {
        sink(record);
        return;
    }
    if let Some(sink) = APPEND_SINK.get() {
        sink(record);
    }
}
struct Stored {
    wire: Vec<u8>,
    header: Header,
    receipt: Receipt,
    quarantine: Option<(Digest, u64, u64)>,
    /// The member-signed removal cutoff this operation is ([`cutoff`]), read once as it is
    /// admitted.
    cut: Option<cutoff::Cut>,
}

/// A member-signed cutoff a reader counts ([`Store::removal_cuts`]).
struct CountedCut {
    /// The cutoff's own operation.
    id: Digest,
    /// Its signer, and the operation's sequence in the signer's chain.
    signer: Actor,
    sequence: u64,
    cut: cutoff::Cut,
    /// Its signer's own seat is removed too.
    signer_removed: bool,
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
    /// A member-signed removal cutoff ([`cutoff`]): a causal parent like any operation, whose
    /// payload no application decodes and no reader counts as a refusal.
    pub cutoff: bool,
}

/// A local handle for authenticated table data. Writers are exclusive;
/// observations retain verified committed snapshots. A recovery observation
/// can include healthy primary data, with both custody claims rechecked. The index retains
/// authenticated records and previously witnessed quarantined headers.
/// Plaintext is decrypted per read. Reopen after a control change; open for a
/// short local batch, then drop the store and session before network I/O.
pub struct Store {
    region: Region,
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
    /// A merged reader keeps the other snapshot's custody checks, never its writer.
    observed_primary: Option<Box<Store>>,
}

/// What [`Store::check`] reads of an observed store's custody, kept apart from its records: the
/// scope, actor and access observation the store was read under, and its folders and the
/// enrollment claim it read; and its region, so a round can tell a recovery store's fence
/// ([`Self::is_recovery`]). It grants no authority and holds no guard, lock or file, so it can
/// outlive the batch that read the store and cross network I/O.
/// An observer's records are the claim's witnessed prefix (`replay::read_observed`), so where
/// [`Self::stands`] in a later session, a read of the store then that succeeds reads the same
/// records.
pub(crate) struct Fence {
    region: Region,
    scope: Scope,
    actor: Actor,
    observation: [u8; 64],
    data_dir: PathBuf,
    claim_dir: PathBuf,
    claim_name: String,
    claim: Vec<u8>,
}

impl Fence {
    pub(crate) const fn scope(&self) -> Scope {
        self.scope
    }

    pub(crate) const fn is_recovery(&self) -> bool {
        matches!(self.region, Region::Recovery)
    }

    /// Whether the store stands in `session` as it was read: what [`Store::check`] holds, by the
    /// same judgments ([`Custody`]): the session current for the scope, the same actor and access
    /// observation, and the claim read again equal to the one read then. A changed claim is
    /// `false`, not the check's `snapshot_changed`: the caller reads the store again.
    ///
    /// # Errors
    /// The session's currency ask fails ([`Custody::admits`]), or the claim read does
    /// ([`Custody::current_claim`]).
    pub(crate) fn stands(&self, session: &Session<'_>) -> Result<bool> {
        let custody = Custody {
            scope: self.scope,
            actor: self.actor,
            observation: &self.observation,
            data_dir: &self.data_dir,
            claim_dir: &self.claim_dir,
            claim_name: &self.claim_name,
            claim: &self.claim,
        };
        Ok(custody.admits(session)?
            && custody.current_claim()?.as_deref() == Some(self.claim.as_slice()))
    }
}

/// The custody a store was read under, which [`Store::check`] and a [`Fence`] judge alike.
struct Custody<'a> {
    scope: Scope,
    actor: Actor,
    observation: &'a [u8; 64],
    data_dir: &'a Path,
    claim_dir: &'a Path,
    claim_name: &'a str,
    claim: &'a [u8],
}

impl Custody<'_> {
    /// Whether `session` reads under the actor and access observation the store was read under.
    ///
    /// # Errors
    /// The session is not current for the scope, its writer-generation check refuses or runs out
    /// of time, or its membership read fails (busy included).
    fn admits(&self, session: &Session<'_>) -> Result<bool> {
        session.context(self.scope)?;
        Ok(self.actor == session.actor() && *self.observation == session.access_observation())
    }

    /// The claim now in the claim folder, once the data folder and the claim volume are judged to
    /// hold the claim read ([`directory::check_and_read_claim`]); none where it cannot be read.
    ///
    /// # Errors
    /// The claim read is malformed, or the data folder's identity or the claim volume's witness is
    /// unreadable or does not hold it.
    fn current_claim(&self) -> Result<Option<Vec<u8>>> {
        directory::check_and_read_claim(self.data_dir, self.claim_dir, self.claim_name, self.claim)
    }
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
        Self::join_region(session, dir, scope, Region::Primary)
    }

    pub(crate) fn join_recovery(session: &Session<'_>, dir: &Path, scope: Scope) -> Result<Self> {
        Self::join_region(session, dir, scope, Region::Recovery)
    }

    fn join_region(
        session: &Session<'_>,
        dir: &Path,
        scope: Scope,
        region: Region,
    ) -> Result<Self> {
        session.mutation_guard()?;
        session.context(scope)?;
        region.validate_directory(session, dir, scope)?;
        if !session.has_control_writer() {
            return Err(Error::new(
                "table enrollment requires a locked control observation",
            ));
        }
        if region == Region::Recovery {
            let tables = dir
                .parent()
                .ok_or_else(|| Error::new("recovery table has no parent"))?;
            aterm_journal::create_private_dir_durable(
                tables
                    .parent()
                    .ok_or_else(|| Error::new("recovery storage has no parent"))?,
            )?;
            aterm_journal::create_private_dir_durable(tables)?;
        }
        let claim_dir = region.claims(session)?;
        aterm_journal::create_private_dir_durable(&claim_dir)?;
        let claim_name = claim_name(scope);
        if matches!(
            private_file::read_private(&claim_dir, &claim_name, CLAIM_LEN),
            Err(PrivateFileError::Missing)
        ) {
            let mut claim = [0u8; CLAIM_LEN];
            claim[..4].copy_from_slice(region.magic());
            claim[4] = b'0';
            claim[5..5 + super::SCOPE_LEN].copy_from_slice(&scope.bytes());
            claim[5 + super::SCOPE_LEN..5 + super::SCOPE_LEN + 48]
                .copy_from_slice(&session.actor());
            claim[5 + super::SCOPE_LEN + 48..ANCHOR_LEN].copy_from_slice(&crypto::random::<16>()?);
            fsx::write_new(&claim_dir.join(&claim_name), &claim, 0o600).map_err(super::failure)?;
            // write_new's best-effort directory barrier is insufficient for this
            // enrollment: publishing a writer depends on its name surviving.
            aterm_durable::durable_dir(&claim_dir)?;
        }
        Self::open_region(
            session,
            dir,
            scope,
            true,
            Capacity::default(),
            false,
            region,
        )
    }
    /// Reopen with the independently retained ready enrollment claim. Missing
    /// custody, anchor, checkpoint or acknowledged bytes is damage, not a join.
    /// An observation session reads without acquiring the data journal's writer.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRecovery",
            action = "Promote",
            project = "aterm_db::table::store::recovery_tests::recovery_transition"
        )
    )]
    pub fn open(session: &Session<'_>, dir: &Path, scope: Scope) -> Result<Self> {
        if !session.has_control_writer() {
            return Self::observe(session, dir, scope);
        }
        let mut store = Self::open_mode(session, dir, scope, false)?;
        // A write a held `Remove` kept pending (M4) seals once its new epoch has arrived, when
        // its table next opens for writing, not only at the table's next write. A seal refused
        // before it appends (the pending set does not open, a merge is due) leaves the write
        // pending and the open standing: the next write runs the same seal and returns its error
        // to that write's caller. An append whose outcome is uncertain poisons the handle, so
        // the open fails with that append's own error.
        if let Err(error) = store.seal_pending(session)
            && (store.poisoned || store.log.as_ref().is_some_and(Log::is_poisoned))
        {
            return Err(error);
        }
        Ok(store)
    }

    /// Read the independently witnessed committed prefix beside any live writer.
    /// This handle cannot append, repair a tail, publish a witness or enroll storage.
    /// A custody change during the read is refused, so no unchecked snapshot escapes.
    pub fn observe(session: &Session<'_>, dir: &Path, scope: Scope) -> Result<Self> {
        Self::open_capacity_mode(session, dir, scope, false, Capacity::default(), true)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRecovery",
            action = "Reopen",
            project = "aterm_db::table::store::recovery_tests::recovery_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRecovery",
            action = "Corrupt",
            project = "aterm_db::table::store::recovery_tests::recovery_transition"
        )
    )]
    pub(crate) fn open_recovery(session: &Session<'_>, dir: &Path, scope: Scope) -> Result<Self> {
        Self::open_region(
            session,
            dir,
            scope,
            false,
            Capacity::default(),
            !session.has_control_writer(),
            Region::Recovery,
        )
    }

    pub(crate) fn observe_recovery(
        session: &Session<'_>,
        dir: &Path,
        scope: Scope,
    ) -> Result<Self> {
        Self::open_region(
            session,
            dir,
            scope,
            false,
            Capacity::default(),
            true,
            Region::Recovery,
        )
    }

    /// Whether this is a recovery replica or combined recovery observation,
    /// with authoring permanently disabled.
    #[must_use]
    pub const fn is_recovery(&self) -> bool {
        matches!(self.region, Region::Recovery)
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
        Self::observe_if_exists_region(session, dir, scope, Region::Primary)
    }

    pub(crate) fn observe_if_exists_region(
        session: &Session<'_>,
        dir: &Path,
        scope: Scope,
        region: Region,
    ) -> Result<Option<Self>> {
        session.context(scope)?;
        region.validate_directory(session, dir, scope)?;
        let claim_dir = region.claims(session)?;
        let claim_name = claim_name(scope);
        if matches!(
            private_file::read_private(&claim_dir, &claim_name, CLAIM_LEN),
            Err(PrivateFileError::Missing)
        ) {
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
        }
        // Enrollment may start after the absence check. Classify its freshly
        // validated claim inside the open, rather than trusting an earlier read.
        match Self::open_region(
            session,
            dir,
            scope,
            false,
            Capacity::default(),
            true,
            region,
        ) {
            Ok(store) => Ok(Some(store)),
            Err(error) if error.is_enrollment_pending() => Ok(None),
            Err(error) => Err(error),
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
        Self::open_region(
            session,
            dir,
            scope,
            initialize,
            capacity,
            read_only,
            Region::Primary,
        )
    }

    fn open_region(
        session: &Session<'_>,
        dir: &Path,
        scope: Scope,
        initialize: bool,
        capacity: Capacity,
        read_only: bool,
        region: Region,
    ) -> Result<Self> {
        // Keep each admitted vault source stable through local replay. Live
        // authority is checked before replay, before finalization and before
        // returning the store. Every record still authenticates its signed
        // envelope and historical evidence. No guard escapes this open or
        // crosses network I/O.
        let sources = session.local_source_reads()?;
        if !read_only {
            session.mutation_guard()?;
        }
        region.validate_directory(session, dir, scope)?;
        if capacity.operations > MAX_RECORDS
            || capacity.bytes > MAX_BYTES
            || capacity.conflicts > MAX_CONFLICTS
        {
            return Err(Error::new("table capacity exceeds custody bounds"));
        }
        session.context(scope)?;
        let claim_dir = region.claims(session)?;
        let claim_name = claim_name(scope);
        let mut claim = private_file::read_private(&claim_dir, &claim_name, CLAIM_LEN)
            .map_err(|_| Error::new("table enrollment claim is missing or damaged"))?;
        let pending = validate_claim_region(session, scope, &claim, region)?;
        if !pending {
            directory::check(dir, &claim_dir, &claim_name, &claim)?;
        }
        if pending && !initialize {
            return Err(Error::enrollment_pending());
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
            region,
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
            observation: session.access_observation(),
            observed_primary: None,
        };
        // The complete acknowledged prefix is verified before any opaque
        // record is considered for quarantine. The tail has no such authority.
        store.witness = replay.witness;
        // The one unwitnessed append a cut-off writer can leave (replay allows no more). A
        // writing open adopts it below and publishes its witness: that makes it a new record
        // here, which the append sink hears of as of any append. Where it came from is lost
        // with the writer, so it is `Local`: one publish too many is harmless, a missed one
        // is not.
        let adopted = (store.witness.count > acknowledged.count)
            .then(|| replay.entries.last())
            .flatten()
            .map(|entry| (entry.receipt.offset, entry.conflict, entry.wire.clone()));
        #[cfg(test)]
        if let Some(observe) = REPLAY_OBSERVER.with(|slot| slot.borrow_mut().take()) {
            observe();
        }
        if !replay.entries.is_empty() {
            sources.with_store_replay(scope, replay.entries.len(), |batch| {
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
                        let stored = replay::restore(batch, receipt, wire, acknowledged.count)?;
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
                Ok(())
            })?;
        }
        // An observation finalizes nothing, and `check` below holds its context.
        if !read_only {
            session.context(scope)?;
            session.mutation_guard()?;
        }
        if let Some(log) = &mut store.log {
            log.finish_open()?;
            if pending {
                store.claim[ANCHOR_LEN..WITNESS_AT].copy_from_slice(&directory::legacy(dir)?);
            }
            if pending || store.witness != acknowledged {
                store.publish_witness(store.witness)?;
                if let Some((offset, conflict, wire)) = &adopted {
                    appended(&Appended {
                        table: scope.digest(),
                        recovery: store.is_recovery(),
                        kind: if *conflict {
                            AppendKind::Conflict
                        } else {
                            AppendKind::Operation
                        },
                        offset: *offset,
                        length: store.witness.count,
                        via: Via::Local,
                        wire,
                    });
                }
            }
            // Complete a possible earlier rename whose directory barrier failed.
            aterm_durable::durable_dir(&store.claim_dir)?;
            directory::remember(dir, &store.claim_dir, &store.claim_name, &store.claim)?;
        }
        #[cfg(test)]
        OPENS_AND_CHECKS.with(|counts| counts.set((counts.get().0 + 1, counts.get().1)));
        store.check(session)?;
        Ok(store)
    }
    pub const fn scope(&self) -> Scope {
        self.scope
    }
    /// Stable identity of this local enrollment, excluding its advancing
    /// receipt witness. An application pin can detect replacement custody even
    /// when generic discovery has already enrolled an empty table again. A
    /// combined observation has no single enrollment identity.
    pub(crate) fn custody_id(&self, session: &Session<'_>) -> Result<Digest> {
        self.check(session)?;
        if self.observed_primary.is_some() {
            return Err(Error::new(
                "combined table observation has no single custody identity",
            ));
        }
        Ok(custody_of(&self.claim))
    }
    /// This store's custody as [`Self::check`] reads it, apart from its records ([`Fence`]); none
    /// for a writer's store, whose records its own appends move, one whose append is uncertain, or
    /// a combined view, whose records are two claims' prefixes.
    pub(crate) fn fence(&self) -> Option<Fence> {
        if self.log.is_some() || self.poisoned || self.observed_primary.is_some() {
            return None;
        }
        Some(Fence {
            region: self.region,
            scope: self.scope,
            actor: self.actor,
            observation: self.observation,
            data_dir: self.data_dir.clone(),
            claim_dir: self.claim_dir.clone(),
            claim_name: self.claim_name.clone(),
            claim: self.claim.clone(),
        })
    }

    /// The custody this store was read under, as [`Self::check`] and its [`Fence`] judge it.
    fn custody(&self) -> Custody<'_> {
        Custody {
            scope: self.scope,
            actor: self.actor,
            observation: &self.observation,
            data_dir: &self.data_dir,
            claim_dir: &self.claim_dir,
            claim_name: &self.claim_name,
            claim: &self.claim,
        }
    }
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableQuarantine",
            action = "ChangedObservation",
            project = "aterm_db::table::store::replay::tests::quarantine_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRecovery",
            action = "ChangedControl",
            project = "aterm_db::table::store::recovery_tests::recovery_transition"
        )
    )]
    fn check(&self, session: &Session<'_>) -> Result<()> {
        #[cfg(test)]
        OPENS_AND_CHECKS.with(|counts| counts.set((counts.get().0, counts.get().1 + 1)));
        let custody = self.custody();
        if !custody.admits(session)?
            || self.poisoned
            || self.log.as_ref().is_some_and(Log::is_poisoned)
        {
            return Err(Error::new(
                "table custody changed or append is uncertain; reopen",
            ));
        }
        // The primary first: its check can wait in a generation guard, and what a writer advanced
        // during that wait is this check's to see.
        if let Some(primary) = &self.observed_primary {
            primary.check(session)?;
            #[cfg(test)]
            if let Some(observe) = PRIMARY_CHECKED.with(|slot| slot.borrow_mut().take()) {
                observe();
            }
        }
        let current = custody.current_claim()?;
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
        self.admit_records(header.actor, 1, bytes)
    }
    fn admit_records(&self, actor: Actor, records: usize, bytes: usize) -> Result<()> {
        if self.records.len().saturating_add(records) > self.capacity.operations
            || self.operation_bytes.saturating_add(bytes) > self.capacity.bytes
        {
            return Err(Error::repair(
                "table operation capacity exhausted; conflict space is reserved",
                aterm_types::network_state::RepairCause::Quota,
            ));
        }
        let mut count = records;
        let mut size = bytes;
        for stored in self.records.values().filter(|r| r.header.actor == actor) {
            count += 1;
            size = size.saturating_add(stored.wire.len());
        }
        let writer_limit = if self.scope == crate::catalog::scope(self.scope.trust()) {
            crate::catalog::RECORDS_PER_CREATOR
        } else {
            WRITER_RECORDS
        };
        if count > writer_limit || size > WRITER_BYTES {
            return Err(Error::repair(
                "table writer evidence quota exhausted",
                aterm_types::network_state::RepairCause::Quota,
            ));
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
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRecovery",
            action = "Receive",
            project = "aterm_db::table::store::recovery_tests::recovery_transition"
        )
    )]
    pub fn ingest(&mut self, session: &Session<'_>, wire: &[u8]) -> Result<Receipt> {
        self.ingest_via(session, wire, Via::Local)
    }

    /// [`Self::ingest`] of a record that arrived by `via`, which the append sink reports.
    pub fn ingest_via(&mut self, session: &Session<'_>, wire: &[u8], via: Via) -> Result<Receipt> {
        session.mutation_guard()?;
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
        let receipt = self.append(SUBJECT, wire, id, via)?;
        self.operation_bytes += wire.len();
        self.index = proposed;
        self.records.insert(
            id,
            Stored {
                wire: wire.into(),
                header: record.header().clone(),
                receipt,
                quarantine: None,
                cut: cutoff::decode(record.payload()),
            },
        );
        self.poisoned = false;
        Ok(receipt)
    }
    // Publish the independently retained witness before returning any receipt, then hand the
    // record to the append sink. The witness is the journal's dependent write
    // (`Log::append_then`), written only once the journal's commit made the record and its
    // checkpoint durable: a lock-free reader that trusts it (`held_lengths`, `read_held`, the
    // egress's catch-up) never relays a record a power loss can still take. Where the claim
    // directory shares the log's device and astream serves the log Ordered it takes barriers
    // only, durable with the next append's commit, so on stable media the log is at most this
    // one record ahead of it, which a writing open adopts; elsewhere it is durable before the
    // receipt.
    fn append(&mut self, subject: &str, wire: &[u8], id: Digest, via: Via) -> Result<Receipt> {
        let log = self
            .log
            .as_mut()
            .ok_or_else(|| Error::new("read-only table observation cannot append"))?;
        let received_ms =
            aterm_time::unix_millis().ok_or_else(|| Error::new("receipt clock unavailable"))?;
        let mut body = Vec::with_capacity(8 + wire.len());
        body.extend_from_slice(&received_ms.to_le_bytes());
        body.extend_from_slice(wire);
        let witness = self.witness.advance(self.witness.count, id, received_ms)?;
        let mut claim = self.claim.clone();
        claim[WITNESS_AT..].copy_from_slice(&witness.bytes());
        self.poisoned = true;
        #[cfg(test)]
        if APPEND_FAILS.with(std::cell::Cell::get) {
            return Err(Error::new("injected append failure"));
        }
        let expected = self.witness.count;
        let mut diverged = false;
        let (claim_dir, claim_name) = (&self.claim_dir, &self.claim_name);
        let offset = match log.append_then(subject, body, claim_dir, |offset, flush| {
            if offset.0 != expected {
                diverged = true;
                return Err(std::io::Error::other(
                    "table log and custody witness diverged",
                ));
            }
            private_file::write_private_flushed(claim_dir, claim_name, &claim, flush)
        }) {
            Ok(offset) => offset.0,
            Err(_) if diverged => {
                return Err(Error::new("table log and custody witness diverged"));
            }
            Err(error) => return Err(error.into()),
        };
        self.claim = claim;
        self.witness = witness;
        appended(&Appended {
            table: self.scope.digest(),
            recovery: self.is_recovery(),
            kind: if subject == CONFLICT_SUBJECT {
                AppendKind::Conflict
            } else {
                AppendKind::Operation
            },
            offset,
            length: self.witness.count,
            via,
            wire,
        });
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
        self.ingest_conflict_via(session, wire, Via::Local)
    }

    /// [`Self::ingest_conflict`] of a certificate that arrived by `via`, which the append sink
    /// reports.
    pub fn ingest_conflict_via(
        &mut self,
        session: &Session<'_>,
        wire: &[u8],
        via: Via,
    ) -> Result<Receipt> {
        session.mutation_guard()?;
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
        let updated_index = if let Some(source) = conflict.discarded() {
            let mut index = self.index.clone();
            index
                .insert(source.header.clone())
                .map_err(|_| Error::new("invalid discarded-source header"))?;
            Some(index)
        } else {
            None
        };
        let receipt = self.append(CONFLICT_SUBJECT, wire, conflict.id(), via)?;
        self.conflicts
            .insert(key, StoredConflict { conflict, receipt });
        if let Some(index) = updated_index {
            self.index = index;
        }
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
        let mut sources_len = 0usize;
        let mut sources_bytes = 0usize;
        let mut own_len = 0usize;
        let mut own_bytes = 0usize;
        let actor = proof.actor();
        for stored in self
            .conflicts
            .values()
            .filter(|stored| stored.conflict.discarded().is_some())
        {
            let wire_len = stored.conflict.wire().len();
            sources_len += 1;
            sources_bytes = sources_bytes.saturating_add(wire_len);
            if stored.conflict.actor() == actor {
                own_len += 1;
                own_bytes = own_bytes.saturating_add(wire_len);
            }
        }
        if sources_len >= self.capacity.operations
            || sources_bytes.saturating_add(proof.wire().len())
                > self.capacity.bytes + self.capacity.operations * DISCARDED_OVERHEAD
            || own_len >= WRITER_RECORDS
            || own_bytes.saturating_add(proof.wire().len())
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
        Ok(self.proof_inventory_unchecked())
    }

    /// [`Self::proof_inventory`] of a store checked in the caller's session as it opened, with
    /// nothing admitted or read from the network since (`Round::store`'s).
    pub(crate) fn proof_inventory_unchecked(&self) -> BTreeSet<ProofKey> {
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
        keys
    }

    /// Export signed negative evidence without granting either source operation
    /// admission. Native witnesses are derived only from durable retained bytes.
    pub fn conflict_wire(&self, session: &Session<'_>, actor: &Actor) -> Result<Option<Vec<u8>>> {
        self.check(session)?;
        let key = self
            .proof_inventory_unchecked()
            .into_iter()
            .find(|key| &key[..48] == actor);
        key.map(|key| self.proof_wire_unchecked(session, &key))
            .transpose()
            .map(Option::flatten)
    }

    pub fn proof_wire(&self, session: &Session<'_>, key: &ProofKey) -> Result<Option<Vec<u8>>> {
        self.check(session)?;
        self.proof_wire_unchecked(session, key)
    }

    pub(crate) fn proof_wire_unchecked(
        &self,
        session: &Session<'_>,
        key: &ProofKey,
    ) -> Result<Option<Vec<u8>>> {
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
        self.commit_catalog_with_merges(session, payload, || {
            Err(Error::new(
                "table frontier requires a bounded merge before writing",
            ))
        })
    }

    /// Join a wide frontier using application no-ops before committing the edit.
    /// The payload factory must produce a command with no application effects.
    /// Prepare every record and admit its aggregate record and byte quotas
    /// before writing. Each accepted merge is independently durable; an
    /// interrupted write can resume from the reduced frontier without repeating
    /// a completed application edit.
    pub(crate) fn commit_catalog_with_merges(
        &mut self,
        session: &Session<'_>,
        payload: &[u8],
        merge: impl FnMut() -> Result<Vec<u8>>,
    ) -> Result<Receipt> {
        self.wait_for_rotation(session, pending::Shape::Catalog, payload)?;
        self.commit_catalog_now(session, payload, merge)
    }

    fn commit_catalog_now(
        &mut self,
        session: &Session<'_>,
        payload: &[u8],
        mut merge: impl FnMut() -> Result<Vec<u8>>,
    ) -> Result<Receipt> {
        let projection = self.catalog_projection(session)?;
        if self.catalog_append_bound(&projection) == 1 {
            let parents = if self.scope == crate::catalog::scope(self.scope.trust()) {
                BTreeSet::new()
            } else {
                projection.heads
            };
            return self.commit_from(session, payload, &projection.ordered, parents);
        }
        let mut previous = self
            .authoring_predecessor(session, &projection.ordered)?
            .cloned();
        let mut heads = projection.heads;
        let mut proposed = self.index.clone();
        let mut prepared = Vec::new();
        let mut bytes = 0usize;
        // Admit the exact sealed sizes and headers, including the final edit.
        // A conservative merge count can refuse a batch that would fit.
        let final_wire = loop {
            let fits = heads.len()
                + usize::from(previous.as_ref().is_some_and(|p| !heads.contains(&p.id)))
                <= MAX_PARENTS;
            // commit_from also names this writer's predecessor. Reserve one
            // parent for it even when another head already descends from it.
            let parents = if fits {
                heads.clone()
            } else {
                heads.iter().copied().take(MAX_PARENTS - 1).collect()
            };
            let merge_payload;
            let next = if fits {
                payload
            } else {
                merge_payload = merge()?;
                &merge_payload
            };
            let wire = self.seal_after(session, next, previous.as_ref(), parents)?;
            let record = session.authenticate(self.scope, &wire)?;
            operation_policy(self.scope, record.header(), record.payload().len())?;
            bytes = bytes.saturating_add(wire.len());
            self.admit_records(self.actor, prepared.len() + 1, bytes)?;
            proposed
                .insert(record.header().clone())
                .map_err(|_| Error::new("table index refused header or capacity"))?;
            if fits {
                break wire;
            }
            // Each prepared merge extends this writer's complete admitted chain
            // and replaces its named visible heads. Nothing is durable yet.
            for parent in &record.header().parents {
                heads.remove(parent);
            }
            heads.insert(record.header().id);
            previous = Some(record.header().clone());
            prepared.push(wire);
        };
        for wire in prepared {
            self.ingest(session, &wire)?;
        }
        self.ingest(session, &final_wire)
    }

    /// Reserve the final record and enough joins for any generated record ids.
    /// Each join replaces at least `MAX_PARENTS - 1` heads with one head. The
    /// writer's predecessor can remove another head, but admission cannot rely
    /// on its position in the next record-id ordering.
    fn catalog_append_bound(&self, projection: &Projection) -> usize {
        let heads = &projection.heads;
        if self.scope == crate::catalog::scope(self.scope.trust())
            || heads.len() < MAX_PARENTS
            || (heads.len() == MAX_PARENTS
                && self
                    .previous(&projection.ordered)
                    .is_none_or(|previous| heads.contains(&previous.id)))
        {
            1
        } else {
            1 + heads
                .len()
                .saturating_sub(MAX_PARENTS)
                .div_ceil(MAX_PARENTS - 2)
                .max(1)
        }
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
        self.wait_for_rotation(session, pending::Shape::Liability, payload)?;
        self.commit_liability_now(session, payload)
    }

    fn commit_liability_now(&mut self, session: &Session<'_>, payload: &[u8]) -> Result<Receipt> {
        let retained = self
            .liability_evidence(session)?
            .iter()
            .map(|record| record.header().id)
            .collect::<Vec<_>>();
        self.commit_from(session, payload, &retained, BTreeSet::new())
    }

    /// Append `payload` on this writer's own chain: its one causal dependency is its own previous
    /// record (the `commit_liability` shape), checked against the catalog projection, so this
    /// writer's withheld or forked history still refuses the append. A peer's head never becomes
    /// a parent, so no merge record is spent and one writer's append-only rows never wait on
    /// another's (docs/DESIGN-alab-lab-model-2026-11-01.md §3.9, the journal ring's appends).
    ///
    /// # Errors
    /// As `commit_catalog`, except that a wide frontier never refuses the append.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AlabJournal",
            action = "FlushCurrent",
            project = "aterm_db::alab::journal::model_tests::commons_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AlabJournal",
            action = "FlushNext",
            project = "aterm_db::alab::journal::model_tests::commons_transition"
        )
    )]
    pub(crate) fn commit_own(&mut self, session: &Session<'_>, payload: &[u8]) -> Result<Receipt> {
        self.wait_for_rotation(session, pending::Shape::Own, payload)?;
        self.commit_own_now(session, payload)
    }

    fn commit_own_now(&mut self, session: &Session<'_>, payload: &[u8]) -> Result<Receipt> {
        let projection = self.catalog_projection(session)?;
        self.commit_from(session, payload, &projection.ordered, BTreeSet::new())
    }

    /// The writer's wait for a removal's rotation (one-mesh M4, §11 B11), before any write:
    /// writes kept pending earlier are sealed first, in order, once the rotation has arrived; and
    /// while it has not, this write joins them, named for the epoch it waited on, and the caller
    /// is told so ([`Error::is_write_pending`]). Nothing is sealed under an epoch that still
    /// wraps a Mac the log removed.
    fn wait_for_rotation(
        &mut self,
        session: &Session<'_>,
        shape: pending::Shape,
        payload: &[u8],
    ) -> Result<()> {
        self.seal_pending(session)?;
        if !session.waits_for_rotation() {
            return Ok(());
        }
        self.check(session)?;
        session.writer()?;
        if payload.len() > crypto::MAX_BODY {
            return Err(Error::new("table operation exceeds its bound"));
        }
        let key = session.pending_key(&self.scope)?;
        let mut writes = pending::load(&self.data_dir, &key)?;
        let write = pending::Pending {
            shape,
            epoch: session.secret_basis(),
            payload: zeroize::Zeroizing::new(payload.to_vec()),
        };
        if !writes.contains(&write) {
            writes.push(write);
            pending::save(&self.data_dir, &key, &writes)?;
        }
        Err(Error::write_pending())
    }

    /// Seal every write this table keeps pending for a removal's rotation, in the order they
    /// were written, under the epoch `session` seals under now; how many were sealed. Nothing is
    /// sealed while the rotation is still due. Each write leaves the set once its operation is
    /// durable, so an interruption seals the rest on the next call (a crash between the two
    /// can seal one write twice, which a table command's own idempotence absorbs, never lose
    /// it).
    ///
    /// # Errors
    /// The pending set does not open, or a write fails as a direct write would; the writes not
    /// yet sealed stay pending.
    pub fn seal_pending(&mut self, session: &Session<'_>) -> Result<usize> {
        if !pending::path(&self.data_dir).exists() || session.waits_for_rotation() {
            return Ok(0);
        }
        let key = session.pending_key(&self.scope)?;
        let writes = pending::load(&self.data_dir, &key)?;
        let mut sealed = 0;
        for (i, write) in writes.iter().enumerate() {
            match write.shape {
                pending::Shape::Catalog => self.commit_catalog_now(session, &write.payload, || {
                    Err(Error::new(
                        "table frontier requires a bounded merge before writing",
                    ))
                }),
                pending::Shape::Own => self.commit_own_now(session, &write.payload),
                pending::Shape::Liability => self.commit_liability_now(session, &write.payload),
            }?;
            pending::save(&self.data_dir, &key, &writes[i + 1..])?;
            sealed += 1;
        }
        Ok(sealed)
    }

    /// The epochs this table's pending writes wait on, in order: what a test, and the per-seat
    /// view, read of a wait.
    ///
    /// # Errors
    /// The pending set does not open.
    pub fn pending_epochs(&self, session: &Session<'_>) -> Result<Vec<Digest>> {
        let key = session.pending_key(&self.scope)?;
        Ok(pending::load(&self.data_dir, &key)?
            .into_iter()
            .map(|write| write.epoch)
            .collect())
    }

    /// The records and wire bytes this replica retains, in all and authored by `actor`: withheld
    /// and excluded records count too, since they hold storage, so a share never resets when a
    /// projection hides a writer's records.
    ///
    /// # Errors
    /// Table custody changed under this handle.
    pub(crate) fn retained_use(
        &self,
        session: &Session<'_>,
        actor: Actor,
    ) -> Result<crate::catalog::ring::Used> {
        self.check(session)?;
        let mut used = crate::catalog::ring::Used::default();
        for stored in self.records.values() {
            used.records += 1;
            used.bytes = used.bytes.saturating_add(stored.wire.len());
            if stored.header.actor == actor {
                used.writer_records += 1;
                used.writer_bytes = used.writer_bytes.saturating_add(stored.wire.len());
            }
        }
        Ok(used)
    }

    fn previous(&self, retained: &[Digest]) -> Option<&Header> {
        retained
            .iter()
            .filter_map(|id| self.index.get(id))
            .filter(|header| header.actor == self.actor)
            .max_by_key(|header| header.sequence)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRecovery",
            action = "AuthorEmpty",
            project = "aterm_db::table::store::recovery_tests::recovery_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRecovery",
            action = "AuthorReceived",
            project = "aterm_db::table::store::recovery_tests::recovery_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRecovery",
            action = "AuthorRestart",
            project = "aterm_db::table::store::recovery_tests::recovery_transition"
        )
    )]
    fn commit_from(
        &mut self,
        session: &Session<'_>,
        payload: &[u8],
        retained: &[Digest],
        parents: BTreeSet<Digest>,
    ) -> Result<Receipt> {
        let previous = self.authoring_predecessor(session, retained)?;
        let wire = self.seal_after(session, payload, previous, parents)?;
        self.ingest(session, &wire)
    }
    fn authoring_predecessor(
        &self,
        session: &Session<'_>,
        retained: &[Digest],
    ) -> Result<Option<&Header>> {
        self.check(session)?;
        if self.region == Region::Recovery {
            return Err(Error::new(
                "recovery replica cannot author records; original writer custody remains fenced",
            ));
        }
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
        let previous = self.previous(retained);
        let sequence = previous
            .map_or(Some(1), |h| h.sequence.checked_add(1))
            .ok_or_else(|| Error::new("table writer sequence exhausted"))?;
        let visible: BTreeSet<_> = retained.iter().copied().collect();
        let mut own_count = 0u64;
        for record in self
            .records
            .values()
            .filter(|record| record.header.actor == self.actor)
        {
            if !visible.contains(&record.header.id) {
                return Err(Error::new(
                    "local table writer has withheld or forked history; repair required",
                ));
            }
            own_count += 1;
        }
        if own_count != sequence - 1 {
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
        Ok(previous)
    }
    fn seal_after(
        &self,
        session: &Session<'_>,
        payload: &[u8],
        previous: Option<&Header>,
        mut parents: BTreeSet<Digest>,
    ) -> Result<Vec<u8>> {
        let sequence = previous
            .map_or(Some(1), |h| h.sequence.checked_add(1))
            .ok_or_else(|| Error::new("table writer sequence exhausted"))?;
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
        session.seal(
            self.scope,
            Causal {
                sequence,
                previous,
                parents: &parents,
            },
            payload,
        )
    }
    /// The member-signed cutoffs of this table's removed writers ([`cutoff`]) that may count:
    /// each one signed by a remover of the writer it cuts, as this session's log names them (the
    /// signers of its effective `Remove`s). [`Self::catalog_projection`] counts each remover's
    /// newest of them ([`Self::newest_cuts`]), a removed remover's only inside its own allowed
    /// prefix. Off under the `table-cutoffs-off` negative control.
    fn removal_cuts(&self, session: &Session<'_>, withheld: &BTreeSet<Digest>) -> Vec<CountedCut> {
        #[cfg(any(test, debug_assertions))]
        if super::table_cutoffs_off() {
            return Vec::new();
        }
        let candidates: Vec<_> = self
            .records
            .iter()
            .filter_map(|(id, stored)| {
                let cut = stored.cut?;
                let signer = stored.header.actor;
                (cut.actor != signer && !withheld.contains(id) && stored.quarantine.is_none())
                    .then_some((*id, signer, stored.header.sequence, cut))
            })
            .collect();
        if candidates.is_empty() {
            return Vec::new();
        }
        let removers = session.removers_of(
            candidates
                .iter()
                .flat_map(|(_, signer, _, cut)| [cut.actor, *signer]),
        );
        candidates
            .into_iter()
            .filter(|(_, signer, _, cut)| {
                removers
                    .get(&cut.actor)
                    .is_some_and(|by| by.contains(signer))
            })
            .map(|(id, signer, sequence, cut)| CountedCut {
                id,
                signer,
                sequence,
                cut,
                signer_removed: removers.contains_key(&signer),
            })
            .collect()
    }

    /// Of `cuts`, each signer's newest cutoff of each writer: what a reader counts, so a cutoff
    /// a refused or failed `Remove` left is replaced by the next attempt's.
    fn newest_cuts<'c>(cuts: impl IntoIterator<Item = &'c CountedCut>) -> Vec<&'c CountedCut> {
        let mut newest: BTreeMap<(Actor, Actor), &CountedCut> = BTreeMap::new();
        for counted in cuts {
            let slot = newest
                .entry((counted.signer, counted.cut.actor))
                .or_insert(counted);
            if counted.sequence > slot.sequence {
                *slot = counted;
            }
        }
        newest.into_values().collect()
    }

    /// The cutoffs `cuts` name, by the writer each cuts, as [`Constraints::catalog_with`] takes
    /// them.
    fn cut_frontiers<'c>(
        cuts: impl IntoIterator<Item = &'c &'c CountedCut>,
    ) -> BTreeMap<Actor, Vec<Option<aterm_secrets::commons::constraints::Frontier>>> {
        let mut frontiers: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for counted in cuts {
            frontiers
                .entry(counted.cut.actor)
                .or_default()
                .push(counted.cut.through);
        }
        frontiers
    }

    /// Under [`super::TABLE_CUT_BY_EPOCH`] alone: each operation of another writer under the
    /// epoch a counted cutoff (`counted`) was signed under that is not in that cutoff's causal
    /// past, what a rule cutting by epoch would withhold.
    #[cfg(any(test, debug_assertions))]
    fn cut_by_epoch(&self, counted: &[Digest]) -> BTreeSet<Digest> {
        let mut cut = BTreeSet::new();
        if !super::TABLE_CUT_BY_EPOCH.with(std::cell::Cell::get) {
            return cut;
        }
        for id in counted {
            let Some(cutoff) = self.records.get(id) else {
                continue;
            };
            let epoch = super::envelope_roster(&cutoff.wire);
            let mut past = BTreeSet::new();
            let mut walk = vec![*id];
            while let Some(at) = walk.pop() {
                if past.insert(at)
                    && let Some(stored) = self.records.get(&at)
                {
                    walk.extend(stored.header.previous);
                    walk.extend(stored.header.parents.iter().copied());
                }
            }
            cut.extend(
                self.records
                    .iter()
                    .filter(|(at, stored)| {
                        !past.contains(*at)
                            && stored.header.actor != cutoff.header.actor
                            && super::envelope_roster(&stored.wire) == epoch
                    })
                    .map(|(at, _)| *at),
            );
        }
        cut
    }

    /// `actor`'s last operation held here (the highest, the lowest digest among a fork's), or
    /// `None` where this table holds none: what a cutoff of it signed now names.
    pub(crate) fn last_of(
        &self,
        actor: Actor,
    ) -> Option<aterm_secrets::commons::constraints::Frontier> {
        self.records
            .values()
            .filter(|stored| stored.header.actor == actor && stored.quarantine.is_none())
            .max_by(|a, b| {
                a.header
                    .sequence
                    .cmp(&b.header.sequence)
                    .then(b.header.id.cmp(&a.header.id))
            })
            .map(|stored| aterm_secrets::commons::constraints::Frontier {
                sequence: stored.header.sequence,
                digest: stored.header.id,
            })
    }

    /// This Mac's newest cutoff of `actor` in this table, if it signed one.
    fn own_cut_of(&self, actor: Actor) -> Option<cutoff::Cut> {
        self.records
            .values()
            .filter(|stored| stored.header.actor == self.actor)
            .filter_map(|stored| Some((stored.header.sequence, stored.cut?)))
            .filter(|(_, cut)| cut.actor == actor)
            .max_by_key(|(sequence, _)| *sequence)
            .map(|(_, cut)| cut)
    }

    /// Sign this Mac's cutoff of `actor`, a writer it is removing, in this table ([`cutoff`]):
    /// a table operation of its own naming `through`. Before the `Remove`
    /// ([`cutoff::Signing::Fresh`]) that is [`Self::last_of`], signed again only when this Mac's
    /// newest cutoff of `actor` here names another operation, since a reader counts only each
    /// remover's newest. As the remover's repair ([`cutoff::Signing::Repair`]) it signs the
    /// frontier saved before the `Remove` unless its newest cutoff names it already, and with none
    /// saved only where this Mac holds no cutoff of `actor`, so a repair never moves a cutoff past
    /// the removal. On a share ([`cutoff::Signing::Supersede`]) it only replaces a cutoff of its
    /// own an earlier attempt left, at the writer's last operation held now: the first remover's
    /// cutoff still bounds the writer, as cutoffs intersect. Whether it signed one; a
    /// cutoff of this Mac itself is never signed.
    ///
    /// # Errors
    /// As [`Self::commit_own`], except that it never waits on a rotation: it is signed before
    /// the `Remove` that makes one due.
    pub(crate) fn sign_cutoff(
        &mut self,
        session: &Session<'_>,
        actor: Actor,
        signing: cutoff::Signing,
    ) -> Result<bool> {
        if actor == self.actor {
            return Ok(false);
        }
        let held = self.own_cut_of(actor);
        let through = match signing {
            cutoff::Signing::Fresh => {
                let through = self.last_of(actor);
                if held.is_some_and(|cut| cut.through == through) {
                    return Ok(false);
                }
                through
            }
            // A frontier kept before the effective `Remove` replaces a cutoff an earlier,
            // refused attempt left; with none kept, a held cutoff stands.
            cutoff::Signing::Repair { saved } => match (held, saved) {
                (Some(cut), Some(saved)) if cut.through == saved => return Ok(false),
                (Some(_), None) => return Ok(false),
                (_, Some(saved)) => saved,
                (None, None) => self.last_of(actor),
            },
            cutoff::Signing::Supersede => match held {
                Some(cut) if cut.through != self.last_of(actor) => self.last_of(actor),
                _ => return Ok(false),
            },
        };
        self.commit_own_now(session, &cutoff::encode(&cutoff::Cut { actor, through }))?;
        Ok(true)
    }

    fn catalog_projection(&self, session: &Session<'_>) -> Result<Projection> {
        self.check(session)?;
        // Peers cannot admit quarantine, so it and its withheld dependents do
        // not form transferable cutoff evidence. Exclude those local-only
        // frontiers; the full index below still retains restrictive forks.
        let withheld = self.quarantine_closure();
        let owned_index;
        let index = if withheld.is_empty()
            && !self
                .conflicts
                .values()
                .any(|stored| stored.conflict.discarded().is_some())
        {
            &self.index
        } else {
            owned_index = self.replication_index_with(&withheld)?;
            &owned_index
        };
        let constraints = session_constraints(session)?;
        let table = self.scope.digest();
        let writers: BTreeSet<_> = session
            .access
            .members()
            .iter()
            .filter(|m| m.role() == Role::Writer)
            .map(|m| m.binding().actor())
            .collect();
        let is_log_backed = session.is_log_backed();
        let mut historical: BTreeMap<(Digest, Actor), bool> = BTreeMap::new();
        // A cutoff signed by a remover that is itself removed counts only inside that signer's
        // own allowed prefix, so a removed Mac's later operation cuts nobody.
        // A removed remover's cutoffs count only inside its own allowed prefix, which the
        // cutoffs of its own removers bound, themselves perhaps removed: walk to a fixpoint, each
        // round counting each signer's newest standing cutoff, so a removed Mac's later cutoff
        // neither cuts nor un-cuts anyone. Bounded by one round per cutoff.
        let cuts = self.removal_cuts(session, &withheld);
        let mut standing = Self::newest_cuts(cuts.iter().filter(|cut| !cut.signer_removed));
        let mut removals = Self::cut_frontiers(&standing);
        let mut allowed = constraints.catalog_with(table, index, &removals);
        if cuts.iter().any(|cut| cut.signer_removed) {
            for _ in 0..=cuts.len() {
                let next = Self::newest_cuts(
                    cuts.iter()
                        .filter(|cut| !cut.signer_removed || allowed.permits(&cut.id)),
                );
                if next
                    .iter()
                    .map(|cut| cut.id)
                    .eq(standing.iter().map(|cut| cut.id))
                {
                    break;
                }
                standing = next;
                removals = Self::cut_frontiers(&standing);
                allowed = constraints.catalog_with(table, index, &removals);
            }
        }
        let counted: Vec<Digest> = standing.iter().map(|cut| cut.id).collect();
        #[cfg(any(test, debug_assertions))]
        let by_epoch = self.cut_by_epoch(&counted);
        #[cfg(not(any(test, debug_assertions)))]
        let by_epoch = {
            let _ = counted;
            BTreeSet::<Digest>::new()
        };
        let cutoffs: BTreeSet<_> = constraints
            .facts()
            .filter_map(|fact| match fact {
                Fact::Cutoff {
                    table: t, actor, ..
                } if *t == table => Some(*actor),
                _ => None,
            })
            .chain(removals.keys().copied())
            .collect();
        let mut excluded: BTreeSet<_> = self
            .records
            .iter()
            .filter(|(id, stored)| {
                withheld.contains(*id)
                    || stored.quarantine.is_some()
                    || !allowed.permits(id)
                    || by_epoch.contains(*id)
                    || !(writers.contains(&stored.header.actor)
                        || cutoffs.contains(&stored.header.actor)
                        || (is_log_backed
                            && super::envelope_roster(&stored.wire).is_some_and(|roster| {
                                *historical
                                    .entry((roster, stored.header.actor))
                                    .or_insert_with(|| {
                                        session.historical_writer_in(&roster, &stored.header.actor)
                                    })
                            })))
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
        session.with_read_batch(self.scope, |batch| {
            let records = self
                .catalog_projection(session)?
                .ordered
                .iter()
                .filter(|id| self.records[*id].cut.is_none())
                .map(|id| batch.authenticate(&self.records[id].wire))
                .collect::<Result<Vec<_>>>()?;
            self.check(session)?;
            Ok(records)
        })
    }
    /// Keep negative application evidence when membership or causal filtering
    /// withdraws a positive record. Quarantined payloads stay unavailable, with
    /// their independently witnessed header metadata. Callers must not interpret
    /// an unavailable payload as absence of a prior request or restriction.
    pub fn metadata_evidence(&self, session: &Session<'_>) -> Result<Vec<MetadataEvidence>> {
        self.metadata_with_append_bound(session)
            .map(|(evidence, _)| evidence)
    }

    /// Read application evidence and its next append's conservative record
    /// count from the same membership and causal projection.
    pub(crate) fn metadata_with_append_bound(
        &self,
        session: &Session<'_>,
    ) -> Result<(Vec<MetadataEvidence>, usize)> {
        session.with_read_batch(self.scope, |batch| {
            let projection = self.catalog_projection(session)?;
            let append_bound = self.catalog_append_bound(&projection);
            let evidence = self.metadata_from(session, batch, projection)?;
            self.check(session)?;
            Ok((evidence, append_bound))
        })
    }
    /// [`Self::catalog`] and [`Self::metadata_evidence`] from one projection and one read batch,
    /// whose closing checks hold both answers. Positive catalog-eligible records are moved into
    /// the returned `Vec<Record>` rather than decrypted twice.
    pub(crate) fn catalog_and_evidence(
        &self,
        session: &Session<'_>,
    ) -> Result<(Vec<Record>, Vec<MetadataEvidence>)> {
        session.with_read_batch(self.scope, |batch| {
            let projection = self.catalog_projection(session)?;
            let ordered = projection.ordered.clone();
            let mut evidence = self.metadata_from(session, batch, projection)?;
            let positive = &mut evidence[..self.records.len()];
            let records = ordered
                .iter()
                .filter(|id| self.records[*id].cut.is_none())
                .map(|id| {
                    let idx = positive
                        .binary_search_by_key(id, |e| e.header.id)
                        .expect("catalog projection id is in store records");
                    positive[idx]
                        .record
                        .take()
                        .ok_or_else(|| Error::new("table payload authentication failed"))
                })
                .collect::<Result<Vec<_>>>()?;
            self.check(session)?;
            Ok((records, evidence))
        })
    }
    fn metadata_from(
        &self,
        session: &Session<'_>,
        batch: &ReadBatch<'_, '_>,
        projection: Projection,
    ) -> Result<Vec<MetadataEvidence>> {
        let eligible: BTreeSet<_> = projection.ordered.into_iter().collect();
        let mut evidence = self
            .records
            .values()
            .map(|stored| {
                let record = if stored.quarantine.is_none() {
                    Some(batch.authenticate(&stored.wire)?)
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
                    cutoff: stored.cut.is_some(),
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
                cutoff: false,
            });
        }
        Ok(evidence)
    }
    /// Local operation inventory, including pending, forked and quarantined
    /// records. Certificates have a separate inventory. Neither is a claim of
    /// complete network history.
    pub fn inventory(
        &self,
        session: &Session<'_>,
    ) -> Result<impl ExactSizeIterator<Item = &Digest>> {
        self.check(session)?;
        Ok(self.inventory_unchecked())
    }
    /// [`Self::inventory`] of a store checked in the caller's session as it opened, with nothing
    /// admitted or read from the network since (`Round::store`'s).
    pub(crate) fn inventory_unchecked(&self) -> impl ExactSizeIterator<Item = &Digest> {
        self.records.keys()
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
        Ok(self.replication_inventory_unchecked())
    }
    pub(crate) fn replication_inventory_unchecked(&self) -> impl Iterator<Item = &Digest> {
        let excluded = self.quarantine_closure();
        self.records
            .iter()
            .filter(move |(id, _)| !excluded.contains(*id))
            .map(|(id, _)| id)
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
        }
        if pending.is_empty() {
            return BTreeSet::new();
        }
        let mut children: BTreeMap<Digest, Vec<Digest>> = BTreeMap::new();
        let mut writers = BTreeMap::<Actor, BTreeMap<u64, Vec<Digest>>>::new();
        for (id, stored) in &self.records {
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
    fn replication_index_with(&self, excluded: &BTreeSet<Digest>) -> Result<Index> {
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
        if !conflicted.is_empty()
            && self
                .records
                .values()
                .any(|stored| conflicted.contains(&stored.header.actor))
        {
            return Err(Error::new(
                "table liability graph is incomplete or conflicting; keep allocations reserved",
            ));
        }
        let has_pending = if self
            .conflicts
            .values()
            .any(|stored| stored.conflict.discarded().is_some())
        {
            let mut positive = Index::new(MAX_RECORDS);
            for stored in self.records.values() {
                positive
                    .insert(stored.header.clone())
                    .map_err(|_| Error::new("invalid liability header"))?;
            }
            !positive.project().pending.is_empty()
        } else {
            !self.index.project().pending.is_empty()
        };
        if has_pending {
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
        Ok(self.wire_unchecked(id))
    }
    pub(crate) fn wire_unchecked(&self, id: &Digest) -> Option<&[u8]> {
        self.records.get(id).map(|stored| stored.wire.as_slice())
    }
    pub fn receipt(&self, session: &Session<'_>, id: &Digest) -> Result<Option<Receipt>> {
        self.check(session)?;
        Ok(self.receipt_unchecked(id))
    }
    pub(crate) fn receipt_unchecked(&self, id: &Digest) -> Option<Receipt> {
        self.records.get(id).map(|stored| stored.receipt)
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
    claim_dir_of(session.bootstrap)
}

/// [`claim_dir`] of `bootstrap`'s custody, for a reader with no session (the runtime's clock
/// store, `crate::clock`).
pub(crate) fn claim_dir_of(
    bootstrap: &aterm_secrets::commons::bootstrap::Bootstrap,
) -> Result<PathBuf> {
    bootstrap
        .journal_dir()
        .parent()
        .map(|p| p.join("table-claims"))
        .ok_or_else(|| Error::new("Commons custody has no parent"))
}

fn validate_claim(session: &Session<'_>, scope: Scope, claim: &[u8]) -> Result<bool> {
    validate_claim_region(session, scope, claim, Region::Primary)
}

fn validate_claim_region(
    session: &Session<'_>,
    scope: Scope,
    claim: &[u8],
    region: Region,
) -> Result<bool> {
    if claim.len() != CLAIM_LEN
        || &claim[..4] != region.magic()
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
    Ok(pending)
}

/// The file name of `table`'s enrollment claim inside [`claim_dir`].
pub(crate) fn claim_name_for(table: &Digest) -> String {
    let mut name = String::with_capacity(64 + 6);
    aterm_codec::hex::push_lower(&mut name, table);
    name.push_str(".claim");
    name
}

/// The file name of `scope`'s enrollment claim inside [`claim_dir`].
pub(crate) fn claim_name(scope: Scope) -> String {
    claim_name_for(&scope.digest())
}

/// The custody identity of the enrollment `claim` ([`Store::custody_id`]): its bytes before the
/// receipt witness.
fn custody_of(claim: &[u8]) -> Digest {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/commons/table-custody/1\0");
    hash.update(&claim[..WITNESS_AT]);
    hash.finalize()
}

/// The directory identity `claim`, an enrollment claim, binds its table to (none for a claim no
/// open has bound yet), in [`directory_identity`]'s encoding: for a test's copy of a lab
/// (`table::fixture::Lab::fork`), whose table directories are other directories.
#[cfg(test)]
pub(crate) fn claimed_directory(claim: &[u8]) -> Option<[u8; 16]> {
    assert_eq!(claim.len(), CLAIM_LEN, "an enrollment claim");
    let identity: [u8; 16] = claim[ANCHOR_LEN..WITNESS_AT].try_into().unwrap();
    (identity != [0; 16]).then_some(identity)
}

/// `dir`'s identity as a table's first open records it in the table's claim.
#[cfg(test)]
pub(crate) fn directory_identity(dir: &Path) -> Result<[u8; 16]> {
    directory::legacy(dir)
}

/// `claim`, a bound enrollment claim, bound to `dir` instead, as a first open of the table there
/// binds it: the claim a test's copy of a lab keeps for the copy of the claim's table directory.
/// Returns the custody identity the claim had and the one it has now, which an application pin
/// of the table holds ([`Store::custody_id`]).
#[cfg(test)]
pub(crate) fn rebind_claim(claim: &mut [u8], dir: &Path) -> Result<(Digest, Digest)> {
    assert!(claimed_directory(claim).is_some(), "a bound claim");
    let before = custody_of(claim);
    claim[ANCHOR_LEN..WITNESS_AT].copy_from_slice(&directory::legacy(dir)?);
    Ok((before, custody_of(claim)))
}
