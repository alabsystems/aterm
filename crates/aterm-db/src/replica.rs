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
        store::{Receipt, Store, Via},
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
impl InventorySummary {
    /// Summarize an exact operation-ID set with the same canonical encoding as
    /// authenticated repair reports. Counts alone do not establish equality.
    pub fn of(ids: &BTreeSet<Digest>) -> Self {
        Self::of_sorted(ids.len(), ids)
    }

    pub(crate) fn of_sorted<'a>(count: usize, ids: impl IntoIterator<Item = &'a Digest>) -> Self {
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
    /// Exact local control head and basis admitted during both inventory reads.
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
        Self::commons_from_bootstrap(paths, config, &bootstrap)
    }

    /// [`Self::commons`] sharing an absolute local-admission deadline. It still
    /// caches no grant: every application batch revalidates custody/membership.
    pub fn commons_until(
        paths: Paths,
        config: PathBuf,
        deadline: std::time::Instant,
    ) -> Result<Self> {
        let bootstrap = Bootstrap::cached_until(&paths, deadline)
            .map_err(crate::table::failure)?
            .ok_or_else(|| Error::new("Commons repair has no secrets custody"))?;
        Self::commons_from_bootstrap(paths, config, &bootstrap)
    }

    fn commons_from_bootstrap(
        paths: Paths,
        config: PathBuf,
        bootstrap: &Bootstrap,
    ) -> Result<Self> {
        let trust = bootstrap.trust();
        let actor = bootstrap
            .request()
            .map_err(crate::table::failure)?
            .binding()
            .actor();
        Ok(Self::commons_context(paths, config, trust, actor))
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
    /// writer grant, which every ingest checks, stands alone.
    fn observe<T>(
        &self,
        bootstrap: &Bootstrap,
        peer: impl Into<Option<Actor>>,
        expected: Option<[u8; 64]>,
        write: bool,
        run: impl FnOnce(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        if bootstrap.trust() != self.trust {
            return Err(Error::new("table repair trust changed"));
        }
        // Acquire every source before session admission and keep it through
        // this local batch. No guard survives the return to network I/O.
        let _sources =
            crate::generation::local_source_reads_until(bootstrap.paths(), admission_deadline())?;
        #[cfg(test)]
        tests::session_boundary(0);
        // Both sessions revalidate the exact vault observation and immutable
        // custody. Only a mutation waits for the control writer; reads use its
        // acknowledged prefix. No session survives this local batch.
        let session = if write {
            Session::open_until(bootstrap, admission_deadline())
        } else {
            Session::observe(bootstrap)
        }
        .map_err(|error| error.context("table repair session admission"))?;
        #[cfg(test)]
        tests::session_boundary(1);
        if session.actor() != self.actor {
            return Err(Error::new("table repair local identity changed"));
        }
        if let Some(peer) = peer.into() {
            session.peer(peer)?;
        }
        if expected.is_some_and(|expected| session.observation().ok() != Some(expected)) {
            return Err(Error::new("control history changed during table repair"));
        }
        run(&session)
    }

    fn round(&self, peer: impl Into<Option<Actor>>) -> Result<Round<'_>> {
        let policy = self
            .commons
            .as_ref()
            .map(|(_, config)| crate::catalog::policy::Policy::read(config))
            .transpose()?;
        let bootstrap = Bootstrap::cached_until(&self.paths, admission_deadline())
            .map_err(|error| {
                Error::from(error).context("table repair bootstrap custody admission")
            })?
            .ok_or_else(|| Error::new("table repair lost secrets custody"))?;
        let (observation, legacy_observation) =
            self.observe(&bootstrap, peer, None, false, |session| {
                Ok((session.observation()?, session.legacy_observation()?))
            })?;
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
            inventory_observation: std::cell::Cell::new(None),
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
        let round = self.round(peer)?;
        round.exchange_with_policy(stream, peer, side, repair, policy)
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
            Ok(store) => Ok(Some(store.inventory(session)?.copied().collect())),
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
                if let Some(wire) = store.wire(session, id)? {
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
            Ok(store) => store.proof_inventory(session).map(Some),
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
                if let Some(wire) = store.proof_wire(session, key)? {
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
        if !round
            .local(None, |session| directory.ring_window(session, now))?
            .contains(&table)
        {
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
                        let ids = store.inventory(session)?.copied().collect();
                        let proofs = store.proof_inventory(session)?;
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

struct Inventory {
    known: BTreeSet<Digest>,
    offered: BTreeSet<Digest>,
    observation: InventoryObservation,
}

struct Round<'a> {
    replica: &'a Replica,
    /// When this side began the round, before its probe: a peer builds its mark after this.
    started: Instant,
    bootstrap: Bootstrap,
    observation: [u8; 64],
    legacy_observation: Option<[u8; 64]>,
    /// The ring windows' clock for this whole exchange.
    now: u64,
    policy: Option<crate::catalog::policy::Policy>,
    tables: BTreeMap<Digest, (Scope, PathBuf)>,
    /// Locally selected storage; switching to recovery never changes membership or the actor.
    commons: Option<(crate::catalog::Directory, PathBuf)>,
    active_table: std::cell::Cell<Option<Digest>>,
    inventory_observation: std::cell::Cell<Option<InventoryObservation>>,
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
        let needs = self.local(peer, |session| {
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

    /// One admitted read of this Mac's state. A store's observer refuses a witness that advances
    /// while it reads ([`Error::snapshot_changed`]). Another of this Mac's rounds ingesting into
    /// that table does just that, as does one ingesting into the catalog, which a Commons round's
    /// admission reads once it has selected an application table outside the ring window
    /// ([`Self::check_table_policy`]). A fresh admission and read is the answer, where giving up
    /// would cost both peers this round. [`LOCAL_READS`] bounds the reads: no sleep separates
    /// them, though each still waits for its own admission.
    fn local<T>(
        &self,
        peer: impl Into<Option<Actor>>,
        mut run: impl FnMut(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        let peer = peer.into();
        for _ in 1..LOCAL_READS {
            match self.admit(peer, false, &mut run) {
                Err(error) if error.is_snapshot_changed() => {}
                result => return result,
            }
        }
        self.admit(peer, false, run)
    }

    /// A [`Self::local`] read's result where the round sends an inventory next: a table that kept
    /// advancing through every read is deferred with `refusal`, as any other store this side
    /// cannot read now, and the round goes on, because an inventory can say it is unavailable and
    /// the peer defers on that too. That covers these reads' own admissions, whose read of a
    /// catalog that keeps advancing refuses the same way. No other read defers like this. A batch
    /// cannot say so ([`Self::send_batch`]), and a bare admission, such as the authorize that runs
    /// next, refuses what it could not authorize, so each of those still costs both peers the round.
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

    fn local_write<T>(
        &self,
        peer: impl Into<Option<Actor>>,
        run: impl FnOnce(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        self.admit(peer, true, run)
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
        run: impl FnOnce(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        // A peer shares the view this round probed, so each admission holds it to that view.
        // With no peer (the mesh's calls) nobody shares it: a control change since the round
        // began is no refusal, and each record's own writer grant is checked at its ingest.
        let peer = peer.into();
        self.replica.observe(
            &self.bootstrap,
            peer,
            peer.map(|_| self.observation),
            write,
            |session| {
                // Custody and journal replay can take time. Read local policy
                // after admitting the session, before admitting this batch.
                self.check_table_policy(session)?;
                run(session)
            },
        )
    }

    fn check_table_policy(&self, session: &Session<'_>) -> Result<()> {
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
        if let Some((directory, _)) = &self.commons
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
    /// admission when none is established yet.
    fn select_tables(&mut self, peer: Option<Actor>, report: &mut Report) -> Result<()> {
        if let Some((directory, config)) = self.commons.as_ref() {
            let recovery = self.local(peer, |session| {
                match directory.observe_primary_with_config(session, config) {
                    Ok(_) => Ok(false),
                    Err(error) if error.is_busy() => Err(error),
                    Err(_) => Ok(true),
                }
            })?;
            if recovery {
                self.commons = Some((directory.recovery(), config.clone()));
                report.recovery = true;
            }
        }
        if let Some((directory, _)) = &self.commons {
            let policy = self.policy.as_ref().expect("Commons policy captured");
            let existing = self.local(peer, |session| {
                let Some(mut tables) = directory.existing_subscriptions(session, policy)? else {
                    return Ok(None);
                };
                // Ring tables join the catalog's; damaged ring state asks for the writer, whose
                // retirement rebuilds it. The catalog's tables are repaired whatever the rings
                // say: the offered tables' window reads a damaged state as none (`ring_view`).
                if directory.is_recovery() {
                    return Ok(Some(tables));
                }
                match directory.existing_ring_subscriptions(session, policy, self.now) {
                    Ok(Some(rings)) => {
                        tables.extend(rings);
                        Ok(Some(tables))
                    }
                    Ok(None) | Err(_) => Ok(None),
                }
            })?;
            self.tables = match existing {
                Some(tables) => tables,
                None => self.local_write(peer, |session| {
                    let mut tables = directory.subscriptions(session, policy)?;
                    if !directory.is_recovery() {
                        tables.extend(
                            directory
                                .ring_subscriptions(session, policy, self.now)
                                .unwrap_or_default(),
                        );
                    }
                    Ok(tables)
                })?,
            };
        }
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
                let window = directory.ring_window(session, self.now)?;
                Ok(remote
                    .difference(&local)
                    .filter(|id| window.contains(*id))
                    .copied()
                    .collect::<BTreeSet<_>>())
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

    fn inventory(
        &self,
        peer: Actor,
        id: Digest,
        refusal: &mut Option<String>,
        cause: &mut Option<aterm_types::network_state::RepairCause>,
    ) -> Result<Option<Inventory>> {
        let read = self.local(peer, |session| {
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
            let known = store.inventory(session)?.copied().collect();
            Ok(Some(Inventory {
                observation: InventoryObservation {
                    scope: store.scope(),
                    local_actor: session.actor(),
                    peer_actor: peer,
                    control_observation: session.observation()?,
                    inventory: InventorySummary::of(&known),
                    observed_at_ms,
                    sampled_at,
                },
                known,
                offered: store.replication_inventory_unchecked().copied().collect(),
            }))
        });
        Self::table_read(read, refusal, cause)
    }

    /// The witnesses of `common` under `salt` (`witness`), in digest order: each table's operation
    /// inventory as its table round reads it, under the admission and subscription check that
    /// round's reads take. A table this side cannot read now, holds no storage for, lies past the
    /// first `read` tables, or comes after [`WITNESS_BUDGET`] reads as [`witness::UNKNOWN`]: it is
    /// repaired early, never skipped.
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
                let content = self.local(peer, |session| {
                    let store = if let Some((directory, _)) = &self.commons {
                        let (scope, _) = &self.tables[id];
                        // Storage this side never enrolled stays so until its table round.
                        let Some(store) = directory.observe_store_if_exists(session, *scope)?
                        else {
                            return Ok(None);
                        };
                        if !directory.is_recovery()
                            && *scope != directory.catalog_scope()
                            && let Some(recovery) = directory
                                .recovery()
                                .observe_store_if_exists(session, *scope)?
                        {
                            recovery
                        } else {
                            store
                        }
                    } else {
                        self.store(session, *id)?
                    };
                    let known = store.inventory(session)?;
                    Ok(Some(
                        InventorySummary::of_sorted(known.len(), known).fingerprint,
                    ))
                });
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
        let before = self.inventory_observation.get()?;
        if report.local_known != report.peer_known || report.deferred != 0 {
            return None;
        }
        let after = self
            .inventory(peer, id, &mut None, &mut None)
            .ok()??
            .observation;
        MatchingInventory::from_observations(report, before, after)
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
        let local = self.local(peer, |session| {
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
            store.proof_inventory(session).map(Some)
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
            let absent = self
                .local(peer, |session| {
                    // A damaged established store stays deferred. Only storage with no
                    // enrollment yet (an empty discovery reservation, or a first
                    // enrollment cut short after its pending claim) needs `join`.
                    Ok(matches!(
                        self.commons
                            .as_ref()
                            .expect("Commons checked")
                            .0
                            .observe_store_if_exists(session, *scope),
                        Ok(None)
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
        let local = self
            .inventory(peer, id, &mut local_error, &mut local_cause)
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
        report.local_offered = Some(InventorySummary::of(&local.offered));
        report.peer_known = Some(InventorySummary::of(&remote));
        self.backfill
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .observe(id, &local.known, &remote);
        self.inventory_observation.set(Some(local.observation));
        let mut ids: Vec<_> = cycle(&local.offered, repair.records.get(&id).copied())
            .filter(|v| !remote.contains(*v))
            .take(BATCH + 1)
            .copied()
            .collect();
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
        let batch = self.local(peer, |session| {
            let batch = prepare(session)?;
            // Config writers need no custody lock. Preserve the send boundary's
            // policy check even when preparation takes time.
            self.check_table_policy(session)?;
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
            self.local(peer, |_| Ok(()))?;
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
