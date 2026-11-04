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
use aterm_types::network_state::{MachineIdentity, Visibility};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod custody;
mod discovery;
#[cfg(any(test, debug_assertions))]
mod isolation;
mod lan;
mod owner;
mod place;
mod report;
mod transport;
mod vault;
pub(crate) use discovery::MAX_UNVERIFIED_ENDPOINTS;
#[cfg(test)]
pub(crate) use discovery::Outcome;
pub use lan::{LanGate, LanTurn};
pub use report::{AuthenticatedConnection, RepairRound};
#[cfg(test)]
pub(crate) use transport::test_worker;

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
#[cfg(any(test, debug_assertions))]
pub(crate) fn private_attachment_port() -> Result<u16> {
    isolation::port()?.ok_or_else(|| crate::Error::new("no private Commons attachment configured"))
}

#[cfg(any(test, debug_assertions))]
pub use discovery::db_discovery_replacement_environment;

const POLL: Duration = Duration::from_secs(2);
const TICK: Duration = Duration::from_millis(100);
/// Local custody admission budget: 2 s in product, 60 s hang detector on loaded test hosts.
#[cfg(not(test))]
const ADMISSION: Duration = Duration::from_secs(2);
#[cfg(test)]
const ADMISSION: Duration = Duration::from_secs(60);
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
    /// What holds the owner before its observer and first LAN use ([`LanGate`]).
    lan: LanGate,
    /// Authenticated contacts go to the machine's shared place; fixtures are inert.
    place: place::Publisher,
    #[cfg(test)]
    isolated: Option<auto_sync_tests::Attachment>,
    /// Counts each transport this network opened (tests).
    #[cfg(test)]
    opens: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// A LAN attachment's stand-in (tests): its address is no loopback one, so the runtime's first
    /// LAN use asks its gate's host, and its transport reaches the LAN only once the flag reads
    /// true ([`transport::Transport::standing_in`]).
    #[cfg(test)]
    stand_in: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Every transport open fails with [`OPEN_FAILS`] (tests).
    #[cfg(test)]
    open_fails: bool,
    /// Each wait the owner took after a try that did not serve, in order (tests; [`Self::retry`]).
    #[cfg(test)]
    retries: Option<Arc<Mutex<Vec<Duration>>>>,
    /// This runtime kept its LAN use ([`Self::note_use`]).
    used: std::cell::Cell<bool>,
    /// What the gate's host last deferred the runtime's first LAN use for ([`Self::may_use`]).
    waits: std::cell::Cell<&'static str>,
}
impl Network {
    /// The live network, whose owner waits at `lan` before its first LAN use.
    /// A guarded debug attachment uses loopback alone, so it asks no LAN permission.
    fn gated(lan: LanGate) -> Result<Self> {
        #[cfg(any(test, debug_assertions))]
        let lan = if isolation::port()?.is_some() {
            LanGate::open()
        } else {
            lan
        };
        Ok(Self {
            repairs: crate::control::PeerCache::default(),
            lan,
            place: place::Publisher::live(),
            #[cfg(test)]
            isolated: None,
            #[cfg(test)]
            opens: None,
            #[cfg(test)]
            stand_in: None,
            #[cfg(test)]
            open_fails: false,
            #[cfg(test)]
            retries: None,
            used: std::cell::Cell::new(false),
            waits: std::cell::Cell::new(""),
        })
    }

    /// The owner's `wait` on `stop` after a try that did not serve: held at its gate, or ended
    /// with a status other than a rebuild.
    fn retry(&self, stop: &Stop, wait: Duration) {
        #[cfg(test)]
        if let Some(retries) = &self.retries {
            retries.lock().unwrap_or_else(|p| p.into_inner()).push(wait);
        }
        stop.wait(wait);
    }

    /// Whether the runtime may open or refresh its transport on `candidates` now: asked of its
    /// gate's host right before its first LAN use ([`LanGate::before_use`]), at each try until it
    /// kept one ([`Self::note_use`]). Only loopback candidates are no LAN use and ask nothing. A use
    /// the host defers is [`Status::LanHeld`], with what it waits for kept for the owner's
    /// observation.
    fn may_use(&self, candidates: &[std::net::SocketAddr]) -> std::result::Result<(), Status> {
        if self.used.get() || candidates.iter().all(|address| address.ip().is_loopback()) {
            return Ok(());
        }
        match self.lan.ready_for_use() {
            None => Ok(()),
            Some(why) => {
                self.waits.set(why);
                Err(Status::LanHeld)
            }
        }
    }

