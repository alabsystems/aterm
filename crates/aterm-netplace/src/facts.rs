// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reading the Mac ([`Sense`]): the default routes and the gateway's ARP entry (route socket and
//! `sysctl`, as `route_source_ipv4` reads them), this Mac's interface addresses, and the path
//! monitor (satisfied, Low Data Mode, expensive, link type). No packet, no child process, no
//! SSID and no UDP connect toward the LAN, so no question can reach the screen. Tests script the
//! facts ([`Scripted`]); headless code reaches nothing but the kernel's tables.
//!
//! THE SCRIPTED-FACTS SEAM ([`Script`]): a test scripts this thread's facts, and the routes its
//! answers took, with [`script_here`]; in a debug build outside this crate's tests,
//! `ATERM_DEBUG_NETPLACE=<file>` ([`SEAM`]) names a [`parse_script`] file read again at every
//! look, so the lab puts one Mac on a scripted phone hotspot and brings it home by rewriting the
//! file. A release build never reads the variable (its read in `seam_file` sits under
//! `cfg(all(debug_assertions, not(test)))`), and a missing file scripts nothing. The file never
//! scripts a proof ([`here_now`], which the vault's admission reads): only a test's thread script
//! does.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::place::{Facts, PathFacts, RouteFacts};

/// Where the facts come from.
pub trait Sense: Send {
    /// The facts now.
    fn facts(&self) -> Facts;

    /// Waits at most `most` for the network to change; `true` when it did.
    fn wait(&self, most: Duration) -> bool;
}

