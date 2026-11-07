// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only local observations, never membership or global liveness claims.

/// Stable authenticated machine identity and its latest observed installation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MachineIdentity {
    /// UUID from the verified Commons binding, never an address or display name.
    pub machine_id: [u8; 16],
    /// Writer incarnation from that binding; not a process count.
    pub incarnation: [u8; 16],
    /// Optional authenticated display metadata.
    pub label: Option<String>,
}

/// Historical peer-reported agreement on retained account-proposal operation IDs.
/// This says nothing about current state, projections, eligibility or secrets.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AccountsExchangeObservation {
    /// Exact account-proposals table, audience generation and Commons trust digest.
    pub scope_digest: [u8; 32],
    /// Local authenticated actor captured by the exchange.
    pub local_actor: [u8; 48],
    /// Original admitted control head and basis, never renewed by a later read.
    pub control_observation: [u8; 64],
    /// Number of retained operation IDs reported matching in that exchange.
    pub inventory_count: u64,
    /// SHA-256 of those exact canonically ordered retained IDs.
    pub inventory_fingerprint: [u8; 32],
    /// Peer resolved through authenticated Commons membership, not its address.
    pub peer: MachineIdentity,
    /// Original sample time in Unix milliseconds; replay never makes it current.
    pub sampled_at_ms: u64,
}

/// Whether this local observer can currently check the network.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Visibility {
    /// No current observation yet, including wake/restart.
    #[default]
    Checking,
    /// Local discovery transport is available; this alone proves no peer.
    Active,
    /// No eligible local network attachment is available.
    Offline,
    /// The person disabled discovery.
    Disabled,
    /// Another local process owns transport, or this observer waits before it may use the local
    /// network (a macOS question of aterm's still to settle); the owner's liveness has not been
    /// read.
    Waiting,
    /// Required control or local observation access is unavailable.
    Unavailable,
}

/// The observation journal's independent read state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryState {
    /// Retained history has not yet been read.
    #[default]
    Restoring,
    /// Retained history was read successfully, including an empty journal.
    Ready,
    /// Retained history could not be read or safely written.
    Unavailable,
}

/// Evidence about a previously authenticated machine, from this observer only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reachability {
    /// A recent authenticated exchange completed during this observer lifetime.
    Reachable,
    /// Seen before; current reachability is unconfirmed.
    LastSeen,
    /// The observer cannot currently check reachability.
    Unknown,
}

/// One bounded recent observation; unknown intervals are not fabricated outages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenceSample {
    /// Local observation time, milliseconds since Unix epoch.
    pub at_ms: u64,
    /// What this observer established at that time.
    pub state: Reachability,
}

/// An outbound service address that completed an authenticated exchange.
/// This is historical local evidence, not current reachability or physical identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedEndpoint {
    /// Address dialled by the successful exchange with this exact peer incarnation.
    pub address: std::net::SocketAddr,
    /// Local completion time, milliseconds since Unix epoch.
    pub observed_at_ms: u64,
}

/// Local boundary reported by an automatic repair attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepairStage {
    /// Routing or authenticated transport setup.
    Transport,
    /// Peer admission or signed membership records.
    Membership,
    /// Shared control history and its acknowledgements.
    Control,
    /// Application catalog, inventory or record exchange.
    Application,
    /// Optional credential handoff after application repair.
    Credentials,
}

/// Historical outcome of one local attempt, never a network convergence claim.
///
/// A failure carries its local cause:
///
/// ```
/// use aterm_types::network_state::{RepairCause, RepairOutcome};
/// assert_eq!(RepairOutcome::Failed(RepairCause::Io).cause(), Some(RepairCause::Io));
/// ```
///
/// and there is no failed outcome without one:
///
/// ```compile_fail,E0308
/// use aterm_types::network_state::RepairOutcome;
/// let outcome: RepairOutcome = RepairOutcome::Failed;
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepairOutcome {
    /// This local attempt failed, for the local cause it names; the remote cause is not
    /// established. A failure always has a cause: there is no failed attempt without one.
    Failed(RepairCause),
    /// The owner stopped this attempt before completion.
    Cancelled,
    /// Some inventory or offered records could not be processed in this round: an
    /// application-stage local refusal names its cause; `None` where no category was recorded (a
    /// peer's unexplained deferral, or control or membership records left unacknowledged, which
    /// can include this Mac's own refusals).
    Deferred(Option<RepairCause>),
    /// No application exchange completed, including with older peers.
    NotObserved,
    /// The application exchange selected no common table.
    NoCommonTable,
    /// One application exchange completed; other tables or records may remain.
    Exchanged,
}

