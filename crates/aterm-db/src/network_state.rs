// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded observer-local history. The astream checkpoint is authoritative;
//! the private JSON projection is a read-only cache for other local processes.

use crate::{Error, Result};
use astream_broker::{BrokerLog, Durability};
use astream_wire::Offset;
use aterm_json::{Value, json};
use aterm_types::network_state::{
    AccountsExchangeObservation, AuthenticatedEndpoint, HistoryState, MachineIdentity,
    NetworkObservation, PeerObservation, PresenceSample, Reachability, Visibility,
};
use aterm_uds::private_file::{self, PrivateFileError};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const SUBJECT: &str = "aterm/network-observer/1";
const MAX_PEERS: usize = 256;
const MAX_SAMPLES: usize = 32;
const MAX_BYTES: usize = 512 * 1024;
const CHECKPOINT: Duration = Duration::from_secs(60);
const REACHABLE: Duration = Duration::from_secs(15);

pub(crate) mod account_exchange;
mod relay;
mod repair;

pub(crate) use relay::{
    error_stage as live_error_stage, read_publication as read_live_publication,
};

/// A second address for the current transport owner's observation, kept only while
/// that runtime holds the corresponding Commons lease.
pub(crate) struct TransportRelay(relay::Server);

impl TransportRelay {
    pub(crate) fn is_running(&self) -> bool {
        self.0.is_running()
    }
}

/// Read the leased transport owner's observation independently of its GUI configuration.
pub(crate) fn read_transport_publication(dir: &Path) -> std::io::Result<Publication> {
    relay::read_owner_at(dir)
}

/// An observation and its original local monotonic sample time. This lease is
/// process-local evidence and is never stored in the history journal.
#[derive(Clone, Debug)]
pub struct Publication {
    observation: NetworkObservation,
    at: Instant,
    contact_ages: BTreeMap<u64, Duration>,
}

impl Publication {
    /// An observation with no established monotonic peer-contact evidence.
    /// Reachable peers remain unconfirmed until the producer supplies that evidence.
    pub fn new(observation: NetworkObservation, at: Instant) -> Self {
        Self {
            observation,
            at,
            contact_ages: BTreeMap::new(),
        }
    }

    /// Original facts, before display-time age normalization.
    pub fn observation(&self) -> &NetworkObservation {
        &self.observation
    }

    /// Sample time on this process's monotonic clock, including later I/O delay.
    pub const fn sampled_at(&self) -> Instant {
        self.at
    }

    /// Current claims require both wall-clock and monotonic age evidence.
    /// Re-reading this value never renews its sample time.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountExchangeRetention",
            action = "Read",
            project = "aterm_db::network_state::account_exchange::tests::retention_transition"
        )
    )]
    pub fn current_at(&self, now: Instant, wall_ms: Option<u64>) -> NetworkObservation {
        let mut state = self.observation.clone();
        let has_current_claim = state.visibility != Visibility::Unavailable
            && (state.visibility != Visibility::Checking || state.observed_at_ms != 0);
        let elapsed = now.checked_duration_since(self.at);
        let current = elapsed.is_some_and(|age| age <= REACHABLE)
            && wall_ms.is_some_and(|wall| {
                state.observed_at_ms != 0
                    && wall
                        .checked_sub(state.observed_at_ms)
                        .is_some_and(|age| age <= 15_000)
            });
        if has_current_claim && !current {
            state.visibility = Visibility::Unavailable;
            state.detail.insert_str(
                0,
                "Current observation expired or its clock is unavailable; retained history remains. ",
            );
        }
        if state.visibility == Visibility::Active {
            let elapsed = elapsed.expect("current publication has a monotonic age");
            let wall = wall_ms.expect("current publication has a wall clock");
            let observed = state.observed_at_ms;
            for peer in &mut state.peers {
                if peer.reachability != Reachability::Reachable {
                    continue;
                }
                let age = |stamp: u64| {
                    if stamp == 0 {
                        return None;
                    }
                    let at_sample = observed.checked_sub(stamp)?;
                    let wall_age = wall.checked_sub(stamp)?;
                    Some(
                        Duration::from_millis(at_sample)
                            .checked_add(elapsed)?
                            .max(Duration::from_millis(wall_age)),
                    )
                };
                let contact = self
                    .contact_ages
                    .get(&peer.slot)
                    .and_then(|age| age.checked_add(elapsed));
                peer.reachability = match (
                    age(peer.last_seen_ms),
                    peer.last_checked_ms.and_then(age),
                    contact,
                ) {
                    (Some(seen), Some(checked), Some(contact))
                        if seen <= REACHABLE && checked <= REACHABLE && contact <= REACHABLE =>
                    {
                        Reachability::Reachable
                    }
                    (Some(_), Some(_), Some(_)) => Reachability::LastSeen,
                    _ => Reachability::Unknown,
                };
            }
        } else {
            for peer in &mut state.peers {
                if peer.reachability == Reachability::Reachable {
                    peer.reachability = Reachability::Unknown;
                }
            }
        }
        state
    }
}

