// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LAN act made in this process: the broker's own (it is root, so exempt), and every act off
//! macOS, where no Local Network privacy gates one. Nothing here asks the broker.

use std::io;
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs as _, UdpSocket};
use std::os::fd::{AsRawFd as _, RawFd};
use std::time::{Duration, Instant};

use super::{ConnectOptions, UdpOptions};
use crate::connect::Inet;

/// The longest bound a request may name: a broker thread is never held longer than this.
pub const MAX_WITHIN: Duration = Duration::from_secs(120);

#[cfg(target_vendor = "apple")]
mod opt {
    pub const IP_BOUND_IF: libc::c_int = 25;
    pub const IPV6_BOUND_IF: libc::c_int = 125;
    pub const IP_MULTICAST_IFINDEX: libc::c_int = 66;
    pub const IPV6_MULTICAST_IF: libc::c_int = 9;
    pub const MCAST_JOIN_GROUP: libc::c_int = 80;
}

#[cfg(not(target_vendor = "apple"))]
mod opt {
    pub const SO_BINDTOIFINDEX: libc::c_int = 62;
    pub const IP_MULTICAST_IF: libc::c_int = 32;
    pub const IPV6_MULTICAST_IF: libc::c_int = 17;
    pub const MCAST_JOIN_GROUP: libc::c_int = 42;
}

/// `struct group_req`: Darwin packs it to 4 bytes (`#pragma pack(4)`), so its group (a
/// `sockaddr_storage`'s 128 bytes) follows the interface at once.
#[cfg(target_vendor = "apple")]
#[repr(C)]
struct GroupReq {
    interface: u32,
    group: [u8; 128],
}

/// `struct group_req`: Linux aligns its `sockaddr_storage` to 8 bytes.
#[cfg(not(target_vendor = "apple"))]
#[repr(C)]
struct GroupReq {
    interface: u32,
    pad: u32,
    group: [u8; 128],
}

const _: () = assert!(size_of::<libc::sockaddr_storage>() == 128);

/// Linux's `struct ip_mreqn`, which its `IP_MULTICAST_IF` takes to name an interface by index.
#[cfg(not(target_vendor = "apple"))]
#[repr(C)]
struct MreqN {
    multiaddr: u32,
    address: u32,
    ifindex: libc::c_int,
}

/// `now + within`, refusing a zero `within` or one past the clock's range.
///
/// # Errors
/// `InvalidInput`.
pub fn deadline_in(within: Duration) -> io::Result<Instant> {
    Some(within)
        .filter(|within| !within.is_zero())
        .and_then(|within| Instant::now().checked_add(within))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid LAN bound"))
}

/// Set one socket option of `value`'s size.
fn set<T>(fd: RawFd, level: libc::c_int, name: libc::c_int, value: &T) -> io::Result<()> {
    // SAFETY: `value` is a live, initialized option of exactly the size passed.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            std::ptr::from_ref(value).cast(),
            libc::socklen_t::try_from(size_of::<T>()).unwrap_or(libc::socklen_t::MAX),
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Bind `fd` to `addr`.
fn bind(fd: RawFd, addr: SocketAddr) -> io::Result<()> {
    let address = Inet::new(addr);
    let (pointer, length) = address.parts();
    // SAFETY: `address` owns the initialized sockaddr for the whole call.
    if unsafe { libc::bind(fd, pointer, length) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Make every packet of `fd` leave by interface `index`.
fn bound_if(fd: RawFd, v4: bool, index: u32) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        let index = libc::c_int::try_from(index).unwrap_or(libc::c_int::MAX);
        if v4 {
            set(fd, libc::IPPROTO_IP, opt::IP_BOUND_IF, &index)
        } else {
            set(fd, libc::IPPROTO_IPV6, opt::IPV6_BOUND_IF, &index)
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = v4;
        let index = libc::c_int::try_from(index).unwrap_or(libc::c_int::MAX);
        set(fd, libc::SOL_SOCKET, opt::SO_BINDTOIFINDEX, &index)
    }
}

/// The unicast hop limit of what `fd` sends.
fn unicast_hops(fd: RawFd, v4: bool, hops: u8) -> io::Result<()> {
    let hops = libc::c_int::from(hops);
    if v4 {
        set(fd, libc::IPPROTO_IP, libc::IP_TTL, &hops)
    } else {
        set(fd, libc::IPPROTO_IPV6, libc::IPV6_UNICAST_HOPS, &hops)
    }
}

/// Apply `options` to the TCP socket `fd` of `addr`'s family, before its connect.
fn configure(fd: RawFd, addr: &SocketAddr, options: ConnectOptions) -> io::Result<()> {
    let v4 = addr.is_ipv4();
    if let Some(index) = options.bound_if {
        bound_if(fd, v4, index)?;
    }
    if let Some(hops) = options.hop_limit {
        unicast_hops(fd, v4, hops)?;
    }
    if let Some(source) = options.source {
        if source.is_ipv4() != v4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("source {source} is not of {addr}'s family"),
            ));
        }
        bind(fd, SocketAddr::new(source, 0))?;
    }
    Ok(())
}

