// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded local presence observation and sealed repair for approved Commons
//! devices. Unverified discovery never activates accounts or grants control.
//! One local owner serves native LAN attachments; other sessions take over
//! after it exits. Offline operation needs no peer or central service.

use crate::network_state::{ObservationHandle, Observer};
use crate::{Result, control::Endpoint};
use aterm_secrets::{
    client::{self, Paths},
    commons::{
        self,
        bootstrap::Bootstrap,
        hearing::{Ledger, Said},
        journal::Journal,
    },
};
use aterm_types::MutexExt;
use aterm_types::network_state::{MachineIdentity, Visibility};
use aterm_uds::stop::Stop;
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod announce;
pub(crate) mod census;
mod custody;
mod discovery;
#[cfg(any(test, debug_assertions))]
mod isolation;
mod owner;
mod place;
mod report;
mod transport;
mod vault;
pub use census::{Cadence, cadence};
pub(crate) use discovery::MAX_UNVERIFIED_ENDPOINTS;
#[cfg(test)]
pub(crate) use discovery::Outcome;
pub use report::{AuthenticatedConnection, RepairRound};
#[cfg(test)]
pub(crate) use transport::test_worker;
#[cfg(test)]
pub use vault::Passes;

/// Unforgeable outside this runtime: workers retain the owner lock until they finish,
/// and recheck its identity before a credential read or publication. Detached endpoints
/// and converge passes have no permit.
#[derive(Clone)]
pub(crate) struct CredentialOwner {
    lease: Arc<owner::Lease>,
    attempted: Arc<Mutex<std::collections::BTreeMap<astream_wire::sync::Digest, Instant>>>,
}

impl CredentialOwner {
    pub(crate) fn recheck(&self) -> Result<()> {
        self.lease.recheck()
    }

    pub(crate) fn try_offer(&self, offer: astream_wire::sync::Digest) -> bool {
        let mut attempted = self.attempted.lock().unwrap_or_else(|p| p.into_inner());
        attempted.retain(|_, at| at.elapsed() < aterm_secrets::need::transfer::LIFETIME);
        if attempted.len() >= crate::database::MAX_RECORDS {
            return false;
        }
        if attempted.contains_key(&offer) {
            return false;
        }
        attempted.insert(offer, Instant::now());
        true
    }

    pub(crate) fn offered_before(&self, offer: &astream_wire::sync::Digest) -> bool {
        self.attempted
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(offer)
            .is_some_and(|at| at.elapsed() < aterm_secrets::need::transfer::LIFETIME)
    }

    #[cfg(test)]
    pub(crate) fn for_test(paths: &Paths) -> Self {
        Self {
            lease: Arc::new(
                owner::Lease::acquire(&paths.dir.join("commons"))
                    .unwrap()
                    .unwrap(),
            ),
            attempted: Arc::default(),
        }
    }
}

/// A debug build's private Commons multicast port. With this variable set,
/// `ATERM_DEBUG_LOOPBACK_ONLY=1` is required and every attachment is loopback.
/// Invalid settings refuse the attachment; release builds ignore this variable.
pub const LOOPBACK_PORT_ENV: &str = "ATERM_DEBUG_COMMONS_LOOPBACK_PORT";

/// The native startup lab's preflight, before it starts any runtime or opens
/// custody. Only a debug build can acknowledge the guarded private attachment.
///
/// # Errors
/// No private port is configured, or its port or loopback guard is invalid.
#[cfg(any(test, debug_assertions))]
pub fn private_attachment_port() -> Result<u16> {
    isolation::port()?.ok_or_else(|| crate::Error::new("no private Commons attachment configured"))
}

#[cfg(any(test, debug_assertions))]
pub use discovery::db_discovery_replacement_environment;

/// How often a serving loop runs its polled checks ([`serve_paced`]), and how long a background
/// owner or a contended one waits between tries.
const POLL: Duration = Duration::from_secs(2);
/// The least time between two ticks of a serving loop ([`serve_paced`]): work a tick leaves (a
/// second connection queued at a listener, a deadline that comes again at once) waits this long,
/// so a loop never ticks more often than ten times a second, and only when something it waits on
/// happened.
const TICK: Duration = Duration::from_millis(100);

/// What one tick of a serving loop ([`serve_paced`]) came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    /// Serve on: the next tick comes when something the loop waits on happens, or by this
    /// deadline, the next its work keeps by the clock (`None`: none before the next round).
    Serve(Option<Instant>),
    /// The loop ends with `Ok(())`: what it serves moved under it (a source, a generation, an
    /// enrollment's witness, Commons control come into view).
    End,
}

/// A serving loop: `tick(state, now, round)` until `stopped`, ending at the first tick that ends
/// it ([`Flow::End`]) or fails. Between ticks it waits (`wait(state, floor, until)`) for what its
/// state waits on to happen ([`rest`]: a connection, a round's end, a push, an append this
/// process made to a table of the custody it serves), for its stop, or for
/// `until`: the earlier of the deadline the tick returned ([`Flow::Serve`]) and the next round;
/// never sooner than `floor`, one [`TICK`] after the tick began. An idle loop so wakes once a
/// [`POLL`], not ten times a second. A peer's beacon wakes nothing: the transport reads the
/// beacons that came since at each round ([`transport::Transport::turn`]).
///
/// `round` says that the loop's polled checks (the generation, lease, custody, configuration and
/// vault rechecks, the address refresh, the transport's read of its peers' beacons) are due in
/// this tick: the first [`POLL`] after the loop starts, then [`POLL`] after the last tick that
/// ran them returned, by `clock`, not after it
/// started. A round slower than the interval (a busy vault waited for, a slow interface read) so
/// still leaves the loop that long at rest before the next, instead of running round after round
/// with no rest between. The rest belongs to the loop, not to the round: no path through a tick
/// can leave it out. What nothing can wait on is read at each tick, and so at each round at the
/// latest: whether a window claims Commons (a lock, which raises no event), the remembered
/// endpoints' and the hearing and clock ledgers' keeping (minutes apart), and the lab journal's
/// flush (a minute apart).
fn serve_paced<S, E>(
    state: &mut S,
    clock: impl Fn() -> Instant,
    stopped: impl Fn() -> bool,
    mut wait: impl FnMut(&S, Instant, Instant),
    mut tick: impl FnMut(&mut S, Instant, bool) -> std::result::Result<Flow, E>,
) -> std::result::Result<(), E> {
    let mut due = clock() + POLL;
    while !stopped() {
        let now = clock();
        let round = now >= due;
        census::count(census::Event::OwnerTick);
        let flow = tick(state, now, round);
        if round {
            due = clock() + POLL;
        }
        let Flow::Serve(next) = flow? else {
            return Ok(());
        };
        wait(state, now + TICK, next.map_or(due, |next| next.min(due)));
    }
    Ok(())
}

/// A serving loop's transport step, which both loops' ticks run ([`serve_selected`],
/// [`serve_presence`]): the transport's turn at `now`, which reads the beacons its peers sent
/// where this tick is the loop's polled round (`round`, [`serve_paced`]). No beacon wakes the
/// loop, so a beacon is read at the next round's tick, and no later.
fn serve_turn(
    transport: &mut transport::Transport,
    now: Instant,
    round: bool,
) -> std::result::Result<(), Status> {
    transport.turn(now, round).map_err(|_| Status::Offline)
}

/// A serving loop's wait between ticks ([`serve_paced`]) on its transport: until a descriptor it
/// waits on is ready ([`transport::Transport::waits_on`]), `stop` is set, or `until` comes, and
/// never sooner than `floor`. A descriptor already ready (what the last tick left, or a ring
/// during it) waits for `floor` alone, as does a transport without a doorbell, which nothing
/// would wake for a round's end; a wait that fails rests a [`TICK`], so none spins. The doorbell
/// is cleared before the tick it wakes reads what it announced.
fn rest(stop: &Stop, transport: &transport::Transport, floor: Instant, until: Instant) {
    match transport.waits_on() {
        Some(fds) => rest_ready(
            floor,
            until,
            || aterm_uds::ready::any_now(&fds).map(|ready| !ready.is_empty()),
            |deadline| stop.wait_ready(&fds, Some(deadline)).map(drop),
            |duration| {
                stop.sleep(duration);
            },
            Instant::now,
        ),
        None => {
            stop.sleep(floor.saturating_duration_since(Instant::now()));
        }
    }
    transport.clear_doorbell();
}

fn rest_ready(
    floor: Instant,
    until: Instant,
    ready: impl FnOnce() -> io::Result<bool>,
    wait: impl FnOnce(Instant) -> io::Result<()>,
    sleep: impl FnOnce(Duration),
    clock: impl FnOnce() -> Instant,
) {
    match ready() {
        Ok(false) => {
            if wait(until.max(floor)).is_err() {
                sleep(TICK);
            } else {
                // Readiness may arrive before the floor: the poll deadline only
                // bounds its latest return. Preserve pacing after an early wake.
                sleep(floor.saturating_duration_since(clock()));
            }
        }
        Ok(true) => sleep(floor.saturating_duration_since(clock())),
        Err(_) => sleep(TICK),
    }
}
/// How often the contact ledger is kept, and a steward refreshes the roster from it.
const KEEP: Duration = Duration::from_secs(600);

/// What a transport open fails with where its network fails every open (tests).
#[cfg(test)]
const OPEN_FAILS: &str = "the test fails the transport's open";

