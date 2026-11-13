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
/// How often the journal and its projection are written while nothing asks for it sooner:
/// continuing contacts, repair counts and the revision ride this checkpoint.
const CHECKPOINT: Duration = Duration::from_secs(60);
/// The least time between two checkpoints that a transition asks for. A transition (a presence
/// sample, a new peer or incarnation, an endpoint's address, a repair's stage or outcome, the
/// local account inventory) is published live at once and written with the next checkpoint, at
/// most this long after the previous one: however often peers change, the journal is appended
/// and the projection replaced at most once per `SETTLE`. An observer that is dropped writes
/// what is pending and closes its history ([`Observer::checkpoint`]); the runtime drops it when
/// it stops ([`crate::runtime::Runtime::shutdown`]). A process that ends without that (killed,
/// crashed, or left by `_exit` or `exec`) loses the transitions of at most this long (plus one
/// serving tick), each of which the next run observes again within one contact period, and
/// leaves its history open: a reader then knows it may be incomplete
/// ([`read_closed_history`]), and the next observer drops its retained repair rounds. This
/// Mac's own identity and trust context, which guard the history against another observer, do
/// not wait ([`Observer::local`]). Half of [`CHECKPOINT`], and of the minute a reader of the
/// projection waits for a transition to reach it.
const SETTLE: Duration = Duration::from_secs(30);
/// The journal is compacted to its latest checkpoint before an append would take it past this,
/// not after every append. With one record at most [`MAX_BYTES`], the journal never holds more
/// than twice that (and its framing), within the `3 * MAX_BYTES` an opening observer admits,
/// whenever the process stops.
const COMPACT: usize = MAX_BYTES;
/// How long a publication stays current, and the shortest contact lease ([`Contact::lease`]).
const REACHABLE: Duration = Duration::from_millis(aterm_types::network_state::REACHABLE_MS);
/// The longest contact lease: no peer reads Reachable on a contact older than this.
const LEASE_MAX: Duration = Duration::from_millis(aterm_types::network_state::LEASE_MAX_MS);

/// A peer's latest authenticated contact on this process's monotonic clock, and the contact
/// period measured from the ones before it.
///
/// Contacts come from completed repair rounds, and the transport makes a bounded number of
/// rounds at once, so one peer's period grows with the number of peers: in a lab of dozens it
/// is longer than [`REACHABLE`]. A fixed window then expires every healthy peer between two of
/// its scheduled contacts, and each one alternates Reachable and LastSeen for good. The lease is
/// measured instead: a peer stays Reachable while its next contact comes within twice its
/// measured period, so it reads LastSeen only once it has missed a whole scheduled contact.
///
/// A gap the lease in force did not cover is not a period by itself: the peer read LastSeen
/// in it, and it may have been away (asleep, off the network) and not merely slow. Such a gap
/// is held as a candidate and becomes the period only when the next gap is at least half of
/// it; a peer that returns to its pace keeps the period it had. A gap longer than
/// [`LEASE_MAX`] is an absence whatever follows it: no lease could have covered it, so it is
/// neither a candidate nor the confirmation of one, however many of them come in a row.
#[derive(Clone, Copy, Debug)]
struct Contact {
    at: Instant,
    /// `None` until a gap measures it.
    period: Option<Duration>,
    /// The latest gap, where the lease then in force did not cover it and the next gap has
    /// not yet confirmed or dismissed it.
    candidate: Option<Duration>,
}

impl Contact {
    fn first(at: Instant) -> Self {
        Self {
            at,
            period: None,
            candidate: None,
        }
    }

