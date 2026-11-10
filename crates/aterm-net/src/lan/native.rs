// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Route, refuse};
use std::{
    ffi::CString,
    io,
    net::{SocketAddr, TcpListener, TcpStream},
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

mod listen;
pub(super) use listen::Prepared;
pub(super) mod multicast;
mod tcp;

#[cfg(any(target_os = "macos", test))]
mod darwin;
#[cfg(any(target_os = "linux", test))]
mod linux;

pub(super) fn route(
    local: Option<SocketAddr>,
    peer: SocketAddr,
    bound: Option<u32>,
    until: Instant,
) -> io::Result<Route> {
    route_at(local, peer, bound, until, Instant::now())
}

fn route_at(
    local: Option<SocketAddr>,
    peer: SocketAddr,
    bound: Option<u32>,
    until: Instant,
    started: Instant,
) -> io::Result<Route> {
    let until = until.min(deadline_at(started));
    within(until)?;
    #[cfg(target_os = "macos")]
    {
        darwin::lookup(local, peer, bound, until)
    }
    #[cfg(target_os = "linux")]
    {
        linux::lookup(local, peer, bound, until)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (local, peer, bound, until);
        Err(refuse(
            "LAN route inspection is unavailable on this platform",
        ))
    }
}

pub(super) fn index(name: &str) -> io::Result<u32> {
    let name = CString::new(name).map_err(|_| refuse("invalid interface name"))?;
    // SAFETY: name is a live nul-terminated string for this call.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    if index == 0 {
        Err(refuse("interface disappeared during route inspection"))
    } else {
        Ok(index)
    }
}

/// The kernel's interface generation ([`darwin::interface_generation`]): it moves whenever an
/// interface or address may have changed. `None` where this OS gives none, so every read of the
/// attachments asks the OS again.
pub(super) fn interface_generation() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        darwin::interface_generation()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// One listener's route and attachment notifications. This socket never sends a query or
/// changes a route: readiness only asks the service to recheck its existing authority.
pub(super) struct RouteChanges {
    socket: Socket,
    room: usize,
}

