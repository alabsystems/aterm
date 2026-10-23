// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Background network reachability probe (`aterm-harness-netprobe`) for `harness::netwatch`.

use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs as _};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use aterm_agent::harness::netwatch::{
    self, API_HOST, API_PORT, NetState, Outcome, Route, RouteFacts, Source,
};
use aterm_agent::supervise::policy::WorkerEnv;
use aterm_agent::supervise::policy::claude_settings;
use aterm_agent::supervise::policy::turn_end::Reach;

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
        // a share of the budget, and would read as a timeout (aterm_http::connect).
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

/// A failure that says the address cannot be reached from here: the kernel's own word, never a
/// timeout.
fn definite_failure(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::HostUnreachable
            | io::ErrorKind::NetworkDown
    ) || raw_unreachable(e)
}

/// A resolver answer that specifically says the name has no address. On macOS std reports
/// `getaddrinfo` errors as `Uncategorized` with no raw OS code, so only the known no-name
/// wording tells that answer from a temporary DNS failure.
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

/// The same four answers by raw errno, for a Unix error std has not mapped to its `ErrorKind`.
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
        Arc::new(Self {
            state: Mutex::new(NetState::with_budget(budget)),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            started: AtomicBool::new(false),
            probe,
            spawned: AtomicUsize::new(0),
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

    /// Signals the probe thread to stop at its next wake.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "netprobe_shutdown_park",
            action = "Stop",
            project = "harness_netprobe::tests::project_shutdown_park"
        )
    )]
    pub(crate) fn stop(&self) {
        let _state = self.lock();
        self.stop.store(true, Ordering::SeqCst);
        self.wake.notify_all();
    }

    fn ensure_thread(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
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

    /// The thread: parked while no lease lives, a probe when one is due, one
    /// at a time.
    fn run(&self) {
        self.run_before_park(|| {});
    }

    /// The loop, with a seam just before parking under the state mutex where tests replay a lost
    /// wake; production's callback is empty.
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
            self.lock()
                .finish(outcome, Instant::now(), SystemTime::now());
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

/// What decides an agent's route, read now. `env` is its exec environment (`None`: unreadable,
/// and nothing else is read), `argv` its command line, `cwd` its directory (not absolute:
/// unknown, and its project settings are reported unread), `managed` and `prefs` Claude Code's
/// managed-settings and managed-preferences directories, `home` this process's home (all
/// parameters so tests read a scratch tree).
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

/// [`stored_login_of_agent`] over what was gathered: the environment
/// (`accounts::ProcessLogin::of_env`), the settings sources (`with_settings`) and the vendor's
/// profile store (`with_profile_store`: one names-only listing of the directory
/// `accounts::profile_configs_dir` names).
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

    /// Which stop publisher the fixture runs: the shipping mutex-guarded one, or the unlocked
    /// negative control.
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

    /// Project the observed park/stop order, independently of the model's transition function.
    fn project_shutdown_park(observed: &[ShutdownObservation]) -> aterm_spec::interp::State {
        let park = observed.iter().position(|step| step.action == "Park");
        let stop = observed.iter().position(|step| step.action == "Stop");
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
        ])
    }

    /// Replay a stop after the real worker checked its predicate but before it parks. The
    /// unlocked publisher skips the mutex; a second wake then releases its worker, so neither
    /// arm leaks.
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

    /// At the real park boundary a stop cannot lose the wake of a worker that checked its
    /// predicate.
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

    /// A loopback port nothing listens on: a real connect is refused.
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

    /// A v6 address the fake connector owns: never dialled.
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

    /// A probe whose resolver answers `addrs` (or fails), whose connects do
    /// what `dial` says per address family, and whose handshake is the real
    /// verified one or a fake answer; every dial is recorded, in order.
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

    /// A loopback listener that accepts one connection and answers `reply`
    /// to whatever it is sent, then closes.
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

    /// Down only on a definite failure: the name not resolving, or every address refused (a
    /// real refused loopback connect) or unreachable by the kernel's word.
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

    /// Two black-holed v6 addresses ahead of a working v4 one: IPv4 is dialled first on its share
    /// of the budget and the probe is Up without dialling a black hole (negative control: not the
    /// resolver's v6-first order).
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

    /// The real verified handshake, offline: a loopback listener that answers the ClientHello
    /// with an HTTP error (a captive portal's shape) reads Unknown, never Down or Up.
    #[test]
    fn a_garbage_answer_to_the_client_hello_reads_unknown() {
        let addr = answering(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        let seam = Seam::new(Ok(vec![addr]), Dial::Real, Dial::Real);
        assert_eq!(probe_once(&seam, BUDGET), Outcome::Unknown);
    }

    /// A resolver that blocks: the ask never waits on it and reads Unknown, with one thread and
    /// one probe in flight however often it is asked.
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

    /// Claude Code's managed preference domain's file name (2.1.300).
    const MDM_DOMAIN: &str = "com.anthropic.claudecode.plist";

    /// A scratch tree for the route's sources.
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

    /// `HOME` (where the vendor's profile store is looked for) and the
    /// variable Claude Code's `os.homedir()` reads
    /// (`aterm_agent::harness::accounts::HOMEDIR_ENV`, `USERPROFILE` on
    /// Windows), both naming `home`: a test's agent resolves its scratch
    /// home alone, never the account's.
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

    /// A quiet tree leaves the stored login. It is gone where a settings source (user, project,
    /// `--settings`) gives an API key, a profile or an `apiKeyHelper` or does not parse, where the
    /// vendor's profile store holds a config (`$HOME/.config/anthropic`, or `ANTHROPIC_CONFIG_DIR`)
    /// or sits in a file-provider domain, and where the environment is unread or empty.
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

    /// A quiet tree is the default route; each source that names a route makes it custom (user,
    /// project, local, managed file, managed drop-in, a managed preference domain's presence,
    /// `--settings` file or inline JSON), as does an unparseable file or an unknown directory.
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

    /// An agent in a file-provider domain of the home is on the default route, its stored login
    /// kept, while no settings document is there (a `stat`, nothing opened); one that is there,
    /// however quiet, is never opened and makes the route custom.
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
