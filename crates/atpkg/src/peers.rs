// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! LAN PEER UNITS: a toolset archive taken from a LAN peer that already holds it, racing the
//! release host. A unit is named by the SHA-256 its signed index row carries, so a peer needs
//! no trust: its bytes are hashed as they arrive and land only when they match, and a peer that
//! sends other bytes is passed over while the release host's download goes on.
//!
//! Sharing is on by default (owner ruling 2026-10-27: "everything must be ON BY DEFAULT in
//! atpkg!!!"). A download races this machine's fleet members ([`set_fleet`]) unless a person
//! writes `[packages] peers` ([`unit_peers`]; `peers = []` takes from none), each peer held to
//! a pace ([`Pace`]) so a stalled or trickling one never holds a download. The process that
//! runs the fleet listener serves this store's kept units to those members ([`share`]), and
//! `aterm pkg serve-units` serves them in the foreground where none runs ([`serve`]); while a
//! server of the store is live, the store keeps a unit beside each build it installs
//! ([`keep`]). The person's off is `serve-units --off` ([`turn_off`]), or Settings ▸ Packages ▸
//! [`SERVE_SWITCH`] flipped off, which runs it: recorded in the store ([`Layout::units_off`]), it
//! stops every server, keeps no unit and starts no server, a foreground one included, until
//! `serve-units --on` ([`turn_on`]). Where serving stands is [`Serves`], read whole for the
//! doctor's row ([`sharing`]) and from the two store records alone for aterm's `status`
//! ([`serves`]). [`DEFAULT_PORT`] is one of the person's ports: a private aterm (a test, an
//! isolated instance) neither listens on nor dials it at any address ([`aterm_types::reach`]),
//! however a peer or a listen names it.

use std::io::{self, BufRead as _, Read, Write as _};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_types::reach::Reach;

use crate::store::Layout;

/// The port a peer named without one is reached on, and every server listens on by default: one
/// of the person's ports, which a private aterm neither listens on nor dials
/// ([`aterm_types::reach`]).
pub const DEFAULT_PORT: u16 = aterm_types::reach::TOOLSET_PORT;

/// [`SERVE_SWITCH`]'s words, spelled once for it and the lines that `concat!` them (the pattern
/// `adoption::UPDATES_SWITCH` uses): a macro, because `concat!` takes literals, not consts.
macro_rules! serve_switch {
    () => {
        "Share ALab tools with the fleet"
    };
}

/// The Settings ▸ Packages switch for LAN sharing: it shows the person's off
/// ([`turned_off`]) and, flipped, runs `aterm pkg serve-units --off` or `--on` ([`turn_off`],
/// [`turn_on`]).
pub const SERVE_SWITCH: &str = serve_switch!();

/// The most fleet members a download asks ([`fleet_peers`]), the most recently heard first.
const FLEET_PEERS_MAX: usize = 16;

/// How often a server looks at its stop flag and the person's off ([`share`], [`serve`]).
const ACCEPT_POLL: Duration = Duration::from_millis(250);

/// How long [`share`] waits to bind again while another server holds its port.
const BIND_RETRY: Duration = Duration::from_secs(30);

/// The free space under the prefix below which a store keeps no new unit ([`keep`]): the
/// doctor's disk row asks for as much.
const KEEP_FLOOR: u64 = 5 * 1024 * 1024 * 1024;

/// This machine's fleet as LAN sharing reads it ([`set_fleet`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fleet {
    /// How many fleet members a server of this store answers: each member's LAN address the
    /// fleet book holds, the set [`Share::members`] admits.
    pub members: usize,
    /// The members a download dials, each as `host:port`, the most recently heard first.
    pub dial: Vec<String>,
    /// Why `dial` leaves members out, when something other than the fleet book does (sharing
    /// off, this Mac off the ALab LAN), as a clause the doctor's row carries.
    pub held: Option<String>,
}

/// Where this machine's fleet is read ([`set_fleet`]).
static FLEET: OnceLock<fn() -> Fleet> = OnceLock::new();

/// Name the read of this machine's fleet ([`Fleet`]). The `aterm` binary sets it once as it
/// starts, since the fleet book is `aterm-net`'s and atpkg reads none itself; with none set, a
/// store's downloads race no peer unless `[packages] peers` names some. A second call is
/// ignored.
pub fn set_fleet(source: fn() -> Fleet) {
    let _ = FLEET.set(source);
}

#[cfg(test)]
thread_local! {
    static FLEET_FOR_TEST: std::cell::RefCell<Option<Fleet>> =
        const { std::cell::RefCell::new(None) };
}

/// `f` with `fleet` as this thread's fleet ([`fleet`]).
#[cfg(test)]
pub(crate) fn with_fleet<R>(fleet: Fleet, f: impl FnOnce() -> R) -> R {
    let was = FLEET_FOR_TEST.with(|held| held.replace(Some(fleet)));
    let ran = f();
    FLEET_FOR_TEST.with(|held| held.replace(was));
    ran
}

/// `f` with `peers` as this thread's fleet members, each one a download dials ([`fleet`]).
#[cfg(test)]
pub(crate) fn with_fleet_peers<R>(peers: &[&str], f: impl FnOnce() -> R) -> R {
    let fleet = Fleet {
        members: peers.len(),
        dial: peers.iter().map(ToString::to_string).collect(),
        held: None,
    };
    with_fleet(fleet, f)
}

/// This machine's fleet as [`set_fleet`]'s source reads it; none while no source is set.
fn fleet() -> Fleet {
    #[cfg(test)]
    let read = FLEET_FOR_TEST.with(|fleet| fleet.borrow().clone());
    #[cfg(not(test))]
    let read = FLEET.get().map(|source| source());
    read.unwrap_or_default()
}

/// This machine's fleet members as unit peers ([`Fleet::dial`]): each a peer [`configured`]
/// admits, once, at most [`FLEET_PEERS_MAX`].
pub(crate) fn fleet_peers() -> Vec<String> {
    dial_list(&fleet())
}

fn dial_list(fleet: &Fleet) -> Vec<String> {
    let mut peers: Vec<String> = Vec::new();
    for peer in configured(&fleet.dial) {
        if peers.len() < FLEET_PEERS_MAX && !peers.contains(&peer) {
            peers.push(peer);
        }
    }
    peers
}