/// Read-only shared data; readers cannot replace the observer's evidence.
#[derive(Clone)]
pub struct ObservationHandle(Arc<Mutex<Publication>>);
impl Default for ObservationHandle {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Publication::new(
            NetworkObservation::default(),
            Instant::now(),
        ))))
    }
}
impl ObservationHandle {
    /// Clone the latest immutable view. This reads no filesystem or network.
    pub fn snapshot(&self) -> NetworkObservation {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .observation
            .clone()
    }
    /// Clone the current view without waiting for the publishing worker. A
    /// contended read has no new evidence; callers retain their previous view
    /// and its original timestamps rather than treating it as a fresh sample.
    pub fn try_snapshot(&self) -> Option<NetworkObservation> {
        self.try_publication()
            .map(|publication| publication.observation)
    }
    /// Read the observation with its original monotonic sample time, without
    /// waiting for its publisher. Consumers must retain this time on cache hits.
    pub fn try_publication(&self) -> Option<Publication> {
        match self.0.try_lock() {
            Ok(state) => Some(state.clone()),
            Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner().clone()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
    fn publication_if_fresh(&self, max_age: Duration, now: Instant) -> Option<Publication> {
        self.try_publication().filter(|publication| {
            now.checked_duration_since(publication.at)
                .is_some_and(|age| age <= max_age)
        })
    }
    #[cfg(test)]
    fn snapshot_if_fresh(&self, max_age: Duration) -> Option<NetworkObservation> {
        self.publication_if_fresh(max_age, Instant::now())
            .map(|publication| publication.observation)
    }
    pub(crate) fn replace(&self, state: NetworkObservation) {
        self.replace_publication(Publication::new(state, Instant::now()));
    }
    pub(crate) fn replace_publication(&self, publication: Publication) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = publication;
    }
}

#[derive(Debug)]
pub(crate) enum OpenFailure {
    Owned,
    Unavailable,
}
impl From<Error> for OpenFailure {
    fn from(_: Error) -> Self {
        Self::Unavailable
    }
}
impl From<std::io::Error> for OpenFailure {
    fn from(_: std::io::Error) -> Self {
        Self::Unavailable
    }
}

/// Stable local location independent of whether a secrets identity exists.
pub fn directory(config: &Path) -> PathBuf {
    config.with_file_name("network-state")
}

/// Whether the Commons runtime's observation history for `config` exists: its
/// `observations.astream` is a regular file (a link is not followed). Only an owner's observer
/// creates it, as it opens (`Observer::open`).
#[cfg(test)]
#[must_use]
pub(crate) fn history_exists(config: &Path) -> bool {
    std::fs::symlink_metadata(directory(config).join("observations.astream"))
        .is_ok_and(|metadata| metadata.file_type().is_file())
}

fn bad() -> Error {
    Error::new("network observer history is invalid or unavailable")
}
fn number(v: &Value, key: &str) -> Result<u64> {
    v.get(key).and_then(Value::as_u64).ok_or_else(bad)
}
fn bytes<const N: usize>(v: &Value, key: &str) -> Result<[u8; N]> {
    let s = v.get(key).and_then(Value::as_str).ok_or_else(bad)?;
    crate::database::decode_hex(s)
}
fn identity(v: &Value) -> Result<MachineIdentity> {
    Ok(MachineIdentity {
        machine_id: bytes(v, "machine")?,
        incarnation: bytes(v, "incarnation")?,
        label: None,
    })
}
fn identity_value(i: &MachineIdentity) -> Value {
    json!({"machine":aterm_codec::hex::encode_lower(&i.machine_id),"incarnation":aterm_codec::hex::encode_lower(&i.incarnation)})
}
fn accounts_exchange_value(exchange: &AccountsExchangeObservation) -> Value {
    json!({"scope_digest":aterm_codec::hex::encode_lower(&exchange.scope_digest),
        "local_actor":aterm_codec::hex::encode_lower(&exchange.local_actor),
        "control_observation":aterm_codec::hex::encode_lower(&exchange.control_observation),
        "inventory_count":exchange.inventory_count,
        "inventory_fingerprint":aterm_codec::hex::encode_lower(&exchange.inventory_fingerprint),
        "peer":identity_value(&exchange.peer),"sampled_at_ms":exchange.sampled_at_ms})
}
fn accounts_exchange(value: &Value) -> Result<Option<AccountsExchangeObservation>> {
    value
        .get("accounts_exchange")
        .filter(|value| !value.is_null())
        .map(|value| {
            let inventory_count = number(value, "inventory_count")?;
            let sampled_at_ms = number(value, "sampled_at_ms")?;
            if inventory_count > crate::replica::MAX_RECORDS as u64 || sampled_at_ms == 0 {
                return Err(bad());
            }
            Ok(AccountsExchangeObservation {
                scope_digest: bytes(value, "scope_digest")?,
                local_actor: bytes(value, "local_actor")?,
                control_observation: bytes(value, "control_observation")?,
                inventory_count,
                inventory_fingerprint: bytes(value, "inventory_fingerprint")?,
                peer: identity(value.get("peer").ok_or_else(bad)?)?,
                sampled_at_ms,
            })
        })
        .transpose()
}
fn endpoint_value(endpoint: &AuthenticatedEndpoint) -> Value {
    json!({"address":endpoint.address.to_string(),"observed_at_ms":endpoint.observed_at_ms})
}
fn endpoint(row: &Value) -> Result<Option<AuthenticatedEndpoint>> {
    row.get("last_authenticated_endpoint")
        .filter(|value| !value.is_null())
        .map(|value| {
            let address = value
                .get("address")
                .and_then(Value::as_str)
                .ok_or_else(bad)?
                .parse()
                .map_err(|_| bad())?;
            let observed_at_ms = number(value, "observed_at_ms")?;
            if observed_at_ms == 0 {
                return Err(bad());
            }
            Ok(AuthenticatedEndpoint {
                address,
                observed_at_ms,
            })
        })
        .transpose()
}
fn state_value(s: Reachability) -> &'static str {
    match s {
        Reachability::Reachable => "reachable",
        Reachability::LastSeen => "last-seen",
        Reachability::Unknown => "unknown",
    }
}
fn state(v: &Value) -> Result<Reachability> {
    match v.as_str() {
        Some("reachable") => Ok(Reachability::Reachable),
        Some("last-seen") => Ok(Reachability::LastSeen),
        Some("unknown") => Ok(Reachability::Unknown),
        _ => Err(bad()),
    }
}

