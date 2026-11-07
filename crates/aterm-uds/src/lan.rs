// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's own LAN traffic, and on macOS the root LAN broker it goes through
//! (`docs/DESIGN-lan-broker-2026-11-05.md`).
//!
//! macOS asks "Allow “aterm” to find devices on local networks?" the first time aterm (or a
//! child aterm is responsible for) browses Bonjour, resolves a `.local` name or connects to a
//! local-network address, and gives it nothing until a person answers. A root launchd daemon is
//! exempt (`docs/measured/local-network-exemption-2026-11-05.md`), and a socket it makes keeps
//! working in the process it is passed to. So on macOS every outbound LAN act of aterm's asks the
//! broker, whose root-owned image setup's one elevation installs. The one exception is the
//! allowed direct path ([`direct`]): while no broker is installed, a process whose code macOS
//! already allowed on the local network, for this account and as the app macOS holds responsible
//! for it, serves the broker's protocol to itself, which raises no question since macOS asks only
//! code it holds no answer for; any other fallback would raise the question again. Below, "the
//! broker" is whichever serves its protocol: the root broker, or that direct path while none is
//! installed. Off macOS the same calls act in-process.

use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::time::Duration;

#[cfg(target_os = "macos")]
mod arrivals;
pub mod broker;
mod classify;
pub mod direct;
pub mod here;
pub mod wire;

#[cfg(target_os = "macos")]
pub use arrivals::Arrivals;
pub use broker::{BrokerHello, Events, Registration};
pub use classify::{is_local_name, is_local_network, on_attached_subnet};
pub use wire::{Advert, Event, Instance, Target};

pub mod install;

/// The broker's launchd label (a system LaunchDaemon, no `UserName`: it runs as root).
pub const LABEL: &str = "systems.alab.aterm-lan";

/// The broker's LaunchDaemon plist.
pub const PLIST_PATH: &str = "/Library/LaunchDaemons/systems.alab.aterm-lan.plist";

/// The root-owned folder holding the broker's image: a clone of the app bundle the elevation ran
/// from, never the person-owned bundle itself.
pub const IMAGE_DIR: &str = "/Library/Application Support/aterm/lan";

/// The broker's image: the bundle under [`IMAGE_DIR`].
pub const IMAGE_BUNDLE: &str = "/Library/Application Support/aterm/lan/aterm.app";

/// The executable launchd runs (`aterm lan serve --source <bundle>`).
pub const IMAGE_EXECUTABLE: &str =
    "/Library/Application Support/aterm/lan/aterm.app/Contents/MacOS/aterm";

/// Where the broker's log goes.
pub const LOG_PATH: &str = "/Library/Logs/aterm/aterm-lan.log";

/// The root-owned (0755) folder the broker makes its socket in.
pub const SOCKET_DIR: &str = "/var/run/systems.alab.aterm-lan";

/// The socket's name under [`SOCKET_DIR`] (mode 0666; clients are admitted by code signature).
pub const SOCKET_NAME: &str = "broker.sock";

/// The broker protocol's version: a client and a broker of different versions refuse each other
/// ([`Unavailable::Outdated`]).
pub const PROTOCOL: u32 = 1;

/// The broker's socket: [`SOCKET_DIR`]/[`SOCKET_NAME`], or [`SOCKET_NAME`] under a test binary's
/// own short root ([`crate::test_binary::short_root`], short enough for `sun_path`), where a test
/// serves its own broker, so no test reaches the person's.
#[must_use]
pub fn socket_path() -> PathBuf {
    match crate::test_binary::short_root() {
        Some(root) => root.join(SOCKET_NAME),
        None => PathBuf::from(SOCKET_DIR).join(SOCKET_NAME),
    }
}

