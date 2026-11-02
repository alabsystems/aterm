// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, volatile address hints. Tokens rotate within a transport lifetime,
//! not hashes of a public network or device identity. A hint grants nothing.

use crate::control::{Repair, Report};
use std::{
    collections::{BTreeMap, VecDeque},
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

/// Room for every member of a full Commons roster, so a steward can dial each one and hear
/// whether it is silent or which aterm it runs (C26).
const MAX_PEERS: usize = aterm_secrets::commons::roster::MAX_MEMBERS + 1;
/// Each cache can retain a full roster's worth of unverified hints independently.
pub(crate) const MAX_UNVERIFIED_ENDPOINTS: usize = 2 * MAX_PEERS;
const PER_IP: usize = 2;
const EXPIRES: Duration = Duration::from_secs(45);
const VERIFIED_EXPIRES: Duration = Duration::from_secs(600);
const WIRE_LEN: usize = 38;
/// Progress can take three extra turns before ordinary discovery gets another turn.
const FOLLOW_UP_BURST: usize = 3;

fn retry_delay(own: &[u8; 32], attempt: u64) -> Duration {
    let mut seed = own.to_vec();
    seed.extend_from_slice(&attempt.to_be_bytes());
    let hash = aterm_digest::Sha256::digest(&seed);
    let jitter = u64::from(u16::from_be_bytes([hash[0], hash[1]])) % 2000;
    Duration::from_millis(3000 + jitter)
}

fn follow_up_delay(own: &[u8; 32], attempt: u64) -> Duration {
    let mut seed = own.to_vec();
    seed.extend_from_slice(&attempt.to_be_bytes());
    let hash = aterm_digest::Sha256::digest(&seed);
    let jitter = u64::from(u16::from_be_bytes([hash[2], hash[3]])) % 300;
    Duration::from_millis(200 + jitter)
}

/// Brief contention keeps this many ordinary retries before failed rounds back off.
const QUICK_BUSY_RETRIES: u32 = 3;
/// The first wait in a failed endpoint's exponential backoff.
const BACKOFF_FIRST: Duration = Duration::from_secs(5);
/// The longest wait between dials of an endpoint that keeps not answering, or whose answer this
/// side keeps refusing.
const BACKOFF_MAX: Duration = Duration::from_secs(120);

/// The `streak`th wait in exponential backoff: [`BACKOFF_FIRST`] doubling to
/// [`BACKOFF_MAX`], plus up to 2 s of jitter.
fn backoff_delay(own: &[u8; 32], attempt: u64, streak: u32) -> Duration {
    let mut seed = own.to_vec();
    seed.extend_from_slice(&attempt.to_be_bytes());
    let hash = aterm_digest::Sha256::digest(&seed);
    let jitter = u64::from(u16::from_be_bytes([hash[4], hash[5]])) % 2000;
    let doubled = BACKOFF_FIRST.saturating_mul(1 << streak.saturating_sub(1).min(16));
    doubled.min(BACKOFF_MAX) + Duration::from_millis(jitter)
}

/// How a dialed round ended, which sets its endpoint's next dial ([`Hints::finish`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The peer authenticated and the round repaired; `transferred` when it moved records.
    Repaired { transferred: bool },
    /// The round ended unfinished after the peer authenticated, or after it took this side's hello
    /// and closed or went quiet before sending its own ([`Repair::unanswered`]): contention (a
    /// crossed dial on either side, a busy writer or a full peer cache on either side), a peer
    /// that closed mid-round, a peer that refuses this Mac or has not yet approved its enrollment
    /// (which the dialing side cannot tell from contention), or a local failure to start one.
    /// The first [`QUICK_BUSY_RETRIES`] failed rounds keep the ordinary interval;
    /// further failures back off until a control exchange completes.
    Busy,
    /// The round never reached the peer's answer: no connection, no TLS within the handshake's
    /// limit (a peer asleep, cut off, hung or saturated), a hello this side refused (its proof,
    /// witness or membership), or a round that panicked. Such a failure repeats until its cause
    /// changes, so failed rounds increase the wait ([`backoff_delay`]).
    Refused,
}
impl Outcome {
    /// What a finished round says of its endpoint. `success` is the worker's: the round
    /// completed, was not cancelled and authenticated its peer. `control` is
    /// the completed control exchange's durable acknowledgement report.
    pub(crate) fn of(success: bool, repair: &Repair, control: Option<Report>) -> Self {
        if success {
            Self::Repaired {
                transferred: control.is_some_and(|r| r.sent != 0 || r.received != 0)
                    || repair_transferred(repair),
            }
        } else if repair.authenticated_actor().is_some() || repair.unanswered() {
            Self::Busy
        } else {
            Self::Refused
        }
    }
}

fn repair_transferred(repair: &Repair) -> bool {
    matches!(repair.handoff(), Some(Ok(count)) if *count > 0)
        || repair.membership().is_some_and(|r| r.sent + r.received > 0)
        || repair.application().is_some_and(|r| {
            r.sent
                + r.received
                + r.keys_sent
                + r.keys_received
                + r.conflicts_sent
                + r.conflicts_received
                > 0
        })
}

#[derive(Debug)]
struct Peer {
    seen: Instant,
    next: Instant,
    verified: bool,
    busy: bool,
    failed: bool,
    /// A push hurried this peer while its round ran ([`Hints::hurry`]): dial it again as the round
    /// ends.
    hurried: bool,
    /// A post-push quiet round already used its immediate confirmation turn. Progress or a
    /// new push re-arms that turn; a peer that cannot take this table keeps ordinary spacing.
    quiet_push: bool,
    /// Consecutive incomplete or refused dials. Changing failure kind is not recovery.
    failures: u32,
    repair: Repair,
}

impl Peer {
    fn established(&self) -> bool {
        self.verified && !self.failed
    }
}

