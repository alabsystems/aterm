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

/// Volatile scheduling hint, never a claim of delivery. Keep one per peer.
/// Advancing past a deferred node prevents it from starving later restrictions;
/// wraparound retries it even if the caller does not maintain a timer.
#[derive(Clone, Default, Debug)]
pub struct Repair {
    phase: Phase,
    peer: Option<(Trust, Actor)>,
    authenticated: Option<Instant>,
    cursor: Option<Digest>,
    membership_cursor: Option<Digest>,
    data: crate::replica::Repair,
    application: Option<crate::replica::Report>,
    membership: Option<Report>,
    membership_error: Option<String>,
    said: Said,
    application_error: Option<String>,
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
    }
    /// The most recently channel-authenticated peer; the round may have failed.
    pub fn actor(&self) -> Option<Actor> {
        self.peer.map(|(_, actor)| actor)
    }
    pub(crate) fn begin_round(&mut self) {
        self.phase = Phase::Connecting;
        self.authenticated = None;
        self.clear_application();
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
    /// The last application upgrade's failure in this round. A missing error
    /// does not establish that an application round completed; check
    /// [`Self::application`] separately. Starting another round clears it.
    pub fn application_error(&self) -> Option<&str> {
        self.application_error.as_deref()
    }
    fn prepare(&mut self, trust: Trust, actor: Actor) {
        self.authenticated = Some(Instant::now());
        if self.peer != Some((trust, actor)) {
            self.cursor = None;
            self.membership_cursor = None;
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
    peers: peers::Cache,
    paths: Option<aterm_secrets::client::Paths>,
    pending: bool,
    /// Behave as 0.145 does: trade no version after a round.
    #[cfg(test)]
    pub(crate) legacy: bool,
    /// Test an F13 peer whose TLS still selects the application-first order.
    #[cfg(test)]
    pub(crate) legacy_repair: bool,
}

/// One exchange's absolute deadline: the product's [`crate::network::WITHIN`]. A test needs each
/// round to complete, and waits out a loaded machine's round as a one-minute hang detector: a
/// round the deadline cuts reads as a missing application report.
#[cfg(not(test))]
const EXCHANGE: Duration = crate::network::WITHIN;
#[cfg(test)]
const EXCHANGE: Duration = Duration::from_secs(60);

impl Endpoint {
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
                peers: peers::Cache::default(),
                paths: Some(paths),
                pending: true,
                #[cfg(test)]
                legacy: false,
                #[cfg(test)]
                legacy_repair: false,
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
            peers: peers::Cache::default(),
            paths: None,
            pending: false,
            #[cfg(test)]
            legacy: false,
            #[cfg(test)]
            legacy_repair: false,
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
            peers: peers::Cache::default(),
            paths: None,
            pending: false,
            #[cfg(test)]
            legacy: false,
            #[cfg(test)]
            legacy_repair: false,
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
                    fleet::client_handshake_database_as(
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
                let recovery_first = channel.transport.stream().conn.alpn_protocol()
                    == Some(fleet::ALPN_DATABASE_REPAIR);
                let result = self.connected_until(
                    channel.transport.stream(),
                    channel.peer.as_ref(),
                    side,
                    repair,
                    until,
                    recovery_first,
                );
                locality
                    .finish()
                    .inspect_err(|_| repair.phase = Phase::Route)?;
                result
            }
            Side::Responder => {
                let mut channel = if negotiate_recovery {
                    fleet::server_handshake_database_as(
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
                let recovery_first = channel.transport.stream().conn.alpn_protocol()
                    == Some(fleet::ALPN_DATABASE_REPAIR);
                let result = self.connected_until(
                    channel.transport.stream(),
                    channel.peer.as_ref(),
                    side,
                    repair,
                    until,
                    recovery_first,
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
            false,
        )
    }

    fn connected_until(
        &self,
        stream: &mut (impl Read + Write),
        proof: Option<&NodeProof>,
        side: Side,
        repair: &mut Repair,
        until: Instant,
        recovery_first: bool,
    ) -> Result<Report> {
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
        let hello = receive_frame(stream, MAX_ACX2_HELLO)?;
        if side == Side::Responder && hello.starts_with(b"ACE1") {
            return enrollment::serve(self, stream, proof, &hello, repair);
        }
        let actor = authenticate(self.trust, self.actor, proof, &hello)?;
        verify_peer_against_local_log(self.paths.as_ref(), self.trust, actor, &hello)?;
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
            let application = match self.trade_recovery(stream, side, &mut lease, both_acx2) {
                Ok(application) => application,
                Err(error) => {
                    lease.membership_error = Some(error.to_string());
                    return Ok(report);
                }
            };
            if application {
                self.repair_application(stream, actor, side, &mut lease);
            }
            // A failed application exchange can leave buffered protocol bytes.
            // The recovery records are already retained; do not reuse that
            // dirty boundary for a version report or claim app convergence.
            if lease.application_error.is_none() {
                if application {
                    lease.phase = Phase::VersionReport;
                    lease.said = self.trade_reports(stream, actor, side);
                    lease.phase = Phase::Complete;
                } else {
                    lease.phase = Phase::ControlComplete;
                }
            }
            return Ok(report);
        }
        self.repair_application(stream, actor, side, &mut lease);
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
            &mut repair.membership_cursor,
            &mut |allow_coalesce| {
                aterm_secrets::client::reconcile_commons_log(paths, allow_coalesce)
                    .is_ok_and(|written| !written.is_empty())
            },
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
    ) -> Result<bool> {
        const RECORDS: u8 = 1;
        const APPLICATION: u8 = 2;
        let flags = (u8::from(self.paths.is_some()) * RECORDS)
            | (u8::from(self.application.is_some()) * APPLICATION);
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
            || remote[4] & !(RECORDS | APPLICATION) != 0
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
                    &mut repair.membership_cursor,
                )?
            });
        }
        Ok(common & APPLICATION != 0)
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
            &mut repair.membership_cursor,
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
    ) {
        if let Some(application) = &self.application {
            repair.phase = Phase::Application;
            let result = application.exchange(stream, actor, side, &mut repair.data);
            repair.application_error = result.as_ref().err().map(ToString::to_string);
            repair.application = result.ok();
        }
    }
}