#[derive(Default)]
struct Network {
    /// Public scheduling hints survive a rebuilt endpoint, scoped to its
    /// current trust and writer. A replacement captures fresh authority.
    repairs: crate::control::PeerCache,
    /// Authenticated contacts go to the machine's shared place; fixtures are inert.
    place: place::Publisher,
    /// The one-mesh transport is on (`aterm_types::mesh_switch::MESH`, read once, in
    /// [`Self::gated`]): a background owner no longer yields to a window, whose tables and
    /// pushes the mesh carries ([`Runtime::start_background`]). Off by default (tests flip it).
    mesh: bool,
    /// Which peers speak `Bus` ([`Runtime::speaks_bus`]): every transport this network opens
    /// holds their seats out of its dials and pushes. Shared with the runtime's handle.
    bus: transport::Bus,
    /// When the selected vault's reader passes ([`vault::Pace`]): on its timer, or each time a
    /// test asks (tests).
    vault: vault::Pace,
    #[cfg(test)]
    isolated: Option<auto_sync_tests::Attachment>,
    /// Counts each transport this network opened (tests).
    #[cfg(test)]
    opens: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// A LAN attachment's stand-in (tests): its address is no loopback one, so the runtime's first
    /// LAN use asks for the LAN broker, and its transport reaches the LAN only once the flag reads
    /// true ([`transport::Transport::standing_in`]).
    #[cfg(test)]
    stand_in: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Every transport open fails with [`OPEN_FAILS`] (tests).
    #[cfg(test)]
    open_fails: bool,
    /// The shared files' materializer of a runtime on this network, and its cadence (tests):
    /// none unless a test asks, so this crate's runtime labs write no shared file.
    #[cfg(test)]
    files: Option<crate::alab::upkeep::Cadence>,
    /// Each wait the owner took after a try that did not serve, in order (tests; [`Self::retry`]).
    #[cfg(test)]
    retries: Option<Arc<Mutex<Vec<Duration>>>>,
    /// The LAN broker's stand-in (tests): what [`Self::may_use`] hears in place of the broker's
    /// hello, the broker answering where there is none.
    #[cfg(test)]
    broker: Option<Arc<dyn Fn() -> std::result::Result<(), &'static str> + Send + Sync>>,
    /// Its Commons serving loop's polled checks never come due (tests): `round` in
    /// [`serve_selected`]'s tick never holds, so what it serves moves under it only by an event of
    /// its own, such as the round that enrolls it ([`witnessed`]).
    #[cfg(test)]
    unpolled: bool,
    /// The LAN broker answered this runtime once ([`Self::may_use`]).
    brokered: std::cell::Cell<bool>,
    /// What the runtime's first LAN use last waited for ([`Self::may_use`]).
    waits: std::cell::Cell<&'static str>,
}
impl Network {
    /// The live network.
    fn live() -> Self {
        Self {
            repairs: crate::control::PeerCache::default(),
            place: place::Publisher::live(),
            mesh: aterm_types::mesh_switch::MESH,
            bus: transport::Bus::default(),
            vault: vault::Pace::Interval,
            #[cfg(test)]
            isolated: None,
            #[cfg(test)]
            opens: None,
            #[cfg(test)]
            stand_in: None,
            #[cfg(test)]
            open_fails: false,
            #[cfg(test)]
            files: None,
            #[cfg(test)]
            retries: None,
            #[cfg(test)]
            broker: None,
            #[cfg(test)]
            unpolled: false,
            brokered: std::cell::Cell::new(false),
            waits: std::cell::Cell::new(""),
        }
    }

    /// The owner's `wait` on `stop` after a try that did not serve: waiting for the LAN broker,
    /// or ended with a status other than a rebuild. A wait longer than [`REPUBLISH`] is slept
    /// in slices of it with `republish` run between two slices, so the observation the owner
    /// publishes stays current for as long as the owner lives: a local reader's relay refuses
    /// a publication older than the reachable window, and a refused owner waits up to
    /// [`REFUSED_MAX`], twice that window.
    fn retry(&self, stop: &Stop, wait: Duration, republish: impl FnMut()) {
        #[cfg(test)]
        if let Some(retries) = &self.retries {
            retries.lock().unwrap_or_else(|p| p.into_inner()).push(wait);
        }
        sleep_sliced(stop, wait, REPUBLISH, republish);
    }

    /// Whether the runtime may open or refresh its transport on `candidates` now: on macOS once
    /// the root LAN broker answers a hello (`aterm_uds::lan::broker::hello`), which makes every
    /// LAN socket of aterm's (`docs/DESIGN-lan-broker-2026-11-05.md`); elsewhere at once. It is
    /// asked at each try until it once answered; a broker that goes away later fails the
    /// transport's binds, which the next refresh tries again. Only loopback candidates are no LAN
    /// use and ask nothing. A runtime waiting for the broker is [`Status::LanHeld`], with what it
    /// waits for kept for the owner's observation.
    fn may_use(&self, candidates: &[std::net::SocketAddr]) -> std::result::Result<(), Status> {
        if self.brokered.get() || candidates.iter().all(|address| address.ip().is_loopback()) {
            return Ok(());
        }
        #[cfg(test)]
        let answer = self.broker.as_ref().map_or(Ok(()), |broker| broker());
        #[cfg(not(test))]
        let answer = broker_answers();
        match answer {
            Ok(()) => {
                self.brokered.set(true);
                Ok(())
            }
            Err(why) => {
                self.waits.set(why);
                Err(Status::LanHeld)
            }
        }
    }

    /// Refresh `transport` on `candidates`.
    fn refresh(&self, transport: &mut transport::Transport, candidates: &[std::net::SocketAddr]) {
        transport.refresh(candidates);
    }

    fn addresses(&self) -> std::result::Result<Vec<std::net::SocketAddr>, Status> {
        // An isolated attachment is one loopback address, there whether or not its flag lets the
        // transport's sends through (tests).
        #[cfg(test)]
        if self.isolated.is_some() {
            return Ok(vec!["127.0.0.1:0".parse().unwrap()]);
        }
        #[cfg(test)]
        if self.stand_in.is_some() {
            return Ok(vec!["192.0.2.1:0".parse().unwrap()]);
        }
        addresses()
    }

    /// Open a transport on `candidates`.
    fn open(
        &self,
        endpoint: Option<Endpoint>,
        candidates: &[std::net::SocketAddr],
    ) -> Result<transport::Transport> {
        self.opening(endpoint, candidates)
            .map(|transport| transport.hearing_bus(self.bus.clone()))
    }

    fn opening(
        &self,
        endpoint: Option<Endpoint>,
        candidates: &[std::net::SocketAddr],
    ) -> Result<transport::Transport> {
        #[cfg(test)]
        if let Some(opens) = &self.opens {
            opens.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        #[cfg(test)]
        if self.open_fails {
            return Err(crate::Error::new(OPEN_FAILS));
        }
        #[cfg(test)]
        if let Some(up) = &self.stand_in {
            return transport::Transport::standing_in(endpoint, up.clone(), candidates);
        }
        #[cfg(test)]
        if let Some(attachment) = &self.isolated {
            return transport::Transport::isolated(
                endpoint,
                attachment.port,
                Some(attachment.delivering.clone()),
            );
        }
        match endpoint {
            Some(endpoint) => transport::Transport::open(endpoint, candidates),
            None => transport::Transport::presence_only(candidates),
        }
    }

    #[cfg(test)]
    fn observed(&self, transport: &mut transport::Transport) {
        if let Some(attachment) = &self.isolated {
            let rounds = transport.take_finished_rounds();
            attachment.observe(transport.application_observations(), rounds);
        }
    }
}

/// Local observation, not a claim of global convergence. Counts of peers are
/// address hints, not authenticated members.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Dormant,
    Disabled,
    Waiting,
    Pending,
    Offline,
    Active {
        interfaces: usize,
        hints: usize,
    },
    /// Local configuration or trust state refused the owner ([`Refusal`]). A window's owner tries
    /// again after a delay that doubles with each consecutive refusal, the background owner every
    /// [`POLL`] ([`Refusals`]).
    Refused(Refusal),
    /// A local writer or changed authenticated snapshot requires fresh admission.
    /// The owner retries after [`POLL`] without accumulating refusal backoff.
    Retrying(&'static str),
    /// The owner waits for the LAN broker before its first LAN use ([`Network::may_use`]): on
    /// macOS no broker answers yet (setup's Mac password installs it), so it opened no LAN socket.
    LanHeld,
}
impl Status {
    /// The refusal a host warns of: one that does not stand ([`Refusal::standing`]). Every other
    /// status, a standing refusal among them, is detail its host logs below a warning.
    #[must_use]
    pub fn news(&self) -> Option<&Refusal> {
        match self {
            Self::Refused(refusal) if !refusal.standing => Some(refusal),
            _ => None,
        }
    }
}

/// What refused an owner's try ([`Status::Refused`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// The step that refused, a bounded label that quotes no path, key or payload.
    pub step: &'static str,
    /// The error the step gave, which names the failed boundary without quoting payloads or keys
    /// ([`crate::Error`]).
    pub error: String,
    /// The previous refusal of this streak ([`Refusals`]) had the same step and error: the
    /// refusal stands, a fact its host has already reported, so a host warns of a refusal once
    /// per cause ([`Status::news`]) and its standing repeats at debug.
    pub standing: bool,
}
impl Refusal {
    fn new(step: &'static str, error: &crate::Error) -> Self {
        Self {
            step,
            error: error.to_string(),
            standing: false,
        }
    }
}
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.step, self.error)
    }
}

/// Preserve the failed boundary, its error and any typed local admission contention.
fn refusal<E: Into<crate::Error>>(why: &'static str) -> impl FnOnce(E) -> Status {
    move |error| {
        let error = error.into();
        if error.is_busy() {
            Status::Retrying(why)
        } else {
            Status::Refused(Refusal::new(why, &error))
        }
    }
}

/// The longest wait between two tries of an owner that keeps being refused.
const REFUSED_MAX: Duration = Duration::from_secs(30);

/// How often a waiting owner publishes its observation again ([`Network::retry`]): the period
/// a serving owner's observer publishes at, well inside the reachable window a reader holds a
/// publication to.
const REPUBLISH: Duration = Duration::from_secs(2);

/// Sleep `wait` on `stop` in slices of at most `every`, running `between` after each slice but
/// the last; a set stop ends the sleep at once and runs nothing more.
fn sleep_sliced(stop: &Stop, wait: Duration, every: Duration, mut between: impl FnMut()) {
    let mut left = wait;
    loop {
        let slice = left.min(every);
        if stop.sleep(slice) {
            return;
        }
        left -= slice;
        if left.is_zero() || slice.is_zero() {
            return;
        }
        between();
    }
}