impl RepairOutcome {
    /// The local refusal this outcome names: a failure's, or a local deferral's.
    #[must_use]
    pub const fn cause(self) -> Option<RepairCause> {
        match self {
            Self::Failed(cause) => Some(cause),
            Self::Deferred(cause) => cause,
            Self::Cancelled | Self::NotObserved | Self::NoCommonTable | Self::Exchanged => None,
        }
    }
}

/// Sanitized metadata from the most recent attempt with an authenticated peer.
/// Reads retain its original timestamp and never make it current or authorize access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepairObservation {
    /// Original worker completion time, milliseconds since Unix epoch.
    pub completed_at_ms: u64,
    /// True when this owner dialled the peer; false for an incoming connection.
    pub outbound: bool,
    /// The local failure/defer boundary, or the last entered boundary if unknown.
    pub stage: Option<RepairStage>,
    /// What this one attempt established, independently of current reachability, with its
    /// value-free local refusal category ([`RepairOutcome::cause`]). No diagnostic text is
    /// published.
    pub outcome: RepairOutcome,
    /// Selected application table, when reported by this attempt.
    pub table: Option<[u8; 32]>,
    /// Durable positive receipts observed from the peer, including retries.
    pub sent: Option<u64>,
    /// Accepted local positive ingests, including retries. A failed attempt's count is a lower bound.
    pub received: Option<u64>,
    /// Offered positive records without a durable acknowledgement in this attempt.
    pub deferred: Option<u64>,
}

impl RepairObservation {
    /// This attempt's value-free local refusal category ([`RepairOutcome::cause`]).
    #[must_use]
    pub const fn cause(&self) -> Option<RepairCause> {
        self.outcome.cause()
    }
}

/// Closed vocabulary for repair refusals; never contains wire or filesystem data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepairCause {
    /// The local membership log is past an older protocol's fence.
    LegacyPastCrossing,
    /// The selected application table was not provisioned here.
    TableUndeclared,
    /// A bounded record, byte or time budget is exhausted.
    Quota,
    /// A filesystem or stream operation failed.
    Io,
    /// This Mac's LAN use waits for its LAN broker: none answers (setup's Mac password installs
    /// it) and no direct path macOS allowed serves this aterm, or the broker speaks another
    /// protocol or refuses this aterm.
    LanHeld,
    /// A dial reached no peer: refused, timed out, or no route to the address.
    Unreachable,
    /// A frame or stored record could not be decoded.
    Decode,
    /// Membership or local custody is not ready for table access.
    NotReady,
    /// The peer's protocol cannot complete this exchange.
    NeedsUpdate,
    /// A refusal without a more specific typed category.
    Other,
}

impl RepairCause {
    /// Stable diagnostic word shared by persisted and command observations.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::LegacyPastCrossing => "legacy-past-crossing",
            Self::TableUndeclared => "table-undeclared",
            Self::Quota => "quota",
            Self::Io => "io",
            Self::LanHeld => "lan-held",
            Self::Unreachable => "unreachable",
            Self::Decode => "decode",
            Self::NotReady => "not-ready",
            Self::NeedsUpdate => "needs-update",
            Self::Other => "other",
        }
    }

    /// The cause whose [`Self::word`] is `word`, if any.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Some(match word {
            "legacy-past-crossing" => Self::LegacyPastCrossing,
            "table-undeclared" => Self::TableUndeclared,
            "quota" => Self::Quota,
            "io" => Self::Io,
            "lan-held" => Self::LanHeld,
            "unreachable" => Self::Unreachable,
            "decode" => Self::Decode,
            "not-ready" => Self::NotReady,
            "needs-update" => Self::NeedsUpdate,
            "other" => Self::Other,
            _ => return None,
        })
    }
}

/// One retained authenticated machine, in stable first-observed slot order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerObservation {
    /// Persistent allocation order; the local cell is separate.
    pub slot: u64,
    /// Verified identity, independent of addresses.
    pub identity: MachineIdentity,
    /// Earliest retained authenticated contact time.
    pub first_seen_ms: u64,
    /// Latest retained authenticated contact time.
    pub last_seen_ms: u64,
    /// Latest current-lifetime observation, if any.
    pub last_checked_ms: Option<u64>,
    /// Latest bounded repair metadata for this peer incarnation, independent of reachability.
    pub last_repair: Option<RepairObservation>,
    /// Latest successful outbound exchange; absent for older or inbound-only observations.
    pub last_authenticated_endpoint: Option<AuthenticatedEndpoint>,
    /// Current local evidence, never inferred from replayed contact.
    pub reachability: Reachability,
    /// Absent until authenticated per-process descriptors exist.
    pub instances: Option<usize>,
    /// Bounded recent history in observation order.
    pub samples: Vec<PresenceSample>,
}