#[derive(Debug)]
pub(super) struct Hints {
    own: [u8; 32],
    previous: Option<[u8; 32]>,
    attempt: u64,
    peers: BTreeMap<SocketAddr, Peer>,
    presence: BTreeMap<[u8; 32], (IpAddr, Instant)>,
    cursor: Option<SocketAddr>,
    /// A push runs ([`Self::hurry`] until [`Self::calm`]).
    hurrying: bool,
    established_cursor: Option<SocketAddr>,
    follow_up: VecDeque<SocketAddr>,
    follow_up_left: usize,
}
impl Hints {
    pub(super) fn new(own: [u8; 32]) -> Self {
        Self {
            own,
            previous: None,
            attempt: 0,
            peers: BTreeMap::new(),
            presence: BTreeMap::new(),
            cursor: None,
            hurrying: false,
            established_cursor: None,
            follow_up: VecDeque::new(),
            follow_up_left: FOLLOW_UP_BURST,
        }
    }
    pub(super) fn rotate(&mut self, own: [u8; 32]) {
        self.previous = Some(self.own);
        self.own = own;
    }
    pub(super) fn wire(&self, port: u16) -> Vec<u8> {
        let mut wire = Vec::with_capacity(WIRE_LEN);
        wire.extend_from_slice(b"ACH2");
        wire.extend_from_slice(&self.own);
        wire.extend_from_slice(&port.to_be_bytes());
        wire
    }
    pub(super) fn presence_wire(&self) -> Vec<u8> {
        let mut wire = b"ACH3".to_vec();
        wire.extend_from_slice(&self.own);
        wire
    }
    pub(super) fn observe_presence(&mut self, wire: &[u8], source: SocketAddr, now: Instant) {
        self.presence
            .retain(|_, (_, seen)| now.saturating_duration_since(*seen) < EXPIRES);
        if wire.len() != 36 || &wire[..4] != b"ACH3" {
            return;
        }
        let token: [u8; 32] = wire[4..].try_into().unwrap();
        if token == self.own || self.previous == Some(token) {
            return;
        }
        let IpAddr::V4(ip) = source.ip() else {
            return;
        };
        if ip.octets()[0] == 0 || ip.octets()[0] >= 224 || ip.is_broadcast() {
            return;
        }
        if let Some((address, seen)) = self.presence.get_mut(&token) {
            if *address == source.ip() {
                *seen = now;
            }
            return;
        }
        if self.presence.len() >= MAX_PEERS
            || self
                .presence
                .values()
                .filter(|(ip, _)| *ip == source.ip())
                .count()
                >= PER_IP
        {
            return;
        }
        self.presence.insert(token, (source.ip(), now));
    }
    fn expire(&mut self, now: Instant) {
        self.peers.retain(|_, p| {
            p.busy
                || (!p.verified && !p.failed)
                || now.saturating_duration_since(p.seen)
                    < if p.verified {
                        VERIFIED_EXPIRES
                    } else {
                        EXPIRES
                    }
        });
    }
    fn replacement(&self, source: Option<IpAddr>) -> Option<SocketAddr> {
        self.peers
            .iter()
            .filter(|(address, peer)| {
                source.is_none_or(|source| address.ip() == source)
                    && peer.failed
                    && !peer.busy
                    && (source.is_some() || !peer.verified)
            })
            // A new port on the same IP can replace a failed old listener.
            // Unrelated IPs cannot evict an authenticated peer with beacons.
            .min_by_key(|(_, peer)| (peer.verified, peer.seen))
            .map(|(address, _)| *address)
    }
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDiscoveryReplacement",
            action = "Observe",
            project = "aterm_db::runtime::discovery::admission_tests::replacement_transition"
        )
    )]
    pub(super) fn observe(&mut self, wire: &[u8], source: SocketAddr, now: Instant) {
        if wire.len() != WIRE_LEN
            || &wire[..4] != b"ACH2"
            || wire[4..36] == self.own
            || self
                .previous
                .is_some_and(|previous| wire[4..36] == previous)
        {
            return;
        }
        let std::net::IpAddr::V4(ip) = source.ip() else {
            return;
        };
        if ip.octets()[0] == 0 || ip.octets()[0] >= 224 || ip.is_broadcast() {
            return;
        }
        let port = u16::from_be_bytes(wire[36..].try_into().unwrap());
        if port == 0 {
            return;
        }
        let address = SocketAddr::from((ip, port));
        self.expire(now);
        // Repeated untrusted claims cannot pin either an untried or failed
        // endpoint forever. Only a successful authenticated round renews it.
        if self.peers.contains_key(&address) {
            return;
        }
        if self.peers.keys().filter(|p| p.ip() == address.ip()).count() >= PER_IP {
            let Some(replace) = self.replacement(Some(address.ip())) else {
                return;
            };
            self.peers.remove(&replace);
        }
        if self.peers.len() == MAX_PEERS {
            let Some(oldest) = self.replacement(None) else {
                return;
            };
            self.peers.remove(&oldest);
        }
        self.peers.insert(
            address,
            Peer {
                seen: now,
                next: now,
                verified: false,
                busy: false,
                failed: false,
                hurried: false,
                quiet_push: false,
                failures: 0,
                repair: Repair::default(),
            },
        );
    }
    pub(super) fn count(&self) -> usize {
        self.peers.len()
    }
    pub(super) fn unverified_count(&self) -> usize {
        self.peers.values().filter(|peer| !peer.verified).count() + self.presence.len()
    }
    pub(super) fn expire_listen_only(&mut self, now: Instant) {
        self.peers
            .retain(|_, peer| now.saturating_duration_since(peer.seen) < EXPIRES);
    }
    pub(super) fn expire_presence(&mut self, now: Instant) {
        self.presence
            .retain(|_, (_, seen)| now.saturating_duration_since(*seen) < EXPIRES);
    }
    /// A completed authenticated round establishes an endpoint until a later
    /// attempt fails. An announcement alone never reserves worker capacity.
    pub(super) fn established(&self, address: SocketAddr) -> bool {
        self.peers.get(&address).is_some_and(Peer::established)
    }
    pub(super) fn has_established(&self) -> bool {
        self.peers.values().any(Peer::established)
    }
    #[cfg(test)]
    pub(super) fn next(&mut self, now: Instant) -> Option<(SocketAddr, Repair)> {
        self.next_allowed(now, true)
    }
    /// Keep a separate position when a reserved worker can serve established
    /// peers only. Those turns must not rewind the ordinary discovery scan.
    pub(super) fn next_allowed(
        &mut self,
        now: Instant,
        allow_discovery: bool,
    ) -> Option<(SocketAddr, Repair)> {
        self.expire(now);
        self.follow_up
            .retain(|address| self.peers.contains_key(address));
        let eligible = |p: &Peer| !p.busy && p.next <= now && (allow_discovery || p.established());
        let follow_up = if self.follow_up_left > 0 {
            self.follow_up
                .iter()
                .copied()
                .find(|address| self.peers.get(address).is_some_and(eligible))
        } else {
            None
        };
        // Keep capacity when every productive peer is still in its short
        // jittered wait. A ready queued peer can use it before then; an
        // ordinary slow dial could otherwise take it for a whole round.
        // A push wants every hurried peer dialled now ([`Self::hurry`]).
        if !self.hurrying
            && self.follow_up_left > 0
            && follow_up.is_none()
            && self.follow_up.iter().any(|address| {
                self.peers
                    .get(address)
                    .is_some_and(|p| !p.busy && (allow_discovery || p.established()))
            })
        {
            return None;
        }
        let cursor = if allow_discovery {
            self.cursor
        } else {
            self.established_cursor
        };
        let address = follow_up.or_else(|| {
            self.peers
                .iter()
                .filter(|(_, p)| eligible(p))
                .filter(|(a, _)| cursor.is_none_or(|cursor| **a > cursor))
                .chain(
                    self.peers
                        .iter()
                        .filter(|(_, p)| eligible(p))
                        .filter(|(a, _)| cursor.is_some_and(|cursor| **a <= cursor)),
                )
                .map(|(a, _)| *a)
                .next()
        })?;
        let peer = self.peers.get_mut(&address).unwrap();
        peer.busy = true;
        // Change jitter on every attempt: fixed per-owner jitter can keep two
        // peers with nearby deadlines colliding at the runtime's tick cadence.
        self.attempt = self.attempt.wrapping_add(1);
        peer.next = now + retry_delay(&self.own, self.attempt);
        if follow_up.is_some() {
            self.follow_up_left -= 1;
        } else {
            // A preferred transfer must not rewind the ordinary scan and keep
            // selecting the same neighbor after every burst.
            if allow_discovery {
                self.cursor = Some(address);
            } else {
                self.established_cursor = Some(address);
            }
            self.follow_up_left = FOLLOW_UP_BURST;
        }
        self.follow_up.retain(|queued| *queued != address);
        Some((address, std::mem::take(&mut peer.repair)))
    }
    /// A push's hurry: dial every verified peer now, one whose round runs now again as that round
    /// ends. Until [`Self::calm`], progress and the first quiet confirmation are followed at
    /// once, so a table first advertised in one round can be taken in the next. Further quiet
    /// rounds keep ordinary spacing. How many peers it hurried; none leaves nothing hurried.
    pub(super) fn hurry(&mut self, now: Instant) -> usize {
        let mut hurried = 0;
        for peer in self.peers.values_mut().filter(|peer| peer.verified) {
            peer.quiet_push = false;
            if peer.busy {
                peer.hurried = true;
            } else {
                peer.next = now;
            }
            hurried += 1;
        }
        self.hurrying = hurried > 0;
        hurried
    }
    /// The push ended: each peer's rounds keep their ordinary spacing again.
    pub(super) fn calm(&mut self) {
        self.hurrying = false;
        for peer in self.peers.values_mut() {
            peer.hurried = false;
            peer.quiet_push = false;
        }
    }
    pub(super) fn finish(
        &mut self,
        address: SocketAddr,
        repair: Repair,
        outcome: Outcome,
        now: Instant,
    ) {
        let Some(peer) = self.peers.get_mut(&address) else {
            return;
        };
        // A slow exchange may have outlived the delay chosen at its start.
        // Start the quiet interval at completion so paired owners cannot keep
        // immediately redialing and colliding on their local custody locks;
        // when this round transferred new membership or application records, use
        // a short jittered follow-up so newly discovered tables sync promptly.
        // An endpoint that keeps not answering (or whose answer this side keeps
        // refusing) waits longer each time, so it cannot hold an outgoing worker
        // every few seconds.
        peer.failures = match outcome {
            Outcome::Repaired { .. } => 0,
            Outcome::Refused | Outcome::Busy => peer.failures.saturating_add(1),
        };
        let delay = match outcome {
            Outcome::Repaired { transferred: true } => follow_up_delay(&self.own, self.attempt),
            Outcome::Repaired { transferred: false } => retry_delay(&self.own, self.attempt),
            Outcome::Busy if peer.failures <= QUICK_BUSY_RETRIES => {
                retry_delay(&self.own, self.attempt)
            }
            Outcome::Busy => {
                backoff_delay(&self.own, self.attempt, peer.failures - QUICK_BUSY_RETRIES)
            }
            Outcome::Refused => backoff_delay(&self.own, self.attempt, peer.failures),
        };
        let success = matches!(outcome, Outcome::Repaired { .. });
        let transferred = matches!(outcome, Outcome::Repaired { transferred: true });
        let hurried = std::mem::take(&mut peer.hurried);
        let follow_push = self.hurrying && success && (transferred || !peer.quiet_push);
        if self.hurrying && success && !hurried {
            // An old round that was in flight when the push began cannot use the new push's
            // confirmation turn. A transferred page re-arms it; repeated silence does not.
            peer.quiet_push = !transferred;
        }
        peer.next = if hurried || follow_push {
            now
        } else {
            now + delay
        };
        if success && repair.actor().is_some() {
            peer.verified = true;
            peer.seen = now;
        }
        peer.busy = false;
        peer.failed = !success;
        peer.repair = repair;
        self.follow_up
            .retain(|queued| self.peers.contains_key(queued));
        if matches!(outcome, Outcome::Repaired { transferred: true }) {
            // A later productive completion joins the queue; it must not erase
            // another peer's pending work. Requeue a continuing transfer after
            // the other peers while keeping the existing discovery burst cap.
            if !self.follow_up.contains(&address) {
                self.follow_up.push_back(address);
            }
        } else {
            self.follow_up.retain(|queued| *queued != address);
        }
    }
}

