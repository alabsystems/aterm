// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Repair independent Commons control and membership history over exporter-bound TLS.
//! Historical authority evidence grants sealed control and verified membership
//! history exchange. Table, vault, administrator and terminal rights are
//! checked separately.
//! No network epoch key is needed, so a retired/offline device can learn newer
//! restrictions. This module neither dials nor listens: it accepts connections
//! admitted by the native LAN policy and rechecks their bound route observations.

use crate::{Error, Result};
use astream_wire::sync::{Actor, Digest};
use aterm_digest::Sha256;
use aterm_net::fleet::{self, FleetOp, FleetScope, FleetSecret, NodeIdentity, NodeProof};
use aterm_secrets::commons::{
    binding::{REQUEST_LEN, Request},
    constraints::Fact,
    hearing::{self, REPORT_LEN, Said, Version},
    journal::{Journal, MAX_NODES, Trust},
    node::{Kind, MAX_NODE, Node},
};
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(crate) mod enrollment;
mod peers;

/// At most 4.5 MiB of sealed nodes are buffered per direction. The durable
/// inventory, rather than an acknowledgement cursor, determines missing data.
pub const RECORDS_PER_ROUND: usize = 16;
const HELLO_FIXED: usize = 4 + 16 + 32 + 32 + REQUEST_LEN;
const MAX_HELLO: usize = HELLO_FIXED + MAX_NODE;
const MAX_ACX2_HELLO: usize = HELLO_FIXED + 4 * 1024 * 1024;

fn failure(_: impl std::fmt::Display) -> Error {
    Error::new("Commons control journal refused the operation")
}

// Secrets errors name failures without exposing secret values. Preserve that
// local evidence at the batch boundary; remote witness failures stay generic.
fn journal_failure(operation: &str, error: aterm_secrets::Error) -> Error {
    Error(format!("Commons control {operation}: {error}"))
}

/// The connection's fixed ordering. This avoids simultaneous large writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Initiator,
    Responder,
}

/// The local operation most recently entered by this attempt. A failed phase
/// does not identify the remote cause or establish delivery or convergence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Connecting,
    Route,
    Tls,
    Membership,
    PeerAdmission,
    ControlInventory,
    InventorySend,
    InventoryReceive,
    ControlBatch,
    /// Send this batch and wait for its durable receipts.
    BatchSend,
    /// Receive a batch, retain its nodes and send receipts.
    BatchReceive,
    ControlComplete,
    /// Negotiate recovery support and retain signed membership records.
    MembershipRecords,
    Application,
    VersionReport,
    /// Optional holder-to-custodian transfer after a successful application round.
    Credentials,
    /// Application repair and optional version reporting finished locally.
    Complete,
}

impl Phase {
    /// Bounded metadata label; no peer data or key material is included.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Route => "route",
            Self::Tls => "tls",
            Self::Membership => "membership",
            Self::PeerAdmission => "peer-admission",
            Self::ControlInventory => "control-local-inventory",
            Self::InventorySend => "control-inventory-send",
            Self::InventoryReceive => "control-inventory-receive",
            Self::ControlBatch => "control-local-batch",
            Self::BatchSend => "control-batch-send-and-receipts",
            Self::BatchReceive => "control-batch-receive-and-retain",
            Self::ControlComplete => "control-complete",
            Self::MembershipRecords => "membership-records",
            Self::Application => "application",
            Self::VersionReport => "version-report",
            Self::Credentials => "credentials",
            Self::Complete => "complete",
        }
    }
}

/// Durable acknowledgements in one bounded round. A zero report does not prove
/// global convergence: other peers or previously unknown branches may exist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub sent: usize,
    pub received: usize,
    /// Offered nodes without a durable acknowledgement, including local refusal.
    pub deferred: usize,
}

/// How long a peer is offered no membership records once it has refused every record this Mac
/// could offer it under one pair of complete inventories ([`MembershipHint`]). Under that
/// evidence only a busy or failing disk makes the next offer fare differently; the hold bounds
/// how late such a refusal is retried, as `network::Repair`'s deferral lapses after the same 60 s.
const REFUSAL_HOLD: Duration = Duration::from_secs(60);

/// Per-peer membership-record scheduling, never a claim of delivery: where the next round's
/// selection resumes, and the run of rounds the peer took none of under one pair of complete
/// inventories.
#[derive(Clone, Default, Debug)]
struct MembershipHint {
    cursor: Option<Digest>,
    refused: Option<Refusal>,
}

/// Consecutive rounds a peer durably took none of, all offered under one pair of complete
/// inventories.
#[derive(Clone, Copy, Debug)]
struct Refusal {
    /// [`hold_key`] of both inventories, so a change on either side ends the run.
    key: Digest,
    /// The run's first offered record. A refused round that offers it again has offered every
    /// record the peer lacks at least once since the run began.
    first: Digest,
    /// When the run last proved the peer refuses whatever this Mac can offer it; `None` while
    /// the run falls short of that.
    held: Option<Instant>,
}

impl MembershipHint {
    /// Whether a round at `now`, under the inventories whose [`hold_key`] is `key`, offers the
    /// peer anything: not for [`REFUSAL_HOLD`] after a run under that key proved it refuses
    /// everything. Two full stores on different branches otherwise re-offered 16 refused records
    /// a round, three rounds a pass, every pass. A change of either inventory ends the run.
    fn offers(&mut self, key: &Digest, now: Instant) -> bool {
        match self.refused {
            Some(run) if run.key != *key => {
                self.refused = None;
                true
            }
            Some(Refusal { held: Some(at), .. }) => {
                now.saturating_duration_since(at) >= REFUSAL_HOLD
            }
            _ => true,
        }
    }

    /// Note a round that offered `offered`, in selection order, under the inventories whose
    /// [`hold_key`] is `key`, of which the peer durably took `taken`. The peer refuses whatever
    /// this Mac can offer once a run of refused rounds offered every record it lacks (the run's
    /// first record comes round again), or at once when `full`: a peer whose complete records
    /// reach `MAX_RECORDS` takes no complete record (`Log::insert`). A run short of that proves
    /// nothing, as a busy disk refuses a round and a peer parks a record whose parent it lacks
    /// without taking it. Once proved, the walk restarts at the earliest record the peer lacks,
    /// so the round after the hold offers parents before their children, and a refused round
    /// under the same key renews the hold.
    fn settle(&mut self, key: Digest, offered: &[Digest], taken: usize, full: bool, now: Instant) {
        let Some(&first) = offered.first() else {
            return;
        };
        if taken > 0 {
            self.refused = None;
            return;
        }
        let proved = match &mut self.refused {
            Some(run) if run.key == key => {
                if run.held.is_some() {
                    run.held = Some(now);
                    false
                } else {
                    offered.contains(&run.first)
                }
            }
            _ => {
                self.refused = Some(Refusal {
                    key,
                    first,
                    held: None,
                });
                full
            }
        };
        if proved && let Some(run) = &mut self.refused {
            run.held = Some(now);
            self.cursor = None;
        }
    }
}

/// Digest of the two sorted, unique complete inventories a round was offered under, so a hint
/// keeps one digest per peer, not its ids.
fn hold_key(local: &BTreeSet<Digest>, remote: &BTreeSet<Digest>) -> Digest {
    let mut hash = Sha256::new();
    for ids in [local, remote] {
        hash.update((ids.len() as u64).to_le_bytes());
        for id in ids {
            hash.update(id);
        }
    }
    hash.finalize()
}

/// Volatile scheduling hint, never a claim of delivery. Keep one per peer.
/// Advancing past a deferred node prevents it from starving later restrictions;
/// wraparound retries it even if the caller does not maintain a timer.
#[derive(Clone, Default, Debug)]
pub struct Repair {
    phase: Phase,
    peer: Option<(Trust, Actor)>,
    authenticated: Option<Instant>,
    /// This side dialed, sent its hello over an established channel, and the peer closed or went
    /// quiet before sending its own ([`Self::unanswered`]).
    unanswered: bool,
    cursor: Option<Digest>,
    membership_hint: MembershipHint,
    data: crate::replica::Repair,
    application: Option<crate::replica::Report>,
    membership: Option<Report>,
    membership_error: Option<String>,
    said: Said,
    application_error: Option<String>,
    handoff: Option<std::result::Result<usize, String>>,
}
impl Repair {
    /// This attempt's local progress, including its last operation on failure.
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    fn clear_application(&mut self) {
        self.application = None;
        self.membership = None;
        self.membership_error = None;
        self.said = Said::Unknown;
        self.application_error = None;
        self.handoff = None;
        self.data.retry_probe = false;
        self.data.deferral = None;
        self.data.batch_deferral = None;
        self.data.interrupted = None;
    }
    /// The most recently channel-authenticated peer; the round may have failed.
    pub fn actor(&self) -> Option<Actor> {
        self.peer.map(|(_, actor)| actor)
    }
    pub(crate) fn begin_round(&mut self) {
        self.phase = Phase::Connecting;
        self.authenticated = None;
        self.unanswered = false;
        self.clear_application();
    }
    /// This round's initiator sent its hello over an established channel and the peer closed or
    /// went quiet before sending its own. A responder answers only once it has admitted the
    /// round, so this is how contention looks from the dialing side (a crossed dial whose other
    /// channel holds the pair, a peer cache at capacity, a preferred round that waited out its
    /// deadline), and equally a peer that refuses this Mac or has not yet approved its enrollment:
    /// the initiator cannot tell them apart.
    pub(crate) const fn unanswered(&self) -> bool {
        self.unanswered
    }
    /// Receive the peer's hello; an initiator that gets none of it marks [`Self::unanswered`].
    fn receive_answer(&mut self, stream: &mut impl Read, side: Side) -> Result<Vec<u8>> {
        let n = read_u32(stream).inspect_err(|_| self.unanswered = side == Side::Initiator)?;
        frame_body(stream, n, MAX_ACX2_HELLO)
    }
    /// An older application probe failed before any application data was sent.
    pub(crate) fn retry_application_probe(&self) -> bool {
        self.data.retry_probe
    }
    /// The peer authenticated during the current round, including a round
    /// whose later control or application exchange failed. A previous round's
    /// identity alone never makes this observation current.
    pub fn authenticated_actor(&self) -> Option<Actor> {
        self.authenticated.and(self.actor())
    }
    pub(crate) fn authenticated_contact(&self) -> Option<(Actor, Instant)> {
        self.actor().zip(self.authenticated)
    }
    /// What this round's peer said of its version ([`hearing::report`]).
    pub(crate) const fn said(&self) -> Said {
        self.said
    }
    /// Only a fully completed application round has a report. None also covers
    /// legacy peers and deferred/failed upgrades, never a claim of no data.
    pub const fn application(&self) -> Option<crate::replica::Report> {
        self.application
    }
    /// Observed progress before an application error, not a completed exchange.
    /// Counts are lower bounds and include idempotent ingests. None also covers
    /// admission/probe failures; it never proves no data moved. A new round clears it.
    pub const fn application_interrupted(&self) -> Option<crate::replica::Report> {
        self.data.interrupted
    }
    /// Membership-record acknowledgements from a completed negotiated exchange,
    /// before application repair under `atdb/2` or after it for older F13 peers.
    /// None also covers unavailable, failed or unsupported membership exchange.
    pub const fn membership(&self) -> Option<Report> {
        self.membership
    }
    /// Failure of the negotiated membership exchange. Control history may
    /// already be retained; no later application exchange was admitted.
    pub fn membership_error(&self) -> Option<&str> {
        self.membership_error.as_deref()
    }