/// Dial `addr` from this process by `deadline` with `options`: a blocking stream kept out of
/// children from its creation.
///
/// # Errors
/// An expired `deadline` (`TimedOut`), an option the kernel refuses, or the connect's error.
pub fn dial(
    addr: &SocketAddr,
    deadline: Instant,
    options: ConnectOptions,
) -> io::Result<TcpStream> {
    crate::connect::tcp_until(addr, deadline, |stream| {
        configure(stream.as_raw_fd(), addr, options)
    })
}

/// Dial each of `addrs` in turn until one connects or `deadline` passes.
///
/// # Errors
/// The last address's error (`NotFound` for none), or `TimedOut`.
pub fn dial_any(
    addrs: &[SocketAddr],
    deadline: Instant,
    options: ConnectOptions,
) -> io::Result<(TcpStream, SocketAddr)> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address to dial");
    for addr in addrs {
        match dial(addr, deadline, options) {
            Ok(stream) => return Ok((stream, *addr)),
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// The most resolutions running at once in this process: a resolver that never answers leaves
/// its thread behind until it does, and in the broker (root) those must not pile up.
pub const MAX_RESOLVING: usize = 32;

/// A bound on threads running at once.
struct Slots {
    live: std::sync::atomic::AtomicUsize,
    max: usize,
}

/// One running thread's place in its [`Slots`], given back when it ends.
struct Slot(&'static Slots);

impl Slots {
    const fn new(max: usize) -> Self {
        Self {
            live: std::sync::atomic::AtomicUsize::new(0),
            max,
        }
    }

    /// A place, or `None` with every one taken.
    fn take(&'static self) -> Option<Slot> {
        use std::sync::atomic::Ordering;
        self.live
            .try_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                (live < self.max).then_some(live + 1)
            })
            .ok()
            .map(|_| Slot(self))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0
            .live
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

static RESOLVING: Slots = Slots::new(MAX_RESOLVING);

/// Resolve `name` with the system resolver (`getaddrinfo`) within `within`, each address on
/// `port`. The resolution runs on its own thread, which a resolver that never answers leaves
/// behind until it does; at most [`MAX_RESOLVING`] run at once.
///
/// # Errors
/// `InvalidInput` for a zero `within`, `WouldBlock` (`EAGAIN`) with [`MAX_RESOLVING`] running,
/// `TimedOut`, or the resolver's error.
pub fn resolve(name: &str, port: u16, within: Duration) -> io::Result<Vec<SocketAddr>> {
    resolve_in(&RESOLVING, name, within, move |asked| {
        (asked, port).to_socket_addrs().map(Iterator::collect)
    })
}

/// [`resolve`] with `lookup` as the resolver, on a thread holding one of `slots`.
fn resolve_in(
    slots: &'static Slots,
    name: &str,
    within: Duration,
    lookup: impl FnOnce(&str) -> io::Result<Vec<SocketAddr>> + Send + 'static,
) -> io::Result<Vec<SocketAddr>> {
    let deadline = deadline_in(within)?;
    let slot = slots
        .take()
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EAGAIN))?;
    let (answer, heard) = std::sync::mpsc::sync_channel(1);
    let asked = name.to_owned();
    std::thread::Builder::new()
        .name("lan-resolve".into())
        .spawn(move || {
            let _slot = slot;
            let _ = answer.send(lookup(&asked));
        })?;
    match heard.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(resolved) => resolved,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("{name} did not resolve within {within:?}"),
        )),
    }
}

/// Join `group` on `fd` (of `group`'s family), on interface `index` or the kernel's choice (0).
fn join(fd: RawFd, group: IpAddr, index: Option<u32>) -> io::Result<()> {
    let address = Inet::new(SocketAddr::new(group, 0));
    let (pointer, length) = address.parts();
    let mut storage = [0u8; 128];
    let length = usize::try_from(length).unwrap_or(0).min(storage.len());
    // SAFETY: `pointer` is `length` initialized bytes of `address`, live for this read, and
    // `storage` has room for them.
    unsafe {
        std::ptr::copy_nonoverlapping(pointer.cast::<u8>(), storage.as_mut_ptr(), length);
    }
    let request = GroupReq {
        interface: index.unwrap_or(0),
        #[cfg(not(target_vendor = "apple"))]
        pad: 0,
        group: storage,
    };
    let level = if group.is_ipv4() {
        libc::IPPROTO_IP
    } else {
        libc::IPPROTO_IPV6
    };
    set(fd, level, opt::MCAST_JOIN_GROUP, &request)
}

