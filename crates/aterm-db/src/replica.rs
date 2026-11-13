// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded application repair after the sealed control exchange. Tables and paths
//! are provisioned locally or discovered through the signed Commons catalog;
//! peers never choose a local path. Every local batch
//! revalidates secrets custody and checks current peer membership. No journal lock
//! crosses network I/O. Dynamic subscriptions default on within an enrolled lab.

use crate::admission::deadline as admission_deadline;
pub use crate::control::Side;
use crate::{
    Error, Result, control,
    table::{
        Scope, Session,
        conflict::ProofKey,
        store::{Enrollment, Fence, Receipt, Store, Via},
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_digest::Sha256;
use aterm_secrets::{
    client::Paths,
    commons::{bootstrap::Bootstrap, journal::Trust},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

/// The bound of a table-ID list: the tables one Mac selects, and a round's clock mark.
pub const MAX_TABLES: usize = 64;

/// One selected table's digest, operation digests, and conflict proof keys.
pub type MeshTableSummary = (Digest, BTreeSet<Digest>, BTreeSet<ProofKey>);
/// The bound of one table's operation inventory.
pub const MAX_RECORDS: usize = 8192;
/// The bound of one table's conflict-proof inventory.
pub const MAX_PROOFS: usize = MAX_RECORDS * 2 + aterm_secrets::commons::constraints::MAX_BINDINGS;
/// The records of one batch, each way; also the bound of a round's history-key needs.
pub const BATCH: usize = 16;
/// A full batch may continue before the ordinary table scan resumes.
const TABLE_BURST: usize = 4;
/// An initiator asks for the peer's table witnesses ([`witness`]) on its first round with a peer,
/// and again on the first round after one this long without a table list traded with it: a peer
/// that restarted, slept or was cut off has its moved tables repaired before the regular turn.
const WITNESS_GAP_MS: u64 = 120_000;
/// How long one side spends reading its tables' content for witnesses in a round, checked before
/// each table's read; a table left unread reads as differing ([`witness::UNKNOWN`]). Tests read
/// every table: there it is a hang detector, never a latency budget that decides an order.
#[cfg(not(test))]
const WITNESS_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(test)]
const WITNESS_BUDGET: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;
/// How many times a round reads its local state before state that keeps moving is its answer
/// ([`Round::local`]).
const LOCAL_READS: usize = 4;
const PROBE: &[u8; 5] = b"ACTP3";
const READY: &[u8; 5] = b"ACTA3";

#[derive(Debug, PartialEq, Eq)]
enum ProbeMismatch {
    Malformed,
    Protocol,
    History {
        head_matches: bool,
        basis_matches: bool,
    },
}

impl ProbeMismatch {
    fn error(self) -> Error {
        match self {
            Self::Malformed => Error::repair(
                "invalid application repair greeting",
                aterm_types::network_state::RepairCause::Decode,
            ),
            Self::Protocol => Error::repair(
                "unsupported application repair protocol greeting",
                aterm_types::network_state::RepairCause::NeedsUpdate,
            ),
            Self::History {
                head_matches,
                basis_matches,
            } => Error::new(&format!(
                "application repair needs matching control history (head_matches={head_matches}, basis_matches={basis_matches})"
            )),
        }
    }
}

/// Classify only the bounded greeting. Never echo untrusted frame bytes or
/// accept a partial control observation when explaining a refusal.
fn validate_probe(
    frame: &[u8],
    tag: &[u8],
    observation: &[u8; 64],
) -> std::result::Result<(), ProbeMismatch> {
    if frame.len() != tag.len() + observation.len() {
        return Err(ProbeMismatch::Malformed);
    }
    if &frame[..tag.len()] != tag {
        return Err(ProbeMismatch::Protocol);
    }
    let remote = &frame[tag.len()..];
    let head_matches = remote[..32] == observation[..32];
    let basis_matches = remote[32..] == observation[32..];
    if !head_matches || !basis_matches {
        return Err(ProbeMismatch::History {
            head_matches,
            basis_matches,
        });
    }
    Ok(())
}

/// The table the rounds this side initiates repair first while it names one both sides hold: the
/// table a push hands on now (`crate::runtime::Pusher`), shared with the runtime that pushes.
pub(crate) type First = std::sync::Arc<std::sync::Mutex<Option<Digest>>>;

/// A bounded operation-ID inventory observed on this authenticated channel.
/// Equal summaries establish equal ID sets under SHA-256's collision
/// resistance, not that the peer can currently decrypt or use every operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InventorySummary {
    pub count: usize,
    /// SHA-256 over the domain `aterm/commons/table-operation-inventory/1\0`,
    /// the little-endian u64 count, and the lexicographically sorted 32-byte IDs.
    /// Local and peer sets use exactly the same encoding.
    pub fingerprint: Digest,
}

#[cfg(test)]
thread_local! {
    static INVENTORY_HASHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl InventorySummary {
    // The empty set's encoding has no scope or actor input. Keep only that protocol constant.
    fn empty() -> Self {
        static EMPTY: std::sync::OnceLock<InventorySummary> = std::sync::OnceLock::new();
        *EMPTY.get_or_init(|| Self::of_sorted(0, &[]))
    }

    /// Summarize an exact operation-ID set with the same canonical encoding as
    /// authenticated repair reports. Counts alone do not establish equality.
    pub fn of(ids: &BTreeSet<Digest>) -> Self {
        Self::of_sorted(ids.len(), ids)
    }

    pub(crate) fn of_sorted<'a>(count: usize, ids: impl IntoIterator<Item = &'a Digest>) -> Self {
        #[cfg(test)]
        INVENTORY_HASHES.set(INVENTORY_HASHES.get().saturating_add(1));
        let mut hash = Sha256::new();
        hash.update(b"aterm/commons/table-operation-inventory/1\0");
        hash.update((count as u64).to_le_bytes());
        for id in ids {
            hash.update(id);
        }
        Self {
            count,
            fingerprint: hash.finalize(),
        }
    }
}

/// One admitted local inventory sample. This metadata grants no authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InventoryObservation {
    pub(crate) scope: Scope,
    pub(crate) local_actor: Actor,
    pub(crate) peer_actor: Actor,
    pub(crate) control_observation: [u8; 64],
    pub(crate) inventory: InventorySummary,
    pub(crate) observed_at_ms: u64,
    pub(crate) sampled_at: Instant,
}

/// Historical evidence that an authenticated peer reported the same retained operation IDs.
/// This establishes neither projection equality, decryptability, nor current/global currency.
/// Its final read, under a fresh admission, stands on the store's unchanged enrollment claim where
/// the store has not moved (`Round::fenced_observation`): it reads neither the table's journal nor
/// its records again, so damage to them under an unchanged claim is found by the next open that
/// reads the journal: a writer's, or an observer's whose folder or files moved since a settled
/// read of them, and in any case one within a fixed count of observer opens
/// (`table::store::journals`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchingInventory {
    observed: InventoryObservation,
}

impl MatchingInventory {
    /// Exact table, trust and audience generation observed.
    pub const fn scope(self) -> Scope {
        self.observed.scope
    }
    /// The admitted local writer incarnation.
    pub const fn local_actor(self) -> Actor {
        self.observed.local_actor
    }
    /// The channel-authenticated peer incarnation.
    pub const fn peer_actor(self) -> Actor {
        self.observed.peer_actor
    }
    /// Exact local control head and basis admitted at both the operation inventory's read and the
    /// final read.
    pub const fn control_observation(self) -> [u8; 64] {
        self.observed.control_observation
    }
    /// Canonical retained-ID count and SHA-256 fingerprint.
    pub const fn inventory(self) -> InventorySummary {
        self.observed.inventory
    }
    /// Original local inventory sample time, never renewed by completion or publication.
    pub const fn observed_at_ms(self) -> u64 {
        self.observed.observed_at_ms
    }
    /// Original process-local inventory sample clock, never a persisted freshness claim.
    pub const fn sampled_at(self) -> Instant {
        self.observed.sampled_at
    }

    /// The successful table exchange and the final admitted read must still name the same
    /// context and exact retained inventory. Offered subsets do not affect this narrower claim.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "Valid",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "DifferentHash",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "DifferentCount",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "MissingLocal",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "MissingPeer",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "WrongScope",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "LocalChanged",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "PeerChanged",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "ControlChanged",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "FinalInventoryChanged",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "FinalScopeChanged",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "ZeroStamp",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "BackwardsSample",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "Deferred",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "OfferedSubset",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeAdmission",
            action = "LaterWallRollback",
            project = "aterm_db::network_state::account_exchange::tests::admission_transition"
        )
    )]
    pub(crate) fn from_observations(
        report: &Report,
        before: InventoryObservation,
        after: InventoryObservation,
    ) -> Option<Self> {
        (report.table == Some(before.scope.digest())
            && report.deferred == 0
            && report.local_known == Some(before.inventory)
            && report.peer_known == Some(before.inventory)
            && before.scope == after.scope
            && before.local_actor == after.local_actor
            && before.peer_actor == after.peer_actor
            && before.control_observation == after.control_observation
            && before.inventory == after.inventory
            && before.observed_at_ms != 0
            && before.sampled_at <= after.sampled_at)
            .then_some(Self { observed: before })
    }
}

/// Bounded observations from one application round. Only a successful exchange
/// completes the report; even then it establishes no global convergence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Optional matching retained-ID observation; only the runtime completion gate may publish it.
    pub matching_inventory: Option<MatchingInventory>,
    /// This round used receive-only recovery storage. It restores no primary writer custody.
    pub recovery: bool,
    /// The selected table, if both peers had one in common.
    pub table: Option<Digest>,
    /// The selected table's scope, whose digest is [`Self::table`]: which application table the
    /// round served, for a reader that would otherwise read custody and the catalog to say.
    pub scope: Option<Scope>,
    /// Local retained operation count before this table's positive repair.
    /// This is inventory knowledge, never a claim of complete network history.
    pub retained: usize,
    /// Local retained operation count after positive operation ingestion in tests.
    #[cfg(test)]
    pub(crate) retained_after: usize,
    /// Exact locally known operation IDs after conflict repair and before any
    /// positive operation batch. None means the inventory was unavailable.
    pub local_known: Option<InventorySummary>,
    /// Locally eligible operation IDs at that same point. These can differ
    /// from known IDs when retained evidence is ineligible for replication.
    pub local_offered: Option<InventorySummary>,
    /// The peer's known operation IDs received before either side sends a
    /// positive operation batch. This observes one table on one peer, not global
    /// convergence, and does not disclose the peer's offered subset.
    pub peer_known: Option<InventorySummary>,
    /// Positive durable receipts observed from the peer, including retries.
    pub sent: usize,
    /// Accepted local ingests, including retries, whether or not receipts reach the peer.
    pub received: usize,
    pub deferred: usize,
    /// The peer now holds every operation of the table this side offered as this round read its
    /// inventory: none the peer lacked was left for a later round (more than one batch), and the
    /// peer accepted each one sent. A round that deferred the table, or selected none, has not.
    /// A push counts only such a round as reaching the peer (`crate::runtime::Pusher`).
    pub caught_up: bool,
    /// The selected table was deferred: either side could not read an inventory of it, or a
    /// batch of it was refused ([`Self::deferred`] counts these, and history keys besides). Such
    /// a round repaired nothing of its table: it is no success (`crate::runtime`'s discovery
    /// retries it soon), and the initiator takes the table again in its next round.
    pub table_deferred: bool,
    /// This side initiated the round and asked the peer which tables differ (`witness`).
    pub witnesses_asked: bool,
    /// Encrypted historical handoffs written to the peer in this
    /// round. This is not a separate durable key-receipt acknowledgement.
    pub keys_sent: usize,
    pub keys_received: usize,
    pub conflicts_sent: usize,
    /// Accepted local conflict ingests, whether or not receipts reach the peer.
    pub conflicts_received: usize,
}
/// Why a completed table round could not exchange an inventory. This is a
/// local observation; an unavailable peer inventory carries no remote cause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryDeferral {
    /// The unavailable inventory: `conflicts` or `operations`.
    pub inventory: &'static str,
    /// The local store's refusal, without application values or ciphertext.
    pub local_error: Option<String>,
    /// Sanitized category of the local refusal; a peer's unavailable inventory has no cause.
    pub local_cause: Option<aterm_types::network_state::RepairCause>,
    /// Local negative evidence must finish moving into the recovery store first.
    pub retaining_primary_conflicts: bool,
    /// The peer sent an unavailable inventory; its reason is not on the wire.
    pub peer_unavailable: bool,
}

/// First local refusal of a fully received batch. No application value or
/// ciphertext is retained here, and the reason is limited to 128 characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchDeferral {
    /// Closed local failure category, independent of the bounded diagnostic string.
    pub cause: aterm_types::network_state::RepairCause,
    /// The received batch: `operations` or `conflicts`.
    pub batch: &'static str,
    /// Local storage opening, ingestion, or receipt validation.
    pub stage: &'static str,
    /// Bounded local error metadata; no frame bytes are copied into it.
    pub reason: String,
}
impl BatchDeferral {
    fn new(batch: &'static str, stage: &'static str, reason: Error) -> Self {
        Self {
            cause: reason.repair_cause(),
            batch,
            stage,
            reason: reason.to_string().chars().take(128).collect(),
        }
    }
}

/// Volatile fairness hints. Inventory and durable receipts decide what is missing.
#[derive(Clone, Default, Debug)]
pub struct Repair {
    history: Option<Digest>,
    table: Option<Digest>,
    table_follow_up: Option<(Digest, usize)>,
    table_yield: bool,
    records: BTreeMap<Digest, Digest>,
    conflicts: BTreeMap<Digest, ProofKey>,
    legacy_probe: bool,
    pub(crate) retry_probe: bool,
    pub(crate) deferral: Option<InventoryDeferral>,
    pub(crate) batch_deferral: Option<BatchDeferral>,
    pub(crate) interrupted: Option<Report>,
    /// What this round's table-ID lists said of the peer's clock (`crate::clock`, F22), kept
    /// whether or not the table repair after them succeeded.
    pub(crate) clock: Option<crate::clock::Reading>,
    /// When (unix ms) this side last traded table-ID lists with the peer as initiator: none, or
    /// one [`WITNESS_GAP_MS`] ago, asks for witnesses ([`witness`]).
    listed_ms: Option<u64>,
    /// When (unix ms) this side last asked the peer for witnesses: a peer's ask heard within
    /// [`WITNESS_GAP_MS`] of it answers this side's own, and is not echoed back.
    asked_ms: Option<u64>,
    /// The common tables whose witnesses differed when this side last asked ([`witness`]): the
    /// regular turn visits these alone while any remain; a table leaves as it is selected.
    differing: BTreeSet<Digest>,
}

/// Volatile work hints learned from authenticated inventory exchanges. These
/// select a table to ask about; every exchange still authorizes its own data.
#[derive(Default)]
pub(crate) struct Backfill {
    wanted: BTreeMap<Digest, BTreeSet<Digest>>,
    after: Option<Digest>,
    initial_after: Option<Digest>,
    hinted: usize,
}

impl Backfill {
    fn regular(&mut self, common: &BTreeSet<Digest>, after: Option<Digest>) -> Option<Digest> {
        // Keep each peer's ordinary cursor, and spread fresh peers across
        // their common tables. Only an actual selection restores hint turns.
        let selected = cycle(common, after.or(self.initial_after)).next().copied();
        if selected.is_some() {
            self.hinted = 0;
            if after.is_none() {
                self.initial_after = selected;
            }
        }
        selected
    }

