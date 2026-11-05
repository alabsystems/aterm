// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deadline-bounded connections whose socket is kept out of children from its creation: a
//! Unix-domain one ([`until`]) and a TCP one ([`tcp_within`]). Only the `socket(2)` call and its
//! mark hold the spawn lock ([`crate::kept_out`]), never the connect.

use std::io::{self, ErrorKind};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::CtlStream;

#[cfg(target_vendor = "apple")]
const SO_NOSIGPIPE: libc::c_int = 0x1022;

/// Connect to a filesystem socket by `deadline`, returning a blocking stream.
/// The descriptor is excluded from spawned children before connecting. A full
/// Linux listen backlog refuses immediately with its original `EAGAIN` error.
///
/// # Errors
/// Invalid paths, an expired deadline, or the original socket, descriptor,
/// connection, polling, or peer-address error.
#[cfg_attr(trust_verify, trust::skip)]
pub fn until(path: &Path, deadline: Instant) -> io::Result<CtlStream> {
    check_deadline(deadline)?;
    // SAFETY: sockaddr_un is a plain C address; zero also terminates sun_path.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "invalid Unix socket path",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = libc::c_char::from_ne_bytes([*byte]);
    }
    let length =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    #[cfg(target_vendor = "apple")]
    {
        address.sun_len = length as u8;
    }
    let stream = CtlStream::from(socket(libc::AF_UNIX)?);
    stream.set_nonblocking(true)?;
    check_deadline(deadline)?;
    // SAFETY: address is initialized, and length covers its family and terminated path.
    let connected = unsafe {
        libc::connect(
            stream.as_raw_fd(),
            std::ptr::from_ref(&address).cast(),
            length,
        )
    };
    if connected < 0 {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINPROGRESS | libc::EINTR) => {
                // Writability alone can describe an unconnected AF_UNIX socket.
                wait_connected(stream.as_raw_fd(), deadline, || {
                    connected_to(stream.take_error()?, stream.peer_addr())
                })?;
            }
            // AF_UNIX EAGAIN (full backlog) has no pending connection to poll.
            _ => return Err(error),
        }
    }
    stream.peer_addr()?;
    check_deadline(deadline)?;
    stream.set_nonblocking(false)?;
    Ok(stream)
}

/// Connect to `addr` within `timeout`, as `TcpStream::connect_timeout` does, returning a blocking
/// stream whose socket was kept out of children from its creation (and closed on `exec`). The
/// connect, which may take all of `timeout`, holds no spawn back. Every TCP dial of aterm's
/// (a fleet dial, a loopback-scope LAN connection, a vault request, a peer fetch) comes here, and
/// a debug build's partition cut ([`crate::partition::check`]) refuses one before any socket is
/// made.
///
/// # Errors
/// A zero `timeout` (std refuses one too) or one past the clock's range (`InvalidInput`), an
/// expired one (`TimedOut`), a debug build's partition cut (`HostUnreachable`), or the original
/// socket, descriptor, connection or polling error.
#[cfg_attr(trust_verify, trust::skip)]
pub fn tcp_within(addr: &SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
    crate::partition::check(*addr)?;
    let deadline = Some(timeout)
        .filter(|timeout| !timeout.is_zero())
        .and_then(|timeout| Instant::now().checked_add(timeout))
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "invalid connect timeout"))?;
    let family = if addr.is_ipv4() {
        libc::AF_INET
    } else {
        libc::AF_INET6
    };
    let stream = TcpStream::from(socket(family)?);
    stream.set_nonblocking(true)?;
    let address = Inet::new(*addr);
    let (pointer, length) = address.parts();
    // SAFETY: address owns the initialized sockaddr for the whole call.
    if unsafe { libc::connect(stream.as_raw_fd(), pointer, length) } < 0 {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINPROGRESS | libc::EINTR | libc::EALREADY) => {
                wait_connected(stream.as_raw_fd(), deadline, || {
                    connected_to(stream.take_error()?, stream.peer_addr())
                })?;
            }
            _ => return Err(error),
        }
    }
    stream.peer_addr()?;
    stream.set_nonblocking(false)?;
    Ok(stream)
}

