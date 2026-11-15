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
/// The longest contact lease: no peer reads Reachable on a contact older than this, whatever
/// its contact was promised.
const LEASE_MAX: Duration = Duration::from_millis(aterm_types::network_state::LEASE_MAX_MS);
/// The most a contact keeps of its lease once what promised it no longer stands
/// ([`Observer::curtail`]): two minutes, in which a scheduler that starts over, or looks again
/// after this Mac's network changed, reaches each peer that is still there.
const CURTAILED: Duration = Duration::from_millis(aterm_types::network_state::CURTAILED_MS);

#[cfg(test)]
thread_local! {
    /// MUTANT of the lease, on for one test thread: a contact keeps its peer Reachable for
    /// [`REACHABLE`] whatever it was promised, so a peer contacted as promised, minutes apart,
    /// reads LastSeen between two of its contacts.
    static UNPROMISED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of [`Observer::left`], on for one test thread: a peer found gone keeps its lease.
    static LEFT_STANDS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of [`Observer::left`], on for one test thread: a later contact forgets that the
    /// peer was found gone, so one that authenticated before the departure and is observed
    /// after it reads Reachable.
    static LEFT_FORGOTTEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of [`Observer::curtail`], on for one test thread: it shortens no lease.
    static UNCURTAILED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of [`Observer::curtail`], on for one test thread: it counts from now whatever it
    /// was asked, so a contact from before this Mac's network changed keeps two more minutes.
    static CURTAILS_FROM_NOW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of [`Observer::curtail`], on for one test thread: it counts from the contact
    /// whatever it was asked, so a scheduler that starts over finds every peer it has not
    /// contacted for two minutes LastSeen before it had its two minutes to reach them.
    static CURTAILS_FROM_CONTACT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of [`Observer::contact`], on for one test thread: a contact made before this
    /// Mac's network changed and told after it keeps its whole promise, so a round that
    /// authenticated before a sleep and ended after it reads its peer Reachable for minutes.
    static LATE_PROMISED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn mutant(cell: &'static std::thread::LocalKey<std::cell::Cell<bool>>) -> bool {
    cell.with(std::cell::Cell::get)
}

/// A peer's latest authenticated contact on this process's monotonic clock, and how long it
/// keeps the peer Reachable.
///
/// Contacts come from completed repair rounds, and the transport's dial scheduler decides when
/// the next round with a peer is due, so the observer measures no pace. Each contact comes with
/// the scheduler's promise, the time within which that peer's next round is due
/// (`crate::runtime`'s transport), and the peer stays Reachable for that long: it reads LastSeen
/// only once a promised round did not come, and a peer contacted as promised makes no
/// reachability transition. Two things end a lease early. The transport found the peer gone
/// ([`Observer::left`]), or what made the promise no longer stands ([`Observer::curtail`]).
#[derive(Clone, Copy, Debug)]
struct Contact {
    at: Instant,
    /// How long after [`Self::at`] the peer reads Reachable: the promise, within [`REACHABLE`]
    /// and [`LEASE_MAX`], less what [`Observer::curtail`] took.
    lease: Duration,
    /// When the peer was last found gone ([`Observer::left`]): a contact older than this keeps
    /// nobody Reachable.
    left: Option<Instant>,
}

impl Contact {
    fn first(at: Instant, promise: Duration) -> Self {
        #[cfg(test)]
        let promise = if mutant(&UNPROMISED) {
            REACHABLE
        } else {
            promise
        };
        Self {
            at,
            lease: promise.clamp(REACHABLE, LEASE_MAX),
            left: None,
        }
    }

    /// This contact followed by one at `at` that was promised `promise`. A contact that is not
    /// later than this one changes nothing: rounds end out of the order they authenticated in,
    /// and the later authentication is the latest evidence. A later one takes its own promise
    /// whole, so what was curtailed of this one is not carried, and stands unless the peer was
    /// found gone after it.
    fn then(self, at: Instant, promise: Duration) -> Self {
        if at <= self.at {
            return self;
        }
        #[cfg(test)]
        if mutant(&LEFT_FORGOTTEN) {
            return Self::first(at, promise);
        }
        Self {
            left: self.left.filter(|left| at < *left),
            ..Self::first(at, promise)
        }
    }

    /// Whether the peer was found gone after this contact.
    fn ended(self) -> bool {
        #[cfg(test)]
        if mutant(&LEFT_STANDS) {
            return false;
        }
        self.left.is_some_and(|left| self.at < left)
    }

    /// How long this contact keeps its peer Reachable: its lease, and nothing once the peer was
    /// found gone after it.
    fn lease(self) -> Duration {
        if self.ended() {
            Duration::ZERO
        } else {
            self.lease
        }
    }
}

/// Where [`Observer::curtail`] counts a contact's remaining time from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Since {
    /// From the contact: this Mac's network changed or it woke, so a peer contacted before then
    /// may be on a network this Mac left; or the scheduler that made the promises ended with
    /// none to follow it at once, so nothing keeps them.
    Contact,
    /// From now: the scheduler that made the promises ended and another starts, which reaches
    /// every peer again from here.
    Now,
}