pub(super) fn check_log_witness(
    trust: Trust,
    request: &Request,
    wire: &[u8],
) -> Result<aterm_secrets::membership_log::Log> {
    type WitnessLogCacheEntry = (Trust, Vec<u8>, [u8; 32], aterm_secrets::membership_log::Log);
    static WITNESS_LOG_CACHE: std::sync::Mutex<std::collections::VecDeque<WitnessLogCacheEntry>> =
        std::sync::Mutex::new(std::collections::VecDeque::new());

    let binding = request.binding();
    if trust.genesis == [0; 32]
        || *binding.network() != trust.network
        || *binding.authority() != trust.authority
    {
        return Err(Error::new("Commons device belongs to another network"));
    }
    let wire_digest = Sha256::digest(wire);
    let request_bytes = request.bytes();
    if let Ok(cache) = WITNESS_LOG_CACHE.lock()
        && let Some((_, _, _, log)) = cache
            .iter()
            .find(|(t, b, d, _)| *t == trust && *b == request_bytes && *d == wire_digest)
    {
        return Ok(log.clone());
    }
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
    if !log.has_historical_seat(binding)
        || log.membership().removal(&binding.actor()).is_some()
        || log.recipient_is_removed(&binding.recipient())
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
    if let Ok(mut cache) = WITNESS_LOG_CACHE.lock() {
        cache.retain(|(t, b, d, _)| !(*t == trust && *b == request_bytes && *d == wire_digest));
        if cache.len() >= 16 {
            cache.pop_front();
        }
        cache.push_back((trust, request_bytes.to_vec(), wire_digest, log.clone()));
    }
    Ok(log)
}

