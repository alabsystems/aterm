// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, volatile address hints. Tokens rotate within a transport lifetime,
//! not hashes of a public network or device identity. A hint grants nothing.

use crate::control::Repair;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    time::{Duration, Instant},
};

const MAX_PEERS: usize = 64;
const PER_IP: usize = 2;
const EXPIRES: Duration = Duration::from_secs(45);
const VERIFIED_EXPIRES: Duration = Duration::from_secs(600);
const WIRE_LEN: usize = 38;

struct Peer {
    seen: Instant,
    next: Instant,
    verified: bool,
    busy: bool,
    failed: bool,
    repair: Repair,
}

pub(super) struct Hints {
    own: [u8; 32],
    previous: Option<[u8; 32]>,
    attempt: u64,
    peers: BTreeMap<SocketAddr, Peer>,
    cursor: Option<SocketAddr>,
}
impl Hints {
    pub(super) fn new(own: [u8; 32]) -> Self {
        Self {
            own,
            previous: None,
            attempt: 0,
            peers: BTreeMap::new(),
            cursor: None,
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
            let replace = self
                .peers
                .iter()
                .filter(|(a, p)| a.ip() == address.ip() && p.failed && !p.verified && !p.busy)
                .min_by_key(|(_, p)| p.seen)
                .map(|(a, _)| *a);
            let Some(replace) = replace else {
                return;
            };
            self.peers.remove(&replace);
        }
        if self.peers.len() == MAX_PEERS {
            let oldest = self
                .peers
                .iter()
                .filter(|(_, p)| p.failed && !p.verified && !p.busy)
                .min_by_key(|(_, p)| p.seen)
                .map(|(a, _)| *a);
            let Some(oldest) = oldest else {
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
                repair: Repair::default(),
            },
        );
    }
    pub(super) fn count(&self) -> usize {
        self.peers.len()
    }
    pub(super) fn next(&mut self, now: Instant) -> Option<(SocketAddr, Repair)> {
        self.expire(now);
        let eligible = |p: &&Peer| !p.busy && p.next <= now;
        let address = self
            .peers
            .iter()
            .filter(|(_, p)| eligible(p))
            .filter(|(a, _)| self.cursor.is_none_or(|cursor| **a > cursor))
            .chain(
                self.peers
                    .iter()
                    .filter(|(_, p)| eligible(p))
                    .filter(|(a, _)| self.cursor.is_some_and(|cursor| **a <= cursor)),
            )
            .map(|(a, _)| *a)
            .next()?;
        let peer = self.peers.get_mut(&address).unwrap();
        peer.busy = true;
        // Change jitter on every attempt: fixed per-owner jitter can keep two
        // peers with nearby deadlines colliding at the runtime's tick cadence.
        self.attempt = self.attempt.wrapping_add(1);
        let mut seed = self.own.to_vec();
        seed.extend_from_slice(&self.attempt.to_be_bytes());
        let hash = aterm_digest::Sha256::digest(&seed);
        let jitter = u64::from(u16::from_be_bytes([hash[0], hash[1]])) % 2000;
        peer.next = now + Duration::from_millis(3000 + jitter);
        self.cursor = Some(address);
        Some((address, std::mem::take(&mut peer.repair)))
    }
    pub(super) fn finish(
        &mut self,
        address: SocketAddr,
        repair: Repair,
        success: bool,
        now: Instant,
    ) {
        if let Some(peer) = self.peers.get_mut(&address) {
            if success && repair.actor().is_some() {
                peer.verified = true;
                peer.seen = now;
            }
            peer.busy = false;
            peer.failed = !success;
            if !success && !peer.verified {
                peer.seen = now;
            }
            peer.repair = repair;
        }
    }
}

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
        for n in 1..=100 {
            hints.observe(&wire(1234), SocketAddr::from(([10, 1, 2, n], 999)), now);
        }
        assert_eq!(hints.count(), MAX_PEERS);
        // Every queued candidate gets a turn even when a slow handshake would
        // put it past the failed-candidate expiry before it was tried.
        hints.expire(now + EXPIRES);
        assert_eq!(hints.count(), MAX_PEERS);
        while let Some((address, repair)) = hints.next(now) {
            hints.finish(address, repair, false, now);
        }
        hints.expire(now + EXPIRES);
        assert_eq!(hints.count(), 0);
    }
    #[test]
    fn beacons_cannot_reset_backoff_or_duplicate_an_active_dial() {
        let mut hints = hints();
        let now = Instant::now();
        hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
        let (address, repair) = hints.next(now).unwrap();
        hints.observe(&wire(1234), "10.1.1.2:1".parse().unwrap(), now);
        assert!(hints.next(now + Duration::from_secs(10)).is_none());
        hints.finish(address, repair, false, now);
        assert!(hints.next(now).is_none());
        assert!(hints.next(now + Duration::from_secs(10)).is_some());
        for port in [2000_u16, 2001, 2002] {
            hints.observe(&wire(port), "10.1.1.2:1".parse().unwrap(), now);
        }
        assert_eq!(hints.count(), PER_IP);
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
        hints.finish(address, repair, false, now);
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
        hints.finish(address, repair, false, now);
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