impl RouteChanges {
    pub(super) fn open() -> io::Result<Self> {
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "LAN route notifications are unavailable on this platform",
            ))
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            #[cfg(target_os = "macos")]
            let socket = Socket::new(libc::PF_ROUTE, 0)?;
            #[cfg(target_os = "linux")]
            let socket = {
                let socket = Socket::new(libc::AF_NETLINK, libc::NETLINK_ROUTE)?;
                // SAFETY: zero initializes sockaddr_nl, including its padding.
                let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
                address.nl_family = libc::AF_NETLINK as _;
                // Linux's rtnetlink UAPI groups: LINK, IPv4/IPv6 IFADDR and ROUTE, and both
                // RULE groups (RTNLGRP_IPV6_RULE is group 19). No unicast lookup replies.
                address.nl_groups = 0x1 | 0x10 | 0x40 | 0x80 | 0x100 | 0x400 | (1 << 18);
                // SAFETY: address is readable for this exact size and socket owns its descriptor.
                if unsafe {
                    libc::bind(
                        socket.0.as_raw_fd(),
                        std::ptr::from_ref(&address).cast(),
                        std::mem::size_of_val(&address) as _,
                    )
                } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                socket
            };
            let _ = set_option(&socket.0, libc::SOL_SOCKET, libc::SO_RCVBUF, 64 * 1024);
            let room = usize::try_from(option(&socket.0, libc::SOL_SOCKET, libc::SO_RCVBUF)?)
                .ok()
                .filter(|room| *room > 0)
                .ok_or_else(|| refuse("invalid route notification receive room"))?;
            Ok(Self { socket, room })
        }
    }

    pub(super) fn fd(&self) -> BorrowedFd<'_> {
        self.socket.0.as_fd()
    }

    /// Consume at most 4096 datagrams. Exhaustion, closed sockets and failed reads cannot
    /// leave an idle service relying on a notification source that no longer works.
    pub(super) fn drain(&self) -> io::Result<bool> {
        let mut buffer = vec![0_u8; MAX_REPLY];
        let mut changed = false;
        let mut queued = 0_usize;
        for _ in 0..4096 {
            #[cfg(target_os = "linux")]
            // SAFETY: zero initializes a writable sender address, including its padding.
            let mut sender: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
            #[cfg(target_os = "linux")]
            let mut length = std::mem::size_of_val(&sender) as libc::socklen_t;
            // SAFETY: buffer is writable for its full size on this owned, nonblocking socket;
            // Linux also supplies a live writable sender address and exact length.
            let received = unsafe {
                #[cfg(target_os = "linux")]
                {
                    libc::recvfrom(
                        self.socket.0.as_raw_fd(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                        libc::MSG_DONTWAIT | libc::MSG_TRUNC,
                        std::ptr::from_mut(&mut sender).cast(),
                        &raw mut length,
                    )
                }
                #[cfg(not(target_os = "linux"))]
                {
                    libc::recv(
                        self.socket.0.as_raw_fd(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                        libc::MSG_DONTWAIT,
                    )
                }
            };
            let received = match usize::try_from(received) {
                Ok(0) => return Err(refuse("route notification socket closed")),
                Ok(received) if received <= buffer.len() => received,
                Ok(_) => return Err(refuse("route notification was truncated")),
                Err(_) => {
                    let error = io::Error::last_os_error();
                    match error.kind() {
                        io::ErrorKind::WouldBlock => {
                            // Darwin may silently drop a message when this room fills.
                            return Ok(changed || queued.saturating_mul(2) >= self.room);
                        }
                        io::ErrorKind::Interrupted => continue,
                        _ => return Err(error),
                    }
                }
            };
            #[cfg(target_os = "linux")]
            if length as usize != std::mem::size_of_val(&sender)
                || sender.nl_family != libc::AF_NETLINK as _
                || sender.nl_pid != 0
            {
                return Err(refuse("route notification is not from the kernel"));
            }
            queued = queued.saturating_add(received);
            #[cfg(target_os = "macos")]
            {
                changed |= darwin::route_changed(&buffer[..received])?;
            }
            #[cfg(target_os = "linux")]
            {
                changed |= linux::route_changed(&buffer[..received])?;
            }
        }
        Err(refuse("route notification drain exceeded its bound"))
    }
}

pub(super) fn physical(name: &str) -> io::Result<bool> {
    #[cfg(target_os = "macos")]
    {
        darwin::physical(name)
    }
    #[cfg(target_os = "linux")]
    {
        linux::physical(name)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = name;
        Ok(false)
    }
}

pub(super) trait LocalSocket: AsRawFd {
    fn address(&self) -> io::Result<SocketAddr>;
}
impl LocalSocket for TcpStream {
    fn address(&self) -> io::Result<SocketAddr> {
        self.local_addr()
    }
}
impl LocalSocket for TcpListener {
    fn address(&self) -> io::Result<SocketAddr> {
        self.local_addr()
    }
}
impl LocalSocket for std::net::UdpSocket {
    fn address(&self) -> io::Result<SocketAddr> {
        self.local_addr()
    }
}

fn option(stream: &impl AsRawFd, level: i32, name: i32) -> io::Result<i32> {
    let mut value = 0_i32;
    let mut length = std::mem::size_of_val(&value) as libc::socklen_t;
    // SAFETY: value and length are live writable buffers of the declared size.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            level,
            name,
            std::ptr::from_mut(&mut value).cast(),
            &raw mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of_val(&value) {
        return Err(refuse("invalid socket policy value"));
    }
    Ok(value)
}

pub(super) fn socket_policy(
    stream: &impl LocalSocket,
    bound: Option<u32>,
    one_hop: bool,
) -> io::Result<()> {
    let v4 = stream.address()?.is_ipv4();
    let (level, class) = if v4 {
        (libc::IPPROTO_IP, libc::IP_TOS)
    } else {
        (libc::IPPROTO_IPV6, libc::IPV6_TCLASS)
    };
    if option(stream, level, class)? != 0
        || option(stream, libc::SOL_SOCKET, libc::SO_DONTROUTE)? != 0
    {
        return Err(refuse(
            "custom traffic-class or direct-routing socket policy is unsupported",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        if option(stream, libc::SOL_SOCKET, libc::SO_MARK)? != 0 {
            return Err(refuse("marked socket routing is unsupported"));
        }
        let mut name = [0_u8; 16];
        let mut length = name.len() as libc::socklen_t;
        // SAFETY: name and length are writable for the supplied capacity.
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_BINDTODEVICE,
                name.as_mut_ptr().cast(),
                &raw mut length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if length as usize > name.len() {
            return Err(refuse("invalid bound interface name"));
        }
        let actual = if length == 0 {
            0
        } else {
            let name = std::ffi::CStr::from_bytes_with_nul(&name[..length as usize])
                .map_err(|_| refuse("invalid bound interface name"))?
                .to_str()
                .map_err(|_| refuse("invalid bound interface name"))?;
            index(name)?
        };
        if actual != bound.unwrap_or(0) {
            return Err(refuse("socket interface binding changed"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        // IP_BOUND_IF / IPV6_BOUND_IF, XNU netinet/in.h and netinet6/in6.h.
        if option(stream, level, if v4 { 25 } else { 125 })? as u32 != bound.unwrap_or(0) {
            return Err(refuse("socket interface binding changed"));
        }
    }
    if one_hop
        && option(
            stream,
            level,
            if v4 {
                libc::IP_TTL
            } else {
                libc::IPV6_UNICAST_HOPS
            },
        )? != 1
    {
        return Err(refuse("LAN hop limit changed"));
    }
    Ok(())
}

pub(super) fn listener_policy(listener: &TcpListener, bound: Option<u32>) -> io::Result<()> {
    socket_policy(listener, bound, bound.is_some())?;
    if listener.local_addr()?.is_ipv6()
        && option(listener, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY)? != 1
    {
        return Err(refuse("dual-stack LAN listeners are unsupported"));
    }
    Ok(())
}

fn set_option(stream: &impl AsRawFd, level: i32, name: i32, value: u32) -> io::Result<()> {
    // SAFETY: value is readable for its exact four-byte socket-option length.
    if unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            level,
            name,
            std::ptr::from_ref(&value).cast(),
            std::mem::size_of_val(&value) as _,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn bind_interface(stream: &impl LocalSocket, index: u32, name: &str) -> io::Result<()> {
    if index == 0 {
        return Err(refuse("invalid LAN interface binding"));
    }
    #[cfg(target_os = "linux")]
    {
        let name = CString::new(name).map_err(|_| refuse("invalid interface name"))?;
        // SAFETY: name is readable for its NUL-terminated socket-option length.
        if unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_BINDTODEVICE,
                name.as_ptr().cast(),
                name.as_bytes_with_nul().len() as _,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = name;
        let v4 = stream.address()?.is_ipv4();
        set_option(
            stream,
            if v4 {
                libc::IPPROTO_IP
            } else {
                libc::IPPROTO_IPV6
            },
            if v4 { 25 } else { 125 },
            index,
        )?;
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = name;
        return Err(refuse("LAN socket binding is unavailable"));
    }
    socket_policy(stream, Some(index), false)
}

pub(super) fn ensure_binding(stream: &TcpStream, index: u32, name: &str) -> io::Result<()> {
    if index == 0 {
        return Err(refuse("invalid LAN interface binding"));
    }
    if socket_policy(stream, Some(index), false).is_ok() {
        return Ok(());
    }
    // Only an unbound, otherwise ordinary socket may acquire a binding. Never
    // silently redirect a live socket already bound to another attachment.
    socket_policy(stream, None, false)?;
    bind_interface(stream, index, name)
}

pub(super) fn restrict_hops(stream: &TcpStream) -> io::Result<()> {
    if stream.local_addr()?.is_ipv4() {
        set_option(stream, libc::IPPROTO_IP, libc::IP_TTL, 1)
    } else {
        set_option(stream, libc::IPPROTO_IPV6, libc::IPV6_UNICAST_HOPS, 1)
    }
}

fn sequence() -> io::Result<u32> {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        value.checked_add(1)
    })
    .map_err(|_| refuse("route query sequence exhausted"))
}

const MAX_REPLY: usize = 64 * 1024;
/// Unrelated messages and interrupted reads do not invalidate the requested route.
/// The absolute deadline bounds a kernel that never supplies its correlated reply.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn receive_route(
    until: Instant,
    mut next: impl FnMut() -> io::Result<Option<Route>>,
) -> io::Result<Route> {
    loop {
        within(until)?;
        if let Some(route) = next()? {
            return Ok(route);
        }
    }
}
const WITHIN: Duration = Duration::from_secs(60);

/// Hang detector for local kernel observations: admission, recheck, bind/listen and route queries.
pub(super) fn deadline() -> Instant {
    deadline_at(Instant::now())
}
fn deadline_at(started: Instant) -> Instant {
    started + WITHIN
}
pub(super) fn within(until: Instant) -> io::Result<()> {
    if Instant::now() >= until {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "LAN observation deadline expired",
        ))
    } else {
        Ok(())
    }
}

struct Socket(OwnedFd);
impl Socket {
    fn new(domain: i32, protocol: i32) -> io::Result<Self> {
        Self::of_kind(domain, libc::SOCK_RAW, protocol)
    }
    fn of_kind(domain: i32, kind: i32, protocol: i32) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        let kind = kind | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK;
        let fd = aterm_uds::kept_out(|| {
            // SAFETY: socket has no pointer arguments; a successful fd is owned below.
            let raw = unsafe { libc::socket(domain, kind, protocol) };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: this is the fresh, uniquely owned successful socket descriptor.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };
            #[cfg(not(target_os = "linux"))]
            {
                // SAFETY: F_GETFL takes no extra argument on this live fd.
                let status = unsafe { libc::fcntl(raw, libc::F_GETFL) };
                if status < 0 {
                    return Err(io::Error::last_os_error());
                }
                // Before the mark, as F_SETFD replaces every descriptor flag.
                // SAFETY: the setters take an integer flag argument on this live fd.
                if unsafe { libc::fcntl(raw, libc::F_SETFL, status | libc::O_NONBLOCK) } < 0
                    || unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(fd)
        })?;
        Ok(Self(fd))
    }
    fn readable(&self, until: Instant) -> io::Result<()> {
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "route query timed out",
                ));
            }
            let mut descriptor = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = aterm_uds::deadline::poll_timeout_ms(left);
            // SAFETY: descriptor is the sole writable pollfd for the count 1.
            let result = unsafe { libc::poll(&raw mut descriptor, 1, millis) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if result > 0 && descriptor.revents & libc::POLLIN != 0 {
                return Ok(());
            }
            if result > 0 {
                return Err(refuse("route query socket became unavailable"));
            }
        }
    }
}