fn verify_peer_against_local_log(
    paths: Option<&aterm_secrets::client::Paths>,
    trust: Trust,
    actor: Actor,
    hello: &[u8],
) -> Result<()> {
    let peer_req = Request::decode(&hello[84..HELLO_FIXED]).map_err(failure)?;
    let peer_log = if hello.starts_with(b"ACX2") {
        Some(check_log_witness(trust, &peer_req, &hello[HELLO_FIXED..])?)
    } else {
        None
    };
    let Some(paths) = paths else {
        return Ok(());
    };
    let recipient = peer_req.binding().recipient();
    let recipient_bech32 = recipient.to_bech32();
    let roster = aterm_secrets::client::swarm_roster(paths)
        .ok()
        .flatten()
        .map(|mut r| {
            if let Ok(Some(denied)) = aterm_secrets::client::swarm_denied_nodes(paths) {
                r.deny_nodes.extend(denied);
                r.canonicalize();
            }
            r
        });
    if let Some(ref roster) = roster {
        let denied_in_roster = roster
            .members
            .iter()
            .filter(|m| m.recipient.trim() == recipient_bech32)
            .any(|m| {
                roster.is_label_denied(&m.label)
                    || m.aliases.iter().any(|a| roster.is_label_denied(a))
                    || m.node_id
                        .as_deref()
                        .is_some_and(|n| roster.is_node_denied(n))
                    || m.node_key
                        .as_deref()
                        .is_some_and(|k| roster.is_node_denied(k))
            });
        if denied_in_roster {
            return Err(Error::new(
                "Commons peer was removed from the membership log",
            ));
        }
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
            || log.audience_left_out_recipient(&EpochAudience::Commons, &recipient)
            || log.audience_left_out_recipient(&EpochAudience::Everyone, &recipient)
            || log_roster.as_ref().is_some_and(|r| {
                r.members
                    .iter()
                    .filter(|m| m.recipient.trim() == recipient_bech32)
                    .any(|m| {
                        r.is_label_denied(&m.label)
                            || m.aliases.iter().any(|a| r.is_label_denied(a))
                            || m.node_id.as_deref().is_some_and(|n| r.is_node_denied(n))
                            || m.node_key.as_deref().is_some_and(|k| r.is_node_denied(k))
                    })
            })
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
                        || log_roster.as_ref().is_some_and(|r| {
                            r.is_label_denied(&m.label)
                                || r.is_label_denied(&m.name)
                                || m.node_key.is_some_and(|nk| {
                                    nk != [0; 32]
                                        && r.is_node_denied(&aterm_codec::hex::encode_lower(&nk))
                                })
                        }))
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
) -> Result<Actor> {
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
            authenticate_identity(own, proof, &request)
        }
        b"ACX2" => {
            if hello.len() > MAX_ACX2_HELLO {
                return Err(Error::new("Commons peer protocol or trust pins differ"));
            }
            let request = Request::decode(&hello[84..HELLO_FIXED]).map_err(failure)?;
            let _ = check_log_witness(trust, &request, &hello[HELLO_FIXED..])?;
            authenticate_identity(own, proof, &request)
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
/// third pass run only when the second pass moved records, which both sides see alike.
fn sync_log_membership(
    stream: &mut (impl Read + Write),
    store: &MembershipDisk<'_>,
    side: Side,
    cursor: &mut Option<Digest>,
    reconcile: &mut dyn FnMut(bool) -> bool,
) -> Result<Report> {
    let mut total = Report::default();
    for _ in 0..64 {
        let (round, done) = exchange_membership_round(stream, store, side, cursor)?;
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
        let (round, done) = exchange_membership_round(stream, store, side, cursor)?;
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
            let (round, done) = exchange_membership_round(stream, store, side, cursor)?;
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
}

impl<'a> MembershipDisk<'a> {
    const fn new(paths: &'a aterm_secrets::client::Paths, trust: Trust) -> Self {
        Self {
            paths,
            trust,
            cached: std::sync::Mutex::new(None),
            #[cfg(test)]
            loads: std::sync::atomic::AtomicUsize::new(0),
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
    cursor: &mut Option<Digest>,
) -> Result<Report> {
    exchange_membership_round(stream, store, side, cursor).map(|(report, _)| report)
}

fn exchange_membership_round(
    stream: &mut (impl Read + Write),
    store: &impl Store,
    side: Side,
    cursor: &mut Option<Digest>,
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
    let ids = store.select_missing(&local, &remote, cursor)?;
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
    let n = read_u32(stream)? as usize;
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