    /// Credential transfers published this round, or a bounded protocol failure. None
    /// means no runtime handoff was negotiated; it says nothing about vault contents.
    pub fn handoff(&self) -> Option<&std::result::Result<usize, String>> {
        self.handoff.as_ref()
    }
    /// The last application upgrade's failure in this round. A missing error
    /// does not establish that an application round completed; check
    /// [`Self::application`] separately. Starting another round clears it.
    pub fn application_error(&self) -> Option<&str> {
        self.application_error.as_deref()
    }
    /// An unavailable inventory in a completed application round. This preserves
    /// a local refusal separately from a peer's unexplained unavailable inventory.
    /// Starting another round clears it without discarding scheduling hints.
    pub fn application_deferral(&self) -> Option<&crate::replica::InventoryDeferral> {
        self.data.deferral.as_ref()
    }
    /// First local batch refusal in this round, including a completed exchange
    /// that acknowledged no incoming operations. Starting another round clears it.
    pub fn application_batch_deferral(&self) -> Option<&crate::replica::BatchDeferral> {
        self.data.batch_deferral.as_ref()
    }
    fn prepare(&mut self, trust: Trust, actor: Actor) {
        self.authenticated = Some(Instant::now());
        if self.peer != Some((trust, actor)) {
            self.cursor = None;
            self.membership_hint = MembershipHint::default();
            self.peer = Some((trust, actor));
            self.data = crate::replica::Repair::default();
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbControlRepair",
            action = "RefusedFirst",
            project = "aterm_db::control::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbControlRepair",
            action = "DeliveredMiddle",
            project = "aterm_db::control::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbControlRepair",
            action = "DeliveredLast",
            project = "aterm_db::control::tests::repair_transition"
        )
    )]
    fn next(&mut self, local: &BTreeSet<Digest>, remote: &BTreeSet<Digest>) -> Vec<Digest> {
        let ids = select(local, remote, self.cursor);
        if let Some(last) = ids.last() {
            self.cursor = Some(*last);
        }
        ids
    }
}

/// Bounded scheduling hints for one runtime, retained across endpoint rebuilds.
/// These hold public IDs and round metadata, never a bootstrap or authority lease.
#[derive(Default)]
pub(crate) struct PeerCache(Mutex<Option<CacheScope>>);

struct CacheScope {
    trust: Trust,
    actor: Actor,
    peers: Arc<peers::Cache>,
    backfill: Arc<Mutex<crate::replica::Backfill>>,
}

/// Locally pinned journal plus an authority-approved device signing identity.
/// Construction uses an already enrolled persistent device; it never creates a
/// network, age identity, writer incarnation, epoch key or plaintext key file.
pub struct Endpoint {
    dir: PathBuf,
    trust: Trust,
    actor: Actor,
    identity: NodeIdentity,
    hello: Vec<u8>,
    /// This device's signed version, traded after a completed round; empty while pending.
    report: Vec<u8>,
    application: Option<crate::replica::Replica>,
    peers: Arc<peers::Cache>,
    paths: Option<aterm_secrets::client::Paths>,
    credentials: Option<(crate::runtime::CredentialOwner, PathBuf)>,
    pending: bool,
    /// Behave as 0.145 does: trade no version after a round.
    #[cfg(test)]
    pub(crate) legacy: bool,
    /// Test an F13 peer whose TLS still selects the application-first order.
    #[cfg(test)]
    pub(crate) legacy_repair: bool,
    /// Use the released 0.146 projection and `atdb/2` TLS offer in wire tests.
    #[cfg(test)]
    pub(crate) legacy_projection: bool,
}

/// One exchange's absolute deadline: the product's [`crate::network::WITHIN`]. A test needs each
/// round to complete, and waits out a loaded machine's round as a one-minute hang detector: a
/// round the deadline cuts reads as a missing application report.
#[cfg(not(test))]
const EXCHANGE: Duration = crate::network::WITHIN;
#[cfg(test)]
const EXCHANGE: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Protocol {
    Legacy,
    Repair,
    Credentials,
}

impl Protocol {
    fn from_alpn(alpn: Option<&[u8]>) -> Self {
        match alpn {
            Some(fleet::ALPN_DATABASE_CREDENTIALS) => Self::Credentials,
            Some(fleet::ALPN_DATABASE_REPAIR) => Self::Repair,
            _ => Self::Legacy,
        }
    }
}

impl Endpoint {
    /// Reuse only volatile repair hints for this same pinned trust and local
    /// writer. Every new connection still authenticates before leasing them.
    pub(crate) fn with_repair_cache(mut self, cache: &PeerCache) -> Self {
        let mut current = cache.0.lock().unwrap_or_else(|poison| poison.into_inner());
        if !current
            .as_ref()
            .is_some_and(|scope| scope.trust == self.trust && scope.actor == self.actor)
        {
            *current = Some(CacheScope {
                trust: self.trust,
                actor: self.actor,
                peers: Arc::default(),
                backfill: Arc::default(),
            });
        }
        let scope = current.as_ref().expect("cache scope installed");
        self.peers = scope.peers.clone();
        if let Some(application) = &mut self.application {
            application.share_backfill(&scope.backfill);
        }
        self
    }

    pub(crate) fn with_credential_owner(
        mut self,
        owner: crate::runtime::CredentialOwner,
        config: PathBuf,
    ) -> Self {
        self.credentials = Some((owner, config));
        self
    }

    /// Construct an endpoint from this batch's already checked membership and
    /// historical binding. Construction performs no additional journal open.
    /// The returned endpoint holds no session lock; drop the session before
    /// exchanging data. Each application batch still rechecks fresh custody.
    pub fn from_session(session: &crate::table::Session<'_>) -> Result<Self> {
        let (bootstrap, journal) = session.control_context()?;
        let request = bootstrap.request().map_err(failure)?;
        if let Some(log_witness) = bootstrap.log_witness().map_err(failure)? {
            return bootstrap.device().with_seed(|seed| {
                let mut endpoint = Self::configured_log(
                    bootstrap.journal_dir(),
                    bootstrap.trust(),
                    &request,
                    &log_witness,
                    seed,
                )?;
                endpoint.paths = Some(bootstrap.paths().clone());
                Ok(endpoint)
            });
        }
        let witness = bootstrap
            .witness_in(journal)
            .map_err(failure)?
            .ok_or_else(|| Error::new("Commons membership has no historical approval"))?;
        bootstrap.device().with_seed(|seed| {
            let mut endpoint = Self::configured(
                bootstrap.journal_dir(),
                bootstrap.trust(),
                &request,
                witness.bytes(),
                seed,
            )?;
            endpoint.paths = Some(bootstrap.paths().clone());
            Ok(endpoint)
        })
    }

    /// Attach default Commons subscriptions using the same locked observation
    /// as local table inspection. Paths must identify that session's custody;
    /// all network repair retains its normal fresh authorization checks.
    pub fn with_commons_from_session(
        mut self,
        session: &crate::table::Session<'_>,
        paths: aterm_secrets::client::Paths,
        config: PathBuf,
    ) -> Result<Self> {
        let (bootstrap, _) = session.control_context()?;
        let application =
            crate::replica::Replica::commons_from_session(paths.clone(), config, session)?;
        if self.dir != bootstrap.journal_dir() || !application.matches(self.trust, self.actor) {
            return Err(Error::new(
                "Commons subscriptions have another control identity",
            ));
        }
        self.application = Some(application);
        self.paths = Some(paths);
        Ok(self)
    }

