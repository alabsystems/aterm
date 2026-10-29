// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded transport workers. A generation joins every worker before its owner
//! lease can be released, including a dial still awaiting its first SYN reply.

use super::{
    discovery::Hints,
    report::{RepairRound, bounded_error},
};
use crate::{
    Result,
    control::{Endpoint, Repair, Side},
};
use aterm_net::lan::{Cancellation, Connection, Listener, Multicast, Scope};
use aterm_secrets::commons::hearing::Said;
use ring::rand::{SecureRandom, SystemRandom};
use std::{
    collections::VecDeque,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 31, 84);
const PORT: u16 = 41384;
const WORKERS: usize = 4;
/// How often every link sends its beacon, after the first one each sends as it opens.
const BEACON: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Channel {
    stopped: bool,
    local: Option<IpAddr>,
    socket: Option<Cancellation>,
}
impl Channel {
    fn install(&mut self, connection: &Connection) -> io::Result<bool> {
        if self.stopped {
            return Ok(false);
        }
        self.socket = Some(connection.cancellation()?);
        self.local = Some(connection.local_addr()?.ip());
        Ok(true)
    }
    fn stop(&mut self) {
        self.stopped = true;
        if let Some(socket) = &self.socket {
            socket.cancel();
        }
    }
}
struct Worker {
    channel: Arc<Mutex<Channel>>,
    task: JoinHandle<(Option<SocketAddr>, Repair, bool, RepairRound)>,
    incoming: bool,
    target: Option<SocketAddr>,
}
impl Worker {
    fn start(
        endpoint: Arc<Endpoint>,
        connection: Option<Connection>,
        target: Option<SocketAddr>,
        scope: Scope,
        repair: Repair,
    ) -> io::Result<Self> {
        let incoming = connection.is_some();
        let channel = Arc::new(Mutex::new(Channel::default()));
        if let Some(connection) = &connection {
            channel
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .install(connection)?;
        }
        let control = channel.clone();
        let task = thread::Builder::new()
            .name("commons-repair".into())
            .spawn(move || {
                let mut repair = repair;
                repair.begin_round();
                let mut exchange_started = false;
                let result = (|| -> Result<crate::control::Report> {
                    let (connection, side) = if let Some(connection) = connection {
                        (connection, Side::Responder)
                    } else {
                        if control.lock().unwrap_or_else(|p| p.into_inner()).stopped {
                            return Err(crate::Error::new(
                                "automatic repair cancelled before connection",
                            ));
                        }
                        let Some(target) = target else {
                            return Err(crate::Error::new("automatic repair has no target"));
                        };
                        (
                            Connection::connect(target, scope, Duration::from_secs(3))?,
                            Side::Initiator,
                        )
                    };
                    if !control
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .install(&connection)?
                    {
                        return Err(crate::Error::new(
                            "automatic repair cancelled before exchange",
                        ));
                    }
                    exchange_started = true;
                    endpoint.exchange(connection, side, &mut repair)
                })();
                let mut control = control.lock().unwrap_or_else(|p| p.into_inner());
                let success =
                    result.is_ok() && !control.stopped && repair.authenticated_actor().is_some();
                let report = RepairRound {
                    target,
                    actor: repair.authenticated_actor(),
                    exchange_started,
                    cancelled: control.stopped,
                    phase: Some(repair.phase()),
                    control: result.map_err(bounded_error),
                    membership: repair.membership(),
                    membership_error: repair.membership_error().map(bounded_error),
                    application: repair.application(),
                    application_error: repair.application_error().map(bounded_error),
                    completed_at_ms: aterm_time::unix_millis().unwrap_or(0),
                    dropped_before: 0,
                };
                if control.stopped {
                    repair.begin_round();
                }
                control.socket = None;
                (target, repair, success, report)
            })?;
        Ok(Self {
            channel,
            task,
            incoming,
            target,
        })
    }
    fn stop(&self) {
        self.channel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .stop();
    }
    fn stop_on(&self, local: IpAddr) {
        let mut channel = self.channel.lock().unwrap_or_else(|p| p.into_inner());
        if channel.local == Some(local) {
            channel.stop();
        }
    }
}

