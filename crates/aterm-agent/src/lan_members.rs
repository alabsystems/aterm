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
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{self, Write as _},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::fd::AsFd as _,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 31, 85);
const PORT: u16 = 41385;
const MAGIC: &[u8; 8] = b"aterm-m1";
const READY_MAGIC: &[u8; 8] = b"aterm-r1";
const NONCE: usize = 16;
const REQUEST: usize = MAGIC.len() + NONCE;
const RESPONSE: usize = REQUEST + 2 + NodeKey::LEN;
const READY: usize = READY_MAGIC.len() + NONCE + 8;
const MAX_LINKS: usize = 8;
const MAX_PEERS: usize = 64;
const REFRESH: Duration = Duration::from_secs(1);
const RETRANSMIT: Duration = Duration::from_millis(100);

fn contained() -> bool {
    cfg!(test)
        || std::env::var_os(aterm_types::world::MARKER).is_some()
        || std::env::var_os("CI").is_some()
        || std::env::var_os(aterm_secrets::discover::LAB_MEMBER_SERVICE_ENV).is_some()
}

fn announcement(incarnation: &[u8; NONCE], generation: u64) -> [u8; READY] {
    let mut wire = [0; READY];
    wire[..READY_MAGIC.len()].copy_from_slice(READY_MAGIC);
    wire[READY_MAGIC.len()..READY_MAGIC.len() + NONCE].copy_from_slice(incarnation);
    wire[READY_MAGIC.len() + NONCE..].copy_from_slice(&generation.to_be_bytes());
    wire
}

fn ready_token(wire: &[u8]) -> Option<[u8; NONCE + 8]> {
    (wire.len() == READY && wire.starts_with(READY_MAGIC)).then(|| {
        wire[READY_MAGIC.len()..]
            .try_into()
            .expect("fixed ready length")
    })
}

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

/// The sockets and nonce of one bounded native solicitation. Authentication may
/// continue between batches; replies already queued retain this same nonce.
pub(crate) struct Probe {
    active: Vec<(Ipv4Addr, Multicast)>,
    nonce: [u8; NONCE],
    deadline: Instant,
    resend: Instant,
    seen: BTreeSet<SocketAddr>,
}

impl Probe {
    /// A lab's private service never reaches the live LAN.
    pub(crate) fn start(within: Duration) -> io::Result<Self> {
        if contained() {
            return Self::on(Vec::new(), within);
        }
        Self::on(links(&sources(), PORT, Scope::Lan), within)
    }

    pub(crate) fn first(&mut self) -> io::Result<Vec<Hint>> {
        self.next(self.deadline)
    }

    /// The complete remaining bounded native union, for a caller whose next
    /// discovery source is a one-shot list. This never renews collection.
    pub(crate) fn complete(&mut self, until: Instant) -> io::Result<Vec<Hint>> {
        let mut all = Vec::new();
        loop {
            let batch = self.next(until)?;
            if batch.is_empty() {
                return Ok(all);
            }
            all.extend(batch);
        }
    }

    fn on(links: Vec<(Ipv4Addr, Multicast)>, within: Duration) -> io::Result<Self> {
        let mut nonce = [0; NONCE];
        if !links.is_empty() && !within.is_zero() {
            aterm_uds::rand::fill(&mut nonce)?;
        }
        let wire = request(&nonce);
        let mut active = Vec::new();
        for (source, link) in links {
            if !within.is_zero() && link.send(&wire).is_ok() {
                active.push((source, link));
            }
        }
        let now = Instant::now();
        Ok(Self {
            active,
            nonce,
            deadline: now + within,
            resend: now + RETRANSMIT,
            seen: BTreeSet::new(),
        })
    }