    /// Subscribe to the enrolled lab's catalog and its declared tables by
    /// default, honoring local policy before every application batch.
    pub fn with_commons(
        mut self,
        paths: aterm_secrets::client::Paths,
        config: PathBuf,
    ) -> Result<Self> {
        let application = crate::replica::Replica::commons(paths.clone(), config)?;
        if !application.matches(self.trust, self.actor) {
            return Err(Error::new(
                "Commons subscriptions have another control identity",
            ));
        }
        self.application = Some(application);
        self.paths = Some(paths);
        Ok(self)
    }

    /// [`Self::with_commons`] with a shared absolute deadline for local custody
    /// admission. Historical control repair does not require current table
    /// membership; application batches retain their own fresh checks.
    pub fn with_commons_until(
        mut self,
        paths: aterm_secrets::client::Paths,
        config: PathBuf,
        deadline: Instant,
    ) -> Result<Self> {
        let application = crate::replica::Replica::commons_until(paths.clone(), config, deadline)?;
        if !application.matches(self.trust, self.actor) {
            return Err(Error::new(
                "Commons subscriptions have another control identity",
            ));
        }
        self.application = Some(application);
        self.paths = Some(paths);
        Ok(self)
    }
    /// Make the rounds this endpoint initiates repair `first`'s table first while it names one both
    /// sides hold ([`crate::replica::Replica::prefer`]); an endpoint with no tables has none to.
    pub(crate) fn prefer(&mut self, first: crate::replica::First) {
        if let Some(application) = self.application.as_mut() {
            application.prefer(first);
        }
    }

    /// Attach an explicit local table set. Discovery/subscription is a higher
    /// layer; no table path or admission can come from a peer's packet.
    pub fn with_tables(
        mut self,
        paths: aterm_secrets::client::Paths,
        tables: Vec<(crate::table::Scope, PathBuf)>,
    ) -> Result<Self> {
        let application = crate::replica::Replica::new(paths.clone(), tables)?;
        if !application.matches(self.trust, self.actor) {
            return Err(Error::new(
                "application repair has another control identity",
            ));
        }
        self.application = Some(application);
        self.paths = Some(paths);
        Ok(self)
    }