#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDiscoveryReplacement",
        action = "AnotherLeft",
        reason = "Model bookkeeping: choose the first endpoint's completed or active state"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDiscoveryReplacement",
        action = "AnotherRight",
        reason = "Model bookkeeping: choose the second endpoint's completed or active state"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDiscoveryReplacement",
        action = "GlobalLimit",
        reason = "Model bookkeeping: choose separate or simultaneous per-IP and total capacity limits"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDiscoveryReplacement",
        action = "RightOlder",
        reason = "Model bookkeeping: choose which endpoint was last confirmed earlier"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDiscoveryReplacement",
        action = "Choose",
        reason = "Model bookkeeping: freeze the native endpoint fixture before Observe"
    )
)]
#[doc(hidden)]
pub fn db_discovery_replacement_environment() {}

#[cfg(test)]
mod admission_tests;

#[cfg(test)]
mod tests {
    use super::*;
    fn hints() -> Hints {
        Hints::new([4; 32])
    }
    fn wire(port: u16) -> Vec<u8> {
        Hints::new([5; 32]).wire(port)
    }
    #[test]
    fn untrusted_hints_have_size_source_and_capacity_bounds() {
        let mut hints = hints();
        let now = Instant::now();
        let mut bad = wire(1234);
        bad.push(0);
        hints.observe(&bad, "10.1.1.2:1".parse().unwrap(), now);
        hints.observe(&hints.wire(1234), "10.1.1.2:1".parse().unwrap(), now);
        assert_eq!(hints.count(), 0);
        for n in 0..MAX_PEERS + 36 {
            let ip = [10, 1, 2 + (n / 200) as u8, 1 + (n % 200) as u8];
            hints.observe(&wire(1234), SocketAddr::from((ip, 999)), now);
        }
        assert_eq!(hints.count(), MAX_PEERS);
        const { assert!(MAX_PEERS > aterm_secrets::commons::roster::MAX_MEMBERS) };
        // Every queued candidate gets a turn even when a slow handshake would
        // put it past the failed-candidate expiry before it was tried.
        hints.expire(now + EXPIRES);
        assert_eq!(hints.count(), MAX_PEERS);
        while let Some((address, repair)) = hints.next(now) {
            hints.finish(address, repair, Outcome::Refused, now);
        }
        hints.expire(now + EXPIRES);
        assert_eq!(hints.count(), 0);
    }

