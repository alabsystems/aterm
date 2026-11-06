// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded transport workers. A generation joins every worker before its owner
//! lease can be released, including a dial still awaiting its first SYN reply.

use super::{
    discovery::{Hints, Outcome},
    report::{AuthenticatedConnection, RepairRound, bounded_error},
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
/// How often the seats' advertised builds and `Bus` speakers are read again.
const SEATS: Duration = Duration::from_secs(2);

/// What says a peer's fleet node key speaks `Bus` ([`super::Runtime::speaks_bus`]), shared so a
/// host can install it after its runtime started; `None` names no peer.
pub(super) type Bus = Arc<Mutex<Option<super::SpeaksBus>>>;

#[derive(Default)]
struct Channel {
    stopped: bool,
    local: Option<IpAddr>,
    connection: Option<AuthenticatedConnection>,
    #[cfg(test)]
    local_endpoint: Option<SocketAddr>,
    socket: Option<Cancellation>,
}
impl Channel {
    fn install(&mut self, connection: &Connection, began_ms: u64) -> io::Result<bool> {
        if self.stopped {
            return Ok(false);
        }
        self.socket = Some(connection.cancellation()?);
        let local = connection.local_addr()?;
        self.local = Some(local.ip());
        self.connection = Some(AuthenticatedConnection {
            local,
            peer: connection.peer_addr()?,
            began_ms,
            authenticated_ms: 0,
        });
        #[cfg(test)]
        {
            self.local_endpoint = Some(local);
        }
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
    /// When its round began: one that began after a push's ask counts for it ([`Push`]).
    started: Instant,
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
        let began_ms = aterm_time::unix_millis().unwrap_or(0);
        let channel = Arc::new(Mutex::new(Channel::default()));
        if let Some(connection) = &connection {
            channel
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .install(connection, began_ms)?;
        }
        let control = channel.clone();
        let task = thread::Builder::new()
            .name("commons-repair".into())
            .spawn(move || {
                let mut repair = repair;
                repair.begin_round();
                let mut connection = connection;
                let mut exchange_started = false;
                let result = (|| -> Result<crate::control::Report> {
                    for attempt in 0..2 {
                        let began_ms = if attempt == 0 {
                            began_ms
                        } else {
                            aterm_time::unix_millis().unwrap_or(0)
                        };
                        let (connection, side) = if let Some(connection) = connection.take() {
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
                            .install(&connection, began_ms)?
                        {
                            return Err(crate::Error::new(
                                "automatic repair cancelled before exchange",
                            ));
                        }
                        exchange_started = true;
                        let result = endpoint.exchange(connection, side, &mut repair);
                        if incoming || attempt != 0 || !repair.retry_application_probe() {
                            return result;
                        }
                        // The old peer closed before any application payload. One
                        // new connection rebuilds and revalidates the entire round.
                    }
                    unreachable!("the second exchange always returns")
                })();
                if let Err(error) = &result
                    && matches!(
                        error.repair_cause(),
                        aterm_types::network_state::RepairCause::LegacyPastCrossing
                    )
                {
                    // A protocol fence can refuse control before entering application repair.
                    // Preserve its cause without fabricating an application exchange.
                    repair.failure_cause(error);
                }
                let mut control = control.lock().unwrap_or_else(|p| p.into_inner());
                let success =
                    result.is_ok() && !control.stopped && repair.authenticated_actor().is_some();
                let completed_at = Instant::now();
                let completed_at_ms = aterm_time::unix_millis().unwrap_or(0);
                let connection = control.connection.zip(repair.authenticated_contact()).map(
                    |(mut connection, (_, authenticated_at))| {
                        connection.authenticated_ms =
                            super::contact_time(completed_at_ms, completed_at, authenticated_at);
                        connection
                    },
                );
                let report = RepairRound {
                    target,
                    actor: repair.authenticated_actor(),
                    connection,
                    exchange_started,
                    cancelled: control.stopped,
                    phase: Some(repair.phase()),
                    control: result.map_err(bounded_error),
                    membership: repair.membership(),
                    membership_error: repair.membership_error().map(bounded_error),
                    application: repair.application(),
                    application_error: repair.application_error().map(bounded_error),
                    application_cause: repair.application_cause(),
                    application_interrupted: repair.application_interrupted(),
                    application_deferral: repair.application_deferral().cloned().map(|mut why| {
                        why.local_error = why.local_error.map(bounded_error);
                        why
                    }),
                    application_batch_deferral: repair.application_batch_deferral().cloned().map(
                        |mut why| {
                            why.reason = bounded_error(why.reason);
                            why
                        },
                    ),
                    handoff: repair.handoff().cloned(),
                    completed_at,
                    completed_at_ms,
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
            started: Instant::now(),
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

#[cfg(test)]
pub(crate) fn test_worker(
    endpoint: Endpoint,
    target: SocketAddr,
    repair: Repair,
) -> (Repair, bool, RepairRound) {
    let worker = Worker::start(
        Arc::new(endpoint),
        None,
        Some(target),
        Scope::Loopback,
        repair,
    )
    .unwrap();
    let (_, repair, success, report) = worker.task.join().unwrap();
    (repair, success, report)
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
        match &self.listener {
            Some(listener) => self.hints.send(&hints.wire(listener.local_addr()?.port())),
            None => self.hints.send(&hints.presence_wire()),
        }
    }
}

/// A push this transport runs ([`Transport::hurry`]): the table it hands on, when it began, and
/// whether a round that began since caught a peer up on that table
/// (`crate::replica::Report::caught_up`).
struct Push {
    table: astream_wire::sync::Digest,
    since: Instant,
    reached: bool,
}

/// What one authenticated peer's rounds told this device since the last
/// [`Transport::take_contacts`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Contact {
    pub(super) actor: astream_wire::sync::Actor,
    /// Its latest authentication.
    pub(super) at: Instant,
    /// What the latest-authenticated round that said anything of its version said, or
    /// [`Said::Unknown`] while none did.
    pub(super) said: Said,
    /// When that round authenticated; [`Self::at`] while none did.
    pub(super) said_at: Instant,
    /// The reading of this Mac's clock against the peer's (`crate::clock`, F22) that the
    /// latest-authenticated round to take one took.
    pub(super) clock: Option<crate::clock::Reading>,
    /// When the round that took [`Self::clock`] authenticated. Rounds are ordered by this steady
    /// clock, never by the wall time a reading was taken at, which a clock set back moves back.
    pub(super) clock_at: Instant,
}
impl Contact {
    fn new(
        actor: astream_wire::sync::Actor,
        at: Instant,
        said: Said,
        clock: Option<crate::clock::Reading>,
    ) -> Self {
        Self {
            actor,
            at,
            said,
            said_at: at,
            clock,
            clock_at: at,
        }
    }
    /// Fold in `clock`, what a round authenticated at `at` read: it replaces the reading held
    /// unless an earlier-authenticated round took it, whichever round finished first. A round
    /// that read nothing keeps the reading held.
    fn read(&mut self, at: Instant, clock: Option<crate::clock::Reading>) {
        if let Some(clock) = clock
            && (self.clock.is_none() || at >= self.clock_at)
        {
            self.clock = Some(clock);
            self.clock_at = at;
        }
    }
    /// Fold in another round with this peer, authenticated at `at`. Rounds finish out of their
    /// authentication order: a crossed dial's loser authenticates a moment after the round it
    /// lost to and ends at once, refused before versions are traded. So a round that said
    /// nothing of its version never hides one that did, whichever finished first, and of two
    /// that said something the later-authenticated one is kept. While no round said anything,
    /// [`Self::said_at`] follows [`Self::at`].
    fn heard(&mut self, at: Instant, said: Said) {
        self.at = self.at.max(at);
        if said != Said::Unknown && (self.said == Said::Unknown || at >= self.said_at) {
            self.said = said;
            self.said_at = at;
        } else if self.said == Said::Unknown {
            self.said_at = self.at;
        }
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
    versions_at: Instant,
    /// Which peers speak `Bus`, read with the advertised builds ([`Hints::bus_speakers`]).
    bus: Bus,
    scope: Scope,
    /// The group's port: [`PORT`] on a LAN, an isolated test attachment's own on loopback.
    port: u16,
    contacts: Vec<Contact>,
    completed: VecDeque<RepairRound>,
    /// The push it runs, if any ([`Self::hurry`]).
    push: Option<Push>,
    /// The latest failed local link operation since the last refresh, bounded like a repair error.
    failure: Option<String>,
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
    /// Application rounds that completed since [`Self::take_finished_rounds`] (tests), including
    /// one that selected no table or deferred its table. A failed round is none of them.
    #[cfg(test)]
    finished_rounds: u64,
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

    /// Application rounds that completed since the last call, whichever table each took or none
    /// ([`Self::finished_rounds`]).
    #[cfg(test)]
    pub(super) fn take_finished_rounds(&mut self) -> u64 {
        std::mem::take(&mut self.finished_rounds)
    }

    /// This transport with `bus` as its `Bus` predicate, read at each seats refresh.
    pub(super) fn hearing_bus(mut self, bus: Bus) -> Self {
        self.bus = bus;
        self
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
            versions_at: now,
            bus: Bus::default(),
            scope,
            port,
            contacts: Vec::new(),
            completed: VecDeque::new(),
            push: None,
            failure: None,
            bound: false,
            #[cfg(test)]
            stand_in: None,
            #[cfg(test)]
            permission: None,
            #[cfg(test)]
            observed: std::collections::BTreeMap::new(),
            #[cfg(test)]
            finished_rounds: 0,
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
        self.failure = None;
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
                        self.fail_link(address.ip(), "listener bind", failure.error);
                        continue;
                    }
                }
            };
            self.bound |= listener.is_some();
            let hints = match Multicast::try_bind(source, GROUP, self.port, self.scope) {
                Ok(hints) => hints,
                Err(failure) => {
                    self.bound |= failure.reached;
                    self.fail_link(source.into(), "multicast bind", failure.error);
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
            match link.beacon(&self.hints) {
                Ok(()) => self.links.push(link),
                Err(error) => self.fail_link(source.into(), "discovery beacon", error),
            }
        }
    }

    fn fail_link(&mut self, source: IpAddr, operation: &str, error: io::Error) {
        self.failure = Some(bounded_error(format!("{operation} on {source}: {error}")));
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
        self.links.is_empty() && self.failure.is_some()
    }
    /// The latest failed local operation, including its source address and OS error when present.
    pub(super) fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }
    pub(super) fn peers(&self) -> usize {
        self.hints.count()
    }
    pub(super) fn unverified_peers(&self) -> usize {
        self.hints.unverified_count()
    }
    /// Authenticated peers since the last call, each at its latest contact and with what its
    /// latest round to say anything said of its version ([`Contact`]).
    pub(super) fn take_contacts(&mut self) -> Vec<Contact> {
        std::mem::take(&mut self.contacts)
    }

    /// Push `table`: dial every verified peer that does not speak `Bus` now ([`Hints::hurry`]); the
    /// rounds this side begins repair `table` first while the endpoint's push names it
    /// (`crate::replica::First`). How many peers it hurried; with none there is no push.
    pub(super) fn hurry(&mut self, table: astream_wire::sync::Digest, now: Instant) -> usize {
        let hurried = self.hints.hurry(now);
        self.push = (hurried > 0).then_some(Push {
            table,
            since: now,
            reached: false,
        });
        hurried
    }

    /// Whether a round that began after the push's [`Self::hurry`] succeeded and caught a peer up
    /// on its table (`crate::replica::Report::caught_up`).
    pub(super) fn reached(&self) -> bool {
        self.push.as_ref().is_some_and(|push| push.reached)
    }

    /// End the push: rounds keep their ordinary spacing again.
    pub(super) fn calm(&mut self) {
        self.push = None;
        self.hints.calm();
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
        let started = worker.started;
        match worker.task.join() {
            Ok((target, repair, success, report)) => {
                // Either side's round counts once it caught the peer up on the table: what this
                // side held as the push began is now the peer's too. A round that left some of
                // it for later, or deferred the table, does not.
                if let Some(push) = self.push.as_mut()
                    && success
                    && started >= push.since
                    && report
                        .application
                        .is_some_and(|app| app.caught_up && app.table == Some(push.table))
                {
                    push.reached = true;
                }
                if let Some((actor, at)) = repair.authenticated_contact() {
                    if let Some(contact) = self.contacts.iter_mut().find(|c| c.actor == actor) {
                        contact.heard(at, repair.said());
                        contact.read(at, repair.clock());
                    } else if self.contacts.len() < 64 {
                        self.contacts
                            .push(Contact::new(actor, at, repair.said(), repair.clock()));
                    }
                }
                #[cfg(test)]
                if let Some(report) = repair.application() {
                    self.finished_rounds += 1;
                    // A round that deferred its table read no inventory: it knows no count.
                    if let Some(table) = report.table
                        && report.local_known.is_some()
                    {
                        self.observed.insert(table, report.retained_after);
                    }
                }
                if let Some(address) = target {
                    let outcome =
                        Outcome::of(success, &repair, report.control.as_ref().ok().copied());
                    self.hints.finish(address, repair, outcome, now);
                }
                self.retain_report(report);
            }
            Err(_) => {
                if let Some(address) = worker.target {
                    self.hints
                        .finish(address, Repair::default(), Outcome::Refused, now);
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
        if now >= self.versions_at {
            self.versions_at = now + SEATS;
            let speaks = self.bus.lock().unwrap_or_else(|p| p.into_inner()).clone();
            self.hints.bus_speakers(match (speaks, &self.endpoint) {
                (Some(speaks), Some(endpoint)) => endpoint.bus_actors(&*speaks),
                _ => std::collections::BTreeSet::new(),
            });
            // This reads a local cache only: Commons never polls the fleet for versions.
            // Isolated attachments and unit tests must not consult the live lab.
            #[cfg(not(test))]
            if self.scope == Scope::Lan
                && let Some(state) = aterm_types::dirs::state_dir()
                && let Some(endpoint) = &self.endpoint
            {
                self.hints
                    .advertised_versions(endpoint.advertised_versions(&state), now);
            }
        }
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
        let mut after_accept = None;
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
                        Ok(connection) => {
                            self.workers.push(Worker::start(
                                endpoint.clone(),
                                Some(connection),
                                None,
                                self.scope,
                                Repair::default(),
                            )?);
                            after_accept = Some(index + 1);
                        }
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
            if let Err(error) = serviced {
                let local = self.links.remove(index).source;
                self.fail_link(local, "discovery service", error);
                for worker in &self.workers {
                    worker.stop_on(local);
                }
            } else {
                index += 1;
            }
        }
        // Keep the next accept behind the last successful listener. Idle
        // polls preserve that place instead of aligning with a busy link.
        if let Some(next) = after_accept
            && next < self.links.len()
        {
            self.links.rotate_left(next);
        }
        if now >= self.beacon {
            self.beacon = now + BEACON;
        }
        // Pending enrollment has no listener, so every worker may search for
        // an approver. A serving endpoint keeps capacity for incoming repair.
        let outgoing_limit = if self
            .endpoint
            .as_ref()
            .is_some_and(|endpoint| endpoint.pending())
        {
            WORKERS
        } else {
            WORKERS / 2
        };
        // Silent discovery candidates must not take every outgoing worker
        // while a completed peer waits for its next ordinary retry. Reserve
        // one outgoing worker for established endpoints; the others discover.
        // Before any peer is established, and while enrolling without a
        // listener, all outgoing capacity remains available for discovery.
        let discovering = self
            .workers
            .iter()
            .filter(|worker| {
                !worker.incoming
                    && worker
                        .target
                        .is_none_or(|address| !self.hints.established(address))
            })
            .count();
        let allow_discovery = outgoing_limit == WORKERS
            || !self.hints.has_established()
            || discovering < outgoing_limit - 1;
        if now >= self.dial
            && let Some(endpoint) = &self.endpoint
            && !self.links.is_empty()
            && self.workers.len() < WORKERS
            && self
                .workers
                .iter()
                .filter(|worker| !worker.incoming)
                .count()
                < outgoing_limit
            && let Some((address, repair)) = self.hints.next_allowed(now, allow_discovery)
        {
            match Worker::start(endpoint.clone(), None, Some(address), self.scope, repair) {
                Ok(worker) => self.workers.push(worker),
                Err(error) => {
                    self.hints
                        .finish(address, Repair::default(), Outcome::Busy, now);
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