    /// Place this endpoint's ring windows by `secs` (seconds since the Unix epoch), not the
    /// system clock.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_ring_clock(mut self, secs: u64) -> Self {
        if let Some(application) = &mut self.application {
            application.ring_clock = Some(secs);
        }
        self
    }

    /// Attach local secrets custody paths so control-only endpoints exchange verified membership
    /// records alongside Commons control history.
    #[must_use]
    pub fn with_membership_paths(mut self, paths: aterm_secrets::client::Paths) -> Self {
        self.paths = Some(paths);
        self
    }
    /// Build the control-only endpoint from encrypted secrets custody. Pending
    /// enrollment has no historical witness and cannot open an endpoint.
    pub fn from_bootstrap(
        bootstrap: &aterm_secrets::commons::bootstrap::Bootstrap,
    ) -> Result<Option<Self>> {
        let journal =
            Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).map_err(failure)?;
        Self::from_bootstrap_in(bootstrap, &journal)
    }

    /// [`Self::from_bootstrap`] with bounded admission to the existing local
    /// journal. Historical approval remains sufficient for control repair,
    /// including after retirement; no current-membership Session is required.
    pub fn from_bootstrap_until(
        bootstrap: &aterm_secrets::commons::bootstrap::Bootstrap,
        deadline: Instant,
    ) -> Result<Option<Self>> {
        let journal =
            Journal::open_existing_until(bootstrap.journal_dir(), bootstrap.trust(), deadline)
                .map_err(failure)?;
        bootstrap.revalidate_custody().map_err(failure)?;
        Self::from_bootstrap_in(bootstrap, &journal)
    }

    fn from_bootstrap_in(
        bootstrap: &aterm_secrets::commons::bootstrap::Bootstrap,
        journal: &Journal,
    ) -> Result<Option<Self>> {
        if let Some(log_witness) = bootstrap.log_witness().map_err(failure)? {
            let request = bootstrap.request().map_err(failure)?;
            return bootstrap
                .device()
                .with_seed(|seed| {
                    let mut endpoint = Self::configured_log(
                        bootstrap.journal_dir(),
                        bootstrap.trust(),
                        &request,
                        &log_witness,
                        seed,
                    )?;
                    endpoint.paths = Some(bootstrap.paths().clone());
                    Ok(endpoint)
                })
                .map(Some);
        }
        let Some(witness) = bootstrap.witness_in(journal).map_err(failure)? else {
            return Ok(None);
        };
        let request = bootstrap.request().map_err(failure)?;
        bootstrap
            .device()
            .with_seed(|seed| {
                let mut endpoint = Self::configured(
                    bootstrap.journal_dir(),
                    bootstrap.trust(),
                    &request,
                    witness.bytes(),
                    seed,
                )?;
                endpoint.paths = Some(bootstrap.paths().clone());
                Ok(endpoint)
            })
            .map(Some)
    }

    /// An authenticated but not yet bound device may discover and dial an
    /// approver. This endpoint must never advertise or accept connections.
    pub fn pending_from_bootstrap(
        bootstrap: &aterm_secrets::commons::bootstrap::Bootstrap,
        paths: aterm_secrets::client::Paths,
    ) -> Result<Self> {
        if bootstrap.log_witness().map_err(failure)?.is_some()
            || bootstrap.witness().map_err(failure)?.is_some()
        {
            return Err(Error::new(
                "an enrolled device cannot use pending enrollment",
            ));
        }
        Self::configured_pending(bootstrap, paths)
    }

    /// [`Self::pending_from_bootstrap`] with bounded local journal admission.
    /// This still refuses historical approval and never accepts inbound peers.
    pub fn pending_from_bootstrap_until(
        bootstrap: &aterm_secrets::commons::bootstrap::Bootstrap,
        paths: aterm_secrets::client::Paths,
        deadline: Instant,
    ) -> Result<Self> {
        if bootstrap.log_witness().map_err(failure)?.is_some()
            || bootstrap
                .witness_until(deadline)
                .map_err(failure)?
                .is_some()
        {
            return Err(Error::new(
                "an enrolled device cannot use pending enrollment",
            ));
        }
        Self::configured_pending(bootstrap, paths)
    }

    fn configured_pending(
        bootstrap: &aterm_secrets::commons::bootstrap::Bootstrap,
        paths: aterm_secrets::client::Paths,
    ) -> Result<Self> {
        let request = bootstrap.request().map_err(failure)?;
        bootstrap.device().with_seed(|seed| {
            let identity = NodeIdentity::from_seed(
                &aterm_codec::hex::encode_lower(request.binding().incarnation()),
                seed,
            )
            .ok_or_else(|| Error::new("invalid pending device identity"))?;
            if identity.key().bytes() != request.binding().writer() {
                return Err(Error::new(
                    "pending device identity does not match its request",
                ));
            }
            let mut hello = b"ACE1".to_vec();
            let trust = bootstrap.trust();
            hello.extend_from_slice(&trust.network);
            hello.extend_from_slice(&trust.authority);
            hello.extend_from_slice(&trust.genesis);
            hello.extend_from_slice(request.bytes());
            Ok(Self {
                dir: bootstrap.journal_dir().into(),
                trust,
                actor: request.binding().actor(),
                identity,
                hello,
                report: Vec::new(),
                application: None,
                peers: Arc::default(),
                paths: Some(paths),
                credentials: None,
                pending: true,
                #[cfg(test)]
                legacy: false,
                #[cfg(test)]
                legacy_repair: false,
                #[cfg(test)]
                legacy_projection: false,
            })
        })
    }

    pub(crate) const fn pending(&self) -> bool {
        self.pending
    }

    /// `witness` is a signed ACN3 node containing this request's exact Bind fact.
    /// A genesis witness must equal the independent pin. Other signed witnesses
    /// establish historical approval only, even before their ancestry arrives.
    /// No journal lock is retained by this handle.
    pub fn new(
        dir: &Path,
        trust: Trust,
        request: &Request,
        witness: &[u8],
        seed: &[u8; 32],
    ) -> Result<Self> {
        let endpoint = Self::configured(dir, trust, request, witness, seed)?;
        drop(Journal::open(dir, trust).map_err(failure)?);
        Ok(endpoint)
    }

    fn configured_log(
        dir: &Path,
        trust: Trust,
        request: &Request,
        log_witness: &[u8],
        seed: &[u8; 32],
    ) -> Result<Self> {
        let _ = check_log_witness(trust, request, log_witness)?;
        let identity = NodeIdentity::from_seed(
            &aterm_codec::hex::encode_lower(request.binding().incarnation()),
            seed,
        )
        .ok_or_else(|| Error::new("invalid Commons signing identity"))?;
        if identity.key().bytes() != request.binding().writer() {
            return Err(Error::new(
                "Commons writer differs from its persistent identity",
            ));
        }
        let mut hello = b"ACX2".to_vec();
        hello.extend_from_slice(&trust.network);
        hello.extend_from_slice(&trust.authority);
        hello.extend_from_slice(&trust.genesis);
        hello.extend_from_slice(request.bytes());
        hello.extend_from_slice(log_witness);
        let actor = request.binding().actor();
        let report =
            hearing::report(seed, trust.network, &actor, Version::own()).map_err(failure)?;
        Ok(Self {
            dir: dir.into(),
            trust,
            actor,
            identity,
            hello,
            report,
            application: None,
            credentials: None,
            peers: Arc::default(),
            paths: None,
            pending: false,
            #[cfg(test)]
            legacy: false,
            #[cfg(test)]
            legacy_repair: false,
            #[cfg(test)]
            legacy_projection: false,
        })
    }

    fn configured(
        dir: &Path,
        trust: Trust,
        request: &Request,
        witness: &[u8],
        seed: &[u8; 32],
    ) -> Result<Self> {
        check_witness(trust, request, witness)?;
        let identity = NodeIdentity::from_seed(
            &aterm_codec::hex::encode_lower(request.binding().incarnation()),
            seed,
        )
        .ok_or_else(|| Error::new("invalid Commons signing identity"))?;
        if identity.key().bytes() != request.binding().writer() {
            return Err(Error::new(
                "Commons writer differs from its persistent identity",
            ));
        }
        let mut hello = b"ACX1".to_vec();
        hello.extend_from_slice(&trust.network);
        hello.extend_from_slice(&trust.authority);
        hello.extend_from_slice(&trust.genesis);
        hello.extend_from_slice(request.bytes());
        hello.extend_from_slice(witness);
        let actor = request.binding().actor();
        let report =
            hearing::report(seed, trust.network, &actor, Version::own()).map_err(failure)?;
        Ok(Self {
            dir: dir.into(),
            trust,
            actor,
            identity,
            hello,
            report,
            application: None,
            credentials: None,
            peers: Arc::default(),
            paths: None,
            pending: false,
            #[cfg(test)]
            legacy: false,
            #[cfg(test)]
            legacy_repair: false,
            #[cfg(test)]
            legacy_projection: false,
        })
    }

    /// Exchange over a connection admitted by the native LAN/loopback policy.
    /// One absolute deadline ([`EXCHANGE`]) covers TLS, authentication and repair.
    /// Route evidence is rechecked after TLS and after the bounded exchange.
    /// This does not start any automatic network service.
    pub fn exchange(
        &self,
        connection: aterm_net::lan::Connection,
        side: Side,
        repair: &mut Repair,
    ) -> Result<Report> {
        repair.begin_round();
        repair.phase = Phase::Route;
        repair.clear_application();
        let (tcp, locality) = connection.into_parts();
        locality.recheck()?;
        let until = Instant::now() + EXCHANGE;
        let _deadline = crate::network::Deadline::arm_within(&tcp, EXCHANGE)?;
        // This PUBLIC discriminator separates networks/protocols. It is not an
        // admission secret. Authority witness + exporter-bound identity below
        // are mandatory before any inventory or control repair is exposed.
        let mut domain = b"aterm/commons/control-transport/1\0".to_vec();
        domain.extend_from_slice(&self.trust.network);
        domain.extend_from_slice(&self.trust.authority);
        domain.extend_from_slice(&self.trust.genesis);
        let discriminator = FleetSecret::new(&Sha256::digest(&domain))
            .map_err(|_| Error::new("invalid Commons protocol discriminator"))?;
        #[cfg(test)]
        let negotiate_recovery = !self.legacy && !self.legacy_repair;
        #[cfg(not(test))]
        let negotiate_recovery = true;
        repair.phase = Phase::Tls;
        match side {
            Side::Initiator => {
                let mut channel = if negotiate_recovery {
                    let handshake = fleet::client_handshake_database_as;
                    #[cfg(test)]
                    let handshake = if self.legacy_projection {
                        fleet::test_client_handshake_database_v2_as
                    } else {
                        handshake
                    };
                    handshake(
                        tcp,
                        &discriminator,
                        Some(&self.identity),
                        Duration::from_secs(30),
                    )
                } else {
                    fleet::client_handshake_as(
                        tcp,
                        &discriminator,
                        Some(&self.identity),
                        FleetOp::Database,
                        Duration::from_secs(30),
                    )
                }
                .map_err(|_| Error::new("Commons control TLS handshake failed"))?;
                locality
                    .recheck()
                    .inspect_err(|_| repair.phase = Phase::Route)?;
                let protocol = Protocol::from_alpn(channel.transport.stream().conn.alpn_protocol());
                let result = self.connected_until(
                    channel.transport.stream(),
                    channel.peer.as_ref(),
                    side,
                    repair,
                    until,
                    protocol,
                );
                locality
                    .finish()
                    .inspect_err(|_| repair.phase = Phase::Route)?;
                result
            }
            Side::Responder => {
                let mut channel = if negotiate_recovery {
                    let handshake = fleet::server_handshake_database_as;
                    #[cfg(test)]
                    let handshake = if self.legacy_projection {
                        fleet::test_server_handshake_database_v2_as
                    } else {
                        handshake
                    };
                    handshake(
                        tcp,
                        &discriminator,
                        Some(&self.identity),
                        Duration::from_secs(30),
                    )
                } else {
                    fleet::server_handshake_as(
                        tcp,
                        &discriminator,
                        Some(&self.identity),
                        FleetScope::Facts,
                        &[FleetOp::Database],
                        Duration::from_secs(30),
                    )
                }
                .map_err(|_| Error::new("Commons control TLS handshake failed"))?;
                locality
                    .recheck()
                    .inspect_err(|_| repair.phase = Phase::Route)?;
                let protocol = Protocol::from_alpn(channel.transport.stream().conn.alpn_protocol());
                let result = self.connected_until(
                    channel.transport.stream(),
                    channel.peer.as_ref(),
                    side,
                    repair,
                    until,
                    protocol,
                );
                locality
                    .finish()
                    .inspect_err(|_| repair.phase = Phase::Route)?;
                result
            }
        }
    }

    #[cfg(test)]
    fn connected(
        &self,
        stream: &mut (impl Read + Write),
        proof: Option<&NodeProof>,
        side: Side,
        repair: &mut Repair,
    ) -> Result<Report> {
        self.connected_until(
            stream,
            proof,
            side,
            repair,
            Instant::now() + EXCHANGE,
            Protocol::Legacy,
        )
    }

    fn connected_until(
        &self,
        stream: &mut (impl Read + Write),
        proof: Option<&NodeProof>,
        side: Side,
        repair: &mut Repair,
        until: Instant,
        protocol: Protocol,
    ) -> Result<Report> {
        let recovery_first = protocol != Protocol::Legacy;
        repair.begin_round();
        repair.phase = Phase::Membership;
        if self.pending {
            if side != Side::Initiator {
                return Err(Error::new("pending enrollment cannot accept connections"));
            }
            return enrollment::client(self, stream, proof, repair);
        }
        if proof.is_none() {
            return Err(Error::new("Commons requires a channel-bound node proof"));
        }
        if side == Side::Initiator {
            send_frame(stream, &self.hello)?;
        }
        let hello = repair.receive_answer(stream, side)?;
        if side == Side::Responder && hello.starts_with(b"ACE1") {
            return enrollment::serve(self, stream, proof, &hello, repair);
        }
        let (actor, peer_log) = authenticate(self.trust, self.actor, proof, &hello)?;
        verify_peer_against_local_log(
            self.paths.as_ref(),
            self.trust,
            actor,
            &hello,
            peer_log.as_deref(),
        )?;
        let authenticated_at = Instant::now();
        repair.prepare(self.trust, actor);
        repair.authenticated = Some(authenticated_at);
        repair.clear_application();
        // A sole channel may use either direction. On an actual collision both
        // ends prefer the lower actor's outgoing channel, so simultaneous
        // responders cannot each keep the channel whose initiator just lost.
        let preferred = (self.actor < actor) == (side == Side::Initiator);
        repair.phase = Phase::PeerAdmission;
        let mut lease = self.peers.acquire_on(actor, repair, preferred, until)?;
        #[cfg(test)]
        tests::peer_cache_stage(tests::PeerCacheStage::Acquired(side));
        lease.prepare(self.trust, actor);
        lease.authenticated = Some(authenticated_at);
        lease.clear_application();
        lease.phase = Phase::Membership;
        if side == Side::Responder {
            send_frame(stream, &self.hello)?;
        }
        #[cfg(test)]
        let both_acx2 =
            !self.legacy && self.hello.starts_with(b"ACX2") && hello.starts_with(b"ACX2");
        #[cfg(not(test))]
        let both_acx2 = self.hello.starts_with(b"ACX2") && hello.starts_with(b"ACX2");
        if !recovery_first && both_acx2 {
            lease.membership = Some(self.sync_acx2_membership(stream, side, &mut lease)?);
        }
        let report = self.repair_control(stream, side, &mut lease)?;
        if recovery_first {
            lease.phase = Phase::MembershipRecords;
            let common = match self.trade_recovery(stream, side, &mut lease, both_acx2, protocol) {
                Ok(common) => common,
                Err(error) => {
                    lease.membership_error = Some(error.to_string());
                    return Ok(report);
                }
            };
            let application = common & 2 != 0;
            if application {
                self.repair_application(stream, actor, side, &mut lease, protocol);
            }
            // A failed application exchange can leave buffered protocol bytes.
            // The recovery records are already retained; do not reuse that
            // dirty boundary for a version report or claim app convergence.
            if lease.application_error.is_none() {
                if application {
                    lease.phase = Phase::VersionReport;
                    lease.said = self.trade_reports(stream, actor, side);
                    if common & 4 != 0 {
                        lease.phase = Phase::Credentials;
                        if let (Some(paths), Some((owner, config))) =
                            (&self.paths, &self.credentials)
                        {
                            lease.handoff = Some(
                                crate::credentials::handoff::exchange(
                                    paths,
                                    config,
                                    owner,
                                    self.trust.network,
                                    self.actor,
                                    actor,
                                    side,
                                    stream,
                                )
                                .map_err(|_| "credential handoff failed".to_owned()),
                            );
                        }
                    }
                    lease.phase = Phase::Complete;
                } else {
                    lease.phase = Phase::ControlComplete;
                }
            }
            return Ok(report);
        }
        self.repair_application(stream, actor, side, &mut lease, protocol);
        #[cfg(test)]
        if self.legacy {
            if lease.application.is_some() {
                lease.phase = Phase::Complete;
            }
            return Ok(report);
        }
        let can_trade_membership = if lease.application.is_some() {
            lease.phase = Phase::VersionReport;
            lease.said = self.trade_reports(stream, actor, side);
            lease.phase = Phase::Complete;
            matches!(lease.said, Said::Version(_))
        } else {
            self.application.is_none()
        };
        if can_trade_membership && !both_acx2 {
            lease.membership = self.trade_membership_records(stream, side, &mut lease);
        }
        Ok(report)
    }

    /// Synchronize verified membership records (`ACX2`) across bounded passes before control/table
    /// repair, closing any local `Wrap` gaps and coalescing concurrent `Commons` epochs (`F18`).
    fn sync_acx2_membership(
        &self,
        stream: &mut (impl Read + Write),
        side: Side,
        repair: &mut Repair,
    ) -> Result<Report> {
        let Some(paths) = self.paths.as_ref() else {
            return Err(Error::new("ACX2 endpoint requires local secrets custody"));
        };
        sync_log_membership(
            stream,
            &MembershipDisk::new(paths, self.trust),
            side,
            &mut repair.membership_hint,
            &mut |allow_coalesce| {
                aterm_secrets::client::reconcile_commons_log(paths, allow_coalesce)
                    .is_ok_and(|written| !written.is_empty())
            },
            Instant::now(),
        )
    }

    /// Available only after TLS selected the new repair order. These bits
    /// describe configured services, not current application permission.
    fn trade_recovery(
        &self,
        stream: &mut (impl Read + Write),
        side: Side,
        repair: &mut Repair,
        both_acx2: bool,
        protocol: Protocol,
    ) -> Result<u8> {
        const RECORDS: u8 = 1;
        const APPLICATION: u8 = 2;
        let credentials = if protocol == Protocol::Credentials {
            4
        } else {
            0
        };
        let flags = (u8::from(self.paths.is_some()) * RECORDS)
            | (u8::from(self.application.is_some()) * APPLICATION)
            | (u8::from(self.credentials.is_some() && self.application.is_some()) * credentials);
        let own = [b'A', b'C', b'R', b'2', flags];
        let remote = match side {
            Side::Initiator => {
                send_frame(stream, &own)?;
                receive_frame(stream, own.len())?
            }
            Side::Responder => {
                let remote = receive_frame(stream, own.len())?;
                send_frame(stream, &own)?;
                remote
            }
        };
        if remote.len() != own.len()
            || remote[..4] != own[..4]
            || remote[4] & !(RECORDS | APPLICATION | credentials) != 0
        {
            return Err(Error::new("invalid Commons repair capabilities"));
        }
        let common = flags & remote[4];
        if common & RECORDS != 0 {
            repair.membership = Some(if both_acx2 {
                self.sync_acx2_membership(stream, side, repair)?
            } else {
                exchange_membership(
                    stream,
                    &MembershipDisk::new(
                        self.paths.as_ref().expect("advertised membership custody"),
                        self.trust,
                    ),
                    side,
                    &mut repair.membership_hint,
                    Instant::now(),
                )?
            });
        }
        Ok(common)
    }
    /// Trade signed versions once a round completed on this side (C26). Each side's next frame
    /// is then the other's report, or its end: a 0.145 peer sends none, reads none and has
    /// closed, which reads as [`Said::Nothing`], as does a report that does not verify.
    fn trade_reports(&self, stream: &mut (impl Read + Write), actor: Actor, side: Side) -> Said {
        if side == Side::Initiator {
            let _ = send_frame(stream, &self.report);
        }
        let said = receive_frame(stream, REPORT_LEN)
            .and_then(|wire| hearing::reported(&wire, self.trust.network, &actor).map_err(failure))
            .map_or(Said::Nothing, Said::Version);
        if side == Side::Responder {
            let _ = send_frame(stream, &self.report);
        }
        said
    }

    /// Exchange verified membership records (`F13`) once both sides advertise local secrets
    /// custody (`ALMR`). A pre-F13 peer closes after version or control repair, so an
    /// unreadable or absent tag ends the round cleanly without failing control repair.
    fn trade_membership_records(
        &self,
        stream: &mut (impl Read + Write),
        side: Side,
        repair: &mut Repair,
    ) -> Option<Report> {
        const READY: &[u8; 4] = b"ALMR";
        const NONE: &[u8; 4] = b"ALMN";
        let own_tag: &[u8; 4] = if self.paths.is_some() { READY } else { NONE };
        let peer_tag = match side {
            Side::Initiator => {
                let _ = send_frame(stream, own_tag);
                receive_frame(stream, 4).ok()?
            }
            Side::Responder => {
                let tag = receive_frame(stream, 4).ok()?;
                let _ = send_frame(stream, own_tag);
                tag
            }
        };
        if own_tag != READY || peer_tag.as_slice() != READY {
            return None;
        }
        let paths = self.paths.as_ref()?;
        exchange_membership(
            stream,
            &MembershipDisk::new(paths, self.trust),
            side,
            &mut repair.membership_hint,
            Instant::now(),
        )
        .ok()
    }

    fn repair_control(
        &self,
        stream: &mut (impl Read + Write),
        side: Side,
        repair: &mut Repair,
    ) -> Result<Report> {
        exchange(
            stream,
            &Disk {
                dir: &self.dir,
                trust: self.trust,
            },
            side,
            repair,
        )
    }

    fn repair_application(
        &self,
        stream: &mut (impl Read + Write),
        actor: Actor,
        side: Side,
        repair: &mut Repair,
        protocol: Protocol,
    ) {
        if let Some(application) = &self.application {
            repair.phase = Phase::Application;
            #[cfg(test)]
            if self.legacy_projection {
                let result =
                    application.exchange_legacy_projection(stream, actor, side, &mut repair.data);
                repair.application_error = result.as_ref().err().map(ToString::to_string);
                repair.application = result.ok();
                return;
            }
            let result = if protocol == Protocol::Credentials {
                application.exchange(stream, actor, side, &mut repair.data)
            } else {
                application.exchange_with_compatibility(stream, actor, side, &mut repair.data)
            };
            repair.application_error = result.as_ref().err().map(ToString::to_string);
            repair.application = result.ok();
        }
    }
}