    fn observe(&mut self, table: Digest, known: &BTreeSet<Digest>, remote: &BTreeSet<Digest>) {
        if !self.wanted.contains_key(&table) && self.wanted.len() >= MAX_TABLES {
            return;
        }
        let wanted = self.wanted.entry(table).or_default();
        wanted.retain(|record| !known.contains(record));
        // Different peers can advertise disjoint sets. Bound the remembered
        // union by one inventory's limit; ordinary scans still visit every table.
        for record in remote.difference(known) {
            if wanted.len() == MAX_RECORDS {
                break;
            }
            wanted.insert(*record);
        }
        if wanted.is_empty() {
            self.wanted.remove(&table);
        }
    }

    fn retained(&mut self, table: Digest, record: Digest) {
        let Some(wanted) = self.wanted.get_mut(&table) else {
            return;
        };
        wanted.remove(&record);
        if wanted.is_empty() {
            self.wanted.remove(&table);
        }
    }

    fn next(&mut self, local: &BTreeSet<Digest>, common: &BTreeSet<Digest>) -> Option<Digest> {
        self.wanted.retain(|table, _| local.contains(table));
        if self.hinted >= TABLE_BURST {
            return None;
        }
        use std::ops::Bound::{Excluded, Unbounded};
        let tail = match self.after {
            Some(after) => self.wanted.range((Excluded(after), Unbounded)),
            None => self.wanted.range::<Digest, _>(..),
        };
        let head = self
            .after
            .into_iter()
            .flat_map(|after| self.wanted.range(..=after));
        let selected = tail
            .chain(head)
            .map(|(table, _)| *table)
            .find(|table| common.contains(table));
        if selected.is_some() {
            self.after = selected;
            self.hinted += 1;
        }
        selected
    }
}

/// A locally provisioned table set. The existing control channel supplies the
/// authenticated peer actor; merely constructing this object opens no network.
pub struct Replica {
    paths: Paths,
    trust: Trust,
    actor: Actor,
    tables: BTreeMap<Digest, (Scope, PathBuf)>,
    commons: Option<(crate::catalog::Directory, PathBuf)>,
    /// The table a push names ([`First`], [`Self::prefer`]).
    first: First,
    backfill: Arc<Mutex<Backfill>>,
    /// Whether a Commons round's table-ID list carries this Mac's clock mark (`crate::clock`);
    /// tests turn it off to stand for a build from before F22.
    marks: bool,
    /// Whether this side asks a peer, and answers one, which tables differ (`witness`); tests turn
    /// it off to stand for a build from before CR1.
    witnesses: bool,
    /// The claims ring's tables within [`crate::clock::BAND`] periods of one period, by digest,
    /// and that period: what a peer that sends no mark offered is matched against.
    offers: Arc<Mutex<Option<RingOffers>>>,
    /// The bootstrap the last open of this replica's vault gave, which each round takes while it
    /// stands ([`Self::opening`]: [`Bootstrap::stands_in`] through the round's opening batch, else
    /// [`Bootstrap::stands`] in [`Self::bootstrap`]) and opens afresh, and keeps, only once it
    /// falls: a quiet round's open re-derived from unchanged files the bootstrap the round
    /// before it had (about a seventh of its CPU: over origin/main `7514a3dd04`, 14.6% of a
    /// sustained round's on-CPU samples over a long log and 16.3% over a short one, 2026-11-09).
    ///
    /// What a round that takes it gives up, by decision: the vault is judged, while the bootstrap
    /// stands, only by the observation memo every session already judges it by
    /// (`client::observe_commons_quiet`), where each open read it afresh, and its pinned files by
    /// the digests kept while they stand, as every session's custody check judges them, where each
    /// open read them again; the round makes no
    /// folder durable, where each open made the vault's folder, `commons/` and the control
    /// journal's folder durable, as no session's read of the journal does; and it takes neither
    /// custody lock, which only the custody writers that change a file `stands` judges (creation,
    /// recovery, an ingest) need. The bootstrap
    /// holds this Mac's device seed and age identity for the replica's life, as its endpoint
    /// already holds the device's signing identity (`Endpoint`'s `identity`) and the runtime's
    /// serve its own bootstrap.
    kept: Mutex<Option<Arc<Bootstrap>>>,
    /// The bootstraps [`Self::bootstrap`] opened afresh, where none it kept stood.
    #[cfg(test)]
    bootstrap_opens: std::sync::atomic::AtomicUsize,
    /// The rounds [`Self::opening`]'s try took the kept bootstrap in.
    #[cfg(test)]
    kept_takes: std::sync::atomic::AtomicUsize,
    /// The clock that places the ring windows, in place of the system's.
    #[cfg(test)]
    pub(crate) ring_clock: Option<u64>,
    /// MUTANT (tests): a peer's ask is always echoed, as CR1's first cut did. A field, not a
    /// thread-local, because a test's responder runs on its own thread.
    #[cfg(test)]
    echoes: bool,
}
/// One period and the claims ring's tables within [`crate::clock::BAND`] periods of it, each
/// table's digest with its period ([`Replica::offers`]).
type RingOffers = (u64, BTreeMap<Digest, u64>);

impl Replica {
    pub fn new(paths: Paths, tables: Vec<(Scope, PathBuf)>) -> Result<Self> {
        if tables.len() > MAX_TABLES {
            return Err(Error::new("too many provisioned tables"));
        }
        let bootstrap = Bootstrap::cached(&paths)
            .map_err(crate::table::failure)?
            .ok_or_else(|| Error::new("table repair has no secrets custody"))?;
        let session = Session::observe(&bootstrap)?;
        let trust = bootstrap.trust();
        let actor = session.actor();
        drop(session);
        let mut map = BTreeMap::new();
        for (scope, path) in tables {
            if scope.trust() != trust || map.insert(scope.digest(), (scope, path)).is_some() {
                return Err(Error::new("duplicate table or foreign table trust"));
            }
        }
        Ok(Self {
            paths,
            trust,
            actor,
            tables: map,
            commons: None,
            first: First::default(),
            backfill: Arc::default(),
            marks: true,
            witnesses: true,
            offers: Arc::default(),
            kept: Mutex::new(Some(Arc::new(bootstrap))),
            #[cfg(test)]
            bootstrap_opens: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            kept_takes: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            ring_clock: None,
            #[cfg(test)]
            echoes: false,
        })
    }
    /// Automatic catalog and application subscriptions. Construction performs
    /// no table initialization and does not stop control repair on table damage.
    pub fn commons(paths: Paths, config: PathBuf) -> Result<Self> {
        let bootstrap = Bootstrap::cached(&paths)
            .map_err(crate::table::failure)?
            .ok_or_else(|| Error::new("Commons repair has no secrets custody"))?;
        Self::commons_from_bootstrap(paths, config, bootstrap)
    }

    /// [`Self::commons`] sharing an absolute local-admission deadline. It still
    /// caches no grant: every application batch revalidates custody/membership,
    /// and its rounds take the bootstrap it opened only while it stands
    /// ([`Self::opening`]).
    pub fn commons_until(
        paths: Paths,
        config: PathBuf,
        deadline: std::time::Instant,
    ) -> Result<Self> {
        let bootstrap = Bootstrap::cached_until(&paths, deadline)
            .map_err(crate::table::failure)?
            .ok_or_else(|| Error::new("Commons repair has no secrets custody"))?;
        Self::commons_from_bootstrap(paths, config, bootstrap)
    }

    fn commons_from_bootstrap(paths: Paths, config: PathBuf, bootstrap: Bootstrap) -> Result<Self> {
        let trust = bootstrap.trust();
        let actor = bootstrap
            .request()
            .map_err(crate::table::failure)?
            .binding()
            .actor();
        let mut replica = Self::commons_context(paths, config, trust, actor);
        replica.kept = Mutex::new(Some(Arc::new(bootstrap)));
        Ok(replica)
    }

    pub(crate) fn commons_from_session(
        paths: Paths,
        config: PathBuf,
        session: &Session<'_>,
    ) -> Result<Self> {
        let (bootstrap, _) = session.control_context()?;
        if paths.dir.join("commons/control") != bootstrap.journal_dir() {
            return Err(Error::new(
                "Commons subscriptions have another custody path",
            ));
        }
        Ok(Self::commons_context(
            paths,
            config,
            bootstrap.trust(),
            session.actor(),
        ))
    }

    fn commons_context(paths: Paths, config: PathBuf, trust: Trust, actor: Actor) -> Self {
        let directory = crate::catalog::Directory::new(&paths, trust);
        Self {
            paths,
            trust,
            actor,
            tables: BTreeMap::new(),
            commons: Some((directory, config)),
            first: First::default(),
            backfill: Arc::default(),
            marks: true,
            witnesses: true,
            offers: Arc::default(),
            kept: Mutex::new(None),
            #[cfg(test)]
            bootstrap_opens: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            kept_takes: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            ring_clock: None,
            #[cfg(test)]
            echoes: false,
        }
    }

    /// Repair `first`'s table first in each round this side initiates while it names one both
    /// sides hold; the other tables keep their turn after it.
    pub(crate) fn prefer(&mut self, first: First) {
        self.first = first;
    }
    pub(crate) fn matches(&self, trust: Trust, actor: Actor) -> bool {
        self.trust == trust && self.actor == actor
    }

    pub(crate) fn share_backfill(&mut self, backfill: &Arc<Mutex<Backfill>>) {
        self.backfill = backfill.clone();
    }

    /// Send no clock mark, as a build from before F22 (tests).
    #[cfg(test)]
    pub(crate) const fn with_marks(mut self, marks: bool) -> Self {
        self.marks = marks;
        self
    }

    /// Neither ask nor answer which tables differ, as a build from before CR1 (tests).
    #[cfg(test)]
    pub(crate) const fn with_witnesses(mut self, witnesses: bool) -> Self {
        self.witnesses = witnesses;
        self
    }

    /// MUTANT: echo every ask heard, as CR1's first cut did (tests).
    #[cfg(test)]
    pub(crate) const fn with_echoes(mut self, echoes: bool) -> Self {
        self.echoes = echoes;
        self
    }

    /// The wall time a clock mark carries and a reading is taken at (ms): the system's, or the
    /// test's ring clock.
    fn wall_ms(&self) -> Option<u64> {
        #[cfg(test)]
        if let Some(now) = self.ring_clock {
            return Some(now.saturating_mul(1000));
        }
        aterm_time::unix_millis()
    }

    /// What the claims tables a peer that sent no mark offered say of its clock, received at
    /// `recv_ms` (`crate::clock::Reading::from_offers`): the claims ring's tables within
    /// [`crate::clock::BAND`] periods of this clock's, matched against `remote`, whose digests are
    /// formed once per period.
    fn offered_reading(
        &self,
        remote: &BTreeSet<Digest>,
        recv_ms: u64,
    ) -> Option<crate::clock::Reading> {
        use crate::catalog::ring::CLAIMS;
        let period = CLAIMS.epoch_of(recv_ms / 1000);
        let mut offers = self
            .offers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if offers.as_ref().is_none_or(|(at, _)| *at != period) {
            let mut tables = BTreeMap::new();
            let band = period.saturating_sub(crate::clock::BAND)
                ..=period.saturating_add(crate::clock::BAND);
            for epoch in band {
                tables.insert(CLAIMS.scope(self.trust, epoch).ok()?.digest(), epoch);
            }
            *offers = Some((period, tables));
        }
        let (_, tables) = offers.as_ref()?;
        let offered: BTreeSet<u64> = remote
            .iter()
            .filter_map(|id| tables.get(id).copied())
            .collect();
        crate::clock::Reading::from_offers(&offered, recv_ms, CLAIMS.period_s)
    }

    /// The clock one exchange places the ring windows by, read once per exchange.
    fn ring_now(&self) -> Result<u64> {
        #[cfg(test)]
        if let Some(now) = self.ring_clock {
            return Ok(now);
        }
        aterm_time::unix_millis()
            .map(|ms| ms / 1000)
            .ok_or_else(|| Error::new("system clock cannot place the ring tables"))
    }