/// A stream socket of `domain`, kept out of children from its creation ([`crate::kept_out`]),
/// closed on `exec`, and on Apple never raising `SIGPIPE`.
fn socket(domain: libc::c_int) -> io::Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let kind = libc::SOCK_STREAM;
    let fd = crate::kept_out(|| {
        // SAFETY: socket takes only integer arguments and returns an owned descriptor.
        let raw = unsafe { libc::socket(domain, kind, 0) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: this fresh descriptor has no other owner, including on error paths.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // Before the mark, as F_SETFD replaces every descriptor flag.
        #[cfg(not(target_os = "linux"))]
        // SAFETY: F_SETFD changes only our descriptor's inheritance flags.
        if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fd)
    })?;
    #[cfg(target_vendor = "apple")]
    {
        // Adopting an OwnedFd bypasses std's Apple socket constructor.
        let enabled: libc::c_int = 1;
        // SAFETY: enabled is a live socket option value of the supplied size.
        if unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                SO_NOSIGPIPE,
                std::ptr::from_ref(&enabled).cast(),
                std::mem::size_of_val(&enabled) as libc::socklen_t,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(fd)
}

/// An IP socket address as `connect(2)` and `bind(2)` take it: aterm's one builder of a raw
/// `sockaddr_in`/`sockaddr_in6` from a [`SocketAddr`].
pub enum Inet {
    /// An IPv4 address.
    V4(libc::sockaddr_in),
    /// An IPv6 address, with its flow label and scope.
    V6(libc::sockaddr_in6),
}

impl Inet {
    /// The raw form of `address`, its length field set where the platform has one.
    #[must_use]
    pub fn new(address: SocketAddr) -> Self {
        match address {
            SocketAddr::V4(address) => {
                // SAFETY: all-zero padding is valid for a sockaddr_in.
                let mut raw: libc::sockaddr_in = unsafe { std::mem::zeroed() };
                raw.sin_family = libc::AF_INET as libc::sa_family_t;
                #[cfg(target_vendor = "apple")]
                {
                    raw.sin_len = std::mem::size_of_val(&raw) as u8;
                }
                raw.sin_port = address.port().to_be();
                raw.sin_addr.s_addr = u32::from_ne_bytes(address.ip().octets());
                Self::V4(raw)
            }
            SocketAddr::V6(address) => {
                // SAFETY: all-zero padding is valid for a sockaddr_in6.
                let mut raw: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
                raw.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                #[cfg(target_vendor = "apple")]
                {
                    raw.sin6_len = std::mem::size_of_val(&raw) as u8;
                }
                raw.sin6_port = address.port().to_be();
                raw.sin6_flowinfo = address.flowinfo();
                raw.sin6_addr.s6_addr = address.ip().octets();
                raw.sin6_scope_id = address.scope_id();
                Self::V6(raw)
            }
        }
    }

    /// The pointer and length a socket call takes, valid while `self` lives.
    #[must_use]
    pub fn parts(&self) -> (*const libc::sockaddr, libc::socklen_t) {
        match self {
            Self::V4(raw) => (
                std::ptr::from_ref(raw).cast(),
                std::mem::size_of_val(raw) as libc::socklen_t,
            ),
            Self::V6(raw) => (
                std::ptr::from_ref(raw).cast(),
                std::mem::size_of_val(raw) as libc::socklen_t,
            ),
        }
    }
}

fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::Error::new(
            ErrorKind::TimedOut,
            "socket connect deadline expired",
        ))
    } else {
        Ok(())
    }
}

/// Connected: no error pending on the socket, and a peer.
fn connected_to<A>(pending: Option<io::Error>, peer: io::Result<A>) -> io::Result<()> {
    match pending {
        Some(error) => Err(error),
        None => peer.map(drop),
    }
}