/// The interface multicast that `fd` sends leaves by.
fn multicast_if(fd: RawFd, v4: bool, index: u32) -> io::Result<()> {
    if !v4 {
        return set(fd, libc::IPPROTO_IPV6, opt::IPV6_MULTICAST_IF, &index);
    }
    #[cfg(target_vendor = "apple")]
    {
        set(fd, libc::IPPROTO_IP, opt::IP_MULTICAST_IFINDEX, &index)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let request = MreqN {
            multiaddr: 0,
            address: 0,
            ifindex: libc::c_int::try_from(index).unwrap_or(libc::c_int::MAX),
        };
        set(fd, libc::IPPROTO_IP, opt::IP_MULTICAST_IF, &request)
    }
}

/// The mDNS port, which mDNSResponder shares (`SO_REUSEPORT`) with every other listener.
pub const MDNS_PORT: u16 = 5353;

/// The port of aterm's Commons discovery group (`aterm-db`'s transport, group 239.255.31.84):
/// aterm's own port, which its every process shares (`SO_REUSEPORT`).
pub const COMMONS_PORT: u16 = 41384;

/// Direct member solicitation, shared by aterm's one-hop responders (group 239.255.31.85).
pub const MEMBERS_PORT: u16 = 41385;

/// Refuse what the broker must not make for anyone: a bind to a named unicast address, or to any
/// port but the kernel's choice (0), [`MDNS_PORT`], [`COMMONS_PORT`] or [`MEMBERS_PORT`], or a group that is not
/// multicast or not of the socket's family. The broker is root, and XNU's `in_pcbbind` skips its
/// cross-user check for a root socket: one bound to a specific address could sit over another
/// user's wildcard binding of the port and take its traffic, and a wildcard one could share a port
/// with another user's specific-address `SO_REUSEPORT` socket and hear what is multicast to it. So
/// the ports are named: an ephemeral one, mDNS's (shared by every user's mDNS listener anyway), and
/// aterm's own Commons and direct member group ports. A passed socket of these is unconnected, and macOS lets the
/// blocked app both `send_to` and `recv_from` on it (measured, 2026-11-05), while the app's own
/// UDP socket can do neither.
///
/// # Errors
/// `PermissionDenied` for the address or the port, `InvalidInput` for a group.
pub fn check_udp(options: &UdpOptions) -> io::Result<()> {
    let at = options.bind.ip();
    if !at.is_unspecified() && !at.is_multicast() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{at} is a unicast address: the LAN broker binds UDP only to the wildcard address \
                 or a multicast group"
            ),
        ));
    }
    let port = options.bind.port();
    if ![0, MDNS_PORT, COMMONS_PORT, MEMBERS_PORT].contains(&port) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "port {port}: the LAN broker binds UDP only to the kernel's choice of port (0), \
                 mDNS's {MDNS_PORT}, aterm's Commons {COMMONS_PORT} or members {MEMBERS_PORT}"
            ),
        ));
    }
    if options.groups.len() > 64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "over 64 groups",
        ));
    }
    for (group, _) in &options.groups {
        if !group.is_multicast() || group.is_ipv4() != options.bind.is_ipv4() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{group} is no multicast group of {}'s family", options.bind),
            ));
        }
    }
    Ok(())
}