struct Link {
    source: IpAddr,
    listener: Option<Listener>,
    hints: Multicast,
    /// An isolated attachment's stand-in for the OS's Local Network permission (tests): while it
    /// reads false, this link's beacon fails as a refused app's LAN send does ([`Self::beacon`]).
    #[cfg(test)]
    permission: Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl Link {
    /// Send this link's beacon to its group: its listener's port where it serves repair, else
    /// opaque presence.
    fn beacon(&self, hints: &Hints) -> io::Result<()> {
        // macOS fails a refused app's LAN send with EHOSTUNREACH ("No route to host").
        #[cfg(test)]
        if self
            .permission
            .as_ref()
            .is_some_and(|allowed| !allowed.load(std::sync::atomic::Ordering::Acquire))
        {
            return Err(io::Error::new(
                io::ErrorKind::HostUnreachable,
                "the isolated attachment's Local Network permission is withheld",
            ));
        }
        let wire = match &self.listener {
            Some(listener) => hints.wire(listener.local_addr()?.port()),
            None => hints.presence_wire(),
        };
        self.hints.send(&wire)
    }
}

pub(super) struct Transport {
    endpoint: Option<Arc<Endpoint>>,
    links: Vec<Link>,
    hints: Hints,
    workers: Vec<Worker>,
    beacon: Instant,
    dial: Instant,
    rotate: Instant,
    scope: Scope,
    /// The group's port: [`PORT`] on a LAN, an isolated test attachment's own on loopback.
    port: u16,
    contacts: Vec<(astream_wire::sync::Actor, Instant, Said)>,
    completed: VecDeque<RepairRound>,
    failed: bool,
    /// A listener or multicast socket of this transport reached the LAN ([`Self::bound`]).
    bound: bool,
    /// A LAN attachment's stand-in (tests): while it reads false each address refreshed is one no
    /// bind reaches (its checks refuse it, as an attachment that changed since its address was
    /// read), and once it reads true it is the loopback ([`Self::standing_in`]).
    #[cfg(test)]
    stand_in: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// What each new link's permission reads on an isolated test attachment
    /// ([`Link::permission`]).
    #[cfg(test)]
    permission: Option<Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(test)]
    observed: std::collections::BTreeMap<astream_wire::sync::Digest, usize>,
}
impl Transport {
    /// A transport on an isolated loopback attachment at `port` (tests), opened as a LAN one is:
    /// its one link binds and sends its first beacon while `permission` (the OS's Local Network
    /// permission's stand-in) reads true, and fails its sends while it reads false.
    #[cfg(test)]
    pub(super) fn isolated(
        endpoint: Option<Endpoint>,
        port: u16,
        permission: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<Self> {
        let mut transport = Self::unopened(endpoint, Scope::Loopback, port)?;
        transport.permission = permission;
        transport.refresh(&[SocketAddr::from((Ipv4Addr::LOCALHOST, 0))]);
        Ok(transport)
    }

    /// A transport on a LAN attachment's stand-in, `up` (tests): opened on `addresses` as a LAN
    /// one is, it binds none of them while `up` reads false, and the loopback in their place once
    /// it reads true, so its refresh reaches the LAN only then.
    #[cfg(test)]
    pub(super) fn standing_in(
        endpoint: Option<Endpoint>,
        up: Arc<std::sync::atomic::AtomicBool>,
        addresses: &[SocketAddr],
    ) -> Result<Self> {
        let mut transport = Self::unopened(endpoint, Scope::Loopback, 0)?;
        transport.stand_in = Some(up);
        transport.refresh(addresses);
        Ok(transport)
    }

    /// The addresses a stand-in's refresh binds in place of `addresses` ([`Self::standing_in`]).
    #[cfg(test)]
    fn stood_in(&self, addresses: &[SocketAddr]) -> Option<Vec<SocketAddr>> {
        let up = self
            .stand_in
            .as_ref()?
            .load(std::sync::atomic::Ordering::Acquire);
        // TEST-NET-1 is no loopback source: a loopback bind's checks refuse it before its socket.
        let source = if up {
            Ipv4Addr::LOCALHOST
        } else {
            Ipv4Addr::new(192, 0, 2, 1)
        };
        Some(vec![SocketAddr::from((source, 0)); addresses.len()])
    }

    #[cfg(test)]
    pub(super) fn application_observations(
        &self,
    ) -> &std::collections::BTreeMap<astream_wire::sync::Digest, usize> {
        &self.observed
    }

    pub(super) fn open(endpoint: Endpoint, addresses: &[SocketAddr]) -> Result<Self> {
        Self::open_mode(Some(endpoint), addresses)
    }
    /// Opaque presence only: no TCP listener, identity advertisement or dialing.
    pub(super) fn presence_only(addresses: &[SocketAddr]) -> Result<Self> {
        Self::open_mode(None, addresses)
    }
    fn open_mode(endpoint: Option<Endpoint>, addresses: &[SocketAddr]) -> Result<Self> {
        #[cfg(any(test, debug_assertions))]
        if let Some(port) = super::isolation::port()? {
            let mut transport = Self::unopened(endpoint, Scope::Loopback, port)?;
            transport.refresh(addresses);
            return Ok(transport);
        }
        let mut transport = Self::unopened(endpoint, Scope::Lan, PORT)?;
        transport.refresh(addresses);
        Ok(transport)
    }
    /// A transport with no link yet, whose group is `port` in `scope`.
    fn unopened(endpoint: Option<Endpoint>, scope: Scope, port: u16) -> Result<Self> {
        let mut token = [0; 32];
        SystemRandom::new()
            .fill(&mut token)
            .map_err(|_| crate::Error::new("discovery randomness unavailable"))?;
        let now = Instant::now();
        Ok(Self {
            endpoint: endpoint.map(Arc::new),
            links: Vec::new(),
            hints: Hints::new(token),
            workers: Vec::new(),
            // Each link sends its first beacon as it opens ([`Self::refresh`]).
            beacon: now + BEACON,
            dial: now,
            rotate: now + Duration::from_secs(300),
            scope,
            port,
            contacts: Vec::new(),
            completed: VecDeque::new(),
            failed: false,
            bound: false,
            #[cfg(test)]
            stand_in: None,
            #[cfg(test)]
            permission: None,
            #[cfg(test)]
            observed: std::collections::BTreeMap::new(),
        })
    }
    /// Open a link on each address in `addresses` that has none. A link counts once its first
    /// beacon went out: under the OS's refusal (macOS's Local Network switch off for aterm) its
    /// sockets bind and its sends fail (EHOSTUNREACH), so a refused link is dropped here, failed,
    /// before any observation or peer's reachability counts it, and the next refresh tries again.
    pub(super) fn refresh(&mut self, addresses: &[SocketAddr]) {
        #[cfg(test)]
        let stood_in = self.stood_in(addresses);
        #[cfg(test)]
        let addresses = stood_in.as_deref().unwrap_or(addresses);
        self.failed = false;
        for address in addresses {
            let IpAddr::V4(source) = address.ip() else {
                continue;
            };
            if self.links.iter().any(|link| link.source == address.ip()) {
                continue;
            }
            let listener = if self
                .endpoint
                .as_ref()
                .is_none_or(|endpoint| endpoint.pending())
            {
                None
            } else {
                match Listener::try_bind(*address, self.scope) {
                    Ok(listener) => Some(listener),
                    Err(failure) => {
                        self.bound |= failure.reached;
                        self.failed = true;
                        continue;
                    }
                }
            };
            self.bound |= listener.is_some();
            let hints = match Multicast::try_bind(source, GROUP, self.port, self.scope) {
                Ok(hints) => hints,
                Err(failure) => {
                    self.bound |= failure.reached;
                    self.failed = true;
                    continue;
                }
            };
            self.bound = true;
            let link = Link {
                source: address.ip(),
                listener,
                hints,
                #[cfg(test)]
                permission: self.permission.clone(),
            };
            if link.beacon(&self.hints).is_ok() {
                self.links.push(link);
            } else {
                self.failed = true;
            }
        }
    }
    /// Whether a listener or multicast socket of this transport reached the LAN since it opened:
    /// it bound, whatever its beacon then did (a refused app's binds succeed and its sends fail),
    /// or it failed only once its socket was made ([`aterm_net::lan::BindFailure`]), after a
    /// bind, join or listen at which the OS may have asked. That is its LAN use. A transport whose
    /// binds all refused at their checks, before any socket (an attachment that changed since its
    /// address was read), made none, and the OS asked nothing.
    pub(super) const fn bound(&self) -> bool {
        self.bound
    }
    pub(super) fn interfaces(&self) -> usize {
        self.links.len()
    }
    pub(super) fn unavailable(&self) -> bool {
        self.links.is_empty() && self.failed
    }
    pub(super) fn peers(&self) -> usize {
        self.hints.count()
    }
    pub(super) fn unverified_peers(&self) -> usize {
        self.hints.unverified_count()
    }
    /// Authenticated peers since the last call, each at its latest contact and with what that
    /// contact said of its version.
    pub(super) fn take_contacts(&mut self) -> Vec<(astream_wire::sync::Actor, Instant, Said)> {
        std::mem::take(&mut self.contacts)
    }

    pub(super) fn reports(&mut self) -> impl Iterator<Item = RepairRound> + '_ {
        self.completed.drain(..)
    }