/// The decoded membership-log witnesses this process holds: room for one per peer a full roster
/// has ([`WITNESS_ENTRIES`]), within the byte bound 16 maximal witnesses had before
/// ([`WITNESS_BYTES`]).
const WITNESS_ENTRIES: usize = aterm_secrets::commons::roster::MAX_MEMBERS + 1;
const WITNESS_BYTES: usize = 16 * (MAX_ACX2_HELLO - HELLO_FIXED);

/// One decoded and verified witness, and the requests it admitted.
struct WitnessEntry {
    trust: Trust,
    digest: [u8; 32],
    bytes: usize,
    log: Arc<aterm_secrets::membership_log::Log>,
    /// The device requests this log gave a historical seat (bounded by [`WITNESS_ENTRIES`]).
    admitted: Vec<Vec<u8>>,
}

/// Verified membership-log witnesses, most recently used last. A peer's hello carries its log
/// witness in every round, and a fleet's peers mostly carry the same log, so a round reuses the
/// decoded log (its every record's signature checked once) of any peer that presented the same
/// bytes, and the verdict for a request this log already admitted. Each round still hashes the
/// bytes it received; a log whose bytes differ is decoded afresh.
pub(super) struct WitnessLogs {
    entries: Mutex<std::collections::VecDeque<WitnessEntry>>,
    max_entries: usize,
    max_bytes: usize,
    /// Witnesses decoded (tests).
    #[cfg(test)]
    pub(super) decodes: std::sync::atomic::AtomicUsize,
}
impl WitnessLogs {
    pub(super) const fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: Mutex::new(std::collections::VecDeque::new()),
            max_entries,
            max_bytes,
            #[cfg(test)]
            decodes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The log a held entry decoded from `digest` under `trust`, moved to most recently used, and
    /// whether it already admitted `request`.
    fn held(
        &self,
        trust: Trust,
        digest: &[u8; 32],
        request: &[u8],
    ) -> Option<(Arc<aterm_secrets::membership_log::Log>, bool)> {
        let mut entries = self.entries.lock().ok()?;
        let at = entries
            .iter()
            .position(|e| e.trust == trust && e.digest == *digest)?;
        let entry = entries.remove(at)?;
        let held = (
            entry.log.clone(),
            entry.admitted.iter().any(|r| r == request),
        );
        entries.push_back(entry);
        Some(held)
    }