    /// One admitted local batch. `peer`, where given, is the channel's authenticated actor and
    /// must be a current member; a record the mesh delivered has no channel peer, and its own
    /// writer grant, which every ingest checks, stands alone. Through `group`, a round's group
    /// look ([`GroupLook`]), a read batch is lent the look the group keeps, where it keeps one
    /// ([`Self::follow`]), and is otherwise the group's first ([`Self::lead`]); a write lets the
    /// group's look go first and admits as it would with no group, as does any admission inside
    /// another's batch.
    fn observe<T>(
        &self,
        bootstrap: &Bootstrap,
        peer: impl Into<Option<Actor>>,
        expected: Option<[u8; 64]>,
        write: bool,
        group: Option<&GroupLook<'_>>,
        run: impl FnOnce(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        if bootstrap.trust() != self.trust {
            return Err(Error::new("table repair trust changed"));
        }
        let peer = peer.into();
        let group = group.filter(|group| group.depth.get() == 0);
        if write && let Some(group) = group {
            group.let_go();
            return group.within(|| self.observe(bootstrap, peer, expected, write, None, run));
        }
        if let Some(group) = group
            && let Some(look) = group.take()
        {
            return self.follow(bootstrap, look, peer, expected, group, run);
        }
        // Acquire every source before session admission and keep it through
        // this local batch. No guard survives the return to network I/O.
        let sources =
            crate::generation::local_source_reads_until(bootstrap.paths(), admission_deadline())?;
        match group {
            Some(group) => self.lead(bootstrap, sources, peer, expected, group, run),
            None => self.admit(bootstrap, sources, peer, expected, write, run),
        }
    }

    /// [`Self::observe`]'s read batch as its round group's first ([`GroupLook`]), admitted on
    /// `sources`, the vault sources read for it, as [`Self::admit`] admits a read, but with its
    /// session lent the look of this vault's source read in place of keeping it, which the group
    /// then keeps for the read admissions after it ([`GroupLook::keep`]), where the batch
    /// succeeded. A read of more than this vault's source (a successor generation's), a writer
    /// generation's candidate home (whose session observes its predecessors, which waits) or
    /// another vault than the group's is admitted as with no group, and leaves the group with
    /// nothing.
    fn lead<T>(
        &self,
        bootstrap: &Bootstrap,
        mut sources: Vec<aterm_secrets::client::CommonsSourceRead>,
        peer: Option<Actor>,
        expected: Option<[u8; 64]>,
        group: &GroupLook<'_>,
        run: impl FnOnce(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        if sources.len() != 1
            || crate::generation::is_candidate(bootstrap.paths())
            || bootstrap.paths() != group.paths
        {
            return self.admit(bootstrap, sources, peer, expected, false, run);
        }
        #[cfg(test)]
        count_grouped(Grouped::Own);
        #[cfg(test)]
        tests::session_boundary(0);
        let look = sources.remove(0).into_look(group.paths);
        let session = Session::observe_in(bootstrap, Some(&look))
            .map_err(|error| error.context("table repair session admission"))?;
        let result = self.lent(session, &look, peer, expected, group, run);
        if result.is_ok() {
            group.keep(bootstrap, look);
        }
        result
    }

    /// [`Self::observe`]'s read batch as a follower in its round group ([`GroupLook`]), admitted
    /// through `look`, the look the group kept, taken out of it for this batch: its own control
    /// journal read, then whether the look's observation still guards the state lock at the
    /// vault's path ([`aterm_secrets::client::VaultLook::guards_source`]) and the look still
    /// shows the control anchor as it first saw it ([`Bootstrap::anchor_stands_in`]), so what the
    /// state lock guards and the anchor that read read stand as of after the read
    /// ([`Session::observe_after_in`]); then its custody, access, generation guard, actor, peer
    /// and expected observation through the look, as a group's first judges them. Where the look
    /// no longer stands, it is dropped with that read, and the batch is admitted on a source read
    /// of its own as the group's first ([`Self::lead`]). The group keeps the look again where the
    /// batch succeeded.
    fn follow<'p, T>(
        &self,
        bootstrap: &Bootstrap,
        look: aterm_secrets::client::VaultLook<'p>,
        peer: Option<Actor>,
        expected: Option<[u8; 64]>,
        group: &GroupLook<'p>,
        run: impl FnOnce(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        #[cfg(test)]
        tests::session_boundary(0);
        let session = Session::observe_after_in(bootstrap, &look, || {
            look.guards_source() && bootstrap.anchor_stands_in(&look)
        })
        .map_err(|error| error.context("table repair session admission"))?;
        let Some(session) = session else {
            drop(look);
            #[cfg(test)]
            count_grouped(Grouped::Reopened);
            let sources = crate::generation::local_source_reads_until(
                bootstrap.paths(),
                admission_deadline(),
            )?;
            return self.lead(bootstrap, sources, peer, expected, group, run);
        };
        #[cfg(test)]
        count_grouped(Grouped::Lent);
        let result = self.lent(session, &look, peer, expected, group, run);
        if result.is_ok() {
            group.keep(bootstrap, look);
        }
        result
    }

    /// A round group's read batch ([`GroupLook`]) on its admitted `session`: the checks every
    /// admission makes at once after its session's open ([`Self::admitted`]) through `look`, the
    /// look lent to the session for the batch ([`Session::lend_look`]), and `run` inside the
    /// group, where an admission admits as with no group ([`GroupLook::within`]).
    fn lent<'s, T>(
        &self,
        mut session: Session<'s>,
        look: &'s aterm_secrets::client::VaultLook<'s>,
        peer: Option<Actor>,
        expected: Option<[u8; 64]>,
        group: &GroupLook<'_>,
        run: impl FnOnce(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        #[cfg(test)]
        tests::session_boundary(1);
        self.admitted(&session, look, peer, expected)?;
        session.lend_look(look)?;
        group.within(|| run(&session))
    }

    /// The checks an admission makes through `look` at once after its session's open: the
    /// session's actor is this replica's, `peer` where given a current member, and its
    /// observation the `expected` one, where given.
    fn admitted(
        &self,
        session: &Session<'_>,
        look: &aterm_secrets::client::VaultLook<'_>,
        peer: Option<Actor>,
        expected: Option<[u8; 64]>,
    ) -> Result<()> {
        if session.actor() != self.actor {
            return Err(Error::new("table repair local identity changed"));
        }
        if let Some(peer) = peer {
            session.peer_in(peer, Some(look))?;
        }
        if expected
            .is_some_and(|expected| session.observation_in(Some(look)).ok() != Some(expected))
        {
            return Err(Error::new("control history changed during table repair"));
        }
        Ok(())
    }

    /// [`Self::observe`]'s batch admitted on `sources`, the vault sources read for it, this
    /// vault's first, outside a round group ([`GroupLook`]).
    fn admit<T>(
        &self,
        bootstrap: &Bootstrap,
        mut sources: Vec<aterm_secrets::client::CommonsSourceRead>,
        peer: Option<Actor>,
        expected: Option<[u8; 64]>,
        write: bool,
        run: impl FnOnce(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        #[cfg(test)]
        tests::session_boundary(0);
        // Both sessions revalidate the exact vault observation and immutable
        // custody. Only a mutation waits for the control writer; reads use its
        // acknowledged prefix. No session survives this local batch. The session's
        // open, its peer and its observation stand for one moment of the vault, and
        // read through one look at it. A read's look is its batch's source read of the
        // vault (the first of `sources`, this vault's), which keeps every writer of the
        // state lock's files out from before the admission until the batch returns, and
        // its session keeps it: the batch's own checks (its currency, its stores'
        // contexts, its peers' roster) judge the moment its admission did, and its
        // store opens and store read batches hold the vault through it. A read admitted
        // through a round group (`Self::lead`, `Self::follow`) is lent its look instead:
        // its group's first's source read's, which the group keeps after the batch. A
        // write's look observes the lock beside the sources and ends before its batch
        // runs, whose checks each observe the vault again.
        let look = if write || sources.is_empty() {
            aterm_secrets::client::VaultLook::new(bootstrap.paths())
        } else {
            sources.remove(0).into_look(bootstrap.paths())
        };
        let mut session = if write {
            Session::open_until_in(bootstrap, admission_deadline(), Some(&look))
        } else {
            Session::observe_in(bootstrap, Some(&look))
        }
        .map_err(|error| error.context("table repair session admission"))?;
        #[cfg(test)]
        tests::session_boundary(1);
        self.admitted(&session, &look, peer, expected)?;
        if write {
            drop(look);
        } else {
            session.keep_look(look)?;
        }
        run(&session)
    }

    /// The bootstrap a round admits its batches through, and what its opening batch observed of
    /// the control history ([`Opened`]).
    ///
    /// Where this replica keeps a bootstrap of its trust, the opening batch is first tried on it
    /// with no wait anywhere, and the kept bootstrap judged through that batch's admission: one try
    /// at the vault's sources ([`crate::generation::try_local_source_reads`], which finds none
    /// while a writer holds one), for a home that is no writer generation's candidate (a
    /// candidate's session observes its predecessors' custody, which waits), admitted as every read
    /// batch is ([`Self::admit`]), whose batch asks first whether the kept bootstrap stands, partly
    /// through the look and the control view its admission took ([`Session::bootstrap_stands`], as
    /// below). Where the try admits and the bootstrap stands, the round takes it with that batch's
    /// observations. On any other outcome (no try, a source busy or absent, the admission refused,
    /// the bootstrap fallen) the try's sources and session drop with it, and the round runs as it
    /// did before the try: the bootstrap this replica keeps while it stands ([`Self::bootstrap`]),
    /// else a fresh open of its vault, and its opening batch on that with the admission's waits
    /// ([`Self::observe`]).
    ///
    /// The try judges the kept bootstrap's vault observation, its pinned files' digests and its
    /// control journal as its admission's look and read saw them, and the rest (the vault's
    /// folders, its custody locks and its creation claim) on its batch's first call, after the
    /// admission and its peer check, where the round judged all of it before the admission: a
    /// change landing between that earlier moment and the one that judges it now makes this round
    /// open afresh and admit, as the round did for a change landing before its judgement, where
    /// for one landing after it the round failed at its admission (or, for a change its admission
    /// does not judge, went on with the bootstrap that had fallen) and the next one opened afresh.
    ///
    /// Through `group`, the round's group look ([`GroupLook`]), the opening batch is its group's
    /// first, the try's ([`Self::lead`]) or the fallback's ([`Self::observe`]), and the group
    /// keeps its look after it where [`GroupLook::keep`] keeps one (the batch went through
    /// [`Self::lead`], not its fallback to [`Self::admit`], and succeeded, and its glance first saw
    /// the control anchor settled): no call on the round's stream separates the opening from the
    /// round's first admission, which is then lent that look.
    fn opening(
        &self,
        peer: Option<Actor>,
        group: Option<&GroupLook<'_>>,
    ) -> Result<(Arc<Bootstrap>, Opened)> {
        let opened =
            |session: &Session<'_>| Ok((session.observation()?, session.legacy_observation()?));
        let held = self
            .kept
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let stands = |session: &Session<'_>| {
            session.bootstrap_stands()?;
            opened(session)
        };
        if let Some(held) = held
            && held.trust() == self.trust
            && !crate::generation::is_candidate(held.paths())
            && let Ok(Some(sources)) = crate::generation::try_local_source_reads(held.paths())
            && let Ok(observed) = match group {
                Some(group) => self.lead(&held, sources, peer, None, group, stands),
                None => self.admit(&held, sources, peer, None, false, stands),
            }
        {
            #[cfg(test)]
            self.kept_takes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok((held, observed));
        }
        let bootstrap = self.bootstrap()?;
        let observed = self.observe(&bootstrap, peer, None, false, group, opened)?;
        Ok((bootstrap, observed))
    }

    /// The bootstrap a round admits its batches through where its opening's try on the one this
    /// replica keeps did not ([`Self::opening`]): the one this replica keeps while it stands
    /// ([`Bootstrap::stands`]), else a fresh open of its vault, kept in its place. A round keeps
    /// the one it took; another round's fresh open only replaces what this replica keeps.
    fn bootstrap(&self) -> Result<Arc<Bootstrap>> {
        let kept = || {
            self.kept
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        let held = kept().clone();
        if let Some(held) = held {
            if held.stands().is_ok() {
                return Ok(held);
            }
            let mut slot = kept();
            if slot.as_ref().is_some_and(|slot| Arc::ptr_eq(slot, &held)) {
                *slot = None;
            }
        }
        let bootstrap = Bootstrap::cached_until(&self.paths, admission_deadline())
            .map_err(|error| {
                Error::from(error).context("table repair bootstrap custody admission")
            })?
            .ok_or_else(|| Error::new("table repair lost secrets custody"))?;
        #[cfg(test)]
        self.bootstrap_opens
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bootstrap = Arc::new(bootstrap);
        *kept() = Some(Arc::clone(&bootstrap));
        Ok(bootstrap)
    }

    /// How many bootstraps [`Self::bootstrap`] opened afresh.
    #[cfg(test)]
    pub(crate) fn test_bootstrap_opens(&self) -> usize {
        self.bootstrap_opens
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// How many rounds [`Self::opening`]'s try took the kept bootstrap in.
    #[cfg(test)]
    pub(crate) fn test_kept_takes(&self) -> usize {
        self.kept_takes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// A round outside any round group ([`Self::round_in`]): the mesh's rounds, which no stream
    /// bounds, and a round a test builds directly. Each of its read batches keeps the look its own
    /// source read stands on ([`Session::keep_look`]).
    fn round(&self, peer: impl Into<Option<Actor>>) -> Result<Round<'_>> {
        self.round_in(peer, None)
    }

    /// A round whose admissions go through `group` where given, the group look of the stream the
    /// round exchanges on ([`GroupLook`], [`Wire`]): its read admissions with no call on that
    /// stream between them, its opening batch's first among them, share one look at the vault,
    /// where the group keeps it ([`GroupLook::keep`]).
    fn round_in<'a>(
        &'a self,
        peer: impl Into<Option<Actor>>,
        group: Option<&'a GroupLook<'a>>,
    ) -> Result<Round<'a>> {
        let policy = self
            .commons
            .as_ref()
            .map(|(_, config)| crate::catalog::policy::Policy::read(config))
            .transpose()?;
        let (bootstrap, (observation, legacy_observation)) = self.opening(peer.into(), group)?;
        Ok(Round {
            replica: self,
            started: Instant::now(),
            bootstrap,
            observation,
            legacy_observation,
            now: self.ring_now()?,
            policy,
            tables: self.tables.clone(),
            commons: self.commons.clone(),
            active_table: std::cell::Cell::new(None),
            inventory_read: std::cell::RefCell::new(None),
            group,
        })
    }

    /// Optional upgrade on an already authenticated, deadline-bounded channel.
    /// Legacy peers close here; the caller preserves their successful control
    /// report on any error. Application errors convey no convergence claim.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "NewMember",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "LostKey",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "Legacy",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "Retired",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "Damaged",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "LostAck",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "Retry",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "DamagedNeighbor",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    pub(crate) fn exchange(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        repair: &mut Repair,
    ) -> Result<Report> {
        self.exchange_with_policy(stream, peer, side, repair, ProbePolicy::Projection)
    }

    pub(crate) fn exchange_with_compatibility(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        repair: &mut Repair,
    ) -> Result<Report> {
        self.exchange_with_policy(stream, peer, side, repair, ProbePolicy::Compatible)
    }

    #[cfg(test)]
    pub(crate) fn exchange_legacy_projection(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        repair: &mut Repair,
    ) -> Result<Report> {
        self.exchange_with_policy(stream, peer, side, repair, ProbePolicy::Legacy)
    }

    fn exchange_with_policy(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        repair: &mut Repair,
        policy: ProbePolicy,
    ) -> Result<Report> {
        repair.deferral = None;
        repair.batch_deferral = None;
        repair.interrupted = None;
        repair.clock = None;
        // The round's read admissions with no call on `stream` between them share one look
        // (`GroupLook`); every call on it lets that look go first (`Wire`), and the group drops
        // here, before the control exchange's next network step.
        let group = GroupLook::new(&self.paths);
        let round = self.round_in(peer, Some(&group))?;
        round.exchange_with_policy(
            &mut Wire {
                stream,
                group: &group,
            },
            peer,
            side,
            repair,
            policy,
        )
    }
}

#[derive(Clone, Copy)]
enum BatchKind {
    Operation,
    Conflict,
}

/// Which store path a record the mesh delivered takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A table operation ([`Store::ingest`]).
    Operation,
    /// A conflict certificate ([`Store::ingest_conflict`]).
    Conflict,
}

/// What became of one record [`Replica::mesh_ingest`] was handed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Entered (or was already held): its receipt.
    Applied(Receipt),
    /// Its writer or roster is not in this Mac's view yet: try again after a membership change.
    Waiting(String),
    /// Refused for good (bad signature, policy, conflicted writer, a table this Mac does not
    /// hold or subscribe).
    Refused(String),
    /// Local contention (busy, snapshot moved): nothing changed, try again.
    Busy(String),
}
impl Verdict {
    fn of(error: &Error) -> Self {
        if error.is_busy() {
            Self::Busy(error.to_string())
        } else if error.is_awaiting_membership() {
            Self::Waiting(error.to_string())
        } else {
            Self::Refused(error.to_string())
        }
    }
}

/// The mesh's entry to this Mac's tables: the same admission and store checks a repair batch
/// runs, with no channel peer. The relay's broker already refused a record from an origin that
/// is not a seat; each record's own writer grant is what admits it here.
impl Replica {
    /// Ingest up to `BATCH` wires of one table that the mesh delivered, each with `Via::Mesh`.
    /// One admission per call. A storage failure answers every wire `Busy` or `Refused` as the
    /// error classifies.
    ///
    /// Each admission of the call (the table selection's, then the batch's) rechecks custody,
    /// this Mac's actor and the Commons policy captured when the call began; a control change
    /// between them is not refused, as a round's pinned probe observation would be, because no
    /// peer shares this view.
    ///
    /// # Errors
    /// More than [`BATCH`] wires.
    pub fn mesh_ingest(
        &self,
        table: Digest,
        kind: Kind,
        wires: &[Vec<u8>],
    ) -> Result<Vec<Verdict>> {
        if wires.len() > BATCH {
            return Err(Error::repair(
                "mesh table batch exceeds its bound",
                aterm_types::network_state::RepairCause::Quota,
            ));
        }
        if wires.is_empty() {
            return Ok(Vec::new());
        }
        let every = |verdict: Verdict| vec![verdict; wires.len()];
        let round = match self.mesh_round(table) {
            Ok(Some(round)) => round,
            Ok(None) => {
                return Ok(every(Verdict::Refused(
                    "table is not held or subscribed on this Mac".into(),
                )));
            }
            Err(error) => return Ok(every(Verdict::of(&error))),
        };
        let max = match kind {
            Kind::Operation => crate::crypto::MAX_OPERATION,
            Kind::Conflict => crate::table::conflict::MAX_CONFLICT,
        };
        let admitted = round.local_write(None, |session| {
            let mut store = match round.store(session, table) {
                Ok(store) => store,
                Err(error) => return Ok(every(Verdict::of(&error))),
            };
            Ok(wires
                .iter()
                .map(|wire| {
                    if wire.len() > max {
                        return Verdict::Refused("table record exceeds its bound".into());
                    }
                    let receipt = match kind {
                        Kind::Operation => store.ingest_via(session, wire, Via::Mesh),
                        Kind::Conflict => store.ingest_conflict_via(session, wire, Via::Mesh),
                    };
                    match receipt {
                        Ok(receipt) if receipt.id == Sha256::digest(wire) => {
                            Verdict::Applied(receipt)
                        }
                        Ok(_) => Verdict::Refused(
                            "table receipt did not name the delivered record".into(),
                        ),
                        Err(error) => Verdict::of(&error),
                    }
                })
                .collect())
        });
        let verdicts = admitted.unwrap_or_else(|error| every(Verdict::of(&error)));
        if kind == Kind::Operation {
            let mut backfill = self.backfill.lock().unwrap_or_else(|p| p.into_inner());
            for verdict in &verdicts {
                if let Verdict::Applied(receipt) = verdict {
                    backfill.retained(table, receipt.id);
                }
            }
        }
        Ok(verdicts)
    }