/// Why aterm's own LAN traffic cannot go out on macOS: the broker is not there to ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// No broker answers its socket: setup's one elevation has not installed it (a standard user,
    /// or a Mac password nobody typed).
    NotInstalled,
    /// The broker speaks another protocol version (`ours`, `theirs`): it adopts a signed update by
    /// itself; an ad-hoc build's broker is renewed by that build's elevation.
    Outdated {
        /// [`PROTOCOL`].
        ours: u32,
        /// The broker's.
        theirs: u32,
    },
    /// Something at the socket is not the root broker (its peer is not uid 0), or it refused this
    /// process's code.
    Refused(String),
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => f.write_str(
                "the LAN broker is not installed: setup's Mac password installs it \
                 (`sudo aterm machines here apply --plain`)",
            ),
            Self::Outdated { ours, theirs } => write!(
                f,
                "the LAN broker speaks protocol {theirs} and this aterm {ours}: a signed update \
                 adopts itself within seconds; an ad-hoc build's broker is renewed by that \
                 build's Mac password"
            ),
            Self::Refused(why) => write!(f, "the LAN broker refused this process: {why}"),
        }
    }
}

impl std::error::Error for Unavailable {}

impl Unavailable {
    /// The broker's absence or refusal an [`std::io::Error`] made by [`LanError::into_io`]
    /// carries, so a caller above the `io` boundary can tell a LAN held for its broker from a
    /// dial that failed.
    #[must_use]
    pub fn of(error: &std::io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref::<Self>()
    }
}

/// Whether a LAN dial's `error` says it reached no peer at its address: refused, timed out, no
/// route, the host down (asleep or gone from the link: `EHOSTDOWN`, which std leaves
/// uncategorized), or no usable source address. A LAN this process may not use yet is
/// [`Unavailable::of`], not this.
#[must_use]
pub fn reached_no_peer(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(
        error.kind(),
        ErrorKind::ConnectionRefused
            | ErrorKind::TimedOut
            | ErrorKind::HostUnreachable
            | ErrorKind::NetworkUnreachable
            | ErrorKind::NetworkDown
            | ErrorKind::AddrNotAvailable
    ) || error.raw_os_error() == Some(libc::EHOSTDOWN)
}

/// Why a LAN act of aterm's did not happen: the broker is not there to ask, or the act failed.
#[derive(Debug)]
pub enum LanError {
    /// No broker to ask ([`Unavailable`]), and on macOS no allowed direct path ([`direct`]) to
    /// serve the act instead.
    Unavailable(Unavailable),
    /// The act itself failed (the broker's refusal of a destination, a connect, a Bonjour error),
    /// or the exchange with the broker did.
    Io(std::io::Error),
}

impl LanError {
    /// This error as an [`std::io::Error`]: [`LanError::Unavailable`] is `NotConnected`, its
    /// words naming the broker and what installs it, and it carries the [`Unavailable`] itself
    /// ([`Unavailable::of`]).
    #[must_use]
    pub fn into_io(self) -> std::io::Error {
        match self {
            Self::Unavailable(why) => std::io::Error::new(std::io::ErrorKind::NotConnected, why),
            Self::Io(error) => error,
        }
    }
}

impl std::fmt::Display for LanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(why) => why.fmt(f),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for LanError {}

impl From<Unavailable> for LanError {
    fn from(why: Unavailable) -> Self {
        Self::Unavailable(why)
    }
}

impl From<std::io::Error> for LanError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<LanError> for std::io::Error {
    fn from(error: LanError) -> Self {
        error.into_io()
    }
}

static BUILD_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The build this process names in its hello to the broker (and a broker's in its hello to each
/// client): the running binary's stamp (`<version>+g<commit>`, [`aterm_stamp::build_id`]), read
/// at run time on the first hello and kept from then on.
#[must_use]
pub fn hello_build() -> &'static str {
    BUILD_ID.get_or_init(aterm_stamp::build_id)
}

/// How a TCP connect is made beside its destination.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnectOptions {
    /// The interface (index) the connection must leave by (`IP_BOUND_IF`/`IPV6_BOUND_IF` on
    /// macOS, `SO_BINDTOIFINDEX` on Linux).
    pub bound_if: Option<u32>,
    /// The local address the connection leaves from (its port is the kernel's choice).
    pub source: Option<IpAddr>,
    /// The connection's hop limit (`IP_TTL`/`IPV6_UNICAST_HOPS`).
    pub hop_limit: Option<u8>,
}