/// A UDP socket made here: bound as `options` says, its groups joined, kept out of children from
/// its creation and closed on `exec`.
///
/// # Errors
/// [`check_udp`]'s refusal, or the socket's, an option's, the bind's or a join's error.
pub fn udp(options: &UdpOptions) -> io::Result<UdpSocket> {
    check_udp(options)?;
    let v4 = options.bind.is_ipv4();
    let family = if v4 { libc::AF_INET } else { libc::AF_INET6 };
    let socket = UdpSocket::from(crate::connect::socket(family, libc::SOCK_DGRAM)?);
    let fd = socket.as_raw_fd();
    if options.reuse {
        let on: libc::c_int = 1;
        set(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, &on)?;
        set(fd, libc::SOL_SOCKET, libc::SO_REUSEPORT, &on)?;
    }
    bind(fd, options.bind)?;
    for (group, index) in &options.groups {
        join(fd, *group, *index)?;
    }
    if let Some(index) = options.multicast_if {
        multicast_if(fd, v4, index)?;
    }
    if let Some(ttl) = options.ttl {
        unicast_hops(fd, v4, ttl)?;
        if v4 {
            // Darwin's `IP_MULTICAST_TTL` takes a byte; Linux takes a byte or an int.
            set(fd, libc::IPPROTO_IP, libc::IP_MULTICAST_TTL, &ttl)?;
        } else {
            let hops = libc::c_int::from(ttl);
            set(fd, libc::IPPROTO_IPV6, libc::IPV6_MULTICAST_HOPS, &hops)?;
        }
    }
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    const MUST_HAPPEN: Duration = Duration::from_secs(60);

    #[test]
    fn a_dial_with_options_reaches_a_loopback_listener() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let options = ConnectOptions {
            bound_if: None,
            source: Some(std::net::Ipv4Addr::LOCALHOST.into()),
            hop_limit: Some(8),
        };
        let (mut client, reached) =
            dial_any(&[addr], Instant::now() + MUST_HAPPEN, options).unwrap();
        assert_eq!(reached, addr);
        let (mut server, _) = listener.accept().unwrap();
        server.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
        client.write_all(b"x").unwrap();
        let mut byte = [0];
        server.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"x");
        let wrong = ConnectOptions {
            source: Some(std::net::Ipv6Addr::LOCALHOST.into()),
            ..ConnectOptions::default()
        };
        assert_eq!(
            dial(&addr, Instant::now() + MUST_HAPPEN, wrong)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn a_udp_socket_binds_joins_and_refuses_a_named_address_another_port_or_a_unicast_group() {
        let options = UdpOptions {
            bind: "0.0.0.0:0".parse().unwrap(),
            reuse: true,
            groups: Vec::new(),
            multicast_if: None,
            ttl: Some(1),
        };
        let socket = udp(&options).unwrap();
        assert!(socket.local_addr().unwrap().port() != 0);
        for refused in ["0.0.0.0:53", "0.0.0.0:7484", "127.0.0.1:0", "[::1]:5353"] {
            let refused = UdpOptions {
                bind: refused.parse().unwrap(),
                ..options.clone()
            };
            assert_eq!(
                check_udp(&refused).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied,
                "{refused:?}"
            );
        }
        for allowed in [
            "0.0.0.0:5353",
            "224.0.0.251:5353",
            "0.0.0.0:41384",
            "239.255.31.84:41384",
            "0.0.0.0:41385",
            "239.255.31.85:41385",
            "[::]:0",
            "[ff02::fb]:5353",
        ] {
            let allowed = UdpOptions {
                bind: allowed.parse().unwrap(),
                ..options.clone()
            };
            check_udp(&allowed).unwrap();
        }
        let unicast = UdpOptions {
            groups: vec![("10.0.0.1".parse().unwrap(), None)],
            ..options
        };
        assert_eq!(
            udp(&unicast).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn resolutions_past_the_bound_are_refused_until_one_ends() {
        static SLOTS: Slots = Slots::new(2);
        let (release, held) = std::sync::mpsc::channel::<()>();
        let held = std::sync::Arc::new(std::sync::Mutex::new(held));
        let hang = || {
            let held = std::sync::Arc::clone(&held);
            move |_: &str| {
                let _ = held.lock().unwrap().recv();
                Ok(Vec::new())
            }
        };
        for _ in 0..2 {
            assert_eq!(
                resolve_in(&SLOTS, "hangs", Duration::from_millis(1), hang())
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::TimedOut
            );
        }
        let refused = resolve_in(&SLOTS, "third", MUST_HAPPEN, hang()).unwrap_err();
        assert_eq!(refused.raw_os_error(), Some(libc::EAGAIN), "{refused}");
        release.send(()).unwrap();
        release.send(()).unwrap();
        let answered = |_: &str| Ok(vec![SocketAddr::from(([127, 0, 0, 1], 7))]);
        let start = Instant::now();
        let resolved = loop {
            match resolve_in(&SLOTS, "fourth", MUST_HAPPEN, answered) {
                Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => {
                    assert!(
                        start.elapsed() < MUST_HAPPEN,
                        "the resolving slots never returned"
                    );
                    crate::deadline::poll_yield();
                }
                other => break other.unwrap(),
            }
        };
        assert_eq!(resolved, vec![SocketAddr::from(([127, 0, 0, 1], 7))]);
    }

    #[test]
    fn localhost_resolves_here_and_a_zero_bound_is_refused() {
        let addrs = resolve("localhost", 7, MUST_HAPPEN).unwrap();
        assert!(
            addrs
                .iter()
                .all(|addr| addr.ip().is_loopback() && addr.port() == 7)
        );
        assert!(!addrs.is_empty());
        assert_eq!(
            resolve("localhost", 7, Duration::ZERO).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