fn encode_value(
    observation: &NetworkObservation,
    trust: Option<aterm_secrets::commons::journal::Trust>,
) -> Value {
    let peers: Vec<_> = observation.peers.iter().map(|p| json!({
        "slot":p.slot,"identity":identity_value(&p.identity),"first":p.first_seen_ms,"last":p.last_seen_ms,
        "last_authenticated_endpoint":p.last_authenticated_endpoint.as_ref().map(endpoint_value),
        "last_repair":p.last_repair.as_ref().map(repair::value),
        "samples":p.samples.iter().map(|s| json!({"at":s.at_ms,"state":state_value(s.state)})).collect::<Vec<_>>()
    })).collect();
    json!({"version":1,"revision":observation.revision,"observed":observation.observed_at_ms,
        "observer":observation.observer.as_ref().map(identity_value),"limited":observation.retention_limited,"peers":peers,
        "accounts_exchange":observation.accounts_exchange.as_ref().map(accounts_exchange_value),
        "trust":trust.map(|t|json!({"network":aterm_codec::hex::encode_lower(&t.network),"authority":aterm_codec::hex::encode_lower(&t.authority),"genesis":aterm_codec::hex::encode_lower(&t.genesis)}))})
}
fn encode(
    observation: &NetworkObservation,
    trust: Option<aterm_secrets::commons::journal::Trust>,
) -> Result<Vec<u8>> {
    let value = encode_value(observation, trust);
    let wire = aterm_json::to_vec(&value).map_err(|_| bad())?;
    if wire.len() > MAX_BYTES {
        return Err(bad());
    }
    Ok(wire)
}
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbAccountExchangeRetention",
        action = "HistoryRead",
        project = "aterm_db::network_state::account_exchange::tests::retention_transition"
    )
)]
fn decode(wire: &[u8]) -> Result<NetworkObservation> {
    if wire.len() > MAX_BYTES {
        return Err(bad());
    }
    let value: Value = aterm_json::from_slice(wire).map_err(|_| bad())?;
    decode_value(&value)
}
fn decode_value(value: &Value) -> Result<NetworkObservation> {
    if number(value, "version")? != 1 {
        return Err(bad());
    }
    let rows = value
        .get("peers")
        .and_then(Value::as_array)
        .ok_or_else(bad)?;
    if rows.len() > MAX_PEERS {
        return Err(bad());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut slots = std::collections::BTreeSet::new();
    let mut peers = Vec::with_capacity(rows.len());
    for row in rows {
        let identity = identity(row.get("identity").ok_or_else(bad)?)?;
        let slot = number(row, "slot")?;
        if !seen.insert(identity.machine_id) || !slots.insert(slot) {
            return Err(bad());
        }
        let samples = row
            .get("samples")
            .and_then(Value::as_array)
            .ok_or_else(bad)?;
        if samples.len() > MAX_SAMPLES {
            return Err(bad());
        }
        peers.push(PeerObservation {
            slot,
            identity,
            first_seen_ms: number(row, "first")?,
            last_seen_ms: number(row, "last")?,
            last_checked_ms: None,
            last_repair: repair::read(row)?,
            last_authenticated_endpoint: endpoint(row)?,
            reachability: Reachability::Unknown,
            instances: None,
            samples: samples
                .iter()
                .map(|s| {
                    Ok(PresenceSample {
                        at_ms: number(s, "at")?,
                        state: state(s.get("state").ok_or_else(bad)?)?,
                    })
                })
                .collect::<Result<_>>()?,
        });
    }
    peers.sort_unstable_by_key(|p| p.slot);
    let observer = value
        .get("observer")
        .filter(|v| !v.is_null())
        .map(identity)
        .transpose()?;
    Ok(NetworkObservation {
        revision: number(value, "revision")?,
        observed_at_ms: number(value, "observed")?,
        observer,
        accounts_exchange: accounts_exchange(value)?,
        history: HistoryState::Ready,
        detail: "Retained history; current reachability has not been checked".into(),
        peers,
        retention_limited: value
            .get("limited")
            .and_then(Value::as_bool)
            .ok_or_else(bad)?,
        ..NetworkObservation::default()
    })
}
fn decode_trust_value(value: &Value) -> Result<Option<aterm_secrets::commons::journal::Trust>> {
    value
        .get("trust")
        .filter(|v| !v.is_null())
        .map(|v| {
            Ok(aterm_secrets::commons::journal::Trust {
                network: bytes(v, "network")?,
                authority: bytes(v, "authority")?,
                genesis: bytes(v, "genesis")?,
            })
        })
        .transpose()
}
fn decode_record(
    wire: &[u8],
) -> Result<(
    NetworkObservation,
    Option<aterm_secrets::commons::journal::Trust>,
)> {
    if wire.len() > MAX_BYTES {
        return Err(bad());
    }
    let value: Value = aterm_json::from_slice(wire).map_err(|_| bad())?;
    Ok((decode_value(&value)?, decode_trust_value(&value)?))
}

/// The live observation another local process (a window, or the node job) publishes of
/// `config`'s Commons, read over its relay within the relay's exchange bound and taken as of now
/// ([`Publication::current_at`]: an expired one reads `Unavailable`). It opens no writer and
/// contacts no peer.
///
/// # Errors
///
/// No local owner answers, or its answer is refused.
pub fn read_live_observation(config: &Path) -> std::io::Result<NetworkObservation> {
    read_live_publication(config)
        .map(|publication| publication.current_at(Instant::now(), aterm_time::unix_millis()))
}

/// Read the last published local history without opening a writer, creating a
/// directory, contacting peers or reading any secret payload. Replayed contacts
/// are always unknown, never current reachability or a current local identity.
pub fn read_history(config: &Path) -> Result<NetworkObservation> {
    match private_file::read_private(&directory(config), "projection.json", MAX_BYTES) {
        Ok(wire) => {
            let mut observation = decode(&wire)?;
            observation.observer = None;
            observation.visibility = Visibility::Unavailable;
            Ok(observation)
        }
        Err(PrivateFileError::Missing) => Ok(NetworkObservation {
            visibility: Visibility::Unavailable,
            history: HistoryState::Unavailable,
            detail: "No retained network projection is available; this read does not check live reachability".into(),
            ..NetworkObservation::default()
        }),
        Err(_) => Err(bad()),
    }
}

pub(crate) struct Observer {
    relay: Option<relay::Server>,
    relay_checked: Instant,
    state: NetworkObservation,
    published: ObservationHandle,
    log: Option<BrokerLog>,
    dir: PathBuf,
    seen: BTreeMap<[u8; 16], Instant>,
    checkpoint: Instant,
    published_at: Instant,
    dirty: bool,
    changed: bool,
    poisoned: bool,
    trust: Option<aterm_secrets::commons::journal::Trust>,
    accepting: bool,
    local_verified: bool,
    accounts_exchange_sampled: Option<Instant>,
    repair_sampled: BTreeMap<[u8; 16], Instant>,
}
impl Observer {
    /// Publish this observer beside its transport lease. Failure leaves repair running;
    /// the runtime retries the relay while it retains that lease.
    pub(crate) fn transport_relay(&self, dir: &Path) -> std::io::Result<TransportRelay> {
        relay::Server::start(dir, self.published.clone()).map(TransportRelay)
    }

    pub(crate) fn open(
        config: &Path,
        published: ObservationHandle,
    ) -> std::result::Result<Self, OpenFailure> {
        let dir = directory(config);
        private_file::create_private_dir(&dir)?;
        aterm_secrets::fsx::write_new(&dir.join("observations.astream"), b"", 0o600)
            .map_err(|_| bad())?;
        let (_, metadata) =
            private_file::open_private(&dir, "observations.astream").map_err(|_| bad())?;
        if metadata.len() > (MAX_BYTES * 3) as u64 {
            return Err(OpenFailure::Unavailable);
        }
        let log = BrokerLog::open_with(dir.join("observations.astream"), Durability::Strict)
            .map_err(|error| {
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::WouldBlock
                ) {
                    OpenFailure::Owned
                } else {
                    OpenFailure::Unavailable
                }
            })?;
        if log.stats().retained_bytes > MAX_BYTES * 3 {
            return Err(OpenFailure::Unavailable);
        }
        let mut state = NetworkObservation {
            history: HistoryState::Ready,
            ..NetworkObservation::default()
        };
        let mut offset = log.read_floor();
        let mut trust = None;
        loop {
            let records = log.read_range(offset, 16);
            if records.is_empty() {
                break;
            }
            for record in records {
                if record.seq != offset || record.subject != SUBJECT {
                    return Err(OpenFailure::Unavailable);
                }
                (state, trust) = decode_record(&record.body)?;
                offset = Offset(record.seq.0.checked_add(1).ok_or_else(bad)?);
            }
        }
        if log.stats().head != offset {
            return Err(OpenFailure::Unavailable);
        }
        let mut observer = Self {
            relay: None,
            relay_checked: Instant::now(),
            state,
            published,
            log: Some(log),
            dir,
            seen: BTreeMap::new(),
            checkpoint: Instant::now(),
            published_at: Instant::now(),
            dirty: true,
            changed: true,
            poisoned: false,
            trust,
            accepting: true,
            local_verified: false,
            accounts_exchange_sampled: None,
            repair_sampled: BTreeMap::new(),
        };
        observer.publish();
        observer.relay = relay::Server::start(&observer.dir, observer.published.clone()).ok();
        Ok(observer)
    }
    pub(crate) fn unavailable(config: &Path, published: ObservationHandle) -> Self {
        let mut state = read_history(config).unwrap_or_default();
        state.history = HistoryState::Unavailable;
        state.detail = "Network history unavailable; retained identities are not current".into();
        let mut observer = Self {
            relay: None,
            relay_checked: Instant::now(),
            state,
            published,
            log: None,
            dir: directory(config),
            seen: BTreeMap::new(),
            checkpoint: Instant::now(),
            published_at: Instant::now(),
            dirty: false,
            changed: true,
            poisoned: true,
            trust: None,
            accepting: false,
            local_verified: false,
            accounts_exchange_sampled: None,
            repair_sampled: BTreeMap::new(),
        };
        observer.publish();
        observer
    }
    /// Keep the retained identity guard while withdrawing its live confirmation.
    pub(crate) fn unconfirm_local(&mut self) {
        self.local_verified = false;
    }

    /// Retain an admitted historical account exchange in original sample order.
    pub(crate) fn accounts_exchange(&mut self, candidate: account_exchange::Candidate) {
        if !self.accepting || !self.local_verified {
            return;
        }
        // Resampling the same local evidence, including through another peer, follows contact
        // progress: publish it live and checkpoint periodically. Persist a changed local
        // inventory or authority context immediately, without changing its original timestamps.
        let transition = self
            .state
            .accounts_exchange
            .as_ref()
            .is_none_or(|previous| {
                let next = &candidate.observation;
                previous.scope_digest != next.scope_digest
                    || previous.local_actor != next.local_actor
                    || previous.control_observation != next.control_observation
                    || previous.inventory_count != next.inventory_count
                    || previous.inventory_fingerprint != next.inventory_fingerprint
            });
        if account_exchange::retain(
            &mut self.state.accounts_exchange,
            &mut self.accounts_exchange_sampled,
            candidate,
        ) {
            self.dirty |= transition;
            self.changed = true;
        }
    }

    pub(crate) fn local(
        &mut self,
        identity: MachineIdentity,
        trust: aterm_secrets::commons::journal::Trust,
    ) -> Result<()> {
        self.local_verified = false;
        if self.log.is_none() {
            return Err(bad());
        }
        if self.trust.is_some_and(|old| old != trust) {
            self.accepting = false;
            self.seen.clear();
            self.state.history = HistoryState::Unavailable;
            return Err(Error::new(
                "network observer trust differs from retained history",
            ));
        }
        if self.trust != Some(trust) {
            self.trust = Some(trust);
            self.dirty = true;
        }
        if self
            .state
            .observer
            .as_ref()
            .is_some_and(|old| old.machine_id != identity.machine_id)
        {
            self.accepting = false;
            self.seen.clear();
            self.state.history = HistoryState::Unavailable;
            return Err(Error::new(
                "network observer identity differs from retained history",
            ));
        }
        if self.state.observer.as_ref() != Some(&identity) {
            if self
                .state
                .observer
                .as_ref()
                .is_none_or(|old| old.incarnation != identity.incarnation)
            {
                for peer in &mut self.state.peers {
                    peer.last_repair = None;
                }
                self.repair_sampled.clear();
            }
            self.state.observer = Some(identity);
            self.dirty = true;
        }
        self.accepting = true;
        self.local_verified = true;
        if !self.poisoned {
            self.state.history = HistoryState::Ready;
        }
        Ok(())
    }
    /// Only callers resolving a completed authenticated exchange to a verified
    /// Commons binding may add an identity. Discovery addresses never call this.
    pub(crate) fn contact(&mut self, identity: MachineIdentity, at_ms: u64, now: Instant) {
        if !self.accepting {
            return;
        }
        let key = identity.machine_id;
        if self
            .state
            .observer
            .as_ref()
            .is_some_and(|own| own.machine_id == key)
        {
            return;
        }
        if let Some(p) = self
            .state
            .peers
            .iter_mut()
            .find(|p| p.identity.machine_id == key)
        {
            self.dirty |= p.identity != identity || p.reachability != Reachability::Reachable;
            if p.identity.incarnation != identity.incarnation {
                p.last_authenticated_endpoint = None;
                p.last_repair = None;
                self.repair_sampled.remove(&key);
            }
            p.identity = identity;
            p.last_seen_ms = at_ms;
            p.last_checked_ms = Some(at_ms);
            if p.reachability != Reachability::Reachable {
                self.state.retention_limited |= p.samples.len() == MAX_SAMPLES;
                sample(p, at_ms, Reachability::Reachable);
            }
        } else {
            if self.state.peers.len() == MAX_PEERS {
                self.dirty |= !self.state.retention_limited;
                self.state.retention_limited = true;
                return;
            }
            let slot = self
                .state
                .peers
                .last()
                .map_or(1, |p| p.slot.saturating_add(1));
            self.state.peers.push(PeerObservation {
                slot,
                identity,
                first_seen_ms: at_ms,
                last_seen_ms: at_ms,
                last_checked_ms: Some(at_ms),
                last_repair: None,
                last_authenticated_endpoint: None,
                reachability: Reachability::Reachable,
                instances: None,
                samples: vec![PresenceSample {
                    at_ms,
                    state: Reachability::Reachable,
                }],
            });
            self.dirty = true;
        }
        self.seen.insert(key, now);
        self.changed = true;
    }
    /// Attach only runtime-validated endpoint evidence to an already observed exact identity.
    /// This does not refresh contact time or reachability.
    pub(crate) fn endpoint(&mut self, identity: &MachineIdentity, endpoint: AuthenticatedEndpoint) {
        if !self.accepting || endpoint.observed_at_ms == 0 {
            return;
        }
        let Some(peer) = self
            .state
            .peers
            .iter_mut()
            .find(|peer| peer.identity == *identity)
        else {
            return;
        };
        if peer
            .last_authenticated_endpoint
            .as_ref()
            .is_some_and(|old| old.observed_at_ms > endpoint.observed_at_ms)
        {
            return;
        }
        self.dirty |= peer
            .last_authenticated_endpoint
            .as_ref()
            .is_none_or(|old| old.address != endpoint.address);
        peer.last_authenticated_endpoint = Some(endpoint);
        self.changed = true;
    }
    pub(crate) fn observe(
        &mut self,
        visibility: Visibility,
        unverified: usize,
        detail: &str,
        at_ms: u64,
        now: Instant,
    ) {
        if self.log.is_some()
            && now.saturating_duration_since(self.relay_checked) >= Duration::from_secs(10)
        {
            self.relay_checked = now;
            if !self.relay.as_ref().is_some_and(relay::Server::is_running) {
                drop(self.relay.take());
                self.relay = relay::Server::start(&self.dir, self.published.clone()).ok();
            }
        }
        let available = visibility == Visibility::Active && self.accepting;
        let detail = if self.poisoned {
            "Network history checkpoint unavailable"
        } else if !self.accepting {
            "Network history belongs to another observer or trust context"
        } else {
            detail
        };
        if self.state.visibility != visibility
            || self.state.unverified_endpoints != unverified
            || self.state.detail != detail
        {
            self.changed = true;
        }
        self.state.visibility = visibility;
        self.state.unverified_endpoints = unverified;
        if self.state.detail != detail {
            self.state.detail.clear();
            self.state.detail.push_str(detail);
        }
        for peer in &mut self.state.peers {
            let next = if !available || at_ms == 0 {
                Reachability::Unknown
            } else if self
                .seen
                .get(&peer.identity.machine_id)
                .is_some_and(|at| now.saturating_duration_since(*at) < REACHABLE)
                && at_ms
                    .checked_sub(peer.last_seen_ms)
                    .is_some_and(|elapsed| elapsed < 15_000)
            {
                Reachability::Reachable
            } else {
                Reachability::LastSeen
            };
            if peer.reachability != next {
                self.state.retention_limited |= peer.samples.len() == MAX_SAMPLES;
                sample(peer, at_ms, next);
                self.dirty = true;
                self.changed = true;
            }
        }
        let checkpoint = self.dirty || now.saturating_duration_since(self.checkpoint) >= CHECKPOINT;
        if self.changed
            || checkpoint
            || now.saturating_duration_since(self.published_at) >= Duration::from_secs(2)
        {
            self.state.observed_at_ms = at_ms;
            self.state.revision = self.state.revision.saturating_add(1);
            // Checkpoint only transitions and periodic contact progress. Each
            // complete checkpoint survives before the old prefix is compacted.
            let result = (|| -> Result<()> {
                if !checkpoint {
                    return Ok(());
                }
                if self.poisoned {
                    return Err(bad());
                }
                let wire = encode(&self.state, self.trust)?;
                let log = self.log.as_mut().ok_or_else(bad)?;
                let seq = log.stats().head.0.checked_add(1).ok_or_else(bad)?;
                let (offset, _) = log
                    .publish_checked(1, seq, SUBJECT.into(), wire.clone())
                    .map_err(|_| bad())?;
                log.retain_before(offset)?;
                private_file::write_private(&self.dir, "projection.json", &wire)?;
                Ok(())
            })();
            if result.is_err() {
                self.poisoned = true;
                self.state.history = HistoryState::Unavailable;
                self.state.detail = "Network history checkpoint unavailable".into();
            }
            if checkpoint {
                self.checkpoint = now;
            }
            self.dirty = false;
            self.changed = false;
            self.published_at = now;
            self.publish();
        }
    }
    fn publish(&mut self) {
        let mut observation = self.state.clone();
        if !self.local_verified {
            observation.observer = None;
        }
        self.published.replace_publication(Publication {
            observation,
            at: self.published_at,
            contact_ages: self
                .state
                .peers
                .iter()
                .filter_map(|peer| {
                    self.seen
                        .get(&peer.identity.machine_id)
                        .and_then(|at| self.published_at.checked_duration_since(*at))
                        .map(|age| (peer.slot, age))
                })
                .collect(),
        });
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        // The read-only endpoint must stop before the journal releases ownership.
        drop(self.relay.take());
    }
}

