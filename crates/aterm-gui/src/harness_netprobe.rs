// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE API'S REACH, MEASURED: the one thread that carries out aterm-agent's
//! `harness::netwatch` for this instance (the outage of 2026-09-27, when
//! every hosted Claude Code turn ended on `API Error: Can't reach the API
//! server … (ENOTFOUND)`). The window's supervisor host asks it, per session,
//! while that session's loop waits at a wall the network answers
//! ([`crate::harness_host`]'s `WorkerIdle::reach`), and the turn-end policy
//! holds the wall while the API is measured DOWN and continues an unreachable
//! one as soon as it is measured UP — never a certificate or proxy refusal:
//! this handshake trusts the platform's store, the agent its own.
//!
//! **One thread, `aterm-harness-netprobe`, at [`crate::qos::Role::Background`]**
//! (useful work nobody is blocked on, never `Housekeeping`, which can starve
//! it for as long as it likes — and a starved probe reads nothing worse than
//! Unknown, since only a DEFINITE failure is Down). Started on the first ask,
//! parked on a condition variable while no lease lives — it costs nothing
//! while nothing waits — and it probes one at a time: a probe still in flight
//! past its budget reads Unknown (`NetState::reach`), and no second one
//! starts beside it. Nothing joins it: a resolve the system resolver holds
//! for minutes holds this thread alone, and a stop wakes it at its next look.
//! The state's lock is taken only to read or write the state — never across
//! I/O, never with another lock held.
//!
//! **One probe** ([`probe_once`]): the system resolver for
//! `api.anthropic.com`, a TCP connect to its addresses — IPv4 and IPv6
//! interleaved, IPv4 first, each attempt given an equal share of what is left
//! of the budget, so a black-holed IPv6 address cannot spend it all while the
//! agent's own Happy-Eyeballs connect goes through on IPv4 — and a TLS
//! handshake the PLATFORM verifies (aterm-http's `Stream::start_tls` over
//! `Trust::PlatformVerifier`, the OS trust store), no request byte sent, all
//! under [`aterm_agent::harness::netwatch::PROBE_BUDGET`]. Down only on a
//! definite failure: a known no-name answer, or every address was refused
//! or unreachable by the kernel's own word (`ECONNREFUSED`, `ENETUNREACH`,
//! `EHOSTUNREACH`, `ENETDOWN`). Up only on a completed verified handshake.
//! Everything else — a temporary/unknown resolver failure, a timeout, a TLS
//! or certificate failure, any other error — is Unknown.
//!
//! **Only the default route is measured** ([`route_of_agent`]): what decides
//! it is read, read-only, from the agent's exec environment (atpkg's
//! `process_args`, the one KERN_PROCARGS2 reader) and every settings source
//! Claude Code reads for it — the user's, the project's, its local one, the
//! managed file and its `managed-settings.d` drop-ins, a managed preference
//! domain, and a `--settings` on its command line. A route the host cannot
//! reproduce, or cannot see, is never measured: its wall is tried on the time
//! ladder. `[harness] probe_api = false` measures nothing at all.
//!
//! Tests inject the resolver, the connector and the handshake ([`Probe`]):
//! no test touches the network — the real connect and the real verified
//! handshake are driven against loopback listeners.

use std::io::{self, Read as _};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use aterm_agent::harness::footer::claude_dir_of;
use aterm_agent::harness::netwatch::{
    self, API_HOST, API_PORT, NetState, Outcome, Route, RouteFacts, SettingsArg, Source,
};
use aterm_agent::supervise::policy::turn_end::Reach;

/// The most of a settings file this reader takes: larger is no file it
/// reads (the route is then custom).
const SETTINGS_CAP: u64 = 256 * 1024;

/// A probe's three steps — the seam the tests replace.
pub(crate) trait Probe: Send + Sync {
    /// The host's addresses.
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>;
    /// A TCP connection to `addr`, within `within`.
    fn connect(&self, addr: SocketAddr, within: Duration) -> io::Result<TcpStream>;
    /// A TLS handshake over `tcp` for `host`, completed and verified by the
    /// platform before `deadline`.
    fn handshake(
        &self,
        tcp: TcpStream,
        host: &str,
        deadline: aterm_http::Deadline,
    ) -> io::Result<()>;
}