    #[test]
    fn producer_hint_capacity_remains_readable_over_the_local_observer_relay() {
        let mut hints = hints();
        let now = Instant::now();
        for index in 0..MAX_PEERS {
            let index = u32::try_from(index).unwrap();
            let mut token = [5; 32];
            token[..4].copy_from_slice(&index.to_be_bytes());
            let remote = Hints::new(token);
            hints.observe(
                &remote.wire(1234),
                SocketAddr::from((std::net::Ipv4Addr::from(0x0a01_0001 + index), 1)),
                now,
            );
            hints.observe_presence(
                &remote.presence_wire(),
                SocketAddr::from((std::net::Ipv4Addr::from(0x0a02_0001 + index), 1)),
                now,
            );
        }
        let count = hints.unverified_count();
        assert_eq!(count, MAX_UNVERIFIED_ENDPOINTS);
        let root = aterm_tempfile::Builder::new()
            .prefix("hint-relay-")
            .tempdir_in("/tmp")
            .unwrap();
        let config = root.path().join("aterm.toml");
        let handle = crate::network_state::ObservationHandle::default();
        let mut owner = crate::network_state::Observer::open(&config, handle.clone()).unwrap();
        owner.observe(
            aterm_types::network_state::Visibility::Active,
            0,
            "discovery hints",
            aterm_time::unix_millis().unwrap(),
            Instant::now(),
        );
        let control = crate::network_state::read_live_observation(&config)
            .expect("the same owner must serve a zero-count control");
        assert_eq!(control.via_local_process, Some(std::process::id()));
        assert_eq!(control.unverified_endpoints, 0);
        owner.observe(
            aterm_types::network_state::Visibility::Active,
            count,
            "discovery hints",
            aterm_time::unix_millis().unwrap(),
            Instant::now(),
        );
        let relayed = crate::network_state::read_live_observation(&config)
            .expect("a producer's full hint count must remain readable");
        assert_eq!(relayed.via_local_process, Some(std::process::id()));
        assert_eq!(relayed.unverified_endpoints, count);
        assert!(
            relayed.peers.is_empty(),
            "hints manufactured trusted machines"
        );
        drop(owner);
    }
    #[test]
    fn reserved_turns_preserve_the_ordinary_discovery_position() {
        let mut hints = hints();
        let now = Instant::now();
        let addresses: Vec<_> = (1..=4)
            .map(|n| SocketAddr::from(([10, 1, 1, n], 1234)))
            .collect();
        for address in &addresses {
            hints.observe(&wire(address.port()), *address, now);
        }
        for address in &addresses[2..] {
            hints.peers.get_mut(address).unwrap().verified = true;
        }
        let (first, repair) = hints.next(now).unwrap();
        assert_eq!(first, addresses[0]);
        let (third, _) = hints.next_allowed(now, false).unwrap();
        let (fourth, _) = hints.next_allowed(now, false).unwrap();
        assert_eq!((third, fourth), (addresses[2], addresses[3]));
        hints.finish(first, repair, Outcome::Refused, now);
        let (next, _) = hints.next(now + Duration::from_secs(10)).unwrap();
        assert_eq!(
            next, addresses[1],
            "reserved established turns must not keep returning discovery to its first candidate"
        );
    }