/// How long an owner waits before it tries again after a try that did not serve. A refusal is a
/// local configuration or trust state, which does not lift by itself within milliseconds, and each
/// try rebuilds custody, an endpoint and a transport (held at its gate, it re-reads the
/// configuration and republishes what holds it), so a window's owner doubles its wait from
/// `TICK * 2` with each consecutive refusal, up to [`REFUSED_MAX`]. A try that served (reported
/// [`Status::Active`] or [`Status::Pending`]) for at least [`REFUSED_MAX`], or that ended to
/// rebuild (`Ok`), starts the doubling again. The background owner (`aterm node serve`) keeps
/// [`POLL`]: it looks for a window that opened only at its next try, and past its gate it holds
/// the observer while it waits, so a doubled wait would keep that window's Commons waiting for up
/// to [`REFUSED_MAX`].
///
/// A streak, in either mode, is one standing refusal while its cause holds: a refusal with the
/// step and error of the streak's previous refusal stands ([`Refusal::standing`]), whatever the
/// tries between reported, and the first refusal of a streak, or one with another cause, does not.
/// A disabled Commons also lifts the cause, though not the doubling: the owner was switched off,
/// so a refusal once it is switched on again is news.
#[derive(Debug)]
struct Refusals {
    /// Consecutive refused tries.
    streak: u32,
    /// The owner serves in the background, where every wait is [`POLL`].
    background: bool,
    /// The step and error of this streak's previous refusal, until Commons is disabled.
    cause: Option<(&'static str, String)>,
}
impl Refusals {
    const fn new(background: bool) -> Self {
        Self {
            streak: 0,
            background,
            cause: None,
        }
    }
    /// The wait after a try that ended with `status` (`None`: it ended to rebuild), having served
    /// for `served` (`None`: it never served), marking a refusal in `status` that stands. Typed
    /// local contention keeps [`POLL`] rather than increasing the persistent-refusal streak.
    fn after(&mut self, status: Option<&mut Status>, served: Option<Duration>) -> Duration {
        if status.is_none() || served.is_some_and(|served| served >= REFUSED_MAX) {
            self.streak = 0;
            self.cause = None;
        }
        let base = TICK * 2;
        match status {
            Some(Status::Refused(refusal)) => {
                let standing = self
                    .cause
                    .as_ref()
                    .is_some_and(|(step, err)| *step == refusal.step && err == &refusal.error);
                refusal.standing = standing;
                if !standing {
                    self.cause = Some((refusal.step, refusal.error.clone()));
                }
            }
            Some(Status::Retrying(_)) => return POLL,
            other => {
                if matches!(other, Some(Status::Disabled)) {
                    self.cause = None;
                }
                return if self.background { POLL } else { base };
            }
        }
        if self.background {
            return POLL;
        }
        let wait = base
            .saturating_mul(1 << self.streak.min(16))
            .min(REFUSED_MAX);
        self.streak = self.streak.saturating_add(1);
        wait
    }
}

/// What a push came to ([`Pusher::push_until`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pushed {
    /// A neighbour acknowledged the table on the mesh's relay ([`Pusher::relay`]), or a repair
    /// round that began after the ask caught a peer up on the table: the peer now holds every
    /// operation of it this runtime offered then (`crate::replica::Report::caught_up`), and its
    /// own rounds carry them on. Data transfers and the first quiet confirmation are followed at
    /// once; further quiet rounds keep ordinary spacing.
    Reached,
    /// This runtime had no serving owner, or that service ended before the push completed.
    /// The table remains available for the serving owner's next rounds.
    NotServing,
    /// This runtime knows no verified peer to dial: none, or only ones that speak `Bus`
    /// ([`Runtime::speaks_bus`]), whose relay did not acknowledge the table.
    NoPeers,
    /// The deadline came first.
    TimedOut,
}

impl Pushed {
    /// The outcome in a few words, as a log line names it.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Reached => "a peer took it",
            Self::NotServing => "Commons repair was unavailable or restarted",
            Self::NoPeers => "no verified peer off the mesh is known",
            Self::TimedOut => "no peer took it before the deadline",
        }
    }
}

/// Hands a table to this runtime's peers now ([`Runtime::pusher`]): what a host pushes when no
/// later round of its own may come (it is about to sleep or stop).
#[derive(Clone, Default)]
pub struct Pusher(Arc<Board>);

/// What answers a push on the mesh's relay ([`Pusher::relay`]).
type RelayAck = Arc<dyn Fn(astream_wire::sync::Digest, Instant) -> Option<Pushed> + Send + Sync>;

impl Pusher {
    /// Install `ack`, the mesh relay's answer to a push, on this pusher and every clone of it (the
    /// runtime's own included); a later call replaces it. `ack(table, deadline)` answers
    /// `Some(Pushed::Reached)` once a neighbour acknowledged the relay past `table`'s latest local
    /// operation, by `deadline` (half of what is left of the push's); any other answer, `None`
    /// included, hands the push to the repair rounds ([`Self::push_until`]) with the rest. It runs on the caller's
    /// thread with no lock of this runtime held.
    pub fn relay(
        &self,
        ack: Arc<dyn Fn(astream_wire::sync::Digest, Instant) -> Option<Pushed> + Send + Sync>,
    ) {
        *self.0.relay.lock().unwrap_or_else(|p| p.into_inner()) = Some(ack);
    }

    /// Hand the table `table` (a scope digest) to this runtime's peers now. The mesh's relay is
    /// asked first ([`Self::relay`]): its acknowledgement is [`Pushed::Reached`] at once, with no
    /// round. Otherwise every verified peer that does not speak `Bus` ([`Runtime::speaks_bus`]) is
    /// dialled at once and each round this runtime begins repairs `table` first where the peer
    /// holds it too. Waits until a round that began after this ask caught a peer up on it
    /// ([`Pushed::Reached`]), the runtime stops serving, or `deadline`, whichever is first; past
    /// the deadline the runtime ends the push. Concurrent callers of the rounds wait for the
    /// current caller within their own deadline; one table's completion never acknowledges
    /// another table. An unfinished call ends if its serving generation changes, even across a
    /// fast restart.
    #[must_use]
    pub fn push_until(&self, table: astream_wire::sync::Digest, deadline: Instant) -> Pushed {
        let relay = self
            .0
            .relay
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        // The relay has half of what is left: a neighbour that follows but does not ack leaves
        // the rounds the rest for the peers that do not speak `Bus`.
        let now = Instant::now();
        let relay_until = now + deadline.saturating_duration_since(now) / 2;
        if let Some(ack) = relay
            && ack(table, relay_until) == Some(Pushed::Reached)
        {
            return Pushed::Reached;
        }
        self.push_by_rounds(table, deadline)
    }

    /// [`Self::push_until`] on the repair rounds alone.
    fn push_by_rounds(&self, table: astream_wire::sync::Digest, deadline: Instant) -> Pushed {
        let board = &*self.0;
        let mut desk = board.desk.lock().unwrap_or_else(|p| p.into_inner());
        let generation = desk.generation;
        loop {
            if !desk.serving || desk.generation != generation {
                return Pushed::NotServing;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Pushed::TimedOut;
            }
            if !desk.pushing {
                break;
            }
            desk = board
                .changed
                .wait_timeout(desk, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        desk.pushing = true;
        desk.asked = desk.asked.saturating_add(1);
        let id = desk.asked;
        desk.settled = None;
        desk.ask = Some(Ask {
            id,
            table,
            deadline,
        });
        if let Some(bell) = &*board.bell.lock_or_recover() {
            bell.ring();
        }
        let result = loop {
            if let Some((settled, pushed)) = desk.settled
                && settled == id
            {
                break pushed;
            }
            if !desk.serving || desk.generation != generation {
                break Pushed::NotServing;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Pushed::TimedOut;
            }
            desk = board
                .changed
                .wait_timeout(desk, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        };
        if desk.ask.is_some_and(|ask| ask.id == id) {
            desk.ask = None;
        }
        desk.pushing = false;
        drop(desk);
        board.changed.notify_all();
        result
    }
}

/// One push asked of the owner ([`Pusher::push_until`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ask {
    id: u64,
    table: astream_wire::sync::Digest,
    deadline: Instant,
}

/// What a [`Pusher`] and its runtime's owner share.
#[derive(Default)]
struct Board {
    desk: Mutex<Desk>,
    changed: Condvar,
    /// The table the endpoint's rounds repair first while a push runs.
    first: crate::replica::First,
    /// The mesh relay's answer to a push, asked before any round ([`Pusher::relay`]).
    relay: Mutex<Option<RelayAck>>,
    /// The serving owner's doorbell, rung with each ask so it takes the ask at once
    /// ([`Board::serving`]).
    bell: Mutex<Option<Arc<aterm_uds::ready::Doorbell>>>,
}

#[derive(Default)]
struct Desk {
    /// The owner serves repair with an open transport ([`Board::serving`]).
    serving: bool,
    /// Each serving lifetime differs, including one that begins before a waiter wakes.
    generation: u64,
    /// One caller owns the pending or in-flight push until its result or deadline.
    pushing: bool,
    /// The last ask's id.
    asked: u64,
    /// The latest ask the owner has not taken.
    ask: Option<Ask>,
    /// The round whose table currently has priority. Its caller may already have timed out.
    active: Option<u64>,
    /// The last settled ask and its outcome, for that caller alone.
    settled: Option<(u64, Pushed)>,
}

impl Desk {
    fn start(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.serving = true;
        self.ask = None;
        self.active = None;
        // The previous caller keeps its result slot until it observes completion or the
        // changed generation. A replacement cannot erase a completed result before then.
        self.generation
    }

    fn stop(&mut self) {
        self.serving = false;
        self.ask = None;
        self.active = None;
    }
}

impl Board {
    /// The owner serves repair until the guard drops; then every waiting push hears it. Each ask
    /// rings `bell`, the owner's doorbell, while it serves.
    fn serving(&self, bell: Option<Arc<aterm_uds::ready::Doorbell>>) -> Serving<'_> {
        *self.bell.lock_or_recover() = bell;
        let generation = {
            let mut desk = self.desk.lock_or_recover();
            let generation = desk.start();
            *self.first.lock_or_recover() = None;
            generation
        };
        self.changed.notify_all();
        Serving {
            board: self,
            generation,
        }
    }

    /// The latest ask not taken yet, its table now the one the rounds repair first.
    fn take(&self) -> Option<Ask> {
        let mut desk = self.desk.lock_or_recover();
        let ask = desk.ask.take()?;
        desk.active = Some(ask.id);
        *self.first.lock_or_recover() = Some(ask.table);
        Some(ask)
    }

    /// Release this active round's priority and answer its caller if still current. A round
    /// superseded by a new one cannot change that new round's priority or result.
    fn settle(&self, id: u64, pushed: Pushed) {
        let mut desk = self.desk.lock_or_recover();
        if desk.active != Some(id) {
            return;
        }
        desk.active = None;
        *self.first.lock_or_recover() = None;
        if id == desk.asked {
            desk.settled = Some((id, pushed));
        }
        drop(desk);
        self.changed.notify_all();
    }
}

/// [`Board::serving`]'s guard.
struct Serving<'a> {
    board: &'a Board,
    generation: u64,
}

