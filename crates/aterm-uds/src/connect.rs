// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deadline-bounded connections whose socket is kept out of children from its creation: a
//! Unix-domain one ([`until`]) and a TCP one ([`tcp_within`]). Only the `socket(2)` call and its
//! mark hold the spawn lock ([`crate::kept_out`]), never the connect.

use std::io::{self, ErrorKind};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsFd as _, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
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
/// connection or polling error.
#[cfg_attr(trust_verify, trust::skip)]
pub fn until(path: &Path, deadline: Instant) -> io::Result<CtlStream> {
    until_cancelled(path, deadline, None)
}

/// Connect by the original deadline, with a latched stop in the same readiness wait.
/// # Errors
/// As [`until`], or `Interrupted` when the stop is set.
pub fn until_stopped(
    path: &Path,
    deadline: Instant,
    stop: &crate::stop::Stop,
) -> io::Result<CtlStream> {
    until_cancelled(path, deadline, Some(stop.bell()?))
}

pub(crate) fn until_cancelled(
    path: &Path,
    deadline: Instant,
    cancel: Option<BorrowedFd<'_>>,
) -> io::Result<CtlStream> {
    until_cancelled_after_connect(path, deadline, cancel, |_, _| {})
}

fn until_cancelled_after_connect(
    path: &Path,
    deadline: Instant,
    cancel: Option<BorrowedFd<'_>>,
    after_connect: impl FnOnce(&CtlStream, Option<&io::Error>),
) -> io::Result<CtlStream> {
    crate::ready::check_cancelled(cancel)?;
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
    let stream = CtlStream::from(socket(libc::AF_UNIX, libc::SOCK_STREAM)?);
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
    let error = (connected < 0).then(io::Error::last_os_error);
    after_connect(&stream, error.as_ref());
    if let Some(error) = error {
        match error.raw_os_error() {
            Some(libc::EINPROGRESS | libc::EINTR) => {
                // Only an initiated connect may use writability and SO_ERROR as completion.
                wait_connected(stream.as_fd(), deadline, cancel, || {
                    connected_to(stream.take_error()?)
                })?;
            }
            // AF_UNIX EAGAIN (full backlog) has no pending connection to poll.
            _ => return Err(error),
        }
    }
    finish_unix_connect(stream, deadline, cancel)
}

/// The successful connect (or pending-connect check) already proved this connection.
/// A Unix peer can send its verdict and close before we return: on macOS getpeername then
/// returns EINVAL even though the verdict remains readable. Do not discard that stream.
fn finish_unix_connect(
    stream: CtlStream,
    deadline: Instant,
    cancel: Option<BorrowedFd<'_>>,
) -> io::Result<CtlStream> {
    crate::ready::check_cancelled(cancel)?;
    check_deadline(deadline)?;
    stream.set_nonblocking(false)?;
    Ok(stream)
}

/// Connect to `addr` within `timeout`, as `TcpStream::connect_timeout` does, returning a blocking
/// stream whose socket was kept out of children from its creation (and closed on `exec`). The
/// connect, which may take all of `timeout`, holds no spawn back. Every TCP dial of aterm's
/// (a fleet dial, a loopback-scope LAN connection, a vault request, a peer fetch) comes here, and
/// a debug build's partition cut ([`crate::partition::check`]) refuses one before any socket is
/// made. On macOS a local-network `addr` ([`crate::lan::is_local_network`]) is then dialled by the
/// root LAN broker ([`crate::lan::connect`]), whose socket macOS's Local Network privacy does not
/// gate, and comes back passed to this process; while no broker is installed, by this process's
/// own server when macOS already allowed its code ([`crate::lan::direct`]); every other address is
/// dialled here.
///
/// # Errors
/// A zero `timeout` (std refuses one too) or one past the clock's range (`InvalidInput`), an
/// expired one (`TimedOut`), a debug build's partition cut (`HostUnreachable`), on macOS a
/// broker that is not there to ask (`NotConnected`, [`crate::lan::LanError::into_io`]) or the
/// broker's refusal of the connect, or the original socket, descriptor, connection or polling
/// error.
#[cfg_attr(trust_verify, trust::skip)]
pub fn tcp_within(addr: &SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
    tcp_within_cancelled(addr, timeout, None)
}

/// [`tcp_within`] with the stop also owning cancellation of a broker request or pending dial.
/// # Errors
/// As [`tcp_within`], or `Interrupted` when the stop is set.
pub fn tcp_within_stopped(
    addr: &SocketAddr,
    timeout: Duration,
    stop: &crate::stop::Stop,
) -> io::Result<TcpStream> {
    tcp_within_cancelled(addr, timeout, Some(stop))
}