/// Whether `name` is a tunnel's interface: a VPN's `utun`, `ipsec`, `ppp`, `tun`, `tap` or
/// WireGuard's `wg`.
#[must_use]
pub fn is_tunnel(name: &str) -> bool {
    ["utun", "ipsec", "ppp", "tun", "tap", "wg"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// The prefix length of an IPv4 netmask.
fn prefix_of(mask: Option<IpAddr>) -> Option<u8> {
    match mask? {
        IpAddr::V4(mask) => u8::try_from(u32::from(mask).count_ones()).ok(),
        IpAddr::V6(_) => None,
    }
}

/// [`RouteFacts`] of `route` with this Mac's address on its interface from `interfaces`: the one
/// whose subnet holds the gateway, else the interface's first IPv4 address. `None` where the
/// interface holds no IPv4 address.
fn route_facts(
    route: &aterm_uds::network::DefaultRoute,
    interfaces: &[aterm_uds::network::InterfaceAddress],
) -> Option<RouteFacts> {
    let mut first = None;
    let mut matched = None;
    for address in interfaces {
        let IpAddr::V4(v4) = address.address else {
            continue;
        };
        if address.name != route.interface {
            continue;
        }
        let prefix = prefix_of(address.netmask).unwrap_or(32);
        if first.is_none() {
            first = Some((v4, prefix));
        }
        if route
            .gateway
            .is_some_and(|gw| crate::place::on_link_v4(v4, prefix, gw))
        {
            matched = Some((v4, prefix));
            break;
        }
    }
    let (address, prefix) = matched.or(first)?;
    Some(RouteFacts {
        interface: route.interface.clone(),
        address,
        prefix,
        gateway: route.gateway,
        gateway_hw: route.gateway_hw.as_deref().map(<[u8]>::to_vec),
    })
}

/// The route facts and tunnel of `routes` over `interfaces`: the primary default's network, or,
/// where a tunnel holds the primary, the first scoped default on another interface with a
/// gateway (the physical network under the tunnel).
#[must_use]
pub fn compose(
    routes: &aterm_uds::network::DefaultRoutes,
    interfaces: &[aterm_uds::network::InterfaceAddress],
) -> (Option<RouteFacts>, Option<String>) {
    let Some(primary) = routes.primary.as_ref() else {
        return (None, None);
    };
    if is_tunnel(&primary.interface) || primary.gateway.is_none() {
        let physical = routes
            .scoped
            .iter()
            .filter(|route| !is_tunnel(&route.interface) && route.gateway.is_some())
            .find_map(|route| route_facts(route, interfaces));
        return (
            physical.or_else(|| route_facts(primary, interfaces)),
            Some(primary.interface.clone()),
        );
    }
    (route_facts(primary, interfaces), None)
}

fn live_routes_now() -> (Option<RouteFacts>, Option<String>) {
    compose(
        &aterm_uds::network::default_routes(),
        &aterm_uds::network::interfaces().unwrap_or_default(),
    )
}

/// The route facts now, read from the kernel's tables with no packet, or the scripted ones
/// ([`scripted`]).
#[must_use]
pub fn routes_now() -> (Option<RouteFacts>, Option<String>) {
    if let Some(script) = scripted() {
        return (script.facts.route, script.facts.tunnel);
    }
    live_routes_now()
}

/// The facts a reader that runs no path monitor reads now: the routes ([`routes_now`]), and the
/// scripted path where the facts are scripted (else none: the record's last reading stands in).
#[must_use]
pub fn reader_facts() -> Facts {
    if let Some(script) = scripted() {
        return script.facts;
    }
    let (route, tunnel) = live_routes_now();
    Facts {
        route,
        tunnel,
        path: None,
    }
}

/// The local address the route to `addr` takes, read with no packet (`route_source_ipv4`): this
/// network's address for a host on it or behind its gateway, a tunnel's for one a VPN routes.
/// `None` off macOS, for an IPv6 address, where the kernel names none (loopback), or where the
/// facts are scripted and the script names none for `addr`.
#[must_use]
pub fn route_via(addr: IpAddr) -> Option<Ipv4Addr> {
    let addr = addr.to_canonical();
    if let Some(script) = scripted() {
        return script
            .via
            .iter()
            .find(|(at, _)| *at == addr)
            .and_then(|(_, via)| *via);
    }
    #[cfg(target_os = "macos")]
    {
        match addr {
            IpAddr::V4(v4) => aterm_uds::network::route_source_ipv4(v4),
            IpAddr::V6(_) => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The interface that holds this Mac's address `local` (`en0`): read from the kernel's interface
/// list with no packet, or, where the facts are scripted, the scripted route's interface when
/// `local` is its address. `None` where no interface holds it.
#[must_use]
pub fn interface_holding(local: Ipv4Addr) -> Option<String> {
    if let Some(script) = scripted() {
        return script
            .facts
            .route
            .filter(|route| route.address == local)
            .map(|route| route.interface);
    }
    aterm_uds::network::interfaces()
        .ok()?
        .into_iter()
        .find(|address| address.address == IpAddr::V4(local))
        .map(|address| address.name)
}

/// The network (strong id) traffic leaves this Mac on now ([`routes_now`],
/// [`NetworkId::of`](crate::place::NetworkId::of)): its gateway's hardware address and its subnet,
/// read with no packet. `None` with no IPv4 default route, or before ARP resolves the gateway.
#[must_use]
pub fn network_now() -> Option<crate::place::NetworkId> {
    routes_now()
        .0
        .as_ref()
        .and_then(crate::place::NetworkId::of)
}

/// `<net/if.h>`'s `IFF_POINTOPOINT` (the same on macOS and Linux).
const IFF_POINTOPOINT: u32 = 0x10;

/// What a caller that must not take memory for proof reads of this Mac now
/// ([`crate::proof`]): the facts with the path read by this process, whether the route's
/// interface is point to point, and every address this Mac holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Here {
    /// The route, the tunnel over it and the path as this process read it now ([`path_once`];
    /// `None`: not read).
    pub facts: Facts,
    /// The route's interface is point to point (`IFF_POINTOPOINT`).
    pub point_to_point: bool,
    /// Every address this Mac holds, on any interface.
    pub own: Vec<IpAddr>,
}

/// [`Here`] now: the kernel's routes and interfaces, read once, and the path as [`path_once`]
/// reads it; where a test scripts this thread's facts ([`script_here`]), the script's, the
/// scripted route's address this Mac's one address. The seam file ([`SEAM`]) never scripts it: a
/// proof ([`crate::proof`]) lets a Mac into the vault, and a debug build told by a file where it
/// is must not let one in by it ([`Reader::Proof`]). It reads Network.framework's path, so never
/// on a window's main thread.
#[must_use]
pub fn here_now() -> Here {
    if let Some(script) = script_of(thread_script(), seam_file, Reader::Proof) {
        let own = script
            .facts
            .route
            .iter()
            .map(|route| IpAddr::V4(route.address))
            .collect();
        return Here {
            facts: script.facts,
            point_to_point: false,
            own,
        };
    }
    let interfaces = aterm_uds::network::interfaces().unwrap_or_default();
    let (route, tunnel) = compose(&aterm_uds::network::default_routes(), &interfaces);
    let point_to_point = route.as_ref().is_some_and(|route| {
        interfaces
            .iter()
            .any(|address| address.name == route.interface && address.flags & IFF_POINTOPOINT != 0)
    });
    let path = path_once(route.as_ref());
    Here {
        facts: Facts {
            route,
            tunnel,
            path,
        },
        point_to_point,
        own: interfaces
            .into_iter()
            .map(|address| address.address)
            .collect(),
    }
}

/// How long a [`path_once`] reading stands.
#[cfg(target_os = "macos")]
const PATH_KEPT: Duration = Duration::from_secs(30);

/// How long [`path_once`] waits for Network.framework's first path update, which a Mac with a
/// network hands over at once.
#[cfg(target_os = "macos")]
const PATH_PATIENCE: Duration = Duration::from_secs(2);

/// What a [`path_once`] reading is kept against: the route it was read on (its interface, this
/// Mac's address there, and its gateway's hardware address), `None` with no route.
#[cfg(any(target_os = "macos", test))]
type PathKey = Option<(String, Ipv4Addr, Option<Vec<u8>>)>;

/// The [`PathKey`] of `route`.
#[cfg(any(target_os = "macos", test))]
fn path_key(route: Option<&RouteFacts>) -> PathKey {
    route.map(|route| {
        (
            route.interface.clone(),
            route.address,
            route.gateway_hw.clone(),
        )
    })
}

/// The reading `held` keeps for the route `key` at `now`: only a satisfied reading (none is never
/// kept) read on that very route within `kept_for`, so a move, or a gateway read since, reads the
/// path again at once.
#[cfg(any(target_os = "macos", test))]
fn kept_path(
    held: Option<&(Instant, PathKey, PathFacts)>,
    key: &PathKey,
    (now, kept_for): (Instant, Duration),
) -> Option<PathFacts> {
    held.filter(|(at, on, _)| on == key && now.saturating_duration_since(*at) < kept_for)
        .map(|(_, _, read)| *read)
}

/// The path this Mac's traffic takes now as a Network.framework path monitor's first update reads
/// it, for a process that runs no keeper's monitor, on `route` (the default route [`here_now`]
/// read): kept for [`PATH_KEPT`] while the route stays the same ([`kept_path`]), waiting at most
/// [`PATH_PATIENCE`]. Only a satisfied path is a reading, and only a reading is kept; `None` where
/// none was read in time, the path is not satisfied, or off macOS. Never on a window's main
/// thread: a member desk's lane and connection threads and the node's automatic admission worker
/// call it, through [`crate::proof::read`] (`aterm_secrets::home_lan::home_net`).
#[cfg(target_os = "macos")]
#[must_use]
pub fn path_once(route: Option<&RouteFacts>) -> Option<PathFacts> {
    static KEPT: Mutex<Option<(Instant, PathKey, PathFacts)>> = Mutex::new(None);
    let key = path_key(route);
    if let Some(read) = kept_path(
        KEPT.lock().unwrap_or_else(PoisonError::into_inner).as_ref(),
        &key,
        (Instant::now(), PATH_KEPT),
    ) {
        return Some(read);
    }
    let (tx, rx) = std::sync::mpsc::sync_channel::<aterm_objc::path_monitor::PathReading>(1);
    let read =
        aterm_objc::path_monitor::PathMonitor::start(c"aterm.netplace.path-once", move |reading| {
            let _ = tx.try_send(reading);
        })
        .and_then(|monitor| {
            let first = rx.recv_timeout(PATH_PATIENCE).ok();
            drop(monitor);
            first
        })
        .filter(|reading| reading.status == aterm_objc::path_monitor::PathStatus::Satisfied)
        .map(|reading| PathFacts {
            usable: Some(true),
            expensive: reading.expensive,
            constrained: reading.constrained,
            link: link_of(reading.link),
        });
    if let Some(read) = read {
        *KEPT.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), key, read));
    }
    read
}

/// No path reader off macOS.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub const fn path_once(_route: Option<&RouteFacts>) -> Option<PathFacts> {
    None
}