/// The peers an archive download races: `written`, the `[packages] peers` a person wrote
/// ([`configured`]; written empty, none), else this machine's fleet members ([`fleet_peers`]).
pub(crate) fn unit_peers(written: Option<&[String]>) -> Vec<String> {
    match written {
        Some(peers) => configured(peers),
        None => fleet_peers(),
    }
}

/// The suffix of a peer download's partial beside its archive (`<asset>.peer.part`).
const PEER_PART_SUFFIX: &str = ".peer.part";

/// The suffix of a build's kept unit (`store/<program>/<build>.unit`, [`keep`]).
pub(crate) const UNIT_SUFFIX: &str = ".unit";

/// The request path a unit is served under: `/unit/<sha256>`.
const UNIT_PATH: &str = "/unit/";

/// How long a peer may take to accept the connection: a LAN peer answers at once or not at all.
#[cfg(not(test))]
const PEER_CONNECT: Duration = Duration::from_millis(750);
/// Unit tests: the hang detector, since every peer these tests dial is listening.
#[cfg(test)]
const PEER_CONNECT: Duration = aterm_spec::wait::MUST_HAPPEN;

/// No byte from a peer for this long ends its transfer, and the release host's goes on.
const PEER_IDLE: Duration = Duration::from_secs(2);

/// The connections a server answers at once.
const SERVE_CONNECTIONS: usize = 8;

/// The connections a server answers from one address at once: a quarter of
/// [`SERVE_CONNECTIONS`], the fleet listener's share (`aterm_net::fleet::per_source`), so one
/// address never holds every slot.
const SERVE_PER_SOURCE: usize = if SERVE_CONNECTIONS < 4 {
    1
} else {
    SERVE_CONNECTIONS / 4
};

/// The longest request or response head read.
const REQUEST_HEAD_MAX: u64 = 8 * 1024;

/// How fast one end of a unit's transfer must go, or the transfer ends: a stalled or trickling
/// peer holds no download for long, and a stalled or trickling client no server slot.
#[derive(Debug, Clone, Copy)]
struct Pace {
    /// The whole head (the response's, or the request's) arrives within this.
    head: Duration,
    /// Each [`Pace::step`] bytes of the body move within this of the step before.
    window: Duration,
    /// The bytes each [`Pace::window`] must move.
    step: u64,
    /// No byte for this long ends the transfer at once.
    idle: Duration,
}

/// A download's pace with a peer: the head within 5 s, then each MiB within 10 s (a floor of
/// about 100 KiB/s), and no byte for [`PEER_IDLE`] ends it.
const PEER_PACE: Pace = Pace {
    head: Duration::from_secs(5),
    window: Duration::from_secs(10),
    step: 1024 * 1024,
    idle: PEER_IDLE,
};

/// A server's pace with a client: the request head within 10 s, then each MiB taken within
/// 30 s, looser than [`PEER_PACE`] so a server never cuts a download its peer would keep.
const SERVE_PACE: Pace = Pace {
    head: Duration::from_secs(10),
    window: Duration::from_secs(30),
    step: 1024 * 1024,
    idle: Duration::from_secs(30),
};

/// A connection read and written against a deadline ([`Pace`]): each read or write waits at
/// most until the deadline and at most the pace's idle time, and one at or past the deadline
/// fails as timed out.
struct Paced<'a> {
    conn: &'a dyn UnitConn,
    until: Option<Instant>,
    idle: Duration,
}

impl<'a> Paced<'a> {
    fn new(conn: &'a dyn UnitConn, within: Duration, idle: Duration) -> Self {
        Self {
            conn,
            until: Instant::now().checked_add(within),
            idle,
        }
    }

    /// Move the deadline to `within` from now.
    fn renew(&mut self, within: Duration) {
        self.until = Instant::now().checked_add(within);
    }

    /// How long the next read or write may wait.
    fn wait(&self) -> io::Result<Duration> {
        let Some(until) = self.until else {
            return Ok(self.idle);
        };
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(left.min(self.idle))
    }
}

/// A socket's timeout (`EAGAIN`, which std names `WouldBlock`) as the deadline it is.
fn timed_out(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::WouldBlock {
        io::ErrorKind::TimedOut.into()
    } else {
        e
    }
}

impl Read for Paced<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.conn.set_read_timeout(Some(self.wait()?))?;
        self.conn.read_unit(buf).map_err(timed_out)
    }
}

impl io::Write for Paced<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.conn.set_write_timeout(Some(self.wait()?))?;
        self.conn.write_unit(buf).map_err(timed_out)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.conn.flush_unit()
    }
}

/// A connection a unit moves on: a TCP socket on [`DEFAULT_PORT`], or a stream of the mesh's
/// `Units` op (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.4), which `aterm` supplies
/// ([`set_units_dialer`]) since atpkg holds no TLS and reads no membership. Each call takes
/// `&self`, as a socket's do, so one reader and one writer share it.
pub trait UnitConn: Send + Sync {
    /// Read what has arrived, as [`Read::read`].
    /// # Errors
    /// The connection's own; a timeout as [`io::ErrorKind::WouldBlock`] or
    /// [`io::ErrorKind::TimedOut`].
    fn read_unit(&self, buf: &mut [u8]) -> io::Result<usize>;
    /// Write some of `buf`, as [`io::Write::write`].
    /// # Errors
    /// The connection's own.
    fn write_unit(&self, buf: &[u8]) -> io::Result<usize>;
    /// Flush what was written.
    /// # Errors
    /// The connection's own.
    fn flush_unit(&self) -> io::Result<()>;
    /// Bound each read (`None`: no bound).
    /// # Errors
    /// The connection's own.
    fn set_read_timeout(&self, within: Option<Duration>) -> io::Result<()>;
    /// Bound each write (`None`: no bound).
    /// # Errors
    /// The connection's own.
    fn set_write_timeout(&self, within: Option<Duration>) -> io::Result<()>;
}

impl UnitConn for TcpStream {
    fn read_unit(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut conn = self;
        conn.read(buf)
    }

    fn write_unit(&self, buf: &[u8]) -> io::Result<usize> {
        let mut conn = self;
        io::Write::write(&mut conn, buf)
    }