    /// Keep `log`, decoded from `digest` (`bytes` long) under `trust`, as having admitted
    /// `request`, evicting the least recently used entries beyond either bound.
    fn keep(
        &self,
        trust: Trust,
        digest: [u8; 32],
        bytes: usize,
        log: &Arc<aterm_secrets::membership_log::Log>,
        request: &[u8],
    ) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        let mut entry = match entries
            .iter()
            .position(|e| e.trust == trust && e.digest == digest)
        {
            Some(at) => entries.remove(at).expect("position is in range"),
            None => WitnessEntry {
                trust,
                digest,
                bytes,
                log: log.clone(),
                admitted: Vec::new(),
            },
        };
        if !entry.admitted.iter().any(|r| r == request) {
            if entry.admitted.len() >= WITNESS_ENTRIES {
                entry.admitted.remove(0);
            }
            entry.admitted.push(request.to_vec());
        }
        entries.push_back(entry);
        while entries.len() > self.max_entries
            || (entries.len() > 1
                && entries.iter().map(|e| e.bytes).sum::<usize>() > self.max_bytes)
        {
            entries.pop_front();
        }
    }

    /// [`check_log_witness`] against this cache.
    pub(super) fn check(
        &self,
        trust: Trust,
        request: &Request,
        wire: &[u8],
    ) -> Result<Arc<aterm_secrets::membership_log::Log>> {
        let binding = request.binding();
        if trust.genesis == [0; 32]
            || *binding.network() != trust.network
            || *binding.authority() != trust.authority
        {
            return Err(Error::new("Commons device belongs to another network"));
        }
        let wire_digest = Sha256::digest(wire);
        let request_bytes = request.bytes();
        let log = match self.held(trust, &wire_digest, request_bytes) {
            Some((log, true)) => return Ok(log),
            Some((log, false)) => log,
            None => {
                #[cfg(test)]
                self.decodes
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Arc::new(decode_log_witness(trust, wire)?)
            }
        };
        admit_by_log_witness(&log, binding)?;
        self.keep(trust, wire_digest, wire.len(), &log, request_bytes);
        Ok(log)
    }
}

/// Decode a peer's membership-log witness under `trust`, verifying each record into its log.
fn decode_log_witness(trust: Trust, wire: &[u8]) -> Result<aterm_secrets::membership_log::Log> {
    let records = aterm_secrets::membership_log::decode_records(wire).map_err(failure)?;
    let mut iter = records.into_iter();
    let genesis = iter
        .next()
        .ok_or_else(|| Error::new("Commons peer lacks historical log approval"))?;
    if let aterm_secrets::membership_log::Body::Genesis { root, .. } = genesis.body() {
        if root.network != trust.network || root.authority != trust.authority {
            return Err(Error::new("Commons peer belongs to another membership log"));
        }
    } else {
        return Err(Error::new("Commons log witness must start with Genesis"));
    }
    let mut log = aterm_secrets::membership_log::Log::new(genesis).map_err(failure)?;
    for record in iter {
        let _ = log.insert(record).map_err(failure)?;
    }
    Ok(log)
}

/// Verify a decoded peer witness and return its log. The log is shared with every other round
/// that presented the same bytes ([`WitnessLogs`]).
pub(super) fn check_log_witness(
    trust: Trust,
    request: &Request,
    wire: &[u8],
) -> Result<Arc<aterm_secrets::membership_log::Log>> {
    static WITNESS_LOGS: WitnessLogs = WitnessLogs::new(WITNESS_ENTRIES, WITNESS_BYTES);
    WITNESS_LOGS.check(trust, request, wire)
}

/// Whether `log` gives `binding` a historical seat it has not lost.
fn admit_by_log_witness(
    log: &aterm_secrets::membership_log::Log,
    binding: &aterm_secrets::commons::binding::Binding,
) -> Result<()> {
    if !log.has_historical_seat(binding)
        || log.membership().removal(&binding.actor()).is_some()
        || log.recipient_is_removed(&binding.recipient())
        || matches!(
            log.membership().node(binding.writer()),
            aterm_secrets::membership_log::NodeStanding::Removed(_)
        )
        || log.audience_left_out(
            &aterm_secrets::membership_log::EpochAudience::Commons,
            &binding.actor(),
        )
        || log.audience_left_out(
            &aterm_secrets::membership_log::EpochAudience::Everyone,
            &binding.actor(),
        )
        || log.audience_left_out_recipient(
            &aterm_secrets::membership_log::EpochAudience::Commons,
            &binding.recipient(),
        )
        || log.audience_left_out_recipient(
            &aterm_secrets::membership_log::EpochAudience::Everyone,
            &binding.recipient(),
        )
        || log.membership().members().iter().any(|(m_actor, m)| {
            (*m_actor == binding.actor() || m.binding.recipient() == binding.recipient())
                && m.node_key.is_some_and(|nk| {
                    nk != [0; 32]
                        && matches!(
                            log.membership().nodes().get(&nk),
                            Some(aterm_secrets::membership_log::NodeStanding::Removed(_))
                        )
                })
        })
    {
        return Err(Error::new("Commons peer lacks historical log approval"));
    }
    Ok(())
}

/// Check the authenticated peer of `hello` against this device's own membership log and
/// roster. `peer_log` is the log its `ACX2` witness decoded to when [`authenticate`] checked it
/// in this same round, so the witness is hashed and looked up once per round.
fn verify_peer_against_local_log(
    paths: Option<&aterm_secrets::client::Paths>,
    trust: Trust,
    actor: Actor,
    hello: &[u8],
    peer_log: Option<&aterm_secrets::membership_log::Log>,
) -> Result<()> {
    let peer_req = Request::decode(&hello[84..HELLO_FIXED]).map_err(failure)?;
    let Some(paths) = paths else {
        return Ok(());
    };
    let recipient = peer_req.binding().recipient();
    let roster = aterm_secrets::client::swarm_roster_now(paths)
        .ok()
        .flatten()
        .map(|mut r| {
            if let Ok(Some(denied)) = aterm_secrets::client::swarm_denied_nodes_now(paths) {
                r.deny_nodes.extend(denied);
                r.canonicalize();
            }
            r
        });
    if let Some(ref roster) = roster
        && crate::table::binding_denied_in_roster(roster, peer_req.binding())
    {
        return Err(Error::new(
            "Commons peer was removed from the membership log",
        ));
    }
    let Ok(Some((local_log, _))) = aterm_secrets::client::trusted_commons_log(paths, trust) else {
        return Ok(());
    };
    let denied_by = |log: &aterm_secrets::membership_log::Log| {
        use aterm_secrets::membership_log::{EpochAudience, NodeStanding};
        let log_roster = aterm_secrets::client::local_swarm_roster_for_log(paths, log)
            .or_else(|| roster.clone());
        let membership = log.membership();
        membership.removal(&actor).is_some()
            || log.recipient_is_removed(&recipient)
            || matches!(
                membership.node(peer_req.binding().writer()),
                NodeStanding::Removed(_)
            )
            || log.audience_left_out_recipient(&EpochAudience::Commons, &recipient)
            || log.audience_left_out_recipient(&EpochAudience::Everyone, &recipient)
            || log_roster
                .as_ref()
                .is_some_and(|r| crate::table::binding_denied_in_roster(r, peer_req.binding()))
            || membership.members().iter().any(|(m_actor, m)| {
                (*m_actor == actor || m.binding.recipient() == recipient)
                    && (log.audience_left_out(&EpochAudience::Commons, m_actor)
                        || log.audience_left_out(&EpochAudience::Everyone, m_actor)
                        || m.node_key.is_some_and(|nk| {
                            nk != [0; 32]
                                && matches!(
                                    membership.nodes().get(&nk),
                                    Some(NodeStanding::Removed(_))
                                )
                        })
                        || log_roster
                            .as_ref()
                            .is_some_and(|r| aterm_secrets::client::member_denied_by_roster(r, m)))
            })
    };
    if denied_by(&local_log) {
        return Err(Error::new(
            "Commons peer was removed from the membership log",
        ));
    }
    if let Some(peer_log) = peer_log {
        if local_log.network() != peer_log.network() {
            return Err(Error::new("Commons peer belongs to another membership log"));
        }
        let mut combined = local_log;
        for record in peer_log.records().skip(1) {
            let _ = combined.insert(record.clone()).map_err(failure)?;
        }
        if denied_by(&combined) || !combined.has_historical_seat(peer_req.binding()) {
            return Err(Error::new(
                "Commons peer was removed from the membership log",
            ));
        }
    }
    Ok(())
}

fn check_witness(trust: Trust, request: &Request, wire: &[u8]) -> Result<()> {
    let binding = request.binding();
    if trust.genesis == [0; 32]
        || *binding.network() != trust.network
        || *binding.authority() != trust.authority
    {
        return Err(Error::new("Commons device belongs to another network"));
    }
    let node = Node::verify(wire, trust.network, trust.authority).map_err(failure)?;
    if (node.kind() == Kind::Genesis && node.id() != trust.genesis)
        || !node
            .facts()
            .iter()
            .any(|fact| matches!(fact, Fact::Bind { binding: approved } if approved == binding))
    {
        return Err(Error::new(
            "Commons peer lacks historical authority approval",
        ));
    }
    Ok(())
}

/// The peer actor `hello` proves on this channel, and the membership log its `ACX2` witness decoded
/// to (none for `ACX1`).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "Approved",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "MissingProof",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "CopiedWitness",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "ForeignPins",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "ForeignGenesis",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "Unapproved",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "ReflectedWriter",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
fn authenticate(
    trust: Trust,
    own: Actor,
    proof: Option<&NodeProof>,
    hello: &[u8],
) -> Result<(Actor, Option<Arc<aterm_secrets::membership_log::Log>>)> {
    if hello.len() <= HELLO_FIXED
        || hello[4..20] != trust.network
        || hello[20..52] != trust.authority
        || hello[52..84] != trust.genesis
    {
        return Err(Error::new("Commons peer protocol or trust pins differ"));
    }
    match &hello[..4] {
        b"ACX1" => {
            if hello.len() > MAX_HELLO {
                return Err(Error::new("Commons peer protocol or trust pins differ"));
            }
            let request = Request::decode(&hello[84..HELLO_FIXED]).map_err(failure)?;
            check_witness(trust, &request, &hello[HELLO_FIXED..])?;
            Ok((authenticate_identity(own, proof, &request)?, None))
        }
        b"ACX2" => {
            if hello.len() > MAX_ACX2_HELLO {
                return Err(Error::new("Commons peer protocol or trust pins differ"));
            }
            let request = Request::decode(&hello[84..HELLO_FIXED]).map_err(failure)?;
            let log = check_log_witness(trust, &request, &hello[HELLO_FIXED..])?;
            Ok((authenticate_identity(own, proof, &request)?, Some(log)))
        }
        _ => Err(Error::new("Commons peer protocol or trust pins differ")),
    }
}