    fn retain_report(&mut self, mut report: RepairRound) {
        const MAX: usize = 64;
        if self.completed.len() == MAX {
            let lost = self.completed.pop_front().expect("full metadata queue");
            report.dropped_before = lost.dropped_before.saturating_add(1);
        }
        self.completed.push_back(report);
    }

    fn finish(&mut self, worker: Worker, now: Instant) {
        match worker.task.join() {
            Ok((target, repair, success, report)) => {
                if let Some((actor, at)) = repair.authenticated_contact() {
                    let said = repair.said();
                    if let Some((_, old, was)) =
                        self.contacts.iter_mut().find(|(id, _, _)| *id == actor)
                    {
                        if at >= *old && said != Said::Unknown {
                            *was = said;
                        }
                        *old = (*old).max(at);
                    } else if self.contacts.len() < 64 {
                        self.contacts.push((actor, at, said));
                    }
                }
                #[cfg(test)]
                if let Some(report) = repair.application()
                    && let Some(table) = report.table
                {
                    self.observed.insert(table, report.retained_after);
                }
                if let Some(address) = target {
                    self.hints.finish(address, repair, success, now);
                }
                self.retain_report(report);
            }
            Err(_) => {
                if let Some(address) = worker.target {
                    self.hints.finish(address, Repair::default(), false, now);
                }
                self.retain_report(RepairRound::panicked(worker.target));
            }
        }
    }