/// Wait by `deadline` for the connect in flight on `fd` to end, then ask `connected` how.
fn wait_connected(
    fd: RawFd,
    deadline: Instant,
    connected: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    loop {
        check_deadline(deadline)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        // Round up so sub-millisecond remaining time does not become a busy poll.
        let millis = remaining.as_millis().saturating_add(u128::from(
            !remaining.subsec_nanos().is_multiple_of(1_000_000),
        ));
        let millis = libc::c_int::try_from(millis).unwrap_or(libc::c_int::MAX);
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: poll points to exactly one initialized descriptor record.
        let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            continue;
        }
        return connected();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStringExt;
    use std::time::Duration;

    /// A hang detector, never a latency budget.
    const MUST_HAPPEN: Duration = Duration::from_secs(60);

    fn deadline() -> Instant {
        Instant::now() + MUST_HAPPEN
    }

    #[test]
    fn connected_stream_has_peer_identity_and_is_blocking_and_cloexec() {
        let root = aterm_tempfile::tempdir().unwrap();
        let path = root.path().join("connect.sock");
        let listener = crate::CtlListener::bind(&path).unwrap();
        crate::keep_out_of_children(&listener).unwrap();
        let mut client = until(&path, deadline()).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        crate::keep_out_of_children(&server).unwrap();
        assert_eq!(crate::peer::peer_uid(&client), Some(crate::peer::our_uid()));
        // SAFETY: both fcntl calls read flags from a live test descriptor.
        let status = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_GETFL) };
        let descriptor = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_GETFD) };
        assert!(status >= 0 && status & libc::O_NONBLOCK == 0);
        assert!(descriptor >= 0 && descriptor & libc::FD_CLOEXEC != 0);
        #[cfg(target_vendor = "apple")]
        {
            let mut enabled: libc::c_int = 0;
            let mut length = std::mem::size_of_val(&enabled) as libc::socklen_t;
            // SAFETY: enabled and length are writable and describe the option buffer.
            assert_eq!(
                unsafe {
                    libc::getsockopt(
                        client.as_raw_fd(),
                        libc::SOL_SOCKET,
                        SO_NOSIGPIPE,
                        std::ptr::from_mut(&mut enabled).cast(),
                        &raw mut length,
                    )
                },
                0
            );
            assert_eq!(enabled, 1);
        }
        server
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        client.write_all(b"x").unwrap();
        let mut byte = [0];
        server.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"x");
    }

    #[test]
    fn expired_missing_and_invalid_paths_refuse() {
        let root = aterm_tempfile::tempdir().unwrap();
        let missing = root.path().join("missing.sock");
        assert_eq!(
            until(&missing, Instant::now()).unwrap_err().kind(),
            ErrorKind::TimedOut
        );
        assert_eq!(
            until(&missing, deadline()).unwrap_err().raw_os_error(),
            Some(libc::ENOENT)
        );
        for path in [
            Path::new("").to_owned(),
            std::ffi::OsString::from_vec(b"bad\0path".to_vec()).into(),
            Path::new(&"x".repeat(256)).to_owned(),
        ] {
            assert_eq!(
                until(&path, deadline()).unwrap_err().kind(),
                ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn a_tcp_connection_blocks_closes_on_exec_and_is_kept_out_of_children() {
        for place in [
            std::net::IpAddr::from(std::net::Ipv4Addr::LOCALHOST),
            std::net::Ipv6Addr::LOCALHOST.into(),
        ] {
            let listener = std::net::TcpListener::bind((place, 0)).unwrap();
            let mut client = tcp_within(&listener.local_addr().unwrap(), MUST_HAPPEN).unwrap();
            let (mut server, _) = listener.accept().unwrap();
            assert_eq!(client.peer_addr().unwrap(), listener.local_addr().unwrap());
            // SAFETY: both fcntl calls read flags from a live test descriptor.
            let status = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_GETFL) };
            // SAFETY: as above.
            let descriptor = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_GETFD) };
            assert!(status >= 0 && status & libc::O_NONBLOCK == 0);
            assert!(descriptor >= 0 && descriptor & libc::FD_CLOEXEC != 0);
            // `FD_CLOFORK`, which Apple's libc crate does not name.
            #[cfg(target_vendor = "apple")]
            assert_eq!(
                descriptor & 2,
                if crate::spawn_lock::tests::clofork_supported() {
                    2
                } else {
                    0
                },
                "{place}: the connection agrees with the kernel's supported flags"
            );
            server.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
            client.write_all(b"x").unwrap();
            let mut byte = [0];
            server.read_exact(&mut byte).unwrap();
            assert_eq!(byte, *b"x");
        }
    }

    #[test]
    fn a_tcp_connect_refused_or_given_no_time_says_so() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert_eq!(
            tcp_within(&addr, MUST_HAPPEN).unwrap_err().kind(),
            ErrorKind::ConnectionRefused
        );
        assert_eq!(
            tcp_within(&addr, Duration::ZERO).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    }

    #[test]
    fn a_dial_the_partition_cuts_reaches_no_listener_until_the_cut_heals() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let cut = format!("{addr}\n");
        let refused =
            crate::partition::cut_here(&cut, || tcp_within(&addr, MUST_HAPPEN)).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::HostUnreachable, "{refused}");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            ErrorKind::WouldBlock,
            "the cut dial made no connection"
        );
        let healed = crate::partition::cut_here("", || tcp_within(&addr, MUST_HAPPEN));
        assert!(healed.is_ok(), "{healed:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn full_linux_backlog_refuses_without_returning_an_unconnected_stream() {
        let root = aterm_tempfile::tempdir().unwrap();
        let path = root.path().join("backlog.sock");
        let listener = crate::CtlListener::bind(&path).unwrap();
        crate::keep_out_of_children(&listener).unwrap();
        // SAFETY: listen changes the queue size of this live listening socket.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let first = until(&path, deadline()).unwrap();
        let refused = until(&path, deadline()).unwrap_err();
        assert_eq!(refused.raw_os_error(), Some(libc::EAGAIN));
        assert!(first.peer_addr().is_ok());
    }
}