    #[test]
    fn failed_established_peers_release_their_reserved_capacity() {
        let mut hints = hints();
        let now = Instant::now();
        let address = "10.1.1.2:1234".parse().unwrap();
        hints.observe(&wire(1234), address, now);
        assert!(!hints.has_established());
        assert!(hints.next_allowed(now, false).is_none());
        hints.peers.get_mut(&address).unwrap().verified = true;
        assert!(hints.has_established());
        let (selected, repair) = hints.next_allowed(now, false).unwrap();
        assert_eq!(selected, address);
        hints.finish(selected, repair, Outcome::Refused, now);
        hints.observe(&wire(1234), address, now);
        assert!(!hints.has_established());
        assert!(
            hints
                .next_allowed(now + Duration::from_secs(10), false)
                .is_none()
        );
        assert_eq!(
            hints.next(now + Duration::from_secs(10)).unwrap().0,
            address
        );
    }

    #[test]
    fn beacons_cannot_reset_backoff_or_duplicate_an_active_dial() {
        let mut hints = hints();
        let now = Instant::now();
        hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
        let (address, repair) = hints.next(now).unwrap();
        hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
        assert!(hints.next(now + Duration::from_secs(10)).is_none());
        hints.finish(address, repair, Outcome::Refused, now);
        assert!(hints.next(now).is_none());
        assert!(hints.next(now + Duration::from_secs(10)).is_some());
        for port in [2000_u16, 2001, 2002] {
            hints.observe(&wire(port), "10.1.1.2:1".parse().unwrap(), now);
        }
        assert_eq!(hints.count(), PER_IP);
    }

    /// A push dials every verified peer at once, and one whose round runs as it begins again as
    /// that round ends; an unverified candidate keeps its own turn. A first quiet success gets
    /// an immediate confirmation while the push runs; once calm, rounds keep their quiet
    /// interval again.
    ///
    /// NEGATIVE CONTROL: make `Hints::hurry` count the verified peers without moving their next
    /// dial, and nothing is due at once.
    #[test]
    fn a_push_dials_every_verified_peer_at_once_and_a_busy_one_as_its_round_ends() {
        let mut hints = hints();
        let now = Instant::now();
        for source in ["10.1.1.2:1", "10.1.1.3:1", "10.1.1.4:1"] {
            hints.observe(&wire(1234), source.parse().unwrap(), now);
        }
        let [idle, busy, cold]: [SocketAddr; 3] =
            ["10.1.1.2:1234", "10.1.1.3:1234", "10.1.1.4:1234"]
                .map(|address| address.parse().unwrap());
        for (address, verified, running) in [
            (idle, true, false),
            (busy, true, true),
            (cold, false, false),
        ] {
            let peer = hints.peers.get_mut(&address).unwrap();
            peer.verified = verified;
            peer.busy = running;
            peer.next = now + Duration::from_secs(30);
        }
        assert!(hints.next(now).is_none());

        assert_eq!(hints.hurry(now), 2, "the verified peers, busy or not");
        let (first, repair) = hints
            .next(now)
            .expect("the idle verified peer is due at once");
        assert_eq!(first, idle);
        assert!(
            hints.next(now).is_none(),
            "the busy one waits for its round; the cold one for its turn"
        );

        // The round that ran as the push began ends, failed: its peer is dialled again at once.
        let ended = now + Duration::from_secs(1);
        hints.finish(busy, Repair::default(), Outcome::Busy, ended);
        assert_eq!(hints.next(ended).map(|(address, _)| address), Some(busy));

        // While the push runs, a round that succeeded is followed at once.
        hints.finish(
            first,
            repair,
            Outcome::Repaired { transferred: false },
            ended,
        );
        let (again, repair) = hints.next(ended).expect("followed at once");
        assert_eq!(again, idle);

        // Calm, a success keeps its quiet interval.
        hints.calm();
        hints.finish(
            again,
            repair,
            Outcome::Repaired { transferred: false },
            ended,
        );
        assert!(hints.next(ended).is_none());
    }

    #[test]
    fn a_quiet_push_bounds_followups_but_a_transferring_push_stays_fast() {
        let mut counts = Vec::new();
        for transferred in [false, true] {
            let mut hints = hints();
            let now = Instant::now();
            hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
            hints
                .peers
                .values_mut()
                .for_each(|peer| peer.verified = true);
            assert_eq!(hints.hurry(now), 1);
            let mut dials = 0;
            // Ten virtual seconds at the runtime's 100 ms tick. The peer keeps
            // answering but, in the quiet case, cannot take the pushed table.
            for tick in 0..100 {
                let at = now + Duration::from_millis(100 * tick);
                if let Some((address, repair)) = hints.next(at) {
                    hints.finish(address, repair, Outcome::Repaired { transferred }, at);
                    dials += 1;
                }
            }
            counts.push(dials);
        }
        println!(
            "ten virtual seconds: quiet={}, transferring={}",
            counts[0], counts[1]
        );
        assert!(counts[0] <= 5, "a quiet push kept redialing: {counts:?}");
        assert_eq!(counts[1], 100, "productive pushes keep making progress");
    }

    #[test]
    fn quiet_push_confirmation_starts_after_the_old_round_and_rearms_on_progress() {
        let mut hints = hints();
        let mut now = Instant::now();
        hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
        hints
            .peers
            .values_mut()
            .for_each(|peer| peer.verified = true);
        let (address, old_round) = hints.next(now).unwrap();
        hints.hurry(now);
        hints.finish(
            address,
            old_round,
            Outcome::Repaired { transferred: false },
            now,
        );
        for _ in 0..2 {
            let (next, repair) = hints
                .next(now)
                .expect("the post-push round and confirmation");
            assert_eq!(next, address);
            hints.finish(next, repair, Outcome::Repaired { transferred: false }, now);
        }
        assert!(hints.next(now).is_none(), "quiet confirmation is bounded");
        now = hints.peers[&address].next;
        let (next, repair) = hints.next(now).unwrap();
        hints.finish(next, repair, Outcome::Repaired { transferred: true }, now);
        for _ in 0..2 {
            let (next, repair) = hints.next(now).expect("progress re-arms confirmation");
            hints.finish(next, repair, Outcome::Repaired { transferred: false }, now);
        }
        assert!(hints.next(now).is_none());
        hints.hurry(now);
        for _ in 0..2 {
            let (next, repair) = hints.next(now).expect("a new push has a new confirmation");
            hints.finish(next, repair, Outcome::Repaired { transferred: false }, now);
        }
        assert!(hints.next(now).is_none());
    }