impl Drop for Serving<'_> {
    fn drop(&mut self) {
        let mut desk = self.board.desk.lock_or_recover();
        if desk.generation != self.generation {
            return;
        }
        desk.stop();
        *self.board.first.lock_or_recover() = None;
        drop(desk);
        *self.board.bell.lock_or_recover() = None;
        self.board.changed.notify_all();
    }
}

/// Whether a peer's fleet node key speaks `Bus` ([`Runtime::speaks_bus`]).
pub type SpeaksBus = Arc<dyn Fn(&[u8; 32]) -> bool + Send + Sync>;

/// Owning process lifetime. Dropping it stops and joins all network workers. A runtime a
/// process keeps in a static is never dropped: that process stops it with [`Self::shutdown`]
/// as it ends.
pub struct Runtime {
    stop: Arc<Stop>,
    /// Set once the owner thread has ended, its observer dropped and its history closed.
    ended: Arc<Stop>,
    task: Mutex<Option<JoinHandle<()>>>,
    observation: ObservationHandle,
    pusher: Pusher,
    /// Its network's `Bus` predicate ([`Self::speaks_bus`]).
    bus: transport::Bus,
}

/// Development observations keep their publication separate from live policy.
/// A temporary policy copy must disable writer recovery: its path cannot become
/// a persistent generation's authorization source.
#[cfg(any(test, debug_assertions))]
pub struct Diagnostic {
    pub observer_config: PathBuf,
    pub recover_writer: bool,
    /// The passes of the runtime's vault reader: each one these ask for, and none on its timer
    /// (tests).
    #[cfg(test)]
    pub vault: Arc<Passes>,
}

impl Runtime {
    /// Start default-on background repair using only this explicit secrets root
    /// and config file. Missing control permits opaque presence discovery and
    /// local observation history, never enrollment or data repair. Call only
    /// from a long-lived app/session host. On macOS its first LAN use waits for the LAN broker
    /// ([`Status::LanHeld`]).
    pub fn start(
        paths: Paths,
        config: PathBuf,
        observe: impl Fn(Status) + Send + 'static,
    ) -> Result<Self> {
        Self::start_on(paths, config, observe, Network::live())
    }

    /// Start the same automatic owner with bounded completed-round diagnostics.
    /// The callback runs on the owner thread after the worker has joined, with
    /// no custody/control/table journal or channel/cache guard held. It receives
    /// no payloads or keys and should return promptly. Lifetime ownership and
    /// observation leases remain held until this runtime stops. Its first LAN use waits for the
    /// LAN broker ([`Status::LanHeld`]).
    pub fn start_with_reports(
        paths: Paths,
        config: PathBuf,
        observe: impl Fn(Status) + Send + 'static,
        reports: impl Fn(RepairRound) + Send + 'static,
    ) -> Result<Self> {
        Self::start_on_with_reports(paths, config, observe, Network::live(), reports)
    }

    /// Observe the production runtime under explicit development instrumentation.
    /// Observer placement changes no policy check or transport-owner lease.
    /// Disable recovery for a temporary policy; use the persistent live policy
    /// when testing recovery with a separate observer.
    #[cfg(any(test, debug_assertions))]
    pub fn start_diagnostic(
        paths: Paths,
        config: PathBuf,
        diagnostic: Diagnostic,
        observe: impl Fn(Status) + Send + 'static,
        reports: impl Fn(RepairRound) + Send + 'static,
    ) -> Result<Self> {
        let network = Network::live();
        #[cfg(test)]
        let network = Network {
            vault: vault::Pace::Asked(diagnostic.vault),
            ..network
        };
        Self::start_mode_with_sources(
            paths,
            config,
            (diagnostic.observer_config, diagnostic.recover_writer),
            observe,
            network,
            false,
            reports,
        )
    }

    /// Start background Commons repair and steward enrollment for `aterm node serve` (C22).
    /// Before the one-mesh transport (`aterm_types::mesh_switch::MESH` off), it owns local
    /// observation and transport while no interactive `aterm` window runs, yields both when a
    /// window claims [`owner::WindowClaim`], and resumes once the last window exits. The claim
    /// is a file lock taken in another process, which raises no event the serving loop can
    /// wait on, so the loop reads it at each tick it takes for another reason, and at its
    /// polled round at the latest: it yields within [`POLL`] (2 s) of the claim, while the
    /// window waits with `OpenFailure::Owned` and tries again. With the mesh on it yields to no window: the node service is the one owner, and a
    /// window's runtime waits for its lease. Its first LAN use waits for the LAN broker as
    /// [`Self::start`]'s does.
    pub fn start_background(
        paths: Paths,
        config: PathBuf,
        observe: impl Fn(Status) + Send + 'static,
    ) -> Result<Self> {
        Self::start_background_on(paths, config, observe, Network::live())
    }

    /// [`Self::start_background`] with [`Self::start_with_reports`]'s completed-round reports.
    pub fn start_background_with_reports(
        paths: Paths,
        config: PathBuf,
        observe: impl Fn(Status) + Send + 'static,
        reports: impl Fn(RepairRound) + Send + 'static,
    ) -> Result<Self> {
        Self::start_mode_with_reports(paths, config, observe, Network::live(), true, reports)
    }

    fn start_on(
        paths: Paths,
        config: PathBuf,
        observe: impl Fn(Status) + Send + 'static,
        network: Network,
    ) -> Result<Self> {
        Self::start_on_with_reports(paths, config, observe, network, |_| {})
    }

    fn start_background_on(
        paths: Paths,
        config: PathBuf,
        observe: impl Fn(Status) + Send + 'static,
        network: Network,
    ) -> Result<Self> {
        Self::start_mode_with_reports(paths, config, observe, network, true, |_| {})
    }

    fn start_on_with_reports(
        paths: Paths,
        config: PathBuf,
        observe: impl Fn(Status) + Send + 'static,
        network: Network,
        reports: impl Fn(RepairRound) + Send + 'static,
    ) -> Result<Self> {
        Self::start_mode_with_reports(paths, config, observe, network, false, reports)
    }

    fn start_mode_with_reports(
        paths: Paths,
        config: PathBuf,
        observe: impl Fn(Status) + Send + 'static,
        network: Network,
        background: bool,
        reports: impl Fn(RepairRound) + Send + 'static,
    ) -> Result<Self> {
        let observer_config = config.clone();
        Self::start_mode_with_sources(
            paths,
            config,
            (observer_config, true),
            observe,
            network,
            background,
            reports,
        )
    }