    pub(super) fn stop(&mut self) {
        // Withdrawing listeners invalidates accepted proofs before cancellation.
        self.links.clear();
        for worker in &self.workers {
            worker.stop();
        }
        while let Some(worker) = self.workers.pop() {
            self.finish(worker, Instant::now());
        }
    }
    fn receive_beacons(
        link: &Link,
        links: &[Link],
        hints: &mut Hints,
        scope: Scope,
        beacon: Instant,
        now: Instant,
    ) -> io::Result<()> {
        // Bound work from an unauthenticated multicast flood per tick.
        let before = (hints.count(), hints.unverified_count());
        for _ in 0..16 {
            let Some((wire, source)) = link.hints.receive()? else {
                break;
            };
            if scope == Scope::Lan && links.iter().any(|local| local.source == source.ip()) {
                continue;
            }
            hints.observe(&wire, source, now);
            hints.observe_presence(&wire, source, now);
        }
        // Answer a newly admitted peer or pending enrollee without waiting out
        // BEACON; Hints' per-IP and global bounds cap admissions.
        if now < beacon
            && link.listener.is_some()
            && (hints.count(), hints.unverified_count()) > before
        {
            link.beacon(hints)?;
        }
        Ok(())
    }
    pub(super) fn tick(&mut self, now: Instant) -> io::Result<()> {
        self.hints.expire_presence(now);
        if self.endpoint.is_none() {
            self.hints.expire_listen_only(now);
        }
        if now >= self.rotate {
            let mut token = [0; 32];
            SystemRandom::new()
                .fill(&mut token)
                .map_err(|_| io::Error::other("discovery randomness unavailable"))?;
            self.hints.rotate(token);
            self.rotate = now + Duration::from_secs(300);
        }
        let mut index = 0;
        while index < self.workers.len() {
            if self.workers[index].task.is_finished() {
                let worker = self.workers.swap_remove(index);
                self.finish(worker, now);
            } else {
                index += 1;
            }
        }
        let mut index = 0;
        while index < self.links.len() {
            let link = &self.links[index];
            let serviced = (|| -> io::Result<()> {
                if let Some(listener) = &link.listener {
                    listener.recheck()?;
                }
                link.hints.recheck()?;
                if now >= self.beacon {
                    link.beacon(&self.hints)?;
                }
                Self::receive_beacons(
                    link,
                    &self.links,
                    &mut self.hints,
                    self.scope,
                    self.beacon,
                    now,
                )?;
                // Unproved inbound peers cannot occupy the outbound repair budget.
                if self.workers.len() < WORKERS
                    && self.workers.iter().filter(|worker| worker.incoming).count() < WORKERS / 2
                    && let Some(listener) = &link.listener
                    && let Some(endpoint) = &self.endpoint
                {
                    match listener.accept() {
                        Ok(connection) => self.workers.push(Worker::start(
                            endpoint.clone(),
                            Some(connection),
                            None,
                            self.scope,
                            Repair::default(),
                        )?),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(_) => {
                            // Refusing one hostile or vanished peer cannot withdraw
                            // a healthy listener. Only its own evidence can do so.
                            listener.recheck()?;
                        }
                    }
                }
                Ok(())
            })();
            if serviced.is_err() {
                self.failed = true;
                let local = self.links.remove(index).source;
                for worker in &self.workers {
                    worker.stop_on(local);
                }
            } else {
                index += 1;
            }
        }
        if now >= self.beacon {
            self.beacon = now + BEACON;
        }
        if now >= self.dial
            && let Some(endpoint) = &self.endpoint
            && !self.links.is_empty()
            && self.workers.len() < WORKERS
            && let Some((address, repair)) = self.hints.next(now)
        {
            match Worker::start(endpoint.clone(), None, Some(address), self.scope, repair) {
                Ok(worker) => self.workers.push(worker),
                Err(error) => {
                    self.hints.finish(address, Repair::default(), false, now);
                    return Err(error);
                }
            }
            self.dial = now + Duration::from_millis(250);
        }
        Ok(())
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests;
