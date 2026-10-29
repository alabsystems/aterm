// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Background network reachability probe (`aterm-harness-netprobe`) for `harness::netwatch`.

use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs as _};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use aterm_agent::harness::netwatch::{
    self, API_HOST, API_PORT, NetState, Outcome, Route, RouteFacts, Source,
};
use aterm_agent::supervise::policy::WorkerEnv;
use aterm_agent::supervise::policy::claude_settings;
use aterm_agent::supervise::policy::turn_end::Reach;

/// One reading of this Mac's network ([`PathSource::watch`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathEvent {
    /// The path in use: whether it can carry traffic ([`NetState::path`]).
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "only macOS's path monitor reads the path")
    )]
    Usable(bool),
    /// The routes moved (an address or a link changed), which says nothing of whether the path
    /// can carry traffic ([`NetState::moved`]): a switch between two usable paths, or on a host
    /// with no path monitor the only word of a change.
    #[cfg_attr(
        all(not(any(target_os = "macos", target_os = "linux")), not(test)),
        expect(dead_code, reason = "the route source is macOS's and Linux's")
    )]
    Moved,
}

/// Where each reading of this Mac's network goes ([`PathSource::watch`]). It may be called once
/// more after its watch is dropped.
pub(crate) type PathSink = Box<dyn Fn(PathEvent) + Send + Sync + 'static>;

/// A started path watch, which keeps reading while it lives.
pub(crate) type PathWatch = Box<dyn std::any::Any + Send>;

/// This Mac's network path ([`NetProbe::watch_path`]), replaceable in tests.
pub(crate) trait PathSource: Send + Sync {
    /// Starts a watch whose every reading reaches `sink`, the first of them the path in use now;
    /// it reads while the returned watch lives. `None` when no watch could start.
    fn watch(&self, sink: PathSink) -> Option<PathWatch>;
}

/// The live path source. On macOS, Network.framework's path monitor
/// ([`aterm_objc::path_monitor::PathMonitor`]): a satisfied path can carry traffic, and so can
/// a satisfiable one (a connection attempt brings it up, as an on-demand VPN), while an
/// unsatisfied path cannot (an invalid one says neither, and reaches no sink). On macOS and
/// Linux, this process's route moves too ([`crate::network_moved`]'s [`Woke::Route`], the
/// kernel's route socket), each a [`PathEvent::Moved`] at once (the probe dials the API, which no
/// fleet herd shares, so it waits out no spread: [`crate::network_moved::watch_at_once`]); the
/// other sources that signal tells of
/// (the fleet's browse and beacons, the Local Network answer) say nothing of the API's reach and
/// are not passed on. Elsewhere none.
///
/// [`Woke::Route`]: crate::network_moved::Woke::Route
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SystemPath;

impl PathSource for SystemPath {
    #[cfg(target_os = "macos")]
    fn watch(&self, sink: PathSink) -> Option<PathWatch> {
        use aterm_objc::path_monitor::PathMonitor;
        let sink: Arc<dyn Fn(PathEvent) + Send + Sync> = Arc::from(sink);
        let routes = RouteMoves::start(Arc::clone(&sink), crate::network_moved::watch_at_once());
        let monitor = PathMonitor::start(c"aterm.harness.net-path", move |reading| {
            if let Some(usable) = usable(reading.status) {
                sink(PathEvent::Usable(usable));
            }
        });
        match (monitor, routes) {
            (None, None) => None,
            (monitor, routes) => Some(Box::new((monitor, routes))),
        }
    }

    #[cfg(target_os = "linux")]
    fn watch(&self, sink: PathSink) -> Option<PathWatch> {
        let routes = RouteMoves::start(Arc::from(sink), crate::network_moved::watch_at_once())?;
        Some(Box::new(routes))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn watch(&self, _: PathSink) -> Option<PathWatch> {
        None
    }
}

/// Whether a Network.framework path of `status` can carry traffic: a satisfied one can, and so
/// can a satisfiable one, which a connection attempt brings up (an on-demand VPN); an
/// unsatisfied one cannot; an invalid one says neither (`None`).
#[cfg(target_os = "macos")]
const fn usable(status: aterm_objc::path_monitor::PathStatus) -> Option<bool> {
    use aterm_objc::path_monitor::PathStatus;
    match status {
        PathStatus::Satisfied | PathStatus::Satisfiable => Some(true),
        PathStatus::Unsatisfied => Some(false),
        PathStatus::Invalid => None,
    }
}

/// Whether a move `woke` heard is passed on as a [`PathEvent::Moved`]: only the kernel's route
/// socket's, since the fleet's browse and beacons and the Local Network answer say nothing of
/// the API's reach.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
const fn route_move(woke: crate::network_moved::Woke) -> bool {
    matches!(woke, crate::network_moved::Woke::Route)
}

/// [`SystemPath`]'s consumer of this process's route moves: a thread that passes each move its
/// watch hears from the route socket ([`route_move`]) on as a [`PathEvent::Moved`] while this
/// lives, and ends at its first wake after its drop, within one [`RouteMoves::SLICE`].
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct RouteMoves(Arc<AtomicBool>);

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl RouteMoves {
    /// The longest the thread waits at once before it looks whether it was dropped.
    const SLICE: Duration = Duration::from_secs(60);

    /// Starts the thread over `moved` (this process's signal, [`crate::network_moved::watch_at_once`]);
    /// `None` when it could not start.
    fn start(
        sink: Arc<dyn Fn(PathEvent) + Send + Sync>,
        mut moved: crate::network_moved::Watch,
    ) -> Option<Self> {
        let dropped = Arc::new(AtomicBool::new(false));
        let ended = Arc::clone(&dropped);
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-harness-netmoved",
            crate::qos::Role::Background,
            move || {
                while !ended.load(Ordering::SeqCst) {
                    if route_move(moved.wait(Self::SLICE)) && !ended.load(Ordering::SeqCst) {
                        sink(PathEvent::Moved);
                    }
                }
            },
        );
        match spawned {
            Ok(_) => Some(Self(dropped)),
            Err(e) => {
                aterm_log::warn!(
                    "harness: the API reach probe hears no route moves ({e}); its cadence alone \
                     finds the API's return after a switch between usable networks"
                );
                None
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for RouteMoves {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A path source that starts no watch (`Acts::inert`).
#[cfg(any(test, feature = "bench-support"))]
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct InertPath;

#[cfg(any(test, feature = "bench-support"))]
impl PathSource for InertPath {
    fn watch(&self, _: PathSink) -> Option<PathWatch> {
        None
    }
}

/// Network probe operations replaceable in tests.
pub(crate) trait Probe: Send + Sync {
    /// Resolves `host` and `port` to socket addresses.
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>;
    /// Connects to `addr` within `within`.
    fn connect(&self, addr: SocketAddr, within: Duration) -> io::Result<TcpStream>;
    /// Completes a platform-verified TLS handshake to `host` over `tcp` before `deadline`.
    fn handshake(
        &self,
        tcp: TcpStream,
        host: &str,
        deadline: aterm_http::Deadline,
    ) -> io::Result<()>;
}

/// Production probe using the system resolver, timed TCP connect, and platform TLS verification.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SystemProbe;

impl Probe for SystemProbe {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok((host, port).to_socket_addrs()?.collect())
    }

    fn connect(&self, addr: SocketAddr, within: Duration) -> io::Result<TcpStream> {
        aterm_http::connect::connect_timeout(&addr, within)
    }

    fn handshake(
        &self,
        tcp: TcpStream,
        host: &str,
        deadline: aterm_http::Deadline,
    ) -> io::Result<()> {
        verified_handshake(tcp, host, deadline)
    }
}

/// Completes a TLS handshake to `host` over `tcp` verified against the OS trust store.
pub(crate) fn verified_handshake(
    tcp: TcpStream,
    host: &str,
    deadline: aterm_http::Deadline,
) -> io::Result<()> {
    let config = aterm_http::tls::client_config(&aterm_http::Trust::PlatformVerifier)
        .map_err(io::Error::other)?;
    aterm_http::stream::Stream::start_tls(
        tcp,
        config,
        host,
        Arc::new(aterm_http::AlwaysAuthorized),
        deadline,
    )
    .map(drop)
}

/// A failure that says the address cannot be reached from here.
fn definite_failure(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::HostUnreachable
            | io::ErrorKind::NetworkDown
    ) || raw_unreachable(e)
}

/// A resolver answer that specifically says the name has no address.
fn definite_name_failure(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::NotFound {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        e.to_string()
            == "failed to lookup address information: nodename nor servname provided, or not known"
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The same four answers by raw errno.
#[cfg(unix)]
fn raw_unreachable(e: &io::Error) -> bool {
    e.raw_os_error().is_some_and(|code| {
        [
            libc::ECONNREFUSED,
            libc::ENETUNREACH,
            libc::EHOSTUNREACH,
            libc::ENETDOWN,
        ]
        .contains(&code)
    })
}

#[cfg(not(unix))]
fn raw_unreachable(_: &io::Error) -> bool {
    false
}

/// `addrs`, IPv4 and IPv6 interleaved, IPv4 first.
fn interleaved(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let (v4, v6): (Vec<SocketAddr>, Vec<SocketAddr>) =
        addrs.into_iter().partition(SocketAddr::is_ipv4);
    let mut out = Vec::with_capacity(v4.len() + v6.len());
    let (mut a, mut b) = (v4.into_iter(), v6.into_iter());
    loop {
        match (a.next(), b.next()) {
            (None, None) => break,
            (x, y) => {
                out.extend(x);
                out.extend(y);
            }
        }
    }
    out
}

/// Runs one probe of the default API route within `budget`.
pub(crate) fn probe_once(probe: &dyn Probe, budget: Duration) -> Outcome {
    let deadline = aterm_http::Deadline::after(budget);
    let in_time = |definite: Outcome| {
        if deadline.remaining().is_some() {
            definite
        } else {
            Outcome::Unknown
        }
    };
    let addrs = match probe.resolve(API_HOST, API_PORT) {
        Ok(addrs) if !addrs.is_empty() => interleaved(addrs),
        Ok(_) => return in_time(Outcome::Down),
        Err(e) if definite_name_failure(&e) => return in_time(Outcome::Down),
        Err(_) => return Outcome::Unknown,
    };
    let n = addrs.len();
    let mut definite = 0usize;
    for (i, addr) in addrs.into_iter().enumerate() {
        let Some(left) = deadline.remaining() else {
            return Outcome::Unknown;
        };
        let share = left / u32::try_from(n - i).unwrap_or(u32::MAX);
        if share.is_zero() {
            return Outcome::Unknown;
        }
        match probe.connect(addr, share) {
            Ok(tcp) => {
                return match probe.handshake(tcp, API_HOST, deadline) {
                    Ok(()) => Outcome::Up,
                    Err(_) => Outcome::Unknown,
                };
            }
            Err(e) if definite_failure(&e) => definite += 1,
            Err(_) => {}
        }
    }
    if definite == n {
        in_time(Outcome::Down)
    } else {
        Outcome::Unknown
    }
}

/// Instance network reachability probe state and worker thread handle.
pub(crate) struct NetProbe {
    state: Mutex<NetState>,
    wake: Condvar,
    stop: AtomicBool,
    started: AtomicBool,
    probe: Arc<dyn Probe>,
    /// Threads spawned (one, ever — the tests' witness).
    spawned: AtomicUsize,
    /// The network path this probe watches from its first ask, peek or path-only read
    /// ([`Self::path_from`]); none until one is installed.
    path: OnceLock<Arc<dyn PathSource>>,
    /// That path's watch while it runs ([`Self::watch_path`]), dropped by [`Self::stop`].
    watch: Mutex<Option<PathWatch>>,
    /// A watch was tried ([`Self::watch_path`]), started or not: none is tried again.
    watched: AtomicBool,
}

impl std::fmt::Debug for NetProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetProbe")
            .field("started", &self.started)
            .field("stop", &self.stop)
            .finish_non_exhaustive()
    }
}

impl NetProbe {
    /// Creates the production probe with default steps and budget.
    pub(crate) fn new() -> Arc<Self> {
        Self::with(Arc::new(SystemProbe), netwatch::PROBE_BUDGET)
    }

    /// Creates a probe using `probe` and `budget`.
    pub(crate) fn with(probe: Arc<dyn Probe>, budget: Duration) -> Arc<Self> {
        Self::with_state(probe, NetState::with_budget(budget))
    }

    /// Creates a probe using `probe` over `state` (its budget and [`netwatch::Cadence`]).
    pub(crate) fn with_state(probe: Arc<dyn Probe>, state: NetState) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(state),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            started: AtomicBool::new(false),
            probe,
            spawned: AtomicUsize::new(0),
            path: OnceLock::new(),
            watch: Mutex::new(None),
            watched: AtomicBool::new(false),
        })
    }