/// The real probe: the system resolver, a timed TCP connect, and the
/// platform-verified handshake.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SystemProbe;

impl Probe for SystemProbe {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok((host, port).to_socket_addrs()?.collect())
    }

    fn connect(&self, addr: SocketAddr, within: Duration) -> io::Result<TcpStream> {
        TcpStream::connect_timeout(&addr, within)
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

/// A TLS handshake to `host` over `tcp`, verified against the operating
/// system's trust store, completed before `deadline` — no request sent. The
/// session is dropped at once.
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

/// A failure that says the address cannot be reached from here — the
/// kernel's own word, never a timeout.
fn definite_failure(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::HostUnreachable
            | io::ErrorKind::NetworkDown
    ) || raw_unreachable(e)
}

/// A resolver answer that specifically says the name has no address. The
/// standard library reports `getaddrinfo` errors as `Uncategorized` with no
/// raw OS code on macOS (measured on this host), so only its known no-name
/// wording can distinguish that answer from a temporary DNS failure. An
/// unmatched platform message (or a changed macOS message) stays Unknown:
/// it must never suppress the harness's retry ladder.
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

/// The same four answers by raw errno, for a Unix error std has not mapped to its
/// `ErrorKind`. Unix-only: these are POSIX errnos (the `libc` crate defines none of
/// them for Windows, whose WSA codes std already maps to the kinds above), so a
/// non-Unix build answers from the kinds alone.
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

/// ONE PROBE of the default route under `budget` (module header): Down only
/// on a definite failure, Up only on a verified handshake, else Unknown.
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
        // No address, or a resolver answer that specifically says no name:
        // the agent's own request fails the same way — its `ENOTFOUND`. A
        // temporary or unrecognized resolver error is no definite outage;
        // a resolver past the budget is no definite word either.
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
            // The first connection decides: a verified handshake, or nothing
            // this probe can say (a captive portal, an intercepting proxy, a
            // clock off after sleep, a handshake that timed out).
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

/// THE INSTANCE'S PROBE: the pure state (`NetState`), the thread that
/// carries it out, and the seam it probes through.
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
    /// The production probe: the system's steps under the real budget.
    pub(crate) fn new() -> Arc<Self> {
        Self::with(Arc::new(SystemProbe), netwatch::PROBE_BUDGET)
    }

    /// A probe through `probe`, each under `budget`.
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

    /// A loop waiting at a network wall asks for session `who`: the verdict
    /// now, its lease renewed, and the thread woken (started on the first
    /// ask) — never blocked on a probe.
    pub(crate) fn ask(self: &Arc<Self>, who: &str) -> Reach {
        let reach = self.lock().ask(who, Instant::now(), SystemTime::now());
        self.ensure_thread();
        self.wake.notify_all();
        reach
    }

    /// Stop the thread at its next look (the host's shutdown). Nothing joins
    /// it.
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.wake.notify_all();
    }

    fn ensure_thread(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("aterm-harness-netprobe".into())
            .spawn(move || {
                crate::qos::set_self(crate::qos::Role::Background);
                me.spawned.fetch_add(1, Ordering::SeqCst);
                me.run();
            });
        if let Err(e) = spawned {
            self.started.store(false, Ordering::SeqCst);
            aterm_log::warn!("harness: the API reach probe could not start: {e}");
        }
    }

    /// The thread: parked while no lease lives, a probe when one is due, one
    /// at a time.
    fn run(&self) {
        loop {
            {
                let mut st = self.lock();
                loop {
                    if self.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let now = Instant::now();
                    match st.next_probe(now, SystemTime::now()) {
                        None => {
                            st = self.wake.wait(st).unwrap_or_else(PoisonError::into_inner);
                        }
                        Some(at) if at > now => {
                            st = self
                                .wake
                                .wait_timeout(st, at - now)
                                .unwrap_or_else(PoisonError::into_inner)
                                .0;
                        }
                        Some(_) => {
                            if st.begin(now) {
                                break;
                            }
                        }
                    }
                }
            }
            let budget = self.lock().budget();
            let outcome = probe_once(&*self.probe, budget);
            self.lock()
                .finish(outcome, Instant::now(), SystemTime::now());
        }
    }
}