/// Network facts owned by the local transport/history producer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkObservation {
    /// Changes only when the published facts change.
    pub revision: u64,
    /// Time of this local sample, milliseconds since Unix epoch.
    pub observed_at_ms: u64,
    /// Kernel-identified local process that relayed this live observation.
    /// None for direct observation and history-only reads; never persisted.
    pub via_local_process: Option<u32>,
    /// Local authenticated identity if available.
    pub observer: Option<MachineIdentity>,
    /// Discovery availability, independent of history and service access.
    pub visibility: Visibility,
    /// Retained-history read health.
    pub history: HistoryState,
    /// Safe fixed explanation; never a raw error or remote payload.
    pub detail: String,
    /// Stable machine slots, not sorted by reachability or labels.
    pub peers: Vec<PeerObservation>,
    /// Bounded unverified address candidates; never a machine count.
    pub unverified_endpoints: usize,
    /// Older detail or identities were compacted at the retention boundary.
    pub retention_limited: bool,
    /// Most recent admitted matching-ID exchange; retained history only.
    pub accounts_exchange: Option<AccountsExchangeObservation>,
}

/// Local access to one shared service, independent of network reachability.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ServiceAvailability {
    /// Access requirements or local state have not been established.
    #[default]
    Unknown,
    /// Required local access is unavailable.
    Unavailable,
    /// Usable local state exists; remote currency has not been established.
    Cached,
    /// Current within the adapter's explicitly observed scope.
    Current,
}

/// The URL's role in a historical vault network observation.
/// The kind reveals no URL and does not authenticate the serving machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VaultNetworkSource {
    /// The configured URL supplied the response.
    ConfiguredUrl,
    /// A peer URL supplied the response.
    PeerUrl,
}

/// A historical network fetch followed by successful local vault materialization.
/// This establishes neither remote currency nor provider sign-in readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VaultNetworkMaterialization {
    /// Closed source kind, without an endpoint or machine identity.
    pub source: VaultNetworkSource,
    /// The signed envelope sequence materialized by that pass.
    pub sequence: u64,
    /// Original network observation time in milliseconds since Unix epoch.
    pub sampled_at_ms: u64,
}

/// A retained actual HTTP 204 response, separate from signed vault materialization.
/// Its status establishes neither vault contents nor authorization or remote currency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VaultHttp204Response {
    /// Closed source role, without an endpoint or machine identity.
    pub source: VaultNetworkSource,
    /// Original response-received time in milliseconds since Unix epoch.
    pub responded_at_ms: u64,
}

/// The result of a retained actual vault network-refresh pass, not cache access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VaultNetworkRefreshOutcome {
    /// The pass completed after an actual network response; no global currency is implied.
    Succeeded,
    /// The attempted network refresh failed; usable local state may remain.
    Failed,
}

/// Sanitized historical refresh evidence, independently of local usability and receipts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VaultNetworkRefresh {
    /// Original completion time; reads and local cache repairs never renew it.
    pub completed_at_ms: u64,
    /// Closed result without an endpoint, error text, credential or response payload.
    pub outcome: VaultNetworkRefreshOutcome,
}

/// Sanitized facts from a service-specific producer, containing no secret values.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ServiceState {
    /// Local usability and scoped currency.
    pub availability: ServiceAvailability,
    /// Local observation time in milliseconds since Unix epoch.
    pub observed_at_ms: u64,
    /// Most recent successful local read/refresh if known.
    pub last_success_ms: Option<u64>,
    /// Historical signed network fetch and materialization, independently of local usability.
    pub network_materialization: Option<VaultNetworkMaterialization>,
    /// Retained HTTP status evidence, independent of local usability and signed materialization.
    pub http204_response: Option<VaultHttp204Response>,
    /// Retained network-refresh result for the current source/configuration and identity.
    pub network_refresh: Option<VaultNetworkRefresh>,
    /// Public sequence or frontier metadata only.
    pub revision: Option<String>,
    /// Sanitized explanation, not raw provider/vault error text.
    pub detail: String,
    /// A failed refresh does not remove an otherwise usable local cache.
    pub refresh_failed: bool,
}

/// Independently observed service access.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ServiceStates {
    /// AI account catalog, separate from provider login readiness.
    pub accounts: ServiceState,
    /// Authorized local secret availability, separate from lab membership.
    pub secrets: ServiceState,
}

/// Immutable composition assembled off the renderer from independent producers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkStateSnapshot {
    /// Host publication revision; service and network writers never share mutation.
    pub revision: u64,
    /// Facts from the network/history producer.
    pub network: NetworkObservation,
    /// Facts from service adapters.
    pub services: ServiceStates,
}