    fn flush_unit(&self) -> io::Result<()> {
        let mut conn = self;
        io::Write::flush(&mut conn)
    }

    fn set_read_timeout(&self, within: Option<Duration>) -> io::Result<()> {
        Self::set_read_timeout(self, within)
    }

    fn set_write_timeout(&self, within: Option<Duration>) -> io::Result<()> {
        Self::set_write_timeout(self, within)
    }
}

/// Opens a `Units` stream to the fleet member at an address a download dials, or `None` where
/// none opens (no member there, a member that does not speak the mesh): the download then dials
/// [`DEFAULT_PORT`] there, the fallback kept until the mesh is the only transport (§11 D3).
pub type UnitsDialer = fn(IpAddr) -> Option<Box<dyn UnitConn>>;

/// Where a download opens a `Units` stream first ([`set_units_dialer`]).
static UNITS: OnceLock<UnitsDialer> = OnceLock::new();

/// Name how a download opens a mesh `Units` stream to a member ([`UnitsDialer`]). The `aterm`
/// binary sets it once as it starts; with none set, each peer is dialed at its `host:port`
/// alone. A second call is ignored.
pub fn set_units_dialer(dialer: UnitsDialer) {
    let _ = UNITS.set(dialer);
}

#[cfg(test)]
thread_local! {
    static UNITS_FOR_TEST: std::cell::Cell<Option<UnitsDialer>> = const { std::cell::Cell::new(None) };
}

/// `f` with `dialer` as this thread's [`UnitsDialer`].
#[cfg(test)]
pub(crate) fn with_units_dialer<R>(dialer: UnitsDialer, f: impl FnOnce() -> R) -> R {
    let was = UNITS_FOR_TEST.with(|held| held.replace(Some(dialer)));
    let ran = f();
    UNITS_FOR_TEST.with(|held| held.set(was));
    ran
}

fn units_dialer() -> Option<UnitsDialer> {
    #[cfg(test)]
    return UNITS_FOR_TEST.with(std::cell::Cell::get);
    #[cfg(not(test))]
    UNITS.get().copied()
}

/// Answer one `Units` request on `conn`, a stream the mesh's listener already admitted, from the
/// units `layout` keeps: as a [`DEFAULT_PORT`] server answers one, at its pace. A store whose
/// person turned sharing off answers none (`404`).
/// # Errors
/// The connection's own, or a request slower than the pace.
pub fn answer_unit(conn: &dyn UnitConn, layout: &Layout) -> io::Result<()> {
    let units = units_dir(layout);
    answer(conn, (!turned_off(layout)).then_some(&*units), &SERVE_PACE)
}

/// The files an archive's bytes may arrive in while it downloads, in the order a reader looks:
/// the release host's partial (`<dl>.part`), a LAN peer's ([`race`]), and `dl` itself.
pub(crate) fn arrival_paths(dl: &Path) -> [PathBuf; 3] {
    [
        suffixed(dl, ".part"),
        suffixed(dl, PEER_PART_SUFFIX),
        dl.to_path_buf(),
    ]
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// `peers` (`[packages] peers`, or the fleet's), each as `host:port` (the port [`DEFAULT_PORT`]
/// when a peer names none); an entry that is not a bare host, IPv4 or `[IPv6]` address with an
/// optional port is dropped.
pub(crate) fn configured(peers: &[String]) -> Vec<String> {
    peers.iter().filter_map(|peer| peer_addr(peer)).collect()
}

fn peer_addr(peer: &str) -> Option<String> {
    let peer = peer.trim();
    let parse_port = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u16>().ok())
            .flatten()
    };
    let (host, port) = match peer.strip_prefix('[') {
        Some(rest) => {
            let (v6, tail) = rest.split_once(']')?;
            v6.parse::<std::net::Ipv6Addr>().ok()?;
            let port = match tail {
                "" => DEFAULT_PORT,
                _ => parse_port(tail.strip_prefix(':')?)?,
            };
            (format!("[{v6}]"), port)
        }
        None => {
            let (host, port) = match peer.rsplit_once(':') {
                Some((host, port)) => (host, parse_port(port)?),
                None => (peer, DEFAULT_PORT),
            };
            let host_ok = !host.is_empty()
                && host.len() <= 253
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
            if !host_ok {
                return None;
            }
            (host.to_string(), port)
        }
    };
    (port != 0).then(|| format!("{host}:{port}"))
}