    /// This contact followed by one at `at`.
    ///
    /// A gap within the lease in force measures the period: a longer one becomes the period at
    /// once, a shorter one lowers it by a quarter of the difference, so a peer contacted at
    /// uneven intervals keeps the lease of its slower ones. A gap past the lease is a candidate
    /// and leaves the period as it is. The gap after a candidate confirms it when it is at
    /// least half of it (the peer is that slow: the longer of the two becomes the period),
    /// and otherwise dismisses it (the peer was away) and measures as any other gap. A gap
    /// past [`LEASE_MAX`] leaves the period as it is and holds no candidate, so the period is
    /// never past [`LEASE_MAX`], and one past half of it is not kept: its lease is the longest
    /// there is.
    fn then(self, at: Instant) -> Self {
        let Some(gap) = at
            .checked_duration_since(self.at)
            .filter(|gap| !gap.is_zero())
        else {
            return self;
        };
        let (period, candidate) = match self.candidate {
            // No lease covers a gap past the longest one, so such a gap measures no pace a
            // lease could keep: the peer was away.
            _ if gap > LEASE_MAX => (self.period, None),
            Some(candidate) if gap >= candidate / 2 => (Some(candidate.max(gap)), None),
            _ if gap > self.lease() => (self.period, Some(gap)),
            _ => (
                Some(match self.period {
                    Some(period) if gap < period => period - (period - gap) / 4,
                    _ => gap,
                }),
                None,
            ),
        };
        Self {
            at,
            period: period.map(|period| period.min(LEASE_MAX / 2)),
            candidate,
        }
    }

