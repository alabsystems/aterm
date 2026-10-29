// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deadline-bounded outbound TCP connections marked `FD_CLOFORK` before `connect(2)`.

use std::io::{self, ErrorKind};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

#[cfg(target_vendor = "apple")]
const SO_NOSIGPIPE: libc::c_int = 0x1022;

enum RawAddr {
    V4(libc::sockaddr_in),
    V6(libc::sockaddr_in6),
}

impl RawAddr {
    fn new(address: SocketAddr) -> Self {
        match address {
            SocketAddr::V4(address) => {
                // SAFETY: all-zero padding is valid for a sockaddr_in.
                let mut raw: libc::sockaddr_in = unsafe { std::mem::zeroed() };
                raw.sin_family = libc::AF_INET as _;
                #[cfg(target_vendor = "apple")]
                {
                    raw.sin_len = std::mem::size_of_val(&raw) as _;
                }
                raw.sin_port = address.port().to_be();
                raw.sin_addr.s_addr = u32::from_ne_bytes(address.ip().octets());
                Self::V4(raw)
            }
            SocketAddr::V6(address) => {
                // SAFETY: all-zero padding is valid for a sockaddr_in6.
                let mut raw: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
                raw.sin6_family = libc::AF_INET6 as _;
                #[cfg(target_vendor = "apple")]
                {
                    raw.sin6_len = std::mem::size_of_val(&raw) as _;
                }
                raw.sin6_port = address.port().to_be();
                raw.sin6_addr.s6_addr = address.ip().octets();
                raw.sin6_scope_id = address.scope_id();
                Self::V6(raw)
            }
        }
    }

    fn parts(&self) -> (*const libc::sockaddr, libc::socklen_t) {
        match self {
            Self::V4(raw) => (
                std::ptr::from_ref(raw).cast(),
                std::mem::size_of_val(raw) as _,
            ),
            Self::V6(raw) => (
                std::ptr::from_ref(raw).cast(),
                std::mem::size_of_val(raw) as _,
            ),
        }
    }
}

/// Connect to `peer` within `within`, returning a blocking [`TcpStream`] whose descriptor was
/// marked `FD_CLOEXEC` and [`aterm_uds::keep_out_of_children`] (`FD_CLOFORK`) before `connect(2)`.
///
/// # Errors
/// Returns `InvalidInput` on a zero or overflowing timeout, `TimedOut` if `within` expires,
/// `HostUnreachable` where a debug build's partition cut lists `peer`
/// ([`aterm_uds::partition::check`]), or the underlying socket/connection OS error.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn connect_timeout_kept(peer: SocketAddr, within: Duration) -> io::Result<TcpStream> {
    aterm_uds::partition::check(peer)?;
    if within.is_zero() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "cannot set a 0 duration timeout",
        ));
    }
    let deadline = Instant::now()
        .checked_add(within)
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "connection timeout overflow"))?;
    let domain = if peer.is_ipv4() {
        libc::AF_INET
    } else {
        libc::AF_INET6
    };
    #[cfg(target_os = "linux")]
    let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK;
    #[cfg(not(target_os = "linux"))]
    let kind = libc::SOCK_STREAM;
    // SAFETY: socket takes only integer arguments and returns a fresh descriptor on success.
    let raw = unsafe { libc::socket(domain, kind, libc::IPPROTO_TCP) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is uniquely owned here, including on every error path below.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    aterm_uds::keep_out_of_children(&fd)?;
    // SAFETY: F_GETFD and F_GETFL read flags from our live descriptor.
    let fd_flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    // SAFETY: as above.
    let fl_flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if fd_flags < 0 || fl_flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_SETFD and F_SETFL update only our descriptor's flags.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, fd_flags | libc::FD_CLOEXEC) } < 0
        // SAFETY: as above.
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, fl_flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    #[cfg(target_vendor = "apple")]
    {
        let enabled: libc::c_int = 1;
        // SAFETY: `enabled` is a live four-byte integer option buffer.
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

    let stream = TcpStream::from(fd);
    let address = RawAddr::new(peer);
    let (pointer, length) = address.parts();
    // SAFETY: `address` owns an initialized sockaddr of `length` bytes for the call.
    let connected = unsafe { libc::connect(stream.as_raw_fd(), pointer, length) };
    if connected < 0 {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINPROGRESS | libc::EINTR | libc::EALREADY) => {
                wait_connected(&stream, deadline)?;
            }
            _ => return Err(error),
        }
    }
    stream.peer_addr()?;
    stream.set_nonblocking(false)?;
    Ok(stream)
}

fn wait_connected(stream: &TcpStream, deadline: Instant) -> io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(ErrorKind::TimedOut, "connection timed out"));
        }
        let millis = remaining.as_millis().saturating_add(u128::from(
            !remaining.subsec_nanos().is_multiple_of(1_000_000),
        ));
        let millis = libc::c_int::try_from(millis).unwrap_or(libc::c_int::MAX);
        let mut poll = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: `poll` points to one initialized pollfd record.
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
        if let Some(error) = stream.take_error()? {
            return Err(error);
        }
        stream.peer_addr()?;
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener};

    #[test]
    fn connect_timeout_kept_sets_flags_and_round_trips_blocking_io() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = connect_timeout_kept(addr, Duration::from_secs(5)).unwrap();
        let (mut server, _) = listener.accept().unwrap();

        // SAFETY: F_GETFD and F_GETFL read flags from our live descriptor.
        let fd_flags = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_GETFD) };
        // SAFETY: as above.
        let fl_flags = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_GETFL) };
        assert!(fd_flags >= 0 && fl_flags >= 0);
        assert_ne!(fd_flags & libc::FD_CLOEXEC, 0, "FD_CLOEXEC must be set");
        #[cfg(target_vendor = "apple")]
        {
            const FD_CLOFORK: libc::c_int = 2;
            assert_ne!(
                fd_flags & FD_CLOFORK,
                0,
                "FD_CLOFORK must be set before connect"
            );
        }
        assert_eq!(
            fl_flags & libc::O_NONBLOCK,
            0,
            "returned stream must be blocking"
        );

        client.write_all(b"ping").unwrap();
        let mut buf = [0u8; 4];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");

        assert_eq!(
            connect_timeout_kept(addr, Duration::ZERO)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
    }

    #[test]
    fn a_dial_the_partition_cuts_reaches_no_listener_until_the_cut_heals() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let cut = format!("{addr}\n");
        let refused = aterm_uds::partition::cut_here(&cut, || {
            connect_timeout_kept(addr, aterm_spec::wait::MUST_HAPPEN)
        })
        .unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::HostUnreachable, "{refused}");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            ErrorKind::WouldBlock,
            "the cut dial made no connection"
        );
        let healed = aterm_uds::partition::cut_here("", || {
            connect_timeout_kept(addr, aterm_spec::wait::MUST_HAPPEN)
        });
        assert!(healed.is_ok(), "{healed:?}");
    }
}