    fn lock(&self) -> MutexGuard<'_, NetState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queries reachability for session `who`, renewing its lease and waking the probe thread.
    pub(crate) fn ask(self: &Arc<Self>, who: &str) -> Reach {
        let reach = self.lock().ask(who, Instant::now(), SystemTime::now());
        self.ensure_thread();
        self.wake.notify_all();
        reach
    }

    /// The verdict now ([`NetState::reach`]) for a session the probe measures, read without an
    /// ask: it takes no lease, starts no thread and probes nothing, so a loop may read it at
    /// every point. It starts the installed path source's watch once ([`Self::watch_path`]),
    /// which sends nothing, so a lost path reads Down before any session asks.
    pub(crate) fn peek(self: &Arc<Self>) -> Reach {
        self.watch_path();
        self.lock().reach(Instant::now(), SystemTime::now())
    }

    /// Installs `source` as the network path this probe watches, from the first ask, peek or
    /// path-only read on ([`Self::watch_path`]); `false` when one was installed already (it
    /// stays).
    pub(crate) fn path_from(&self, source: Arc<dyn PathSource>) -> bool {
        self.path.set(source).is_ok()
    }

    /// Starts the installed path source's watch, once ever: each reading reaches
    /// [`Self::path_moved`] while this probe lives. Nothing with no source installed, once a
    /// watch was tried (one that could not start is not tried again), or the probe stopped.
    pub(crate) fn watch_path(self: &Arc<Self>) {
        let Some(source) = self.path.get() else {
            return;
        };
        let mut watch = self.watch.lock().unwrap_or_else(PoisonError::into_inner);
        if self.stop.load(Ordering::SeqCst) || self.watched.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = Arc::downgrade(self);
        *watch = source.watch(Box::new(move |event| {
            if let Some(me) = me.upgrade() {
                me.path_moved(event);
            }
        }));
        if watch.is_none() {
            aterm_log::info!(
                "harness: no network path watch; the probe's cadence alone finds the API's \
                 return, and a session it does not measure reads unknown"
            );
        }
    }