    /// Keep, once for this runtime, that it made a LAN use, with its gate's keeper
    /// ([`LanGate::keeping_use`]): it opened or refreshed its transport on `candidates`, one of
    /// them is not a loopback address, and the transport reached the LAN (`bound`,
    /// [`transport::Transport::bound`]). A use the keeper could not keep is tried again at the
    /// next. A try that reached none is no use: the gate's host hears it
    /// ([`LanGate::missing_use`]), and the next try asks it again ([`Self::may_use`]). A try on
    /// loopback candidates only, or none, is no try: nothing is asked, kept or missed. Nothing is kept per home: the OS asks
    /// each app on each Mac, and only the host knows whose question its runtime's use was.
    fn note_use(&self, candidates: &[std::net::SocketAddr], bound: bool) {
        if self.used.get() || candidates.iter().all(|address| address.ip().is_loopback()) {
            return;
        }
        self.used.set(self.lan.used(bound));
    }

    /// Refresh `transport` on `candidates`, a LAN use where it reaches the LAN
    /// ([`Self::note_use`]).
    fn refresh(&self, transport: &mut transport::Transport, candidates: &[std::net::SocketAddr]) {
        transport.refresh(candidates);
        self.note_use(candidates, transport.bound());
    }

    fn addresses(&self) -> std::result::Result<Vec<std::net::SocketAddr>, Status> {
        // An isolated attachment is one loopback address, there whether or not its flag (the OS's
        // Local Network permission) lets the transport's sends through (tests).
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

    /// Open a transport on `candidates`, a LAN use where it reaches the LAN
    /// ([`Self::note_use`]); one that fails to open reaches none.
    fn open(
        &self,
        endpoint: Option<Endpoint>,
        candidates: &[std::net::SocketAddr],
    ) -> Result<transport::Transport> {
        let opened = self.opening(endpoint, candidates);
        self.note_use(
            candidates,
            opened.as_ref().is_ok_and(transport::Transport::bound),
        );
        opened
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
                Some(attachment.lan_allowed.clone()),
            );
        }
        match endpoint {
            Some(endpoint) => transport::Transport::open(endpoint, candidates),
            None => transport::Transport::presence_only(candidates),
        }
    }

    #[cfg(test)]
    fn observed(&self, transport: &transport::Transport) {
        if let Some(attachment) = &self.isolated {
            attachment.observe(transport.application_observations());
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
    /// The owner waits before its first LAN use ([`LanGate`]): it opened no observer and no
    /// socket yet, or (its host deferred the use, [`LanGate::before_use`]) no LAN socket.
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
                let cause = Some((refusal.step, refusal.error.clone()));
                refusal.standing = self.cause == cause;
                self.cause = cause;
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

#[derive(Default)]
struct Stop {
    requested: Mutex<bool>,
    changed: Condvar,
}
impl Stop {
    fn requested(&self) -> bool {
        *self.requested.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn request(&self) {
        *self.requested.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.changed.notify_all();
    }
    fn wait(&self, duration: Duration) {
        let lock = self.requested.lock().unwrap_or_else(|p| p.into_inner());
        let _ = self
            .changed
            .wait_timeout_while(lock, duration, |stop| !*stop);
    }
}

/// What a push came to ([`Pusher::push_until`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pushed {
    /// A repair round that began after the ask caught a peer up on the table: the peer now holds
    /// every operation of it this runtime offered then (`crate::replica::Report::caught_up`), and
    /// its own rounds carry them on. Data transfers and the first quiet confirmation are
    /// followed at once; further quiet rounds keep ordinary spacing.
    Reached,
    /// This runtime had no serving owner, or that service ended before the push completed.
    /// The table remains available for the serving owner's next rounds.
    NotServing,
    /// This runtime knows no verified peer to dial.
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
            Self::NoPeers => "no verified peer is known",
            Self::TimedOut => "no peer took it before the deadline",
        }
    }
}

/// Hands a table to this runtime's peers now ([`Runtime::pusher`]): what a host pushes when no
/// later round of its own may come (it is about to sleep or stop).
#[derive(Clone, Default)]
pub struct Pusher(Arc<Board>);