fn authenticate_identity(
    own: Actor,
    proof: Option<&NodeProof>,
    request: &Request,
) -> Result<Actor> {
    let peer = proof.ok_or_else(|| Error::new("Commons requires a channel-bound node proof"))?;
    let actor = request.binding().actor();
    if peer.key.bytes().as_slice() != &actor[..32]
        || peer.node != aterm_codec::hex::encode_lower(&actor[32..])
        || actor[..32] == own[..32]
    {
        return Err(Error::new(
            "Commons peer proof differs from its approved device",
        ));
    }
    Ok(actor)
}

// No Disk method performs IO on the network. Reads take no lock: they answer
// from the journal's acknowledged prefix beside any writer. Only `retain` of a
// valid record not yet held takes the writer lock, and only for its ingest, so
// even a slow remote cannot own the local journal's writer lock. A writer kept
// busy past the open's short wait leaves the new records unacknowledged.
trait Store {
    const MAX_WIRE: usize;
    fn inventory(&self) -> Result<BTreeSet<Digest>>;
    fn select_missing(
        &self,
        local: &BTreeSet<Digest>,
        remote: &BTreeSet<Digest>,
        cursor: &mut Option<Digest>,
    ) -> Result<Vec<Digest>> {
        let ids = select(local, remote, *cursor);
        if let Some(last) = ids.last() {
            *cursor = Some(*last);
        }
        Ok(ids)
    }
    fn batch(&self, ids: &[Digest]) -> Result<Vec<Vec<u8>>>;
    fn retain(&self, wires: &[Vec<u8>]) -> Result<Vec<bool>>;
}
struct Disk<'a> {
    dir: &'a Path,
    trust: Trust,
}
impl Store for Disk<'_> {
    const MAX_WIRE: usize = MAX_NODE;
    fn inventory(&self) -> Result<BTreeSet<Digest>> {
        let journal = Journal::read_existing(self.dir, self.trust)
            .map_err(|error| journal_failure("inventory", error))?;
        Ok(journal.inventory().copied().collect())
    }
    fn batch(&self, ids: &[Digest]) -> Result<Vec<Vec<u8>>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let journal = Journal::read_existing(self.dir, self.trust)
            .map_err(|error| journal_failure("batch", error))?;
        ids.iter()
            .map(|id| {
                journal
                    .node(id)
                    .map(|node| node.bytes().to_vec())
                    .ok_or_else(|| Error::new("Commons control record disappeared"))
            })
            .collect()
    }
    fn retain(&self, wires: &[Vec<u8>]) -> Result<Vec<bool>> {
        // Reject garbage before taking the writer lock or replaying a large
        // journal. Authenticated legacy evidence is still retained: omitting it
        // could hide restrictions that this version cannot safely interpret.
        let valid: Vec<bool> = wires
            .iter()
            .map(|wire| {
                Node::verify(wire, self.trust.network, self.trust.authority).is_ok_and(|node| {
                    node.kind() != Kind::Genesis || node.id() == self.trust.genesis
                })
            })
            .collect();
        if !valid.iter().any(|valid| *valid) {
            return Ok(valid);
        }
        // Peers mostly offer what is already held. Acknowledge those from the
        // lock-free view; take the writer only when a valid record is new.
        let known = Journal::read_existing(self.dir, self.trust)
            .map_err(|error| journal_failure("retain inventory", error))?;
        let held: Vec<bool> = wires
            .iter()
            .zip(&valid)
            .map(|(wire, valid)| *valid && known.receipt(&Sha256::digest(wire)).is_some())
            .collect();
        drop(known);
        let writer = if valid.iter().zip(&held).any(|(valid, held)| *valid && !held) {
            match Journal::open_existing_until(
                self.dir,
                self.trust,
                Instant::now() + Duration::from_secs(2),
            ) {
                Ok(journal) => Some(journal),
                // Another local writer kept the journal: acknowledge what is
                // held, and leave the new records for the peer to offer again.
                Err(error) if error.kind() == aterm_secrets::ErrorKind::JournalBusy => None,
                Err(error) => return Err(journal_failure("retain", error)),
            }
        } else {
            None
        };
        let Some(mut journal) = writer else {
            // The view may show a checkpoint whose rename is not yet durable: a
            // writer between its rename and its directory barrier, or one whose
            // barrier failed. Make the entry durable before calling these
            // records durable; a later checkpoint only extends this one.
            if held.iter().any(|held| *held) {
                aterm_durable::durable_dir(self.dir).map_err(failure)?;
            }
            return Ok(held);
        };
        Ok(wires
            .iter()
            .zip(valid)
            .map(|(wire, valid)| {
                // Journal verifies signatures and pins and commits its checkpoint
                // before returning a receipt. Uncertain appends yield no positive ack.
                valid
                    && journal
                        .ingest(wire)
                        .is_ok_and(|receipt| receipt.id == Sha256::digest(wire))
            })
            .collect())
    }
}

/// Synchronize verified membership records (`ACX2`) in bounded passes, with a `reconcile`
/// between them that may sign records (it closes `Wrap` gaps and, given `true`, coalesces
/// concurrent `Commons` epochs) and answers whether it wrote any. The second `reconcile` and the
/// third pass run only when the second pass moved records, which both sides see alike. `now`
/// places the peer's refusal hold ([`MembershipHint::offers`]).
fn sync_log_membership(
    stream: &mut (impl Read + Write),
    store: &MembershipDisk<'_>,
    side: Side,
    hint: &mut MembershipHint,
    reconcile: &mut dyn FnMut(bool) -> bool,
    now: Instant,
) -> Result<Report> {
    let mut total = Report::default();
    for _ in 0..64 {
        let (round, done) = exchange_membership_round(stream, store, side, hint, now)?;
        total.sent += round.sent;
        total.received += round.received;
        total.deferred = round.deferred;
        if done {
            break;
        }
    }
    if reconcile(side == Side::Initiator) {
        store.invalidate();
    }
    let mut second_moved = false;
    for _ in 0..16 {
        let (round, done) = exchange_membership_round(stream, store, side, hint, now)?;
        total.sent += round.sent;
        total.received += round.received;
        total.deferred = round.deferred;
        if round.sent + round.received + round.deferred > 0 {
            second_moved = true;
        }
        if done {
            break;
        }
    }
    if second_moved {
        if reconcile(side == Side::Initiator) {
            store.invalidate();
        }
        for _ in 0..8 {
            let (round, done) = exchange_membership_round(stream, store, side, hint, now)?;
            total.sent += round.sent;
            total.received += round.received;
            total.deferred = round.deferred;
            if done {
                break;
            }
        }
    }
    Ok(total)
}

struct MembershipDisk<'a> {
    paths: &'a aterm_secrets::client::Paths,
    trust: Trust,
    cached: std::sync::Mutex<Option<aterm_secrets::membership_log::Store>>,
    /// How many times it read the records file.
    #[cfg(test)]
    loads: std::sync::atomic::AtomicUsize,
    /// How many records it put in batches for the peer.
    #[cfg(test)]
    batched: std::sync::atomic::AtomicUsize,
    /// How many more batches it takes none of, as when another writer holds the state lock past
    /// `retain_records_for_commons`'s wait.
    #[cfg(test)]
    busy: std::sync::atomic::AtomicUsize,
}

impl<'a> MembershipDisk<'a> {
    const fn new(paths: &'a aterm_secrets::client::Paths, trust: Trust) -> Self {
        Self {
            paths,
            trust,
            cached: std::sync::Mutex::new(None),
            #[cfg(test)]
            loads: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            batched: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            busy: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn invalidate(&self) {
        if let Ok(mut guard) = self.cached.lock() {
            *guard = None;
        }
    }

    fn with_store<T>(
        &self,
        op: &'static str,
        f: impl FnOnce(&aterm_secrets::membership_log::Store) -> Result<T>,
    ) -> Result<T> {
        let mut guard = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_none() {
            #[cfg(test)]
            self.loads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let store = aterm_secrets::client::load_records(self.paths)
                .map_err(|error| journal_failure(op, error))?;
            *guard = Some(store);
        }
        f(guard.as_ref().expect("populated above"))
    }
}

impl Store for MembershipDisk<'_> {
    const MAX_WIRE: usize = aterm_secrets::membership_log::MAX_RECORD;

    fn inventory(&self) -> Result<BTreeSet<Digest>> {
        self.with_store("membership inventory", |store| {
            let Some(log) = store.log() else {
                return Ok(BTreeSet::new());
            };
            if let aterm_secrets::membership_log::Body::Genesis { root, .. } = log.genesis().body()
                && *root
                    != (aterm_secrets::membership_log::Root {
                        network: self.trust.network,
                        authority: self.trust.authority,
                    })
            {
                return Ok(BTreeSet::new());
            }
            Ok(store.complete_inventory())
        })
    }

    fn select_missing(
        &self,
        local: &BTreeSet<Digest>,
        remote: &BTreeSet<Digest>,
        cursor: &mut Option<Digest>,
    ) -> Result<Vec<Digest>> {
        if local.is_subset(remote) {
            return Ok(Vec::new());
        }
        self.with_store("membership select", |store| {
            let Some(log) = store.log() else {
                return Ok(Vec::new());
            };
            if let aterm_secrets::membership_log::Body::Genesis { root, .. } = log.genesis().body()
                && *root
                    != (aterm_secrets::membership_log::Root {
                        network: self.trust.network,
                        authority: self.trust.authority,
                    })
            {
                return Ok(Vec::new());
            }
            let ids = store.complete_missing(remote, cursor.as_ref(), RECORDS_PER_ROUND);
            if let Some(last) = ids.last() {
                *cursor = Some(*last);
            }
            Ok(ids)
        })
    }

    fn batch(&self, ids: &[Digest]) -> Result<Vec<Vec<u8>>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        #[cfg(test)]
        self.batched
            .fetch_add(ids.len(), std::sync::atomic::Ordering::Relaxed);
        self.with_store("membership batch", |store| {
            ids.iter()
                .map(|id| {
                    store
                        .complete_bytes(id)
                        .map(<[u8]>::to_vec)
                        .ok_or_else(|| Error::new("membership record disappeared"))
                })
                .collect()
        })
    }