    fn start_mode_with_sources(
        paths: Paths,
        config: PathBuf,
        (observer_config, recover_writer): (PathBuf, bool),
        observe: impl Fn(Status) + Send + 'static,
        network: Network,
        background: bool,
        reports: impl Fn(RepairRound) + Send + 'static,
    ) -> Result<Self> {
        let stop = Arc::new(Stop::default());
        let worker = stop.clone();
        let ended = Arc::new(Stop::default());
        let ending = Ended(ended.clone());
        let observation = ObservationHandle::default();
        let published = observation.clone();
        let pusher = Pusher::default();
        let board = pusher.0.clone();
        let bus = network.bus.clone();
        // A background owner yields to a window only before the one-mesh transport.
        let yields = background && !network.mesh;
        #[cfg(test)]
        let files = network.files;
        #[cfg(not(test))]
        let files = Some(crate::alab::upkeep::Cadence::LIVE);
        let task = thread::Builder::new()
            .name("commons-owner".into())
            .spawn(move || {
                // Declared first, so dropped last: after the observer, whose drop writes what
                // is pending and closes its history.
                let _ending = ending;
                // The shared files' materializer lives as long as this owner, whatever each
                // try at serving comes to: `[commons]` off, no membership and an unfinished
                // succession are states a reader needs recorded, and in none of them does
                // this thread serve. It rests while another process owns Commons.
                let elsewhere = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let _files = files.and_then(|cadence| {
                    crate::alab::upkeep::Materializer::start(
                        paths.clone(),
                        config.clone(),
                        cadence,
                        elsewhere.clone(),
                    )
                });
                let mut window_claim = None;
                let mut previous = None;
                let mut report = |status: Status| {
                    elsewhere.store(
                        status == Status::Waiting,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    if previous.as_ref() != Some(&status) {
                        observe(status.clone());
                        previous = Some(status);
                    }
                };
                // One streak of refusals ([`Refusals`]).
                let mut refusals = Refusals::new(background);
                while !worker.is_set() {
                    if yields && owner::window_claimed(&observer_config) {
                        let publication = waiting_publication(
                            &paths,
                            &config,
                            Visibility::Waiting,
                            "An aterm window owns Commons repair while it runs",
                        );
                        published.replace_publication(publication);
                        report(Status::Waiting);
                        worker.sleep(POLL);
                        continue;
                    }
                    if !background && window_claim.is_none() {
                        // Keep the working background owner until this window owns Commons.
                        // Retain priority across observer contention afterward.
                        window_claim = owner::WindowClaim::acquire(&observer_config).ok();
                    }
                    let mut history = match Observer::open(&observer_config, published.clone()) {
                        Ok(history) => history,
                        Err(crate::network_state::OpenFailure::Unavailable) => {
                            Observer::unavailable(&observer_config, published.clone())
                        }
                        Err(crate::network_state::OpenFailure::Owned) => {
                            published.replace_publication(waiting_publication(
                                &paths,
                                &config,
                                Visibility::Waiting,
                                "Another local aterm owns network observation; its live snapshot is unavailable",
                            ));
                            report(Status::Waiting);
                            worker.sleep(if background { POLL } else { TICK * 2 });
                            continue;
                        }
                    };
                    if !background && window_claim.is_none() {
                        window_claim = owner::WindowClaim::acquire(&observer_config).ok();
                    }
                    while !worker.is_set() {
                        let mut serving = None;
                        let mut served = serve_mode_with_recovery(
                            (&paths, &config, recover_writer),
                            yields.then_some(observer_config.as_path()),
                            &worker,
                            &mut |status| {
                                if matches!(status, Status::Active { .. } | Status::Pending) {
                                    serving.get_or_insert_with(Instant::now);
                                }
                                report(status);
                            },
                            &mut history,
                            &network,
                            &reports,
                            &board,
                        );
                        let wait = refusals.after(
                            served.as_mut().err(),
                            serving.map(|since: Instant| since.elapsed()),
                        );
                        if let Err(status) = served {
                            history.unconfirm_local();
                            if yields
                                && status == Status::Waiting
                                && owner::window_claimed(&observer_config)
                            {
                                report(Status::Waiting);
                                break;
                            }
                            let (visibility, detail) = match status {
                                Status::Disabled => {
                                    (Visibility::Disabled, "Network visibility disabled")
                                }
                                Status::Waiting => (
                                    Visibility::Waiting,
                                    "Another local process owns Commons repair",
                                ),
                                Status::Retrying(_) => (
                                    Visibility::Waiting,
                                    "Local Commons admission is busy or changed; retrying",
                                ),
                                Status::LanHeld => (Visibility::Waiting, network.waits.get()),
                                _ => (Visibility::Unavailable, "Network observation unavailable"),
                            };
                            let waiting = status == Status::Waiting;
                            let mut publish = || {
                                if waiting
                                    && let Some(publication) = transport_publication(&paths)
                                {
                                    // Keep the owner's original sample time and kernel identity.
                                    // This process's config relay must not forward it recursively.
                                    published.replace_publication(publication);
                                } else {
                                    history.observe(
                                        visibility,
                                        0,
                                        detail,
                                        aterm_time::unix_millis().unwrap_or(0),
                                        Instant::now(),
                                    );
                                }
                            };
                            publish();
                            report(status);
                            // Published again through a long wait, so a local reader finds
                            // this observer alive for as long as it is.
                            network.retry(&worker, wait, publish);
                        }
                    }
                    history.observe(
                        Visibility::Unavailable,
                        0,
                        "Network observation stopped",
                        aterm_time::unix_millis().unwrap_or(0),
                        Instant::now(),
                    );
                }
            })?;
        Ok(Self {
            stop,
            ended,
            task: Mutex::new(Some(task)),
            observation,
            pusher,
            bus,
        })
    }
    pub fn stop(mut self) {
        self.join();
    }
    /// Stop this runtime through a shared reference, for a process that keeps it in a static
    /// and is ending: ask the owner thread to stop and wait up to `within` for it to end, which
    /// drops its observer, so the network history holds every transition that observer saw and
    /// says it is closed. `false` when the thread had not ended in time; it stops when it next
    /// reads the request, and a process that exits first leaves the history open, as one that
    /// was killed does. A second call, and the later drop, find nothing to stop.
    pub fn shutdown(&self, within: Duration) -> bool {
        self.stop.set();
        if !self.ended.sleep(within) {
            return false;
        }
        self.finish();
        true
    }
    /// Pure shared observation; readers clone the value off the render path.
    pub fn network_observation(&self) -> ObservationHandle {
        self.observation.clone()
    }
    /// What hands a table to this runtime's peers now ([`Pusher::push_until`]).
    pub fn pusher(&self) -> Pusher {
        self.pusher.clone()
    }
    /// Install `pred`, which says whether a peer's fleet node key speaks `Bus` (the mesh carries
    /// its tables); a later call replaces it. Within one discovery refresh (2 s), each current
    /// seat whose node key `pred` names is held out of this runtime's dials and pushes: a push
    /// counts and hurries only the other peers. Such a peer stays known (its beacons, presence and
    /// contacts as before), its incoming rounds are still served, and it is dialled again once
    /// `pred` no longer names it. A peer this side cannot yet bind to a seat (no signed fleet
    /// advertisement and no authenticated round) may take one round first. With no predicate
    /// every peer is dialled, as before the mesh.
    pub fn speaks_bus(&self, pred: SpeaksBus) {
        *self.bus.lock().unwrap_or_else(|p| p.into_inner()) = Some(pred);
    }
    fn join(&mut self) {
        self.stop.set();
        self.finish();
    }
    /// Join the owner thread, which was asked to stop, and publish that observation stopped.
    fn finish(&self) {
        let task = self.task.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(task) = task {
            let _ = task.join();
            let mut observation = self.observation.snapshot();
            observation.visibility = Visibility::Unavailable;
            observation.detail = "Network observation stopped".into();
            observation.revision = observation.revision.saturating_add(1);
            observation.observed_at_ms = aterm_time::unix_millis().unwrap_or(0);
            for peer in &mut observation.peers {
                peer.reachability = aterm_types::network_state::Reachability::Unknown;
            }
            self.observation.replace(observation);
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.join();
    }
}

/// Sets its stop when the owner thread ends, however it ends ([`Runtime::shutdown`]).
struct Ended(Arc<Stop>);

impl Drop for Ended {
    fn drop(&mut self) {
        self.0.set();
    }
}

/// What an owner with no observer of its own publishes: the live owner's relayed snapshot when
/// one answers, else the retained history with `visibility` and `detail`.
fn waiting_publication(
    paths: &Paths,
    config: &Path,
    visibility: Visibility,
    detail: &str,
) -> crate::network_state::Publication {
    read_live_publication(paths, config).unwrap_or_else(|_| {
        let mut retained = crate::network_state::read_history(config).unwrap_or_default();
        retained.visibility = visibility;
        retained.detail = detail.into();
        crate::network_state::Publication::new(retained, Instant::now())
    })
}

/// Find the current generation's transport owner using read-only custody observation.
/// No default configuration path or historical generation can stand in for that owner.
fn transport_publication(paths: &Paths) -> Option<crate::network_state::Publication> {
    let selected =
        crate::generation::observed_paths_until(paths, crate::admission::deadline()).ok()?;
    crate::network_state::read_transport_publication(&selected.dir.join("commons")).ok()
}

/// Prefer the vault's active transport owner, including one with another GUI configuration.
/// The configuration relay still supplies discovery-only and held-owner observations.
/// This reads local custody and a same-user relay without starting repair or contacting peers.
///
/// # Errors
///
/// Neither the selected vault's transport owner nor the configuration's observer supplies an
/// accepted live publication.
pub fn read_live_publication(
    paths: &Paths,
    config: &Path,
) -> std::io::Result<crate::network_state::Publication> {
    if let Some(publication) = transport_publication(paths) {
        Ok(publication)
    } else {
        crate::network_state::read_live_publication(config)
    }
}

/// Whether `config`'s `[commons] enabled` lets this Mac take part in the shared data (on when
/// the file or the table is absent). A read: the converge pass's `sync.shared-data` row asks it.
///
/// # Errors
///
/// Returns an error when `config` is not an owned safe file or not UTF-8 TOML, or when its
/// `commons` entry is not a table, names an unknown key, holds a non-boolean `enabled`, or an
/// `exclude_tables` that is not a bounded array of distinct canonical table ids.
pub fn enabled(config: &Path) -> Result<bool> {
    Ok(crate::catalog::policy::Policy::read(config)?.enabled)
}

#[cfg(test)]
fn serve(
    paths: &Paths,
    config: &Path,
    stop: &Stop,
    report: &mut impl FnMut(Status),
    history: &mut Observer,
    network: &Network,
    reports: &impl Fn(RepairRound),
) -> std::result::Result<(), Status> {
    serve_mode(
        (paths, config),
        None,
        stop,
        report,
        history,
        network,
        reports,
        &Board::default(),
    )
}

#[cfg(test)]
#[expect(
    clippy::too_many_arguments,
    reason = "the owner's paths, yield, stop, reporters, history, network and its pushes' board"
)]
fn serve_mode(
    (paths, config): (&Paths, &Path),
    window_config: Option<&Path>,
    stop: &Stop,
    report: &mut impl FnMut(Status),
    history: &mut Observer,
    network: &Network,
    reports: &impl Fn(RepairRound),
    board: &Board,
) -> std::result::Result<(), Status> {
    serve_mode_with_recovery(
        (paths, config, true),
        window_config,
        stop,
        report,
        history,
        network,
        reports,
        board,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the owner's paths, yield, stop, reporters, history, network and its pushes' board"
)]
fn serve_mode_with_recovery(
    (paths, config, recover_writer): (&Paths, &Path, bool),
    window_config: Option<&Path>,
    stop: &Stop,
    report: &mut impl FnMut(Status),
    history: &mut Observer,
    network: &Network,
    reports: &impl Fn(RepairRound),
    board: &Board,
) -> std::result::Result<(), Status> {
    // A background owner yields to a window's claim before it reads its configuration, so one
    // its configuration refuses still yields.
    if window_config.is_some_and(owner::window_claimed) {
        return Err(Status::Waiting);
    }
    if !enabled(config).map_err(refusal("configuration"))? {
        return Err(Status::Disabled);
    }
    let paths = if recover_writer {
        crate::generation::advance_while(paths, config, &|| stop.is_set())
            .map_err(refusal("writer recovery"))?
    } else {
        paths.clone()
    };
    if stop.is_set() {
        return Ok(());
    }
    serve_selected(
        (&paths, config),
        window_config,
        stop,
        report,
        history,
        network,
        reports,
        board,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the owner's paths, yield, stop, reporters, history, network and its pushes' board"
)]
fn serve_selected(
    (paths, config): (&Paths, &Path),
    window_config: Option<&Path>,
    stop: &Stop,
    report: &mut impl FnMut(Status),
    history: &mut Observer,
    network: &Network,
    reports: &impl Fn(RepairRound),
    board: &Board,
) -> std::result::Result<(), Status> {
    history.unconfirm_local();
    if window_config.is_some_and(owner::window_claimed) {
        return Err(Status::Waiting);
    }
    if !enabled(config).map_err(refusal("configuration"))? {
        return Err(Status::Disabled);
    }
    let source_snapshot = match commons::observe_until(paths, crate::admission::deadline()) {
        Ok(commons::Observation::Visible(snapshot)) => snapshot,
        _ => {
            return serve_presence(
                (paths, config),
                window_config,
                stop,
                report,
                history,
                network,
            );
        }
    };
    let commons_dir = paths.dir.join("commons");
    let lease = owner::Lease::acquire(&commons_dir)
        .map_err(refusal("owner lease"))?
        .ok_or(Status::Waiting)?;
    let lease = Arc::new(lease);
    // Locals drop in reverse order: transport closes/drains before lease unlock.
    let deadline = crate::admission::deadline();
    let bootstrap = Bootstrap::cached_until(paths, deadline)
        .map_err(refusal("secrets custody"))?
        .ok_or(Status::Dormant)?;
    let request = bootstrap.request().map_err(refusal("device request"))?;
    let endpoint = match Endpoint::from_bootstrap_until(&bootstrap, deadline)
        .map_err(refusal("control endpoint"))?
    {
        Some(endpoint) => endpoint
            .with_commons_until(paths.clone(), config.to_owned(), deadline)
            .map_err(refusal("Commons subscriptions"))?,
        None => Endpoint::pending_from_bootstrap_until(&bootstrap, paths.clone(), deadline)
            .map_err(refusal("enrollment endpoint"))?,
    };
    let mut endpoint = endpoint
        .with_repair_cache(&network.repairs)
        .with_credential_owner(
            CredentialOwner {
                lease: lease.clone(),
                attempted: Arc::default(),
            },
            config.to_owned(),
        );
    endpoint.prefer(board.first.clone());
    let pending = endpoint.pending();
    let custody = Arc::new(
        custody::Custody::capture(paths, bootstrap.journal_dir())
            .map_err(refusal("custody capture"))?,
    );
    let current_snapshot =
        match commons::observe_until(paths, deadline).map_err(refusal("vault observation"))? {
            commons::Observation::Visible(snapshot) => snapshot,
            commons::Observation::Unavailable(_) => return Err(Status::Dormant),
        };
    if !current_snapshot.same_observation(&source_snapshot) {
        return Ok(());
    }
    // Observation failures never gate independently authorized data repair.
    let mut confirmation = Confirmation::first(
        history,
        (paths, request.binding(), bootstrap.trust()),
        crate::admission::deadline(),
    );
    let candidates = network.addresses()?;
    network.may_use(&candidates)?;
    let mut transport = network
        .open(Some(endpoint), &candidates)
        .map_err(refusal("transport"))?;
    // Created after the lease, dropped before it: only the actual transport owner
    // publishes at the shared vault address, even with distinct GUI configurations.
    let mut owner_relay = history.transport_relay(&commons_dir).ok();
    let vault_reader = if pending {
        None
    } else {
        vault::Reader::start(paths, config, &lease, &custody, network.vault.clone())
            .map_err(refusal("vault reader"))?
    };
    let mut hearing = (!pending).then(|| Hearing::open(&bootstrap));
    let mut clocks = if pending {
        None
    } else {
        ClockStore::open(&bootstrap)
    };
    // The lab journal's flusher, on a thread of its own; an enrolling owner has no writer to
    // publish anything with. (The shared files' materializer is the owner thread's, not this
    // serving loop's: it passes in the states this function never reaches.)
    let mut lab = (!pending).then(|| crate::alab::upkeep::Upkeep::new(Instant::now()));
    let mut refresh = Instant::now() + Duration::from_secs(30);
    // While it serves, each append this process makes to a table of this custody rings the owner,
    // which dials every established peer for that table rather than leaving the record to each
    // pair's turn. An enrolling owner has no tables. Where no listener can be had, and for what
    // another process appends, the record goes at each pair's turn, as it did.
    let _announcing = (!pending)
        .then(|| crate::table::store::claim_dir_of(&bootstrap).ok())
        .flatten()
        .and_then(|claims| crate::table::store::listen(claims, transport.announcer()));
    let result = {
        let _serving = board.serving(transport.doorbell());
        let mut pushing: Option<Ask> = None;
        serve_paced(
            &mut transport,
            Instant::now,
            || stop.is_set(),
            |transport, floor, until| rest(stop, transport, floor, until),
            |transport, now, round| {
                if window_config.is_some_and(owner::window_claimed) {
                    return Err(Status::Waiting);
                }
                #[cfg(test)]
                let round = round && !network.unpolled;
                if round {
                    crate::generation::guard(paths, false).map_err(refusal("generation guard"))?;
                    if now >= refresh {
                        if crate::generation::refresh_required(paths)
                            .map_err(refusal("generation refresh"))?
                        {
                            return Ok(Flow::End);
                        }
                        refresh = now + Duration::from_secs(30);
                    }
                    lease.recheck().map_err(refusal("owner lease"))?;
                    if owner_relay.as_ref().is_none_or(|relay| !relay.is_running()) {
                        drop(owner_relay.take());
                        owner_relay = history.transport_relay(&commons_dir).ok();
                    }
                    custody.recheck().map_err(refusal("custody"))?;
                    if !enabled(config).map_err(refusal("configuration"))? {
                        return Err(Status::Disabled);
                    }
                    confirmation.again(history, now);
                    match commons::observe_until_settled(paths, now + Duration::from_millis(200))
                        .map_err(refusal("vault observation"))?
                    {
                        commons::Observation::Visible(snapshot) => {
                            if !snapshot.same_observation(&source_snapshot) {
                                return Ok(Flow::End);
                            }
                        }
                        commons::Observation::Unavailable(commons::Unavailable::Busy) => {}
                        commons::Observation::Unavailable(_) => return Err(Status::Dormant),
                    }
                    if pending && witnessed(&bootstrap)? {
                        return Ok(Flow::End);
                    }
                    let candidates = network.addresses()?;
                    network.may_use(&candidates)?;
                    network.refresh(transport, &candidates);
                }
                if let Some(ask) = board.take() {
                    pushing = Some(ask);
                    if transport.hurry(ask.table, now) == 0 {
                        pushing = None;
                        transport.calm();
                        board.settle(ask.id, Pushed::NoPeers);
                    }
                }
                serve_turn(transport, now, round)?;
                if let Some(ask) = pushing {
                    let settled = if transport.reached() {
                        Some(Pushed::Reached)
                    } else {
                        (now >= ask.deadline).then_some(Pushed::TimedOut)
                    };
                    if let Some(pushed) = settled {
                        pushing = None;
                        transport.calm();
                        board.settle(ask.id, pushed);
                    }
                }
                let completed: Vec<_> = transport.reports().collect();
                // The round that enrolls this device brings its witness into view: an enrolling
                // owner asks for it at the tick one of its rounds completes its control exchange,
                // not only at its next polled round.
                let witness_due = pending && completed.iter().any(|round| round.control.is_ok());
                if let Some(lab) = &mut lab {
                    lab.tick(paths, now);
                }
                #[cfg(test)]
                network.observed(transport);
                let contacts = transport.take_contacts();
                if let Some(hearing) = &mut hearing {
                    hearing.tick(paths, &bootstrap, &contacts, now);
                }
                // The Commons journal and this custody's access, read once this tick for every
                // reader of its contacts, and only by the first that needs them.
                let resolved = std::cell::OnceCell::new();
                if let Some(clocks) = &mut clocks {
                    clocks.tick(&bootstrap, &resolved, &contacts, now);
                }
                observe_contacts(
                    &bootstrap,
                    &resolved,
                    history,
                    contacts
                        .into_iter()
                        .map(|contact| (contact.actor, contact.at))
                        .collect(),
                    &completed,
                    &network.place,
                    || (aterm_time::unix_millis().unwrap_or(0), Instant::now()),
                );
                for completed in completed {
                    reports(completed);
                }
                observe_transport(
                    history,
                    transport,
                    if pending {
                        ObservationMode::Enrollment
                    } else {
                        ObservationMode::Repair
                    },
                );
                report(if transport.interfaces() == 0 {
                    Status::Offline
                } else if pending {
                    Status::Pending
                } else {
                    Status::Active {
                        interfaces: transport.interfaces(),
                        hints: transport.peers(),
                    }
                });
                if witness_due && witnessed(&bootstrap)? {
                    return Ok(Flow::End);
                }
                let due = transport.next_due(now);
                Ok(Flow::Serve(Some(
                    pushing.map_or(due, |ask| ask.deadline.min(due)),
                )))
            },
        )
    };
    // Every exit drains and joins workers before the owner lease is released,
    // including cancellation or a configuration/source transition.
    drop(vault_reader);
    transport.stop();
    for completed in transport.reports() {
        reports(completed);
    }
    drop(owner_relay);
    result
}

