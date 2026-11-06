// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Direct, one-hop member solicitation. A reply supplies a socket address, never
//! admission: the ordinary member/fleet handshake still proves the peer. Neither
//! a DNS lookup nor a Bonjour browse is involved in this path.

use aterm_net::{
    fleet::NodeKey,
    lan::{Multicast, Scope},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write as _},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::fd::AsFd as _,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 31, 85);
const PORT: u16 = 41385;
const MAGIC: &[u8; 8] = b"aterm-m1";
const NONCE: usize = 16;
const REQUEST: usize = MAGIC.len() + NONCE;
const RESPONSE: usize = REQUEST + 2 + NodeKey::LEN;
const MAX_LINKS: usize = 8;
const MAX_PEERS: usize = 64;
const REFRESH: Duration = Duration::from_secs(1);
const RETRANSMIT: Duration = Duration::from_millis(100);

/// An untrusted address and claimed public key, checked by the later handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hint {
    pub address: SocketAddr,
    pub key: NodeKey,
}

fn sources() -> Vec<Ipv4Addr> {
    source_addresses(
        aterm_uds::network::interfaces()
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.address),
    )
}

fn source_addresses(addresses: impl IntoIterator<Item = IpAddr>) -> Vec<Ipv4Addr> {
    addresses
        .into_iter()
        .filter_map(|address| match address {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn links(sources: &[Ipv4Addr], port: u16, scope: Scope) -> Vec<(Ipv4Addr, Multicast)> {
    sources
        .iter()
        .copied()
        .filter_map(|source| {
            Multicast::bind(source, GROUP, port, scope)
                .ok()
                .map(|s| (source, s))
        })
        .take(MAX_LINKS)
        .collect()
}

fn request(nonce: &[u8; NONCE]) -> [u8; REQUEST] {
    let mut wire = [0; REQUEST];
    wire[..MAGIC.len()].copy_from_slice(MAGIC);
    wire[MAGIC.len()..].copy_from_slice(nonce);
    wire
}

fn response(ask: &[u8], port: u16, key: NodeKey) -> Option<[u8; RESPONSE]> {
    if ask.len() != REQUEST || !ask.starts_with(MAGIC) || port == 0 {
        return None;
    }
    let mut wire = [0; RESPONSE];
    wire[..REQUEST].copy_from_slice(ask);
    wire[REQUEST..REQUEST + 2].copy_from_slice(&port.to_be_bytes());
    wire[REQUEST + 2..].copy_from_slice(key.bytes());
    Some(wire)
}

fn hint(wire: &[u8], source: SocketAddr, nonce: &[u8; NONCE]) -> Option<Hint> {
    if wire.len() != RESPONSE || wire[..REQUEST] != request(nonce) {
        return None;
    }
    let port = u16::from_be_bytes(wire[REQUEST..REQUEST + 2].try_into().ok()?);
    (port != 0).then(|| Hint {
        address: SocketAddr::new(source.ip(), port),
        key: NodeKey::from_bytes(
            wire[REQUEST + 2..]
                .try_into()
                .expect("fixed response length"),
        ),
    })
}

/// Ask now and return the first batch already received, without a settle window.
/// The caller authenticates these hints and may use another discovery source if
/// no current member answers. A lab's private service never reaches the live LAN.
pub(crate) fn probe(within: Duration) -> io::Result<Vec<Hint>> {
    if cfg!(test)
        || std::env::var_os(aterm_types::world::MARKER).is_some()
        || std::env::var_os("CI").is_some()
        || std::env::var_os(aterm_secrets::discover::LAB_MEMBER_SERVICE_ENV).is_some()
    {
        return Ok(Vec::new());
    }
    probe_on(&links(&sources(), PORT, Scope::Lan), within)
}

fn probe_on(links: &[(Ipv4Addr, Multicast)], within: Duration) -> io::Result<Vec<Hint>> {
    if links.is_empty() || within.is_zero() {
        return Ok(Vec::new());
    }
    let mut nonce = [0; NONCE];
    aterm_uds::rand::fill(&mut nonce)?;
    let wire = request(&nonce);
    let mut active = Vec::new();
    for (_, link) in links {
        if link.send(&wire).is_ok() {
            active.push(link);
        }
    }
    if active.is_empty() {
        return Ok(Vec::new());
    }
    let deadline = Instant::now() + within;
    let mut resend = Instant::now() + RETRANSMIT;
    let fds: Vec<_> = active.iter().map(|link| link.as_fd()).collect();
    loop {
        let ready = aterm_uds::ready::any_until(&fds, deadline.min(resend))?;
        let now = Instant::now();
        if now >= deadline {
            return Ok(Vec::new());
        }
        if now >= resend {
            for link in &active {
                let _ = link.send(&wire);
            }
            resend = now + RETRANSMIT;
        }
        let mut found = BTreeMap::new();
        for index in ready {
            // An untrusted flood has a fixed per-read budget.
            for _ in 0..MAX_PEERS {
                let Some((wire, source)) = active[index].receive()? else {
                    break;
                };
                if let Some(hint) = hint(&wire, source, &nonce) {
                    found.entry(hint.address).or_insert(hint);
                }
            }
        }
        if !found.is_empty() {
            return Ok(found.into_values().take(MAX_PEERS).collect());
        }
        if Instant::now() >= deadline {
            return Ok(Vec::new());
        }
    }
}

struct ReplyWindow {
    began: Instant,
    count: usize,
    sources: BTreeMap<IpAddr, usize>,
    last: BTreeMap<(IpAddr, [u8; NONCE]), Instant>,
}

impl ReplyWindow {
    fn new(now: Instant) -> Self {
        Self {
            began: now,
            count: 0,
            sources: BTreeMap::new(),
            last: BTreeMap::new(),
        }
    }

    fn admit(&mut self, source: IpAddr, ask: &[u8], now: Instant) -> bool {
        if now.saturating_duration_since(self.began) >= REFRESH {
            *self = Self::new(now);
        }
        let nonce: [u8; NONCE] = ask[MAGIC.len()..REQUEST]
            .try_into()
            .expect("validated request length");
        if self.count >= MAX_PEERS
            || self.sources.get(&source).copied().unwrap_or(0) >= 4
            || self
                .last
                .get(&(source, nonce))
                .is_some_and(|at| now.saturating_duration_since(*at) < RETRANSMIT)
        {
            return false;
        }
        self.count += 1;
        *self.sources.entry(source).or_default() += 1;
        self.last.insert((source, nonce), now);
        true
    }
}

/// The full member set for a person's phrase: its owner may answer after another
/// member. Automatic admission uses [`probe`] and continues only if proof fails.
pub(crate) fn members(within: Duration) -> aterm_secrets::Result<Vec<String>> {
    let began = Instant::now();
    let mut direct: Vec<_> = probe(within.min(Duration::from_millis(500)))
        .unwrap_or_default()
        .into_iter()
        .map(|hint| hint.address.to_string())
        .collect();
    match aterm_secrets::discover::browse_members(within.saturating_sub(began.elapsed())) {
        Ok(more) => {
            for address in more {
                if !direct.contains(&address) {
                    direct.push(address);
                }
            }
            Ok(direct)
        }
        Err(_) if !direct.is_empty() => Ok(direct),
        Err(error) => Err(error),
    }
}

/// Owned by the fleet listener's advertiser. Drop interrupts readiness and joins
/// the worker before the listener's ownership can be released.
pub(crate) struct Responder {
    pub port: u16,
    pub key: NodeKey,
    stop: Option<std::io::PipeWriter>,
    task: Option<JoinHandle<()>>,
}

struct Network {
    scope: Scope,
    port: u16,
    sources: Box<dyn Fn() -> Vec<Ipv4Addr> + Send>,
    #[cfg(test)]
    ready: Option<std::sync::mpsc::Sender<usize>>,
}

impl Responder {
    pub fn finished(&self) -> bool {
        self.task.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub fn start(
        port: u16,
        key: NodeKey,
        allowed: impl Fn() -> bool + Send + 'static,
    ) -> io::Result<Self> {
        Self::start_on(
            port,
            key,
            allowed,
            Network {
                scope: Scope::Lan,
                port: PORT,
                sources: Box::new(sources),
                #[cfg(test)]
                ready: None,
            },
        )
    }

    fn start_on(
        port: u16,
        key: NodeKey,
        allowed: impl Fn() -> bool + Send + 'static,
        network: Network,
    ) -> io::Result<Self> {
        let (reader, writer) = std::io::pipe()?;
        let task = thread::Builder::new()
            .name("aterm-member-lan".into())
            .spawn(move || serve(reader, port, key, allowed, network))?;
        Ok(Self {
            port,
            key,
            stop: Some(writer),
            task: Some(task),
        })
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        if let Some(mut stop) = self.stop.take() {
            let _ = stop.write_all(&[1]);
        }
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

fn serve(
    stop: std::io::PipeReader,
    port: u16,
    key: NodeKey,
    allowed: impl Fn() -> bool,
    network: Network,
) {
    #[cfg(test)]
    let mut network = network;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let routes = (network.scope == Scope::Lan)
        .then(aterm_uds::network::RouteChanges::open)
        .flatten();
    let mut sockets = Vec::new();
    let mut refresh = Instant::now();
    let mut replies = ReplyWindow::new(Instant::now());
    loop {
        let now = Instant::now();
        if now >= refresh {
            let current = (network.sources)();
            sockets.retain(|(source, link): &(Ipv4Addr, Multicast)| {
                current.contains(source) && link.recheck().is_ok()
            });
            if allowed() {
                for source in current {
                    if sockets.len() >= MAX_LINKS {
                        break;
                    }
                    if !sockets.iter().any(|(held, _)| *held == source) {
                        sockets.extend(links(&[source], network.port, network.scope));
                    }
                }
            } else {
                sockets.clear();
            }
            refresh = now + REFRESH;
            #[cfg(test)]
            if let Some(ready) = network.ready.take() {
                let _ = ready.send(sockets.len());
            }
        }
        let mut fds: Vec<_> = sockets.iter().map(|(_, link)| link.as_fd()).collect();
        fds.push(stop.as_fd());
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(routes) = &routes {
            fds.push(routes.as_fd());
        }
        let Ok(ready) = aterm_uds::ready::any_until(&fds, refresh) else {
            return;
        };
        if ready.contains(&sockets.len()) {
            return;
        }
        for index in ready {
            if index > sockets.len() {
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                if routes
                    .as_ref()
                    .is_some_and(|routes| routes.wait(Duration::ZERO))
                {
                    refresh = Instant::now();
                }
                continue;
            }
            for _ in 0..MAX_PEERS {
                let Ok(Some((wire, source))) = sockets[index].1.receive() else {
                    break;
                };
                let Some(reply) = response(&wire, port, key) else {
                    continue;
                };
                // Bounded replies allow concurrent local joiners and lost-packet
                // retransmissions. Every answer goes to the fixed one-hop group.
                if !replies.admit(source.ip(), &wire, Instant::now()) || !allowed() {
                    continue;
                }
                let _ = sockets[index].1.send(&reply);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_net::fleet::{FleetOp, FleetScope, FleetSecret, NodeIdentity};

    const HANG: Duration = Duration::from_secs(60);

    #[test]
    fn ineligible_addresses_do_not_consume_the_physical_link_budget() {
        let mut addresses: Vec<IpAddr> = (1..=MAX_LINKS as u8)
            .map(|last| Ipv4Addr::new(10, 0, 0, last).into())
            .collect();
        addresses.push(Ipv4Addr::LOCALHOST.into());
        let selected = source_addresses(addresses);
        let bound = links(&selected, 0, Scope::Loopback);
        assert_eq!(bound.len(), 1);
        assert_eq!(bound[0].0, Ipv4Addr::LOCALHOST);
    }

    #[test]
    fn reply_budget_allows_concurrent_joiners_and_bounded_packet_loss_recovery() {
        let now = Instant::now();
        let source: IpAddr = Ipv4Addr::LOCALHOST.into();
        let first = request(&[1; NONCE]);
        let second = request(&[2; NONCE]);
        let mut window = ReplyWindow::new(now);
        assert!(window.admit(source, &first, now));
        assert!(
            window.admit(source, &second, now),
            "init and subscriber can ask together"
        );
        assert!(!window.admit(source, &first, now), "duplicate flood");
        assert!(
            window.admit(source, &first, now + RETRANSMIT),
            "lost response"
        );
        assert!(window.admit(source, &second, now + RETRANSMIT));
        assert!(
            !window.admit(source, &first, now + RETRANSMIT * 2),
            "per-source ceiling"
        );
        for last in 1..=MAX_PEERS - 4 {
            assert!(window.admit(Ipv4Addr::new(10, 0, 0, last as u8).into(), &first, now));
        }
        assert!(
            !window.admit(Ipv4Addr::new(10, 0, 1, 1).into(), &first, now),
            "global ceiling"
        );
        assert!(window.admit(source, &first, now + REFRESH));
    }

    #[test]
    fn packets_are_bounded_versioned_and_bound_to_this_solicitation() {
        let nonce = [7; NONCE];
        let source: SocketAddr = "127.0.0.1:41385".parse().unwrap();
        let key = NodeKey::from_bytes([9; 32]);
        let ask = request(&nonce);
        let reply = response(&ask, 7100, key).unwrap();
        assert_eq!(
            hint(&reply, source, &nonce),
            Some(Hint {
                address: "127.0.0.1:7100".parse().unwrap(),
                key,
            })
        );
        assert!(
            hint(&reply, source, &[8; NONCE]).is_none(),
            "old or unsolicited reply"
        );
        assert!(hint(&reply[..RESPONSE - 1], source, &nonce).is_none());
        let mut extended = reply.to_vec();
        extended.push(0);
        assert!(hint(&extended, source, &nonce).is_none());
        assert!(response(&ask, 0, key).is_none());
        assert!(
            response(&reply, 7100, key).is_none(),
            "responses never elicit responses"
        );
        let mut unknown = ask;
        unknown[0] ^= 1;
        assert!(response(&unknown, 7100, key).is_none());
    }

    #[test]
    fn native_solicitation_reaches_the_member_handshake_without_dns() {
        let reservation = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
        let discovery_port = reservation.port();
        let client = links(&[Ipv4Addr::LOCALHOST], discovery_port, Scope::Loopback);
        let identity = |seed| {
            let provisional =
                NodeIdentity::from_seed("00000000000000000000000000000000", &[seed; 32]).unwrap();
            NodeIdentity::from_seed(
                &aterm_net::fleet_member::node_principal(provisional.key()),
                &[seed; 32],
            )
            .unwrap()
        };
        let server = identity(11);
        let requester = identity(12);
        let secret = FleetSecret::new(&[7; 32]).unwrap();
        let listener = aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let address = listener.local_addr().unwrap();
        let (ready, bound) = std::sync::mpsc::channel();
        let responder = Responder::start_on(
            address.port(),
            server.key(),
            || true,
            Network {
                scope: Scope::Loopback,
                port: discovery_port,
                sources: Box::new(|| vec![Ipv4Addr::LOCALHOST]),
                ready: Some(ready),
            },
        )
        .unwrap();
        assert_eq!(bound.recv_timeout(HANG).unwrap(), 1);
        let hints = probe_on(&client, HANG).unwrap();
        assert_eq!(
            hints,
            [Hint {
                address,
                key: server.key()
            }]
        );
        assert!(!responder.finished());
        let server_secret = secret.clone();
        let (done, finished) = std::sync::mpsc::channel();
        let server_task = thread::spawn(move || {
            // Prove the actual key twice: a copied or forged discovery claim
            // must not pass the ordinary membership-bound handshake.
            for _ in 0..2 {
                let (tcp, _) = listener.accept().unwrap();
                aterm_net::fleet::server_handshake_as(
                    tcp,
                    &server_secret,
                    Some(&server),
                    FleetScope::Facts,
                    &[FleetOp::Survey],
                    HANG,
                )
                .unwrap();
            }
            done.send(()).unwrap();
        });
        let keys = [(hints[0].key, aterm_net::fleet_member::Standing::Member)]
            .into_iter()
            .collect();
        for (claim, expected) in [(hints[0].key, true), (requester.key(), false)] {
            let advert = aterm_secrets::discover::Advert {
                instance: "direct-lan".into(),
                host: address.ip().to_string(),
                port: address.port(),
                txt: vec![
                    ("v".into(), "2".into()),
                    ("id".into(), aterm_net::fleet_member::node_principal(claim)),
                    ("port".into(), address.port().to_string()),
                ],
            };
            assert_eq!(
                crate::node_service::prove_discovery_address(
                    &advert,
                    &secret,
                    Some(&requester),
                    &keys
                )
                .is_some(),
                expected
            );
        }
        finished.recv_timeout(HANG).unwrap();
        server_task.join().unwrap();
        drop(responder);
    }

    #[test]
    fn sharing_hold_opens_no_discovery_socket_and_owner_drop_wakes_the_worker() {
        let (ready, bound) = std::sync::mpsc::channel();
        let responder = Responder::start_on(
            7100,
            NodeKey::from_bytes([3; 32]),
            || false,
            Network {
                scope: Scope::Loopback,
                port: 0,
                sources: Box::new(|| vec![Ipv4Addr::LOCALHOST]),
                ready: Some(ready),
            },
        )
        .unwrap();
        assert_eq!(bound.recv_timeout(HANG).unwrap(), 0);
        assert!(!responder.finished());
        drop(responder);
    }
}