/// The place's link for a path monitor's.
#[cfg(target_os = "macos")]
fn link_of(link: aterm_objc::path_monitor::PathLink) -> crate::place::Link {
    use aterm_objc::path_monitor::PathLink;
    match link {
        PathLink::Wifi => crate::place::Link::Wifi,
        PathLink::Wired => crate::place::Link::Wired,
        PathLink::Cellular => crate::place::Link::Cellular,
        PathLink::Loopback => crate::place::Link::Loopback,
        PathLink::Other => crate::place::Link::Other,
    }
}

/// The variable naming the scripted-facts file, read by a debug build alone (never a test build,
/// whose tests script facts per thread with [`script_here`]).
pub const SEAM: &str = "ATERM_DEBUG_NETPLACE";

/// Facts scripted in place of the Mac's: the routes and path, and the local address the route to
/// each listed address takes (an address the script does not list takes none).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Script {
    /// The facts.
    pub facts: Facts,
    /// Per address, the local address its route takes (`route_source_ipv4`'s answer).
    pub via: Vec<(IpAddr, Option<Ipv4Addr>)>,
}

/// A hardware address `aa:bb:cc:dd:ee:ff`.
fn parse_hw(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() {
        return None;
    }
    let mut bytes = Vec::with_capacity(6);
    for part in text.split(':') {
        if bytes.len() >= 20 {
            return None;
        }
        bytes.push(u8::from_str_radix(part, 16).ok()?);
    }
    Some(bytes)
}