fn sample(peer: &mut PeerObservation, at_ms: u64, state: Reachability) {
    peer.reachability = state;
    if peer.samples.len() == MAX_SAMPLES {
        peer.samples.remove(0);
    }
    peer.samples.push(PresenceSample { at_ms, state });
}

/// Safe metadata-only representation for a local read command. Endpoint ages use this
/// read's wall clock; an unavailable clock or future stamp leaves the age unknown.
pub fn value(observation: &NetworkObservation) -> Value {
    let now = aterm_time::unix_millis();
    let visibility = match observation.visibility {
        Visibility::Checking => "Checking",
        Visibility::Active => "Active",
        Visibility::Offline => "Offline",
        Visibility::Disabled => "Disabled",
        Visibility::Waiting => "Waiting",
        Visibility::Unavailable => "Unavailable",
    };
    let history = match observation.history {
        HistoryState::Ready => "Ready",
        HistoryState::Restoring => "Restoring",
        HistoryState::Unavailable => "Unavailable",
    };
    json!({"revision":observation.revision,"observed_at_ms":observation.observed_at_ms,
        "via_local_process":observation.via_local_process,
        "visibility":visibility,"history":history,"detail":observation.detail,
        "observer":observation.observer.as_ref().map(identity_value),"unverified_endpoints":observation.unverified_endpoints,
        "retention_limited":observation.retention_limited,"instances":Value::Null,
        "accounts_exchange":observation.accounts_exchange.as_ref().map(accounts_exchange_value),
        "peers":observation.peers.iter().map(|p|json!({"slot":p.slot,"identity":identity_value(&p.identity),"first_seen_ms":p.first_seen_ms,
            "last_seen_ms":p.last_seen_ms,"reachability":state_value(p.reachability),"instances":Value::Null,
            "last_repair":p.last_repair.as_ref().map(repair::value),
            "last_authenticated_endpoint":p.last_authenticated_endpoint.as_ref().map(|endpoint| json!({
                "address":endpoint.address.to_string(),"observed_at_ms":endpoint.observed_at_ms,
                "age_ms":now.and_then(|now|now.checked_sub(endpoint.observed_at_ms))
            }))})).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reachable_publication(at: Instant) -> Publication {
        Publication {
            observation: NetworkObservation {
                revision: 23,
                observed_at_ms: 100_000,
                visibility: Visibility::Active,
                history: HistoryState::Ready,
                observer: Some(peer(1, 1)),
                peers: vec![PeerObservation {
                    slot: 1,
                    identity: peer(2, 1),
                    first_seen_ms: 99_000,
                    last_seen_ms: 100_000,
                    last_checked_ms: Some(100_000),
                    last_repair: None,
                    last_authenticated_endpoint: None,
                    reachability: Reachability::Reachable,
                    instances: None,
                    samples: vec![PresenceSample {
                        at_ms: 100_000,
                        state: Reachability::Reachable,
                    }],
                }],
                ..NetworkObservation::default()
            },
            at,
            contact_ages: BTreeMap::from([(1, Duration::ZERO)]),
        }
    }

    #[test]
    fn publication_cannot_become_current_again_after_wall_clock_rollback() {
        let at = Instant::now();
        let publication = reachable_publication(at);
        let current = publication.current_at(at, Some(100_000));
        assert_eq!(current.visibility, Visibility::Active);
        assert_eq!(current.peers[0].reachability, Reachability::Reachable);
        for (now, wall) in [
            (at + Duration::from_secs(20), Some(120_000)),
            (at + Duration::from_secs(21), Some(113_000)),
            (at - Duration::from_secs(1), Some(100_000)),
            (at, Some(99_999)),
            (at, None),
        ] {
            let expired = publication.current_at(now, wall);
            assert_eq!(expired.visibility, Visibility::Unavailable);
            assert_eq!(expired.peers[0].reachability, Reachability::Unknown);
            let mut expected = current.clone();
            expected.visibility = expired.visibility;
            expected.detail.clone_from(&expired.detail);
            expected.peers[0].reachability = Reachability::Unknown;
            assert_eq!(expired, expected, "expiry changed retained evidence");
            assert_eq!(
                value(&expired)["peers"][0]["reachability"],
                json!("unknown")
            );
        }
        assert_eq!(publication.observation, current);
    }

    #[test]
    fn a_nonactive_publication_has_no_current_peer_claim() {
        let at = Instant::now();
        for (visibility, observed_at_ms) in [
            (Visibility::Unavailable, 100_000),
            (Visibility::Checking, 0),
            (Visibility::Checking, 100_000),
            (Visibility::Offline, 100_000),
            (Visibility::Disabled, 100_000),
            (Visibility::Waiting, 100_000),
        ] {
            let mut publication = reachable_publication(at);
            publication.observation.visibility = visibility;
            publication.observation.observed_at_ms = observed_at_ms;
            let current = publication.current_at(at, Some(100_000));
            let mut expected = publication.observation.clone();
            expected.peers[0].reachability = Reachability::Unknown;
            assert_eq!(
                current, expected,
                "{visibility:?} kept a current peer claim"
            );
            assert_eq!(
                publication.observation.peers[0].reachability,
                Reachability::Reachable
            );
        }
    }

    #[test]
    fn peer_age_includes_elapsed_time_since_its_published_observation() {
        let at = Instant::now();
        let mut publication = Publication {
            observation: NetworkObservation {
                observed_at_ms: 100_000,
                visibility: Visibility::Active,
                peers: vec![PeerObservation {
                    slot: 1,
                    identity: peer(2, 1),
                    first_seen_ms: 91_000,
                    last_seen_ms: 91_000,
                    last_checked_ms: Some(91_000),
                    last_repair: None,
                    last_authenticated_endpoint: None,
                    reachability: Reachability::Reachable,
                    instances: None,
                    samples: Vec::new(),
                }],
                ..NetworkObservation::default()
            },
            at,
            contact_ages: BTreeMap::from([(1, Duration::from_secs(9))]),
        };
        assert_eq!(
            publication
                .current_at(at + Duration::from_secs(6), Some(101_000))
                .peers[0]
                .reachability,
            Reachability::Reachable
        );
        assert_eq!(
            publication
                .current_at(at + Duration::from_secs(7), Some(101_000))
                .peers[0]
                .reachability,
            Reachability::LastSeen
        );
        for checked in [None, Some(0), Some(100_001)] {
            publication.observation.peers[0].last_checked_ms = checked;
            assert_eq!(
                publication.current_at(at, Some(101_000)).peers[0].reachability,
                Reachability::Unknown
            );
        }
    }

    #[test]
    fn checkpoint_publication_preserves_the_input_sample_instant() {
        let root = aterm_tempfile::tempdir().unwrap();
        let handle = ObservationHandle::default();
        let mut observer = Observer::open(&root.path().join("config"), handle.clone()).unwrap();
        let sampled = Instant::now() - Duration::from_secs(20);
        observer.observe(Visibility::Active, 0, "sampled", 100_000, sampled);
        let publication = handle.try_publication().unwrap();
        assert_eq!(publication.at, sampled);
        assert_eq!(
            publication
                .current_at(sampled + Duration::from_secs(21), Some(101_000))
                .visibility,
            Visibility::Unavailable
        );
    }

    #[test]
    fn contact_age_survives_a_wall_rollback_before_publication() {
        let root = aterm_tempfile::tempdir().unwrap();
        let handle = ObservationHandle::default();
        let mut observer = Observer::open(&root.path().join("config"), handle.clone()).unwrap();
        let at = Instant::now();
        observer.contact(peer(2, 1), 100_000, at);
        observer.observe(
            Visibility::Active,
            0,
            "sampled",
            102_000,
            at + Duration::from_secs(10),
        );
        let publication = handle.try_publication().unwrap();
        assert_eq!(
            publication.observation.peers[0].reachability,
            Reachability::Reachable
        );
        assert_eq!(
            publication
                .current_at(at + Duration::from_secs(17), Some(109_000))
                .peers[0]
                .reachability,
            Reachability::LastSeen
        );
    }

    #[test]
    fn missing_or_overflowing_contact_age_cannot_authorize_reachability() {
        let at = Instant::now();
        let mut publication = Publication::new(
            NetworkObservation {
                observed_at_ms: 100_000,
                visibility: Visibility::Active,
                peers: vec![PeerObservation {
                    slot: 1,
                    identity: peer(2, 1),
                    first_seen_ms: 99_000,
                    last_seen_ms: 99_000,
                    last_checked_ms: Some(99_000),
                    last_repair: None,
                    last_authenticated_endpoint: None,
                    reachability: Reachability::Reachable,
                    instances: None,
                    samples: Vec::new(),
                }],
                ..NetworkObservation::default()
            },
            at,
        );
        assert_eq!(
            publication.current_at(at, Some(100_000)).peers[0].reachability,
            Reachability::Unknown
        );
        publication.contact_ages.insert(1, Duration::MAX);
        assert_eq!(
            publication
                .current_at(at + Duration::from_secs(1), Some(101_000))
                .peers[0]
                .reachability,
            Reachability::Unknown
        );
    }
    #[test]
    fn contended_snapshot_has_no_new_evidence_and_recovers_after_publish() {
        let handle = handle();
        let mut publisher = handle.0.lock().unwrap();
        publisher.observation.observed_at_ms = 1000;
        publisher.observation.revision = 7;
        let reader = handle.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || send.send(reader.try_snapshot()).unwrap());
        let result = receive.recv_timeout(Duration::from_secs(60));
        drop(publisher);
        task.join().unwrap();
        assert_eq!(
            result.unwrap(),
            None,
            "a busy publisher must not stall the reader"
        );
        let published = handle.try_snapshot().unwrap();
        assert_eq!(published.observed_at_ms, 1000);
        assert_eq!(published.revision, 7);
    }

    fn peer(machine: u8, incarnation: u8) -> MachineIdentity {
        MachineIdentity {
            machine_id: [machine; 16],
            incarnation: [incarnation; 16],
            label: None,
        }
    }
    fn handle() -> ObservationHandle {
        ObservationHandle::default()
    }
    fn trust() -> aterm_secrets::commons::journal::Trust {
        aterm_secrets::commons::journal::Trust {
            network: [1; 16],
            authority: [2; 32],
            genesis: [3; 32],
        }
    }
    fn exchange() -> AccountsExchangeObservation {
        AccountsExchangeObservation {
            scope_digest: [3; 32],
            local_actor: [4; 48],
            control_observation: [5; 64],
            inventory_count: 2,
            inventory_fingerprint: [6; 32],
            peer: peer(2, 1),
            sampled_at_ms: 1000,
        }
    }
    #[test]
    fn historical_exchange_codec_is_bounded_and_old_history_has_no_witness() {
        let observation = NetworkObservation {
            accounts_exchange: Some(exchange()),
            ..NetworkObservation::default()
        };
        let wire = encode(&observation, None).unwrap();
        assert_eq!(
            decode(&wire).unwrap().accounts_exchange,
            observation.accounts_exchange
        );
        let mut old: Value = aterm_json::from_slice(&wire).unwrap();
        old.as_object_mut().unwrap().remove("accounts_exchange");
        assert_eq!(
            decode(&aterm_json::to_vec(&old).unwrap())
                .unwrap()
                .accounts_exchange,
            None
        );
        for (field, value) in [
            ("scope_digest", json!("short")),
            ("local_actor", json!("short")),
            ("control_observation", json!("short")),
            ("inventory_fingerprint", json!("short")),
            ("inventory_count", json!(crate::replica::MAX_RECORDS + 1)),
            ("sampled_at_ms", json!(0)),
        ] {
            let mut invalid: Value = aterm_json::from_slice(&wire).unwrap();
            invalid
                .as_object_mut()
                .unwrap()
                .get_mut("accounts_exchange")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(field.into(), value);
            assert!(
                decode(&aterm_json::to_vec(&invalid).unwrap()).is_err(),
                "{field}"
            );
        }
    }
    #[test]
    fn journal_reopen_preserves_exchange_without_renewing_contact_or_sample() {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let at = Instant::now();
        let expected = exchange();
        let mut observer = Observer::open(&config, handle()).unwrap();
        observer.local(peer(1, 1), trust()).unwrap();
        observer.accounts_exchange(account_exchange::Candidate {
            observation: expected.clone(),
            sampled_at: at,
        });
        let mut replay = expected.clone();
        replay.sampled_at_ms = 99_000;
        observer.accounts_exchange(account_exchange::Candidate {
            observation: replay,
            sampled_at: at,
        });
        observer.observe(
            Visibility::Active,
            0,
            "observing",
            2000,
            at + Duration::from_secs(1),
        );
        assert_eq!(observer.state.accounts_exchange, Some(expected.clone()));
        assert!(
            observer.state.peers.is_empty(),
            "historical agreement is not a contact"
        );
        assert_eq!(
            read_history(&config).unwrap().accounts_exchange,
            Some(expected.clone())
        );
        drop(observer);
        let mut reopened = Observer::open(&config, handle()).unwrap();
        assert_eq!(reopened.accounts_exchange_sampled, None);
        reopened.observe(
            Visibility::Disabled,
            0,
            "disabled",
            50_000,
            at + Duration::from_secs(60),
        );
        assert_eq!(reopened.state.accounts_exchange, Some(expected));
        assert!(reopened.state.peers.is_empty());
    }
    #[test]
    fn unchanged_account_exchanges_publish_live_and_checkpoint_periodically() {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let at = Instant::now();
        let published = handle();
        let mut observer = Observer::open(&config, published.clone()).unwrap();
        observer.local(peer(1, 1), trust()).unwrap();
        observer.contact(peer(2, 1), 1000, at);
        observer.contact(peer(3, 1), 1000, at);
        let projection = |observer: &Observer| {
            private_file::read_private(&observer.dir, "projection.json", MAX_BYTES).unwrap()
        };
        let first = exchange();
        observer.accounts_exchange(account_exchange::Candidate {
            observation: first.clone(),
            sampled_at: at,
        });
        observer.observe(Visibility::Active, 0, "observing", 2000, at);
        let first_head = observer.log.as_ref().unwrap().stats().head;
        let first_projection = projection(&observer);
        assert_eq!(
            read_history(&config).unwrap().accounts_exchange,
            Some(first.clone())
        );

        let repeated = AccountsExchangeObservation {
            sampled_at_ms: 3000,
            ..first
        };
        observer.accounts_exchange(account_exchange::Candidate {
            observation: repeated.clone(),
            sampled_at: at + Duration::from_secs(1),
        });
        observer.observe(
            Visibility::Active,
            0,
            "observing",
            3000,
            at + Duration::from_secs(1),
        );
        assert_eq!(
            published.snapshot().accounts_exchange,
            Some(repeated.clone())
        );
        assert_eq!(observer.log.as_ref().unwrap().stats().head, first_head);
        assert_eq!(projection(&observer), first_projection);

        let alternate = AccountsExchangeObservation {
            peer: peer(3, 1),
            sampled_at_ms: 4000,
            ..repeated
        };
        observer.accounts_exchange(account_exchange::Candidate {
            observation: alternate.clone(),
            sampled_at: at + Duration::from_secs(2),
        });
        observer.observe(
            Visibility::Active,
            0,
            "observing",
            4000,
            at + Duration::from_secs(2),
        );
        assert_eq!(
            published.snapshot().accounts_exchange,
            Some(alternate.clone())
        );
        assert_eq!(
            observer.log.as_ref().unwrap().stats().head,
            first_head,
            "an alternate peer does not turn unchanged local evidence into a durable transition"
        );
        assert_eq!(projection(&observer), first_projection);

        observer.contact(peer(2, 1), 62_000, at + CHECKPOINT);
        observer.contact(peer(3, 1), 62_000, at + CHECKPOINT);
        observer.observe(Visibility::Active, 0, "observing", 62_000, at + CHECKPOINT);
        let periodic_head = observer.log.as_ref().unwrap().stats().head;
        assert!(periodic_head > first_head);
        assert_eq!(
            read_history(&config).unwrap().accounts_exchange,
            Some(alternate.clone())
        );
        let log = observer.log.as_ref().unwrap();
        let records = log.read_range(log.read_floor(), 1);
        assert_eq!(
            decode(&records[0].body).unwrap().accounts_exchange,
            Some(alternate.clone()),
            "the journal stores the latest original sample, not checkpoint time"
        );

        let mut inventory_changed = alternate;
        inventory_changed.inventory_fingerprint[0] ^= 1;
        inventory_changed.sampled_at_ms = 5000;
        observer.accounts_exchange(account_exchange::Candidate {
            observation: inventory_changed.clone(),
            sampled_at: at + CHECKPOINT + Duration::from_secs(1),
        });
        observer.observe(
            Visibility::Active,
            0,
            "observing",
            63_000,
            at + CHECKPOINT + Duration::from_secs(1),
        );
        let inventory_head = observer.log.as_ref().unwrap().stats().head;
        assert!(
            inventory_head > periodic_head,
            "changed retained IDs checkpoint immediately"
        );
        assert_eq!(
            read_history(&config).unwrap().accounts_exchange,
            Some(inventory_changed.clone())
        );

        let mut control_changed = inventory_changed;
        control_changed.control_observation[0] ^= 1;
        control_changed.sampled_at_ms = 6000;
        observer.accounts_exchange(account_exchange::Candidate {
            observation: control_changed.clone(),
            sampled_at: at + CHECKPOINT + Duration::from_secs(2),
        });
        observer.observe(
            Visibility::Active,
            0,
            "observing",
            64_000,
            at + CHECKPOINT + Duration::from_secs(2),
        );
        assert!(
            observer.log.as_ref().unwrap().stats().head > inventory_head,
            "changed control context checkpoints immediately"
        );
        assert_eq!(
            read_history(&config).unwrap().accounts_exchange,
            Some(control_changed)
        );
    }

    #[test]
    fn retained_endpoint_requires_exact_identity_and_never_refreshes_reachability() {
        let root = aterm_tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut observer = Observer::open(&root.path().join("config"), handle()).unwrap();
        let identity = peer(2, 1);
        let endpoint = AuthenticatedEndpoint {
            address: "192.0.2.3:41384".parse().unwrap(),
            observed_at_ms: 2000,
        };
        observer.endpoint(&identity, endpoint.clone());
        assert!(observer.state.peers.is_empty());
        observer.contact(identity.clone(), 1000, now);
        observer.endpoint(&peer(3, 1), endpoint.clone());
        observer.endpoint(&peer(2, 2), endpoint.clone());
        assert!(
            observer.state.peers[0]
                .last_authenticated_endpoint
                .is_none()
        );
        observer.endpoint(&identity, endpoint.clone());
        observer.observe(
            Visibility::Active,
            1,
            "old contact",
            20_000,
            now + Duration::from_secs(19),
        );
        assert_eq!(observer.state.peers[0].reachability, Reachability::LastSeen);
        assert_eq!(observer.state.peers[0].last_seen_ms, 1000);
        assert_eq!(
            observer.state.peers[0].last_authenticated_endpoint,
            Some(endpoint.clone())
        );
        let restored = decode(&encode(&observer.state, None).unwrap()).unwrap();
        assert_eq!(
            restored.peers[0].last_authenticated_endpoint,
            Some(endpoint)
        );
        assert_eq!(restored.peers[0].reachability, Reachability::Unknown);
        observer.contact(peer(2, 2), 21_000, now + Duration::from_secs(20));
        assert!(
            observer.state.peers[0]
                .last_authenticated_endpoint
                .is_none()
        );
    }

    #[test]
    fn old_version_one_history_has_no_endpoint_and_future_address_age_is_unknown() {
        let mut observation = reachable_publication(Instant::now()).observation;
        let wire = encode(&observation, None).unwrap();
        let mut old: Value = aterm_json::from_slice(&wire).unwrap();
        old.as_object_mut()
            .unwrap()
            .get_mut("peers")
            .unwrap()
            .as_array_mut()
            .unwrap()[0]
            .as_object_mut()
            .unwrap()
            .remove("last_authenticated_endpoint");
        let decoded = decode(&aterm_json::to_vec(&old).unwrap()).unwrap();
        assert!(decoded.peers[0].last_authenticated_endpoint.is_none());
        assert_eq!(decoded.peers[0].identity, observation.peers[0].identity);
        observation.peers[0].last_authenticated_endpoint = Some(AuthenticatedEndpoint {
            address: "[::1]:41384".parse().unwrap(),
            observed_at_ms: u64::MAX,
        });
        let encoded = encode(&observation, None).unwrap();
        assert_eq!(
            aterm_json::from_slice::<Value>(&encoded).unwrap()["version"],
            json!(1)
        );
        assert_eq!(
            decode(&encoded).unwrap().peers[0].last_authenticated_endpoint,
            observation.peers[0].last_authenticated_endpoint
        );
        assert_eq!(
            value(&observation)["peers"][0]["last_authenticated_endpoint"],
            json!({"address":"[::1]:41384","observed_at_ms":u64::MAX,"age_ms":null})
        );
    }
    #[test]
    fn history_restores_stable_slots_but_never_live_reachability() {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("aterm.toml");
        let now = Instant::now();
        let out = handle();
        let mut observer = Observer::open(&config, out.clone()).unwrap();
        observer.local(peer(1, 1), trust()).unwrap();
        observer.contact(peer(2, 1), 1000, now);
        observer.observe(Visibility::Active, 12, "observing", 1000, now);
        assert!(
            matches!(Observer::open(&config, handle()), Err(OpenFailure::Owned)),
            "a concurrent observer is ownership contention, not corrupt history"
        );
        assert_eq!(
            out.snapshot().peers[0].reachability,
            Reachability::Reachable
        );
        assert_eq!(observer.log.as_ref().unwrap().stats().retained_records, 1);
        let first_floor = observer.log.as_ref().unwrap().read_floor();
        let checkpoint_at = now + CHECKPOINT;
        observer.contact(peer(2, 1), 61_000, checkpoint_at);
        observer.observe(Visibility::Active, 12, "observing", 61_000, checkpoint_at);
        assert!(observer.log.as_ref().unwrap().read_floor() > first_floor);
        assert!(
            matches!(Observer::open(&config, handle()), Err(OpenFailure::Owned)),
            "compacting a checkpoint must retain exclusive observer ownership"
        );
        let read = read_history(&config).unwrap();
        assert_eq!(read.visibility, Visibility::Unavailable);
        assert_eq!(
            read.observer, None,
            "saved identity is not this reader's identity"
        );
        assert_eq!(read.peers.len(), 1, "address hints never become machines");
        assert_eq!(read.peers[0].reachability, Reachability::Unknown);
        assert_eq!(read.peers[0].last_checked_ms, None);
        assert_eq!(read.peers[0].last_seen_ms, 61_000);
        drop(observer);
        let restarted = handle();
        let mut reopened = Observer::open(&config, restarted.clone()).unwrap();
        assert_eq!(restarted.snapshot().observer, None);
        assert_eq!(
            reopened.state.observer,
            Some(peer(1, 1)),
            "retain the identity guard internally"
        );
        reopened.local(peer(1, 2), trust()).unwrap();
        assert_eq!(reopened.state.peers[0].reachability, Reachability::Unknown);
        reopened.contact(peer(2, 2), 62_000, checkpoint_at + Duration::from_secs(1));
        reopened.contact(peer(3, 1), 62_000, checkpoint_at + Duration::from_secs(1));
        reopened.observe(
            Visibility::Active,
            0,
            "observing",
            62_000,
            checkpoint_at + Duration::from_secs(1),
        );
        assert_eq!(restarted.snapshot().observer, Some(peer(1, 2)));
        assert_eq!(
            reopened
                .state
                .peers
                .iter()
                .map(|p| p.slot)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(reopened.state.peers[0].first_seen_ms, 1000);
        assert_eq!(reopened.state.peers[0].identity.incarnation, [2; 16]);
        reopened.unconfirm_local();
        reopened.publish();
        assert_eq!(restarted.snapshot().observer, None);
        assert_eq!(reopened.state.observer, Some(peer(1, 2)));
        assert_eq!(restarted.snapshot().peers.len(), 2);
        reopened.local(peer(1, 2), trust()).unwrap();
        assert!(
            reopened.local(peer(9, 1), trust()).is_err(),
            "another observer cannot inherit prior contacts"
        );
        reopened.publish();
        assert_eq!(restarted.snapshot().observer, None);
        let mut foreign = trust();
        foreign.network = [7; 16];
        assert!(
            reopened.local(peer(1, 1), foreign).is_err(),
            "another network cannot inherit identity slots"
        );
    }
    #[test]
    fn wall_time_jump_and_unavailable_observer_withdraw_current_claims() {
        let root = aterm_tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut observer = Observer::open(&root.path().join("config"), handle()).unwrap();
        observer.contact(peer(2, 1), 1000, now);
        observer.observe(Visibility::Active, 0, "ok", 1000, now);
        observer.observe(
            Visibility::Active,
            0,
            "ok",
            100_000,
            now + Duration::from_millis(100),
        );
        assert_eq!(
            observer.state.peers[0].reachability,
            Reachability::LastSeen,
            "sleep cannot preserve a stale green cell"
        );
        observer.contact(peer(2, 1), 100_001, now + Duration::from_secs(1));
        observer.observe(
            Visibility::Unavailable,
            0,
            "unavailable",
            100_001,
            now + Duration::from_secs(1),
        );
        assert_eq!(observer.state.peers[0].reachability, Reachability::Unknown);
        assert_eq!(
            observer.state.peers[0].samples.last().unwrap().state,
            Reachability::Unknown
        );
    }
    #[test]
    fn repeated_contacts_coalesce_and_history_is_bounded() {
        let root = aterm_tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut observer = Observer::open(&root.path().join("config"), handle()).unwrap();
        observer.contact(peer(2, 1), 1000, now);
        observer.observe(Visibility::Active, 0, "ok", 1000, now);
        let head = observer.log.as_ref().unwrap().stats().head;
        for tick in 1..10 {
            observer.contact(
                peer(2, 1),
                1000 + tick * 1000,
                now + Duration::from_secs(tick),
            );
            observer.observe(
                Visibility::Active,
                tick as usize,
                "ok",
                1000 + tick * 1000,
                now + Duration::from_secs(tick),
            );
        }
        assert_eq!(
            observer.log.as_ref().unwrap().stats().head,
            head,
            "packets and repeated success do not append per heartbeat"
        );
        for tick in 10..50 {
            observer.observe(
                Visibility::Unavailable,
                0,
                "unavailable",
                tick * 1000,
                now + Duration::from_secs(tick),
            );
            observer.contact(peer(2, 1), tick * 1000, now + Duration::from_secs(tick));
            observer.observe(
                Visibility::Active,
                0,
                "ok",
                tick * 1000,
                now + Duration::from_secs(tick),
            );
        }
        assert_eq!(observer.log.as_ref().unwrap().stats().retained_records, 1);
        assert_eq!(observer.state.peers[0].samples.len(), MAX_SAMPLES);
    }
    #[test]
    fn read_only_history_does_not_create_state() {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        assert_eq!(
            read_history(&config).unwrap().history,
            HistoryState::Unavailable
        );
        let observed = read_history(&config).unwrap();
        assert_eq!(observed.visibility, Visibility::Unavailable);
        assert!(!observed.detail.is_empty());
        assert!(!directory(&config).exists());
    }
    #[test]
    fn corrupt_projection_cannot_replace_authoritative_journal_history() {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let now = Instant::now();
        let mut observer = Observer::open(&config, handle()).unwrap();
        observer.contact(peer(2, 1), 1000, now);
        observer.observe(Visibility::Active, 0, "ok", 1000, now);
        drop(observer);
        private_file::write_private(&directory(&config), "projection.json", b"bad cache").unwrap();
        assert!(read_history(&config).is_err());
        let restored = Observer::open(&config, handle()).unwrap();
        assert_eq!(restored.state.peers[0].identity.machine_id, [2; 16]);
        drop(restored);
        let path = directory(&config).join("observations.astream");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] ^= 1;
        std::fs::write(path, bytes).unwrap();
        assert!(
            Observer::open(&config, handle()).is_err(),
            "cache never heals a corrupt journal"
        );
    }
    #[test]
    fn identities_are_bounded_without_reassigning_old_slots() {
        let root = aterm_tempfile::tempdir().unwrap();
        let now = Instant::now();
        let config = root.path().join("config");
        let mut observer = Observer::open(&config, handle()).unwrap();
        for n in 0..=MAX_PEERS {
            let mut identity = peer(0, 1);
            identity.machine_id[..2].copy_from_slice(&(n as u16).to_be_bytes());
            observer.contact(identity, 1000, now);
        }
        assert_eq!(observer.state.peers.len(), MAX_PEERS);
        assert!(observer.state.retention_limited);
        assert_eq!(observer.state.peers[0].slot, 1);
        assert_eq!(observer.state.peers[MAX_PEERS - 1].slot, MAX_PEERS as u64);
        observer.observe(Visibility::Active, 0, "ok", 1000, now);
        let head = observer.log.as_ref().unwrap().stats().head;
        let retained = observer.state.peers.clone();
        assert!(read_history(&config).unwrap().retention_limited);
        for elapsed in 1..=3 {
            let mut overflow = peer(0, 1);
            overflow.machine_id[..2].copy_from_slice(&(MAX_PEERS as u16).to_be_bytes());
            let at = now + Duration::from_secs(elapsed);
            observer.contact(overflow, 1000 + elapsed * 1000, at);
            observer.observe(Visibility::Active, 0, "ok", 1000 + elapsed * 1000, at);
            assert_eq!(observer.log.as_ref().unwrap().stats().head, head);
            assert_eq!(observer.state.peers, retained);
            assert!(observer.state.retention_limited);
        }
    }
    #[test]
    fn unavailable_storage_keeps_network_health_independent_without_rebuilding_history() {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let now = Instant::now();
        let mut durable = Observer::open(&config, handle()).unwrap();
        durable.contact(peer(2, 1), 1000, now);
        durable.observe(Visibility::Active, 0, "ok", 1000, now);
        drop(durable);
        let before = std::fs::read(directory(&config).join("observations.astream")).unwrap();
        let mut memory = Observer::unavailable(&config, handle());
        memory.contact(peer(3, 1), 2000, now);
        memory.observe(Visibility::Active, 3, "presence active", 2000, now);
        assert_eq!(memory.state.visibility, Visibility::Active);
        assert_eq!(memory.state.history, HistoryState::Unavailable);
        assert_eq!(memory.state.peers.len(), 1);
        assert_eq!(memory.state.peers[0].reachability, Reachability::Unknown);
        assert_eq!(memory.state.unverified_endpoints, 3);
        assert_eq!(
            std::fs::read(directory(&config).join("observations.astream")).unwrap(),
            before
        );
    }
}
