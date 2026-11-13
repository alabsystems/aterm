// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Background network reachability probe (`aterm-harness-netprobe`) for `harness::netwatch`.

use std::collections::HashMap;
use std::io::{self, Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use aterm_agent::harness::netwatch::{self, NetState, Outcome, Route, RouteFacts, Source};
use aterm_agent::harness::witness::{RouteKey, Target, Witnesses};
use aterm_agent::supervise::policy::WorkerEnv;
use aterm_agent::supervise::policy::claude_settings;
use aterm_agent::supervise::policy::turn_end::Reach;

/// Maximum endpoint probes, including the default one. A new endpoint beyond this count uses
/// the path-only reading; live probes are never evicted or confused with another endpoint.
pub(crate) const MAX_ENDPOINT_PROBES: usize = 64;

/// One reading of this Mac's network ([`PathSource::watch`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathEvent {
    /// The path in use: whether it can carry traffic ([`NetState::path`]). Only macOS's path
    /// monitor reads it; an endpoint's own probe is handed the last one ([`NetProbe::to`]).
    Usable(bool),
    /// The routes moved (an address or a link changed), which says nothing of whether the path
    /// can carry traffic ([`NetState::moved`]): a switch between two usable paths, or on a host
    /// with no path monitor the only word of a change.
    #[cfg_attr(
        all(not(any(target_os = "macos", target_os = "linux")), not(test)),
        expect(dead_code, reason = "the route source is macOS's and Linux's")
    )]
    Moved,
    /// The network place settled onto a network (`crate::network_moved::Woke::Place`): the new
    /// network is usable, so the API's reach is measured now (and again while it reads down),
    /// whether or not a session holds a lease ([`NetProbe::settled`]), and the place's internet
    /// axis says what this network gives. Built on every platform: [`NetProbe::settled`] hands it
    /// on.
    Settled,
    /// The path came back after a loss (a usable reading after an unusable one, [`PathEdge`]),
    /// told right after that [`PathEvent::Usable`]: on the network the path left and came back
    /// to no place settles (a settle back within `aterm_netplace::moves::REST_MS` is not told),
    /// so the API's reach is measured again under the [`SETTLED_LEASE`]
    /// ([`NetProbe::remeasure`]) and the internet answer from before the loss does not stand
    /// untested. Only macOS's path source hears it.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the path monitor is macOS's")
    )]
    Back,
    /// This Mac woke from a sleep (`aterm_uds::power`'s wake after a will-sleep, [`WakeEdge`]):
    /// whatever network it woke on, the internet answer measured before the sleep says nothing
    /// of now, so the API's reach is measured again under the [`SETTLED_LEASE`]
    /// ([`NetProbe::remeasure`]). On the network it slept on no place settles, so nothing else
    /// would. Only macOS's path source hears it.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the wake source is macOS's")
    )]
    Woke,
}

/// WHICH PATH READING IS A RETURN ([`PathEvent::Back`]): a usable reading after an unusable one.
/// The watch's first reading is none (nothing was lost), nor a usable one after a usable one.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PathEdge {
    /// The last reading.
    last: Option<bool>,
}

#[cfg(any(target_os = "macos", test))]
impl PathEdge {
    /// `usable` read: whether it is the path's return after a loss.
    pub(crate) fn read(&mut self, usable: bool) -> bool {
        let back = usable && self.last == Some(false);
        self.last = Some(usable);
        back
    }
}

/// WHICH POWER ANNOUNCEMENT IS A WAKE ([`PathEvent::Woke`]): `aterm_uds::power` hands a
/// [`Power::Woke`](aterm_uds::power::Power::Woke) both for a wake and for an idle sleep that did
/// not come (`kIOMessageSystemWillNotSleep`, after which nothing slept), so a wake is the first
/// `Woke` after a `WillSleep`, and each sleep owes one measure.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WakeEdge {
    /// A will-sleep was heard and its wake not yet.
    slept: bool,
}

#[cfg(any(target_os = "macos", test))]
impl WakeEdge {
    /// `power` heard: whether it is the wake from a sleep heard before it.
    pub(crate) fn hear(&mut self, power: aterm_uds::power::Power) -> bool {
        match power {
            aterm_uds::power::Power::WillSleep => {
                self.slept = true;
                false
            }
            aterm_uds::power::Power::Woke => std::mem::take(&mut self.slept),
        }
    }
}