/// Whether `sha256` names a unit: 64 lowercase hex digits.
fn is_unit_name(sha256: &str) -> bool {
    sha256.len() == 64
        && sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Fetch the unit `sha256` (`size` signed bytes) to `dest` from the release host and `peers` at
/// once. `cdn` is the release host's download, which gives up when its flag is set; the peers
/// are asked one after another for the unit, each transfer hashed as it lands in
/// `<dest>.peer.part`. Whichever source holds the whole unit first is `dest`: a peer's only
/// when it sent exactly `size` bytes that hash to `sha256`, and the other source is stopped.
/// With no peer that holds the unit, or a row that signs no size, this is `cdn` alone. Each
/// peer is held to [`PEER_PACE`], so once `cdn` has failed this waits on a peer only while its
/// transfer keeps that pace.
pub(crate) fn race(
    peers: &[String],
    sha256: &str,
    size: u64,
    dest: &Path,
    cdn: impl FnOnce(&Path, &AtomicBool) -> Result<(), String>,
) -> Result<(), String> {
    race_paced(peers, sha256, size, dest, cdn, &PEER_PACE)
}

/// [`race`] with each peer held to `pace`.
fn race_paced(
    peers: &[String],
    sha256: &str,
    size: u64,
    dest: &Path,
    cdn: impl FnOnce(&Path, &AtomicBool) -> Result<(), String>,
    pace: &Pace,
) -> Result<(), String> {
    let sha256 = sha256.to_ascii_lowercase();
    if peers.is_empty() || size == 0 || !is_unit_name(&sha256) {
        return cdn(dest, &AtomicBool::new(false));
    }
    let part = suffixed(dest, PEER_PART_SUFFIX);
    let cdn_stop = AtomicBool::new(false);
    let peer_stop = AtomicBool::new(false);
    // Read on this thread: where a download opens a mesh stream is this caller's.
    let units = units_dialer();
    std::thread::scope(|scope| {
        let from_peer = std::thread::Builder::new()
            .name("atpkg-peer-unit".into())
            .spawn_scoped(scope, || {
                let got = from_peers(peers, (&sha256, size), &part, &peer_stop, (pace, units));
                if got.is_some() {
                    cdn_stop.store(true, Ordering::Release);
                }
                got
            });
        let from_cdn = cdn(dest, &cdn_stop);
        if from_cdn.is_ok() {
            peer_stop.store(true, Ordering::Release);
        }
        let peer = from_peer.ok().and_then(|t| t.join().ok()).flatten();
        match (from_cdn, peer) {
            (Ok(()), _) => {
                let _ = std::fs::remove_file(&part);
                Ok(())
            }
            (Err(_), Some(peer)) => {
                std::fs::rename(&part, dest).map_err(|e| {
                    let _ = std::fs::remove_file(&part);
                    format!("keep the unit LAN peer {peer} sent: {e}")
                })?;
                println!(
                    "atpkg: took {sha256:.12}… from LAN peer {peer} (its bytes match the signed digest)"
                );
                Ok(())
            }
            (Err(e), None) => {
                let _ = std::fs::remove_file(&part);
                Err(e)
            }
        }
    })
}

/// Ask `peers` in turn for the unit until one sends the whole of it with the signed digest,
/// each held to `pace`: that peer, its bytes at `part`. `None` when none did, or `stop` was set
/// first.
fn from_peers(
    peers: &[String],
    (sha256, size): (&str, u64),
    part: &Path,
    stop: &AtomicBool,
    (pace, units): (&Pace, Option<UnitsDialer>),
) -> Option<String> {
    for peer in peers {
        if stop.load(Ordering::Acquire) {
            break;
        }
        match from_peer(peer, (sha256, size), part, stop, (pace, units)) {
            Ok(()) => return Some(peer.clone()),
            Err(PeerMiss::Other(got)) => {
                eprintln!(
                    "atpkg: LAN peer {peer} sent bytes that hash to {got:.12}…, not the signed \
                     unit {sha256:.12}…; passed over"
                );
            }
            Err(PeerMiss::Unavailable) => {}
        }
        let _ = std::fs::remove_file(part);
    }
    None
}

/// Why one peer's transfer did not land.
enum PeerMiss {
    /// No answer, no unit, a short or oversized body, a transfer slower than its [`Pace`], or
    /// a stop.
    Unavailable,
    /// The whole body arrived and hashes to this, not the signed digest.
    Other(String),
}

fn from_peer(
    peer: &str,
    (sha256, size): (&str, u64),
    part: &Path,
    stop: &AtomicBool,
    (pace, units): (&Pace, Option<UnitsDialer>),
) -> Result<(), PeerMiss> {
    let unavailable = |_| PeerMiss::Unavailable;
    let conn = connect(peer, units).map_err(unavailable)?;
    let mut paced = Paced::new(&*conn, pace.head, pace.idle);
    write!(
        paced,
        "GET {UNIT_PATH}{sha256} HTTP/1.1\r\nHost: {peer}\r\nConnection: close\r\n\r\n"
    )
    .map_err(unavailable)?;
    let mut body = io::BufReader::new(paced);
    let declared = response_length(&mut body).ok_or(PeerMiss::Unavailable)?;
    if declared != size {
        return Err(PeerMiss::Unavailable);
    }
    let _ = std::fs::remove_file(part);
    let mut file = crate::platform::open_create_new_write(part, 0o600).map_err(unavailable)?;
    let mut digest = crate::tree::BulkSha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    // The body keeps the pace: each step of it within a window of the step before.
    body.get_mut().renew(pace.window);
    let mut stepped = 0u64;
    while got < declared {
        if stop.load(Ordering::Acquire) {
            return Err(PeerMiss::Unavailable);
        }
        let want = usize::try_from(declared - got).map_or(buf.len(), |rest| rest.min(buf.len()));
        let n = match body.read(buf.get_mut(..want).ok_or(PeerMiss::Unavailable)?) {
            Ok(0) | Err(_) => return Err(PeerMiss::Unavailable),
            Ok(n) => n,
        };
        let bytes = buf.get(..n).ok_or(PeerMiss::Unavailable)?;
        got = got.saturating_add(n as u64);
        stepped = stepped.saturating_add(n as u64);
        if pace.step > 0 && stepped >= pace.step {
            stepped %= pace.step;
            body.get_mut().renew(pace.window);
        }
        digest.update(bytes);
        file.write_all(bytes).map_err(unavailable)?;
    }
    let got = digest.finalize_hex();
    if got.eq_ignore_ascii_case(sha256) {
        Ok(())
    } else {
        Err(PeerMiss::Other(got))
    }
}

/// A connection to `peer` (`host:port`): a mesh `Units` stream to the member at one of its
/// addresses ([`set_units_dialer`]), else a TCP one to the address itself, each tried within
/// [`PEER_CONNECT`] that a private aterm may dial ([`aterm_types::reach::check`]; a `Units`
/// stream dials the member's fleet listener, which the fleet's own guard judges).
///
/// # Errors
/// `peer` names no address, or the last address's refusal or failed connect.
fn connect(peer: &str, dialer: Option<UnitsDialer>) -> io::Result<Box<dyn UnitConn>> {
    connect_by(peer, dialer, |addr| {
        aterm_uds::connect_tcp(addr, PEER_CONNECT)
    })
}

/// [`connect`] with `dial` making each TCP connection, asked only of an address the rule lets
/// this process dial: a test hands it one that makes no socket, so a rule that let the person's
/// port through would fail it before any connect(2) reached the person's server.
///
/// # Errors
/// As [`connect`]'s, the last failure being `dial`'s.
fn connect_by(
    peer: &str,
    dialer: Option<UnitsDialer>,
    dial: impl Fn(&SocketAddr) -> io::Result<TcpStream>,
) -> io::Result<Box<dyn UnitConn>> {
    use std::net::ToSocketAddrs as _;
    let addrs: Vec<SocketAddr> = peer.to_socket_addrs()?.collect();
    if let Some(stream) = addrs
        .iter()
        .find_map(|addr| dialer.and_then(|units| units(addr.ip())))
    {
        return Ok(stream);
    }
    let mut last = None;
    for addr in &addrs {
        match aterm_types::reach::check(Reach::Dial(*addr)).and_then(|()| dial(addr)) {
            Ok(conn) => return Ok(Box::new(conn)),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("LAN peer {peer} has no address"),
        )
    }))
}

