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
//! file. A release build never reads the variable (its read in [`scripted`] sits under
//! `cfg(all(debug_assertions, not(test)))`), and a missing file scripts nothing.

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
    let mut candidates = interfaces
        .iter()
        .filter_map(|address| match address.address {
            IpAddr::V4(v4) if address.name == route.interface => {
                Some((v4, prefix_of(address.netmask).unwrap_or(32)))
            }
            _ => None,
        });
    let all: Vec<(Ipv4Addr, u8)> = candidates.by_ref().collect();
    let (address, prefix) = all
        .iter()
        .find(|(address, prefix)| {
            route.gateway.is_some_and(|gw| {
                RouteFacts {
                    interface: String::new(),
                    address: *address,
                    prefix: *prefix,
                    gateway: None,
                    gateway_hw: None,
                }
                .on_link(IpAddr::V4(gw))
            })
        })
        .or_else(|| all.first())
        .copied()?;
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

/// The route facts now, read from the kernel's tables with no packet, or the scripted ones
/// ([`scripted`]).
#[must_use]
pub fn routes_now() -> (Option<RouteFacts>, Option<String>) {
    if let Some(script) = scripted() {
        return (script.facts.route, script.facts.tunnel);
    }
    compose(
        &aterm_uds::network::default_routes(),
        &aterm_uds::network::interfaces().unwrap_or_default(),
    )
}

/// The facts a reader that runs no path monitor reads now: the routes ([`routes_now`]), and the
/// scripted path where the facts are scripted (else none: the record's last reading stands in).
#[must_use]
pub fn reader_facts() -> Facts {
    if let Some(script) = scripted() {
        return script.facts;
    }
    let (route, tunnel) = routes_now();
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
    let bytes: Option<Vec<u8>> = text
        .split(':')
        .map(|part| u8::from_str_radix(part, 16).ok())
        .collect();
    bytes.filter(|bytes| !bytes.is_empty() && bytes.len() <= 20)
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
    for line in text.lines().map(str::trim) {
        let words: Vec<&str> = line.split_whitespace().collect();
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

/// The scripted facts now: this thread's [`script_here`], else (a debug build outside this
/// crate's tests) the file [`SEAM`] names, read now; `None` in a release build, or with neither.
#[must_use]
pub fn scripted() -> Option<Script> {
    #[cfg(any(test, debug_assertions))]
    if let Some(script) = HERE.with(|here| here.borrow().clone()) {
        return Some(script);
    }
    #[cfg(all(debug_assertions, not(test)))]
    {
        let file = std::env::var_os(SEAM)?;
        std::fs::read_to_string(file)
            .ok()
            .map(|text| parse_script(&text))
    }
    #[cfg(not(all(debug_assertions, not(test))))]
    {
        None
    }
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
                use aterm_objc::path_monitor::{PathLink, PathStatus};
                let facts = PathFacts {
                    usable: match reading.status {
                        PathStatus::Satisfied | PathStatus::Satisfiable => Some(true),
                        PathStatus::Unsatisfied => Some(false),
                        PathStatus::Invalid => None,
                    },
                    expensive: reading.expensive,
                    constrained: reading.constrained,
                    link: match reading.link {
                        PathLink::Wifi => crate::place::Link::Wifi,
                        PathLink::Wired => crate::place::Link::Wired,
                        PathLink::Cellular => crate::place::Link::Cellular,
                        PathLink::Loopback => crate::place::Link::Loopback,
                        PathLink::Other => crate::place::Link::Other,
                    },
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
        let (route, tunnel) = routes_now();
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
