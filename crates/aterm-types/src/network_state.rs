// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only local observations, never membership or global liveness claims.

/// Stable authenticated machine identity and its latest observed installation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineIdentity {
    /// UUID from the verified Commons binding, never an address or display name.
    pub machine_id: [u8; 16],
    /// Writer incarnation from that binding; not a process count.
    pub incarnation: [u8; 16],
    /// Optional authenticated display metadata.
    pub label: Option<String>,
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
    /// Another local process owns transport; its liveness has not been read.
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

/// Sanitized facts from a service-specific producer, containing no secret values.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ServiceState {
    /// Local usability and scoped currency.
    pub availability: ServiceAvailability,
    /// Local observation time in milliseconds since Unix epoch.
    pub observed_at_ms: u64,
    /// Most recent successful local read/refresh if known.
    pub last_success_ms: Option<u64>,
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