/// Read a response head from `conn`: the body's `Content-Length` when the status is 200 and
/// the head declares one, within [`REQUEST_HEAD_MAX`].
fn response_length(conn: &mut impl io::BufRead) -> Option<u64> {
    let mut head = conn.take(REQUEST_HEAD_MAX);
    let mut line = String::new();
    head.read_line(&mut line).ok()?;
    let ok = line.split_ascii_whitespace().nth(1) == Some("200");
    let mut length = None;
    loop {
        line.clear();
        if head.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            let value = value.trim();
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let parsed: u64 = value.parse().ok()?;
            if length.is_some_and(|prev| prev != parsed) {
                return None;
            }
            length = Some(parsed);
        }
    }
    ok.then_some(length).flatten()
}

/// `<prefix>/units`: the units this store keeps, each a link named by its SHA-256 to a build's
/// kept unit (`<build>.unit`). A server makes it once it listens ([`share`], [`serve`]).
fn units_dir(layout: &Layout) -> PathBuf {
    layout.prefix.join("units")
}

/// Whether the person turned LAN sharing off (`serve-units --off`, [`turn_off`]): one `lstat`
/// of [`Layout::units_off`].
#[must_use]
pub fn turned_off(layout: &Layout) -> bool {
    std::fs::symlink_metadata(layout.units_off()).is_ok()
}

/// When this store keeps units — a server of it is live ([`serving`]), [`units_dir`] is made
/// and the person has not turned sharing off ([`turned_off`]) — keep `dl`, the download a build
/// at `build_dir` was staged from, whose signed row names `sha256`, as that unit: linked beside
/// the build (`<build>.unit`, which goes when the build does) and under `units/`. So a store
/// with no server (a Mac that left its fleet, or whose port another program holds) keeps no
/// new unit that nothing would serve. The bytes are hashed first; a file that does not hash to
/// `sha256` is not kept, and neither is any unit while less than [`KEEP_FLOOR`] is free under
/// the prefix.
pub(crate) fn keep(layout: &Layout, dl: &Path, sha256: &str, build_dir: &Path) {
    let free = crate::freespace::available_bytes(&layout.prefix);
    keep_with_free(layout, dl, sha256, build_dir, free);
}

/// [`keep`] with the free space under the prefix as read (`None`: unread, which keeps).
fn keep_with_free(layout: &Layout, dl: &Path, sha256: &str, build_dir: &Path, free: Option<u64>) {
    let units = units_dir(layout);
    let sha256 = sha256.to_ascii_lowercase();
    if !is_unit_name(&sha256)
        || !std::fs::symlink_metadata(&units).is_ok_and(|m| m.is_dir())
        || turned_off(layout)
        || serving(layout).is_none()
    {
        return;
    }
    sweep(&units);
    if free.is_some_and(|free| free < KEEP_FLOOR) {
        return;
    }
    let named = units.join(&sha256);
    if std::fs::symlink_metadata(&named).is_ok() {
        return;
    }
    if !crate::tree::file_sha256(dl).is_ok_and(|got| got == sha256) {
        return;
    }
    let Some(beside) = crate::store::sidecar_path(build_dir, UNIT_SUFFIX) else {
        return;
    };
    let _ = std::fs::remove_file(&beside);
    if std::fs::hard_link(dl, &beside).is_ok() && std::fs::hard_link(&beside, &named).is_err() {
        let _ = std::fs::remove_file(&beside);
    }
}

/// Remove every kept unit no build holds any more: gc's sweep of `units/`, so a discarded
/// build's archive goes with it.
pub(crate) fn sweep_kept(layout: &Layout) {
    sweep(&units_dir(layout));
}

/// Remove every kept unit no build holds any more (`units/` entries with no other link).
fn sweep(units: &Path) {
    let Ok(entries) = std::fs::read_dir(units) else {
        return;
    };
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata()
            && (!meta.is_file() || links(&meta) < 2)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(unix)]
fn links(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    meta.nlink()
}

#[cfg(not(unix))]
fn links(_meta: &std::fs::Metadata) -> u64 {
    // No link count to read: a kept unit stands until `serve-units --off` sweeps it.
    2
}