    /// The path's watch read whether it can carry traffic ([`NetState::path`]), or heard the
    /// routes move ([`NetState::moved`]). The reading is recorded under the state's mutex, and
    /// when it owes a probe the thread is woken under that mutex too, so a thread that checked
    /// its schedule and is about to park cannot miss the wake. A change of the path after its
    /// first reading is logged once, and a move that owes a probe once, outside the lock.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "netprobe_shutdown_park",
            action = "Nudge",
            project = "harness_netprobe::tests::project_shutdown_park"
        )
    )]
    pub(crate) fn path_moved(&self, event: PathEvent) {
        let (was, owed) = {
            let mut st = self.lock();
            let was = st.path_satisfied();
            let owed = match event {
                PathEvent::Usable(satisfied) => {
                    st.path(satisfied, Instant::now(), SystemTime::now())
                }
                PathEvent::Moved => st.moved(Instant::now()),
            };
            if owed {
                self.wake.notify_all();
            }
            (was, owed)
        };
        let satisfied = match event {
            PathEvent::Usable(satisfied) => satisfied,
            PathEvent::Moved => {
                if owed {
                    aterm_log::info!(
                        "harness: the network's routes moved; the API reach is measured again \
                         once they settle"
                    );
                }
                return;
            }
        };
        match (was, satisfied) {
            (Some(true), false) => {
                aterm_log::info!("harness: network path lost; the API reach reads down");
            }
            (Some(false), true) => {
                aterm_log::info!(
                    "harness: network path back; the API reach is read again once it settles"
                );
            }
            _ => {}
        }
    }

    /// When a new measure can next land ([`NetState::landing`]): always later than now, `None`
    /// while no lease lives and no probe flies. It takes no lease and never wakes the thread.
    pub(crate) fn landing(&self) -> Option<Instant> {
        self.lock().landing(Instant::now(), SystemTime::now())
    }

    /// The verdict the network path alone gives a session this probe does not measure
    /// ([`NetState::path_reach`]: Down while the path is lost, for at most
    /// [`netwatch::LOST_CAP`], or settling, unless a fresh Up was measured since the loss; else
    /// Unknown). It starts the installed path source's watch once ([`Self::watch_path`]); it
    /// takes no lease, starts no thread and probes nothing, so a session that only reads the
    /// path sends nothing to the API host.
    pub(crate) fn path_reach(self: &Arc<Self>) -> Reach {
        self.watch_path();
        self.lock().path_reach(Instant::now(), SystemTime::now())
    }

    /// When the path-only verdict can next change ([`NetState::path_landing`]): always later
    /// than now, `None` with no path lost or settling. It starts nothing and takes no lease.
    pub(crate) fn path_landing(&self) -> Option<Instant> {
        self.lock().path_landing(Instant::now(), SystemTime::now())
    }

    /// Signals the probe thread to stop at its next wake, then drops the path's watch (a
    /// reading it already queued may still land, and changes nothing a stopped thread acts on).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "netprobe_shutdown_park",
            action = "Stop",
            project = "harness_netprobe::tests::project_shutdown_park"
        )
    )]
    pub(crate) fn stop(&self) {
        {
            let _state = self.lock();
            self.stop.store(true, Ordering::SeqCst);
            self.wake.notify_all();
        }
        let watch = self
            .watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        drop(watch);
    }

    /// The probe thread, spawned at the first ask, and the path's watch with it
    /// ([`Self::watch_path`]).
    fn ensure_thread(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        self.watch_path();
        let me = Arc::clone(self);
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-harness-netprobe",
            crate::qos::Role::Background,
            move || {
                me.spawned.fetch_add(1, Ordering::SeqCst);
                me.run();
            },
        );
        if let Err(e) = spawned {
            self.started.store(false, Ordering::SeqCst);
            aterm_log::warn!("harness: the API reach probe could not start: {e}");
        }
    }

    /// The thread: parked while no lease lives, a probe when one is due, one at a time.
    fn run(&self) {
        self.run_before_park(|| {});
    }

    /// Runs the probe loop with `before_park` called before parking on the condvar.
    fn run_before_park(&self, mut before_park: impl FnMut()) {
        loop {
            {
                let mut st = self.lock();
                loop {
                    if self.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let now = Instant::now();
                    let wait = match st.next_probe(now, SystemTime::now()) {
                        None => None,
                        Some(at) if at > now => Some(at - now),
                        Some(_) => {
                            if st.begin(now) {
                                break;
                            }
                            continue;
                        }
                    };
                    before_park();
                    st = self.park(st, wait);
                }
            }
            let budget = self.lock().budget();
            let outcome = probe_once(&*self.probe, budget);
            self.settle(outcome, Instant::now(), SystemTime::now());
        }
    }

    /// A probe ended with `outcome`: its measure recorded, and a change of the verdict
    /// ([`netwatch::Edge`]) logged once, outside the lock. A repeat logs nothing.
    fn settle(&self, outcome: Outcome, now: Instant, wall: SystemTime) {
        let (edge, waiting) = {
            let mut st = self.lock();
            let edge = st.finish(outcome, now, wall);
            (edge, st.waiting(now))
        };
        if let Some(edge) = edge {
            aterm_log::info!("{}", edge_line(&edge, waiting));
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "netprobe_shutdown_park",
            action = "Park",
            project = "harness_netprobe::tests::project_shutdown_park"
        )
    )]
    fn park<'a>(
        &self,
        state: MutexGuard<'a, NetState>,
        wait: Option<Duration>,
    ) -> MutexGuard<'a, NetState> {
        match wait {
            None => self
                .wake
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner),
            Some(wait) => {
                self.wake
                    .wait_timeout(state, wait)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
        }
    }
}

/// The log line of one change of the measured verdict, `waiting` leases live.
fn edge_line(edge: &netwatch::Edge, waiting: usize) -> String {
    let probe = format!("probe {} ms, {waiting} waiting", edge.took.as_millis());
    match edge.from {
        Some(from) => format!(
            "harness: API reach {}→{} after {} ({probe})",
            from.word(),
            edge.to.word(),
            crate::presence::fmt_dur(edge.held),
        ),
        None => format!(
            "harness: API reach {} (no fresh measure before it; {probe})",
            edge.to.word()
        ),
    }
}

/// What decides an agent's route, read now.
fn gather(
    env: Option<Vec<String>>,
    argv: &[String],
    cwd: &Path,
    managed: Option<&Path>,
    prefs: &Path,
    home: Option<&Path>,
) -> RouteFacts {
    let Some(env) = env else {
        return RouteFacts::default();
    };
    let worker = WorkerEnv::new(atpkg::caller_shell::ProcArgs {
        argv: argv.to_vec(),
        env: env.clone(),
        ..Default::default()
    });
    let settings = claude_settings::sources_under(&worker, cwd, managed, home)
        .into_iter()
        .chain(claude_settings::managed_preferences(&worker, prefs))
        .map(|source| (source.name, Source::from(source.text)))
        .collect();
    RouteFacts {
        env: Some(env),
        settings,
    }
}

/// Reads agent `pid`'s API route from its environment and Claude Code settings.
pub(crate) fn route_of_agent(pid: u32, argv: &[String], cwd: &str) -> Route {
    let env = atpkg::caller_shell::process_args(pid).map(|a| a.env);
    netwatch::route_of(&gather(
        env,
        argv,
        Path::new(cwd),
        claude_settings::MANAGED_DIR.map(Path::new),
        Path::new(claude_settings::MANAGED_PREFERENCES_DIR),
        aterm_types::dirs::home_dir().as_deref(),
    ))
}

/// Returns whether agent `pid`'s settings leave its stored login active.
pub(crate) fn stored_login_of_agent(pid: u32, argv: &[String], cwd: &str) -> bool {
    let env = atpkg::caller_shell::process_args(pid).map(|a| a.env);
    let home = aterm_types::dirs::home_dir();
    stored_login_of(
        &gather(
            env,
            argv,
            Path::new(cwd),
            claude_settings::MANAGED_DIR.map(Path::new),
            Path::new(claude_settings::MANAGED_PREFERENCES_DIR),
            home.as_deref(),
        ),
        home.as_deref(),
    )
}

/// Whether a relaunch line of `argv` in `cwd` finds its settings chain admit the usage tap,
/// read as the relaunch's own gate reads it (`upgrade_drive::tap_settings_admit`: under the
/// host's home, `settings.json` and `cowork_settings.json` alike), not as the running
/// process's own environment picks one of the two.
pub(crate) fn tap_settings_admit_line(argv: &[String], cwd: &str) -> bool {
    aterm_primer::home_dir().is_some_and(|home| {
        aterm_agent::harness::upgrade_drive::tap_settings_admit(&home, argv, cwd)
    })
}

