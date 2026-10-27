// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded observer-local history. The astream checkpoint is authoritative;
//! the private JSON projection is a read-only cache for other local processes.

use crate::{Error, Result};
use astream_broker::{BrokerLog, Durability};
use astream_wire::Offset;
use aterm_json::{Value, json};
use aterm_types::network_state::{
    HistoryState, MachineIdentity, NetworkObservation, PeerObservation, PresenceSample,
    Reachability, Visibility,
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

/// Read-only shared data; readers cannot replace the observer's evidence.
#[derive(Clone, Default)]
pub struct ObservationHandle(Arc<Mutex<NetworkObservation>>);
impl ObservationHandle {
    /// Clone the latest immutable view. This reads no filesystem or network.
    pub fn snapshot(&self) -> NetworkObservation {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
    pub(crate) fn replace(&self, state: NetworkObservation) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = state;
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

fn encode(
    observation: &NetworkObservation,
    trust: Option<aterm_secrets::commons::journal::Trust>,
) -> Result<Vec<u8>> {
    let peers: Vec<_> = observation.peers.iter().map(|p| json!({
        "slot":p.slot,"identity":identity_value(&p.identity),"first":p.first_seen_ms,"last":p.last_seen_ms,
        "samples":p.samples.iter().map(|s| json!({"at":s.at_ms,"state":state_value(s.state)})).collect::<Vec<_>>()
    })).collect();
    let value = json!({"version":1,"revision":observation.revision,"observed":observation.observed_at_ms,
        "observer":observation.observer.as_ref().map(identity_value),"limited":observation.retention_limited,"peers":peers,
        "trust":trust.map(|t|json!({"network":aterm_codec::hex::encode_lower(&t.network),"authority":aterm_codec::hex::encode_lower(&t.authority),"genesis":aterm_codec::hex::encode_lower(&t.genesis)}))});
    let wire = aterm_json::to_vec(&value).map_err(|_| bad())?;
    if wire.len() > MAX_BYTES {
        return Err(bad());
    }
    Ok(wire)
}
fn decode(wire: &[u8]) -> Result<NetworkObservation> {
    if wire.len() > MAX_BYTES {
        return Err(bad());
    }
    let value: Value = aterm_json::from_slice(wire).map_err(|_| bad())?;
    if number(&value, "version")? != 1 {
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
    let mut peers = Vec::new();
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
    peers.sort_by_key(|p| p.slot);
    let observer = value
        .get("observer")
        .filter(|v| !v.is_null())
        .map(identity)
        .transpose()?;
    Ok(NetworkObservation {
        revision: number(&value, "revision")?,
        observed_at_ms: number(&value, "observed")?,
        observer,
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
fn decode_trust(wire: &[u8]) -> Result<Option<aterm_secrets::commons::journal::Trust>> {
    let value: Value = aterm_json::from_slice(wire).map_err(|_| bad())?;
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

/// Read the last published local history without opening a writer, creating a
/// directory, contacting peers or reading any secret payload. Replayed contacts
/// are always unknown, never current reachability or a current local identity.
pub fn read_history(config: &Path) -> Result<NetworkObservation> {
    match private_file::read_private(&directory(config), "projection.json", MAX_BYTES) {
        Ok(wire) => {
            let mut observation = decode(&wire)?;
            observation.observer = None;
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
}
impl Observer {
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
                if record.subject != SUBJECT {
                    return Err(OpenFailure::Unavailable);
                }
                state = decode(&record.body)?;
                trust = decode_trust(&record.body)?;
                offset = Offset(record.seq.0.checked_add(1).ok_or_else(bad)?);
            }
        }
        let mut observer = Self {
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
        };
        observer.publish();
        Ok(observer)
    }
    pub(crate) fn unavailable(config: &Path, published: ObservationHandle) -> Self {
        let mut state = read_history(config).unwrap_or_default();
        state.history = HistoryState::Unavailable;
        state.detail = "Network history unavailable; retained identities are not current".into();
        let mut observer = Self {
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
        };
        observer.publish();
        observer
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
            p.identity = identity;
            p.last_seen_ms = at_ms;
            p.last_checked_ms = Some(at_ms);
            if p.reachability != Reachability::Reachable {
                self.state.retention_limited |= p.samples.len() == MAX_SAMPLES;
                sample(p, at_ms, Reachability::Reachable);
            }
        } else {
            if self.state.peers.len() == MAX_PEERS {
                self.state.retention_limited = true;
                self.dirty = true;
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
    pub(crate) fn observe(
        &mut self,
        visibility: Visibility,
        unverified: usize,
        detail: &str,
        at_ms: u64,
        now: Instant,
    ) {
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
        self.state.detail = detail.into();
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
        self.published.replace(observation);
    }
}

fn sample(peer: &mut PeerObservation, at_ms: u64, state: Reachability) {
    peer.reachability = state;
    if peer.samples.len() == MAX_SAMPLES {
        peer.samples.remove(0);
    }
    peer.samples.push(PresenceSample { at_ms, state });
}

/// Safe metadata-only representation for a local read command.
pub fn value(observation: &NetworkObservation) -> Value {
    json!({"revision":observation.revision,"observed_at_ms":observation.observed_at_ms,
        "visibility":format!("{:?}",observation.visibility),"history":format!("{:?}",observation.history),"detail":observation.detail,
        "observer":observation.observer.as_ref().map(identity_value),"unverified_endpoints":observation.unverified_endpoints,
        "retention_limited":observation.retention_limited,"instances":Value::Null,
        "peers":observation.peers.iter().map(|p|json!({"slot":p.slot,"identity":identity_value(&p.identity),"first_seen_ms":p.first_seen_ms,
            "last_seen_ms":p.last_seen_ms,"reachability":state_value(p.reachability),"instances":Value::Null})).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let read = read_history(&config).unwrap();
        assert_eq!(
            read.observer, None,
            "saved identity is not this reader's identity"
        );
        assert_eq!(read.peers.len(), 1, "address hints never become machines");
        assert_eq!(read.peers[0].reachability, Reachability::Unknown);
        assert_eq!(read.peers[0].last_checked_ms, None);
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
        reopened.contact(peer(2, 2), 2000, now + Duration::from_secs(1));
        reopened.contact(peer(3, 1), 2000, now + Duration::from_secs(1));
        reopened.observe(
            Visibility::Active,
            0,
            "observing",
            2000,
            now + Duration::from_secs(1),
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
        let mut observer = Observer::open(&root.path().join("config"), handle()).unwrap();
        for n in 0..=MAX_PEERS {
            let mut identity = peer(0, 1);
            identity.machine_id[..2].copy_from_slice(&(n as u16).to_be_bytes());
            observer.contact(identity, 1000, now);
        }
        assert_eq!(observer.state.peers.len(), MAX_PEERS);
        assert!(observer.state.retention_limited);
        assert_eq!(observer.state.peers[0].slot, 1);
        assert_eq!(observer.state.peers[MAX_PEERS - 1].slot, MAX_PEERS as u64);
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