/// An observer's unbroken stretch of listening for its peers
/// ([`NetworkObservation::listening_since_ms`]): when it began on the wall clock, and the latest
/// observation in it on both clocks.
///
/// An observation continues the stretch only where it follows the one before within
/// [`REACHABLE`] on the wall clock and on the monotonic one. Anything longer is a break: the
/// publication expired in it, so no reader heard this observer, and it heard no peer (a serving
/// loop observes every few seconds while it runs). A sleep is such a break: macOS's `Instant`
/// (its uptime clock) stands still while the Mac sleeps and the wall clock runs on, so the
/// first observation after a wake follows the last before it by the sleep on the wall clock.
/// So is a stalled loop (both clocks run on), and a wall clock that steps back or ahead by more
/// than [`REACHABLE`], whose earlier stamps no longer measure time listened on it.
#[derive(Clone, Copy, Debug)]
struct Listening {
    since_ms: u64,
    at_ms: u64,
    at: Instant,
}

impl Listening {
    /// A stretch beginning with the observation at wall `at_ms` and monotonic `at`.
    fn first(at_ms: u64, at: Instant) -> Self {
        Self {
            since_ms: at_ms,
            at_ms,
            at,
        }
    }

    /// This stretch with the observation at wall `at_ms` and monotonic `at`: continued where it
    /// follows the latest within [`REACHABLE`] on both clocks, else begun again there.
    fn then(self, at_ms: u64, at: Instant) -> Self {
        let follows = at_ms
            .checked_sub(self.at_ms)
            .is_some_and(|gap| Duration::from_millis(gap) <= REACHABLE)
            && at
                .checked_duration_since(self.at)
                .is_some_and(|gap| gap <= REACHABLE);
        if follows {
            Self { at_ms, at, ..self }
        } else {
            Self::first(at_ms, at)
        }
    }
}

pub(crate) mod account_exchange;
mod relay;
mod repair;

pub(crate) use relay::{
    error_stage as live_error_stage, read_publication as read_live_publication,
};