/// A UDP socket to make: bound, with multicast groups joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UdpOptions {
    /// Where it binds: the wildcard address or a multicast group, on port 0 (the kernel's choice)
    /// or mDNS's 5353 ([`here::check_udp`]). The broker runs as root and binds nothing a person's
    /// process could not.
    pub bind: SocketAddr,
    /// `SO_REUSEADDR` and `SO_REUSEPORT`, so another socket can share the port (mDNS's 5353).
    pub reuse: bool,
    /// Multicast groups to join, each on an interface (index), or the kernel's choice.
    pub groups: Vec<(IpAddr, Option<u32>)>,
    /// The interface (index) multicast it sends leaves by.
    pub multicast_if: Option<u32>,
    /// The unicast and multicast hop limit of what it sends.
    pub ttl: Option<u8>,
}

/// Dial `target` on `port` within `within` and answer the connection and the address it reached.
/// A [`Target::Addr`]'s own port is replaced by `port`. A debug build's partition cut
/// ([`crate::partition::check`]) refuses a listed address before any socket is made, whether it
/// was named or a name resolved to it.
///
/// A name resolves first ([`resolve`]: on macOS a name [`is_local_name`] names is resolved by the
/// broker, as `.local` resolution is gated too), and each of its addresses is dialled in
/// turn. On macOS an address [`is_local_network`] classifies is dialled by the broker and the
/// connection passed to this process; every other address, and every one off macOS, is dialled
/// here.
///
/// # Errors
/// [`LanError::Unavailable`] when the broker is needed and not there, or [`LanError::Io`]: the
/// broker's refusal (a destination that is not local network), a failed resolution, the last
/// address's connect error (a cut one's `HostUnreachable`), or an expired `within` (`TimedOut`).
pub fn connect(
    target: &Target,
    port: u16,
    within: Duration,
    options: ConnectOptions,
) -> Result<(TcpStream, SocketAddr), LanError> {
    let deadline = here::deadline_in(within)?;
    let addrs = match target {
        Target::Addr(addr) => vec![with_port(*addr, port)],
        Target::Host(name) => resolve(name, within)?
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect(),
    };
    let mut last = None;
    for addr in addrs {
        if let Err(error) = crate::partition::check(addr) {
            last = Some(LanError::Io(error));
            continue;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let dialled = if cfg!(target_os = "macos") && is_local_network(&addr.ip()) {
            if remaining.is_zero() {
                Err(LanError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{addr} was not reached within {within:?}"),
                )))
            } else {
                broker::connect(&Target::Addr(addr), port, remaining, options)
            }
        } else {
            here::dial(&addr, deadline, options)
                .map(|stream| (stream, addr))
                .map_err(LanError::Io)
        };
        match dialled {
            Ok(connected) => return Ok(connected),
            Err(error @ LanError::Unavailable(_)) => return Err(error),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| {
        LanError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the name resolved to no address",
        ))
    }))
}

/// `addr` on `port`.
fn with_port(mut addr: SocketAddr, port: u16) -> SocketAddr {
    addr.set_port(port);
    addr
}

/// A bound UDP socket with its groups joined: on macOS made by the root LAN broker and passed to
/// this process, so what it sends to the LAN (the mDNS group) is not gated, or while none is
/// installed made in this process by the allowed direct path ([`direct`]), whose code macOS
/// already allowed; off macOS made here.
///
/// # Errors
/// [`LanError::Unavailable`] on macOS without a broker, or [`LanError::Io`]: a bind
/// [`here::check_udp`] refuses, a group that is not multicast, or the socket's own error.
pub fn udp(options: UdpOptions) -> Result<UdpSocket, LanError> {
    if cfg!(target_os = "macos") {
        broker::udp(options)
    } else {
        here::udp(&options).map_err(LanError::Io)
    }
}

/// The addresses `name` resolves to within `within`: on macOS a [`is_local_name`] name is
/// resolved by the broker (mDNSResponder applies the asking process's policy); every
/// other name, and every name off macOS, resolves here.
///
/// # Errors
/// [`LanError::Unavailable`] on macOS without a broker for a local name, or [`LanError::Io`]: the
/// resolver's error, or an expired `within` (`TimedOut`).
pub fn resolve(name: &str, within: Duration) -> Result<Vec<IpAddr>, LanError> {
    if cfg!(target_os = "macos") && is_local_name(name) {
        return broker::resolve(name, within);
    }
    let mut seen = Vec::new();
    for addr in here::resolve(name, 0, within)? {
        if !seen.contains(&addr.ip()) {
            seen.push(addr.ip());
        }
    }
    Ok(seen)
}