/// Whether an enrolling owner's custody now holds its membership witness, in the membership log or
/// the control journal: what it serves moved under it, and it rebuilds as a member. Asked at each
/// polled round, and at the tick one of its rounds completes its control exchange, which is what
/// brings the witness into view.
fn witnessed(bootstrap: &Bootstrap) -> std::result::Result<bool, Status> {
    if bootstrap
        .log_witness()
        .map_err(refusal("log witness"))?
        .is_some()
    {
        return Ok(true);
    }
    let journal = aterm_secrets::commons::journal::Journal::read_existing(
        bootstrap.journal_dir(),
        bootstrap.trust(),
    )
    .map_err(refusal("control journal"))?;
    Ok(bootstrap
        .witness_in(&journal)
        .map_err(refusal("control witness"))?
        .is_some())
}

/// This device's contact ledger ([`Ledger`]): every authenticated peer and the version it
/// reported, and an unbroken watch while peers are heard. Kept whenever a peer or a version is
/// new, and every [`KEEP`], when an admin first refreshes the roster from it (silent members
/// leave, returned ones come back). Failures never stop repair: a ledger not kept holds the
/// C26 gate.
struct Hearing {
    ledger: Ledger,
    changed: bool,
    kept: Option<Instant>,
}
impl Hearing {
    fn open(bootstrap: &Bootstrap) -> Self {
        Self {
            ledger: Ledger::read(bootstrap),
            changed: true,
            kept: None,
        }
    }
    fn tick(
        &mut self,
        paths: &Paths,
        bootstrap: &Bootstrap,
        contacts: &[transport::Contact],
        now: Instant,
    ) {
        let Some(now_ms) = aterm_time::unix_millis() else {
            return;
        };
        self.changed |= record_contacts(&mut self.ledger, contacts, now_ms, now);
        let due = self
            .kept
            .is_none_or(|kept| now.saturating_duration_since(kept) >= KEEP);
        if !self.changed && !due {
            return;
        }
        self.ledger.watch(now_ms);
        if due {
            self.kept = Some(now);
            refresh(paths, bootstrap, &mut self.ledger, now_ms);
        }
        // A write that fails is tried again at the next change or [`KEEP`], not every tick.
        self.changed = false;
        let _ = self.ledger.write(bootstrap);
    }
}