    #[test]
    fn a_slow_round_still_has_a_quiet_interval_after_completion() {
        let mut hints = hints();
        let now = Instant::now();
        let source = "10.1.1.2:1".parse().unwrap();
        hints.observe(&wire(1234), source, now);
        let (address, repair) = hints.next(now).unwrap();
        let finished = now + Duration::from_secs(30);
        hints.finish(address, repair, Outcome::Refused, finished);
        hints.observe(&wire(1234), source, finished);
        assert!(hints.next(finished).is_none());
        assert!(hints.next(finished + Duration::from_secs(2)).is_none());
        assert!(hints.next(finished + Duration::from_secs(10)).is_some());
    }

    #[test]
    fn a_productive_peer_keeps_capacity_during_its_short_wait() {
        let mut hints = hints();
        let now = Instant::now();
        let addresses: Vec<SocketAddr> = (1..=3)
            .map(|n| SocketAddr::from(([10, 1, 1, n], 1234)))
            .collect();
        for address in &addresses {
            hints.observe(&wire(address.port()), *address, now);
        }
        // One slow dial remains busy while the other outgoing slot completes
        // a productive round. The third peer must not take that last slot.
        let (slow, _) = hints.next(now).unwrap();
        assert_eq!(slow, addresses[0]);
        let (productive, repair) = hints.next(now).unwrap();
        assert_eq!(productive, addresses[1]);
        hints.finish(
            productive,
            repair,
            Outcome::Repaired { transferred: true },
            now,
        );
        let due = hints.peers[&productive].next;
        assert!(hints.next(now).is_none());
        assert!(hints.next(now + (due - now) / 2).is_none());
        let (address, repair) = hints.next(due).unwrap();
        assert_eq!(address, productive);
        hints.finish(
            address,
            repair,
            Outcome::Repaired { transferred: false },
            due,
        );
        assert_eq!(hints.next(due).unwrap().0, addresses[2]);
    }

    #[test]
    fn a_ready_follow_up_can_pass_a_waiting_peer_without_losing_either_turn() {
        // This reproducible token makes a fast repeated exchange ready before
        // a peer that finished earlier. Use the production retry jitter.
        let mut hints = Hints::new([17; 32]);
        let now = Instant::now();
        let addresses: Vec<SocketAddr> = (1..=3)
            .map(|n| SocketAddr::from(([10, 1, 1, n], 1234)))
            .collect();
        for address in &addresses {
            hints.observe(&wire(address.port()), *address, now);
        }
        let (first, first_repair) = hints.next(now).unwrap();
        let (second, second_repair) = hints.next(now).unwrap();
        hints.finish(
            first,
            first_repair,
            Outcome::Repaired { transferred: true },
            now,
        );
        assert!(hints.next(now).is_none());
        let resume = now + Duration::from_millis(500);
        hints.finish(
            second,
            second_repair,
            Outcome::Repaired { transferred: true },
            resume,
        );
        let (address, repair) = hints.next(resume).unwrap();
        assert_eq!(address, first);
        hints.finish(
            address,
            repair,
            Outcome::Repaired { transferred: true },
            resume + Duration::from_millis(100),
        );
        // Both writers completed through the normal path. On ordinary 100 ms
        // ticks, the later queued retry becomes eligible one tick earlier.
        assert!(hints.next(resume + Duration::from_millis(300)).is_none());
        let earlier = resume + Duration::from_millis(400);
        let later = resume + Duration::from_millis(500);
        let (address, repair) = hints
            .next(earlier)
            .expect("a ready queued peer must not wait for another peer's deadline");
        assert_eq!(address, first);
        hints.finish(
            address,
            repair,
            Outcome::Repaired { transferred: false },
            earlier,
        );
        assert!(
            hints.next(earlier).is_none(),
            "ordinary discovery must still reserve the waiting peer's capacity"
        );
        let (address, repair) = hints.next(later).unwrap();
        assert_eq!(address, second, "the waiting peer kept its queued turn");
        hints.finish(
            address,
            repair,
            Outcome::Repaired { transferred: false },
            later,
        );
        assert_eq!(hints.next(later).unwrap().0, addresses[2]);
    }

    #[test]
    fn progress_gets_a_bounded_burst_without_starving_the_ordinary_scan() {
        let mut hints = hints();
        let mut at = Instant::now();
        let addresses: Vec<SocketAddr> = (1..=4)
            .map(|n| SocketAddr::from(([10, 1, 1, n], 1234)))
            .collect();
        for address in &addresses {
            hints.observe(&wire(address.port()), *address, at);
        }
        let (first, repair) = hints.next(at).unwrap();
        assert_eq!(first, addresses[0]);
        hints.finish(first, repair, Outcome::Repaired { transferred: true }, at);
        for expected in &addresses[1..] {
            for _ in 0..FOLLOW_UP_BURST {
                at = hints.peers[&first].next;
                let (address, repair) = hints.next(at).unwrap();
                assert_eq!(address, first, "completed data gets its follow-up");
                hints.finish(address, repair, Outcome::Repaired { transferred: true }, at);
            }
            let (address, repair) = hints.next(at).unwrap();
            assert_eq!(
                address, *expected,
                "every ordinary candidate still gets its turn"
            );
            hints.finish(address, repair, Outcome::Busy, at);
        }
    }