/// The longest lease a contact may be promised and still be read by this Mac's other processes:
/// what the observation's local relay carries in the version it writes. A Reachable peer with a
/// longer one stops the relay answering at all.
pub(crate) const RELAYED_LEASE: Duration = relay::WRITTEN_LEASE;

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
                // The lease the producer gave the contact ([`Contact::lease`]), which stays on a
                // peer that still reads Reachable so a later reader ages it against the same
                // window.
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
            // An observation that is not current and Active says nothing of listening now.
            state.listening_since_ms = None;
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
        "last_catalog_repair":observation.last_catalog_repair.as_ref().map(repair::value),
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
            last_repair: repair::read(row, "last_repair")?,
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
        // A record from before a round over the catalog was kept names none.
        last_catalog_repair: repair::read(value, "last_catalog_repair")?,
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
    /// When this Mac's network last changed or it last woke ([`Self::curtail`] counting from the
    /// contact): a contact made by then is promised no more than [`CURTAILED`], however late
    /// its round ends and it is told here ([`Self::contact`]).
    moved: Option<Instant>,
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
    /// This run's current stretch of listening, as of its latest observation; none in a history
    /// just opened, which every run begins without, and while it does not listen.
    listening: Option<Listening>,
    accounts_exchange_sampled: Option<Instant>,
    repair_sampled: BTreeMap<[u8; 16], Instant>,
    /// When the kept round over the catalog was sampled
    /// ([`NetworkObservation::last_catalog_repair`]): one sampled before it does not replace it.
    catalog_sampled: Option<Instant>,
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
            state.last_catalog_repair = None;
        }
        let mut observer = Self {
            relay: None,
            relay_checked: Instant::now(),
            state,
            published,
            log: Some(log),
            dir,
            seen: BTreeMap::new(),
            moved: None,
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
            listening: None,
            accounts_exchange_sampled: None,
            repair_sampled: BTreeMap::new(),
            catalog_sampled: None,
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
            moved: None,
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
            listening: None,
            accounts_exchange_sampled: None,
            repair_sampled: BTreeMap::new(),
            catalog_sampled: None,
        };
        observer.publish();
        observer
    }
    /// End as a killed process's observer does: its relay gone and its journal released, with
    /// nothing written and the history left as its last checkpoint has it.
    #[cfg(any(test, debug_assertions))]
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

    /// Confirm this Mac's own Commons `identity` and `trust` context as the history's observer.
    /// A history retained under another trust context is refused, and so is one another machine
    /// observed, unless that machine is one of `succeeded`'s: the Commons machines of the writer
    /// generations this home succeeds on this disk, as its authenticated lineage proves them
    /// (`runtime::succeeded_machines`), asked only when the retained observer is another machine.
    /// Such a history is this Mac's own from before its writer succession, and this observer
    /// takes it up ([`Self::take_up`]); a refused one stays refused, its contacts ignored and its
    /// history unavailable, until a later confirmation proves it (the serving session tries
    /// again, `runtime::Confirmation`).
    ///
    /// # Errors
    /// The history has no journal, or it is refused.
    pub(crate) fn local(
        &mut self,
        identity: MachineIdentity,
        trust: aterm_secrets::commons::journal::Trust,
        succeeded: impl FnOnce() -> Vec<[u8; 16]>,
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
        if let Some(retained) = self
            .state
            .observer
            .as_ref()
            .map(|old| old.machine_id)
            .filter(|old| *old != identity.machine_id)
        {
            if !succeeded().contains(&retained) {
                self.accepting = false;
                self.seen.clear();
                self.state.history = HistoryState::Unavailable;
                return Err(Error::new(
                    "network observer identity differs from retained history",
                ));
            }
            self.take_up(&identity);
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
                // This installation's catalog is its own: no round of the one before speaks
                // for it.
                self.state.last_catalog_repair = None;
                self.repair_sampled.clear();
                self.catalog_sampled = None;
            }
            // Contacts until now were taken for another identity: listening for this one begins
            // with its first observation.
            self.listening = None;
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
    /// Take this history up for `identity`, the successor of the machine that observed it
    /// ([`Self::local`]). Its peers are this Mac's own evidence of whom it heard and when,
    /// whichever of its seats heard them, so each keeps its slot, its first and last contact and
    /// its samples: a peer this Mac goes on hearing is contacted into its slot again, one it no
    /// longer hears goes unheard from its last contact (as across a restart), and a reader that
    /// waits while a peer is known (the registry import's empty-view guard) still knows it. What
    /// the earlier seat's rounds produced as another Commons actor is dropped, as a reopen and a
    /// change of incarnation drop it: each peer's latest repair round, the lab's kept round over
    /// the catalog, the account exchange, the live contacts and their sampling. A peer that is
    /// `identity` itself leaves the list, since an observer never contacts itself. The revision
    /// goes on counting. [`Self::local`] then records the new identity, which the next
    /// observation checkpoints at once, as it does any change of this Mac's own identity.
    fn take_up(&mut self, identity: &MachineIdentity) {
        self.state
            .peers
            .retain(|peer| peer.identity.machine_id != identity.machine_id);
        for peer in &mut self.state.peers {
            peer.last_repair = None;
        }
        self.state.last_catalog_repair = None;
        self.catalog_sampled = None;
        self.state.accounts_exchange = None;
        self.seen.clear();
        self.repair_sampled.clear();
        self.accounts_exchange_sampled = None;
        self.dirty = true;
        self.changed = true;
    }
    /// Only callers resolving a completed authenticated exchange to a verified
    /// Commons binding may add an identity. Discovery addresses never call this.
    ///
    /// `promise` is the time within which the caller's dial scheduler promises this peer its
    /// next round, counted from `now`, the contact: the contact keeps the peer Reachable that
    /// long ([`Contact`]). Only an authenticated round makes a promise; nothing heard without
    /// one (a beacon, an address) reaches here. A contact made by the time this Mac's network
    /// last changed ([`Self::curtail`], counting from the contact) is promised what the
    /// contacts curtailed then kept, [`CURTAILED`] at most: rounds end, and are told here, out
    /// of the order they authenticated in.
    pub(crate) fn contact(
        &mut self,
        identity: MachineIdentity,
        at_ms: u64,
        now: Instant,
        promise: Duration,
    ) {
        if !self.accepting {
            return;
        }
        #[cfg(test)]
        let moved = self.moved.filter(|_| !mutant(&LATE_PROMISED));
        #[cfg(not(test))]
        let moved = self.moved;
        let promise = if moved.is_some_and(|moved| now <= moved) {
            promise.min(CURTAILED)
        } else {
            promise
        };
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
            if p.identity.incarnation != identity.incarnation {
                p.last_authenticated_endpoint = None;
                p.last_repair = None;
                self.repair_sampled.remove(&key);
                // What the installation before this one was promised, and its departure, say
                // nothing of this one.
                self.seen.remove(&key);
            }
            // A contact from before the peer was found gone is retained evidence of that
            // contact and no reachability: only one after it reads Reachable again.
            let stands = self
                .seen
                .get(&key)
                .is_none_or(|held| !held.then(now, promise).ended());
            let returns = stands && p.reachability != Reachability::Reachable;
            self.dirty |= p.identity != identity || returns;
            p.identity = identity;
            p.last_seen_ms = at_ms;
            p.last_checked_ms = Some(at_ms);
            if returns {
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
            .map_or(Contact::first(now, promise), |previous| {
                previous.then(now, promise)
            });
        self.seen.insert(key, contact);
        self.changed = true;
    }
    /// The transport found this exact peer gone, by a dial that began at `at`: a contact older
    /// than `at` keeps it Reachable no longer, whatever it was promised, and the next
    /// [`Self::observe`] reads it LastSeen. A contact at `at` or later stands, as does every
    /// other peer's. A peer with no contact in this observer's lifetime has no lease to end,
    /// and another installation of the machine is another peer.
    ///
    /// Only the transport's own evidence of a departure may call this: a dial of an endpoint
    /// the peer authenticated at that reached nobody, or the second in a row that failed
    /// (`crate::runtime`'s dial scheduler). It grants nothing: it sets no contact and no time
    /// anything else reads.
    pub(crate) fn left(&mut self, identity: &MachineIdentity, at: Instant) {
        if !self.accepting
            || !self.state.peers.iter().any(|peer| {
                peer.identity.machine_id == identity.machine_id
                    && peer.identity.incarnation == identity.incarnation
            })
        {
            return;
        }
        if let Some(contact) = self.seen.get_mut(&identity.machine_id) {
            contact.left = Some(contact.left.map_or(at, |left| left.max(at)));
        }
    }
    /// What promised the contacts made by `now` no longer stands, so each keeps at most
    /// [`CURTAILED`] of its lease, counted `since` the contact or now ([`Since`]). A lease is
    /// only ever shortened: a contact with less left keeps what it had, and one made after `now`
    /// keeps its promise. Counted from the contact, that holds for a contact made by `now` and
    /// told later too ([`Self::contact`]); counted from now, the scheduler that made the
    /// promises has ended and tells no more.
    pub(crate) fn curtail(&mut self, now: Instant, since: Since) {
        #[cfg(test)]
        let since = if mutant(&UNCURTAILED) {
            return;
        } else if mutant(&CURTAILS_FROM_NOW) {
            Since::Now
        } else if mutant(&CURTAILS_FROM_CONTACT) {
            Since::Contact
        } else {
            since
        };
        if since == Since::Contact {
            self.moved = Some(self.moved.map_or(now, |moved| moved.max(now)));
        }
        for contact in self.seen.values_mut() {
            let Some(age) = now.checked_duration_since(contact.at) else {
                continue;
            };
            let most = match since {
                Since::Contact => CURTAILED,
                Since::Now => age.saturating_add(CURTAILED),
            };
            contact.lease = contact.lease.min(most);
        }
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
        // It listens while it takes every contact as this Mac's confirmed observer with
        // discovery Active ([`Listening`]); a change of when it began is published at once, so
        // no reader judges an absence over a break.
        let listening =
            (available && self.local_verified && at_ms != 0).then(|| match self.listening {
                Some(listening) => listening.then(at_ms, now),
                None => Listening::first(at_ms, now),
            });
        if listening.map(|l| l.since_ms) != self.listening.map(|l| l.since_ms) {
            self.changed = true;
        }
        self.listening = listening;
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
        // When this run's listening began travels with the live observation, as the lease
        // below does, and is never part of `self.state`: a restored history has not listened.
        observation.listening_since_ms = self
            .listening
            .filter(|_| self.local_verified)
            .map(|listening| listening.since_ms);
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

/// This Mac's network observer as another crate's test drives it: opened on the history its
/// configuration names, confirmed as this Mac's own identity, told of contacts and observations
/// as the Commons runtime's serving loop tells it, and ended as a stopping runtime ends it
/// (dropped: its history closed) or as a killed process does ([`Self::kill`]: its history left
/// open). Its publication is read through the [`ObservationHandle`] it opened with, as the
/// runtime's readers read it (the account claims' presence,
/// `crates/aterm/src/account_claims_tests.rs`). No runtime path reaches it.
#[doc(hidden)]
#[cfg(any(test, debug_assertions))]
pub struct TestObserver(Observer);

#[cfg(any(test, debug_assertions))]
impl TestObserver {
    /// Open the history `config` names, publishing to `published`.
    ///
    /// # Errors
    /// Another observer owns the history, or it is invalid or unavailable.
    pub fn open(config: &Path, published: ObservationHandle) -> Result<Self> {
        Observer::open(config, published)
            .map(Self)
            .map_err(|_| bad())
    }

    /// Confirm `identity` in the `trust` context as this history's observer, succeeding no
    /// other machine.
    ///
    /// # Errors
    /// The history is refused.
    pub fn local(
        &mut self,
        identity: MachineIdentity,
        trust: aterm_secrets::commons::journal::Trust,
    ) -> Result<()> {
        self.0.local(identity, trust, Vec::new)
    }

    /// An authenticated contact of `identity` at wall `at_ms` and monotonic `now`, its next round
    /// promised within [`REACHABLE`], the shortest promise a contact keeps.
    pub fn contact(&mut self, identity: MachineIdentity, at_ms: u64, now: Instant) {
        self.0.contact(identity, at_ms, now, REACHABLE);
    }

    /// One observation, as the serving loop makes one each tick, at wall `at_ms` and monotonic
    /// `now`.
    pub fn observe(&mut self, visibility: Visibility, at_ms: u64, now: Instant) {
        self.0.observe(visibility, 0, "observing", at_ms, now);
    }

    /// End as a killed process's observer does: nothing written, the history left as its last
    /// checkpoint has it.
    pub fn kill(self) {
        self.0.die();
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
        "observer":observation.observer.as_ref().map(identity_value),
        "listening_since_ms":observation.listening_since_ms,"unverified_endpoints":observation.unverified_endpoints,
        "retention_limited":observation.retention_limited,"instances":Value::Null,
        "accounts_exchange":observation.accounts_exchange.as_ref().map(accounts_exchange_value),
        "last_catalog_repair":observation.last_catalog_repair.as_ref().map(repair::value),
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