/// The script a [`SEAM`] file holds, one fact a line (blank lines and `#` comments skipped, a
/// line this build cannot read passed over):
///
/// - `route <interface> <address>/<prefix> [via <gateway>] [hw <aa:bb:cc:dd:ee:ff>]`
/// - `tunnel <interface>`
/// - `path <usable|unusable|invalid> [expensive] [constrained] <wifi|wired|cellular|loopback|other>`
/// - `via <address> <local address|->`
#[must_use]
pub fn parse_script(text: &str) -> Script {
    let mut script = Script::default();
    let mut words = Vec::with_capacity(8);
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        words.clear();
        words.extend(line.split_whitespace());
        match words.as_slice() {
            ["route", interface, cidr, rest @ ..] => {
                let Some((address, prefix)) = cidr.split_once('/') else {
                    continue;
                };
                let (Ok(address), Ok(prefix)) = (address.parse(), prefix.parse::<u8>()) else {
                    continue;
                };
                let mut route = RouteFacts {
                    interface: (*interface).to_owned(),
                    address,
                    prefix: prefix.min(32),
                    gateway: None,
                    gateway_hw: None,
                };
                for pair in rest.chunks(2) {
                    match pair {
                        ["via", gateway] => route.gateway = gateway.parse().ok(),
                        ["hw", hw] => route.gateway_hw = parse_hw(hw),
                        _ => {}
                    }
                }
                script.facts.route = Some(route);
            }
            ["tunnel", interface] => script.facts.tunnel = Some((*interface).to_owned()),
            ["path", state, rest @ ..] => {
                let usable = match *state {
                    "usable" => Some(true),
                    "unusable" => Some(false),
                    "invalid" => None,
                    _ => continue,
                };
                let Some(link) = rest
                    .last()
                    .and_then(|word| crate::place::Link::from_word(word))
                else {
                    continue;
                };
                script.facts.path = Some(PathFacts {
                    usable,
                    expensive: rest.contains(&"expensive"),
                    constrained: rest.contains(&"constrained"),
                    link,
                });
            }
            ["via", addr, local] => {
                if let Ok(addr) = addr.parse::<IpAddr>() {
                    script
                        .via
                        .push((addr.to_canonical(), local.parse::<Ipv4Addr>().ok()));
                }
            }
            _ => {}
        }
    }
    script
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static HERE: std::cell::RefCell<Option<Script>> = const { std::cell::RefCell::new(None) };
}