impl Pusher {
    /// Hand the table `table` (a scope digest) to this runtime's peers now: every verified peer is
    /// dialled at once and each round this runtime begins repairs `table` first where the peer
    /// holds it too. Waits until a round that began after this ask caught a peer up on it
    /// ([`Pushed::Reached`]), the runtime stops serving, or `deadline`, whichever is first; past
    /// the deadline the runtime ends the push. Concurrent callers wait for the current caller
    /// within their own deadline; one table's completion never acknowledges another table.
    /// An unfinished call ends if its serving generation changes, even across a fast restart.
    #[must_use]
    pub fn push_until(&self, table: astream_wire::sync::Digest, deadline: Instant) -> Pushed {
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
    fn desk(&self) -> std::sync::MutexGuard<'_, Desk> {
        self.desk.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn first(&self) -> std::sync::MutexGuard<'_, Option<astream_wire::sync::Digest>> {
        self.first.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The owner serves repair until the guard drops; then every waiting push hears it.
    fn serving(&self) -> Serving<'_> {
        let generation = {
            let mut desk = self.desk();
            let generation = desk.start();
            *self.first() = None;
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
        let mut desk = self.desk();
        let ask = desk.ask.take()?;
        desk.active = Some(ask.id);
        *self.first() = Some(ask.table);
        Some(ask)
    }

    /// Release this active round's priority and answer its caller if still current. A round
    /// superseded by a new one cannot change that new round's priority or result.
    fn settle(&self, id: u64, pushed: Pushed) {
        let mut desk = self.desk();
        if desk.active != Some(id) {
            return;
        }
        desk.active = None;
        *self.first() = None;
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
        let mut desk = self.board.desk();
        if desk.generation != self.generation {
            return;
        }
        desk.stop();
        *self.board.first() = None;
        drop(desk);
        self.board.changed.notify_all();
    }
}

/// Owning process lifetime. Dropping it stops and joins all network workers.
pub struct Runtime {
    stop: Arc<Stop>,
    task: Option<JoinHandle<()>>,
    observation: ObservationHandle,
    pusher: Pusher,
}

/// Development observations keep their publication separate from live policy.
/// A temporary policy copy must disable writer recovery: its path cannot become
/// a persistent generation's authorization source.
#[cfg(any(test, debug_assertions))]
pub struct Diagnostic {
    pub observer_config: PathBuf,
    pub recover_writer: bool,
}

impl Runtime {
    /// Start default-on background repair using only this explicit secrets root
    /// and config file. Missing control permits opaque presence discovery and
    /// local observation history, never enrollment or data repair. Call only
    /// from a long-lived app/session host. `lan` holds the owner before its observer and its
    /// first LAN use until the host says the LAN is the runtime's ([`LanGate`]).
    pub fn start(
        paths: Paths,
        config: PathBuf,
        lan: LanGate,
        observe: impl Fn(Status) + Send + 'static,
    ) -> Result<Self> {
        Self::start_on(paths, config, observe, Network::gated(lan)?)
    }

    /// Start the same automatic owner with bounded completed-round diagnostics.
    /// The callback runs on the owner thread after the worker has joined, with
    /// no custody/control/table journal or channel/cache guard held. It receives
    /// no payloads or keys and should return promptly. Lifetime ownership and
    /// observation leases remain held until this runtime stops.
    pub fn start_with_reports(
        paths: Paths,
        config: PathBuf,
        lan: LanGate,
        observe: impl Fn(Status) + Send + 'static,
        reports: impl Fn(RepairRound) + Send + 'static,
    ) -> Result<Self> {
        Self::start_on_with_reports(paths, config, observe, Network::gated(lan)?, reports)
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
        lan: LanGate,
        observe: impl Fn(Status) + Send + 'static,
        reports: impl Fn(RepairRound) + Send + 'static,
    ) -> Result<Self> {
        Self::start_mode_with_sources(
            paths,
            config,
            (diagnostic.observer_config, diagnostic.recover_writer),
            observe,
            Network::gated(lan)?,
            false,
            reports,
        )
    }

    /// Start background Commons repair and steward enrollment for `aterm node serve` (C22).
    /// Owns local observation and transport while no interactive `aterm` window runs, yields
    /// both within one tick when a window claims [`owner::WindowClaim`], and resumes once the
    /// last window exits. `lan` gates its first LAN use as [`Self::start`]'s does.
    pub fn start_background(
        paths: Paths,
        config: PathBuf,
        lan: LanGate,
        observe: impl Fn(Status) + Send + 'static,
    ) -> Result<Self> {
        Self::start_background_on(paths, config, observe, Network::gated(lan)?)
    }