/// One settings source, read only if it is a regular file of at most
/// [`SETTINGS_CAP`] bytes of text — opened non-blocking, so a FIFO planted
/// there cannot park the reader.
fn read_source(path: &Path) -> Source {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Source::Missing,
        Err(_) => return Source::Unreadable,
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Source::Unreadable;
    }
    let mut bytes = Vec::new();
    if file.take(SETTINGS_CAP + 1).read_to_end(&mut bytes).is_err()
        || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > SETTINGS_CAP
    {
        return Source::Unreadable;
    }
    String::from_utf8(bytes).map_or(Source::Unreadable, Source::Text)
}

/// What decides an agent's route, read now (module header): `env` its exec
/// environment (`None`: unreadable), `argv` its command line, `cwd` its
/// directory, `managed` Claude Code's managed-settings directory and `prefs`
/// the managed-preferences one (parameters, so the tests read a scratch
/// tree).
fn gather(
    env: Option<Vec<String>>,
    argv: &[String],
    cwd: &Path,
    managed: &Path,
    prefs: &Path,
) -> RouteFacts {
    let var = |key: &str| {
        env.as_ref()?
            .iter()
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
    };
    let cwd = cwd.is_absolute().then_some(cwd);
    let claude_dir = claude_dir_of(var("CLAUDE_CONFIG_DIR"), var("HOME"));
    let mut settings: Vec<(String, Source)> = Vec::new();
    if claude_dir.is_none() {
        settings.push(("the user's settings".to_string(), Source::Unreadable));
    }
    if cwd.is_none() {
        // The agent's directory is not known (a snapshot taken before its
        // cwd was read): its project settings are a source this host cannot
        // see, so the route is not the default (review, 2026-09-27: they were
        // skipped, and a proxy in `.claude/settings.local.json` read Default
        // and armed the Down hold).
        settings.push(("the project's settings".to_string(), Source::Unreadable));
    }
    for (at, path) in netwatch::settings_files(claude_dir.as_deref(), cwd, managed) {
        settings.push((at, read_source(&path)));
    }
    match std::fs::read_dir(managed.join("managed-settings.d")) {
        Ok(entries) => {
            let mut drop_ins: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect();
            drop_ins.sort();
            for path in drop_ins {
                settings.push((format!("{}", path.display()), read_source(&path)));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => settings.push((
            "the managed settings' drop-ins".to_string(),
            Source::Unreadable,
        )),
    }
    // A managed preference domain is a plist this reader does not parse: its
    // presence alone is a route it cannot see.
    let mut domains = vec![prefs.join(netwatch::MANAGED_PREFS_FILE)];
    if let Some(user) = var("USER")
        .or_else(|| var("LOGNAME"))
        .filter(|u| !u.is_empty())
    {
        domains.push(prefs.join(user).join(netwatch::MANAGED_PREFS_FILE));
    }
    for domain in domains {
        if std::fs::symlink_metadata(&domain).is_ok() {
            settings.push((
                "a managed preference domain".to_string(),
                Source::Unreadable,
            ));
        }
    }
    for arg in netwatch::settings_args(argv) {
        let source = match arg {
            SettingsArg::Inline(json) => Source::Text(json),
            SettingsArg::File(path) if path.is_absolute() => read_source(&path),
            SettingsArg::File(path) => match cwd {
                Some(cwd) => read_source(&cwd.join(path)),
                None => Source::Unreadable,
            },
        };
        settings.push(("--settings".to_string(), source));
    }
    RouteFacts { env, settings }
}

/// AGENT `pid`'S ROUTE, read now: its exec environment and every settings
/// source Claude Code reads for it ([`netwatch::route_of`]).
pub(crate) fn route_of_agent(pid: u32, argv: &[String], cwd: &str) -> Route {
    let env = atpkg::caller_shell::process_args(pid).map(|a| a.env);
    netwatch::route_of(&gather(
        env,
        argv,
        Path::new(cwd),
        Path::new(netwatch::MANAGED_DIR),
        Path::new(netwatch::MANAGED_PREFS_DIR),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};
    use std::sync::mpsc;

    const BUDGET: Duration = Duration::from_secs(2);

    /// A loopback port nothing listens on: a real connect is refused.
    ///
    /// Dropping the listener does not close it while a sibling test's
    /// `fork`/`posix_spawn` holds a copy of its descriptor (until that child
    /// `exec`s — ~0.5 s under load), and in that window a connect is ACCEPTED
    /// into the child's backlog. So the port is handed out only once a real
    /// connect is refused, against a bounded deadline that fails loudly.
    fn refused_port() -> SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok() {
            assert!(
                Instant::now() < deadline,
                "{addr} still accepts 10 s after its listener was dropped"
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
                // A raw errno on Unix keeps `raw_unreachable` exercised; elsewhere
                // the kind is the whole answer.
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

    /// DOWN ONLY ON A DEFINITE FAILURE: the name not resolving; every address
    /// refused (a real refused loopback connect) or unreachable by the
    /// kernel's own word. UNKNOWN for a timeout anywhere among them, and for
    /// a handshake that failed. UP only on a completed handshake.
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
        // A timeout among them is no definite word.
        let hole = Seam::new(Ok(vec![v6(1), refused_port()]), Dial::BlackHole, Dial::Real);
        assert_eq!(
            probe_once(&hole, Duration::from_millis(200)),
            Outcome::Unknown
        );
        // A connection whose handshake fails: a captive portal, a proxy.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
        let open = listener.local_addr().expect("addr");
        let mut portal = Seam::new(Ok(vec![open]), Dial::Real, Dial::Real);
        portal.handshake = Some(false);
        assert_eq!(probe_once(&portal, BUDGET), Outcome::Unknown);
        let mut good = Seam::new(Ok(vec![open]), Dial::Real, Dial::Real);
        good.handshake = Some(true);
        assert_eq!(probe_once(&good, BUDGET), Outcome::Up);
    }

    /// A BLACK-HOLED IPv6 CANNOT EAT THE BUDGET: with two v6 addresses that
    /// swallow every SYN ahead of a working v4 one, IPv4 is dialled FIRST, on
    /// its share of the budget, and the probe is Up well inside it.
    /// NEGATIVE CONTROL: the order the resolver gave (v6 first) is not the
    /// order dialled.
    #[test]
    fn a_black_holed_ipv6_cannot_eat_the_budget() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback");
        let v4 = listener.local_addr().expect("addr");
        let mut seam = Seam::new(Ok(vec![v6(1), v6(2), v4]), Dial::BlackHole, Dial::Real);
        seam.handshake = Some(true);
        let began = Instant::now();
        assert_eq!(probe_once(&seam, BUDGET), Outcome::Up);
        assert!(began.elapsed() < BUDGET / 2, "{:?}", began.elapsed());
        let dialled = seam.dialled.lock().unwrap();
        assert_eq!(dialled[0].0, v4, "IPv4 first: {dialled:?}");
        assert!(dialled[0].1 <= BUDGET / 3 + Duration::from_millis(5));
        assert_eq!(dialled.len(), 1, "{dialled:?}");
        drop(listener);
    }

    /// THE REAL VERIFIED HANDSHAKE, OFFLINE: a loopback listener that answers
    /// the ClientHello with an HTTP error (a captive portal's shape) is no
    /// verified handshake — Unknown, never Down, never Up.
    #[test]
    fn a_garbage_answer_to_the_client_hello_reads_unknown() {
        let addr = answering(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        let seam = Seam::new(Ok(vec![addr]), Dial::Real, Dial::Real);
        assert_eq!(probe_once(&seam, BUDGET), Outcome::Unknown);
    }

    /// A resolver that blocks: the ask never waits on it and reads Unknown
    /// once the probe is past its budget; exactly one thread, and one probe
    /// in flight however often it is asked. What the stuck probe finally
    /// finds — past its budget — is Unknown too, never Down. NEGATIVE
    /// CONTROL: a resolver that fails AT ONCE reads Down.
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
        let until = Instant::now() + Duration::from_secs(5);
        while stuck.calls.load(Ordering::SeqCst) == 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(80));
        for _ in 0..5 {
            assert_eq!(np.ask("s-1"), Reach::Unknown);
            assert_eq!(np.ask("s-2"), Reach::Unknown);
        }
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(stuck.calls.load(Ordering::SeqCst), 1, "one probe in flight");
        assert_eq!(np.spawned.load(Ordering::SeqCst), 1, "one thread");
        release.send(()).expect("the stuck resolve");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(np.ask("s-1"), Reach::Unknown, "an overrun is no Down");
        np.stop();
        drop(release);

        // The control: a resolver that fails at once is a definite Down.
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
        let np = NetProbe::with(Arc::new(Nx), BUDGET);
        let asked = Instant::now();
        let mut seen = np.ask("s-1");
        while !matches!(seen, Reach::Down { .. }) && asked.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
            seen = np.ask("s-1");
        }
        assert!(matches!(seen, Reach::Down { .. }), "{seen:?}");
        // The run's start stands while it runs: the same value asked again.
        assert_eq!(np.ask("s-1"), seen);
        np.stop();
    }

    /// A scratch tree for the route's sources.
    fn scratch(tag: &str) -> PathBuf {
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

    /// THE ROUTE'S SOURCES, READ-ONLY: a quiet tree is the default route;
    /// each source that names a route makes it custom — the user's, the
    /// project's, the local one, the managed file, a managed drop-in, a
    /// managed preference domain's mere presence, and a `--settings` file or
    /// inline JSON — and so does an unparseable file, an environment that
    /// cannot locate the user's settings, and an agent directory not known
    /// (its project settings unseen). Nothing is written but by the test.
    #[test]
    fn the_route_reads_every_settings_source_claude_code_reads() {
        let proxy = "{\"env\":{\"HTTPS_PROXY\":\"http://proxy.corp:3128\"}}";
        let dir = scratch("route");
        let env = || {
            Some(vec![
                format!("HOME={}", dir.join("home").display()),
                "USER=owner".to_string(),
            ])
        };
        let facts = |argv: &[&str]| {
            let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
            netwatch::route_of(&gather(
                env(),
                &argv,
                &dir.join("proj"),
                &dir.join("managed"),
                &dir.join("prefs"),
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
        write(
            &dir.join("prefs/owner").join(netwatch::MANAGED_PREFS_FILE),
            "plist",
        );
        assert!(
            matches!(facts(&["claude"]), Route::Custom(_)),
            "an MDM domain"
        );
        std::fs::remove_file(dir.join("prefs/owner").join(netwatch::MANAGED_PREFS_FILE))
            .expect("rm");
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
        // No HOME, no CLAUDE_CONFIG_DIR: the user's settings cannot be found.
        let lost = netwatch::route_of(&gather(
            Some(vec!["USER=owner".to_string()]),
            &[],
            &dir.join("proj"),
            &dir.join("managed"),
            &dir.join("prefs"),
        ));
        assert!(matches!(lost, Route::Custom(_)), "{lost:?}");
        // The agent's directory not known (an empty or relative cwd): its
        // project settings cannot be read, so the route is custom — even
        // with every source this host CAN see quiet (the control above).
        for cwd in ["", "proj"] {
            let blind = netwatch::route_of(&gather(
                env(),
                &[],
                Path::new(cwd),
                &dir.join("managed"),
                &dir.join("prefs"),
            ));
            assert!(matches!(blind, Route::Custom(_)), "{cwd:?}: {blind:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