/// The person's off (`serve-units --off`): record it ([`Layout::units_off`]), so every server
/// of this store stops within [`ACCEPT_POLL`] and none starts, then remove `units/` and every
/// build's kept unit, so the store keeps none. It stands until [`turn_on`]. A store not made
/// yet has its prefix made for the record ([`Layout::ensure_dir`]), so an off a person writes
/// before the first pass holds from that pass on.
///
/// # Errors
/// The prefix could not be made, the record could not be written, or the `units/` directory
/// could not be removed.
pub fn turn_off(layout: &Layout) -> io::Result<()> {
    let off = layout.units_off();
    if std::fs::symlink_metadata(&off).is_err() {
        if std::fs::symlink_metadata(&layout.prefix).is_ok_and(|m| !m.file_type().is_dir()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "prefix is not a real directory",
            ));
        }
        layout.ensure_dir(&layout.prefix)?;
        let mut record = crate::platform::open_create_write(&off, 0o600)?;
        writeln!(
            record,
            "# This machine shares no toolset archive on the LAN (`aterm pkg serve-units --off`).\n\
             # `aterm pkg serve-units --on` removes this record."
        )?;
        // A person's choice: it stands through a power loss.
        crate::platform::sync_durable(&record)?;
    }
    let units = units_dir(layout);
    if std::fs::symlink_metadata(&units).is_ok() {
        crate::store::remove_tree(&units)?;
    }
    let store = layout.prefix.join("store");
    if std::fs::symlink_metadata(&store).is_ok_and(|m| m.file_type().is_dir()) {
        for program in std::fs::read_dir(store).into_iter().flatten().flatten() {
            if !program.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            for entry in std::fs::read_dir(program.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                if !entry.file_type().is_ok_and(|t| t.is_file()) {
                    continue;
                }
                let name = entry.file_name();
                if name
                    .to_str()
                    .and_then(|n| n.strip_suffix(UNIT_SUFFIX))
                    .is_some_and(|build| crate::store::parse_build_name(build).is_some())
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
    Ok(())
}

/// Lift the person's off ([`turn_off`]): the process that runs the fleet listener serves this
/// store's units again within [`ACCEPT_POLL`], and the store keeps them from then on.
///
/// # Errors
/// The record could not be removed.
pub fn turn_on(layout: &Layout) -> io::Result<()> {
    match std::fs::remove_file(layout.units_off()) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Maximum bytes read from `<prefix>/units.serving`.
const MAX_UNITS_SERVING_BYTES: usize = 4096;

/// Where a server of this store listens now: the address [`Layout::units_serving`] records,
/// while the process that wrote it lives.
#[must_use]
pub fn serving(layout: &Layout) -> Option<SocketAddr> {
    let text = crate::metadata_io::read_bounded_regular_utf8(
        &layout.units_serving(),
        MAX_UNITS_SERVING_BYTES,
    )
    .ok()?;
    let (addr, pid) = serving_record(&text)?;
    crate::progress::pid_alive(pid).then_some(addr)
}

fn serving_record(text: &str) -> Option<(SocketAddr, u32)> {
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    let pid = field("pid")?;
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((field("addr")?.parse().ok()?, pid.parse().ok()?))
}

/// Record that this process serves this store's units on `addr` ([`serving`]).
fn record_serving(layout: &Layout, addr: SocketAddr) {
    static SERVING_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dest = layout.units_serving();
    let seq = SERVING_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dest.with_file_name(format!("units.serving.tmp-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let written = crate::platform::open_create_new_write(&tmp, 0o644)
        .and_then(|mut record| write!(record, "addr={addr}\npid={}\n", std::process::id()))
        .and_then(|()| std::fs::rename(&tmp, &dest));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Remove this process's record of serving, leaving another server's.
fn clear_serving(layout: &Layout) {
    let path = layout.units_serving();
    let ours = crate::metadata_io::read_bounded_regular_utf8(&path, MAX_UNITS_SERVING_BYTES)
        .ok()
        .and_then(|text| serving_record(&text))
        .is_some_and(|(_, pid)| pid == std::process::id());
    if ours {
        let _ = std::fs::remove_file(path);
    }
}

/// What LAN sharing reads as in a store, for the doctor's row ([`sharing`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sharing {
    /// The person turned it off (`serve-units --off`).
    pub off: bool,
    /// Where a live server of this store listens ([`serving`]).
    pub serving: Option<SocketAddr>,
    /// How many units the store keeps, and their bytes.
    pub kept: (usize, u64),
    /// This machine's fleet members a server answers ([`Fleet::members`]).
    pub members: usize,
    /// The members its downloads dial ([`fleet_peers`]).
    pub dials: usize,
    /// Why its downloads dial fewer members than it has, when something says ([`Fleet::held`]).
    pub held: Option<String>,
    /// `[packages] peers` as a person wrote it; `None` takes from the fleet members.
    pub written: Option<usize>,
}

/// Where the doctor's row says this Mac's serving stands ([`Sharing::serves`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Serves {
    /// The person's off stands.
    Off,
    /// A server of this store listens there, answering this Mac's fleet members when it has
    /// some (a foreground `serve-units` answers any address).
    Serving(SocketAddr),
    /// On, with fleet members, and no server live now: aterm serves while it runs.
    Idle,
    /// On, with no fleet member to serve.
    NoMember,
}

impl Serves {
    /// Where serving stands: the person's off first, then a live server (where it listens),
    /// then whether this Mac has `members` to serve. The doctor's row ([`Sharing::serves`]) and
    /// `status` ([`serves`]) both decide by it.
    #[must_use]
    pub const fn of(off: bool, serving: Option<SocketAddr>, members: usize) -> Self {
        match (off, serving) {
            (true, _) => Self::Off,
            (false, Some(at)) => Self::Serving(at),
            (false, None) if members > 0 => Self::Idle,
            (false, None) => Self::NoMember,
        }
    }
}

/// Where serving stands in `layout`'s store for a Mac with `members` fleet members to serve
/// ([`Serves::of`]), read from the store's two records alone: the person's off
/// ([`turned_off`], one `lstat`) and a live server's ([`serving`], one small read and a liveness
/// check of the pid it names). It counts no kept unit and reads no fleet book, so aterm's
/// `status` can say it (`units=`) where [`sharing`] would cost a directory walk.
#[must_use]
pub fn serves(layout: &Layout, members: usize) -> Serves {
    Serves::of(turned_off(layout), serving(layout), members)
}

/// Whom the doctor's row says this Mac's downloads race ([`Sharing::takes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Takes {
    /// `[packages] peers` names these many peers (none: `peers = []`).
    Written(usize),
    /// These many fleet members.
    Fleet(usize),
    /// None of its fleet members, held back ([`Sharing::held`]).
    Held,
    /// No peer: the release host alone.
    ReleaseHost,
}

/// LAN sharing in `layout`'s store, `written` the `[packages] peers` a person wrote.
#[must_use]
pub fn sharing(layout: &Layout, written: Option<&[String]>) -> Sharing {
    let mut kept = (0usize, 0u64);
    for entry in std::fs::read_dir(units_dir(layout))
        .into_iter()
        .flatten()
        .flatten()
    {
        if entry.file_type().is_ok_and(|t| t.is_file())
            && let Ok(meta) = entry.metadata()
            && links(&meta) >= 2
        {
            kept = (kept.0.saturating_add(1), kept.1.saturating_add(meta.len()));
        }
    }
    let fleet = fleet();
    Sharing {
        off: turned_off(layout),
        serving: serving(layout),
        kept,
        members: fleet.members,
        dials: dial_list(&fleet).len(),
        held: fleet.held,
        written: written.map(|peers| configured(peers).len()),
    }
}

impl Sharing {
    /// Where serving stands: the person's off first, then a live server, then whether this Mac
    /// has members to serve ([`Serves::of`]).
    #[must_use]
    pub const fn serves(&self) -> Serves {
        Serves::of(self.off, self.serving, self.members)
    }

    /// Whom downloads race: a written `[packages] peers` first, then the members dialed, then
    /// members none of whom is dialed.
    #[must_use]
    pub fn takes(&self) -> Takes {
        match self.written {
            Some(n) => Takes::Written(n),
            None if self.dials > 0 => Takes::Fleet(self.dials),
            None if self.members > 0 => Takes::Held,
            None => Takes::ReleaseHost,
        }
    }

    /// The doctor's line: whether this Mac serves its toolset archives, where and to whom, what
    /// it keeps, and whom its downloads race.
    #[must_use]
    pub fn line(&self) -> String {
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let kept = format!(
            "{} ({})",
            plural(self.kept.0, "toolset archive", "toolset archives"),
            crate::cost::human_bytes(self.kept.1)
        );
        let members = plural(self.members, "fleet member", "fleet members");
        let serves = match self.serves() {
            Serves::Off => concat!(
                "off (Settings ▸ Packages ▸ \"",
                serve_switch!(),
                "\" or `aterm pkg serve-units --off`): this Mac keeps and serves no toolset \
                 archive; that switch or `aterm pkg serve-units --on` turns it back on"
            )
            .to_owned(),
            Serves::Serving(at) if self.members > 0 => {
                format!("serving {kept} on {at} to this Mac's {members}")
            }
            Serves::Serving(at) => format!("serving {kept} on {at}"),
            Serves::Idle => {
                format!("on: keeps {kept} for this Mac's {members}, served while aterm runs")
            }
            Serves::NoMember if self.kept.0 > 0 => format!(
                "on, with no fleet member to share with (`aterm fleet join` shares toolset \
                 downloads with a fleet); it still keeps {kept}, which go with their builds"
            ),
            Serves::NoMember => "on, with no fleet member to share with (`aterm fleet join` \
                                 shares toolset downloads with a fleet)"
                .to_owned(),
        };
        let takes = match self.takes() {
            Takes::Written(0) => "downloads race no LAN peer (`[packages] peers = []`)".to_owned(),
            Takes::Written(n) => format!(
                "downloads race the {} `[packages] peers` names",
                plural(n, "LAN peer", "LAN peers")
            ),
            Takes::Fleet(n) => format!(
                "downloads race {} of its {members}",
                plural(n, "fleet member", "fleet members")
            ),
            Takes::Held => match &self.held {
                Some(why) => format!("downloads race none of its {members} yet: {why}"),
                None => format!("downloads race none of its {members}"),
            },
            Takes::ReleaseHost => "downloads come from the release host alone".to_owned(),
        };
        format!("LAN sharing {serves}; {takes}")
    }
}

/// How [`share`] serves: where, until when, and to whom.
pub struct Share<'a> {
    /// The address to listen on.
    pub listen: SocketAddr,
    /// Cleared to stop: the listener closes within [`ACCEPT_POLL`].
    pub running: &'a AtomicBool,
    /// The addresses whose connections are answered (this machine's fleet members'); a
    /// connection from any other is closed unread. Read at most once every
    /// [`Share::refresh`], so a host that connects in a loop costs a lookup, not a read.
    pub members: &'a dyn Fn() -> Vec<IpAddr>,
    /// How long one read of [`Share::members`] stands.
    pub refresh: Duration,
    /// Hears each change in where sharing stands, as one line.
    pub note: &'a dyn Fn(&str),
}

/// Serve this store's kept units while `how.running` holds: the default-on server of the
/// process that runs the fleet listener (owner ruling 2026-10-27). While the person's off
/// stands ([`turn_off`]) nothing listens. Otherwise `how.listen` is bound, again every
/// [`BIND_RETRY`] while another server holds it, `units/` is made and the bound address is
/// recorded ([`serving`]) until it closes; the store keeps each unit it installs while that
/// record stands ([`keep`]). `GET /unit/<sha256>` from an address `how.members` names answers a
/// kept unit's bytes, and any other request is answered 404 or 405 and closed: nothing lists
/// what the store keeps. At most [`SERVE_CONNECTIONS`] are answered at once,
/// [`SERVE_PER_SOURCE`] from one address, each held to [`SERVE_PACE`].
pub fn share(layout: &Layout, how: &Share<'_>) {
    let mut said = String::new();
    let mut say = |line: String| {
        if line != said {
            (how.note)(&line);
            said = line;
        }
    };
    let mut known: Option<(Instant, Vec<IpAddr>)> = None;
    let mut admits = |ip: IpAddr| {
        let stale = known
            .as_ref()
            .is_none_or(|(read, _)| read.elapsed() >= how.refresh);
        if stale {
            let members = (how.members)()
                .into_iter()
                .map(|member| member.to_canonical())
                .collect();
            known = Some((Instant::now(), members));
        }
        known
            .as_ref()
            .is_some_and(|(_, members)| members.contains(&ip))
    };
    while how.running.load(Ordering::Acquire) {
        if turned_off(layout) {
            say("LAN toolset sharing off (`aterm pkg serve-units --off`)".to_owned());
            std::thread::sleep(ACCEPT_POLL);
            continue;
        }
        let listener = match open(layout, how.listen) {
            Ok(listener) => listener,
            Err(e) => {
                say(format!(
                    "LAN toolset sharing not listening on {}: {e}; tried again every {}s",
                    how.listen,
                    BIND_RETRY.as_secs()
                ));
                let mut waited = Duration::ZERO;
                while waited < BIND_RETRY && how.running.load(Ordering::Acquire) {
                    std::thread::sleep(ACCEPT_POLL);
                    waited += ACCEPT_POLL;
                }
                continue;
            }
        };
        let at = listener.local_addr().unwrap_or(how.listen);
        record_serving(layout, at);
        say(format!(
            "LAN toolset sharing: serving this Mac's toolset archives to its fleet members on {at}"
        ));
        answer_until(&listener, layout, how.running, &mut admits);
        drop(listener);
        clear_serving(layout);
    }
}

/// Serve this store's kept units on `listen` in the foreground (`aterm pkg serve-units`), to
/// any address, until the person turns sharing off. Otherwise as [`share`]; `ready` hears the
/// bound address. While the person's off stands this serves nothing and leaves it standing:
/// only [`turn_on`] lifts it.
///
/// # Errors
/// The person's off stands, `listen` is one of the person's ports and this is a private aterm
/// ([`aterm_types::reach::check`]), `listen` could not be bound, or `units/` could not be made.
pub fn serve(
    layout: &Layout,
    listen: SocketAddr,
    ready: impl FnOnce(SocketAddr),
) -> io::Result<()> {
    if turned_off(layout) {
        return Err(io::Error::other(
            "LAN sharing is off (`aterm pkg serve-units --off`); `aterm pkg serve-units --on` \
             turns it back on",
        ));
    }
    let listener = open(layout, listen)?;
    let at = listener.local_addr()?;
    record_serving(layout, at);
    ready(at);
    answer_until(&listener, layout, &AtomicBool::new(true), &mut |_| true);
    clear_serving(layout);
    Ok(())
}

/// Bind `listen`, non-blocking and kept out of every child, then make `units/` (sweeping what
/// no build holds): a store whose bind fails makes nothing. A store not made yet is left to
/// the pass that makes it, and a private aterm binds none of the person's ports
/// ([`aterm_types::reach::check`]).
fn open(layout: &Layout, listen: SocketAddr) -> io::Result<TcpListener> {
    aterm_types::reach::check(Reach::Listen(listen))?;
    if !layout.prefix.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no toolset store at {} yet", layout.prefix.display()),
        ));
    }
    let listener = aterm_uds::kept_out(|| TcpListener::bind(listen))?;
    listener.set_nonblocking(true)?;
    let units = units_dir(layout);
    crate::platform::make_dirs_born(&units, 0o755)?;
    sweep(&units);
    Ok(listener)
}

