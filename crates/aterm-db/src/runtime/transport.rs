// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded transport workers. A generation joins every worker before its owner
//! lease can be released, including a dial still awaiting its first SYN reply.

use super::{
    announce::{self, Announced},
    census::{self, Event},
    discovery::{Hints, MemoryScope, Outcome, REMEMBERED_MAX, REMEMBERED_NAME, Remembered},
    report::{AuthenticatedConnection, RepairRound, bounded_failure},
};
use crate::{
    Result,
    control::{Endpoint, Failure, Repair, Side},
};
use aterm_net::lan::{Cancellation, Connection, Heard, Listener, Multicast, Scope};
use aterm_secrets::commons::hearing::Said;
use aterm_uds::ready::Doorbell;
use ring::rand::{SecureRandom, SystemRandom};
use std::{
    collections::VecDeque,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::fd::{AsFd as _, BorrowedFd},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 31, 84);
const PORT: u16 = aterm_uds::lan::here::COMMONS_PORT;
/// The most rounds peers that dialled in run here at once.
const INCOMING: usize = 2;
/// The most rounds this side dials that run at once ([`dial_room`]); an enrolling endpoint, which
/// has no listener, runs [`INCOMING`] more.
const ROUNDS: usize = 2;
/// How long a discovery dial holds a place among [`ROUNDS`] before its peer authenticates, while
/// this side has found no peer (CR2). One that has not authenticated by then is parked: it keeps
/// its socket and may still become a round, and the next dial starts beside it, as happy eyeballs
/// (RFC 8305) staggers connection attempts. An endpoint whose accept queue is full, or a host that
/// drops the SYN, then holds its own dial for up to its handshake or connect limit, but the next
/// dial for no longer than this. `ROUNDS × SPACING`: both places fill before the first parks.
const STAGGER: Duration = Duration::from_millis(500);
/// The most dials this side has open at once, parked ones included (CR2): with [`INCOMING`],
/// what one node's discovery costs the LAN. Seventy Macs that find no peer at once hold at most
/// 70 × `DIALS` = 420 connections, six per Mac, within each listener's backlog of 128; once a
/// Mac has found one, it holds at most `ROUNDS + 1`.
const DIALS: usize = 6;
/// The least time between two dials this side starts: at most four a second.
const SPACING: Duration = Duration::from_millis(250);
/// How often every link sends its beacon, after the first one each sends as it opens.
const BEACON: Duration = Duration::from_secs(5);
/// How often the seats' advertised builds and `Bus` speakers are read again.
const SEATS: Duration = Duration::from_secs(2);
/// The most datagrams one hearing turn reads from one link ([`Transport::receive_beacons`]),
/// dropped ones included: what bounds the work an unauthenticated multicast flood asks of a
/// turn. A serving loop hears at its polled round ([`super::POLL`] apart) and as its own beacon
/// comes due ([`BEACON`] apart), so a flood costs it at most 224 reads a second a link: 160 at
/// its rounds, 16 for each [`super::TICK`] a round spans, and 64 at its beacons. A lab of 85
/// peers beaconing every [`BEACON`] sends 34 between two rounds, and all 85 answer a Mac that
/// joins at once, so 119 wait at a round then, 238 where every peer is heard on two
/// attachments. What a turn leaves stays in the socket for the next, while the socket has
/// room: it asks its kernel for room for more than one turn's read of the largest datagrams
/// (`aterm_net::lan`'s multicast socket), and a beacon a full socket drops is sent again a
/// [`BEACON`] later.
const HEARD: usize = 320;
const _: () = assert!(HEARD as u128 == 16 * (super::POLL.as_millis() / super::TICK.as_millis()));

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
    /// When its round began, by the transport's clock: one that began after a push's ask counts
    /// for it ([`Push`]), and a dial holds a place among [`ROUNDS`] for [`STAGGER`] after it
    /// ([`dial_room`]).
    started: Instant,
    /// Raised once its round's peer authenticates ([`Repair::hear`]): the dial holds its place
    /// from then on ([`dial_room`]).
    heard: Arc<std::sync::atomic::AtomicBool>,
    /// Raised as its thread's round ends, before it rings the transport's doorbell ([`Ended`]):
    /// the tick that the ring wakes takes the round, whose thread may still be returning.
    ended: Arc<AtomicBool>,
}

