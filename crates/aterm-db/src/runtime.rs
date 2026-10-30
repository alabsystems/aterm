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
mod report;
mod transport;
pub(crate) use discovery::MAX_UNVERIFIED_ENDPOINTS;
pub use lan::{LanGate, LanTurn};
pub use report::RepairRound;

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

#[derive(Default)]
struct Network {
    /// What holds the owner before its observer and first LAN use ([`LanGate`]).
    lan: LanGate,
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
    /// Every transport open fails (tests).
    #[cfg(test)]
    open_fails: bool,
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
            lan,
            #[cfg(test)]
            isolated: None,
            #[cfg(test)]
            opens: None,
            #[cfg(test)]
            stand_in: None,
            #[cfg(test)]
            open_fails: false,
            used: std::cell::Cell::new(false),
            waits: std::cell::Cell::new(""),
        })
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
            return Err(crate::Error::new("the test fails the transport's open"));
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    Refused,
    /// The owner waits before its first LAN use ([`LanGate`]): it opened no observer and no
    /// socket yet, or (its host deferred the use, [`LanGate::before_use`]) no LAN socket.
    LanHeld,
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

/// Owning process lifetime. Dropping it stops and joins all network workers.
pub struct Runtime {
    stop: Arc<Stop>,
    task: Option<JoinHandle<()>>,
    observation: ObservationHandle,
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
        let stop = Arc::new(Stop::default());
        let worker = stop.clone();
        let observation = ObservationHandle::default();
        let published = observation.clone();
        let task = thread::Builder::new()
            .name("commons-owner".into())
            .spawn(move || {
                let mut network = network;
                let observer_dir = crate::network_state::directory(&config);
                let _window_claim = (!background)
                    .then(|| owner::WindowClaim::acquire(&observer_dir).ok())
                    .flatten();
                let mut previous = None;
                let mut report = |status| {
                    if previous != Some(status) {
                        observe(status);
                        previous = Some(status);
                    }
                };
                // What this owner serves of its wait while its gate or a disabled Commons holds it.
                let mut wait_relay = crate::network_state::WaitRelay::default();
                while !worker.requested() {
                    if background && owner::window_claimed(&observer_dir) {
                        wait_relay.stop();
                        let publication = crate::network_state::read_live_publication(&config)
                            .unwrap_or_else(|_| {
                                let mut retained =
                                    crate::network_state::read_history(&config).unwrap_or_default();
                                retained.visibility = Visibility::Waiting;
                                retained.detail =
                                    "An aterm window owns Commons repair while it runs".into();
                                crate::network_state::Publication::new(retained, Instant::now())
                            });
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
                    if let Some((visibility, detail, status)) = network.lan.held(&config) {
                        published.replace_publication(
                            crate::network_state::read_owner_publication(&config)
                                .unwrap_or_else(|_| held_publication(&config, visibility, detail)),
                        );
                        wait_relay.keep(&config, &published);
                        report(status);
                        worker.wait(if background { POLL } else { TICK * 2 });
                        continue;
                    }
                    wait_relay.stop();
                    // Before the observer, whose history then says nothing of a LAN use: a
                    // history with no record is one an aterm with no gate left.
                    let _ = crate::network_state::begin_lan_use(&config);
                    let mut history = match Observer::open(&config, published.clone()) {
                        Ok(history) => history,
                        Err(crate::network_state::OpenFailure::Unavailable) => {
                            Observer::unavailable(&config, published.clone())
                        }
                        Err(crate::network_state::OpenFailure::Owned) => {
                            published.replace_publication(waiting_publication(
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
                        if let Err(status) = serve_mode(
                            (&paths, &config),
                            background.then_some(observer_dir.as_path()),
                            &worker,
                            &mut report,
                            &mut history,
                            &network,
                            &reports,
                        ) {
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
                                Status::LanHeld => (Visibility::Waiting, network.waits.get()),
                                _ => (Visibility::Unavailable, "Network observation unavailable"),
                            };
                            history.observe(
                                visibility,
                                0,
                                detail,
                                aterm_time::unix_millis().unwrap_or(0),
                                Instant::now(),
                            );
                            report(status);
                            worker.wait(if background { POLL } else { TICK * 2 });
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
        })
    }
    pub fn stop(mut self) {
        self.join();
    }
    /// Pure shared observation; readers clone the value off the render path.
    pub fn network_observation(&self) -> ObservationHandle {
        self.observation.clone()
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
    config: &Path,
    visibility: Visibility,
    detail: &str,
) -> crate::network_state::Publication {
    crate::network_state::read_live_publication(config).unwrap_or_else(|_| {
        let mut retained = crate::network_state::read_history(config).unwrap_or_default();
        retained.visibility = visibility;
        retained.detail = detail.into();
        crate::network_state::Publication::new(retained, Instant::now())
    })
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
    )
}

fn serve_mode(
    (paths, config): (&Paths, &Path),
    yield_to_window: Option<&Path>,
    stop: &Stop,
    report: &mut impl FnMut(Status),
    history: &mut Observer,
    network: &Network,
    reports: &impl Fn(RepairRound),
) -> std::result::Result<(), Status> {
    history.unconfirm_local();
    if yield_to_window.is_some_and(owner::window_claimed) {
        return Err(Status::Waiting);
    }
    if !enabled(config).map_err(|_| Status::Refused)? {
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
    let lease = owner::Lease::acquire(&paths.dir.join("commons"))
        .map_err(|_| Status::Refused)?
        .ok_or(Status::Waiting)?;
    // Locals drop in reverse order: transport closes/drains before lease unlock.
    let deadline = Instant::now() + ADMISSION;
    let bootstrap = Bootstrap::cached_until(paths, deadline)
        .map_err(|_| Status::Refused)?
        .ok_or(Status::Dormant)?;
    let request = bootstrap.request().map_err(|_| Status::Refused)?;
    let endpoint =
        match Endpoint::from_bootstrap_until(&bootstrap, deadline).map_err(|_| Status::Refused)? {
            Some(endpoint) => endpoint
                .with_commons_until(paths.clone(), config.to_owned(), deadline)
                .map_err(|_| Status::Refused)?,
            None => Endpoint::pending_from_bootstrap_until(&bootstrap, paths.clone(), deadline)
                .map_err(|_| Status::Refused)?,
        };
    let pending = endpoint.pending();
    let custody =
        custody::Custody::capture(paths, bootstrap.journal_dir()).map_err(|_| Status::Refused)?;
    let current_snapshot =
        match commons::observe_until(paths, deadline).map_err(|_| Status::Refused)? {
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
        .map_err(|_| Status::Refused)?;
    let mut hearing = (!pending).then(|| Hearing::open(&bootstrap));
    let mut refused = network.lan.refused();
    let result = (|| {
        let mut poll = Instant::now() + POLL;
        while !stop.requested() {
            if yield_to_window.is_some_and(owner::window_claimed) {
                return Err(Status::Waiting);
            }
            let now = Instant::now();
            if now >= poll {
                lease.recheck().map_err(|_| Status::Refused)?;
                custody.recheck().map_err(|_| Status::Refused)?;
                if !enabled(config).map_err(|_| Status::Refused)? {
                    return Err(Status::Disabled);
                }
                match commons::observe_until(paths, now + Duration::from_millis(200))
                    .map_err(|_| Status::Refused)?
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
                        .map_err(|_| Status::Refused)?
                        .is_some()
                    {
                        return Ok(());
                    }
                    let journal = aterm_secrets::commons::journal::Journal::read_existing(
                        bootstrap.journal_dir(),
                        bootstrap.trust(),
                    )
                    .map_err(|_| Status::Refused)?;
                    if bootstrap
                        .witness_in(&journal)
                        .map_err(|_| Status::Refused)?
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
            transport.tick(now).map_err(|_| Status::Offline)?;
            for completed in transport.reports() {
                reports(completed);
            }
            #[cfg(test)]
            network.observed(&transport);
            let contacts = transport.take_contacts();
            if let Some(hearing) = &mut hearing {
                hearing.tick(paths, &bootstrap, &contacts, now);
            }
            observe_contacts(
                &bootstrap,
                history,
                contacts
                    .into_iter()
                    .map(|(actor, at, _)| (actor, at))
                    .collect(),
                || (aterm_time::unix_millis().unwrap_or(0), Instant::now()),
            );
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
    transport.stop();
    for completed in transport.reports() {
        reports(completed);
    }
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
        contacts: &[(astream_wire::sync::Actor, Instant, Said)],
        now: Instant,
    ) {
        let Some(now_ms) = aterm_time::unix_millis() else {
            return;
        };
        for (actor, at, said) in contacts {
            self.changed |= self
                .ledger
                .record(*actor, contact_time(now_ms, now, *at), *said);
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

/// Resolve completed exchanges through current membership before publishing
/// identity. Injected clock reads keep freshness tests independent of speed.
fn observe_contacts(
    bootstrap: &Bootstrap,
    history: &mut Observer,
    contacts: Vec<(astream_wire::sync::Actor, Instant)>,
    clock: impl Fn() -> (u64, Instant),
) {
    if contacts.is_empty() {
        return;
    }
    let journal = aterm_secrets::commons::journal::Journal::read_existing(
        bootstrap.journal_dir(),
        bootstrap.trust(),
    );
    if let Ok(journal) = journal
        && bootstrap.revalidate_custody().is_ok()
        && let Ok(Some(access)) = bootstrap.access_in(&journal)
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
    }
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
    report(Status::Dormant);
    while !stop.requested() {
        if yield_to_window.is_some_and(owner::window_claimed) {
            return Err(Status::Waiting);
        }
        let now = Instant::now();
        if now >= poll {
            if !enabled(config).map_err(|_| Status::Refused)? {
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
    history.observe(
        visibility,
        transport.unverified_peers(),
        detail,
        aterm_time::unix_millis().unwrap_or(0),
        Instant::now(),
    );
}

#[cfg(not(test))]
fn addresses() -> std::result::Result<Vec<std::net::SocketAddr>, Status> {
    #[cfg(debug_assertions)]
    if isolation::port().map_err(|_| Status::Refused)?.is_some() {
        return Ok(vec![(std::net::Ipv4Addr::LOCALHOST, 0).into()]);
    }
    aterm_net::lan::Listener::candidates()
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