    /// The digests of `table`'s operations this Mac holds (`store.inventory`), `None` for a table
    /// it does not hold.
    ///
    /// # Errors
    /// Custody, admission or the store's read fails, other than the table being absent.
    pub fn mesh_inventory(&self, table: Digest) -> Result<Option<BTreeSet<Digest>>> {
        let Some(round) = self.mesh_round(table)? else {
            return Ok(None);
        };
        round.local(None, |session| match round.store(session, table) {
            Ok(store) => Ok(Some(store.inventory_unchecked().copied().collect())),
            Err(error) if error.is_busy() => Err(error),
            Err(_) => Ok(None),
        })
    }

    /// The wire of operation `id` of `table`, where held, for each of `ids` in order; an id this
    /// Mac does not hold is left out.
    ///
    /// # Errors
    /// Custody, admission or the store's read fails.
    pub fn mesh_wires(&self, table: Digest, ids: &[Digest]) -> Result<Vec<Vec<u8>>> {
        let Some(round) = self.mesh_round(table)? else {
            return Ok(Vec::new());
        };
        round.local(None, |session| {
            let store = round.store(session, table)?;
            let mut wires = Vec::new();
            for id in ids {
                if let Some(wire) = store.wire_unchecked(id) {
                    wires.push(wire.to_vec());
                }
            }
            Ok(wires)
        })
    }

    /// The conflict proofs `table`'s store holds, by key; `None` where this Mac does not select
    /// the table or its store is not open to it.
    ///
    /// # Errors
    /// Custody, admission or a busy store.
    pub fn mesh_proofs(&self, table: Digest) -> Result<Option<BTreeSet<ProofKey>>> {
        let Some(round) = self.mesh_round(table)? else {
            return Ok(None);
        };
        round.local(None, |session| match round.store(session, table) {
            Ok(store) => Ok(Some(store.proof_inventory_unchecked())),
            Err(error) if error.is_busy() => Err(error),
            Err(_) => Ok(None),
        })
    }

    /// The wire of conflict proof `key` of `table`, where held, for each of `keys` in order; a
    /// key this Mac does not hold is left out.
    ///
    /// # Errors
    /// Custody, admission or the store's read fails.
    pub fn mesh_proof_wires(&self, table: Digest, keys: &[ProofKey]) -> Result<Vec<Vec<u8>>> {
        let Some(round) = self.mesh_round(table)? else {
            return Ok(Vec::new());
        };
        round.local(None, |session| {
            let store = round.store(session, table)?;
            let mut wires = Vec::new();
            for key in keys {
                if let Some(wire) = store.proof_wire_unchecked(session, key)? {
                    wires.push(wire);
                }
            }
            Ok(wires)
        })
    }

    /// Whether this Mac takes `table`'s records now, held or not (the mesh's `Inventory` asks
    /// before a neighbour gives it a table it does not hold): a table it selects, which a
    /// declaration that entered a moment ago subscribes; or a table of a ring's current window it
    /// has no storage for, which it takes up here as a repair round takes up one a peer offers
    /// (`adopt_ring_tables`: held durably under the control writer, local policy honoured), so the
    /// next selection lists it. A take-up that does not happen now (policy, no storage slot left,
    /// recovery, a writer it could not get) answers `false`, and the table is offered again on the
    /// next exchange, as a round's is.
    ///
    /// # Errors
    /// Custody, admission, the subscription read or the ring window's read fails.
    pub fn mesh_takes(&self, table: Digest) -> Result<bool> {
        let mut round = self.round(None)?;
        round.select_tables(None, &mut Report::default())?;
        if round.tables.contains_key(&table) {
            return Ok(true);
        }
        let (Some((directory, _)), Some(policy)) = (&round.commons, &round.policy) else {
            return Ok(false);
        };
        if directory.is_recovery() {
            return Ok(false);
        }
        let now = round.now;
        if !round.local(None, |session| directory.ring_permits(session, table, now))? {
            return Ok(false);
        }
        let offered = BTreeSet::from([table]);
        let taken = round.local_write(None, |session| {
            directory.adopt_ring_tables(session, policy, now, &offered)
        });
        Ok(taken.is_ok_and(|taken| taken.contains_key(&table)))
    }

    /// The tables this replica selects (its subscriptions), by digest.
    ///
    /// # Errors
    /// Custody, admission or the subscription read fails.
    pub fn mesh_tables(&self) -> Result<BTreeSet<Digest>> {
        let mut round = self.round(None)?;
        round.select_tables(None, &mut Report::default())?;
        Ok(round.tables.keys().copied().collect())
    }

    /// Each selected table with its operation digests and conflict proof keys, read in one
    /// catalog selection and one store open per table.
    ///
    /// # Errors
    /// Custody, admission, the subscription read, or a busy store fails.
    pub fn mesh_tables_summary(&self) -> Result<Vec<MeshTableSummary>> {
        let mut round = self.round(None)?;
        round.select_tables(None, &mut Report::default())?;
        let tables: Vec<Digest> = round.tables.keys().copied().collect();
        round.local(None, |session| {
            let mut out = Vec::with_capacity(tables.len());
            for &table in &tables {
                round.active_table.set(Some(table));
                match round.store(session, table) {
                    Ok(store) => {
                        let ids = store.inventory_unchecked().copied().collect();
                        let proofs = store.proof_inventory_unchecked();
                        out.push((table, ids, proofs));
                    }
                    Err(error) if error.is_busy() => return Err(error),
                    Err(_) => {
                        out.push((table, BTreeSet::new(), BTreeSet::new()));
                    }
                }
            }
            Ok(out)
        })
    }

    /// A round with no channel peer that has selected this Mac's tables, with `table` its active
    /// one; `None` where this Mac does not select `table`.
    fn mesh_round(&self, table: Digest) -> Result<Option<Round<'_>>> {
        let mut round = self.round(None)?;
        round.select_tables(None, &mut Report::default())?;
        if !round.tables.contains_key(&table) {
            return Ok(None);
        }
        round.active_table.set(Some(table));
        Ok(Some(round))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProbePolicy {
    Projection,
    Compatible,
    /// The released 0.146 reader and writer, with current local authorization.
    #[cfg(test)]
    Legacy,
}

#[derive(Debug)]
enum ProbeFailure {
    Closed,
    Mismatch(ProbeMismatch),
    Io(std::io::Error),
}

impl ProbeFailure {
    fn io(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            Self::Closed
        } else {
            Self::Io(error)
        }
    }

    fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Closed | Self::Mismatch(ProbeMismatch::History { .. })
        )
    }

    fn into_error(self) -> Error {
        match self {
            Self::Closed => Error::new("application probe closed before agreement"),
            Self::Mismatch(reason) => reason.error(),
            Self::Io(error) => error.into(),
        }
    }
}

enum OfferedIds {
    All,
    Only(BTreeSet<Digest>),
    Except(BTreeSet<Digest>),
}

struct Inventory {
    known: BTreeSet<Digest>,
    offered: OfferedIds,
    offered_count: usize,
    observation: InventoryObservation,
    /// The store's custody as this read found it, which the round's matching inventory stands
    /// on where it holds ([`Round::fenced_observation`]).
    fence: Option<Fence>,
}

impl Inventory {
    fn new(
        known: BTreeSet<Digest>,
        excluded: BTreeSet<Digest>,
        observation: InventoryObservation,
        fence: Option<Fence>,
    ) -> Self {
        let (offered_count, offered) = if known.is_empty() || excluded.is_empty() {
            (known.len(), OfferedIds::All)
        } else if excluded.len() <= known.len() / 2 {
            // At most E of N known IDs can be excluded. Here E <= N/2, so the mask
            // is no larger than the offer; keep it and count only their intersection.
            let count = known.len() - known.intersection(&excluded).count();
            if count == known.len() {
                (count, OfferedIds::All)
            } else {
                (count, OfferedIds::Except(excluded))
            }
        } else if known.is_disjoint(&excluded) {
            (known.len(), OfferedIds::All)
        } else {
            // A dense mask can leave a sparse offer. Build that set once rather than
            // counting its IDs and then visiting them again to materialize the same set.
            let ids: BTreeSet<_> = known.difference(&excluded).copied().collect();
            (ids.len(), OfferedIds::Only(ids))
        };
        Self {
            known,
            offered,
            offered_count,
            observation,
            fence,
        }
    }

    fn offered_summary(&self) -> InventorySummary {
        match &self.offered {
            OfferedIds::All => self.observation.inventory,
            OfferedIds::Only(ids) if ids.is_empty() => InventorySummary::empty(),
            OfferedIds::Only(ids) => InventorySummary::of(ids),
            OfferedIds::Except(excluded) => {
                InventorySummary::of_sorted(self.offered_count, self.known.difference(excluded))
            }
        }
    }

    /// Reuse this admitted sample's canonical fingerprint only for the exact same ID set.
    /// This compares retained inventories, never the offered subset or authority to use them.
    fn peer_summary(&self, peer: &BTreeSet<Digest>) -> InventorySummary {
        if *peer == self.known {
            self.observation.inventory
        } else {
            InventorySummary::of(peer)
        }
    }

    fn offered_cycle(&self, after: Option<Digest>) -> impl Iterator<Item = &Digest> {
        use std::ops::Bound::{Excluded, Included, Unbounded};
        (self.offered_count != 0)
            .then(|| {
                let tail = self.offered_range(match after {
                    Some(after) => (Excluded(after), Unbounded),
                    None => (Unbounded, Unbounded),
                });
                let head = after
                    .into_iter()
                    .flat_map(move |after| self.offered_range((Unbounded, Included(after))));
                tail.chain(head)
            })
            .into_iter()
            .flatten()
    }

    fn offered_range(
        &self,
        bounds: (std::ops::Bound<Digest>, std::ops::Bound<Digest>),
    ) -> impl Iterator<Item = &Digest> {
        let (included, exclusions) = match &self.offered {
            OfferedIds::All => (&self.known, None),
            OfferedIds::Only(ids) => (ids, None),
            OfferedIds::Except(excluded) => (&self.known, Some(excluded)),
        };
        let all = exclusions.is_none();
        let mut excluded = exclusions
            .map(|excluded| excluded.range(bounds))
            .into_iter()
            .flatten()
            .peekable();
        included.range(bounds).filter(move |id| {
            if all {
                return true;
            }
            // Both ranges are sorted. Advance each excluded ID at most once instead of searching the
            // exclusion tree again for each retained ID.
            loop {
                match excluded.peek().map(|blocked| (*blocked).cmp(*id)) {
                    Some(std::cmp::Ordering::Less) => {
                        excluded.next();
                    }
                    Some(std::cmp::Ordering::Equal) => {
                        excluded.next();
                        return false;
                    }
                    Some(std::cmp::Ordering::Greater) | None => return true,
                }
            }
        })
    }
}

/// What a round's opening batch observed of the control history (`Replica::opening`): its
/// session's observation and legacy observation, the round keeps as its own (`Round`'s
/// `observation` and `legacy_observation`).
type Opened = ([u8; 64], Option<[u8; 64]>);

struct Round<'a> {
    replica: &'a Replica,
    /// When this side began the round, before its probe: a peer builds its mark after this.
    started: Instant,
    /// What its batches are admitted through ([`Replica::opening`]).
    bootstrap: Arc<Bootstrap>,
    observation: [u8; 64],
    legacy_observation: Option<[u8; 64]>,
    /// The ring windows' clock for this whole exchange.
    now: u64,
    policy: Option<crate::catalog::policy::Policy>,
    tables: BTreeMap<Digest, (Scope, PathBuf)>,
    /// Locally selected storage; switching to recovery never changes membership or the actor.
    commons: Option<(crate::catalog::Directory, PathBuf)>,
    active_table: std::cell::Cell<Option<Digest>>,
    /// The table's operation inventory as the round read it, with that read's fence.
    inventory_read: std::cell::RefCell<Option<(InventoryObservation, Option<Fence>)>>,
    /// The group look its admissions go through ([`GroupLook`]): a round on a stream's, `None`
    /// for the mesh's and a round a test builds directly ([`Replica::round`]).
    group: Option<&'a GroupLook<'a>>,
}

/// One look at a replica's vault ([`aterm_secrets::client::VaultLook`]) that a round's read
/// admissions share while no call on the round's stream separates them: a round group. The
/// group's first read admission takes its look from its own source read of the vault, as every
/// read batch does, and is lent it ([`Session::lend_look`]); the group keeps it after that batch
/// returns, where its glance first saw the control anchor settled
/// ([`Bootstrap::anchor_settled_in`], no `lstat` taken), so an anchor that has not settled starts
/// no follower ([`Replica::lead`]). Each read admission after it, a follower, is lent that look in
/// place of a source read and a look of its own ([`Replica::follow`]), and makes the rest of its
/// admission as any read: its own control journal read; then, through the look, whether its
/// observation still guards the state lock at the vault's path
/// ([`aterm_secrets::client::VaultLook::guards_source`]) and it still shows the control anchor as
/// its glance first saw it ([`Bootstrap::anchor_stands_in`]), a fresh `lstat` of each, where
/// either does not the look dropped and the batch admitted as a group's first; then its custody,
/// access and generation guard, its actor, peer and expected observation, the round's policy and,
/// where its gate asks, the table's subscription ([`Round::admit`]).
///
/// The group lets its look go at every call on the round's stream ([`Wire`]), before every write
/// admission, at any read batch's error (a follower takes the look out of the group for its batch,
/// and the group keeps it again only after a batch that succeeded), and at the round's end, where
/// it drops (`Replica::exchange_with_policy`). An admission inside another's batch admits as with
/// no group ([`Self::within`]). A loop of admissions whose length the peer or the table count sets
/// ends a group at each of them: each witness read ([`Round::table_witnesses`], one admission for
/// each table both sides list) lets the group's look go before its admission and after it, so no
/// group spans a read of every listed table. Every other loop of a round admits once, or calls the
/// stream at each turn, except `Round::local_gated`'s retries, which admit up to [`LOCAL_READS`]
/// times with no stream call between them: each admission after its first follows one that
/// failed, after which the group keeps no look, so each admits as a group's first. A group is so
/// the in-process work between two calls on the stream, with no witness read inside it. The loops
/// it can span between its admissions are in memory and make no admission and no I/O, each over
/// at most [`MAX_TABLES`] table IDs (as the responder's `witness::split` of a peer's list, and its
/// common tables) or [`BATCH`] items (as `Round::receive_batch`'s backfill hints and receipts for
/// a received batch's records).
///
/// Against each read admission taking its own look, this changes:
/// - BC-34a, a state-lock writer waits for the group. A taker of the vault's state lock that could
///   take it between two read admissions no stream call separates finds it held shared until the
///   group's next stream call, error or end. A blocking writer waits for the group plus any shared
///   holds that overlap it. The takers that make one try and do not wait (`aterm-secrets`'
///   `Admin::retire_commons_members` through `StateLock::try_take`, `network_refresh::retain` and
///   `http204_observation::retain` through `StateLock::try_take_existing`,
///   `waiting::forget_unsent` and the waiting view's `StateLock::try_take`) meet Busy more often
///   only by the gaps between a group's admissions that they could have entered before. A blocking
///   writer's outcome is a run of today's with the writer arriving after the group's last
///   admission. A one-try taker's Busy is a run of today's with the taker trying during one of the
///   group's admissions.
/// - BC-34b, a change made without the state lock is seen at the next group, not the next
///   admission. A change by no holder of the vault's state lock (neither the lock the group holds
///   shared nor one at a lock file put in its place, which a follower sees) to a file the look
///   stamps or judges (the vault's folder made a symlink, replaced, or given another mode or
///   owner; the lock file given another mode, owner or link count; the memo inputs, the records or
///   `state.toml` written by a process that ignores the lock; the identity, the device age
///   identity and claim, under the Commons custody locks, the fold mark, the Commons claim and the
///   default pin), landing after the group's first admission looked at it, is seen at the next
///   group's first admission, or sooner at a fresh check within the group (a store read batch's
///   opening fence and closing look). A follower's reuse of a kept digest is judged against the
///   group's first `lstat` of the file, at most one group old. A refusal of such a change can move
///   past the stream call that ends the group, and carries the stage context of the admission that
///   makes it. A lock file replaced or removed, and the control anchor, are not among these: each
///   follower looks at them afresh.
/// - BC-34c, at a follower the generation link is checked after its control journal read and its
///   custody, by its session's generation guard, where a group's first checks it in its source read
///   before either: a different check can refuse first, and the link's refusal is the same failure,
///   under the session admission's context ("table repair session admission").
struct GroupLook<'p> {
    /// The replica's vault: only a look at it is kept.
    paths: &'p Paths,
    look: std::cell::Cell<Option<aterm_secrets::client::VaultLook<'p>>>,
    /// How many admissions through this group run inside one another's batch: one inside a batch
    /// admits as with no group.
    depth: std::cell::Cell<u32>,
}