/// The connections a server answers now, by source address ([`SERVE_CONNECTIONS`],
/// [`SERVE_PER_SOURCE`]).
#[derive(Debug, Default)]
struct Slots(Mutex<Vec<IpAddr>>);

impl Slots {
    /// Take a slot for a connection from `ip`: none while every slot is held, or `ip` holds
    /// its share.
    fn take(&self, ip: IpAddr) -> bool {
        let mut held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let from_ip = held.iter().filter(|other| **other == ip).count();
        if held.len() >= SERVE_CONNECTIONS || from_ip >= SERVE_PER_SOURCE {
            return false;
        }
        held.push(ip);
        true
    }

    /// Give back a slot `ip` took.
    fn give(&self, ip: IpAddr) {
        let mut held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(at) = held.iter().position(|other| *other == ip) {
            held.swap_remove(at);
        }
    }
}

/// Answer `listener`'s connections from addresses `admits` takes until `running` is cleared or
/// the person turns sharing off, as [`Slots`] allow.
fn answer_until(
    listener: &TcpListener,
    layout: &Layout,
    running: &AtomicBool,
    admits: &mut dyn FnMut(IpAddr) -> bool,
) {
    let units = units_dir(layout);
    let slots = Arc::new(Slots::default());
    while running.load(Ordering::Acquire) && !turned_off(layout) {
        let (conn, from) = match aterm_uds::kept_out(|| listener.accept()) {
            Ok(accepted) => accepted,
            Err(_) => {
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
        };
        let ip = from.ip().to_canonical();
        // An accepted socket takes the listener's non-blocking mode on BSD and macOS.
        if !admits(ip) || conn.set_nonblocking(false).is_err() || !slots.take(ip) {
            continue;
        }
        let (dir, held) = (units.clone(), Arc::clone(&slots));
        let spawned = std::thread::Builder::new()
            .name("atpkg-serve-unit".into())
            .spawn(move || {
                let _ = answer(&conn, Some(&dir), &SERVE_PACE);
                // The slot is free before the client hears the close.
                held.give(ip);
                drop(conn);
            });
        if spawned.is_err() {
            slots.give(ip);
        }
    }
}

/// Answer one connection's request ([`answer_until`]), held to `pace`.
/// `units` is `None` for a store that serves none now: each request is read and answered `404`.
fn answer(conn: &dyn UnitConn, units: Option<&Path>, pace: &Pace) -> io::Result<()> {
    let mut reader =
        io::BufReader::new(Paced::new(conn, pace.head, pace.idle).take(REQUEST_HEAD_MAX));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_ascii_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    // The rest of the head is read and ignored: the request line says everything.
    // A head that ends on EOF or exceeds REQUEST_HEAD_MAX before its blank line is refused.
    let mut header = String::new();
    loop {
        header.clear();
        if reader.read_line(&mut header)? == 0 {
            return Ok(());
        }
        if header.trim_end().is_empty() {
            break;
        }
    }
    let mut out = Paced::new(conn, pace.window, pace.idle);
    if method != "GET" {
        return status(&mut out, "405 Method Not Allowed");
    }
    let (Some(units), Some(sha256)) = (
        units,
        target.strip_prefix(UNIT_PATH).filter(|s| is_unit_name(s)),
    ) else {
        return status(&mut out, "404 Not Found");
    };
    let path = units.join(sha256);
    let mut opened = crate::metadata_io::open_regular_with_meta(&path)
        .ok()
        .and_then(|(file, meta)| {
            if links(&meta) >= 2 {
                Some((file, meta.len()))
            } else {
                // A unit no build holds any more is swept as it is missed.
                let _ = std::fs::remove_file(&path);
                None
            }
        });
    let Some((file, len)) = opened.as_mut() else {
        return status(&mut out, "404 Not Found");
    };
    write!(
        out,
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {len}\r\n\
         Connection: close\r\n\r\n",
    )?;
    // The body keeps the pace: each step of it taken within a window of the step before.
    let mut limited = file.take(*len);
    let mut buf = vec![0u8; 256 * 1024];
    let mut stepped = 0u64;
    loop {
        let n = limited.read(&mut buf)?;
        let Some(bytes) = buf.get(..n).filter(|bytes| !bytes.is_empty()) else {
            break;
        };
        out.write_all(bytes)?;
        stepped = stepped.saturating_add(n as u64);
        if pace.step > 0 && stepped >= pace.step {
            stepped %= pace.step;
            out.renew(pace.window);
        }
    }
    out.flush()
}

fn status(out: &mut Paced<'_>, line: &str) -> io::Result<()> {
    write!(
        out,
        "HTTP/1.1 {line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    out.flush()
}

#[cfg(test)]
#[path = "peers_tests.rs"]
mod tests;