/// `f` with this thread's facts scripted by `script`, whatever the Mac or the environment says:
/// the seam's arm for tests, this crate's and the lanes' that read the place.
#[cfg(any(test, debug_assertions))]
pub fn script_here<R>(script: Script, f: impl FnOnce() -> R) -> R {
    let was = HERE.with(|here| here.replace(Some(script)));
    let ran = f();
    HERE.with(|here| here.replace(was));
    ran
}

/// Who reads the scripted facts ([`script_of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reader {
    /// A report, the keeper, a lane's pacing, a heartbeat's notes: the seam file scripts them, so
    /// the lab puts a Mac on a scripted network.
    Report,
    /// A proof ([`here_now`]): only a test's thread script, never the seam file.
    Proof,
}

/// The script `reader` takes: this thread's (`thread`, a test's [`script_here`]), else, for a
/// [`Reader::Report`] alone, the seam file's (`file`, read only then).
fn script_of(
    thread: Option<Script>,
    file: impl FnOnce() -> Option<Script>,
    reader: Reader,
) -> Option<Script> {
    thread.or_else(|| match reader {
        Reader::Report => file(),
        Reader::Proof => None,
    })
}

/// This thread's [`script_here`]; `None` in a release build.
fn thread_script() -> Option<Script> {
    #[cfg(any(test, debug_assertions))]
    {
        HERE.with(|here| here.borrow().clone())
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        None
    }
}

/// The file [`SEAM`] names, read now, in a debug build outside this crate's tests; `None` in a
/// release build, or with no file.
fn seam_file() -> Option<Script> {
    #[cfg(all(debug_assertions, not(test)))]
    {
        let file = std::env::var_os(SEAM)?;
        aterm_uds::atomic_file::read_regular_text(std::path::Path::new(&file), 1024 * 1024)
            .ok()
            .map(|text| parse_script(&text))
    }
    #[cfg(not(all(debug_assertions, not(test))))]
    {
        None
    }
}

/// The scripted facts now: this thread's [`script_here`], else (a debug build outside this
/// crate's tests) the file [`SEAM`] names, read now; `None` in a release build, or with neither.
#[must_use]
pub fn scripted() -> Option<Script> {
    script_of(thread_script(), seam_file, Reader::Report)
}

/// The live facts: the kernel's tables, this process's route-change socket, and on macOS a path
/// monitor whose every reading wakes [`Sense::wait`].
pub struct Live {
    routes: Option<aterm_uds::network::RouteChanges>,
    path: Arc<PathState>,
    #[cfg(target_os = "macos")]
    _monitor: Option<aterm_objc::path_monitor::PathMonitor>,
}

#[derive(Default)]
struct PathState {
    reading: Mutex<Option<PathFacts>>,
    changed: AtomicBool,
    wake: Condvar,
}

impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live").finish_non_exhaustive()
    }
}