    #[test]
    fn completed_peers_keep_their_follow_up_turns() {
        let mut hints = hints();
        let mut at = Instant::now();
        let addresses: Vec<SocketAddr> = (1..=4)
            .map(|n| SocketAddr::from(([10, 1, 1, n], 1234)))
            .collect();
        for address in &addresses {
            hints.observe(&wire(address.port()), *address, at);
        }
        let (first, first_repair) = hints.next(at).unwrap();
        let (second, second_repair) = hints.next(at).unwrap();
        assert_eq!((first, second), (addresses[0], addresses[1]));
        for (address, repair) in [(first, first_repair), (second, second_repair)] {
            hints.finish(address, repair, Outcome::Repaired { transferred: true }, at);
        }
        for (preferred, ordinary) in [
            ([first, second, first], addresses[2]),
            ([second, first, second], addresses[3]),
        ] {
            for expected in preferred {
                at = hints.peers.values().map(|peer| peer.next).max().unwrap();
                let (address, repair) = hints.next(at).unwrap();
                assert_eq!(
                    address, expected,
                    "a later completion must not erase an earlier queued turn"
                );
                hints.finish(address, repair, Outcome::Repaired { transferred: true }, at);
            }
            let (address, repair) = hints.next(at).unwrap();
            assert_eq!(
                address, ordinary,
                "queued work must still yield to discovery"
            );
            hints.finish(
                address,
                repair,
                Outcome::Repaired { transferred: false },
                at,
            );
        }
    }

    #[test]
    fn a_failed_queued_peer_preserves_another_peers_follow_up() {
        let mut hints = hints();
        let mut at = Instant::now();
        for n in 1..=2 {
            hints.observe(&wire(1234), SocketAddr::from(([10, 1, 1, n], 1234)), at);
        }
        let (first, first_repair) = hints.next(at).unwrap();
        let (second, second_repair) = hints.next(at).unwrap();
        for (address, repair) in [(first, first_repair), (second, second_repair)] {
            hints.finish(address, repair, Outcome::Repaired { transferred: true }, at);
        }
        at = hints.peers.values().map(|peer| peer.next).max().unwrap();
        let (address, repair) = hints.next(at).unwrap();
        assert_eq!(address, first);
        hints.finish(address, repair, Outcome::Refused, at);
        at = hints.peers[&second].next.max(at);
        let (address, repair) = hints.next(at).unwrap();
        assert_eq!(
            address, second,
            "one failure must not discard another peer's work"
        );
        hints.finish(
            address,
            repair,
            Outcome::Repaired { transferred: false },
            at,
        );
        assert!(hints.next(at).is_none());
    }

    #[test]
    fn a_failed_follow_up_returns_to_ordinary_discovery() {
        let mut hints = hints();
        let mut at = Instant::now();
        for n in 1..=2 {
            hints.observe(&wire(1234), SocketAddr::from(([10, 1, 1, n], 1234)), at);
        }
        let (first, repair) = hints.next(at).unwrap();
        hints.finish(first, repair, Outcome::Repaired { transferred: true }, at);
        at = hints.peers[&first].next;
        let (address, repair) = hints.next(at).unwrap();
        assert_eq!(address, first);
        hints.finish(address, repair, Outcome::Refused, at);
        assert_ne!(hints.next(at).unwrap().0, first);
    }

    #[test]
    fn failed_retries_do_not_keep_a_vanished_unverified_endpoint_alive() {
        let mut hints = hints();
        let now = Instant::now();
        hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
        let mut tries = 0;
        for seconds in 0..45 {
            let tried = now + Duration::from_secs(seconds);
            if let Some((address, repair)) = hints.next(tried) {
                tries += 1;
                hints.finish(address, repair, Outcome::Refused, tried);
            }
        }
        // 0 s, then 5, 10 and 20 s (each plus up to 2 s) after each failure.
        assert!(tries >= 3, "the candidate kept {tries} turns");
        assert!(
            hints.next(now + Duration::from_secs(46)).is_none(),
            "connection failures kept an endpoint with no new announcement alive"
        );
        assert_eq!(hints.count(), 0);
    }

    #[test]
    fn an_untried_candidate_gets_one_round_even_after_its_announcement_expires() {
        let mut hints = hints();
        let now = Instant::now();
        let source = "10.1.1.2:1".parse().unwrap();
        hints.observe(&wire(1234), source, now);
        let tried = now + Duration::from_secs(60);
        let (address, repair) = hints.next(tried).expect("untried candidate kept its turn");
        hints.expire(tried);
        assert_eq!(hints.count(), 1, "an active round must remain tracked");
        hints.finish(address, repair, Outcome::Refused, tried);
        assert!(hints.next(tried + Duration::from_secs(10)).is_none());

        // A new announcement grants a fresh attempt after the stale slot has expired.
        hints.observe(&wire(1234), source, tried + Duration::from_secs(11));
        assert!(hints.next(tried + Duration::from_secs(11)).is_some());
    }

    /// Dials of one verified endpoint in ten virtual minutes at 100 ms ticks, each ending as
    /// `outcome` says for the dial's index.
    fn dials_in_ten_minutes(mut outcome: impl FnMut(usize) -> Outcome) -> usize {
        let mut hints = hints();
        let now = Instant::now();
        hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
        hints
            .peers
            .values_mut()
            .for_each(|peer| peer.verified = true);
        let mut dials = 0;
        for tick in 0..6000 {
            let at = now + Duration::from_millis(100 * tick);
            if let Some((address, repair)) = hints.next(at) {
                hints.finish(address, repair, outcome(dials), at);
                dials += 1;
            }
        }
        dials
    }