/// The lease a settled place takes on the probe ([`NetProbe::settled`]): one measure, then it
/// lapses with no session renewing it, unless that measure read down ([`SETTLED_DOWN_FOR`]).
pub(crate) const SETTLED_LEASE: &str = "network-place";

/// How long after the place settles its lease is renewed while the API reads down
/// ([`keep_settled`]): a router's Wi-Fi comes back a minute or two before its WAN, and one
/// measure would read the whole stay down with no session asking. The probe then measures at its
/// down cadence until an Up, for at most this long; past it the place's down note goes stale
/// (`aterm_netplace::place::INTERNET_FRESH_MS`) and the place reads what the path says.
pub(crate) const SETTLED_DOWN_FOR: Duration = Duration::from_secs(10 * 60);

/// After a probe that ended `outcome` at `now`, while the place's settle (`until`: its watch's
/// end) still runs and the API did not read up: the settled lease renewed in `st`, so the probe
/// measures again at its down cadence. An Up, or the watch run out, ends it. The watch's next
/// state.
fn keep_settled(
    st: &mut NetState,
    outcome: Outcome,
    until: Option<Instant>,
    (now, wall): (Instant, SystemTime),
) -> Option<Instant> {
    match until {
        Some(until) if outcome != Outcome::Up && now < until => {
            let _ = st.ask(SETTLED_LEASE, now, wall);
            Some(until)
        }
        _ => None,
    }
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
/// (the fleet's browse and beacons, the network place) say nothing of the API's reach and
/// are not passed on. On macOS the path's return after a loss is told too, as a
/// [`PathEvent::Back`] after its reading ([`PathEdge`]), and so is a wake from sleep
/// (`aterm_uds::power::listen`, IOKit's root power domain, on a thread of its own that ends
/// with the watch), as a [`PathEvent::Woke`] ([`WakeEdge`]). Elsewhere none.
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
        let woke = Arc::clone(&sink);
        let mut edge = WakeEdge::default();
        let power = aterm_uds::power::listen(Box::new(move |announced| {
            if edge.hear(announced) {
                woke(PathEvent::Woke);
            }
        }));
        if power.is_none() {
            aterm_log::info!(
                "harness: no wake announcement; the API reach is measured again at a wake only \
                 once the path or the network place moves"
            );
        }
        let edge = Mutex::new(PathEdge::default());
        let monitor = PathMonitor::start(c"aterm.harness.net-path", move |reading| {
            if let Some(usable) = usable(reading.status) {
                sink(PathEvent::Usable(usable));
                if edge
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .read(usable)
                {
                    sink(PathEvent::Back);
                }
            }
        });
        match (monitor, routes) {
            (None, None) => None,
            (monitor, routes) => Some(Box::new((monitor, routes, power))),
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
/// socket's, since the fleet's browse and beacons and the network place say nothing of the API's
/// reach.
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
                    let woke = moved.wait(Self::SLICE);
                    if ended.load(Ordering::SeqCst) {
                        break;
                    }
                    if route_move(woke) {
                        sink(PathEvent::Moved);
                    } else if woke == crate::network_moved::Woke::Place {
                        sink(PathEvent::Settled);
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
    /// Reads an HTTP answer from a plain-HTTP endpoint `host` over `tcp` before `deadline`
    /// ([`http_answer`]): its server process answered, which the kernel's accept alone does not
    /// say (v6 §14.17.5.4: a session's `http://` endpoint, the lab stub's among them).
    fn http_answers(
        &self,
        tcp: TcpStream,
        host: &str,
        deadline: aterm_http::Deadline,
    ) -> io::Result<()> {
        http_answer(tcp, host, deadline)
    }
}

/// A `HEAD /` to `host` over `tcp`, answered before `deadline` with a status line (`HTTP/…`,
/// whatever its status): the endpoint's server process answered. A connection closed with no
/// answer, or one that answers no HTTP, is an error.
pub(crate) fn http_answer(
    mut tcp: TcpStream,
    host: &str,
    deadline: aterm_http::Deadline,
) -> io::Result<()> {
    let left = deadline
        .remaining()
        .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
    tcp.set_write_timeout(Some(left))?;
    tcp.set_read_timeout(Some(left))?;
    tcp.write_all(
        format!(
            "HEAD / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: aterm-harness-probe\r\n\
             Connection: close\r\n\r\n"
        )
        .as_bytes(),
    )?;
    let mut head = [0_u8; 5];
    tcp.read_exact(&mut head)?;
    if &head == b"HTTP/" {
        Ok(())
    } else {
        Err(io::Error::other("the endpoint answered no HTTP"))
    }
}

/// Production probe using LAN-aware resolution, timed TCP connect, and platform TLS verification.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SystemProbe;

impl Probe for SystemProbe {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        #[cfg(unix)]
        {
            aterm_uds::lan::resolve(host, netwatch::PROBE_BUDGET)
                .map(|addresses| {
                    addresses
                        .into_iter()
                        .map(|ip| SocketAddr::new(ip, port))
                        .collect()
                })
                .map_err(io::Error::from)
        }
        #[cfg(windows)]
        {
            use std::net::ToSocketAddrs as _;
            Ok((host, port).to_socket_addrs()?.collect())
        }
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
    #[cfg(target_os = "macos")]
    let worded = e.to_string()
        == "failed to lookup address information: nodename nor servname provided, or not known";
    #[cfg(not(target_os = "macos"))]
    let worded = false;
    e.kind() == io::ErrorKind::NotFound || worded
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

/// Runs one probe of the default API route within `budget` ([`probe_target`]).
#[cfg(test)]
pub(crate) fn probe_once(probe: &dyn Probe, budget: Duration) -> Outcome {
    probe_target(probe, &Target::anthropic(), budget)
}

/// Runs one probe of `target` within `budget`: resolve, connect, then a verified TLS handshake
/// for an `https` endpoint, an HTTP answer for a plain one ([`Probe::http_answers`]). A name that
/// does not resolve, or every address refused or unreachable, is Down; any other failure Unknown.
pub(crate) fn probe_target(probe: &dyn Probe, target: &Target, budget: Duration) -> Outcome {
    let deadline = aterm_http::Deadline::after(budget);
    let in_time = |definite: Outcome| {
        if deadline.remaining().is_some() {
            definite
        } else {
            Outcome::Unknown
        }
    };
    let addrs = match probe.resolve(&target.host, target.port) {
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
                let answered = if target.tls {
                    probe.handshake(tcp, &target.host, deadline)
                } else {
                    probe.http_answers(tcp, &target.host, deadline)
                };
                return match answered {
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
    /// The endpoint this probe reaches: Claude Code's default route for the instance's own, a
    /// session's named endpoint for one [`Self::to`] made (v6 §14.17.5.4).
    target: Target,
    /// The probes of the other endpoints sessions' launches name ([`Self::to`]), each made at its
    /// first ask and fed this probe's path readings ([`Self::path_moved`]).
    others: Mutex<HashMap<Target, Arc<NetProbe>>>,
    /// Capacity has been reported once, without printing another launch's URL.
    saturated: AtomicBool,
    /// The path's last usability reading this probe heard, which a probe [`Self::to`] makes
    /// starts from.
    usable: Mutex<Option<bool>>,
    /// The instance's book of answered turns, the witnesses on each route (v6 §14.17.5.4,
    /// [`Self::answered`], [`Self::witness`]), kept on the instance's own probe.
    witnesses: Mutex<Witnesses>,
    /// Until when the settled place's lease is renewed while the API reads down
    /// ([`keep_settled`]); `None` outside a settle's watch.
    settled_until: Mutex<Option<Instant>>,
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
        Self::reaching(probe, state, Target::anthropic())
    }

    /// Creates a probe of `target` using `probe` over `state`.
    fn reaching(probe: Arc<dyn Probe>, state: NetState, target: Target) -> Arc<Self> {
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
            target,
            others: Mutex::default(),
            saturated: AtomicBool::new(false),
            usable: Mutex::default(),
            witnesses: Mutex::default(),
            settled_until: Mutex::new(None),
        })
    }

    /// THE PROBE OF `target` (v6 §14.17.5.4, slice 12c): this one where it reaches `target`, else
    /// the instance's probe of that endpoint, made at its first ask with this probe's budget and
    /// the path's last reading, and fed every later one ([`Self::path_moved`]). It probes nothing
    /// until a session asks it, and its thread parks while no lease lives, so an endpoint no tab
    /// waits on uses no probe work. The table and parked threads are bounded by
    /// [`MAX_ENDPOINT_PROBES`]; a new target at capacity returns `None`, never another target's
    /// reading. Existing targets remain available and all stop with this instance.
    pub(crate) fn to(self: &Arc<Self>, target: &Target) -> Option<Arc<Self>> {
        if *target == self.target {
            return Some(Arc::clone(self));
        }
        self.watch_path();
        let mut others = self.others.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(probe) = others.get(target) {
            return Some(Arc::clone(probe));
        }
        if others.len() >= MAX_ENDPOINT_PROBES - 1 {
            if !self.saturated.swap(true, Ordering::SeqCst) {
                aterm_log::info!(
                    "harness: API endpoint probe capacity reached; new endpoints use path observations"
                );
            }
            return None;
        }
        let budget = self.lock().budget();
        let probe = Self::reaching(
            Arc::clone(&self.probe),
            NetState::with_budget(budget),
            target.clone(),
        );
        if self.stop.load(Ordering::SeqCst) {
            probe.stop();
        }
        let usable = *self.usable.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(usable) = usable {
            probe.path_moved(PathEvent::Usable(usable));
        }
        aterm_log::info!(
            "harness: the API reach of {} is probed apart from {}",
            target.says(),
            self.target.says()
        );
        others.insert(target.clone(), Arc::clone(&probe));
        Some(probe)
    }

    /// The endpoint this probe reaches.
    #[cfg(test)]
    pub(crate) fn target(&self) -> &Target {
        &self.target
    }

    /// The endpoints probed apart from this one ([`Self::to`]), each made at a session's first
    /// ask: what a test reads to know which endpoints sessions' asks reached, without a wait.
    #[cfg(test)]
    pub(crate) fn others(&self) -> Vec<Target> {
        let mut targets: Vec<Target> = self
            .others
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        targets.sort();
        targets
    }

    /// Session `sid`'s turn on route `key` ended on a real reply `at`: booked as a witness on that
    /// route ([`Witnesses::answered`]).
    /// A real reply from an internet host also says the internet answered when that reply came,
    /// for this Mac's network place (`crate::net_place::note_internet`, a turn reply stamped with
    /// the reply's own time, which can be hours old: the place counts it only for the stay it came
    /// in, and a stale one is noted nowhere).
    pub(crate) fn answered(&self, key: RouteKey, sid: &str, at: Instant) {
        let age_ms = u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX);
        crate::net_place::note_internet(
            true,
            aterm_netplace::InternetSource::TurnReply,
            &key.base.target.host,
            aterm_netplace::unix_ms_now().saturating_sub(age_ms),
        );
        self.witnesses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .answered(key, sid, at);
    }

    /// The witness on route `key` for session `sid` ([`Witnesses::witness`]).
    pub(crate) fn witness(&self, key: &RouteKey, sid: &str) -> Option<Instant> {
        self.witnesses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .witness(key, sid)
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
                me.heard(event);
            }
        }));
        if watch.is_none() {
            aterm_log::info!(
                "harness: no network path watch; the probe's cadence alone finds the API's \
                 return, and a session it does not measure reads unknown"
            );
        }
    }

    /// ONE READING OF THE INSTALLED PATH SOURCE's watch ([`Self::watch_path`]): the place settled
    /// ([`Self::settled`]); the path came back after a loss ([`PathEvent::Back`]) or this Mac
    /// woke from a sleep ([`PathEvent::Woke`]), each measured again once under the
    /// [`SETTLED_LEASE`] ([`Self::remeasure`]), since on the network the path came back to, or
    /// the one this Mac slept on, no place settles and the internet answer from before would
    /// stand untested. Every other reading is [`Self::path_moved`]'s. Only this, the instance's
    /// own probe, takes the lease: an endpoint's probe ([`Self::to`]) is fed the path alone.
    fn heard(self: &Arc<Self>, event: PathEvent) {
        match event {
            PathEvent::Settled => self.settled(),
            PathEvent::Back | PathEvent::Woke => self.remeasure(),
            PathEvent::Usable(_) | PathEvent::Moved => self.path_moved(event),
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
        if let PathEvent::Usable(usable) = event {
            *self.usable.lock().unwrap_or_else(PoisonError::into_inner) = Some(usable);
        }
        let others: Vec<Arc<Self>> = self
            .others
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        for other in others {
            other.path_moved(event);
        }
        let (was, owed) = {
            let mut st = self.lock();
            let was = st.path_satisfied();
            let owed = match event {
                PathEvent::Usable(satisfied) => {
                    st.path(satisfied, Instant::now(), SystemTime::now())
                }
                PathEvent::Moved | PathEvent::Settled => st.moved(Instant::now()),
                PathEvent::Back | PathEvent::Woke => false,
            };
            if owed {
                self.wake.notify_all();
            }
            (was, owed)
        };
        let satisfied = match event {
            PathEvent::Usable(satisfied) => satisfied,
            PathEvent::Back | PathEvent::Woke => return,
            PathEvent::Moved | PathEvent::Settled => {
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

    /// THE NETWORK PLACE SETTLED onto a network ([`PathEvent::Settled`]): the routes moved
    /// ([`Self::path_moved`]), and the API's reach is measured now under the [`SETTLED_LEASE`],
    /// which no session renews, so it lapses after that measure, or, while the API reads down,
    /// once an Up came or [`SETTLED_DOWN_FOR`] passed ([`keep_settled`]). A route move alone owes
    /// a probe only to a session's lease, and with no session asking (the usual case while every
    /// turn ends on a reply) nothing measured the new network: its internet read "path up, not
    /// measured" for as long as no turn ended on one.
    pub(crate) fn settled(self: &Arc<Self>) {
        self.path_moved(PathEvent::Settled);
        self.remeasure();
    }

    /// THE API's REACH MEASURED AGAIN with no session asking: the [`SETTLED_LEASE`] taken, which
    /// no session renews, so it lapses after one measure, or, while the API reads down, once an
    /// Up came or [`SETTLED_DOWN_FOR`] passed ([`keep_settled`]). The place settling
    /// ([`Self::settled`]), the path's return after a loss and a wake from sleep
    /// ([`Self::heard`]) each take it once; the measure lands in the place's internet axis
    /// ([`Self::settle`]).
    pub(crate) fn remeasure(self: &Arc<Self>) {
        *self
            .settled_until
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Instant::now().checked_add(SETTLED_DOWN_FOR);
        let _ = self.ask(SETTLED_LEASE);
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
        let others: Vec<Arc<Self>> = self
            .others
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        for other in others {
            other.stop();
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
            let outcome = probe_target(&*self.probe, &self.target, budget);
            self.settle(outcome, Instant::now(), SystemTime::now());
        }
    }

    /// A probe ended with `outcome`: its measure recorded, and a change of the verdict
    /// ([`netwatch::Edge`]) logged once, outside the lock. A repeat logs nothing.
    fn settle(&self, outcome: Outcome, now: Instant, wall: SystemTime) {
        let (edge, waiting) = {
            let mut st = self.lock();
            let edge = st.finish(outcome, now, wall);
            let mut until = self
                .settled_until
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *until = keep_settled(&mut st, outcome, *until, (now, wall));
            drop(until);
            (edge, st.waiting(now))
        };
        if let Some(edge) = edge {
            aterm_log::info!("{}", edge_line(&edge, waiting));
        }
        // A measured answer, either way, is the internet axis of this Mac's network place.
        match outcome {
            Outcome::Up | Outcome::Down => crate::net_place::note_internet(
                outcome == Outcome::Up,
                aterm_netplace::InternetSource::ApiProbe,
                &self.target.host,
                aterm_netplace::unix_ms_now(),
            ),
            Outcome::Unknown => {}
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

/// Reads agent `pid`'s launch's route variables from its environment and Claude Code settings
/// ([`netwatch::launch_env`]), or why they name no one route.
pub(crate) fn launch_env_of_agent(
    pid: u32,
    argv: &[String],
    cwd: &str,
) -> Result<Vec<String>, netwatch::Unnamed> {
    let env = atpkg::caller_shell::process_args(pid).map(|a| a.env);
    netwatch::launch_env(&gather(
        env,
        argv,
        Path::new(cwd),
        claude_settings::MANAGED_DIR.map(Path::new),
        Path::new(claude_settings::MANAGED_PREFERENCES_DIR),
        aterm_types::dirs::home_dir().as_deref(),
    ))
}

/// How agent `pid` authenticates once its settings and profile store are read: they only move
/// its environment's route off its account's own credential.
pub(crate) fn auth_route_of_agent(
    pid: u32,
    argv: &[String],
    cwd: &str,
) -> aterm_agent::harness::accounts::AuthRoute {
    let env = atpkg::caller_shell::process_args(pid).map(|a| a.env);
    let home = aterm_types::dirs::home_dir();
    route_of_facts(
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

/// What agent `pid`'s Claude Code settings make of `/login` (`claude_settings::login_pin_under`,
/// the post-landing review of 2026-11-02): read under its own environment (its
/// `CLAUDE_CONFIG_DIR` or home), its launch directory and this Mac's managed settings, the
/// fence the route's reader uses. An environment that cannot be read is
/// [`LoginPin::Unread`](aterm_agent::supervise::policy::login::LoginPin::Unread).
pub(crate) fn login_pin_of_agent(
    pid: u32,
    argv: &[String],
    cwd: &str,
) -> aterm_agent::supervise::policy::login::LoginPin {
    use aterm_agent::supervise::policy::login::LoginPin;
    let Some(env) = atpkg::caller_shell::process_args(pid).map(|a| a.env) else {
        return LoginPin::Unread;
    };
    let worker = WorkerEnv::new(atpkg::caller_shell::ProcArgs {
        argv: argv.to_vec(),
        env,
        ..Default::default()
    });
    claude_settings::login_pin_under(
        &worker,
        Path::new(cwd),
        claude_settings::MANAGED_DIR.map(Path::new),
        Path::new(claude_settings::MANAGED_PREFERENCES_DIR),
        aterm_types::dirs::home_dir().as_deref(),
    )
}

/// The default permission mode of agent `pid`'s Claude Code settings
/// (`claude_settings::permission_default_under`): read under its own environment (its
/// `CLAUDE_CONFIG_DIR` or home), its line's `--settings`, its launch directory and this Mac's
/// managed settings, the fence the route's reader uses. An environment that cannot be read is
/// unread.
pub(crate) fn permission_default_of_agent(
    pid: u32,
    argv: &[String],
    cwd: &str,
) -> claude_settings::PermissionDefault {
    let Some(env) = atpkg::caller_shell::process_args(pid).map(|a| a.env) else {
        return claude_settings::PermissionDefault {
            mode: claude_settings::Resolved::Unread(
                "its agent's environment cannot be read".to_string(),
            ),
            bypass_disabled: None,
        };
    };
    let worker = WorkerEnv::new(atpkg::caller_shell::ProcArgs {
        argv: argv.to_vec(),
        env,
        ..Default::default()
    });
    claude_settings::permission_default_under(
        &worker,
        Path::new(cwd),
        claude_settings::MANAGED_DIR.map(Path::new),
        Path::new(claude_settings::MANAGED_PREFERENCES_DIR),
        aterm_types::dirs::home_dir().as_deref(),
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

/// [`auth_route_of_agent`] over what was gathered: an unread or empty environment is outside.
fn route_of_facts(
    facts: &RouteFacts,
    home: Option<&Path>,
) -> aterm_agent::harness::accounts::AuthRoute {
    use aterm_agent::harness::accounts;
    let Some(env) = facts.env.as_ref().filter(|env| !env.is_empty()) else {
        return accounts::AuthRoute::Outside;
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
        .route
}

#[cfg(test)]
#[path = "harness_netprobe_tests.rs"]
mod tests;