impl Live {
    /// Starts the readers: the route-change socket and, on macOS, the path monitor.
    #[must_use]
    pub fn start() -> Self {
        let path = Arc::new(PathState::default());
        #[cfg(target_os = "macos")]
        let monitor = {
            let path = Arc::clone(&path);
            aterm_objc::path_monitor::PathMonitor::start(c"aterm.netplace.path", move |reading| {
                use aterm_objc::path_monitor::PathStatus;
                let facts = PathFacts {
                    usable: match reading.status {
                        PathStatus::Satisfied | PathStatus::Satisfiable => Some(true),
                        PathStatus::Unsatisfied => Some(false),
                        PathStatus::Invalid => None,
                    },
                    expensive: reading.expensive,
                    constrained: reading.constrained,
                    link: link_of(reading.link),
                };
                let mut held = path.reading.lock().unwrap_or_else(PoisonError::into_inner);
                if *held != Some(facts) {
                    *held = Some(facts);
                    path.changed.store(true, Ordering::SeqCst);
                    path.wake.notify_all();
                }
            })
        };
        Self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            routes: aterm_uds::network::RouteChanges::open(),
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            routes: None,
            path,
            #[cfg(target_os = "macos")]
            _monitor: monitor,
        }
    }
}

/// The longest one wait on the route socket lasts before a path reading is looked at.
const SLICE: Duration = Duration::from_secs(5);

impl Sense for Live {
    fn facts(&self) -> Facts {
        if let Some(script) = scripted() {
            return script.facts;
        }
        let (route, tunnel) = live_routes_now();
        Facts {
            route,
            tunnel,
            path: *self
                .path
                .reading
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        }
    }