impl<'p> GroupLook<'p> {
    const fn new(paths: &'p Paths) -> Self {
        Self {
            paths,
            look: std::cell::Cell::new(None),
            depth: std::cell::Cell::new(0),
        }
    }

    /// Drop the look this group keeps, if any, and the shared hold of the vault's state lock it
    /// stands on.
    fn let_go(&self) {
        drop(self.look.take());
    }

    /// The look this group keeps, taken out of it for a follower's batch.
    fn take(&self) -> Option<aterm_secrets::client::VaultLook<'p>> {
        self.look.take()
    }

    /// Keep `look`, a group admission's after its batch succeeded, for the next read admission,
    /// where its glance first saw `bootstrap`'s control anchor settled
    /// ([`Bootstrap::anchor_settled_in`]); otherwise drop it.
    fn keep(&self, bootstrap: &Bootstrap, look: aterm_secrets::client::VaultLook<'p>) {
        if bootstrap.anchor_settled_in(&look) {
            self.look.set(Some(look));
        }
    }

    /// `run`, where any admission through this group admits as with no group: a batch's own run.
    fn within<T>(&self, run: impl FnOnce() -> T) -> T {
        self.depth.set(self.depth.get() + 1);
        let result = run();
        self.depth.set(self.depth.get() - 1);
        result
    }
}

/// A round's stream: each call lets the round's group look go first ([`GroupLook::let_go`]), then
/// makes the same call on the stream, so the stream sees the calls it would without the group and
/// no shared hold of the vault crosses network I/O. A method this does not forward is the
/// default, made of `read` or `write` calls, which let go too.
struct Wire<'w, 'p, S> {
    stream: &'w mut S,
    group: &'w GroupLook<'p>,
}

impl<S: Read> Read for Wire<'_, '_, S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.group.let_go();
        self.stream.read(buf)
    }

    fn read_vectored(&mut self, bufs: &mut [std::io::IoSliceMut<'_>]) -> std::io::Result<usize> {
        self.group.let_go();
        self.stream.read_vectored(bufs)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        self.group.let_go();
        self.stream.read_exact(buf)
    }

    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        self.group.let_go();
        self.stream.read_to_end(buf)
    }

    fn read_to_string(&mut self, buf: &mut String) -> std::io::Result<usize> {
        self.group.let_go();
        self.stream.read_to_string(buf)
    }
}

impl<S: Write> Write for Wire<'_, '_, S> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.group.let_go();
        self.stream.write(buf)
    }

    fn write_vectored(&mut self, bufs: &[std::io::IoSlice<'_>]) -> std::io::Result<usize> {
        self.group.let_go();
        self.stream.write_vectored(bufs)
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.group.let_go();
        self.stream.write_all(buf)
    }

    fn write_fmt(&mut self, args: std::fmt::Arguments<'_>) -> std::io::Result<()> {
        self.group.let_go();
        self.stream.write_fmt(args)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.group.let_go();
        self.stream.flush()
    }
}

/// How a read admission through a round group was made ([`GroupLook`]), counted on its thread
/// ([`GROUPED`]).
#[cfg(test)]
#[derive(Clone, Copy)]
enum Grouped {
    /// On its own source read and look, as its group's first: the opening batch, the first after a
    /// stream call, error or write, and a follower's re-admission.
    Own,
    /// Lent its group's look, a follower.
    Lent,
    /// A follower whose look no longer stood after its journal read, re-admitted as a first.
    Reopened,
}

#[cfg(test)]
thread_local! {
    /// The read admissions through a round group on this thread, by how each was made
    /// ([`Grouped`]): own, lent, reopened.
    static GROUPED: std::cell::Cell<(u32, u32, u32)> = const { std::cell::Cell::new((0, 0, 0)) };
}

#[cfg(test)]
fn count_grouped(how: Grouped) {
    GROUPED.with(|grouped| {
        let (own, lent, reopened) = grouped.get();
        grouped.set(match how {
            Grouped::Own => (own + 1, lent, reopened),
            Grouped::Lent => (own, lent + 1, reopened),
            Grouped::Reopened => (own, lent, reopened + 1),
        });
    });
}

/// What a round's admission ([`Round::admit`]) checks besides its session, peer and observation.
/// Every admission checks the local policy: Commons enabled, the policy the round captured
/// unchanged, the catalog not excluded. The active table's subscription
/// ([`crate::catalog::Directory::permits`], which reads the catalog for an application table
/// outside the ring windows) is checked where the table's data leaves or enters this Mac: at
/// [`Gate::Subscription`] admissions, and at each batch's send boundary, which checks it inside
/// the batch's [`Gate::Policy`] admission once the batch is prepared ([`Round::send_batch`]).
/// Below, either is a subscription check. A quiet application round of a Commons catalog read
/// the catalog for it 13 times on each side, at every admission and again at each send boundary,
/// and reads it 5 times: at the conflict and operation inventories' authorizes, each batch's send
/// boundary and the matching inventory's read (2026-11-11).
///
/// Against checking the subscription at every admission, this changes:
/// - BC1, where a refusal lands. A withdrawal of the table (by a catalog conflict, a ring
///   retirement or the storage listing), or an error reading those, that a `Policy` admission
///   meets is refused at the next subscription check, with the same message, and no more of the
///   table is sent before it. Its stage changes: "storage admission" becomes "storage enrollment"
///   or "conflicts", "operation inventory" becomes "inventory exchange", the initiator's empty
///   conflict batch's "conflicts" becomes "inventory exchange", and the responder's "operation
///   receive" becomes "operation send". A send whose preparation fails while the table is
///   withdrawn meets the withdrawal's refusal ([`Round::send_batch`]): withdrawn before the
///   batch's admission, as when that admission checked the subscription, and, newly, withdrawn
///   during the preparation, where that admission's check, made before preparing, let the
///   preparation's own error through.
/// - BC2, a withdrawal that lifts before the next subscription check no longer fails the round;
///   every send still stands on its own check.
/// - BC3, catalog read errors at `Policy` admissions. At the enrollment read and the empty batches'
///   admissions, busy, damage or a catalog that keeps advancing failed the round and no longer
///   does. At `retain_primary`'s read and the proof and operation inventory reads, a catalog that
///   keeps advancing deferred the table and no longer does, and busy or damage failed the round
///   and no longer does. Under sustained churn the round still fails, at the next subscription
///   check, after [`LOCAL_READS`] reads (the matching inventory's read gives up its match
///   instead, BC4); a burst that only those reads meet now repairs instead of deferring or failing.
/// - BC4, the initiator's last receive of an empty batch. A withdrawal visible only there no
///   longer fails the round, which reads as completed. After a quiet step the matching
///   inventory's reads check the subscription and refuse it, so no match is published. A step
///   that moved records reads no matching inventory (the inventories differ, and
///   [`Round::matching_inventory`] returns `None` before any admission) and checks nothing more,
///   since nothing of the table leaves this Mac after that receive.
/// - BC5, a `Policy` admission reads a withdrawn table's store (its claim, inventories, batch
///   wires) before the next subscription check refuses: a `Subscription` admission, or, for a
///   batch's wires, the send boundary inside the batch's own admission. These are read sessions,
///   which write nothing, each authenticated under its own session, their results dropped on the
///   refusal. What those reads put in the report (`recovery`, a deferral's local error) can differ
///   in an interrupted round's report.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Gate {
    /// The local policy and the active table's subscription: every write admission, the last
    /// admission before each inventory or receipt send, the matching inventory's read the runtime
    /// publishes, the witness reads, every mesh entry and every admission before a table is
    /// active. The default ([`Round::local`]). A batch's send is checked at its send boundary
    /// instead, inside its `Policy` admission ([`Round::send_batch`]).
    Subscription,
    /// The local policy alone ([`Round::local_policy_only`]): a read whose result leaves this Mac
    /// only after a check of the table's subscription, with nothing of the table sent or written
    /// in between, or never. That check is a later `Subscription` admission of this round and
    /// table, or a batch's send boundary ([`Round::send_batch`]): the batch's own, within the same
    /// admission, or a later batch's.
    Policy,
}