    /// [`Self::start_background`] with [`Self::start_with_reports`]'s completed-round reports.
    pub fn start_background_with_reports(
        paths: Paths,
        config: PathBuf,
        lan: LanGate,
        observe: impl Fn(Status) + Send + 'static,
        reports: impl Fn(RepairRound) + Send + 'static,
    ) -> Result<Self> {
        Self::start_mode_with_reports(paths, config, observe, Network::gated(lan)?, true, reports)
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
        let observation = ObservationHandle::default();
        let published = observation.clone();
        let pusher = Pusher::default();
        let board = pusher.0.clone();
        let task = thread::Builder::new()
            .name("commons-owner".into())
            .spawn(move || {
                let mut network = network;
                let observer_dir = crate::network_state::directory(&observer_config);
                let mut window_claim = None;
                let mut previous = None;
                let mut report = |status: Status| {
                    if previous.as_ref() != Some(&status) {
                        observe(status.clone());
                        previous = Some(status);
                    }
                };
                // What this owner serves of its wait while its gate or a disabled Commons holds it.
                let mut wait_relay = crate::network_state::WaitRelay::default();
                // One streak of refusals, whether the gate or a try refused ([`Refusals`]).
                let mut refusals = Refusals::new(background);
                while !worker.requested() {
                    if background && owner::window_claimed(&observer_dir) {
                        wait_relay.stop();
                        let publication = waiting_publication(
                            &paths,
                            &config,
                            Visibility::Waiting,
                            "An aterm window owns Commons repair while it runs",
                        );
                        published.replace_publication(publication);
                        report(Status::Waiting);
                        worker.wait(POLL);
                        continue;
                    }
                    // Before the observer: opening it creates the observation history, and an
                    // owner holds no observation or transport ownership while its LAN waits. It
                    // shows the journal owner's live observation where one answers, else its own
                    // wait, which it serves to other local processes as an open owner serves its
                    // observation.
                    if let Some((visibility, detail, mut status)) = network.lan.held(&config) {
                        published.replace_publication(
                            transport_publication(&paths)
                                .or_else(|| crate::network_state::read_owner_publication(&config).ok())
                                .unwrap_or_else(|| held_publication(&config, visibility, detail)),
                        );
                        wait_relay.keep(&observer_config, &published);
                        let wait = refusals.after(Some(&mut status), None);
                        report(status);
                        network.retry(&worker, wait);
                        continue;
                    }
                    if !background && window_claim.is_none() {
                        // Keep the working background owner until this window's first LAN
                        // gate opens. Retain priority across observer contention afterward.
                        window_claim = owner::WindowClaim::acquire(&observer_dir).ok();
                    }
                    wait_relay.stop();
                    // Before the observer, whose history then says nothing of a LAN use: a
                    // history with no record is one an aterm with no gate left.
                    let _ = crate::network_state::begin_lan_use(&observer_config);
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
                            worker.wait(if background { POLL } else { TICK * 2 });
                            continue;
                        }
                    };
                    while !worker.requested() {
                        let mut serving = None;
                        let mut served = serve_mode_with_recovery(
                            (&paths, &config, recover_writer),
                            background.then_some(observer_dir.as_path()),
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
                            if background
                                && status == Status::Waiting
                                && owner::window_claimed(&observer_dir)
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
                            if status == Status::Waiting
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
                            report(status);
                            network.retry(&worker, wait);
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
            task: Some(task),
            observation,
            pusher,
        })
    }
    pub fn stop(mut self) {
        self.join();
    }
    /// Pure shared observation; readers clone the value off the render path.
    pub fn network_observation(&self) -> ObservationHandle {
        self.observation.clone()
    }
    /// What hands a table to this runtime's peers now ([`Pusher::push_until`]).
    pub fn pusher(&self) -> Pusher {
        self.pusher.clone()
    }
    fn join(&mut self) {
        self.stop.request();
        if let Some(task) = self.task.take() {
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

/// What a held owner publishes as its own wait: the retained history with `visibility` and
/// `detail`, observed now, so it reads as current here and over its relay.
fn held_publication(
    config: &Path,
    visibility: Visibility,
    detail: &str,
) -> crate::network_state::Publication {
    let mut retained = crate::network_state::read_history(config).unwrap_or_default();
    retained.visibility = visibility;
    retained.detail = detail.into();
    retained.observed_at_ms = aterm_time::unix_millis().unwrap_or(0);
    crate::network_state::Publication::new(retained, Instant::now())
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
        crate::generation::observed_paths_until(paths, Instant::now() + ADMISSION).ok()?;
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
    yield_to_window: Option<&Path>,
    stop: &Stop,
    report: &mut impl FnMut(Status),
    history: &mut Observer,
    network: &Network,
    reports: &impl Fn(RepairRound),
    board: &Board,
) -> std::result::Result<(), Status> {
    serve_mode_with_recovery(
        (paths, config, true),
        yield_to_window,
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
    yield_to_window: Option<&Path>,
    stop: &Stop,
    report: &mut impl FnMut(Status),
    history: &mut Observer,
    network: &Network,
    reports: &impl Fn(RepairRound),
    board: &Board,
) -> std::result::Result<(), Status> {
    if !enabled(config).map_err(refusal("configuration"))? {
        return Err(Status::Disabled);
    }
    let paths = if recover_writer {
        crate::generation::advance_while(paths, config, &|| stop.requested())
            .map_err(refusal("writer recovery"))?
    } else {
        paths.clone()
    };
    if stop.requested() {
        return Ok(());
    }
    serve_selected(
        (&paths, config),
        yield_to_window,
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
    yield_to_window: Option<&Path>,
    stop: &Stop,
    report: &mut impl FnMut(Status),
    history: &mut Observer,
    network: &Network,
    reports: &impl Fn(RepairRound),
    board: &Board,
) -> std::result::Result<(), Status> {
    history.unconfirm_local();
    if yield_to_window.is_some_and(owner::window_claimed) {
        return Err(Status::Waiting);
    }
    if !enabled(config).map_err(refusal("configuration"))? {
        return Err(Status::Disabled);
    }
    let source_snapshot = match commons::observe_until(paths, Instant::now() + ADMISSION) {
        Ok(commons::Observation::Visible(snapshot)) => snapshot,
        _ => {
            return serve_presence(
                (paths, config),
                yield_to_window,
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
    let deadline = Instant::now() + ADMISSION;
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
    let _ = history.local(machine_identity(request.binding()), bootstrap.trust());
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
        vault::Reader::start(paths, config, &lease, &custody).map_err(refusal("vault reader"))?
    };
    let mut hearing = (!pending).then(|| Hearing::open(&bootstrap));
    let mut clocks = if pending {
        None
    } else {
        ClockStore::open(&bootstrap)
    };
    // The lab journal's flusher, on a thread of its own; an enrolling owner has no writer to
    // publish anything with.
    let mut lab = (!pending).then(|| crate::alab::upkeep::Upkeep::new(Instant::now()));
    let mut refused = network.lan.refused();
    let mut refresh = Instant::now() + Duration::from_secs(30);
    let result = (|| {
        let _serving = board.serving();
        let mut pushing: Option<Ask> = None;
        let mut poll = Instant::now() + POLL;
        while !stop.requested() {
            if yield_to_window.is_some_and(owner::window_claimed) {
                return Err(Status::Waiting);
            }
            let now = Instant::now();
            if now >= poll {
                crate::generation::guard(paths, false).map_err(refusal("generation guard"))?;
                if now >= refresh {
                    if crate::generation::refresh_required(paths)
                        .map_err(refusal("generation refresh"))?
                    {
                        return Ok(());
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
                match commons::observe_until(paths, now + Duration::from_millis(200))
                    .map_err(refusal("vault observation"))?
                {
                    commons::Observation::Visible(snapshot) => {
                        if !snapshot.same_observation(&source_snapshot) {
                            return Ok(());
                        }
                    }
                    commons::Observation::Unavailable(commons::Unavailable::Busy) => {}
                    commons::Observation::Unavailable(_) => return Err(Status::Dormant),
                }
                if pending {
                    if bootstrap
                        .log_witness()
                        .map_err(refusal("log witness"))?
                        .is_some()
                    {
                        return Ok(());
                    }
                    let journal = aterm_secrets::commons::journal::Journal::read_existing(
                        bootstrap.journal_dir(),
                        bootstrap.trust(),
                    )
                    .map_err(refusal("control journal"))?;
                    if bootstrap
                        .witness_in(&journal)
                        .map_err(refusal("control witness"))?
                        .is_some()
                    {
                        return Ok(());
                    }
                }
                let candidates = network.addresses()?;
                network.may_use(&candidates)?;
                network.refresh(&mut transport, &candidates);
                refused = network.lan.refused();
                poll = now + POLL;
            }
            if let Some(ask) = board.take() {
                pushing = Some(ask);
                if transport.hurry(ask.table, now) == 0 {
                    pushing = None;
                    transport.calm();
                    board.settle(ask.id, Pushed::NoPeers);
                }
            }
            transport.tick(now).map_err(|_| Status::Offline)?;
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
            if let Some(lab) = &mut lab {
                lab.tick(paths, now);
            }
            #[cfg(test)]
            network.observed(&transport);
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
                &transport,
                if pending {
                    ObservationMode::Enrollment
                } else {
                    ObservationMode::Repair
                },
                refused,
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
            stop.wait(TICK);
        }
        Ok(())
    })();
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
        for contact in contacts {
            // What the peer said, at the time of the round that said it; then its latest
            // contact, which can be a later round that ended before versions were traded.
            self.changed |= self.ledger.record(
                contact.actor,
                contact_time(now_ms, now, contact.said_at),
                contact.said,
            );
            if contact.at != contact.said_at {
                self.changed |= self.ledger.record(
                    contact.actor,
                    contact_time(now_ms, now, contact.at),
                    Said::Unknown,
                );
            }
        }
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
        Instant::now() + ADMISSION,
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
        crate::catalog::Descriptor::application(bootstrap.trust(), "accounts", "proposals", 1)
            .ok()?
            .scope;
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
    yield_to_window: Option<&Path>,
    stop: &Stop,
    report: &mut impl FnMut(Status),
    history: &mut Observer,
    network: &Network,
) -> std::result::Result<(), Status> {
    if yield_to_window.is_some_and(owner::window_claimed) {
        return Err(Status::Waiting);
    }
    let candidates = network.addresses()?;
    network.may_use(&candidates)?;
    let mut transport = network
        .open(None, &candidates)
        .map_err(|_| Status::Offline)?;
    let mut refused = network.lan.refused();
    let mut poll = Instant::now() + POLL;
    let mut refresh = Instant::now() + Duration::from_secs(30);
    report(Status::Dormant);
    while !stop.requested() {
        if yield_to_window.is_some_and(owner::window_claimed) {
            return Err(Status::Waiting);
        }
        let now = Instant::now();
        if now >= poll {
            crate::generation::guard(paths, false).map_err(refusal("generation guard"))?;
            if now >= refresh {
                if crate::generation::refresh_required(paths)
                    .map_err(refusal("generation refresh"))?
                {
                    return Ok(());
                }
                refresh = now + Duration::from_secs(30);
            }
            if !enabled(config).map_err(refusal("configuration"))? {
                return Err(Status::Disabled);
            }
            if matches!(
                commons::observe_until(paths, now + Duration::from_millis(200)),
                Ok(commons::Observation::Visible(_))
            ) {
                return Ok(());
            }
            let candidates = network.addresses()?;
            network.may_use(&candidates)?;
            network.refresh(&mut transport, &candidates);
            refused = network.lan.refused();
            poll = now + POLL;
        }
        transport.tick(now).map_err(|_| Status::Offline)?;
        observe_transport(history, &transport, ObservationMode::Discovery, refused);
        stop.wait(TICK);
    }
    Ok(())
}

/// [`LAN_REFUSED`] followed by a literal suffix.
macro_rules! concat_refused {
    ($suffix:literal) => {
        concat!(
            "macOS refused aterm the local network when it last asked: turn aterm on in System \
             Settings > Privacy & Security > Local Network",
            $suffix
        )
    };
}

/// What an observation says while the host's records hear that the OS refused the runtime the LAN
/// and its transport's links all failed ([`observe_transport`]).
const LAN_REFUSED: &str = concat_refused!("");

#[derive(Clone, Copy)]
enum ObservationMode {
    Repair,
    Enrollment,
    Discovery,
}

/// The description must reflect the same transport evidence as visibility. A refusal shows as
/// links that bound and then failed (the OS fails a refused app's LAN sends): a transport whose
/// links all failed (Unavailable), while the host's records hear that the OS `refused` the runtime
/// the LAN ([`LanGate::with_refusal`]), says that refusal and where to lift it. Peers the
/// transport still holds hints of do not say otherwise: with no link left nothing is heard now,
/// they were heard before its links failed (a refusal made mid-session among them), and with no
/// link to dial them a repairing transport keeps them for good. With no candidate address (Offline) the refusal is not the
/// cause, whatever the records say: no usable interface is.
fn observe_transport(
    history: &mut Observer,
    transport: &transport::Transport,
    mode: ObservationMode,
    refused: bool,
) {
    let (visibility, detail) = if transport.unavailable() {
        (
            Visibility::Unavailable,
            match (mode, refused) {
                (ObservationMode::Repair, false) => "Local network sockets unavailable",
                (ObservationMode::Enrollment, false) => {
                    "Local network sockets unavailable; enrollment pending"
                }
                (ObservationMode::Discovery, false) => {
                    "Local network sockets unavailable; Commons control unavailable"
                }
                (ObservationMode::Repair, true) => LAN_REFUSED,
                (ObservationMode::Enrollment, true) => concat_refused!("; enrollment pending"),
                (ObservationMode::Discovery, true) => {
                    concat_refused!("; Commons control unavailable")
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
    // A recorded OS refusal has its actionable explanation above. Otherwise retain the actual
    // socket failure while unavailable; a healthy or addressless transport has its own cause.
    let failure = if visibility == Visibility::Unavailable && !refused {
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
mod accounts_exchange_tests {
    use super::*;
    use crate::replica::{InventoryObservation, InventorySummary, MatchingInventory};
    use aterm_secrets::commons::roster::Role;

    fn matching(observed: InventoryObservation) -> MatchingInventory {
        let report = crate::replica::Report {
            table: Some(observed.scope.digest()),
            local_known: Some(observed.inventory),
            peer_known: Some(observed.inventory),
            ..Default::default()
        };
        MatchingInventory::from_observations(&report, observed, observed).unwrap()
    }

    fn round(exchange: MatchingInventory) -> RepairRound {
        let mut round = RepairRound::panicked(None);
        round.actor = Some(exchange.peer_actor());
        round.exchange_started = true;
        round.control = Ok(Default::default());
        round.application = Some(crate::replica::Report {
            table: Some(exchange.scope().digest()),
            local_known: Some(exchange.inventory()),
            peer_known: Some(exchange.inventory()),
            matching_inventory: Some(exchange),
            ..Default::default()
        });
        round
    }

    fn sample(bootstrap: &Bootstrap, peer: &Bootstrap) -> InventoryObservation {
        let session = crate::table::Session::observe(bootstrap).unwrap();
        InventoryObservation {
            scope: crate::catalog::Descriptor::application(
                bootstrap.trust(),
                "accounts",
                "proposals",
                1,
            )
            .unwrap()
            .scope,
            local_actor: session.actor(),
            peer_actor: peer.request().unwrap().binding().actor(),
            control_observation: session.observation().unwrap(),
            inventory: InventorySummary::of(&Default::default()),
            observed_at_ms: 1000,
            sampled_at: Instant::now(),
        }
    }

    #[test]
    fn account_exchange_remapping_requires_exact_scope_actor_context_and_peer_binding() {
        let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let peer = lab.bootstrap(1);
        let journal = Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
        let access = bootstrap.access_in(&journal).unwrap().unwrap();
        let observed = sample(&bootstrap, &peer);
        let candidate =
            accounts_exchange_candidate(&bootstrap, &journal, &access, matching(observed)).unwrap();
        assert_eq!(
            candidate.observation.peer,
            machine_identity(peer.request().unwrap().binding())
        );
        assert_eq!(candidate.observation.sampled_at_ms, observed.observed_at_ms);
        assert_eq!(candidate.sampled_at, observed.sampled_at);
        let mut wrong_table = observed;
        wrong_table.scope =
            crate::catalog::Descriptor::application(bootstrap.trust(), "notes", "pages", 1)
                .unwrap()
                .scope;
        let mut wrong_actor = observed;
        wrong_actor.local_actor = observed.peer_actor;
        let mut wrong_control = observed;
        wrong_control.control_observation[0] ^= 1;
        let mut unknown_peer = observed;
        unknown_peer.peer_actor = [9; 48];
        let mut own_binding = observed;
        own_binding.peer_actor = observed.local_actor;
        for changed in [
            wrong_table,
            wrong_actor,
            wrong_control,
            unknown_peer,
            own_binding,
        ] {
            assert!(
                accounts_exchange_candidate(&bootstrap, &journal, &access, matching(changed))
                    .is_none()
            );
        }
    }

    #[test]
    fn contact_projection_rechecks_real_custody_and_never_retimes_an_exchange() {
        let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let peer = lab.bootstrap(1);
        let observed = sample(&bootstrap, &peer);
        let completed = round(matching(observed));
        let local = machine_identity(bootstrap.request().unwrap().binding());
        for dir in ["network-one", "network-two"] {
            aterm_secrets::fsx::ensure_private_dir(&lab._home.path().join(dir)).unwrap();
        }
        let handle = ObservationHandle::default();
        let mut history =
            Observer::open(&lab._home.path().join("network-one/config"), handle.clone()).unwrap();
        history.local(local.clone(), bootstrap.trust()).unwrap();
        observe_contacts(
            &bootstrap,
            &std::cell::OnceCell::new(),
            &mut history,
            Vec::new(),
            std::slice::from_ref(&completed),
            &place::Publisher::default(),
            || panic!("historical evidence must use its own sample clock"),
        );
        history.observe(
            Visibility::Active,
            0,
            "observing",
            2000,
            observed.sampled_at + Duration::from_secs(1),
        );
        assert_eq!(
            handle.snapshot().accounts_exchange.unwrap().sampled_at_ms,
            1000
        );
        std::fs::remove_file(lab.paths[0].dir.join("identity")).unwrap();
        let handle = ObservationHandle::default();
        let mut refused =
            Observer::open(&lab._home.path().join("network-two/config"), handle.clone()).unwrap();
        refused.local(local, bootstrap.trust()).unwrap();
        observe_contacts(
            &bootstrap,
            &std::cell::OnceCell::new(),
            &mut refused,
            Vec::new(),
            &[completed],
            &place::Publisher::default(),
            || panic!("custody refusal cannot become a new sample"),
        );
        refused.observe(
            Visibility::Active,
            0,
            "observing",
            3000,
            observed.sampled_at + Duration::from_secs(2),
        );
        assert!(handle.snapshot().accounts_exchange.is_none());
    }

    #[test]
    fn authenticated_application_failure_is_published_separately_from_peer_contact() {
        use aterm_types::network_state::{RepairOutcome, RepairStage};
        let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let peer = lab.bootstrap(1);
        let actor = peer.request().unwrap().binding().actor();
        let local = machine_identity(bootstrap.request().unwrap().binding());
        let at = Instant::now();
        let config = lab._home.path().join("repair-observer/config");
        aterm_secrets::fsx::ensure_private_dir(config.parent().unwrap()).unwrap();
        let handle = ObservationHandle::default();
        let mut history = Observer::open(&config, handle.clone()).unwrap();
        history.local(local, bootstrap.trust()).unwrap();
        let mut failed = RepairRound::panicked(Some("127.0.0.1:41384".parse().unwrap()));
        failed.actor = Some(actor);
        failed.exchange_started = true;
        failed.phase = Some(crate::control::Phase::Application);
        failed.control = Ok(Default::default());
        failed.application_error = Some("private fixture error".into());
        failed.completed_at_ms = 1500;
        observe_contacts(
            &bootstrap,
            &std::cell::OnceCell::new(),
            &mut history,
            vec![(actor, at)],
            std::slice::from_ref(&failed),
            &place::Publisher::default(),
            || (2000, at + Duration::from_secs(1)),
        );
        history.observe(
            Visibility::Active,
            0,
            "observing",
            2000,
            at + Duration::from_secs(1),
        );
        let state = handle.snapshot();
        assert_eq!(state.peers.len(), 1);
        assert_eq!(state.peers[0].last_seen_ms, 1000);
        assert_eq!(
            state.peers[0].reachability,
            aterm_types::network_state::Reachability::Reachable
        );
        let attempt = state.peers[0].last_repair.as_ref().unwrap();
        assert_eq!(attempt.completed_at_ms, 1500);
        assert_eq!(attempt.outcome, RepairOutcome::Failed);
        assert_eq!(attempt.stage, Some(RepairStage::Application));
        assert!(
            !crate::network_state::value(&state)
                .to_string()
                .contains("private fixture")
        );

        failed.actor = Some([9; 48]);
        failed.completed_at_ms = 2500;
        observe_contacts(
            &bootstrap,
            &std::cell::OnceCell::new(),
            &mut history,
            Vec::new(),
            &[failed.clone()],
            &place::Publisher::default(),
            || panic!("an unbound peer must not establish a contact"),
        );
        std::fs::remove_file(lab.paths[0].dir.join("identity")).unwrap();
        failed.actor = Some(actor);
        observe_contacts(
            &bootstrap,
            &std::cell::OnceCell::new(),
            &mut history,
            Vec::new(),
            &[failed],
            &place::Publisher::default(),
            || panic!("unavailable custody must not establish a contact"),
        );
        history.observe(
            Visibility::Active,
            0,
            "observing",
            3000,
            at + Duration::from_secs(2),
        );
        assert_eq!(
            handle.snapshot().peers[0].last_repair,
            state.peers[0].last_repair
        );
    }
}