    fn wait(&self, most: Duration) -> bool {
        let deadline = Instant::now() + most;
        loop {
            if self.path.changed.swap(false, Ordering::SeqCst) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let slice = left.min(SLICE);
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            if let Some(routes) = &self.routes {
                if routes.wait(slice) {
                    return true;
                }
                continue;
            }
            let held = self
                .path
                .reading
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let _ = self
                .path
                .wake
                .wait_timeout(held, slice)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Scripted facts for tests: [`Scripted::set`] changes them and wakes a waiter. Like a route
/// socket, a change made while no one waits is told to the next wait.
#[derive(Debug, Default)]
pub struct Scripted {
    /// The facts, how many times they were set, and how many of those a wait has told.
    facts: Mutex<(Facts, u64, u64)>,
    wake: Condvar,
    waits: std::sync::atomic::AtomicUsize,
}

impl Scripted {
    /// Facts to start from.
    #[must_use]
    pub fn new(facts: Facts) -> Self {
        Self {
            facts: Mutex::new((facts, 0, 0)),
            wake: Condvar::new(),
            waits: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// How many waits began: a test's witness that its reader got as far as waiting.
    #[must_use]
    pub fn waits(&self) -> usize {
        self.waits.load(Ordering::SeqCst)
    }

    /// Changes the facts and wakes a waiter.
    pub fn set(&self, facts: Facts) {
        let mut held = self.facts.lock().unwrap_or_else(PoisonError::into_inner);
        held.0 = facts;
        held.1 += 1;
        self.wake.notify_all();
    }
}

impl Sense for Arc<Scripted> {
    fn facts(&self) -> Facts {
        self.facts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .clone()
    }

    fn wait(&self, most: Duration) -> bool {
        let held = self.facts.lock().unwrap_or_else(PoisonError::into_inner);
        self.waits.fetch_add(1, Ordering::SeqCst);
        let (mut held, _) = self
            .wake
            .wait_timeout_while(held, most, |held| held.1 == held.2)
            .unwrap_or_else(PoisonError::into_inner);
        let changed = held.1 != held.2;
        held.2 = held.1;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_uds::network::{DefaultRoute, DefaultRoutes, InterfaceAddress};

    fn address(name: &str, ip: [u8; 4], mask: [u8; 4]) -> InterfaceAddress {
        InterfaceAddress {
            name: name.to_owned(),
            address: IpAddr::V4(Ipv4Addr::from(ip)),
            netmask: Some(IpAddr::V4(Ipv4Addr::from(mask))),
            flags: 0,
        }
    }

    /// THE SCRIPTED-FACTS SEAM: a script file's lines read back as facts and routes, and a
    /// thread that scripts them reads them from every reader (the routes, a reader's facts, the
    /// route an answer took, the live sense), while another thread reads the Mac's own.
    /// NEGATIVE CONTROL: `route_via` that skips the script (its `if let Some(script)` arm removed)
    /// asks the kernel for the scripted address, and "the scripted route" fails.
    #[test]
    fn the_debug_seam_scripts_the_facts_on_this_thread_alone() {
        let script = parse_script(
            "# a phone hotspot\n\
             route en0 172.20.10.2/28 via 172.20.10.1 hw 02:00:00:00:00:01\n\
             path usable expensive wifi\n\
             via 192.168.4.9 172.20.10.2\n\
             via 10.8.0.1 -\n\
             nonsense line\n",
        );
        let route = script.facts.route.clone().expect("the route");
        assert_eq!(route.address, Ipv4Addr::new(172, 20, 10, 2));
        assert_eq!(route.prefix, 28);
        assert_eq!(route.gateway, Some(Ipv4Addr::new(172, 20, 10, 1)));
        assert_eq!(route.gateway_hw, Some(vec![2, 0, 0, 0, 0, 1]));
        let path = script.facts.path.expect("the path");
        assert!(path.expensive && path.usable == Some(true) && !path.constrained);
        assert_eq!(path.link, crate::place::Link::Wifi);
        let home: IpAddr = "192.168.4.9".parse().expect("an address");
        script_here(script.clone(), || {
            assert_eq!(routes_now().0.as_ref(), Some(&route));
            assert_eq!(reader_facts(), script.facts);
            assert_eq!(
                route_via(home),
                Some(Ipv4Addr::new(172, 20, 10, 2)),
                "the scripted route"
            );
            assert_eq!(route_via("10.8.0.1".parse().expect("an address")), None);
            assert_eq!(Live::start().facts(), script.facts);
        });
        let elsewhere = std::thread::spawn(scripted).join().expect("another thread");
        assert_eq!(elsewhere, None, "another thread reads the Mac's own");
        assert_eq!(parse_script(""), Script::default());
    }

    /// The seam file scripts every reader but a proof: a report takes the file's script where the
    /// thread has none, a proof (what the vault's admission reads, [`here_now`]) never does and
    /// reads the Mac's own, and a test's thread script scripts both.
    /// NEGATIVE CONTROL: `script_of` handing a proof the file's script too (its `Reader::Proof`
    /// arm → `file()`) gives the proof the file's facts, and "a proof never takes the file's"
    /// fails.
    #[test]
    fn the_seam_file_never_scripts_a_proof() {
        let file = parse_script("route en0 192.168.1.20/24 via 192.168.1.1 hw 02:00:00:00:00:01\n");
        let thread = parse_script("route en0 10.0.0.2/24 via 10.0.0.1\n");
        let from_file = || Some(file.clone());
        assert_eq!(
            script_of(None, from_file, Reader::Report),
            Some(file.clone()),
            "a report takes the file's"
        );
        assert_eq!(
            script_of(None, from_file, Reader::Proof),
            None,
            "a proof never takes the file's"
        );
        for reader in [Reader::Report, Reader::Proof] {
            assert_eq!(
                script_of(Some(thread.clone()), from_file, reader),
                Some(thread.clone()),
                "{reader:?}: a test's thread script"
            );
        }
        let routed = script_here(thread.clone(), here_now);
        assert_eq!(
            routed.facts, thread.facts,
            "here_now takes the thread's script"
        );
    }

    /// A path reading is kept only on the route it was read on, and only while it stands: back on
    /// the home LAN from a phone hotspot (another address, another gateway), or once the gateway's
    /// hardware address is read, the path is read again at once, not after the hotspot's
    /// "expensive" reading has run out; an unsatisfied reading (none) is never kept at all, since
    /// [`path_once`] stores only a reading.
    /// NEGATIVE CONTROL: `kept_path` keeping by time alone (its `on == key &&` dropped) hands the
    /// hotspot's reading to the home route, and "a move reads the path again" fails.
    #[test]
    fn a_path_reading_is_kept_only_on_the_route_it_was_read_on() {
        let route = |address: [u8; 4], hw: Option<u8>| RouteFacts {
            interface: "en0".to_owned(),
            address: Ipv4Addr::from(address),
            prefix: 24,
            gateway: None,
            gateway_hw: hw.map(|hw| vec![0, 1, 2, 3, 4, hw]),
        };
        let hotspot = route([172, 20, 10, 2], Some(1));
        let expensive = PathFacts {
            usable: Some(true),
            expensive: true,
            constrained: false,
            link: crate::place::Link::Wifi,
        };
        let at = Instant::now();
        let held = (at, path_key(Some(&hotspot)), expensive);
        let kept = Duration::from_secs(30);
        let soon = at + Duration::from_secs(5);
        assert_eq!(
            kept_path(Some(&held), &path_key(Some(&hotspot)), (soon, kept)),
            Some(expensive),
            "the same route keeps its reading"
        );
        assert_eq!(
            kept_path(
                Some(&held),
                &path_key(Some(&route([192, 168, 1, 20], Some(9)))),
                (soon, kept)
            ),
            None,
            "a move reads the path again"
        );
        let unread = route([192, 168, 1, 20], None);
        let unread_held = (at, path_key(Some(&unread)), expensive);
        assert_eq!(
            kept_path(
                Some(&unread_held),
                &path_key(Some(&route([192, 168, 1, 20], Some(9)))),
                (soon, kept)
            ),
            None,
            "a gateway read since reads the path again"
        );
        assert_eq!(
            kept_path(Some(&held), &path_key(Some(&hotspot)), (at + kept, kept)),
            None,
            "a reading past its time is read again"
        );
        assert_eq!(
            kept_path(None, &path_key(None), (soon, kept)),
            None,
            "nothing held"
        );
    }

    /// With a tunnel holding the primary default, the network is the scoped physical default
    /// under it, with that interface's address and prefix, and the tunnel is named; with none,
    /// the primary's own.
    /// NEGATIVE CONTROL: composing from the primary whatever holds it keys the place to the
    /// tunnel's address (`100.64.0.7`), and this test fails.
    #[test]
    fn a_tunnel_holding_the_default_is_looked_through_to_the_physical_network() {
        let gw = Ipv4Addr::new(192, 168, 4, 1);
        let interfaces = [
            address("en0", [192, 168, 4, 31], [255, 255, 252, 0]),
            address("utun4", [100, 64, 0, 7], [255, 255, 255, 255]),
        ];
        let routes = DefaultRoutes {
            primary: Some(DefaultRoute {
                interface: "utun4".to_owned(),
                gateway: None,
                gateway_hw: None,
            }),
            scoped: vec![DefaultRoute {
                interface: "en0".to_owned(),
                gateway: Some(gw),
                gateway_hw: Some(vec![1, 2, 3, 4, 5, 6].into_boxed_slice()),
            }],
        };
        let (route, tunnel) = compose(&routes, &interfaces);
        let route = route.expect("the physical network");
        assert_eq!(route.address, Ipv4Addr::new(192, 168, 4, 31));
        assert_eq!(route.prefix, 22);
        assert_eq!(route.gateway, Some(gw));
        assert_eq!(tunnel.as_deref(), Some("utun4"));
        let plain = DefaultRoutes {
            primary: routes.scoped.first().cloned(),
            scoped: Vec::new(),
        };
        let (route, tunnel) = compose(&plain, &interfaces);
        assert_eq!(route.map(|r| r.interface), Some("en0".to_owned()));
        assert_eq!(tunnel, None);
        assert_eq!(
            compose(&DefaultRoutes::default(), &interfaces),
            (None, None)
        );
    }
}