    /// A verified endpoint that stopped answering (or whose proof this Mac refuses) is dialed at a
    /// doubling interval, up to two minutes, so a few dead peers cannot hold both outgoing
    /// workers every few seconds; brief contention keeps the ordinary interval, and one repaired round
    /// starts the doubling again. NEGATIVE CONTROL: with `Hints::finish` giving a refusal
    /// `retry_delay` as it did, the refused endpoint is dialed more than 100 times.
    #[test]
    fn refused_dials_back_off_to_two_minutes_and_a_repaired_round_resets_them() {
        let refused = dials_in_ten_minutes(|_| Outcome::Refused);
        assert!(refused <= 12, "{refused} refused dials in ten minutes");
        let mut hints = hints();
        let now = Instant::now();
        hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
        hints
            .peers
            .values_mut()
            .for_each(|peer| peer.verified = true);
        let mut at = now;
        let mut waits = Vec::new();
        for outcome in [
            Outcome::Refused,
            Outcome::Refused,
            Outcome::Refused,
            Outcome::Repaired { transferred: false },
            Outcome::Refused,
        ] {
            let (address, repair) = hints.next(at).unwrap();
            hints.finish(address, repair, outcome, at);
            let next = hints.peers[&address].next;
            waits.push(next - at);
            at = next;
        }
        // 5, 10 and 20 s, each plus under 2 s of jitter.
        let fives: Vec<_> = waits[..3].iter().map(|wait| wait.as_secs() / 5).collect();
        assert_eq!(fives, [1, 2, 4], "{waits:?}");
        assert!(waits[3] < Duration::from_secs(5), "{waits:?}");
        assert!(
            (BACKOFF_FIRST..BACKOFF_FIRST * 2).contains(&waits[4]),
            "a repaired round left the doubling at {waits:?}"
        );
    }

    #[test]
    fn persistent_incomplete_rounds_back_off_after_brief_contention() {
        let busy = dials_in_ten_minutes(|_| Outcome::Busy);
        assert!(busy <= 16, "{busy} incomplete dials in ten minutes");
        let mixed = dials_in_ten_minutes(|attempt| {
            if attempt % 2 == 0 {
                Outcome::Busy
            } else {
                Outcome::Refused
            }
        });
        assert!(mixed <= 16, "{mixed} mixed failed dials in ten minutes");
        println!("ten virtual minutes: incomplete={busy}, mixed={mixed}");

        let mut hints = hints();
        let mut at = Instant::now();
        hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), at);
        hints
            .peers
            .values_mut()
            .for_each(|peer| peer.verified = true);
        for attempt in 0..10 {
            let (address, repair) = hints.next(at).unwrap();
            hints.finish(address, repair, Outcome::Busy, at);
            let next = hints.peers[&address].next;
            let delay = next - at;
            if attempt < 3 {
                assert!(
                    (Duration::from_secs(3)..Duration::from_secs(5)).contains(&delay),
                    "{attempt}: {delay:?}"
                );
            } else {
                let seconds = (5_u64 << (attempt - 3)).min(120);
                assert!(
                    (Duration::from_secs(seconds)..Duration::from_secs(seconds + 2))
                        .contains(&delay),
                    "{attempt}: {delay:?}"
                );
            }
            at = next;
        }
        let (address, repair) = hints.next(at).unwrap();
        hints.finish(
            address,
            repair,
            Outcome::Repaired { transferred: false },
            at,
        );
        let next = hints.peers[&address].next;
        assert!(next - at < Duration::from_secs(5));
        at = next;
        let (address, repair) = hints.next(at).unwrap();
        hints.finish(address, repair, Outcome::Busy, at);
        assert!(
            hints.peers[&address].next - at < Duration::from_secs(5),
            "a completed round must restore fast recovery"
        );
    }

    #[test]
    fn verified_and_busy_entries_survive_untrusted_floods() {
        let mut hints = hints();
        let now = Instant::now();
        let source = "10.1.1.2:1".parse().unwrap();
        hints.observe(&wire(1234), source, now);
        let (address, _) = hints.next(now).unwrap();
        for n in 1..=100 {
            hints.observe(&wire(1234), SocketAddr::from(([10, 1, 2, n], 1)), now);
        }
        assert!(hints.peers.contains_key(&address));
        let peer = hints.peers.get_mut(&address).unwrap();
        peer.busy = false;
        peer.verified = true;
        let later = now + Duration::from_secs(30);
        hints.observe(&wire(1234), source, later);
        assert_eq!(hints.peers[&address].seen, now);
        hints.expire(now + VERIFIED_EXPIRES);
        assert!(!hints.peers.contains_key(&address));
    }

    #[test]
    fn rotating_tokens_preserves_repair_progress_and_rejects_own_previous_beacon() {
        let mut hints = hints();
        let now = Instant::now();
        let old = hints.wire(1234);
        hints.observe(&wire(2000), "10.1.1.2:1".parse().unwrap(), now);
        let (address, repair) = hints.next(now).unwrap();
        hints.rotate([6; 32]);
        assert_ne!(old, hints.wire(1234));
        hints.observe(&old, "10.1.1.3:1".parse().unwrap(), now);
        hints.observe(&hints.wire(1234), "10.1.1.3:1".parse().unwrap(), now);
        assert_eq!(hints.count(), 1);
        hints.finish(address, repair, Outcome::Refused, now);
        assert_eq!(
            hints.next(now + Duration::from_secs(10)).unwrap().0,
            address
        );
    }

    #[test]
    fn repeated_failed_ports_cannot_pin_a_source_and_untried_hints_keep_their_turn() {
        let mut hints = hints();
        let now = Instant::now();
        let source = "10.1.1.2:1".parse().unwrap();
        hints.observe(&wire(1), source, now);
        hints.observe(&wire(2), source, now);
        let (address, repair) = hints.next(now).unwrap();
        hints.finish(address, repair, Outcome::Refused, now);
        hints.observe(&wire(3), source, now);
        assert!(hints.peers.contains_key(&"10.1.1.2:3".parse().unwrap()));
        for n in 1..=100 {
            hints.observe(&wire(1234), SocketAddr::from(([10, 2, 3, n], 1)), now);
        }
        assert!(hints.peers.contains_key(&"10.1.1.2:3".parse().unwrap()));
        hints.observe(&wire(3), source, now + Duration::from_secs(40));
        assert_eq!(hints.peers[&"10.1.1.2:3".parse().unwrap()].seen, now);
    }
}