/// A repair thread's end, told as its body returns or unwinds: it raises the worker's `ended`,
/// then rings the doorbell the serving loop waits on ([`Transport::waits_on`]), so the loop takes
/// the round at once rather than at its next polled round.
struct Ended {
    ended: Arc<AtomicBool>,
    bell: Option<Arc<Doorbell>>,
}
impl Drop for Ended {
    fn drop(&mut self) {
        self.ended.store(true, Ordering::Release);
        if let Some(bell) = &self.bell {
            bell.ring();
        }
    }
}

impl Worker {
    fn start(
        endpoint: Arc<Endpoint>,
        connection: Option<Connection>,
        target: Option<SocketAddr>,
        scope: Scope,
        bell: Option<Arc<Doorbell>>,
        repair: Repair,
        started: Instant,
    ) -> io::Result<Self> {
        let incoming = connection.is_some();
        let began_ms = aterm_time::unix_millis().unwrap_or(0);
        let heard = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut repair = repair;
        repair.hear(heard.clone());
        let channel = Arc::new(Mutex::new(Channel::default()));
        if let Some(connection) = &connection {
            channel
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .install(connection, began_ms)?;
        }
        let control = channel.clone();
        let ended = Arc::new(AtomicBool::new(false));
        let end = Ended {
            ended: ended.clone(),
            bell,
        };
        let task = thread::Builder::new()
            .name("commons-repair".into())
            .spawn(move || {
                let _end = end;
                // What this thread appends, its round ingested: no listener announces it again.
                announce::on_round();
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
                            let connection =
                                Connection::connect(target, scope, Duration::from_secs(3))
                                    .inspect_err(|error| {
                                        // The host there refused it: nothing listens at that
                                        // port (CR2). A timeout, or no route (this Mac's link
                                        // went, or that host sleeps), says nothing of it.
                                        if error.kind() == io::ErrorKind::ConnectionRefused {
                                            repair.disown();
                                        }
                                    })
                                    .map_err(dial_failure)?;
                            (connection, Side::Initiator)
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
                        // new connection rebuilds and revalidates the entire round; a
                        // redial that fails is this round's failure, not the probe's.
                        #[cfg(test)]
                        let redial = !endpoint.redial_keeps_first_attempt;
                        #[cfg(not(test))]
                        let redial = true;
                        if redial {
                            repair.redial();
                        }
                    }
                    unreachable!("the second exchange always returns")
                })();
                if let Err(error) = &result
                    && matches!(
                        error.repair_cause(),
                        aterm_types::network_state::RepairCause::LegacyPastCrossing
                    )
                {
                    // A protocol fence can refuse control before entering application repair:
                    // the dial scheduler hears its cause without a fabricated application
                    // exchange. The round's report carries it as the control failure's cause.
                    repair.fenced(error);
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
                    control: result.map_err(|error| bounded_failure(&Failure::of(&error))),
                    membership: repair.membership(),
                    membership_error: repair.membership_error().map(bounded_failure),
                    application: repair.application(),
                    application_error: repair.application_error().map(bounded_failure),
                    application_interrupted: repair.application_interrupted(),
                    application_deferral: repair.application_deferral().cloned().map(|mut why| {
                        why.local_error = why.local_error.map(super::report::bounded_error);
                        why
                    }),
                    application_batch_deferral: repair.application_batch_deferral().cloned().map(
                        |mut why| {
                            why.reason = super::report::bounded_error(why.reason);
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
            started,
            heard,
            ended,
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
        None,
        repair,
        Instant::now(),
    )
    .unwrap();
    let (_, repair, success, report) = worker.task.join().unwrap();
    (repair, success, report)
}

struct Link {
    source: IpAddr,
    listener: Option<Listener>,
    hints: Multicast,
    /// An isolated attachment's stand-in for a network that delivers this link's sends (tests):
    /// while it reads false, its beacon fails as a send to an unreachable network does
    /// ([`Self::beacon`]).
    #[cfg(test)]
    delivering: Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl Link {
    /// Send this link's beacon to its group: its listener's port where it serves repair, else
    /// opaque presence.
    fn beacon(&self, hints: &Hints) -> io::Result<()> {
        #[cfg(test)]
        if self
            .delivering
            .as_ref()
            .is_some_and(|delivering| !delivering.load(std::sync::atomic::Ordering::Acquire))
        {
            return Err(io::Error::new(
                io::ErrorKind::HostUnreachable,
                "the isolated attachment delivers no send",
            ));
        }
        match &self.listener {
            Some(listener) => self.hints.send(&hints.wire(listener.local_addr()?.port())),
            None => self.hints.send(&hints.presence_wire()),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// MUTANT of CR1, on for one test thread: an inbound round tells discovery nothing, as before.
    static UNVOUCHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Where a transport keeps the endpoints it verified across restarts ([`Hints::remember`]): a
/// private cache beside its endpoint's Commons custody keys, read only where it names the same
/// trust and seat. A file that is missing, damaged or another's reads as none, and a write that
/// fails stops nothing: the next start reads what it finds. On macOS the cache moves into
/// `.ubd` under that directory; custody keys and authoritative journals stay in place.
struct Memory {
    dir: std::path::PathBuf,
    name: String,
    scope: MemoryScope,
}

#[cfg(all(test, target_os = "macos"))]
#[path = "transport_quiet_memory_tests.rs"]
mod quiet_memory_tests;
impl Memory {
    fn of(endpoint: &Endpoint) -> Option<Self> {
        let (dir, trust, actor) = endpoint.discovery_memory()?;
        let mut scope: MemoryScope = [0; std::mem::size_of::<MemoryScope>()];
        scope[..16].copy_from_slice(&trust.network);
        scope[16..48].copy_from_slice(&trust.authority);
        scope[48..80].copy_from_slice(&trust.genesis);
        scope[80..].copy_from_slice(&actor);
        // This is only a dial hint. A refused migration leaves its data untouched and
        // disables persistence for this transport; authenticated discovery still runs.
        Self::prepare(dir, scope).ok()
    }
    fn prepare(dir: &std::path::Path, scope: MemoryScope) -> io::Result<Self> {
        let path =
            aterm_uds::quiet_state::prepare_file_cache(&dir.join(REMEMBERED_NAME), REMEMBERED_MAX)?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("discovery cache needs a filename"))?
            .to_owned();
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::other("discovery cache needs a parent"))?
            .to_owned();
        Ok(Self { dir, name, scope })
    }
    fn read(&self) -> Vec<Remembered> {
        aterm_uds::private_file::read_private(&self.dir, &self.name, REMEMBERED_MAX)
            .ok()
            .and_then(|bytes| Remembered::decode(&self.scope, &bytes))
            .unwrap_or_default()
    }
    fn write(&self, entries: &[Remembered]) {
        let bytes = Remembered::encode(&self.scope, entries);
        // One that fails is kept again at the next change or refresh ([`Hints::remembered`]).
        let _kept = aterm_uds::private_file::write_private(&self.dir, &self.name, &bytes);
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
    /// Where it keeps the endpoints it verified across restarts, if it serves a seat.
    memory: Option<Memory>,
    workers: Vec<Worker>,
    beacon: Instant,
    dial: Instant,
    rotate: Instant,
    versions_at: Instant,
    /// A turn has read its links' beacons ([`Self::turn`]): the first does, asked or not.
    listened: bool,
    /// The fleet book's signature verdicts as of the last seats read ([`Self::hear_versions`]).
    held: aterm_net::fleet_member::HeldSignatures,
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
    /// What each round's end, each push asked of the owner and each local append rings
    /// ([`Self::waits_on`]); none where the pipe could not be made, and the owner then ticks
    /// every [`super::TICK`].
    doorbell: Option<Arc<Doorbell>>,
    /// The tables this process appended to since the last turn took them ([`Self::announcer`]).
    announced: Arc<Announced>,
    /// A LAN attachment's stand-in (tests): while it reads false each address refreshed is one no
    /// bind reaches (its checks refuse it, as an attachment that changed since its address was
    /// read), and once it reads true it is the loopback ([`Self::standing_in`]).
    #[cfg(test)]
    stand_in: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// What each new link's delivery reads on an isolated test attachment
    /// ([`Link::delivering`]).
    #[cfg(test)]
    delivering: Option<Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(test)]
    observed: std::collections::BTreeMap<astream_wire::sync::Digest, usize>,
    /// Application rounds that completed since [`Self::take_finished_rounds`] (tests), including
    /// one that selected no table or deferred its table. A failed round is none of them.
    #[cfg(test)]
    finished_rounds: u64,
}
impl Transport {
    /// A transport on an isolated loopback attachment at `port` (tests), opened as a LAN one is:
    /// its one link binds and sends its first beacon while `delivering` reads true, and fails its
    /// sends while it reads false.
    #[cfg(test)]
    pub(super) fn isolated(
        endpoint: Option<Endpoint>,
        port: u16,
        delivering: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<Self> {
        let mut transport = Self::unopened(endpoint, Scope::Loopback, port)?;
        transport.delivering = delivering;
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
        let (mut token, mut seed) = ([0; 32], [0; 32]);
        for random in [&mut token, &mut seed] {
            SystemRandom::new()
                .fill(random)
                .map_err(|_| crate::Error::new("discovery randomness unavailable"))?;
        }
        let now = Instant::now();
        // What it verified before a restart is dialled first ([`Hints::remember`]).
        let memory = endpoint.as_ref().and_then(Memory::of);
        // Its beacons tag its lab, and it dials its lab's first and no other's, in an order of
        // its own (CR2).
        let mut hints = match &endpoint {
            Some(endpoint) => Hints::tagged(endpoint.beacon_key(), token, seed),
            None => Hints::new(token),
        };
        if let Some(memory) = &memory
            && let Some(now_ms) = aterm_time::unix_millis()
        {
            hints.remember(memory.read(), now, now_ms);
        }
        Ok(Self {
            endpoint: endpoint.map(Arc::new),
            links: Vec::new(),
            hints,
            memory,
            workers: Vec::new(),
            // Each link sends its first beacon as it opens ([`Self::refresh`]).
            beacon: now + BEACON,
            dial: now,
            rotate: now + Duration::from_secs(300),
            versions_at: now,
            listened: false,
            held: aterm_net::fleet_member::HeldSignatures::default(),
            bus: Bus::default(),
            scope,
            port,
            contacts: Vec::new(),
            completed: VecDeque::new(),
            push: None,
            failure: None,
            doorbell: Doorbell::new().ok().map(Arc::new),
            announced: Arc::default(),
            #[cfg(test)]
            stand_in: None,
            #[cfg(test)]
            delivering: None,
            #[cfg(test)]
            observed: std::collections::BTreeMap::new(),
            #[cfg(test)]
            finished_rounds: 0,
        })
    }
    /// Open a link on each address in `addresses` that has none. A link counts once its first
    /// beacon went out: where the network fails its sends (EHOSTUNREACH) though its sockets bound,
    /// the link is dropped here, failed, before any observation or peer's reachability counts it,
    /// and the next refresh tries again.
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
                match Listener::bind(*address, self.scope) {
                    Ok(listener) => Some(listener),
                    Err(error) => {
                        self.fail_link(address.ip(), "listener bind", error);
                        continue;
                    }
                }
            };
            let hints = match Multicast::bind(source, GROUP, self.port, self.scope) {
                Ok(hints) => hints,
                Err(error) => {
                    self.fail_link(source.into(), "multicast bind", error);
                    continue;
                }
            };
            let link = Link {
                source: address.ip(),
                listener,
                hints,
                #[cfg(test)]
                delivering: self.delivering.clone(),
            };
            match link.beacon(&self.hints) {
                Ok(()) => self.links.push(link),
                Err(error) => self.fail_link(source.into(), "discovery beacon", error),
            }
        }
    }

    fn fail_link(&mut self, source: IpAddr, operation: &str, error: io::Error) {
        self.failure = Some(super::report::bounded_error(format!(
            "{operation} on {source}: {error}"
        )));
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
        #[cfg(test)]
        let unvouched = || UNVOUCHED.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let unvouched = || false;
        let started = worker.started;
        // Each round's end counts once, by who dialled it.
        census::count(if worker.incoming {
            Event::RoundIn
        } else {
            Event::RoundOut
        });
        match worker.task.join() {
            Ok((target, repair, success, report)) => {
                let outcome = Outcome::of(success, &repair, report.control.as_ref().ok().copied());
                if outcome == (Outcome::Repaired { transferred: false }) {
                    census::count(Event::RoundQuiet);
                }
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
                    self.hints.finish(address, repair, outcome, now);
                } else if let Some(actor) = repair.authenticated_actor().filter(|_| success)
                    && let Some(connection) = &report.connection
                    && !unvouched()
                {
                    // A peer that dialled in and completed a round is up: its endpoints here
                    // drop their backoff as one this side dialled would, and its address leads
                    // its beacons; where the round moved nothing, this side's own dial of the
                    // peer waits the ordinary interval again (`Hints::vouch`).
                    let quiet = outcome == (Outcome::Repaired { transferred: false });
                    self.hints.vouch(connection.peer.ip(), actor, quiet, now);
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
    /// Read what waits in `link`'s multicast socket, at most [`HEARD`] datagrams: a hearing
    /// turn's read ([`Self::turn`]). A datagram the socket dropped is passed over, and the read
    /// goes on behind it. A beacon is a hint and grants nothing, however many are read.
    fn receive_beacons(
        link: &Link,
        links: &[Link],
        hints: &mut Hints,
        scope: Scope,
        beacon: Instant,
        now: Instant,
    ) -> io::Result<()> {
        let before = (hints.count(), hints.unverified_count());
        for _ in 0..HEARD {
            let (wire, source) = match link.hints.hear()? {
                Heard::Empty => break,
                Heard::Discarded => continue,
                Heard::Hint(wire, source) => (wire, source),
            };
            census::count(Event::Beacon);
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
    /// Its outgoing workers as dial admission reads them ([`dial_room`]).
    fn dialing(&self) -> Vec<Dialing> {
        self.workers
            .iter()
            .filter(|worker| !worker.incoming)
            .map(|worker| Dialing {
                started: worker.started,
                established: worker
                    .target
                    .is_some_and(|address| self.hints.established(address)),
                heard: worker.heard.load(Ordering::Acquire),
            })
            .collect()
    }

    /// The doorbell its rounds' ends ring, for the owner's pushes to ring too
    /// (`super::Board::serving`); none where the pipe could not be made.
    pub(super) fn doorbell(&self) -> Option<Arc<Doorbell>> {
        self.doorbell.clone()
    }

    /// The listener that tells this transport of its custody's local appends
    /// (`crate::table::store::listen`, [`Announced::listener`]): it notes the table and rings
    /// the doorbell, and the turn the ring wakes names the table to each peer's hint
    /// ([`Endpoint::announce`]) and dials every established peer ([`Hints::announce`]).
    pub(super) fn announcer(
        &self,
    ) -> impl Fn(&crate::table::store::Appended<'_>) + Send + Sync + 'static {
        self.announced.listener(self.doorbell.clone())
    }

    /// Take what rang the doorbell, before the tick that reads what it announced.
    pub(super) fn clear_doorbell(&self) {
        if let Some(bell) = &self.doorbell {
            bell.clear();
        }
    }

    /// What the serving loop waits on between ticks besides its stop and the clock
    /// ([`Self::next_due`]): the doorbell, which each round's end, each push and each local
    /// append rings ([`Self::announcer`]); and each
    /// link's listener while its incoming rounds leave room for one more, as [`Self::turn`]
    /// accepts only then (a connection it cannot take yet waits for a round's end, which rings).
    /// No multicast socket is among them: a beacon wakes nothing, and waits in its socket for
    /// the next hearing turn ([`Self::turn`]), the loop's polled round (`super::POLL`) at the
    /// latest. `None` without a doorbell: nothing would wake the loop for a round's end, so it
    /// keeps its tick.
    pub(super) fn waits_on(&self) -> Option<Vec<BorrowedFd<'_>>> {
        let bell = self.doorbell.as_ref()?;
        let accepts = self.endpoint.is_some()
            && self.workers.iter().filter(|worker| worker.incoming).count() < INCOMING;
        let mut fds = vec![bell.fd()];
        for link in &self.links {
            if accepts && let Some(listener) = &link.listener {
                fds.push(listener.as_fd());
            }
        }
        Some(fds)
    }

    /// When [`Self::turn`] next has work by the clock alone, as `now` reads it: its links' next
    /// beacon, its token's rotation, and its next dial ([`dial_due`]). A connection, a round's
    /// end, a push and a local append come without the clock and wake the loop through
    /// [`Self::waits_on`]. Its seats' read ([`SEATS`]), the beacons its peers sent, the expiry
    /// of endpoints and presence heard ([`Hints::expire_presence`]) and the remembered
    /// endpoints' refresh, which nothing waits for, ride on the owner's polled round
    /// (`super::POLL`, the same two seconds as [`SEATS`]), so each comes at most that much late.
    pub(super) fn next_due(&self, now: Instant) -> Instant {
        let mut due = self.rotate;
        if !self.links.is_empty() {
            due = due.min(self.beacon);
            if let Some(endpoint) = &self.endpoint
                && let Some(dial) = dial_due(
                    &self.hints,
                    &self.dialing(),
                    endpoint.pending(),
                    self.dial,
                    now,
                )
            {
                due = due.min(dial);
            }
        }
        due
    }

    /// Read the seats' advertised builds from the fleet book in `state`
    /// ([`Endpoint::advertised_versions`]). The book is read whole each time; an entry's
    /// signature is checked only where the entry changed since the last read ([`Self::held`]).
    fn hear_versions(&mut self, state: &std::path::Path, now: Instant) {
        if let Some(endpoint) = &self.endpoint {
            self.hints
                .advertised_versions(endpoint.advertised_versions(state, &mut self.held), now);
        }
    }

    /// [`Self::turn`], hearing (tests).
    #[cfg(test)]
    pub(super) fn tick(&mut self, now: Instant) -> io::Result<()> {
        self.turn(now, true)
    }

    /// One turn of the serving loop at `now`. `hear` says to read the beacons that wait in its
    /// links' sockets: the loop asks at its polled round (`super::POLL`). A turn also hears
    /// when its own beacon is due, and at a transport's first turn. Any other turn (a round's
    /// end, a connection, a dial that came due) leaves them in their sockets, where they wake
    /// nothing ([`Self::waits_on`]): a peer's beacon, and this side's answer to a new one, are
    /// acted on up to a polled round later than they came.
    pub(super) fn turn(&mut self, now: Instant, hear: bool) -> io::Result<()> {
        let hear = hear || now >= self.beacon || !std::mem::replace(&mut self.listened, true);
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
            {
                self.hear_versions(&state, now);
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
            let worker = &self.workers[index];
            if worker.ended.load(Ordering::Acquire) || worker.task.is_finished() {
                let worker = self.workers.swap_remove(index);
                self.finish(worker, now);
            } else {
                index += 1;
            }
        }
        if let Some((tables, unnamed)) = self.announced.take() {
            census::count(Event::Announce);
            if let Some(endpoint) = &self.endpoint {
                endpoint.announce(&tables, unnamed);
            }
            self.hints.announce(now);
        }
        if let Some(memory) = &self.memory
            && let Some(now_ms) = aterm_time::unix_millis()
            && let Some(remembered) = self.hints.remembered(now, now_ms)
        {
            memory.write(&remembered);
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
                if hear {
                    Self::receive_beacons(
                        link,
                        &self.links,
                        &mut self.hints,
                        self.scope,
                        self.beacon,
                        now,
                    )?;
                }
                // Unproved inbound peers cannot occupy the outbound repair budget, nor dials the
                // inbound one.
                if self.workers.iter().filter(|worker| worker.incoming).count() < INCOMING
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
                                self.doorbell.clone(),
                                Repair::default(),
                                now,
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
        if let Some(endpoint) = &self.endpoint
            && !self.links.is_empty()
        {
            let dialing = self.dialing();
            if let Some((address, repair)) = next_dial(
                &mut self.hints,
                &dialing,
                endpoint.pending(),
                &mut self.dial,
                now,
            ) {
                match Worker::start(
                    endpoint.clone(),
                    None,
                    Some(address),
                    self.scope,
                    self.doorbell.clone(),
                    repair,
                    now,
                ) {
                    Ok(worker) => self.workers.push(worker),
                    Err(error) => {
                        self.hints
                            .finish(address, Repair::default(), Outcome::Busy, now);
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }
}

/// An outgoing worker as dial admission reads it ([`dial_room`]).
#[derive(Clone, Copy, Debug)]
struct Dialing {
    /// When it was dialled ([`Worker::started`]).
    started: Instant,
    /// Its endpoint completed an authenticated round before ([`Hints::established`]).
    established: bool,
    /// Its peer authenticated in this round ([`Worker::heard`]).
    heard: bool,
}

#[cfg(test)]
thread_local! {
    /// MUTANT of CR2's stagger, on for one test thread: every outgoing worker holds its place
    /// among [`ROUNDS`] until its round ends, as before CR2, so slow endpoints that took every
    /// place hold the next dial for their whole connect or handshake limit.
    pub(super) static UNSTAGGERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of CR2's bound, on for one test thread: parked dials are not counted against
    /// [`DIALS`], so endpoints that never answer open a new dial every [`SPACING`].
    pub(super) static UNBOUNDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of CR2's first cut, on for one test thread: discovery dials park, and new ones start
    /// beside them, whether or not this side has found a peer, so a serving node drifts to
    /// [`DIALS`] open dials in steady state.
    pub(super) static PARKS_ALWAYS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this side may start a dial at `now` beside its outgoing workers `dialing`, and if it
/// may, whether it may go to an endpoint no round established yet (discovery).
///
/// A dial holds a place among [`ROUNDS`] while its endpoint is established (a round with a known
/// peer) or its peer has authenticated (`heard`), and for [`STAGGER`] after it starts. One that
/// has done neither by then is parked: it keeps its socket and may still become a round, and the
/// next dial starts beside it. At most [`DIALS`] are open at once, parked ones included.
///
/// Until this side finds a peer (none established, no dial heard), discovery dials go on beside
/// parked ones. Once it has found one, a serving endpoint keeps a place for its peers' rounds and
/// runs one discovery dial at a time, parked or not, as before CR2: it holds at most `ROUNDS + 1`
/// dials, and the dials its search left parked only drain, never keeping an established peer's
/// round out while fewer than [`DIALS`] are open. An enrolling endpoint (`pending`) has no
/// listener, so it runs [`INCOMING`] more rounds and reserves none.
fn dial_room(
    dialing: &[Dialing],
    pending: bool,
    has_established: bool,
    now: Instant,
) -> Option<bool> {
    #[cfg(test)]
    let (unstaggered, unbounded, parks_always) = (
        UNSTAGGERED.with(std::cell::Cell::get),
        UNBOUNDED.with(std::cell::Cell::get),
        PARKS_ALWAYS.with(std::cell::Cell::get),
    );
    #[cfg(not(test))]
    let (unstaggered, unbounded, parks_always) = (false, false, false);
    let rounds = if pending { ROUNDS + INCOMING } else { ROUNDS };
    if dialing.len() >= DIALS && !unbounded {
        return None;
    }
    let young =
        |dial: &Dialing| unstaggered || now.saturating_duration_since(dial.started) < STAGGER;
    let placed = dialing
        .iter()
        .filter(|dial| dial.established || dial.heard || young(dial))
        .count();
    if placed >= rounds {
        return None;
    }
    let found = (has_established || dialing.iter().any(|dial| dial.heard)) && !parks_always;
    let discovering = dialing
        .iter()
        .filter(|dial| !dial.established && !dial.heard)
        .count();
    Some(pending || !found || discovering < rounds - 1)
}

/// The next endpoint to dial at `now`, if [`dial_room`] leaves room beside `dialing` and the last
/// dial was [`SPACING`] ago (`dial` is when the next may start, moved on by one that does), with
/// the repair state its last round left ([`Hints::next_allowed`]).
fn next_dial(
    hints: &mut Hints,
    dialing: &[Dialing],
    pending: bool,
    dial: &mut Instant,
    now: Instant,
) -> Option<(SocketAddr, Repair)> {
    if now < *dial {
        return None;
    }
    let discovery = dial_room(dialing, pending, hints.has_established(), now)?;
    let next = hints.next_allowed(now, discovery)?;
    *dial = now + SPACING;
    Some(next)
}
/// When [`next_dial`] may next start a dial beside `dialing`, by the clock alone, as `now` reads
/// it: where [`dial_room`] leaves room, once the spacing since the last dial (`dial`) and the
/// earliest wait of an endpoint `hints` holds have passed ([`Hints::next_dial_at`]); where it
/// leaves none, once the youngest discovery dial holding a place parks ([`STAGGER`]). `None`
/// where only something the loop hears can make room or an endpoint (a round's end, a beacon).
/// It may be early, never late: a tick it wakes that starts no dial asks again after it.
fn dial_due(
    hints: &Hints,
    dialing: &[Dialing],
    pending: bool,
    dial: Instant,
    now: Instant,
) -> Option<Instant> {
    match dial_room(dialing, pending, hints.has_established(), now) {
        Some(discovery) => hints.next_dial_at(now, discovery).map(|at| at.max(dial)),
        None => dialing
            .iter()
            .filter(|dialed| !dialed.established && !dialed.heard)
            .map(|dialed| dialed.started + STAGGER)
            .filter(|parks| *parks > now)
            .min(),
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A dial's failure as a typed refusal: this Mac's LAN held for its broker
/// ([`aterm_uds::lan::Unavailable::of`], [`RepairCause::LanHeld`]), a dial that reached no peer
/// ([`aterm_uds::lan::reached_no_peer`], [`RepairCause::Unreachable`]), or another I/O failure
/// ([`RepairCause::Io`]). A peer's record names it only for a round whose first exchange
/// authenticated and whose redial then failed ([`Repair::redial`]): a first dial that fails has
/// no authenticated peer to record it against, and a LAN that waits for its broker from the start
/// is the observation's own detail.
///
/// [`RepairCause::LanHeld`]: aterm_types::network_state::RepairCause::LanHeld
/// [`RepairCause::Unreachable`]: aterm_types::network_state::RepairCause::Unreachable
/// [`RepairCause::Io`]: aterm_types::network_state::RepairCause::Io
pub(super) fn dial_failure(error: io::Error) -> crate::Error {
    use aterm_types::network_state::RepairCause;
    let cause = if aterm_uds::lan::Unavailable::of(&error).is_some() {
        RepairCause::LanHeld
    } else if aterm_uds::lan::reached_no_peer(&error) {
        RepairCause::Unreachable
    } else {
        RepairCause::Io
    };
    crate::Error::repair(&error.to_string(), cause)
}

#[cfg(test)]
mod stagger_tests;
#[cfg(test)]
mod tests;