    pub(crate) fn next(&mut self, until: Instant) -> io::Result<Vec<Hint>> {
        if self.active.is_empty() || self.seen.len() >= MAX_PEERS || Instant::now() >= until {
            return Ok(Vec::new());
        }
        let deadline = self.deadline.min(until);
        let fds: Vec<_> = self.active.iter().map(|(_, link)| link.as_fd()).collect();
        loop {
            // A proof may outlast the original collection lease. Drain only what is
            // ready then; never renew its wait or the caller's final proof deadline.
            let ready = if Instant::now() >= deadline {
                let interests: Vec<_> = fds
                    .iter()
                    .copied()
                    .map(|fd| aterm_uds::ready::Watch::new(fd, aterm_uds::ready::Interest::Read))
                    .collect();
                aterm_uds::ready::any_interests_now(&interests)?
            } else {
                aterm_uds::ready::any_until(&fds, deadline.min(self.resend))?
            };
            let now = Instant::now();
            if now >= until {
                return Ok(Vec::new());
            }
            if now < self.deadline && now >= self.resend {
                let wire = request(&self.nonce);
                for (_, link) in &self.active {
                    let _ = link.send(&wire);
                }
                self.resend = now + RETRANSMIT;
            }
            let mut found = BTreeMap::new();
            for index in ready {
                // An untrusted flood has a fixed per-read budget.
                for _ in 0..MAX_PEERS {
                    let Some((wire, source)) = self.active[index].1.receive()? else {
                        break;
                    };
                    if let Some(hint) = hint(&wire, source, &self.nonce)
                        && self.seen.len() < MAX_PEERS
                        && self.seen.insert(hint.address)
                    {
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
}

/// An owned passive receiver of untrusted member availability hints. Hearing one only
/// owes a fresh ordinary solicitation; it supplies no address, identity or admission.
pub(crate) struct Arrivals {
    network: Network,
    paths: Option<aterm_secrets::client::Paths>,
    active: Vec<(Ipv4Addr, Multicast)>,
    seen: VecDeque<[u8; NONCE + 8]>,
}

impl Arrivals {
    pub(crate) fn start(paths: &aterm_secrets::client::Paths) -> io::Result<Self> {
        Self::on(
            Network {
                scope: Scope::Lan,
                port: PORT,
                sources: Box::new(|| if contained() { Vec::new() } else { sources() }),
                #[cfg(test)]
                ready: None,
            },
            Some(paths.clone()),
        )
    }

    fn on(network: Network, paths: Option<aterm_secrets::client::Paths>) -> io::Result<Self> {
        let mut result = Self {
            network,
            paths,
            active: Vec::new(),
            seen: VecDeque::new(),
        };
        result.refresh()?;
        Ok(result)
    }

    fn refresh(&mut self) -> io::Result<()> {
        if let Some(paths) = &self.paths
            && let Err(error) = aterm_secrets::client::refuse_lab_member_browse(paths)
        {
            self.active.clear();
            return Err(io::Error::other(error));
        }
        let current = (self.network.sources)();
        self.active
            .retain(|(source, link)| current.contains(source) && link.recheck().is_ok());
        for source in current {
            if self.active.len() >= MAX_LINKS {
                break;
            }
            if !self.active.iter().any(|(held, _)| *held == source) {
                self.active
                    .extend(links(&[source], self.network.port, self.network.scope));
            }
        }
        if self.active.is_empty() && !contained() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "no native member arrival socket was admitted on this network",
            ));
        }
        Ok(())
    }

    fn fds(&self) -> Vec<std::os::fd::BorrowedFd<'_>> {
        self.active.iter().map(|(_, link)| link.as_fd()).collect()
    }

    fn poll(&mut self) -> io::Result<bool> {
        if self.active.is_empty() {
            return Ok(false);
        }
        let interests: Vec<_> = self
            .active
            .iter()
            .map(|(_, link)| {
                aterm_uds::ready::Watch::new(link.as_fd(), aterm_uds::ready::Interest::Read)
            })
            .collect();
        let ready = aterm_uds::ready::any_interests_now(&interests)?;
        let mut arrived = false;
        for index in ready {
            for _ in 0..MAX_PEERS {
                let Some((wire, _)) = self.active[index].1.receive()? else {
                    break;
                };
                let Some(token) = ready_token(&wire) else {
                    continue;
                };
                if self.seen.contains(&token) {
                    continue;
                }
                if self.seen.len() == MAX_PEERS {
                    self.seen.pop_front();
                }
                self.seen.push_back(token);
                arrived = true;
            }
        }
        Ok(arrived)
    }
}

impl aterm_secrets::client::MemberArrivals for Arrivals {
    fn refresh(&mut self) -> io::Result<()> {
        Self::refresh(self)
    }

    fn fds(&self) -> Vec<std::os::fd::BorrowedFd<'_>> {
        Self::fds(self)
    }

    fn poll(&mut self) -> io::Result<bool> {
        Self::poll(self)
    }
}

#[cfg(test)]
fn probe_on(links: Vec<(Ipv4Addr, Multicast)>, within: Duration) -> io::Result<Vec<Hint>> {
    let mut probe = Probe::on(links, within)?;
    probe.first()
}

