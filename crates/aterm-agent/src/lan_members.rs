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
    io::{self, Read as _, Write as _},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::fd::AsFd as _,
    sync::{
        Arc, Mutex, PoisonError, Weak,
        atomic::{AtomicBool, Ordering},
    },
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
    /// A fresh real solicitation on this fixture's already owned loopback network.
    #[cfg(test)]
    pub(crate) fn probe_for_test(&self, within: Duration) -> io::Result<Vec<Hint>> {
        assert_eq!(self.network.scope, Scope::Loopback);
        probe_on(
            links(
                &(self.network.sources)(),
                self.network.port,
                self.network.scope,
            ),
            within,
        )
    }

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
    control: Arc<Control>,
    task: Option<JoinHandle<()>>,
}

struct Control {
    state: Mutex<ControlState>,
}

struct ControlState {
    writer: Option<std::fs::File>,
    rung: bool,
    stopped: bool,
    ready: Option<Weak<AtomicBool>>,
}

enum ControlEvent {
    Stop,
    Ready(Option<Weak<AtomicBool>>),
}

impl ControlState {
    fn ring(&mut self) {
        if self.rung {
            return;
        }
        let Some(writer) = &self.writer else {
            return;
        };
        // Only this mutex owns writer use. One coalesced byte is sufficient even when a
        // producer outruns the receiver; a full or closed pipe never blocks the producer.
        let mut output = writer;
        let result = aterm_uds::ready::with_nonblocking(writer.as_fd(), || output.write_all(&[1]));
        match result {
            Ok(()) => self.rung = true,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.rung = true,
            Err(_) => {
                self.stopped = true;
                self.writer = None;
            }
        }
    }
}

impl Control {
    fn announce(&self, generation: Weak<AtomicBool>) {
        if !generation
            .upgrade()
            .is_some_and(|live| live.load(Ordering::Acquire))
        {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.stopped {
            state.ready = Some(generation);
            state.ring();
        }
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.stopped = true;
        state.ready = None;
        state.ring();
        state.writer = None;
    }

    fn take(&self, reader: &mut std::io::PipeReader) -> ControlEvent {
        if !matches!(reader.read(&mut [0; 64]), Ok(n) if n > 0) {
            return ControlEvent::Stop;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.rung = false;
        if state.stopped {
            ControlEvent::Stop
        } else {
            ControlEvent::Ready(state.ready.take())
        }
    }
}

struct Network {
    scope: Scope,
    port: u16,
    sources: Box<dyn Fn() -> Vec<Ipv4Addr> + Send>,
    #[cfg(test)]
    ready: Option<std::sync::mpsc::Sender<usize>>,
}

impl Responder {
    /// The existing native transport on a private loopback group/port. The receiver exists
    /// before the responder binds, and the returned receipt reports its actual bound links.
    #[cfg(test)]
    pub(crate) fn loopback_for_test(
        port: u16,
        key: NodeKey,
    ) -> io::Result<(Self, Arrivals, std::sync::mpsc::Receiver<usize>)> {
        let reservation = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback)?;
        let discovery_port = reservation.port();
        let network = |ready| Network {
            scope: Scope::Loopback,
            port: discovery_port,
            sources: Box::new(|| vec![Ipv4Addr::LOCALHOST]),
            ready,
        };
        let arrivals = Arrivals::on(network(None), None)?;
        let (ready, bound) = std::sync::mpsc::channel();
        let responder = Self::start_on(port, key, || true, network(Some(ready)))?;
        Ok((responder, arrivals, bound))
    }

    pub fn announce_ready(&self, generation: Weak<AtomicBool>) {
        self.control.announce(generation);
    }
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
        let control = Arc::new(Control {
            state: Mutex::new(ControlState {
                writer: Some(std::fs::File::from(std::os::fd::OwnedFd::from(writer))),
                rung: false,
                stopped: false,
                ready: None,
            }),
        });
        let mut incarnation = [0; NONCE];
        aterm_uds::rand::fill(&mut incarnation)?;
        let task = thread::Builder::new()
            .name("aterm-member-lan".into())
            .spawn({
                let control = Arc::clone(&control);
                move || serve(reader, &control, port, key, incarnation, allowed, network)
            })?;
        Ok(Self {
            port,
            key,
            control,
            task: Some(task),
        })
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        self.control.stop();
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

fn serve(
    mut stop: std::io::PipeReader,
    control: &Control,
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
    'serving: loop {
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
            match control.take(&mut stop) {
                ControlEvent::Stop => return,
                ControlEvent::Ready(Some(live)) => {
                    let mut withdrawn = false;
                    for (_, link) in &sockets {
                        if !live
                            .upgrade()
                            .is_some_and(|live| live.load(Ordering::Acquire))
                        {
                            break;
                        }
                        if !allowed() {
                            withdrawn = true;
                            break;
                        }
                        if !live
                            .upgrade()
                            .is_some_and(|live| live.load(Ordering::Acquire))
                        {
                            break;
                        }
                        let Some(next) = generation.checked_add(1) else {
                            return;
                        };
                        generation = next;
                        let _ = link.send(&announcement(&incarnation, generation));
                    }
                    if withdrawn {
                        // Match ordinary refresh's hold semantics. A transient failed
                        // check must not consume the capability edge while retaining
                        // silent old bindings when permission returns.
                        sockets.clear();
                        refresh = Instant::now();
                        continue 'serving;
                    }
                }
                ControlEvent::Ready(None) => {}
            }
        }
        for index in ready {
            if index == sockets.len() {
                continue;
            }
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
#[path = "lan_members_tests.rs"]
mod tests;