/// Record `contacts` in `ledger`, each instant placed on the wall clock that read `now_ms` at
/// `now`. Whether this changed a version or added a member.
fn record_contacts(
    ledger: &mut Ledger,
    contacts: &[transport::Contact],
    now_ms: u64,
    now: Instant,
) -> bool {
    let mut changed = false;
    for contact in contacts {
        // What the peer said, at the time of the round that said it; then its latest
        // contact, which can be a later round that ended before versions were traded.
        changed |= ledger.record(
            contact.actor,
            contact_time(now_ms, now, contact.said_at),
            contact.said,
        );
        if contact.at != contact.said_at {
            changed |= ledger.record(
                contact.actor,
                contact_time(now_ms, now, contact.at),
                Said::Unknown,
            );
        }
    }
    changed
}

/// This device's readings of its clock against each peer's (`crate::clock`, F22): the latest per
/// Commons machine, resolved through the current members as [`observe_contacts`] does, kept in
/// `clock.state` in the claim directory when a reading's class or the standing changes, or every
/// [`KEEP`] once a reading changed. Every claims read and write judges that file on its own clock.
/// A write that fails stops nothing: the claims read the file they find, or none.
struct ClockStore {
    dir: std::path::PathBuf,
    clocks: crate::clock::Clocks,
}
impl ClockStore {
    fn open(bootstrap: &Bootstrap) -> Option<Self> {
        let dir = crate::clock::dir(bootstrap)?;
        Some(Self {
            clocks: crate::clock::Clocks::open(&dir),
            dir,
        })
    }

    fn tick(
        &mut self,
        bootstrap: &Bootstrap,
        resolved: &std::cell::OnceCell<Option<Resolved>>,
        contacts: &[transport::Contact],
        now: Instant,
    ) {
        let read: Vec<_> = contacts
            .iter()
            .filter_map(|contact| Some((contact.actor, contact.clock?, contact.clock_at)))
            .collect();
        if read.is_empty() && !self.clocks.due(now, KEEP) {
            return;
        }
        let Some(now_ms) = aterm_time::unix_millis() else {
            return;
        };
        let Some(Resolved { access, .. }) =
            resolved.get_or_init(|| Resolved::read(bootstrap)).as_ref()
        else {
            return;
        };
        for (actor, reading, at) in read {
            if let Some(member) = access
                .members()
                .iter()
                .find(|member| member.binding().actor() == actor)
            {
                self.clocks
                    .observe(*member.binding().machine(), reading, at);
            }
        }
        let members = crate::clock::members(access.members().iter().map(|m| m.binding()));
        let me = *access.local().binding().machine();
        self.clocks
            .tick(&self.dir, &members, &me, now_ms, now, KEEP);
    }
}

/// The Commons journal and this custody's access in it, as the readers of one tick's contacts
/// resolve their peers ([`ClockStore::tick`], [`observe_contacts`]).
struct Resolved {
    journal: Journal,
    access: aterm_secrets::commons::roster::Access,
}
impl Resolved {
    /// The journal and the access it gives this custody, revalidated now; none where the journal
    /// cannot be read, the custody is no longer valid, or it grants no access.
    fn read(bootstrap: &Bootstrap) -> Option<Self> {
        let journal = Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).ok()?;
        bootstrap.revalidate_custody().ok()?;
        let access = bootstrap.access_in(&journal).ok()??;
        Some(Self { journal, access })
    }
}

/// On an admin, refresh the Commons roster from `ledger`; elsewhere, nothing.
fn refresh(paths: &Paths, bootstrap: &Bootstrap, ledger: &mut Ledger, now_ms: u64) {
    if bootstrap.is_log_backed().unwrap_or(false) {
        return;
    }
    let Ok(admin) = client::admin_cached(paths) else {
        return;
    };
    // A busy writer only defers this to the next [`KEEP`]: repair never waits on it.
    if let Ok(mut journal) = Journal::open_existing_until(
        bootstrap.journal_dir(),
        bootstrap.trust(),
        crate::admission::deadline(),
    ) {
        let _ = admin.refresh_commons_roster(bootstrap, &mut journal, ledger, now_ms);
    }
}

/// Resolve authenticated contacts, their socket paths and successful outbound endpoints
/// through current membership before publishing identity. Clock reads keep freshness tests
/// independent of speed.
fn observe_contacts(
    bootstrap: &Bootstrap,
    resolved: &std::cell::OnceCell<Option<Resolved>>,
    history: &mut Observer,
    contacts: Vec<(astream_wire::sync::Actor, Instant)>,
    completed: &[RepairRound],
    place: &place::Publisher,
    clock: impl Fn() -> (u64, Instant),
) {
    let endpoints: Vec<_> = completed
        .iter()
        .filter_map(RepairRound::authenticated_endpoint)
        .collect();
    let exchanges: Vec<_> = completed
        .iter()
        .filter_map(RepairRound::matching_inventory)
        .collect();
    let attempts: Vec<_> = completed
        .iter()
        .filter_map(RepairRound::observation)
        .collect();
    if contacts.is_empty() && endpoints.is_empty() && exchanges.is_empty() && attempts.is_empty() {
        return;
    }
    let mut answers = Vec::new();
    if let Some(Resolved { journal, access }) =
        resolved.get_or_init(|| Resolved::read(bootstrap)).as_ref()
    {
        for (actor, authenticated_at) in contacts {
            if let Some(member) = access
                .members()
                .iter()
                .find(|m| m.binding().actor() == actor)
            {
                let (now_ms, now) = clock();
                history.contact(
                    machine_identity(member.binding()),
                    contact_time(now_ms, now, authenticated_at),
                    authenticated_at,
                );
            }
        }
        for (actor, endpoint) in endpoints {
            if let Some(member) = access
                .members()
                .iter()
                .find(|m| m.binding().actor() == actor)
            {
                history.endpoint(&machine_identity(member.binding()), endpoint);
            }
        }
        for (actor, attempt, sampled_at) in attempts {
            if let Some(member) = access
                .members()
                .iter()
                .find(|member| member.binding().actor() == actor)
            {
                history.repair(&machine_identity(member.binding()), attempt, sampled_at);
            }
        }
        for exchange in exchanges {
            if let Some(candidate) =
                accounts_exchange_candidate(bootstrap, journal, access, exchange)
            {
                history.accounts_exchange(candidate);
            }
        }
        for round in completed {
            if let Some(peer) = access
                .members()
                .iter()
                .find(|member| Some(member.binding().actor()) == round.actor)
                && let Some(answer) = place::answer(round, access.local().binding(), peer.binding())
            {
                answers.push(answer);
            }
        }
    }
    place.note(answers);
}

/// Remap a completed immutable witness through this custody's current Commons binding.
/// A changed local context may not relabel an older exchange as this owner's evidence.
fn accounts_exchange_candidate(
    bootstrap: &Bootstrap,
    journal: &Journal,
    access: &aterm_secrets::commons::roster::Access,
    exchange: crate::replica::MatchingInventory,
) -> Option<crate::network_state::account_exchange::Candidate> {
    let scope =
        crate::catalog::application_scope(bootstrap.trust(), "accounts", "proposals").ok()?;
    if exchange.scope() != scope || exchange.local_actor() != access.local().binding().actor() {
        return None;
    }
    let mut control = [0; 64];
    if access.is_log_backed() {
        if !access.is_current(journal) {
            return None;
        }
        control[..32].copy_from_slice(access.head());
    } else {
        let aterm_secrets::commons::journal::State::Ready { head, .. } = journal.state() else {
            return None;
        };
        control[..32].copy_from_slice(&head);
    }
    control[32..].copy_from_slice(access.basis());
    if exchange.control_observation() != control {
        return None;
    }
    let peer = access
        .members()
        .iter()
        .find(|member| member.binding().actor() == exchange.peer_actor())?;
    if peer.binding().machine() == access.local().binding().machine() {
        return None;
    }
    let inventory = exchange.inventory();
    if inventory.count > crate::replica::MAX_RECORDS {
        return None;
    }
    Some(crate::network_state::account_exchange::Candidate {
        observation: aterm_types::network_state::AccountsExchangeObservation {
            scope_digest: scope.digest(),
            local_actor: exchange.local_actor(),
            control_observation: exchange.control_observation(),
            inventory_count: u64::try_from(inventory.count).ok()?,
            inventory_fingerprint: inventory.fingerprint,
            peer: machine_identity(peer.binding()),
            sampled_at_ms: exchange.observed_at_ms(),
        },
        sampled_at: exchange.sampled_at(),
    })
}

fn machine_identity(binding: &aterm_secrets::commons::binding::Binding) -> MachineIdentity {
    MachineIdentity {
        machine_id: *binding.machine(),
        incarnation: *binding.incarnation(),
        label: None,
    }
}

/// How long a serving session waits after its confirmation of its own identity as its network
/// history's observer was refused before it tries again ([`Confirmation::again`]).
const RECONFIRM: Duration = Duration::from_secs(15);

/// The most a confirmation tried again within a serving session waits on the home's lineage
/// ([`Confirmation::again`]): the serving loop's polled round waits on it, so a vault writer that
/// holds the state lock for long holds repair back at most this long each [`RECONFIRM`].
const RECONFIRM_PASS: Duration = Duration::from_secs(2);