fn u16_at(bytes: &[u8], at: usize) -> io::Result<u16> {
    bytes
        .get(at..at + 2)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u16::from_ne_bytes)
        .ok_or_else(|| refuse("truncated route integer"))
}
fn u32_at(bytes: &[u8], at: usize) -> io::Result<u32> {
    bytes
        .get(at..at + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_ne_bytes)
        .ok_or_else(|| refuse("truncated route integer"))
}
fn align4(length: usize) -> io::Result<usize> {
    length
        .checked_add(3)
        .map(|n| n & !3)
        .ok_or_else(|| refuse("route length overflow"))
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn unrelated_route_messages_do_not_discard_the_correlated_reply() {
        let expected = Route {
            index: 1,
            source: Some("127.0.0.1".parse().unwrap()),
            direct: false,
        };
        let mut unrelated = 64;
        let actual = receive_route(Instant::now() + Duration::from_secs(60), || {
            if unrelated > 0 {
                unrelated -= 1;
                Ok(None)
            } else {
                Ok(Some(expected.clone()))
            }
        });
        assert_eq!(actual.unwrap(), expected);
    }

    #[test]
    fn route_receive_keeps_the_deadline_and_real_read_errors() {
        let mut calls = 0;
        assert_eq!(
            receive_route(Instant::now(), || {
                calls += 1;
                Ok(None)
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(calls, 0);
        assert_eq!(
            receive_route(Instant::now() + Duration::from_secs(60), || {
                calls += 1;
                Err(io::ErrorKind::PermissionDenied.into())
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(calls, 1);
    }

    fn loopback() -> (&'static str, u32) {
        let name = if cfg!(target_os = "macos") {
            "lo0"
        } else {
            "lo"
        };
        (name, index(name).unwrap())
    }

    #[test]
    fn expired_route_lookup_cannot_restart_the_deadline() {
        assert_eq!(
            route(None, "127.0.0.1:9".parse().unwrap(), None, Instant::now())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn native_route_observation_survives_scheduling_delay() {
        let (_, index) = loopback();
        let peer = "127.0.0.1:9".parse().unwrap();
        let started = Instant::now() - Duration::from_secs(3);
        let observed =
            route_at(None, peer, None, started + Duration::from_secs(60), started).unwrap();
        assert_eq!(observed.index, index);
        assert_eq!(
            route_at(
                None,
                peer,
                None,
                Instant::now() + Duration::from_secs(60),
                Instant::now() - Duration::from_secs(60)
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn accepted_sockets_acquire_or_preserve_but_never_replace_their_binding() {
        let (name, index) = loopback();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (accepted, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        ensure_binding(&accepted, index, name).unwrap();
        ensure_binding(&accepted, index, name).unwrap();
        restrict_hops(&accepted).unwrap();
        socket_policy(&accepted, Some(index), true).unwrap();
        assert!(ensure_binding(&accepted, index.checked_add(1).unwrap(), name).is_err());
        socket_policy(&accepted, Some(index), true).unwrap();
    }
}