    /// How long this contact keeps its peer Reachable: twice the measured period, at least
    /// [`REACHABLE`] (also the lease of a peer whose period is not measured yet) and at most
    /// [`LEASE_MAX`].
    ///
    /// Invariant: a peer whose period was measured and whose every next contact comes within
    /// twice its measured period, up to [`LEASE_MAX`], makes no reachability transition. A peer
    /// slower than [`REACHABLE`] is measured by its third contact (its second gap confirms its
    /// first), so it reads LastSeen twice per run before that, and once more each time its pace
    /// slows past its lease. A peer that was away keeps the lease it had: after its last
    /// contact it reads LastSeen within twice the period measured before it left, however
    /// many contacts it made between absences longer than [`LEASE_MAX`].
    fn lease(self) -> Duration {
        self.period.map_or(REACHABLE, |period| {
            period.saturating_mul(2).clamp(REACHABLE, LEASE_MAX)
        })
    }
}

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
                        .is_some_and(|age| age <= aterm_types::network_state::REACHABLE_MS)
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
                // The producer's measured lease ([`Contact::lease`]), which stays on a peer
                // that still reads Reachable so a later reader ages it against the same window.
                let lease = Duration::from_millis(peer.reachable_for_ms());
                peer.reachability = match (
                    age(peer.last_seen_ms),
                    peer.last_checked_ms.and_then(age),
                    contact,
                ) {
                    (Some(seen), Some(checked), Some(contact))
                        if seen <= lease && checked <= lease && contact <= lease =>
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
        for peer in &mut state.peers {
            if peer.reachability != Reachability::Reachable {
                peer.lease_ms = None;
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
    aterm_uds::quiet_state::selected(&config.with_file_name("network-state"))
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
#[cfg(test)]
fn encode(
    observation: &NetworkObservation,
    trust: Option<aterm_secrets::commons::journal::Trust>,
) -> Result<Vec<u8>> {
    encode_record(observation, trust, false)
}
/// One journal record, also the projection's bytes. `closed` says its observer wrote it as it
/// stopped, so nothing it observed is missing from it ([`record_closed`]).
fn encode_record(
    observation: &NetworkObservation,
    trust: Option<aterm_secrets::commons::journal::Trust>,
    closed: bool,
) -> Result<Vec<u8>> {
    let mut value = encode_value(observation, trust);
    if let Some(record) = value.as_object_mut() {
        record.insert("closed".into(), Value::Bool(closed));
    }
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
            lease_ms: None,
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
/// Whether a record's observer wrote it as it stopped. A record that does not say so (one
/// written while its observer ran, or by an aterm from before records said) may lack up to
/// [`SETTLE`] of what that observer saw after it.
fn record_closed(value: &Value) -> bool {
    value.get("closed").and_then(Value::as_bool) == Some(true)
}
fn decode_record(
    wire: &[u8],
) -> Result<(
    NetworkObservation,
    Option<aterm_secrets::commons::journal::Trust>,
    bool,
)> {
    if wire.len() > MAX_BYTES {
        return Err(bad());
    }
    let value: Value = aterm_json::from_slice(wire).map_err(|_| bad())?;
    Ok((
        decode_value(&value)?,
        decode_trust_value(&value)?,
        record_closed(&value),
    ))
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

/// Whether a failed [`read_live_observation`] found no observer listening: its endpoint is
/// not there, or nothing accepts on it (the socket of a process that was killed). Any other
/// failure came from an observer that is running and did not answer this read: it refused a
/// publication that is no longer current, or the exchange ran out of time.
#[must_use]
pub fn no_live_observer(error: &std::io::Error) -> bool {
    relay::stale(error)
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

/// [`read_history`] where its observer closed it as it stopped, so nothing that observer saw
/// is missing from it; `None` where the history is open: its observer is running
/// (and its live observation is the one to read), or ended without closing it and may have seen
/// up to [`SETTLE`] more than it wrote. A history that does not exist is closed: nothing was
/// observed.
///
/// # Errors
///
/// The history is there and cannot be read.
pub fn read_closed_history(config: &Path) -> Result<Option<NetworkObservation>> {
    match private_file::read_private(&directory(config), "projection.json", MAX_BYTES) {
        Ok(wire) => {
            let value: Value = aterm_json::from_slice(&wire).map_err(|_| bad())?;
            if !record_closed(&value) {
                return Ok(None);
            }
        }
        Err(PrivateFileError::Missing) => {}
        Err(_) => return Err(bad()),
    }
    read_history(config).map(Some)
}

pub(crate) struct Observer {
    relay: Option<relay::Server>,
    relay_checked: Instant,
    state: NetworkObservation,
    published: ObservationHandle,
    log: Option<BrokerLog>,
    dir: PathBuf,
    seen: BTreeMap<[u8; 16], Contact>,
    /// When the journal was last written, or this observer opened.
    checkpoint: Instant,
    /// The journal's latest complete checkpoint, which every compaction keeps.
    latest: Option<Offset>,
    published_at: Instant,
    /// A transition waits for its checkpoint: the next one [`SETTLE`] allows, or this
    /// observer's drop.
    dirty: bool,
    /// The next observation checkpoints whatever [`SETTLE`] says: the first of a run, and a
    /// change of this Mac's own identity or trust context.
    urgent: bool,
    /// This run's latest checkpoint says the history is open, so its drop closes it.
    open: bool,
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
        let legacy = config.with_file_name("network-state");
        aterm_uds::quiet_state::prepare(&legacy, || {
            aterm_secrets::fsx::write_new(&legacy.join("observations.astream"), b"", 0o600)
                .map_err(std::io::Error::other)?;
            let (_, metadata) = private_file::open_private(&legacy, "observations.astream")
                .map_err(std::io::Error::other)?;
            if metadata.len() > (MAX_BYTES * 3) as u64 {
                return Err(std::io::Error::other("network history exceeds its bound"));
            }
            BrokerLog::open_opts(
                legacy.join("observations.astream"),
                aterm_journal::resident_options(Durability::Strict),
                || Ok(astream_broker::Prepared::Nothing),
            )
        })
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
        let dir = directory(config);
        private_file::create_private_dir(&dir)?;
        aterm_secrets::fsx::write_new(&dir.join("observations.astream"), b"", 0o600)
            .map_err(|_| bad())?;
        let (_, metadata) =
            private_file::open_private(&dir, "observations.astream").map_err(|_| bad())?;
        if metadata.len() > (MAX_BYTES * 3) as u64 {
            return Err(OpenFailure::Unavailable);
        }
        let log = BrokerLog::open_opts(
            dir.join("observations.astream"),
            aterm_journal::resident_options(Durability::Strict),
            || Ok(astream_broker::Prepared::Nothing),
        )
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
        let mut latest = None;
        let mut closed = true;
        loop {
            let records = log.read_range(offset, 16);
            if records.is_empty() {
                break;
            }
            for record in records {
                if record.seq != offset || record.subject != SUBJECT {
                    return Err(OpenFailure::Unavailable);
                }
                (state, trust, closed) = decode_record(&record.body)?;
                latest = Some(record.seq);
                offset = Offset(record.seq.0.checked_add(1).ok_or_else(bad)?);
            }
        }
        if log.stats().head != offset {
            return Err(OpenFailure::Unavailable);
        }
        if !closed {
            // The observer that wrote this ended without closing it: a repair round later
            // than the one retained for a peer may be missing, and a retained round must not
            // speak for the latest. Each is observed again within one contact period.
            for peer in &mut state.peers {
                peer.last_repair = None;
            }
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
            latest,
            published_at: Instant::now(),
            dirty: false,
            urgent: true,
            open: false,
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
            latest: None,
            published_at: Instant::now(),
            dirty: false,
            urgent: false,
            open: false,
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
    /// End as a killed process's observer does: its relay gone and its journal released, with
    /// nothing written and the history left as its last checkpoint has it.
    #[cfg(test)]
    pub(crate) fn die(mut self) {
        drop(self.relay.take());
        self.log = None;
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
        // progress: publish it live and checkpoint periodically. A changed local inventory or
        // authority context is a transition, written with the next checkpoint ([`SETTLE`])
        // without changing its original timestamps.
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
            self.urgent = true;
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
            self.urgent = true;
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
                if !self.state.retention_limited {
                    self.state.retention_limited = true;
                    self.dirty = true;
                    self.changed = true;
                }
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
                lease_ms: None,
            });
            self.dirty = true;
        }
        let contact = self
            .seen
            .get(&key)
            .map_or(Contact::first(now), |previous| previous.then(now));
        self.seen.insert(key, contact);
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
                .is_some_and(|contact| {
                    let lease = contact.lease();
                    now.saturating_duration_since(contact.at) < lease
                        && at_ms
                            .checked_sub(peer.last_seen_ms)
                            .is_some_and(|elapsed| Duration::from_millis(elapsed) < lease)
                })
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
        let since = now.saturating_duration_since(self.checkpoint);
        let checkpoint = self.urgent || since >= CHECKPOINT || (self.dirty && since >= SETTLE);
        if self.changed
            || checkpoint
            || now.saturating_duration_since(self.published_at) >= Duration::from_secs(2)
        {
            self.state.observed_at_ms = at_ms;
            self.state.revision = self.state.revision.saturating_add(1);
            if checkpoint {
                // One checkpoint carries every transition since the last one, and the contact
                // progress with them.
                self.checkpoint(false);
                self.checkpoint = now;
            }
            self.changed = false;
            self.published_at = now;
            self.publish();
        }
    }
    /// Write the whole state as one journal record, then replace the projection with the same
    /// bytes. The latest complete checkpoint survives every step: the journal is compacted to
    /// it only before an append that would pass [`COMPACT`], and the new record is durable
    /// before a later compaction drops that one. A failure leaves the history unavailable for
    /// this observer's lifetime.
    ///
    /// `closed` is false while this observer runs and true for the record its drop writes: an
    /// open record tells a reader, and the next observer, that transitions later than it may
    /// be missing ([`read_closed_history`]).
    fn checkpoint(&mut self, closed: bool) {
        let result = (|| -> Result<()> {
            if self.poisoned {
                return Err(bad());
            }
            let wire = encode_record(&self.state, self.trust, closed)?;
            let log = self.log.as_mut().ok_or_else(bad)?;
            if let Some(latest) = self.latest
                && log.stats().retained_bytes.saturating_add(wire.len()) > COMPACT
            {
                log.retain_before(latest)?;
            }
            let seq = log.stats().head.0.checked_add(1).ok_or_else(bad)?;
            let (offset, _) = log
                .publish_checked(1, seq, SUBJECT.into(), wire.clone())
                .map_err(|_| bad())?;
            self.latest = Some(offset);
            private_file::write_private(&self.dir, "projection.json", &wire)?;
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
            self.state.history = HistoryState::Unavailable;
            self.state.detail = "Network history checkpoint unavailable".into();
        }
        self.dirty = false;
        self.urgent = false;
        self.open = !closed;
    }
    fn publish(&mut self) {
        let mut observation = self.state.clone();
        if !self.local_verified {
            observation.observer = None;
        }
        // The lease each Reachable peer is read against travels with the live observation and
        // is never part of `self.state`, which the journal stores.
        for peer in &mut observation.peers {
            if peer.reachability == Reachability::Reachable {
                peer.lease_ms = self
                    .seen
                    .get(&peer.identity.machine_id)
                    .and_then(|contact| u64::try_from(contact.lease().as_millis()).ok());
            }
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
                        .and_then(|contact| self.published_at.checked_duration_since(contact.at))
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
        // A transition still waiting for its checkpoint is written before the journal closes,
        // in the record that closes a history this run left open. An observer that wrote
        // nothing and has nothing pending leaves the history as it found it. A process that
        // ends without dropping its observer leaves it open ([`SETTLE`]).
        if (self.dirty || self.open) && self.log.is_some() && !self.poisoned {
            self.checkpoint(true);
        }
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
#[path = "network_state_tests.rs"]
mod tests;