/// A serving session's confirmation of this owner's own identity as its network history's
/// observer ([`Observer::local`]): tried as the session starts ([`Self::first`]), and, while it
/// is refused, again at a polled round once [`RECONFIRM`] has passed ([`Self::again`]). A refusal
/// can come of a passing failure: the lineage that proves a writer successor's earlier seats
/// ([`succeeded_machines`]) can meet a vault writer that holds the state lock (its reads wait for
/// one only until their deadline, its denial check not at all), and a session can begin while
/// the subscriber still applies the vault version that restarted it. Tried once, such a refusal
/// lasted the whole session (until the vault next changed, a refresh came or the owner stopped),
/// the history unavailable and every contact dropped, so this Mac heard none of its peers. A
/// confirmation that succeeded is not tried again; one refused for a trust context or a missing
/// journal is tried again at no cost, as only another machine's history reads the lineage.
struct Confirmation<'a> {
    paths: &'a Paths,
    binding: &'a aterm_secrets::commons::binding::Binding,
    trust: aterm_secrets::commons::journal::Trust,
    confirmed: bool,
    next: Instant,
}

impl<'a> Confirmation<'a> {
    /// Confirm `binding` (this owner's, under `paths`, in the `trust` context) as `history`'s
    /// observer, its lineage read by `deadline` at most.
    fn first(
        history: &mut Observer,
        (paths, binding, trust): (
            &'a Paths,
            &'a aterm_secrets::commons::binding::Binding,
            aterm_secrets::commons::journal::Trust,
        ),
        deadline: Instant,
    ) -> Self {
        let mut confirmation = Self {
            paths,
            binding,
            trust,
            confirmed: false,
            next: Instant::now(),
        };
        confirmation.confirm(history, Instant::now(), deadline);
        confirmation
    }

    /// Confirm again at `now` where the last confirmation was refused and [`RECONFIRM`] has
    /// passed since, the lineage read within [`RECONFIRM_PASS`].
    fn again(&mut self, history: &mut Observer, now: Instant) {
        if !self.confirmed && now >= self.next {
            self.confirm(history, now, now + RECONFIRM_PASS);
        }
    }

    fn confirm(&mut self, history: &mut Observer, now: Instant, deadline: Instant) {
        self.confirmed = history
            .local(machine_identity(self.binding), self.trust, || {
                succeeded_machines(self.paths, self.binding, deadline)
            })
            .is_ok();
        self.next = now + RECONFIRM;
    }
}

/// The Commons machines of the writer generations `selected`'s home succeeds on this disk,
/// oldest first, as its authenticated lineage proves them from the home it began at
/// ([`crate::generation::lineage_until`] of [`crate::generation::predecessors`]' last), where
/// that lineage ends at `current`, this owner's own binding (a newer succession may have
/// completed since `selected` was named): the machines whose network history this owner's
/// observer takes up ([`Observer::local`]). None for a home that succeeds none, which reads no
/// lineage, and none while the lineage cannot be proved (a vault writer holding the state lock
/// included) or ends at another binding, so such a history stays refused until a later
/// confirmation proves it ([`Confirmation`]).
fn succeeded_machines(
    selected: &Paths,
    current: &aterm_secrets::commons::binding::Binding,
    deadline: Instant,
) -> Vec<[u8; 16]> {
    let Some(original) = crate::generation::predecessors(selected)
        .ok()
        .and_then(|mut predecessors| predecessors.pop())
    else {
        return Vec::new();
    };
    let Ok(lineage) = crate::generation::lineage_until(&original, deadline) else {
        return Vec::new();
    };
    match lineage.split_last() {
        Some((own, earlier)) if own == current => {
            earlier.iter().map(|binding| *binding.machine()).collect()
        }
        _ => Vec::new(),
    }
}

fn contact_time(now_ms: u64, observed: Instant, authenticated: Instant) -> u64 {
    let elapsed = u64::try_from(
        observed
            .saturating_duration_since(authenticated)
            .as_millis(),
    )
    .unwrap_or(u64::MAX);
    now_ms.saturating_sub(elapsed)
}

fn serve_presence(
    (paths, config): (&Paths, &Path),
    window_config: Option<&Path>,
    stop: &Stop,
    report: &mut impl FnMut(Status),
    history: &mut Observer,
    network: &Network,
) -> std::result::Result<(), Status> {
    if window_config.is_some_and(owner::window_claimed) {
        return Err(Status::Waiting);
    }
    let candidates = network.addresses()?;
    network.may_use(&candidates)?;
    let mut transport = network
        .open(None, &candidates)
        .map_err(|_| Status::Offline)?;
    let mut refresh = Instant::now() + Duration::from_secs(30);
    report(Status::Dormant);
    serve_paced(
        &mut transport,
        Instant::now,
        || stop.is_set(),
        |transport, floor, until| rest(stop, transport, floor, until),
        |transport, now, round| {
            if window_config.is_some_and(owner::window_claimed) {
                return Err(Status::Waiting);
            }
            if round {
                crate::generation::guard(paths, false).map_err(refusal("generation guard"))?;
                if now >= refresh {
                    if crate::generation::refresh_required(paths)
                        .map_err(refusal("generation refresh"))?
                    {
                        return Ok(Flow::End);
                    }
                    refresh = now + Duration::from_secs(30);
                }
                if !enabled(config).map_err(refusal("configuration"))? {
                    return Err(Status::Disabled);
                }
                // Control this Mac cannot see stays unseen until a vault file moves: the
                // observation kept from the last poll answers until then
                // (`commons::observe_settled`).
                if matches!(
                    commons::observe_until_settled(paths, now + Duration::from_millis(200)),
                    Ok(commons::Observation::Visible(_))
                ) {
                    return Ok(Flow::End);
                }
                let candidates = network.addresses()?;
                network.may_use(&candidates)?;
                network.refresh(transport, &candidates);
            }
            serve_turn(transport, now, round)?;
            observe_transport(history, transport, ObservationMode::Discovery);
            Ok(Flow::Serve(Some(transport.next_due(now))))
        },
    )
}

#[derive(Clone, Copy)]
enum ObservationMode {
    Repair,
    Enrollment,
    Discovery,
}

/// The description must reflect the same transport evidence as visibility. A transport whose
/// links all failed is Unavailable, its latest socket failure kept beside the detail. Peers the
/// transport still holds hints of do not say otherwise: with no link left nothing is heard now,
/// they were heard before its links failed, and with no link to dial them a repairing transport
/// keeps them for good. With no candidate address the transport is Offline: no usable interface.
fn observe_transport(
    history: &mut Observer,
    transport: &transport::Transport,
    mode: ObservationMode,
) {
    let (visibility, detail) = if transport.unavailable() {
        (
            Visibility::Unavailable,
            match mode {
                ObservationMode::Repair => "Local network sockets unavailable",
                ObservationMode::Enrollment => {
                    "Local network sockets unavailable; enrollment pending"
                }
                ObservationMode::Discovery => {
                    "Local network sockets unavailable; Commons control unavailable"
                }
            },
        )
    } else if transport.interfaces() == 0 {
        (
            Visibility::Offline,
            match mode {
                ObservationMode::Repair => "No usable local network interface",
                ObservationMode::Enrollment => {
                    "No usable local network interface; enrollment pending"
                }
                ObservationMode::Discovery => {
                    "No usable local network interface; Commons control unavailable"
                }
            },
        )
    } else {
        (
            Visibility::Active,
            match mode {
                ObservationMode::Repair => "Authenticated repair observer active",
                ObservationMode::Enrollment => "Discovery active; enrollment pending",
                ObservationMode::Discovery => "Discovery only; Commons control unavailable",
            },
        )
    };
    // Retain the actual socket failure while unavailable; a healthy or addressless transport has
    // its own cause.
    let failure = if visibility == Visibility::Unavailable {
        transport.failure().map(|why| format!("{detail}: {why}"))
    } else {
        None
    };
    history.observe(
        visibility,
        transport.unverified_peers(),
        failure.as_deref().unwrap_or(detail),
        aterm_time::unix_millis().unwrap_or(0),
        Instant::now(),
    );
}

/// Whether the root LAN broker answers a hello, else what the runtime waits for, as its
/// observation's detail ([`Network::may_use`]). Off macOS sockets are made in-process: nothing
/// is waited for.
#[cfg(not(test))]
fn broker_answers() -> std::result::Result<(), &'static str> {
    #[cfg(target_os = "macos")]
    {
        use aterm_uds::lan::{LanError, Unavailable};
        match aterm_uds::lan::broker::hello() {
            Ok(_) => Ok(()),
            Err(LanError::Unavailable(Unavailable::NotInstalled)) => {
                Err("Waits for the LAN broker, which setup's Mac password installs")
            }
            Err(LanError::Unavailable(Unavailable::Outdated { .. })) => {
                Err("Waits for the LAN broker to be renewed for this aterm")
            }
            Err(LanError::Unavailable(Unavailable::Refused(_))) => {
                Err("Waits for a LAN broker that admits this aterm")
            }
            Err(LanError::Unavailable(Unavailable::Unchecked(_))) => {
                Err("Waits for the LAN broker to check this aterm")
            }
            Err(LanError::Io(_)) => Err("Waits for the LAN broker to answer"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

#[cfg(not(test))]
fn addresses() -> std::result::Result<Vec<std::net::SocketAddr>, Status> {
    #[cfg(debug_assertions)]
    let isolated = isolation::port();
    #[cfg(not(debug_assertions))]
    let isolated = Ok(None);
    usable_addresses(isolated, aterm_net::lan::Listener::candidates)
}

/// The runtime's candidates under `isolated`, the private attachment's setting: a port is
/// loopback alone and never reads `lan`, an invalid setting refuses, and with none set they are
/// `lan`'s IPv4 addresses (an unreadable `lan` is offline).
fn usable_addresses(
    isolated: crate::Result<Option<u16>>,
    lan: impl FnOnce() -> std::io::Result<Vec<std::net::SocketAddr>>,
) -> std::result::Result<Vec<std::net::SocketAddr>, Status> {
    if isolated.map_err(refusal("isolated attachment"))?.is_some() {
        return Ok(vec![(std::net::Ipv4Addr::LOCALHOST, 0).into()]);
    }
    lan()
        .map(|addresses| {
            addresses
                .into_iter()
                .filter(|address| address.is_ipv4())
                .collect()
        })
        .map_err(|_| Status::Offline)
}

#[cfg(test)]
fn addresses() -> std::result::Result<Vec<std::net::SocketAddr>, Status> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod auto_sync_tests;

#[cfg(test)]
mod auto_enrollment_tests;

#[cfg(test)]
mod process_sync_tests;

#[cfg(test)]
#[path = "runtime_accounts_exchange_tests.rs"]
mod accounts_exchange_tests;