fn tcp_within_cancelled(
    addr: &SocketAddr,
    timeout: Duration,
    stop: Option<&crate::stop::Stop>,
) -> io::Result<TcpStream> {
    crate::partition::check(*addr)?;
    let deadline = Some(timeout)
        .filter(|timeout| !timeout.is_zero())
        .and_then(|timeout| Instant::now().checked_add(timeout))
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "invalid connect timeout"))?;
    #[cfg(target_os = "macos")]
    if crate::lan::is_local_network(&addr.ip()) {
        return match stop {
            Some(stop) => crate::lan::broker::connect_stopped(
                &crate::lan::Target::Addr(*addr),
                addr.port(),
                timeout,
                crate::lan::ConnectOptions::default(),
                stop,
            ),
            None => crate::lan::broker::connect(
                &crate::lan::Target::Addr(*addr),
                addr.port(),
                timeout,
                crate::lan::ConnectOptions::default(),
            ),
        }
        .map(|(stream, _)| stream)
        .map_err(crate::lan::LanError::into_io);
    }
    let cancel = stop.map(crate::stop::Stop::bell).transpose()?;
    tcp_until_cancelled(addr, deadline, |_| Ok(()), cancel)
}

/// Dial `addr` from this process by `deadline`, `configure` given the socket before its connect
/// (a bound interface, a source address, a hop limit): [`tcp_within`]'s connect, with no
/// partition cut and no broker. Returns a blocking stream kept out of children from its creation.
///
/// # Errors
/// An expired `deadline` (`TimedOut`), `configure`'s error, or the original socket, descriptor,
/// connection or polling error.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn tcp_until(
    addr: &SocketAddr,
    deadline: Instant,
    configure: impl FnOnce(&TcpStream) -> io::Result<()>,
) -> io::Result<TcpStream> {
    tcp_until_cancelled(addr, deadline, configure, None)
}

pub(crate) fn tcp_until_cancelled(
    addr: &SocketAddr,
    deadline: Instant,
    configure: impl FnOnce(&TcpStream) -> io::Result<()>,
    cancel: Option<BorrowedFd<'_>>,
) -> io::Result<TcpStream> {
    tcp_until_cancelled_after_connect(addr, deadline, configure, cancel, |_, _| {})
}

fn tcp_until_cancelled_after_connect(
    addr: &SocketAddr,
    deadline: Instant,
    configure: impl FnOnce(&TcpStream) -> io::Result<()>,
    cancel: Option<BorrowedFd<'_>>,
    after_connect: impl FnOnce(&TcpStream, Option<&io::Error>),
) -> io::Result<TcpStream> {
    crate::ready::check_cancelled(cancel)?;
    check_deadline(deadline)?;
    let family = if addr.is_ipv4() {
        libc::AF_INET
    } else {
        libc::AF_INET6
    };
    let stream = TcpStream::from(socket(family, libc::SOCK_STREAM)?);
    configure(&stream)?;
    stream.set_nonblocking(true)?;
    let address = Inet::new(*addr);
    let (pointer, length) = address.parts();
    // SAFETY: address owns the initialized sockaddr for the whole call.
    let connected = unsafe { libc::connect(stream.as_raw_fd(), pointer, length) };
    let error = (connected < 0).then(io::Error::last_os_error);
    after_connect(&stream, error.as_ref());
    if let Some(error) = error {
        match error.raw_os_error() {
            Some(libc::EINPROGRESS | libc::EINTR | libc::EALREADY) => {
                wait_connected(stream.as_fd(), deadline, cancel, || {
                    connected_to(stream.take_error()?)
                })?;
            }
            _ => return Err(error),
        }
    }
    // A successful connect remains established even if the peer already sent and closed.
    crate::ready::check_cancelled(cancel)?;
    stream.set_nonblocking(false)?;
    Ok(stream)
}

/// A socket of `domain` and `kind` (`SOCK_STREAM`, `SOCK_DGRAM`), kept out of children from its
/// creation ([`crate::kept_out`]), closed on `exec`, and on Apple never raising `SIGPIPE`.
pub(crate) fn socket(domain: libc::c_int, kind: libc::c_int) -> io::Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    let kind = kind | libc::SOCK_CLOEXEC;
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

/// Completion of an initiated connect: SO_ERROR retains a failure after writability.
fn connected_to(pending: Option<io::Error>) -> io::Result<()> {
    match pending {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Wait by `deadline` for the connect in flight on `fd` to end, then ask `connected` how.
fn wait_connected(
    fd: BorrowedFd<'_>,
    deadline: Instant,
    cancel: Option<BorrowedFd<'_>>,
    connected: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    crate::ready::until_cancelled(fd, crate::ready::Interest::Write, deadline, cancel)?;
    crate::ready::check_cancelled(cancel)?;
    connected()
}

#[cfg(test)]
#[path = "connect_tests.rs"]
mod tests;