/// Bonjour's own calls exist on macOS alone.
fn bonjour_here() -> Result<(), LanError> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        Err(LanError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "aterm browses and advertises Bonjour on macOS only",
        )))
    }
}

/// Browse `service` (`_alab._tcp`) in `domain` (empty: the default ones), through the broker:
/// [`Event::Added`] and [`Event::Removed`] until the [`Events`] are dropped.
///
/// # Errors
/// [`LanError::Unavailable`] without a broker, or `Unsupported` off macOS.
pub fn browse(service: &str, domain: &str) -> Result<Events, LanError> {
    bonjour_here()?;
    broker::browse(service, domain)
}

/// Resolve the instance `name` of `service` in `domain`, through the broker: [`Event::Resolved`]
/// for each answer until the [`Events`] are dropped.
///
/// # Errors
/// As [`browse`].
pub fn resolve_service(name: &str, service: &str, domain: &str) -> Result<Events, LanError> {
    bonjour_here()?;
    broker::resolve_service(name, service, domain)
}

/// Query `name`'s records of `rrtype` (1 `A`, 28 `AAAA`, 16 `TXT`, …), through the broker:
/// [`Event::Record`] for each one added or removed until the [`Events`] are dropped.
///
/// # Errors
/// As [`browse`].
pub fn query(name: &str, rrtype: u16) -> Result<Events, LanError> {
    bonjour_here()?;
    broker::query(name, rrtype)
}

/// Advertise `advert` through the broker, once mDNSResponder has registered it: the advert
/// lasts as long as the [`Registration`], and ends when it is dropped (or this process dies).
///
/// # Errors
/// As [`browse`], or the registration's own error.
pub fn register(advert: &Advert) -> Result<Registration, LanError> {
    bonjour_here()?;
    broker::register(advert)
}

/// What a test may change about this thread's LAN classification and its broker.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seam {
    /// Loopback is local network, so a test's loopback listener exercises the broker path
    /// ([`is_local_network`]).
    pub loopback_is_local: bool,
    /// The protocol this thread's hellos name instead of [`PROTOCOL`].
    pub protocol: Option<u32>,
    /// The broker socket this thread's client dials instead of [`socket_path`].
    pub socket: Option<PathBuf>,
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static SEAM: std::cell::RefCell<Seam> = std::cell::RefCell::new(Seam::default());
}

/// `f` with this thread's LAN [`Seam`] set to `seam`, then restored: the test arm of the broker's
/// classification, its protocol and its socket. A broker serving under a seam carries it into
/// each thread it starts.
#[cfg(any(test, debug_assertions))]
pub fn seam_here<R>(seam: Seam, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Seam>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(was) = self.0.take() {
                SEAM.with(|here| here.replace(was));
            }
        }
    }
    let _restore = Restore(Some(SEAM.with(|here| here.replace(seam))));
    f()
}

/// This thread's LAN [`Seam`].
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn seam() -> Seam {
    SEAM.with(|here| here.borrow().clone())
}

/// The protocol this thread names: [`PROTOCOL`], or a test's [`Seam::protocol`].
pub(crate) fn protocol() -> u32 {
    #[cfg(any(test, debug_assertions))]
    if let Some(protocol) = SEAM.with(|here| here.borrow().protocol) {
        return protocol;
    }
    PROTOCOL
}

/// The socket this thread's client dials: [`socket_path`], or a test's [`Seam::socket`]. A watch
/// of a broker's arrival looks at it and its folder.
#[must_use]
pub fn dialled_socket() -> PathBuf {
    #[cfg(any(test, debug_assertions))]
    if let Some(socket) = SEAM.with(|here| here.borrow().socket.clone()) {
        return socket;
    }
    socket_path()
}

/// Whether this thread classifies loopback as local network ([`Seam::loopback_is_local`]).
pub(crate) fn loopback_is_local() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        SEAM.with(|here| here.borrow().loopback_is_local)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

#[cfg(test)]
mod tests;