/// [`stored_login_of_agent`] over what was gathered.
fn stored_login_of(facts: &RouteFacts, home: Option<&Path>) -> bool {
    use aterm_agent::harness::accounts;
    let Some(env) = facts.env.as_ref().filter(|env| !env.is_empty()) else {
        return false;
    };
    let pairs = || env.iter().filter_map(|kv| kv.split_once('='));
    let profile = accounts::profile_configs_dir(pairs()).is_some_and(|configs| {
        let unseen = home.is_some_and(|home| {
            let real = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
            let resolved = std::fs::canonicalize(&configs).unwrap_or_else(|_| configs.clone());
            atpkg::protected::under_file_provider_domain(home, &configs)
                || atpkg::protected::under_file_provider_domain(&real, &resolved)
        });
        unseen || accounts::profile_configured(&configs)
    });
    accounts::ProcessLogin::of_env(pairs())
        .with_settings(&facts.settings)
        .with_profile_store(profile)
        .stored_login
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};
    use std::sync::mpsc;

    const BUDGET: Duration = Duration::from_secs(2);

    /// Which publisher of a wake (a stop, or a path's nudge) the fixture runs: the shipping
    /// mutex-guarded one, or the unlocked negative control.
    #[derive(Clone, Copy)]
    enum ShutdownPublisher {
        MutexGuarded,
        HistoricalUnlocked,
    }

    #[derive(Debug)]
    struct ShutdownObservation {
        action: &'static str,
        stopped: bool,
    }

    impl ShutdownObservation {
        fn read(action: &'static str, probe: &NetProbe) -> Self {
            Self {
                action,
                stopped: probe.stop.load(Ordering::SeqCst),
            }
        }
    }

    fn project_shutdown_park(observed: &[ShutdownObservation]) -> aterm_spec::interp::State {
        let at = |action: &str| observed.iter().position(|step| step.action == action);
        let (park, stop, nudge) = (at("Park"), at("Stop"), at("Nudge"));
        aterm_spec::interp::State::from([
            ("checked", i64::from(park.is_none())),
            ("parked", i64::from(park.is_some())),
            (
                "stopped",
                i64::from(observed.last().is_some_and(|step| step.stopped)),
            ),
            (
                "notified",
                i64::from(park.zip(stop).is_some_and(|(park, stop)| park < stop)),
            ),
            ("nudged", i64::from(nudge.is_some())),
            (
                "woken",
                i64::from(park.zip(nudge).is_some_and(|(park, nudge)| park < nudge)),
            ),
        ])
    }

    /// A schedule that probes again only an hour after an Up, and at once for a path's return
    /// (no settle and no gap).
    fn slow_but_nudged() -> NetState {
        NetState::with_cadence(
            BUDGET,
            netwatch::Cadence {
                steady: Duration::from_secs(3600),
                gap: Duration::ZERO,
                settle: Duration::ZERO,
            },
        )
    }

    /// A probe that measures Up over a loopback listener that never accepts, and says each
    /// resolve on a channel.
    struct Answering {
        listener: TcpListener,
        resolved: Mutex<mpsc::Sender<()>>,
    }

    impl Answering {
        fn new() -> (Arc<Self>, mpsc::Receiver<()>) {
            let (tx, rx) = mpsc::channel();
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
            (
                Arc::new(Self {
                    listener,
                    resolved: Mutex::new(tx),
                }),
                rx,
            )
        }
    }

    impl Probe for Answering {
        fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
            assert_eq!((host, port), (API_HOST, API_PORT));
            let _ = self.resolved.lock().unwrap().send(());
            Ok(vec![self.listener.local_addr()?])
        }
        fn connect(&self, addr: SocketAddr, within: Duration) -> io::Result<TcpStream> {
            SystemProbe.connect(addr, within)
        }
        fn handshake(&self, _: TcpStream, _: &str, _: aterm_http::Deadline) -> io::Result<()> {
            Ok(())
        }
    }

    /// A path source that hands its sink to the test, counts the watches it started, and
    /// says when the watch is dropped.
    #[derive(Default)]
    struct Handing {
        sink: Mutex<Option<PathSink>>,
        started: AtomicUsize,
        dropped: Arc<AtomicBool>,
    }

    /// [`Handing`]'s watch: its drop is the stop's witness.
    struct HandedWatch(Arc<AtomicBool>);

    impl Drop for HandedWatch {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl PathSource for Handing {
        fn watch(&self, sink: PathSink) -> Option<PathWatch> {
            self.started.fetch_add(1, Ordering::SeqCst);
            *self.sink.lock().unwrap() = Some(sink);
            Some(Box::new(HandedWatch(Arc::clone(&self.dropped))))
        }
    }

    impl Handing {
        /// The watch reads the path: `satisfied`, on the calling thread.
        fn read(&self, satisfied: bool) {
            let sink = self.sink.lock().unwrap();
            (sink.as_ref().expect("a watch started"))(PathEvent::Usable(satisfied));
        }
    }

    /// The real probe loop, parked an hour after an Up, and `event` published at its checked,
    /// mutex-held park boundary by `publisher`: the shipping [`NetProbe::path_moved`], or a
    /// notification ahead of the nudge it announces. The event is a path's return (the path
    /// lost before the park) or a route move (the path usable throughout). Either way the worker
    /// then probes it (the unlocked one only once rescued) and stops.
    fn nudge_at_park_trace(
        publisher: ShutdownPublisher,
        event: PathEvent,
    ) -> Vec<ShutdownObservation> {
        let (probe, resolved) = Answering::new();
        let np = NetProbe::with_state(probe, slow_but_nudged());
        let released = Arc::downgrade(&np);
        let (t, w) = (Instant::now(), SystemTime::now());
        {
            let mut st = np.lock();
            st.ask("s-1", t, w);
            assert!(!st.path(true, t, w));
            if event == PathEvent::Usable(true) {
                assert!(
                    !st.path(false, t, w),
                    "PRECONDITION: a lost path owes no probe"
                );
            }
        }
        let trace = Arc::new(Mutex::new(Vec::new()));
        let (checked_tx, checked_rx) = mpsc::channel();
        let (park_tx, park_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = {
            let np = Arc::clone(&np);
            let trace = Arc::clone(&trace);
            std::thread::spawn(move || {
                let mut first_park = true;
                np.run_before_park(|| {
                    if first_park {
                        first_park = false;
                        checked_tx.send(()).unwrap();
                        park_rx.recv().unwrap();
                        trace
                            .lock()
                            .unwrap()
                            .push(ShutdownObservation::read("Park", &np));
                    }
                });
                done_tx.send(()).unwrap();
            })
        };
        resolved
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the lease's first probe");
        checked_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("worker reached its checked, mutex-held park boundary after the Up");
        let nudger = match publisher {
            ShutdownPublisher::HistoricalUnlocked => {
                np.wake.notify_all();
                trace
                    .lock()
                    .unwrap()
                    .push(ShutdownObservation::read("Nudge", &np));
                None
            }
            ShutdownPublisher::MutexGuarded => {
                let np = Arc::clone(&np);
                let trace = Arc::clone(&trace);
                Some(std::thread::spawn(move || {
                    np.path_moved(event);
                    trace
                        .lock()
                        .unwrap()
                        .push(ShutdownObservation::read("Nudge", &np));
                }))
            }
        };
        park_tx.send(()).unwrap();
        if matches!(publisher, ShutdownPublisher::HistoricalUnlocked) {
            let owed = match event {
                PathEvent::Usable(satisfied) => {
                    np.lock().path(satisfied, Instant::now(), SystemTime::now())
                }
                PathEvent::Moved => np.lock().moved(Instant::now()),
            };
            assert!(
                owed,
                "the nudge its notification announced, published with no wake of its own"
            );
            let _state = np.lock();
            np.wake.notify_all();
        }
        if let Some(nudger) = nudger {
            nudger.join().unwrap();
        }
        resolved
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the parked worker woke and probed the path's return");
        np.stop();
        done_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("stopped worker exits without another ask or probe");
        worker.join().unwrap();
        drop(np);
        assert!(released.upgrade().is_none(), "no worker retains the probe");
        Arc::try_unwrap(trace).unwrap().into_inner().unwrap()
    }

    /// A path's return reaches a probe thread parked for an hour: `path_moved` publishes the
    /// owed probe and its wake under the state mutex, so the worker that checked its schedule
    /// and parks wakes and probes the return at once (NudgedWaiterHasWake). A notification ahead
    /// of the nudge, outside the mutex, is the model's `Buggy=2`, which the trace replays.
    #[test]
    fn a_paths_return_cannot_lose_the_park_wake_and_the_parked_probe_measures_it() {
        let model = aterm_models_gui::netprobe_shutdown_park_model();
        let observed =
            nudge_at_park_trace(ShutdownPublisher::MutexGuarded, PathEvent::Usable(true));
        assert_eq!(
            observed.iter().map(|step| step.action).collect::<Vec<_>>(),
            ["Park", "Nudge"]
        );
        let mut state = project_shutdown_park(&[]);
        assert_eq!(state, model.init_state());
        for (index, step) in observed.iter().enumerate() {
            assert!(model.fire(step.action, &mut state), "{step:?}: {state:?}");
            assert_eq!(state, project_shutdown_park(&observed[..=index]));
            assert!(model.check_invariant("NudgedWaiterHasWake", &state));
            assert!(model.check_invariant("StoppedWaiterHasWake", &state));
        }

        let old_order = nudge_at_park_trace(
            ShutdownPublisher::HistoricalUnlocked,
            PathEvent::Usable(true),
        );
        assert_eq!(
            old_order.iter().map(|step| step.action).collect::<Vec<_>>(),
            ["Nudge", "Park"]
        );
        assert!(!model.action_enabled(old_order[0].action, &project_shutdown_park(&[])));
        let mutant = aterm_spec::interp::with_buggy(&model, 2);
        let mut state = project_shutdown_park(&[]);
        assert_eq!(state, mutant.init_state());
        for (index, step) in old_order.iter().enumerate() {
            assert!(mutant.fire(step.action, &mut state));
            assert_eq!(state, project_shutdown_park(&old_order[..=index]));
        }
        assert!(
            !mutant.check_invariant("NudgedWaiterHasWake", &state),
            "a notification ahead of the nudge must be caught"
        );
    }

    /// A route move on a usable path reaches a probe thread parked for an hour the same way: it
    /// publishes the owed probe and its wake under the state mutex (NudgedWaiterHasWake), so the
    /// parked worker probes at once, and says so in one line; a notification ahead of it is the
    /// model's `Buggy=2`.
    #[test]
    fn a_route_move_cannot_lose_the_park_wake_and_the_parked_probe_measures_it() {
        let model = aterm_models_gui::netprobe_shutdown_park_model();
        let observed = nudge_at_park_trace(ShutdownPublisher::MutexGuarded, PathEvent::Moved);
        assert_eq!(
            observed.iter().map(|step| step.action).collect::<Vec<_>>(),
            ["Park", "Nudge"]
        );
        let mut state = project_shutdown_park(&[]);
        for (index, step) in observed.iter().enumerate() {
            assert!(model.fire(step.action, &mut state), "{step:?}: {state:?}");
            assert_eq!(state, project_shutdown_park(&observed[..=index]));
            assert!(model.check_invariant("NudgedWaiterHasWake", &state));
        }
        let old_order =
            nudge_at_park_trace(ShutdownPublisher::HistoricalUnlocked, PathEvent::Moved);
        let mutant = aterm_spec::interp::with_buggy(&model, 2);
        let mut state = project_shutdown_park(&[]);
        for step in &old_order {
            assert!(mutant.fire(step.action, &mut state));
        }
        assert!(
            !mutant.check_invariant("NudgedWaiterHasWake", &state),
            "a notification ahead of the move's nudge must be caught"
        );

        let (probe, _resolved) = Answering::new();
        let np = NetProbe::with_state(probe, slow_but_nudged());
        let moved_lines = || {
            crate::test_log::logged_while(|| np.path_moved(PathEvent::Moved))
                .into_iter()
                .filter(|line| line.starts_with("harness: the network's routes moved"))
                .count()
        };
        assert_eq!(moved_lines(), 0, "no lease: nothing owed, nothing said");
        let (t, w) = (Instant::now(), SystemTime::now());
        np.lock().ask("s-1", t, w);
        assert_eq!(moved_lines(), 1, "a move that owes a probe is said once");
    }

    /// The installed path source starts at the probe's first ask and only then, once; its
    /// readings reach the schedule (a loss reads Down at once, a return wakes the parked
    /// thread to probe it) and each change is logged once; stop drops the watch, and a stopped
    /// probe starts none.
    #[test]
    fn the_first_ask_starts_the_path_watch_once_and_stop_drops_it() {
        let (probe, resolved) = Answering::new();
        let np = NetProbe::with_state(probe, slow_but_nudged());
        np.watch_path();
        let source = Arc::new(Handing::default());
        assert!(np.path_from(Arc::clone(&source) as Arc<dyn PathSource>));
        assert!(
            !np.path_from(Arc::new(Handing::default())),
            "one source, installed once"
        );
        assert_eq!(
            source.started.load(Ordering::SeqCst),
            0,
            "not before an ask"
        );
        assert_eq!(np.ask("s-1"), Reach::Unknown);
        assert_eq!(source.started.load(Ordering::SeqCst), 1, "the first ask");
        resolved
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the first probe");
        let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !matches!(np.ask("s-1"), Reach::Up { .. }) {
            assert!(Instant::now() < until, "the first probe's Up never landed");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(source.started.load(Ordering::SeqCst), 1, "only the first");
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&lines);
        crate::test_log::observe(
            move |record| {
                let line = record.args().to_string();
                if line.starts_with("harness: network path") {
                    seen.lock().unwrap().push(line);
                }
            },
            || {
                source.read(true);
                source.read(false);
                assert!(
                    matches!(
                        np.lock().reach(Instant::now(), SystemTime::now()),
                        Reach::Down { .. }
                    ),
                    "a lost path reads Down at once"
                );
                source.read(false);
                source.read(true);
            },
        );
        resolved
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the return woke the parked thread to probe it");
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("network path lost"), "{lines:?}");
        assert!(lines[1].contains("network path back"), "{lines:?}");
        assert!(!source.dropped.load(Ordering::SeqCst));
        np.stop();
        assert!(
            source.dropped.load(Ordering::SeqCst),
            "stop drops the watch"
        );
        np.watch_path();
        assert_eq!(
            source.started.load(Ordering::SeqCst),
            1,
            "a stopped probe starts no watch"
        );
    }

    /// A session the probe does not measure reads the path alone: its first read starts the
    /// installed watch (once ever, shared with a later ask), and no read takes a lease, starts
    /// the thread or probes the API host. A loss reads Down at once with a look owed, and the
    /// return (no settle here) Unknown with none.
    #[test]
    fn a_path_only_read_starts_the_watch_once_and_probes_nothing() {
        let (probe, resolved) = Answering::new();
        let np = NetProbe::with_state(probe, slow_but_nudged());
        assert_eq!(np.path_reach(), Reach::Unknown, "no source: no reading");
        let source = Arc::new(Handing::default());
        assert!(np.path_from(Arc::clone(&source) as Arc<dyn PathSource>));
        assert_eq!(np.path_reach(), Reach::Unknown, "no reading yet");
        assert_eq!(source.started.load(Ordering::SeqCst), 1, "the first read");
        assert_eq!(np.path_reach(), Reach::Unknown);
        assert_eq!(source.started.load(Ordering::SeqCst), 1, "only the first");
        assert_eq!(np.path_landing(), None, "nothing lost");
        source.read(true);
        source.read(false);
        let read = Instant::now();
        let Reach::Down { since } = np.path_reach() else {
            panic!("a lost path reads Down at once: {:?}", np.path_reach());
        };
        assert!(since <= read, "since the loss");
        assert!(
            np.path_landing().is_some_and(|at| at > read),
            "a lost path owes a look"
        );
        source.read(true);
        assert_eq!(np.path_reach(), Reach::Unknown, "the return, settled");
        assert_eq!(np.path_landing(), None);
        assert_eq!(np.spawned.load(Ordering::SeqCst), 0, "no thread");
        assert!(!np.started.load(Ordering::SeqCst));
        assert_eq!(np.lock().waiting(Instant::now()), 0, "no lease");
        assert!(resolved.try_recv().is_err(), "nothing probed");
        assert_eq!(
            np.ask("s-1"),
            Reach::Down { since },
            "a measured session's return stands Down until a probe measures it"
        );
        assert_eq!(
            source.started.load(Ordering::SeqCst),
            1,
            "an ask after a path-only read starts no second watch"
        );
        resolved
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the ask's probe");
        np.stop();
        assert!(source.dropped.load(Ordering::SeqCst), "stop drops it");
        assert_eq!(np.path_reach(), Reach::Unknown, "the last reading stands");
        assert_eq!(source.started.load(Ordering::SeqCst), 1, "none after stop");
    }

    /// A peek reads the verdict a measured session's ask would get, unasked: its first read
    /// starts the installed path watch (once ever), and no peek takes a lease, starts the thread
    /// or probes the API host. A path that can carry traffic reads no Up; a lost path reads Down
    /// at once, and so does its return until a probe measures it; a measure reads as measured.
    /// NEGATIVE CONTROL (run): with [`NetProbe::peek`]'s `watch_path` call removed, the first
    /// peek starts no watch and the test fails at "the first peek".
    #[test]
    fn a_peek_reads_the_measured_verdict_with_no_lease_thread_or_probe() {
        let (probe, resolved) = Answering::new();
        let np = NetProbe::with_state(probe, slow_but_nudged());
        assert_eq!(np.peek(), Reach::Unknown, "no source: nothing to read");
        let source = Arc::new(Handing::default());
        assert!(np.path_from(Arc::clone(&source) as Arc<dyn PathSource>));
        assert_eq!(np.peek(), Reach::Unknown, "nothing read or measured yet");
        assert_eq!(source.started.load(Ordering::SeqCst), 1, "the first peek");
        source.read(true);
        assert_eq!(np.peek(), Reach::Unknown, "a working path is no Up");
        source.read(false);
        let read = Instant::now();
        let Reach::Down { since } = np.peek() else {
            panic!("a lost path reads Down at once: {:?}", np.peek());
        };
        assert!(since <= read, "since the loss");
        source.read(true);
        assert_eq!(
            np.peek(),
            Reach::Down { since },
            "the return stands Down until a probe measures it"
        );
        let measured = Instant::now();
        np.settle(Outcome::Up, measured, SystemTime::now());
        assert!(
            matches!(np.peek(), Reach::Up { since } if since == measured),
            "the measure reads as measured: {:?}",
            np.peek()
        );
        assert_eq!(source.started.load(Ordering::SeqCst), 1, "only the first");
        assert_eq!(np.spawned.load(Ordering::SeqCst), 0, "no thread");
        assert!(!np.started.load(Ordering::SeqCst));
        assert_eq!(np.lock().waiting(Instant::now()), 0, "no lease");
        assert!(resolved.try_recv().is_err(), "nothing probed");
        np.stop();
    }

    /// Network.framework's satisfiable path (a connection attempt brings it up, as an on-demand
    /// VPN) reads usable, as a satisfied one does; an unsatisfied one does not, and an invalid
    /// one says nothing. Only the route socket's moves are passed on.
    #[test]
    fn a_satisfiable_path_reads_usable_and_only_route_moves_pass() {
        #[cfg(target_os = "macos")]
        {
            use aterm_objc::path_monitor::PathStatus;
            assert_eq!(usable(PathStatus::Satisfied), Some(true));
            assert_eq!(
                usable(PathStatus::Satisfiable),
                Some(true),
                "a satisfiable path is one a connection brings up"
            );
            assert_eq!(usable(PathStatus::Unsatisfied), Some(false));
            assert_eq!(usable(PathStatus::Invalid), None);
        }
        use crate::network_moved::Woke;
        assert!(route_move(Woke::Route));
        // No Windows build constructs `Woke::Browse` (its source is macOS's), tests included.
        #[cfg(unix)]
        assert!(!route_move(Woke::Browse));
        for other in [Woke::Timer, Woke::Beacon, Woke::LocalNetwork] {
            assert!(!route_move(other), "{other:?}");
        }
    }

    /// [`RouteMoves`] passes each route move its watch hears on as one [`PathEvent::Moved`], and
    /// once dropped its thread ends at the next wake, passing nothing more: its sink, which only
    /// the thread holds, is released.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn route_moves_pass_each_route_move_and_end_once_dropped() {
        use crate::network_moved::{Moved, Woke};
        let moved = Arc::new(Moved::new());
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&events);
        let sink: Arc<dyn Fn(PathEvent) + Send + Sync> =
            Arc::new(move |event| seen.lock().unwrap().push(event));
        let held = Arc::downgrade(&sink);
        let routes = RouteMoves::start(sink, moved.watch()).expect("the thread starts");
        let until = |count: usize, what: &str| {
            let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while events.lock().unwrap().len() < count {
                assert!(Instant::now() < deadline, "{what}");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        moved.tell(Woke::Route);
        until(1, "the route move passed on");
        moved.tell(Woke::Route);
        until(2, "the next one too");
        drop(routes);
        moved.tell(Woke::Route);
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while held.upgrade().is_some() {
            assert!(
                Instant::now() < deadline,
                "the dropped watch's thread never ended"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            *events.lock().unwrap(),
            [PathEvent::Moved, PathEvent::Moved],
            "nothing passed after the drop"
        );
    }

    /// A path source whose watch cannot start is tried once, not at every read.
    #[test]
    fn a_watch_that_cannot_start_is_tried_once() {
        #[derive(Default)]
        struct Refusing(AtomicUsize);
        impl PathSource for Refusing {
            fn watch(&self, _: PathSink) -> Option<PathWatch> {
                self.0.fetch_add(1, Ordering::SeqCst);
                None
            }
        }
        let (probe, _resolved) = Answering::new();
        let np = NetProbe::with_state(probe, slow_but_nudged());
        let source = Arc::new(Refusing::default());
        assert!(np.path_from(Arc::clone(&source) as Arc<dyn PathSource>));
        for _ in 0..3 {
            assert_eq!(np.path_reach(), Reach::Unknown);
        }
        np.watch_path();
        assert_eq!(source.0.load(Ordering::SeqCst), 1);
    }

    fn shutdown_at_park_trace(publisher: ShutdownPublisher) -> Vec<ShutdownObservation> {
        let probe = Arc::new(Seam::new(
            Err(io::Error::other("this fixture must never resolve")),
            Dial::Unreachable,
            Dial::Unreachable,
        ));
        let np = NetProbe::with(probe, BUDGET);
        let released = Arc::downgrade(&np);
        let trace = Arc::new(Mutex::new(Vec::new()));
        let (checked_tx, checked_rx) = mpsc::channel();
        let (park_tx, park_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = {
            let np = Arc::clone(&np);
            let trace = Arc::clone(&trace);
            std::thread::spawn(move || {
                let mut first_park = true;
                np.run_before_park(|| {
                    if first_park {
                        first_park = false;
                        checked_tx.send(()).unwrap();
                        park_rx.recv().unwrap();
                        trace
                            .lock()
                            .unwrap()
                            .push(ShutdownObservation::read("Park", &np));
                    }
                });
                done_tx.send(()).unwrap();
            })
        };
        checked_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("worker reached its checked, mutex-held park boundary");
        let stopper = match publisher {
            ShutdownPublisher::HistoricalUnlocked => {
                np.stop.store(true, Ordering::SeqCst);
                np.wake.notify_all();
                trace
                    .lock()
                    .unwrap()
                    .push(ShutdownObservation::read("Stop", &np));
                None
            }
            ShutdownPublisher::MutexGuarded => {
                let np = Arc::clone(&np);
                let trace = Arc::clone(&trace);
                Some(std::thread::spawn(move || {
                    np.stop();
                    trace
                        .lock()
                        .unwrap()
                        .push(ShutdownObservation::read("Stop", &np));
                }))
            }
        };
        park_tx.send(()).unwrap();
        if matches!(publisher, ShutdownPublisher::HistoricalUnlocked) {
            let _state = np.lock();
            np.wake.notify_all();
        }
        if let Some(stopper) = stopper {
            stopper.join().unwrap();
        }
        done_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("stopped worker exits without another ask or probe");
        worker.join().unwrap();
        drop(np);
        assert!(released.upgrade().is_none(), "no worker retains the probe");
        Arc::try_unwrap(trace).unwrap().into_inner().unwrap()
    }

    #[test]
    fn shutdown_cannot_lose_the_park_wake_or_retain_the_worker() {
        let model = aterm_models_gui::netprobe_shutdown_park_model();
        let observed = shutdown_at_park_trace(ShutdownPublisher::MutexGuarded);
        assert_eq!(
            observed.iter().map(|step| step.action).collect::<Vec<_>>(),
            ["Park", "Stop"]
        );
        let mut state = project_shutdown_park(&[]);
        assert_eq!(state, model.init_state());
        for (index, step) in observed.iter().enumerate() {
            assert!(model.fire(step.action, &mut state), "{step:?}: {state:?}");
            assert_eq!(state, project_shutdown_park(&observed[..=index]));
            assert!(model.check_invariant("StoppedWaiterHasWake", &state));
        }

        let old_order = shutdown_at_park_trace(ShutdownPublisher::HistoricalUnlocked);
        assert_eq!(
            old_order.iter().map(|step| step.action).collect::<Vec<_>>(),
            ["Stop", "Park"]
        );
        assert!(!model.action_enabled(old_order[0].action, &project_shutdown_park(&[])));
        let mutant = aterm_spec::interp::with_buggy(&model, 1);
        let mut state = project_shutdown_park(&[]);
        assert_eq!(state, mutant.init_state());
        for (index, step) in old_order.iter().enumerate() {
            assert!(mutant.fire(step.action, &mut state));
            assert_eq!(state, project_shutdown_park(&old_order[..=index]));
        }
        assert!(
            !mutant.check_invariant("StoppedWaiterHasWake", &state),
            "the old notification before parking must be caught"
        );
    }

    /// The probe thread's settle logs one line per change of the measured verdict, on the
    /// thread that settled, and none for a repeat: Down, Down, Up, Up is two lines, the
    /// second the Down→Up edge with the lease that was waiting.
    #[test]
    fn each_change_of_the_measured_reach_is_logged_once_and_a_repeat_is_not() {
        const S: Duration = Duration::from_secs(1);
        let probe = Arc::new(Seam::new(
            Err(io::Error::other("this fixture must never resolve")),
            Dial::Unreachable,
            Dial::Unreachable,
        ));
        let np = NetProbe::with(probe, BUDGET);
        let (t, w) = (Instant::now(), SystemTime::now());
        assert_eq!(np.lock().ask("s-1", t, w), Reach::Unknown);
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&lines);
        crate::test_log::observe(
            move |record| {
                let line = record.args().to_string();
                if line.starts_with("harness: API reach") {
                    seen.lock().unwrap().push(line);
                }
            },
            || {
                for (outcome, at) in [
                    (Outcome::Down, 0),
                    (Outcome::Down, 3),
                    (Outcome::Up, 18),
                    (Outcome::Up, 50),
                ] {
                    let began = t + at * S;
                    assert!(np.lock().begin(began));
                    np.settle(outcome, began + S / 10, w + at * S);
                }
            },
        );
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].contains("reach down (no fresh measure"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("down→up after 18s") && lines[1].contains("1 waiting"),
            "{lines:?}"
        );
        assert_eq!(
            np.lock().reach(t + 51 * S, w + 51 * S),
            Reach::Up {
                since: t + 18 * S + S / 10
            }
        );
    }

    fn refused_port() -> SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok() {
            assert!(
                Instant::now() < deadline,
                "{addr} still accepts a minute after its listener was dropped"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        addr
    }

    fn v6(n: u16) -> SocketAddr {
        SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, n), 443))
    }

    /// What a fake connect to one address does.
    #[derive(Clone, Copy, Debug)]
    enum Dial {
        /// The kernel's own `EHOSTUNREACH`.
        Unreachable,
        /// A black hole: the whole share spent, then a timeout.
        BlackHole,
        /// The real connector (a loopback address).
        Real,
    }

    struct Seam {
        addrs: io::Result<Vec<SocketAddr>>,
        v6: Dial,
        v4: Dial,
        handshake: Option<bool>,
        dialled: Mutex<Vec<(SocketAddr, Duration)>>,
    }

    impl Seam {
        fn new(addrs: io::Result<Vec<SocketAddr>>, v6: Dial, v4: Dial) -> Self {
            Self {
                addrs,
                v6,
                v4,
                handshake: None,
                dialled: Mutex::default(),
            }
        }
    }

    impl Probe for Seam {
        fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
            assert_eq!((host, port), (API_HOST, API_PORT));
            match &self.addrs {
                Ok(a) => Ok(a.clone()),
                Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
            }
        }
        fn connect(&self, addr: SocketAddr, within: Duration) -> io::Result<TcpStream> {
            self.dialled.lock().unwrap().push((addr, within));
            match if addr.is_ipv4() { self.v4 } else { self.v6 } {
                #[cfg(unix)]
                Dial::Unreachable => Err(io::Error::from_raw_os_error(libc::EHOSTUNREACH)),
                #[cfg(not(unix))]
                Dial::Unreachable => Err(io::Error::from(io::ErrorKind::HostUnreachable)),
                Dial::BlackHole => {
                    std::thread::sleep(within);
                    Err(io::Error::new(io::ErrorKind::TimedOut, "black hole"))
                }
                Dial::Real => SystemProbe.connect(addr, within),
            }
        }
        fn handshake(
            &self,
            tcp: TcpStream,
            host: &str,
            deadline: aterm_http::Deadline,
        ) -> io::Result<()> {
            match self.handshake {
                Some(true) => Ok(()),
                Some(false) => Err(io::Error::other("TLS handshake failed: bad certificate")),
                None => verified_handshake(tcp, host, deadline),
            }
        }
    }

    fn answering(reply: &'static [u8]) -> SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                use std::io::{Read as _, Write as _};
                let mut hello = [0u8; 512];
                let _ = s.read(&mut hello);
                let _ = s.write_all(reply);
            }
        });
        addr
    }

    #[test]
    fn the_probe_reads_down_only_on_definite_failures() {
        let nx = Seam::new(
            Err(io::Error::from(io::ErrorKind::NotFound)),
            Dial::Real,
            Dial::Real,
        );
        assert_eq!(probe_once(&nx, BUDGET), Outcome::Down, "NXDOMAIN");
        #[cfg(target_os = "macos")]
        {
            let macos_no_name = Seam::new(
                Err(io::Error::other(
                    "failed to lookup address information: nodename nor servname provided, or not known",
                )),
                Dial::Real,
                Dial::Real,
            );
            assert_eq!(probe_once(&macos_no_name, BUDGET), Outcome::Down);
        }
        let temporary = Seam::new(
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "temporary DNS failure",
            )),
            Dial::Real,
            Dial::Real,
        );
        assert_eq!(
            probe_once(&temporary, BUDGET),
            Outcome::Unknown,
            "a temporary resolver error keeps the normal retry ladder"
        );
        let unrecognized = Seam::new(
            Err(io::Error::other(
                "failed to lookup address information: unexpected error",
            )),
            Dial::Real,
            Dial::Real,
        );
        assert_eq!(probe_once(&unrecognized, BUDGET), Outcome::Unknown);
        let refused = Seam::new(Ok(vec![refused_port()]), Dial::Real, Dial::Real);
        assert_eq!(probe_once(&refused, BUDGET), Outcome::Down, "refused");
        let both = Seam::new(
            Ok(vec![v6(1), refused_port()]),
            Dial::Unreachable,
            Dial::Real,
        );
        assert_eq!(
            probe_once(&both, BUDGET),
            Outcome::Down,
            "unreachable v6, refused v4"
        );
        let hole = Seam::new(Ok(vec![v6(1), refused_port()]), Dial::BlackHole, Dial::Real);
        assert_eq!(
            probe_once(&hole, Duration::from_millis(200)),
            Outcome::Unknown
        );
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
        let open = listener.local_addr().expect("addr");
        let mut portal = Seam::new(Ok(vec![open]), Dial::Real, Dial::Real);
        portal.handshake = Some(false);
        assert_eq!(probe_once(&portal, BUDGET), Outcome::Unknown);
        let mut good = Seam::new(Ok(vec![open]), Dial::Real, Dial::Real);
        good.handshake = Some(true);
        assert_eq!(probe_once(&good, BUDGET), Outcome::Up);
    }

    #[test]
    fn a_black_holed_ipv6_cannot_eat_the_budget() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
        let v4 = listener.local_addr().expect("addr");
        let mut seam = Seam::new(Ok(vec![v6(1), v6(2), v4]), Dial::BlackHole, Dial::Real);
        seam.handshake = Some(true);
        assert_eq!(probe_once(&seam, BUDGET), Outcome::Up);
        let dialled = seam.dialled.lock().unwrap();
        assert_eq!(dialled[0].0, v4, "IPv4 first: {dialled:?}");
        assert!(dialled[0].1 <= BUDGET / 3 + Duration::from_millis(5));
        assert_eq!(dialled.len(), 1, "{dialled:?}");
        drop(listener);
    }

    #[test]
    fn a_garbage_answer_to_the_client_hello_reads_unknown() {
        let addr = answering(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        let seam = Seam::new(Ok(vec![addr]), Dial::Real, Dial::Real);
        assert_eq!(probe_once(&seam, BUDGET), Outcome::Unknown);
    }

    #[test]
    fn a_resolver_that_blocks_past_the_budget_reads_unknown_and_starts_no_second_probe() {
        struct Stuck {
            gate: Mutex<mpsc::Receiver<()>>,
            calls: AtomicUsize,
        }
        impl Probe for Stuck {
            fn resolve(&self, _: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let _ = self.gate.lock().unwrap().recv();
                Err(io::Error::from(io::ErrorKind::NotFound))
            }
            fn connect(&self, _: SocketAddr, _: Duration) -> io::Result<TcpStream> {
                unreachable!("never resolved")
            }
            fn handshake(&self, _: TcpStream, _: &str, _: aterm_http::Deadline) -> io::Result<()> {
                unreachable!("never connected")
            }
        }
        let (release, gate) = mpsc::channel();
        let stuck = Arc::new(Stuck {
            gate: Mutex::new(gate),
            calls: AtomicUsize::new(0),
        });
        let np = NetProbe::with(
            Arc::clone(&stuck) as Arc<dyn Probe>,
            Duration::from_millis(40),
        );
        assert_eq!(np.ask("s-1"), Reach::Unknown, "no measure yet, no wait");
        let until = Instant::now() + Duration::from_secs(60);
        while stuck.calls.load(Ordering::SeqCst) == 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        for _ in 0..5 {
            assert_eq!(np.ask("s-1"), Reach::Unknown);
            assert_eq!(np.ask("s-2"), Reach::Unknown);
        }
        assert_eq!(stuck.calls.load(Ordering::SeqCst), 1, "one probe in flight");
        assert_eq!(np.spawned.load(Ordering::SeqCst), 1, "one thread");
        np.stop();
        drop(release);

        struct Nx;
        impl Probe for Nx {
            fn resolve(&self, _: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
                Err(io::Error::from(io::ErrorKind::NotFound))
            }
            fn connect(&self, _: SocketAddr, _: Duration) -> io::Result<TcpStream> {
                unreachable!()
            }
            fn handshake(&self, _: TcpStream, _: &str, _: aterm_http::Deadline) -> io::Result<()> {
                unreachable!()
            }
        }
        assert_eq!(probe_once(&Nx, Duration::ZERO), Outcome::Unknown);
        let np = NetProbe::with(Arc::new(Nx), BUDGET);
        let asked = Instant::now();
        let mut seen = np.ask("s-1");
        while !matches!(seen, Reach::Down { .. }) && asked.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(5));
            seen = np.ask("s-1");
        }
        assert!(matches!(seen, Reach::Down { .. }), "{seen:?}");
        assert_eq!(np.ask("s-1"), seen);
        np.stop();
    }

    const MDM_DOMAIN: &str = "com.anthropic.claudecode.plist";

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aterm-netprobe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["home/.claude", "proj/.claude", "managed", "prefs"] {
            std::fs::create_dir_all(dir.join(sub)).expect("scratch");
        }
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::write(path, text).expect("write");
    }

    fn homed(home: &Path) -> Vec<String> {
        let mut vars = vec![
            format!("HOME={}", home.display()),
            format!(
                "{}={}",
                aterm_agent::harness::accounts::HOMEDIR_ENV,
                home.display()
            ),
        ];
        vars.dedup();
        vars
    }

    #[test]
    fn an_agents_settings_and_profile_store_can_take_its_stored_login_away() {
        let dir = scratch("stored");
        let home = dir.join("home");
        let env = |more: &[String]| {
            let mut env = homed(&home);
            env.push("USER=owner".to_string());
            env.extend(more.iter().cloned());
            Some(env)
        };
        let stored = |env: Option<Vec<String>>, argv: &[&str]| {
            let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
            stored_login_of(
                &gather(
                    env,
                    &argv,
                    &dir.join("proj"),
                    Some(&dir.join("managed")),
                    &dir.join("prefs"),
                    Some(&dir.join("home")),
                ),
                Some(&home),
            )
        };
        let user = home.join(".claude/settings.json");
        assert!(stored(env(&[]), &["claude"]), "a quiet tree");
        write(&user, "{\"model\":\"opus\",\"env\":{\"EDITOR\":\"vi\"}}");
        assert!(stored(env(&[]), &["claude"]), "a model and an editor");

        for (why, text) in [
            (
                "an API key in an env block",
                "{\"env\":{\"ANTHROPIC_API_KEY\":\"k\"}}",
            ),
            ("an apiKeyHelper", "{\"apiKeyHelper\":\"/opt/key.sh\"}"),
            ("a profile", "{\"env\":{\"ANTHROPIC_PROFILE\":\"p\"}}"),
            ("a file that does not parse", "{"),
        ] {
            write(&user, text);
            assert!(!stored(env(&[]), &["claude"]), "{why}: the user's");
            write(&user, "{}");
            write(&dir.join("proj/.claude/settings.local.json"), text);
            assert!(!stored(env(&[]), &["claude"]), "{why}: the project's");
            std::fs::remove_file(dir.join("proj/.claude/settings.local.json")).expect("rm");
            assert!(
                !stored(env(&[]), &["claude", "--settings", text]),
                "{why}: --settings"
            );
            assert!(stored(env(&[]), &["claude"]), "{why}: gone again");
        }

        let configs = home.join(".config/anthropic/configs");
        std::fs::create_dir_all(&configs).expect("configs");
        write(&configs.join("notes.txt"), "x");
        assert!(stored(env(&[]), &["claude"]), "no config in the store");
        write(&configs.join("default.json"), "{}");
        assert!(!stored(env(&[]), &["claude"]), "a config in the store");
        std::fs::remove_file(configs.join("default.json")).expect("rm");
        let elsewhere = dir.join("store/configs");
        std::fs::create_dir_all(&elsewhere).expect("configs");
        write(&elsewhere.join("work.json"), "{}");
        let named = [format!(
            "ANTHROPIC_CONFIG_DIR={}",
            dir.join("store").display()
        )];
        assert!(
            !stored(env(&named), &["claude"]),
            "where the variable names it"
        );
        let cloud = [format!(
            "ANTHROPIC_CONFIG_DIR={}",
            home.join("Library/CloudStorage/x").display()
        )];
        assert!(!stored(env(&cloud), &["claude"]), "a store nobody lists");
        assert!(!home.join("Library/CloudStorage").exists(), "nor made");

        assert!(!stored(None, &["claude"]));
        let empty = RouteFacts {
            env: Some(Vec::new()),
            settings: Vec::new(),
        };
        assert!(!stored_login_of(&empty, Some(&home)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_route_reads_every_settings_source_claude_code_reads() {
        let proxy = "{\"env\":{\"HTTPS_PROXY\":\"http://proxy.corp:3128\"}}";
        let dir = scratch("route");
        let env = || {
            let mut env = homed(&dir.join("home"));
            env.push("USER=owner".to_string());
            Some(env)
        };
        let facts = |argv: &[&str]| {
            let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
            netwatch::route_of(&gather(
                env(),
                &argv,
                &dir.join("proj"),
                Some(&dir.join("managed")),
                &dir.join("prefs"),
                Some(&dir.join("home")),
            ))
        };
        write(
            &dir.join("home/.claude/settings.json"),
            "{\"model\":\"opus\"}",
        );
        assert_eq!(facts(&["claude"]), Route::Default);
        for file in [
            "home/.claude/settings.json",
            "proj/.claude/settings.json",
            "proj/.claude/settings.local.json",
            "managed/managed-settings.json",
        ] {
            let path = dir.join(file);
            let before = std::fs::read_to_string(&path).ok();
            write(&path, proxy);
            assert!(matches!(facts(&["claude"]), Route::Custom(_)), "{file}");
            write(&path, "{\"env\":");
            assert!(
                matches!(facts(&["claude"]), Route::Custom(_)),
                "{file} broken"
            );
            match before {
                Some(text) => write(&path, &text),
                None => std::fs::remove_file(&path).expect("rm"),
            }
            assert_eq!(facts(&["claude"]), Route::Default, "{file} restored");
        }
        std::fs::create_dir_all(dir.join("managed/managed-settings.d")).expect("drop-ins");
        write(&dir.join("managed/managed-settings.d/10-ok.json"), "{}");
        assert_eq!(facts(&["claude"]), Route::Default);
        write(&dir.join("managed/managed-settings.d/20-gw.json"), proxy);
        assert!(matches!(facts(&["claude"]), Route::Custom(_)));
        std::fs::remove_file(dir.join("managed/managed-settings.d/20-gw.json")).expect("rm");
        std::fs::create_dir_all(dir.join("prefs/owner")).expect("prefs");
        write(&dir.join("prefs/owner").join(MDM_DOMAIN), "plist");
        assert!(
            matches!(facts(&["claude"]), Route::Custom(_)),
            "an MDM domain"
        );
        std::fs::remove_file(dir.join("prefs/owner").join(MDM_DOMAIN)).expect("rm");
        write(&dir.join("proj/team.json"), proxy);
        assert!(matches!(
            facts(&["claude", "--settings", "team.json"]),
            Route::Custom(_)
        ));
        assert!(matches!(
            facts(&["claude", &format!("--settings={proxy}")]),
            Route::Custom(_)
        ));
        assert_eq!(facts(&["claude", "--settings", "{}"]), Route::Default);
        for cwd in ["", "proj"] {
            let blind = netwatch::route_of(&gather(
                env(),
                &[],
                Path::new(cwd),
                Some(&dir.join("managed")),
                &dir.join("prefs"),
                Some(&dir.join("home")),
            ));
            assert!(matches!(blind, Route::Custom(_)), "{cwd:?}: {blind:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_project_in_a_file_provider_domain_is_measured_unless_a_document_is_there() {
        let dir = scratch("provider");
        let home = dir.join("home");
        let cloud = home.join("Library/CloudStorage/SyncProvider-acct/proj");
        std::fs::create_dir_all(&cloud).expect("the project");
        let facts = |cwd: &Path| {
            gather(
                Some(
                    homed(&home)
                        .into_iter()
                        .chain(["USER=owner".to_string()])
                        .collect(),
                ),
                &["claude".to_string()],
                cwd,
                Some(&dir.join("managed")),
                &dir.join("prefs"),
                Some(&home),
            )
        };
        assert_eq!(netwatch::route_of(&facts(&cloud)), Route::Default);
        assert!(stored_login_of(&facts(&cloud), Some(&home)));
        let quiet = "{\"permissions\":{\"allow\":[\"Bash(ls:*)\"]}}";
        std::fs::create_dir_all(cloud.join(".claude")).expect(".claude");
        write(&cloud.join(".claude/settings.local.json"), quiet);
        let r = netwatch::route_of(&facts(&cloud));
        assert!(
            matches!(&r, Route::Custom(why) if why.contains("settings.local.json could not be read")),
            "{r:?}"
        );
        assert!(!stored_login_of(&facts(&cloud), Some(&home)));
        write(&dir.join("proj/.claude/settings.local.json"), quiet);
        assert_eq!(
            netwatch::route_of(&facts(&dir.join("proj"))),
            Route::Default
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