impl std::ops::Deref for Round<'_> {
    type Target = Replica;
    fn deref(&self) -> &Replica {
        self.replica
    }
}
impl Round<'_> {
    #[cfg(test)]
    fn exchange(
        self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        repair: &mut Repair,
    ) -> Result<Report> {
        self.exchange_with_policy(stream, peer, side, repair, ProbePolicy::Projection)
    }

    /// Table `id`'s store, opened in `session`. In a read session (each [`Self::local`]'s or
    /// [`Self::local_policy_only`]'s), every path ends in an open whose last step checks the store
    /// (`Store::check`: the session's currency and the enrollment claim), so the reads that follow
    /// it in the same closure stand on that check and take the store's unchecked accessors: a
    /// checked one would check the store again for nothing, once more for each store a quiet table
    /// round opens (2026-11-09). A control writer's `Store::open` seals its pending writes after
    /// that check, so this holds of read sessions only.
    fn store(&self, session: &Session<'_>, id: Digest) -> Result<Store> {
        let (scope, path) = &self.tables[&id];
        if let Some((directory, _)) = &self.commons
            && *scope != directory.catalog_scope()
        {
            // Keep an established recovery store, including its negative evidence.
            // Otherwise catalog damage alone must not split a healthy app replica.
            let recovery = directory.recovery();
            if let Some(store) = recovery.observe_store_if_exists(session, *scope)? {
                return if session.has_control_writer() {
                    recovery.open_store(session, *scope)
                } else {
                    Ok(store)
                };
            }
            if directory.is_recovery() {
                let primary = directory.primary();
                match primary.observe_store_if_exists(session, *scope) {
                    Ok(Some(store)) => {
                        return if session.has_control_writer() {
                            primary.open_store(session, *scope)
                        } else {
                            Ok(store)
                        };
                    }
                    Err(error) if error.is_busy() => return Err(error),
                    _ => {}
                }
            }
        }
        if let Some((directory, _)) = &self.commons
            && session.has_control_writer()
        {
            directory.enroll(session, *scope)
        } else if let Some((directory, _)) = &self.commons {
            directory.open_store(session, *scope)
        } else {
            Store::open(session, path, *scope)
        }
    }

    fn prepare_batch<K>(
        &self,
        session: &Session<'_>,
        id: Digest,
        keys: &[K],
        mut encode: impl FnMut(&Session<'_>, &Store, &K) -> Result<Vec<u8>>,
    ) -> Result<Vec<Vec<u8>>> {
        // The sender supplies its current admitted session. No table contents
        // are needed when the outgoing batch has no keys to encode.
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let store = self.store(session, id)?;
        keys.iter()
            .map(|key| encode(session, &store, key))
            .collect()
    }

    fn retain_primary(&self, peer: Actor, id: Digest) -> Result<bool> {
        let Some((directory, _)) = &self.commons else {
            return Ok(true);
        };
        let scope = self.tables[&id].0;
        if scope == directory.catalog_scope() {
            return Ok(true);
        }
        let primary = directory.primary();
        let recovery = directory.recovery();
        // Whether to retain is no table data: the write below and the conflict round's
        // authorize check the subscription (`Gate`).
        let needs = self.local_policy_only(peer, |session| {
            // With no recovery store there is nothing to retain into, and its claim alone says so
            // (`Store::enrollment_region`), so the primary is not opened here: a primary that
            // would refuse busy (`FailureKind::Busy`; a snapshot that changed was already deferred
            // by this check's caller, `Self::table_read`) beside no recovery store is not this
            // check's refusal, which would fail the round, but the proof inventory read's that
            // follows, which defers. Beside a recovery store, or a recovery claim that cannot be
            // judged, the primary is read first.
            if matches!(
                recovery.enrollment(session, scope),
                Ok(Enrollment::Absent | Enrollment::Pending)
            ) {
                return Ok(false);
            }
            let source = match primary.observe_store_if_exists(session, scope) {
                Ok(Some(source)) => source,
                Err(error) if error.is_busy() => return Err(error),
                _ => return Ok(false),
            };
            let Some(target) = recovery.observe_store_if_exists(session, scope)? else {
                return Ok(false);
            };
            Ok(!target.retains_replica_data(session, &source)?)
        })?;
        if !needs {
            return Ok(true);
        }
        self.local_write(peer, |session| {
            let source = match primary.observe_store_if_exists(session, scope) {
                Ok(Some(source)) => source,
                Err(error) if error.is_busy() => return Err(error),
                _ => return Ok(true),
            };
            let mut target = recovery.open_store(session, scope)?;
            target.retain_replica_batch(session, &source, BATCH)
        })
    }

    /// One admitted read of this Mac's state, which checks the active table's subscription
    /// ([`Gate::Subscription`]): the catalog is read for it only at such admissions and at a
    /// batch's send boundary ([`Self::send_batch`]). A store's observer refuses a witness that
    /// advances while it reads ([`Error::snapshot_changed`]). Another of this Mac's rounds
    /// ingesting into that table does just that, as does one ingesting into the catalog, which a
    /// Commons round's subscription check reads once it has selected an application table outside
    /// the ring window ([`Self::check_table_policy`]). A fresh admission and read is the answer,
    /// where giving up would cost both peers this round. [`LOCAL_READS`] bounds the reads: no
    /// sleep separates them, though each still waits for its own admission.
    fn local<T>(
        &self,
        peer: impl Into<Option<Actor>>,
        run: impl FnMut(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        self.local_gated(peer, Gate::Subscription, run)
    }

    /// [`Self::local`], its admissions checking the local policy alone ([`Gate::Policy`]): a read
    /// whose result leaves this Mac only after a check of the table's subscription, with nothing
    /// of the table sent or written in between, or never.
    fn local_policy_only<T>(
        &self,
        peer: impl Into<Option<Actor>>,
        run: impl FnMut(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        self.local_gated(peer, Gate::Policy, run)
    }

    fn local_gated<T>(
        &self,
        peer: impl Into<Option<Actor>>,
        gate: Gate,
        mut run: impl FnMut(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        let peer = peer.into();
        for _ in 1..LOCAL_READS {
            match self.admit(peer, false, gate, &mut run) {
                Err(error) if error.is_snapshot_changed() => {}
                result => return result,
            }
        }
        self.admit(peer, false, gate, run)
    }

    /// A read's result where the round sends an inventory next: a table that kept advancing
    /// through every read is deferred with `refusal`, as any other store this side cannot read
    /// now, and the round goes on, because an inventory can say it is unavailable and the peer
    /// defers on that too. No other read defers like this. A batch cannot say so
    /// ([`Self::send_batch`]), and a bare admission, such as the authorize that runs next, refuses
    /// what it could not authorize, so each of those still costs both peers the round.
    fn table_read<T>(
        result: Result<Option<T>>,
        refusal: &mut Option<String>,
        cause: &mut Option<aterm_types::network_state::RepairCause>,
    ) -> Result<Option<T>> {
        match result {
            Err(error) if error.is_snapshot_changed() => {
                *refusal = Some(error.to_string());
                *cause = Some(error.repair_cause());
                Ok(None)
            }
            result => result,
        }
    }

    /// One admitted write of this Mac's state, which checks the active table's subscription: no
    /// write is admitted on the local policy alone ([`Gate`]).
    fn local_write<T>(
        &self,
        peer: impl Into<Option<Actor>>,
        run: impl FnOnce(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        self.admit(peer, true, Gate::Subscription, run)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRepairPolicy",
            action = "Attempt",
            project = "aterm_db::replica::tests::policy::policy_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRepairPolicy",
            action = "Retry",
            project = "aterm_db::replica::tests::policy::policy_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRepairPolicy",
            action = "Restore",
            project = "aterm_db::replica::tests::policy::policy_transition"
        )
    )]
    fn admit<T>(
        &self,
        peer: impl Into<Option<Actor>>,
        write: bool,
        gate: Gate,
        run: impl FnOnce(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        // A peer shares the view this round probed, so each admission holds it to that view.
        // With no peer (the mesh's calls) nobody shares it: a control change since the round
        // began is no refusal, and each record's own writer grant is checked at its ingest.
        // Through the round's group look a read shares the look of the read admissions before it
        // since the group last ended (a call on its stream, a write admission, a witness read, an
        // error), where the group kept it (`GroupLook`).
        let peer = peer.into();
        #[cfg(test)]
        tests::table_admission(self.active_table.get());
        self.replica.observe(
            &self.bootstrap,
            peer,
            peer.map(|_| self.observation),
            write,
            self.group,
            |session| {
                // Custody and journal replay can take time. Read local policy, and where `gate`
                // says the table's subscription, after admitting the session, before admitting
                // this batch.
                self.check_table_policy(session, gate)?;
                run(session)
            },
        )
    }

    /// End the round's group ([`GroupLook::let_go`]): the next read admission takes its own look.
    fn end_group(&self) {
        if let Some(group) = self.group {
            group.let_go();
        }
    }

    /// Refuse a changed or disabled local policy, and, where `gate` is [`Gate::Subscription`], an
    /// active table this Mac no longer subscribes.
    fn check_table_policy(&self, session: &Session<'_>, gate: Gate) -> Result<()> {
        if let Some((directory, config)) = &self.commons {
            let policy = crate::catalog::policy::Policy::read(config)?;
            if !policy.enabled
                || Some(&policy) != self.policy.as_ref()
                || policy
                    .excluded
                    .contains(&directory.catalog_scope().digest())
            {
                return Err(Error::new(
                    "Commons subscription policy changed or disabled",
                ));
            }
        }
        if gate == Gate::Subscription
            && let Some((directory, _)) = &self.commons
            && let Some(id) = self.active_table.get()
            && !directory.permits(
                session,
                self.policy.as_ref().expect("Commons policy captured"),
                id,
                self.now,
            )?
        {
            return Err(Error::new("table subscription changed during repair"));
        }
        Ok(())
    }

    fn exchange_with_policy(
        self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        repair: &mut Repair,
        policy: ProbePolicy,
    ) -> Result<Report> {
        self.local(peer, |_| Ok(()))?;
        let compatible = policy == ProbePolicy::Compatible;
        let primary = match policy {
            #[cfg(test)]
            ProbePolicy::Legacy => self.legacy_observation.unwrap_or(self.observation),
            _ => self.observation,
        };
        let sent = if compatible && repair.legacy_probe {
            self.legacy_observation.unwrap_or(self.observation)
        } else {
            primary
        };
        match side {
            Side::Initiator => {
                self.send_probe_value(stream, PROBE, &sent)?;
                if let Err(error) = self.receive_probe_value(stream, READY, &sent, None) {
                    if compatible
                        && self
                            .legacy_observation
                            .is_some_and(|legacy| legacy != self.observation)
                        && error.retryable()
                    {
                        // Only a preflight mismatch/EOF may select the other locally
                        // derived alias on a fresh channel. No app bytes passed yet.
                        repair.legacy_probe = !repair.legacy_probe;
                        repair.retry_probe = true;
                    }
                    return Err(error.into_error());
                }
            }
            Side::Responder => {
                let received = self
                    .receive_probe_value(
                        stream,
                        PROBE,
                        &primary,
                        compatible.then_some(self.legacy_observation).flatten(),
                    )
                    .map_err(ProbeFailure::into_error)?;
                self.local(peer, |_| Ok(()))?;
                self.send_probe_value(stream, READY, &received)?;
            }
        }
        let mut report = Report::default();
        let result = self.exchange_tables(stream, peer, side, repair, &mut report);
        repair.interrupted = result.is_err().then_some(report);
        result.map(|()| report)
    }

    /// Select this Mac's tables for the round: the Commons storage region (recovery when the
    /// primary catalog cannot be read), then its subscriptions, enrolling them under a write
    /// admission when none is established yet. One read admission reads the primary catalog once
    /// for both: whether it can be read places the round, and its projection under the round's
    /// policy names the subscriptions. A retry of that admission (`Round::local`) decides the
    /// region afresh.
    fn select_tables(&mut self, peer: Option<Actor>, report: &mut Report) -> Result<()> {
        let Some((directory, config)) = self.commons.clone() else {
            return Ok(());
        };
        let policy = self.policy.clone().expect("Commons policy captured");
        let recovery = std::cell::Cell::new(false);
        let existing = self.local(peer, |session| {
            recovery.set(false);
            match directory.observe_primary_selecting(session, &config, &policy) {
                Err(error) if error.is_busy() => Err(error),
                Err(_) => {
                    recovery.set(true);
                    directory
                        .recovery()
                        .existing_subscriptions(session, &policy)
                }
                Ok(observed) => {
                    let Some(mut tables) = directory.selected_subscriptions(observed, &policy)?
                    else {
                        return Ok(None);
                    };
                    // Ring tables join the catalog's; damaged ring state asks for the writer,
                    // whose retirement rebuilds it. The catalog's tables are repaired whatever the
                    // rings say: the offered tables' window reads a damaged state as none
                    // (`ring_view`).
                    if directory.is_recovery() {
                        return Ok(Some(tables));
                    }
                    match directory.existing_ring_subscriptions(session, &policy, self.now) {
                        Ok(Some(rings)) => {
                            tables.extend(rings);
                            Ok(Some(tables))
                        }
                        Ok(None) | Err(_) => Ok(None),
                    }
                }
            }
        });
        if recovery.get() {
            self.commons = Some((directory.recovery(), config));
            report.recovery = true;
        }
        let existing = existing?;
        let Some((directory, _)) = &self.commons else {
            return Ok(());
        };
        self.tables = match existing {
            Some(tables) => tables,
            None => self.local_write(peer, |session| {
                let mut tables = directory.subscriptions(session, &policy)?;
                if !directory.is_recovery() {
                    tables.extend(
                        directory
                            .ring_subscriptions(session, &policy, self.now)
                            .unwrap_or_default(),
                    );
                }
                Ok(tables)
            })?,
        };
        Ok(())
    }

    fn exchange_tables(
        mut self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        repair: &mut Repair,
        report: &mut Report,
    ) -> Result<()> {
        self.keys(stream, peer, side, repair, report)?;
        self.select_tables(Some(peer), report)?;
        repair.records.retain(|id, _| self.tables.contains_key(id));
        repair
            .conflicts
            .retain(|id, _| self.tables.contains_key(id));
        let local: BTreeSet<_> = self.local(peer, |_| Ok(self.tables.keys().copied().collect()))?;
        // A Commons round's list carries this Mac's clock mark while it has room below the bound
        // every receiver since 0.145 enforces; the marks a peer sent come out before anything
        // else reads its list, so no mark is ever common, selected, adopted or pushed.
        let marks = self.marks && self.commons.is_some();
        let mut listed = with_mark(&local, marks.then(|| self.wall_ms()).flatten());
        // A first round with this peer, or the first after a long gap, asks which tables differ
        // (`witness`): the ask, as the mark, goes only where the list has room below the bound.
        let listed_ms = self.wall_ms();
        let salt = if side == Side::Initiator
            && self.witnesses
            && listed.len() < MAX_TABLES
            && repair.listed_ms.is_none_or(|at| {
                listed_ms.is_none_or(|now| now.saturating_sub(at) >= WITNESS_GAP_MS)
            }) {
            witness_salt()
        } else {
            None
        };
        if let Some(salt) = salt {
            listed.insert(witness::ask(salt));
        }
        let (received, answered) = match side {
            Side::Initiator => {
                let received = exchange_ids(stream, side, &listed, MAX_TABLES, || {
                    self.local(peer, |_| Ok(()))
                })?;
                if self.witnesses {
                    witness::split(received)
                } else {
                    (received, witness::Heard::default())
                }
            }
            Side::Responder => {
                // As `exchange_ids`, but a peer's ask is answered in the list this side sends:
                // the witnesses of the tables both list, as many chunks as fit below the bound.
                let received = receive_ids(stream, MAX_TABLES)?;
                self.local(peer, |_| Ok(()))?;
                let (received, heard) = if self.witnesses {
                    witness::split(received)
                } else {
                    (received, witness::Heard::default())
                };
                if let [salt] = heard.asks[..] {
                    // The peer starts over with this side: this side's next round asks it too,
                    // once. An ask within the gap of this side's own is that ask's echo, and
                    // echoing it back would have the two sides ask each other every round.
                    #[cfg(test)]
                    let echoes = self.echoes;
                    #[cfg(not(test))]
                    let echoes = false;
                    if echoes
                        || repair.asked_ms.is_none_or(|at| {
                            listed_ms.is_none_or(|now| now.saturating_sub(at) >= WITNESS_GAP_MS)
                        })
                    {
                        repair.listed_ms = None;
                    }
                    let common: BTreeSet<_> = local
                        .intersection(&received)
                        .filter(|id| crate::clock::read_mark(id).is_none())
                        .copied()
                        .collect();
                    // Only the chunks that fit go out, so only their tables are read.
                    let room = MAX_TABLES.saturating_sub(listed.len());
                    let witnesses = self.table_witnesses(
                        peer,
                        &common,
                        &salt,
                        room.saturating_mul(witness::PER_CHUNK),
                    );
                    listed.extend(witness::chunks(&witnesses, room));
                    // The witness reads took time: authorize again right before sending.
                    self.local(peer, |_| Ok(()))?;
                }
                send_ids(stream, &listed, MAX_TABLES)?;
                (received, heard)
            }
        };
        let (remote, heard) = if self.marks {
            crate::clock::split_owned(received)
        } else {
            (received, crate::clock::Heard::Nothing)
        };
        if marks && let Some(recv) = self.wall_ms() {
            repair.clock = match heard {
                crate::clock::Heard::One(peer) => {
                    crate::clock::Reading::from_mark(peer, recv, self.started.elapsed())
                }
                crate::clock::Heard::Several => None,
                crate::clock::Heard::Nothing => self.offered_reading(&remote, recv),
            };
        }
        let common: BTreeSet<_> = local.intersection(&remote).copied().collect();
        if side == Side::Initiator {
            if let Some(salt) = salt
                && !answered.chunks.is_empty()
                && let Some(differing) = witness::differing(
                    &common,
                    &self.table_witnesses(peer, &common, &salt, common.len()),
                    &answered,
                )
            {
                repair.differing = differing;
            }
            if salt.is_some() {
                repair.asked_ms = listed_ms;
                report.witnesses_asked = true;
            }
            repair.listed_ms = listed_ms;
            repair.differing.retain(|id| common.contains(id));
        }
        let force_regular = side == Side::Initiator && std::mem::take(&mut repair.table_yield);
        let follow_up = if side == Side::Initiator {
            let saved = repair.table_follow_up.take();
            if force_regular {
                None
            } else {
                saved.filter(|(id, _)| common.contains(id))
            }
        } else {
            None
        };
        // While a push names a table the peer holds too, each round this side initiates repairs
        // it first (`crate::runtime::Pusher`); a follow-up of another table waits, and no backfill
        // hint is spent meanwhile. The turns below resume once the push ends.
        let pushed = if side == Side::Initiator {
            (*self
                .first
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner))
            .filter(|id| common.contains(id))
        } else {
            None
        };
        let follow_up = follow_up.filter(|(id, _)| pushed.is_none_or(|first| first == *id));
        let hinted =
            if side == Side::Initiator && !force_regular && follow_up.is_none() && pushed.is_none()
            {
                self.backfill
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .next(&local, &common)
            } else {
                None
            };
        let priority = pushed.or(follow_up.map(|(id, _)| id)).or(hinted);
        // While tables whose witnesses differed remain, the regular turn visits only those, in the
        // scan's own order (fresh peers still start apart); the scan goes on from the last.
        #[cfg(test)]
        let unwitnessed = UNWITNESSED.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let unwitnessed = false;
        let turn = if unwitnessed || repair.differing.is_empty() {
            &common
        } else {
            &repair.differing
        };
        let selected = match side {
            Side::Initiator => {
                let chosen = priority.or_else(|| {
                    self.backfill
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .regular(turn, repair.table)
                });
                if let Some(id) = chosen {
                    repair.differing.remove(&id);
                }
                self.local(peer, |_| Ok(()))?;
                control::send_frame(stream, chosen.as_ref().map_or(&[][..], |id| id.as_slice()))?;
                chosen
            }
            Side::Responder => {
                let chosen = control::receive_frame(stream, 32)?;
                if chosen.is_empty() {
                    None
                } else {
                    Some(chosen.as_slice().try_into().map_err(|_| {
                        Error::repair(
                            "invalid selected table",
                            aterm_types::network_state::RepairCause::Decode,
                        )
                    })?)
                }
            }
        };
        // A ring table the peer lists and this Mac lacks is taken up for a later round; this
        // round keeps to the tables both sides listed. One that cannot be taken up now is
        // offered again next round.
        if let Some((directory, _)) = &self.commons
            && !directory.is_recovery()
        {
            let policy = self.policy.as_ref().expect("Commons policy captured");
            let offered = self.local(peer, |session| {
                let lacked: BTreeSet<_> = remote.difference(&local).copied().collect();
                directory.ring_tables_among(session, self.now, &lacked)
            })?;
            if !offered.is_empty() {
                let _ = self.local_write(peer, |session| {
                    directory.adopt_ring_tables(session, policy, self.now, &offered)
                });
            }
        }
        let Some(id) = selected else {
            return Ok(());
        };
        if !common.contains(&id) {
            return Err(Error::repair(
                "peer selected an unprovisioned table",
                aterm_types::network_state::RepairCause::TableUndeclared,
            ));
        }
        // A peer chooses inbound tables. Preserve our outgoing scan so its
        // restarted scan cannot keep rewinding ours to the first table.
        if side == Side::Initiator && priority.is_none() {
            repair.table = Some(id);
        }
        if priority.is_some() {
            // A failed or empty preferred round also yields. A permanently
            // unavailable history must not replace the ordinary table scan.
            repair.table_yield = true;
        }
        report.table = Some(id);
        report.scope = self.tables.get(&id).map(|(scope, _)| *scope);
        let deferred = report.deferred;
        self.table(stream, peer, side, id, repair, report)?;
        report.table_deferred = report.deferred > deferred;
        #[cfg(test)]
        let retried = !DEFERRAL_PASSES.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let retried = true;
        if retried
            && side == Side::Initiator
            && priority.is_none()
            && report.table_deferred
            && repair.table_follow_up.is_none()
        {
            // A deferred table repaired nothing: the next round takes it again, once, as a
            // follow-up, where the scan would leave it until it came round again. A second
            // deferral yields to the scan as any preferred round does.
            repair.table_follow_up = Some((id, 0));
            repair.table_yield = false;
        }
        if side == Side::Initiator
            && (report.sent >= BATCH
                || report.received >= BATCH
                || report.conflicts_sent >= BATCH
                || report.conflicts_received >= BATCH)
        {
            let completed = follow_up.map_or(1, |(_, completed)| completed + 1);
            if completed < TABLE_BURST {
                repair.table_follow_up = Some((id, completed));
                repair.table_yield = false;
            } else {
                repair.table_yield = true;
            }
        }
        if repair.deferral.is_none() && repair.batch_deferral.is_none() {
            // Observation failure cannot turn completed repair into a different result.
            report.matching_inventory = self.matching_inventory(peer, id, report);
        }
        Ok(())
    }

    #[cfg(test)]
    fn receive_probe(&self, stream: &mut impl Read, tag: &[u8; 5]) -> Result<()> {
        self.receive_probe_value(stream, tag, &self.observation, None)
            .map(|_| ())
            .map_err(ProbeFailure::into_error)
    }

    fn send_probe_value(
        &self,
        stream: &mut impl Write,
        tag: &[u8; 5],
        value: &[u8; 64],
    ) -> Result<()> {
        let mut frame = [0; 69];
        frame[..5].copy_from_slice(tag);
        frame[5..].copy_from_slice(value);
        control::send_frame(stream, &frame)
    }

    fn receive_probe_value(
        &self,
        stream: &mut impl Read,
        tag: &[u8; 5],
        expected: &[u8; 64],
        alternate: Option<[u8; 64]>,
    ) -> std::result::Result<[u8; 64], ProbeFailure> {
        let mut size = [0; 4];
        stream.read_exact(&mut size).map_err(ProbeFailure::io)?;
        let mut frame = [0; 69];
        if u32::from_le_bytes(size) as usize != frame.len() {
            return Err(ProbeFailure::Mismatch(ProbeMismatch::Malformed));
        }
        stream.read_exact(&mut frame).map_err(ProbeFailure::io)?;
        let value: [u8; 64] = frame[5..].try_into().expect("fixed probe length");
        if let Err(mismatch) = validate_probe(&frame, tag, expected) {
            match mismatch {
                ProbeMismatch::History { .. } if alternate == Some(value) => {}
                mismatch => return Err(ProbeFailure::Mismatch(mismatch)),
            }
        }
        Ok(value)
    }

    fn keys(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        repair: &mut Repair,
        report: &mut Report,
    ) -> Result<()> {
        let (needs, next) =
            self.local(peer, |session| session.history_needs(repair.history, BATCH))?;
        repair.history = next;
        let local: BTreeSet<_> = needs.into_iter().collect();
        let remote = exchange_ids(stream, side, &local, BATCH, || self.local(peer, |_| Ok(())))?;
        match side {
            Side::Initiator => {
                self.send_keys(stream, peer, &remote, report)?;
                self.receive_keys(stream, peer, &local, report)?;
            }
            Side::Responder => {
                self.receive_keys(stream, peer, &local, report)?;
                self.send_keys(stream, peer, &remote, report)?;
            }
        }
        Ok(())
    }
    fn send_keys(
        &self,
        stream: &mut impl Write,
        peer: Actor,
        needs: &BTreeSet<Digest>,
        report: &mut Report,
    ) -> Result<()> {
        let requested: Vec<_> = needs.iter().copied().collect();
        let keys = self.local(peer, |session| session.handoffs(peer, &requested))?;
        for key in keys {
            stream.write_all(&[u8::from(key.is_some())])?;
            if let Some(key) = key {
                control::write_frame(stream, &key)?;
                report.keys_sent += 1;
            }
        }
        stream.flush()?;
        Ok(())
    }
    fn receive_keys(
        &self,
        stream: &mut impl Read,
        peer: Actor,
        needs: &BTreeSet<Digest>,
        report: &mut Report,
    ) -> Result<()> {
        let mut keys = Vec::with_capacity(needs.len());
        for source in needs {
            let mut present = [0];
            stream.read_exact(&mut present)?;
            match present[0] {
                0 => {
                    keys.push((*source, None));
                }
                1 => {
                    let key = control::receive_frame(
                        stream,
                        aterm_secrets::commons::history::MAX_HANDOFF,
                    )?;
                    keys.push((*source, Some(key)));
                }
                _ => {
                    return Err(Error::repair(
                        "invalid historical key response",
                        aterm_types::network_state::RepairCause::Decode,
                    ));
                }
            }
        }
        // One bounded, fully buffered batch. Validate current membership after
        // its final network read; never hold custody locks while receiving it.
        let import = |session: &Session<'_>| {
            Ok(keys
                .iter()
                .filter(|(source, key)| {
                    key.as_ref()
                        .is_some_and(|key| session.import_handoff(*source, key).is_ok())
                })
                .count())
        };
        let accepted = if keys.iter().any(|(_, key)| key.is_some()) {
            self.local_write(peer, import)?
        } else {
            self.local(peer, import)?
        };
        report.keys_received += accepted;
        report.deferred += keys.len() - accepted;
        Ok(())
    }

    /// Table `id`'s operation inventory under an admission that checks its subscription
    /// ([`Gate::Subscription`]); a store this side cannot read now is `None`, with `refusal`.
    fn inventory(
        &self,
        peer: Actor,
        id: Digest,
        refusal: &mut Option<String>,
        cause: &mut Option<aterm_types::network_state::RepairCause>,
    ) -> Result<Option<Inventory>> {
        self.inventory_gated(peer, id, Gate::Subscription, refusal, cause)
    }

    /// [`Self::inventory`] under `gate`'s admission.
    fn inventory_gated(
        &self,
        peer: Actor,
        id: Digest,
        gate: Gate,
        refusal: &mut Option<String>,
        cause: &mut Option<aterm_types::network_state::RepairCause>,
    ) -> Result<Option<Inventory>> {
        let read = self.local_gated(peer, gate, |session| {
            let sampled_at = Instant::now();
            let observed_at_ms = aterm_time::unix_millis().unwrap_or(0);
            let store = match self.store(session, id) {
                Ok(store) => store,
                Err(error) if error.is_snapshot_changed() => return Err(error),
                Err(error) => {
                    *refusal = Some(error.to_string());
                    *cause = Some(error.repair_cause());
                    return Ok(None);
                }
            };
            let known = store.inventory_unchecked().copied().collect();
            let observation = InventoryObservation {
                scope: store.scope(),
                local_actor: session.actor(),
                peer_actor: peer,
                control_observation: session.observation()?,
                inventory: InventorySummary::of(&known),
                observed_at_ms,
                sampled_at,
            };
            Ok(Some(Inventory::new(
                known,
                store.quarantine_closure(),
                observation,
                store.fence(),
            )))
        });
        Self::table_read(read, refusal, cause)
    }

    /// The witnesses of `common` under `salt` (`witness`), in digest order: operation IDs in each
    /// table's independently witnessed receipts, under the round's admission and subscription
    /// check, without authenticating envelopes or decrypting values. These only order repairs;
    /// the selected table still takes every full inventory and data check. A table this side
    /// cannot inspect now, holds no storage for, lies past the
    /// first `read` tables, or comes after [`WITNESS_BUDGET`] reads as [`witness::UNKNOWN`]: it is
    /// repaired early, never skipped. Each table's read ends the round's group before its
    /// admission and after it ([`Self::end_group`]): the peer and the table count set how many
    /// there are, so none shares a look with another admission ([`GroupLook`]).
    fn table_witnesses(
        &self,
        peer: Actor,
        common: &BTreeSet<Digest>,
        salt: &[u8; 16],
        read: usize,
    ) -> Vec<witness::Witness> {
        let started = Instant::now();
        common
            .iter()
            .enumerate()
            .map(|(at, id)| {
                if at >= read || started.elapsed() >= WITNESS_BUDGET {
                    return witness::UNKNOWN;
                }
                let before = self.active_table.replace(Some(*id));
                self.end_group();
                let content = self.local(peer, |session| {
                    let ids = if let Some((directory, _)) = &self.commons {
                        let (scope, _) = &self.tables[id];
                        // Storage this side never enrolled stays so until its table round.
                        let Some(ids) = directory.repair_ids(session, *scope)? else {
                            return Ok(None);
                        };
                        if !directory.is_recovery()
                            && *scope != directory.catalog_scope()
                            && let Some(recovery) =
                                directory.recovery().repair_ids(session, *scope)?
                        {
                            recovery
                        } else {
                            ids
                        }
                    } else {
                        let (scope, path) = &self.tables[id];
                        let Some(ids) = Store::repair_ids_region(
                            session,
                            path,
                            *scope,
                            crate::table::store::Region::Primary,
                        )?
                        else {
                            return Ok(None);
                        };
                        ids
                    };
                    Ok(Some(InventorySummary::of(&ids).fingerprint))
                });
                self.end_group();
                self.active_table.set(before);
                witness::witness(salt, id, content.ok().flatten())
            })
            .collect()
    }

    fn matching_inventory(
        &self,
        peer: Actor,
        id: Digest,
        report: &Report,
    ) -> Option<MatchingInventory> {
        let read = self.inventory_read.borrow();
        let (before, fence) = read.as_ref()?;
        if report.local_known != report.peer_known || report.deferred != 0 {
            return None;
        }
        let after = match fence
            .as_ref()
            .and_then(|fence| self.fenced_observation(peer, id, before, fence))
        {
            Some(after) => after,
            None => {
                self.inventory(peer, id, &mut None, &mut None)
                    .ok()??
                    .observation
            }
        };
        MatchingInventory::from_observations(report, *before, after)
    }

    /// `before`'s inventory observed again under a fresh read admission without reading the store
    /// again, where [`Self::store`] would open the store `fence` was taken of and it stands as
    /// that read found it ([`Self::opens_fenced`]): its records, and so its inventory, are the
    /// same claim's witnessed prefix. A store that moved, another store, or any error is `None`,
    /// and the caller reads the inventory again.
    fn fenced_observation(
        &self,
        peer: Actor,
        id: Digest,
        before: &InventoryObservation,
        fence: &Fence,
    ) -> Option<InventoryObservation> {
        self.local(peer, |session| {
            let sampled_at = Instant::now();
            let observed_at_ms = aterm_time::unix_millis().unwrap_or(0);
            if !self.opens_fenced(session, id, fence)? {
                return Ok(None);
            }
            Ok(Some(InventoryObservation {
                scope: fence.scope(),
                local_actor: session.actor(),
                peer_actor: peer,
                control_observation: session.observation()?,
                inventory: before.inventory,
                observed_at_ms,
                sampled_at,
            }))
        })
        .ok()
        .flatten()
    }

    /// Whether [`Self::store`] would open, in this read `session`, the store `fence` was taken of
    /// with its custody unchanged ([`Fence::stands`]). Its choice among a Commons application
    /// table's stores is this one: an existing recovery store first, so a primary's fence holds
    /// only while no recovery store exists. A directory in recovery, whose choice reads the
    /// primary too, declines.
    fn opens_fenced(&self, session: &Session<'_>, id: Digest, fence: &Fence) -> Result<bool> {
        let (scope, _) = &self.tables[&id];
        if let Some((directory, _)) = &self.commons
            && *scope != directory.catalog_scope()
        {
            if directory.is_recovery() {
                return Ok(false);
            }
            if !fence.is_recovery()
                && directory
                    .recovery()
                    .observe_store_if_exists(session, *scope)?
                    .is_some()
            {
                return Ok(false);
            }
        }
        fence.stands(session)
    }
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableConflict",
            action = "Replicate",
            project = "aterm_db::table::store::replay::tests::conflict::conflict_transition"
        )
    )]
    fn conflicts(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        id: Digest,
        repair: &mut Repair,
        report: &mut Report,
    ) -> Result<bool> {
        let mut local_error = None;
        let mut local_cause = None;
        let ready = Self::table_read(
            self.retain_primary(peer, id).map(Some),
            &mut local_error,
            &mut local_cause,
        )?;
        report.recovery |= ready == Some(false);
        // Sent only after the authorize that `exchange_inventory` runs first, which checks the
        // table's subscription (`Gate`).
        let local = self.local_policy_only(peer, |session| {
            if ready != Some(true) {
                return Ok(None);
            }
            let store = match self.store(session, id) {
                Ok(store) => store,
                Err(error) if error.is_snapshot_changed() => return Err(error),
                Err(error) => {
                    local_error = Some(error.to_string());
                    local_cause = Some(error.repair_cause());
                    return Ok(None);
                }
            };
            report.recovery |= store.is_recovery();
            Ok(Some(store.proof_inventory_unchecked()))
        });
        let local = Self::table_read(local, &mut local_error, &mut local_cause)?;
        let remote = exchange_inventory(stream, side, local.as_ref(), MAX_PROOFS, || {
            self.local(peer, |_| Ok(()))
        })?;
        let peer_unavailable = remote.is_none();
        let (Some(local), Some(remote)) = (local, remote) else {
            repair.deferral = Some(InventoryDeferral {
                inventory: "conflicts",
                local_error,
                local_cause,
                retaining_primary_conflicts: ready == Some(false),
                peer_unavailable,
            });
            report.deferred += 1;
            return Ok(false);
        };
        let proofs: Vec<_> = cycle(&local, repair.conflicts.get(&id).copied())
            .filter(|key| {
                let mut fork = [0; 81];
                fork[..48].copy_from_slice(&key[..48]);
                fork[48] = 1;
                !remote.contains(*key) && !remote.contains(&fork)
            })
            .take(BATCH)
            .copied()
            .collect();
        if let Some(last) = proofs.last() {
            repair.conflicts.insert(id, *last);
        }
        let prepare = |session: &Session<'_>| {
            self.prepare_batch(session, id, &proofs, |session, store, key| {
                store
                    .proof_wire_unchecked(session, key)?
                    .ok_or_else(|| Error::new("table conflict disappeared"))
            })
        };
        match side {
            Side::Initiator => {
                self.send_batch(stream, peer, prepare, BatchKind::Conflict, report)?;
                self.receive_batch(stream, peer, id, BatchKind::Conflict, repair, report)?;
            }
            Side::Responder => {
                self.receive_batch(stream, peer, id, BatchKind::Conflict, repair, report)?;
                self.send_batch(stream, peer, prepare, BatchKind::Conflict, report)?;
            }
        }
        Ok(true)
    }

    fn table(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        id: Digest,
        repair: &mut Repair,
        report: &mut Report,
    ) -> Result<()> {
        self.active_table.set(Some(id));
        let at = |stage: &str, error: Error| {
            error.context(&format!(
                "table repair {} {stage}",
                aterm_codec::hex::encode_lower(&id)
            ))
        };
        if self.commons.is_some() {
            let (scope, _) = &self.tables[&id];
            // Whether to enroll is no table data: the enrollment below, or else the conflict
            // round's authorize, checks the table's subscription (`Gate`).
            let absent = self
                .local_policy_only(peer, |session| {
                    // A damaged established store stays deferred. Only storage with no
                    // enrollment yet (an empty discovery reservation, or a first
                    // enrollment cut short after its pending claim) needs `join`, which its
                    // claim alone says (`Store::enrollment_region`): this opens no store.
                    Ok(matches!(
                        self.commons
                            .as_ref()
                            .expect("Commons checked")
                            .0
                            .enrollment(session, *scope),
                        Ok(Enrollment::Absent | Enrollment::Pending)
                    ))
                })
                .map_err(|error| at("storage admission", error))?;
            if absent {
                self.local_write(peer, |session| self.store(session, id).map(drop))
                    .map_err(|error| at("storage enrollment", error))?;
            }
        }
        if !self
            .conflicts(stream, peer, side, id, repair, report)
            .map_err(|error| at("conflicts", error))?
        {
            return Ok(());
        }
        // Reopen after negative evidence is durably applied in both directions.
        let mut local_error = None;
        let mut local_cause = None;
        // Sent only after the authorize that `exchange_inventory` runs first, which checks the
        // table's subscription (`Gate`).
        let local = self
            .inventory_gated(peer, id, Gate::Policy, &mut local_error, &mut local_cause)
            .map_err(|error| at("operation inventory", error))?;
        let remote = exchange_inventory(
            stream,
            side,
            local.as_ref().map(|inventory| &inventory.known),
            MAX_RECORDS,
            || self.local(peer, |_| Ok(())),
        )
        .map_err(|error| at("inventory exchange", error))?;
        let peer_unavailable = remote.is_none();
        let (Some(local), Some(remote)) = (local, remote) else {
            repair.deferral = Some(InventoryDeferral {
                inventory: "operations",
                local_error,
                local_cause,
                retaining_primary_conflicts: false,
                peer_unavailable,
            });
            report.deferred += 1;
            return Ok(());
        };
        report.retained = local.known.len();
        #[cfg(test)]
        {
            report.retained_after = local.known.len();
        }
        report.local_known = Some(local.observation.inventory);
        report.local_offered = Some(local.offered_summary());
        report.peer_known = Some(local.peer_summary(&remote));
        self.backfill
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .observe(id, &local.known, &remote);
        let mut ids: Vec<_> = local
            .offered_cycle(repair.records.get(&id).copied())
            .filter(|v| !remote.contains(*v))
            .take(BATCH + 1)
            .copied()
            .collect();
        self.inventory_read
            .replace(Some((local.observation, local.fence)));
        // One more than a batch says the peer lacks more than this round sends.
        let whole = ids.len() <= BATCH;
        ids.truncate(BATCH);
        if let Some(last) = ids.last() {
            repair.records.insert(id, *last);
        }
        let prepare = |session: &Session<'_>| {
            self.prepare_batch(session, id, &ids, |_, store, key| {
                store
                    .wire_unchecked(key)
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| Error::new("table record disappeared"))
            })
            .map_err(|error| at("batch preparation", error))
        };
        let sent = report.sent;
        match side {
            Side::Initiator => {
                self.send_batch(stream, peer, prepare, BatchKind::Operation, report)
                    .map_err(|error| at("operation send", error))?;
                self.receive_batch(stream, peer, id, BatchKind::Operation, repair, report)
                    .map_err(|error| at("operation receive", error))?;
            }
            Side::Responder => {
                self.receive_batch(stream, peer, id, BatchKind::Operation, repair, report)
                    .map_err(|error| at("operation receive", error))?;
                self.send_batch(stream, peer, prepare, BatchKind::Operation, report)
                    .map_err(|error| at("operation send", error))?;
            }
        }
        report.caught_up = whole && report.sent - sent == ids.len();
        Ok(())
    }
    /// Send a batch this side prepares and read its receipts. A table that keeps advancing through
    /// every read of the batch fails the round here rather than deferring: the wire has no
    /// unavailable batch, and an empty one tells the peer this side holds nothing more it lacks.
    /// The peer's round would then read as leaving nothing to fetch
    /// ([`crate::runtime::RepairRound::drained_table`]), and as exchanged with nothing to bring
    /// to the wait before a first import (`empty_view_wait`).
    /// The batch's admission checks the local policy; its send boundary, after preparing, checks
    /// the policy and the table's subscription ([`Gate`]). A preparation that fails while the
    /// table is withdrawn meets the withdrawal's refusal, not its own error.
    fn send_batch(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        prepare: impl Fn(&Session<'_>) -> Result<Vec<Vec<u8>>>,
        kind: BatchKind,
        report: &mut Report,
    ) -> Result<()> {
        // Prepare under the same current custody, membership and policy admission
        // that permits this send. Release its guards before any network I/O.
        let batch = self.local_policy_only(peer, |session| {
            let batch = match prepare(session) {
                Ok(batch) => batch,
                Err(error) => {
                    // A preparation that fails while the table is withdrawn (its storage
                    // retired, say) meets the withdrawal's refusal first: a withdrawal before
                    // this admission, as when the admission checked the subscription, and one
                    // during the preparation, which that admission did not see.
                    self.check_table_policy(session, Gate::Subscription)?;
                    return Err(error);
                }
            };
            // Config writers need no custody lock. The send boundary checks the policy and the
            // table's subscription after preparing, however long preparation takes: the
            // admission checked the policy alone (`Gate`).
            self.check_table_policy(session, Gate::Subscription)?;
            Ok(batch)
        })?;
        stream.write_all(&(batch.len() as u32).to_le_bytes())?;
        for wire in &batch {
            control::write_frame(stream, wire)?;
        }
        stream.flush()?;
        for wire in &batch {
            let mut receipt = [0; 33];
            stream.read_exact(&mut receipt)?;
            if receipt[..32] != Sha256::digest(wire) || receipt[32] > 1 {
                return Err(Error::repair(
                    "invalid table receipt",
                    aterm_types::network_state::RepairCause::Decode,
                ));
            }
            if receipt[32] == 1 {
                match kind {
                    BatchKind::Operation => report.sent += 1,
                    BatchKind::Conflict => report.conflicts_sent += 1,
                }
            } else {
                report.deferred += 1;
            }
        }
        Ok(())
    }
    fn receive_batch(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        id: Digest,
        kind: BatchKind,
        repair: &mut Repair,
        report: &mut Report,
    ) -> Result<()> {
        let n = control::read_u32(stream)? as usize;
        if n > BATCH {
            return Err(Error::repair(
                "table batch exceeds its bound",
                aterm_types::network_state::RepairCause::Quota,
            ));
        }
        let mut wires = Vec::with_capacity(n);
        let mut ids = BTreeSet::new();
        for _ in 0..n {
            let max = match kind {
                BatchKind::Operation => crate::crypto::MAX_OPERATION,
                BatchKind::Conflict => crate::table::conflict::MAX_CONFLICT,
            };
            let wire = control::receive_frame(stream, max)?;
            let record = Sha256::digest(&wire);
            if !ids.insert(record) {
                return Err(Error::new("duplicate table batch record"));
            }
            wires.push((record, wire));
        }
        if wires.is_empty() {
            // Nothing to write, and the flush has nothing to send: every send before it flushed.
            // What this side sends next stands on its own check of the table's subscription
            // (`Gate`).
            self.local_policy_only(peer, |_| Ok(()))?;
            stream.flush()?;
            return Ok(());
        }
        let batch = match kind {
            BatchKind::Operation => "operations",
            BatchKind::Conflict => "conflicts",
        };
        let mut refusal = None;
        #[cfg(test)]
        let mut retained_after = None;
        let verdicts = self.local_write(peer, |session| {
            let mut store = match self.store(session, id) {
                Ok(store) => store,
                Err(error) => {
                    refusal = Some(BatchDeferral::new(batch, "storage", error));
                    return Ok(vec![false; n]);
                }
            };
            let verdicts = wires
                .iter()
                .map(|(record, wire)| {
                    let receipt = match kind {
                        BatchKind::Operation => store.ingest(session, wire),
                        BatchKind::Conflict => store.ingest_conflict(session, wire),
                    };
                    match receipt {
                        Ok(receipt) if receipt.id == *record => true,
                        Ok(_) => {
                            if refusal.is_none() {
                                refusal = Some(BatchDeferral::new(
                                    batch,
                                    "receipt",
                                    Error::repair(
                                        "table receipt did not name the received operation",
                                        aterm_types::network_state::RepairCause::Decode,
                                    ),
                                ));
                            }
                            false
                        }
                        Err(error) => {
                            if refusal.is_none() {
                                refusal = Some(BatchDeferral::new(batch, "ingest", error));
                            }
                            false
                        }
                    }
                })
                .collect::<Vec<_>>();
            #[cfg(test)]
            {
                retained_after = matches!(kind, BatchKind::Operation)
                    .then(|| store.inventory(session).ok().map(Iterator::count))
                    .flatten();
            }
            Ok(verdicts)
        })?;
        if repair.batch_deferral.is_none() {
            repair.batch_deferral = refusal;
        }
        #[cfg(test)]
        if let Some(count) = retained_after {
            report.retained_after = count;
        }
        // Ingestion completed before these receipts. Preserve all observed
        // verdicts even when current authority or the return channel now fails.
        for accepted in &verdicts {
            if *accepted {
                match kind {
                    BatchKind::Operation => report.received += 1,
                    BatchKind::Conflict => report.conflicts_received += 1,
                }
            } else {
                report.deferred += 1;
            }
        }
        self.local(peer, |_| Ok(()))?;
        if matches!(kind, BatchKind::Operation) {
            let mut backfill = self.backfill.lock().unwrap_or_else(|p| p.into_inner());
            for ((record, _), accepted) in wires.iter().zip(&verdicts) {
                if *accepted {
                    backfill.retained(id, *record);
                }
            }
        }
        let mut receipts = Vec::with_capacity(wires.len() * 33);
        for ((record, _), accepted) in wires.iter().zip(verdicts) {
            receipts.extend_from_slice(record);
            receipts.push(u8::from(accepted));
        }
        stream.write_all(&receipts)?;
        stream.flush()?;
        Ok(())
    }
}