    fn retain(&self, wires: &[Vec<u8>]) -> Result<Vec<bool>> {
        if wires.is_empty() {
            return Ok(Vec::new());
        }
        #[cfg(test)]
        if self.busy.load(std::sync::atomic::Ordering::Relaxed) > 0 {
            self.busy.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(vec![false; wires.len()]);
        }
        let verdicts = aterm_secrets::client::retain_records_for_commons(
            self.paths,
            self.trust.network,
            self.trust.authority,
            wires,
        )
        .map_err(|error| journal_failure("membership retain", error))?;
        if verdicts.iter().any(|&v| v) {
            self.invalidate();
        }
        Ok(verdicts)
    }
}

fn exchange_membership(
    stream: &mut (impl Read + Write),
    store: &impl Store,
    side: Side,
    hint: &mut MembershipHint,
    now: Instant,
) -> Result<Report> {
    exchange_membership_round(stream, store, side, hint, now).map(|(report, _)| report)
}

fn exchange_membership_round(
    stream: &mut (impl Read + Write),
    store: &impl Store,
    side: Side,
    hint: &mut MembershipHint,
    now: Instant,
) -> Result<(Report, bool)> {
    let local = store.inventory()?;
    let remote = match side {
        Side::Initiator => {
            send_inventory(stream, &local)?;
            receive_inventory(stream)?
        }
        Side::Responder => {
            let remote = receive_inventory(stream)?;
            send_inventory(stream, &local)?;
            remote
        }
    };
    let unsent_local = local.difference(&remote).count();
    let unsent_remote = remote.difference(&local).count();
    let key = hold_key(&local, &remote);
    let ids = if hint.offers(&key, now) {
        store.select_missing(&local, &remote, &mut hint.cursor)?
    } else {
        Vec::new()
    };
    let batch = store.batch(&ids)?;
    let (sent, received, deferred) = match side {
        Side::Initiator => {
            let (sent, out) = send_batch(stream, &batch)?;
            let (received, incoming) = receive_batch(stream, store)?;
            (sent, received, out + incoming)
        }
        Side::Responder => {
            let (received, incoming) = receive_batch(stream, store)?;
            let (sent, out) = send_batch(stream, &batch)?;
            (sent, received, out + incoming)
        }
    };
    hint.settle(
        key,
        &ids,
        sent,
        remote.len() >= aterm_secrets::membership_log::MAX_RECORDS,
        now,
    );
    let done =
        (unsent_local <= RECORDS_PER_ROUND && unsent_remote <= RECORDS_PER_ROUND && deferred == 0)
            || (sent + received == 0);
    Ok((
        Report {
            sent,
            received,
            deferred,
        },
        done,
    ))
}

fn exchange(
    stream: &mut (impl Read + Write),
    store: &impl Store,
    side: Side,
    repair: &mut Repair,
) -> Result<Report> {
    repair.phase = Phase::ControlInventory;
    let local = store.inventory()?;
    let remote = match side {
        Side::Initiator => {
            repair.phase = Phase::InventorySend;
            send_inventory(stream, &local)?;
            repair.phase = Phase::InventoryReceive;
            receive_inventory(stream)?
        }
        Side::Responder => {
            repair.phase = Phase::InventoryReceive;
            let remote = receive_inventory(stream)?;
            repair.phase = Phase::InventorySend;
            send_inventory(stream, &local)?;
            remote
        }
    };
    let ids = repair.next(&local, &remote);
    repair.phase = Phase::ControlBatch;
    let batch = store.batch(&ids)?;
    let (sent, received, deferred) = match side {
        Side::Initiator => {
            repair.phase = Phase::BatchSend;
            let (sent, out) = send_batch(stream, &batch)?;
            repair.phase = Phase::BatchReceive;
            let (received, incoming) = receive_batch(stream, store)?;
            (sent, received, out + incoming)
        }
        Side::Responder => {
            repair.phase = Phase::BatchReceive;
            let (received, incoming) = receive_batch(stream, store)?;
            repair.phase = Phase::BatchSend;
            let (sent, out) = send_batch(stream, &batch)?;
            (sent, received, out + incoming)
        }
    };
    repair.phase = Phase::ControlComplete;
    Ok(Report {
        sent,
        received,
        deferred,
    })
}

fn select(
    local: &BTreeSet<Digest>,
    remote: &BTreeSet<Digest>,
    cursor: Option<Digest>,
) -> Vec<Digest> {
    local
        .iter()
        .filter(|id| cursor.is_none_or(|cursor| **id > cursor))
        .chain(
            local
                .iter()
                .filter(|id| cursor.is_some_and(|cursor| **id <= cursor)),
        )
        .filter(|id| !remote.contains(*id))
        .take(RECORDS_PER_ROUND)
        .copied()
        .collect()
}
fn send_inventory(stream: &mut impl Write, ids: &BTreeSet<Digest>) -> Result<()> {
    if ids.len() > MAX_NODES {
        return Err(Error::new("Commons inventory exceeds its bound"));
    }
    stream.write_all(&(ids.len() as u32).to_le_bytes())?;
    for id in ids {
        stream.write_all(id)?;
    }
    stream.flush()?;
    Ok(())
}
fn receive_inventory(stream: &mut impl Read) -> Result<BTreeSet<Digest>> {
    let n = read_u32(stream)? as usize;
    if n > MAX_NODES {
        return Err(Error::new("Commons inventory exceeds its bound"));
    }
    let mut ids = BTreeSet::new();
    for _ in 0..n {
        let mut id = [0; 32];
        stream.read_exact(&mut id)?;
        if ids.last().is_some_and(|previous| previous >= &id) {
            return Err(Error::new("Commons inventory is not sorted and unique"));
        }
        ids.insert(id);
    }
    Ok(ids)
}
fn send_batch(stream: &mut (impl Read + Write), batch: &[Vec<u8>]) -> Result<(usize, usize)> {
    stream.write_all(&(batch.len() as u32).to_le_bytes())?;
    for wire in batch {
        send_frame(stream, wire)?;
    }
    stream.flush()?;
    let mut retained = 0;
    for wire in batch {
        let mut ack = [0; 33];
        stream.read_exact(&mut ack)?;
        if ack[..32] != Sha256::digest(wire) || ack[32] > 1 {
            return Err(Error::new("Commons peer returned an invalid receipt"));
        }
        retained += usize::from(ack[32]);
    }
    Ok((retained, batch.len() - retained))
}
fn receive_batch<S: Store>(stream: &mut (impl Read + Write), store: &S) -> Result<(usize, usize)> {
    let n = read_u32(stream)? as usize;
    if n == 0 {
        return Ok((0, 0));
    }
    if n > RECORDS_PER_ROUND {
        return Err(Error::new("Commons batch exceeds its bound"));
    }
    let mut wires = Vec::with_capacity(n);
    let mut ids = BTreeSet::new();
    for _ in 0..n {
        let wire = receive_frame(stream, S::MAX_WIRE)?;
        if !ids.insert(Sha256::digest(&wire)) {
            return Err(Error::new("duplicate Commons batch record"));
        }
        wires.push(wire);
    }
    // Acquire the local journal only after the entire bounded batch has arrived.
    let verdicts = store.retain(&wires).unwrap_or_else(|_| vec![false; n]);
    if verdicts.len() != n {
        return Err(Error::new("invalid local Commons receipt count"));
    }
    let mut retained = 0;
    for (wire, durable) in wires.iter().zip(verdicts) {
        stream.write_all(&Sha256::digest(wire))?;
        stream.write_all(&[u8::from(durable)])?;
        retained += usize::from(durable);
    }
    stream.flush()?;
    Ok((retained, n - retained))
}
pub(crate) fn send_frame(stream: &mut impl Write, bytes: &[u8]) -> Result<()> {
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()?;
    Ok(())
}
pub(crate) fn receive_frame(stream: &mut impl Read, max: usize) -> Result<Vec<u8>> {
    let n = read_u32(stream)?;
    frame_body(stream, n, max)
}
/// The rest of a frame whose length `n` was read.
fn frame_body(stream: &mut impl Read, n: u32, max: usize) -> Result<Vec<u8>> {
    let n = n as usize;
    if n > max {
        return Err(Error::new("Commons frame exceeds its bound"));
    }
    let mut bytes = vec![0; n];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}
pub(crate) fn read_u32(stream: &mut impl Read) -> Result<u32> {
    let mut bytes = [0; 4];
    stream.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests;