/// Actual loopback UDP with an expired collection lease. Each reply is queued at
/// the probe before returning, so consumer tests exercise queued continuation
/// without making scheduler timing into an admission or latency assertion.
///
/// Readability alone proves nothing once an earlier datagram is still queued, and
/// macOS delivers loopback multicast asynchronously on lo0's input thread. So each
/// reply is followed by a fence the probe ignores; the member hearing its own fence
/// loop back shows the interface delivered every earlier datagram, from any sender
/// on this group and port, to every socket joined there.
#[cfg(test)]
pub(crate) fn queued_test_probe() -> (Probe, impl Fn(u16, NodeKey) + Send + Sync) {
    const FENCE: &[u8] = b"aterm-m1-test-fence";
    let hang = Duration::from_secs(60);
    let member = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
    let mut probe = Probe::on(
        links(&[Ipv4Addr::LOCALHOST], member.port(), Scope::Loopback),
        hang,
    )
    .unwrap();
    let until = Instant::now() + hang;
    let ask = loop {
        assert!(
            !aterm_uds::ready::any_until(&[member.as_fd()], until)
                .unwrap()
                .is_empty(),
            "a native request must arrive"
        );
        if let Some((wire, _)) = member.receive().unwrap()
            && wire.len() == REQUEST
        {
            break wire;
        }
    };
    probe.deadline = Instant::now();
    (probe, move |port, key| {
        member.send(&response(&ask, port, key).unwrap()).unwrap();
        member.send(FENCE).unwrap();
        let until = Instant::now() + hang;
        loop {
            assert!(
                !aterm_uds::ready::any_until(&[member.as_fd()], until)
                    .unwrap()
                    .is_empty(),
                "the reply's fence must loop back"
            );
            if member
                .receive()
                .unwrap()
                .is_some_and(|(wire, _)| wire == FENCE)
            {
                break;
            }
        }
    })
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
        let mut incarnation = [0; NONCE];
        aterm_uds::rand::fill(&mut incarnation)?;
        let task = thread::Builder::new()
            .name("aterm-member-lan".into())
            .spawn(move || serve(reader, port, key, incarnation, allowed, network))?;
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
    incarnation: [u8; NONCE],
    allowed: impl Fn() -> bool,
    network: Network,
) {
    #[cfg(test)]
    let mut network = network;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let mut routes = (network.scope == Scope::Lan)
        .then(aterm_uds::network::RouteChanges::open)
        .and_then(Result::ok);
    let mut sockets = Vec::new();
    let mut refresh = Instant::now();
    let mut replies = ReplyWindow::new(Instant::now());
    let mut generation = 0_u64;
    loop {
        let now = Instant::now();
        if now >= refresh {
            let current = (network.sources)();
            sockets.retain(|(source, link): &(Ipv4Addr, Multicast)| {
                current.contains(source) && link.recheck().is_ok()
            });
            let mut announcements = Vec::new();
            if allowed() {
                'sources: for source in current {
                    if sockets.len() >= MAX_LINKS {
                        break;
                    }
                    if !sockets.iter().any(|(held, _)| *held == source) {
                        for link in links(&[source], network.port, network.scope) {
                            let Some(next) = generation.checked_add(1) else {
                                return;
                            };
                            generation = next;
                            if !allowed() {
                                sockets.clear();
                                announcements.clear();
                                break 'sources;
                            }
                            announcements.push((sockets.len(), generation));
                            sockets.push(link);
                        }
                    }
                }
            } else {
                sockets.clear();
            }
            // Finish every potentially blocking bind before announcing any attachment.
            // Each newly bound allowed link emits once; healthy refreshes emit nothing.
            for (index, generation) in announcements {
                if !allowed() {
                    sockets.clear();
                    break;
                }
                let _ = sockets[index]
                    .1
                    .send(&announcement(&incarnation, generation));
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
                // A route socket that fails is dropped: it would poll readable again at once
                // and fail again, a spin; the refresh timer still paces the rounds.
                match routes
                    .as_ref()
                    .map(aterm_uds::network::RouteChanges::read_waiting)
                {
                    Some(Some(true)) => refresh = Instant::now(),
                    Some(None) => routes = None,
                    Some(Some(false)) | None => {}
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

        let ready = announcement(&nonce, 1);
        assert!(ready_token(&ready).is_some());
        assert!(ready_token(&ready[..READY - 1]).is_none());
        let mut extended = ready.to_vec();
        extended.push(0);
        assert!(ready_token(&extended).is_none());
        assert!(ready_token(&ask).is_none());
        assert!(ready_token(&reply).is_none());
        assert!(hint(&ready, source, &nonce).is_none());
        assert!(response(&ready, 7100, key).is_none());
    }

    /// A later reply carries the original nonce. Starting another solicitation cannot
    /// recover it, and no DNS or membership authority is involved in receiving it.
    #[test]
    fn a_native_solicitation_keeps_later_replies_after_its_first_batch() {
        let member = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
        let client = links(&[Ipv4Addr::LOCALHOST], member.port(), Scope::Loopback);
        let mut probe = Probe::on(client, HANG).unwrap();
        let until = Instant::now() + HANG;
        let ask = loop {
            aterm_uds::ready::any_until(&[member.as_fd()], until).unwrap();
            if let Some((wire, _)) = member.receive().unwrap()
                && wire.len() == REQUEST
            {
                break wire;
            }
            assert!(Instant::now() < until, "a native request must arrive");
        };
        member
            .send(&response(&ask, 7101, NodeKey::from_bytes([1; 32])).unwrap())
            .unwrap();
        let first = probe.next(until).unwrap();
        assert_eq!(
            first
                .iter()
                .map(|hint| hint.address.port())
                .collect::<Vec<_>>(),
            [7101]
        );
        // Publish only after the consumer took its first batch. This models a second
        // member's reply while the caller authenticates the first, without a timing race.
        member
            .send(&response(&ask, 7102, NodeKey::from_bytes([2; 32])).unwrap())
            .unwrap();
        let next = probe.next(until).unwrap();
        assert_eq!(
            next.iter()
                .map(|hint| hint.address.port())
                .collect::<Vec<_>>(),
            [7102]
        );
    }

    #[test]
    fn native_reply_continuation_rejects_duplicates_foreign_nonces_and_expired_proof() {
        let (mut probe, reply) = queued_test_probe();
        let until = Instant::now() + HANG;
        reply(7101, NodeKey::from_bytes([1; 32]));
        assert_eq!(probe.next(until).unwrap().len(), 1);
        reply(7101, NodeKey::from_bytes([9; 32]));
        let sender = Multicast::bind(
            Ipv4Addr::LOCALHOST,
            GROUP,
            probe.active[0].1.port(),
            Scope::Loopback,
        )
        .unwrap();
        let mut foreign = probe.nonce;
        foreign[0] ^= 1;
        sender
            .send(&response(&request(&foreign), 7199, NodeKey::from_bytes([9; 32])).unwrap())
            .unwrap();
        sender.send(b"malformed").unwrap();
        reply(7102, NodeKey::from_bytes([2; 32]));
        let later = probe.next(until).unwrap();
        assert_eq!(
            later
                .iter()
                .map(|hint| hint.address.port())
                .collect::<Vec<_>>(),
            [7102]
        );
        reply(7103, NodeKey::from_bytes([3; 32]));
        assert!(
            probe.next(Instant::now()).unwrap().is_empty(),
            "a queued reply cannot renew the proof deadline"
        );
    }

    #[test]
    fn native_reply_cardinality_is_global_across_batches() {
        let (mut probe, reply) = queued_test_probe();
        let until = Instant::now() + HANG;
        reply(7200, NodeKey::from_bytes([1; 32]));
        assert_eq!(probe.next(until).unwrap().len(), 1);
        for offset in 1..=MAX_PEERS {
            reply(7200 + offset as u16, NodeKey::from_bytes([2; 32]));
        }
        let rest = probe.complete(until).unwrap();
        assert_eq!(rest.len(), MAX_PEERS - 1);
        assert_eq!(probe.seen.len(), MAX_PEERS);
        assert!(probe.next(until).unwrap().is_empty());
    }

    #[test]
    fn native_solicitation_reaches_the_member_handshake_without_dns() {
        let reservation = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
        let discovery_port = reservation.port();
        let mut arrivals = Arrivals::on(
            Network {
                scope: Scope::Loopback,
                port: discovery_port,
                sources: Box::new(|| vec![Ipv4Addr::LOCALHOST]),
                ready: None,
            },
            None,
        )
        .unwrap();
        // A real bounded solicitation first misses: no responder exists yet. The receiver
        // remains owned after that probe, and a later bind must wake it through the wire.
        let mut missed = Probe::on(
            links(&[Ipv4Addr::LOCALHOST], discovery_port, Scope::Loopback),
            RETRANSMIT,
        )
        .unwrap();
        assert!(missed.first().unwrap().is_empty());
        drop(missed);
        assert!(!arrivals.poll().unwrap());
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
        let until = Instant::now() + HANG;
        while !arrivals.poll().unwrap() {
            assert!(
                Instant::now() < until,
                "the late member announced no arrival"
            );
            aterm_uds::ready::any_until(&arrivals.fds(), until).unwrap();
        }
        assert!(!arrivals.poll().unwrap());
        let hints = probe_on(
            links(&[Ipv4Addr::LOCALHOST], discovery_port, Scope::Loopback),
            HANG,
        )
        .unwrap();
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
        let mut inert = Arrivals::on(
            Network {
                scope: Scope::Loopback,
                port: 0,
                sources: Box::new(Vec::new),
                ready: None,
            },
            None,
        )
        .unwrap();
        assert!(inert.fds().is_empty());
        assert!(!inert.poll().unwrap());
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