fn cycle<const N: usize>(
    ids: &BTreeSet<[u8; N]>,
    after: Option<[u8; N]>,
) -> impl Iterator<Item = &[u8; N]> {
    use std::ops::Bound::{Excluded, Unbounded};
    let tail = match after {
        Some(after) => ids.range((Excluded(after), Unbounded)),
        None => ids.range::<[u8; N], _>(..),
    };
    let head = after.into_iter().flat_map(move |after| ids.range(..=after));
    tail.chain(head)
}
/// A fresh salt for one ask's witnesses (`witness`), so that no two contents' witnesses collide
/// in every round: none when the system's random source fails, and the round asks nothing.
fn witness_salt() -> Option<[u8; 16]> {
    use ring::rand::SecureRandom;
    let mut salt = [0; 16];
    ring::rand::SystemRandom::new().fill(&mut salt).ok()?;
    Some(salt)
}

#[cfg(test)]
thread_local! {
    /// Mutant (tests): the regular turn ignores the tables whose witnesses differed.
    pub(crate) static UNWITNESSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Mutant (tests): a deferred table is passed over until the scan comes round again.
    pub(crate) static DEFERRAL_PASSES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `local` with the clock mark of `wall` (ms) where one is given and the list has room for it
/// below [`MAX_TABLES`], which every receiver since 0.145 enforces (`receive_ids`): a full list
/// goes without.
fn with_mark(local: &BTreeSet<Digest>, wall: Option<u64>) -> BTreeSet<Digest> {
    let mut listed = local.clone();
    if let Some(wall) = wall
        && listed.len() < MAX_TABLES
    {
        listed.insert(crate::clock::mark(wall));
    }
    listed
}

/// Exchange two sorted, unique ID lists of at most `max` each: the initiator sends first, and
/// `authorize` runs before this side sends its own.
///
/// # Errors
/// A list exceeds `max` or is not sorted and unique, `authorize` refuses, or the stream fails.
pub fn exchange_ids<const N: usize>(
    stream: &mut (impl Read + Write),
    side: Side,
    ids: &BTreeSet<[u8; N]>,
    max: usize,
    authorize: impl Fn() -> Result<()>,
) -> Result<BTreeSet<[u8; N]>> {
    match side {
        Side::Initiator => {
            authorize()?;
            send_ids(stream, ids, max)?;
            receive_ids(stream, max)
        }
        Side::Responder => {
            let remote = receive_ids(stream, max)?;
            authorize()?;
            send_ids(stream, ids, max)?;
            Ok(remote)
        }
    }
}
fn send_ids<const N: usize>(
    stream: &mut impl Write,
    ids: &BTreeSet<[u8; N]>,
    max: usize,
) -> Result<()> {
    if ids.len() > max {
        return Err(Error::new("application inventory exceeds its bound"));
    }
    let mut frame = Vec::with_capacity(4 + ids.len() * N);
    frame.extend_from_slice(&(ids.len() as u32).to_le_bytes());
    for id in ids {
        frame.extend_from_slice(id);
    }
    stream.write_all(&frame)?;
    stream.flush()?;
    Ok(())
}
fn receive_ids<const N: usize>(stream: &mut impl Read, max: usize) -> Result<BTreeSet<[u8; N]>> {
    let n = control::read_u32(stream)? as usize;
    if n > max {
        return Err(Error::new("application inventory exceeds its bound"));
    }
    let mut wire = vec![0; n * N];
    stream.read_exact(&mut wire)?;
    let mut ids = BTreeSet::new();
    let mut previous: Option<[u8; N]> = None;
    for id in wire.as_chunks::<N>().0 {
        if previous.is_some_and(|last| &last >= id) {
            return Err(Error::new("application inventory is not sorted and unique"));
        }
        previous = Some(*id);
        ids.insert(*id);
    }
    Ok(ids)
}
/// [`exchange_ids`] where either side may say its inventory is unavailable (`None`), as a store
/// it cannot read now.
///
/// # Errors
/// As [`exchange_ids`], or an invalid ready byte.
pub fn exchange_inventory<const N: usize>(
    stream: &mut (impl Read + Write),
    side: Side,
    local: Option<&BTreeSet<[u8; N]>>,
    maximum: usize,
    authorize: impl Fn() -> Result<()>,
) -> Result<Option<BTreeSet<[u8; N]>>> {
    match side {
        Side::Initiator => {
            authorize()?;
            send_inventory(stream, local, maximum)?;
            receive_inventory(stream, maximum)
        }
        Side::Responder => {
            let remote = receive_inventory(stream, maximum)?;
            authorize()?;
            send_inventory(stream, local, maximum)?;
            Ok(remote)
        }
    }
}

fn send_inventory<const N: usize>(
    stream: &mut impl Write,
    ids: Option<&BTreeSet<[u8; N]>>,
    maximum: usize,
) -> Result<()> {
    stream.write_all(&[u8::from(ids.is_some())])?;
    if let Some(ids) = ids {
        send_ids(stream, ids, maximum)?;
    }
    stream.flush()?;
    Ok(())
}
fn receive_inventory<const N: usize>(
    stream: &mut impl Read,
    maximum: usize,
) -> Result<Option<BTreeSet<[u8; N]>>> {
    let mut ready = [0];
    stream.read_exact(&mut ready)?;
    match ready[0] {
        0 => Ok(None),
        1 => Ok(Some(receive_ids(stream, maximum)?)),
        _ => Err(Error::repair(
            "invalid table inventory state",
            aterm_types::network_state::RepairCause::Decode,
        )),
    }
}

#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbRepairPolicy",
        action = "NextPolicy",
        reason = "The conformance fixture selects one of eight independent config edits before admission; Round::admit observes the resulting policy but does not choose the writer's edit. policy_admission_conforms_and_stale_policy_and_false_receipts_are_detected exercises every selection."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbRepairPolicy",
        action = "Receiving",
        reason = "The conformance fixture selects an outgoing or incoming batch; the shared Round::admit admission does not select its caller's transfer direction. policy_admission_conforms_and_stale_policy_and_false_receipts_are_detected exercises both directions and their retained storage."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbRepairPolicy",
        action = "AfterOpen",
        reason = "The conformance fixture selects whether the independent config edit runs before or after session admission; this selector is not a production admission step. policy_admission_conforms_and_stale_policy_and_false_receipts_are_detected exercises both boundaries, whose hook fires only at its selected boundary."
    )
)]
#[doc(hidden)]
pub fn db_repair_policy_environment() {}

#[cfg(test)]
mod mesh_tests;
#[cfg(test)]
pub(crate) mod tests;
mod witness;
