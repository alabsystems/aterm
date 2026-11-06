// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Route, refuse};
use std::{
    ffi::CString,
    io,
    net::{SocketAddr, TcpListener, TcpStream},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
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
    let until = until.min(deadline());
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
const MAX_MESSAGES: usize = 64;
const WITHIN: Duration = Duration::from_secs(2);

/// When a LAN observation begun now must end (an admission, a recheck, a listener's bind and
/// listen, and the cap on every route query's own deadline): [`WITHIN`] from now, or in a test
/// binary (a crate's unit tests, or a `tests/*.rs` binary, which links this library without
/// `cfg(test)`) the hang detector from now, since its routes, sockets and listeners are this
/// machine's kernel's, which always answer however loaded the machine is.
pub(super) fn deadline() -> Instant {
    #[cfg(debug_assertions)]
    if aterm_uds::test_binary::scratch_root().is_some() {
        // The hang detector (`aterm_spec::wait::MUST_HAPPEN`), which this library cannot name.
        return Instant::now() + Duration::from_secs(60);
    }
    Instant::now() + WITHIN
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
            let millis = left.as_millis().min(i32::MAX as u128).max(1) as i32;
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
    fn accepted_sockets_acquire_or_preserve_but_never_replace_their_binding() {
        let (name, index) = loopback();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        ensure_binding(&accepted, index, name).unwrap();
        ensure_binding(&accepted, index, name).unwrap();
        restrict_hops(&accepted).unwrap();
        socket_policy(&accepted, Some(index), true).unwrap();
        assert!(ensure_binding(&accepted, index.checked_add(1).unwrap(), name).is_err());
        socket_policy(&accepted, Some(index), true).unwrap();
    }
}
